//! Per-market coverage state (ARCHITECTURE §7.1 duties 1, 2, 5, 8, 9; ECON §5.8): when the
//! market needs the keeper's next observation, deviation bursts, the per-second SLO with its
//! degraded mode, and the daily budget.
//!
//! - **Cadence** ([`MarketTrack::observation_due`]): every slot during a deviation burst
//!   (duty 2; never while an S3 on an executable venue is due, duty 5); once per second when
//!   the market needs it (duty 1: a near position or `|P_spot/P_ema − 1| > 2 %`; duty 5: S3 due
//!   on an executable venue, or the post-change warm-up of a market with open positions after a
//!   `PoolSwitched`, until `ema_warm == 0`, M4R, and after it for as long as the post-change
//!   reference guard is set, `ema_guard == 1`, M4F-1: every second then carries an honest bucket
//!   low, so attacker-only seconds cannot lift `L_mlo` and clear the guard; duty 2's liquidity
//!   trigger on a market with open positions, `liq_cover_s` from a foreign bucket liquidity,
//!   FITS-2 (b); `decide::MarketNeeds::liq_covered`); at once when the
//!   route is gone and no S3 chain runs yet, then every `no_route_interval_s` (the chain
//!   restarts after `S3_OBS_GAP_S`); every
//!   `idle_interval_s` with open positions; a Raydium market without positions every
//!   `listing_interval_s` inside its listing window and every second at its end until its
//!   post-change warm-up is over (M4V-5, M4R: the first open after the window reads an honest
//!   `P_ema`).
//! - **Bursts** ([`MarketTrack::check_foreign`]): the keeper remembers the spot prices it read
//!   in each second; an observation in a bucket (the open one, or the last completed one) that
//!   deviates from that second's view by more than `burst_dev_bps` was not the keeper's, and —
//!   **when the excursion reverted** (M7-SEC-2), the keeper's latest read, at or after that
//!   second, more than `burst_dev_bps` away from the deviating extreme too — starts 60 s of
//!   per-slot observations. An atomic `[pump, crank, dump]` bundle leaves exactly that: a bucket
//!   extreme nobody else can see before or after it. A foreign price still there at the keeper's
//!   next read is a genuine move (someone's crank after an honest trade, a trade landing between
//!   the keeper's read and its own crank, another keeper instance on the other side of a trade),
//!   which duty 1's per-second coverage above `ema_dev_bps` follows; as built at M7 each of those
//!   started a burst, so one plain `crank_price` after any honest ≥ 2 % move bought 116 keeper
//!   cranks charged to the O17 budget (the M7-SEC-2 reproducer: 51 such cranks, 0.000255 SOL,
//!   spent the default 0.03-SOL budget). A price burst again needs a ≥ 2 % price round trip.
//!   Only **prices** start a burst: the per-slot liquidity bursts M4F-1 added stay
//!   reverted (M4G-2: an atomic `[deposit, crank_price, withdraw]` bought a 60-s per-slot burst,
//!   25–150× the attacker's cost in the keeper's).
//! - **Liquidity coverage** ([`MarketTrack::check_foreign`], FITS-2 (b), owner decision of
//!   2026-09-29): the keeper also remembers the venue depths `√(X·Y)` it read in each second. On
//!   a market **with open positions**, a bucket whose liquidity extremes `llo` / `lhi` (the open
//!   bucket's, or those of the ring entry that integrated the last completed one) deviate from
//!   that second's depth view by more than `burst_dev_bps` — an LP-wrapped
//!   `[withdraw, crank, deposit]` (the withdraw drag of `L_ema` / `L_slow`, FITV-2) or
//!   `[deposit, crank, withdraw]` (the freeze, M4F-1) — starts **per-second** (not per-slot)
//!   observations for `liq_cover_s` (10 s) from that second, extended by every recurrence. Every
//!   second of the window then carries an honest bucket extreme, so the attacker has no
//!   attacker-only second to drag or push the EMA with while it keeps wrapping, and pays one
//!   transaction per second of the keeper's coverage (1:1); an isolated wrap buys at most
//!   `liq_cover_s` observations. The coverage holds the EMA; it cannot lift a low the attacker
//!   reached genuinely — its LP withdrawn for real for a few seconds before it starts wrapping,
//!   nothing foreign then — which stays frozen at 1:1 (FITX-1, ECON §12.12 residual 10, owner
//!   item O18). A market without open positions never gets liquidity-triggered
//!   coverage (nothing there rests on the liquidity EMA). A venue change (graduation, a Raydium
//!   adoption) rewrites the ring's liquidity to the new pool's, so the depths viewed before it are
//!   dropped.
//! - **Degraded mode** ([`MarketTrack::close_second`], [`MarketTrack::on_coverage_gap`],
//!   [`MarketTrack::on_budget_exhausted`]): a second in which an observation was required and
//!   none landed is a miss; `degraded_after_misses` consecutive misses, a `CoverageGap` event
//!   from the program (an integration after more than `COVERAGE_GAP_S` — 40 s since OWNER-M7-12
//!   — without one on a market with open positions), or (O17) a charge of the market the global
//!   budget refused while it has open positions ([`crate::budget`]) make the market degraded:
//!   the keeper raises [`crate::alerts::Alert::Degraded`]
//!   and — the designated pauser holding the `coverage_authority` key (`[coverage_pause]`) —
//!   sends `set_coverage_pause(market, true)`, which pauses opens only; otherwise the alert pages
//!   the Emergency multisig to `set_pause` the market. The state lifts after `recovery_s`
//!   (10 min) of met coverage ([`crate::alerts::Alert::CoverageRestored`]; seconds that require
//!   nothing count as met, [`MarketTrack::advance`]), and the pause with it. **Timing
//!   tolerances (M7-SEC-1):** a `CoverageGap` in the keeper's own transaction degrades only above
//!   `max(idle_interval_s + gap_tolerance_s, COVERAGE_GAP_S)` (the program measures landing
//!   seconds, the keeper schedules from its read: landing jitter made 31–32-s gaps at the idle
//!   cadence when the constant was 30 s —
//!   [`crate::config::CoverageConfig::gap_degrades_above`]); a tick that follows the previous
//!   one by at most `1 + tick_gap_tolerance_s` seconds (a tick that waited for its confirmations)
//!   misses only the skipped seconds that required per-second coverage, and only a longer skip
//!   means the keeper was down (as built at M7 any skipped second restarted the recovery, so a
//!   market whose ticks send never recovered). A market whose
//!   on-chain `paused_coverage` flag is set when the keeper first sees it (a restart, another
//!   instance's pause) starts `Recovering` ([`MarketTrack::sync_pause_flag`]: [`FlagSet::Found`]);
//!   one that turns set while the keeper runs without its having set it is
//!   [`FlagSet::Foreign`] (★OWNER-M7-3 (b): a second or compromised coverage key). A flag the
//!   keeper only adopted that someone else clears — since 2026-10-01 the Emergency multisig can
//!   — stays clear: the adopted recovery ends ([`MarketTrack::on_flag_cleared_by_other`]); the
//!   designated pauser re-sets only a flag on a market it degraded itself
//!   ([`MarketTrack::own_degraded`]).
//! - **Budget** ([`crate::budget`]; the global budget of the owner's decision A of 2026-10-01):
//!   a charge the budget refuses skips the observation (a miss) and, on a market with open
//!   positions, degrades it at once (O17: opens paused, exits continue). The per-UTC-day spend on
//!   the market's observations is tallied here ([`MarketTrack::spend`]) for the metrics. Bursts and
//!   liquidity-anomaly coverage run on covered markets only ([`MarketTrack::stop_bursts`] for the
//!   others).

use std::collections::{HashMap, VecDeque};

use anchor_lang::prelude::Pubkey;
use shortwin::state::Market;

use crate::alerts::{Alert, DegradedReason};
use crate::config::CoverageConfig;
use crate::decide::MarketNeeds;

/// Seconds of own spot views kept for the burst check.
const VIEW_HISTORY_S: i64 = 120;

