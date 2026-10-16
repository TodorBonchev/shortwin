//! The keeper loop (ARCHITECTURE §7.1): one [`Keeper::tick`] per slot.
//!
//! Each tick:
//!
//! 1. reads the clock, re-discovers markets every `discovery_interval_s`, reads the open
//!    positions, and loads every covered market that has positions or anything time-critical
//!    pending, and the others ("cold") every `cold_interval_s`, staggered (M7-SEC-8;
//!    [`crate::view::load_with_positions`]);
//! 2. per market: records its own spot and depth view and checks the buckets for a foreign
//!    deviating observation (duty 2: a price starts a per-slot burst; on a market with open
//!    positions a liquidity starts `liq_cover_s` of per-second coverage, FITS-2 (b)); projects the prologue at the clock and evaluates every open
//!    position ([`crate::decide`]); sends the exits the projection allows — `liquidate` at the
//!    first second its bound is met (duty 3), `expire` when the expiry bound is met, and
//!    `sol_settle` at most once per second for armed positions and positions without a normal
//!    buyback (duty 4) or whose S3 is due — each simulated first, trading exits with the first
//!    extras set that simulates (duty 10); a landed exit **is** the market's observation for the
//!    second (duty 7);
//! 3. queues a `crank_price` for every market whose observation is due and was not made by an
//!    exit ([`crate::coverage`]), and sends the queue in batches: several markets per v1
//!    transaction (6), v0 batches (4) where v1 is not accepted; each batch is simulated, a
//!    market whose crank fails simulation is dropped and cranked alone (duty 7);
//! 4. closes the previous second's coverage accounting (misses → degraded mode; `CoverageGap`
//!    events from the program → degraded mode; duty 9) and the metrics (duty 8).
//!
//! After the exits and before the cranks, Raydium pool selection (duty 5's fast path and duty
//! 12, the pool monitor; [`crate::pool_monitor`]) on every covered Raydium and list-only
//! market: discover the mint's Raydium SOL pools, rank the eligible ones with the program's own
//! verifier, and send `propose_pool` / `execute_pool_switch` exactly when the program's own
//! `pool_select::decide` accepts — a list-only market's first pool, a listing-window
//! replacement (5 %), a zero-position adoption (25 %; in the tick the last position closes),
//! the fast path's window (opened at the first armed second on the deepest candidate, improved
//! by a deeper one, executed at the first second after it); pool events are reported, a market
//! pinned by open positions to a much thinner pool is alerted (R4B-10). A market adopted in
//! the tick is re-read, so its crank that tick names the new pool.
//!
//! Before the markets, the venue drift monitor (duty 10, [`crate::monitor`]): the venue
//! ProgramData headers against the `VenueGuard` latch — an upgrade is flagged with
//! `flag_venue_upgraded` in the slot the keeper sees it, at the exit priority ceiling — and
//! every covered market's pool and CPMM config against the last tick (Raydium admin actions:
//! route lost or restored, status, open time, fee against the opens and hard caps), as alerts.
//! A moved slot the header cannot judge (AIR-01: a fingerprinted venue's with its latch bit clear)
//! is judged by the flag's own simulation — the program's rule on the whole account, all such
//! venues of the tick simulated together (one priority price, one blockhash, the simulations in
//! parallel) — so an upgrade is still flagged before the exits, and the loader's permissionless
//! `ExtendProgram` (`VenueNotUpgraded`, from a bank that holds the deploy the header showed) is
//! neither flagged nor paged. What needs the whole ProgramData (a slot that simulation could not
//! judge — refused, answered from a bank before the deploy, or short.win not deployed in that
//! slot —, a move during a review, the record of the deploy under review, Token-2022's changes)
//! is read after the exits and cranks, all in one read, and judged by the same rule.
//!
//! **List-only markets** (M5): covered like any market with their detection set as ReadSet — one
//! with positions open (a curve that graduated elsewhere) is observed on the no-route cadence and
//! its positions are settled in S3 when due (duty 5, `sol_settle` on the detection set,
//! M2-SEC-5); one whose detection set shows a pump curve again is cranked onto it, and the
//! trader's curve accounts are then created ([`crate::graduation::redetection_due`]).
//!
//! **Graduation** (duty 6, [`crate::graduation`]; M5, M6; pump-final §7): before a curve market's
//! exits, pump `migrate_v2` top-level as soon as its curve is complete and unmigrated, then — as
//! soon as its canonical pool is valid, whoever migrated — the `→ PumpSwap` transition (the pool
//! accounts its trades require, `crank_price` onto the pool, the pool's first observation) in one
//! transaction, which is the market's observation for the second; a PumpSwap market whose pool
//! accounts are missing gets them created. A liquidatable or expired position whose `Q` exceeds
//! its live curve's `rT` is liquidated or expired like any other: pump v3 buys past `rT` by
//! synthetic migration inside the exit, which completes the curve (`PumpMigrating` in the same
//! instruction); the keeper then migrates and transitions the market in the same tick, and the
//! market's other positions exit on the pool at the post-change warm-up's gate.
//!
//! **The curve TradeExtras format** (pump-final §7, §15 "Release coordination"): for one release
//! the keeper builds both a curve exit's v3 list (10 keys) and shortwin 1.0.0's (20,
//! [`crate::venue::CurveExtrasFormat`]), and uses the one the deployed short.win parses, known per
//! short.win's ProgramData deploy slot (its header, read once a tick when a curve exit is due).
//! At start-up and at each new deploy slot the first curve exit is simulated with V3; if that
//! fails on account parsing (`BadRemainingAccounts`, `VenueAccountMismatch`) it is simulated
//! with V2. The format that gets past account parsing is kept until the deploy slot changes (or
//! an exit with it fails on account parsing, which tries the other at once). Neither, on a deploy
//! slot no exit found the format of, alerts once per position ([`Alert::CurveExtrasFormatUnknown`]);
//! on one whose format an exit found, the refusal is the exit's own — its market's ReadSet or
//! VenueExtras raise the same codes — and the known format's is paged as any refusal. Every send
//! is simulated first, on a bank at or past the deploy slot read (a node of an RPC pool behind
//! that read answers for the build before: not sent, tried again the next tick), so a Governance
//! upgrade landing between two ticks costs no failed send. On 1.0.0 a curve exit past `rT` is
//! refused `CurveStraddle` (1.0.0's exits never buy past `rT`): the position goes to `sol_settle`
//! instead, unpaged, and waits for the upgrade (pump-final §15 step 1).
//!
//! **The trader side** (duties 6 and 13, [`crate::sweep`]): right after a full wipe, the venue
//! cashback claims the trader needs and `sweep_trader` (a deposit, and hence an open, waits for
//! both post-wipe bits); every `sweep.check_interval_s`, `sweep_trader` when the trader side holds
//! anything, and the weekly cashback claim on cashback coins.
//!
//! Batches are **packed by size**: a batch takes up to `batch_v1` / `batch_v0` cranks and is cut
//! where its transaction would exceed the format's size or account limit, so any mix of venue
//! account sets (the mock's 3 ReadSet keys, AMM v4's 4, CPMM's 5) is batched as densely as fits.
//!
//! Every transaction: simulated first, compute-unit limit `⌈simulated × 1.1⌉`, priority price
//! from recent per-account fees, escalated after misses, capped at the O17 ceiling (duty 14).
//! A refusal because one of its programs was deployed, upgraded or extended in the slot ("Program
//! is not deployed": AIR-18 — the loader's permissionless `ExtendProgram` blocks short.win or a
//! venue for the rest of a slot) is transient ([`ExitResult::NotDeployed`]) when the program's
//! ProgramData, read as of the refused slot, shows that deploy: no alert, no back-off; the next
//! tick, the next slot, sends again — a run of them lasting [`NOT_DEPLOYED_ALERT_S`] is paged once
//! ([`Alert::ProgramNotDeployed`]). The loader's same answer for a program that stays
//! undeployable (closed, failing verification) is handled as any refusal, alerted.
//! Duty 3's Jito bundle path for ordinary exits is not built (see the crate README); no keeper
//! duty sends a bundle since the straddle bundle's retirement (pump-final §7).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use anyhow::anyhow;
use shortwin::engine::pool_select::{self, FitContext, Proposal, SelectParams, Situation};
use shortwin::engine::{guard, prologue};
use shortwin::errors::ShortwinError;
use shortwin::events::{CoverageGap, SolSettle};
use shortwin::state::{EXTRAS_IX_BUY, MARKET_FLAG_PAUSED_COVERAGE, Market, VenueGuard, VenueKind};
use shortwin::venues::ExtrasSel;
use shortwin_math::tier::OraclePrice;
use shortwin_services_common::tx::{self, ComputeBudget, TxFormat};
use shortwin_services_common::{AccountData, Chain, ChainClock, TxError, TxOutcome};
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;
use solana_signer::Signer;

use crate::alerts::{Alert, PauseUnavailable};
use crate::budget::{self, Admission, BudgetParams, ChargeClass, ChargeReq, GlobalBudget, Tier};
use crate::config::{KeeperConfig, TxFormatPolicy};
use crate::coverage::{Due, FlagSet, MarketTrack};
use crate::decide::{self, MarketNeeds, Plan, PositionEval};
use crate::events::{self, PoolEvent, PoolEventKind};
use crate::fees;
use crate::graduation::{self, MigrateRecord};
use crate::ixs;
use crate::metrics::Metrics;
use crate::monitor::{self, FeeCaps, LatchAction, PoolWatch};
use crate::pda;
use crate::pool_monitor::{self, Action, PoolPlan, PoolTrack, Seen};
use crate::sweep::{self, SweepFacts, SweepKind, SweepRecord};
use crate::venue::{self, CurveExtrasFormat, FormatEvidence, PammAccount};
use crate::view::{self, ChainView, MarketView, PositionView};

/// M7-SEC-8: a cold market's poll phase in `[0, every)` (from its key), so the cold markets'
/// reads spread over the interval.
fn cold_phase(key: &Pubkey, every: i64) -> i64 {
    let b = key.to_bytes();
    let h = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    i64::try_from(h % u64::try_from(every.max(1)).unwrap_or(1)).unwrap_or(0)
}

/// Whether the cluster accepts v1 transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V1Support {
    Unknown,
    Yes,
    /// Refused; re-probed from `until`.
    No {
        until: i64,
    },
}

/// The exit instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExitKind {
    Liquidate,
    Expire,
    SolSettle,
}

/// What became of an exit attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitResult {
    /// Landed (the transaction's outcome, its format, the extras set used, the compute-unit
    /// limit it requested — `⌈simulated × 1.1⌉`, what a leader charges against the per-account
    /// block budget, C0-17 — and its serialized size).
    Landed {
        outcome: TxOutcome,
        format: TxFormat,
        extras: ExtrasSel,
        unit_limit: u32,
        bytes: usize,
    },
    /// The program refused it in simulation (`code`: its custom error, if any).
    Refused {
        code: Option<u32>,
        outcome: TxOutcome,
    },
    /// Sent, and failed on chain.
    Failed { outcome: TxOutcome },
    /// Dry run: simulated only.
    DryRun { outcome: TxOutcome },
    /// An S3 probe whose simulation settles nothing: not sent.
    NoOp,
    /// T-CASCADE: simulated, but its compute-unit limit would take the market's requests this
    /// slot above `budget.market_cu_per_slot`; not sent (retried next slot).
    Deferred { unit_limit: u32 },
    /// AIR-18: the cluster would not run a program of the transaction this slot ("Program is not
    /// deployed", [`events::not_deployed`]), in simulation or on chain, and `program` — the one
    /// the logs name, short.win or the venue — was deployed, upgraded or extended in that slot
    /// (its ProgramData shows it, [`events::deployed_in_refused_slot`]; the loader's
    /// `ExtendProgram` is permissionless). Transient: not alerted, no back-off, retried in the
    /// next tick (slot). The same refusal of a program that stays undeployable is `Refused` or
    /// `Failed`.
    NotDeployed { program: Pubkey, outcome: TxOutcome },
    /// A transport or building error.
    Error(String),
}

impl ExitResult {
    /// The landed outcome, if any.
    pub fn landed(&self) -> Option<&TxOutcome> {
        match self {
            Self::Landed { outcome, .. } => Some(outcome),
            _ => None,
        }
    }

    /// What this attempt says about the curve TradeExtras format it carried (pump-final §7;
    /// [`venue::format_evidence`]): a send follows a simulation that got past account parsing,
    /// unless the transaction then failed on it — a Governance upgrade between the simulation and
    /// the landing.
    pub fn format_evidence(&self) -> FormatEvidence {
        match self {
            Self::Landed { .. } | Self::DryRun { .. } | Self::Deferred { .. } => {
                FormatEvidence::Parsed
            }
            Self::Refused { outcome, .. } | Self::Failed { outcome } => {
                venue::format_evidence(outcome)
            }
            Self::NoOp | Self::NotDeployed { .. } | Self::Error(_) => FormatEvidence::Unknown,
        }
    }

    /// The custom error of a refusal or an on-chain failure.
    fn code(&self) -> Option<u32> {
        match self {
            Self::Refused { outcome, .. } | Self::Failed { outcome } => {
                outcome.err.as_ref().and_then(TxError::custom_code)
            }
            _ => None,
        }
    }
}

/// One exit attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitRecord {
    pub market: Pubkey,
    pub position: Pubkey,
    pub kind: ExitKind,
    /// The plan that led to it.
    pub plan: Plan,
    /// A `PumpCurve` liquidation or expiry: the TradeExtras format it carried (pump-final §7).
    pub curve_format: Option<CurveExtrasFormat>,
    pub result: ExitResult,
}

/// pump-final §7, the upgrade window: one curve exit whose TradeExtras format the keeper had to
/// find — none known for short.win's deploy slot, or the known one refused on account parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveFormatRecord {
    pub market: Pubkey,
    pub position: Pubkey,
    /// short.win's ProgramData deploy slot as read this tick (`None`: unreadable).
    pub deploy_slot: Option<u64>,
    /// The formats refused on account parsing, in the order tried, with the custom error.
    pub refused: Vec<(CurveExtrasFormat, Option<u32>)>,
    /// The format that got past account parsing (now the known one), or `None`: neither did, or
    /// the attempt said nothing (a refusal before account parsing, a transport error, AIR-18).
    pub chosen: Option<CurveExtrasFormat>,
}

/// One crank batch sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchRecord {
    pub markets: Vec<Pubkey>,
    pub format: TxFormat,
    /// Serialized size and account keys of the sent transaction.
    pub bytes: usize,
    pub accounts: usize,
    pub units: u64,
    pub fee: u64,
    /// Priority price, micro-lamports per CU.
    pub price: u64,
    /// Compute-unit limit requested (`⌈simulated × 1.1⌉`).
    pub unit_limit: u32,
}

/// What became of a pool-selection attempt (`propose_pool`, `execute_pool_switch`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolResult {
    /// Landed, with its pool events.
    Landed {
        outcome: TxOutcome,
        events: Vec<PoolEvent>,
    },
    /// The program refused it in simulation (`code`: its custom error, if any).
    Refused {
        code: Option<u32>,
        outcome: TxOutcome,
    },
    /// Sent, and failed on chain.
    Failed { outcome: TxOutcome },
    /// Dry run: simulated only.
    DryRun { outcome: TxOutcome },
    /// AIR-18: a program of the transaction could not run this slot, deployed in it (as
    /// [`ExitResult::NotDeployed`]): no alert, no back-off, retried in the next tick (slot).
    NotDeployed { program: Pubkey, outcome: TxOutcome },
    /// A transport or building error.
    Error(String),
}

impl PoolResult {
    /// Whether it landed.
    pub fn landed(&self) -> bool {
        matches!(self, Self::Landed { .. })
    }
}

/// One pool-selection attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolActionRecord {
    pub market: Pubkey,
    pub action: Action,
    pub result: PoolResult,
}

/// Pool-selection refusals that are the program's normal "not now" answers after a race (a
/// proposal or adoption landed between the keeper's read and its send; the latch) — no alert.
fn benign_pool(code: Option<u32>) -> bool {
    [
        ShortwinError::PoolNotDominant,
        ShortwinError::PositionsOpen,
        ShortwinError::PoolWindowClosed,
        ShortwinError::PoolSwitchNotDue,
        ShortwinError::NoPendingSwitch,
        ShortwinError::VenueUpgraded,
    ]
    .into_iter()
    .any(|e| code == Some(u32::from(e)))
}

/// A failing Raydium market without positions whose candidate the fast path could take (the
/// plan's `keep_chain`): duty 5's no-route cadence does not observe a market without positions,
/// so the pool monitor queues its crank — at once while the chain has not started, then every
/// `every` seconds (< `S3_OBS_GAP_S`, so the chain stays continuous until the fast path arms).
fn keep_pool_fail_chain(
    key: Pubkey,
    mv: Option<&MarketView>,
    now: i64,
    every: i64,
    report: &TickReport,
    crank_queue: &mut Vec<(Pubkey, Due)>,
) {
    let Some(mv) = mv else { return };
    let m = &mv.market;
    if crank_queue.iter().any(|(k, _)| *k == key) || report.observed(&key) {
        return;
    }
    if m.pool_fail_since == 0 || now.saturating_sub(m.last_pool_fail_obs) >= every {
        crank_queue.push((key, Due::NoRoute));
    }
}

/// What one tick did (for tests, logs and metrics).
#[derive(Clone, Debug, Default)]
pub struct TickReport {
    pub clock: Option<ChainClock>,
    pub exits: Vec<ExitRecord>,
    /// Markets whose `crank_price` landed.
    pub cranked: Vec<Pubkey>,
    /// Markets whose crank failed (alone), with why.
    pub crank_failed: Vec<(Pubkey, String)>,
    pub batches: Vec<BatchRecord>,
    /// Markets observed this tick (by a crank or an exit) and why an observation was due
    /// (`None`: an exit observed a market that needed none).
    pub observed: Vec<(Pubkey, Option<Due>)>,
    /// Markets whose observation was due, with why.
    pub due: Vec<(Pubkey, Due)>,
    pub alerts: Vec<Alert>,
    pub evaluations: BTreeMap<Pubkey, Vec<PositionEval>>,
    pub needs: BTreeMap<Pubkey, MarketNeeds>,
    /// Foreign deviating price observations detected (per-slot bursts, duty 2): (market,
    /// second).
    pub bursts: Vec<(Pubkey, i64)>,
    /// Foreign deviating bucket liquidity detected on markets with open positions (per-second
    /// coverage started or extended, FITS-2 (b)): (market, second).
    pub liq_covers: Vec<(Pubkey, i64)>,
    /// `flag_venue_upgraded` calls that landed this tick: (venue, deploy slot) (duty 10).
    pub flagged: Vec<(u8, u64)>,
    /// AIR-01: watched ProgramData extensions first seen this tick — the loader's
    /// `ExtendProgram` moved the deploy slot, the ELF is the one the latch (or, for Token-2022,
    /// the keeper's last read) knows: (program, deploy slot). Neither flagged nor paged (duty 10).
    pub extended: Vec<(Pubkey, u64)>,
    /// Pool selection (duty 5's fast path, duty 12): the plan of every market whose candidates
    /// were evaluated this tick.
    pub pool_plans: BTreeMap<Pubkey, PoolPlan>,
    /// `propose_pool` / `execute_pool_switch` attempts.
    pub pool_actions: Vec<PoolActionRecord>,
    /// The pool events of the keeper's landed pool-selection transactions.
    pub pool_events: Vec<PoolEvent>,
    /// Duty 6: the keeper's `migrate_v2` attempts.
    pub migrations: Vec<MigrateRecord>,
    /// Duty 6 (M5): the trader accounts the keeper created on a curve market (after a `None →`
    /// pump re-detection), with the result.
    pub curve_accounts: Vec<(Pubkey, ExitResult)>,
    /// Duty 6 (M6): the pool accounts the keeper created on a PumpSwap market (ARCH §3.12's
    /// existence requirement), with the result.
    pub pamm_accounts: Vec<(Pubkey, ExitResult)>,
    /// Duty 6 (M6): the keeper's `→ PumpSwap` transitions.
    pub transitions: Vec<TransitionRecord>,
    /// pump-final §7: the curve exits whose TradeExtras format the keeper had to find.
    pub curve_formats: Vec<CurveFormatRecord>,
    /// Duties 6 and 13: the keeper's sweeps (post-wipe, trader side, cashback claims).
    pub sweeps: Vec<SweepRecord>,
    /// O17: the keeper's `set_coverage_pause` attempts (the designated pauser only).
    pub coverage_pauses: Vec<CoveragePauseRecord>,
    /// Duty 11: the keeper's `crank_sol_usd` attempt this tick (the update's publish time).
    pub oracle: Option<(i64, ExitResult)>,
    /// T-CASCADE: per market, the compute units the keeper's landed exits requested this tick's
    /// slot (their `⌈simulated × 1.1⌉` limits, against `budget.market_cu_per_slot`).
    pub exit_cu: BTreeMap<Pubkey, u64>,
    pub skipped: Vec<(Pubkey, String)>,
    pub errors: Vec<String>,
}

impl TickReport {
    /// The exits of `kind` that landed.
    pub fn landed(&self, kind: ExitKind) -> impl Iterator<Item = &ExitRecord> {
        self.exits
            .iter()
            .filter(move |e| e.kind == kind && e.result.landed().is_some())
    }

    /// Whether `market` was observed this tick.
    pub fn observed(&self, market: &Pubkey) -> bool {
        self.observed.iter().any(|(m, _)| m == market)
    }
}

/// One `set_coverage_pause` attempt of the keeper (O17; the designated pauser).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoveragePauseRecord {
    pub market: Pubkey,
    pub paused: bool,
    pub result: ExitResult,
}

/// One `→ PumpSwap` transition of the keeper (duty 6, M6; [`crate::graduation::transition_ixs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionRecord {
    pub market: Pubkey,
    /// Pool accounts created in it.
    pub created: usize,
    /// The `VenueStateChanged` events of the transaction: `(from, to)` venue bytes.
    pub venue_changes: Vec<(u8, u8)>,
    pub result: ExitResult,
}

/// The `VenueStateChanged` events of `out`: `(from, to)`.
fn venue_changes(out: &TxOutcome) -> Vec<(u8, u8)> {
    events::of::<shortwin::events::VenueStateChanged>(out)
        .iter()
        .map(|e| (e.from, e.to))
        .collect()
}

/// Whether the landed exit `out` completed its market's curve: its `VenueStateChanged
/// PumpCurve → PumpMigrating` (pump-final §6.1.7: `sync_after_leg` after a buy of `rT` or more).
fn completes_curve(out: &TxOutcome) -> bool {
    venue_changes(out).contains(&(VenueKind::PumpCurve as u8, VenueKind::PumpMigrating as u8))
}

/// What a send attempt came to.
enum Sent {
    Landed {
        out: TxOutcome,
        format: TxFormat,
        bytes: usize,
        accounts: usize,
        price: u64,
        unit_limit: u32,
    },
    SimFailed {
        out: TxOutcome,
        format: TxFormat,
    },
    Failed {
        out: TxOutcome,
    },
    DryRun {
        out: TxOutcome,
    },
    TooLarge,
    /// Simulated; its limit exceeds the room left (`send_ixs_within`). Not sent.
    Deferred {
        unit_limit: u32,
    },
    /// AIR-18: refused in simulation or failed on chain because a program of the transaction
    /// could not run this slot ([`events::not_deployed`]) — `program`, the one the logs name,
    /// deployed in it ([`Keeper::deployed_this_slot`]).
    NotDeployed {
        out: TxOutcome,
        program: Pubkey,
    },
}

/// AIR-18: a refusal more than this many seconds after the program's last one starts a new run
/// ([`NotDeployedRun`]).
pub const NOT_DEPLOYED_RUN_GAP_S: i64 = 5;

/// AIR-18: a run of transient refusals of one program that has lasted this many seconds is paged
/// ([`Alert::ProgramNotDeployed`]), once per run.
pub const NOT_DEPLOYED_ALERT_S: i64 = 30;

/// AIR-18: a run of transient "Program is not deployed" refusals of one program (the program
/// deployed, upgraded or extended in each refused slot): refusals at most
/// [`NOT_DEPLOYED_RUN_GAP_S`] apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotDeployedRun {
    /// The second of the run's first refusal.
    pub since: i64,
    /// The second of its last refusal.
    pub last: i64,
    /// Refusals in the run.
    pub refusals: u64,
    /// Paged ([`Alert::ProgramNotDeployed`]).
    pub alerted: bool,
}

impl NotDeployedRun {
    /// Books a refusal at `now`; `true` when the run has now lasted [`NOT_DEPLOYED_ALERT_S`] and
    /// was not paged yet (the caller pages it).
    pub fn note(&mut self, now: i64) -> bool {
        if self.refusals == 0
            || now < self.last
            || now.saturating_sub(self.last) > NOT_DEPLOYED_RUN_GAP_S
        {
            *self = Self {
                since: now,
                ..Self::default()
            };
        }
        self.last = now;
        self.refusals = self.refusals.saturating_add(1);
        let due = !self.alerted && now.saturating_sub(self.since) >= NOT_DEPLOYED_ALERT_S;
        self.alerted |= due;
        due
    }
}

