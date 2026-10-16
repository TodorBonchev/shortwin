//! Curve buybacks on the executed path: the plain curve buy, pump's synthetic migration
//! `B̂_v3`, the straddle `B̂_str` (V1-9) and the v1.1 two-leg `B̂₂` (ECON §3.1, §10.1–§10.2;
//! ARCH §2.2, §4.3, §5.4; pump-final §5).
//!
//! Since pump's 2026-10-08 redeploy a v3 buy (`buy_v3`) of more than the curve's real token
//! reserve `rT` executes in one instruction ("synthetic migration"): the buyer takes `rT` from
//! the curve, which completes, and the rest `q − rT` from the would-be PumpSwap pool
//! `(curve_base_ata − rT, rQ + n_curve(rT) − pool_migration_fee)`, both parts at the curve's fee
//! schedule. [`curve_synthetic`] prices exactly that (`B̂_v3`, pump-final §5.2). The buy writes
//! `post_complete_base_out` / `post_complete_quote_in` (`pcB` / `pcQ`) on the curve, and
//! `migrate_v2` then seeds the pool with the live `curve_base_ata` and `rQ + pcQ − fee`
//! ([`migrated_pool`] with [`MigrationParams::post_complete_quote_in`]).
//!
//! [`curve_straddle`] (`B̂_str`: all of `q` on the post-migration pool at the PumpSwap tier) is
//! the quote of a `PumpMigrating` market's EMA-leg states, whose `q > rT` can only execute on the
//! real pool once it exists. On a stuck migration FV prices that pool, ρ-scaled after a synthetic
//! pool part ([`migrating_fv`], pump-final §5.4).

use super::{BuyQuote, liquidity, reprice, sol_leg};
use crate::fees::{self, FeeBps, PammFeeSchedule};
use crate::num::Rounding;

/// `migrate_v2`'s token seed on a standard curve completed without a pool part: 206.9 M tokens
/// (R-PUMP §7.4; the EMA leg's deterministic `X_p`, ARCH §4.3; `shortwin_sim.PUMP_POOL_TOKENS`).
pub const STANDARD_POOL_TOKENS: u128 = 206_900_000_000_000;

/// The divisor of `migrate_v2`'s boost `⌊X₁·Y₁/10¹⁵⌋` ([`migration_boost`]).
pub const MIGRATION_BOOST_DIVISOR: u128 = 1_000_000_000_000_000;

/// A pump bonding-curve state `(vT, vQ, rT, rQ)` (R-PUMP §2.1), live or repriced
/// ([`curve_reprice`](super::curve_reprice)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CurveState {
    /// `virtual_token_reserves`.
    pub vt: u128,
    /// `virtual_quote_reserves` (lamports).
    pub vq: u128,
    /// `real_token_reserves`.
    pub rt: u128,
    /// `real_quote_reserves` (lamports; a synthetic buy's pool part is not in it, pump-final V5).
    pub rq: u128,
    /// `complete`: the curve no longer trades (pump reverts with 6005).
    pub complete: bool,
}

/// What `migrate_v2` seeds the canonical pool with (R-PUMP §7.4; ARCH §4.3; pump-final §5.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MigrationParams {
    /// `Global.pool_migration_fee` (@146): lamports the curve keeps (15,000,001 today).
    pub migration_fee: u64,
    /// Tokens seeded into the pool. Live leg: `curve_base_ata.amount − rT`, the would-be pool a
    /// synthetic buy prices its pool part on (donations included, as pump prices it). EMA / FV
    /// leg: [`STANDARD_POOL_TOKENS`], or, on a curve completed with a pool part (`pc ≠ 0`), the
    /// live `curve_base_ata.amount` (`rT = 0`): the tokens pump priced the pool part on and
    /// `migrate_v2` seeds, donations included (pump-final §5.4).
    pub pool_tokens: u128,
    /// `BondingCurve.post_complete_quote_in` (@158): the synthetic buy's quote, which
    /// `migrate_v2` adds to the pool on top of `rQ − fee` (0 on a live curve and on every
    /// non-synthetic one).
    pub post_complete_quote_in: u64,
}