/// The keeper's own view of one second: the lowest and highest spot price and venue depth
/// `√(X·Y)` it read (the depth `None` when the venue gave none, or when it was read before a
/// venue change).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct View {
    sec: i64,
    lo: u128,
    hi: u128,
    liq: Option<(u128, u128)>,
}

/// A market's venue identity (`Market.venue` and the first pool-cache key): a change rewrites
/// the ring's liquidity (graduation, a Raydium adoption).
pub type VenueId = (u8, Pubkey);

/// What [`MarketTrack::check_foreign`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Foreign {
    /// The second of a newly detected foreign **price** whose excursion reverted (a burst
    /// started or extended; duty 2).
    pub price: Option<i64>,
    /// The second of a foreign price the keeper's read in a later second still showed: a genuine
    /// move, no burst (M7-SEC-2).
    pub genuine: Option<i64>,
    /// The second of a newly detected foreign **liquidity** on a market with open positions
    /// (liquidity coverage started or extended; FITS-2 (b)).
    pub liquidity: Option<i64>,
    /// The liquidity deviation started a new window (none ran at that second).
    pub liq_started: bool,
}

/// Why an observation is due now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Due {
    /// Deviation burst: every slot (duty 2).
    Slot,
    /// Once this second (duties 1, 5).
    Second,
    /// No route: the S3 chain cadence (duty 5).
    NoRoute,
    /// Open positions: the idle cadence (duty 1).
    Idle,
    /// A Raydium market without positions inside its listing window: the listing cadence
    /// (M4V-5).
    Listing,
}

/// A `paused_coverage` flag the keeper found newly set and did not set itself
/// ([`MarketTrack::sync_pause_flag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagSet {
    /// Set at the keeper's first sight of the market (a restart).
    Found,
    /// Seen clear, then set (another instance holding the key, or the key compromised).
    Foreign,
}

/// The degraded-mode state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoverageState {
    #[default]
    Healthy,
    /// Coverage cannot be met (opens should pause, reason `KeeperCoverage`).
    Degraded { since: i64 },
    /// Coverage met again since `since`; lifts after `recovery_s`.
    Recovering { degraded_since: i64, since: i64 },
}

impl CoverageState {
    /// Degraded or recovering (the pause is still on).
    pub fn is_degraded(&self) -> bool {
        !matches!(self, Self::Healthy)
    }

    /// The second the market was degraded, while degraded or recovering.
    pub fn degraded_since(&self) -> Option<i64> {
        match *self {
            Self::Healthy => None,
            Self::Degraded { since } => Some(since),
            Self::Recovering { degraded_since, .. } => Some(degraded_since),
        }
    }
}

/// One market's coverage bookkeeping (module docs).
#[derive(Clone, Debug, Default)]
pub struct MarketTrack {
    /// The keeper's last landed observation (second, slot).
    pub last_obs: Option<(i64, u64)>,
    /// A deviation burst runs while `now < burst_until`.
    pub burst_until: i64,
    /// Seconds whose foreign price deviation is decided: it started a burst (reverted), or the
    /// keeper's read in a later second found the price still there (a genuine move, M7-SEC-2).
    price_decided: Vec<i64>,
    /// The keeper's latest read: (second, spot price) — the reference that tells a reverted
    /// excursion from a genuine move (M7-SEC-2).
    spot_now: Option<(i64, u128)>,
    /// Liquidity-triggered per-second coverage runs while `now < liq_until` (FITS-2 (b)).
    pub liq_until: i64,
    /// Seconds whose foreign liquidity already started or extended the coverage.
    liq_seen: Vec<i64>,
    /// Own views per second (price and depth ranges read in that second).
    views: VecDeque<View>,
    /// The venue the depth views were read on.
    venue: Option<VenueId>,
    pub state: CoverageState,
    /// Consecutive seconds with a required observation and none landed.
    pub miss_streak: u32,
    /// The second being accumulated: (second, required, landed).
    current: Option<(i64, bool, bool)>,
    /// Last second a `sol_settle` was sent for a position (at most one per second).
    pub settle_sent: HashMap<Pubkey, i64>,
    /// Last S3 probe (simulated `sol_settle`) of a position.
    pub s3_probe: HashMap<Pubkey, i64>,
    /// UTC day and lamports spent on observations that day.
    pub spend_day: i64,
    pub spent_today: u64,
    /// The keeper degraded the market itself (misses, a gap, a budget refusal) in its current
    /// degraded episode — not only adopted a flag found set ([`Self::sync_pause_flag`]). The
    /// designated pauser re-sets a cleared flag only then (★OWNER-M7-3 (b)).
    pub own_degraded: bool,
    /// The on-chain `paused_coverage` flag as last read ([`Self::sync_pause_flag`]).
    pub flag_seen: Option<bool>,
    /// The last second that was a miss or a degradation event (a refused observation, a gap,
    /// seconds the keeper did not run while the market had open positions).
    last_bad: i64,
    /// The second of the keeper's last tick ([`Self::advance`]).
    last_tick: Option<i64>,
    /// The last tick second at which the market needed an observation every second (duties 1,
    /// 2, 5; [`Self::note_every_second`]): the seconds a slow tick skips after it were required
    /// (M7-SEC-1 (b)).
    every_second_at: Option<i64>,
    /// The open positions at the last read (M7-SEC-3: a new one while the market is held
    /// degraded with its flag set is alerted).
    positions_seen: Vec<Pubkey>,
}

impl MarketTrack {
    /// Records the spot price and the venue depth `√(X·Y)` the keeper read at `now` on the venue
    /// `venue` ([`VenueId`]); a new venue drops the depths viewed on the previous one.
    pub fn record_view(
        &mut self,
        now: i64,
        price: Option<u128>,
        liquidity: Option<u128>,
        venue: VenueId,
    ) {
        if self.venue != Some(venue) {
            if self.venue.is_some() {
                for v in &mut self.views {
                    v.liq = None;
                }
            }
            self.venue = Some(venue);
        }
        self.spot_now = price.map(|p| (now, p));
        let Some(p) = price else { return };
        match self.views.back_mut() {
            Some(v) if v.sec == now => {
                v.lo = v.lo.min(p);
                v.hi = v.hi.max(p);
                v.liq = match (v.liq, liquidity) {
                    (Some((lo, hi)), Some(l)) => Some((lo.min(l), hi.max(l))),
                    (Some(r), None) => Some(r),
                    (None, l) => l.map(|l| (l, l)),
                };
            }
            _ => self.views.push_back(View {
                sec: now,
                lo: p,
                hi: p,
                liq: liquidity.map(|l| (l, l)),
            }),
        }
        while self
            .views
            .front()
            .is_some_and(|v| now.saturating_sub(v.sec) > VIEW_HISTORY_S)
        {
            self.views.pop_front();
        }
    }

    /// Whether `position` was open at the last read ([`Self::set_positions`]).
    pub fn knows_position(&self, position: &Pubkey) -> bool {
        self.positions_seen.contains(position)
    }

    /// Whether `positions` (this tick's open positions of the market) differ from the set at the
    /// last read (decision A: a below-floor market is read in the tick its positions change).
    pub fn positions_changed(&self, positions: impl Iterator<Item = Pubkey>) -> bool {
        let mut n = 0usize;
        for p in positions {
            if !self.positions_seen.contains(&p) {
                return true;
            }
            n += 1;
        }
        n != self.positions_seen.len()
    }

    /// The open positions this read shows.
    pub fn set_positions(&mut self, positions: impl Iterator<Item = Pubkey>) {
        self.positions_seen.clear();
        self.positions_seen.extend(positions);
    }

    fn view_at(&self, sec: i64) -> Option<&View> {
        self.views.iter().find(|v| v.sec == sec)
    }

    /// Whether bucket extremes `(lo, hi)` of second `sec` deviate from the keeper's own view
    /// of that second by more than `dev_bps` (`None`: no view of that second).
    pub fn deviates(&self, sec: i64, lo: u128, hi: u128, dev_bps: u32) -> Option<bool> {
        let v = self.view_at(sec)?;
        Some(outside(lo, hi, v.lo, v.hi, dev_bps))
    }