/// What became of a `flag_venue_upgraded` attempt ([`Keeper::send_flag`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum FlagSent {
    /// It landed: the latch holds the deploy.
    Landed,
    /// Simulated only (`dry_run`): the program would flag it.
    DryRun,
    /// The program answered `VenueNotUpgraded` in simulation, on the bank of slot `at` (the
    /// simulation's context slot; 0: unknown): the latch already holds this deploy slot
    /// (someone flagged it first), or — a moved slot the keeper leaves to the program (AIR-01) —
    /// the loader's `ExtendProgram`. Only a bank at or past the deploy slot the header showed
    /// holds that deploy: an earlier one (a node of an RPC pool behind the header read) still
    /// holds the deploy before it, and its answer is not this deploy's judgment.
    NotUpgraded { at: u64 },
    /// Attempted for this deploy already this second.
    Throttled,
    /// AIR-18: refused because short.win was deployed, upgraded or extended in the slot
    /// ([`Sent::NotDeployed`]): nothing judged, nothing alerted; the next tick (the next slot)
    /// tries again.
    NotDeployed,
    /// Not sent, or refused otherwise (the detail).
    Failed(String),
}

/// AIR-01: what a moved ProgramData deploy slot needs from the whole account, judged after the
/// tick's exits and cranks ([`Keeper::inspect_programs`]): nothing before the exits waits on it —
/// a review blocks opens already, Token-2022 is alert only, and a record or the keeper's own
/// judgment follows a flag already attempted this tick.
#[derive(Clone, Debug)]
enum Inspection {
    /// Latch venue `venue`, its bit clear and a fingerprint recorded, whose flag's simulation
    /// could not judge the moved slot: the keeper's own judgment, by the program's rule on the
    /// whole account. `failure`: why the flag did not go out, alerted with an upgrade
    /// (`VenueFlagFailed`) — a refusal, or an answer from a bank before the deploy; `None` when
    /// short.win itself was not deployed in that slot (AIR-18: the upgrade is paged, and the
    /// flag goes out in a later slot without a failure alert).
    Unjudged { venue: u8, failure: Option<String> },
    /// Latch venue `venue` during a review, moved past the deploy the keeper recorded: another
    /// upgrade, or an extension of the recorded deploy ([`Keeper::judge_review`]).
    Review(u8),
    /// Latch venue `venue`: the record of the deploy at the header's slot — the one the keeper
    /// reported as an upgrade, or found under review unrecorded (a restart) — for its review.
    Record(u8),
    /// Token-2022 (alert only): moved from deploy `previous`, last read whole with `before`.
    Alert {
        previous: u64,
        before: monitor::ElfFacts,
    },
    /// Token-2022 not read whole yet: the deploy its next change is judged against.
    Baseline,
}

/// AIR-01: the moved deploy slots [`Keeper::watch_programs`] leaves to the whole account
/// (program, header deploy slot, what is needed), with the latch as it read it.
struct Inspections {
    guard: VenueGuard,
    items: Vec<(monitor::WatchedProgram, u64, Inspection)>,
}

/// Duty 10's pump-curve findings (pump-final §7; [`monitor::CurveFindings`]), as alert keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CurveFinding {
    QuoteOffset,
    LivePostComplete,
    MigratingFvUndefined,
}

/// Custom errors that are the program's normal "not now" answers (no alert).
fn benign(code: u32) -> bool {
    [
        ShortwinError::ExitBoundNotMet,
        ShortwinError::ReferenceStale,
        ShortwinError::NotLiquidatable,
        ShortwinError::UseSolSettle,
        ShortwinError::UseExpire,
        ShortwinError::NotExpired,
        ShortwinError::SettlementNotAllowed,
    ]
    .into_iter()
    .any(|e| u32::from(e) == code)
}

/// pump-final §15 step 1, the upgrade window: shortwin 1.0.0 — a curve exit carrying its list
/// (`format` V2) — refuses a liquidation or expiry whose `Q` exceeds the live curve's `rT` with
/// `CurveStraddle` (its exits never buy past `rT`; the bundle that did is retired). Not alerted:
/// the position goes to `sol_settle` — S2 when its legs hold, else a no-op crank, as 1.0.0's keeper
/// sent it while its bundle did not go out — and waits for the upgrade, whose v3 exit buys past
/// `rT`. The v3 build refuses `CurveStraddle` only on a borrower's close.
fn waits_for_upgrade(code: Option<u32>, format: Option<CurveExtrasFormat>) -> bool {
    format == Some(CurveExtrasFormat::V2) && is(code, ShortwinError::CurveStraddle)
}

/// An exit's refusal or on-chain failure as an alert's detail (`ExitRefused`,
/// [`Alert::CurveExtrasFormatUnknown`]): its error and the program's own error line
/// (`AnchorError thrown in <source>:<line>. Error Code: …`), if any. On a curve exit's
/// account-parsing refusal the source line tells the TradeExtras parser from the ReadSet's
/// (`check_read`) or the VenueExtras' (`parse_extras`), which raise the same codes.
fn refusal_brief(r: &ExitResult) -> String {
    match r {
        ExitResult::Refused { outcome, .. } | ExitResult::Failed { outcome } => {
            match outcome
                .logs
                .iter()
                .rev()
                .find(|l| l.contains("Error Code:"))
            {
                Some(l) => format!(
                    "{:?} ({})",
                    outcome.err,
                    l.strip_prefix("Program log: ").unwrap_or(l)
                ),
                None => format!("{:?}", outcome.err),
            }
        }
        other => format!("{other:?}"),
    }
}

/// pump-final §7: the key of a raised [`Alert::CurveExtrasFormatUnknown`] — market, position,
/// short.win's deploy slot, and each list's refusal code.
type FormatAlertKey = (
    Pubkey,
    Pubkey,
    Option<u64>,
    Vec<(CurveExtrasFormat, Option<u32>)>,
);

/// U28: `⌊B̂_live · 10⁴ / B̂_ref⌋` of an evaluation, when both are defined.
fn b_ratio_bps(e: &PositionEval) -> Option<u64> {
    let (live, r) = (e.b_live?, e.b_ref?);
    (r > 0).then(|| u64::try_from(u128::from(live) * 10_000 / u128::from(r)).unwrap_or(u64::MAX))
}

fn is(code: Option<u32>, e: ShortwinError) -> bool {
    code == Some(u32::from(e))
}

/// The keeper (module docs).
pub struct Keeper<C: Chain> {
    chain: C,
    payer: Keypair,
    cfg: KeeperConfig,
    selection: Option<Vec<Pubkey>>,
    markets: Vec<Pubkey>,
    last_discovery: Option<i64>,
    tracks: BTreeMap<Pubkey, MarketTrack>,
    v1: V1Support,
    lookup_tables: Vec<AddressLookupTableAccount>,
    lookup_loaded: bool,
    alerts: Vec<Alert>,
    /// Duty 10: each covered market's pool as last seen.
    pools: BTreeMap<Pubkey, PoolWatch>,
    /// Duty 10: each watched ProgramData's deploy slot as last seen (`None`: unreadable).
    program_slots: BTreeMap<Pubkey, Option<u64>>,
    /// Upgrades already alerted (venue, deploy slot), and flag failures already alerted.
    upgrades_alerted: HashSet<(u8, u64)>,
    flag_failures_alerted: HashSet<(u8, u64)>,
    /// The second of the last `flag_venue_upgraded` attempt per (venue, deploy slot): at most
    /// one a second for the same upgrade; a new deploy slot is attempted at once.
    flag_tried: BTreeMap<(u8, u64), i64>,
    /// AIR-01: per watched ProgramData address, the deploy slot and [`monitor::ElfFacts`] of the
    /// account as the keeper last read it whole ([`Self::read_whole`]); for Token-2022, the deploy
    /// its next change is judged against.
    program_elf: BTreeMap<Pubkey, (u64, monitor::ElfFacts)>,
    /// AIR-01: per venue whose latch bit is set, the `(len, fingerprint)` record of the deploy the
    /// keeper reported as an upgrade (flag or refresh) or found flagged: an extension of it during
    /// the review is not another upgrade.
    flagged_elf: BTreeMap<u8, (u32, [u8; shortwin::state::GUARD_ELF_LEN])>,
    /// AIR-01: per watched program, the deploy slot of the last extension reported (once per
    /// slot; a latch venue's slot judged an extension is not simulated again).
    extension_seen: BTreeMap<Pubkey, u64>,
    /// AIR-01: Token-2022 changes already alerted (program, deploy slot).
    programs_alerted: HashSet<(Pubkey, u64)>,
    /// Duties 5 and 12: each covered Raydium or list-only market's pool-selection state.
    pool_tracks: BTreeMap<Pubkey, PoolTrack>,
    /// The chain the pool discovery's `getProgramAccounts` calls go to (`None`: `chain`).
    discovery: Option<C>,
    /// Duty 6: the second of the last `migrate_v2` attempt per market (one a second).
    migrate_tried: BTreeMap<Pubkey, i64>,
    /// pump-final §7: the curve TradeExtras format the deployed short.win parses, with the
    /// ProgramData deploy slot it was found at (`None`: unreadable then).
    curve_format: Option<(Option<u64>, CurveExtrasFormat)>,
    /// short.win's ProgramData deploy slot as read in a tick: (the tick's slot, the deploy slot).
    shortwin_deploy: Option<(u64, Option<u64>)>,
    /// pump-final §7: the [`Alert::CurveExtrasFormatUnknown`] already raised ([`FormatAlertKey`]);
    /// a market's entries are cleared once a curve exit on it gets past account parsing.
    format_alerted: HashSet<FormatAlertKey>,
    /// Duty 10 (pump-final §7): the curve findings already alerted, per (market, finding).
    curve_alerted: HashSet<(Pubkey, CurveFinding)>,
    /// Duty 6: graduation alerts already raised, per (market or position, detail).
    graduation_alerted: HashSet<(Pubkey, String)>,
    /// Duty 6 (M5): curve markets whose trader accounts (`trader_base_ata`, the pump UVA) the
    /// keeper found present; the second of the last attempt to create them per market.
    curve_ready: HashSet<Pubkey>,
    curve_prepare_tried: BTreeMap<Pubkey, i64>,
    /// Duty 6 (M6): per PumpSwap market, the pool accounts found present (re-checked when the list
    /// changes: a coin-creator change); the second of the last attempt to create them.
    pamm_ready: BTreeMap<Pubkey, Vec<Pubkey>>,
    pamm_prepare_tried: BTreeMap<Pubkey, i64>,
    /// Duty 6 (M6): the second of the last transition attempt per market.
    transition_tried: BTreeMap<Pubkey, i64>,
    /// Duties 6 and 13: the second of the last trader-side read, post-wipe attempt and cashback
    /// claim per market.
    sweep_checked: BTreeMap<Pubkey, i64>,
    post_wipe_tried: BTreeMap<Pubkey, i64>,
    cashback_claimed: BTreeMap<Pubkey, i64>,
    /// O17: the `coverage_authority` key of the designated pauser (`[coverage_pause]`).
    coverage_authority: Option<Arc<Keypair>>,
    /// O17: the second of the last `set_coverage_pause` attempt per market (one a second).
    pause_tried: BTreeMap<Pubkey, i64>,
    /// O17: the flag value the keeper is bringing a market to (until the flag matches).
    pause_intent: BTreeMap<Pubkey, bool>,
    /// O17: coverage-pause alerts already raised, per (market, detail); a market's entries are
    /// cleared once its flag matches its coverage state.
    pause_alerted: HashSet<(Pubkey, String)>,
    /// T-CASCADE: (slot, compute units requested) of the keeper's exits per market.
    slot_cu: BTreeMap<Pubkey, (u64, u64)>,
    /// Duty 11: the second of the next read of the Pyth feed; the oracle alerts raised (cleared
    /// when a crank lands); the last aging alert: (the cache's `publish_time` it was raised for,
    /// the second it was raised) — one per stall (M7D-6), repeated hourly within one.
    oracle_next: i64,
    oracle_alerted: HashSet<String>,
    oracle_aging_alerted: Option<(i64, i64)>,
    /// The slot of the tick in progress.
    tick_slot: u64,
    /// AIR-18: this tick's ProgramData header reads of the programs a refusal named
    /// ([`Self::deployed_this_slot`]): the slot each was read as of, and the deploy slot it showed
    /// (`None`: no ProgramData). A read answers the program's refusals at or before that slot.
    deploy_slots: BTreeMap<Pubkey, (u64, Option<u64>)>,
    /// AIR-18: each program's run of transient "not deployed" refusals.
    not_deployed_runs: BTreeMap<Pubkey, NotDeployedRun>,
    /// Decision A (2026-10-01; ARCH §7.1 duty 8): the global keeper budget ([`crate::budget`]).
    budget: GlobalBudget,
    /// The charge class of each market's crank queued this tick ([`crate::budget::ChargeReq`]):
    /// exit-critical — the cranks exits depend on: a position near a threshold (duty 1: live CR
    /// below `near_cr_bps`·L, deadline within the window, armed, the post-change warm-up; without
    /// it `P_ema` stays where it was and no liquidation becomes possible), a liquidatable or
    /// expired position waiting for a fresh reference or its bound, the S3 chain, a market
    /// without a route: charged, never throttled ("exits continue") —, baseline or discretionary.
    crank_charges: BTreeMap<Pubkey, ChargeReq>,
    /// Markets whose crank this tick a graduation step forced (after the keeper's `migrate_v2`, a
    /// list-only market's re-detection): a graduation move, neither charged nor
    /// throttled.
    free_cranks: HashSet<Pubkey>,
    /// Markets whose crank the budget refused this tick.
    refused: HashSet<Pubkey>,
    /// Decision A: the second at which each below-floor market (tier 2) is read next — when its
    /// idle crank is due (not every tick: M7-SEC-8).
    idle_next: BTreeMap<Pubkey, i64>,
    /// M7-SEC-8: markets classified cold at their last full read (no open positions, nothing
    /// time-critical pending: read every `coverage.cold_interval_s`, staggered), and the second
    /// of each market's last full read.
    cold: HashSet<Pubkey>,
    last_read: BTreeMap<Pubkey, i64>,
    /// M7-SEC-3: positions already alerted as opened while the keeper held their market degraded.
    open_alerted: HashSet<Pubkey>,
    /// M7-SEC-8: this tick's trader-side facts of the markets whose periodic sweep check is due,
    /// read in one call before the markets ([`Self::prefetch_sweeps`]).
    sweep_prefetch: BTreeMap<Pubkey, Vec<Option<AccountData>>>,
    /// Counters (duty 8).
    pub metrics: Metrics,
}

impl<C: Chain> Keeper<C> {
    /// A keeper on `chain` paying with `payer` (a hot key with no protocol authority).
    pub fn new(chain: C, payer: Keypair, cfg: KeeperConfig) -> anyhow::Result<Self> {
        cfg.validate()?;
        Ok(Self {
            selection: cfg.markets.keys()?,
            chain,
            payer,
            cfg,
            markets: Vec::new(),
            last_discovery: None,
            tracks: BTreeMap::new(),
            v1: V1Support::Unknown,
            lookup_tables: Vec::new(),
            lookup_loaded: false,
            alerts: Vec::new(),
            pools: BTreeMap::new(),
            program_slots: BTreeMap::new(),
            upgrades_alerted: HashSet::new(),
            flag_failures_alerted: HashSet::new(),
            flag_tried: BTreeMap::new(),
            program_elf: BTreeMap::new(),
            flagged_elf: BTreeMap::new(),
            extension_seen: BTreeMap::new(),
            programs_alerted: HashSet::new(),
            pool_tracks: BTreeMap::new(),
            discovery: None,
            migrate_tried: BTreeMap::new(),
            curve_format: None,
            shortwin_deploy: None,
            format_alerted: HashSet::new(),
            curve_alerted: HashSet::new(),
            graduation_alerted: HashSet::new(),
            curve_ready: HashSet::new(),
            curve_prepare_tried: BTreeMap::new(),
            pamm_ready: BTreeMap::new(),
            pamm_prepare_tried: BTreeMap::new(),
            transition_tried: BTreeMap::new(),
            sweep_checked: BTreeMap::new(),
            post_wipe_tried: BTreeMap::new(),
            cashback_claimed: BTreeMap::new(),
            coverage_authority: None,
            pause_tried: BTreeMap::new(),
            pause_intent: BTreeMap::new(),
            pause_alerted: HashSet::new(),
            slot_cu: BTreeMap::new(),
            oracle_next: 0,
            oracle_alerted: HashSet::new(),
            oracle_aging_alerted: None,
            tick_slot: 0,
            deploy_slots: BTreeMap::new(),
            not_deployed_runs: BTreeMap::new(),
            budget: GlobalBudget::default(),
            crank_charges: BTreeMap::new(),
            free_cranks: HashSet::new(),
            refused: HashSet::new(),
            idle_next: BTreeMap::new(),
            cold: HashSet::new(),
            last_read: BTreeMap::new(),
            open_alerted: HashSet::new(),
            sweep_prefetch: BTreeMap::new(),
            metrics: Metrics::default(),
        })
    }

    /// O17: this instance holds the `coverage_authority` key `kp` (a separate key from its fee
    /// payer). With `coverage_pause.enabled` it pauses opens on a degraded market with open
    /// positions and resumes them after `recovery_s` of restored coverage
    /// ([`crate::config::CoveragePauseConfig`]).
    pub fn with_coverage_authority(mut self, kp: Keypair) -> Self {
        self.coverage_authority = Some(Arc::new(kp));
        self
    }

    /// Replaces (or, with `None`, drops) the `coverage_authority` key between ticks (a key
    /// rotation; [`Self::with_coverage_authority`]).
    pub fn set_coverage_authority(&mut self, kp: Option<Keypair>) {
        self.coverage_authority = kp.map(Arc::new);
    }

    /// The `coverage_authority` key this instance holds, if any.
    pub fn coverage_authority(&self) -> Option<Pubkey> {
        self.coverage_authority.as_ref().map(|k| k.pubkey())
    }

    /// Sends the pool discovery's `getProgramAccounts` calls to `chain` instead of the
    /// keeper's own (`pool_select.discovery_rpc`, `--discovery-rpc`).
    pub fn with_discovery_chain(mut self, chain: C) -> Self {
        self.discovery = Some(chain);
        self
    }

    /// The chain the pool discovery uses.
    fn discovery_chain(&self) -> &C {
        self.discovery.as_ref().unwrap_or(&self.chain)
    }

    /// A market's pool-selection state (duties 5 and 12).
    pub fn pool_track(&self, market: &Pubkey) -> Option<&PoolTrack> {
        self.pool_tracks.get(market)
    }

    /// The chain backend.
    pub fn chain(&self) -> &C {
        &self.chain
    }

    /// The fee payer's key.
    pub fn payer(&self) -> Pubkey {
        self.payer.pubkey()
    }

    /// The configuration.
    pub fn config(&self) -> &KeeperConfig {
        &self.cfg
    }

    /// Mutable configuration (budgets, ceilings) between ticks.
    pub fn config_mut(&mut self) -> &mut KeeperConfig {
        &mut self.cfg
    }

    /// Whether v1 transactions are accepted, as far as the keeper knows.
    pub fn v1_support(&self) -> V1Support {
        self.v1
    }

    /// A market's coverage state.
    pub fn track(&self, market: &Pubkey) -> Option<&MarketTrack> {
        self.tracks.get(market)
    }

    /// pump-final §7: the curve TradeExtras format the keeper knows the deployed short.win
    /// parses, and short.win's ProgramData deploy slot it was found at (`None`: none known yet).
    pub fn curve_format(&self) -> Option<(Option<u64>, CurveExtrasFormat)> {
        self.curve_format
    }

    /// The global keeper budget (decision A of 2026-10-01; [`crate::budget`]).
    pub fn budget(&self) -> &GlobalBudget {
        &self.budget
    }

    /// The budget's parameters this tick: `[budget]`, the idle cadence, the value-at-risk floor
    /// clamped to the deployment's `market_max_notional` (its bound), and (M7D-5) the per-crank
    /// charge estimated before the keeper has measured any: one signature shared by a full batch
    /// of the transaction format in use, `⌈5,000 / batch⌉` (834 at `batch_v1` = 6).
    fn budget_params(&self, config: &shortwin::state::GlobalConfig) -> BudgetParams {
        let b = &self.cfg.budget;
        let batch = match self.cfg.tx_format {
            TxFormatPolicy::V0 => self.cfg.batch_v0,
            _ => self.cfg.batch_v1,
        };
        let batch = u64::try_from(batch.max(1)).unwrap_or(1);
        BudgetParams {
            global: b.global_daily_lamports,
            var_floor: b.var_floor_lamports.min(config.params.market_max_notional),
            reserve_margin_bps: b.reserve_margin_bps,
            tail_share_bps: b.tail_share_bps,
            idle_interval_s: self.cfg.coverage.idle_interval_s,
            first_charge: budget::FIRST_CHARGE_LAMPORTS.div_ceil(batch),
        }
    }

    /// The covered markets as of the last discovery.
    pub fn markets(&self) -> &[Pubkey] {
        &self.markets
    }

    /// The lookup tables v0 transactions use (loaded at the first tick).
    pub fn lookup_tables(&self) -> &[AddressLookupTableAccount] {
        &self.lookup_tables
    }

    fn format(&self, now: i64) -> TxFormat {
        match self.cfg.tx_format {
            TxFormatPolicy::V1 => TxFormat::V1,
            TxFormatPolicy::V0 => TxFormat::V0,
            TxFormatPolicy::Auto => match self.v1 {
                V1Support::No { until } if now < until => TxFormat::V0,
                _ => TxFormat::V1,
            },
        }
    }

    /// A v1 refusal under the `auto` policy switches to v0 (returns `true`: retry).
    fn on_rejected(&mut self, format: TxFormat, reason: &str, now: i64) -> bool {
        if format != TxFormat::V1 || self.cfg.tx_format != TxFormatPolicy::Auto {
            return false;
        }
        self.v1 = V1Support::No {
            until: now.saturating_add(self.cfg.v1_reprobe_s),
        };
        self.metrics.v1_rejections += 1;
        self.alerts.push(Alert::V1Unsupported {
            reason: reason.to_string(),
        });
        true
    }

    async fn load_lookup_tables(&mut self) -> anyhow::Result<()> {
        if self.lookup_loaded {
            return Ok(());
        }
        let keys = self.cfg.lookup_table_keys()?;
        if !keys.is_empty() {
            let accs = self.chain.accounts(&keys).await?;
            self.lookup_tables = keys
                .iter()
                .zip(accs)
                .map(|(k, a)| {
                    let a = a.ok_or_else(|| anyhow!("lookup table {k} not found"))?;
                    tx::parse_lookup_table(*k, &a.owner, &a.data)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            // A28: the protocol table should hold every static key (`shortwin-build-alt`); a
            // missing one only costs bytes, so this is a warning, not an error.
            let have: HashSet<Pubkey> = self
                .lookup_tables
                .iter()
                .flat_map(|t| t.addresses.iter().copied())
                .collect();
            let missing: Vec<String> = crate::alt::static_entries()
                .into_iter()
                .filter(|e| !have.contains(&e.key))
                .map(|e| format!("{} ({})", e.key, e.label))
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    missing = ?missing,
                    "the lookup tables lack protocol ALT keys (scripts/build-alt.sh extend)"
                );
            }
        }
        self.lookup_loaded = true;
        Ok(())
    }

    async fn priority_price(&self, markets: &[Pubkey], exit: bool) -> u64 {
        let b = &self.cfg.budget;
        let recent = self
            .chain
            .recent_priority_fees(markets)
            .await
            .unwrap_or_default();
        let misses = markets
            .iter()
            .filter_map(|m| self.tracks.get(m))
            .map(|t| t.miss_streak)
            .max()
            .unwrap_or(0);
        let ceiling = if exit {
            b.exit_priority_ceiling
        } else {
            b.priority_ceiling
        };
        fees::priority_price(
            &recent,
            b.priority_percentile_bps,
            b.priority_floor,
            ceiling,
            b.escalate_bps,
            misses,
        )
    }

    /// Simulates `ixs` (compute ceiling, no priority price) in the current format, switching to
    /// v0 once on a v1 refusal.
    ///
    /// The simulation carries the priority price the send will use: a v0 transaction then has
    /// the same ComputeBudget instructions (each consumes compute units) as the one sent with
    /// `⌈units × 1.1⌉`.
    async fn simulate(
        &mut self,
        ixs: &[Instruction],
        now: i64,
        price: u64,
    ) -> anyhow::Result<(TxOutcome, TxFormat, solana_hash::Hash)> {
        self.simulate_signed(ixs, &[], now, price).await
    }

    /// [`Self::simulate`] with `signers` besides the fee payer (the `coverage_authority`).
    async fn simulate_signed(
        &mut self,
        ixs: &[Instruction],
        signers: &[&Keypair],
        now: i64,
        price: u64,
    ) -> anyhow::Result<(TxOutcome, TxFormat, solana_hash::Hash)> {
        for _ in 0..2 {
            let format = self.format(now);
            let alts = if format == TxFormat::V0 {
                self.lookup_tables.clone()
            } else {
                Vec::new()
            };
            let bh = self.chain.latest_blockhash().await?;
            let budget = ComputeBudget {
                unit_price: price,
                ..ComputeBudget::SIMULATION
            };
            let t = tx::build(format, &self.payer, signers, ixs, &budget, bh, &alts)?;
            if !tx::fits(format, &t) {
                return Ok((
                    TxOutcome {
                        err: Some(TxError::Rejected(format!(
                            "too large: {} B, {} accounts",
                            tx::size(&t),
                            tx::account_count(&t)
                        ))),
                        ..TxOutcome::default()
                    },
                    format,
                    bh,
                ));
            }
            self.metrics.simulations += 1;
            let sim = self.chain.simulate(&t).await?;
            if let Some(TxError::Rejected(r)) = &sim.err
                && self.on_rejected(format, r, now)
            {
                continue;
            }
            if sim.err.is_some() {
                self.metrics.simulation_failures += 1;
            }
            return Ok((sim, format, bh));
        }
        Err(anyhow!(
            "the cluster refused the transaction in both formats"
        ))
    }

