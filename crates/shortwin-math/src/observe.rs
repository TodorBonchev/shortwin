//! Owner: oracle track. Observation buckets, EMA integration and the history ring (ECON
//! §5.1–§5.4; ARCH §3.0 prologue steps 5 and 7, §4.4). Normative reference:
//! `Market.observe`, `_integrate`, `ring_medians` and `_rewrite_ring_liq` of
//! `sim/shortwin_sim.py` v0.4.1, which this module reproduces to the unit.
//!
//! Prices `P` are WAD-scaled (lamports per raw token unit × 10¹⁸) and liquidities
//! `L = √(X·Y)`; both are `u128`. An observation must be non-zero: a zero price has no
//! reprojection (`reprice` is undefined at `P = 0`) and would pin the EMA at 0 forever,
//! so the caller treats a zero spot as "price undefined" and records nothing.
//!
//! Every state struct is `#[repr(C)]`, fixed-size, all-integer and free of implicit
//! padding whether `u128` is 8- or 16-byte aligned, so the layout is identical on SBF,
//! x86-64 and wasm. These are math types, not account layouts: the program converts them
//! to and from its `PodU128` zero-copy fields (ARCH §2.2, §2.3). Emptiness is encoded by a
//! zero low (`lo == 0`), never by a timestamp, so second 0 is a valid second.
//!
//! **Post-change confirmation** (M4R, **PROPOSED**, owner item O18; ECON §5.2, ARCH §4.4): a
//! venue change or a price start reads no sample of the new pool that one transaction could
//! wrap. It writes an anchor ([`rewrite_liquidity`] with the pool's `⌊D·10⁴/λ_min⌋`, or
//! [`Observation::new_raydium`]) and marks the state unconfirmed ([`mark_restart`]: `Ema.warm =
//! EMA_WARMUP_BUCKETS`, `Ema.guard = 1`); the next [`EMA_WARMUP_BUCKETS`] integrations re-seed
//! the EMA from the post-change buckets only, on the side that cannot force an exit
//! ([`integrate`]'s warm-up branch; the price seed rises no faster than the v0.4.1 EMA can,
//! M4X-1, and since the owner's decision of 2026-09-30 falls no faster either, M6-SEC-1), and
//! the guard clears once `L_ema` agrees with the ring's liquidity lows; while it is set, `L_slow`
//! integrates no more than those lows (M4X-2). A state that was never marked (`warm = guard =
//! 0`) integrates exactly as v0.4.1.

use crate::consts::{COVERAGE_GAP_S, EMA_TAU_S, EMA_WARMUP_BUCKETS, REF_GUARD_TOL_BPS};
use crate::error::MathError;
use crate::num::{BPS, Rounding, U256, mul_div, mul_le, overflow_to_max, saturating_u64, to_u128};
use crate::quote::reprice;

pub use crate::consts::RING_LEN;

/// The observation bucket of the current second (ECON §5.1; ARCH §2.3.4 `b_*`).
///
/// Open iff `lo != 0`. `lo`/`hi` are the lowest and highest spot price recorded in second
/// `sec`, `llo`/`lhi` the lowest and highest liquidity.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Bucket {
    pub lo: u128,
    pub hi: u128,
    pub llo: u128,
    pub lhi: u128,
    pub sec: i64,
    pub _pad: [u8; 8],
}

impl Bucket {
    /// The empty bucket.
    pub const EMPTY: Self = Self {
        lo: 0,
        hi: 0,
        llo: 0,
        lhi: 0,
        sec: 0,
        _pad: [0; 8],
    };

    /// Whether the bucket holds at least one observation.
    pub const fn is_open(&self) -> bool {
        self.lo != 0
    }

    /// Empties the bucket.
    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }
}

/// The last completed bucket (ECON §5.4; ARCH §2.3.4 `lb_*`): the evidence of the robust
/// spot and of the exit references. Set iff `lo != 0`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LastBucket {
    pub lo: u128,
    pub hi: u128,
    pub sec: i64,
    pub _pad: [u8; 8],
}

impl LastBucket {
    /// No completed bucket yet.
    pub const NONE: Self = Self {
        lo: 0,
        hi: 0,
        sec: 0,
        _pad: [0; 8],
    };

    /// Whether a completed bucket exists.
    pub const fn is_set(&self) -> bool {
        self.lo != 0
    }

    /// A completed bucket exists and `now − sec ≤ max_age_s` (ECON §5.4, §8.2: the fresh
    /// reference; ARCH §3.6 precondition 2, `ref_max_age_s`).
    pub fn is_fresh(&self, now: i64, max_age_s: u8) -> bool {
        self.is_set() && now.saturating_sub(self.sec) <= i64::from(max_age_s)
    }
}

/// EMA state (ECON §5.2; ARCH §2.3.4 `ema_price`, `ema_liq`, `liq_slow`, `ema_ts`,
/// `slow_ts`, `ema_count`, `ema_warm`, `ema_guard`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ema {
    /// `P_ema` (WAD).
    pub price: u128,
    /// `L_ema`, the fast liquidity EMA.
    pub liq: u128,
    /// `L_slow`, the slow liquidity EMA (`liq_slow_tau_s`).
    pub liq_slow: u128,
    /// Second of the last integration of `price` and `liq`.
    pub ts: i64,
    /// Second of the last integration of `liq_slow`.
    pub slow_ts: i64,
    /// Number of integrations so far (`ema_count`; the settlement persistence counts it).
    pub count: u64,
    /// Integrations left in the post-change warm-up (`ema_warm`; M4R): [`EMA_WARMUP_BUCKETS`]
    /// after a restart ([`mark_restart`]), counted down by [`integrate`]; `0` = confirmed.
    pub warm: u8,
    /// `1` while the post-change reference guard is set (`ema_guard`; M4R): the exit
    /// reference reads `min(L_ema, L_mlo)` (`bounds::ref_liquidity`). Set by [`mark_restart`],
    /// cleared by [`integrate`] once the warm-up is over and `L_ema·10⁴ ≤ L_mlo·(10⁴ +
    /// REF_GUARD_TOL_BPS)`.
    pub guard: u8,
    pub _pad: [u8; 6],
}

impl Ema {
    /// State at market creation: `P_ema = P_spot`, `L_ema = L_slow = √(X·Y)` (ECON §5.2;
    /// ARCH §4.4 "Initialisation"). `InvalidInput` if `p` or `l` is 0.
    pub fn new(now: i64, p: u128, l: u128) -> Result<Self, MathError> {
        nonzero(p, l)?;
        Ok(Self {
            price: p,
            liq: l,
            liq_slow: l,
            ts: now,
            slow_ts: now,
            count: 0,
            warm: 0,
            guard: 0,
            _pad: [0; 6],
        })
    }
}

/// Marks the observation state unconfirmed after a restart (M4R; module docs): `warm =
/// EMA_WARMUP_BUCKETS`, `guard = 1`. Every Raydium adoption calls it after
/// [`rewrite_liquidity`], and [`Observation::new_raydium`] for a Raydium price start.
pub fn mark_restart(ema: &mut Ema) {
    ema.warm = EMA_WARMUP_BUCKETS;
    ema.guard = 1;
}

/// The post-change warm-up is over (`ema.warm == 0`): opens and keeper exits may read the EMA
/// (M4R; ARCH §3.4 step 3, §3.6 precondition 1b, §3.7, §3.8 step 3).
pub const fn warmed(ema: &Ema) -> bool {
    ema.warm == 0
}

/// One ring entry (ECON §2.1 `RingEntry`, §5.4; ARCH §2.3.4, §4.4): the EMA state after an
/// integration, its cached reprojection and the extremes of the bucket it integrated.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RingEntry {
    /// `P_ema` after the step (growth `g`, the expiry reference).
    pub p: u128,
    /// `L_ema` after the step.
    pub l: u128,
    /// Cached `(x, y) = reprice(l, p)` (decision 3): the position-independent state that the
    /// liquidation EMA leg and the D4 margin quote on. `(0, 0)` when `reprice` is undefined
    /// (the sim's `reprice(l, p) or (0, 0)`); a quote on `x = 0` is undefined.
    pub x: u128,
    pub y: u128,
    /// Lowest and highest spot price of the integrated bucket (v0.4.1, R4B-2: the
    /// settlement box's medians). The creation entry holds `P` in both.
    pub lo: u128,
    pub hi: u128,
    /// Lowest and highest liquidity of the integrated bucket (the creation entry: `L`).
    pub llo: u128,
    pub lhi: u128,
    /// Second of the integration (the roll time, not the bucket's second).
    pub ts: i64,
    pub _pad: [u8; 8],
}

impl RingEntry {
    /// An entry whose `(x, y)` is `reprice(l, p)`, or `(0, 0)` where that is undefined
    /// (sim `Market._entry`).
    pub fn reprojected(
        ts: i64,
        p: u128,
        l: u128,
        lo: u128,
        hi: u128,
        llo: u128,
        lhi: u128,
    ) -> Self {
        let (x, y) = reprice(l, p).unwrap_or((0, 0));
        Self {
            p,
            l,
            x,
            y,
            lo,
            hi,
            llo,
            lhi,
            ts,
            _pad: [0; 8],
        }
    }
}

/// The last [`RING_LEN`] integrations (ECON §5.4). `entries[head]` is the newest; `len`
/// entries are valid. A ring built by [`Ring::new`] is never empty.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ring {
    pub entries: [RingEntry; RING_LEN],
    pub head: u8,
    pub len: u8,
    pub _pad: [u8; 14],
}

impl Ring {
    /// The ring at market creation: one entry `(now, P, L, reprice(L, P), P, P, L, L)`
    /// (sim `Market.__init__`; ARCH §4.4 "Initialisation"). `InvalidInput` if `p` or `l`
    /// is 0.
    pub fn new(now: i64, p: u128, l: u128) -> Result<Self, MathError> {
        nonzero(p, l)?;
        let mut ring = Self::default();
        ring.push(RingEntry::reprojected(now, p, l, p, p, l, l))?;
        Ok(ring)
    }

    /// `InvalidInput` unless `head < RING_LEN` and `len ≤ RING_LEN` (a corrupted account).
    pub fn check(&self) -> Result<(), MathError> {
        if usize::from(self.head) < RING_LEN && usize::from(self.len) <= RING_LEN {
            Ok(())
        } else {
            Err(MathError::InvalidInput)
        }
    }

    /// Number of valid entries (at most [`RING_LEN`]).
    pub fn len(&self) -> usize {
        usize::from(self.len).min(RING_LEN)
    }

    /// Whether the ring has no entry.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Slot of the `k`-th newest entry (`k = 0` is the newest).
    fn slot(&self, k: usize) -> Option<usize> {
        if k >= self.len() {
            return None;
        }
        usize::from(self.head)
            .checked_add(RING_LEN)?
            .checked_sub(k)?
            .checked_rem(RING_LEN)
    }

