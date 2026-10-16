//! Owner: oracle track. Bounds on keeper-initiated buybacks: the exit reference prices
//! and `max_in` (ECON §2.1, §8.2, §12.2; ARCH §3.6 step 6, §3.7, §5.2). Normative
//! reference: `Market.exit_reference`, `ema_growth_bps`, `kappa_eff` and the `max_in`
//! lines of `_buyback_exit` in `sim/shortwin_sim.py` v0.4.1 (default switches:
//! `ref_cap_lo = True`, `liq_reserve = "none"`, `exp_reserve = "rent+fee"`); every
//! division floors, as there.
//!
//! The reference cost `B̂_ref = B̂(Q; reprice(L_b, P_ref))` at the risk fee is the quote
//! track's; this module supplies `P_ref`, `L_b` ([`ref_liquidity`], on Raydium with the
//! [`ref_depth_cap`] of M4F-1, `Market.ref_depth_cap` in the sim) and, from `B̂_ref`,
//! the bounds [`max_in_liq`] and [`max_in_exp`]. It also supplies the liquidity the
//! liquidation spot leg is judged at, `max(L_live, L_floor)` with `L_floor = L_ema`, or `min(L_ema,
//! L_mhi)` while the post-change guard is set ([`spot_liquidity`], FITS-2, AIR-17). The keeper executes iff
//! `B̂_live ≤ max_in`. δ is a fixed, locked term (`delta_liq_bps` / `delta_exp_bps`):
//! v0.4 removed the Dutch clock (ECON #49).

use crate::consts::EMA_TAU_S;
use crate::error::MathError;
use crate::num::{BPS, Rounding, mul_bps_u64, mul_div};
use crate::observe::{
    Ema, LastBucket, Ring, ring_liq_lower_median, ring_liq_upper_median, sustained_growth,
};
use crate::pool_select::depth_at_lambda;

/// Exit-bound terms, locked per position (ARCH §2.3.3, §2.3.5 "locked exit-bound and
/// persistence terms"; ECON §8.2, §15).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExitParams {
    /// κ_max (`kappa_liq_bps`).
    pub kappa_liq_bps: u16,
    /// κ_base (`kappa_base_bps`).
    pub kappa_base_bps: u16,
    /// Fixed liquidation δ over `B̂_ref` (`delta_liq_bps`).
    pub delta_liq_bps: u16,
    /// Expiry δ (`delta_exp_bps`).
    pub delta_exp_bps: u16,
    /// Steps of sustained growth (`kappa_growth_n`).
    pub kappa_growth_n: u8,
    /// Freshness of the completed bucket (`ref_max_age_s`).
    pub ref_max_age_s: u8,
}

impl ExitParams {
    /// Defaults (ECON §8.2, §15): κ_max 25 %, κ_base 0, δ_liq 2 %, δ_exp 1 %, growth over
    /// 3 steps, reference fresh within 2 s.
    pub const DEFAULT: Self = Self {
        kappa_liq_bps: 2_500,
        kappa_base_bps: 0,
        delta_liq_bps: 200,
        delta_exp_bps: 100,
        kappa_growth_n: 3,
        ref_max_age_s: 2,
    };
}

