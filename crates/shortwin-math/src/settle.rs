//! Owner: oracle track. SOL settlement on the settlement box: the box, the S1/S2 legs,
//! the refutation, "not liquidatable", FV's `p'` per evaluated corner, the amount check,
//! the S3 timer and the one arming clock for S1 and S2 (ECON §5.6, §8.6, §10.3; ARCH §3.8,
//! §4.5, §5.3; A65). Normative reference: `Market.settle_box`, `_s1_at`, `_s2_at`,
//! `_legs`, `_refuted`, `_not_liq_box`, `fv_parts`, `_s3_due`, `_settlement`, `_settle`
//! and the non-liquidatable branch of `try_exit(settle_only = True)` in
//! `sim/shortwin_sim.py` v0.4.1 (default switches: `box_obs = False`,
//! `pprime_raw = False`, `fv_cap = True`).
//!
//! **One rule.** A settlement decision may rest on EMA evidence only if manipulated
//! seconds could not have produced it, so every EMA-based decision must come out the same
//! on the whole box spanned by `(P_ema, L_ema)` and the ring medians of the bucket
//! extremes ([`box_of_capped`]). `B̂` and σ increase with `P` and decrease with `L`, so "the same
//! everywhere" is the corner least favourable to the decision: the S1/S2 legs at
//! `(P_lo, L_hi)`, the refutation and "not liquidatable" at `(P_hi, L_lo)`, and an S2/S3
//! amount must agree within `fv_dev_bps` between those two corners, each with its own
//! `p'`.
//!
//! This module never quotes: the caller supplies the position's risk quotes on repriced
//! states through [`BoxQuotes`] (the quote module at `φ_risk = min(φ_live,
//! venue_fee_hard_cap_bps_pos)`; ARCH §4.3) and the live-state legs as values.

use crate::consts::RING_LEN;
use crate::error::MathError;
use crate::num::BPS;
use crate::observe::{Ema, Ring, ring_medians};
use crate::risk::{Budget, SettleKind, cr_below, fair_value};

/// The settlement box `[P_lo, P_hi] × [L_lo, L_hi]` (ECON §5.6; ARCH §3.8, §4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettleBox {
    /// `min(P_ema, P_mlo)`.
    pub p_lo: u128,
    /// `max(P_ema, P_mhi)`.
    pub p_hi: u128,
    /// `min(L_ema, L_mlo)`.
    pub l_lo: u128,
    /// `max(L_ema, min(L_mhi, cap))`, `cap` the market's depth cap if it has one (AIR-15).
    pub l_hi: u128,
}

impl SettleBox {
    /// The box reduced to the point `(p, l)`.
    pub const fn point(p: u128, l: u128) -> Self {
        Self {
            p_lo: p,
            p_hi: p,
            l_lo: l,
            l_hi: l,
        }
    }

    /// Whether the box is a single point.
    pub const fn is_point(&self) -> bool {
        self.p_lo == self.p_hi && self.l_lo == self.l_hi
    }

    /// Whether `(p, l)` lies in the box.
    pub const fn contains(&self, p: u128, l: u128) -> bool {
        self.p_lo <= p && p <= self.p_hi && self.l_lo <= l && l <= self.l_hi
    }
}

/// The settlement box of a market without a locked depth (the curve, the mock):
/// [`box_of_capped`] with no depth cap.
///
/// Errors: as [`box_of_capped`].
pub fn box_of(ema: &Ema, ring: &Ring, now: i64, executable: bool) -> Result<SettleBox, MathError> {
    box_of_capped(ema, ring, now, executable, None)
}

/// The settlement box of a market (ECON §5.6; ARCH §3.8; sim `Market.settle_box`):
///
/// ```text
/// (P_mlo, P_mhi, L_mlo, L_mhi) = ring_medians(ring)
/// P_lo = min(P_ema, P_mlo), P_hi = max(P_ema, P_mhi), L_lo = min(L_ema, L_mlo)
/// L_hi = max(L_ema, min(L_mhi, cap))                cap = depth_cap, or no cap for None
/// ```
///
/// or the EMA point `(P_ema, L_ema)` alone if `now − ema_ts > RING_LEN` seconds **and the
/// venue cannot execute now** (`!executable`, this instruction's `venue_executable`): a venue
/// that records no observations cannot have been pushed since, and a halted venue's S3 must
/// not be blocked. On an executable venue the medians always apply (M2-SEC-1): a stale
/// integration proves nothing there, because a lender instruction's roll (ARCH §3.0 A16)
/// integrates a pushed bucket without recording, so one attacker-only second followed by
/// `RING_LEN + 1` quiet seconds would otherwise collapse the box onto the pushed EMA. `ema`
/// and `ring` are the post-roll state (ARCH §3.0).
///
/// `depth_cap` (AIR-15) is the market's [`crate::bounds::ref_depth_cap`], `Some(D)` — the locked
/// depth, `⌊D·10⁴/λ⌋` at `λ` = 100 % since AIR-14 — on a Raydium market and a canonical PumpSwap
/// pool, at which [`crate::bounds::ref_liquidity`] caps the exit reference's EMA terms (M4F-1),
/// and `None` on the other venues. A deposit wrapped around any record
/// raises that second's liquidity high, which honest records in the same second cannot lower,
/// so deposit-wrapped cranks in 4 of the 8 seconds would otherwise set `L_mhi` at will, widen
/// the box and defer the S1/S2 legs and the S2/S3 amount check for no swap fee. `L_ema` is
/// never capped (the box keeps the EMA point), so `L_hi` never falls below `L_ema` or below
/// `min(L_mhi, cap)`, which only attacker-only seconds lower and no capital-only bundle moves
/// (`D` is deposit- and withdraw-invariant). Depth above the cap — liquidity beyond the locked
/// depth (a PumpSwap pool's unburned LP, a Raydium pool's unlocked LP) — is guarded by `L_ema`
/// alone, as in the exit reference.
///
/// Errors: `InvalidInput` for a corrupted or empty ring.
pub fn box_of_capped(
    ema: &Ema,
    ring: &Ring,
    now: i64,
    executable: bool,
    depth_cap: Option<u128>,
) -> Result<SettleBox, MathError> {
    let age = i128::from(now).saturating_sub(i128::from(ema.ts));
    let ring_len = i128::try_from(RING_LEN).map_err(|_| MathError::Overflow)?;
    if !executable && age > ring_len {
        return Ok(SettleBox::point(ema.price, ema.liq));
    }
    let (p_mlo, p_mhi, l_mlo, l_mhi) = ring_medians(ring)?;
    let l_mhi = depth_cap.map_or(l_mhi, |cap| l_mhi.min(cap));
    Ok(SettleBox {
        p_lo: ema.price.min(p_mlo),
        p_hi: ema.price.max(p_mhi),
        l_lo: ema.liq.min(l_mlo),
        l_hi: ema.liq.max(l_mhi),
    })
}

/// The position's quotes on repriced states `reprice(L, P)` that the box predicates read
/// (ARCH §4.5). Every method must be a pure function of `(P, L)` for the position under
/// evaluation (the predicates may evaluate them in any order, and short-circuit).
pub trait BoxQuotes {
    /// Risk quote `B̂(Q; reprice(L, P))`: the total buyback cost in lamports at
    /// `φ_risk = min(φ_live, venue_fee_hard_cap_bps_pos)` (the sims' `quote_repriced`);
    /// `None` if undefined (no reprojection, `Q ≥ X`, …), which every predicate reads as
    /// `+∞`.
    fn cost(&mut self, p: u128, l: u128) -> Result<Option<u64>, MathError>;

