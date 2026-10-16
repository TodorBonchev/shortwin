//! Keeper metrics (ARCHITECTURE §7.1 duty 8: per-market coverage seconds, CU and lamports
//! spent; "Keys and ops": missed liquidations, coverage SLO misses, budget burn), rendered in
//! the Prometheus text format by [`Metrics::render`].

use std::collections::BTreeMap;
use std::fmt::Write;

use anchor_lang::prelude::Pubkey;

/// One market's counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarketMetrics {
    /// Seconds in which the keeper landed at least one observation.
    pub covered_seconds: u64,
    /// Observations landed (cranks and exits carrying the ReadSet).
    pub observations: u64,
    /// `crank_price` instructions landed.
    pub cranks: u64,
    pub liquidations: u64,
    pub expiries: u64,
    /// `sol_settle` calls landed (arming re-checks included).
    pub settle_calls: u64,
    /// Settlements executed (S1, S2, S3).
    pub settlements: u64,
    /// Compute units attributed to this market.
    pub cu: u64,
    /// Lamports spent on this market (fees, split evenly across a batch).
    pub lamports: u64,
    /// Seconds that needed an observation and got none.
    pub misses: u64,
    /// Deviation bursts started.
    pub bursts: u64,
    /// Liquidity-triggered per-second coverage windows started (FITS-2 (b)).
    pub liq_covers: u64,
    /// `CoverageGap` events seen.
    pub coverage_gaps: u64,
    /// M7-SEC-1 (a): `CoverageGap` events at most `idle_interval_s + gap_tolerance_s` long
    /// (landing jitter of the idle cadence), which did not degrade the market.
    pub coverage_gaps_tolerated: u64,
    /// M7-SEC-2: foreign prices off the keeper's view that its next read still showed (genuine
    /// moves: no burst).
    pub foreign_moves: u64,
    /// M7-SEC-3: opens that landed while the keeper held the market degraded.
    pub opens_while_degraded: u64,
    /// Admin actions seen on the market's pool or CPMM config (duty 10).
    pub venue_admin_changes: u64,
    /// `propose_pool` calls landed (duties 5 and 12).
    pub pool_proposals: u64,
    /// `execute_pool_switch` calls landed (duty 5).
    pub pool_executions: u64,
    /// Pool adoptions seen (the keeper's or anyone's).
    pub pool_switches: u64,
    /// pump `migrate_v2` calls of the keeper landed (duty 6).
    pub migrations: u64,
    /// pump-final §7: the keeper's landed liquidations and expiries that bought past a live
    /// curve's `rT` by pump's synthetic migration (`B̂_v3`), completing the curve.
    pub synthetic_exits: u64,
    /// `→ PumpSwap` transitions the keeper landed (duty 6, M6).
    pub transitions: u64,
    /// `sweep_trader` calls of the keeper landed (duties 6 and 13), with the venue cashback claims
    /// among them.
    pub sweeps: u64,
    pub cashback_claims: u64,
    /// T-CASCADE: exits held to the next slot by `budget.market_cu_per_slot`.
    pub exits_deferred: u64,
    /// O17: `set_coverage_pause(true)` / `(false)` calls of the keeper landed.
    pub coverage_pauses: u64,
    pub coverage_unpauses: u64,
    /// O17: times a refused charge of the global budget degraded the market (with open
    /// positions).
    pub budget_degraded: u64,
    /// Decision A (2026-10-01; ARCH §7.1 duty 8): charges of the market the global budget refused.
    pub budget_refused: u64,
    /// The market's value at risk `V` (lamports) at its last read.
    pub var_lamports: u64,
    /// Its class: 1 covered, 2 below the floor, 3 without positions.
    pub budget_tier: u64,
    /// Its rank among the covered markets (1 = the most value at risk; 0: not covered).
    pub budget_rank: u64,
    /// Its unspent entitlement `U` (lamports).
    pub budget_unspent_lamports: u64,
    /// Today's budget spend by charge class: exit-critical, baseline, discretionary (lamports).
    pub budget_spent_exit_lamports: u64,
    pub budget_spent_baseline_lamports: u64,
    pub budget_spent_discretionary_lamports: u64,
    /// The market's `paused_coverage` flag as last read (1) or not (0).
    pub paused_coverage: bool,
    /// Degraded (1) or not (0).
    pub degraded: bool,
    /// Duty 10 (pump-final §7) on a `PumpMigrating` market, at its last read: `ρ − 1` in bps —
    /// the price of the pool `migrate_v2` will create (after a synthetic pool part: the live
    /// `curve_base_ata`, `rQ − fee + pcQ`) over the same curve's without the pool part or a
    /// donation (pump-final §5.4; 0 without a pool part); `None` off a migrating market or
    /// when it does not compute (the post-complete fields not known, an unreadable input).
    pub synthetic_completion_delta_bps: Option<i64>,
    /// Duty 10 on a `PumpMigrating` market: the tokens donated to `curve_base_ata`,
    /// `A + pcB − 206.9e12` (pump-final §5.4; negative only if pump's own accounting broke);
    /// `None` off a migrating market.
    pub curve_base_ata_donation: Option<i128>,
    /// The last second covered.
    last_covered: i64,
}