impl Default for ExitParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Reference liquidity (ECON §8.2, V2-14; M4F-1):
///
/// ```text
/// L_b = max(L_live, min(max(L_ema', L_slow), cap))      cap = depth_cap, or no cap for None
/// ```
///
/// The deepest term gives the lowest reference cost, so an LP pull in a quiet second cannot
/// loosen the bound. `L_live` is the venue's `√(X·Y)` (`L_ema` when the venue has no price, as
/// the sim) and is never capped.
///
/// `L_ema' = L_ema`, except while the post-change reference guard is set (`ema.guard = 1`;
/// M4R, **PROPOSED**, owner item O18): then `L_ema' = min(L_ema, L_mlo)`, `L_mlo` the lower
/// median of the ring's bucket liquidity lows ([`ring_liq_lower_median`]). A restart's warm-up
/// seeds `L_ema` from the buckets' **highs** (`observe::integrate`), which capital pushes up
/// with a deposit around each crank; the guard keeps such a push from raising the reference
/// liquidity, and so from blocking the exit bound, until `L_ema` agrees with the lows within
/// `REF_GUARD_TOL_BPS` (an honest switch: at the end of the warm-up).
///
/// `depth_cap` (M4F-1, defense in depth) is the pool's locked depth `D` itself
/// ([`ref_depth_cap`] at `λ` = 100 %) on a canonical PumpSwap pool — since M6R-1 (ECON #105) and
/// M6E-1, its `λ_min` being [`PUMPSWAP_LOCK_MIN_BPS`](crate::consts::PUMPSWAP_LOCK_MIN_BPS) = 100 %
/// since OWNER-M6D-1 (ECON #106) — and, since AIR-14, on a Raydium market, whose cap was
/// `⌊D·10⁴/pool_lock_min_bps⌋` (2D at the default), the most depth Raydium sizing credits. As
/// `D ≤ L_live` on the pool the exit reads, `L_b = L_live` there: a third-party deposit is credited
/// to no exit (at `2D` a deposit held into a deadline and pulled there held a position's expiry up
/// for tens of minutes: M6E-1 on PumpSwap, AIR-14 on Raydium), and a push of `L_ema` or `L_slow`
/// by deposit-wrapped cranks reaches no reference. What the cap gives up is V2-14's layer above
/// `D`: a buyback wrapped in an unlocked LP's own withdrawal is priced on the drained pool, at
/// worst at `D`. `D` is deposit- and withdraw-invariant up to the whole-bps floor of `λ` (at most
/// 1 bp of the pool's `√(X·Y)` in that instruction), so no capital-only bundle moves the cap
/// materially. `None` on the pump curve and the mock, which have no locked depth and no pool
/// selection (no switch, no guard), as before.
///
/// Errors: `InvalidInput` for a corrupted ring while the guard is set.
pub fn ref_liquidity(
    l_live: u128,
    ema: &Ema,
    ring: &Ring,
    depth_cap: Option<u128>,
) -> Result<u128, MathError> {
    let credited = guarded_ema_liquidity(ema, ring)?.max(ema.liq_slow);
    let credited = depth_cap.map_or(credited, |cap| credited.min(cap));
    Ok(l_live.max(credited))
}

/// The guarded EMA liquidity `L_ema'` (M4R): `min(L_ema, L_mlo)` while the post-change reference
/// guard is set (`ema.guard = 1`), else `L_ema`; `L_mlo` the lower median of the ring's bucket
/// liquidity lows ([`ring_liq_lower_median`]). `ema` and `ring` are the post-roll state (ARCH
/// §3.0), which no instruction's own observation is in: nothing inside the transaction moves it.
///
/// Errors: `InvalidInput` for a corrupted ring while the guard is set.
pub fn guarded_ema_liquidity(ema: &Ema, ring: &Ring) -> Result<u128, MathError> {
    if ema.guard != 0 {
        Ok(ema.liq.min(ring_liq_lower_median(ring)?))
    } else {
        Ok(ema.liq)
    }
}

/// The liquidity the liquidation **spot leg** is judged at (FITS-2 (a), owner decision of
/// 2026-09-29; ECON §5.5; ARCH §3.6 precondition 3, §4.5): `max(L_live, L_floor)`, `L_live` the
/// venue's `√(X·Y)` on the pre-trade state (`L_ema` on a venue without a price) and `L_floor =
/// L_ema`, except while the post-change guard is set (`ema.guard = 1`), when it is `min(L_ema,
/// L_mhi)`, `L_mhi` the upper median of the ring's bucket liquidity **highs**
/// ([`ring_liq_upper_median`]; AIR-17). The spot leg's risk quote is `B̂(Q; reprice(max(L_live,
/// L_floor), P_rs))`.
///
/// Why: `L_live` alone is the one input of "CR-liquidatable" a transaction can move by itself —
/// an unlocked LP's `[withdraw, liquidate, deposit]` drains it inside the bundle and turns the
/// spot leg true at a depth nobody trades at (FITS-2: a position below about half its open cap
/// force-liquidated in a narrow band under its honest threshold). `L_floor` comes from the
/// post-roll state, which no instruction's own observation enters, so a withdrawal in the same
/// transaction cannot lower the maximum below it. A deposit raises `L_live` and only makes the leg
/// harder to meet (as before). A pushed `L_ema` makes it harder too, but wherever `P_rs ≥ P_ema`
/// (a pump) only where the EMA leg — judged on the ring's cached `reprice(L_ema, P_ema)`, the same
/// pushed `L_ema` in its newest entry — is false already: `B̂` rises with `P` and falls with `L`,
/// and `L_floor ≤ L_ema`.
///
/// AIR-17, the guard: the floor used to be [`guarded_ema_liquidity`], `min(L_ema, L_mlo)`, the
/// lower median of the bucket liquidity **lows** — the exit reference's guard. A bucket low is the
/// minimum of its second, so an unlocked LP's `[withdraw, crank_price, deposit]` in 4 of every 8
/// seconds set `L_mlo` to the drained depth even with the keeper observing every second, and the
/// same wraps kept the guard from clearing: a liquidation wrapped in the LP's withdrawal was judged
/// at the drained depth, which forced positions in the spike-and-retreat band (the EMA leg true,
/// `P_rs < P_ema`). A bucket high is the maximum of its second: one honest observation in it
/// keeps it at the pool's depth, and lowering `L_mhi` takes 5 of 8 attacker-only seconds, which
/// the keeper's per-second coverage while the guard is set denies. A deposit-wrapped crank in 4 of
/// every 8 seconds can raise `L_mhi`, so the floor rises to at most `L_ema` (`min`): such a push
/// defers, never forces, and in a pump defers nothing the EMA leg does not.
///
/// What it does not stop (FITX-1, ECON §12.12 residual 10; owner item O18): a low `L_ema` reached
/// **genuinely** — the unlocked LP withdraws for real for a few seconds, then re-deposits and
/// freezes it with withdraw-wrapped cranks, which the keeper's per-second coverage holds but
/// cannot lift — is read here like any other, and forces exactly the liquidations the same LP's
/// withdrawal for good forces.
///
/// A genuinely withdrawn pool: the spot leg waits at most until `L_ema` (τ = `EMA_TAU_S` = 5 s)
/// has decayed toward the new depth. The EMA leg reads the same decayed `L_ema` in its newest ring
/// entry, so wherever `P_rs ≥ P_ema` (a pump, the case liquidations are for) the spot leg at
/// `L_ema` holds whenever the EMA leg's newest entry does: the rule adds no delay beyond the EMA
/// leg's own `liq_persist_n`-entry lag. The exit bound's `L_b` ([`ref_liquidity`]) keeps the guard
/// on the lows.
///
/// Errors: `InvalidInput` for a corrupted ring while the guard is set.
pub fn spot_liquidity(l_live: u128, ema: &Ema, ring: &Ring) -> Result<u128, MathError> {
    let floor = if ema.guard != 0 {
        ema.liq.min(ring_liq_upper_median(ring)?)
    } else {
        ema.liq
    };
    Ok(l_live.max(floor))
}

