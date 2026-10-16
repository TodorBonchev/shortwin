//! What a run did: actions by kind, short.win instructions landed and refused (by error), the
//! events emitted (a branch-coverage proxy: `Liquidate`, `SolSettle` S1/S2/S3, `CoverageGap`,
//! `PoolSwitched`, `VenueStateChanged`, …), and **anomalies** — outcomes that are not invariant
//! violations but that a reviewer should see (a program panic inside a short.win instruction, a
//! compute-budget exhaustion, a framework error on a well-formed instruction; an I14 replacement
//! the program accepted was one until M10, when it became a failure). [`Ctx::send`] records every
//! transaction the fuzzer sends.

use std::collections::BTreeMap;

use anchor_lang::Discriminator;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use litesvm::types::TransactionMetadata;
use shortwin_litesvm_tests::{Env, TxResult, events};
use solana_instruction::error::InstructionError;
use solana_keypair::Keypair;
use solana_transaction_error::TransactionError;

/// An anomaly class: how often, and the first place it was seen.
#[derive(Clone, Debug, Default)]
pub struct Anomaly {
    pub count: u64,
    /// `profile seed #step action` of the first occurrence (a reproducer).
    pub first: String,
    /// The first occurrence's detail (error, a log excerpt).
    pub detail: String,
}

/// The deficit tripwire's reach (M10, [`crate::shortfall`]): every negative SOL balance a lender
/// covered before its withdrawal, against its pro-rata share of the realised shortfall.
#[derive(Clone, Copy, Debug, Default)]
pub struct Deficits {
    /// Deficits checked against their bound.
    pub checked: u64,
    /// Runs with at least one.
    pub runs: u64,
    /// The largest deficit (lamports), that lender's bound then and its cumulative bound over
    /// the run (no restarts, [`crate::shortfall::Ledger`]).
    pub largest: u64,
    pub largest_bound: u128,
    pub largest_total_bound: u128,
    /// The largest `deficit / bound` (ppm) over the deficits with a bound above 0 (one with a
    /// bound of 0 must stay within the dust).
    pub tightest_ppm: u64,
    /// The smallest `deficit / bound` (ppm) over the deficits above the dust: how loose the
    /// bound gets.
    pub loosest_ppm: Option<u64>,
}