/// The post-migration pool `(X_p, Y_p)` = (tokens, effective SOL) (ECON §3.1; ARCH §4.3;
/// pump-final §5.1): `X_p = pool_tokens`, `Y_p = rQ + n_curve(rT) − pool_migration_fee + pcQ`,
/// with `n_curve(rT)` the curve quote's `n = ⌈rT·vQ/(vT − rT)⌉ + 1` of ECON §3.1 (0 if `rT = 0`),
/// as the sim's straddle.
///
/// It does not depend on the path *before* completion, except through a synthetic completion,
/// which adds `pcQ` (here) and removes `pcB` tokens from the live `curve_base_ata` (the caller's
/// `pool_tokens`). The completing buy adds pump's `⌈·⌉` (v3) or `⌊·⌋ + 1` (v2) to `rQ`, so `Y_p`
/// is at most one lamport above the pool `migrate_v2` creates (higher `Y_p`, higher quote:
/// conservative); on a complete curve (`rT = 0`) it is exact. `migrate_v2`'s boost
/// ([`migration_boost`]) moves part of `Y_p` into the pool's `virtual_quote_reserves` without
/// changing the effective reserve, so it does not change the price (ECON §3.1).
///
/// `None` iff `rT ≥ vT` (with `rT > 0`), the migration fee exceeds `rQ + n_curve(rT)`,
/// `n_curve` overflows `u64`, or `Y_p` overflows `u128`.
pub fn migrated_pool(state: &CurveState, m: &MigrationParams) -> Option<(u128, u128)> {
    let n = if state.rt == 0 {
        0
    } else {
        let rest = state.vt.checked_sub(state.rt)?;
        let n = super::muldiv(state.rt, state.vq, rest, Rounding::Ceil)?.checked_add(1)?;
        u128::from(super::to_u64(n)?)
    };
    let fee = u128::from(m.migration_fee);
    let y = match state.rq.checked_sub(fee) {
        Some(rest) => rest.checked_add(n)?,
        None => n.checked_sub(fee.checked_sub(state.rq)?)?,
    };
    Some((
        m.pool_tokens,
        y.checked_add(u128::from(m.post_complete_quote_in))?,
    ))
}

/// `migrate_v2`'s boost: the quote it moves into the new PumpSwap pool's
/// `virtual_quote_reserves`, `⌊X₁·Y₁/10¹⁵⌋` on the pool it creates, `(X₁, Y₁)` = (the live
/// `curve_base_ata`, `rQ + pcQ − pool_migration_fee`) (P1 correction (b); pump-final §3.4 (d),
/// measured on pump's 2026-10-08 and 2026-10-13 ELFs with and without donations). Without a
/// token donation to `curve_base_ata` it is ECON §3.1's `⌊(Y₁ − pcQ)·2069/10⁴⌋` or one more (a
/// pool part rounds its quote up, so `X₁·Y₁` can pass the next multiple of 10¹⁵); a donation
/// raises `X₁` and with it the boost. The pool's effective reserve stays `Y₁` (vault + boost),
/// so the boost moves no price. `None` iff the quotient exceeds `u128`.
pub fn migration_boost(x1: u128, y1: u128) -> Option<u128> {
    super::muldiv(x1, y1, MIGRATION_BOOST_DIVISOR, Rounding::Floor)
}

/// Buys `q` on the executed path of a curve market before pump's synthetic migration
/// (ECON §3.1, §10.2; ARCH §4.3, §5.4):
///
/// - `q ≤ rT`: the curve itself, [`sol_leg::buy_exact_out`] with `eps1 = true` (ECON's
///   quote) and `curve_fees`;
/// - `q > rT`: `B̂_str(q) = B̂_pool(q; X_p, Y_p)` on the post-migration pool of
///   [`migrated_pool`], at the PumpSwap fee tier of `mcap(Y_p, X_p)`
///   ([`fees::pump_amm`], including the schedule's risk cap).
///
/// Since pump-final §5.3 this prices only a `PumpMigrating` market's states (the EMA leg's ring
/// entries reproject to live-looking curves, `complete = false`): their `q > rT` executes on the
/// real pool after migration. A live curve's `q > rT` is [`curve_synthetic`]
/// ([`CurvePricing`](super::CurvePricing) picks). For `q > rT`, [`BuyQuote::net`] is the pool's
/// pre-fee input (σ divides it by the curve's price, ARCH §4.3) and the post-trade reserves are
/// the pool's.
///
/// Not monotone at `q = rT` (review F-A2): the pool's `Y_p` sits ≈ 0.02 % below the
/// curve's completion price, so `B̂_str(rT + 1)` can be below the curve's `B̂(rT)` by up
/// to ≈ 0.02 % (and in `P` on the EMA leg where `rT(P)` crosses `q`). Each side is
/// monotone; [`CurvePricing::liquidation_price`](super::CurvePricing::liquidation_price)
/// searches them separately.
///
/// `None` iff `q = 0`, the curve is `complete`, the curve quote is `None`, or (for
/// `q > rT`) [`migrated_pool`] is `None`, the pool fee cannot be resolved, or the pool
/// quote is `None` (e.g. `q ≥ X_p`).
pub fn curve_straddle(
    q: u64,
    state: &CurveState,
    curve_fees: &FeeBps,
    m: &MigrationParams,
    pool_fees: &PammFeeSchedule<'_>,
) -> Option<BuyQuote> {
    if q == 0 || state.complete {
        return None;
    }
    if u128::from(q) <= state.rt {
        return sol_leg::buy_exact_out(q, state.vt, state.vq, true, curve_fees);
    }
    let (xp, yp) = migrated_pool(state, m)?;
    let fees = fees::pump_amm(yp, xp, pool_fees).ok()?;
    sol_leg::buy_exact_out(q, xp, yp, false, &fees)
}