    /// Whether bucket liquidity extremes `(llo, lhi)` of second `sec` deviate from the depth
    /// `√(X·Y)` the keeper read in that second by more than `dev_bps` (`None`: no depth view of
    /// that second on the current venue).
    pub fn liq_deviates(&self, sec: i64, llo: u128, lhi: u128, dev_bps: u32) -> Option<bool> {
        let (vlo, vhi) = self.view_at(sec)?.liq?;
        Some(outside(llo, lhi, vlo, vhi, dev_bps))
    }

    /// Duty 2: checks the market's open bucket and last completed bucket for an observation
    /// the keeper did not make — a price, or (FITS-2 (b)) a liquidity, off the keeper's own view
    /// of that second by more than `burst_dev_bps`. A foreign price starts (or extends) a
    /// per-slot burst of `burst_s` from that second **if the excursion reverted** (M7-SEC-2):
    /// the deviating extreme (a high above the keeper's range of that second and the one
    /// before, a low below it) is more than `burst_dev_bps` off the keeper's latest read too
    /// ([`Self::record_view`], the snapshot that already contains the bucket). A foreign price
    /// the latest read — necessarily in a later second — still shows is a genuine move (no
    /// burst: duty 1 follows a genuine move). Without a read (a venue with no price) a foreign
    /// price starts a burst, as before. A foreign liquidity starts (or extends) per-second
    /// coverage of `liq_cover_s` from that second, only when `open` (the market has
    /// open positions); without positions it is not even recorded. The last completed bucket's
    /// liquidity extremes are those of the ring entry that integrated it (the newest entry,
    /// pushed after that second with the same price extremes).
    pub fn check_foreign(&mut self, m: &Market, cov: &CoverageConfig, open: bool) -> Foreign {
        let mut found = Foreign::default();
        let (lb_lo, lb_hi) = (m.lb_lo.get(), m.lb_hi.get());
        let lb_liq = m
            .observation()
            .ring
            .newest()
            .filter(|e| e.ts > m.lb_sec && e.lo == lb_lo && e.hi == lb_hi)
            .map(|e| (e.llo, e.lhi));
        let buckets = [
            (
                m.b_sec,
                m.b_lo.get(),
                m.b_hi.get(),
                Some((m.b_llo.get(), m.b_lhi.get())),
            ),
            (m.lb_sec, lb_lo, lb_hi, lb_liq),
        ];
        let dev = cov.burst_dev_bps;
        for (sec, lo, hi, liq) in buckets {
            if sec == 0 || lo == 0 {
                continue;
            }
            if !self.price_decided.contains(&sec)
                && let Some(v) = self.view_at(sec).copied()
            {
                // The keeper's own range around that second: its reads of that second and of the
                // one before (M7-SEC-2: a price it saw a second earlier is the market moving
                // away from it — another instance's or its own crank before a genuine move —,
                // not an excursion nobody else can see).
                let (vlo, vhi) = self
                    .view_at(sec.saturating_sub(1))
                    .map_or((v.lo, v.hi), |p| (v.lo.min(p.lo), v.hi.max(p.hi)));
                let below = under(lo, vlo, dev);
                let above = over(hi, vhi, dev);
                if below || above {
                    // M7-SEC-2: did the excursion revert by the keeper's latest read? A low the
                    // price is back above, or a high it is back below, by more than `dev`. (A read
                    // in the bucket's own second is part of the range above, so an excursion off
                    // the range is off that read too: only a later read can show it stayed.)
                    let reverted = self.spot_now.is_none_or(|(_, p)| {
                        (below && under(lo, p, dev)) || (above && over(hi, p, dev))
                    });
                    self.price_decided.push(sec);
                    if reverted {
                        self.burst_until = self.burst_until.max(sec.saturating_add(cov.burst_s));
                        found.price = Some(sec);
                    } else {
                        found.genuine = Some(sec);
                    }
                }
            }
            if !open || cov.liq_cover_s <= 0 || self.liq_seen.contains(&sec) {
                continue;
            }
            let depth = liq
                .filter(|(llo, _)| *llo != 0)
                .and_then(|(llo, lhi)| self.liq_deviates(sec, llo, lhi, dev));
            if depth == Some(true) {
                self.liq_seen.push(sec);
                found.liq_started |= self.liq_until <= sec;
                self.liq_until = self.liq_until.max(sec.saturating_add(cov.liq_cover_s));
                found.liquidity = Some(sec);
            }
        }
        for seen in [&mut self.price_decided, &mut self.liq_seen] {
            if seen.len() > 64 {
                seen.drain(..32);
            }
        }
        found
    }

    /// Whether a burst runs at `now`.
    pub fn in_burst(&self, now: i64) -> bool {
        now < self.burst_until
    }

    /// Ends any price burst and liquidity-anomaly window (a market that is not covered gets
    /// neither: owner decision A of 2026-10-01, ARCH §7.1 duties 2 and 8).
    pub fn stop_bursts(&mut self) {
        self.burst_until = 0;
        self.liq_until = 0;
    }

    /// Whether liquidity-triggered per-second coverage runs at `now` (FITS-2 (b)).
    pub fn liq_covered(&self, now: i64) -> bool {
        now < self.liq_until
    }

    /// Whether an observation is due at (`now`, `slot`) given the market's needs, and why.
    pub fn observation_due(
        &self,
        needs: &MarketNeeds,
        now: i64,
        slot: u64,
        cov: &CoverageConfig,
    ) -> Option<Due> {
        let since = |interval: i64| {
            self.last_obs
                .is_none_or(|(t, _)| now.saturating_sub(t) >= interval)
        };
        let this_second = self.last_obs.is_none_or(|(t, _)| t < now);
        if self.in_burst(now) && !needs.s3_due_executable {
            // Every slot, with or without positions: the burst's own point (duty 2).
            return self
                .last_obs
                .is_none_or(|(_, s)| s < slot)
                .then_some(Due::Slot);
        }
        if needs.per_second && this_second {
            return Some(Due::Second);
        }
        if needs.start_chain && this_second {
            return Some(Due::NoRoute);
        }
        if needs.no_route && since(cov.no_route_interval_s) {
            return Some(Due::NoRoute);
        }
        if needs.open_positions && since(cov.idle_interval_s) {
            return Some(Due::Idle);
        }
        if needs.warm_up && this_second {
            return Some(Due::Second);
        }
        if needs.listing && since(cov.listing_interval_s) {
            return Some(Due::Listing);
        }
        None
    }

    /// Records a landed observation (a crank, or an exit that carries the ReadSet).
    pub fn landed(&mut self, now: i64, slot: u64) {
        self.last_obs = Some((now, slot));
        self.note(now, false, true);
    }

    /// Records that an observation was required at `now`.
    pub fn required(&mut self, now: i64) {
        self.note(now, true, false);
    }

    /// Records that at `now` the market needed an observation every second (duty 1 near a
    /// threshold or off its EMA, duty 2's burst, duty 5, a warm-up): the seconds a slow tick
    /// skips after it were required ([`Self::advance`], M7-SEC-1 (b)).
    pub fn note_every_second(&mut self, now: i64) {
        self.every_second_at = Some(now);
    }

    fn note(&mut self, now: i64, required: bool, landed: bool) {
        match &mut self.current {
            Some((s, r, l)) if *s == now => {
                *r |= required;
                *l |= landed;
            }
            _ => self.current = Some((now, required, landed)),
        }
    }

    /// Closes every second before `now`: a second that required an observation and got none
    /// is a miss; misses make the market degraded, met seconds recover it (module docs).
    /// Alerts go to `alerts`.
    pub fn close_second(
        &mut self,
        market: Pubkey,
        now: i64,
        cov: &CoverageConfig,
        alerts: &mut Vec<Alert>,
    ) -> Option<bool> {
        let (sec, required, landed) = self.current?;
        if sec >= now {
            return None;
        }
        self.current = None;
        let missed = required && !landed;
        if missed {
            self.last_bad = self.last_bad.max(sec);
            self.miss_streak = self.miss_streak.saturating_add(1);
            if self.miss_streak >= cov.degraded_after_misses.max(1) {
                self.degrade(
                    market,
                    sec,
                    DegradedReason::Misses(self.miss_streak),
                    alerts,
                );
            }
        } else {
            self.miss_streak = 0;
            self.recover(market, sec, cov, alerts);
        }
        Some(missed)
    }