impl Deficits {
    fn merge(&mut self, o: &Deficits) {
        self.checked += o.checked;
        self.runs += o.runs;
        if o.largest > self.largest {
            self.largest = o.largest;
            self.largest_bound = o.largest_bound;
            self.largest_total_bound = o.largest_total_bound;
        }
        self.tightest_ppm = self.tightest_ppm.max(o.tightest_ppm);
        self.loosest_ppm = match (self.loosest_ppm, o.loosest_ppm) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Counters of one run, or merged over many.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub runs: u64,
    pub actions: BTreeMap<String, u64>,
    pub skipped: BTreeMap<String, u64>,
    pub txs: u64,
    pub txs_failed: u64,
    /// short.win instruction → `[landed, refused]`.
    pub ix: BTreeMap<String, [u64; 2]>,
    /// `instruction:Error` → count.
    pub errors: BTreeMap<String, u64>,
    /// short.win event → count (`SolSettle` by condition).
    pub events: BTreeMap<String, u64>,
    pub anomalies: BTreeMap<String, Anomaly>,
    /// Where the fuzzer is (for anomaly reproducers): `profile seed #step action`.
    pub cursor: String,
    pub hook_checkpoints: u64,
    pub wind_downs: u64,
    /// The deficit tripwire (M10).
    pub deficits: Deficits,
    /// The wind-down's claim liveness (M10): the largest `lender_sol` and the largest positive
    /// `pending` left unpaid after the second claim pass (lamports).
    pub wind_down_lender_sol_max: u64,
    pub wind_down_unpaid_max: u64,
    /// KI-66's never-returns model ([`crate::world::World::absent_owners`]): the largest total
    /// the present lenders stayed owed while lenders held behind a rebase record were away.
    pub wind_down_absent_owed_max: u64,
}

impl Stats {
    /// Adds `o` into `self`.
    pub fn merge(&mut self, o: &Stats) {
        self.runs += o.runs;
        for (k, v) in &o.actions {
            *self.actions.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.skipped {
            *self.skipped.entry(k.clone()).or_default() += v;
        }
        self.txs += o.txs;
        self.txs_failed += o.txs_failed;
        for (k, v) in &o.ix {
            let e = self.ix.entry(k.clone()).or_default();
            e[0] += v[0];
            e[1] += v[1];
        }
        for (k, v) in &o.errors {
            *self.errors.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.events {
            *self.events.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.anomalies {
            let e = self.anomalies.entry(k.clone()).or_default();
            if e.count == 0 {
                e.first = v.first.clone();
                e.detail = v.detail.clone();
            }
            e.count += v.count;
        }
        self.hook_checkpoints += o.hook_checkpoints;
        self.wind_downs += o.wind_downs;
        self.deficits.merge(&o.deficits);
        self.wind_down_lender_sol_max = self
            .wind_down_lender_sol_max
            .max(o.wind_down_lender_sol_max);
        self.wind_down_unpaid_max = self.wind_down_unpaid_max.max(o.wind_down_unpaid_max);
        self.wind_down_absent_owed_max = self
            .wind_down_absent_owed_max
            .max(o.wind_down_absent_owed_max);
    }

    /// Records an anomaly of class `class` at the cursor.
    pub fn anomaly(&mut self, class: &str, detail: String) {
        let e = self.anomalies.entry(class.to_string()).or_default();
        if e.count == 0 {
            e.first = self.cursor.clone();
            e.detail = detail;
        }
        e.count += 1;
    }

    /// Total actions executed (not skipped).
    pub fn executed(&self) -> u64 {
        self.actions.values().sum()
    }

    /// Records one transaction.
    pub fn record(&mut self, ixs: &[Instruction], res: &TxResult) {
        self.txs += 1;
        let failed_at = match res {
            Ok(_) => None,
            Err(f) => {
                self.txs_failed += 1;
                match &f.err {
                    TransactionError::InstructionError(i, _) => Some(usize::from(*i)),
                    _ => Some(usize::MAX),
                }
            }
        };
        let logs: &[String] = match res {
            Ok(m) => &m.logs,
            Err(f) => &f.meta.logs,
        };
        for (i, ix) in ixs.iter().enumerate() {
            if ix.program_id != shortwin::ID {
                continue;
            }
            let name = ix_name(&ix.data);
            let slot = self.ix.entry(name.to_string()).or_default();
            match failed_at {
                None => slot[0] += 1,
                Some(at) if at == i => {
                    slot[1] += 1;
                    let err = match res {
                        Err(f) => error_name(&f.err, logs),
                        Ok(_) => String::new(),
                    };
                    *self.errors.entry(format!("{name}:{err}")).or_default() += 1;
                }
                Some(_) => {}
            }
        }
        if let Ok(meta) = res {
            self.record_events(meta);
        }
    }

    fn record_events(&mut self, meta: &TransactionMetadata) {
        for payload in events::raw(meta) {
            if payload.len() < 8 {
                continue;
            }
            let name = event_name(&payload[..8]);
            if name == "SolSettle" {
                for ev in events::of::<shortwin::events::SolSettle>(meta) {
                    *self
                        .events
                        .entry(format!("SolSettle(S{})", ev.condition))
                        .or_default() += 1;
                }
                continue;
            }
            if name != "unknown_event" {
                *self.events.entry(name.to_string()).or_default() += 1;
            }
        }
        for _ in events::logged::<shortwin::events::CoverageGap>(meta) {
            *self.events.entry("CoverageGap".into()).or_default() += 1;
        }
    }
}

/// The number of short.win instructions (`programs/shortwin/src/lib.rs`; [`ix_name`]'s table).
pub const IX_COUNT: usize = 30;

/// A short.win instruction's name from its Anchor discriminator.
pub fn ix_name(data: &[u8]) -> &'static str {
    use shortwin::instruction as i;
    let d = data.get(..8).unwrap_or(&[]);
    macro_rules! table {
        ($($t:ident => $n:literal),* $(,)?) => {
            $(if d == i::$t::DISCRIMINATOR { return $n; })*
        };
    }
    table!(
        InitializeConfig => "initialize_config",
        ProposeConfig => "propose_config",
        ApplyConfig => "apply_config",
        CancelConfig => "cancel_config",
        SetPause => "set_pause",
        SetCoveragePause => "set_coverage_pause",
        DisableVenue => "disable_venue",
        FlagVenueUpgraded => "flag_venue_upgraded",
        RevalidateVenue => "revalidate_venue",
        SweepProtocolFees => "sweep_protocol_fees",
        CreateMarket => "create_market",
        FlagUnsafeMint => "flag_unsafe_mint",
        Deposit => "deposit",
        Withdraw => "withdraw",
        ClaimSol => "claim_sol",
        CoverDeficit => "cover_deficit",
        SyncLender => "sync_lender",
        CrankSolUsd => "crank_sol_usd",
        OpenShort => "open_short",
        CloseShort => "close_short",
        AddMargin => "add_margin",
        Liquidate => "liquidate",
        Expire => "expire",
        SolSettle => "sol_settle",
        CrankPrice => "crank_price",
        SweepTrader => "sweep_trader",
        ProposePool => "propose_pool",
        ExecutePoolSwitch => "execute_pool_switch",
        SetRouteBroken => "set_route_broken",
        SetVenueExtras => "set_venue_extras",
    );
    "unknown"
}

/// A short.win event's name from its discriminator.
pub fn event_name(d: &[u8]) -> &'static str {
    use shortwin::events as e;
    macro_rules! table {
        ($($t:ident),* $(,)?) => {
            $(if d == e::$t::DISCRIMINATOR { return stringify!($t); })*
        };
    }
    table!(
        MarketCreated,
        Open,
        Close,
        Liquidate,
        Expire,
        SolSettle,
        SettleArmed,
        SettleDisarmed,
        EpochRolled,
        ProtocolFeesSwept,
        MarginAdded,
        PriceCranked,
        VenueStateChanged,
        RouteChainChanged,
        TraderSwept,
        VenueRewardsClaimed,
        MintSafetyTripped,
        VenueUpgradeFlagged,
        VenueRevalidated,
        CoverageGap,
        CoveragePauseSet,
        ConfigProposed,
        ConfigApplied,
        ConfigCancelled,
        PauseSet,
        VenueDisabled,
        RouteBrokenSet,
        VenueExtrasSet,
        Deposit,
        Withdraw,
        ClaimSol,
        DeficitCovered,
        LenderSynced,
        PoolProposed,
        PoolSwitched,
        PoolSwitchCancelled,
    );
    "unknown_event"
}

/// A failure's name: the Anchor error name from the logs (`Error Code: X.`), else the
/// transaction error.
pub fn error_name(err: &TransactionError, logs: &[String]) -> String {
    for line in logs.iter().rev() {
        if let Some(rest) = line.split("Error Code: ").nth(1)
            && let Some(name) = rest.split('.').next()
        {
            return name.to_string();
        }
    }
    match err {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => {
            format!("Custom({c})")
        }
        TransactionError::InstructionError(_, e) => format!("{e:?}"),
        e => format!("{e:?}"),
    }
}

/// The failing instruction's index and error, if the transaction failed inside one.
pub fn failure(res: &TxResult) -> Option<(usize, &InstructionError)> {
    match res {
        Err(f) => match &f.err {
            TransactionError::InstructionError(i, e) => Some((usize::from(*i), e)),
            _ => None,
        },
        Ok(_) => None,
    }
}

/// The short.win error code of a failure inside a short.win instruction.
pub fn shortwin_code(ixs: &[Instruction], res: &TxResult) -> Option<u32> {
    let (i, e) = failure(res)?;
    if ixs.get(i).map(|ix| ix.program_id) != Some(shortwin::ID) {
        return None;
    }
    match e {
        InstructionError::Custom(c) => Some(*c),
        _ => None,
    }
}

/// Whether a failure is inside a short.win instruction.
pub fn failed_in_shortwin(ixs: &[Instruction], res: &TxResult) -> bool {
    failure(res).is_some_and(|(i, _)| ixs.get(i).map(|ix| ix.program_id) == Some(shortwin::ID))
}

/// The world's sending context: the VM and the counters. Every fuzzer transaction goes through
/// [`Ctx::send`], i.e. through `Env::send` and its invariant hooks.
pub struct Ctx<'a> {
    pub env: &'a mut Env,
    pub stats: &'a mut Stats,
}

impl Ctx<'_> {
    /// Sends `ixs` (hooks on), records it, and flags program-level faults of short.win
    /// instructions as anomalies: a panic, a compute-budget exhaustion, a framework (non-
    /// short.win) error code on an instruction the fuzzer built well-formed.
    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> TxResult {
        let res = self.env.send(ixs, signers);
        self.stats.record(ixs, &res);
        self.stats.hook_checkpoints = self.env.hook_checkpoints;
        if let Err(f) = &res
            && failed_in_shortwin(ixs, &res)
        {
            let logs = f.meta.logs.join("\n");
            if logs.contains("panicked at") {
                self.stats
                    .anomaly("program-panic", excerpt(&f.meta.logs, "panicked at"));
            }
            match failure(&res) {
                Some((i, InstructionError::ComputationalBudgetExceeded))
                | Some((i, InstructionError::ProgramFailedToComplete)) => {
                    let name = ix_name(&ixs[i].data);
                    self.stats.anomaly(
                        &format!("compute-exhausted:{name}"),
                        excerpt(&f.meta.logs, "consumed"),
                    );
                }
                Some((i, InstructionError::Custom(c)))
                    if !(6000..6000 + crate::world::ERR_NAMES.len() as u32).contains(c) =>
                {
                    let name = ix_name(&ixs[i].data);
                    // A venue's (or a token program's) own error surfacing through short.win's
                    // CPI is the venue refusing (counted); only short.win's own non-program
                    // codes (Anchor's framework errors) are anomalies.
                    let me = shortwin::ID.to_string();
                    let cpi_failed =
                        f.meta.logs.iter().any(|l| {
                            l.contains("failed: custom program error") && !l.contains(&me)
                        });
                    if cpi_failed {
                        *self
                            .stats
                            .events
                            .entry(format!(
                                "venue-cpi-refusal:{name}:{}",
                                error_name(&f.err, &f.meta.logs)
                            ))
                            .or_default() += 1;
                        return res;
                    }
                    // KI-67: a claim_sol after a claim_sol of the same position in one
                    // transaction finds it closed when the first claim emptied it (the claim
                    // action's oracle checks that this happens exactly then).
                    let not_init = u32::from(anchor_lang::error::ErrorCode::AccountNotInitialized);
                    if name == "claim_sol"
                        && *c == not_init
                        && i > 0
                        && ix_name(&ixs[i - 1].data) == "claim_sol"
                        && ixs[i - 1].accounts == ixs[i].accounts
                    {
                        *self
                            .stats
                            .events
                            .entry("claim-after-close-refused".into())
                            .or_default() += 1;
                        return res;
                    }
                    self.stats.anomaly(
                        &format!(
                            "framework-error:{name}:{}",
                            error_name(&f.err, &f.meta.logs)
                        ),
                        tail(&f.meta.logs, 40),
                    );
                }
                _ => {}
            }
        }
        res
    }

