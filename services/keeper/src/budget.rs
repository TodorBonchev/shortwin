//! The global keeper budget (ARCHITECTURE §7.1 duty 8; ★owner decision A of 2026-10-01 on
//! OWNER-M7-13 / -14, SPEC §14 M7-13 / M7-14; ECON §5.8, §15 #109; config `[budget]`).
//!
//! One daily budget `G` (`global_daily_lamports`) replaces the per-market budgets of M7 as built
//! (`per_market_daily_lamports`, retired). It funds markets **in order of their lenders' value at
//! risk**; price bursts and liquidity-anomaly coverage draw on it; markets below a value-at-risk
//! floor get the idle cadence only; a market the budget can no longer cover is degraded (duty 9)
//! — per market, as before.
//!
//! - **Value at risk** ([`value_at_risk`]): `V_m = ⌊Market.borrowed_q · min(Market.ema_price,
//!   P_ACC_MAX) / 10¹⁸⌋` lamports (U256 intermediate, saturating at `u64::MAX`), from the `Market`
//!   as the tick read it — the lenders' borrowed inventory `ΣB_Q` at the protocol's own smoothed
//!   price, the product on which rent accrues and the S2/S3 immediate part.
//! - **Classes of market** ([`Tier`]): **covered** — open positions and `V ≥ var_floor_lamports`,
//!   ranked by `V` descending (ties: the older `created_at`, then the smaller key, bytewise);
//!   **below the floor** — open positions and `V` below it: the idle cadence only, read at that
//!   cadence and in the seconds its last observation's reference stays fresh while a position is
//!   near an exit (M7D-1); **without positions** — warm-ups and listing windows only.
//! - **Classes of charge** ([`ChargeClass`]): exit-critical (the as-built never-throttled set — on a
//!   covered market at most one crank a second, M7D-4; below the floor the idle crank an exit
//!   waits on, M7D-2), baseline (the idle cadence, duty 5's guard coverage, warm-ups and listing
//!   windows of markets without positions), discretionary (duty 2's price bursts — every further
//!   crank of a second — and liquidity-anomaly coverage).
//! - **Balances** (reset at 00:00 UTC, [`GlobalBudget::roll`]): the free pool `F` (starts at
//!   `G`), each covered market's unspent entitlement `U_m`, the day's tier-2 spend `T`, the
//!   overdraft `O`. A covered market's entitlement `R_m = ⌈ĉ_m · ⌈86,400 / idle_interval_s⌉ ·
//!   (10⁴ + reserve_margin_bps) / 10⁴⌉` ([`BudgetParams::entitlement`]; `ĉ_m` its last per-crank
//!   charge — before its first, the keeper's measured average per-crank charge, or
//!   `⌈5,000 / batch⌉` before any, [`BudgetParams::first_charge`], M7D-5); funded in rank order
//!   at 00:00 (`U_m = min(R_m, F)`); a market covered at second `s` of the day gets
//!   `⌈R_m·(86,400 − s)/86,400⌉` from `F`, then from the unspent entitlements of lower-ranked
//!   covered markets, lowest-ranked first; a rank change moves no balance.
//! - **Once a day** (M7D-3): a market is funded at most its day's entitlement (`entitled`, fixed at
//!   its first covering of the day); a re-join gets nothing more. What a join takes from
//!   lower-ranked markets is **restricted** — it pays only exit-critical and baseline charges,
//!   never discretionary ones — and a market that stops being covered returns its restricted `U`
//!   to the covered markets something was taken from (lowest-ranked first, up to what each lost),
//!   the rest of its `U` to `F`.
//! - **Re-estimate** (M7D-5): an entitlement funded at an estimated `ĉ` is recomputed at the
//!   market's first measured crank (prorated from the second it was first covered); what it holds
//!   above the new entitlement is released (restricted lamports back to the markets they came
//!   from, the rest to `F`), and whenever `F` grows that way (or a market leaves) the covered
//!   markets still short of their entitlement are topped up from `F` in rank order, up to what the
//!   rest of the day needs.
//! - **Admission** of a charge `c` on market `m` ([`GlobalBudget::admit`]), in order until `c` is
//!   covered: (1) `U_m` (a discretionary charge only its unrestricted part); (2) `F` — for a
//!   tier-2 market only while `T + c ≤ ⌊G · tail_share_bps / 10⁴⌋`; (3) for an exit-critical or
//!   baseline charge of a covered market — never a discretionary one — the lower-ranked covered
//!   markets' `U`, lowest-ranked first; (4) what is left: an exit-critical charge (on a covered
//!   market or below the floor), or a tier-3 warm-up or listing-window charge, is sent anyway and
//!   booked to `O` ([`Alert::ExitOverdraft`] once a day); any other charge is **refused**
//!   (nothing booked; [`Alert::BudgetExhausted`] at the day's first refusal) — on a market with
//!   open positions that is degraded mode at once (`DegradedReason::Budget`).
//! - **Settlement** ([`GlobalBudget::settle`]): a charge is admitted at its estimate `ĉ_m` and
//!   booked at what the crank's landed transaction cost the market (a batch's fee split equally):
//!   less is returned to the sources it was drawn from (the last first), more is drawn from the
//!   market's own `U`, then `F` (a tier-2 market's only within the tail cap), then — when the
//!   charge may overdraw — `O`; what is still left (an estimate's error the balances cannot take) is
//!   counted in [`GlobalBudget::excess`], never in `O`. A crank that does not land is released
//!   ([`GlobalBudget::release`]): everything returns.
//!
//! Within a tick the keeper presents charges exit-critical first, then baseline, then
//! discretionary (each in rank order), then tier 2 (exit-critical first), then tier 3
//! ([`GlobalBudget::order`]). `G = 0` is no budget: every charge is admitted (and still booked,
//! for the metrics). The budget restarts with the process (a restart funds the rest of the day
//! afresh, at the estimate, re-estimated at each market's first crank).

use std::collections::{BTreeMap, HashMap};

use anchor_lang::prelude::Pubkey;
use shortwin::state::Market;
use shortwin_math::consts::P_ACC_MAX;
use shortwin_math::num::{Rounding, WAD, mul_div};

use crate::alerts::Alert;

/// Seconds in a UTC day (the budget's period).
pub const DAY_S: i64 = 86_400;
const DAY_U128: u128 = 86_400;

/// One signature's base fee: an unbatched crank's charge at zero priority (the keeper's
/// [`BudgetParams::first_charge`] divides it by its batch size).
pub const FIRST_CHARGE_LAMPORTS: u64 = 5_000;

/// `V_m = ⌊borrowed_q · min(ema_price, P_ACC_MAX) / 10¹⁸⌋` lamports (module docs), saturating.
pub fn value_at_risk(m: &Market) -> u64 {
    let p = m.ema_price.get().min(P_ACC_MAX);
    mul_div(u128::from(m.borrowed_q), p, WAD, Rounding::Floor)
        .map_or(u64::MAX, |v| u64::try_from(v).unwrap_or(u64::MAX))
}

/// A market's class (module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Open positions and `V ≥ var_floor_lamports` (tier 1): ranked, funded.
    Covered,
    /// Open positions and `V < var_floor_lamports` (tier 2): the idle cadence only.
    BelowFloor,
    /// No open positions (tier 3): warm-ups and listing windows only.
    #[default]
    NoPositions,
}

impl Tier {
    /// The class of a market with (`open`) or without positions and value at risk `var`.
    pub fn of(open: bool, var: u64, floor: u64) -> Self {
        match (open, var >= floor) {
            (false, _) => Self::NoPositions,
            (true, true) => Self::Covered,
            (true, false) => Self::BelowFloor,
        }
    }

    /// 1, 2 or 3 (the metrics' gauge).
    pub fn number(self) -> u64 {
        match self {
            Self::Covered => 1,
            Self::BelowFloor => 2,
            Self::NoPositions => 3,
        }
    }
}

/// A charge's class (module docs), most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChargeClass {
    /// The as-built never-throttled set: per-second coverage of a position near a threshold,
    /// armed or warming up (at most one crank a second, M7D-4); a liquidatable or expired position
    /// waiting for a fresh reference or its bound (below the floor too, M7D-2); the S3 chain; a
    /// market without a route. May overdraw.
    ExitCritical,
    /// The idle cadence, duty 5's guard coverage after a switch, duty 1's per-second coverage of
    /// a market off its EMA, and the warm-ups and listing windows of markets without positions.
    Baseline,
    /// Duty 2's price bursts (every crank of a second after its first) and liquidity-anomaly
    /// coverage: never drawn from another market's entitlement, nor from the part of the market's
    /// own that a join took from lower-ranked markets.
    Discretionary,
}

impl ChargeClass {
    fn index(self) -> usize {
        match self {
            Self::ExitCritical => 0,
            Self::Baseline => 1,
            Self::Discretionary => 2,
        }
    }
}