    /// Seconds that require no observation are met (a market whose positions closed while it
    /// was degraded, one whose budget no longer matters, the seconds between idle observations):
    /// a degraded market whose last closed second (`now − 1`) came after its last miss or
    /// degradation event is recovering from the second after that event, and once the last
    /// closed second is `recovery_s` past the start of the recovery with no miss pending the
    /// degraded state lifts ([`Alert::CoverageRestored`]) — the rule [`Self::close_second`]
    /// applies to required seconds, for the seconds that require none.
    ///
    /// Seconds in which the keeper did not run (no tick) on a market with open positions
    /// (`open`), M7-SEC-1 (b):
    /// - if the market needed an observation every second at the last tick
    ///   ([`Self::note_every_second`]), each skipped second is a missed required second, exactly
    ///   as [`Self::close_second`] counts one (`degraded_after_misses` consecutive ones degrade
    ///   the market; an isolated one does not restart a recovery);
    /// - if more than `tick_gap_tolerance_s` seconds were skipped, the keeper was down: those
    ///   seconds were not covered, and a degraded or recovering market restarts its recovery
    ///   after them (a healthy one on the idle cadence is degraded by the program's
    ///   `CoverageGap` at its next integration);
    /// - otherwise (the idle cadence, a tick that waited about a second for its confirmations)
    ///   the skipped seconds required nothing and are met. As built at M7 any skipped second
    ///   restarted the recovery, so on mainnet — where a tick that sends waits ≈ 1 s for
    ///   `confirmed` — a degraded market whose ticks sent never recovered.
    pub fn advance(
        &mut self,
        market: Pubkey,
        now: i64,
        open: bool,
        cov: &CoverageConfig,
        alerts: &mut Vec<Alert>,
    ) {
        let closed = now.saturating_sub(1);
        if let Some(t) = self.last_tick
            && now.saturating_sub(t) > 1
            && open
        {
            let skipped = now.saturating_sub(t).saturating_sub(1);
            if self.every_second_at == Some(t) {
                self.last_bad = self.last_bad.max(closed);
                self.miss_streak = self
                    .miss_streak
                    .saturating_add(u32::try_from(skipped).unwrap_or(u32::MAX));
                if self.miss_streak >= cov.degraded_after_misses.max(1) {
                    self.degrade(
                        market,
                        closed,
                        DegradedReason::Misses(self.miss_streak),
                        alerts,
                    );
                }
            }
            if skipped > cov.tick_gap_tolerance_s {
                self.last_bad = self.last_bad.max(closed);
                if let CoverageState::Recovering { degraded_since, .. } = self.state {
                    self.state = CoverageState::Degraded {
                        since: degraded_since,
                    };
                }
            }
        }
        self.last_tick = Some(self.last_tick.map_or(now, |t| t.max(now)));
        if closed > self.last_bad {
            // The last closed second needed nothing, or was met: no miss is pending.
            self.miss_streak = 0;
        }
        if let CoverageState::Degraded { since } = self.state
            && closed > self.last_bad
        {
            self.state = CoverageState::Recovering {
                degraded_since: since,
                since: self.last_bad.saturating_add(1).max(since),
            };
        }
        if let CoverageState::Recovering {
            degraded_since,
            since,
        } = self.state
            && self.miss_streak == 0
            && closed.saturating_sub(since) >= cov.recovery_s
        {
            self.state = CoverageState::Healthy;
            self.own_degraded = false;
            alerts.push(Alert::CoverageRestored {
                market,
                degraded_s: closed.saturating_sub(degraded_since),
            });
        }
    }

    /// The market's on-chain `paused_coverage` flag as this tick read it: a flag **newly** set
    /// while the keeper holds the market healthy — found set at its first sight of the market (a
    /// restart: [`FlagSet::Found`]), or set by someone else since the last read (another
    /// instance, a compromised key: [`FlagSet::Foreign`]) — starts the market `Recovering` from
    /// `now`, so the pause lifts only after `recovery_s` of this keeper's own met coverage (never
    /// at once: two instances never flip-flop the flag). A flag that stays set through the
    /// keeper's own degraded episode is the keeper's to lift once it has recovered. `unpausing`:
    /// the keeper's own unpause of this market is pending (its flag still set until it lands).
    /// Returns how a set flag was adopted, if it was.
    pub fn sync_pause_flag(
        &mut self,
        now: i64,
        paused_on_chain: bool,
        unpausing: bool,
    ) -> Option<FlagSet> {
        let seen = self.flag_seen;
        let newly = paused_on_chain && seen != Some(true);
        self.flag_seen = Some(paused_on_chain);
        if newly && !unpausing && self.state == CoverageState::Healthy {
            self.state = CoverageState::Recovering {
                degraded_since: now,
                since: now,
            };
            return Some(if seen.is_none() {
                FlagSet::Found
            } else {
                FlagSet::Foreign
            });
        }
        None
    }

    /// The flag was found cleared, and not by this keeper (★OWNER-M7-3 (b): the Emergency
    /// multisig, or the coverage key elsewhere), while the keeper holds the market degraded. If
    /// the keeper only adopted the pause ([`Self::own_degraded`] false: found set at a restart,
    /// or set by someone else) it stays clear — the adopted recovery ends, the market is healthy
    /// — and this returns `true`; a market the keeper degraded itself stays degraded (the
    /// designated pauser sets the flag again).
    pub fn on_flag_cleared_by_other(&mut self) -> bool {
        if self.state.is_degraded() && !self.own_degraded {
            self.state = CoverageState::Healthy;
            return true;
        }
        false
    }

    /// O17, the global budget ([`crate::budget`]): a charge of the market was refused while it
    /// has open positions — degraded at once (opens pause; exits continue), not after
    /// `degraded_after_misses`. `spent` and `budget`: the whole budget's day spend and `G`;
    /// `rank`: the market's rank (`None`: below the floor).
    pub fn on_budget_exhausted(
        &mut self,
        market: Pubkey,
        now: i64,
        spent: u64,
        budget: u64,
        rank: Option<u32>,
        alerts: &mut Vec<Alert>,
    ) {
        self.last_bad = self.last_bad.max(now);
        self.degrade(
            market,
            now,
            DegradedReason::Budget {
                spent,
                budget,
                rank,
            },
            alerts,
        );
    }

    /// The program reported a `CoverageGap` on this market (A69): degraded.
    pub fn on_coverage_gap(
        &mut self,
        market: Pubkey,
        now: i64,
        gap_s: i64,
        open_positions: u32,
        alerts: &mut Vec<Alert>,
    ) {
        let before = self.state;
        self.last_bad = self.last_bad.max(now.saturating_sub(1));
        self.degrade(
            market,
            now,
            DegradedReason::CoverageGap {
                gap_s,
                open_positions,
            },
            alerts,
        );
        // The gap is in the past and this integration ended it: unless misses keep the
        // market degraded, coverage is met again from this second (the recovery clock
        // restarts).
        self.state = match before {
            CoverageState::Degraded { since } => CoverageState::Degraded { since },
            CoverageState::Healthy => CoverageState::Recovering {
                degraded_since: now,
                since: now,
            },
            CoverageState::Recovering { degraded_since, .. } => CoverageState::Recovering {
                degraded_since,
                since: now,
            },
        };
    }

    fn degrade(
        &mut self,
        market: Pubkey,
        now: i64,
        reason: DegradedReason,
        alerts: &mut Vec<Alert>,
    ) {
        self.own_degraded = true;
        match self.state {
            CoverageState::Degraded { .. } => {}
            CoverageState::Healthy => {
                self.state = CoverageState::Degraded { since: now };
                alerts.push(Alert::Degraded { market, reason });
            }
            CoverageState::Recovering { degraded_since, .. } => {
                // Still paused: back to degraded without a new page.
                self.state = CoverageState::Degraded {
                    since: degraded_since,
                };
            }
        }
    }