    /// SPEC slippage `σ(Q; reprice(L, P))` in bps: the fee-free premium of the quote over
    /// the marginal price of the repriced state ([`crate::risk::sigma_xy`]; the sims'
    /// `slip_repriced`); `u64::MAX` if undefined.
    fn sigma(&mut self, p: u128, l: u128) -> Result<u64, MathError>;

    /// `B̂_FV(P, L)` of the fair value: [`BoxQuotes::cost`] at `(P, L)`, except on a stuck
    /// migration (S3 on a completed curve), where it prices the deterministic
    /// post-migration pool repriced to `P` (ECON §8.6; sim `fv_parts`).
    fn fv_cost(&mut self, p: u128, l: u128) -> Result<Option<u64>, MathError> {
        self.cost(p, l)
    }
}

/// All S1 legs hold (ECON §5.6; ARCH §4.5): `B̂ + ert > V − K_s` on the live state
/// (`b_live`, the risk quote on the live reserves), at the robust spot
/// `reprice(L_live, P_rs)` and at the box corner `reprice(L_hi, P_lo)`. `ert` enters so
/// that no band exists where neither a buyback nor S1 is possible — at a live fee within the
/// position's locked hard cap; above it the buyback's bound and these legs stay at the cap, and
/// the fee is no route for the position, which S3 settles after `s3_wait_s` (AIR-19).
///
/// Errors: `InvalidInput` if `K_s > V`; those of `quotes`.
pub fn s1_legs<Q: BoxQuotes>(
    quotes: &mut Q,
    budget: &Budget,
    b_live: Option<u64>,
    p_rs: u128,
    l_live: u128,
    bx: &SettleBox,
) -> Result<bool, MathError> {
    Ok(budget.exceeded_by(b_live)?
        && budget.exceeded_by(quotes.cost(p_rs, l_live)?)?
        && budget.exceeded_by(quotes.cost(bx.p_lo, bx.l_hi)?)?)
}

/// All S2 legs hold (ECON §5.6; ARCH §4.5): `σ > σ_max` on the live state (`sigma_live`),
/// at the robust spot `reprice(L_live, P_rs)` and at the box corner `reprice(L_hi, P_lo)`.
///
/// Errors: those of `quotes`.
pub fn s2_legs<Q: BoxQuotes>(
    quotes: &mut Q,
    sigma_max_bps: u32,
    sigma_live: u64,
    p_rs: u128,
    l_live: u128,
    bx: &SettleBox,
) -> Result<bool, MathError> {
    let max = u64::from(sigma_max_bps);
    Ok(sigma_live > max
        && quotes.sigma(p_rs, l_live)? > max
        && quotes.sigma(bx.p_lo, bx.l_hi)? > max)
}

/// Box-robust refutation of S1 and S2 (ECON §5.6; ARCH §4.5; R3A-6 a): at the corner
/// least favourable to refuting, `B̂(Q; reprice(L_lo, P_hi)) + ert ≤ V − K_s` and
/// `σ(Q; reprice(min(L_live, L_lo), P_hi)) ≤ σ_max` (an atomic LP deposit cannot raise
/// `min(L_live, ·)`). One attacker-only second cannot refute: the median corner does not
/// move. `l_live` is the venue's live `√(X·Y)` (refutation is only evaluated on an
/// executable venue, which has a price).
///
/// Errors: `InvalidInput` if `K_s > V`; those of `quotes`.
pub fn refuted<Q: BoxQuotes>(
    quotes: &mut Q,
    budget: &Budget,
    sigma_max_bps: u32,
    l_live: u128,
    bx: &SettleBox,
) -> Result<bool, MathError> {
    if budget.exceeded_by(quotes.cost(bx.p_hi, bx.l_lo)?)? {
        return Ok(false);
    }
    Ok(quotes.sigma(bx.p_hi, l_live.min(bx.l_lo))? <= u64::from(sigma_max_bps))
}

/// "Not liquidatable" that disarms (ECON §5.6; ARCH §3.8 step 2, §4.5; R3A-6 b): the
/// position is not expired, its EMA leg fails, **and** at the corner most favourable to
/// liquidation `CR(V_eff, B̂(Q; reprice(L_lo, P_hi))) ≥ L` (an undefined quote is not
/// `≥ L`). A failing spot leg alone never disarms.
///
/// Errors: `InvalidInput` if the quote is 0 lamports; those of `quotes`.
pub fn not_liquidatable<Q: BoxQuotes>(
    quotes: &mut Q,
    expired: bool,
    ema_leg: bool,
    v_eff: u64,
    cr_liq_bps: u32,
    bx: &SettleBox,
) -> Result<bool, MathError> {
    if expired || ema_leg {
        return Ok(false);
    }
    match quotes.cost(bx.p_hi, bx.l_lo)? {
        None => Ok(false),
        b => Ok(!cr_below(v_eff, b, cr_liq_bps)?),
    }
}

/// FV's `p'` at one evaluated state `(P, L)` (ECON §8.6; ARCH §4.5; SPEC §5.3, O15;
/// v0.4.1, R4A-3): the locked expiry fee if the position is expired and
/// `⌊V_eff·10⁴/B̂_FV(P, L)⌋ ≥ L` there, else the locked penalty. Decided at each corner,
/// never from the raw legs, so one contrary observation cannot switch the reading.
/// (`b_fv = 0` is impossible for `Q > 0` and reads as "not below".)
pub fn pprime(
    expired: bool,
    v_eff: u64,
    b_fv: u64,
    cr_liq_bps: u32,
    penalty_bps: u32,
    expiry_fee_bps: u32,
) -> u32 {
    let not_below = u128::from(v_eff).saturating_mul(u128::from(BPS))
        >= u128::from(cr_liq_bps).saturating_mul(u128::from(b_fv));
    if expired && not_below {
        expiry_fee_bps
    } else {
        penalty_bps
    }
}

/// The S2/S3 amount at one state `(P, L)`: `x(P, L) = min(V − K_s, FV(P, L))` with
/// `FV = B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due` and `p'` from [`pprime`] at the same state (ECON
/// §8.6; ARCH §4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CornerAmount {
    /// `x(P, L)`: `V − K_s` when FV is undefined (the cap does not bind).
    pub x: u64,
    /// `FV(P, L)`, saturating at `u64::MAX`; `None` if `B̂_FV` is undefined.
    pub fv: Option<u64>,
    /// `p'(P, L)`; `None` if `B̂_FV` is undefined.
    pub pprime: Option<u32>,
}

/// [`CornerAmount`] at `(p, l)` for a take `V − K_s = take` (sim `fv_parts` / `fv_at`).
///
/// Errors: those of `quotes`.
#[allow(clippy::too_many_arguments)]
pub fn amount_at<Q: BoxQuotes>(
    quotes: &mut Q,
    p: u128,
    l: u128,
    take: u64,
    expired: bool,
    v_eff: u64,
    r_due: u64,
    terms: &SettleTerms,
) -> Result<CornerAmount, MathError> {
    let Some(b) = quotes.fv_cost(p, l)? else {
        return Ok(CornerAmount {
            x: take,
            fv: None,
            pprime: None,
        });
    };
    let rate = pprime(
        expired,
        v_eff,
        b,
        terms.cr_liq_bps,
        terms.penalty_bps,
        terms.expiry_fee_bps,
    );
    let fv = fair_value(Some(b), rate, r_due);
    Ok(CornerAmount {
        x: fv.map_or(take, |f| f.min(take)),
        fv,
        pprime: Some(rate),
    })
}