/// `B̂_v3(q)`: a live curve's buyback along pump's v3 path, synthetic migration included
/// (pump-final §5.2; owner D3):
///
/// ```text
/// q ≤ rT : sol_leg::buy_exact_out(q, vT, vQ, ε₁ = 1, curve_fees)            (curve_straddle's)
/// q > rT : leg1  = rT > 0 ? buy_exact_out(rT, vT, vQ, ε₁ = 1, φ) : none      n₁ = ⌈rT·vQ/(vT−rT)⌉ + 1
///          (X_s, Y_s) = migrated_pool(state, m)                              Y_s = rQ + n₁ − fee + pcQ
///          leg2  = buy_exact_out(q − rT, X_s, Y_s, ε₁ = 1, φ)               None unless q − rT < X_s
///          B̂_v3 = leg1.cost + leg2.cost, net = n₁ + n₂, post-trade reserves leg2's
/// ```
///
/// `φ` = `synthetic_fees`, the fees pump charges on **both** parts of such a buy: the curve
/// `FeeConfig` tier at the curve's pre-trade market cap on the mint's live supply
/// ([`fees::pump_curve_at_supply`]; P1 correction (a)), capped in the risk view. `curve_fees` (the
/// existing curve path's `φ`, [`fees::pump_curve`]) prices `q ≤ rT` exactly as before. `X_s` is
/// `m.pool_tokens`: the live `curve_base_ata − rT` on the live leg (a donation included, as pump
/// prices it), [`STANDARD_POOL_TOKENS`] on the EMA leg, the box, `B̂_ref` and D4 (a live curve
/// carries no pool part, pump-final G-3).
///
/// **Against pump's charge** (pump-final §5.2; T-PUMP-MATH-V3 (b)): pump charges
/// `n₁ᵖ = ⌈rT·vQ/(vT−rT)⌉` (v3; `⌊·⌋ + 1` on an exact division, measured), then
/// `n₂ᵖ = ⌈Y_sᵖ·(q − rT)/(X_s − q + rT)⌉` on `Y_sᵖ = rQ + n₁ᵖ − fee`, and `⌈n·bᵢ/10⁴⌉` per fee
/// component and part. With `Δ₂ = 1 + ⌈q₂/(X_s − q₂)⌉` and `bᵢ` the non-zero components,
/// `0 ≤ B̂_v3 − charge ≤ (1 + Σᵢ⌈bᵢ/10⁴⌉) + (Δ₂ + Σᵢ⌈Δ₂·bᵢ/10⁴⌉)`, which is `≤ 7` at today's
/// fees for `q₂ ≤ X_s/2` (the per-market cap keeps every position there, pump-final C8). The
/// lower bound holds for every `q`, so `max_in ≥ B̂_v3` never trips pump's 6002.
///
/// Continuous and non-decreasing in `q` at `rT` when `synthetic_fees = curve_fees` (F-A2's dip is
/// gone on live curves). `None` iff `q = 0`, the curve is `complete`, a leg is `None` (in
/// particular `q − rT ≥ X_s`, pump's `NotEnoughTokensToBuy`; `+∞` as `B̂_str` for `q ≥ X_p`), or a
/// sum leaves `u64`.
pub fn curve_synthetic(
    q: u64,
    state: &CurveState,
    curve_fees: &FeeBps,
    synthetic_fees: &FeeBps,
    m: &MigrationParams,
) -> Option<BuyQuote> {
    if q == 0 || state.complete {
        return None;
    }
    let q128 = u128::from(q);
    if q128 <= state.rt {
        return sol_leg::buy_exact_out(q, state.vt, state.vq, true, curve_fees);
    }
    let leg1 = if state.rt == 0 {
        None
    } else {
        Some(sol_leg::buy_exact_out(
            u64::try_from(state.rt).ok()?,
            state.vt,
            state.vq,
            true,
            synthetic_fees,
        )?)
    };
    let (xs, ys) = migrated_pool(state, m)?;
    let rest = u64::try_from(q128.checked_sub(state.rt)?).ok()?;
    let leg2 = sol_leg::buy_exact_out(rest, xs, ys, true, synthetic_fees)?;
    let (c1, n1) = leg1.map_or((0, 0), |l| (l.cost, l.net));
    Some(BuyQuote {
        cost: leg2.cost.checked_add(c1)?,
        net: leg2.net.checked_add(n1)?,
        ..leg2
    })
}