    /// Simulates `ixs`, then sends them with the compute budget of the simulation
    /// (`⌈units × 1.1⌉`) and the priority price for `markets`.
    async fn send_ixs(
        &mut self,
        ixs: &[Instruction],
        markets: &[Pubkey],
        exit: bool,
        now: i64,
    ) -> anyhow::Result<Sent> {
        self.send_ixs_within(ixs, &[], markets, exit, now, None, None)
            .await
    }

    /// [`Self::send_ixs`] with `signers` besides the fee payer, and — with `room` — not sent
    /// ([`Sent::Deferred`]) when its compute-unit limit `⌈simulated × 1.1⌉` exceeds `room`
    /// (T-CASCADE's per-market, per-slot cap); with `sim_floor`, not sent (an error) when the
    /// simulation ran on a bank before that slot ([`Self::send_exit_floored`]).
    #[allow(clippy::too_many_arguments)]
    async fn send_ixs_within(
        &mut self,
        ixs: &[Instruction],
        signers: &[&Keypair],
        markets: &[Pubkey],
        exit: bool,
        now: i64,
        room: Option<u64>,
        sim_floor: Option<u64>,
    ) -> anyhow::Result<Sent> {
        for _ in 0..2 {
            let price = self.priority_price(markets, exit).await;
            let (sim, format, bh) = self.simulate_signed(ixs, signers, now, price).await?;
            if let Some(TxError::Rejected(r)) = &sim.err
                && r.starts_with("too large")
            {
                return Ok(Sent::TooLarge);
            }
            // A context slot of 0 is unknown (a backend that does not say): taken as current.
            if let Some(floor) = sim_floor
                && sim.slot != 0
                && sim.slot < floor
            {
                return Err(anyhow!(
                    "simulated on slot {}, before short.win's deploy slot {floor} (a node behind \
                     the keeper's header read): not sent",
                    sim.slot
                ));
            }
            if sim.err.is_some() {
                if let Some(program) = self.deployed_this_slot(&sim, now).await {
                    return Ok(Sent::NotDeployed { out: sim, program });
                }
                return Ok(Sent::SimFailed { out: sim, format });
            }
            if self.cfg.dry_run {
                return Ok(Sent::DryRun { out: sim });
            }
            let budget = ComputeBudget {
                unit_limit: tx::cu_limit(sim.units_consumed),
                unit_price: price,
            };
            if let Some(room) = room
                && u64::from(budget.unit_limit) > room
            {
                return Ok(Sent::Deferred {
                    unit_limit: budget.unit_limit,
                });
            }
            let alts = if format == TxFormat::V0 {
                self.lookup_tables.clone()
            } else {
                Vec::new()
            };
            let t = tx::build(format, &self.payer, signers, ixs, &budget, bh, &alts)?;
            let (bytes, accounts) = (tx::size(&t), tx::account_count(&t));
            let out = self.chain.send(&t).await?;
            match &out.err {
                Some(TxError::Rejected(r)) => {
                    if self.on_rejected(format, &r.clone(), now) {
                        continue;
                    }
                    self.metrics.tx_failed += 1;
                    return Ok(Sent::Failed { out });
                }
                Some(_) => {
                    self.metrics.tx_failed += 1;
                    if let Some(program) = self.deployed_this_slot(&out, now).await {
                        return Ok(Sent::NotDeployed { out, program });
                    }
                    return Ok(Sent::Failed { out });
                }
                None => {}
            }
            self.metrics.tx_landed += 1;
            if format == TxFormat::V1 {
                self.v1 = V1Support::Yes;
            }
            return Ok(Sent::Landed {
                out,
                format,
                bytes,
                accounts,
                price,
                unit_limit: budget.unit_limit,
            });
        }
        Err(anyhow!(
            "the cluster refused the transaction in both formats"
        ))
    }

    /// AIR-18: whether `out`, refused "Program is not deployed" ([`events::not_deployed`]), is the
    /// loader's one-slot refusal of a program deployed, upgraded or extended in the refused slot:
    /// the program the logs name ([`events::failing_program`]) shows that deploy in its
    /// ProgramData header ([`events::deployed_in_refused_slot`]). The refused slot is the
    /// outcome's own (a simulation's bank, a failed transaction's block; the tick's clock when
    /// the backend does not say), and the header is read as of that slot or later
    /// ([`Self::program_deploy_slot`]). One read answers the program's refusals at or before its
    /// slot for the rest of the tick: at most one read per program and refused slot. Then the
    /// refusal is booked — counted (`Metrics::not_deployed`), logged, its run tracked (a run
    /// lasting [`NOT_DEPLOYED_ALERT_S`] is paged once, [`Alert::ProgramNotDeployed`]) — and the
    /// program returned: no other alert, no back-off, the next tick (the next slot) sends again.
    /// `None` for any other failure, and for the loader's same answer about a program that does
    /// not come back by itself — closed, failing verification (an ELF a feature gate retired),
    /// not under the upgradeable loader, not named in the logs — or whose header cannot be read:
    /// the caller handles it as any refusal, alerted.
    async fn deployed_this_slot(&mut self, out: &TxOutcome, now: i64) -> Option<Pubkey> {
        if !events::not_deployed(out) {
            return None;
        }
        let slot = if out.slot > 0 {
            out.slot
        } else {
            self.tick_slot
        };
        let Some(program) = events::failing_program(out) else {
            tracing::warn!(
                slot,
                "program not deployed, none named (AIR-18): handled as a refusal"
            );
            return None;
        };
        let deploy = match self.deploy_slots.get(&program) {
            Some((read_at, deploy)) if *read_at >= slot => *deploy,
            _ => match self.program_deploy_slot(&program, slot).await {
                Some(deploy) => {
                    self.deploy_slots.insert(program, (slot, deploy));
                    deploy
                }
                None => None,
            },
        };
        if !events::deployed_in_refused_slot(deploy, slot) {
            tracing::warn!(
                %program,
                deploy_slot = ?deploy,
                slot,
                "program not deployed, and not deployed in this slot (AIR-18): handled as a \
                 refusal"
            );
            return None;
        }
        self.metrics.not_deployed += 1;
        tracing::info!(
            %program,
            deploy_slot = ?deploy,
            slot,
            "program deployed, upgraded or extended in this slot (AIR-18): retried in the next \
             slot"
        );
        let run = self.not_deployed_runs.entry(program).or_default();
        if run.note(now) {
            let (since, refusals) = (run.since, run.refusals);
            self.alerts.push(Alert::ProgramNotDeployed {
                program,
                since,
                refusals,
            });
        }
        Some(program)
    }

    /// AIR-18: the deploy slot `program`'s ProgramData header shows as of slot `slot` or later —
    /// `Some(None)`: no ProgramData (closed, or not under the upgradeable loader); `None`: not
    /// read. The read names `slot` as its minimum context slot: a node of an RPC pool that has not
    /// reached the refused slot refuses it rather than show the deploy before it, which would
    /// make a one-slot refusal look like one that stays; the read is tried twice (the pool's next
    /// node).
    async fn program_deploy_slot(&mut self, program: &Pubkey, slot: u64) -> Option<Option<u64>> {
        let pd = monitor::program_data_address(program);
        let mut failure = None;
        for _ in 0..2 {
            match self
                .chain
                .account_headers_at(&[pd], monitor::HEADER_LEN, slot)
                .await
            {
                Ok(v) => return Some(monitor::deploy_slot(v.first().and_then(Option::as_ref))),
                Err(e) => failure = Some(e),
            }
        }
        if let Some(e) = failure {
            tracing::warn!(%program, slot, "ProgramData header (AIR-18): {e:#}");
        }
        None
    }

    async fn discover(&mut self, now: i64) -> anyhow::Result<()> {
        let due = self.last_discovery.is_none_or(|t| {
            now.saturating_sub(t) >= self.cfg.coverage.discovery_interval_s || now < t
        });
        if !due {
            return Ok(());
        }
        let all = view::discover_markets(&self.chain).await?;
        self.markets = match &self.selection {
            None => all,
            Some(sel) => all.into_iter().filter(|k| sel.contains(k)).collect(),
        };
        self.last_discovery = Some(now);
        Ok(())
    }

    /// One tick (module docs).
    pub async fn tick(&mut self) -> anyhow::Result<TickReport> {
        self.load_lookup_tables().await?;
        let clock = self.chain.clock().await?;
        self.tick_slot = clock.slot;
        self.discover(clock.unix_timestamp).await?;
        // M7-SEC-8: the open positions first (one `getProgramAccounts`), then in full only the
        // markets with positions or something time-critical pending, and the cold ones on their
        // staggered poll.
        let positions = view::open_positions(&self.chain).await?;
        let read = self.markets_to_read(clock.unix_timestamp, &positions);
        let mut view = view::load_with_positions(&self.chain, &read, &positions).await?;
        let now = view.clock.unix_timestamp;
        self.tick_slot = view.clock.slot;
        self.deploy_slots.clear();
        self.crank_charges.clear();
        self.free_cranks.clear();
        self.refused.clear();
        // Decision A (2026-10-01; ARCH §7.1 duty 8): the global budget's UTC day, and the class
        // of every market read this tick (its value at risk from the `Market` as read).
        let params = self.budget_params(&view.config);
        self.budget.roll(now, &params);
        let known: HashSet<Pubkey> = self.markets.iter().copied().collect();
        let gone: Vec<Pubkey> = self
            .budget
            .markets
            .keys()
            .filter(|k| !known.contains(k))
            .copied()
            .collect();
        for k in gone {
            self.budget.forget(&k);
            self.idle_next.remove(&k);
        }
        for mv in &view.markets {
            let open = !mv.positions.is_empty() || mv.market.open_positions > 0;
            let var = budget::value_at_risk(&mv.market);
            self.budget
                .classify(now, mv.key, open, var, mv.market.created_at, &params);
        }
        let mut report = TickReport {
            clock: Some(view.clock),
            skipped: view.skipped.clone(),
            ..TickReport::default()
        };
        self.metrics.ticks += 1;
        // Close the previous seconds' coverage accounting before this second's work.
        for (key, track) in &mut self.tracks {
            if let Some(true) = track.close_second(*key, now, &self.cfg.coverage, &mut self.alerts)
            {
                self.metrics.market(*key).misses += 1;
            }
            // Seconds that required nothing are met too (a market whose positions closed while
            // it was degraded; the seconds between idle observations); seconds the keeper did not
            // run on a market with open positions are not.
            let open = view
                .market(key)
                .is_some_and(|mv| !mv.positions.is_empty() || mv.market.open_positions > 0);
            track.advance(*key, now, open, &self.cfg.coverage, &mut self.alerts);
        }

        // Duty 10: the latch first (an upgrade is flagged in the slot it is seen, before any exit
        // or crank; AIR-01: a moved slot the header cannot judge is judged by the flag's own
        // simulation), then the markets' pools. What needs a whole ProgramData read (AIR-01: a
        // review, a record, Token-2022) waits for the end of duties 1–4.
        let inspections = if self.cfg.venue_watch.flag_upgrades {
            self.watch_programs(now, &mut report).await
        } else {
            None
        };
        if self.cfg.venue_watch.pool_admin {
            self.watch_pools(&view);
        }
        // Duty 10 (pump-final §7): pump's post-complete fields on the curve markets.
        self.watch_curves(&view);
        // Duty 11: the SOL/USD cache the tiers read (before the markets: a crank this tick then
        // tiers with it).
        if self.cfg.oracle.enabled {
            self.oracle_duty(&view, &mut report).await;
        }

        self.prefetch_sweeps(&view).await;
        let mut crank_queue: Vec<(Pubkey, Due)> = Vec::new();
        for idx in 0..view.markets.len() {
            if let Err(e) = self
                .process_market(&mut view, idx, &mut report, &mut crank_queue)
                .await
            {
                report
                    .errors
                    .push(format!("{}: {e:#}", view.markets[idx].key));
            }
        }
        self.send_cranks(&view, crank_queue, &mut report, true)
            .await;
        // AIR-01: the whole ProgramData reads (up to 10 MiB each) after the exits and cranks.
        if let Some(inspections) = inspections {
            self.inspect_programs(inspections, now, &mut report).await;
        }
        // Pool selection after this second's observations and exits (M4-SEC-7): its RPC work —
        // discovery, ranking, the best candidates' accounts — never delays duties 1–4; the
        // cranks it asks for (a failing market's `pool_fail` chain) go out in a batch of their
        // own.
        if self.cfg.pool_select.enabled {
            let mut chain_cranks: Vec<(Pubkey, Due)> = Vec::new();
            self.select_pools(&mut view, &mut report, &mut chain_cranks)
                .await;
            if !chain_cranks.is_empty() {
                // Pool selection's chain cranks: neither charged nor throttled (ARCH §7.1 duty 8).
                self.send_cranks(&view, chain_cranks, &mut report, false)
                    .await;
            }
        }

        // O17: the designated pauser brings each market's `paused_coverage` flag in line with its
        // coverage state (after this second's observations, which may have ended a gap).
        self.coverage_pauses(&view, &mut report).await;

        for (key, track) in &self.tracks {
            self.metrics.market(*key).degraded = track.state.is_degraded();
        }
        self.schedule_idle_reads(&view, &report);
        self.budget_metrics(&view);
        self.classify_reads(&view, &report);
        for a in self.alerts.drain(..) {
            a.log();
            report.alerts.push(a);
        }
        Ok(report)
    }

    /// M7-SEC-8: the trader-side facts (`sweep::fact_keys`) of every market whose periodic sweep
    /// check is due this tick, in one read (one round trip; as built each market's check was a
    /// read of its own, so list-only junk markets added one sequential call a minute each). A
    /// post-wipe sweep, or a market an exit landed on this tick, reads its facts fresh.
    async fn prefetch_sweeps(&mut self, view: &ChainView) {
        self.sweep_prefetch.clear();
        let cfg = &self.cfg.sweep;
        if !cfg.sweep {
            return;
        }
        let now = view.clock.unix_timestamp;
        let due: Vec<&MarketView> =
            view.markets
                .iter()
                .filter(|mv| {
                    !(cfg.post_wipe && sweep::wipe_pending(&mv.market))
                        && self.sweep_checked.get(&mv.key).is_none_or(|t| {
                            now.saturating_sub(*t) >= cfg.check_interval_s || now < *t
                        })
                })
                .collect();
        if due.is_empty() {
            return;
        }
        let keys: Vec<Pubkey> = due
            .iter()
            .flat_map(|mv| sweep::fact_keys(&mv.market))
            .collect();
        let Ok(accs) = self.chain.accounts(&keys).await else {
            return;
        };
        for (mv, chunk) in due.iter().zip(accs.chunks(sweep::FACT_KEYS_LEN)) {
            self.sweep_prefetch.insert(mv.key, chunk.to_vec());
        }
    }

    /// M7-SEC-8: the covered markets this tick reads in full — every market with open positions
    /// (`positions`, read this tick), every market not classified cold at its last read (new
    /// markets included), and each cold market on its poll: every `cold_interval_s`, at a phase
    /// staggered by its key (and at once when a poll was skipped). Decision A (2026-10-01; ARCH
    /// §7.1 duty 8): a market with positions **below the value-at-risk floor** at its last read is
    /// read only when its idle crank is due ([`Self::schedule_idle_reads`]), in the tick its set
    /// of open positions changes (this tick's first read shows it), and while the keeper's own
    /// `set_coverage_pause` on it is pending — junk with positions does not grow the per-tick
    /// reads.
    fn markets_to_read(
        &mut self,
        now: i64,
        positions: &std::collections::HashMap<Pubkey, Vec<PositionView>>,
    ) -> Vec<Pubkey> {
        let every = self.cfg.coverage.cold_interval_s.max(1);
        let mut out = Vec::with_capacity(self.markets.len());
        let mut skipped = 0u64;
        let mut idle_skipped = 0u64;
        for k in &self.markets {
            if let Some(ps) = positions.get(k)
                && self.budget.tier(k) == Some(Tier::BelowFloor)
            {
                let changed = self
                    .tracks
                    .get(k)
                    .is_none_or(|t| t.positions_changed(ps.iter().map(|p| p.key)));
                let due = self.idle_next.get(k).is_none_or(|t| now >= *t);
                if changed || due || self.pause_intent.contains_key(k) {
                    out.push(*k);
                } else {
                    idle_skipped += 1;
                }
                continue;
            }
            let hot = positions.contains_key(k) || !self.cold.contains(k);
            let due = self.last_read.get(k).is_none_or(|t| {
                now < *t
                    || now.saturating_sub(*t) >= every
                    || (now > *t && now.saturating_add(cold_phase(k, every)).rem_euclid(every) == 0)
            });
            if hot || due {
                out.push(*k);
            } else {
                skipped += 1;
            }
        }
        self.metrics.markets_read = self
            .metrics
            .markets_read
            .saturating_add(u64::try_from(out.len()).unwrap_or(u64::MAX));
        self.metrics.markets_cold_skipped =
            self.metrics.markets_cold_skipped.saturating_add(skipped);
        self.metrics.markets_idle_skipped = self
            .metrics
            .markets_idle_skipped
            .saturating_add(idle_skipped);
        out
    }

    /// Decision A: after the tick, each below-floor market read gets the second of its next read
    /// — when its next idle crank is due (`idle_interval_s` after its last observation; a crank
    /// due and not landed is retried the next second), or `idle_interval_s` from now when the
    /// budget refused it (the tail spent).
    ///
    /// M7D-1: while a position is near an exit (near a threshold or its deadline, expired, armed,
    /// waiting only for a fresh reference, or with any plan but `Idle` / `Watch`), the market is
    /// read **every second the reference of the keeper's last observation is still fresh** (its
    /// positions' `ref_max_age_s`, 2 s by default): an exit needs a completed bucket at most that
    /// old, so the crank at `t` is followed by the read — and the exit the program accepts — at
    /// `t + 1` (and `t + 2`). Cranks stay at the idle cadence; a landed exit is an observation,
    /// so a cascade keeps going second by second. As reviewed, each read came 30 s after the
    /// last observation and so always found its own reference stale: no exit ever went.
    fn schedule_idle_reads(&mut self, view: &ChainView, report: &TickReport) {
        let now = view.clock.unix_timestamp;
        let idle = self.cfg.coverage.idle_interval_s.max(1);
        for mv in &view.markets {
            if self.budget.tier(&mv.key) != Some(Tier::BelowFloor) {
                self.idle_next.remove(&mv.key);
                continue;
            }
            let last = self.tracks.get(&mv.key).and_then(|t| t.last_obs);
            let next = if self.refused.contains(&mv.key) {
                now.saturating_add(idle)
            } else {
                let fresh_for = mv
                    .positions
                    .iter()
                    .map(|p| i64::from(p.pos.ref_max_age_s))
                    .max()
                    .unwrap_or(0)
                    .max(1);
                let near_exit = report.evaluations.get(&mv.key).is_some_and(|evs| {
                    evs.iter().any(|e| {
                        e.near
                            || e.expired
                            || e.armed
                            || e.stale_exit
                            || !matches!(e.plan, Plan::Idle | Plan::Watch)
                    })
                });
                match last {
                    Some((t, _)) if near_exit && now < t.saturating_add(fresh_for) => {
                        now.saturating_add(1)
                    }
                    Some((t, _)) => t.saturating_add(idle).max(now.saturating_add(1)),
                    None => now.saturating_add(1),
                }
            };
            self.idle_next.insert(mv.key, next);
        }
    }

    /// Decision A: the budget's per-market gauges of the markets read this tick, and its global
    /// balances.
    fn budget_metrics(&mut self, view: &ChainView) {
        let ranked = self.budget.ranked();
        for mv in &view.markets {
            let Some(b) = self.budget.markets.get(&mv.key) else {
                continue;
            };
            let rank = ranked
                .iter()
                .position(|k| *k == mv.key)
                .map_or(0, |i| i + 1);
            let m = self.metrics.market(mv.key);
            m.var_lamports = b.var;
            m.budget_tier = b.tier.number();
            m.budget_rank = u64::try_from(rank).unwrap_or(u64::MAX);
            m.budget_unspent_lamports = b.unspent;
            m.budget_spent_exit_lamports = b.spent[0];
            m.budget_spent_baseline_lamports = b.spent[1];
            m.budget_spent_discretionary_lamports = b.spent[2];
        }
        let g = &self.budget;
        self.metrics.budget_global_lamports = g.global;
        self.metrics.budget_free_lamports = g.free;
        self.metrics.budget_tail_lamports = g.tail;
        self.metrics.budget_overdraft_lamports = g.overdraft;
        self.metrics.budget_spent_lamports = g.spent;
        self.metrics.budget_excess_lamports = g.excess;
    }

    /// M7-SEC-8: after the tick, each market read is classified: **hot** (read every tick) while it
    /// is a PumpSwap or Raydium market, a curve near completion ([`Self::hot`]), or has open
    /// positions or anything time-critical pending — a per-second, no-route,
    /// listing-window or warm-up need, a guard, a burst or liquidity window, degraded mode or the
    /// coverage flag, a graduation step (a completed or migrating curve, a valid canonical pool,
    /// a re-detection), a post-wipe sweep, a pool-selection window, chain or pending ranking —,
    /// **cold** otherwise. A market whose needs this tick did not compute stays hot.
    fn classify_reads(&mut self, view: &ChainView, report: &TickReport) {
        let now = view.clock.unix_timestamp;
        for mv in &view.markets {
            self.last_read.insert(mv.key, now);
            if self.hot(mv, report.needs.get(&mv.key), now) {
                self.cold.remove(&mv.key);
            } else {
                self.cold.insert(mv.key);
            }
        }
        let known: HashSet<Pubkey> = self.markets.iter().copied().collect();
        self.cold.retain(|k| known.contains(k));
        self.last_read.retain(|k, _| known.contains(k));
    }

    /// Whether `mv` must be read every tick ([`Self::classify_reads`]). Only a list-only market,
    /// or a curve market that a buy of more than `coverage.hot_curve_completion_lamports` is still
    /// away from completing, can be cold — the markets anyone can list for a fee and rent
    /// (≈ 0.035–0.038 SOL at ARCH §3.2's rent — the M7 review counted ≈ 0.047; a pump coin
    /// ≈ 0.02 SOL more); a PumpSwap or Raydium market needs real liquidity to exist and stays hot,
    /// so its venue-drift alerts come at once.
    fn hot(&self, mv: &MarketView, needs: Option<&MarketNeeds>, now: i64) -> bool {
        let Some(n) = needs else { return true };
        let m = mv.market.as_ref();
        let key = mv.key;
        let cheap = match mv.kind {
            VenueKind::None => true,
            VenueKind::PumpCurve => graduation::completion_quote(mv)
                .is_some_and(|(_, cost)| cost > self.cfg.coverage.hot_curve_completion_lamports),
            _ => false,
        };
        if !cheap {
            return true;
        }
        let open = !mv.positions.is_empty() || m.open_positions > 0;
        let needs_now = n.per_second
            || n.no_route
            || n.start_chain
            || n.listing
            || n.warm_up
            || n.warming
            || n.guarded
            || n.s3_due_executable
            || n.liq_covered;
        let track = self.tracks.get(&key).is_some_and(|t| {
            t.in_burst(now)
                || t.liq_covered(now)
                || t.state.is_degraded()
                || t.flag_seen == Some(true)
        });
        let graduation = mv.kind == VenueKind::PumpMigrating
            || graduation::migration_due(mv)
            || graduation::transition_due(mv)
            || graduation::redetection_due(mv);
        let pool = m.fp_t0 != 0
            || m.pool_fail_since != 0
            || m.no_route_since != 0
            || self.pool_tracks.get(&key).is_some_and(|t| {
                t.pinned.is_some()
                    || !t.rank_queue.is_empty()
                    || t.keep_chain
                    || t.last_pool_fail == Some(true)
            });
        open || needs_now
            || track
            || self.pause_intent.contains_key(&key)
            || m.has_flag(MARKET_FLAG_PAUSED_COVERAGE)
            || graduation
            || sweep::wipe_pending(m)
            || pool
    }