    /// The `k`-th newest entry (`k = 0` is the newest).
    pub fn get(&self, k: usize) -> Option<&RingEntry> {
        self.entries.get(self.slot(k)?)
    }

    /// The newest entry.
    pub fn newest(&self) -> Option<&RingEntry> {
        self.get(0)
    }

    /// Entries from the newest to the oldest.
    pub fn iter(&self) -> impl Iterator<Item = &RingEntry> + '_ {
        (0..self.len()).filter_map(move |k| self.get(k))
    }

    /// Appends `e` as the newest entry, dropping the oldest when full.
    pub fn push(&mut self, e: RingEntry) -> Result<(), MathError> {
        self.check()?;
        let head = if self.len == 0 {
            0
        } else {
            usize::from(self.head)
                .checked_add(1)
                .and_then(|h| h.checked_rem(RING_LEN))
                .ok_or(MathError::Overflow)?
        };
        let slot = self.entries.get_mut(head).ok_or(MathError::InvalidInput)?;
        *slot = e;
        self.head = u8::try_from(head).map_err(|_| MathError::Overflow)?;
        self.len = u8::try_from(
            self.len()
                .checked_add(1)
                .ok_or(MathError::Overflow)?
                .min(RING_LEN),
        )
        .map_err(|_| MathError::Overflow)?;
        Ok(())
    }
}

/// Market-level EMA parameters (ARCH §2.3.3 scope M, §2.3.4 `ema_params`; ECON §5.2,
/// §15). τ is the compile-time constant [`EMA_TAU_S`] (R3B-6).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EmaParams {
    /// Δt_max (`ema_dtmax_s`).
    pub dtmax_s: u16,
    pub _pad: [u8; 2],
    /// C (`ema_clamp_bps`): an observation is clamped to `[P/(1 + C), P·(1 + C)]`.
    pub clamp_bps: u32,
    /// `liq_slow_tau_s`.
    pub slow_tau_s: u32,
    /// `liq_slow_dtmax_s`.
    pub slow_dtmax_s: u32,
}

impl EmaParams {
    /// Defaults (ECON §5.7, §15): Δt_max = 2 s, C = 100 %, slow τ = 3,600 s, slow
    /// Δt_max = 60 s.
    pub const DEFAULT: Self = Self::new(2, 10_000, 3_600, 60);

    /// The parameters with the given values.
    pub const fn new(dtmax_s: u16, clamp_bps: u32, slow_tau_s: u32, slow_dtmax_s: u32) -> Self {
        Self {
            dtmax_s,
            _pad: [0; 2],
            clamp_bps,
            slow_tau_s,
            slow_dtmax_s,
        }
    }

    /// `InvalidInput` outside the compiled hard bounds (ARCH §2.3.3; ECON §15; sim
    /// `TIER_BOUNDS` and `COMPILED_BOUNDS`; [`crate::params::Rule::bounds`]): Δt_max ∈
    /// [1, 2], C ∈ [5,000, 10,000], slow τ ∈ [600, 86,400], slow Δt_max ∈ [1, 600].
    pub fn validate(&self) -> Result<(), MathError> {
        use crate::params::Rule;
        let within = |rule: Rule, v: u32| {
            let (lo, hi) = rule.bounds();
            (lo..=hi).contains(&u64::from(v))
        };
        let ok = within(Rule::EmaDtmaxS, self.dtmax_s.into())
            && within(Rule::EmaClampBps, self.clamp_bps)
            && within(Rule::LiqSlowTauS, self.slow_tau_s)
            && within(Rule::LiqSlowDtmaxS, self.slow_dtmax_s);
        if ok {
            Ok(())
        } else {
            Err(MathError::InvalidInput)
        }
    }
}

impl Default for EmaParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The whole observation state of a market, in the order the prologue uses it
/// (ARCH §3.0 steps 5 and 7). A convenience wrapper over the free functions.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Observation {
    pub ema: Ema,
    pub ring: Ring,
    pub bucket: Bucket,
    pub last: LastBucket,
}

impl Observation {
    /// State at market creation (ARCH §4.4 "Initialisation"): unmarked. The mock and the pump
    /// venues start this way.
    pub fn new(now: i64, p: u128, l: u128) -> Result<Self, MathError> {
        Ok(Self {
            ema: Ema::new(now, p, l)?,
            ring: Ring::new(now, p, l)?,
            bucket: Bucket::EMPTY,
            last: LastBucket::NONE,
        })
    }

    /// A Raydium price start (M4R; ARCH §3.2, §4.4: a Raydium listing, a list-only market's
    /// first adoption): [`Observation::new`] at the pool's spot `p` with `L = min(l_sample,
    /// cap)` — `cap = ⌊D·10⁴/λ_min⌋` the anchor, which one wrapped deposit cannot exceed — and
    /// `L_slow = min(L, slow_cap)`, `slow_cap` the pool's locked depth `D`, as an adoption's
    /// [`VenueChange::RaydiumLocked`] leaves it; then [`mark_restart`]. AIR-13: `L_slow` started
    /// at `L`, so a deposit wrapped around the start seeded it at up to `D/λ_min` (2D at the
    /// default `λ_min`); the warm-up re-seeds `L_ema` from the pool's own buckets but leaves
    /// `L_slow`, which sizing, the tier and the exit reference read, to decay at `τ_slow`.
    /// `InvalidInput` if `p`, `l_sample`, `cap` or `slow_cap` is 0.
    pub fn new_raydium(
        now: i64,
        p: u128,
        l_sample: u128,
        cap: u128,
        slow_cap: u128,
    ) -> Result<Self, MathError> {
        if slow_cap == 0 {
            return Err(MathError::InvalidInput);
        }
        let mut o = Self::new(now, p, l_sample.min(cap))?;
        o.ema.liq_slow = o.ema.liq_slow.min(slow_cap);
        mark_restart(&mut o.ema);
        Ok(o)
    }

    /// Prologue step 5: [`integrate`] a completed bucket, if any; `Some(gap)` if it rolled.
    pub fn roll(&mut self, now: i64, params: &EmaParams) -> Result<Option<i64>, MathError> {
        integrate(
            &mut self.ema,
            &mut self.ring,
            &mut self.bucket,
            &mut self.last,
            now,
            params,
        )
    }

    /// Prologue step 7: [`record`] the pre-trade `(P, L)` of an executable venue.
    pub fn record(&mut self, now: i64, p: u128, l: u128) -> Result<(), MathError> {
        record(&mut self.bucket, now, p, l)
    }

    /// [`rewrite_liquidity`] at a venue change.
    pub fn rewrite_liquidity(&mut self, l_new: u128, kind: VenueChange) -> Result<(), MathError> {
        rewrite_liquidity(&mut self.ema, &mut self.ring, &mut self.bucket, l_new, kind)
    }

    /// [`mark_restart`] on the EMA.
    pub fn mark_restart(&mut self) {
        mark_restart(&mut self.ema);
    }
}

fn nonzero(p: u128, l: u128) -> Result<(), MathError> {
    if p == 0 || l == 0 {
        Err(MathError::InvalidInput)
    } else {
        Ok(())
    }
}

/// Adds the pre-trade spot `(P, L)` to the bucket of second `now` (ECON §5.1; ARCH §4.4
/// `record`, prologue step 7). Every instruction of the second records; there is no
/// per-slot limit.
///
/// The bucket must be empty or already be second `now`: a completed bucket is rolled by
/// [`integrate`] first (prologue step 5), so an open bucket of another second means the
/// caller skipped the roll or the clock went backwards. Errors: `InvalidInput` in that
/// case, or if `p` or `l` is 0.
pub fn record(bucket: &mut Bucket, now: i64, p: u128, l: u128) -> Result<(), MathError> {
    nonzero(p, l)?;
    if !bucket.is_open() {
        *bucket = Bucket {
            lo: p,
            hi: p,
            llo: l,
            lhi: l,
            sec: now,
            _pad: [0; 8],
        };
        return Ok(());
    }
    if bucket.sec != now {
        return Err(MathError::InvalidInput);
    }
    bucket.lo = bucket.lo.min(p);
    bucket.hi = bucket.hi.max(p);
    bucket.llo = bucket.llo.min(l);
    bucket.lhi = bucket.lhi.max(l);
    Ok(())
}