/// v1.1 in-program straddle `B̂₂(q) = B̂_curve(rT) + B̂_pool(q − rT)` (ECON §3.1, §10.2;
/// ARCH §2.2): the position itself completes the curve and buys the rest on the new pool.
/// It understates the MVP bundle's cost and must not be used for MVP pricing (V1-9); pump's
/// synthetic migration is [`curve_synthetic`].
///
/// `cost` and `net` are the sums of both legs; the post-trade reserves are the pool's.
/// For `q ≤ rT` it equals [`curve_straddle`]. `None` iff either leg is `None`.
pub fn curve_two_leg(
    q: u64,
    state: &CurveState,
    curve_fees: &FeeBps,
    m: &MigrationParams,
    pool_fees: &PammFeeSchedule<'_>,
) -> Option<BuyQuote> {
    if q == 0 || state.complete {
        return None;
    }
    let q128 = u128::from(q);
    if q128 <= state.rt {
        return sol_leg::buy_exact_out(q, state.vt, state.vq, true, curve_fees);
    }
    let leg1 = if state.rt == 0 {
        None
    } else {
        Some(sol_leg::buy_exact_out(
            u64::try_from(state.rt).ok()?,
            state.vt,
            state.vq,
            true,
            curve_fees,
        )?)
    };
    let (xp, yp) = migrated_pool(state, m)?;
    let fees = fees::pump_amm(yp, xp, pool_fees).ok()?;
    let rest = u64::try_from(q128.checked_sub(state.rt)?).ok()?;
    let leg2 = sol_leg::buy_exact_out(rest, xp, yp, false, &fees)?;
    let (c1, n1) = leg1.map_or((0, 0), |l| (l.cost, l.net));
    Some(BuyQuote {
        cost: leg2.cost.checked_add(c1)?,
        net: leg2.net.checked_add(n1)?,
        ..leg2
    })
}

/// FV's price on a `PumpMigrating` market after a synthetic pool part (pump-final §5.4):
/// `p' = ⌈⌈p·Y₁/Y₀⌉·X₀/X₁⌉`, i.e. `p·ρ` with `ρ = P(X₁, Y₁)/P(X₀, Y₀)` rounded up (FV higher:
/// lender side). `(X₁, Y₁)` is the pool `migrate_v2` will create, `(X₀, Y₀)` the same curve's
/// pool without the pool part or a donation (`migrated_pool` with [`STANDARD_POOL_TOKENS`] and
/// `pcQ = 0`). `P_ema` tracks the frozen curve, whose final price sits 0.024 % above
/// `P(X₀, Y₀)`; ρ moves it onto the pool that will open. ρ = 1 (`(X₁, Y₁) = (X₀, Y₀)`) returns
/// `p` exactly. Two `mul_div`s with the 256-bit fallback.
///
/// `None` iff `X₁ = 0`, `Y₀ = 0` or a quotient exceeds `u128`.
pub fn migrating_fv_price(p: u128, x0: u128, y0: u128, x1: u128, y1: u128) -> Option<u128> {
    let scaled = super::muldiv(p, y1, y0, Rounding::Ceil)?;
    super::muldiv(scaled, x0, x1, Rounding::Ceil)
}