/// One charge the keeper asks the budget for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChargeReq {
    pub class: ChargeClass,
    /// A warm-up or listing-window crank (on a market without positions: may overdraw, M4V-5,
    /// OWNER-M6SK-2).
    pub warm_up: bool,
}

impl ChargeReq {
    /// A charge of `class`, not a warm-up.
    pub const fn of(class: ChargeClass) -> Self {
        Self {
            class,
            warm_up: false,
        }
    }
}

/// The budget's parameters for one tick (`[budget]` and `[coverage] idle_interval_s`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetParams {
    /// `G`, lamports a UTC day (0: no budget).
    pub global: u64,
    /// `var_floor_lamports` (clamped to the deployment's `market_max_notional` by the keeper).
    pub var_floor: u64,
    pub reserve_margin_bps: u32,
    pub tail_share_bps: u32,
    pub idle_interval_s: i64,
    /// M7D-5: `ĉ` of a market without a measured crank while the keeper has measured none at all
    /// (a start or restart): `⌈5,000 / the batch size⌉` — one signature shared by a full batch.
    pub first_charge: u64,
}

impl BudgetParams {
    /// `⌊G · tail_share_bps / 10⁴⌋`: the most the tier-2 markets may draw from `F` in a day.
    pub fn tail_cap(&self) -> u64 {
        let v = u128::from(self.global) * u128::from(self.tail_share_bps) / 10_000;
        u64::try_from(v).unwrap_or(u64::MAX)
    }

    /// `R = ⌈charge · ⌈86,400 / idle_interval_s⌉ · (10⁴ + reserve_margin_bps) / 10⁴⌉`.
    pub fn entitlement(&self, charge: u64) -> u64 {
        let idle = self.idle_interval_s.max(1);
        let cranks = u128::try_from(DAY_S.div_euclid(idle) + i64::from(DAY_S % idle != 0))
            .unwrap_or(u128::MAX);
        let num = u128::from(charge)
            .saturating_mul(cranks)
            .saturating_mul(10_000 + u128::from(self.reserve_margin_bps));
        u64::try_from(num.div_ceil(10_000)).unwrap_or(u64::MAX)
    }

    /// `⌈r · (86,400 − s) / 86,400⌉` for second `s` of the day.
    pub fn prorated(r: u64, sec_of_day: i64) -> u64 {
        let left = u128::try_from(DAY_S - sec_of_day.clamp(0, DAY_S)).unwrap_or(0);
        u64::try_from((u128::from(r) * left).div_ceil(DAY_U128)).unwrap_or(u64::MAX)
    }
}

/// Where a part of a charge was drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The market's own `U`, its unrestricted part.
    Own,
    /// The market's own `U`, its restricted part (M7D-3).
    OwnRestricted,
    /// `F`.
    Free,
    /// `F`, as tier-2 spend (`T`).
    Tail,
    /// A lower-ranked covered market's `U` (`restricted`: its restricted part).
    Lower { key: Pubkey, restricted: bool },
    /// `O`.
    Overdraft,
}

/// An admitted charge, until its crank lands ([`GlobalBudget::settle`]) or does not
/// ([`GlobalBudget::release`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub market: Pubkey,
    pub req: ChargeReq,
    /// The estimate admitted (`ĉ_m`).
    pub amount: u64,
    parts: Vec<(Source, u64)>,
    tier: Tier,
    day: i64,
}

/// The budget's answer to a charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Admitted(Reservation),
    /// Refused (nothing booked); the market's rank (`None`: not covered).
    Refused {
        rank: Option<u32>,
    },
}

/// One market's budget state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarketBudget {
    pub tier: Tier,
    /// `V_m` at the last read.
    pub var: u64,
    pub created_at: i64,
    /// `U_m` (covered markets only).
    pub unspent: u64,
    /// M7D-3: the part of `U_m` a join took from lower-ranked markets' entitlements (≤ `unspent`):
    /// it pays only exit-critical and baseline charges, and returns to the markets it was taken
    /// from when this market stops being covered.
    pub restricted: u64,
    /// The entitlement funded today (all of `R_m`, prorated, or what was left of it), less what
    /// higher-ranked markets took from it (preemption): `U_m` plus what the market itself drew
    /// from `U_m`.
    pub funded: u64,
    /// M7D-3: what the market was granted today (the 00:00 funding, its join, top-ups) less what a
    /// re-estimate released: never above [`Self::entitled`], so a market that leaves and re-joins
    /// is not funded again.
    pub granted: u64,
    /// Today's entitlement: `R_m` at `ĉ_m`, prorated to the second the market was first covered
    /// today (`entitled_from`); re-estimated at its first measured crank when it rested on an
    /// estimate (M7D-5).
    pub entitled: u64,
    /// The second of the UTC day the market was first covered today (`None`: not yet today).
    pub entitled_from: Option<i64>,
    /// M7D-5: `entitled` rests on an estimated `ĉ` (no crank of the market measured yet).
    pub estimated: bool,
    /// M7D-3: lamports of `U_m` other markets took today (a join's preemption, admission step 3),
    /// net of what came back: what a leaving market's restricted `U` may return to.
    pub taken: u64,
    /// `ĉ_m`: what its last observation crank was charged.
    pub last_charge: Option<u64>,
    /// Today's spend by charge class (exit-critical, baseline, discretionary).
    pub spent: [u64; 3],
    /// Charges refused today.
    pub refused: u64,
}

impl MarketBudget {
    /// `ĉ_m`, or `est` before the market's first measured crank (module docs).
    pub fn charge_or(&self, est: u64) -> u64 {
        self.last_charge.unwrap_or(est)
    }

    /// `ĉ_m`, or one unbatched signature before its first crank.
    pub fn charge(&self) -> u64 {
        self.charge_or(FIRST_CHARGE_LAMPORTS)
    }

    /// The unrestricted part of `U_m` (what a discretionary charge may draw).
    pub fn general(&self) -> u64 {
        self.unspent.saturating_sub(self.restricted)
    }

    /// Today's spend.
    pub fn spent_today(&self) -> u64 {
        self.spent.iter().fold(0u64, |a, b| a.saturating_add(*b))
    }

    /// Takes `t` (≤ `unspent`) from `U_m` for another market — the restricted part first — and
    /// returns how much of it was restricted.
    fn give_up(&mut self, t: u64) -> u64 {
        let r = t.min(self.restricted);
        self.restricted -= r;
        self.unspent -= t;
        self.funded = self.funded.saturating_sub(t);
        self.taken = self.taken.saturating_add(t);
        r
    }
}

/// The global budget (module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalBudget {
    /// The UTC day the balances belong to.
    pub day: Option<i64>,
    /// `F`.
    pub free: u64,
    /// `T`: today's tier-2 spend from `F`.
    pub tail: u64,
    /// `O`: today's charges booked beyond `G` (exit-critical, tier-3 warm-ups).
    pub overdraft: u64,
    /// Today's spend (every booked charge).
    pub spent: u64,
    /// Today's spend above admitted estimates that no balance could take (never in `O`).
    pub excess: u64,
    /// `G` of the last roll.
    pub global: u64,
    /// `⌊G · tail_share_bps / 10⁴⌋` of the last roll or admission.
    pub tail_cap: u64,
    /// M7D-5: the keeper's measured per-crank charge (a running average of every landed crank's
    /// share; `None` before the first): `ĉ` of a market without a measured crank of its own.
    pub avg_charge: Option<u64>,
    pub markets: BTreeMap<Pubkey, MarketBudget>,
    /// The parameters and clock of the last roll, classification or admission (a settlement's
    /// re-estimate and top-up use them).
    last: Option<(i64, BudgetParams)>,
    exhausted_day: Option<i64>,
    overdraft_day: Option<i64>,
}

impl GlobalBudget {
    /// `ĉ` of a market without a measured crank: the measured average, else `first_charge`.
    pub fn estimate(&self, p: &BudgetParams) -> u64 {
        self.avg_charge.unwrap_or(p.first_charge).max(1)
    }