impl MarketMetrics {
    /// Counts `now` as covered (once per second).
    pub fn cover(&mut self, now: i64) {
        if self.last_covered != now {
            self.last_covered = now;
            self.covered_seconds = self.covered_seconds.saturating_add(1);
        }
    }
}

/// U28's bucket bounds (ARCH §12): `B̂_live/B̂_ref` in bps — ECON §5.7's battery assumes ±0.3 %
/// and ±1 % between a live quote and the one-second-old reference.
pub const B_RATIO_BUCKETS_BPS: [u64; 9] = [
    9_700, 9_900, 9_970, 10_000, 10_030, 10_100, 10_300, 10_500, 11_000,
];

/// A histogram over [`B_RATIO_BUCKETS_BPS`] (Prometheus `histogram`; the last count is above the
/// largest bound).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Histogram {
    pub counts: [u64; 10],
    pub sum: u64,
    pub count: u64,
}

impl Histogram {
    /// Records `v`.
    pub fn record(&mut self, v: u64) {
        let i = B_RATIO_BUCKETS_BPS
            .iter()
            .position(|b| v <= *b)
            .unwrap_or(B_RATIO_BUCKETS_BPS.len());
        self.counts[i] += 1;
        self.sum = self.sum.saturating_add(v);
        self.count += 1;
    }

    fn render(&self, s: &mut String, name: &str, label: &str) {
        let mut cum = 0;
        for (i, b) in B_RATIO_BUCKETS_BPS.iter().enumerate() {
            cum += self.counts[i];
            let _ = writeln!(s, "{name}_bucket{{{label},le=\"{b}\"}} {cum}");
        }
        let _ = writeln!(s, "{name}_bucket{{{label},le=\"+Inf\"}} {}", self.count);
        let _ = writeln!(s, "{name}_sum{{{label}}} {}", self.sum);
        let _ = writeln!(s, "{name}_count{{{label}}} {}", self.count);
    }
}

/// All counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metrics {
    pub markets: BTreeMap<Pubkey, MarketMetrics>,
    pub ticks: u64,
    pub tx_landed: u64,
    pub tx_failed: u64,
    pub simulations: u64,
    pub simulation_failures: u64,
    pub batches_v1: u64,
    pub batches_v0: u64,
    pub v1_rejections: u64,
    /// `flag_venue_upgraded` calls landed (duty 10).
    pub venue_flags: u64,
    /// AIR-01: watched ProgramData extensions seen, the venues' and Token-2022's (the loader's
    /// `ExtendProgram`: the deploy slot moved, the ELF did not) — neither flagged nor paged.
    pub venue_extensions: u64,
    /// AIR-18: sends refused (in simulation or on chain) because a program of the transaction was
    /// deployed, upgraded or extended in the slot ("Program is not deployed", the deploy shown by
    /// its ProgramData): retried the next slot, not alerted (a run of them lasting 30 s is). The
    /// same refusal of a program that stays undeployable is not counted here: it is alerted.
    pub not_deployed: u64,
    /// Raydium pool discoveries run (duty 12; four `getProgramAccounts` calls each).
    pub pool_discoveries: u64,
    /// O17: `set_coverage_pause` attempts refused or failed.
    pub coverage_pause_failures: u64,
    /// M7-SEC-8: market reads per tick — markets read in full this tick (with positions, or
    /// something time-critical pending, or their cold poll due) and markets skipped as cold.
    pub markets_read: u64,
    pub markets_cold_skipped: u64,
    /// Duty 11: `crank_sol_usd` calls landed, reads of the feed, refusals, and the lamports spent
    /// (a global line: no market's budget pays it).
    pub oracle_cranks: u64,
    pub oracle_reads: u64,
    pub oracle_refusals: u64,
    pub oracle_lamports: u64,
    /// Decision A (2026-10-01; ARCH §7.1 duty 8): the global budget's balances — `G`, the free
    /// pool `F`, today's tier-2 spend `T`, the overdraft `O`, today's spend and the spend above the
    /// admitted estimates no balance could take — and the markets it skipped reading because they
    /// are below the floor and their idle crank is not due.
    pub budget_global_lamports: u64,
    pub budget_free_lamports: u64,
    pub budget_tail_lamports: u64,
    pub budget_overdraft_lamports: u64,
    pub budget_spent_lamports: u64,
    pub budget_excess_lamports: u64,
    pub markets_idle_skipped: u64,
    /// U28 (ARCH §12): `B̂_live/B̂_ref` (bps) of the keeper's landed liquidations and expiries, at
    /// its decision, and of the positions it evaluated waiting for their bound (`LiquidateWait`,
    /// `ExpireWait`; once per position and tick).
    pub b_ratio_landed: Histogram,
    pub b_ratio_wait: Histogram,
    /// pump-final §7: the keeper's landed synthetic curve exits, all markets
    /// (`MarketMetrics::synthetic_exits`).
    pub synthetic_exits: u64,
    /// pump-final §7, the upgrade window: curve exits whose TradeExtras format had to be found
    /// (no format known for the deployed short.win's deploy slot, or the known one refused on
    /// account parsing), and those where neither format got past account parsing.
    pub curve_format_probes: u64,
    pub curve_format_failures: u64,
}