/// The box amount check (ECON §5.6; ARCH §3.8 "Amount"; I-E5): settle only if
/// `(x(P_hi, L_lo) − x(P_lo, L_hi))·10⁴ ≤ fv_dev_bps·x(P_lo, L_hi)`, exactly (a negative
/// difference always agrees).
pub fn amount_ok(x_lo: u64, x_hi: u64, fv_dev_bps: u32) -> bool {
    let diff = u128::from(x_hi.saturating_sub(x_lo)).saturating_mul(u128::from(BPS));
    diff <= u128::from(fv_dev_bps).saturating_mul(u128::from(x_lo))
}

/// S3 timing terms, locked per position (ARCH §2.3.5; ECON §10.3, §15).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct S3Terms {
    /// `s3_wait_s`.
    pub s3_wait_s: u32,
    /// `grad_grace_s`.
    pub grad_grace_s: u32,
    /// `max_migration_pause_s`.
    pub max_migration_pause_s: u32,
}

impl S3Terms {
    /// Defaults (ECON §15): 600 s, 600 s, 3,600 s.
    pub const DEFAULT: Self = Self {
        s3_wait_s: 600,
        grad_grace_s: 600,
        max_migration_pause_s: 3_600,
    };
}

/// The market's S3 chain as the prologue left it (ARCH §3.0, §5.3; ECON §10.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct S3Chain {
    /// `s3_no_route` holds now for the position (`!venue_executable ∨ route-broken flag ∨
    /// φ_live > min(global hard cap, the position's locked cap)`; the program's
    /// `engine::route::position_no_route`, AIR-19).
    pub no_route_now: bool,
    /// Start of the continuous no-route chain (`no_route_since`); `None` when no chain runs.
    pub no_route_since: Option<i64>,
    /// `graduated_at`; `None` if unset (0 on-chain).
    pub graduated_at: Option<i64>,
    /// The venue is `PumpMigrating` now.
    pub migrating: bool,
    /// `migration_seen_at`; `None` if unset.
    pub migration_seen_at: Option<i64>,
}

/// S3 is due (ECON §10.3; ARCH §3.8 step 4, §5.3; sim `_s3_due`): the position is
/// liquidatable or expired, `s3_no_route` holds now, the continuous chain is unbroken, and
///
/// ```text
/// now ≥ max(no_route_since, graduated_at + grad_grace_s, migration_seen_at + max_migration_pause_s·[PumpMigrating]) + s3_wait_s
/// ```
///
/// with the position's locked terms. A migrating venue with no recorded
/// `migration_seen_at` is not due. S3 needs no arming; its amount obeys the box check.
pub fn s3_due(chain: &S3Chain, terms: &S3Terms, now: i64, liquidatable_or_expired: bool) -> bool {
    if !chain.no_route_now || !liquidatable_or_expired {
        return false;
    }
    let Some(since) = chain.no_route_since else {
        return false;
    };
    let mut start = i128::from(since);
    if let Some(g) = chain.graduated_at {
        start = start.max(i128::from(g).saturating_add(i128::from(terms.grad_grace_s)));
    }
    if chain.migrating {
        let Some(m) = chain.migration_seen_at else {
            return false;
        };
        start = start.max(i128::from(m).saturating_add(i128::from(terms.max_migration_pause_s)));
    }
    i128::from(now) >= start.saturating_add(i128::from(terms.s3_wait_s))
}

/// Per-position settlement arming state (ECON §5.6; ARCH §2.3.5 `s_armed`, `s_count`,
/// `s_since`, `s_last`): one eligibility clock for S1 and S2.
///
/// A disarm clears `armed` only; the other fields keep their last values (as the sim).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ArmState {
    /// `ema_count` when the clock was armed (`s_count`).
    pub count: u64,
    /// Second the clock was armed (`s_since`).
    pub since: i64,
    /// Last evaluation that kept the clock running (`s_last`).
    pub last: i64,
    /// 1 while armed (`s_armed`; a lapsed arm keeps 1 until re-armed or disarmed on the box,
    /// and no evaluation revives it, see [`ArmState::is_live`]).
    pub armed: u8,
    pub _pad: [u8; 7],
}

impl ArmState {
    /// Armed, and the last evaluation was at most `arm_gap_s` ago.
    pub fn is_live(&self, now: i64, arm_gap_s: u16) -> bool {
        self.armed != 0 && now.saturating_sub(self.last) <= i64::from(arm_gap_s)
    }
}

/// Why an armed position was disarmed (`SettleDisarmed{reason}`; ARCH §3.8, §3.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisarmReason {
    /// Box-robust "not liquidatable" ([`not_liquidatable`]; step 2).
    NotLiquidatable,
    /// Box-robust refutation ([`refuted`]; step 3).
    Refuted,
}

/// What an evaluation did to the arming clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ArmChange {
    /// Nothing (`ArmState` unchanged).
    #[default]
    None,
    /// The clock keeps running: only `last = now` was written.
    Kept,
    /// Armed by this evaluation (`count = ema_count`, `since = last = now`; `SettleArmed`).
    Armed,
    /// Disarmed (`armed = 0`; `SettleDisarmed`).
    Disarmed(DisarmReason),
}

/// The position's locked terms that a settlement reads (ARCH §2.3.5; ECON §5.6, §8.6,
/// §15).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettleTerms {
    /// `L`, the liquidation CR (`cr_liq_bps`).
    pub cr_liq_bps: u32,
    /// σ_max (`max_liq_slip_bps`), the S2 threshold.
    pub sigma_max_bps: u32,
    /// The liquidation penalty (FV's `p'` unless expired and not CR-liquidatable).
    pub penalty_bps: u32,
    /// The expiry fee (FV's `p'` for an expired, not CR-liquidatable corner; O15).
    pub expiry_fee_bps: u32,
    /// `fv_dev_bps`, the box tolerance of an S2/S3 amount.
    pub fv_dev_bps: u32,
    /// `settle_persist_n`: EMA updates on one clock before S1/S2 execute.
    pub persist_n: u16,
    /// `settle_arm_gap_s`: evaluations further apart re-arm.
    pub arm_gap_s: u16,
    /// The S3 timer's terms.
    pub s3: S3Terms,
}

/// What `sol_settle` knows after its prologue (ARCH §3.8 step 1): the post-roll EMA
/// state, the box, the budget, and the position's legs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettleInputs {
    /// `now`.
    pub now: i64,
    /// The position's deadline (expired iff `now ≥ deadline`).
    pub deadline: i64,
    /// `ema_count` after this instruction's roll.
    pub ema_count: u64,
    /// `P_ema` after the roll (the point FV is paid at, D3).
    pub p_ema: u128,
    /// `L_ema` after the roll.
    pub l_ema: u128,
    /// [`box_of_capped`] on the post-roll state, with the market's depth cap.
    pub bx: SettleBox,
    /// `V`, `K_s` (locked terms, `K_s ≤ V`) and the effective `ert`.
    pub budget: Budget,
    /// `R_due`.
    pub r_due: u64,
    /// CR-liquidatable: spot leg ∧ EMA leg ([`crate::risk::spot_leg`],
    /// [`crate::persist::liq_ema_leg`]), expiry not included.
    pub cr_liquidatable: bool,
    /// The EMA leg alone.
    pub ema_leg: bool,
    /// `venue_executable` (false while `PumpMigrating`): gates step 3 (arming and legs).
    pub executable: bool,
    /// The post-change warm-up is over (`observe::warmed` after this instruction's roll; M4R,
    /// **PROPOSED**, owner item O18). While it is not, on an executable venue, [`evaluate`]
    /// decides nothing.
    pub warmed: bool,
    /// Robust spot `P_rs` ([`crate::observe::robust_spot`]); `None` without a fresh
    /// completed bucket.
    pub p_rs: Option<u128>,
    /// `L_live`, the venue's `√(X·Y)`.
    pub l_live: u128,
    /// The risk quote on the live reserves (`B̂_live` at `φ_risk`); `None` if undefined.
    pub b_live: Option<u64>,
    /// σ on the live reserves (`u64::MAX` if undefined).
    pub sigma_live: u64,
    /// The market's S3 chain.
    pub s3: S3Chain,
}