    /// Starts a new UTC day if `now` is in one (00:00 UTC; the keeper's first tick too): `F = G`,
    /// `T = O = 0`, the day's spend reset, and every covered market's entitlement funded from the
    /// top, `U_m = min(R_m, F)` (prorated to `now`'s second of the day). Returns whether it rolled.
    /// Within a day a changed `G` moves the difference into (or out of) `F`.
    pub fn roll(&mut self, now: i64, p: &BudgetParams) -> bool {
        self.last = Some((now, *p));
        let day = now.div_euclid(DAY_S);
        if self.day == Some(day) {
            // `G` changed during the day (the owner's configuration): the difference goes to (or
            // comes out of) the free pool.
            if p.global != self.global {
                self.free = if p.global > self.global {
                    self.free.saturating_add(p.global - self.global)
                } else {
                    self.free.saturating_sub(self.global - p.global)
                };
                self.global = p.global;
                self.tail_cap = p.tail_cap();
            }
            return false;
        }
        self.day = Some(day);
        self.global = p.global;
        self.tail_cap = p.tail_cap();
        self.free = p.global;
        self.tail = 0;
        self.overdraft = 0;
        self.spent = 0;
        self.excess = 0;
        for m in self.markets.values_mut() {
            m.unspent = 0;
            m.restricted = 0;
            m.funded = 0;
            m.granted = 0;
            m.entitled = 0;
            m.entitled_from = None;
            m.estimated = false;
            m.taken = 0;
            m.spent = [0; 3];
            m.refused = 0;
        }
        if p.global == 0 {
            return true;
        }
        let sec = now.rem_euclid(DAY_S);
        let est = self.estimate(p);
        for k in self.ranked() {
            if let Some(m) = self.markets.get_mut(&k) {
                m.entitled = BudgetParams::prorated(p.entitlement(m.charge_or(est)), sec);
                m.entitled_from = Some(sec);
                m.estimated = m.last_charge.is_none();
                let u = m.entitled.min(self.free);
                m.unspent = u;
                m.funded = u;
                m.granted = u;
                self.free -= u;
            }
        }
        true
    }

    /// The tier of `key` at its last read (`None`: never classified).
    pub fn tier(&self, key: &Pubkey) -> Option<Tier> {
        self.markets.get(key).map(|m| m.tier)
    }

    /// The covered markets, highest rank first: `V` descending, then the older `created_at`, then
    /// the smaller key (bytewise).
    pub fn ranked(&self) -> Vec<Pubkey> {
        let mut v: Vec<(&Pubkey, &MarketBudget)> = self
            .markets
            .iter()
            .filter(|(_, m)| m.tier == Tier::Covered)
            .collect();
        v.sort_by(|(ka, a), (kb, b)| {
            b.var
                .cmp(&a.var)
                .then(a.created_at.cmp(&b.created_at))
                .then(ka.cmp(kb))
        });
        v.into_iter().map(|(k, _)| *k).collect()
    }

    /// The rank of a covered market (1 = the most value at risk).
    pub fn rank(&self, key: &Pubkey) -> Option<u32> {
        self.ranked()
            .iter()
            .position(|k| k == key)
            .and_then(|i| u32::try_from(i + 1).ok())
    }

    /// Classifies `key` from a fresh read (`open`: it has positions; `var`: its `V`), and moves
    /// the balances a change of class implies: a market that becomes covered is funded — once a
    /// day: prorated, from `F`, then from lower-ranked markets, lowest first —; one that stops
    /// being covered returns its `U` (module docs). A rank change moves nothing. Returns its tier.
    pub fn classify(
        &mut self,
        now: i64,
        key: Pubkey,
        open: bool,
        var: u64,
        created_at: i64,
        p: &BudgetParams,
    ) -> Tier {
        self.roll(now, p);
        let tier = Tier::of(open, var, p.var_floor);
        let m = self.markets.entry(key).or_default();
        let was = m.tier;
        m.var = var;
        m.created_at = created_at;
        m.tier = tier;
        match (was == Tier::Covered, tier == Tier::Covered) {
            (false, true) => self.join(now, key, p),
            (true, false) => {
                self.leave(&key);
                self.top_up(now, p);
            }
            _ => {}
        }
        tier
    }

    /// Forgets a market the keeper no longer covers (its `U` returns as on a leave).
    pub fn forget(&mut self, key: &Pubkey) {
        if let Some(m) = self.markets.get_mut(key) {
            m.tier = Tier::NoPositions;
        }
        self.leave(key);
        self.markets.remove(key);
        if let Some((now, p)) = self.last {
            self.top_up(now, &p);
        }
    }

    /// A market became covered: its entitlement — fixed at its first covering of the day — less
    /// what it was already granted today, from `F`, then from the lower-ranked markets' `U`,
    /// lowest-ranked first (restricted: module docs).
    fn join(&mut self, now: i64, key: Pubkey, p: &BudgetParams) {
        if p.global == 0 {
            return;
        }
        let est = self.estimate(p);
        let Some(m) = self.markets.get_mut(&key) else {
            return;
        };
        if m.entitled_from.is_none() {
            let sec = now.rem_euclid(DAY_S);
            m.entitled = BudgetParams::prorated(p.entitlement(m.charge_or(est)), sec);
            m.entitled_from = Some(sec);
            m.estimated = m.last_charge.is_none();
        }
        let mut need = m.entitled.saturating_sub(m.granted);
        let from_free = need.min(self.free);
        self.free -= from_free;
        need -= from_free;
        let mut from_lower = 0u64;
        if need > 0 {
            let ranked = self.ranked();
            let below = ranked
                .iter()
                .position(|k| *k == key)
                .map_or(&ranked[..0], |i| &ranked[i + 1..]);
            for k in below.iter().rev() {
                if need == 0 {
                    break;
                }
                if let Some(v) = self.markets.get_mut(k) {
                    let t = need.min(v.unspent);
                    v.give_up(t);
                    need -= t;
                    from_lower += t;
                }
            }
        }
        if let Some(m) = self.markets.get_mut(&key) {
            let got = from_free + from_lower;
            m.unspent += got;
            m.restricted += from_lower;
            m.funded += got;
            m.granted += got;
        }
    }

    /// A market stopped being covered: its restricted `U` returns to the markets something was
    /// taken from, the rest to `F`.
    fn leave(&mut self, key: &Pubkey) {
        let Some(m) = self.markets.get_mut(key) else {
            return;
        };
        let (u, r) = (m.unspent, m.restricted);
        m.unspent = 0;
        m.restricted = 0;
        m.funded = m.funded.saturating_sub(u);
        self.free = self.free.saturating_add(u - r);
        self.return_restricted(key, r);
    }

    /// Returns `amount` restricted lamports (of `from`'s `U`) to the covered markets other markets
    /// took from (`taken`), lowest-ranked first, up to what each lost; the rest to `F`.
    fn return_restricted(&mut self, from: &Pubkey, amount: u64) {
        let mut left = amount;
        if left > 0 {
            for k in self.ranked().iter().rev() {
                if left == 0 {
                    break;
                }
                if k == from {
                    continue;
                }
                if let Some(m) = self.markets.get_mut(k) {
                    let t = left.min(m.taken);
                    m.taken -= t;
                    m.unspent += t;
                    m.funded += t;
                    left -= t;
                }
            }
        }
        self.free = self.free.saturating_add(left);
    }

    /// Tops up from `F`, in rank order, every covered market still short of its entitlement
    /// (`granted < entitled`: funded partly or not at all when it was covered, or re-estimated
    /// upwards) — at most up to what the rest of the day needs at its `ĉ` (M7D-5).
    fn top_up(&mut self, now: i64, p: &BudgetParams) {
        if p.global == 0 || self.free == 0 {
            return;
        }
        let sec = now.rem_euclid(DAY_S);
        let est = self.estimate(p);
        for k in self.ranked() {
            if self.free == 0 {
                break;
            }
            let Some(m) = self.markets.get_mut(&k) else {
                continue;
            };
            let cap = m.entitled.saturating_sub(m.granted);
            if cap == 0 {
                continue;
            }
            let need = BudgetParams::prorated(p.entitlement(m.charge_or(est)), sec)
                .saturating_sub(m.unspent);
            let t = cap.min(need).min(self.free);
            m.unspent += t;
            m.funded += t;
            m.granted += t;
            self.free -= t;
        }
    }

    /// M7D-5: the first measured crank (`actual`) of a covered market whose entitlement rested on
    /// an estimate: the entitlement is recomputed at it (prorated from the second it was first
    /// covered); what the market holds above it is released (restricted lamports back to the
    /// markets they came from, the rest to `F`), and the markets still short are topped up.
    fn re_estimate(&mut self, key: Pubkey, actual: u64) {
        let Some((now, p)) = self.last else { return };
        let Some(m) = self.markets.get_mut(&key) else {
            return;
        };
        if !m.estimated || m.tier != Tier::Covered || p.global == 0 {
            return;
        }
        m.estimated = false;
        let from = m.entitled_from.unwrap_or(0);
        let new = BudgetParams::prorated(p.entitlement(actual), from);
        let grew = new > m.entitled;
        m.entitled = new;
        let over = m.granted.saturating_sub(new).min(m.unspent);
        if over > 0 {
            let r = over.min(m.restricted);
            m.restricted -= r;
            m.unspent -= over;
            m.funded = m.funded.saturating_sub(over);
            m.granted -= over;
            self.free = self.free.saturating_add(over - r);
            self.return_restricted(&key, r);
        }
        if over > 0 || grew {
            self.top_up(now, &p);
        }
    }