/// The Raydium (M4F-1) and PumpSwap (M6R-1) cap of [`ref_liquidity`]'s EMA terms:
/// `⌊D·10⁴/λ⌋` ([`depth_at_lambda`]), `D` the pool's locked depth (`Market.pool_d`, refreshed
/// at every executable observation). The program passes `λ` = 100 % on both venues, i.e. `D`
/// itself (PumpSwap: [`PUMPSWAP_LOCK_MIN_BPS`](crate::consts::PUMPSWAP_LOCK_MIN_BPS), OWNER-M6D-1,
/// ECON #106, M6E-1; Raydium since AIR-14, `pool_lock_min_bps` before); 0 for `λ = 0`, which
/// leaves `L_b = L_live`.
pub fn ref_depth_cap(locked_depth: u128, lambda_min_bps: u32) -> u128 {
    depth_at_lambda(locked_depth, u64::from(lambda_min_bps))
}

/// `κ_eff = min(κ_max, κ_base + (EMA_TAU_S + 1)·g)` in bps, `g` the sustained growth in
/// bps/s (ECON §2.1, §8.2): in steady exponential growth the EMA lags spot by ≈ (τ + 1)·g.
pub fn kappa_eff(g_bps: u64, params: &ExitParams) -> u16 {
    let k = u128::from(EMA_TAU_S)
        .saturating_add(1)
        .saturating_mul(u128::from(g_bps))
        .saturating_add(u128::from(params.kappa_base_bps))
        .min(u128::from(params.kappa_liq_bps));
    u16::try_from(k).unwrap_or(params.kappa_liq_bps)
}