/// Rolls a completed bucket into the EMA, the slow liquidity EMA and the ring (ECON §5.2;
/// ARCH §4.4 `integrate`, prologue step 5). Nothing happens unless the bucket is open and
/// `now > bucket.sec`, so the EMA an instruction reads never contains that instruction's
/// own observation (the one-second lag) and it moves at most once per second.
///
/// A confirmed state (`warm = 0`) steps as v0.4.1:
///
/// ```text
/// obs   = clamp(P_ema, b_lo, b_hi)                        directional aggregation
/// obs   = clamp(obs, ⌊P_ema·10⁴/(10⁴+C)⌋, ⌊P_ema·(10⁴+C)/10⁴⌋)
/// lobs  = the same for L_ema with b_llo, b_lhi
/// dt    = min(now − ema_ts, Δt_max)
/// P_ema = ⌊(P_ema·τ + obs·dt)/(τ + dt)⌋ ;  L_ema likewise with lobs      τ = EMA_TAU_S
/// sin   = min(lobs, L_mlo) while guard = 1, else lobs                   (M4X-2)
/// dts   = min(now − slow_ts, liq_slow_dtmax_s) ; L_slow = ⌊(L_slow·τ_slow + sin·dts)/(τ_slow + dts)⌋
/// ring.push(now, P_ema, L_ema, reprice(L_ema, P_ema), b_lo, b_hi, b_llo, b_lhi)
/// ```
///
/// then `ema_ts = slow_ts = now`, `count += 1`. A state in the post-change warm-up (`warm > 0`,
/// M4R; module docs) instead **re-seeds** the EMA over the post-change entries only — this
/// bucket and the `k = EMA_WARMUP_BUCKETS − warm` newest ring entries, which the earlier
/// warm-up integrations pushed — with no blend:
///
/// ```text
/// P_ema = max(min(min lo, price_up_bound(P_ema, dt)),    (the fresh price, at most one
///             price_down_bound(P_ema, dt))                 v0.4.1 step either way; M4X-1,
///                                                          M6-SEC-1)
/// L_ema = min(max lhi, ⌊lomed(lhi)·(10⁴+C)/10⁴⌋)          lomed = index ⌊(n−1)/2⌋ of the sorted highs
/// ring.push(now, P_ema, L_ema, reprice(L_ema, P_ema), b_lo, b_hi, b_llo, b_lhi)
/// ema_ts = now, count += 1, warm −= 1                     (L_slow and slow_ts unchanged)
/// ```
///
/// The price seed moves per integration by no more than the v0.4.1 step can, up
/// ([`price_up_bound`]; M4X-1) and down ([`price_down_bound`]; M6-SEC-1, owner decision of
/// 2026-09-30): attacker-only warm-up seconds reach no higher than the same seconds do in steady
/// state with a large enough pump, and one deliberately recorded low (a pool pushed down and
/// restored inside one transaction) pulls the seed down by at most one steady step per remaining
/// warm-up integration (×11/12 at `dt = 1`, ×6/7 at `dt = 2`, C = 100 %; the low stays in the
/// running minimum until the gate, ≥ ×0.4986 over 8 integrations) instead of to the low at once;
/// a genuine move during the halt is tracked at that rate either way. A seed that lags a genuine
/// fall forces no exit: the spot leg reads the live price. A withdraw-wrapped sample lowers only
/// its bucket's `llo`, which the seed never reads; one honest bucket among them bounds `L_ema`
/// from below by `min(H, (1 + C)·D)`, and `P_ema` from above by its low once the seed has fallen
/// to it; pushing `L_ema` above `(1 + C)·H` needs 5 of the 8 buckets. After either branch, a
/// set guard with `warm = 0` clears iff `L_ema·10⁴ ≤ L_mlo·(10⁴ + REF_GUARD_TOL_BPS)`, `L_mlo`
/// the lower median of the ring's bucket liquidity lows after the push (the same value as
/// [`ring_liq_lower_median`] then); nothing else sets or clears it. While the guard is set, the
/// steady step feeds `L_slow` with `min(lobs, L_mlo)`: the reference `L_b` reads `L_slow`
/// unguarded (`bounds::ref_liquidity`), so a deposit freeze of `L_ema` must not climb into it
/// (M4X-2).
///
/// Either way the bucket becomes the last completed bucket and is cleared. The clamped
/// `obs`/`lobs` drive the step but are not stored (v0.4.1: the ring keeps the bucket extremes,
/// R4B-2). Every division floors, exactly as the sim. If `dt ≤ 0` (only reachable when the EMA
/// was re-initialised after the bucket opened) the EMA and ring are left alone — no step, no
/// warm-up count — and only the bucket moves, again as the sim.
///
/// Returns `None` if nothing was rolled, else `Some(now − ema_ts)` with the EMA timestamp
/// **before** the roll: the pre-roll gap that the caller reports as `CoverageGap` when the
/// market has open positions ([`coverage_gap`]; ARCH §4.4, A69).
///
/// All arithmetic is computed before anything is written, so an error leaves the state
/// untouched. Errors: `InvalidInput` for a corrupted ring or a `warm` above
/// [`EMA_WARMUP_BUCKETS`] (never written); `DivByZero` if `τ_slow + dts` is 0 (only with
/// parameters outside [`EmaParams::validate`]).
pub fn integrate(
    ema: &mut Ema,
    ring: &mut Ring,
    bucket: &mut Bucket,
    last: &mut LastBucket,
    now: i64,
    params: &EmaParams,
) -> Result<Option<i64>, MathError> {
    if !bucket.is_open() || now <= bucket.sec {
        return Ok(None);
    }
    ring.check()?;
    let gap = now.saturating_sub(ema.ts);
    let dt = elapsed(now, ema.ts, params.dtmax_s.into());
    if dt > 0 {
        // `L_mlo` after the push, needed while the guard is set and the warm-up ends with this
        // integration or has ended: for `L_slow`'s input (M4X-2) and the guard check.
        let l_mlo = if ema.guard != 0 && ema.warm <= 1 {
            Some(lows_median_after(ring, bucket.llo)?)
        } else {
            None
        };
        let (mut next, entry) = if ema.warm > 0 {
            warm_step(ema, ring, bucket, now, dt, params)?
        } else {
            steady_step(ema, bucket, now, dt, params, l_mlo)?
        };
        if next.guard != 0 && next.warm == 0 && l_mlo.is_some_and(|m| guard_clears(next.liq, m)) {
            next.guard = 0;
        }
        ring.push(entry)?;
        *ema = next;
    }
    *last = LastBucket {
        lo: bucket.lo,
        hi: bucket.hi,
        sec: bucket.sec,
        _pad: [0; 8],
    };
    bucket.clear();
    Ok(Some(gap))
}

/// The largest v0.4.1 price step from `p_ema` over `dt` (capped) seconds (ECON §5.2; M4X-1): the
/// step of [`integrate`] with its observation at the upper clamp, `⌊(p_ema·τ + ⌊p_ema·(10⁴ +
/// C)/10⁴⌋·dt)/(τ + dt)⌋`, `τ = EMA_TAU_S`, `C = clamp_bps` — `×(τ + dt·(1 + C))/(τ + dt)`, 9/7 at
/// `dt = 2` and 7/6 at `dt = 1` with C = 100 %. No integration of a confirmed state moves `P_ema`
/// above it; the warm-up bounds its price seed by it, and the I18 hook recomputes it.
pub fn price_up_bound(p_ema: u128, dt: u128, clamp_bps: u32) -> Result<u128, MathError> {
    let bps = u128::from(BPS);
    let den = bps
        .checked_add(u128::from(clamp_bps))
        .ok_or(MathError::Overflow)?;
    let obs = overflow_to_max(mul_div(p_ema, den, bps, Rounding::Floor))?;
    weighted(p_ema, u128::from(EMA_TAU_S), obs, dt)
}

/// The largest v0.4.1 price **fall** from `p_ema` over `dt` (capped) seconds (M6-SEC-1, owner
/// decision of 2026-09-30; the mirror of [`price_up_bound`]): the step of [`integrate`] with its
/// observation at the lower clamp, `⌊(p_ema·τ + ⌊p_ema·10⁴/(10⁴ + C)⌋·dt)/(τ + dt)⌋` — 11/12 at
/// `dt = 1` and 6/7 at `dt = 2` with C = 100 %. No integration of a confirmed state moves `P_ema`
/// below it; the warm-up bounds its price seed by it.
pub fn price_down_bound(p_ema: u128, dt: u128, clamp_bps: u32) -> Result<u128, MathError> {
    let bps = u128::from(BPS);
    let den = bps
        .checked_add(u128::from(clamp_bps))
        .ok_or(MathError::Overflow)?;
    let obs = mul_div(p_ema, bps, den, Rounding::Floor)?;
    weighted(p_ema, u128::from(EMA_TAU_S), obs, dt)
}

/// The v0.4.1 step of a confirmed state ([`integrate`]): the next EMA and the entry to push.
/// `l_mlo` is `Some` (the ring's liquidity-low median after the push) iff the guard is set:
/// `L_slow` then integrates `min(lobs, L_mlo)` (M4X-2).
fn steady_step(
    ema: &Ema,
    bucket: &Bucket,
    now: i64,
    dt: u128,
    params: &EmaParams,
    l_mlo: Option<u128>,
) -> Result<(Ema, RingEntry), MathError> {
    let c = u128::from(params.clamp_bps);
    let obs = aggregate(ema.price, bucket.lo, bucket.hi, c)?;
    let lobs = aggregate(ema.liq, bucket.llo, bucket.lhi, c)?;
    let tau = u128::from(EMA_TAU_S);
    let price = weighted(ema.price, tau, obs, dt)?;
    let liq = weighted(ema.liq, tau, lobs, dt)?;
    let dts = elapsed(now, ema.slow_ts, params.slow_dtmax_s.into());
    let slow_in = l_mlo.map_or(lobs, |m| lobs.min(m));
    let liq_slow = weighted(ema.liq_slow, params.slow_tau_s.into(), slow_in, dts)?;
    let count = ema.count.checked_add(1).ok_or(MathError::Overflow)?;
    let entry = RingEntry::reprojected(
        now, price, liq, bucket.lo, bucket.hi, bucket.llo, bucket.lhi,
    );
    Ok((
        Ema {
            price,
            liq,
            liq_slow,
            ts: now,
            slow_ts: now,
            count,
            ..*ema
        },
        entry,
    ))
}

/// The warm-up step of an unconfirmed state ([`integrate`]; M4R): the seed over this bucket and
/// the `EMA_WARMUP_BUCKETS − warm` newest (post-change) ring entries, the price bounded by the
/// largest v0.4.1 step up (M4X-1) and down (M6-SEC-1) from the current `P_ema`.
fn warm_step(
    ema: &Ema,
    ring: &Ring,
    bucket: &Bucket,
    now: i64,
    dt: u128,
    params: &EmaParams,
) -> Result<(Ema, RingEntry), MathError> {
    let k = usize::from(
        EMA_WARMUP_BUCKETS
            .checked_sub(ema.warm)
            .ok_or(MathError::InvalidInput)?,
    );
    let mut highs = [0u128; RING_LEN];
    *highs.first_mut().ok_or(MathError::InvalidInput)? = bucket.lhi;
    let mut p = bucket.lo;
    for i in 0..k {
        let e = ring.get(i).ok_or(MathError::InvalidInput)?;
        p = p.min(e.lo);
        *highs
            .get_mut(i.checked_add(1).ok_or(MathError::Overflow)?)
            .ok_or(MathError::InvalidInput)? = e.lhi;
    }
    // M4X-1: never above the largest v0.4.1 step; M6-SEC-1: never below the largest v0.4.1 fall
    // (`down ≤ P_ema ≤ up`, so the clamp is well formed).
    let p = p
        .min(price_up_bound(ema.price, dt, params.clamp_bps)?)
        .max(price_down_bound(ema.price, dt, params.clamp_bps)?);
    let n = k.checked_add(1).ok_or(MathError::Overflow)?;
    let highs = highs.get_mut(..n).ok_or(MathError::InvalidInput)?;
    highs.sort_unstable();
    let max = *highs.last().ok_or(MathError::InvalidInput)?;
    // n − 1 = k: the lower median's index is ⌊k/2⌋.
    let lomed = *highs.get(k / 2).ok_or(MathError::InvalidInput)?;
    let bps = u128::from(BPS);
    let up = bps
        .checked_add(u128::from(params.clamp_bps))
        .ok_or(MathError::Overflow)?;
    let l = max.min(overflow_to_max(mul_div(lomed, up, bps, Rounding::Floor))?);
    let count = ema.count.checked_add(1).ok_or(MathError::Overflow)?;
    let warm = ema.warm.checked_sub(1).ok_or(MathError::Underflow)?;
    let entry = RingEntry::reprojected(now, p, l, bucket.lo, bucket.hi, bucket.llo, bucket.lhi);
    Ok((
        Ema {
            price: p,
            liq: l,
            ts: now,
            count,
            warm,
            ..*ema
        },
        entry,
    ))
}

/// Whether a set reference guard clears (M4R): `l_ema·10⁴ ≤ l_mlo·(10⁴ + REF_GUARD_TOL_BPS)`, with
/// `l_mlo` from [`lows_median_after`].
fn guard_clears(l_ema: u128, l_mlo: u128) -> bool {
    mul_le(
        l_ema,
        u128::from(BPS),
        l_mlo,
        u128::from(BPS).saturating_add(u128::from(REF_GUARD_TOL_BPS)),
    )
}