    /// The covered markets' ranks (1 = the most value at risk).
    pub fn ranks(&self) -> HashMap<Pubkey, u32> {
        self.ranked()
            .into_iter()
            .enumerate()
            .map(|(i, k)| (k, u32::try_from(i + 1).unwrap_or(u32::MAX)))
            .collect()
    }

    /// The presentation order of a charge within a tick (module docs): exit-critical, baseline,
    /// discretionary — each by rank ([`Self::ranks`]) — then tier 2 (exit-critical first), then
    /// tier 3 (each by key).
    pub fn order(
        &self,
        key: &Pubkey,
        req: ChargeReq,
        ranks: &HashMap<Pubkey, u32>,
    ) -> (u8, u32, Pubkey) {
        let class = match req.class {
            ChargeClass::ExitCritical => 0,
            ChargeClass::Baseline => 1,
            ChargeClass::Discretionary => 2,
        };
        match self.tier(key).unwrap_or_default() {
            Tier::Covered => (class, ranks.get(key).copied().unwrap_or(u32::MAX), *key),
            Tier::BelowFloor => (3, u32::from(class), *key),
            Tier::NoPositions => (4, 0, *key),
        }
    }

    /// Admits a charge of `ĉ_m` on `key` (module docs); a refusal books nothing and raises
    /// [`Alert::BudgetExhausted`] at the day's first; an overdraft raises [`Alert::ExitOverdraft`]
    /// at the day's first.
    pub fn admit(
        &mut self,
        now: i64,
        key: Pubkey,
        req: ChargeReq,
        p: &BudgetParams,
        alerts: &mut Vec<Alert>,
    ) -> Admission {
        self.roll(now, p);
        self.tail_cap = p.tail_cap();
        let day = self.day.unwrap_or_default();
        let est = self.estimate(p);
        let (c, tier) = {
            let m = self.markets.entry(key).or_default();
            (m.charge_or(est), m.tier)
        };
        let mut res = Reservation {
            market: key,
            req,
            amount: c,
            parts: Vec::new(),
            tier,
            day,
        };
        if p.global == 0 {
            return Admission::Admitted(res);
        }
        let mut need = c;
        match tier {
            Tier::Covered => {
                if let Some(m) = self.markets.get_mut(&key) {
                    if req.class != ChargeClass::Discretionary {
                        let r = need.min(m.restricted);
                        m.restricted -= r;
                        m.unspent -= r;
                        need -= r;
                        push(&mut res.parts, Source::OwnRestricted, r);
                    }
                    let t = need.min(m.general());
                    m.unspent -= t;
                    need -= t;
                    push(&mut res.parts, Source::Own, t);
                }
                let t = need.min(self.free);
                self.free -= t;
                need -= t;
                push(&mut res.parts, Source::Free, t);
                if need > 0 && req.class != ChargeClass::Discretionary {
                    let ranked = self.ranked();
                    let below = ranked
                        .iter()
                        .position(|k| *k == key)
                        .map_or(&ranked[..0], |i| &ranked[i + 1..]);
                    for k in below.iter().rev() {
                        if need == 0 {
                            break;
                        }
                        if let Some(m) = self.markets.get_mut(k) {
                            let t = need.min(m.unspent);
                            let r = m.give_up(t);
                            need -= t;
                            push(
                                &mut res.parts,
                                Source::Lower {
                                    key: *k,
                                    restricted: true,
                                },
                                r,
                            );
                            push(
                                &mut res.parts,
                                Source::Lower {
                                    key: *k,
                                    restricted: false,
                                },
                                t - r,
                            );
                        }
                    }
                }
            }
            Tier::BelowFloor => {
                if self.tail.saturating_add(c) <= p.tail_cap() && self.free >= c {
                    self.free -= c;
                    self.tail += c;
                    need = 0;
                    push(&mut res.parts, Source::Tail, c);
                }
            }
            Tier::NoPositions => {
                let t = need.min(self.free);
                self.free -= t;
                need -= t;
                push(&mut res.parts, Source::Free, t);
            }
        }
        if need > 0 {
            if overdraws(req, tier) {
                self.overdraft = self.overdraft.saturating_add(need);
                push(&mut res.parts, Source::Overdraft, need);
                if self.overdraft_day != Some(day) {
                    self.overdraft_day = Some(day);
                    alerts.push(Alert::ExitOverdraft {
                        market: key,
                        overdraft: self.overdraft,
                        budget: p.global,
                    });
                }
            } else {
                let parts = std::mem::take(&mut res.parts);
                self.give_back(key, &parts, u64::MAX);
                let rank = self.rank(&key);
                if let Some(m) = self.markets.get_mut(&key) {
                    m.refused = m.refused.saturating_add(1);
                }
                if self.exhausted_day != Some(day) {
                    self.exhausted_day = Some(day);
                    alerts.push(Alert::BudgetExhausted {
                        market: key,
                        spent: self.spent,
                        budget: p.global,
                    });
                }
                return Admission::Refused { rank };
            }
        }
        Admission::Admitted(res)
    }

    /// Books an admitted charge at `actual` lamports (the crank landed; module docs) and makes
    /// it the market's `ĉ_m` (and part of the keeper's measured average); a covered market's first
    /// measured crank re-estimates its entitlement (M7D-5).
    pub fn settle(&mut self, res: Reservation, actual: u64) {
        self.avg_charge = Some(match self.avg_charge {
            None => actual,
            Some(a) => (a.saturating_mul(7).saturating_add(actual)) / 8,
        });
        let m = self.markets.entry(res.market).or_default();
        m.last_charge = Some(actual);
        let i = res.req.class.index();
        m.spent[i] = m.spent[i].saturating_add(actual);
        self.spent = self.spent.saturating_add(actual);
        if self.day != Some(res.day) || self.global == 0 {
            return;
        }
        let key = res.market;
        if actual <= res.amount {
            self.give_back(key, &res.parts, res.amount - actual);
        } else {
            self.draw_extra(&res, actual - res.amount);
        }
        self.re_estimate(key, actual);
    }

    /// A settlement above its estimate: the extra from the market's own `U` (a discretionary
    /// charge its unrestricted part only), then `F` (tier 2 within the tail cap), then `O` for an
    /// overdrawing charge, else `excess`.
    fn draw_extra(&mut self, res: &Reservation, mut extra: u64) {
        if res.tier == Tier::Covered
            && let Some(m) = self.markets.get_mut(&res.market)
        {
            if res.req.class != ChargeClass::Discretionary {
                let r = extra.min(m.restricted);
                m.restricted -= r;
                m.unspent -= r;
                extra -= r;
            }
            let t = extra.min(m.general());
            m.unspent -= t;
            extra -= t;
        }
        // A tier-2 market's extra stays within the tail cap: `T` never exceeds it.
        let room = if res.tier == Tier::BelowFloor {
            self.tail_cap.saturating_sub(self.tail)
        } else {
            u64::MAX
        };
        let t = extra.min(self.free).min(room);
        self.free -= t;
        extra -= t;
        if res.tier == Tier::BelowFloor {
            self.tail = self.tail.saturating_add(t);
        }
        if extra > 0 {
            if overdraws(res.req, res.tier) {
                self.overdraft = self.overdraft.saturating_add(extra);
            } else {
                self.excess = self.excess.saturating_add(extra);
            }
        }
    }

    /// Returns an admitted charge whose crank did not land: nothing is booked.
    pub fn release(&mut self, res: Reservation) {
        if self.day == Some(res.day) {
            self.give_back(res.market, &res.parts, u64::MAX);
        }
    }

    /// Returns up to `amount` of `parts` to their sources, the last-drawn first.
    fn give_back(&mut self, market: Pubkey, parts: &[(Source, u64)], mut amount: u64) {
        for (src, a) in parts.iter().rev() {
            if amount == 0 {
                break;
            }
            let t = amount.min(*a);
            amount -= t;
            match *src {
                Source::Own => self.credit(&market, t, false),
                Source::OwnRestricted => self.credit(&market, t, true),
                Source::Lower { key, restricted } => {
                    if let Some(m) = self.markets.get_mut(&key)
                        && m.tier == Tier::Covered
                    {
                        m.funded = m.funded.saturating_add(t);
                        m.taken = m.taken.saturating_sub(t);
                    }
                    self.credit(&key, t, restricted);
                }
                Source::Free => self.free = self.free.saturating_add(t),
                Source::Tail => {
                    self.free = self.free.saturating_add(t);
                    self.tail = self.tail.saturating_sub(t);
                }
                Source::Overdraft => self.overdraft = self.overdraft.saturating_sub(t),
            }
        }
    }