/// Liquidation reference price (ECON §2.1, §8.2; ARCH §3.6 step 6):
///
/// ```text
/// g     = sustained_growth(ring, kappa_growth_n)
/// κ_eff = min(κ_max, κ_base + (EMA_TAU_S + 1)·g)
/// lo_x  = ⌊lb_lo·(10⁴ + g·max(0, now − lb_sec))/10⁴⌋
/// P_ref = min( max(P_ema, lo_x), ⌊P_ema·(10⁴ + κ_eff)/10⁴⌋, ⌊lo_x·(10⁴ + κ_eff)/10⁴⌋ )
/// ```
///
/// The last term (v0.4.1, R4B-2) keeps a `P_ema` that one attacker-only second pushed and
/// later seconds froze from lifting the bound: a frozen EMA has no sustained growth, so
/// the reference stays at the honest bucket low; in a genuine pump `P_ema ≤ lo_x` and it
/// never binds. The bound is then `⌊B̂_ref·(10⁴ + delta_liq_bps)/10⁴⌋` ([`max_in_liq`]).
///
/// `Ok(None)` if no completed bucket is fresh within `ref_max_age_s` (`ReferenceStale`:
/// crank first). Errors: `Overflow` if `P_ref` itself exceeds `u128`; `InvalidInput` for a
/// corrupted ring.
pub fn liq_reference(
    ema: &Ema,
    ring: &Ring,
    last: &LastBucket,
    now: i64,
    params: &ExitParams,
) -> Result<Option<u128>, MathError> {
    if !last.is_fresh(now, params.ref_max_age_s) {
        return Ok(None);
    }
    let g = sustained_growth(ring, params.kappa_growth_n)?;
    let kappa = u128::from(kappa_eff(g, params));
    let bps = u128::from(BPS);
    // Fresh ⇒ age ≤ ref_max_age_s ≤ 255, so g·age < 2⁷².
    let age = u128::try_from(i128::from(now).saturating_sub(i128::from(last.sec))).unwrap_or(0);
    let growth = u128::from(g)
        .checked_mul(age)
        .and_then(|x| x.checked_add(bps))
        .ok_or(MathError::Overflow)?;
    let widen = bps.saturating_add(kappa);
    // Past u128 a term cannot be the minimum of a finite one: `None` stands for "above".
    let lo_x = above_u128(mul_div(last.lo, growth, bps, Rounding::Floor))?;
    let ema_cap = above_u128(mul_div(ema.price, widen, bps, Rounding::Floor))?;
    let lo_cap = match lo_x {
        Some(x) => above_u128(mul_div(x, widen, bps, Rounding::Floor))?,
        None => None,
    };
    let hi = lo_x.map(|x| x.max(ema.price));
    [hi, ema_cap, lo_cap]
        .into_iter()
        .flatten()
        .min()
        .map(Some)
        .ok_or(MathError::Overflow)
}

