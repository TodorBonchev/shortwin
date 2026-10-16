//! Owner: risk track. CR and the liquidation legs, "normal buyback allowed", σ, D4 margin, open fee, fair value, `K_s`/`K_e` (ECON §2.1, §3.3, §4, §5.5, §8.4–§8.7; ARCH §4.5).
//!
//! Everything here takes quote results and prices as plain inputs (a quote closure for
//! [`ring_below`] and [`liquidation_price`]); the quote module produces them. A [`Quote`]
//! of `None` is an undefined quote (`Q ≥ X`, a complete curve, a venue that cannot
//! execute), which every predicate treats as `+∞` (ARCH §4.3). Which fee a quote is taken
//! at matters and is the caller's job (ARCH §4.3 "two fee views"): *execution* quotes use
//! the live venue fee, *risk* quotes (every CR, the settlement legs, the refutation, FV)
//! use `min(φ_live, venue_fee_hard_cap_bps)`.
//!
//! The settlement decisions (S1/S2 legs, refutation, "not liquidatable", the S2/S3 amount
//! and the arming clock) are evaluated on the settlement box and live in `settle`
//! (ECON §5.6 v0.4.1; ARCH §3.8, §4.5); v0.3's single-point forms (`s1_legs`, `s2_legs`,
//! `refuted`, `settle_kind`, `fv_ok`, `fv_gate`) are gone (ARCH §9 M0b). They build on the
//! primitives here ([`Budget::exceeded_by`], [`sigma`], [`cr_below`], [`fair_value`]).
//!
//! Only the SPEC-literal D3/D4 readings are implemented (ARCH §9 owner gates): the
//! margin's EMA leg on the newest ring entry's cached reserves, FV on `(P, L)` states of
//! the box and paid at `(P_ema, L_ema)`, S1 whenever all its legs hold.

use crate::error::MathError;
use crate::num::{
    BPS, Rounding, U256, div_wad, mul_bps, mul_bps_u64, mul_div, mul_div_u256, saturating_u64,
    to_u64,
};

/// An executable buyback cost `B̂` in lamports, or `None` when undefined (`+∞`).
pub type Quote = Option<u64>;

/// `WAD·10⁴`: scale of `n·WAD·10⁴/(q·P)` in [`sigma`].
const WAD_BPS: u128 = 10_000_000_000_000_000_000_000;

/// SOL-settlement kind (SPEC §5.3; ECON §8.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SettleKind {
    /// A real gap: the vault cannot cover the buyback. Lenders take `V − K_s`, never capped.
    S1 = 1,
    /// Buyback slippage above `σ_max`. Lenders' take capped at FV.
    S2 = 2,
    /// No executable route for `s3_wait_s`. Lenders' take capped at FV.
    S3 = 3,
}

/// Locked SOL-settlement keeper terms (ARCH §2.3.3 `settle_keeper_*`; SPEC §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettleKeeperTerms {
    /// `settle_keeper_fixed`, lamports.
    pub fixed: u64,
    /// `settle_keeper_bps`, bps of `V`.
    pub bps: u32,
    /// `settle_keeper_cap`, lamports.
    pub cap: u64,
}

impl SettleKeeperTerms {
    /// SPEC §6.3 / ADOPTED Q4 defaults: 0.01 SOL + 1 % of the vault, capped at 0.25 SOL.
    pub const DEFAULT: Self = Self {
        fixed: 10_000_000,
        bps: 100,
        cap: 250_000_000,
    };
}

/// Locked expiry keeper terms (ARCH §2.3.3 `expiry_keeper_*`; ECON §8.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExpiryKeeperTerms {
    /// `expiry_keeper_fixed`, lamports.
    pub fixed: u64,
    /// `expiry_keeper_bps`, bps of `B̂_ref`.
    pub bps: u32,
    /// `expiry_keeper_cap`, lamports.
    pub cap: u64,
}

impl ExpiryKeeperTerms {
    /// ECON §8.4 defaults: 0.001 SOL + 0.1 % of `B̂_ref`, capped at 0.05 SOL.
    pub const DEFAULT: Self = Self {
        fixed: 1_000_000,
        bps: 10,
        cap: 50_000_000,
    };
}