    /// Credits `t` to a covered market's `U` (`restricted`: to its restricted part); a market no
    /// longer covered returns it as on a leave (restricted lamports to the markets taken from, the
    /// rest to `F`).
    fn credit(&mut self, key: &Pubkey, t: u64, restricted: bool) {
        match self.markets.get_mut(key) {
            Some(m) if m.tier == Tier::Covered => {
                m.unspent = m.unspent.saturating_add(t);
                if restricted {
                    m.restricted = m.restricted.saturating_add(t);
                }
            }
            _ if restricted => self.return_restricted(key, t),
            _ => self.free = self.free.saturating_add(t),
        }
    }

    /// The sum of the covered markets' `U`.
    pub fn unspent_total(&self) -> u64 {
        self.markets
            .values()
            .fold(0u64, |a, m| a.saturating_add(m.unspent))
    }
}

fn push(parts: &mut Vec<(Source, u64)>, src: Source, t: u64) {
    if t > 0 {
        parts.push((src, t));
    }
}

/// Step (4): an exit-critical charge (on any market with positions), and a warm-up or
/// listing-window charge of a market without positions, overdraw rather than being refused.
fn overdraws(req: ChargeReq, tier: Tier) -> bool {
    req.class == ChargeClass::ExitCritical || (tier == Tier::NoPositions && req.warm_up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    const G: u64 = 1_000_000_000;

    fn params() -> BudgetParams {
        BudgetParams {
            global: G,
            var_floor: 1_000_000_000,
            reserve_margin_bps: 10_000,
            tail_share_bps: 2_000,
            idle_interval_s: 30,
            first_charge: FIRST_CHARGE_LAMPORTS,
        }
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

    fn key(i: u8) -> Pubkey {
        Pubkey::new_from_array([i; 32])
    }

    /// `U` of `k` in `b` (0 if unknown).
    fn u_of(b: &GlobalBudget, k: &Pubkey) -> u64 {
        b.markets.get(k).map_or(0, |m| m.unspent)
    }

    /// Conservation: `G` is in `F`, in a covered market's `U`, booked (the day's spend less what
    /// went to `O` or `excess`) or `reserved` by an admitted charge not yet settled (all of it:
    /// what it overdrew is in `O` until then).
    fn conserved(b: &GlobalBudget, reserved: u64) -> bool {
        let booked = i128::from(b.spent) - i128::from(b.overdraft) - i128::from(b.excess);
        i128::from(b.free) + i128::from(b.unspent_total()) + booked + i128::from(reserved)
            == i128::from(b.global)
    }

    #[test]
    fn value_at_risk_is_the_borrowed_inventory_at_the_ema() {
        let mut m = Market::zeroed();
        m.borrowed_q = 3_000_000_000_000;
        m.ema_price.set(WAD / 1_000); // 0.001 lamport per raw token
        assert_eq!(value_at_risk(&m), 3_000_000_000);
        m.ema_price.set(u128::MAX);
        m.borrowed_q = u64::MAX;
        // Clamped at P_ACC_MAX, then saturating at u64::MAX.
        assert_eq!(value_at_risk(&m), u64::MAX);
        m.borrowed_q = 1;
        assert_eq!(value_at_risk(&m), u64::try_from(P_ACC_MAX / WAD).unwrap());
    }

    #[test]
    fn the_entitlement_is_twice_a_quiet_day_by_default() {
        let p = params();
        assert_eq!(p.entitlement(5_000), 2 * 2_880 * 5_000);
        assert_eq!(p.entitlement(1), 5_760);
        let p0 = BudgetParams {
            reserve_margin_bps: 0,
            idle_interval_s: 7,
            ..p
        };
        // ⌈86,400 / 7⌉ = 12,343 cranks.
        assert_eq!(p0.entitlement(3), 3 * 12_343);
        assert_eq!(BudgetParams::prorated(86_400, 43_200), 43_200);
        assert_eq!(BudgetParams::prorated(3, 86_399), 1);
        assert_eq!(p.tail_cap(), 200_000_000);
    }

    /// The classes, the ranking and its tie-breaks.
    #[test]
    fn classes_and_ranking() {
        let p = params();
        let mut b = GlobalBudget::default();
        let now = 10 * DAY_S;
        assert_eq!(
            b.classify(now, key(1), false, 9 * G, 0, &p),
            Tier::NoPositions
        );
        assert_eq!(
            b.classify(now, key(2), true, G - 1, 0, &p),
            Tier::BelowFloor
        );
        assert_eq!(b.classify(now, key(3), true, G, 5, &p), Tier::Covered);
        assert_eq!(b.classify(now, key(4), true, G, 4, &p), Tier::Covered);
        assert_eq!(b.classify(now, key(6), true, 2 * G, 9, &p), Tier::Covered);
        assert_eq!(b.classify(now, key(5), true, G, 4, &p), Tier::Covered);
        // V descending; equal V: the older; equal age: the smaller key.
        assert_eq!(b.ranked(), vec![key(6), key(4), key(5), key(3)]);
        assert_eq!(b.rank(&key(5)), Some(3));
        assert_eq!(b.rank(&key(2)), None);
    }

    /// The presentation order within a tick: exit-critical, baseline, discretionary (each by
    /// rank), then tier 2, then tier 3.
    #[test]
    fn charges_are_presented_by_class_then_rank() {
        let p = params();
        let mut b = GlobalBudget::default();
        let now = 11 * DAY_S;
        b.classify(now, key(1), true, 2 * G, 0, &p);
        b.classify(now, key(2), true, 3 * G, 0, &p);
        b.classify(now, key(3), true, G / 2, 0, &p);
        b.classify(now, key(4), false, 0, 0, &p);
        let ranks = b.ranks();
        let mut q = [
            (key(4), ChargeReq::of(ChargeClass::Baseline)),
            (key(3), ChargeReq::of(ChargeClass::Baseline)),
            (key(1), ChargeReq::of(ChargeClass::Discretionary)),
            (key(2), ChargeReq::of(ChargeClass::Baseline)),
            (key(1), ChargeReq::of(ChargeClass::Baseline)),
            (key(2), ChargeReq::of(ChargeClass::ExitCritical)),
        ];
        q.sort_by_key(|(k, r)| b.order(k, *r, &ranks));
        let got: Vec<(u8, ChargeClass)> =
            q.iter().map(|(k, r)| (k.to_bytes()[0], r.class)).collect();
        assert_eq!(
            got,
            vec![
                (2, ChargeClass::ExitCritical),
                (2, ChargeClass::Baseline),
                (1, ChargeClass::Baseline),
                (1, ChargeClass::Discretionary),
                (3, ChargeClass::Baseline),
                (4, ChargeClass::Baseline),
            ]
        );
    }

    /// A day's roll funds the covered markets' entitlements in rank order: the top ones in full,
    /// at most one partly, the rest nothing; `F` keeps what is left; `T`, `O` and the spend reset.
    #[test]
    fn entitlements_are_funded_in_rank_order_at_00_00_utc() {
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let p = BudgetParams {
                global: 1 + rng.below(400_000_000),
                ..params()
            };
            let mut b = GlobalBudget::default();
            let day = 20_000 + i64::try_from(rng.below(100)).unwrap();
            let t = day * DAY_S + i64::try_from(rng.below(86_400)).unwrap();
            let n = 1 + rng.below(30);
            for i in 0..n {
                let k = key(u8::try_from(i).unwrap());
                b.classify(t, k, true, G + rng.below(50 * G), 0, &p);
                if let Some(m) = b.markets.get_mut(&k) {
                    m.last_charge = Some(500 + rng.below(20_000));
                }
            }
            // Spend some, overdraw some; then the next day.
            b.overdraft = 7;
            b.tail = 9;
            b.spent = 11;
            b.roll((day + 1) * DAY_S, &p);
            assert_eq!((b.tail, b.overdraft, b.spent), (0, 0, 0), "seed {seed}");
            let mut left = p.global;
            let mut partial_seen = false;
            for k in b.ranked() {
                let m = &b.markets[&k];
                let r = p.entitlement(m.charge());
                let want = r.min(left);
                assert_eq!(m.unspent, want, "seed {seed}");
                if want < r {
                    assert!(
                        !partial_seen || want == 0,
                        "seed {seed}: one partial at most"
                    );
                    partial_seen = true;
                }
                left -= want;
            }
            assert_eq!(b.free, left, "seed {seed}");
            assert!(conserved(&b, 0), "seed {seed}");
        }
    }

    /// A market covered mid-day gets its prorated entitlement from `F`, then from the
    /// lower-ranked markets' `U`, lowest-ranked first — never from a higher-ranked one; one that
    /// stops being covered returns its `U` to `F` (and its restricted part to the markets it came
    /// from), and the markets still short are topped up from it; a rank change moves no balance.
    #[test]
    fn a_late_joiner_preempts_the_lowest_ranked_first_and_rank_changes_move_nothing() {
        let p = BudgetParams {
            global: 3 * 2 * 2_880 * 5_000 / 2,
            ..params()
        };
        let r = p.entitlement(5_000);
        let mut b = GlobalBudget::default();
        let day = 30_000 * DAY_S;
        // Three markets at 00:00: ranks a > c > d; funded r, r/2, 0.
        b.roll(day, &p);
        b.classify(day, key(1), true, 30 * G, 0, &p);
        b.classify(day, key(3), true, 20 * G, 0, &p);
        b.classify(day, key(4), true, 10 * G, 0, &p);
        assert_eq!([1, 3, 4].map(|i| b.markets[&key(i)].unspent), [r, r / 2, 0]);
        assert_eq!(b.free, 0);
        // At noon `b` joins between a and c: half of r, taken from d (nothing), then c.
        let noon = day + DAY_S / 2;
        b.classify(noon, key(2), true, 25 * G, 0, &p);
        assert_eq!(b.ranked(), vec![key(1), key(2), key(3), key(4)]);
        assert_eq!(b.markets[&key(2)].unspent, r / 2);
        assert_eq!(b.markets[&key(3)].unspent, 0);
        assert_eq!(
            b.markets[&key(1)].unspent,
            r,
            "a higher rank is never taken"
        );
        // A rank change moves nothing.
        let before = b.clone();
        b.classify(noon + 5, key(4), true, 99 * G, 0, &p);
        assert_eq!(b.ranked()[0], key(4));
        assert_eq!(b.free, before.free);
        for i in [1, 2, 3, 4] {
            assert_eq!(b.markets[&key(i)].unspent, before.markets[&key(i)].unspent);
        }
        // What b took from c is restricted (M7D-3): exit-critical and baseline charges only.
        assert_eq!(b.markets[&key(2)].restricted, r / 2);
        assert_eq!(b.markets[&key(3)].taken, r / 2);
        // a's positions close: its U returns to F, and the covered markets still short of their
        // day's entitlement are topped up from it in rank order (M7D-5) — up to what the rest of
        // the day needs: d (now rank 1, funded nothing at 00:00), then c (half of r).
        b.classify(noon + 6, key(1), false, 0, 0, &p);
        assert_eq!(b.markets[&key(1)].unspent, 0);
        let half = BudgetParams::prorated(r, DAY_S / 2 + 6);
        assert_eq!(b.markets[&key(4)].unspent, half);
        assert_eq!(b.markets[&key(3)].unspent, half);
        assert_eq!(b.free, r - 2 * half);
        assert!(conserved(&b, 0));
    }

    /// M7D-3: a market is funded at most once a UTC day, and what its join took from lower-ranked
    /// markets pays only exit-critical and baseline charges and returns to them when it leaves.
    /// ARCH §7.1 duty 8 "What it guarantees": a covered market keeps its entitlement against every
    /// other market's bursts; only higher-ranked markets' exit-critical and baseline spend can take
    /// it. As reviewed, a higher-ranked junk market that left (V below the floor) and re-joined was
    /// funded afresh at each join — from F, then from the lowest-ranked markets' U — and spent each
    /// entitlement on discretionary charges (bursts): three cycles took 2 R from B and C.
    #[test]
    fn m7d_3_rejoining_launders_lower_entitlements_into_bursts() {
        let r = params().entitlement(5_000); // 28,800,000: one quiet day twice at 5,000/crank
        let p = BudgetParams {
            global: 3 * r,
            ..params()
        };
        let (a, b_, c, j) = (key(1), key(2), key(3), key(9));
        let mut b = GlobalBudget::default();
        let day = 20 * DAY_S;
        let mut alerts = Vec::new();
        b.classify(day, a, true, 10 * G, 0, &p);
        b.classify(day, b_, true, 2 * G, 0, &p);
        b.classify(day, c, true, 2 * G, 1, &p);
        assert_eq!(b.free, 0, "three entitlements fund G");
        // The junk market (V 5 G: below A, above B and C) joins, bursts, dips below the floor,
        // re-joins: three cycles of 60 s.
        let mut burst = 0u64;
        let mut joined = Vec::new();
        for cycle in 0..3i64 {
            let t = day + 60 + 60 * cycle;
            b.classify(t, j, true, 5 * G, 9, &p);
            joined.push((b.markets[&j].unspent, b.markets[&j].restricted));
            let disc = ChargeReq::of(ChargeClass::Discretionary);
            while let Admission::Admitted(res) = b.admit(t, j, disc, &p, &mut alerts) {
                b.settle(res, 5_000);
                burst += 5_000;
            }
            b.classify(t + 30, j, true, G - 1, 9, &p);
            assert!(conserved(&b, 0));
        }
        let jm = &b.markets[&j];
        println!(
            "M7D-3: J's U at each join (U, restricted) {joined:?}; its discretionary spend {burst} (R = \
             {r}); U of B {} and C {}; F {}",
            b.markets[&b_].unspent, b.markets[&c].unspent, b.free
        );
        assert_eq!(jm.spent[2], burst);
        // Its first join: its prorated entitlement, all of it from C (the lowest), restricted.
        let first = BudgetParams::prorated(r, 60);
        assert_eq!(joined[0], (first, first));
        // Its re-joins: nothing more (funded once a day).
        assert_eq!(&joined[1..], &[(0, 0), (0, 0)]);
        assert_eq!(
            burst, 0,
            "no burst from what it took from lower-ranked markets"
        );
        // C got back what J took (J's leave), B and C hold their entitlements.
        assert_eq!(b.markets[&c].unspent, r);
        assert_eq!(b.markets[&b_].unspent, r);
        assert_eq!(b.markets[&c].taken, 0);
        // B and C made no charge; A (the only higher-ranked market) none either.
        let ac = b.admit(
            day + 400,
            c,
            ChargeReq::of(ChargeClass::Baseline),
            &p,
            &mut alerts,
        );
        let Admission::Admitted(ac) = ac else {
            panic!("M7D-3: C's baseline refused after J's bursts spent {burst} lamports")
        };
        b.settle(ac, 5_000);
        // While covered, J's restricted lamports pay its baseline and exit-critical charges.
        b.classify(day + 500, j, true, 5 * G, 9, &p);
        let Admission::Admitted(res) = b.admit(
            day + 500,
            j,
            ChargeReq::of(ChargeClass::Baseline),
            &p,
            &mut alerts,
        ) else {
            panic!("J's baseline: lower-ranked markets' U (priority)")
        };
        b.settle(res, 5_000);
        assert!(conserved(&b, 0));
    }

    /// M7D-3: a junk market's join with a free pool: what came from `F` is its own (bursts may
    /// spend it), what came from lower-ranked markets is restricted; restricted lamports a
    /// cheaper-than-estimate settlement gives back stay restricted.
    #[test]
    fn a_joins_preempted_part_never_pays_a_burst() {
        let r = params().entitlement(5_000);
        let p = BudgetParams {
            global: 2 * r + 50_000,
            ..params()
        };
        let mut b = GlobalBudget::default();
        let day = 21 * DAY_S;
        let mut alerts = Vec::new();
        b.classify(day, key(1), true, 3 * G, 0, &p);
        b.classify(day, key(2), true, 2 * G, 0, &p);
        assert_eq!(b.free, 50_000);
        b.classify(day + 10, key(9), true, 5 * G, 0, &p);
        let jm = &b.markets[&key(9)];
        let want = BudgetParams::prorated(r, 10);
        assert_eq!((jm.unspent, jm.restricted), (want, want - 50_000));
        assert_eq!(
            b.markets[&key(2)].taken,
            want - 50_000,
            "the lowest-ranked first"
        );
        assert_eq!(b.markets[&key(1)].taken, 0);
        // Ten bursts from its own 50,000; the eleventh refused (F is 0, and never another's U).
        let disc = ChargeReq::of(ChargeClass::Discretionary);
        for _ in 0..10 {
            let Admission::Admitted(res) = b.admit(day + 11, key(9), disc, &p, &mut alerts) else {
                panic!()
            };
            b.settle(res, 5_000);
        }
        assert!(matches!(
            b.admit(day + 11, key(9), disc, &p, &mut alerts),
            Admission::Refused { .. }
        ));
        // A baseline charge draws the restricted part; a cheaper landing gives it back restricted.
        let Admission::Admitted(res) = b.admit(
            day + 12,
            key(9),
            ChargeReq::of(ChargeClass::Baseline),
            &p,
            &mut alerts,
        ) else {
            panic!()
        };
        b.settle(res, 1_000);
        let jm = &b.markets[&key(9)];
        assert_eq!(jm.restricted, want - 50_000 - 1_000);
        assert_eq!(jm.general(), 0);
        assert!(matches!(
            b.admit(day + 13, key(9), disc, &p, &mut alerts),
            Admission::Refused { .. }
        ));
        assert!(conserved(&b, 0));
    }

    /// M7D-5: ARCH §7.1 duty 8 / ECON §5.8: "1 SOL funds 208 covered markets' entitlements at
    /// 0 µlamports/CU batched". As reviewed, a keeper (re)started at 12:00 UTC funded each market
    /// at the unbatched first-crank estimate (5,000 lamports), never revisited it when the
    /// market's batched charge (≈ 833 at k = 6) was known, and so refused — degraded, opens paused
    /// until 00:00 UTC — every covered market below rank ≈ 70 of 100. Now the first measured crank
    /// re-estimates the entitlement, releases the excess and tops up the markets still short, in
    /// rank order.
    #[test]
    fn m7d_5_a_restart_funds_at_the_unbatched_estimate_until_midnight() {
        let p = params();
        let mut b = GlobalBudget::default();
        let noon = 30 * DAY_S + DAY_S / 2;
        let mut alerts = Vec::new();
        let keys: Vec<Pubkey> = (0..100u8)
            .map(|i| Pubkey::new_from_array([i.wrapping_add(10); 32]))
            .collect();
        for (i, k) in keys.iter().enumerate() {
            let var = (200 - i as u64) * G;
            b.classify(noon, *k, true, var, 0, &p);
        }
        // Every market's first idle crank lands batched (k = 6): 833 lamports.
        let mut refused = Vec::new();
        for t in 0..4i64 {
            let now = noon + 30 * t;
            for (i, k) in keys.iter().enumerate() {
                match b.admit(
                    now,
                    *k,
                    ChargeReq::of(ChargeClass::Baseline),
                    &p,
                    &mut alerts,
                ) {
                    Admission::Admitted(res) => b.settle(res, 833),
                    Admission::Refused { .. } => {
                        if !refused.contains(&(i + 1)) {
                            refused.push(i + 1);
                        }
                    }
                }
            }
            assert!(conserved(&b, 0));
        }
        let funded = keys.iter().filter(|k| b.markets[*k].funded > 0).count();
        let unspent: u64 = b.unspent_total();
        let half = BudgetParams::prorated(p.entitlement(833), DAY_S / 2);
        println!(
            "M7D-5: after a noon restart {funded} of 100 covered markets funded; refused ranks {:?}..{:?} ({} markets); F {}; unspent U {unspent} (the 100 markets' batched half-day needs {})",
            refused.first(),
            refused.last(),
            refused.len(),
            b.free,
            100 * half
        );
        assert!(
            refused.is_empty(),
            "M7D-5: {} covered markets refused after a restart at noon",
            refused.len()
        );
        assert_eq!(funded, 100);
        // Each holds its batched half-day entitlement less its own four cranks; the rest is free.
        for k in &keys {
            let m = &b.markets[k];
            assert!(!m.estimated);
            assert_eq!(m.unspent + m.spent_today(), m.funded);
            assert!(m.funded >= half - 833 && m.funded <= half, "{}", m.funded);
        }
        assert!(b.free >= p.global - 100 * half);
    }

    /// M7D-5, the keeper's tick: every market of a tick is admitted before any crank of it lands.
    /// At the keeper's first-crank estimate `⌈5,000 / batch⌉` (834 at `batch_v1` = 6) a noon
    /// restart funds all 100 markets at once; at the unbatched 5,000 the first tick refuses ranks
    /// 71–100 (their entitlement not funded yet), and the landed batch's re-estimate funds them
    /// all for the next.
    #[test]
    fn a_restart_at_the_batched_estimate_funds_every_market_in_its_first_tick() {
        let noon = 31 * DAY_S + DAY_S / 2;
        let keys: Vec<Pubkey> = (0..100u8)
            .map(|i| Pubkey::new_from_array([i.wrapping_add(10); 32]))
            .collect();
        let tick = |b: &mut GlobalBudget, now: i64, p: &BudgetParams| {
            let mut alerts = Vec::new();
            let mut held = Vec::new();
            let mut refused = 0;
            for k in &keys {
                match b.admit(
                    now,
                    *k,
                    ChargeReq::of(ChargeClass::Baseline),
                    p,
                    &mut alerts,
                ) {
                    Admission::Admitted(r) => held.push(r),
                    Admission::Refused { .. } => refused += 1,
                }
            }
            for r in held {
                b.settle(r, 833);
            }
            refused
        };
        for (first, want) in [(834, [0, 0]), (FIRST_CHARGE_LAMPORTS, [30, 0])] {
            let p = BudgetParams {
                first_charge: first,
                ..params()
            };
            let mut b = GlobalBudget::default();
            for (i, k) in keys.iter().enumerate() {
                b.classify(noon, *k, true, (200 - i as u64) * G, 0, &p);
            }
            let got = [tick(&mut b, noon, &p), tick(&mut b, noon + 30, &p)];
            assert_eq!(got, want, "first_charge {first}");
            assert!(conserved(&b, 0));
        }
    }

    /// Admission: own `U`, then `F`, then — never for a discretionary charge — the lower-ranked
    /// markets' `U`, lowest first; the overdraft for exit-critical charges (and a warm-up without
    /// positions) only; a refusal books nothing; the alerts once a day.
    #[test]
    fn admission_order_preemption_overdraft_and_refusal() {
        let p = BudgetParams {
            global: 30_000,
            reserve_margin_bps: 0,
            ..params()
        };
        let mut b = GlobalBudget::default();
        let day = 40_000 * DAY_S;
        let mut alerts = Vec::new();
        b.roll(day, &p);
        for (i, v) in [(1u8, 3 * G), (2, 2 * G), (3, G)] {
            b.classify(day, key(i), true, v, 0, &p);
        }
        // Funded: 1 takes everything (R = 14.4 M ≫ G).
        assert_eq!(b.markets[&key(1)].unspent, 30_000);
        // Give 2 and 3 some U to preempt.
        b.markets.get_mut(&key(1)).unwrap().unspent = 10_000;
        b.markets.get_mut(&key(2)).unwrap().unspent = 10_000;
        b.markets.get_mut(&key(3)).unwrap().unspent = 10_000;
        let base = ChargeReq::of(ChargeClass::Baseline);
        let disc = ChargeReq::of(ChargeClass::Discretionary);
        let exit = ChargeReq::of(ChargeClass::ExitCritical);
        // Two of 1's charges from its own U.
        for _ in 0..2 {
            let Admission::Admitted(r) = b.admit(day, key(1), base, &p, &mut alerts) else {
                panic!()
            };
            b.settle(r, 5_000);
        }
        assert_eq!(b.markets[&key(1)].unspent, 0);
        // A discretionary charge of 1 with U and F spent: refused, nothing moved.
        let snap = b.clone();
        assert_eq!(
            b.admit(day, key(1), disc, &p, &mut alerts),
            Admission::Refused { rank: Some(1) }
        );
        assert_eq!(b.markets[&key(2)].unspent, snap.markets[&key(2)].unspent);
        assert_eq!(b.markets[&key(3)].unspent, snap.markets[&key(3)].unspent);
        assert!(matches!(alerts.as_slice(), [Alert::BudgetExhausted { .. }]));
        // A baseline charge of 1: taken from the lowest-ranked (3) first.
        let Admission::Admitted(r) = b.admit(day, key(1), base, &p, &mut alerts) else {
            panic!()
        };
        b.settle(r, 5_000);
        assert_eq!(
            (b.markets[&key(2)].unspent, b.markets[&key(3)].unspent),
            (10_000, 5_000)
        );
        // 3 (the lowest) can take nothing from above it: its baseline after its own U refused.
        b.markets.get_mut(&key(3)).unwrap().unspent = 0;
        assert_eq!(
            b.admit(day, key(3), base, &p, &mut alerts),
            Admission::Refused { rank: Some(3) }
        );
        // An exit-critical charge of 3: overdraft.
        let Admission::Admitted(r) = b.admit(day, key(3), exit, &p, &mut alerts) else {
            panic!()
        };
        b.settle(r, 5_000);
        assert_eq!(b.overdraft, 5_000);
        assert!(
            alerts
                .iter()
                .filter(|a| matches!(a, Alert::BudgetExhausted { .. }))
                .count()
                == 1
        );
        assert!(matches!(alerts.last(), Some(Alert::ExitOverdraft { .. })));
        // A warm-up without positions overdraws; another charge without positions is refused.
        b.classify(day, key(9), false, 0, 0, &p);
        let warm = ChargeReq {
            class: ChargeClass::Baseline,
            warm_up: true,
        };
        assert!(matches!(
            b.admit(day, key(9), warm, &p, &mut alerts),
            Admission::Admitted(_)
        ));
        assert!(matches!(
            b.admit(day, key(9), base, &p, &mut alerts),
            Admission::Refused { rank: None }
        ));
    }

    /// The tail: below-floor markets draw from `F` only while `T + c ≤ ⌊G·tail_share⌋`, never from
    /// a covered market's `U`, and are never overdrawn.
    #[test]
    fn the_tail_cap_holds_whatever_the_junk() {
        let p = BudgetParams {
            global: 100_000,
            reserve_margin_bps: 0,
            ..params()
        };
        let mut b = GlobalBudget::default();
        let day = 41_000 * DAY_S;
        let mut alerts = Vec::new();
        b.roll(day, &p);
        // One real market covered (R = 14.4 M: all of G), then F for the tail by hand.
        b.classify(day, key(200), true, 50 * G, 0, &p);
        b.markets.get_mut(&key(200)).unwrap().unspent = 40_000;
        b.free = 60_000;
        let real = b.markets[&key(200)].unspent;
        let mut admitted = 0;
        for i in 0..100u8 {
            b.classify(day, key(i), true, G / 2, 0, &p);
            if let Admission::Admitted(r) = b.admit(
                day,
                key(i),
                ChargeReq::of(ChargeClass::Baseline),
                &p,
                &mut alerts,
            ) {
                b.settle(r, 5_000);
                admitted += 1;
            }
            assert!(b.tail <= p.tail_cap());
        }
        assert_eq!(admitted, 4, "⌊100,000 · 20 %⌋ / 5,000");
        assert_eq!(b.tail, 20_000);
        assert_eq!(b.markets[&key(200)].unspent, real);
        assert_eq!(b.overdraft, 0);
    }

    /// A `G` raised or lowered during the day moves the difference into or out of `F`; `G = 0`
    /// admits everything.
    #[test]
    fn a_changed_budget_moves_the_free_pool() {
        let mut p = BudgetParams {
            global: 10_000,
            ..params()
        };
        let mut b = GlobalBudget::default();
        let day = 43_000 * DAY_S;
        let mut alerts = Vec::new();
        b.roll(day, &p);
        assert_eq!(b.free, 10_000);
        p.global = 30_000;
        assert!(!b.roll(day + 5, &p));
        assert_eq!((b.free, b.global), (30_000, 30_000));
        p.global = 5_000;
        b.roll(day + 6, &p);
        assert_eq!(b.free, 5_000);
        p.global = 0;
        b.roll(day + 7, &p);
        assert_eq!(b.free, 0);
        b.classify(day + 7, key(1), true, G / 10, 0, &p);
        assert!(matches!(
            b.admit(
                day + 7,
                key(1),
                ChargeReq::of(ChargeClass::Baseline),
                &p,
                &mut alerts
            ),
            Admission::Admitted(_)
        ));
    }

    /// Settlement: a cheaper landing returns the difference (the last-drawn source first); a
    /// dearer one draws own `U`, then `F`, then `O` (overdrawing charges) or `excess`; a release
    /// returns everything.
    #[test]
    fn settlement_and_release() {
        let p = BudgetParams {
            global: 1_000_000,
            reserve_margin_bps: 0,
            ..params()
        };
        let mut b = GlobalBudget::default();
        let day = 42_000 * DAY_S;
        let mut alerts = Vec::new();
        b.roll(day, &p);
        b.classify(day, key(1), true, 5 * G, 0, &p);
        b.markets.get_mut(&key(1)).unwrap().unspent = 3_000;
        b.free = 997_000;
        let Admission::Admitted(r) = b.admit(
            day,
            key(1),
            ChargeReq::of(ChargeClass::Baseline),
            &p,
            &mut alerts,
        ) else {
            panic!()
        };
        assert_eq!((b.markets[&key(1)].unspent, b.free), (0, 995_000));
        b.settle(r, 1_000);
        // 4,000 returned: 2,000 to F (drawn last), 2,000 to U.
        assert_eq!((b.markets[&key(1)].unspent, b.free), (2_000, 997_000));
        assert_eq!(b.markets[&key(1)].charge(), 1_000);
        let Admission::Admitted(r) = b.admit(
            day,
            key(1),
            ChargeReq::of(ChargeClass::Discretionary),
            &p,
            &mut alerts,
        ) else {
            panic!()
        };
        b.release(r);
        assert_eq!((b.markets[&key(1)].unspent, b.free), (2_000, 997_000));
        let Admission::Admitted(r) = b.admit(
            day,
            key(1),
            ChargeReq::of(ChargeClass::Baseline),
            &p,
            &mut alerts,
        ) else {
            panic!()
        };
        b.settle(r, 3_500);
        assert_eq!((b.markets[&key(1)].unspent, b.free), (0, 995_500));
        assert!(conserved(&b, 0));
    }

    /// The global properties under random charges, landings, releases, reclassifications and day
    /// rolls: conservation (`F + ΣU + booked = G`), `T` within the tail cap, every balance
    /// non-negative, `O` only from overdrawing charges, a discretionary charge never touching
    /// another market's `U`, a refusal moving nothing.
    #[test]
    fn property_random_days() {
        for seed in 1..=200u64 {
            let mut rng = Rng(seed.wrapping_mul(0xd1b5_4a32_d192_ed03) | 1);
            let p = BudgetParams {
                global: rng.below(5_000_000),
                var_floor: G,
                reserve_margin_bps: u32::try_from(rng.below(30_000)).unwrap(),
                tail_share_bps: u32::try_from(rng.below(10_001)).unwrap(),
                idle_interval_s: 30,
                first_charge: 1 + rng.below(9_000),
            };
            let mut b = GlobalBudget::default();
            let mut alerts = Vec::new();
            let mut now = 50_000 * DAY_S + i64::try_from(rng.below(86_400)).unwrap();
            let classes = [
                ChargeClass::ExitCritical,
                ChargeClass::Baseline,
                ChargeClass::Discretionary,
            ];
            for _ in 0..2_000 {
                now += i64::try_from(rng.below(120)).unwrap();
                let k = key(u8::try_from(rng.below(12)).unwrap());
                match rng.below(10) {
                    0..=2 => {
                        let open = rng.below(5) != 0;
                        let before = b.clone();
                        b.classify(now, k, open, rng.below(4 * G), 0, &p);
                        // M7D-3: funded at most its day's entitlement — a re-join adds nothing
                        // beyond it; a join or a top-up grants nobody more than that.
                        if before.day == b.day {
                            for (kk, m) in &b.markets {
                                let g0 = before.markets.get(kk).map_or(0, |m| m.granted);
                                assert!(m.granted <= g0.max(m.entitled), "seed {seed}");
                            }
                        }
                    }
                    _ => {
                        b.roll(now, &p);
                        let req = ChargeReq {
                            class: classes[usize::try_from(rng.below(3)).unwrap()],
                            warm_up: rng.below(2) == 0,
                        };
                        let before = b.clone();
                        let o0 = b.overdraft;
                        match b.admit(now, k, req, &p, &mut alerts) {
                            Admission::Refused { .. } => {
                                assert_eq!(b.free, before.free, "seed {seed}");
                                assert_eq!(b.tail, before.tail);
                                assert_eq!(b.overdraft, before.overdraft);
                                for (kk, m) in &b.markets {
                                    let m0 = before.markets.get(kk).cloned().unwrap_or_default();
                                    assert_eq!(
                                        (m.unspent, m.restricted, m.taken, m.funded),
                                        (m0.unspent, m0.restricted, m0.taken, m0.funded),
                                        "seed {seed}: a refusal moved {kk}'s balances"
                                    );
                                }
                            }
                            Admission::Admitted(r) => {
                                let tier = before.tier(&k).unwrap_or_default();
                                if b.overdraft > o0 {
                                    assert!(overdraws(req, tier), "seed {seed}");
                                }
                                if req.class == ChargeClass::Discretionary {
                                    for (kk, m) in &b.markets {
                                        if *kk != k {
                                            assert_eq!(
                                                m.unspent,
                                                u_of(&before, kk),
                                                "seed {seed}: a discretionary charge took {kk}'s U"
                                            );
                                        }
                                        // M7D-3: nor the restricted part of its own.
                                        assert_eq!(
                                            m.restricted,
                                            before.markets.get(kk).map_or(0, |m| m.restricted),
                                            "seed {seed}: a discretionary charge spent restricted U"
                                        );
                                    }
                                }
                                // Admitted: every lamport of the estimate drawn somewhere.
                                let drawn: u64 = r.parts.iter().map(|(_, a)| *a).sum();
                                assert!(p.global == 0 || drawn == r.amount, "seed {seed}");
                                assert!(p.global == 0 || conserved(&b, r.amount), "seed {seed}");
                                if rng.below(4) == 0 {
                                    b.release(r);
                                } else {
                                    let actual = r.amount * (50 + rng.below(100)) / 100;
                                    b.settle(r, actual);
                                }
                            }
                        }
                    }
                }
                assert!(b.tail <= p.tail_cap(), "seed {seed}");
                assert!(p.global == 0 || conserved(&b, 0), "seed {seed}");
                for m in b.markets.values() {
                    assert!(m.restricted <= m.unspent, "seed {seed}");
                    assert!(m.tier == Tier::Covered || m.unspent == 0, "seed {seed}");
                }
            }
        }
    }
}