fn above_u128(r: Result<u128, MathError>) -> Result<Option<u128>, MathError> {
    match r {
        Ok(x) => Ok(Some(x)),
        Err(MathError::Overflow) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Expiry reference price (ECON §2.1, §8.2; ARCH §3.7): `P_ref = min(P_ema, min over the
/// ring of p_i, lb_lo)`, the lowest recent robust price (nothing is urgent at expiry); the
/// bound is `⌊B̂_ref·(10⁴ + delta_exp_bps)/10⁴⌋` ([`max_in_exp`]). `Ok(None)` if no
/// completed bucket is fresh. Errors: `InvalidInput` for a corrupted ring.
pub fn expiry_reference(
    ema: &Ema,
    ring: &Ring,
    last: &LastBucket,
    now: i64,
    params: &ExitParams,
) -> Result<Option<u128>, MathError> {
    ring.check()?;
    if !last.is_fresh(now, params.ref_max_age_s) {
        return Ok(None);
    }
    Ok(Some(
        ring.iter()
            .map(|e| e.p)
            .fold(ema.price.min(last.lo), u128::min),
    ))
}

/// The expiry fee on the reference cost, `π_ref = ⌈expiry_fee_bps·B̂_ref/10⁴⌉` (ECON §2.1,
/// §8.2; ARCH §3.7): the part of an expiry's reserve beyond rent in [`max_in_exp`].
///
/// Errors: `Overflow` (only for a fee above 10⁴·u64::MAX / B̂_ref, i.e. never in bounds).
pub fn pi_ref(b_ref: u64, expiry_fee_bps: u16) -> Result<u64, MathError> {
    mul_bps_u64(b_ref, u64::from(expiry_fee_bps), Rounding::Ceil)
}

/// `⌊b·(10⁴ + bps)/10⁴⌋` as `i128` (exact: `b < 2⁶⁴`, `bps < 2¹⁶`).
fn scaled(b: u64, bps: u16) -> Option<i128> {
    let s = i128::from(BPS);
    i128::from(b)
        .checked_mul(s.checked_add(i128::from(bps))?)?
        .checked_div(s)
}

/// `V − K_s − ert`: the most a buyback may spend so that S1 stays disjoint (ECON §5.6).
fn s1_budget(v: u64, k_s: u64, ert: u64) -> Option<i128> {
    i128::from(v)
        .checked_sub(i128::from(k_s))?
        .checked_sub(i128::from(ert))
}

/// Liquidation bound (ECON §8.2 v0.4.1; ARCH §3.6 step 6, §5.2; SPEC §5.2):
///
/// ```text
/// max_in = min( ⌊B̂_ref·(10⁴ + δ_liq)/10⁴⌋,  V − K_s − ert )
/// ```
///
/// No rent or penalty reserve (R4A-1, R4B-1): any reserve opens a dead band in which a due
/// liquidation waits while the S1 legs are false, so honest liquidations in fast pumps gap
/// into S1. Liveness (I-E11): a liquidatable, buyback-allowed position with a fresh
/// reference executes as soon as `B̂_live ≤ ⌊B̂_ref·(10⁴ + δ)/10⁴⌋`.
///
/// `None` if the minimum is negative (`V − K_s − ert < 0`: no buyback can be afforded).
/// `v`, `k_s` and `ert` are the position's (`ert` its effective venue-rent tolerance, 0 on
/// Raydium).
pub fn max_in_liq(b_ref: u64, delta_bps: u16, v: u64, k_s: u64, ert: u64) -> Option<u64> {
    let m = scaled(b_ref, delta_bps)?.min(s1_budget(v, k_s, ert)?);
    u64::try_from(m).ok()
}

/// Expiry bound (ECON §8.2; ARCH §3.7, §5.2): rent and the expiry fee stay senior unless
/// the fair cost itself leaves no room for them:
///
/// ```text
/// max_in = min( ⌊B̂_ref·(10⁴ + δ_exp)/10⁴⌋,  V − K_s − ert,  max(V − R − π_ref − ert, ⌊B̂_ref·(10⁴ + ε)/10⁴⌋) )
/// ```
///
/// with `R = R_due`, `π_ref` from [`pi_ref`] and ε the position's `exec_tol_bps` (the
/// expiry bound's tolerance since v0.4.1). `None` if the minimum is negative.
#[allow(clippy::too_many_arguments)]
pub fn max_in_exp(
    b_ref: u64,
    delta_bps: u16,
    eps_bps: u16,
    v: u64,
    k_s: u64,
    r: u64,
    pi_ref: u64,
    ert: u64,
) -> Option<u64> {
    let senior = i128::from(v)
        .checked_sub(i128::from(r))?
        .checked_sub(i128::from(pi_ref))?
        .checked_sub(i128::from(ert))?
        .max(scaled(b_ref, eps_bps)?);
    let m = scaled(b_ref, delta_bps)?
        .min(s1_budget(v, k_s, ert)?)
        .min(senior);
    u64::try_from(m).ok()
}

const _: () = assert!(core::mem::size_of::<ExitParams>() == 10);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::observe::RingEntry;

    const WAD: u128 = crate::num::WAD;

    fn state(prices: &[(i64, u128)], lb: (i64, u128)) -> (Ema, Ring, LastBucket) {
        let mut ring = Ring::default();
        for &(ts, p) in prices {
            ring.push(RingEntry {
                p,
                l: 1,
                lo: p,
                hi: p,
                ts,
                ..RingEntry::default()
            })
            .unwrap();
        }
        let (ts, p) = *prices.last().unwrap();
        let ema = Ema {
            price: p,
            liq: 1,
            liq_slow: 1,
            ts,
            slow_ts: ts,
            count: 3,
            warm: 0,
            guard: 0,
            _pad: [0; 6],
        };
        let last = LastBucket {
            lo: lb.1,
            hi: lb.1,
            sec: lb.0,
            _pad: [0; 8],
        };
        (ema, ring, last)
    }

    #[test]
    fn liquidation_reference_follows_sustained_growth() {
        let prm = ExitParams::DEFAULT;
        // +1 %/s for three steps: g = 100 bps/s, κ_eff = 600 bps.
        let p = [
            (0, 100 * WAD),
            (1, 101 * WAD),
            (2, 10_201 * WAD / 100),
            (3, 1_030_301 * WAD / 10_000),
        ];
        let (ema, ring, last) = state(&p, (2, 110 * WAD));
        let r = liq_reference(&ema, &ring, &last, 3, &prm).unwrap().unwrap();
        // lo_x = lo·(1 + 1 %·1 s) = 111.1 WAD, capped at P_ema·1.06 (and lo_x·1.06).
        assert_eq!(r, (ema.price * 10_600 / 10_000).min(1111 * WAD / 10));
        // Stale bucket.
        assert_eq!(liq_reference(&ema, &ring, &last, 5, &prm).unwrap(), None);
        // A flat ring gives no growth: P_ref = min(max(P_ema, lo), P_ema, lo) = P_ema.
        let flat = [
            (0, 100 * WAD),
            (1, 100 * WAD),
            (2, 100 * WAD),
            (3, 100 * WAD),
        ];
        let (ema, ring, last) = state(&flat, (3, 150 * WAD));
        assert_eq!(
            liq_reference(&ema, &ring, &last, 3, &prm).unwrap(),
            Some(100 * WAD)
        );
        assert_eq!(
            expiry_reference(&ema, &ring, &last, 3, &prm).unwrap(),
            Some(100 * WAD)
        );
    }

    #[test]
    fn pushed_and_frozen_ema_cannot_lift_the_reference() {
        // v0.4.1 (R4B-2): P_ema pushed 16 % above an honest bucket low, no sustained growth.
        let prm = ExitParams {
            kappa_base_bps: 1_000,
            ..ExitParams::DEFAULT
        };
        let (mut ema, ring, last) = state(&[(0, 100 * WAD), (1, 100 * WAD)], (1, 100 * WAD));
        ema.price = 116 * WAD;
        // v0.4: min(max(116, 100), 116·1.1) = 116; v0.4.1 also ≤ 100·1.1 = 110.
        assert_eq!(
            liq_reference(&ema, &ring, &last, 1, &prm).unwrap(),
            Some(110 * WAD)
        );
    }

    #[test]
    fn reference_overflow_edges() {
        let prm = ExitParams {
            kappa_base_bps: 100,
            ..ExitParams::DEFAULT
        };
        let (mut ema, ring, mut last) = state(&[(0, 1), (1, 1)], (1, u128::MAX));
        ema.price = u128::MAX;
        // lo_x = MAX, both caps (×1.01) above u128: P_ref = max(P_ema, lo_x) = MAX.
        assert_eq!(
            liq_reference(&ema, &ring, &last, 1, &prm).unwrap(),
            Some(u128::MAX)
        );
        last.lo = 7;
        assert_eq!(liq_reference(&ema, &ring, &last, 1, &prm).unwrap(), Some(7));
    }

    #[test]
    fn max_in_terms() {
        // Liquidation: bound or S1 budget, no reserve.
        assert_eq!(max_in_liq(1_000, 200, 10_000, 100, 0), Some(1_020));
        assert_eq!(max_in_liq(1_000, 200, 1_100, 100, 0), Some(1_000));
        assert_eq!(max_in_liq(1_000, 200, 1_100, 100, 50), Some(950));
        assert_eq!(max_in_liq(1_000, 200, 100, 200, 0), None);
        assert_eq!(
            max_in_liq(u64::MAX, u16::MAX, u64::MAX, 0, 0),
            Some(u64::MAX)
        );
        // Expiry: rent and fee senior unless the fair cost needs the room.
        let pi = pi_ref(1_000, 200).unwrap();
        assert_eq!(pi, 20);
        assert_eq!(
            max_in_exp(1_000, 100, 10, 10_000, 100, 50, pi, 0),
            Some(1_010)
        );
        assert_eq!(
            max_in_exp(1_000, 500, 10, 1_200, 100, 1_000, pi, 0),
            Some(1_001)
        );
        assert_eq!(max_in_exp(1_000, 500, 10, 1_060, 0, 20, pi, 0), Some(1_020));
        assert_eq!(max_in_exp(1_000, 100, 10, 100, 200, 0, 0, 0), None);
        assert_eq!(pi_ref(1_001, 200), Ok(21));
    }

    /// `L_b` with and without the post-change reference guard (M4R).
    #[test]
    fn reference_liquidity_under_the_guard() {
        let (mut ema, mut ring, _) = state(&[(0, WAD), (1, WAD), (2, WAD)], (2, WAD));
        // Bucket lows 300, 200, 100 (lower median 200); L_ema 900, L_slow 150.
        for (e, llo) in ring.entries.iter_mut().zip([300u128, 200, 100]) {
            e.llo = llo;
        }
        ema.liq = 900;
        ema.liq_slow = 150;
        assert_eq!(ref_liquidity(50, &ema, &ring, None), Ok(900));
        ema.guard = 1;
        assert_eq!(ref_liquidity(50, &ema, &ring, None), Ok(200));
        assert_eq!(ref_liquidity(250, &ema, &ring, None), Ok(250));
        ema.liq_slow = 400;
        assert_eq!(ref_liquidity(250, &ema, &ring, None), Ok(400));
        // L_ema below the lows: the guard changes nothing.
        ema.liq = 120;
        ema.liq_slow = 1;
        assert_eq!(ref_liquidity(50, &ema, &ring, None), Ok(120));
        ring.head = 9;
        assert_eq!(
            ref_liquidity(50, &ema, &ring, None),
            Err(MathError::InvalidInput)
        );
        ema.guard = 0;
        assert_eq!(ref_liquidity(50, &ema, &ring, None), Ok(120));
    }

    /// FITS-2 (a): the spot leg's liquidity is `max(L_live, L_floor)` — a drained `L_live` never
    /// takes it below the EMA's depth (under the guard `min(L_ema, L_mhi)`, AIR-17); a deeper
    /// `L_live` is kept; `L_slow` and the depth cap do not enter.
    #[test]
    fn spot_liquidity_is_the_live_depth_floored_at_the_guarded_ema() {
        let (mut ema, mut ring, _) = state(&[(0, WAD), (1, WAD), (2, WAD)], (2, WAD));
        for (e, (llo, lhi)) in
            ring.entries
                .iter_mut()
                .zip([(300u128, 1_000u128), (200, 700), (100, 600)])
        {
            (e.llo, e.lhi) = (llo, lhi);
        }
        ema.liq = 900;
        ema.liq_slow = 5_000;
        // A same-transaction withdrawal drains L_live to 50: the leg is judged at L_ema.
        assert_eq!(guarded_ema_liquidity(&ema, &ring), Ok(900));
        assert_eq!(spot_liquidity(50, &ema, &ring), Ok(900));
        assert_eq!(spot_liquidity(1_200, &ema, &ring), Ok(1_200));
        // Under the guard the floor is min(L_ema, L_mhi) = 700, the upper median of the highs
        // (AIR-17); the exit reference's L_ema' = min(L_ema, L_mlo) = 200.
        ema.guard = 1;
        assert_eq!(guarded_ema_liquidity(&ema, &ring), Ok(200));
        assert_eq!(spot_liquidity(50, &ema, &ring), Ok(700));
        assert_eq!(spot_liquidity(250, &ema, &ring), Ok(700));
        assert_eq!(spot_liquidity(800, &ema, &ring), Ok(800));
        // Highs above L_ema (a deposit push): never above L_ema.
        ema.liq = 650;
        assert_eq!(spot_liquidity(50, &ema, &ring), Ok(650));
        ema.liq = 900;
        // The exit bound keeps its own terms (L_slow, the cap).
        assert_eq!(ref_liquidity(50, &ema, &ring, None), Ok(5_000));
        ring.head = 9;
        assert_eq!(
            spot_liquidity(50, &ema, &ring),
            Err(MathError::InvalidInput)
        );
        ema.guard = 0;
        assert_eq!(spot_liquidity(50, &ema, &ring), Ok(900));
    }

    /// AIR-17: a Raydium adoption (the anchor, `L_slow ≤ D`, marked), then the keeper's honest
    /// observation at the pool's depth `H` in every second and an unlocked LP's `[withdraw,
    /// crank_price, deposit]` — a record at the drained depth `D` — in 4 of every 8 seconds
    /// (`wrapped`), or a deposit-wrapped crank (a record at `pushed`) in every second.
    fn guarded_pool(wrapped: Option<u128>, pushed: Option<u128>) -> crate::observe::Observation {
        const U: u128 = 1_000_000_000;
        let prm = crate::observe::EmaParams::DEFAULT;
        let mut o = crate::observe::Observation::new(0, 2 * WAD, 3_000 * U).unwrap();
        for t in 1..4i64 {
            o.record(t, 2 * WAD, 3_000 * U).unwrap();
            o.roll(t + 1, &prm).unwrap();
        }
        o.rewrite_liquidity(
            4_000 * U,
            crate::observe::VenueChange::RaydiumLocked {
                slow_cap: 1_500 * U,
            },
        )
        .unwrap();
        o.mark_restart();
        for t in 20..60i64 {
            o.record(t, 2 * WAD, 3_000 * U).unwrap();
            if let Some(d) = wrapped.filter(|_| t % 8 < 4) {
                o.record(t, 2 * WAD, d).unwrap();
            }
            if let Some(e) = pushed {
                o.record(t, 2 * WAD, e).unwrap();
            }
            o.roll(t + 1, &prm).unwrap();
        }
        o
    }

    /// AIR-17: while the post-change guard is set, the spot leg's floor is `min(L_ema, L_mhi)` —
    /// the upper median of the bucket liquidity **highs**, which a withdrawal in a second the
    /// keeper also observes cannot lower — not the lower median of the lows, which the wraps set
    /// to the drained depth. A liquidation wrapped in the same LP's withdrawal (`L_live = D`) is
    /// judged at the pool's depth, not at `D`; a deposit push can lift the floor no higher than
    /// `L_ema` (the bound FITS-2's argument needs) and, as before, forces nothing.
    #[test]
    fn withdraw_wrapped_cranks_do_not_lower_the_guarded_spot_floor() {
        const U: u128 = 1_000_000_000;
        let (h, d) = (3_000 * U, 1_500 * U);
        let o = guarded_pool(Some(d), None);
        assert_eq!((o.ema.warm, o.ema.guard), (0, 1));
        let (_, _, l_mlo, l_mhi) = crate::observe::ring_medians(&o.ring).unwrap();
        assert_eq!((l_mlo, l_mhi), (d, h));
        assert!(o.ema.liq * 100 > h * 99, "{}", o.ema.liq);
        // The withdraw-wrapped liquidation: L_live drained to D.
        let l_spot = spot_liquidity(d, &o.ema, &o.ring).unwrap();
        assert_eq!(
            l_spot,
            o.ema.liq.min(h),
            "the spot leg judged at {l_spot}, D = {d}"
        );
        // A position the pool keeps 1 % above its liquidation CR (q 2 % of the token reserve at
        // H) is CR-liquidatable at D; at the floor its spot leg stays false.
        let pr = crate::quote::Pricing::RayV4(crate::fees::ray_v4(25, 10_000).unwrap());
        let p = 2 * WAD;
        let (x_h, _) = crate::quote::reprice(h, p).unwrap();
        let q = u64::try_from(x_h / 50).unwrap();
        let b = |l: u128| pr.buy_repriced(q, l, p).map(|b| b.cost);
        let l_bps = 12_500u32;
        let v_eff = u64::try_from(u128::from(b(h).unwrap()) * 12_625 / 10_000).unwrap();
        let leg = |l: u128| crate::risk::spot_leg(true, true, v_eff, b(l), l_bps).unwrap();
        assert!(!leg(h) && leg(d));
        assert!(!leg(l_spot));
        // The exit reference keeps its guard on the lows (M4R).
        assert_eq!(guarded_ema_liquidity(&o.ema, &o.ring), Ok(d));
        // A deposit freeze: the floor follows the pushed highs only up to L_ema.
        let o = guarded_pool(None, Some(9_000 * U));
        assert_eq!(o.ema.guard, 1);
        let l_spot = spot_liquidity(d, &o.ema, &o.ring).unwrap();
        assert!(l_spot <= o.ema.liq, "{l_spot} > {}", o.ema.liq);
    }

    /// M4F-1: on Raydium the EMA terms are capped at `⌊D·10⁴/λ_min⌋`; `L_live` never is, and
    /// the guard still applies under the cap.
    #[test]
    fn reference_liquidity_is_capped_at_the_sized_depth() {
        let (mut ema, mut ring, _) = state(&[(0, WAD), (1, WAD), (2, WAD)], (2, WAD));
        for (e, llo) in ring.entries.iter_mut().zip([300u128, 200, 100]) {
            e.llo = llo;
        }
        // D 250 over λ_min 50 %: the pool is credited with at most 500.
        let cap = ref_depth_cap(250, 5_000);
        assert_eq!(cap, 500);
        ema.liq = 900;
        ema.liq_slow = 150;
        assert_eq!(ref_liquidity(50, &ema, &ring, Some(cap)), Ok(500));
        assert_eq!(ref_liquidity(50, &ema, &ring, None), Ok(900));
        // L_live above the cap is kept.
        assert_eq!(ref_liquidity(700, &ema, &ring, Some(cap)), Ok(700));
        // Below the cap nothing changes.
        ema.liq = 400;
        assert_eq!(ref_liquidity(50, &ema, &ring, Some(cap)), Ok(400));
        // L_slow is capped too.
        ema.liq_slow = 800;
        assert_eq!(ref_liquidity(50, &ema, &ring, Some(cap)), Ok(500));
        // The guard first (min(L_ema, L_mlo) = 200), then the cap on max(200, L_slow).
        ema.liq = 900;
        ema.liq_slow = 150;
        ema.guard = 1;
        assert_eq!(ref_liquidity(50, &ema, &ring, Some(cap)), Ok(200));
        assert_eq!(ref_liquidity(50, &ema, &ring, Some(120)), Ok(120));
        // λ_min = 0 (outside the compiled bounds): cap 0, the live depth alone.
        assert_eq!(ref_depth_cap(250, 0), 0);
        assert_eq!(ref_liquidity(50, &ema, &ring, Some(0)), Ok(50));
        // A saturating cap never binds.
        assert_eq!(ref_depth_cap(u128::MAX, 2_000), u128::MAX);
        // PumpSwap (OWNER-M6D-1, M6E-1): the cap at λ_min = 100 % is D itself, and with the live
        // depth at or above D (a canonical pool) the reference is the live pool whatever the EMA
        // terms credit.
        let pumpswap = ref_depth_cap(250, crate::consts::PUMPSWAP_LOCK_MIN_BPS);
        assert_eq!(pumpswap, 250);
        ema.guard = 0;
        ema.liq = 900;
        ema.liq_slow = 800;
        for l_live in [250u128, 251, 400, 1_000] {
            assert_eq!(
                ref_liquidity(l_live, &ema, &ring, Some(pumpswap)),
                Ok(l_live)
            );
        }
        ema.guard = 1;
        ring.head = 9;
        assert_eq!(
            ref_liquidity(50, &ema, &ring, Some(cap)),
            Err(MathError::InvalidInput)
        );
    }

    #[test]
    fn kappa() {
        let prm = ExitParams::DEFAULT;
        assert_eq!(kappa_eff(0, &prm), 0);
        assert_eq!(kappa_eff(100, &prm), 600);
        assert_eq!(kappa_eff(u64::MAX, &prm), 2_500);
        let base = ExitParams {
            kappa_base_bps: 3_000,
            ..prm
        };
        assert_eq!(kappa_eff(0, &base), 2_500);
    }
}