/// `B̂_FV(q; P)` on a `PumpMigrating` market (ARCH §4.5; pump-final §5.4): the buy of `q` on the
/// pool `migrate_v2` will create, `(X₁, Y₁)` = [`migrated_pool`]`(state, m)`, repriced to
/// `reprice(isqrt(X₁·Y₁), p')` at that pool's own PumpSwap tier ([`fees::pump_amm`] on
/// `(Y₁, X₁)`), `ε₁ = 0` (`shortwin_sim.Market.fv_parts`). `p' = p` without a pool part
/// (`m.post_complete_quote_in = 0`): bit-identical to the FV before pump's v3, the deterministic
/// pool of the standard `m.pool_tokens`. After one, `p'` = [`migrating_fv_price`] with
/// `(X₀, Y₀)` = [`migrated_pool`] on [`STANDARD_POOL_TOKENS`] and `pcQ = 0`.
///
/// `state` is the complete curve as read (`rT = 0`, so `Y₁` is exact), `m.pool_tokens` the
/// standard 206.9e12 without a pool part and the live `curve_base_ata` after one. Whether FV is
/// defined at all is the caller's (pump-final §6.1.5: a migrating curve whose post-complete
/// fields break pump's invariants has none).
///
/// `None` iff a pool, the pool's fee, its liquidity, `p'`, `reprice` or the quote is.
pub fn migrating_fv(
    q: u64,
    p: u128,
    state: &CurveState,
    m: &MigrationParams,
    pool_fees: &PammFeeSchedule<'_>,
) -> Option<BuyQuote> {
    let (x1, y1) = migrated_pool(state, m)?;
    let p = if m.post_complete_quote_in == 0 {
        p
    } else {
        let base = MigrationParams {
            migration_fee: m.migration_fee,
            pool_tokens: STANDARD_POOL_TOKENS,
            post_complete_quote_in: 0,
        };
        let (x0, y0) = migrated_pool(state, &base)?;
        migrating_fv_price(p, x0, y0, x1, y1)?
    };
    let fees = fees::pump_amm(y1, x1, pool_fees).ok()?;
    let l = liquidity(x1, y1).ok()?;
    let (x, y) = reprice(l, p)?;
    sol_leg::buy_exact_out(q, x, y, false, &fees)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::fees::FeeTier;
    use crate::num::WAD;

    const CURVE: FeeBps = FeeBps::new(0, 95, 30);
    const TIERS: [FeeTier; 2] = [
        FeeTier {
            threshold: 0,
            fees: FeeBps::new(2, 93, 30),
        },
        FeeTier {
            threshold: 420_000_000_000,
            fees: FeeBps::new(20, 5, 95),
        },
    ];
    const MIG: MigrationParams = MigrationParams {
        migration_fee: 15_000_001,
        pool_tokens: STANDARD_POOL_TOKENS,
        post_complete_quote_in: 0,
    };

    fn sched() -> PammFeeSchedule<'static> {
        PammFeeSchedule {
            tiers: &TIERS,
            base_supply: 1_000_000_000_000_000,
            creator_fee_configurable: true,
            pool_creator_fee_bps: 0,
            coin_creator_is_default: false,
            cap_bps: None,
        }
    }

    /// R-PUMP §2.1/§7.4: the curve completed by `SfdLRmJp…` ends at vT 279.9e12,
    /// vQ 115,005,359,346, rQ 85,005,359,346; `migrate_v2` then moves 84,990,359,345.
    #[test]
    fn migration_amount() {
        let done = CurveState {
            vt: 279_900_000_000_000,
            vq: 115_005_359_346,
            rt: 0,
            rq: 85_005_359_346,
            complete: true,
        };
        assert_eq!(
            migrated_pool(&done, &MIG),
            Some((206_900_000_000_000, 84_990_359_345))
        );
    }

    /// T-MATH-1 (pump-final §5.1): 98u8RJ's curve after its synthetic completion `2vVRr9eg…`
    /// (`rQ` 85,005,359,190, the pool part `pcQ` 2,642,475,552, `curve_base_ata` 206.9e12 −
    /// `pcB` 6,238,850,918,425 = 200,661,149,081,575 before `migrate_v2`, no donation) gives
    /// exactly the pool `PostCompleteBuyEvent` reported after the buy and `migrate_v2` created.
    #[test]
    fn migrated_pool_adds_the_post_complete_quote() {
        let done = CurveState {
            vt: 279_900_000_000_000,
            vq: 115_005_359_190,
            rt: 0,
            rq: 85_005_359_190,
            complete: true,
        };
        let m = MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: 200_661_149_081_575,
            post_complete_quote_in: 2_642_475_552,
        };
        assert_eq!(
            migrated_pool(&done, &m),
            Some((200_661_149_081_575, 87_632_834_741))
        );
        // Without the pool part: the pool the buy priced its pool part on (`pool_*_before`).
        assert_eq!(
            migrated_pool(&done, &MIG),
            Some((206_900_000_000_000, 84_990_359_189))
        );
        // pcQ is added after the fee: the fee must still be covered by rQ + n.
        let poor = CurveState {
            rq: 15_000_000,
            ..done
        };
        assert_eq!(migrated_pool(&poor, &m), None);
        let edge = CurveState {
            rq: 15_000_001,
            ..done
        };
        assert_eq!(
            migrated_pool(&edge, &m),
            Some((200_661_149_081_575, 2_642_475_552))
        );
        assert_eq!(
            migrated_pool(
                &done,
                &MigrationParams {
                    post_complete_quote_in: u64::MAX,
                    ..m
                }
            ),
            Some((
                200_661_149_081_575,
                85_005_359_190 - 15_000_001 + u128::from(u64::MAX)
            ))
        );
        // The boost migrate_v2 moves into the pool's VQR: ⌊X₁·Y₁/10¹⁵⌋ (correction (b)); with
        // no donation, ⌊(Y₁ − pcQ)·2069/10⁴⌋ = ⌊84,990,359,189·0.2069⌋ here.
        assert_eq!(
            migration_boost(200_661_149_081_575, 87_632_834_741),
            Some(17_584_505_316)
        );
        assert_eq!(84_990_359_189u128 * 2_069 / 10_000, 17_584_505_316);
        assert_eq!(
            migration_boost(u128::MAX, u128::MAX),
            None,
            "the quotient leaves u128"
        );
    }

    #[test]
    fn straddle_prices_all_of_q_on_the_pool() {
        let s = CurveState {
            vt: 400_000_000_000_000,
            vq: 80_475_000_000,
            rt: 120_100_000_000_000,
            rq: 50_475_000_000,
            complete: false,
        };
        let (xp, yp) = migrated_pool(&s, &MIG).unwrap();
        let q = 150_000_000_000_000;
        let b = curve_straddle(q, &s, &CURVE, &MIG, &sched()).unwrap();
        let f = fees::pump_amm(yp, xp, &sched()).unwrap();
        assert_eq!(b, sol_leg::buy_exact_out(q, xp, yp, false, &f).unwrap());
        // The two-leg form is cheaper: the position buys rT on the curve (V1-9).
        let two = curve_two_leg(q, &s, &CURVE, &MIG, &sched()).unwrap();
        assert!(two.cost < b.cost);
        // At or below rT both are the curve quote.
        let rt = u64::try_from(s.rt).unwrap();
        let c = sol_leg::buy_exact_out(rt, s.vt, s.vq, true, &CURVE).unwrap();
        assert_eq!(curve_straddle(rt, &s, &CURVE, &MIG, &sched()), Some(c));
        assert_eq!(curve_two_leg(rt, &s, &CURVE, &MIG, &sched()), Some(c));
        assert!(
            curve_straddle(rt + 1, &s, &CURVE, &MIG, &sched())
                .unwrap()
                .cost
                > c.cost
        );
        // Complete curves and oversize buys have no quote.
        let done = CurveState {
            complete: true,
            ..s
        };
        assert_eq!(curve_straddle(1, &done, &CURVE, &MIG, &sched()), None);
        assert_eq!(
            curve_straddle(206_900_000_000_000, &s, &CURVE, &MIG, &sched()),
            None
        );
        assert_eq!(curve_straddle(0, &s, &CURVE, &MIG, &sched()), None);
    }

    /// pump-final §5.2: `B̂_v3` is the curve leg for `rT` plus the would-be pool's leg for
    /// `q − rT`, both at the synthetic fee with `ε₁ = 1`; the curve path below `rT` is unchanged.
    #[test]
    fn synthetic_buys_the_rest_from_the_would_be_pool() {
        let s = CurveState {
            vt: 400_000_000_000_000,
            vq: 80_475_000_000,
            rt: 120_100_000_000_000,
            rq: 50_475_000_000,
            complete: false,
        };
        let phi = FeeBps::new(0, 80, 25);
        let rt = u64::try_from(s.rt).unwrap();
        let q = 150_000_000_000_000;
        let leg1 = sol_leg::buy_exact_out(rt, s.vt, s.vq, true, &phi).unwrap();
        let (xs, ys) = migrated_pool(&s, &MIG).unwrap();
        assert_eq!(ys, s.rq + u128::from(leg1.net) - 15_000_001);
        let leg2 = sol_leg::buy_exact_out(q - rt, xs, ys, true, &phi).unwrap();
        let b = curve_synthetic(q, &s, &CURVE, &phi, &MIG).unwrap();
        assert_eq!(
            b,
            BuyQuote {
                cost: leg1.cost + leg2.cost,
                net: leg1.net + leg2.net,
                ..leg2
            }
        );
        // At or below rT: the curve quote at the curve path's fee (curve_straddle's).
        for q in [1, rt / 2, rt] {
            assert_eq!(
                curve_synthetic(q, &s, &CURVE, &phi, &MIG),
                curve_straddle(q, &s, &CURVE, &MIG, &sched())
            );
        }
        // Past rT it is far below B̂_str here (most of q on the curve, not the pool).
        assert!(b.cost < curve_straddle(q, &s, &CURVE, &MIG, &sched()).unwrap().cost);
        // Continuous at rT with one schedule: B̂_v3(rT + 1) − B̂_v3(rT) is the pool's 1 token.
        let at = curve_synthetic(rt, &s, &CURVE, &CURVE, &MIG).unwrap();
        let past = curve_synthetic(rt + 1, &s, &CURVE, &CURVE, &MIG).unwrap();
        assert_eq!(
            past.cost - at.cost,
            sol_leg::buy_exact_out(1, xs, ys, true, &CURVE)
                .unwrap()
                .cost
        );
        assert!(past.cost - at.cost <= 4);
        // pump's NotEnoughTokensToBuy: q − rT ≥ X_s has no quote; one token less has.
        let edge = u64::try_from(s.rt + xs).unwrap();
        assert_eq!(curve_synthetic(edge, &s, &CURVE, &phi, &MIG), None);
        assert_eq!(curve_synthetic(u64::MAX, &s, &CURVE, &phi, &MIG), None);
        let half = u64::try_from(s.rt + xs / 2).unwrap();
        assert!(curve_synthetic(half, &s, &CURVE, &phi, &MIG).is_some());
        // Just below X_s the pool leg's n leaves u64: no quote either (+∞).
        assert_eq!(curve_synthetic(edge - 1, &s, &CURVE, &phi, &MIG), None);
        // rT = 0 on a live-looking state (an EMA-leg reprojection past completion): pool only.
        let empty = CurveState { rt: 0, ..s };
        let (xs0, ys0) = migrated_pool(&empty, &MIG).unwrap();
        assert_eq!(
            curve_synthetic(5, &empty, &CURVE, &phi, &MIG),
            sol_leg::buy_exact_out(5, xs0, ys0, true, &phi)
        );
        // No quote on a complete curve or for q = 0.
        let done = CurveState {
            complete: true,
            ..s
        };
        assert_eq!(curve_synthetic(1, &done, &CURVE, &phi, &MIG), None);
        assert_eq!(curve_synthetic(0, &s, &CURVE, &phi, &MIG), None);
    }

    /// ρ on 98u8RJ (pump-final §5.4, T-MATH-4): `(X₀, Y₀)` = (206.9e12, 84,990,359,189) without
    /// the pool part, `(X₁, Y₁)` = (200,661,149,081,575, 87,632,834,741) with it: ρ = 1.06315.
    /// And on `E1bYRCNq…` (the `synthetic_stuck` fixture: `rQ` 14,097,072,881,278, `pcQ`
    /// 21,720,607,727,767, `curve_base_ata` 81,431,414,026,289): ρ = 6.4556.
    #[test]
    fn migrating_fv_price_reprices_to_the_post_buy_pool() {
        let rho = |x0, y0, x1, y1| migrating_fv_price(WAD, x0, y0, x1, y1).unwrap();
        let r = rho(
            206_900_000_000_000,
            84_990_359_189,
            200_661_149_081_575,
            87_632_834_741,
        );
        assert_eq!(r / 100_000_000_000_000, 10_631, "ρ(98u8RJ) = 1.0631…");
        assert_eq!(
            r / 1_000_000_000_000,
            1_063_149,
            "1.063149…, §5.4's 1.06315"
        );
        let e = rho(
            206_900_000_000_000,
            14_097_072_881_278 - 15_000_001,
            81_431_414_026_289,
            14_097_072_881_278 - 15_000_001 + 21_720_607_727_767,
        );
        assert_eq!(e / 100_000_000_000_000, 64_556, "ρ(E1bY) = 6.4556…");
        // ρ = 1 returns p itself; undefined on X₁ = 0 or Y₀ = 0.
        assert_eq!(migrating_fv_price(123_456, 7, 11, 7, 11), Some(123_456));
        assert_eq!(migrating_fv_price(1, 1, 1, 0, 1), None);
        assert_eq!(migrating_fv_price(1, 1, 0, 1, 1), None);
        // Rounded up: never below the exact product.
        // ⌈⌈10·5/3⌉·3/7⌉ = ⌈17·3/7⌉ = 8 ≥ 150/21.
        assert_eq!(migrating_fv_price(10, 3, 3, 7, 5), Some(8));
    }

    /// pump-final §5.4: FV on a migrating curve without a pool part is the pool `migrate_v2`
    /// creates from the standard 206.9e12, at `P`; after one it is the post-buy pool at `P·ρ`.
    #[test]
    fn migrating_fv_follows_the_pool_part() {
        let done = CurveState {
            vt: 279_900_000_000_000,
            vq: 115_005_359_190,
            rt: 0,
            rq: 85_005_359_190,
            complete: true,
        };
        let q = 10_000_000_000_000;
        let (x0, y0) = migrated_pool(&done, &MIG).unwrap();
        let p0 = y0 * WAD / x0;
        // No pool part: today's formula (the program's fv_repriced before pump-final §6.2).
        let old = |p: u128| {
            let f = fees::pump_amm(y0, x0, &sched()).unwrap();
            let (x, y) = reprice(liquidity(x0, y0).unwrap(), p).unwrap();
            sol_leg::buy_exact_out(q, x, y, false, &f)
        };
        for p in [p0, p0 * 2, p0 / 3] {
            assert_eq!(migrating_fv(q, p, &done, &MIG, &sched()), old(p));
        }
        // After 98u8RJ's pool part: FV at P(X₀, Y₀) prices the created pool at its own price.
        let m = MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: 200_661_149_081_575,
            post_complete_quote_in: 2_642_475_552,
        };
        let (x1, y1) = migrated_pool(&done, &m).unwrap();
        let fv = migrating_fv(q, p0, &done, &m, &sched()).unwrap();
        let f1 = fees::pump_amm(y1, x1, &sched()).unwrap();
        let on_pool = sol_leg::buy_exact_out(q, x1, y1, false, &f1).unwrap();
        // reprice's √P granularity (≈ 5·10⁻⁸ here) is the only difference.
        assert!(
            fv.cost.abs_diff(on_pool.cost) * 1_000_000 <= on_pool.cost,
            "{} vs {}",
            fv.cost,
            on_pool.cost
        );
        assert!(fv.cost > old(p0).unwrap().cost, "ρ > 1 raises FV");
    }

    /// pump-final §5.4 (T-MATH-4): ρ < 1 is reachable. A small pool part (`pcB` 1e9 tokens at
    /// pump's `⌈⌉` quote) and then a donation of 1e13 tokens to `curve_base_ata`, which
    /// `migrate_v2` seeds with the rest: the pool that opens is cheaper than `P(X₀, Y₀)`, and FV
    /// follows it down.
    #[test]
    fn migrating_fv_price_falls_below_one_after_a_donation() {
        let done = CurveState {
            vt: 279_900_000_000_000,
            vq: 115_005_359_190,
            rt: 0,
            rq: 85_005_359_190,
            complete: true,
        };
        let (x0, y0) = migrated_pool(&done, &MIG).unwrap();
        let pcb = 1_000_000_000;
        // The pool part's quote as pump charges it: ⌈Y₀·pcB/(X₀ − pcB)⌉.
        let pcq = (y0 * pcb).div_ceil(x0 - pcb);
        assert_eq!(pcq, 410_782);
        let m = MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: x0 - pcb + 10_000_000_000_000,
            post_complete_quote_in: u64::try_from(pcq).unwrap(),
        };
        let (x1, y1) = migrated_pool(&done, &m).unwrap();
        assert_eq!((x1, y1), (216_899_000_000_000, 84_990_769_971));
        let rho = migrating_fv_price(WAD, x0, y0, x1, y1).unwrap();
        assert!(rho < WAD, "ρ {rho} < 1");
        assert_eq!(rho / 1_000_000_000_000, 953_904, "ρ = 0.953904…");
        // FV follows: below the FV at the unscaled P (the pool without the pool part).
        let q = 10_000_000_000_000;
        let p0 = y0 * WAD / x0;
        let fv = migrating_fv(q, p0, &done, &m, &sched()).unwrap();
        let unscaled = migrating_fv(q, p0, &done, &MIG, &sched()).unwrap();
        assert!(
            fv.cost < unscaled.cost,
            "ρ < 1 lowers FV: {} vs {}",
            fv.cost,
            unscaled.cost
        );
    }
}