    async fn process_market(
        &mut self,
        view: &mut ChainView,
        idx: usize,
        report: &mut TickReport,
        crank_queue: &mut Vec<(Pubkey, Due)>,
    ) -> anyhow::Result<()> {
        let clock = view.clock;
        let (now, slot) = (clock.unix_timestamp, clock.slot);
        let key = view.markets[idx].key;
        let cov = self.cfg.coverage.clone();
        {
            let mv = &view.markets[idx];
            let open = !mv.positions.is_empty() || mv.market.open_positions > 0;
            let track = self.tracks.entry(key).or_default();
            track.record_view(
                now,
                mv.state.price(),
                mv.state.liquidity(),
                (mv.market.venue, mv.market.pool_cache[0]),
            );
            let unpausing = self.pause_intent.get(&key) == Some(&false);
            let flag = mv.market.has_flag(MARKET_FLAG_PAUSED_COVERAGE);
            let covered = self.budget.tier(&key) == Some(Tier::Covered);
            let pauser = self.cfg.coverage_pause.enabled;
            // M7-SEC-3: while the keeper holds the market degraded, a flag found cleared that it did
            // not clear, and a position that appeared while the flag was last seen set, were
            // signed by the `coverage_authority` key outside this instance.
            if let Some(since) = track.state.degraded_since()
                && track.flag_seen == Some(true)
            {
                if !flag && !unpausing {
                    self.alerts
                        .push(Alert::CoveragePauseCleared { market: key });
                }
                for p in mv
                    .positions
                    .iter()
                    .filter(|p| !track.knows_position(&p.key))
                {
                    if self.open_alerted.insert(p.key) {
                        self.metrics.market(key).opens_while_degraded += 1;
                        self.alerts.push(Alert::OpenWhileDegraded {
                            market: key,
                            position: p.key,
                            degraded_since: since,
                        });
                    }
                }
                // ★OWNER-M7-3 (b): a pause this keeper only adopted (found set, or set by
                // someone else) that someone else cleared — the Emergency multisig — stays clear;
                // a market the keeper degraded itself stays degraded (the pauser sets it again).
                if !flag && !unpausing {
                    track.on_flag_cleared_by_other();
                }
            }
            track.set_positions(mv.positions.iter().map(|p| p.key));
            match track.sync_pause_flag(now, flag, unpausing) {
                // ★OWNER-M7-3 (b): the designated pauser saw the flag clear, then set, without
                // having sent the set: a second or compromised coverage key.
                Some(FlagSet::Foreign) if pauser => {
                    self.alerts
                        .push(Alert::CoveragePauseForeign { market: key });
                }
                Some(_) => self.alerts.push(Alert::CoveragePauseFound { market: key }),
                None => {}
            }
            if covered {
                let found = track.check_foreign(&mv.market, &cov, open);
                if let Some(sec) = found.price {
                    report.bursts.push((key, sec));
                    self.metrics.market(key).bursts += 1;
                }
                if found.genuine.is_some() {
                    self.metrics.market(key).foreign_moves += 1;
                }
                if let Some(sec) = found.liquidity {
                    report.liq_covers.push((key, sec));
                    if found.liq_started {
                        self.metrics.market(key).liq_covers += 1;
                    }
                }
            } else {
                // Decision A (ARCH §7.1 duties 2, 8): price bursts and liquidity-anomaly coverage
                // run on covered markets only.
                track.stop_bursts();
            }
        }

        // Duty 6 (M5, M6): a list-only market whose detection set shows a pump venue again is
        // cranked onto it (`crank_price`'s re-detection, ARCH §2.5 `None` row); a curve or
        // PumpSwap market's trader accounts, which the re-detection and the graduation do not
        // create, are created when missing.
        let mut force_crank = graduation::redetection_due(&view.markets[idx]);
        if view.markets[idx].kind == VenueKind::PumpCurve && !self.curve_ready.contains(&key) {
            self.prepare_curve_trader(view, idx, now, report).await?;
        }
        if view.markets[idx].kind == VenueKind::PumpSwap {
            self.prepare_pamm_trader(view, idx, now, report).await?;
        }

        // Duty 6: `migrate_v2` as soon as the curve is complete (before the exits: a migrating
        // market's exits pause until then).
        let mut observed = false;
        if self.cfg.graduation.migrate
            && graduation::migration_due(&view.markets[idx])
            && self.migrate(view, idx, now, report).await?
        {
            force_crank = true;
        }
        // Duty 6 (M6): the `→ PumpSwap` transition as soon as the canonical pool is valid (after
        // the keeper's own `migrate_v2` or anyone's): the pool's accounts, the crank onto the pool
        // and its first observation, which is the market's observation this second.
        if graduation::transition_due(&view.markets[idx])
            && self.transition(view, idx, now, slot, report).await?
        {
            observed = true;
        }

        let mut attempted: HashSet<(Pubkey, ExitKind)> = HashSet::new();
        // pump-final §7: a keeper exit of this tick completed the curve (`VenueStateChanged
        // PumpCurve → PumpMigrating`): `migrate_v2` and the transition follow in this tick.
        let mut completed = false;
        let mut due_before: Option<Option<Due>> = None;
        loop {
            let mv = view.markets[idx].clone();
            let proj = decide::project(&view.config, &mv, clock)?;
            let evals: Vec<PositionEval> = decide::evaluate_all(&mv, &proj, &cov)
                .into_iter()
                .filter_map(|r| match r {
                    Ok(e) => Some(e),
                    Err((k, e)) => {
                        report.errors.push(format!("{key}/{k}: {e}"));
                        None
                    }
                })
                .collect();
            let needs = decide::market_needs(
                &mv,
                &proj,
                &evals,
                &cov,
                view.config.params.pool_list_window_s,
                self.tracks[&key].liq_covered(now),
            );
            if due_before.is_none() {
                // U28: the positions waiting for their bound, once per tick.
                for e in &evals {
                    if matches!(e.plan, Plan::LiquidateWait | Plan::ExpireWait)
                        && let Some(bps) = b_ratio_bps(e)
                    {
                        self.metrics.b_ratio_wait.record(bps);
                    }
                }
                // Decision A (ARCH §7.1 duty 8): below the value-at-risk floor, the idle cadence
                // only — no per-second coverage of any kind; exits still go on these reads.
                let below = self.budget.tier(&key) == Some(Tier::BelowFloor);
                let due_needs = if below {
                    MarketNeeds {
                        open_positions: needs.open_positions,
                        ..MarketNeeds::default()
                    }
                } else {
                    needs
                };
                let due = self.tracks[&key].observation_due(&due_needs, now, slot, &cov);
                if let Some(t) = self.tracks.get_mut(&key)
                    && !below
                    && (needs.per_second || needs.warm_up || t.in_burst(now))
                {
                    // M7-SEC-1 (b): the seconds a slow tick skips after this one were required.
                    t.note_every_second(now);
                }
                if let Some(d) = due {
                    if let Some(t) = self.tracks.get_mut(&key) {
                        t.required(now);
                    }
                    report.due.push((key, d));
                }
                due_before = Some(due);
            }
            report.needs.insert(key, needs);
            report.evaluations.insert(key, evals.clone());

            // The next exit to attempt.
            let next = evals.iter().find_map(|e| {
                let kind = match e.plan {
                    Plan::Liquidate => ExitKind::Liquidate,
                    Plan::Expire => ExitKind::Expire,
                    Plan::SolSettle | Plan::S3Probe => ExitKind::SolSettle,
                    _ => return None,
                };
                (!attempted.contains(&(e.key, kind))).then_some((e.clone(), kind))
            });
            let Some((ev, kind)) = next else { break };
            attempted.insert((ev.key, kind));
            let Some(pv) = mv.positions.iter().find(|p| p.key == ev.key).cloned() else {
                continue;
            };
            // The S3 probe's previous second, while this attempt holds the throttle.
            let mut probed_before: Option<Option<i64>> = None;
            if kind == ExitKind::SolSettle {
                let track = self.tracks.entry(key).or_default();
                if track.settle_sent.get(&pv.key) == Some(&now) {
                    continue;
                }
                if ev.plan == Plan::S3Probe {
                    let last = track.s3_probe.get(&pv.key).copied();
                    let throttled =
                        last.is_some_and(|t| now.saturating_sub(t) < cov.s3_probe_interval_s);
                    if throttled && !ev.expired {
                        continue;
                    }
                    probed_before = Some(track.s3_probe.insert(pv.key, now));
                }
            }
            let (result, format) = self.attempt(&mv, &pv, kind, ev.plan, now, report).await;
            // AIR-18: a refusal in this slot is no probe: the next tick (slot) probes again.
            if matches!(result, ExitResult::NotDeployed { .. })
                && let Some(before) = probed_before
            {
                let probes = &mut self.tracks.entry(key).or_default().s3_probe;
                match before {
                    Some(t) => probes.insert(pv.key, t),
                    None => probes.remove(&pv.key),
                };
            }
            if matches!(result, ExitResult::Deferred { .. }) {
                // T-CASCADE: the market's exit budget for this slot is spent; the rest (most
                // under-collateralised first: `evaluate_all`'s order) goes out next slot.
                self.metrics.market(key).exits_deferred += 1;
                self.account_exit(&mv, &pv, kind, ev.plan, format, result, now, slot, report);
                break;
            }
            let mut chained: Option<ExitKind> = None;
            if let ExitResult::Refused { code, .. } = &result {
                if is(*code, ShortwinError::UseSolSettle) || waits_for_upgrade(*code, format) {
                    chained = Some(ExitKind::SolSettle);
                } else if is(*code, ShortwinError::UseExpire) {
                    chained = Some(ExitKind::Expire);
                }
            }
            // Duty 4: `sol_settle` at most once a second per position.
            if chained == Some(ExitKind::SolSettle)
                && self
                    .tracks
                    .get(&key)
                    .is_some_and(|t| t.settle_sent.get(&pv.key) == Some(&now))
            {
                chained = None;
            }
            let exit_landed =
                result.landed().is_some() && matches!(kind, ExitKind::Liquidate | ExitKind::Expire);
            let completes = exit_landed && result.landed().is_some_and(completes_curve);
            let mut landed =
                self.account_exit(&mv, &pv, kind, ev.plan, format, result, now, slot, report);
            // U28: `B̂_live/B̂_ref` at the keeper's decision of a landed liquidation or expiry.
            if exit_landed && let Some(bps) = b_ratio_bps(&ev) {
                self.metrics.b_ratio_landed.record(bps);
            }
            // The program redirected the exit (`UseSolSettle`, `UseExpire`; on shortwin 1.0.0 a
            // curve exit past `rT`, [`waits_for_upgrade`]): follow it once.
            let mut chained_completes = false;
            if !landed
                && let Some(next_kind) = chained
                && attempted.insert((pv.key, next_kind))
            {
                let (r, format) = self
                    .attempt(&mv, &pv, next_kind, ev.plan, now, report)
                    .await;
                chained_completes =
                    next_kind == ExitKind::Expire && r.landed().is_some_and(completes_curve);
                landed =
                    self.account_exit(&mv, &pv, next_kind, ev.plan, format, r, now, slot, report);
            }
            // pump-final §7: an exit that completed the curve — past the live curve's `rT`, by
            // pump's synthetic migration, when the position's `Q` exceeded it.
            if completes || chained_completes {
                completed = true;
                if ev.synthetic {
                    self.metrics.market(key).synthetic_exits += 1;
                    self.metrics.synthetic_exits += 1;
                }
            }
            if landed {
                observed = true;
                // Re-read the market: the exit moved it (a buyback, an arming write, a
                // settlement that removed a position).
                let fresh = view::load(&self.chain, &[key]).await?;
                if let Some(m) = fresh.markets.into_iter().next() {
                    view.markets[idx] = m;
                }
            }
        }

        // pump-final §7: the keeper's own exit completed the curve this tick — `migrate_v2` and the
        // transition now rather than at the next tick's projection (duty 6).
        if completed {
            if self.cfg.graduation.migrate
                && graduation::migration_due(&view.markets[idx])
                && self.migrate(view, idx, now, report).await?
            {
                force_crank = true;
            }
            if graduation::transition_due(&view.markets[idx])
                && self.transition(view, idx, now, slot, report).await?
            {
                observed = true;
            }
        }

        let due = due_before.flatten();
        if observed {
            report.observed.push((key, due));
        } else if let Some(d) = due.or(force_crank.then_some(Due::Second)) {
            if force_crank {
                // A graduation step's crank (after the keeper's `migrate_v2`, a list-only
                // market's re-detection): a graduation move, never charged or throttled.
                self.free_cranks.insert(key);
            } else {
                let req = self.charge_of(&key, d, now, report);
                self.crank_charges.insert(key, req);
            }
            crank_queue.push((key, d));
        }

        // Duties 6 and 13: the trader side, after this second's exits (a settlement that wiped the
        // pool is followed by its post-wipe sweep in the same second).
        self.sweep_duties(view, idx, now, report).await?;
        Ok(())
    }

    /// Decision A (2026-10-01; ARCH §7.1 duty 8): the charge class of `key`'s crank due for `d`
    /// at `now`. On a covered market: **exit-critical** — the cranks exits depend on, never
    /// throttled (O17): a position near a threshold (the EMA leg must follow the price for a
    /// liquidation to become possible), a liquidatable or expired position waiting for a fresh
    /// reference (`Observe`) or its bound (`*Wait`), an armed position, the S3 chain, a market
    /// without a route — **at most one crank a second** (M7D-4: the per-second coverage these
    /// need; a burst's further per-slot cranks in a second the keeper already observed are
    /// discretionary) —; **discretionary** — a price burst (`Due::Slot`) or per-second coverage
    /// owed to the liquidity anomaly alone —; **baseline** otherwise (the idle cadence, the guard
    /// coverage after a switch, per-second coverage of a market off its EMA). Below the floor
    /// (M7D-2): the idle crank an exit waits on — a position that waits only for a fresh
    /// reference (`PositionEval::stale_exit`) or its bound, an armed position, the S3 chain, a
    /// market without a route — is exit-critical (the tail, then the overdraft: exits are never
    /// paused); any other idle crank baseline (the tail). Without positions: a warm-up or
    /// listing-window crank (may overdraw), or another baseline charge.
    fn charge_of(&self, key: &Pubkey, d: Due, now: i64, report: &TickReport) -> ChargeReq {
        let evals = report.evaluations.get(key);
        let needs = report.needs.get(key).copied().unwrap_or_default();
        let chain = needs.no_route || needs.start_chain || needs.s3_due_executable;
        match self.budget.tier(key).unwrap_or_default() {
            Tier::Covered => {
                let serves_exit = evals.is_some_and(|evs| {
                    evs.iter()
                        .any(|e| e.near || !matches!(e.plan, Plan::Idle | Plan::Watch))
                }) || chain;
                // M7D-4: an observation of the keeper's already landed this second (a crank in an
                // earlier slot, an exit): the per-second coverage an exit needs is met.
                let observed = self
                    .tracks
                    .get(key)
                    .and_then(|t| t.last_obs)
                    .is_some_and(|(t, _)| t >= now);
                let near =
                    evals.is_some_and(|evs| evs.iter().any(|e| e.near || e.plan != Plan::Idle));
                let liq_only = needs.liq_covered
                    && !(near
                        || needs.ema_deviates
                        || needs.warming
                        || needs.guarded
                        || needs.s3_due_executable);
                let class = if serves_exit && !(d == Due::Slot && observed) {
                    ChargeClass::ExitCritical
                } else if d == Due::Slot || (d == Due::Second && liq_only) {
                    ChargeClass::Discretionary
                } else {
                    ChargeClass::Baseline
                };
                ChargeReq::of(class)
            }
            Tier::BelowFloor => {
                let serves_exit = evals.is_some_and(|evs| {
                    evs.iter().any(|e| {
                        e.stale_exit || e.armed || !matches!(e.plan, Plan::Idle | Plan::Watch)
                    })
                }) || chain;
                ChargeReq::of(if serves_exit {
                    ChargeClass::ExitCritical
                } else {
                    ChargeClass::Baseline
                })
            }
            Tier::NoPositions => ChargeReq {
                class: ChargeClass::Baseline,
                warm_up: d == Due::Listing || needs.warm_up,
            },
        }
    }

    /// Duty 6 (M5; module docs): the trader's base ATA and pump UVA of `view.markets[idx]`'s curve
    /// market ([`graduation::trader_account_keys`]) are read once; the missing ones are created
    /// (at most one attempt a second), and the market is marked ready when both exist.
    async fn prepare_curve_trader(
        &mut self,
        view: &ChainView,
        idx: usize,
        now: i64,
        report: &mut TickReport,
    ) -> anyhow::Result<()> {
        let mv = &view.markets[idx];
        let key = mv.key;
        let keys = graduation::trader_account_keys(&mv.market);
        let accs = self.chain.accounts(&keys).await?;
        let missing = [
            accs.first().is_none_or(Option::is_none),
            accs.get(1).is_none_or(Option::is_none),
        ];
        if missing == [false, false] {
            self.curve_ready.insert(key);
            return Ok(());
        }
        if self.curve_prepare_tried.get(&key) == Some(&now) {
            return Ok(());
        }
        self.curve_prepare_tried.insert(key, now);
        let ixs = graduation::trader_account_ixs(&self.payer.pubkey(), &mv.market, missing);
        let result = self.send_exit(&ixs, key, ExtrasSel::NONE, now).await;
        if result.landed().is_some() {
            self.curve_ready.insert(key);
        } else if matches!(result, ExitResult::NotDeployed { .. }) {
            // AIR-18: transient; the next tick (slot) tries again.
            self.curve_prepare_tried.remove(&key);
        }
        report.curve_accounts.push((key, result));
        Ok(())
    }

    /// The pool accounts of `accounts` (from [`graduation::pamm_accounts_of`]) that do not exist
    /// yet: a UVA not owned by PumpSwap (or empty), a WSOL ATA that is not an SPL Token account.
    async fn missing_pamm(
        &self,
        accounts: &[(Pubkey, PammAccount)],
    ) -> anyhow::Result<Vec<(Pubkey, PammAccount)>> {
        let keys: Vec<Pubkey> = accounts.iter().map(|(k, _)| *k).collect();
        let accs = self.chain.accounts(&keys).await?;
        Ok(accounts
            .iter()
            .zip(accs)
            .filter(|((_, what), acc)| {
                let owner = what.owner_program();
                acc.as_ref()
                    .is_none_or(|a| a.owner != owner || a.data.is_empty())
            })
            .map(|(a, _)| *a)
            .collect())
    }

    /// Duty 6 (M6): the pool accounts `view.markets[idx]`'s PumpSwap trades require (the trader's
    /// base ATA, and ARCH §3.12's existence requirement: the trader's pAMM UVA, the coin-creator
    /// vault's WSOL ATA, the two recipients' WSOL ATAs, on a cashback coin the trader's cashback
    /// ATA) are read once per list
    /// (a coin-creator change makes a new list); the missing ones are created (at most one attempt
    /// a second), and the list is marked ready when every account exists.
    async fn prepare_pamm_trader(
        &mut self,
        view: &ChainView,
        idx: usize,
        now: i64,
        report: &mut TickReport,
    ) -> anyhow::Result<()> {
        let mv = &view.markets[idx];
        let key = mv.key;
        // A pool that does not parse (a layout change, A63) has no trade to prepare for.
        let Ok(accounts) = graduation::pamm_accounts_of(mv) else {
            return Ok(());
        };
        let list: Vec<Pubkey> = accounts.iter().map(|(k, _)| *k).collect();
        if self.pamm_ready.get(&key) == Some(&list) {
            return Ok(());
        }
        let missing = self.missing_pamm(&accounts).await?;
        if missing.is_empty() {
            self.pamm_ready.insert(key, list);
            return Ok(());
        }
        if self.pamm_prepare_tried.get(&key) == Some(&now) {
            return Ok(());
        }
        self.pamm_prepare_tried.insert(key, now);
        let ixs = graduation::pamm_account_ixs(&self.payer.pubkey(), &mv.market, &missing);
        let result = self.send_exit(&ixs, key, ExtrasSel::NONE, now).await;
        if result.landed().is_some() {
            self.pamm_ready.insert(key, list);
        } else if matches!(result, ExitResult::NotDeployed { .. }) {
            // AIR-18: transient; the next tick (slot) tries again, nothing alerted.
            self.pamm_prepare_tried.remove(&key);
        } else if !matches!(result, ExitResult::DryRun { .. }) {
            let detail = format!("pool accounts: {result:?}");
            self.graduation_alert(
                key,
                &detail,
                Alert::TransitionFailed {
                    market: key,
                    detail: detail.clone(),
                },
            );
        }
        report.pamm_accounts.push((key, result));
        Ok(())
    }

    /// Duty 6 (M6; [`graduation::transition_ixs`]): the `→ PumpSwap` transition of
    /// `view.markets[idx]` — its missing pool accounts, `crank_price` on the curve's ReadSet onto
    /// the pool, `crank_price` on the pool (its first observation) — at most once a second, at
    /// the exit priority; the market is re-read after it lands. Returns whether it landed (the
    /// market's observation this second).
    async fn transition(
        &mut self,
        view: &mut ChainView,
        idx: usize,
        now: i64,
        slot: u64,
        report: &mut TickReport,
    ) -> anyhow::Result<bool> {
        let mv = view.markets[idx].clone();
        let key = mv.key;
        if self.transition_tried.get(&key) == Some(&now) {
            return Ok(false);
        }
        self.transition_tried.insert(key, now);
        let missing = match graduation::pamm_accounts_of(&mv) {
            Ok(a) => self.missing_pamm(&a).await?,
            Err(e) => {
                report
                    .errors
                    .push(format!("{key}: transition accounts: {e:#}"));
                Vec::new()
            }
        };
        let ixs = graduation::transition_ixs(&self.payer.pubkey(), &mv, &missing);
        let result = self.send_exit(&ixs, key, ExtrasSel::NONE, now).await;
        let landed = result.landed().cloned();
        let changes = landed.as_ref().map(venue_changes).unwrap_or_default();
        match &landed {
            Some(out) => {
                self.on_landed(key, out, now, slot, false);
                self.metrics.market(key).transitions += 1;
            }
            // AIR-18: transient; the next tick (slot) tries again, nothing alerted.
            None if matches!(result, ExitResult::NotDeployed { .. }) => {
                self.transition_tried.remove(&key);
            }
            None if !matches!(result, ExitResult::DryRun { .. }) => {
                let detail = format!("transition: {result:?}");
                self.graduation_alert(
                    key,
                    &detail,
                    Alert::TransitionFailed {
                        market: key,
                        detail: detail.clone(),
                    },
                );
            }
            None => {}
        }
        report.transitions.push(TransitionRecord {
            market: key,
            created: missing.len(),
            venue_changes: changes,
            result,
        });
        if landed.is_some() {
            let fresh = view::load(&self.chain, &[key]).await?;
            if let Some(m) = fresh.markets.into_iter().next() {
                view.markets[idx] = m;
            }
        }
        Ok(landed.is_some())
    }

    /// Duties 6 and 13 ([`crate::sweep`]): right after a full wipe (the program's post-wipe bits
    /// still clear) the venue cashback claims the trader needs and `sweep_trader`, at most once a
    /// second; every `sweep.check_interval_s` the trader side is read, and `sweep_trader` goes out
    /// when it holds anything, with the venue cashback claims once the claimable cashback reaches
    /// `sweep.cashback_min_lamports` (at most every `sweep.cashback_interval_s`).
    async fn sweep_duties(
        &mut self,
        view: &ChainView,
        idx: usize,
        now: i64,
        report: &mut TickReport,
    ) -> anyhow::Result<()> {
        let cfg = self.cfg.sweep.clone();
        let mv = &view.markets[idx];
        let key = mv.key;
        let m = mv.market.as_ref();
        let post_wipe =
            cfg.post_wipe && sweep::wipe_pending(m) && self.post_wipe_tried.get(&key) != Some(&now);
        let check = cfg.sweep
            && self
                .sweep_checked
                .get(&key)
                .is_none_or(|t| now.saturating_sub(*t) >= cfg.check_interval_s || now < *t);
        if !post_wipe && !check {
            return Ok(());
        }
        // The cadences this attempt holds, as they were (AIR-18: a refusal in this slot gives
        // them back, so the next tick tries again).
        let checked_before = check.then(|| self.sweep_checked.insert(key, now));
        // M7-SEC-8: the tick's prefetched facts, unless this is a post-wipe sweep or an exit
        // landed on the market this tick (the trader side may have moved).
        let exited = report
            .exits
            .iter()
            .any(|e| e.market == key && e.result.landed().is_some());
        let prefetched = (!post_wipe && !exited)
            .then(|| self.sweep_prefetch.remove(&key))
            .flatten();
        let accs = match prefetched {
            Some(a) => a,
            None => self.chain.accounts(&sweep::fact_keys(m)).await?,
        };
        let facts = SweepFacts::of(m, &accs);
        let payer = self.payer.pubkey();
        let claim_due = facts.claimable() > 0
            && facts.claimable() >= cfg.cashback_min_lamports
            && self
                .cashback_claimed
                .get(&key)
                .is_none_or(|t| now.saturating_sub(*t) >= cfg.cashback_interval_s || now < *t);
        let mut claimed_before = None;
        let (kind, ixs) = if post_wipe {
            self.post_wipe_tried.insert(key, now);
            (
                SweepKind::PostWipe,
                sweep::post_wipe_ixs(&payer, &key, m, &facts),
            )
        } else if check && claim_due {
            claimed_before = Some(self.cashback_claimed.insert(key, now));
            (
                SweepKind::Cashback,
                sweep::post_wipe_ixs(&payer, &key, m, &facts),
            )
        } else if check && facts.sweep_due() {
            (SweepKind::Sweep, vec![sweep::sweep_trader_ix(&key, m)])
        } else {
            return Ok(());
        };
        let result = self.send_exit(&ixs, key, ExtrasSel::NONE, now).await;
        match &result {
            ExitResult::Landed { .. } => {
                let mm = self.metrics.market(key);
                mm.sweeps += 1;
                if facts.claimable() > 0 && kind != SweepKind::Sweep {
                    mm.cashback_claims += 1;
                }
            }
            ExitResult::DryRun { .. } | ExitResult::NoOp | ExitResult::Deferred { .. } => {}
            // AIR-18: transient; the next tick (slot) sends it again, nothing alerted.
            ExitResult::NotDeployed { .. } => {
                if kind == SweepKind::PostWipe {
                    self.post_wipe_tried.remove(&key);
                }
                for (map, before) in [
                    (&mut self.sweep_checked, checked_before),
                    (&mut self.cashback_claimed, claimed_before),
                ] {
                    match before {
                        Some(Some(t)) => {
                            map.insert(key, t);
                        }
                        Some(None) => {
                            map.remove(&key);
                        }
                        None => {}
                    }
                }
            }
            other => {
                let detail = match other {
                    ExitResult::Refused { outcome, .. } | ExitResult::Failed { outcome } => {
                        format!("{:?}", outcome.err)
                    }
                    ExitResult::Error(e) => e.clone(),
                    _ => String::new(),
                };
                self.graduation_alert(
                    key,
                    &format!("sweep {kind:?}: {detail}"),
                    Alert::SweepFailed {
                        market: key,
                        post_wipe: kind == SweepKind::PostWipe,
                        detail,
                    },
                );
            }
        }
        report.sweeps.push(SweepRecord {
            market: key,
            kind,
            facts,
            instructions: ixs.len(),
            result,
        });
        Ok(())
    }