/// `L_mlo` of the ring after pushing an entry with liquidity low `new_llo`: the lower median of
/// `new_llo` and the lows of the `RING_LEN − 1` newest entries (M4R guard; M4X-2).
fn lows_median_after(ring: &Ring, new_llo: u128) -> Result<u128, MathError> {
    let mut lows = [0u128; RING_LEN];
    *lows.first_mut().ok_or(MathError::InvalidInput)? = new_llo;
    let keep = ring.len().min(RING_LEN.saturating_sub(1));
    for i in 0..keep {
        let e = ring.get(i).ok_or(MathError::InvalidInput)?;
        *lows
            .get_mut(i.checked_add(1).ok_or(MathError::Overflow)?)
            .ok_or(MathError::InvalidInput)? = e.llo;
    }
    let n = keep.checked_add(1).ok_or(MathError::Overflow)?;
    lower_median(lows.get_mut(..n).ok_or(MathError::InvalidInput)?)
}

/// The lower median (index `⌊(n − 1)/2⌋` of the sorted values) of a non-empty slice, which it
/// sorts. `InvalidInput` if empty.
fn lower_median(vals: &mut [u128]) -> Result<u128, MathError> {
    vals.sort_unstable();
    let idx = vals.len().checked_sub(1).ok_or(MathError::InvalidInput)? / 2;
    vals.get(idx).copied().ok_or(MathError::InvalidInput)
}

/// `L_mlo`, the lower median of the ring entries' bucket liquidity lows `llo` — the third value
/// of [`ring_medians`], alone (M4R's reference guard, `bounds::ref_liquidity`). Errors:
/// `InvalidInput` for a corrupted or empty ring.
pub fn ring_liq_lower_median(ring: &Ring) -> Result<u128, MathError> {
    ring.check()?;
    let mut lows = [0u128; RING_LEN];
    let n = ring.len();
    for (slot, e) in lows.iter_mut().zip(ring.iter()) {
        *slot = e.llo;
    }
    lower_median(lows.get_mut(..n).ok_or(MathError::InvalidInput)?)
}

/// `L_mhi`, the upper median (index `⌊n/2⌋` of the sorted values) of the ring entries' bucket
/// liquidity highs `lhi` — the fourth value of [`ring_medians`], alone (AIR-17: the spot leg's
/// floor while the post-change guard is set, `bounds::spot_liquidity`). Errors: `InvalidInput` for
/// a corrupted or empty ring.
pub fn ring_liq_upper_median(ring: &Ring) -> Result<u128, MathError> {
    ring.check()?;
    let mut highs = [0u128; RING_LEN];
    let n = ring.len();
    for (slot, e) in highs.iter_mut().zip(ring.iter()) {
        *slot = e.lhi;
    }
    let highs = highs.get_mut(..n).ok_or(MathError::InvalidInput)?;
    highs.sort_unstable();
    highs.get(n / 2).copied().ok_or(MathError::InvalidInput)
}

/// Whether an integration with pre-roll gap `gap_s` ([`integrate`]) emits `CoverageGap`:
/// the market has open positions and `gap_s > COVERAGE_GAP_S` (ARCH §4.4, A69).
pub const fn coverage_gap(gap_s: i64, open_positions: u64) -> bool {
    open_positions > 0 && gap_s > COVERAGE_GAP_S
}

/// `min(max(now − since, 0), cap)` seconds.
fn elapsed(now: i64, since: i64, cap: u128) -> u128 {
    let d = i128::from(now).saturating_sub(i128::from(since));
    u128::try_from(d).map_or(0, |d| d.min(cap))
}

/// Directional aggregation of one bucket side, then the ±C clamp (ECON §5.2).
fn aggregate(e: u128, lo: u128, hi: u128, c_bps: u128) -> Result<u128, MathError> {
    let bps = u128::from(BPS);
    let x = e.max(lo).min(hi);
    let den = bps.checked_add(c_bps).ok_or(MathError::Overflow)?;
    let lower = mul_div(e, bps, den, Rounding::Floor)?;
    // The upper clamp only ever bounds a u128 from above: past u128 it cannot bind.
    let upper = overflow_to_max(mul_div(e, den, bps, Rounding::Floor))?;
    Ok(x.max(lower).min(upper))
}

/// `⌊(a·wa + b·wb)/(wa + wb)⌋`. ECON §2.2 bounds `P_ema·(τ + dt)` at 4.2·10³⁶, so the u128
/// path is the one taken; the 256-bit fallback keeps any larger state exact (the result
/// lies between `a` and `b`, so it fits).
fn weighted(a: u128, wa: u128, b: u128, wb: u128) -> Result<u128, MathError> {
    let den = wa.checked_add(wb).ok_or(MathError::Overflow)?;
    let fast = a
        .checked_mul(wa)
        .zip(b.checked_mul(wb))
        .and_then(|(x, y)| x.checked_add(y));
    if let Some(num) = fast {
        return num.checked_div(den).ok_or(MathError::DivByZero);
    }
    let num = U256::from_product(a, wa)
        .checked_add(U256::from_product(b, wb))
        .ok_or(MathError::Overflow)?;
    let (q, _) = num.div_rem_u128(den).ok_or(MathError::DivByZero)?;
    to_u128(q)
}

/// Robust spot `P_rs = min(P_live, lb_lo)`, defined only while the last completed bucket
/// is fresh (`now − lb_sec ≤ max_age_s`) (ECON §5.4; ARCH §4.4). `None` means the spot leg
/// cannot be confirmed this second: the keeper cranks one second and acts the next.
pub fn robust_spot(p_live: u128, last: &LastBucket, now: i64, max_age_s: u8) -> Option<u128> {
    last.is_fresh(now, max_age_s).then(|| p_live.min(last.lo))
}

/// Sustained EMA growth `g` in bps per second (ECON §2.1, §5.4; ARCH §4.4):
/// `max(0, lower median over the newest n ring steps of ⌊(P_i − P_{i−1})·10⁴/(P_{i−1}·Δt_i)⌋)`,
/// the lower median the value at index `⌊(n − 1)/2⌋` of the sorted rates.
///
/// 0 if the ring has fewer than `n + 1` entries, if `n` is 0, if any of those steps has `Δt ≤ 0`
/// or a zero price, or if more than `⌊(n − 1)/2⌋` of them (2 of 3, 2 of 4) rate 0 or less —
/// flat, falling or rising by under 1 bp/s (sim `ema_growth_bps`). A single held-pump second
/// produces one step; only a trend over a majority of the window opens the cap. AIR-22: the
/// minimum read before let one flat step in
/// every `n` — a low recorded at or below `P_ema` in a pump's bucket, which the directional
/// aggregation turns into `obs = P_ema` — hold `g`, and with it `κ_eff`, at 0 through a genuine
/// pump. Saturates at `u64::MAX` bps/s (the sim's integer is unbounded); an integrated step grows
/// at most ×(1 + C) ≤ ×2, so rings built by [`integrate`] stay orders of magnitude below it.
/// Errors: `InvalidInput` for a corrupted ring.
pub fn sustained_growth(ring: &Ring, n: u8) -> Result<u64, MathError> {
    ring.check()?;
    let n = usize::from(n);
    if n == 0 || ring.len() <= n {
        return Ok(0);
    }
    // `n < ring.len() ≤ RING_LEN`.
    let mut rates = [0u128; RING_LEN];
    for (k, slot) in rates.iter_mut().enumerate().take(n) {
        let old = ring
            .get(k.checked_add(1).ok_or(MathError::Overflow)?)
            .ok_or(MathError::InvalidInput)?;
        let new = ring.get(k).ok_or(MathError::InvalidInput)?;
        if new.ts <= old.ts || old.p == 0 {
            return Ok(0);
        }
        // A flat or falling step's rate is ≤ 0; as 0 it orders the same under `max(0, ·)`.
        let Some(rise) = new.p.checked_sub(old.p).filter(|r| *r > 0) else {
            continue;
        };
        let dt = u128::try_from(i128::from(new.ts).saturating_sub(i128::from(old.ts)))
            .map_err(|_| MathError::Underflow)?;
        // ⌊⌊a/b⌋/c⌋ = ⌊a/(b·c)⌋; a rate past u128 orders above every finite one.
        *slot = overflow_to_max(mul_div(rise, u128::from(BPS), old.p, Rounding::Floor))?
            .checked_div(dt)
            .ok_or(MathError::DivByZero)?;
    }
    let g = lower_median(rates.get_mut(..n).ok_or(MathError::InvalidInput)?)?;
    Ok(saturating_u64(g))
}

/// The ring medians of the settlement box `(P_mlo, P_mhi, L_mlo, L_mhi)` (ECON §2.1, §5.6;
/// ARCH §2.2, §4.5; sim `Market.ring_medians`): the **lower** median of the entries' bucket
/// lows `lo` and `llo` (index `⌊(n − 1)/2⌋` of the sorted values, the 4th smallest of 8)
/// and the **upper** median of their highs `hi` and `lhi` (index `⌊n/2⌋`). Moving `P_mlo`
/// up or `P_mhi` down needs 5 of 8 entries, and only an attacker-only second can raise a
/// bucket low or lower a bucket high (R4B-2, R4A-9).
///
/// Errors: `InvalidInput` for a corrupted or empty ring (a market's ring always holds its
/// creation entry).
pub fn ring_medians(ring: &Ring) -> Result<(u128, u128, u128, u128), MathError> {
    ring.check()?;
    let n = ring.len();
    let lower = n.checked_sub(1).ok_or(MathError::InvalidInput)? / 2;
    let upper = n / 2;
    let mut cols = [[0u128; RING_LEN]; 4];
    for (k, e) in ring.iter().enumerate() {
        for (col, v) in cols.iter_mut().zip([e.lo, e.hi, e.llo, e.lhi]) {
            *col.get_mut(k).ok_or(MathError::InvalidInput)? = v;
        }
    }
    let mut pick = |col: usize, idx: usize| -> Result<u128, MathError> {
        let vals = cols
            .get_mut(col)
            .and_then(|c| c.get_mut(..n))
            .ok_or(MathError::InvalidInput)?;
        vals.sort_unstable();
        vals.get(idx).copied().ok_or(MathError::InvalidInput)
    };
    Ok((
        pick(0, lower)?,
        pick(1, upper)?,
        pick(2, lower)?,
        pick(3, upper)?,
    ))
}