impl SettleInputs {
    /// `now ≥ deadline`.
    pub const fn expired(&self) -> bool {
        self.now >= self.deadline
    }

    /// `V_eff = V − R_due`, floored at 0.
    pub const fn v_eff(&self) -> u64 {
        self.budget.v.saturating_sub(self.r_due)
    }
}

/// An executing settlement (ARCH §3.8 "Effects"; ECON §8.6). The payout order and the
/// S2/S3 vesting lump are `waterfall::settle`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Settlement {
    /// S1, S2 or S3.
    pub kind: SettleKind,
    /// `K_s` to the keeper.
    pub k_s: u64,
    /// Lenders' take `x`: S1 `V − K_s`, never capped; S2/S3 `x(P_ema, L_ema)`.
    pub lenders: u64,
    /// `V − K_s − x`, to the borrower through the position close.
    pub borrower: u64,
    /// S2/S3: `FV(P_ema, L_ema)` (`None`: undefined, or S1).
    pub fv: Option<u64>,
    /// S2/S3: `p'(P_ema, L_ema)`.
    pub pprime: Option<u32>,
    /// S2/S3: the amounts at `(P_lo, L_hi)` and `(P_hi, L_lo)` that passed [`amount_ok`].
    pub corners: Option<[CornerAmount; 2]>,
}

/// The outcome of one [`evaluate`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Evaluation {
    /// What happened to the arming clock.
    pub arm: ArmChange,
    /// The settlement this call executes, if any.
    pub settlement: Option<Settlement>,
    /// An S2 was due (persisted, S1 legs false) but its amount was not box-consistent: it
    /// waits, with no further state change.
    pub s2_waited: bool,
    /// S3 was due but its amount was not box-consistent: it waits.
    pub s3_waited: bool,
}

/// One `sol_settle` evaluation after the prologue (ARCH §3.8 step 1: the caller computes
/// `R_due`, `K_s`, the legs and [`box_of_capped`]), in ARCH §3.8's order, which equals the sim's
/// `try_exit(settle_only = True)` step for step:
///
/// 2. Not CR-liquidatable and not expired: an armed position is disarmed only if
///    [`not_liquidatable`] holds on the box (`SettleDisarmed{NotLiquidatable}`); otherwise
///    a live clock keeps running (`last = now`) and a lapsed one is left as it is, for
///    step 3 to re-arm (AIR-16; ECON §5.6). Nothing else happens.
/// 3. If the venue is executable: `live = armed ∧ now − last ≤ arm_gap_s`.
///    - Armed (live or lapsed) and [`refuted`] on the box: disarm (`Refuted`).
///    - Else, with a fresh completed bucket (`p_rs` set): if the [`s1_legs`] or the
///      [`s2_legs`] hold, arm unless live (`count = ema_count`, `since = now`), set
///      `last = now`, and once `ema_count − count ≥ persist_n` execute — **S1 if the S1
///      legs hold, else S2**; an S2 whose amount is not box-consistent waits. If neither
///      holds and the arm is live, the clock keeps running.
///    - Else (stale bucket) a live clock keeps running.
/// 4. S3 ([`s3_due`], no arming): settle, subject to the same amount check. (An S2 that
///    waited in step 3 falls through to here, as in the sim.)
///
/// **Post-change gate** (M4R, **PROPOSED**, owner item O18; ARCH §3.8): while the observation
/// state is in its post-change warm-up (`!inp.warmed`) on an **executable** venue, nothing is
/// evaluated — no arm change, no settlement, no step 4: the EMA, the ring and the box hold the
/// restart's anchor and partial seeds, which no decision may rest on. S3 cannot be due then
/// (every restart clears the no-route chain — an adoption — or has no position — a creation —
/// and the warm-up ends within `EMA_WARMUP_BUCKETS` observed seconds), and a venue that cannot
/// execute is never gated, so S3 is never deadlocked.
///
/// Every "no change" is an `Ok` with no settlement: the call's prologue (observation, route
/// chains) stands and the keeper's per-second call doubles as a crank. The state is only
/// written as described (a disarm clears `armed` alone).
///
/// Errors: `InvalidInput` if `K_s > V`; `Underflow` if the armed count exceeds
/// `ema_count` (a corrupted state); those of `quotes`. On error the state may already
/// hold this call's arm update; the program reverts the transaction.
pub fn evaluate<Q: BoxQuotes>(
    state: &mut ArmState,
    inp: &SettleInputs,
    terms: &SettleTerms,
    quotes: &mut Q,
) -> Result<Evaluation, MathError> {
    let now = inp.now;
    if inp.executable && !inp.warmed {
        debug_assert!(!s3_due(&inp.s3, &terms.s3, now, true));
        return Ok(Evaluation::default());
    }
    let expired = inp.expired();
    let v_eff = inp.v_eff();
    let bx = &inp.bx;
    let mut out = Evaluation::default();

    // Step 2 of ARCH §3.8: not liquidatable and not expired.
    if !(inp.cr_liquidatable || expired) {
        if state.armed != 0 {
            if not_liquidatable(quotes, expired, inp.ema_leg, v_eff, terms.cr_liq_bps, bx)? {
                state.armed = 0;
                out.arm = ArmChange::Disarmed(DisarmReason::NotLiquidatable);
            } else if state.is_live(now, terms.arm_gap_s) {
                // AIR-16: only a live clock keeps running; a lapsed one is left for step 3 to
                // re-arm (ECON §5.6), never revived with its old count.
                state.last = now;
                out.arm = ArmChange::Kept;
            }
        }
        return Ok(out);
    }

    // Step 3: S1 / S2 on one clock, on an executable venue.
    if inp.executable {
        let live = state.is_live(now, terms.arm_gap_s);
        if state.armed != 0 && refuted(quotes, &inp.budget, terms.sigma_max_bps, inp.l_live, bx)? {
            state.armed = 0;
            out.arm = ArmChange::Disarmed(DisarmReason::Refuted);
        } else if let Some(p_rs) = inp.p_rs {
            let s1 = s1_legs(quotes, &inp.budget, inp.b_live, p_rs, inp.l_live, bx)?;
            let s2 = !s1
                && s2_legs(
                    quotes,
                    terms.sigma_max_bps,
                    inp.sigma_live,
                    p_rs,
                    inp.l_live,
                    bx,
                )?;
            if s1 || s2 {
                if live {
                    out.arm = ArmChange::Kept;
                } else {
                    state.armed = 1;
                    state.count = inp.ema_count;
                    state.since = now;
                    out.arm = ArmChange::Armed;
                }
                state.last = now;
                let updates = inp
                    .ema_count
                    .checked_sub(state.count)
                    .ok_or(MathError::Underflow)?;
                if updates >= u64::from(terms.persist_n) {
                    let kind = if s1 { SettleKind::S1 } else { SettleKind::S2 };
                    match settlement(kind, inp, terms, quotes)? {
                        Some(s) => {
                            out.settlement = Some(s);
                            return Ok(out);
                        }
                        None => out.s2_waited = true,
                    }
                }
            } else if live {
                state.last = now;
                out.arm = ArmChange::Kept;
            }
        } else if live {
            state.last = now;
            out.arm = ArmChange::Kept;
        }
    }

    // Step 4: S3, market-level, no arming.
    if s3_due(&inp.s3, &terms.s3, now, true) {
        match settlement(SettleKind::S3, inp, terms, quotes)? {
            Some(s) => out.settlement = Some(s),
            None => out.s3_waited = true,
        }
    }
    Ok(out)
}