    /// Raises a graduation alert once per (`key`, detail).
    fn graduation_alert(&mut self, key: Pubkey, detail: &str, alert: Alert) {
        if self.graduation_alerted.insert((key, detail.to_string())) {
            self.alerts.push(alert);
        }
    }

    /// Duty 6: pump `migrate_v2` of `view.markets[idx]`'s completed curve, top-level and alone
    /// ([`graduation::migrate_v2_ix`]), at most once a second; the market is re-read after it
    /// lands. Returns whether it landed.
    async fn migrate(
        &mut self,
        view: &mut ChainView,
        idx: usize,
        now: i64,
        report: &mut TickReport,
    ) -> anyhow::Result<bool> {
        let mv = view.markets[idx].clone();
        let key = mv.key;
        if self.migrate_tried.get(&key) == Some(&now) {
            return Ok(false);
        }
        self.migrate_tried.insert(key, now);
        let result =
            match graduation::migrate_v2_ix(&self.payer.pubkey(), &mv.market, &mv.read_accounts) {
                Ok(ix) => self.send_exit(&[ix], key, ExtrasSel::NONE, now).await,
                Err(e) => ExitResult::Error(format!("{e:#}")),
            };
        let landed = result.landed().is_some();
        match &result {
            ExitResult::Landed { .. } => self.metrics.market(key).migrations += 1,
            ExitResult::DryRun { .. } | ExitResult::NoOp | ExitResult::Deferred { .. } => {}
            // AIR-18: transient; the next tick (slot) tries again, nothing alerted.
            ExitResult::NotDeployed { .. } => {
                self.migrate_tried.remove(&key);
            }
            other => {
                let detail = match other {
                    ExitResult::Refused { outcome, .. } | ExitResult::Failed { outcome } => {
                        format!("{:?}", outcome.err)
                    }
                    ExitResult::Error(e) => e.clone(),
                    _ => String::new(),
                };
                self.graduation_alert(
                    key,
                    &format!("migrate: {detail}"),
                    Alert::MigrateFailed {
                        market: key,
                        detail: detail.clone(),
                    },
                );
            }
        }
        report.migrations.push(MigrateRecord {
            market: key,
            result,
        });
        if landed {
            let fresh = view::load(&self.chain, &[key]).await?;
            if let Some(m) = fresh.markets.into_iter().next() {
                view.markets[idx] = m;
            }
        }
        Ok(landed)
    }

    /// Books an exit attempt; returns whether it landed (an observation of the market).
    #[allow(clippy::too_many_arguments)]
    fn account_exit(
        &mut self,
        mv: &MarketView,
        pv: &PositionView,
        kind: ExitKind,
        plan: Plan,
        curve_format: Option<CurveExtrasFormat>,
        result: ExitResult,
        now: i64,
        slot: u64,
        report: &mut TickReport,
    ) -> bool {
        let key = mv.key;
        let mut landed = false;
        match &result {
            ExitResult::Landed {
                outcome,
                unit_limit,
                ..
            } => {
                landed = true;
                *report.exit_cu.entry(key).or_default() += u64::from(*unit_limit);
                let settled = !events::of::<SolSettle>(outcome).is_empty();
                self.on_landed(key, outcome, now, slot, false);
                let m = self.metrics.market(key);
                match kind {
                    ExitKind::Liquidate => m.liquidations += 1,
                    ExitKind::Expire => m.expiries += 1,
                    ExitKind::SolSettle => {
                        m.settle_calls += 1;
                        if settled {
                            m.settlements += 1;
                        }
                    }
                }
                if kind == ExitKind::SolSettle {
                    self.tracks
                        .entry(key)
                        .or_default()
                        .settle_sent
                        .insert(pv.key, now);
                }
            }
            ExitResult::Refused { code, .. } => {
                if !code.is_some_and(benign) && !waits_for_upgrade(*code, curve_format) {
                    self.alerts.push(Alert::ExitRefused {
                        market: key,
                        position: pv.key,
                        code: *code,
                        detail: refusal_brief(&result),
                    });
                }
            }
            ExitResult::Failed { outcome } => {
                self.alerts.push(Alert::ExitRefused {
                    market: key,
                    position: pv.key,
                    code: outcome.err.as_ref().and_then(TxError::custom_code),
                    detail: format!("failed on chain: {}", refusal_brief(&result)),
                });
            }
            ExitResult::Error(e) => report.errors.push(format!("{key}/{}: {e}", pv.key)),
            // AIR-18: transient (not alerted): the next tick (slot) sends it again.
            ExitResult::DryRun { .. }
            | ExitResult::NoOp
            | ExitResult::Deferred { .. }
            | ExitResult::NotDeployed { .. } => {}
        }
        report.exits.push(ExitRecord {
            market: key,
            position: pv.key,
            kind,
            plan,
            curve_format,
            result,
        });
        landed
    }

    /// Books a landed transaction on `market`: coverage, spend, `CoverageGap` events.
    fn on_landed(&mut self, market: Pubkey, out: &TxOutcome, now: i64, slot: u64, crank: bool) {
        let track = self.tracks.entry(market).or_default();
        track.landed(now, slot);
        if crank {
            track.spend(now, out.fee);
        }
        let m = self.metrics.market(market);
        m.cover(now);
        m.observations += 1;
        m.cu = m.cu.saturating_add(out.units_consumed);
        m.lamports = m.lamports.saturating_add(out.fee);
        self.note_gaps(out, now);
    }

    /// `CoverageGap` events in the keeper's own transaction (A69): degraded mode, unless the gap
    /// is landing jitter of the idle cadence — at most `idle_interval_s + gap_tolerance_s`
    /// (M7-SEC-1 (a); [`crate::config::CoverageConfig::gap_degrades_above`]).
    fn note_gaps(&mut self, out: &TxOutcome, now: i64) {
        let above = self.cfg.coverage.gap_degrades_above();
        for gap in events::logged::<CoverageGap>(out) {
            let m = self.metrics.market(gap.market);
            m.coverage_gaps += 1;
            if gap.gap_s <= above {
                m.coverage_gaps_tolerated += 1;
                continue;
            }
            let track = self.tracks.entry(gap.market).or_default();
            track.on_coverage_gap(
                gap.market,
                now,
                gap.gap_s,
                gap.open_positions,
                &mut self.alerts,
            );
        }
    }

    /// Simulates and sends one exit (module docs); trading exits try the extras sets in order, and
    /// on a `PumpCurve` market in the TradeExtras format the deployed short.win parses
    /// ([`Self::attempt_curve`]). Returns the result and, for a curve trading exit, the format it
    /// carried.
    async fn attempt(
        &mut self,
        mv: &MarketView,
        pv: &PositionView,
        kind: ExitKind,
        plan: Plan,
        now: i64,
        report: &mut TickReport,
    ) -> (ExitResult, Option<CurveExtrasFormat>) {
        match kind {
            ExitKind::SolSettle => (self.attempt_settle(mv, pv, plan, now).await, None),
            ExitKind::Liquidate | ExitKind::Expire if mv.kind == VenueKind::PumpCurve => {
                let (r, f) = self.attempt_curve(mv, pv, kind, now, report).await;
                (r, Some(f))
            }
            ExitKind::Liquidate | ExitKind::Expire => (
                self.attempt_trading(mv, pv, kind, CurveExtrasFormat::V3, now, None)
                    .await,
                None,
            ),
        }
    }

    /// pump-final §7, the upgrade window (module docs): a `PumpCurve` liquidation or expiry in
    /// the TradeExtras format known for short.win's current ProgramData deploy slot, else V3
    /// first; a refusal on account parsing ([`ExitResult::format_evidence`]) tries the other
    /// format at once. The format that gets past account parsing becomes the known one for that
    /// deploy slot. When neither does:
    ///
    /// - on a deploy slot whose format an earlier exit found (the header read now), the format is
    ///   not in doubt — the same binary parsed it — and stays known: the refusals are this exit's
    ///   own (its market's ReadSet or VenueExtras, which raise the same codes, or a list's
    ///   content). The known format's refusal is the one returned, booked and paged as any refusal
    ///   (`ExitRefused`, its detail the program's error line): the other list's is only the
    ///   deployed parser refusing a list it does not take;
    /// - otherwise none is known any more, and [`Alert::CurveExtrasFormatUnknown`] is raised once
    ///   per position, deploy slot and pair of refusal codes ([`FormatAlertKey`]), its detail the
    ///   program's error lines ([`refusal_brief`]); the last attempt is returned.
    ///
    /// Every simulation must come from a bank at or past the deploy slot read
    /// ([`Self::send_exit_floored`]): one a node of an RPC pool behind that read answers is the
    /// build before's, so the exit is not sent, says nothing about the format and is tried again
    /// the next tick (an error of the tick).
    ///
    /// Returns the result and the format it carried.
    async fn attempt_curve(
        &mut self,
        mv: &MarketView,
        pv: &PositionView,
        kind: ExitKind,
        now: i64,
        report: &mut TickReport,
    ) -> (ExitResult, CurveExtrasFormat) {
        let deploy = self.shortwin_deploy_slot().await;
        // An unreadable deploy slot keeps whatever format is known.
        let known = self
            .curve_format
            .filter(|(at, _)| deploy.is_none() || *at == deploy)
            .map(|(_, f)| f);
        let first = known.unwrap_or(CurveExtrasFormat::V3);
        let mut refused: Vec<(CurveExtrasFormat, Option<u32>)> = Vec::new();
        let mut details: Vec<String> = Vec::new();
        let mut attempts: Vec<(ExitResult, CurveExtrasFormat)> = Vec::new();
        for format in [first, first.other()] {
            let r = self
                .attempt_trading(mv, pv, kind, format, now, deploy)
                .await;
            match r.format_evidence() {
                FormatEvidence::ParseFailed => {
                    refused.push((format, r.code()));
                    details.push(format!("{format:?}: {}", refusal_brief(&r)));
                    attempts.push((r, format));
                }
                evidence => {
                    let parsed = evidence == FormatEvidence::Parsed;
                    if parsed {
                        self.curve_format = Some((deploy, format));
                        // The market parses again: a later failure is alerted anew.
                        self.format_alerted.retain(|k| k.0 != mv.key);
                    }
                    if known.is_none() || !refused.is_empty() {
                        self.metrics.curve_format_probes += 1;
                        report.curve_formats.push(CurveFormatRecord {
                            market: mv.key,
                            position: pv.key,
                            deploy_slot: deploy,
                            refused,
                            chosen: parsed.then_some(format),
                        });
                    }
                    return (r, format);
                }
            }
        }
        // Neither list got past account parsing.
        self.metrics.curve_format_probes += 1;
        self.metrics.curve_format_failures += 1;
        report.curve_formats.push(CurveFormatRecord {
            market: mv.key,
            position: pv.key,
            deploy_slot: deploy,
            refused: refused.clone(),
            chosen: None,
        });
        // A format found on this very deploy slot (`known` holds only such a one when the header
        // was read) stays: the binary that parsed it runs still.
        let confirmed = deploy.is_some() && known.is_some();
        if !confirmed {
            self.curve_format = None;
            if self
                .format_alerted
                .insert((mv.key, pv.key, deploy, refused))
            {
                self.alerts.push(Alert::CurveExtrasFormatUnknown {
                    market: mv.key,
                    position: pv.key,
                    deploy_slot: deploy,
                    detail: details.join("; "),
                });
            }
        }
        // Confirmed: the known format's refusal (tried first) is this exit's own.
        let pick = if confirmed {
            attempts.into_iter().next()
        } else {
            attempts.pop()
        };
        pick.unwrap_or_else(|| {
            (
                ExitResult::Error("no TradeExtras format tried".into()),
                first,
            )
        })
    }

    /// short.win's ProgramData deploy slot (pump-final §7: the key of the known curve TradeExtras
    /// format), its header read at most once a tick; `None` when unreadable.
    async fn shortwin_deploy_slot(&mut self) -> Option<u64> {
        if let Some((at, deploy)) = self.shortwin_deploy
            && at == self.tick_slot
        {
            return deploy;
        }
        let pd = monitor::program_data_address(&shortwin::ID);
        let deploy = match self.chain.account_headers(&[pd], monitor::HEADER_LEN).await {
            Ok(v) => monitor::deploy_slot(v.first().and_then(Option::as_ref)),
            Err(e) => {
                tracing::warn!("short.win ProgramData header (curve TradeExtras format): {e:#}");
                None
            }
        };
        self.shortwin_deploy = Some((self.tick_slot, deploy));
        deploy
    }

    /// `sol_settle` of `pv` (an S3 probe sends only a settlement that settles).
    async fn attempt_settle(
        &mut self,
        mv: &MarketView,
        pv: &PositionView,
        plan: Plan,
        now: i64,
    ) -> ExitResult {
        let keeper = self.payer.pubkey();
        let ix = ixs::sol_settle(&keeper, mv, pv);
        if plan == Plan::S3Probe {
            // Probe: send only a settlement.
            let price = self.priority_price(&[mv.key], true).await;
            match self.simulate(std::slice::from_ref(&ix), now, price).await {
                Ok((sim, _, _))
                    if sim.err.is_none() && events::of::<SolSettle>(&sim).is_empty() =>
                {
                    return ExitResult::NoOp;
                }
                Ok((sim, _, _)) if sim.err.is_some() => {
                    if let Some(program) = self.deployed_this_slot(&sim, now).await {
                        return ExitResult::NotDeployed {
                            program,
                            outcome: sim,
                        };
                    }
                    return ExitResult::Refused {
                        code: sim.err.as_ref().and_then(TxError::custom_code),
                        outcome: sim,
                    };
                }
                Ok(_) => {}
                Err(e) => return ExitResult::Error(format!("{e:#}")),
            }
        }
        self.send_exit(&[ix], mv.key, ExtrasSel::NONE, now).await
    }

    /// A trading exit (`liquidate`, `expire`) of `pv`, a curve market's TradeExtras in `format`,
    /// with each extras set in turn until one gets past the set's own refusals; with `floor`,
    /// simulated on a bank at that slot or later ([`Self::send_exit_floored`]).
    async fn attempt_trading(
        &mut self,
        mv: &MarketView,
        pv: &PositionView,
        kind: ExitKind,
        format: CurveExtrasFormat,
        now: i64,
        floor: Option<u64>,
    ) -> ExitResult {
        let keeper = self.payer.pubkey();
        let choices =
            venue::extras_candidates(mv.kind, EXTRAS_IX_BUY, &mv.market, mv.extras.as_deref());
        let mut last = ExitResult::Error("no extras set to try".into());
        // M6-SEC-4: a coin creator changed since the market's last crank needs its vault
        // ATA (T11) to exist; the exit creates it idempotently in its own transaction.
        let prep = venue::pamm_creator_vault_prep(&keeper, &mv.market, mv.kind, &mv.read_accounts);
        for c in choices {
            let built = match kind {
                ExitKind::Liquidate => ixs::liquidate_as(&keeper, mv, pv, &c, format),
                _ => ixs::expire_as(&keeper, mv, pv, &c, format),
            };
            let ix = match built {
                Ok(ix) => ix,
                Err(e) => return ExitResult::Error(format!("{e:#}")),
            };
            let mut exit: Vec<Instruction> = prep.iter().cloned().collect();
            exit.push(ix);
            let r = self
                .send_exit_floored(&exit, mv.key, c.sel, now, floor)
                .await;
            // Another extras set is worth trying after a venue-side failure or a
            // refusal of the set itself: `BadExtras`, `BadRemainingAccounts` and
            // `DeniedAccount` (raised only by `venues::parse_extras`; since M10 batch 3
            // also for a set naming the adopted pool's accounts or the borrower, so a
            // hostile `current` must not stall the exit — M2-SEC-2). The program's other
            // refusals do not depend on the set, nor does a program not deployed this
            // slot (AIR-18, `ExitResult::NotDeployed`: retried the next slot).
            let retry = match &r {
                ExitResult::Refused { outcome, .. } => {
                    let code = outcome.err.as_ref().and_then(TxError::custom_code);
                    events::failing_program(outcome).is_some_and(|p| p != shortwin::ID)
                        || is(code, ShortwinError::BadExtras)
                        || is(code, ShortwinError::BadRemainingAccounts)
                        || is(code, ShortwinError::DeniedAccount)
                }
                _ => false,
            };
            last = r;
            if !retry {
                break;
            }
        }
        last
    }

    /// T-CASCADE: the compute units the keeper may still request on exits of `market` in
    /// `slot` — `budget.market_cu_per_slot` less its landed exits' limits, and
    /// `budget.payer_cu_per_slot` less all of the slot's exits' limits, whichever is less; `None`:
    /// no cap, or nothing sent yet this slot under either (the first exit always goes, whatever
    /// its size).
    fn exit_room(&self, market: &Pubkey, slot: u64) -> Option<u64> {
        let used = |k: &Pubkey| {
            self.slot_cu
                .get(k)
                .filter(|(s, _)| *s == slot)
                .map_or(0, |(_, c)| *c)
        };
        let payer = self.payer.pubkey();
        [
            (self.cfg.budget.market_cu_per_slot, used(market)),
            (self.cfg.budget.payer_cu_per_slot, used(&payer)),
        ]
        .into_iter()
        .filter(|(cap, used)| *cap > 0 && *used > 0)
        .map(|(cap, used)| cap.saturating_sub(used))
        .min()
    }

    /// Books `unit_limit` requested by a landed exit of `market` in `slot`, on the market and on
    /// the fee payer (T-CASCADE).
    fn book_exit_cu(&mut self, market: Pubkey, slot: u64, unit_limit: u32) {
        for k in [market, self.payer.pubkey()] {
            let e = self.slot_cu.entry(k).or_insert((slot, 0));
            if e.0 != slot {
                *e = (slot, 0);
            }
            e.1 = e.1.saturating_add(u64::from(unit_limit));
        }
    }

    async fn send_exit(
        &mut self,
        ixs: &[Instruction],
        market: Pubkey,
        extras: ExtrasSel,
        now: i64,
    ) -> ExitResult {
        self.send_exit_floored(ixs, market, extras, now, None).await
    }

    /// [`Self::send_exit`] whose simulation must come from a bank at slot `floor` or later
    /// (pump-final §7: a curve exit, `floor` short.win's ProgramData deploy slot as read this
    /// tick). A simulation from an earlier bank — a node of an RPC pool behind that read — ran
    /// the build before, so its answer, pass or refusal, says nothing of the deployed one: the
    /// exit is not sent ([`ExitResult::Error`], tried again the next tick).
    async fn send_exit_floored(
        &mut self,
        ixs: &[Instruction],
        market: Pubkey,
        extras: ExtrasSel,
        now: i64,
        floor: Option<u64>,
    ) -> ExitResult {
        let slot = self.tick_slot;
        let room = self.exit_room(&market, slot);
        match self
            .send_ixs_within(ixs, &[], &[market], true, now, room, floor)
            .await
        {
            Ok(Sent::Landed {
                out,
                format,
                bytes,
                unit_limit,
                ..
            }) => {
                self.book_exit_cu(market, slot, unit_limit);
                ExitResult::Landed {
                    outcome: out,
                    format,
                    extras,
                    unit_limit,
                    bytes,
                }
            }
            Ok(Sent::Deferred { unit_limit }) => ExitResult::Deferred { unit_limit },
            Ok(Sent::NotDeployed { out, program }) => ExitResult::NotDeployed {
                program,
                outcome: out,
            },
            Ok(Sent::SimFailed { out, .. }) => ExitResult::Refused {
                code: out.err.as_ref().and_then(TxError::custom_code),
                outcome: out,
            },
            Ok(Sent::Failed { out }) => ExitResult::Failed { outcome: out },
            Ok(Sent::DryRun { out }) => ExitResult::DryRun { outcome: out },
            Ok(Sent::TooLarge) => ExitResult::Error("transaction too large".into()),
            Err(e) => ExitResult::Error(format!("{e:#}")),
        }
    }

    /// How many of `ixs` (a prefix) fit one transaction of `format`, with the compute-budget
    /// and priority-price overhead a send carries (duty 7's size packing).
    fn fitting(&self, format: TxFormat, ixs: &[Instruction]) -> usize {
        let budget = ComputeBudget {
            unit_limit: tx::MAX_COMPUTE_UNITS,
            unit_price: 1,
        };
        let alts: &[AddressLookupTableAccount] = match format {
            TxFormat::V0 => &self.lookup_tables,
            TxFormat::V1 => &[],
        };
        let hash = solana_hash::Hash::default();
        (1..=ixs.len())
            .rev()
            .find(|&k| {
                tx::build(format, &self.payer, &[], &ixs[..k], &budget, hash, alts)
                    .is_ok_and(|t| tx::fits(format, &t))
            })
            .unwrap_or(0)
    }

    /// Duty 5's Raydium fast path and duty 12's pool monitor ([`crate::pool_monitor`]) on every
    /// covered Raydium and list-only market (module docs): after the exits, so a market whose
    /// last position the keeper just closed is proposed for in the same tick, and after the
    /// tick's cranks (M4-SEC-7: its RPC work never delays an observation or an exit); the
    /// observation that keeps a failing market's `pool_fail` chain goes out in a batch of its
    /// own right after, and a market adopted this tick is cranked on its new pool from the
    /// next tick on.
    async fn select_pools(
        &mut self,
        view: &mut ChainView,
        report: &mut TickReport,
        crank_queue: &mut Vec<(Pubkey, Due)>,
    ) {
        let covered: Vec<Pubkey> = view
            .markets
            .iter()
            .filter(|m| m.kind.is_raydium())
            .map(|m| m.key)
            .chain(view.list_only.iter().map(|l| l.key))
            .collect();
        // M7-SEC-8: at most `discoveries_per_tick` discoveries (four `getProgramAccounts` calls
        // each) this tick — the fast cadence first, then markets with open positions, then the
        // longest waiting; the others keep their candidates and are discovered in a later tick.
        let now = view.clock.unix_timestamp;
        let pcfg = self.cfg.pool_select.clone();
        let mut due: Vec<(u8, i64, Pubkey)> = Vec::new();
        for key in &covered {
            let stored = view
                .markets
                .iter()
                .find(|m| m.key == *key)
                .map(|m| (m.market.as_ref(), !m.positions.is_empty()))
                .or_else(|| {
                    view.list_only
                        .iter()
                        .find(|l| l.key == *key)
                        .map(|l| (l.market.as_ref(), false))
                });
            let Some((m, with_positions)) = stored else {
                continue;
            };
            let fast = m.pool_fail_since != 0 || m.fp_t0 != 0;
            let track = self.pool_tracks.get(key);
            if track.is_none_or(|t| t.discovery_due(now, fast, &pcfg)) {
                let rank = if fast {
                    0
                } else if with_positions || m.open_positions > 0 {
                    1
                } else {
                    2
                };
                let last = track.and_then(|t| t.last_discovery).unwrap_or(i64::MIN);
                due.push((rank, last, *key));
            }
        }
        due.sort();
        let may_discover: HashSet<Pubkey> = due
            .iter()
            .take(pcfg.discoveries_per_tick.max(1))
            .map(|(_, _, k)| *k)
            .collect();
        let mut latched: Option<Option<VenueGuard>> = None;
        for key in &covered {
            if let Err(e) = self
                .select_market(
                    view,
                    *key,
                    may_discover.contains(key),
                    &mut latched,
                    report,
                    crank_queue,
                )
                .await
            {
                report.errors.push(format!("{key}: pool selection: {e:#}"));
            }
        }
        // Keep the tracks of the markets not read this tick (cold, M7-SEC-8): their venue is
        // as last seen.
        let read: HashSet<Pubkey> = view.markets.iter().map(|m| m.key).collect();
        let keep: HashSet<Pubkey> = covered
            .into_iter()
            .chain(self.markets.iter().copied().filter(|k| !read.contains(k)))
            .collect();
        self.pool_tracks.retain(|k, _| keep.contains(k));
    }

    /// Whether the `VenueGuard` latch holds an upgrade of the program of a candidate on `kind`
    /// (`VenueUpgraded`: no proposal of it until `revalidate_venue`; M4-SEC-8: the candidate's
    /// own program only); the guard is read at most once a tick. An unreadable guard counts as
    /// clear (the simulation decides).
    async fn candidate_latched(
        &self,
        kind: VenueKind,
        cache: &mut Option<Option<VenueGuard>>,
    ) -> bool {
        if cache.is_none() {
            let g = self
                .chain
                .accounts(&[pda::venue_guard()])
                .await
                .ok()
                .and_then(|v| v.into_iter().next().flatten())
                .and_then(|a| view::decode_zc::<VenueGuard>(&a));
            *cache = Some(g);
        }
        cache
            .as_ref()
            .and_then(|g| g.as_ref())
            .is_some_and(|g| guard::candidate_latched(g, kind))
    }