/// The kind of venue change that rewrites the ring's liquidity (ARCH §2.5, §2.6, §4.4,
/// A72; ECON §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VenueChange {
    /// Graduation to the canonical pool: `L_ema = L_slow = l_new` (sim `_switch_to_pool`).
    Graduation,
    /// Raydium adoption or fast path as ARCH v0.4.1 rule 4 and the sim state it: `L_ema =
    /// min(l_new, L_ema)`, `L_slow = min(l_new, L_slow)`, so a lagging `L_slow` is never raised
    /// (sim `fast_switch`; kept for the differential vectors). The program uses
    /// [`VenueChange::RaydiumLocked`] (M4-SEC-5, M4-SEC-6).
    Raydium,
    /// Raydium adoption since the M4 security review (M4-SEC-5, M4-SEC-6) and M4R: `L_ema =
    /// l_new` — the new pool's anchor `⌊D·10⁴/λ_min⌋` (`pool_select::depth_at_lambda`; no sample
    /// of the pool, which one transaction can wrap; the post-change warm-up re-seeds it from the
    /// next [`EMA_WARMUP_BUCKETS`] buckets, [`mark_restart`], [`integrate`]) — and `L_slow =
    /// min(L_slow, slow_cap)` with `slow_cap` the new pool's locked depth `D`: never raised.
    RaydiumLocked {
        /// The new pool's locked depth `D` (> 0).
        slow_cap: u128,
    },
}

/// Rewrites the liquidity side of the observation state at a venue change (ECON §5.2
/// "Initialisation and venue changes"; ARCH §4.4 `rewrite_liquidity`, A72; sim
/// `_switch_to_pool` / `fast_switch` and `_rewrite_ring_liq`): `L_ema` and `L_slow` as
/// [`VenueChange`] says; then **every** ring entry gets `l = llo = lhi = L_ema` (the reset
/// value) and `(x, y) = reprice(L_ema, p)` recomputed, while its `ts`, `p`, `lo` and `hi`
/// carry over; the open bucket is cleared. `P_ema`, the EMA timestamps, the count and the
/// last completed bucket are unchanged. `l_new` is the new venue's `√(X·Y)`.
///
/// Without the rewrite the ring medians would carry the old venue's liquidity and the
/// settlement box would stay more than 1 % wide in `L` for 4 s after each change. It neither
/// marks nor clears the warm-up: a Raydium adoption calls [`mark_restart`] after it (M4R).
///
/// Errors: `InvalidInput` if `l_new` (or a [`VenueChange::RaydiumLocked`] cap) is 0 or the
/// ring is corrupted; nothing is written then.
pub fn rewrite_liquidity(
    ema: &mut Ema,
    ring: &mut Ring,
    bucket: &mut Bucket,
    l_new: u128,
    kind: VenueChange,
) -> Result<(), MathError> {
    if l_new == 0 {
        return Err(MathError::InvalidInput);
    }
    ring.check()?;
    let (liq, liq_slow) = match kind {
        VenueChange::Graduation => (l_new, l_new),
        VenueChange::Raydium => (l_new.min(ema.liq), l_new.min(ema.liq_slow)),
        VenueChange::RaydiumLocked { slow_cap } => {
            if slow_cap == 0 {
                return Err(MathError::InvalidInput);
            }
            (l_new, ema.liq_slow.min(slow_cap))
        }
    };
    for k in 0..ring.len() {
        let slot = ring.slot(k).ok_or(MathError::InvalidInput)?;
        let e = ring.entries.get_mut(slot).ok_or(MathError::InvalidInput)?;
        *e = RingEntry::reprojected(e.ts, e.p, liq, e.lo, e.hi, liq, liq);
    }
    ema.liq = liq;
    ema.liq_slow = liq_slow;
    bucket.clear();
    Ok(())
}