/// The amount of a settlement of `kind` (ARCH §3.8 "Amount"; sim `_settle`): S1 pays
/// `V − K_s`, never capped; S2/S3 pay `x(P_ema, L_ema)` iff [`amount_ok`] holds between
/// the corners `(P_lo, L_hi)` and `(P_hi, L_lo)`, else `None` (wait).
///
/// Errors: `InvalidInput` if `K_s > V`; those of `quotes`.
pub fn settlement<Q: BoxQuotes>(
    kind: SettleKind,
    inp: &SettleInputs,
    terms: &SettleTerms,
    quotes: &mut Q,
) -> Result<Option<Settlement>, MathError> {
    let k_s = inp.budget.k_s;
    let take = inp
        .budget
        .v
        .checked_sub(k_s)
        .ok_or(MathError::InvalidInput)?;
    if kind == SettleKind::S1 {
        return Ok(Some(Settlement {
            kind,
            k_s,
            lenders: take,
            borrower: 0,
            fv: None,
            pprime: None,
            corners: None,
        }));
    }
    let (expired, v_eff, r_due, bx) = (inp.expired(), inp.v_eff(), inp.r_due, &inp.bx);
    let mut at = |p: u128, l: u128| amount_at(quotes, p, l, take, expired, v_eff, r_due, terms);
    let lo = at(bx.p_lo, bx.l_hi)?;
    let hi = at(bx.p_hi, bx.l_lo)?;
    if !amount_ok(lo.x, hi.x, terms.fv_dev_bps) {
        return Ok(None);
    }
    let ema = at(inp.p_ema, inp.l_ema)?;
    Ok(Some(Settlement {
        kind,
        k_s,
        lenders: ema.x,
        borrower: take.checked_sub(ema.x).ok_or(MathError::Invariant)?,
        fv: ema.fv,
        pprime: ema.pprime,
        corners: Some([lo, hi]),
    }))
}

const _: () = assert!(core::mem::size_of::<ArmState>() == 32);

// ---- route predicates and chains (ARCH §3.0 step 8, §5.3; ECON §10.3) -------------------

/// `s3_no_route = !venue_executable ∨ route-broken flag ∨ φ_live > hard cap` (ARCH §5.3;
/// sim `Market.s3_no_route`). It feeds only the S3 chain, `sol_settle`'s "no route now" and
/// the open gate (V2-1). The caller picks the cap: for the market's chain the lower of the
/// global `venue_fee_hard_cap_bps` (R3A-4 c) and the lowest cap an open position locked, for a
/// position the lower of the global cap and its own (AIR-19; the program's `engine::route`).
pub fn s3_no_route(executable: bool, route_broken: bool, phi_ppm: u32, hard_cap_bps: u64) -> bool {
    !executable || route_broken || crate::fees::exceeds_cap(phi_ppm, hard_cap_bps)
}

/// `pool_fail = !venue_executable ∨ φ_live > hard cap` (ARCH §2.6 rule 3, §5.3; sim
/// `Market.pool_fail`): venue-side evidence only. It never reads the route-broken flag
/// (V1-7, T-S3-BROKEN) and feeds only the fast-path chain.
pub fn pool_fail(executable: bool, phi_ppm: u32, hard_cap_bps: u64) -> bool {
    !executable || crate::fees::exceeds_cap(phi_ppm, hard_cap_bps)
}