    /// Reports what changed on `market` since the keeper last saw it and was not its own doing:
    /// another pool adopted, a collection window opened or closed (someone else's
    /// `propose_pool` / `execute_pool_switch`).
    fn note_pool_changes(&mut self, key: Pubkey, market: &Market, fast_window_s: u32) {
        let seen = Seen::of(market);
        let track = self.pool_tracks.entry(key).or_default();
        let Some(before) = track.seen.replace(seen) else {
            return;
        };
        if before.pool != seen.pool {
            self.metrics.market(key).pool_switches += 1;
            self.alerts.push(Alert::PoolSwitched {
                market: key,
                from: before.pool,
                to: seen.pool,
                venue: market.venue_kind().unwrap_or(VenueKind::None),
                kind: None,
                d_inc: before.pool_d,
                d_cand: seen.pool_d,
                by_keeper: false,
            });
        } else if before.fp_t0 == 0 && seen.fp_t0 != 0 {
            self.alerts.push(Alert::PoolWindow {
                market: key,
                candidate: seen.fp_pool,
                d_cand: market.fp_d.get(),
                window_ends: pool_select::window_ends(seen.fp_t0, fast_window_s).unwrap_or(0),
                improved: false,
                by_keeper: false,
            });
        } else if before.fp_t0 != 0 && seen.fp_t0 == 0 {
            self.alerts.push(Alert::PoolSwitchCancelled {
                market: key,
                candidate: before.fp_pool,
                by_keeper: false,
            });
        }
    }

    /// One market's pool selection this tick ([`Self::select_pools`]): discovery and evaluation
    /// on their cadence, the plan, and at most one action (two when a resolved window is
    /// followed by a new proposal in the same tick).
    async fn select_market(
        &mut self,
        view: &mut ChainView,
        key: Pubkey,
        may_discover: bool,
        latched: &mut Option<Option<VenueGuard>>,
        report: &mut TickReport,
        crank_queue: &mut Vec<(Pubkey, Due)>,
    ) -> anyhow::Result<()> {
        let clock = view.clock;
        let now = clock.unix_timestamp;
        let sp = SelectParams::of(&view.config.params);
        let pcfg = self.cfg.pool_select.clone();
        for round in 0..2 {
            // The market as the next instruction sees it: a Raydium market's prologue projected
            // to the clock (`pool_d` refreshed, the `pool_fail` chain stepped); a list-only
            // market as stored.
            let (market, s, mv): (Box<Market>, Situation, Option<MarketView>) =
                // (A list-only market is in `view.markets` too, with its detection set as ReadSet:
                // the list-only branch below is its pool-selection view.)
                match view
                    .markets
                    .iter()
                    .find(|m| m.key == key && m.kind != VenueKind::None)
                {
                    Some(mv) if mv.kind.is_raydium() => {
                        let proj = decide::project(&view.config, mv, clock)?;
                        let s = Situation::of(&proj.market, mv.kind, proj.now, proj.pool_fail);
                        (proj.market, s, Some(mv.clone()))
                    }
                    Some(_) => return Ok(()),
                    None => match view.list_only.iter().find(|l| l.key == key) {
                        Some(l) => {
                            let t = prologue::market_now(&l.market, now);
                            let s = Situation::of(&l.market, VenueKind::None, t, false);
                            (l.market.clone(), s, None)
                        }
                        None => return Ok(()),
                    },
                };
            if round == 0 {
                self.note_pool_changes(key, &market, sp.fast_window_s);
            }

            // Discovery (the mint's Raydium SOL pools), then their ranking within this tick's
            // key budget (M4-SEC-7): only the best `max_candidates` are evaluated.
            let fast = s.pool_fail || s.fp_t0 != 0;
            let discover = may_discover
                && self
                    .pool_tracks
                    .get(&key)
                    .is_none_or(|t| t.discovery_due(now, fast, &pcfg));
            if discover {
                let found = pool_monitor::discover(self.discovery_chain(), &market.mint).await;
                self.metrics.pool_discoveries += 1;
                let t = self.pool_tracks.entry(key).or_default();
                t.last_discovery = Some(now);
                match found {
                    Ok(c) => t.start_ranking(c),
                    Err(e) => report.errors.push(format!("{key}: pool discovery: {e:#}")),
                }
            }
            let t = self.pool_tracks.entry(key).or_default();
            if round == 0 && !t.rank_queue.is_empty() {
                let batch = t.next_rank_batch(pcfg.rank_keys_per_tick);
                let keys: Vec<Pubkey> = batch.iter().flat_map(|c| c.rank_keys()).collect();
                let accs = self.chain.accounts(&keys).await?;
                let scored: Vec<(u128, pool_monitor::CandidatePool)> = batch
                    .into_iter()
                    .zip(accs.chunks(2))
                    .map(|(c, a)| (pool_monitor::score(&c, a), c))
                    .collect();
                let t = self.pool_tracks.entry(key).or_default();
                if t.finish_rank_batch(scored, pcfg.max_candidates) {
                    t.last_eval = None;
                }
            }

            // Evaluation, on its cadence (every tick while it matters).
            let track = self.pool_tracks.entry(key).or_default();
            let urgent = pool_monitor::urgent(&s, &sp, track);
            let window_due = s.fp_t0 != 0
                && pool_select::window_ends(s.fp_t0, sp.fast_window_s).is_ok_and(|e| s.now >= e);
            if !track.eval_due(now, urgent, &pcfg) && !window_due {
                if track.keep_chain {
                    let every = self.cfg.coverage.no_route_interval_s;
                    keep_pool_fail_chain(key, mv.as_ref(), now, every, report, crank_queue);
                }
                return Ok(());
            }
            let cands = track.candidates.clone();
            let list_only = s.venue == VenueKind::None;
            let mut keys: Vec<Pubkey> = cands
                .iter()
                .flat_map(|c| c.read_keys.iter().copied())
                .collect();
            // D1's detection set, only when there is a candidate to check it for.
            if list_only && !cands.is_empty() {
                keys.extend(ixs::detection_set(&market));
            }
            let accs = if keys.is_empty() {
                Vec::new()
            } else {
                self.chain.accounts(&keys).await?
            };
            let mut at = 0usize;
            let mut evaluated = Vec::with_capacity(cands.len());
            // Switch only if all fit: the program's check on the market as the next instruction
            // sees it, with the cached oracle (the tier the market would have on a candidate).
            let fit = FitContext {
                params: &view.config.params,
                tiers: &view.config.tiers,
                oracle: view
                    .oracle
                    .as_ref()
                    .map_or(OraclePrice::default(), |o| OraclePrice {
                        price: o.price,
                        conf: o.conf,
                        expo: o.expo,
                        publish_time: o.publish_time,
                    }),
                now: s.now,
            };
            for c in &cands {
                let n = c.read_keys.len();
                let slice = accs.get(at..at.saturating_add(n)).unwrap_or(&[]);
                at = at.saturating_add(n);
                evaluated.push(pool_monitor::evaluate(&market, &fit, now, c, slice));
            }
            // M4-SEC-8 / M4V-4: the program refuses a proposal of a candidate whose own program
            // is latched, so the plan ranks only the others — after a latched window candidate is
            // cancelled, the best pool on the other Raydium program opens the next window.
            let mut any_latched = false;
            for e in &mut evaluated {
                if e.d().is_some() && self.candidate_latched(e.kind, latched).await {
                    any_latched = true;
                    e.verdict = pool_monitor::Verdict::Refused {
                        code: Some(u32::from(ShortwinError::VenueUpgraded)),
                        detail: "the candidate's program awaits the venue review".into(),
                    };
                }
            }
            let mut plan = pool_monitor::plan(&s, &sp, &market, &evaluated);
            if plan.action.is_none() && plan.blocked.is_none() && any_latched {
                plan.blocked = Some(u32::from(ShortwinError::VenueUpgraded));
            }
            if list_only && !cands.is_empty() {
                // D1: a pump coin keeps its pump venue (`crank_price`'s re-detection, M5).
                let det = accs.get(at..at.saturating_add(2)).unwrap_or(&[]);
                let refused = match pool_monitor::classify(&market, det) {
                    Ok(class) if class.pump_venue() => {
                        Some(Some(u32::from(ShortwinError::PumpVenuePrecedence)))
                    }
                    Ok(_) => None,
                    Err(code) => Some(code),
                };
                if let Some(code) = refused {
                    plan.action = None;
                    plan.blocked = code;
                }
            }
            // The latch of the candidate's own program (M4-SEC-8). An execution is sent anyway:
            // the program cancels a window whose candidate's program is latched (M4V-4), and a
            // proposal on the other program can follow in the same tick.
            if let Some(Action::Propose { kind, .. }) = plan.action
                && self.candidate_latched(kind, latched).await
            {
                plan.action = None;
                plan.blocked = Some(u32::from(ShortwinError::VenueUpgraded));
            }
            let track = self.pool_tracks.entry(key).or_default();
            track.evaluated = evaluated;
            track.last_eval = Some(now);
            track.last_pool_fail = Some(s.pool_fail);
            track.keep_chain = plan.keep_chain;
            let pinned = if plan.pinned {
                plan.best.map(|b| b.0)
            } else {
                None
            };
            if let Some(since) = track.note_pinned(pinned, now, &pcfg)
                && let Some((candidate, d_cand)) = plan.best
            {
                self.alerts.push(Alert::PoolPinned {
                    market: key,
                    incumbent: pool_monitor::incumbent_pool(&market),
                    candidate,
                    d_inc: s.pool_d,
                    d_cand,
                    open_positions: s.open_positions,
                    since,
                });
            }
            if plan.keep_chain {
                let every = self.cfg.coverage.no_route_interval_s;
                keep_pool_fail_chain(key, mv.as_ref(), now, every, report, crank_queue);
            }
            report.pool_plans.insert(key, plan.clone());

            // Act: at most once a second per pool; a refused proposal backs off.
            let Some(action) = plan.action else {
                return Ok(());
            };
            let track = self.pool_tracks.entry(key).or_default();
            if track.tried == Some((now, action.pool()))
                || (matches!(action, Action::Propose { .. }) && now < track.backoff_until)
            {
                return Ok(());
            }
            track.tried = Some((now, action.pool()));
            let ix = match &action {
                Action::Execute { candidate, .. } => match &mv {
                    Some(mv) => ixs::execute_pool_switch(mv, candidate),
                    // A list-only market's window (positions open, M4-SEC-4).
                    None => ixs::execute_pool_switch_with(
                        &key,
                        &market.mint,
                        &ixs::detection_set(&market),
                        candidate,
                    ),
                },
                Action::Propose {
                    kind, candidate, ..
                } => {
                    let prefix: Vec<Pubkey> = match &mv {
                        Some(mv) => mv.read_keys.clone(),
                        None => ixs::detection_set(&market).to_vec(),
                    };
                    ixs::propose_pool(&key, &market.mint, &prefix, *kind, candidate)
                }
            };
            let result = match self.send_ixs(&[ix], &[key], true, now).await {
                Ok(Sent::Landed { out, .. }) => {
                    let events = events::pool_events(&out);
                    PoolResult::Landed {
                        outcome: out,
                        events,
                    }
                }
                Ok(Sent::SimFailed { out, .. }) => PoolResult::Refused {
                    code: out.err.as_ref().and_then(TxError::custom_code),
                    outcome: out,
                },
                Ok(Sent::Failed { out }) => PoolResult::Failed { outcome: out },
                Ok(Sent::DryRun { out }) => PoolResult::DryRun { outcome: out },
                Ok(Sent::NotDeployed { out, program }) => PoolResult::NotDeployed {
                    program,
                    outcome: out,
                },
                Ok(Sent::TooLarge) => PoolResult::Error("transaction too large".into()),
                Ok(Sent::Deferred { .. }) => PoolResult::Error("deferred".into()),
                Err(e) => PoolResult::Error(format!("{e:#}")),
            };
            let landed = self.account_pool_action(key, &market, &action, &result, now, report);
            report.pool_actions.push(PoolActionRecord {
                market: key,
                action: action.clone(),
                result,
            });
            if !landed {
                return Ok(());
            }
            // Re-read the market: the action moved it (an adoption, a window).
            let fresh = view::load(&self.chain, &[key]).await?;
            view.markets.retain(|m| m.key != key);
            view.list_only.retain(|l| l.key != key);
            let seen = fresh
                .markets
                .iter()
                .map(|m| Seen::of(&m.market))
                .chain(fresh.list_only.iter().map(|l| Seen::of(&l.market)))
                .next();
            view.markets.extend(fresh.markets);
            view.list_only.extend(fresh.list_only);
            let track = self.pool_tracks.entry(key).or_default();
            if seen.is_some() {
                track.seen = seen;
            }
            // A resolved window may be followed by a new proposal in the same tick.
            if !matches!(action, Action::Execute { .. }) {
                return Ok(());
            }
            track.last_eval = None;
        }
        Ok(())
    }

    /// Books a pool-selection attempt (metrics, alerts, back-off); returns whether it landed.
    fn account_pool_action(
        &mut self,
        key: Pubkey,
        before: &Market,
        action: &Action,
        result: &PoolResult,
        now: i64,
        report: &mut TickReport,
    ) -> bool {
        let pool = action.pool();
        let backoff = if action.fast() {
            1
        } else {
            self.cfg.pool_select.eval_interval_s
        };
        let kind = match action {
            Action::Execute { kind, .. } | Action::Propose { kind, .. } => *kind,
        };
        match result {
            PoolResult::Landed { events, .. } => {
                let m = self.metrics.market(key);
                match action {
                    Action::Execute { .. } => m.pool_executions += 1,
                    Action::Propose { .. } => m.pool_proposals += 1,
                }
                for e in events {
                    report.pool_events.push(*e);
                    match e.what {
                        PoolEventKind::Switched => {
                            self.metrics.market(key).pool_switches += 1;
                            self.alerts.push(Alert::PoolSwitched {
                                market: key,
                                from: pool_monitor::incumbent_pool(before),
                                to: e.candidate,
                                venue: kind,
                                kind: Some(e.kind),
                                d_inc: e.d_inc,
                                d_cand: e.d_cand,
                                by_keeper: true,
                            });
                        }
                        PoolEventKind::Proposed => self.alerts.push(Alert::PoolWindow {
                            market: key,
                            candidate: e.candidate,
                            d_cand: e.d_cand,
                            window_ends: e.window_ends,
                            improved: matches!(
                                action,
                                Action::Propose {
                                    expect: Proposal::ImproveWindow,
                                    ..
                                }
                            ),
                            by_keeper: true,
                        }),
                        PoolEventKind::Cancelled => self.alerts.push(Alert::PoolSwitchCancelled {
                            market: key,
                            candidate: e.candidate,
                            by_keeper: true,
                        }),
                    }
                }
                let t = self.pool_tracks.entry(key).or_default();
                t.refusals_alerted.clear();
                t.backoff_until = 0;
                true
            }
            PoolResult::Refused { code, outcome } => {
                self.pool_refused(key, pool, *code, format!("{:?}", outcome.err), now, backoff);
                false
            }
            PoolResult::Failed { outcome } => {
                let code = outcome.err.as_ref().and_then(TxError::custom_code);
                self.pool_refused(
                    key,
                    pool,
                    code,
                    format!("failed on chain: {:?}", outcome.err),
                    now,
                    backoff,
                );
                false
            }
            PoolResult::Error(e) => {
                report.errors.push(format!("{key}: pool selection: {e}"));
                false
            }
            PoolResult::DryRun { .. } => {
                // Simulated only: the same cadence as a refusal, without an alert.
                let t = self.pool_tracks.entry(key).or_default();
                t.backoff_until = now.saturating_add(backoff);
                false
            }
            // AIR-18: transient: no back-off, no alert; the next tick (slot) acts again.
            PoolResult::NotDeployed { .. } => {
                self.pool_tracks.entry(key).or_default().tried = None;
                false
            }
        }
    }

    /// A refused or failed pool-selection attempt: back off; alert once per pool and code unless
    /// the refusal is a race's normal answer ([`benign_pool`]).
    fn pool_refused(
        &mut self,
        key: Pubkey,
        pool: Pubkey,
        code: Option<u32>,
        detail: String,
        now: i64,
        backoff: i64,
    ) {
        let t = self.pool_tracks.entry(key).or_default();
        t.backoff_until = now.saturating_add(backoff);
        if benign_pool(code) || t.refusals_alerted.contains(&(pool, code)) {
            return;
        }
        t.refusals_alerted.push((pool, code));
        self.alerts.push(Alert::PoolSelectRefused {
            market: key,
            pool,
            code,
            detail,
        });
    }

    /// AIR-01: the deploy slot and [`monitor::ElfFacts`] of each ProgramData of `wanted` (its
    /// address, the deploy slot its header showed), read whole — all in one read; the keeper's
    /// last read of an account stands in while its deploy slot is unchanged. The loader deploys a
    /// program at most once a slot (`Upgrade` and `ExtendProgram` both refuse while the
    /// ProgramData's slot is the current one: "Program was deployed in this block already",
    /// "Program was extended in this block already"), so a deploy slot names the account's
    /// contents. Per entry: `Ok(Some(_))` at the slot the account holds now (a later deploy than
    /// its header's may have landed since: it is judged as read); `Ok(None)` when the account is
    /// gone, is no ProgramData or the read lags its header; `Err` when the read failed (each
    /// caller's fail-safe).
    async fn read_whole(
        &mut self,
        wanted: &[(Pubkey, u64)],
        report: &mut TickReport,
    ) -> Vec<Result<Option<(u64, monitor::ElfFacts)>, ()>> {
        let mut fetch: Vec<Pubkey> = Vec::new();
        for (k, slot) in wanted {
            let hit = self.program_elf.get(k).is_some_and(|(s, _)| s == slot);
            if !hit && !fetch.contains(k) {
                fetch.push(*k);
            }
        }
        let mut read: BTreeMap<Pubkey, Option<(u64, monitor::ElfFacts)>> = BTreeMap::new();
        if !fetch.is_empty() {
            match self.chain.accounts(&fetch).await {
                Ok(accs) => {
                    for (k, acc) in fetch.iter().zip(accs) {
                        let facts = acc.as_ref().and_then(|a| {
                            Some((monitor::deploy_slot(Some(a))?, monitor::ElfFacts::of(a)?))
                        });
                        if let Some((s, f)) = facts
                            && self.program_elf.get(k).is_none_or(|(old, _)| *old <= s)
                        {
                            self.program_elf.insert(*k, (s, f));
                        }
                        read.insert(*k, facts);
                    }
                }
                Err(e) => report
                    .errors
                    .push(format!("ProgramData {fetch:?} (whole): {e:#}")),
            }
        }
        wanted
            .iter()
            .map(|(k, slot)| {
                let facts = if fetch.contains(k) {
                    *read.get(k).ok_or(())?
                } else {
                    self.program_elf.get(k).copied()
                };
                Ok(facts.filter(|(s, _)| s >= slot))
            })
            .collect()
    }

    /// Duty 10, upgrades ([`crate::monitor`]): reads the latch and the watched ProgramData
    /// headers in one call, before the markets; flags every upgrade the program would accept
    /// (`flag_venue_upgraded`, simulated first, at the exit priority ceiling) and pages it once
    /// per deploy; Token-2022's changes are alerted only. The same upgrade is attempted at most
    /// once a second (an unflagged one is retried every second; one whose simulation ran on a
    /// bank before it, or which short.win refused as not deployed, the next tick); a new deploy
    /// slot is attempted at once.
    ///
    /// AIR-01: a moved slot the header cannot judge with the venue's bit clear
    /// ([`LatchAction::Inspect`]: a fingerprint recorded) is judged here too, by the program: the
    /// flag's simulation is its rule on the whole account — every such venue of the tick
    /// together, after the others ([`Self::judge_inspected`]). An upgrade passes — paged, and
    /// flagged in the slot it is seen, before the tick's exits and cranks; the loader's
    /// `ExtendProgram` answers `VenueNotUpgraded` from a bank that holds the deploy — booked
    /// ([`Self::note_extension`]), neither flagged nor paged, and that deploy slot not simulated
    /// again. What needs the whole account is returned for [`Self::inspect_programs`], after the
    /// exits and cranks, where nothing before the exits waits on it: a slot the flag's
    /// simulation could not judge (the keeper's own judgment; the flag tried again), a move
    /// during a review past the deploy the keeper recorded (opens are blocked already), the
    /// record of the deploy the keeper reports or finds under review (a restart), and
    /// Token-2022 (alert only).
    async fn watch_programs(&mut self, now: i64, report: &mut TickReport) -> Option<Inspections> {
        let watched = monitor::watched_programs();
        let guard_key = pda::venue_guard();
        // One read: the latch whole, the ProgramData accounts sliced to (at least) their header.
        let mut keys = vec![guard_key];
        keys.extend(watched.iter().map(|w| w.program_data));
        let len = monitor::HEADER_LEN.max(VenueGuard::SPACE);
        let accs = match self.chain.account_headers(&keys, len).await {
            Ok(a) => a,
            Err(e) => {
                report
                    .errors
                    .push(format!("venue guard / ProgramData: {e:#}"));
                return None;
            }
        };
        let Some(guard) = accs
            .first()
            .and_then(Option::as_ref)
            .and_then(view::decode_zc::<VenueGuard>)
        else {
            report.errors.push("VenueGuard not found".into());
            return None;
        };
        let mut items = Vec::new();
        // AIR-01: the moved slots the flag's simulation judges, all together after the loop.
        let mut inspect = Vec::new();
        let headers = accs.get(1..).unwrap_or(&[]);
        for (w, acc) in watched.iter().zip(headers.iter()) {
            let slot = monitor::deploy_slot(acc.as_ref());
            let prev = self.program_slots.insert(w.program_data, slot);
            let Some(slot) = slot else {
                match prev {
                    // It was readable: a program closed or re-owned is worth a page.
                    Some(Some(_)) => self
                        .alerts
                        .push(Alert::ProgramDataUnreadable { program: w.program }),
                    None => tracing::warn!(program = %w.program, "ProgramData not readable"),
                    Some(None) => {}
                }
                continue;
            };
            let Some(venue) = w.guard else {
                // Token-2022 (alert only), against the deploy the keeper last read whole.
                match self.program_elf.get(&w.program_data).copied() {
                    Some((judged, _)) if judged >= slot => {}
                    Some((judged, before)) => items.push((
                        *w,
                        slot,
                        Inspection::Alert {
                            previous: judged,
                            before,
                        },
                    )),
                    None => {
                        // Not read whole yet: the header alone (fail-safe), and a first read.
                        if let Some(Some(p)) = prev
                            && p != slot
                        {
                            self.program_upgraded(w, p, slot);
                        }
                        items.push((*w, slot, Inspection::Baseline));
                    }
                }
                continue;
            };
            let flagged = guard.is_flagged(venue);
            if !flagged {
                self.flagged_elf.remove(&venue);
            }
            let i = usize::from(venue);
            // AIR-01: the venues whose reported deploy the keeper records for its review (pump
            // and PumpSwap have no fingerprint: they sit at the 10 MiB cap, so only `Upgrade`
            // moves their slot).
            let recorded = VenueGuard::elf_index(venue).is_some();
            match monitor::latch_action(&guard, venue, slot) {
                // AIR-01: under a review the keeper did not report itself (a restart), the record
                // of the deploy under review.
                LatchAction::Nothing => {
                    if flagged && recorded && !self.flagged_elf.contains_key(&venue) {
                        items.push((*w, slot, Inspection::Record(venue)));
                    }
                }
                // AIR-01: a deploy slot the program already judged an extension.
                LatchAction::Inspect if self.extension_seen.get(&w.program) == Some(&slot) => {}
                // AIR-01: the flag's simulation is the program's own rule on the whole account.
                LatchAction::Inspect => {
                    let known = guard.guard_slot.get(i).copied().unwrap_or(0);
                    inspect.push((*w, venue, slot, known));
                }
                LatchAction::Refresh if self.flagged_elf.contains_key(&venue) => {
                    items.push((*w, slot, Inspection::Review(venue)));
                }
                LatchAction::Flag | LatchAction::Refresh => {
                    let known = if flagged {
                        guard.flagged_slot.get(i)
                    } else {
                        guard.guard_slot.get(i)
                    };
                    self.report_upgrade(w, venue, slot, known.copied().unwrap_or(0), now, report)
                        .await;
                    if recorded {
                        items.push((*w, slot, Inspection::Record(venue)));
                    }
                }
                LatchAction::Extended => {}
            }
        }
        if !inspect.is_empty() {
            self.judge_inspected(inspect, now, report, &mut items).await;
        }
        (!items.is_empty()).then_some(Inspections { guard, items })
    }