const _: () = {
    assert!(core::mem::size_of::<Bucket>() == 80);
    assert!(core::mem::size_of::<LastBucket>() == 48);
    assert!(core::mem::size_of::<Ema>() == 80);
    assert!(core::mem::size_of::<RingEntry>() == 144);
    assert!(core::mem::size_of::<Ring>() == 1_168);
    assert!(core::mem::size_of::<EmaParams>() == 16);
    assert!(core::mem::size_of::<Observation>() == 1_376);
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    const WAD: u128 = crate::num::WAD;

    fn entry(ts: i64, p: u128, lo: u128, hi: u128, llo: u128, lhi: u128) -> RingEntry {
        RingEntry::reprojected(ts, p, 1_000, lo, hi, llo, lhi)
    }

    #[test]
    fn layouts_have_no_implicit_padding() {
        assert_eq!(offset_of!(Bucket, sec), 64);
        assert_eq!(offset_of!(LastBucket, sec), 32);
        assert_eq!(offset_of!(Ema, ts), 48);
        assert_eq!(offset_of!(Ema, count), 64);
        assert_eq!(offset_of!(Ema, warm), 72);
        assert_eq!(offset_of!(Ema, guard), 73);
        assert_eq!(offset_of!(Ema, _pad), 74);
        assert_eq!(offset_of!(RingEntry, x), 32);
        assert_eq!(offset_of!(RingEntry, lo), 64);
        assert_eq!(offset_of!(RingEntry, lhi), 112);
        assert_eq!(offset_of!(RingEntry, ts), 128);
        assert_eq!(offset_of!(Ring, head), 1_152);
        assert_eq!(offset_of!(Ring, len), 1_153);
        assert_eq!(offset_of!(EmaParams, clamp_bps), 4);
        assert_eq!(offset_of!(Observation, ring), 80);
        assert_eq!(offset_of!(Observation, bucket), 1_248);
        assert_eq!(offset_of!(Observation, last), 1_328);
    }

    #[test]
    fn creation_entry() {
        let o = Observation::new(7, 4 * WAD, 1_000).unwrap();
        let e = *o.ring.newest().unwrap();
        assert_eq!((e.ts, e.p, e.l), (7, 4 * WAD, 1_000));
        assert_eq!((e.lo, e.hi, e.llo, e.lhi), (4 * WAD, 4 * WAD, 1_000, 1_000));
        // sp = 2·10⁹: X = 1,000·10⁹/(2·10⁹) = 500, Y = 2,000.
        assert_eq!((e.x, e.y), (500, 2_000));
        assert_eq!(ring_medians(&o.ring), Ok((4 * WAD, 4 * WAD, 1_000, 1_000)));
        assert_eq!(Ring::new(0, 0, 1), Err(MathError::InvalidInput));
    }

    #[test]
    fn record_and_lag() {
        let mut o = Observation::new(0, WAD, 1_000).unwrap();
        let prm = EmaParams::DEFAULT;
        o.record(1, 2 * WAD, 1_000).unwrap();
        // Same second: nothing to roll, the EMA does not see its own observation.
        assert_eq!(o.roll(1, &prm), Ok(None));
        assert_eq!(o.ema.price, WAD);
        o.record(1, 3 * WAD, 900).unwrap();
        assert_eq!((o.bucket.lo, o.bucket.hi), (2 * WAD, 3 * WAD));
        assert_eq!((o.bucket.llo, o.bucket.lhi), (900, 1_000));
        assert_eq!(o.roll(2, &prm), Ok(Some(2)));
        assert_eq!(o.roll(2, &prm), Ok(None));
        // obs = clamp(WAD, 2 WAD, 3 WAD) = 2 WAD; dt = 2 (capped); (5·1 + 2·2)/7 WAD.
        assert_eq!(o.ema.price, (5 * WAD + 4 * WAD) / 7);
        assert_eq!(o.ema.count, 1);
        assert_eq!(o.ring.len(), 2);
        let e = o.ring.newest().unwrap();
        assert_eq!((e.lo, e.hi, e.llo, e.lhi), (2 * WAD, 3 * WAD, 900, 1_000));
        assert_eq!((e.x, e.y), reprice(e.l, e.p).unwrap());
        assert_eq!(o.last.sec, 1);
        assert!(!o.bucket.is_open());
    }

    #[test]
    fn record_rejects_other_second_and_zero() {
        let mut b = Bucket::EMPTY;
        record(&mut b, 5, 1, 1).unwrap();
        assert_eq!(record(&mut b, 6, 1, 1), Err(MathError::InvalidInput));
        assert_eq!(record(&mut b, 4, 1, 1), Err(MathError::InvalidInput));
        assert_eq!(record(&mut b, 5, 0, 1), Err(MathError::InvalidInput));
        assert_eq!(record(&mut b, 5, 1, 0), Err(MathError::InvalidInput));
    }

    #[test]
    fn mixed_bucket_freezes_ema() {
        let mut o = Observation::new(0, 10 * WAD, 7).unwrap();
        o.record(1, 5 * WAD, 7).unwrap();
        o.record(1, 50 * WAD, 7).unwrap();
        o.roll(2, &EmaParams::DEFAULT).unwrap();
        assert_eq!(o.ema.price, 10 * WAD);
        // The frozen second keeps its honest extremes (v0.4.1).
        let e = o.ring.newest().unwrap();
        assert_eq!((e.lo, e.hi), (5 * WAD, 50 * WAD));
    }

    #[test]
    fn step_bounds_at_default_clamp() {
        // +16.7 % at dt = 1 and +28.6 % at dt = 2 (ECON §5.2).
        for (dt, num, den) in [(1i64, 7u128, 6u128), (2, 9, 7)] {
            let mut o = Observation::new(0, 6 * WAD, 7).unwrap();
            o.record(10, 1_000 * WAD, 7).unwrap();
            o.ema.ts = 11 - dt;
            assert_eq!(o.roll(11, &EmaParams::DEFAULT), Ok(Some(dt)));
            assert_eq!(o.ema.price, 6 * WAD * num / den);
        }
    }

    #[test]
    fn stale_ema_rolls_without_a_step() {
        let mut o = Observation::new(10, WAD, 7).unwrap();
        o.record(5, 2 * WAD, 7).unwrap();
        // ema_ts = 10 > now = 6: dt ≤ 0, only the bucket moves; the gap is negative.
        assert_eq!(o.roll(6, &EmaParams::DEFAULT), Ok(Some(-4)));
        assert_eq!((o.ema.price, o.ema.count, o.ring.len()), (WAD, 0, 1));
        assert_eq!((o.last.lo, o.last.sec), (2 * WAD, 5));
        // OWNER-M7-12 (2026-10-01): the threshold is 40 s (was 30).
        assert!(coverage_gap(41, 1) && !coverage_gap(40, 1) && !coverage_gap(100, 0));
        assert!(!coverage_gap(31, 1) && !coverage_gap(32, 1));
    }

    #[test]
    fn ring_wraps_newest_first() {
        let mut r = Ring::new(0, 1, 1).unwrap();
        for i in 1..=20u128 {
            r.push(RingEntry {
                p: i + 1,
                ts: i64::try_from(i).unwrap(),
                ..RingEntry::default()
            })
            .unwrap();
        }
        assert_eq!(r.len(), RING_LEN);
        let ts: std::vec::Vec<i64> = r.iter().map(|e| e.ts).collect();
        assert_eq!(ts, [20, 19, 18, 17, 16, 15, 14, 13]);
        let mut bad = r;
        bad.head = 8;
        assert_eq!(bad.check(), Err(MathError::InvalidInput));
        assert_eq!(sustained_growth(&bad, 3), Err(MathError::InvalidInput));
        assert_eq!(ring_medians(&bad), Err(MathError::InvalidInput));
        assert_eq!(ring_medians(&Ring::default()), Err(MathError::InvalidInput));
    }

    #[test]
    fn growth_and_medians() {
        let mut r = Ring::new(0, 100, 1).unwrap();
        for (ts, p) in [(1i64, 110u128), (2, 121), (4, 145)] {
            r.push(entry(ts, p, p, p, 1, 1)).unwrap();
        }
        // Steps: +1,000 bps/1 s, +1,000 bps/1 s, ⌊2,396/2⌋ = 991 bps/s: the lower median 1,000
        // (AIR-22; the minimum, 991, before).
        assert_eq!(sustained_growth(&r, 3).unwrap(), 1_000);
        assert_eq!(sustained_growth(&r, 4).unwrap(), 0);
        assert_eq!(sustained_growth(&r, 0).unwrap(), 0);
        // Lows 100, 110, 121, 145: lower median 110; highs alike: upper median 121.
        assert_eq!(ring_medians(&r).unwrap(), (110, 121, 1, 1));
        // Eight entries: 4th smallest low, 5th smallest high.
        let mut r = Ring::default();
        for i in 0..8u128 {
            let ts = i64::try_from(i).unwrap();
            r.push(entry(ts, 50, 10 + i, 20 + i, 30 - i, 40 - i))
                .unwrap();
        }
        assert_eq!(ring_medians(&r).unwrap(), (13, 24, 26, 37));
        // The liquidity medians alone (the reference guard; AIR-17's guarded spot floor).
        assert_eq!(ring_liq_lower_median(&r), Ok(26));
        assert_eq!(ring_liq_upper_median(&r), Ok(37));
        assert_eq!(
            ring_liq_upper_median(&Ring::default()),
            Err(MathError::InvalidInput)
        );
    }

    /// A genuine 1 %/s pump observed every second by the keeper; in every second `t` with
    /// `flat(t)` a second record at `P_ema` — the attacker's `[sell to P_ema, crank_price, buy
    /// back]` — makes the bucket straddle `P_ema`, which the directional aggregation turns into an
    /// exactly flat step. Returns the observation state after `secs` seconds and how many of the
    /// windows read after the first ten seconds had `sustained_growth(ring, 3) == 0`.
    fn pump_with_flat_steps(secs: i64, flat: impl Fn(i64) -> bool) -> (Observation, usize) {
        let prm = EmaParams::DEFAULT;
        let mut o = Observation::new(0, 100 * WAD, 1_000).unwrap();
        let mut spot = 100 * WAD;
        let mut zeroed = 0;
        for t in 1..secs {
            spot = spot * 101 / 100;
            o.record(t, spot, 1_000).unwrap();
            if flat(t) {
                let p_ema = o.ema.price;
                o.record(t, p_ema, 1_000).unwrap();
            }
            o.roll(t + 1, &prm).unwrap();
            if t > 10 {
                zeroed += usize::from(sustained_growth(&o.ring, 3).unwrap() == 0);
            }
        }
        (o, zeroed)
    }

    /// AIR-22: one flat step in every three no longer zeroes the sustained growth — `g` is the
    /// lower median of the newest `n` step rates, not their minimum, so `κ_eff` and with it the
    /// liquidation reference keep following a genuine pump; a majority of flat steps (a genuine
    /// stall, or a pump that stopped) still gives 0.
    #[test]
    fn one_flat_step_in_three_keeps_the_sustained_growth() {
        let (honest, zeroed) = pump_with_flat_steps(60, |_| false);
        assert_eq!(zeroed, 0);
        let g_honest = sustained_growth(&honest.ring, 3).unwrap();
        assert!(g_honest > 50, "{g_honest}");
        let (o, zeroed) = pump_with_flat_steps(60, |t| t % 3 == 0);
        assert_eq!(
            zeroed, 0,
            "a flat step every 3 s zeroed g in {zeroed} windows"
        );
        let g = sustained_growth(&o.ring, 3).unwrap();
        assert!(g > 50, "{g}");
        // The liquidation reference opens up with the growth (κ_eff = 6·g > 0): above the
        // `min(P_ema, lb_lo)` a zero `g` leaves.
        let prm = crate::bounds::ExitParams::DEFAULT;
        let now = o.last.sec + 1;
        let p_ref = crate::bounds::liq_reference(&o.ema, &o.ring, &o.last, now, &prm)
            .unwrap()
            .unwrap();
        assert!(p_ref > o.ema.price.min(o.last.lo), "{p_ref}");
        // Two flat steps in every three still zero it (a majority of the window), as does a pump
        // that stopped: the newest two steps flat (the keeper's records at the frozen P_ema).
        let (_, zeroed) = pump_with_flat_steps(60, |t| t % 3 != 0);
        assert_eq!(zeroed, 49);
        let mut stalled = o;
        for t in 60..62i64 {
            let p_ema = stalled.ema.price;
            stalled.record(t, p_ema, 1_000).unwrap();
            stalled.roll(t + 1, &EmaParams::DEFAULT).unwrap();
        }
        assert_eq!(sustained_growth(&stalled.ring, 3).unwrap(), 0);
        // A genuine stall: a flat market.
        let mut flat = Observation::new(0, 100 * WAD, 1_000).unwrap();
        for t in 1..20i64 {
            flat.record(t, 100 * WAD, 1_000).unwrap();
            flat.roll(t + 1, &EmaParams::DEFAULT).unwrap();
        }
        assert_eq!(sustained_growth(&flat.ring, 3).unwrap(), 0);
    }

    #[test]
    fn rewrite_liquidity_resets_every_entry() {
        let setup = || {
            let mut o = Observation::new(0, WAD, 120).unwrap();
            o.ema.liq = 100;
            o.ema.liq_slow = 50;
            o.ring
                .push(entry(1, 2 * WAD, WAD, 3 * WAD, 90, 130))
                .unwrap();
            o.ring
                .push(entry(2, 3 * WAD, 2 * WAD, 4 * WAD, 40, 45))
                .unwrap();
            o.record(5, WAD, 90).unwrap();
            o
        };
        let mut o = setup();
        o.rewrite_liquidity(200, VenueChange::Raydium).unwrap();
        assert_eq!((o.ema.price, o.ema.liq, o.ema.liq_slow), (WAD, 100, 50));
        assert!(
            o.ring
                .iter()
                .all(|e| (e.l, e.llo, e.lhi) == (100, 100, 100))
        );
        assert!(
            o.ring
                .iter()
                .all(|e| (e.x, e.y) == reprice(100, e.p).unwrap())
        );
        let ps: std::vec::Vec<_> = o.ring.iter().map(|e| (e.ts, e.p, e.lo, e.hi)).collect();
        assert_eq!(
            ps,
            [
                (2, 3 * WAD, 2 * WAD, 4 * WAD),
                (1, 2 * WAD, WAD, 3 * WAD),
                (0, WAD, WAD, WAD)
            ]
        );
        assert_eq!(o.bucket, Bucket::EMPTY);

        let mut o = setup();
        o.rewrite_liquidity(70, VenueChange::Raydium).unwrap();
        assert_eq!((o.ema.liq, o.ema.liq_slow), (70, 50));
        assert!(
            o.ring
                .iter()
                .all(|e| e.l == 70 && e.llo == 70 && e.lhi == 70)
        );

        let mut o = setup();
        o.rewrite_liquidity(200, VenueChange::Graduation).unwrap();
        assert_eq!((o.ema.liq, o.ema.liq_slow), (200, 200));
        assert!(
            o.ring
                .iter()
                .all(|e| e.l == 200 && e.llo == 200 && e.lhi == 200)
        );
        let before = o;
        assert_eq!(
            o.rewrite_liquidity(0, VenueChange::Graduation),
            Err(MathError::InvalidInput)
        );
        assert_eq!(o, before);

        // M4-SEC-5 / M4-SEC-6: a deeper pool's liquidity is taken as given (no `min` with the
        // old `L_ema`), `L_slow` only ever falls, to at most the pool's locked depth.
        let mut o = setup();
        o.rewrite_liquidity(200, VenueChange::RaydiumLocked { slow_cap: 180 })
            .unwrap();
        assert_eq!((o.ema.liq, o.ema.liq_slow), (200, 50));
        assert!(
            o.ring.iter().all(|e| (e.l, e.llo, e.lhi) == (200, 200, 200)
                && (e.x, e.y) == reprice(200, e.p).unwrap())
        );
        assert_eq!(o.bucket, Bucket::EMPTY);
        let mut o = setup();
        o.rewrite_liquidity(70, VenueChange::RaydiumLocked { slow_cap: 35 })
            .unwrap();
        assert_eq!((o.ema.liq, o.ema.liq_slow), (70, 35));
        assert!(o.ring.iter().all(|e| (e.l, e.llo, e.lhi) == (70, 70, 70)));
        let mut o = setup();
        let before = o;
        assert_eq!(
            o.rewrite_liquidity(70, VenueChange::RaydiumLocked { slow_cap: 0 }),
            Err(MathError::InvalidInput)
        );
        assert_eq!(o, before);
    }

    /// A priced market after a Raydium adoption (M4R): the ring rewritten to the anchor `cap`,
    /// `L_slow` to at most `D`, marked; ring prices from before the change.
    fn adopted(cap: u128, d: u128) -> Observation {
        let mut o = Observation::new(0, 4 * WAD, 5_000).unwrap();
        for (t, p) in [(1i64, 5 * WAD), (2, 6 * WAD), (3, 3 * WAD)] {
            o.record(t, p, 5_000).unwrap();
            o.roll(t + 1, &EmaParams::DEFAULT).unwrap();
        }
        o.rewrite_liquidity(cap, VenueChange::RaydiumLocked { slow_cap: d })
            .unwrap();
        o.mark_restart();
        o
    }

    /// The v0.4.1 price step, recomputed from ECON §5.2 alone (τ = 5).
    fn v041(p: u128, lo: u128, hi: u128, dt: u128, c_bps: u128) -> u128 {
        let obs = p.max(lo).min(hi);
        let obs = obs
            .max(p * 10_000 / (10_000 + c_bps))
            .min(p * (10_000 + c_bps) / 10_000);
        (p * 5 + obs * dt) / (5 + dt)
    }

    /// The largest v0.4.1 step (the observation at `+C`), recomputed.
    fn up(p: u128, dt: u128, c_bps: u128) -> u128 {
        (p * 5 + p * (10_000 + c_bps) / 10_000 * dt) / (5 + dt)
    }

    /// The largest v0.4.1 fall (the observation at the lower clamp), recomputed (M6-SEC-1).
    fn down(p: u128, dt: u128, c_bps: u128) -> u128 {
        (p * 5 + p * 10_000 / (10_000 + c_bps) * dt) / (5 + dt)
    }

    /// The §4.1 seed over the post-change buckets `(lo, lhi)` at C = `c_bps`, the price clamped
    /// to `[lower, bound]`: the largest v0.4.1 fall and step from the current `P_ema` (M6-SEC-1,
    /// M4X-1).
    fn seed(buckets: &[(u128, u128)], c_bps: u128, lower: u128, bound: u128) -> (u128, u128) {
        let p = buckets
            .iter()
            .map(|b| b.0)
            .min()
            .unwrap()
            .min(bound)
            .max(lower);
        let mut h: std::vec::Vec<u128> = buckets.iter().map(|b| b.1).collect();
        h.sort_unstable();
        let lomed = h[(h.len() - 1) / 2];
        (
            p,
            (*h.last().unwrap()).min(lomed * (10_000 + c_bps) / 10_000),
        )
    }

    #[test]
    fn mark_restart_and_new_raydium() {
        let mut e = Ema::new(3, WAD, 7).unwrap();
        assert!(warmed(&e) && e.guard == 0);
        mark_restart(&mut e);
        assert_eq!((e.warm, e.guard), (EMA_WARMUP_BUCKETS, 1));
        assert!(!warmed(&e));
        // A price start: L = min(sample, anchor), L_slow = min(L, D) (AIR-13), marked; the mock /
        // pump start is unmarked.
        let o = Observation::new_raydium(9, 2 * WAD, 1_000, 700, 900).unwrap();
        assert_eq!(
            (o.ema.liq, o.ema.liq_slow, o.ema.warm, o.ema.guard),
            (700, 700, 8, 1)
        );
        let e0 = *o.ring.newest().unwrap();
        assert_eq!(
            (e0.l, e0.llo, e0.lhi, e0.lo, e0.hi),
            (700, 700, 700, 2 * WAD, 2 * WAD)
        );
        let o = Observation::new_raydium(9, 2 * WAD, 600, 700, 900).unwrap();
        assert_eq!((o.ema.liq, o.ema.liq_slow), (600, 600));
        // A deposit-wrapped sample over a fully locked pool (2D, the anchor at λ_min = 50 %):
        // L_ema and the ring take it, L_slow the locked depth.
        let o = Observation::new_raydium(9, 2 * WAD, 1_000, 1_000, 500).unwrap();
        assert_eq!((o.ema.liq, o.ema.liq_slow), (1_000, 500));
        assert_eq!(o.ring.newest().unwrap().l, 1_000);
        assert_eq!(
            Observation::new_raydium(9, 2 * WAD, 600, 0, 900),
            Err(MathError::InvalidInput)
        );
        assert_eq!(
            Observation::new_raydium(9, 2 * WAD, 600, 700, 0),
            Err(MathError::InvalidInput)
        );
        let o = Observation::new(9, 2 * WAD, 600).unwrap();
        assert_eq!((o.ema.warm, o.ema.guard), (0, 0));
    }

    /// The warm-up seed for n = 1..8 post-change buckets: `P_ema = max(min(min lo, the largest
    /// v0.4.1 step), the largest v0.4.1 fall)` (M4X-1, M6-SEC-1), `L_ema = min(max lhi,
    /// ⌊lomed(lhi)·(10⁴ + C)/10⁴⌋)`, no blend; `warm` and `count` step, `L_slow` and `slow_ts`
    /// stay; the ring's `(p, l, x, y)` of the new entry is the seed.
    #[test]
    fn warm_up_seeds_from_the_post_change_buckets() {
        let buckets: [(u128, u128, u128, u128); 8] = [
            (5 * WAD, 6 * WAD, 900, 1_000),
            (4 * WAD, 7 * WAD, 400, 1_100),
            (6 * WAD, 6 * WAD, 1_000, 1_000),
            (5 * WAD, 5 * WAD, 1_000, 9_000),
            (7 * WAD, 8 * WAD, 950, 1_050),
            (5 * WAD, 5 * WAD, 990, 990),
            (3 * WAD, 9 * WAD, 1_000, 20_000),
            (5 * WAD, 5 * WAD, 1_000, 30_000),
        ];
        let prm = EmaParams::new(2, 7_000, 3_600, 60);
        let mut o = adopted(2_000, 800);
        let (slow, slow_ts, count0) = (o.ema.liq_slow, o.ema.slow_ts, o.ema.count);
        assert_eq!(slow, 800);
        let mut seen = std::vec::Vec::new();
        let (mut bound, mut floor) = (0, 0);
        for (i, &(lo, hi, llo, lhi)) in buckets.iter().enumerate() {
            let t = 10 + i64::try_from(i).unwrap();
            o.record(t, lo, llo).unwrap();
            o.record(t, hi, lhi).unwrap();
            let (ts0, p0) = (o.ema.ts, o.ema.price);
            assert_eq!(o.roll(t + 1, &prm).unwrap(), Some(t + 1 - ts0));
            seen.push((lo, lhi));
            let dt = u128::try_from(t + 1 - ts0).unwrap().min(2);
            let (lower, cap) = (down(p0, dt, 7_000), up(p0, dt, 7_000));
            let low = seen.iter().map(|b| b.0).min().unwrap();
            bound += usize::from(cap < low);
            floor += usize::from(lower > low);
            let (p, l) = seed(&seen, 7_000, lower, cap);
            assert_eq!((o.ema.price, o.ema.liq), (p, l), "n = {}", i + 1);
            assert_eq!(o.ema.warm, 7 - u8::try_from(i).unwrap());
            assert_eq!(o.ema.count, count0 + 1 + u64::try_from(i).unwrap());
            assert_eq!(
                (o.ema.liq_slow, o.ema.slow_ts, o.ema.ts),
                (slow, slow_ts, t + 1)
            );
            let e = *o.ring.newest().unwrap();
            assert_eq!((e.ts, e.p, e.l), (t + 1, p, l));
            assert_eq!((e.lo, e.hi, e.llo, e.lhi), (lo, hi, llo, lhi));
            assert_eq!((e.x, e.y), reprice(l, p).unwrap());
        }
        // Checks of the table: n = 4 reads the 9,000 high only up to ⌊1,000·1.7⌋; the 400 low
        // (a withdrawal) is never read; at n = 8 five highs are above 1,100. The up-bound is never
        // reached here; the fall bound (M6-SEC-1) holds the seed above the 4·WAD and 3·WAD lows
        // (≈ ×0.93 per second at C = 70 %).
        assert_eq!(seed(&seen[..4], 7_000, 0, u128::MAX), (4 * WAD, 1_700));
        assert_eq!(o.ema.liq, 1_050 * 17_000 / 10_000);
        assert_eq!(bound, 0);
        assert!(floor > 0);
        assert!(o.ema.price > 3 * WAD);
        // After the warm-up the v0.4.1 step resumes (a blend from the seed).
        let before = o.ema;
        o.record(18, 5 * WAD, 1_000).unwrap();
        o.roll(19, &prm).unwrap();
        assert_eq!(o.ema.warm, 0);
        assert!(o.ema.price > before.price && o.ema.price < 5 * WAD);
        assert_eq!(o.ema.slow_ts, 19);
    }

    #[test]
    fn warm_up_edges() {
        // dt ≤ 0: no step, no count down (the sim's re-initialised EMA).
        let mut o = adopted(2_000, 800);
        o.ema.ts = 50;
        o.record(20, WAD, 1_000).unwrap();
        assert_eq!(o.roll(21, &EmaParams::DEFAULT), Ok(Some(-29)));
        assert_eq!((o.ema.warm, o.ema.guard, o.ema.liq), (8, 1, 2_000));
        // A `warm` above EMA_WARMUP_BUCKETS (never written) or a corrupted ring: nothing moves.
        for corrupt in [0u8, 1] {
            let mut o = adopted(2_000, 800);
            if corrupt == 0 {
                o.ema.warm = EMA_WARMUP_BUCKETS + 1;
            } else {
                o.ring.head = 9;
            }
            o.record(20, WAD, 1_000).unwrap();
            let before = o;
            assert_eq!(
                o.roll(21, &EmaParams::DEFAULT),
                Err(MathError::InvalidInput)
            );
            assert_eq!(o, before);
        }
        // A seed past u128 on the up-bound: `max lhi` binds.
        let mut o = adopted(2_000, 800);
        o.record(20, WAD, u128::MAX).unwrap();
        o.roll(21, &EmaParams::DEFAULT).unwrap();
        assert_eq!(o.ema.liq, u128::MAX);
    }

    /// M4X-1: the warm-up price seed rises per integration by at most the largest v0.4.1 step.
    /// Eight attacker-only buckets pumped 2.5× (the keeper absent): the seed climbs ×9/7 (the
    /// first roll, `dt = 2`) then ×7/6 until the pump, where the old seed read 2.5× at once; the
    /// v0.4.1 EMA under the attacker's best steady-state pumps (each at `+C` of its `P_ema`)
    /// reaches at least as high at every step. M6-SEC-1: a lower bucket pulls the seed down by at
    /// most the largest v0.4.1 fall per integration (as built it fell to the low at once), and the
    /// running minimum keeps pulling it until the gate.
    #[test]
    fn warm_up_price_seed_rises_no_faster_than_the_v041_ema() {
        let prm = EmaParams::DEFAULT;
        let mut o = Observation::new(0, WAD, 1_000).unwrap();
        o.rewrite_liquidity(2_000, VenueChange::RaydiumLocked { slow_cap: 900 })
            .unwrap();
        let mut steady = o;
        o.mark_restart();
        let mut want = WAD;
        for t in 1..9i64 {
            let dt = if t == 1 { 2 } else { 1 };
            o.record(t, 5 * WAD / 2, 1_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
            want = up(want, dt, 10_000).min(5 * WAD / 2);
            assert_eq!(o.ema.price, want, "t = {t}");
            assert_eq!(o.ring.newest().unwrap().p, want);
            let best = steady.ema.price * 2;
            steady.record(t, best, 1_000).unwrap();
            steady.roll(t + 1, &prm).unwrap();
            assert!(o.ema.price <= steady.ema.price, "t = {t}");
        }
        assert_eq!(o.ema.warm, 0);
        // 9/7·(7/6)⁴ ≈ 2.38 < 2.5 ≤ 9/7·(7/6)⁵ ≈ 2.78: the pump binds from the 6th integration.
        let mut p = up(WAD, 2, 10_000);
        for _ in 0..4 {
            p = up(p, 1, 10_000);
        }
        assert!(p < 5 * WAD / 2 && up(p, 1, 10_000) >= 5 * WAD / 2);
        assert_eq!(o.ema.price, 5 * WAD / 2);
        // A dump in the warm-up (M6-SEC-1): the seed falls by at most the largest v0.4.1 step
        // (×6/7 on the first roll, `dt = 2`) and then, the running minimum still reading the dumped
        // bucket, by ×11/12 a second until it reaches the low; after the gate the v0.4.1 step.
        let mut o = Observation::new(0, 4 * WAD, 1_000).unwrap();
        o.rewrite_liquidity(2_000, VenueChange::RaydiumLocked { slow_cap: 900 })
            .unwrap();
        o.mark_restart();
        o.record(1, WAD, 1_000).unwrap();
        o.roll(2, &prm).unwrap();
        let mut want = down(4 * WAD, 2, 10_000);
        assert_eq!(o.ema.price, want);
        assert_eq!(want, 4 * WAD * 6 / 7);
        for t in 2..9i64 {
            o.record(t, 3 * WAD, 1_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
            want = down(want, 1, 10_000).max(WAD);
            assert_eq!(o.ema.price, want, "t = {t}");
        }
        assert_eq!(o.ema.warm, 0);
        // 4·(6/7)·(11/12)⁷ ≈ 1.87 > 1: the seed never reached the dumped low.
        assert!(o.ema.price > WAD);
        let p8 = o.ema.price;
        o.record(9, 3 * WAD, 1_000).unwrap();
        o.roll(10, &prm).unwrap();
        assert_eq!(o.ema.price, v041(p8, 3 * WAD, 3 * WAD, 1, 10_000));
        assert_eq!(price_up_bound(WAD, 1, 10_000), Ok(WAD * 7 / 6));
        assert_eq!(price_up_bound(7 * WAD, 2, 10_000), Ok(9 * WAD));
        assert_eq!(price_down_bound(12 * WAD, 1, 10_000), Ok(11 * WAD));
        assert_eq!(price_down_bound(7 * WAD, 2, 10_000), Ok(6 * WAD));
    }

    /// M6-SEC-1 (the review's reproducer, on the math): one warm-up second whose bucket holds a low
    /// far below the pool (a pool pushed down and restored inside one transaction) among honest
    /// buckets at the pool's price. As built the seed took the low and kept it for the rest of the
    /// warm-up (the running minimum), and after the gate climbed back by at most ×7/6 a second;
    /// now it falls by one steady step and ends the warm-up within one fall step of the honest
    /// seed.
    #[test]
    fn a_recorded_low_in_the_warm_up_moves_the_seed_by_one_steady_step() {
        let prm = EmaParams::DEFAULT;
        let run = |low_at: Option<i64>| -> Observation {
            let mut o = Observation::new(0, 2 * WAD, 1_000).unwrap();
            o.rewrite_liquidity(2_000, VenueChange::RaydiumLocked { slow_cap: 900 })
                .unwrap();
            o.mark_restart();
            for t in 1..9i64 {
                o.record(t, 2 * WAD, 1_000).unwrap();
                if low_at == Some(t) {
                    o.record(t, 2 * WAD / 4, 1_000).unwrap();
                }
                o.roll(t + 1, &prm).unwrap();
            }
            o
        };
        let honest = run(None);
        assert_eq!((honest.ema.price, honest.ema.warm), (2 * WAD, 0));
        for at in 1..9i64 {
            let pushed = run(Some(at));
            assert_eq!(pushed.ema.warm, 0);
            // The low bucket stays in the running minimum until the gate, so the seed falls one
            // step a second from it on, never to the low itself.
            let dt = if at == 1 { 2 } else { 1 };
            let mut want = down(2 * WAD, dt, 10_000);
            for _ in at + 1..9 {
                want = down(want, 1, 10_000);
            }
            assert_eq!(pushed.ema.price, want.max(2 * WAD / 4), "low at {at}");
            assert!(pushed.ema.price > 2 * WAD / 4, "low at {at}");
        }
        // The worst case (the low in the first warm-up second): ×(6/7)·(11/12)⁷ ≈ 0.47 of the
        // pool's price, where the as-built seed read 0.25.
        let worst = run(Some(1)).ema.price;
        assert!(worst * 100 > 2 * WAD * 46 && worst * 100 < 2 * WAD * 48);
    }

    /// M4X-2: while the guard is set, `L_slow` integrates `min(lobs, L_mlo)`: a deposit freeze
    /// (every bucket's high pushed, the lows honest) holds `L_ema` up but never lifts `L_slow`
    /// above the honest lows; once the guard clears it follows `lobs` as in v0.4.1.
    #[test]
    fn the_guard_keeps_l_slow_at_the_honest_lows() {
        let prm = EmaParams::DEFAULT;
        // Pool-sized liquidities (the per-second increment of L_slow is ≈ 4·10⁵ here).
        const U: u128 = 1_000_000_000;
        let mut o = Observation::new(0, 4 * WAD, 5_000 * U).unwrap();
        for t in 1..4i64 {
            o.record(t, 4 * WAD, 5_000 * U).unwrap();
            o.roll(t + 1, &prm).unwrap();
        }
        o.rewrite_liquidity(
            4_000 * U,
            VenueChange::RaydiumLocked {
                slow_cap: 1_500 * U,
            },
        )
        .unwrap();
        o.mark_restart();
        assert_eq!(o.ema.liq_slow, 1_500 * U);
        let mut unguarded = None;
        for t in 20..1_220i64 {
            o.record(t, 2 * WAD, 3_000 * U).unwrap();
            o.record(t, 2 * WAD, 6_000 * U).unwrap();
            let pre = o.ema;
            o.roll(t + 1, &prm).unwrap();
            if pre.warm == 0 {
                assert_eq!(o.ema.guard, 1);
                assert!(o.ema.liq > 3_000 * U * 101 / 100);
                assert!(
                    o.ema.liq_slow <= 3_000 * U,
                    "t = {t}: L_slow {}",
                    o.ema.liq_slow
                );
                if unguarded.is_none() {
                    // The same step without the guard: L_slow would integrate the frozen L_ema.
                    let mut u = pre;
                    u.guard = 0;
                    let mut r = o.ring;
                    let mut b = Bucket::EMPTY;
                    let mut l = LastBucket::NONE;
                    record(&mut b, t, 2 * WAD, 3_000 * U).unwrap();
                    record(&mut b, t, 2 * WAD, 6_000 * U).unwrap();
                    integrate(&mut u, &mut r, &mut b, &mut l, t + 1, &prm).unwrap();
                    unguarded = Some((pre.liq_slow, u.liq_slow, o.ema.liq_slow));
                }
            }
        }
        let (before, without, with) = unguarded.unwrap();
        assert!(without > with && with > before, "{before} {without} {with}");
        // Twenty minutes of freeze (τ_slow = 1 h): L_slow has moved from D towards the lows,
        // 1,500 + 1,500·(1 − e^(−1/3)) ≈ 1,925, never towards L_ema.
        assert!(
            o.ema.liq_slow > 1_900 * U && o.ema.liq_slow < 1_950 * U,
            "{}",
            o.ema.liq_slow
        );
        // The freeze stops: the guard clears and L_slow resumes lobs.
        for t in 1_220..1_400i64 {
            o.record(t, 2 * WAD, 3_000 * U).unwrap();
            o.roll(t + 1, &prm).unwrap();
        }
        assert_eq!(o.ema.guard, 0);
        assert!(o.ema.liq_slow <= 3_000 * U);
    }

    /// The reference guard: set by a restart only, cleared at the gate on an honest ring, kept
    /// while `L_ema > 1.01·L_mlo`, and never set by `integrate`.
    #[test]
    fn reference_guard() {
        let prm = EmaParams::DEFAULT;
        // Honest: every bucket (H, H): cleared by the 8th warm-up integration.
        let mut o = adopted(4_000, 1_500);
        for t in 20..28i64 {
            o.record(t, 2 * WAD, 3_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
            let gate = t == 27;
            assert_eq!(
                (o.ema.warm == 0, o.ema.guard),
                (gate, u8::from(!gate)),
                "t = {t}"
            );
        }
        assert_eq!(
            (o.ema.liq, ring_liq_lower_median(&o.ring).unwrap()),
            (3_000, 3_000)
        );
        // Deposit-wrapped highs in every bucket, honest lows: L_ema = min(max lhi, lomed·2) far
        // above L_mlo: the guard stays until the lows catch up with L_ema.
        let mut o = adopted(4_000, 1_500);
        for t in 20..40i64 {
            o.record(t, 2 * WAD, 3_000).unwrap();
            o.record(t, 2 * WAD, 6_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
        }
        assert_eq!((o.ema.warm, o.ema.guard), (0, 1));
        assert!(o.ema.liq * 10_000 > ring_liq_lower_median(&o.ring).unwrap() * 10_100);
        // Once the pool is left alone the EMA falls to the lows and the guard clears at 1 %.
        let mut cleared = None;
        for t in 40..200i64 {
            o.record(t, 2 * WAD, 3_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
            if o.ema.guard == 0 {
                cleared = Some(t);
                break;
            }
        }
        assert!(cleared.is_some());
        assert!(o.ema.liq * 10_000 <= 3_000 * 10_100);
        // A confirmed state far above its lows never sets the guard.
        let mut o = Observation::new(0, WAD, 10_000).unwrap();
        for t in 1..20i64 {
            o.record(t, WAD, 1).unwrap();
            o.record(t, WAD, 10_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
            assert_eq!((o.ema.warm, o.ema.guard), (0, 0));
        }
        assert_eq!(
            ring_liq_lower_median(&o.ring).unwrap(),
            ring_medians(&o.ring).unwrap().2
        );
    }

    #[test]
    fn robust_spot_freshness() {
        let last = LastBucket {
            lo: 5,
            hi: 9,
            sec: 10,
            _pad: [0; 8],
        };
        assert_eq!(robust_spot(7, &last, 12, 2), Some(5));
        assert_eq!(robust_spot(3, &last, 12, 2), Some(3));
        assert_eq!(robust_spot(7, &last, 13, 2), None);
        assert_eq!(robust_spot(7, &LastBucket::NONE, 0, 2), None);
    }

    #[test]
    fn params_bounds() {
        assert_eq!(EmaParams::DEFAULT.validate(), Ok(()));
        for bad in [
            EmaParams::new(0, 10_000, 3_600, 60),
            EmaParams::new(3, 10_000, 3_600, 60),
            EmaParams::new(2, 4_999, 3_600, 60),
            EmaParams::new(2, 10_001, 3_600, 60),
            EmaParams::new(2, 10_000, 599, 60),
            EmaParams::new(2, 10_000, 86_401, 60),
            EmaParams::new(2, 10_000, 3_600, 0),
            EmaParams::new(2, 10_000, 3_600, 601),
        ] {
            assert_eq!(bad.validate(), Err(MathError::InvalidInput));
        }
        assert_eq!(EmaParams::new(1, 5_000, 600, 1).validate(), Ok(()));
    }

    #[test]
    fn huge_prices_do_not_overflow() {
        let p = u128::MAX / 3;
        let mut o = Observation::new(0, p, u128::MAX).unwrap();
        o.record(1, u128::MAX, u128::MAX).unwrap();
        o.roll(2, &EmaParams::DEFAULT).unwrap();
        assert!(o.ema.price > p);
        assert_eq!(o.ema.liq, u128::MAX);
        // No reprojection past u128: the entry caches (0, 0).
        let e = o.ring.newest().unwrap();
        assert_eq!((e.x, e.y), (0, 0));
    }
}