/// The settlement budget of a position: vault `V`, keeper reward `K_s` and venue-side
/// rent tolerance `ert` (ECON §5.6, §8). A buyback is affordable while
/// `B̂ + ert ≤ V − K_s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Budget {
    /// Tracked vault lamports `V`.
    pub v: u64,
    /// `K_s` from [`k_s`] (at most `V`).
    pub k_s: u64,
    /// The position's locked effective `ert` (0 on Raydium).
    pub ert: u64,
}

impl Budget {
    /// `B̂ + ert > V − K_s`: the vault cannot pay this buyback and the keeper (an S1 leg,
    /// ECON §5.6). An undefined quote always exceeds.
    ///
    /// Errors: `InvalidInput` if `K_s > V`.
    pub fn exceeded_by(&self, b_hat: Quote) -> Result<bool, MathError> {
        let room = self
            .v
            .checked_sub(self.k_s)
            .ok_or(MathError::InvalidInput)?;
        Ok(match b_hat {
            None => true,
            Some(b) => u128::from(b).saturating_add(u128::from(self.ert)) > u128::from(room),
        })
    }
}

/// `V_eff = V − R_due`, floored at 0 (ECON §3.3). A rent above the vault leaves no
/// collateral; every CR predicate reads 0 the same way it reads a negative value.
pub const fn v_eff(v: u64, r_due: u64) -> u64 {
    v.saturating_sub(r_due)
}

/// `CR = ⌊V_eff·10⁴/B̂⌋` in bps (ECON §3.3; ARCH §4.5). An undefined quote gives
/// `CR = 0` (SPEC §4/§5.1: no route ⇒ `CR_spot := 0`).
///
/// Errors: `InvalidInput` if `B̂ = 0` (a positive `Q` always costs ≥ 1 lamport);
/// `Overflow` if the ratio exceeds `u64` (only for absurd inputs).
pub fn cr_bps(v_eff: u64, b_hat: Quote) -> Result<u64, MathError> {
    match b_hat {
        None => Ok(0),
        Some(0) => Err(MathError::InvalidInput),
        Some(b) => to_u64(mul_div(
            u128::from(v_eff),
            u128::from(BPS),
            u128::from(b),
            Rounding::Floor,
        )?),
    }
}

/// `CR(V_eff, B̂) < L`, evaluated exactly as `V_eff·10⁴ < L·B̂` (equal to
/// `cr_bps(..) < L` since `L` is an integer; the sims' `V_eff·10⁴ // B̂ < L`). Used by the
/// spot leg, by every ring-entry test of the EMA leg, by the settlement box's
/// "not liquidatable" corner and by FV's `p'` (ECON §5.5, §5.6, §8.6; ARCH §4.5).
///
/// Errors: `InvalidInput` if `B̂ = 0`.
pub fn cr_below(v_eff: u64, b_hat: Quote, l_bps: u32) -> Result<bool, MathError> {
    match b_hat {
        None => Ok(true),
        Some(0) => Err(MathError::InvalidInput),
        Some(b) => {
            let lhs = u128::from(v_eff).saturating_mul(u128::from(BPS));
            let rhs = u128::from(l_bps).saturating_mul(u128::from(b));
            Ok(lhs < rhs)
        }
    }
}

/// The ring predicate `below(x, y)` that [`persist::liq_ema_leg`] takes (ECON §5.5;
/// ARCH §4.5, decision 2): `CR(V_eff, B̂(Q; x_i, y_i)) < L_liq` through [`cr_below`] on a
/// ring entry's cached reprojection `(x_i, y_i) = reprice(l_i, p_i)`, so every CR
/// predicate of the crate rounds identically (sim `Market.status`: `below(quote_xy(Q,
/// X_i, Y_i))`). `quote_at(x, y)` is the position's **risk** quote on that state
/// ([`Pricing::buy`] at the capped fee; on the curve, the straddle on the cached state);
/// `None` (undefined) counts as below. The closure is agnostic to what its two arguments
/// are, so a caller may also pass `(P, L)` with a repricing quote.
///
/// [`persist::liq_ema_leg`]: crate::persist::liq_ema_leg
/// [`Pricing::buy`]: crate::quote::Pricing::buy
pub fn ring_below<F>(
    v_eff: u64,
    cr_liq_bps: u32,
    mut quote_at: F,
) -> impl FnMut(u128, u128) -> Result<bool, MathError>
where
    F: FnMut(u128, u128) -> Quote,
{
    move |x, y| cr_below(v_eff, quote_at(x, y), cr_liq_bps)
}