    /// [`Ctx::send`] with a compute-unit limit instruction first when `cu` is set.
    pub fn send_cu(
        &mut self,
        cu: Option<u32>,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> TxResult {
        match cu {
            Some(units) => {
                let mut all = vec![shortwin_litesvm_tests::pump::cu_limit_ix(units)];
                all.extend_from_slice(ixs);
                self.send(&all, signers)
            }
            None => self.send(ixs, signers),
        }
    }

    /// The fee payer's key.
    pub fn payer(&self) -> Pubkey {
        use solana_signer::Signer;
        self.env.payer.pubkey()
    }
}

/// The last `n` log lines.
pub fn tail(logs: &[String], n: usize) -> String {
    logs[logs.len().saturating_sub(n)..].join("\n")
}

/// The log lines around the first one containing `needle` (at most 12).
pub fn excerpt(logs: &[String], needle: &str) -> String {
    let at = logs.iter().position(|l| l.contains(needle)).unwrap_or(0);
    let lo = at.saturating_sub(6);
    let hi = (at + 6).min(logs.len());
    logs[lo..hi].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`IX_COUNT`] and [`ix_name`]'s table cover every instruction of the program's
    /// `#[program]` module.
    #[test]
    fn every_instruction_is_named() {
        let lib = include_str!("../../../programs/shortwin/src/lib.rs");
        let me = include_str!("stats.rs");
        let fns: Vec<&str> = lib
            .lines()
            .filter_map(|l| l.strip_prefix("    pub fn "))
            .filter_map(|l| l.split(['(', '<']).next())
            .collect();
        assert_eq!(fns.len(), IX_COUNT, "{fns:?}");
        for f in fns {
            assert!(
                me.contains(&format!("=> \"{f}\"")),
                "{f} missing from ix_name"
            );
        }
        assert_eq!(ix_name(&[0; 8]), "unknown");
    }
}