    /// AIR-01 (duty 10): the moved slots of [`Self::watch_programs`] the header cannot judge
    /// (`todo`: the program, its latch venue, the header's deploy slot, the latch's
    /// `guard_slot`), judged before the tick's exits by the flag's own simulation, each at most
    /// once a second per deploy ([`Self::flag_due`]). They are simulated together
    /// ([`Self::simulate_flags`]): one priority price and one blockhash, read at the same time,
    /// then the simulations in parallel — two round trips for any number of venues, where one
    /// venue after another cost three each.
    ///
    /// - The program would flag it: paged, flagged ([`Self::send_flag_now`]: simulated again
    ///   and sent at the exit priority ceiling) and, landed, its record read after the exits.
    /// - `VenueNotUpgraded` from a bank at or past the header's deploy slot — a bank that holds
    ///   that deploy: the loader's `ExtendProgram`, booked ([`Self::note_extension`]).
    /// - `VenueNotUpgraded` from a bank before it — a node of an RPC pool behind the header
    ///   read still holds the deploy before (the validated one, or an earlier extension), where
    ///   the program answers the same for an upgrade —, or any other failure: not this deploy's
    ///   judgment. The whole account decides after the exits ([`Inspection::Unjudged`]: an
    ///   upgrade is paged with the flag's failure), and the flag is tried again — the next tick
    ///   after a lagging answer, the next second after a failure.
    /// - Refused because short.win was deployed in that slot (AIR-18): the whole account decides
    ///   too, an upgrade paged without a flag failure, and the flag goes out in a later slot. A
    ///   stranger extending short.win in every slot thus holds the flag back, not the page (opens
    ///   ordered before the slot's extension still run).
    async fn judge_inspected(
        &mut self,
        todo: Vec<(monitor::WatchedProgram, u8, u64, u64)>,
        now: i64,
        report: &mut TickReport,
        items: &mut Vec<(monitor::WatchedProgram, u64, Inspection)>,
    ) {
        let due: Vec<(monitor::WatchedProgram, u8, u64, u64)> = todo
            .into_iter()
            .filter(|(_, venue, slot, _)| self.flag_due(*venue, *slot, now))
            .collect();
        if due.is_empty() {
            return;
        }
        let venues: Vec<u8> = due.iter().map(|(_, venue, _, _)| *venue).collect();
        let sims = self.simulate_flags(&venues, now, report).await;
        for ((w, venue, slot, known), sim) in due.into_iter().zip(sims) {
            let flag = match sim {
                Some(sent) => self.flag_outcome(venue, slot, sent),
                None => self.send_flag_now(venue, slot, now, report).await,
            };
            match flag {
                FlagSent::Landed => {
                    self.page_upgrade(&w, venue, slot, known);
                    self.flag_landed(venue, slot, report);
                    items.push((w, slot, Inspection::Record(venue)));
                }
                FlagSent::DryRun => self.page_upgrade(&w, venue, slot, known),
                FlagSent::NotUpgraded { at } if at >= slot => {
                    self.note_extension(&w, slot, report);
                }
                FlagSent::NotUpgraded { at } => {
                    let failure = format!(
                        "VenueNotUpgraded simulated on slot {at}, before deploy slot {slot} (a \
                         node behind the header read); flagged again the next tick"
                    );
                    tracing::info!(
                        program = %w.program,
                        deploy_slot = slot,
                        context_slot = at,
                        "the flag's simulation ran before the deploy (AIR-01): judged on the \
                         whole account after the exits"
                    );
                    items.push((
                        w,
                        slot,
                        Inspection::Unjudged {
                            venue,
                            failure: Some(failure),
                        },
                    ));
                }
                FlagSent::NotDeployed => {
                    items.push((
                        w,
                        slot,
                        Inspection::Unjudged {
                            venue,
                            failure: None,
                        },
                    ));
                }
                // Filtered by `flag_due` above.
                FlagSent::Throttled => {}
                FlagSent::Failed(failure) => {
                    items.push((
                        w,
                        slot,
                        Inspection::Unjudged {
                            venue,
                            failure: Some(failure),
                        },
                    ));
                }
            }
        }
    }

    /// AIR-01 (duty 10): `flag_venue_upgraded(venue)` of each of `venues` simulated as
    /// [`Self::send_ixs`] would, but together: one priority price (each writes only the
    /// `VenueGuard`) and one blockhash, read at the same time, then every simulation at once.
    /// Per venue: the outcome a send would have come to before sending — `SimFailed`,
    /// `NotDeployed` ([`Self::deployed_this_slot`]), `TooLarge`, `DryRun` or an error — or
    /// `None` where the simulation passed (an upgrade: [`Self::send_flag_now`] sends it, after
    /// simulating it again) or the cluster refused the v1 format (the next attempt is v0's).
    async fn simulate_flags(
        &mut self,
        venues: &[u8],
        now: i64,
        report: &mut TickReport,
    ) -> Vec<Option<anyhow::Result<Sent>>> {
        let writable = [pda::venue_guard()];
        let (price, blockhash) = futures::join!(
            self.priority_price(&writable, true),
            self.chain.latest_blockhash()
        );
        let blockhash = match blockhash {
            Ok(bh) => bh,
            Err(e) => {
                return venues
                    .iter()
                    .map(|_| Some(Err(anyhow!("getLatestBlockhash: {e:#}"))))
                    .collect();
            }
        };
        let format = self.format(now);
        let alts = if format == TxFormat::V0 {
            self.lookup_tables.clone()
        } else {
            Vec::new()
        };
        let budget = ComputeBudget {
            unit_price: price,
            ..ComputeBudget::SIMULATION
        };
        // Each venue's transaction: `Ok(None)` too large, `Err` not built.
        let mut built = Vec::with_capacity(venues.len());
        for venue in venues {
            let t = match ixs::flag_venue_upgraded(*venue) {
                Ok(ix) => tx::build(format, &self.payer, &[], &[ix], &budget, blockhash, &alts)
                    .map(|t| tx::fits(format, &t).then_some(t)),
                Err(e) => {
                    let detail = format!("flag_venue_upgraded({venue}): {e:#}");
                    report.errors.push(detail.clone());
                    Err(anyhow!(detail))
                }
            };
            built.push(t);
        }
        let chain = &self.chain;
        let sims = futures::future::join_all(built.iter().map(|t| async move {
            match t {
                Ok(Some(t)) => Some(chain.simulate(t).await),
                _ => None,
            }
        }))
        .await;
        let mut v0 = false;
        let mut out = Vec::with_capacity(venues.len());
        for (t, sim) in built.into_iter().zip(sims) {
            let verdict = match (t, sim) {
                (Err(e), _) => Some(Err(e)),
                (Ok(None), _) => Some(Ok(Sent::TooLarge)),
                (Ok(Some(_)), None) => Some(Err(anyhow!("not simulated"))),
                (Ok(Some(_)), Some(sim)) => {
                    self.metrics.simulations += 1;
                    match sim {
                        Err(e) => Some(Err(e)),
                        Ok(sim) => {
                            if let Some(TxError::Rejected(r)) = &sim.err
                                && (v0 || self.on_rejected(format, r, now))
                            {
                                // The format the cluster refused: sent the ordinary way, in v0.
                                v0 = true;
                                out.push(None);
                                continue;
                            }
                            if let Some(TxError::Rejected(r)) = &sim.err
                                && r.starts_with("too large")
                            {
                                self.metrics.simulation_failures += 1;
                                Some(Ok(Sent::TooLarge))
                            } else if sim.err.is_some() {
                                self.metrics.simulation_failures += 1;
                                match self.deployed_this_slot(&sim, now).await {
                                    Some(program) => {
                                        Some(Ok(Sent::NotDeployed { out: sim, program }))
                                    }
                                    None => Some(Ok(Sent::SimFailed { out: sim, format })),
                                }
                            } else if self.cfg.dry_run {
                                Some(Ok(Sent::DryRun { out: sim }))
                            } else {
                                None
                            }
                        }
                    }
                }
            };
            out.push(verdict);
        }
        out
    }

    /// AIR-01 (duty 10): what [`Self::watch_programs`] left to the whole account, after the
    /// tick's exits and cranks: the ProgramData accounts read whole in one read
    /// ([`Self::read_whole`]), each judged at the slot it holds now.
    ///
    /// - A slot the flag's simulation could not judge: the program's rule against the latch's
    ///   fingerprint ([`monitor::latch_action_with`]) — an upgrade is paged, with the flag's
    ///   failure if it has one (`VenueFlagFailed`; the flag is tried again: the next tick or
    ///   second), an extension booked.
    /// - A move during a review past the recorded deploy ([`Self::judge_review`]).
    /// - The record of a deploy under review, while the account still holds it (else the next
    ///   change is refreshed and paged, as the program would accept it).
    /// - Token-2022, against the deploy the keeper last read whole ([`monitor::upgraded_since`]):
    ///   an upgrade is alerted (`ProgramUpgraded`), the loader's `ExtendProgram` only logged and
    ///   counted.
    ///
    /// When the whole read fails, a moved slot counts as an upgrade (fail-safe: paged; during a
    /// review the refresh is the program's to accept).
    async fn inspect_programs(
        &mut self,
        inspections: Inspections,
        now: i64,
        report: &mut TickReport,
    ) {
        let Inspections { guard, items } = inspections;
        let wanted: Vec<(Pubkey, u64)> = items
            .iter()
            .map(|(w, slot, _)| (w.program_data, *slot))
            .collect();
        let read = self.read_whole(&wanted, report).await;
        for ((w, header_slot, what), read) in items.into_iter().zip(read) {
            // The deploy slot judged and the account's facts (`None`: the read failed).
            let (slot, facts) = match read {
                Ok(Some((slot, facts))) => (slot, Some(facts)),
                // Gone, no ProgramData or a lagging read: the next tick.
                Ok(None) => continue,
                Err(()) => (header_slot, None),
            };
            match what {
                // A first read: kept by `read_whole`.
                Inspection::Baseline => {}
                Inspection::Record(venue) => {
                    if slot == header_slot
                        && let Some(reported) = facts.as_ref().and_then(monitor::ElfFacts::record)
                    {
                        self.flagged_elf.insert(venue, reported);
                    }
                }
                Inspection::Unjudged { venue, failure } => {
                    let upgrade = match &facts {
                        Some(f) => match monitor::latch_action_with(&guard, venue, slot, f) {
                            LatchAction::Flag => Some(true),
                            LatchAction::Extended => Some(false),
                            _ => None,
                        },
                        None => Some(true),
                    };
                    match upgrade {
                        Some(true) => {
                            let known = guard.guard_slot.get(usize::from(venue));
                            self.page_upgrade(&w, venue, slot, known.copied().unwrap_or(0));
                            // AIR-18: none when short.win refused the flag as not deployed.
                            if let Some(failure) = failure {
                                self.flag_failed(venue, slot, failure);
                            }
                        }
                        Some(false) => self.note_extension(&w, slot, report),
                        None => {}
                    }
                }
                Inspection::Review(venue) => {
                    self.judge_review(&guard, &w, venue, slot, facts.as_ref(), now, report)
                        .await;
                }
                Inspection::Alert { previous, before } => match facts {
                    Some(facts) if slot > previous => {
                        if monitor::upgraded_since(&before, &facts) {
                            self.program_upgraded(&w, previous, slot);
                        } else {
                            self.note_extension(&w, slot, report);
                        }
                    }
                    // The deploy already judged (a read lagging the last one).
                    Some(_) => {}
                    None => self.program_upgraded(&w, previous, header_slot),
                },
            }
        }
    }

    /// AIR-01: latch venue `venue`'s ProgramData moved to deploy `slot` during its review, past
    /// the deploy the keeper recorded; `facts`: the whole account (`None`: unread). The program
    /// refreshes the flag on any later slot; the keeper refreshes and pages only another upgrade
    /// — the account no longer holds the recorded deploy ([`monitor::ElfFacts::is_upgrade_since`]),
    /// or it could not be read (fail-safe) — and records that deploy instead (unread: the next
    /// tick records the deploy under review). An extension of the recorded deploy, which anyone
    /// can repeat every slot, is only booked ([`Self::note_extension`]).
    #[allow(clippy::too_many_arguments)]
    async fn judge_review(
        &mut self,
        guard: &VenueGuard,
        w: &monitor::WatchedProgram,
        venue: u8,
        slot: u64,
        facts: Option<&monitor::ElfFacts>,
        now: i64,
        report: &mut TickReport,
    ) {
        if monitor::latch_action(guard, venue, slot) != LatchAction::Refresh {
            return;
        }
        let upgrade = match (facts, self.flagged_elf.get(&venue)) {
            (Some(f), Some(reported)) => f.is_upgrade_since(*reported),
            _ => true,
        };
        if !upgrade {
            self.note_extension(w, slot, report);
            return;
        }
        match facts.and_then(monitor::ElfFacts::record) {
            Some(reported) => self.flagged_elf.insert(venue, reported),
            None => self.flagged_elf.remove(&venue),
        };
        let known = guard.flagged_slot.get(usize::from(venue));
        self.report_upgrade(w, venue, slot, known.copied().unwrap_or(0), now, report)
            .await;
    }

    /// AIR-01: books the loader's `ExtendProgram` of `w`'s ProgramData at deploy `slot` (the ELF
    /// is the one known), once per slot: logged and counted (`TickReport::extended`,
    /// `Metrics::venue_extensions`), neither flagged nor paged.
    fn note_extension(&mut self, w: &monitor::WatchedProgram, slot: u64, report: &mut TickReport) {
        if self.extension_seen.insert(w.program, slot) != Some(slot) {
            self.metrics.venue_extensions += 1;
            report.extended.push((w.program, slot));
            tracing::info!(
                program = %w.program,
                deploy_slot = slot,
                "ProgramData extended, not upgraded (AIR-01): nothing to flag or page"
            );
        }
    }

    /// Duty 10: alerts, once per deploy, that a watched program without a latch index
    /// (Token-2022) changed from deploy `previous` to deploy `slot` (alert only).
    fn program_upgraded(&mut self, w: &monitor::WatchedProgram, previous: u64, slot: u64) {
        if self.programs_alerted.insert((w.program, slot)) {
            self.alerts.push(Alert::ProgramUpgraded {
                program: w.program,
                previous_slot: previous,
                deploy_slot: slot,
            });
        }
    }

    /// Duty 10: pages the upgrade of latch venue `venue` to deploy `slot` — judged by the keeper,
    /// on the header or during a review — once per deploy (`known`: the slot the latch validated,
    /// or the one flagged during a review), and flags it ([`Self::send_flag`]); a failure to flag
    /// is alerted once per deploy.
    async fn report_upgrade(
        &mut self,
        w: &monitor::WatchedProgram,
        venue: u8,
        slot: u64,
        known: u64,
        now: i64,
        report: &mut TickReport,
    ) {
        self.page_upgrade(w, venue, slot, known);
        match self.send_flag(venue, slot, now, report).await {
            FlagSent::Landed => self.flag_landed(venue, slot, report),
            FlagSent::Failed(detail) => self.flag_failed(venue, slot, detail),
            // `NotUpgraded`: someone flagged it first (the latch already holds this slot), or —
            // answered from a bank before the deploy — the next tick asks again
            // ([`Self::flag_outcome`]).
            FlagSent::DryRun
            | FlagSent::NotUpgraded { .. }
            | FlagSent::Throttled
            | FlagSent::NotDeployed => {}
        }
    }

    /// Duty 10: pages the upgrade of latch venue `venue` to deploy `slot`, once per deploy
    /// (`known`: the slot the latch validated, or the one flagged during a review).
    fn page_upgrade(&mut self, w: &monitor::WatchedProgram, venue: u8, slot: u64, known: u64) {
        if self.upgrades_alerted.insert((venue, slot)) {
            self.alerts.push(Alert::VenueUpgraded {
                venue,
                program: w.program,
                known_slot: known,
                deploy_slot: slot,
            });
        }
    }

    /// Duty 10: books the keeper's `flag_venue_upgraded(venue)` of deploy `slot`, landed.
    fn flag_landed(&mut self, venue: u8, slot: u64, report: &mut TickReport) {
        self.metrics.venue_flags += 1;
        report.flagged.push((venue, slot));
        self.alerts.push(Alert::VenueUpgradeFlagged {
            venue,
            deploy_slot: slot,
        });
    }

    /// Duty 10: alerts, once per deploy, that the upgrade of latch venue `venue` to deploy `slot`
    /// could not be flagged (`detail`).
    fn flag_failed(&mut self, venue: u8, slot: u64, detail: String) {
        if self.flag_failures_alerted.insert((venue, slot)) {
            self.alerts.push(Alert::VenueFlagFailed { venue, detail });
        }
    }

    /// Duty 10: `flag_venue_upgraded(venue)` for deploy `slot`, simulated first and sent at the
    /// exit priority ceiling, at most once a second per deploy (a new deploy slot at once;
    /// [`Self::flag_due`]). Its simulation is the program's own judgment of the moved slot when
    /// it ran on a bank that holds that deploy ([`FlagSent::NotUpgraded`]: not an upgrade it
    /// would flag — AIR-01).
    async fn send_flag(
        &mut self,
        venue: u8,
        slot: u64,
        now: i64,
        report: &mut TickReport,
    ) -> FlagSent {
        if !self.flag_due(venue, slot, now) {
            return FlagSent::Throttled;
        }
        self.send_flag_now(venue, slot, now, report).await
    }

    /// Duty 10: whether `flag_venue_upgraded(venue)` of deploy `slot` may be attempted at `now`
    /// — not attempted for that deploy this second yet — and, if so, books the attempt.
    fn flag_due(&mut self, venue: u8, slot: u64, now: i64) -> bool {
        if self.flag_tried.get(&(venue, slot)) == Some(&now) {
            return false;
        }
        self.flag_tried.retain(|(v, _), _| *v != venue);
        self.flag_tried.insert((venue, slot), now);
        true
    }

    /// [`Self::send_flag`] without the once-a-second check (the caller booked the attempt).
    async fn send_flag_now(
        &mut self,
        venue: u8,
        slot: u64,
        now: i64,
        report: &mut TickReport,
    ) -> FlagSent {
        let ix = match ixs::flag_venue_upgraded(venue) {
            Ok(ix) => ix,
            Err(e) => {
                let detail = format!("flag_venue_upgraded({venue}): {e:#}");
                report.errors.push(detail.clone());
                return FlagSent::Failed(detail);
            }
        };
        let guard_key = pda::venue_guard();
        let sent = self.send_ixs(&[ix], &[guard_key], true, now).await;
        self.flag_outcome(venue, slot, sent)
    }

    /// Duty 10: what an attempt `sent` of `flag_venue_upgraded(venue)` for deploy `slot` came to.
    /// An attempt that cannot have judged that deploy is not throttled — the next tick (the next
    /// slot) tries again: `VenueNotUpgraded` from a bank before the deploy (AIR-01: a node behind
    /// the header read, where the deploy before it still stands), and a refusal because
    /// short.win was deployed in the slot (AIR-18).
    fn flag_outcome(&mut self, venue: u8, slot: u64, sent: anyhow::Result<Sent>) -> FlagSent {
        match sent {
            Ok(Sent::Landed { .. }) => FlagSent::Landed,
            Ok(Sent::DryRun { .. }) => FlagSent::DryRun,
            Ok(Sent::SimFailed { out, .. })
                if is(
                    out.err.as_ref().and_then(TxError::custom_code),
                    ShortwinError::VenueNotUpgraded,
                ) =>
            {
                if out.slot < slot {
                    self.flag_tried.remove(&(venue, slot));
                }
                FlagSent::NotUpgraded { at: out.slot }
            }
            // AIR-18: short.win runs again from the next slot: the next tick tries again.
            Ok(Sent::NotDeployed { .. }) => {
                self.flag_tried.remove(&(venue, slot));
                FlagSent::NotDeployed
            }
            Ok(Sent::SimFailed { out, .. }) | Ok(Sent::Failed { out }) => {
                FlagSent::Failed(format!("{:?}", out.err))
            }
            Ok(Sent::TooLarge) => FlagSent::Failed("transaction too large".into()),
            Ok(Sent::Deferred { .. }) => FlagSent::Failed("deferred".into()),
            Err(e) => FlagSent::Failed(format!("{e:#}")),
        }
    }

    /// Duty 10, pool admin actions ([`crate::monitor`]): every covered market's pool against
    /// the last tick; each change is an alert.
    fn watch_pools(&mut self, view: &ChainView) {
        for mv in &view.markets {
            // AIR-19: the hard cap of this market's S3 chain (its mark, when below the global).
            let caps = FeeCaps::of_market(&view.config, &mv.market);
            // The pump curve has no pool admin: its fee tier moves with its market cap and its
            // end is graduation (duty 6), not a lost route. A list-only market has no pool.
            if matches!(
                mv.kind,
                VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::None
            ) {
                continue;
            }
            let snap = monitor::snapshot(mv);
            let pool = mv.read_keys.first().copied().unwrap_or_default();
            let watch = self.pools.entry(mv.key).or_default();
            if watch.pool != Some(pool) {
                // A new pool (an adoption): its first sight, not a change of the old one.
                *watch = PoolWatch {
                    pool: Some(pool),
                    ..PoolWatch::default()
                };
            }
            let changes = watch.observe(mv.kind, snap, caps);
            for change in changes {
                self.metrics.market(mv.key).venue_admin_changes += 1;
                self.alerts.push(Alert::VenueAdmin {
                    market: mv.key,
                    venue: mv.kind,
                    pool,
                    change,
                });
            }
        }
    }

    /// Duty 10 on the pump curve markets read this tick (pump-final §7; [`monitor::curve_findings`]):
    /// a live curve breaking pump's invariant `vQ − rQ = ivqr`
    /// ([`Alert::CurveQuoteOffsetInconsistent`]), a live curve carrying post-complete fields
    /// ([`Alert::LiveCurvePostComplete`], G-3) and a migrating curve whose post-complete fields are
    /// not known ([`Alert::MigratingFvUndefined`]) are alerted once each while they last; a
    /// migrating curve's `ρ − 1` and `curve_base_ata` donation are the market's gauges
    /// (`synthetic_completion_delta_bps`, `curve_base_ata_donation`). Nothing is read for it: the
    /// ReadSet accounts and the program's venue state of the tick's view.
    fn watch_curves(&mut self, view: &ChainView) {
        for mv in &view.markets {
            let Some(f) = monitor::curve_findings(mv) else {
                if let Some(m) = self.metrics.markets.get_mut(&mv.key) {
                    m.synthetic_completion_delta_bps = None;
                    m.curve_base_ata_donation = None;
                }
                continue;
            };
            let key = mv.key;
            let findings = [
                (
                    CurveFinding::QuoteOffset,
                    f.quote_offset_inconsistent.map(|(vq, rq, ivqr)| {
                        Alert::CurveQuoteOffsetInconsistent {
                            market: key,
                            virtual_quote_reserves: vq,
                            real_quote_reserves: rq,
                            initial_virtual_quote_reserves: ivqr,
                        }
                    }),
                ),
                (
                    CurveFinding::LivePostComplete,
                    f.live_post_complete
                        .map(|(pcb, pcq)| Alert::LiveCurvePostComplete {
                            market: key,
                            post_complete_base_out: pcb,
                            post_complete_quote_in: pcq,
                        }),
                ),
                (
                    CurveFinding::MigratingFvUndefined,
                    f.migrating_fv_undefined
                        .map(|(pcb, pcq)| Alert::MigratingFvUndefined {
                            market: key,
                            post_complete_base_out: pcb,
                            post_complete_quote_in: pcq,
                        }),
                ),
            ];
            for (finding, alert) in findings {
                match alert {
                    Some(a) => {
                        if self.curve_alerted.insert((key, finding)) {
                            self.alerts.push(a);
                        }
                    }
                    None => {
                        self.curve_alerted.remove(&(key, finding));
                    }
                }
            }
            let m = self.metrics.market(key);
            m.synthetic_completion_delta_bps = f.synthetic_completion_delta_bps;
            m.curve_base_ata_donation = f.curve_base_ata_donation;
        }
    }