/// One observation of a continuous route chain (ARCH §3.0 step 8, §5.3; ECON §10.3; C0-16;
/// sim `Market._chain`): `(since, last)` with 0 for "unset" (the program's encoding of the
/// sim's `None`).
///
/// ```text
/// hit:   since = now if since == 0 or last == 0 or now − last > S3_OBS_GAP_S; last = now
/// miss:  since = last = 0          (an observation with a route clears the chain)
/// ```
///
/// Returns the new `(since, last)`. Observations further apart than `S3_OBS_GAP_S` restart
/// the chain, so separate outages never add up (ECON §10.3 self-test).
pub fn chain_step(since: i64, last: i64, now: i64, hit: bool) -> (i64, i64) {
    if !hit {
        return (0, 0);
    }
    let restart = since == 0 || last == 0 || now.saturating_sub(last) > crate::consts::S3_OBS_GAP_S;
    (if restart { now } else { since }, now)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::observe::{EmaParams, Observation};

    const WAD: u128 = crate::num::WAD;

    /// A toy venue whose quotes rise with `P` and fall with `L` like the real ones:
    /// `B̂ = ⌊1,000·P/L⌋` lamports, `σ = ⌊100·P/L⌋` bps. It records every call.
    struct Toy {
        calls: std::vec::Vec<(char, u128, u128)>,
    }

    impl BoxQuotes for Toy {
        fn cost(&mut self, p: u128, l: u128) -> Result<Option<u64>, MathError> {
            self.calls.push(('c', p, l));
            Ok(u64::try_from(p * 1_000 / l).ok())
        }
        fn sigma(&mut self, p: u128, l: u128) -> Result<u64, MathError> {
            self.calls.push(('s', p, l));
            Ok(u64::try_from(p * 100 / l).unwrap())
        }
    }

    fn toy() -> Toy {
        Toy {
            calls: std::vec::Vec::new(),
        }
    }

    const TERMS: SettleTerms = SettleTerms {
        cr_liq_bps: 12_500,
        sigma_max_bps: 2_000,
        penalty_bps: 500,
        expiry_fee_bps: 200,
        fv_dev_bps: 100,
        persist_n: 3,
        arm_gap_s: 10,
        s3: S3Terms::DEFAULT,
    };

    fn inputs(now: i64, bx: SettleBox) -> SettleInputs {
        SettleInputs {
            now,
            deadline: 1_000,
            ema_count: 100,
            p_ema: bx.p_lo,
            l_ema: bx.l_hi,
            bx,
            budget: Budget {
                v: 1_000,
                k_s: 20,
                ert: 0,
            },
            r_due: 0,
            cr_liquidatable: true,
            ema_leg: true,
            executable: true,
            warmed: true,
            p_rs: Some(bx.p_lo),
            l_live: bx.l_hi,
            b_live: None,
            sigma_live: u64::MAX,
            s3: S3Chain {
                no_route_now: false,
                no_route_since: None,
                graduated_at: None,
                migrating: false,
                migration_seen_at: None,
            },
        }
    }

    #[test]
    fn box_uses_the_bucket_extremes() {
        let prm = EmaParams::DEFAULT;
        let mut o = Observation::new(0, 100 * WAD, 1_000).unwrap();
        for t in 1..=8 {
            o.record(t, 100 * WAD, 1_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
        }
        // One attacker-only push to 3x, then frozen seconds that hold the honest price too.
        o.record(9, 300 * WAD, 1_000).unwrap();
        o.roll(10, &prm).unwrap();
        let pushed = o.ema.price;
        assert!(pushed > 110 * WAD);
        for t in 10..14 {
            o.record(t, 100 * WAD, 1_000).unwrap();
            o.record(t, o.ema.price + 1, 1_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
            assert_eq!(o.ema.price, pushed, "frozen");
        }
        let bx = box_of(&o.ema, &o.ring, 14, true).unwrap();
        // The lower median of the lows stays at the honest price: 1 push + 4 frozen seconds
        // move the lows of one entry only.
        assert_eq!(bx.p_lo, 100 * WAD);
        assert_eq!(bx.p_hi, pushed + 1);
        assert!(bx.contains(o.ema.price, o.ema.liq));
        assert_eq!(box_of(&o.ema, &o.ring, 14, false).unwrap(), bx);
        // After more than RING_LEN seconds without an integration, on a venue that cannot
        // execute: the EMA point.
        let far = box_of(&o.ema, &o.ring, o.ema.ts + 9, false).unwrap();
        assert!(far.is_point() && far.p_lo == pushed);
        assert_eq!(box_of(&o.ema, &o.ring, o.ema.ts + 8, false).unwrap(), bx);
        // On an executable venue a stale integration never collapses it (M2-SEC-1).
        for age in [8, 9, 100, 1_000_000] {
            assert_eq!(box_of(&o.ema, &o.ring, o.ema.ts + age, true).unwrap(), bx);
        }
        let mut empty = o;
        empty.ring = Ring::default();
        for exec in [false, true] {
            assert_eq!(
                box_of(&empty.ema, &empty.ring, 14, exec),
                Err(MathError::InvalidInput)
            );
        }
    }

    /// M2-SEC-1 on the observation module alone: one attacker-only pushed second, integrated
    /// by a roll without a record (what a lender instruction's A16 roll does), then more than
    /// `RING_LEN` quiet seconds. On an executable venue the box keeps the honest medians of
    /// the ring (the pushed entry is one of 8), so the pushed EMA alone decides nothing; only
    /// a venue that cannot execute collapses to the EMA point.
    #[test]
    fn a_rolled_push_does_not_collapse_the_box_on_an_executable_venue() {
        let prm = EmaParams::DEFAULT;
        let mut o = Observation::new(0, 100 * WAD, 1_000).unwrap();
        for t in 1..=8 {
            o.record(t, 100 * WAD, 1_000).unwrap();
            o.roll(t + 1, &prm).unwrap();
        }
        // The push at t = 9 is rolled at t = 11 (dt = 2: up to +28.6 %), then silence.
        o.record(9, 300 * WAD, 1_000).unwrap();
        o.roll(11, &prm).unwrap();
        let pushed = o.ema.price;
        assert!(pushed > 125 * WAD, "{pushed}");
        let now = o.ema.ts + 9;
        let collapsed = box_of(&o.ema, &o.ring, now, false).unwrap();
        assert!(collapsed.is_point() && collapsed.p_lo == pushed);
        let bx = box_of(&o.ema, &o.ring, now, true).unwrap();
        assert_eq!((bx.p_lo, bx.p_hi), (100 * WAD, pushed));
        assert!(!bx.is_point());
    }

    /// AIR-15: a deposit wrapped around a crank raises that second's liquidity high, which the
    /// honest record in the same second cannot lower, so wraps in 5 of the 8 seconds double the
    /// upper median `L_mhi` (`L_ema` does not move) and, uncapped, widen `L_hi`: the S1 corner
    /// `(P_lo, L_hi)` then fails where the honest depth holds S1 — a deferral bought with no swap
    /// fee. With the market's depth cap `L_hi = max(L_ema, min(L_mhi, cap))`.
    #[test]
    fn deposit_wrapped_highs_widen_l_hi_only_up_to_the_depth_cap() {
        let prm = EmaParams::DEFAULT;
        let p = 1_000_000;
        let mut o = Observation::new(0, p, 1_000).unwrap();
        for t in 1..=8 {
            o.record(t, p, 1_000).unwrap();
            if t >= 4 {
                o.record(t, p, 2_000).unwrap();
            }
            o.roll(t + 1, &prm).unwrap();
        }
        assert_eq!(o.ema.liq, 1_000, "a covered second does not move L_ema");
        let wide = box_of(&o.ema, &o.ring, 9, true).unwrap();
        assert_eq!((wide.l_lo, wide.l_hi), (1_000, 2_000), "L_mhi doubled");
        assert_eq!(box_of_capped(&o.ema, &o.ring, 9, true, None), Ok(wide));
        // PumpSwap: the cap is the burned depth D, here the honest depth: no widening at all.
        let pswap = box_of_capped(&o.ema, &o.ring, 9, true, Some(1_000)).unwrap();
        assert_eq!(
            pswap,
            SettleBox {
                l_hi: 1_000,
                ..wide
            }
        );
        // Raydium, λ 75 % over λ_min 50 %: ⌊D·10⁴/λ_min⌋ = 1.5× the depth.
        let ray = box_of_capped(&o.ema, &o.ring, 9, true, Some(1_500)).unwrap();
        assert_eq!(
            ray,
            SettleBox {
                l_hi: 1_500,
                ..wide
            }
        );
        // A cap at or above L_mhi changes nothing; one below L_ema keeps the EMA point inside.
        assert_eq!(
            box_of_capped(&o.ema, &o.ring, 9, true, Some(2_000)),
            Ok(wide)
        );
        let low = box_of_capped(&o.ema, &o.ring, 9, true, Some(10)).unwrap();
        assert!(low.l_hi == o.ema.liq && low.contains(o.ema.price, o.ema.liq));
        // The EMA point of a halted venue after RING_LEN seconds, cap or not.
        let far = o.ema.ts + 9;
        assert_eq!(
            box_of_capped(&o.ema, &o.ring, far, false, Some(1_500)),
            Ok(SettleBox::point(p, 1_000))
        );
        // B̂ = 10⁹/L at the S1 corner: S1 holds at the honest depth (10⁶ > V − K_s) but not at
        // the doubled one (5·10⁵); capped, the wraps cannot defer it.
        let budget = Budget {
            v: 800_000,
            k_s: 20,
            ert: 0,
        };
        let mut q = toy();
        assert_eq!(s1_legs(&mut q, &budget, None, p, 1_000, &wide), Ok(false));
        assert_eq!(s1_legs(&mut q, &budget, None, p, 1_000, &pswap), Ok(true));
    }

    /// AIR-20 (kept; `engine::arming` module docs): the S1 legs read the raw `L_live`, so inside
    /// an unlocked LP's `[withdraw, sol_settle, deposit]` the live and robust-spot legs read the
    /// drained pool and the box corner `(P_lo, L_hi)` is the only veto. It vetoes whatever the
    /// honest robust spot vetoes while that lies in the box, and nothing once the honest state has
    /// left it (here depth above `L_hi` that the medians have not followed yet).
    #[test]
    fn a_drained_live_state_leaves_the_box_corner_as_the_only_s1_veto() {
        let budget = Budget {
            v: 1_000,
            k_s: 20,
            ert: 0,
        };
        let mut q = toy();
        // The corner B̂(10, 10) = 1,000 > V − K_s = 980: S1 holds there.
        let bx = SettleBox {
            p_lo: 10,
            p_hi: 10,
            l_lo: 8,
            l_hi: 10,
        };
        // The honest pool is deeper than L_hi (L_live = 20): B̂ = 500, a buyback the vault pays.
        assert_eq!(s1_legs(&mut q, &budget, Some(500), 10, 20, &bx), Ok(false));
        // Drained to L = 5 inside the bundle: B̂ = 2,000 live and at the robust spot.
        assert_eq!(s1_legs(&mut q, &budget, Some(2_000), 10, 5, &bx), Ok(true));
        // Once the medians follow the depth (L_hi = 20), the corner vetoes the drain too.
        let followed = SettleBox { l_hi: 20, ..bx };
        assert_eq!(
            s1_legs(&mut q, &budget, Some(2_000), 10, 5, &followed),
            Ok(false)
        );
    }

    #[test]
    fn predicates_read_their_corners() {
        let bx = SettleBox {
            p_lo: 10,
            p_hi: 20,
            l_lo: 1,
            l_hi: 2,
        };
        let b = Budget {
            v: 1_000,
            k_s: 20,
            ert: 0,
        };
        let mut q = toy();
        // cost(P_lo, L_hi) = 5,000 > 980 ⇒ S1 at the box corner.
        assert_eq!(s1_legs(&mut q, &b, None, 10, 2, &bx), Ok(true));
        assert!(q.calls.contains(&('c', 10, 2)));
        let mut q = toy();
        assert_eq!(s1_legs(&mut q, &b, Some(980), 10, 2, &bx), Ok(false));
        assert!(q.calls.is_empty(), "short-circuits on the live leg");
        // σ(P_lo, L_hi) = 500 bps ≤ 2,000.
        let mut q = toy();
        assert_eq!(s2_legs(&mut q, 2_000, u64::MAX, 10, 2, &bx), Ok(false));
        assert_eq!(s2_legs(&mut q, 400, u64::MAX, 10, 2, &bx), Ok(true));
        // Refutation at (P_hi, L_lo): cost 20,000 > 980 ⇒ not refuted, σ never read.
        let mut q = toy();
        assert_eq!(refuted(&mut q, &b, 2_000, 5, &bx), Ok(false));
        assert_eq!(q.calls, [('c', 20, 1)]);
        let rich = Budget {
            v: 100_000,
            k_s: 20,
            ert: 0,
        };
        let mut q = toy();
        assert_eq!(refuted(&mut q, &rich, 2_000, 5, &bx), Ok(true));
        assert_eq!(q.calls, [('c', 20, 1), ('s', 20, 1)]);
        assert_eq!(refuted(&mut q, &rich, 1_999, 5, &bx), Ok(false));
        // Not liquidatable at (P_hi, L_lo): CR = V_eff/20,000.
        let mut q = toy();
        assert_eq!(
            not_liquidatable(&mut q, false, false, 25_000, 12_500, &bx),
            Ok(true)
        );
        assert_eq!(
            not_liquidatable(&mut q, false, false, 24_999, 12_500, &bx),
            Ok(false)
        );
        assert_eq!(
            not_liquidatable(&mut q, true, false, 25_000, 12_500, &bx),
            Ok(false)
        );
        assert_eq!(
            not_liquidatable(&mut q, false, true, 25_000, 12_500, &bx),
            Ok(false)
        );
    }

    #[test]
    fn pprime_and_amounts() {
        assert_eq!(pprime(true, 125, 100, 12_500, 500, 200), 200);
        assert_eq!(pprime(true, 124, 100, 12_500, 500, 200), 500);
        assert_eq!(pprime(false, 1_000, 100, 12_500, 500, 200), 500);
        assert!(amount_ok(1_000, 1_010, 100));
        assert!(!amount_ok(1_000, 1_011, 100));
        assert!(amount_ok(1_000, 900, 0));
        assert!(amount_ok(0, 0, 0));
        let mut q = toy();
        // B̂_FV(10, 2) = 5,000; not expired ⇒ penalty 5 %: FV = 5,000 + 250 + 7.
        let a = amount_at(&mut q, 10, 2, 9_000, false, 0, 7, &TERMS).unwrap();
        assert_eq!(
            a,
            CornerAmount {
                x: 5_257,
                fv: Some(5_257),
                pprime: Some(500)
            }
        );
        let a = amount_at(&mut q, 10, 2, 5_000, false, 0, 7, &TERMS).unwrap();
        assert_eq!(a.x, 5_000);
    }

    #[test]
    fn s3_timer() {
        let t = S3Terms::DEFAULT;
        let mut c = S3Chain {
            no_route_now: true,
            no_route_since: Some(100),
            graduated_at: None,
            migrating: false,
            migration_seen_at: None,
        };
        assert!(!s3_due(&c, &t, 699, true));
        assert!(s3_due(&c, &t, 700, true));
        assert!(!s3_due(&c, &t, 700, false));
        c.graduated_at = Some(500);
        assert!(!s3_due(&c, &t, 1_699, true) && s3_due(&c, &t, 1_700, true));
        c.migrating = true;
        assert!(!s3_due(&c, &t, 10_000, true));
        c.migration_seen_at = Some(0);
        assert!(!s3_due(&c, &t, 4_199, true) && s3_due(&c, &t, 4_200, true));
        c.no_route_now = false;
        assert!(!s3_due(&c, &t, 10_000, true));
        c.no_route_now = true;
        c.no_route_since = None;
        assert!(!s3_due(&c, &t, 10_000, true));
    }

    #[test]
    fn arming_clock() {
        let bx = SettleBox::point(10, 2);
        let mut st = ArmState::default();
        let mut q = toy();
        // S1 legs hold: arm at count 100, settle once 3 more updates have passed.
        let mut inp = inputs(10, bx);
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.arm, ArmChange::Armed);
        assert_eq!((st.armed, st.count, st.since, st.last), (1, 100, 10, 10));
        // Legs false but no refutation: the clock keeps running (a spot-leg veto).
        inp.now = 12;
        inp.ema_count = 102;
        inp.b_live = Some(10);
        inp.budget.v = 1_000;
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.arm, ArmChange::Kept);
        assert_eq!((st.count, st.since, st.last), (100, 10, 12));
        // Stale bucket: still kept.
        inp.now = 13;
        inp.p_rs = None;
        assert_eq!(
            evaluate(&mut st, &inp, &TERMS, &mut q).unwrap().arm,
            ArmChange::Kept
        );
        // Legs hold again, 3 updates since arming: S1 for V − K_s.
        inp.now = 14;
        inp.ema_count = 103;
        inp.p_rs = Some(10);
        inp.b_live = None;
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        let s = e.settlement.unwrap();
        assert_eq!(
            (s.kind, s.lenders, s.borrower, s.k_s),
            (SettleKind::S1, 980, 0, 20)
        );
        // A gap longer than arm_gap_s re-arms.
        let mut st2 = st;
        inp.now = 30;
        inp.ema_count = 120;
        let e = evaluate(&mut st2, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.arm, ArmChange::Armed);
        assert_eq!((st2.count, st2.since), (120, 30));
        // Not liquidatable, EMA leg false, box says CR ≥ L: disarm; the rest stays.
        inp.cr_liquidatable = false;
        inp.ema_leg = false;
        inp.budget.v = 1_000_000;
        let e = evaluate(&mut st2, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.arm, ArmChange::Disarmed(DisarmReason::NotLiquidatable));
        assert_eq!(
            (st2.armed, st2.count, st2.since, st2.last),
            (0, 120, 30, 30)
        );
        let e = evaluate(&mut st2, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e, Evaluation::default());
        // Corrupted count.
        let mut bad = ArmState {
            armed: 1,
            count: 500,
            last: 30,
            ..ArmState::default()
        };
        inp.cr_liquidatable = true;
        inp.budget.v = 1_000;
        assert_eq!(
            evaluate(&mut bad, &inp, &TERMS, &mut q),
            Err(MathError::Underflow)
        );
    }

    /// AIR-16 (ECON §5.6: evaluations more than `settle_arm_gap_s` apart re-arm the clock): a
    /// clock that lapsed (no evaluation for more than `arm_gap_s`) is not revived by a step-2
    /// evaluation (not CR-liquidatable, the box does not say "not liquidatable"), so the next
    /// step-3 evaluation re-arms it and the persistence window restarts, as step 3 does for a
    /// lapsed clock itself; it never settles at once on the old count.
    #[test]
    fn step_2_does_not_revive_a_lapsed_clock() {
        let bx = SettleBox::point(10, 2);
        let mut q = toy();
        // Armed at t = 10 on count 100 (S1 legs hold).
        let mut st = ArmState::default();
        let mut inp = inputs(10, bx);
        assert_eq!(
            evaluate(&mut st, &inp, &TERMS, &mut q).unwrap().arm,
            ArmChange::Armed
        );
        let armed = st;
        // 15 s without an evaluation (> arm_gap_s = 10), then a call in step 2: the spot leg
        // fails (e.g. a stale bucket or a same-transaction dump), the EMA leg holds, so the box
        // cannot say "not liquidatable".
        inp.now = 25;
        inp.ema_count = 120;
        inp.cr_liquidatable = false;
        let step2 = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        let after_step2 = st;
        // The next second, in step 3 with the legs holding and 20 ≥ persist_n updates since the
        // old arm.
        inp.now = 26;
        inp.cr_liquidatable = true;
        let step3 = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert!(
            step3.settlement.is_none(),
            "a lapsed clock must re-arm, not settle at once on its old count: {step3:?}"
        );
        assert_eq!(step3.arm, ArmChange::Armed);
        assert_eq!((st.armed, st.count, st.since, st.last), (1, 120, 26, 26));
        assert_eq!(
            step2,
            Evaluation::default(),
            "step 2 leaves a lapsed clock alone"
        );
        assert_eq!(after_step2, armed);
        // A live clock in step 2 still keeps running.
        inp.now = 30;
        inp.cr_liquidatable = false;
        assert_eq!(
            evaluate(&mut st, &inp, &TERMS, &mut q).unwrap().arm,
            ArmChange::Kept
        );
        assert_eq!((st.count, st.since, st.last), (120, 26, 30));
        // Persistence restarted at 120: S1 once 3 more updates have passed, within the gap.
        inp.now = 31;
        inp.ema_count = 123;
        inp.cr_liquidatable = true;
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.settlement.map(|s| s.kind), Some(SettleKind::S1));
    }

    /// M4R: in the post-change warm-up an executable venue's evaluation decides nothing (no
    /// arm, no disarm, no settlement); a venue that cannot execute is not gated (S3 proceeds).
    #[test]
    fn warm_up_gates_an_executable_venue_only() {
        let bx = SettleBox::point(10, 2);
        let mut q = toy();
        let mut inp = inputs(10, bx);
        inp.warmed = false;
        for armed in [0u8, 1] {
            let mut st = ArmState {
                armed,
                count: 1,
                since: 1,
                last: 9,
                _pad: [0; 7],
            };
            let before = st;
            assert_eq!(
                evaluate(&mut st, &inp, &TERMS, &mut q).unwrap(),
                Evaluation::default()
            );
            assert_eq!(st, before);
            // Not liquidatable either: no disarm.
            let mut calm = inp;
            calm.cr_liquidatable = false;
            calm.ema_leg = false;
            calm.budget.v = 1_000_000;
            assert_eq!(
                evaluate(&mut st, &calm, &TERMS, &mut q).unwrap(),
                Evaluation::default()
            );
            assert_eq!(st, before);
        }
        // The same inputs once warmed: armed.
        inp.warmed = true;
        let mut st = ArmState::default();
        assert_eq!(
            evaluate(&mut st, &inp, &TERMS, &mut q).unwrap().arm,
            ArmChange::Armed
        );
        // A venue that cannot execute while warming: S3 as always.
        let mut inp = inputs(700, bx);
        inp.warmed = false;
        inp.executable = false;
        inp.budget = Budget {
            v: 100_000,
            k_s: 250,
            ert: 0,
        };
        inp.s3 = S3Chain {
            no_route_now: true,
            no_route_since: Some(0),
            graduated_at: None,
            migrating: false,
            migration_seen_at: None,
        };
        let mut st = ArmState::default();
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.settlement.unwrap().kind, SettleKind::S3);
    }

    #[test]
    fn route_predicates_and_chain_step() {
        // s3_no_route reads the flag; pool_fail never does (V1-7).
        assert!(!s3_no_route(true, false, 2_500, 500));
        assert!(s3_no_route(false, false, 2_500, 500));
        assert!(s3_no_route(true, true, 2_500, 500));
        assert!(s3_no_route(true, false, 50_001, 500));
        assert!(!s3_no_route(true, false, 50_000, 500));
        assert!(!pool_fail(true, 50_000, 500));
        assert!(pool_fail(true, 50_001, 500));
        assert!(pool_fail(false, 0, 500));
        // The chain (sim `_chain`): start, continue within the gap, restart after it, clear.
        assert_eq!(chain_step(0, 0, 100, true), (100, 100));
        assert_eq!(chain_step(100, 100, 160, true), (100, 160));
        assert_eq!(chain_step(100, 160, 221, true), (221, 221));
        assert_eq!(chain_step(100, 160, 170, false), (0, 0));
        assert_eq!(chain_step(0, 0, 170, false), (0, 0));
    }

    #[test]
    fn refutation_and_s3_in_one_call() {
        // Armed, refuted on the box, and S3 due: disarm, then settle S3 at x(P_ema, L_ema).
        let bx = SettleBox::point(10, 2);
        let mut st = ArmState {
            armed: 1,
            count: 1,
            since: 1,
            last: 1,
            _pad: [0; 7],
        };
        let mut inp = inputs(700, bx);
        inp.budget = Budget {
            v: 100_000,
            k_s: 250,
            ert: 0,
        };
        inp.s3 = S3Chain {
            no_route_now: true,
            no_route_since: Some(0),
            graduated_at: None,
            migrating: false,
            migration_seen_at: None,
        };
        let mut q = toy();
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert_eq!(e.arm, ArmChange::Disarmed(DisarmReason::Refuted));
        let s = e.settlement.unwrap();
        // B̂_FV = 5,000, penalty 5 %: FV = 5,250 < V − K_s.
        assert_eq!(
            (s.kind, s.lenders, s.fv, s.pprime),
            (SettleKind::S3, 5_250, Some(5_250), Some(500))
        );
        assert_eq!(s.borrower, 100_000 - 250 - 5_250);
        assert_eq!(s.corners.unwrap()[0].x, 5_250);
        // A box wider than fv_dev_bps makes it wait.
        inp.bx = SettleBox {
            p_lo: 10,
            p_hi: 11,
            l_lo: 2,
            l_hi: 2,
        };
        let e = evaluate(&mut st, &inp, &TERMS, &mut q).unwrap();
        assert!(e.s3_waited && e.settlement.is_none());
    }
}