/// Liquidation price for UIs and keepers (ECON §3.3 "Liquidation price"; ARCH §7.3): the
/// smallest price `P ≥ p_from` (WAD) with `CR(V_eff, quote_at(P)) < L`, found by
/// bisection on the exact predicate [`cr_below`]. `quote_at(P)` is `B̂(Q)` on the state
/// repriced to `P` at the risk fee (`Pricing::buy_repriced` on `L_live` for the spot
/// leg, `L_ema` for the EMA leg); `None` counts as below.
///
/// Returns `Some(p_from)` if the predicate already holds at `p_from`, and `None` if it
/// does not hold even at `u128::MAX`. The result always satisfies the predicate while
/// `P − 1` (if `≥ p_from`) does not; it is *the* liquidation price only when `quote_at` is
/// non-decreasing in `P` above `p_from`. That holds for every constant-product family on
/// `reprice` with fixed fees (a higher price means fewer tokens and more SOL in the pool),
/// but not for the curve straddle on `curve_reprice`, which dips where `rT(P)` crosses
/// `Q` (use [`CurvePricing::liquidation_price`], review F-A2), nor for PumpSwap fees
/// re-resolved per repriced state (a tier threshold lowers the fee as `P` rises). ECON's
/// closed form `s = (−bQ + √(b²Q² + 4(1+φ)·K·Q·b))/(2(1+φ)·√K·Q)` is the real-valued
/// version of the same crossing (continuous fee, no rounding). At most 130 quotes;
/// off-chain only.
///
/// [`CurvePricing::liquidation_price`]: crate::quote::CurvePricing::liquidation_price
///
/// Errors: `InvalidInput` if a quote is 0 lamports (as [`cr_below`]).
pub fn liquidation_price<F>(
    p_from: u128,
    v_eff: u64,
    cr_liq_bps: u32,
    mut quote_at: F,
) -> Result<Option<u128>, MathError>
where
    F: FnMut(u128) -> Quote,
{
    let mut below = |p: u128| cr_below(v_eff, quote_at(p), cr_liq_bps);
    if below(p_from)? {
        return Ok(Some(p_from));
    }
    if !below(u128::MAX)? {
        return Ok(None);
    }
    // Invariant: the predicate fails at `lo` and holds at `hi`.
    let (mut lo, mut hi) = (p_from, u128::MAX);
    while lo.checked_add(1).is_some_and(|next| next < hi) {
        let mid = lo.midpoint(hi);
        if below(mid)? {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(Some(hi))
}

/// Liquidation spot leg (ECON §5.5; ARCH §4.5):
/// `venue_executable ? (fresh ∧ CR(V_eff, B̂_rs) < L) : true`.
///
/// `b_rs` is the risk quote `B̂(Q; reprice(max(L_live, L_ema'), P_rs))` at the robust spot
/// `P_rs = min(P_live, lb_lo)` and the liquidity [`crate::bounds::spot_liquidity`] (FITS-2: a
/// withdrawal inside the transaction cannot take it below the guarded EMA's depth);
/// `fresh` is `lb_sec ≥ now − ref_max_age_s` (without a
/// fresh completed bucket the leg cannot be confirmed). `executable` is
/// `venue_executable` only: the route-broken flag and the fee hard cap never make the
/// leg true (V2-1).
///
/// Errors: `InvalidInput` if `B̂_rs = 0`.
pub fn spot_leg(
    executable: bool,
    fresh: bool,
    v_eff: u64,
    b_rs: Quote,
    l_bps: u32,
) -> Result<bool, MathError> {
    if !executable {
        return Ok(true);
    }
    Ok(fresh && cr_below(v_eff, b_rs, l_bps)?)
}

/// `liquidatable ⇔ (spot leg ∧ EMA leg) ∨ now ≥ deadline` (SPEC §5.1; ECON §5.5).
pub const fn liquidatable(spot_leg: bool, ema_leg: bool, now: i64, deadline: i64) -> bool {
    (spot_leg && ema_leg) || now >= deadline
}

/// Normal buyback allowed: `B̂_live + ert ≤ V − K_s ∧ σ_live ≤ σ_max` (ECON §5.6;
/// ARCH §3.6 step 4), with `B̂_live` and `σ_live` at the **live** fee (execution).
///
/// Errors: as [`Budget::exceeded_by`].
pub fn buyback_allowed(
    budget: &Budget,
    b_live: Quote,
    sigma_live: u64,
    sigma_max_bps: u32,
) -> Result<bool, MathError> {
    Ok(!budget.exceeded_by(b_live)? && sigma_live <= u64::from(sigma_max_bps))
}

/// Marginal price `P = ⌊Y·WAD/X⌋` of a constant-product state (ECON §2).
///
/// Errors: `DivByZero` if `x = 0`.
pub fn price_wad(x: u128, y: u128) -> Result<u128, MathError> {
    div_wad(y, x, Rounding::Floor)
}

/// SPEC S2 slippage `σ = ⌊n·WAD·10⁴/(q·P)⌋ − 10⁴` in bps: the fee-free average premium of
/// buying `q` over the pre-trade marginal price `P` (ECON §5.6; ARCH §4.3).
///
/// `n` is the quote's fee-free input [`BuyQuote::net`]: ECON §3.1's `n(q)` (pump venues
/// including the curve's ε₁; `⌈Y·q/(X − q)⌉` on Raydium, also when a CPMM creator fee is
/// charged on the token side, review F-S4; for `B̂_str` the pool's `n` over the curve's
/// `P`). `None` (undefined quote) gives `u64::MAX` (`+∞`), and so does a premium beyond
/// `u64`. A premium below 0 (impossible for an exact-out constant-product quote) reads 0.
///
/// [`BuyQuote::net`]: crate::quote::BuyQuote::net
///
/// Numerator `n·WAD·10⁴` and denominator `Q·P` take the u128 path whenever both fit
/// (every `n` below 3.4·10¹⁶ lamports and `P` below ≈ 2·10³⁴), a 256-bit division
/// otherwise (ECON §2.2); the results are identical.
///
/// Errors: `DivByZero` if `q = 0` or `P = 0`.
pub fn sigma(n: Option<u64>, q: u64, p_wad: u128) -> Result<u64, MathError> {
    let Some(n) = n else {
        return Ok(u64::MAX);
    };
    let fast = u128::from(n)
        .checked_mul(WAD_BPS)
        .zip(u128::from(q).checked_mul(p_wad));
    let ratio = match fast {
        Some((num, den)) => {
            let r = num.checked_div(den).ok_or(MathError::DivByZero)?;
            u64::try_from(r).ok()
        }
        None => mul_div_u256(
            U256::from_u64(n),
            U256::from_u128(WAD_BPS),
            U256::from_product(u128::from(q), p_wad),
            Rounding::Floor,
        )?
        .to_u64(),
    };
    Ok(match ratio {
        Some(r) => r.saturating_sub(BPS),
        None => u64::MAX,
    })
}

/// [`sigma`] on the state `(X, Y)` the quote was taken on, with `P = ⌊Y·WAD/X⌋`
/// (`shortwin_sim.Market.slip_at`).
///
/// Errors: `DivByZero` if `x`, `q` or the price is 0.
pub fn sigma_xy(n: Option<u64>, q: u64, x: u128, y: u128) -> Result<u64, MathError> {
    sigma(n, q, price_wad(x, y)?)
}

/// D4 margin (owner decision; SPEC-literal reading):
/// `M = max(0, ⌈CR_open·max(B̂_spot, B̂_ema)/10⁴⌉ − S)` (ECON §4; ARCH §3.4 steps 9–10;
/// sim `Market.open`).
///
/// `b_spot` is `B̂(Q)` on the post-open live reserves (`N_open`). `b_ema` is `B̂(Q)` on the
/// **newest ring entry's cached `(x, y)`** — the EMA reserves as they stand after this
/// instruction's roll, unshifted by the open's own sale (R3A-12; ECON §2.1 order of
/// operations, step 4). The sim quotes both at the live venue fee (`quote_spot`,
/// `quote_at` without the cap).
///
/// An undefined `b_ema` prices the margin on `b_spot` alone: the sim's `B_margin =
/// max(qs[1], qe[1] if qe else qs[1])` (normative; ARCH §3.4 step 10 with §4.3's `+∞`
/// would refuse the open instead — a documentation item in the crate's design notes).
///
/// Errors: `Overflow`.
pub fn margin_d4(s: u64, b_spot: u64, b_ema: Quote, cr_open_bps: u32) -> Result<u64, MathError> {
    let b_ema = b_ema.unwrap_or(b_spot);
    let need = mul_bps(
        u128::from(b_spot.max(b_ema)),
        u128::from(cr_open_bps),
        Rounding::Ceil,
    )?;
    to_u64(need.saturating_sub(u128::from(s)))
}

/// Open fee `⌈open_fee_bps·N_open/10⁴⌉`, paid on top of `M` to the protocol (ECON §8.7;
/// ARCH §3.4 step 11).
///
/// Errors: `Overflow`.
pub fn open_fee(n_open: u64, open_fee_bps: u32) -> Result<u64, MathError> {
    mul_bps_u64(n_open, u64::from(open_fee_bps), Rounding::Ceil)
}

/// The rate `p'` in FV at one evaluated state: the locked penalty if `penalty_applies`,
/// else the locked expiry fee (D3; ECON §2.1, §8.6; ARCH §4.5; owner decision O15).
///
/// v0.4.1 (R4A-3) decides it **at the same `(P, L)`** FV is evaluated at, never from the
/// raw legs: `penalty_applies = ¬(expired ∧ CR(V_eff, B̂_FV(P, L)) ≥ L)`, i.e.
/// `!expired || cr_below(V_eff, B̂_FV, L)` (sim `Market.fv_parts`; `settle::pprime`).
pub const fn fv_rate_bps(penalty_applies: bool, penalty_bps: u32, expiry_fee_bps: u32) -> u32 {
    if penalty_applies {
        penalty_bps
    } else {
        expiry_fee_bps
    }
}

/// Fair value `FV(P, L) = B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due` (D3; ECON §2.1, §8.6; ARCH §4.5;
/// sim `Market.fv_parts`).
///
/// `b_fv` is the risk quote `B̂(Q; reprice(L, P))` at `min(φ_live, venue_fee_hard_cap_bps)`
/// on the evaluated state — a settlement-box corner, or `(P_ema, L_ema)` for the amount
/// paid; on a stuck migration, the deterministic post-migration pool at `P`. `rate_bps` is
/// [`fv_rate_bps`] decided on the same state. An undefined `b_fv` gives `None` (`+∞`: the
/// cap does not bind). FV saturates at `u64::MAX`, which is never below `V − K_s`, so the
/// cap it implies is exact.
pub fn fair_value(b_fv: Quote, rate_bps: u32, r_due: u64) -> Quote {
    let b = b_fv?;
    let fee = u128::from(b)
        .saturating_mul(u128::from(rate_bps))
        .div_ceil(u128::from(BPS));
    let fv = u128::from(b)
        .saturating_add(fee)
        .saturating_add(u128::from(r_due));
    Some(saturating_u64(fv))
}

/// SOL-settlement keeper reward `K_s = min(fixed + ⌊V·bps/10⁴⌋, cap, V)` (SPEC §6.3;
/// ECON §5.6; ARCH §4.5), locked terms.
///
/// Errors: none in practice (`Overflow` is unreachable because `K_s ≤ V`).
pub fn k_s(v: u64, terms: &SettleKeeperTerms) -> Result<u64, MathError> {
    let pct = mul_bps(u128::from(v), u128::from(terms.bps), Rounding::Floor)?;
    let k = u128::from(terms.fixed)
        .saturating_add(pct)
        .min(u128::from(terms.cap))
        .min(u128::from(v));
    to_u64(k)
}

/// Expiry keeper reward before truncation, `min(fixed + ⌊bps·B̂_ref/10⁴⌋, cap)`
/// (ECON §8.4; ARCH §3.7), locked terms. [`crate::waterfall::expire`] further limits it
/// to what the borrower has left, `V − B − r − e`.
///
/// Errors: none in practice (`Overflow` is unreachable because `K_e ≤ cap`).
pub fn k_e(b_ref: u64, terms: &ExpiryKeeperTerms) -> Result<u64, MathError> {
    let pct = mul_bps(u128::from(b_ref), u128::from(terms.bps), Rounding::Floor)?;
    to_u64(
        u128::from(terms.fixed)
            .saturating_add(pct)
            .min(u128::from(terms.cap)),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::num::WAD;

    #[test]
    fn cr_and_legs() {
        assert_eq!(cr_bps(200, Some(100)), Ok(20_000));
        assert_eq!(cr_bps(199, Some(100)), Ok(19_900));
        assert_eq!(cr_bps(5, None), Ok(0));
        assert_eq!(cr_bps(5, Some(0)), Err(MathError::InvalidInput));
        assert_eq!(cr_below(125, Some(100), 12_500), Ok(false));
        assert_eq!(cr_below(124, Some(100), 12_500), Ok(true));
        assert_eq!(cr_below(u64::MAX, None, 10_500), Ok(true));
        assert_eq!(v_eff(10, 11), 0);
        assert_eq!(spot_leg(false, false, u64::MAX, Some(1), 12_500), Ok(true));
        assert_eq!(spot_leg(true, false, 0, None, 12_500), Ok(false));
        assert_eq!(spot_leg(true, true, 0, Some(1), 12_500), Ok(true));
        assert!(liquidatable(false, false, 10, 10));
        assert!(!liquidatable(true, false, 9, 10));
    }

    #[test]
    fn budget_and_buyback() {
        let b = Budget {
            v: 1_000,
            k_s: 100,
            ert: 10,
        };
        assert_eq!(b.exceeded_by(Some(890)), Ok(false));
        assert_eq!(b.exceeded_by(Some(891)), Ok(true));
        assert_eq!(b.exceeded_by(None), Ok(true));
        let bad = Budget { k_s: 1_001, ..b };
        assert_eq!(bad.exceeded_by(Some(1)), Err(MathError::InvalidInput));
        assert_eq!(buyback_allowed(&b, Some(890), 2_000, 2_000), Ok(true));
        assert_eq!(buyback_allowed(&b, Some(891), 2_000, 2_000), Ok(false));
        assert_eq!(buyback_allowed(&b, Some(890), 2_001, 2_000), Ok(false));
        assert_eq!(buyback_allowed(&b, None, 0, 2_000), Ok(false));
        assert_eq!(SettleKind::S1 as u8, 1);
        assert_eq!(SettleKind::S3 as u8, 3);
    }

    #[test]
    fn sigma_matches_definition() {
        // X = 1000, Y = 1000 (P = 1 WAD); buying 100 costs n = ⌈1000·100/900⌉ = 112.
        assert_eq!(sigma_xy(Some(112), 100, 1_000, 1_000), Ok(1_200));
        assert_eq!(sigma(None, 1, 1), Ok(u64::MAX));
        assert_eq!(sigma(Some(1), 0, WAD), Err(MathError::DivByZero));
        assert_eq!(sigma(Some(u64::MAX), 1, 1), Ok(u64::MAX));
        assert_eq!(sigma(Some(1), 2, WAD), Ok(0));
        // The u128 path and the 256-bit fallback agree across the boundary.
        for (n, q, p) in [
            (34_000_000_000_000_000u64, 1u64, WAD),
            (u64::MAX, 3, 7 * WAD),
            (1_000, 10_000_000_000_000_000, 10u128.pow(34)),
            (5, u64::MAX, u128::MAX / 2),
        ] {
            let exact = mul_div_u256(
                U256::from_u64(n),
                U256::from_u128(WAD_BPS),
                U256::from_product(u128::from(q), p),
                Rounding::Floor,
            )
            .unwrap()
            .to_u64()
            .map_or(u64::MAX, |r| r.saturating_sub(BPS));
            assert_eq!(sigma(Some(n), q, p), Ok(exact), "{n} {q} {p}");
        }
    }

    #[test]
    fn margin_fee_fv_keepers() {
        assert_eq!(margin_d4(100, 100, Some(101), 20_000), Ok(102));
        assert_eq!(margin_d4(300, 100, Some(101), 20_000), Ok(0));
        // M0B-S-3 (review): an undefined EMA quote falls back to the spot quote, as the
        // sim's `Market.open`: ⌈2·1,000⌉ − 100 = 1,900.
        assert_eq!(margin_d4(100, 1_000, None, 20_000), Ok(1_900));
        assert_eq!(margin_d4(0, 1, None, 20_000), Ok(2));
        assert_eq!(
            margin_d4(100, 1_000, None, 20_000),
            margin_d4(100, 1_000, Some(1_000), 20_000)
        );
        assert_eq!(
            margin_d4(0, u64::MAX, None, 20_000),
            Err(MathError::Overflow)
        );
        assert_eq!(open_fee(1_001, 25), Ok(3));
        assert_eq!(fair_value(Some(1_001), 500, 7), Some(1_001 + 51 + 7));
        assert_eq!(fair_value(None, 500, 7), None);
        assert_eq!(fair_value(Some(u64::MAX), 500, 7), Some(u64::MAX));
        assert_eq!(fv_rate_bps(true, 500, 200), 500);
        assert_eq!(fv_rate_bps(false, 500, 200), 200);
        let s = SettleKeeperTerms::DEFAULT;
        assert_eq!(k_s(1_000_000_000, &s), Ok(20_000_000));
        assert_eq!(k_s(5_000_000, &s), Ok(5_000_000));
        assert_eq!(k_s(100_000_000_000, &s), Ok(250_000_000));
        assert_eq!(k_s(u64::MAX, &s), Ok(250_000_000));
        let e = ExpiryKeeperTerms::DEFAULT;
        assert_eq!(k_e(1_000_000_000, &e), Ok(2_000_000));
        assert_eq!(k_e(u64::MAX, &e), Ok(50_000_000));
    }

    #[test]
    fn liquidation_price_is_the_crossing() {
        use crate::fees::FeeBps;
        use crate::quote::Pricing;
        let v = Pricing::PumpSwap(FeeBps::new(20, 5, 95));
        let (q, l) = (10_000_000_000_000u64, 3_000_000_000_000_000u128);
        let quote = |p: u128| v.buy_repriced(q, l, p).map(|b| b.cost);
        let p0 = 30 * WAD / 1_000_000;
        let v_eff = 2 * quote(p0).unwrap();
        let below = |p: u128| cr_below(v_eff, quote(p), 12_500).unwrap();
        let p_liq = liquidation_price(p0, v_eff, 12_500, quote)
            .unwrap()
            .unwrap();
        assert!(p_liq > p0 && below(p_liq) && !below(p_liq - 1));
        // Around 1.6x the opening price for CR 200 % -> 125 % (ECON §3.3's closed form).
        assert!(p_liq > p0 * 3 / 2 && p_liq < p0 * 17 / 10);
        assert_eq!(
            liquidation_price(p_liq, v_eff, 12_500, quote),
            Ok(Some(p_liq))
        );
        assert_eq!(liquidation_price(1, u64::MAX, 1, |_| Some(1)), Ok(None));
        assert_eq!(
            liquidation_price(5, 1, 10_000, |_| Some(0)),
            Err(MathError::InvalidInput)
        );
        let mut ring_pred = ring_below(v_eff, 12_500, |p, l2| {
            v.buy_repriced(q, l2, p).map(|b| b.cost)
        });
        assert_eq!(ring_pred(p_liq, l), Ok(true));
        assert_eq!(ring_pred(p_liq - 1, l), Ok(false));
    }
}