    /// Duty 11 (ARCH §7.1, §4.7; C1-12): every `oracle.interval_s` (≤ 30 s) the keeper reads the
    /// configured Pyth `PriceUpdateV2`; an update newer than the `OracleCache` that the program's
    /// own rules accept (`pyth::check` on the keeper's view: verification, feed, age, confidence,
    /// step) is cranked at once — simulated first, at the observation ceiling, outside every
    /// market's budget (a global line) — and a failed crank is retried every second. A newer
    /// update the rules refuse raises `Alert::OracleRefused` (once per reason until a crank
    /// lands); a cache older than `oracle.aging_alert_s` raises `Alert::OracleAging` (once per
    /// stall — per cache `publish_time`, M7D-6 — and hourly while it lasts).
    async fn oracle_duty(&mut self, view: &ChainView, report: &mut TickReport) {
        use shortwin_venue_state::pyth;
        let now = view.clock.unix_timestamp;
        let feed = view.config.pyth_sol_usd;
        if feed == Pubkey::default() {
            return;
        }
        let p = &view.config.params;
        let cached = view
            .oracle
            .as_ref()
            .map_or_else(pyth::Cached::default, |c| pyth::Cached {
                price: c.price,
                expo: c.expo,
                publish_time: c.publish_time,
            });
        let age = now.saturating_sub(cached.publish_time);
        // M7D-6: once per stall — per cache `publish_time` —, and hourly while one lasts. As
        // reviewed the alert was rate-limited to once an hour across stalls, which held while
        // the cache went stale after 3,600 s (alert at 1,800 s); with `oracle_stale_s` 600
        // (decision C) a second stall within ~50 min of an alert reached Tier C unalerted.
        if cached.publish_time > 0
            && age > self.cfg.oracle.aging_alert_s
            && self.oracle_aging_alerted.is_none_or(|(published, t)| {
                published != cached.publish_time || now.saturating_sub(t) >= 3_600
            })
        {
            self.oracle_aging_alerted = Some((cached.publish_time, now));
            self.alerts.push(Alert::OracleAging {
                age_s: age,
                publish_time: cached.publish_time,
            });
        }
        if now < self.oracle_next {
            return;
        }
        self.oracle_next = now.saturating_add(self.cfg.oracle.interval_s);
        self.metrics.oracle_reads += 1;
        let acc = match self.chain.accounts(&[feed]).await {
            Ok(mut v) => v.pop().flatten(),
            Err(e) => {
                report.errors.push(format!("oracle feed {feed}: {e:#}"));
                self.oracle_next = now.saturating_add(1);
                return;
            }
        };
        let update = match acc
            .as_ref()
            .map(|a| pyth::read(&a.owner.to_bytes(), &a.data))
        {
            Some(Ok(u)) => u,
            other => {
                let detail = match other {
                    None => "missing".to_string(),
                    Some(Err(e)) => format!("{e:?}"),
                    Some(Ok(_)) => unreachable!(),
                };
                if self.oracle_alerted.insert(format!("unreadable {detail}")) {
                    self.alerts.push(Alert::OracleUnreadable {
                        account: feed,
                        detail,
                    });
                }
                return;
            }
        };
        if update.publish_time <= cached.publish_time {
            return;
        }
        let limits = pyth::Limits {
            max_age_s: p.oracle_max_age_s,
            stale_s: p.oracle_stale_s,
            max_conf_bps: p.oracle_max_conf_bps,
            max_step_bps: p.oracle_max_step_bps,
        };
        if let Err(r) = pyth::check(&update, &view.config.pyth_feed_id, &cached, now, &limits) {
            self.metrics.oracle_refusals += 1;
            if self.oracle_alerted.insert(format!("{r:?}")) {
                self.alerts.push(Alert::OracleRefused {
                    publish_time: update.publish_time,
                    code: None,
                    reason: format!("{r:?}"),
                });
            }
            return;
        }
        let ix = ixs::crank_sol_usd(&feed);
        let result = match self
            .send_ixs(&[ix], &[pda::oracle_cache()], false, now)
            .await
        {
            Ok(Sent::Landed {
                out,
                format,
                bytes,
                unit_limit,
                ..
            }) => {
                self.metrics.oracle_cranks += 1;
                self.metrics.oracle_lamports = self.metrics.oracle_lamports.saturating_add(out.fee);
                self.oracle_alerted.clear();
                ExitResult::Landed {
                    outcome: out,
                    format,
                    extras: ExtrasSel::NONE,
                    unit_limit,
                    bytes,
                }
            }
            Ok(Sent::SimFailed { out, .. }) => ExitResult::Refused {
                code: out.err.as_ref().and_then(TxError::custom_code),
                outcome: out,
            },
            Ok(Sent::Failed { out }) => ExitResult::Failed { outcome: out },
            Ok(Sent::DryRun { out }) => ExitResult::DryRun { outcome: out },
            Ok(Sent::TooLarge) => ExitResult::Error("transaction too large".into()),
            Ok(Sent::Deferred { unit_limit }) => ExitResult::Deferred { unit_limit },
            Ok(Sent::NotDeployed { out, program }) => ExitResult::NotDeployed {
                program,
                outcome: out,
            },
            Err(e) => ExitResult::Error(format!("{e:#}")),
        };
        match &result {
            ExitResult::Refused { code, outcome } => {
                self.metrics.oracle_refusals += 1;
                // "Not newer" after a race with another cranker is benign.
                let benign = is(*code, ShortwinError::OracleStale);
                if !benign && self.oracle_alerted.insert(format!("code {code:?}")) {
                    self.alerts.push(Alert::OracleRefused {
                        publish_time: update.publish_time,
                        code: *code,
                        reason: format!("{:?}", outcome.err),
                    });
                }
                if !benign {
                    self.oracle_next = now.saturating_add(1);
                }
            }
            // AIR-18: transient (not alerted); the next tick (slot) cranks again.
            ExitResult::NotDeployed { .. } => self.oracle_next = now,
            ExitResult::Failed { .. } | ExitResult::Error(_) => {
                self.oracle_next = now.saturating_add(1);
            }
            _ => {}
        }
        report.oracle = Some((update.publish_time, result));
    }

    /// O17's automatic degraded mode (module docs; [`crate::config::CoveragePauseConfig`]): on
    /// every covered market, the `paused_coverage` flag should be set while the market is
    /// degraded or recovering and has open positions (or the flag is already set: it lifts only
    /// with the recovery), and clear otherwise. The designated pauser sends
    /// `set_coverage_pause(market, want)` — simulated first, at the exit priority ceiling,
    /// outside the observation budget, at most once a second per market, retried every second
    /// until the flag matches; others only alert (the `Degraded` alert pages the Emergency
    /// multisig). A market without positions is never paused: its opens wait for its own
    /// warm-up, and nothing rests on its coverage. ★OWNER-M7-3 (b) (2026-10-01): a cleared flag
    /// is set again only on a market this keeper degraded itself
    /// ([`crate::coverage::MarketTrack::own_degraded`]) — a pause it only adopted that the
    /// Emergency multisig cleared stays clear.
    async fn coverage_pauses(&mut self, view: &ChainView, report: &mut TickReport) {
        let now = view.clock.unix_timestamp;
        for mv in &view.markets {
            let key = mv.key;
            let Some(track) = self.tracks.get(&key) else {
                continue;
            };
            let flag = mv.market.has_flag(MARKET_FLAG_PAUSED_COVERAGE);
            self.metrics.market(key).paused_coverage = flag;
            let open = !mv.positions.is_empty() || mv.market.open_positions > 0;
            let mut want = track.state.is_degraded() && (open || flag);
            if want && !flag && !track.own_degraded {
                // Adopted, then cleared by someone else (the Emergency): not set again.
                want = false;
            }
            if want == flag {
                self.pause_alerted.retain(|(k, _)| *k != key);
                self.pause_intent.remove(&key);
                continue;
            }
            if !self.cfg.coverage_pause.enabled {
                continue;
            }
            let unavailable = match &self.coverage_authority {
                None => Some(PauseUnavailable::NoKey),
                Some(_) if view.config.coverage_authority == Pubkey::default() => {
                    Some(PauseUnavailable::AuthorityUnset)
                }
                Some(kp) if kp.pubkey() != view.config.coverage_authority => {
                    Some(PauseUnavailable::WrongKey {
                        configured: view.config.coverage_authority,
                        held: kp.pubkey(),
                    })
                }
                Some(_) => None,
            };
            if let Some(reason) = unavailable {
                if self
                    .pause_alerted
                    .insert((key, format!("{want}: {reason:?}")))
                {
                    self.alerts.push(Alert::CoveragePauseUnavailable {
                        market: key,
                        paused: want,
                        reason,
                    });
                }
                continue;
            }
            if self.pause_tried.get(&key) == Some(&now) {
                continue;
            }
            self.pause_tried.insert(key, now);
            self.pause_intent.insert(key, want);
            let Some(kp) = self.coverage_authority.clone() else {
                continue;
            };
            let ix = ixs::set_coverage_pause(&kp.pubkey(), &key, want);
            let sent = self
                .send_ixs_within(&[ix], &[kp.as_ref()], &[key], true, now, None, None)
                .await;
            let result = match sent {
                Ok(Sent::Landed {
                    out,
                    format,
                    bytes,
                    unit_limit,
                    ..
                }) => {
                    let m = self.metrics.market(key);
                    m.lamports = m.lamports.saturating_add(out.fee);
                    if want {
                        m.coverage_pauses += 1;
                    } else {
                        m.coverage_unpauses += 1;
                    }
                    m.paused_coverage = want;
                    // The flag is now `want` (M7-SEC-3: a later read that finds it otherwise, or
                    // a position opened meanwhile, was someone else's doing).
                    if let Some(t) = self.tracks.get_mut(&key) {
                        t.flag_seen = Some(want);
                    }
                    self.pause_alerted.retain(|(k, _)| *k != key);
                    self.alerts.push(Alert::CoveragePauseSent {
                        market: key,
                        paused: want,
                    });
                    ExitResult::Landed {
                        outcome: out,
                        format,
                        extras: ExtrasSel::NONE,
                        unit_limit,
                        bytes,
                    }
                }
                Ok(Sent::SimFailed { out, .. }) => ExitResult::Refused {
                    code: out.err.as_ref().and_then(TxError::custom_code),
                    outcome: out,
                },
                Ok(Sent::Failed { out }) => ExitResult::Failed { outcome: out },
                Ok(Sent::DryRun { out }) => ExitResult::DryRun { outcome: out },
                Ok(Sent::TooLarge) => ExitResult::Error("transaction too large".into()),
                Ok(Sent::Deferred { unit_limit }) => ExitResult::Deferred { unit_limit },
                Ok(Sent::NotDeployed { out, program }) => ExitResult::NotDeployed {
                    program,
                    outcome: out,
                },
                Err(e) => ExitResult::Error(format!("{e:#}")),
            };
            let failure = match &result {
                ExitResult::Refused { code, outcome } => {
                    Some((*code, format!("{:?}", outcome.err)))
                }
                ExitResult::Failed { outcome } => Some((
                    outcome.err.as_ref().and_then(TxError::custom_code),
                    format!("failed on chain: {:?}", outcome.err),
                )),
                ExitResult::Error(e) => Some((None, e.clone())),
                // AIR-18: transient (not alerted); the next tick (slot) sends it again.
                ExitResult::NotDeployed { .. } => {
                    self.pause_tried.remove(&key);
                    None
                }
                _ => None,
            };
            if let Some((code, detail)) = failure {
                self.metrics.coverage_pause_failures += 1;
                if self
                    .pause_alerted
                    .insert((key, format!("{want}: {code:?}")))
                {
                    self.alerts.push(Alert::CoveragePauseFailed {
                        market: key,
                        paused: want,
                        code,
                        detail,
                    });
                }
            }
            report.coverage_pauses.push(CoveragePauseRecord {
                market: key,
                paused: want,
                result,
            });
        }
    }

    /// Sends the queued cranks in batches (module docs). `charged`: the cranks go through the
    /// global budget (decision A, [`crate::budget`]) — presented exit-critical, baseline,
    /// discretionary (each by rank), then tier 2, then tier 3; a refused charge is not sent and,
    /// on a market with open positions, degrades it at once; an admitted one is booked at its
    /// landed share of the batch's fee, or returned when it does not land. A graduation step's
    /// crank, and pool selection's (`charged` false), are neither charged nor throttled.
    async fn send_cranks(
        &mut self,
        view: &ChainView,
        queue: Vec<(Pubkey, Due)>,
        report: &mut TickReport,
        charged: bool,
    ) {
        let now = view.clock.unix_timestamp;
        let slot = view.clock.slot;
        let params = self.budget_params(&view.config);
        let req_of = |charges: &BTreeMap<Pubkey, ChargeReq>, k: &Pubkey| {
            charges
                .get(k)
                .copied()
                .unwrap_or(ChargeReq::of(ChargeClass::Baseline))
        };
        let mut queue = queue;
        if charged {
            let ranks = self.budget.ranks();
            queue.sort_by_cached_key(|(k, _)| {
                self.budget.order(k, req_of(&self.crank_charges, k), &ranks)
            });
        }
        let mut ready: Vec<(Pubkey, Due)> = Vec::new();
        let mut held: BTreeMap<Pubkey, budget::Reservation> = BTreeMap::new();
        for (k, d) in queue {
            if !charged || self.free_cranks.contains(&k) {
                ready.push((k, d));
                continue;
            }
            let req = req_of(&self.crank_charges, &k);
            match self.budget.admit(now, k, req, &params, &mut self.alerts) {
                Admission::Admitted(r) => {
                    held.insert(k, r);
                    ready.push((k, d));
                }
                Admission::Refused { rank } => {
                    self.refused.insert(k);
                    self.metrics.market(k).budget_refused += 1;
                    // O17: a refused charge on a market with open positions is degraded mode at
                    // once (opens paused; exits, never budgeted, continue).
                    let open = view
                        .market(&k)
                        .is_some_and(|mv| !mv.positions.is_empty() || mv.market.open_positions > 0);
                    if open {
                        let spent = self.budget.spent;
                        let track = self.tracks.entry(k).or_default();
                        let was = track.state.is_degraded();
                        track.on_budget_exhausted(
                            k,
                            now,
                            spent,
                            params.global,
                            rank,
                            &mut self.alerts,
                        );
                        if !was {
                            self.metrics.market(k).budget_degraded += 1;
                        }
                    }
                    report.crank_failed.push((
                        k,
                        format!("refused by the global keeper budget ({req:?}, rank {rank:?})"),
                    ));
                }
            }
        }
        let why: BTreeMap<Pubkey, Due> = ready.iter().copied().collect();
        let mut pending: VecDeque<Pubkey> = ready.iter().map(|(k, _)| *k).collect();
        let mut alone: Vec<Pubkey> = Vec::new();
        // A cap below the format's batch size after a transaction came out too large.
        let mut cap = usize::MAX;
        loop {
            // The batch size of the format in use now (a v1 refusal switches to v0 mid-way).
            let size = match self.format(now) {
                TxFormat::V1 => self.cfg.batch_v1,
                TxFormat::V0 => self.cfg.batch_v0,
            }
            .min(cap)
            .max(1);
            let n = size.min(pending.len());
            if n == 0 {
                break;
            }
            // Cut the batch where its transaction would stop fitting the format (duty 7).
            let candidates: Vec<Instruction> = pending
                .iter()
                .take(n)
                .filter_map(|k| view.market(k).map(ixs::crank_price))
                .collect();
            let n = if candidates.len() == n {
                self.fitting(self.format(now), &candidates).max(1)
            } else {
                n
            };
            let batch: Vec<Pubkey> = pending.drain(..n).collect();
            let ixs: Vec<Instruction> = batch
                .iter()
                .filter_map(|k| view.market(k).map(ixs::crank_price))
                .collect();
            let requeue = |pending: &mut VecDeque<Pubkey>, keys: Vec<Pubkey>| {
                for k in keys.into_iter().rev() {
                    pending.push_front(k);
                }
            };
            match self.send_ixs(&ixs, &batch, false, now).await {
                Ok(Sent::Landed {
                    out,
                    format,
                    bytes,
                    accounts,
                    price,
                    unit_limit,
                }) => {
                    match format {
                        TxFormat::V1 => self.metrics.batches_v1 += 1,
                        TxFormat::V0 => self.metrics.batches_v0 += 1,
                    }
                    let n = u64::try_from(batch.len()).unwrap_or(1).max(1);
                    for k in &batch {
                        let share = TxOutcome {
                            fee: out.fee / n,
                            units_consumed: out.units_consumed / n,
                            ..TxOutcome::default()
                        };
                        if let Some(r) = held.remove(k) {
                            self.budget.settle(r, share.fee);
                        }
                        self.on_landed(*k, &share, now, slot, true);
                        self.metrics.market(*k).cranks += 1;
                        report.cranked.push(*k);
                        report.observed.push((*k, why.get(k).copied()));
                    }
                    self.note_gaps(&out, now);
                    report.batches.push(BatchRecord {
                        markets: batch,
                        format,
                        bytes,
                        accounts,
                        units: out.units_consumed,
                        fee: out.fee,
                        price,
                        unit_limit,
                    });
                }
                Ok(Sent::SimFailed { out, .. }) | Ok(Sent::Failed { out }) if batch.len() == 1 => {
                    report
                        .crank_failed
                        .push((batch[0], format!("{:?}", out.err)));
                }
                Ok(Sent::SimFailed { out, format }) => {
                    // Drop the failing market and crank it alone; batch the rest again.
                    // v0 prepends SetComputeUnitLimit and, with a price, SetComputeUnitPrice.
                    let offset = match format {
                        TxFormat::V1 => 0,
                        TxFormat::V0 => {
                            1 + usize::from(self.priority_price(&batch, false).await > 0)
                        }
                    };
                    match out
                        .err
                        .as_ref()
                        .and_then(TxError::instruction)
                        .and_then(|i| i.checked_sub(offset))
                    {
                        Some(i) if i < batch.len() => {
                            let mut rest = batch;
                            alone.push(rest.remove(i));
                            requeue(&mut pending, rest);
                        }
                        _ => alone.extend(batch),
                    }
                }
                Ok(Sent::Failed { out }) if matches!(out.err, Some(TxError::Dropped(_))) => {
                    // Not landed in time: a miss this second (the price escalates next tick);
                    // resending each market alone would only add load.
                    for k in batch {
                        report.crank_failed.push((k, format!("{:?}", out.err)));
                    }
                }
                Ok(Sent::Failed { .. }) => alone.extend(batch),
                Ok(Sent::TooLarge) => {
                    let size_now = match self.format(now) {
                        TxFormat::V1 => self.cfg.batch_v1,
                        TxFormat::V0 => self.cfg.batch_v0,
                    };
                    if batch.len() == 1 {
                        report
                            .crank_failed
                            .push((batch[0], "transaction too large".into()));
                    } else {
                        if size_now >= batch.len() {
                            // Same format, still too large: halve the batches from now on.
                            cap = batch.len() / 2;
                        }
                        requeue(&mut pending, batch);
                    }
                }
                Ok(Sent::DryRun { .. }) => {}
                Ok(Sent::Deferred { .. }) => {
                    for k in batch {
                        report.crank_failed.push((k, "deferred".into()));
                    }
                }
                // AIR-18: short.win is not deployed this slot (`crank_price` makes no venue CPI):
                // every crank left goes out in the next tick (slot), none alone now.
                Ok(Sent::NotDeployed { out, .. }) => {
                    let why = format!("program not deployed this slot: {:?}", out.err);
                    for k in batch.into_iter().chain(pending.drain(..)) {
                        report.crank_failed.push((k, why.clone()));
                    }
                    for k in alone.drain(..) {
                        report.crank_failed.push((k, why.clone()));
                    }
                }
                Err(e) => {
                    for k in batch {
                        report.crank_failed.push((k, format!("{e:#}")));
                    }
                }
            }
        }
        let mut alone = alone.into_iter();
        while let Some(k) = alone.next() {
            let Some(mv) = view.market(&k) else { continue };
            let ix = ixs::crank_price(mv);
            match self.send_ixs(&[ix], &[k], false, now).await {
                Ok(Sent::Landed {
                    out,
                    format,
                    bytes,
                    accounts,
                    price,
                    unit_limit,
                }) => {
                    if let Some(r) = held.remove(&k) {
                        self.budget.settle(r, out.fee);
                    }
                    self.on_landed(k, &out, now, slot, true);
                    self.metrics.market(k).cranks += 1;
                    report.cranked.push(k);
                    report.observed.push((k, why.get(&k).copied()));
                    report.batches.push(BatchRecord {
                        markets: vec![k],
                        format,
                        bytes,
                        accounts,
                        units: out.units_consumed,
                        fee: out.fee,
                        price,
                        unit_limit,
                    });
                }
                Ok(Sent::SimFailed { out, .. }) | Ok(Sent::Failed { out }) => {
                    report.crank_failed.push((k, format!("{:?}", out.err)));
                }
                Ok(Sent::TooLarge) => report
                    .crank_failed
                    .push((k, "transaction too large".into())),
                Ok(Sent::Deferred { .. }) => report.crank_failed.push((k, "deferred".into())),
                Ok(Sent::DryRun { .. }) => {}
                // AIR-18: as in a batch: the rest goes out in the next tick (slot).
                Ok(Sent::NotDeployed { out, .. }) => {
                    let why = format!("program not deployed this slot: {:?}", out.err);
                    report.crank_failed.push((k, why.clone()));
                    for k in alone.by_ref() {
                        report.crank_failed.push((k, why.clone()));
                    }
                }
                Err(e) => report.crank_failed.push((k, format!("{e:#}"))),
            }
        }
        // Admitted cranks that did not land: nothing is booked.
        for (_, r) in held {
            self.budget.release(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pump-final §7: what an exit attempt says about its curve TradeExtras format. A landed,
    /// dry-run or deferred exit simulated past account parsing; a refusal or an on-chain failure
    /// is judged by its outcome — so an exit simulated on 1.0.0 that fails on chain on the account
    /// parser of a build upgraded in between tries the other format; a transport error, a no-op
    /// probe or a program not deployed in the slot say nothing.
    #[test]
    fn exit_results_tell_the_curve_format() {
        let failed = |code: u32| TxOutcome {
            err: Some(TxError::Custom { index: 2, code }),
            logs: vec![format!(
                "Program {} failed: custom program error",
                shortwin::ID
            )],
            ..TxOutcome::default()
        };
        let mismatch = u32::from(ShortwinError::VenueAccountMismatch);
        let landed = ExitResult::Landed {
            outcome: TxOutcome::default(),
            format: TxFormat::V1,
            extras: ExtrasSel::NONE,
            unit_limit: 1,
            bytes: 1,
        };
        for (r, want) in [
            (landed, FormatEvidence::Parsed),
            (
                ExitResult::DryRun {
                    outcome: TxOutcome::default(),
                },
                FormatEvidence::Parsed,
            ),
            (
                ExitResult::Deferred { unit_limit: 1 },
                FormatEvidence::Parsed,
            ),
            (
                ExitResult::Failed {
                    outcome: failed(mismatch),
                },
                FormatEvidence::ParseFailed,
            ),
            (
                ExitResult::Refused {
                    code: Some(mismatch),
                    outcome: failed(mismatch),
                },
                FormatEvidence::ParseFailed,
            ),
            (
                ExitResult::Failed {
                    outcome: failed(u32::from(ShortwinError::ExitBoundNotMet)),
                },
                FormatEvidence::Parsed,
            ),
            (
                ExitResult::Failed {
                    outcome: failed(u32::from(ShortwinError::VenueMigrating)),
                },
                FormatEvidence::Unknown,
            ),
            (ExitResult::NoOp, FormatEvidence::Unknown),
            (ExitResult::Error("rpc".into()), FormatEvidence::Unknown),
            (
                ExitResult::NotDeployed {
                    program: shortwin::ID,
                    outcome: TxOutcome::default(),
                },
                FormatEvidence::Unknown,
            ),
        ] {
            assert_eq!(r.format_evidence(), want, "{r:?}");
        }
    }

    /// pump-final §15 step 1: only 1.0.0's `CurveStraddle` on a V2 curve exit waits for the
    /// upgrade (`sol_settle`, unpaged); the same code on a v3 exit, another code on a V2 exit, or
    /// a refusal without a code is an ordinary refusal.
    #[test]
    fn only_a_v2_exits_curve_straddle_waits_for_the_upgrade() {
        let straddle = Some(u32::from(ShortwinError::CurveStraddle));
        assert!(waits_for_upgrade(straddle, Some(CurveExtrasFormat::V2)));
        assert!(!waits_for_upgrade(straddle, Some(CurveExtrasFormat::V3)));
        assert!(!waits_for_upgrade(straddle, None));
        for code in [
            Some(u32::from(ShortwinError::VenueAccountMismatch)),
            Some(u32::from(ShortwinError::ExitBoundNotMet)),
            None,
        ] {
            assert!(!waits_for_upgrade(code, Some(CurveExtrasFormat::V2)));
        }
    }

    /// `CurveExtrasFormatUnknown`'s detail: a refusal's error and the program's own error line
    /// (its source line tells which parser refused), never the whole outcome — no slot, no logs,
    /// so the alert's key and size stay put however long the failure lasts.
    #[test]
    fn a_format_refusal_reads_as_its_error_and_the_programs_error_line() {
        let code = u32::from(ShortwinError::BadRemainingAccounts);
        let anchor = format!(
            "AnchorError thrown in programs/shortwin/src/venues/mod.rs:162. Error Code: \
             BadRemainingAccounts. Error Number: {code}. Error Message: x."
        );
        let outcome = |logs: Vec<String>, slot: u64| TxOutcome {
            err: Some(TxError::Custom { index: 1, code }),
            logs,
            units_consumed: slot / 3,
            slot,
            ..TxOutcome::default()
        };
        let logs = vec![
            format!("Program {} invoke [1]", shortwin::ID),
            format!("Program log: {anchor}"),
            format!(
                "Program {} failed: custom program error: {code:#x}",
                shortwin::ID
            ),
        ];
        let want = format!("Some(Custom {{ index: 1, code: {code} }}) ({anchor})");
        for slot in [100, 200] {
            let r = ExitResult::Refused {
                code: Some(code),
                outcome: outcome(logs.clone(), slot),
            };
            assert_eq!(refusal_brief(&r), want);
            let r = ExitResult::Failed {
                outcome: outcome(logs.clone(), slot),
            };
            assert_eq!(refusal_brief(&r), want);
        }
        let bare = ExitResult::Refused {
            code: Some(code),
            outcome: outcome(Vec::new(), 7),
        };
        assert_eq!(
            refusal_brief(&bare),
            format!("Some(Custom {{ index: 1, code: {code} }})")
        );
    }

    /// AIR-18: refusals at most `NOT_DEPLOYED_RUN_GAP_S` apart are one run, paged once when it has
    /// lasted `NOT_DEPLOYED_ALERT_S`; a longer pause starts a new run, paged again only when it too
    /// lasts that long.
    #[test]
    fn a_run_of_refusals_is_paged_once_after_it_lasted_the_alert_time() {
        let mut run = NotDeployedRun::default();
        let t0 = 1_790_000_000;
        let paged: Vec<i64> = (0..=NOT_DEPLOYED_ALERT_S + 10)
            .filter(|s| run.note(t0 + s))
            .collect();
        assert_eq!(paged, vec![NOT_DEPLOYED_ALERT_S]);
        assert_eq!(run.since, t0);
        assert_eq!(
            run.refusals,
            u64::try_from(NOT_DEPLOYED_ALERT_S).unwrap() + 11
        );
        // Refusals `NOT_DEPLOYED_RUN_GAP_S` apart keep the run; one more second of pause ends it.
        let last = run.last;
        assert!(!run.note(last + NOT_DEPLOYED_RUN_GAP_S));
        assert_eq!(run.since, t0, "the same run");
        let later = run.last + NOT_DEPLOYED_RUN_GAP_S + 1;
        assert!(!run.note(later));
        assert_eq!((run.since, run.refusals, run.alerted), (later, 1, false));
        // A new run is paged when it too lasts the alert time; a clock going back restarts it.
        let mut paged = Vec::new();
        for s in (0..=NOT_DEPLOYED_ALERT_S).step_by(5) {
            if run.note(later + s) {
                paged.push(s);
            }
        }
        assert_eq!(paged, vec![NOT_DEPLOYED_ALERT_S]);
        assert!(!run.note(later));
        assert_eq!((run.since, run.refusals), (later, 1));
    }
}