impl Metrics {
    /// The counters of `market`.
    pub fn market(&mut self, market: Pubkey) -> &mut MarketMetrics {
        self.markets.entry(market).or_default()
    }

    /// Prometheus text exposition.
    pub fn render(&self) -> String {
        let mut s = String::new();
        let g = |s: &mut String, name: &str, help: &str, v: u64| {
            let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}");
        };
        g(
            &mut s,
            "shortwin_keeper_ticks_total",
            "Keeper ticks.",
            self.ticks,
        );
        g(
            &mut s,
            "shortwin_keeper_tx_landed_total",
            "Transactions landed.",
            self.tx_landed,
        );
        g(
            &mut s,
            "shortwin_keeper_tx_failed_total",
            "Transactions that failed on chain.",
            self.tx_failed,
        );
        g(
            &mut s,
            "shortwin_keeper_simulations_total",
            "Simulations run.",
            self.simulations,
        );
        g(
            &mut s,
            "shortwin_keeper_simulation_failures_total",
            "Simulations that failed.",
            self.simulation_failures,
        );
        g(
            &mut s,
            "shortwin_keeper_batches_v1_total",
            "Crank batches sent as v1.",
            self.batches_v1,
        );
        g(
            &mut s,
            "shortwin_keeper_batches_v0_total",
            "Crank batches sent as v0.",
            self.batches_v0,
        );
        g(
            &mut s,
            "shortwin_keeper_v1_rejections_total",
            "v1 transactions the cluster refused.",
            self.v1_rejections,
        );
        g(
            &mut s,
            "shortwin_keeper_venue_flags_total",
            "flag_venue_upgraded calls landed.",
            self.venue_flags,
        );
        g(
            &mut s,
            "shortwin_keeper_venue_extensions_total",
            "Venue and Token-2022 ProgramData extensions seen, not upgrades: nothing flagged or \
             paged (AIR-01).",
            self.venue_extensions,
        );
        g(
            &mut s,
            "shortwin_keeper_not_deployed_total",
            "Sends refused because a program was deployed, upgraded or extended in the slot \
             (AIR-18; its ProgramData shows the deploy): retried the next slot.",
            self.not_deployed,
        );
        g(
            &mut s,
            "shortwin_keeper_pool_discoveries_total",
            "Raydium pool discoveries run (four getProgramAccounts calls each).",
            self.pool_discoveries,
        );
        g(
            &mut s,
            "shortwin_keeper_coverage_pause_failures_total",
            "set_coverage_pause attempts refused or failed (O17).",
            self.coverage_pause_failures,
        );
        g(
            &mut s,
            "shortwin_keeper_markets_read_total",
            "Markets read in full by the ticks (M7-SEC-8).",
            self.markets_read,
        );
        g(
            &mut s,
            "shortwin_keeper_markets_cold_skipped_total",
            "Cold markets (no positions, nothing pending) not read by a tick (M7-SEC-8).",
            self.markets_cold_skipped,
        );
        g(
            &mut s,
            "shortwin_keeper_oracle_cranks_total",
            "crank_sol_usd calls landed (duty 11).",
            self.oracle_cranks,
        );
        g(
            &mut s,
            "shortwin_keeper_oracle_reads_total",
            "Reads of the Pyth PriceUpdateV2 (duty 11).",
            self.oracle_reads,
        );
        g(
            &mut s,
            "shortwin_keeper_oracle_refusals_total",
            "Newer oracle updates the program refuses (duty 11).",
            self.oracle_refusals,
        );
        g(
            &mut s,
            "shortwin_keeper_oracle_lamports_total",
            "Lamports spent on crank_sol_usd (duty 11).",
            self.oracle_lamports,
        );
        g(
            &mut s,
            "shortwin_keeper_synthetic_exits_total",
            "Liquidations and expiries that bought past a live curve's rT by pump's synthetic \
             migration and completed it (pump-final §7).",
            self.synthetic_exits,
        );
        g(
            &mut s,
            "shortwin_keeper_curve_format_probes_total",
            "Curve exits whose TradeExtras format (v3, or shortwin 1.0.0's v2) was found by \
             simulation (pump-final §7's upgrade window).",
            self.curve_format_probes,
        );
        g(
            &mut s,
            "shortwin_keeper_curve_format_failures_total",
            "Curve exits that got past account parsing with neither TradeExtras format.",
            self.curve_format_failures,
        );
        g(
            &mut s,
            "shortwin_keeper_markets_idle_skipped_total",
            "Below-floor markets not read by a tick: their idle crank not due (decision A).",
            self.markets_idle_skipped,
        );
        let gauge = |s: &mut String, name: &str, help: &str, v: u64| {
            let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}");
        };
        gauge(
            &mut s,
            "shortwin_keeper_budget_global_lamports",
            "The global daily keeper budget G (decision A of 2026-10-01).",
            self.budget_global_lamports,
        );
        gauge(
            &mut s,
            "shortwin_keeper_budget_free_lamports",
            "The global budget's free pool F.",
            self.budget_free_lamports,
        );
        gauge(
            &mut s,
            "shortwin_keeper_budget_tail_lamports",
            "Today's spend of the below-floor markets from the free pool (T).",
            self.budget_tail_lamports,
        );
        gauge(
            &mut s,
            "shortwin_keeper_budget_overdraft_lamports",
            "Today's charges booked beyond G (exit-critical cranks, warm-ups without positions).",
            self.budget_overdraft_lamports,
        );
        gauge(
            &mut s,
            "shortwin_keeper_budget_spent_lamports",
            "Today's spend of the global budget.",
            self.budget_spent_lamports,
        );
        gauge(
            &mut s,
            "shortwin_keeper_budget_excess_lamports",
            "Today's spend above the admitted estimates that no balance could take.",
            self.budget_excess_lamports,
        );
        let name = "shortwin_keeper_exit_b_live_over_ref_bps";
        let _ = writeln!(
            s,
            "# HELP {name} B_live/B_ref (bps) at the keeper's exit decisions (U28).\n# TYPE {name} histogram"
        );
        self.b_ratio_landed
            .render(&mut s, name, "outcome=\"landed\"");
        self.b_ratio_wait.render(&mut s, name, "outcome=\"wait\"");
        type Row = (&'static str, &'static str, fn(&MarketMetrics) -> u64);
        let rows: [Row; 39] = [
            (
                "covered_seconds",
                "Seconds with a keeper observation.",
                |m| m.covered_seconds,
            ),
            ("observations", "Observations landed.", |m| m.observations),
            ("cranks", "crank_price instructions landed.", |m| m.cranks),
            ("liquidations", "Liquidations landed.", |m| m.liquidations),
            ("expiries", "Expiries landed.", |m| m.expiries),
            ("settle_calls", "sol_settle calls landed.", |m| {
                m.settle_calls
            }),
            ("settlements", "SOL settlements executed.", |m| {
                m.settlements
            }),
            ("compute_units", "Compute units spent.", |m| m.cu),
            ("lamports", "Lamports spent.", |m| m.lamports),
            (
                "misses",
                "Seconds that needed an observation and got none.",
                |m| m.misses,
            ),
            ("bursts", "Deviation bursts started.", |m| m.bursts),
            (
                "liq_covers",
                "Liquidity-triggered per-second coverage windows started.",
                |m| m.liq_covers,
            ),
            ("coverage_gaps", "CoverageGap events seen.", |m| {
                m.coverage_gaps
            }),
            (
                "coverage_gaps_tolerated",
                "CoverageGap events within idle_interval_s + gap_tolerance_s (landing jitter).",
                |m| m.coverage_gaps_tolerated,
            ),
            (
                "foreign_moves",
                "Foreign prices still there at the keeper's next read (genuine moves, no burst).",
                |m| m.foreign_moves,
            ),
            (
                "opens_while_degraded",
                "Opens that landed while the keeper held the market degraded.",
                |m| m.opens_while_degraded,
            ),
            (
                "venue_admin_changes",
                "Admin actions seen on the market's pool or CPMM config.",
                |m| m.venue_admin_changes,
            ),
            ("pool_proposals", "propose_pool calls landed.", |m| {
                m.pool_proposals
            }),
            (
                "pool_executions",
                "execute_pool_switch calls landed.",
                |m| m.pool_executions,
            ),
            ("pool_switches", "Pool adoptions seen.", |m| m.pool_switches),
            ("migrations", "pump migrate_v2 calls landed.", |m| {
                m.migrations
            }),
            (
                "synthetic_exits",
                "Liquidations and expiries that bought past a live curve's rT (pump v3 synthetic \
                 migration) and completed it.",
                |m| m.synthetic_exits,
            ),
            (
                "transitions",
                "Transitions onto the canonical PumpSwap pool landed.",
                |m| m.transitions,
            ),
            ("sweeps", "sweep_trader calls landed.", |m| m.sweeps),
            (
                "cashback_claims",
                "Venue cashback claims landed (with a sweep).",
                |m| m.cashback_claims,
            ),
            (
                "exits_deferred",
                "Exits held to the next slot by the per-market CU cap (T-CASCADE).",
                |m| m.exits_deferred,
            ),
            (
                "coverage_pauses",
                "set_coverage_pause(true) calls of the keeper landed (O17).",
                |m| m.coverage_pauses,
            ),
            (
                "coverage_unpauses",
                "set_coverage_pause(false) calls of the keeper landed (O17).",
                |m| m.coverage_unpauses,
            ),
            (
                "budget_degraded",
                "Times a refused charge of the global budget degraded the market (O17).",
                |m| m.budget_degraded,
            ),
            (
                "budget_refused",
                "Charges of the market the global budget refused.",
                |m| m.budget_refused,
            ),
            (
                "var_lamports",
                "The market's value at risk V (lamports).",
                |m| m.var_lamports,
            ),
            (
                "budget_tier",
                "1 covered, 2 below the value-at-risk floor, 3 without positions.",
                |m| m.budget_tier,
            ),
            (
                "budget_rank",
                "Rank among the covered markets by value at risk (0: not covered).",
                |m| m.budget_rank,
            ),
            (
                "budget_unspent_lamports",
                "The market's unspent entitlement U.",
                |m| m.budget_unspent_lamports,
            ),
            (
                "budget_spent_exit_lamports",
                "Today's exit-critical budget spend.",
                |m| m.budget_spent_exit_lamports,
            ),
            (
                "budget_spent_baseline_lamports",
                "Today's baseline budget spend.",
                |m| m.budget_spent_baseline_lamports,
            ),
            (
                "budget_spent_discretionary_lamports",
                "Today's discretionary budget spend (bursts, liquidity anomalies).",
                |m| m.budget_spent_discretionary_lamports,
            ),
            (
                "paused_coverage",
                "1 while the market's paused_coverage flag is set (O17).",
                |m| u64::from(m.paused_coverage),
            ),
            ("degraded", "1 while the market is in degraded mode.", |m| {
                u64::from(m.degraded)
            }),
        ];
        for (name, help, get) in rows {
            let full = format!("shortwin_keeper_market_{name}");
            let kind = if matches!(
                name,
                "degraded"
                    | "paused_coverage"
                    | "var_lamports"
                    | "budget_tier"
                    | "budget_rank"
                    | "budget_unspent_lamports"
                    | "budget_spent_exit_lamports"
                    | "budget_spent_baseline_lamports"
                    | "budget_spent_discretionary_lamports"
            ) {
                "gauge"
            } else {
                "counter"
            };
            let _ = writeln!(s, "# HELP {full} {help}\n# TYPE {full} {kind}");
            for (k, m) in &self.markets {
                let _ = writeln!(s, "{full}{{market=\"{k}\"}} {}", get(m));
            }
        }
        // Duty 10 on `PumpMigrating` markets (pump-final §7): signed gauges, only for the markets
        // that have one.
        type Signed = (
            &'static str,
            &'static str,
            fn(&MarketMetrics) -> Option<i128>,
        );
        let signed: [Signed; 2] = [
            (
                "synthetic_completion_delta_bps",
                "rho - 1 in bps on a migrating curve: the pool migrate_v2 will create over the \
                 curve's without its synthetic pool part or a donation (pump-final §5.4).",
                |m| m.synthetic_completion_delta_bps.map(i128::from),
            ),
            (
                "curve_base_ata_donation",
                "Tokens donated to a migrating curve's curve_base_ata: A + pcB - 206.9e12.",
                |m| m.curve_base_ata_donation,
            ),
        ];
        for (name, help, get) in signed {
            let full = format!("shortwin_keeper_market_{name}");
            let _ = writeln!(s, "# HELP {full} {help}\n# TYPE {full} gauge");
            for (k, m) in &self.markets {
                if let Some(v) = get(m) {
                    let _ = writeln!(s, "{full}{{market=\"{k}\"}} {v}");
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_per_market_series() {
        let mut m = Metrics::default();
        let k = Pubkey::new_unique();
        m.market(k).cover(5);
        m.market(k).cover(5);
        m.market(k).cover(6);
        m.market(k).lamports = 5_000;
        let out = m.render();
        assert!(out.contains(&format!(
            "shortwin_keeper_market_covered_seconds{{market=\"{k}\"}} 2"
        )));
        assert!(out.contains(&format!(
            "shortwin_keeper_market_lamports{{market=\"{k}\"}} 5000"
        )));
        assert!(out.contains("# TYPE shortwin_keeper_market_degraded gauge"));
        assert!(out.contains("# TYPE shortwin_keeper_market_budget_rank gauge"));
        assert!(out.contains("# TYPE shortwin_keeper_market_budget_refused counter"));
        assert!(out.contains("# TYPE shortwin_keeper_budget_free_lamports gauge"));
        // pump-final §7: the synthetic exits, and the migrating-curve gauges only where set.
        m.market(k).synthetic_exits = 2;
        m.synthetic_exits = 2;
        let j = Pubkey::new_unique();
        m.market(j).synthetic_completion_delta_bps = Some(-12);
        m.market(j).curve_base_ata_donation = Some(1_000_000);
        let out = m.render();
        assert!(out.contains(&format!(
            "shortwin_keeper_market_synthetic_exits{{market=\"{k}\"}} 2"
        )));
        assert!(out.contains("shortwin_keeper_synthetic_exits_total 2"));
        assert!(out.contains("# TYPE shortwin_keeper_market_synthetic_completion_delta_bps gauge"));
        assert!(out.contains(&format!(
            "shortwin_keeper_market_synthetic_completion_delta_bps{{market=\"{j}\"}} -12"
        )));
        assert!(out.contains(&format!(
            "shortwin_keeper_market_curve_base_ata_donation{{market=\"{j}\"}} 1000000"
        )));
        assert!(!out.contains(&format!(
            "shortwin_keeper_market_curve_base_ata_donation{{market=\"{k}\"}}"
        )));
        assert!(!out.contains("straddles"));
    }

    /// U28's histogram: cumulative buckets, `+Inf`, sum and count, per outcome.
    #[test]
    fn renders_the_b_ratio_histogram() {
        let mut m = Metrics::default();
        for v in [9_990, 10_000, 10_020, 12_000] {
            m.b_ratio_landed.record(v);
        }
        m.b_ratio_wait.record(10_400);
        let out = m.render();
        let n = "shortwin_keeper_exit_b_live_over_ref_bps";
        assert!(out.contains(&format!("# TYPE {n} histogram")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"landed\",le=\"9970\"}} 0")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"landed\",le=\"10000\"}} 2")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"landed\",le=\"10030\"}} 3")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"landed\",le=\"11000\"}} 3")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"landed\",le=\"+Inf\"}} 4")));
        assert!(out.contains(&format!("{n}_sum{{outcome=\"landed\"}} 42010")));
        assert!(out.contains(&format!("{n}_count{{outcome=\"wait\"}} 1")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"wait\",le=\"10300\"}} 0")));
        assert!(out.contains(&format!("{n}_bucket{{outcome=\"wait\",le=\"10500\"}} 1")));
    }
}