    fn recover(&mut self, market: Pubkey, now: i64, cov: &CoverageConfig, alerts: &mut Vec<Alert>) {
        match self.state {
            CoverageState::Healthy => {}
            CoverageState::Degraded { since } => {
                self.state = CoverageState::Recovering {
                    degraded_since: since,
                    since: now,
                };
            }
            CoverageState::Recovering {
                degraded_since,
                since,
            } => {
                if now.saturating_sub(since) >= cov.recovery_s {
                    self.state = CoverageState::Healthy;
                    self.own_degraded = false;
                    alerts.push(Alert::CoverageRestored {
                        market,
                        degraded_s: now.saturating_sub(degraded_since),
                    });
                }
            }
        }
    }

    /// Adds `lamports` to today's observation spend (the market's tally; the admission is the
    /// global budget's, [`crate::budget`]).
    pub fn spend(&mut self, now: i64, lamports: u64) {
        self.roll_day(now);
        self.spent_today = self.spent_today.saturating_add(lamports);
    }

    fn roll_day(&mut self, now: i64) {
        let day = now.div_euclid(86_400);
        if day != self.spend_day {
            self.spend_day = day;
            self.spent_today = 0;
        }
    }
}

/// Whether `[lo, hi]` reaches more than `dev_bps` below `vlo` or above `vhi`.
fn outside(lo: u128, hi: u128, vlo: u128, vhi: u128, dev_bps: u32) -> bool {
    under(lo, vlo, dev_bps) || over(hi, vhi, dev_bps)
}

/// `lo` is more than `dev_bps` below `v`.
fn under(lo: u128, v: u128, dev_bps: u32) -> bool {
    let d = u128::from(dev_bps);
    lo.saturating_mul(10_000) < v.saturating_mul(10_000u128.saturating_sub(d))
}

/// `hi` is more than `dev_bps` above `v`.
fn over(hi: u128, v: u128, dev_bps: u32) -> bool {
    let d = u128::from(dev_bps);
    hi.saturating_mul(10_000) > v.saturating_mul(10_000u128.saturating_add(d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    fn cov() -> CoverageConfig {
        CoverageConfig::default()
    }

    #[test]
    fn cadence_follows_the_needs() {
        let c = cov();
        let mut t = MarketTrack::default();
        let idle = MarketNeeds {
            open_positions: true,
            ..MarketNeeds::default()
        };
        // Nothing landed yet: due at once.
        assert_eq!(t.observation_due(&idle, 100, 250, &c), Some(Due::Idle));
        t.landed(100, 250);
        assert_eq!(t.observation_due(&idle, 129, 322, &c), None);
        assert_eq!(t.observation_due(&idle, 130, 325, &c), Some(Due::Idle));
        let near = MarketNeeds {
            per_second: true,
            open_positions: true,
            ..MarketNeeds::default()
        };
        assert_eq!(t.observation_due(&near, 100, 251, &c), None);
        assert_eq!(t.observation_due(&near, 101, 252, &c), Some(Due::Second));
        let no_route = MarketNeeds {
            no_route: true,
            open_positions: true,
            ..MarketNeeds::default()
        };
        assert_eq!(
            t.observation_due(&no_route, 130, 325, &c),
            Some(Due::NoRoute)
        );
        // The route just went away and no chain runs on chain yet: observe at once.
        let start = MarketNeeds {
            no_route: true,
            start_chain: true,
            open_positions: true,
            ..MarketNeeds::default()
        };
        assert_eq!(t.observation_due(&start, 101, 252, &c), Some(Due::NoRoute));
        assert_eq!(t.observation_due(&start, 100, 252, &c), None);
        // No positions, no route, no burst: nothing.
        assert_eq!(
            t.observation_due(&MarketNeeds::default(), 1_000, 9_000, &c),
            None
        );
        // M4V-5: a Raydium market without positions in its listing window, on the listing
        // cadence; at the window's end (and until its price state warmed up) every second.
        let listing = MarketNeeds {
            listing: true,
            ..MarketNeeds::default()
        };
        let mut t = MarketTrack::default();
        assert_eq!(
            t.observation_due(&listing, 1_000, 9_000, &c),
            Some(Due::Listing)
        );
        t.landed(1_000, 9_000);
        assert_eq!(t.observation_due(&listing, 1_059, 9_100, &c), None);
        assert_eq!(
            t.observation_due(&listing, 1_060, 9_200, &c),
            Some(Due::Listing)
        );
        let warm_up = MarketNeeds {
            listing: true,
            warm_up: true,
            ..MarketNeeds::default()
        };
        assert_eq!(t.observation_due(&warm_up, 1_000, 9_001, &c), None);
        assert_eq!(
            t.observation_due(&warm_up, 1_001, 9_002, &c),
            Some(Due::Second)
        );
    }

    const VENUE: VenueId = (1, Pubkey::new_from_array([7; 32]));

    /// The price found by [`MarketTrack::check_foreign`] (a market with open positions).
    fn price_of(t: &mut MarketTrack, m: &Market, c: &CoverageConfig) -> Option<i64> {
        t.check_foreign(m, c, true).price
    }

    #[test]
    fn a_foreign_deviation_starts_a_per_slot_burst() {
        let c = cov();
        let mut t = MarketTrack::default();
        let p = 1_000_000u128;
        t.record_view(10, Some(p), None, VENUE);
        t.record_view(10, Some(p + 5_000), None, VENUE); // +0.5 %: the keeper's own range
        let mut m = Market::zeroed();
        m.b_sec = 10;
        m.b_lo.set(p);
        m.b_hi.set(p + 5_000);
        assert_eq!(price_of(&mut t, &m, &c), None);
        // Someone observed +3 % in second 10.
        m.b_hi.set(p * 103 / 100);
        assert_eq!(price_of(&mut t, &m, &c), Some(10));
        assert_eq!(t.burst_until, 70);
        // Seen once: not re-detected when the bucket completes.
        m.lb_sec = 10;
        m.lb_lo.set(p);
        m.lb_hi.set(p * 103 / 100);
        m.b_sec = 11;
        m.b_lo.set(p);
        m.b_hi.set(p);
        t.record_view(11, Some(p), None, VENUE);
        assert_eq!(price_of(&mut t, &m, &c), None);
        let needs = MarketNeeds::default();
        t.landed(11, 30);
        assert_eq!(t.observation_due(&needs, 11, 30, &c), None);
        assert_eq!(t.observation_due(&needs, 11, 31, &c), Some(Due::Slot));
        assert_eq!(t.observation_due(&needs, 70, 200, &c), None);
        // An S3 on an executable venue suppresses the per-slot burst (duty 5).
        let s3 = MarketNeeds {
            per_second: true,
            s3_due_executable: true,
            ..MarketNeeds::default()
        };
        assert_eq!(t.observation_due(&s3, 11, 31, &c), None);
        // Without an own view of a second, nothing is concluded.
        assert_eq!(t.deviates(500, 1, 1, 200), None);
    }

    /// M4G-2 and FITS-2 (b): an atomic `[deposit, crank_price, withdraw]` or `[withdraw,
    /// crank_price, deposit]` leaves the bucket's price at the keeper's own view and its liquidity
    /// extremes off the pool's `√(X·Y)` — in the open bucket, or in the ring entry that integrated
    /// the last completed one. It starts **no** per-slot burst (only a foreign price does); on a
    /// market with open positions it starts `liq_cover_s` (10 s) of **per-second** coverage from
    /// its second, extended by each recurrence; on a market without positions nothing. Honest
    /// depths start nothing, and a venue change drops the depths viewed before it.
    #[test]
    fn a_foreign_liquidity_starts_per_second_coverage_only_with_positions() {
        use shortwin::state::RingEntry as AccRingEntry;
        let c = cov();
        let (p, h) = (1_000_000u128, 5_000_000u128);
        let open_bucket = |sec: i64, llo: u128, lhi: u128| {
            let mut m = Market::zeroed();
            m.b_sec = sec;
            m.b_lo.set(p);
            m.b_hi.set(p);
            m.b_llo.set(llo);
            m.b_lhi.set(lhi);
            m
        };
        // Without open positions: nothing, not even recorded.
        let mut t = MarketTrack::default();
        t.record_view(20, Some(p), Some(h), VENUE);
        for (llo, lhi) in [(8 * h, 8 * h), (h / 2, h)] {
            let f = t.check_foreign(&open_bucket(20, llo, lhi), &c, false);
            assert_eq!(f, Foreign::default());
        }
        assert!(!t.liq_covered(20) && t.burst_until == 0);
        // With positions: honest depths (within 2 %) start nothing.
        let f = t.check_foreign(&open_bucket(20, h, h + h / 100), &c, true);
        assert_eq!(f, Foreign::default());
        // A withdrawal-wrapped crank (the lows pulled 50 % under the depth): 10 s per second.
        let f = t.check_foreign(&open_bucket(20, h / 2, h), &c, true);
        assert_eq!(
            f,
            Foreign {
                price: None,
                genuine: None,
                liquidity: Some(20),
                liq_started: true
            }
        );
        assert_eq!((t.liq_until, t.burst_until), (30, 0), "no per-slot burst");
        // Seen once: the same second does not extend it again.
        assert_eq!(
            t.check_foreign(&open_bucket(20, h / 2, h), &c, true),
            Foreign::default()
        );
        let needs = |liq: bool| MarketNeeds {
            open_positions: true,
            per_second: liq,
            liq_covered: liq,
            ..MarketNeeds::default()
        };
        t.landed(20, 50);
        // Per second, not per slot: nothing more in second 20, one observation in 21.
        assert_eq!(
            t.observation_due(&needs(t.liq_covered(20)), 20, 51, &c),
            None
        );
        assert_eq!(
            t.observation_due(&needs(t.liq_covered(21)), 21, 52, &c),
            Some(Due::Second)
        );
        // A recurrence in second 25 (a deposit-wrapped crank, 7× the pool) extends it to 35.
        t.record_view(25, Some(p), Some(h), VENUE);
        let f = t.check_foreign(&open_bucket(25, h, 8 * h), &c, true);
        assert_eq!((f.liquidity, f.liq_started), (Some(25), false));
        assert_eq!(t.liq_until, 35);
        assert!(t.liq_covered(34) && !t.liq_covered(35));
        // The last completed bucket: its integrating ring entry carries the pushed lows.
        t.record_view(40, Some(p), Some(h), VENUE);
        t.record_view(41, Some(p), Some(h), VENUE);
        let mut m = Market::zeroed();
        m.lb_sec = 40;
        m.lb_lo.set(p);
        m.lb_hi.set(p);
        m.b_sec = 41;
        m.ring_count = 1;
        let mut e = AccRingEntry {
            ts: 41,
            ..AccRingEntry::default()
        };
        e.lo.set(p);
        e.hi.set(p);
        e.llo.set(h / 3);
        e.lhi.set(h);
        m.ring[0] = e;
        let f = t.check_foreign(&m, &c, true);
        assert_eq!(
            (f.price, f.liquidity, f.liq_started),
            (None, Some(40), true)
        );
        assert_eq!(t.liq_until, 50);
        // A ring entry that did not integrate that bucket (other price extremes) says nothing.
        t.record_view(43, Some(p), Some(h), VENUE);
        m.lb_sec = 43;
        m.ring[0].ts = 44;
        m.ring[0].lo.set(p + 1);
        assert_eq!(t.check_foreign(&m, &c, true), Foreign::default());
        // A price off the view in the same bucket still starts a per-slot burst.
        let mut m = open_bucket(41, h, h);
        m.b_hi.set(p * 103 / 100);
        assert_eq!(price_of(&mut t, &m, &c), Some(41));
        // A venue change (an adoption rewrote the ring's liquidity): depths read on the old
        // venue are not compared any more; the new venue's are.
        let mut t = MarketTrack::default();
        t.record_view(60, Some(p), Some(h), VENUE);
        t.record_view(
            61,
            Some(p),
            Some(3 * h),
            (2, Pubkey::new_from_array([9; 32])),
        );
        let f = t.check_foreign(&open_bucket(60, 3 * h, 3 * h), &c, true);
        assert_eq!(f, Foreign::default());
        let f = t.check_foreign(&open_bucket(61, h, 3 * h), &c, true);
        assert_eq!(f.liquidity, Some(61));
        // `liq_cover_s = 0` disables the trigger.
        let off = CoverageConfig {
            liq_cover_s: 0,
            ..cov()
        };
        let mut t = MarketTrack::default();
        t.record_view(70, Some(p), Some(h), VENUE);
        let f = t.check_foreign(&open_bucket(70, h / 2, h), &off, true);
        assert_eq!(f, Foreign::default());
    }

    #[test]
    fn misses_degrade_and_ten_minutes_of_coverage_restore() {
        let c = cov();
        let mk = Pubkey::new_unique();
        let mut t = MarketTrack::default();
        let mut alerts = Vec::new();
        for s in 0..3 {
            t.required(s);
            t.close_second(mk, s + 1, &c, &mut alerts);
        }
        assert!(matches!(t.state, CoverageState::Degraded { since: 2 }));
        assert!(matches!(
            alerts.as_slice(),
            [Alert::Degraded {
                reason: DegradedReason::Misses(3),
                ..
            }]
        ));
        alerts.clear();
        for s in 3..(3 + 601) {
            t.required(s);
            t.landed(s, 0);
            t.close_second(mk, s + 1, &c, &mut alerts);
        }
        assert_eq!(t.state, CoverageState::Healthy);
        assert!(matches!(
            alerts.as_slice(),
            [Alert::CoverageRestored { .. }]
        ));
    }

    /// O17 (the global budget, decision A of 2026-10-01): a refused charge on a market with
    /// positions degrades it at once (one page); the recovery lifts it after `recovery_s` even
    /// when nothing is required in between ([`MarketTrack::advance`]); a flag found set at first
    /// sight starts the recovery.
    #[test]
    fn a_refused_charge_degrades_at_once_and_idle_seconds_recover() {
        let c = cov();
        let mk = Pubkey::new_unique();
        let mut t = MarketTrack::default();
        let mut alerts = Vec::new();
        t.on_budget_exhausted(mk, 100, 9_000, 1_000_000_000, Some(3), &mut alerts);
        t.on_budget_exhausted(mk, 101, 9_000, 1_000_000_000, Some(3), &mut alerts);
        assert!(matches!(t.state, CoverageState::Degraded { since: 100 }));
        assert!(t.own_degraded);
        assert!(matches!(
            alerts.as_slice(),
            [Alert::Degraded {
                reason: DegradedReason::Budget {
                    spent: 9_000,
                    budget: 1_000_000_000,
                    rank: Some(3)
                },
                ..
            }]
        ));
        alerts.clear();
        // An exit lands (never budgeted): the second is met, recovery starts...
        t.landed(130, 0);
        t.close_second(mk, 131, &c, &mut alerts);
        assert!(matches!(
            t.state,
            CoverageState::Recovering { since: 130, .. }
        ));
        // ...but the next observation the budget skips degrades it again (the keeper calls
        // `on_budget_exhausted` at every refusal), without a new page.
        t.required(160);
        t.on_budget_exhausted(mk, 160, 9_000, 1_000_000_000, Some(3), &mut alerts);
        t.close_second(mk, 161, &c, &mut alerts);
        assert!(matches!(t.state, CoverageState::Degraded { since: 100 }));
        assert!(alerts.is_empty());
        // A new UTC day: observations land again; nothing else is required in between (the idle
        // cadence) and the recovery completes 600 s after the first met second.
        let day = 86_400;
        t.landed(day, 0);
        t.close_second(mk, day + 1, &c, &mut alerts);
        for now in (day + 2)..=(day + 600) {
            t.advance(mk, now, true, &c, &mut alerts);
            assert!(alerts.is_empty(), "{now}");
        }
        t.advance(mk, day + 601, true, &c, &mut alerts);
        assert_eq!(t.state, CoverageState::Healthy);
        assert!(!t.own_degraded, "a healthy market's episode is over");
        assert!(matches!(
            alerts.as_slice(),
            [Alert::CoverageRestored { degraded_s, .. }] if *degraded_s == day + 600 - 100
        ));
        // Budget-degraded, then its positions close: nothing is required any more, so the
        // seconds after the last refusal are met and the pause lifts 600 s after it.
        let mut t = MarketTrack::default();
        t.on_budget_exhausted(mk, 1_000, 1, 1, None, &mut alerts);
        alerts.clear();
        for now in 1_001..=1_601 {
            t.advance(mk, now, false, &c, &mut alerts);
        }
        assert!(matches!(
            t.state,
            CoverageState::Recovering { since: 1_001, .. }
        ));
        t.advance(mk, 1_602, false, &c, &mut alerts);
        assert_eq!(t.state, CoverageState::Healthy);
        assert_eq!(alerts.len(), 1);
        // The keeper down while a market with positions recovers: the recovery restarts after
        // the downtime (those seconds were not covered).
        let mut t = MarketTrack::default();
        t.on_budget_exhausted(mk, 2_000, 1, 1, None, &mut alerts);
        alerts.clear();
        for now in 2_001..=2_300 {
            t.advance(mk, now, true, &c, &mut alerts);
        }
        assert!(matches!(
            t.state,
            CoverageState::Recovering { since: 2_001, .. }
        ));
        t.advance(mk, 2_900, true, &c, &mut alerts);
        assert!(matches!(t.state, CoverageState::Degraded { since: 2_000 }));
        t.advance(mk, 2_901, true, &c, &mut alerts);
        assert!(matches!(
            t.state,
            CoverageState::Recovering { since: 2_900, .. }
        ));
        for now in 2_902..=3_500 {
            t.advance(mk, now, true, &c, &mut alerts);
        }
        assert!(t.state.is_degraded() && alerts.is_empty());
        t.advance(mk, 3_501, true, &c, &mut alerts);
        assert_eq!(t.state, CoverageState::Healthy);
        alerts.clear();
        // A set flag on a healthy market (first sight: a restart): recovering from now; seen
        // again while recovering, nothing more; still set once the keeper has recovered, it is
        // the keeper's to lift (not adopted again).
        let mut t = MarketTrack::default();
        assert_eq!(t.sync_pause_flag(500, true, false), Some(FlagSet::Found));
        assert_eq!(t.sync_pause_flag(501, true, false), None);
        assert!(matches!(
            t.state,
            CoverageState::Recovering { since: 500, .. }
        ));
        assert!(!t.own_degraded, "adopted, not degraded by this keeper");
        t.advance(mk, 1_101, false, &c, &mut alerts);
        assert_eq!(t.state, CoverageState::Healthy);
        assert_eq!(t.sync_pause_flag(1_101, true, false), None);
        assert_eq!(t.state, CoverageState::Healthy);
        // Cleared, then set again by someone else: adopted, and foreign (★OWNER-M7-3 (b)).
        assert_eq!(t.sync_pause_flag(1_102, false, false), None);
        assert_eq!(
            t.sync_pause_flag(1_103, true, false),
            Some(FlagSet::Foreign)
        );
        alerts.clear();
        let mut t = MarketTrack::default();
        assert_eq!(t.sync_pause_flag(500, false, false), None);
        assert_eq!(t.state, CoverageState::Healthy);
        // The keeper's own unpause pending: not adopted again.
        assert_eq!(t.sync_pause_flag(501, true, true), None);
        assert_eq!(t.state, CoverageState::Healthy);
    }

    /// ★OWNER-M7-3 (b): a flag someone else clears — the Emergency multisig, typically — ends an
    /// adopted recovery (the market is healthy; the pauser does not set it again), but not the
    /// keeper's own degraded episode (it stays degraded; the pauser re-sets the flag). A
    /// recovery completed clears the episode.
    #[test]
    fn a_cleared_flag_ends_only_an_adopted_recovery() {
        let c = cov();
        let mk = Pubkey::new_unique();
        let mut alerts = Vec::new();
        // Adopted (found set, or set by someone else): cleared → healthy.
        let mut t = MarketTrack::default();
        assert!(t.sync_pause_flag(10, true, false).is_some());
        assert!(t.on_flag_cleared_by_other());
        assert_eq!(t.state, CoverageState::Healthy);
        // Degraded by the keeper itself: stays degraded.
        let mut t = MarketTrack::default();
        t.on_coverage_gap(mk, 20, 45, 1, &mut alerts);
        assert!(t.own_degraded && t.state.is_degraded());
        assert!(!t.on_flag_cleared_by_other());
        assert!(t.state.is_degraded());
        // Adopted first, then the keeper's own misses: its own episode now.
        let mut t = MarketTrack::default();
        assert!(t.sync_pause_flag(30, true, false).is_some());
        for s in 31..34 {
            t.required(s);
            t.close_second(mk, s + 1, &c, &mut alerts);
        }
        assert!(t.own_degraded);
        assert!(!t.on_flag_cleared_by_other());
        // Not degraded at all: nothing to end.
        let mut t = MarketTrack::default();
        assert!(!t.on_flag_cleared_by_other());
    }

    #[test]
    fn the_spend_tally_is_per_utc_day() {
        let mut t = MarketTrack::default();
        t.spend(10, 10_000);
        t.spend(11, 5_000);
        assert_eq!(t.spent_today, 15_000);
        t.spend(86_400, 1);
        assert_eq!(t.spent_today, 1);
    }

    /// Non-covered markets get no burst and no liquidity-anomaly window.
    #[test]
    fn stopping_bursts() {
        let mut t = MarketTrack {
            burst_until: 100,
            liq_until: 90,
            ..MarketTrack::default()
        };
        assert!(t.in_burst(50) && t.liq_covered(50));
        t.stop_bursts();
        assert!(!t.in_burst(50) && !t.liq_covered(50));
    }

    /// A deterministic xorshift64 stream for the property tests.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    /// One keeper tick of a market with open positions at `now`, as `Keeper::tick` drives the
    /// track: close the previous second, advance, note the needs, the observation if due and
    /// `lands`.
    fn tick(
        t: &mut MarketTrack,
        mk: Pubkey,
        now: i64,
        needs: &MarketNeeds,
        lands: bool,
        c: &CoverageConfig,
        alerts: &mut Vec<Alert>,
    ) -> Option<Due> {
        t.close_second(mk, now, c, alerts);
        t.advance(mk, now, true, c, alerts);
        if needs.per_second {
            t.note_every_second(now);
        }
        let due = t.observation_due(needs, now, 0, c);
        if due.is_some() {
            t.required(now);
            if lands {
                t.landed(now, 0);
            }
        }
        due
    }

    /// M7-SEC-1 (b), property: on the idle cadence, ticks that skip up to `tick_gap_tolerance_s`
    /// seconds (a tick that waited for its confirmations) never degrade a healthy market, and a
    /// degraded one recovers `recovery_s` after its last bad second whatever the skips — as
    /// built at M7 any skipped second restarted the recovery, so it never completed.
    #[test]
    fn slow_ticks_on_the_idle_cadence_neither_degrade_nor_block_the_recovery() {
        let c = cov();
        let mk = Pubkey::new_unique();
        let idle = MarketNeeds {
            open_positions: true,
            ..MarketNeeds::default()
        };
        for seed in 1..=200u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let mut t = MarketTrack::default();
            let mut alerts = Vec::new();
            let mut now = 1_000_000;
            for _ in 0..3_000 {
                tick(&mut t, mk, now, &idle, true, &c, &mut alerts);
                now += 1 + i64::try_from(
                    rng.below(u64::try_from(c.tick_gap_tolerance_s).unwrap() + 1),
                )
                .unwrap();
            }
            assert_eq!(t.state, CoverageState::Healthy, "seed {seed}");
            assert!(alerts.is_empty(), "seed {seed}: {alerts:?}");
            // Degraded by an outage (the gap's event in this tick's crank), then the same slow
            // ticks: healthy again ≈ recovery_s later.
            tick(&mut t, mk, now, &idle, true, &c, &mut alerts);
            t.on_coverage_gap(mk, now, 45, 1, &mut alerts);
            let degraded_at = now;
            let mut lifted = None;
            for _ in 0..3_000 {
                now += 1 + i64::try_from(
                    rng.below(u64::try_from(c.tick_gap_tolerance_s).unwrap() + 1),
                )
                .unwrap();
                tick(&mut t, mk, now, &idle, true, &c, &mut alerts);
                if t.state == CoverageState::Healthy {
                    lifted = Some(now - degraded_at);
                    break;
                }
            }
            let lifted = lifted.unwrap_or_else(|| panic!("seed {seed}: never recovered"));
            assert!(
                (c.recovery_s..=c.recovery_s + 1 + c.tick_gap_tolerance_s).contains(&lifted),
                "seed {seed}: recovered after {lifted} s"
            );
        }
    }

    /// M7-SEC-1 (b): a longer stall is the keeper down — a recovering market restarts its
    /// recovery — and on a market that needed an observation every second the skipped seconds
    /// are misses: `degraded_after_misses` (3) in a row degrade it, an isolated one does not.
    #[test]
    fn skipped_required_seconds_are_misses_and_a_long_stall_restarts_the_recovery() {
        let c = cov();
        let mk = Pubkey::new_unique();
        let near = MarketNeeds {
            per_second: true,
            open_positions: true,
            ..MarketNeeds::default()
        };
        let mut alerts = Vec::new();
        // Isolated skipped seconds (a slow tick every other tick) never degrade.
        let mut t = MarketTrack::default();
        let mut now = 5_000;
        for i in 0..600 {
            tick(&mut t, mk, now, &near, true, &c, &mut alerts);
            now += if i % 2 == 0 { 2 } else { 1 };
        }
        assert_eq!(t.state, CoverageState::Healthy);
        assert!(alerts.is_empty(), "{alerts:?}");
        // A tick three seconds late: three required seconds missed in a row.
        tick(&mut t, mk, now, &near, true, &c, &mut alerts);
        now += 4;
        tick(&mut t, mk, now, &near, true, &c, &mut alerts);
        assert!(t.state.is_degraded(), "{:?}", t.state);
        assert!(matches!(
            alerts.as_slice(),
            [Alert::Degraded {
                reason: DegradedReason::Misses(3),
                ..
            }]
        ));
        // The same skip on the idle cadence is nothing.
        let idle = MarketNeeds {
            open_positions: true,
            ..MarketNeeds::default()
        };
        let mut t = MarketTrack::default();
        let mut alerts = Vec::new();
        tick(&mut t, mk, 9_000, &idle, true, &c, &mut alerts);
        tick(&mut t, mk, 9_004, &idle, true, &c, &mut alerts);
        assert_eq!(t.state, CoverageState::Healthy);
        // A stall over the tolerance while recovering restarts the recovery.
        t.on_coverage_gap(mk, 9_010, 40, 1, &mut alerts);
        let mut now = 9_011;
        for _ in 0..300 {
            tick(&mut t, mk, now, &idle, true, &c, &mut alerts);
            now += 1;
        }
        assert!(matches!(t.state, CoverageState::Recovering { .. }));
        // A skip of the tolerance plus one second.
        now += c.tick_gap_tolerance_s + 1;
        tick(&mut t, mk, now, &idle, true, &c, &mut alerts);
        assert!(
            matches!(t.state, CoverageState::Degraded { .. }),
            "{:?}",
            t.state
        );
        let restart = now;
        tick(&mut t, mk, now + 1, &idle, true, &c, &mut alerts);
        assert!(
            matches!(t.state, CoverageState::Recovering { since, .. } if since >= restart),
            "{:?}",
            t.state
        );
        // Within the tolerance it would not have.
        let mut t2 = MarketTrack::default();
        t2.on_coverage_gap(mk, 9_010, 40, 1, &mut alerts);
        let mut now = 9_011;
        for _ in 0..300 {
            tick(&mut t2, mk, now, &idle, true, &c, &mut alerts);
            now += 1;
        }
        // (`now` is already the second after the last tick: a skip of exactly the tolerance.)
        now += c.tick_gap_tolerance_s;
        tick(&mut t2, mk, now, &idle, true, &c, &mut alerts);
        assert!(matches!(t2.state, CoverageState::Recovering { since, .. } if since <= 9_011));
    }

    /// M7-SEC-1, the review's property: required observations that land within two seconds of
    /// schedule — every tick a second or a few late, the crank landing — never degrade a market,
    /// on any mix of per-second and idle needs.
    #[test]
    fn observations_landing_within_two_seconds_of_schedule_never_degrade() {
        let c = cov();
        let mk = Pubkey::new_unique();
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0xd1b5_4a32_d192_ed03) | 1);
            let mut t = MarketTrack::default();
            let mut alerts = Vec::new();
            let mut now = 2_000_000;
            for _ in 0..2_000 {
                let needs = MarketNeeds {
                    per_second: rng.below(4) == 0,
                    open_positions: true,
                    ..MarketNeeds::default()
                };
                tick(&mut t, mk, now, &needs, true, &c, &mut alerts);
                // The next tick 1–3 s later: at most two skipped seconds, never three in a row.
                now += 1 + i64::try_from(rng.below(3)).unwrap();
            }
            assert!(alerts.is_empty(), "seed {seed}: {alerts:?}");
            assert_eq!(t.state, CoverageState::Healthy, "seed {seed}");
        }
    }

    /// M7-SEC-2, property: a foreign price the keeper's next read (in a later second) still shows
    /// is a genuine move — no burst, decided once; an excursion that reverted by the next read is
    /// a burst; a price the keeper itself saw the second before is no excursion at all.
    #[test]
    fn only_reverted_excursions_burst() {
        let c = cov();
        let p = 1_000_000u128;
        let bucket = |sec: i64, lo: u128, hi: u128| {
            let mut m = Market::zeroed();
            m.b_sec = sec;
            m.b_lo.set(lo);
            m.b_hi.set(hi);
            m
        };
        for seed in 1..=500u64 {
            let mut rng = Rng(seed.wrapping_mul(0xa076_1d64_78bd_642f) | 1);
            let mut t = MarketTrack::default();
            // The keeper read `p` in second 10 (before the move).
            t.record_view(10, Some(p), None, VENUE);
            // Someone observed `p·(1 ± k)` in second 10, k ∈ (2.5 %, 50 %).
            let k = 250 + rng.below(4_750);
            let up = rng.below(2) == 0;
            let moved = if up {
                p * (10_000 + u128::from(k)) / 10_000
            } else {
                p * 10_000 / (10_000 + u128::from(k))
            };
            let m = if up {
                bucket(10, p, moved)
            } else {
                bucket(10, moved, p)
            };
            let genuine = rng.below(2) == 0;
            // The keeper's next read, 1–2 s later: the price still there (± 0.5 %), or back.
            let later = 11 + i64::try_from(rng.below(2)).unwrap();
            let jitter = 9_950 + u128::from(rng.below(100));
            let next = if genuine { moved } else { p } * jitter / 10_000;
            t.record_view(later, Some(next), None, VENUE);
            let f = t.check_foreign(&m, &c, true);
            if genuine {
                assert_eq!((f.price, f.genuine), (None, Some(10)), "seed {seed}");
                assert_eq!(t.burst_until, 0);
            } else {
                assert_eq!((f.price, f.genuine), (Some(10), None), "seed {seed}");
                assert_eq!(t.burst_until, 10 + c.burst_s);
            }
            // Decided once.
            assert_eq!(t.check_foreign(&m, &c, true), Foreign::default());
        }
        // In the bucket's own second the current read is part of the range: an extreme off it
        // reverted (a burst at once).
        let mut t = MarketTrack::default();
        t.record_view(20, Some(p), None, VENUE);
        t.record_view(20, Some(p * 101 / 100), None, VENUE);
        let f = t.check_foreign(&bucket(20, p, p * 106 / 100), &c, true);
        assert_eq!((f.price, f.genuine), (Some(20), None));
        // A price the keeper read the second before (another instance's crank before a genuine
        // move, M7-SEC-2 (c)): no excursion.
        let mut t = MarketTrack::default();
        t.record_view(30, Some(p), None, VENUE);
        t.record_view(31, Some(p * 103 / 100), None, VENUE);
        let m = bucket(31, p, p * 103 / 100);
        assert_eq!(t.check_foreign(&m, &c, true), Foreign::default());
    }
}
