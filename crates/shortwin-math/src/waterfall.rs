//! Owner: waterfall track. Close, liquidation (with the keeper floor), expiry and settlement (with the vesting lump) waterfalls; legs sum to `V` exactly (ECON §2.1, §8.1–§8.7, I-E4, I-E10; ARCH §5.2).
//!
//! Each waterfall is a pure function of the position's vault `V`, the measured venue
//! spend `B` and quotes, using the position's **locked** split, rent share, fee and keeper
//! terms. Its five legs — venue, keeper, protocol, lenders, borrower — are non-negative
//! and sum to `V` exactly (I-E4, I9); every cut is rounded down and lenders take the
//! remainder (ECON §2.1). The borrower leg stays on the position account, which is closed
//! to the borrower (A18). [`Payout::lump`] is the part of the lenders' SOL that vests
//! (`pool::repay` / `pool::settle`; ECON §9.4, SPEC §2.4 O16).
//!
//! Normative reference: `shortwin_sim.Market._buyback_exit`, `try_exit(borrower_close)`
//! and `_settle` v0.4.1 with their default switches.

use crate::error::MathError;
use crate::num::{BPS, Rounding, WAD, div, mul_bps_u64, to_u64};
use crate::risk::{self, ExpiryKeeperTerms, Quote, SettleKind};

/// Liquidation penalty split lender / protocol / keeper, bps summing to 10⁴
/// (SPEC §6.3; ARCH §2.3.3 `penalty_split`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PenaltySplit {
    /// Lenders' share (they also take the rounding remainder).
    pub lender_bps: u32,
    /// Protocol share, rounded down.
    pub protocol_bps: u32,
    /// Keeper share, rounded down.
    pub keeper_bps: u32,
}

impl PenaltySplit {
    /// SPEC §6.3: 50 / 20 / 30.
    pub const DEFAULT: Self = Self {
        lender_bps: 5_000,
        protocol_bps: 2_000,
        keeper_bps: 3_000,
    };

    fn validate(&self) -> Result<(), MathError> {
        let sum = u64::from(self.lender_bps)
            .saturating_add(u64::from(self.protocol_bps))
            .saturating_add(u64::from(self.keeper_bps));
        if sum == BPS {
            Ok(())
        } else {
            Err(MathError::InvalidInput)
        }
    }
}

/// Who gets what out of a position's vault `V`.
///
/// `buyback + keeper + protocol + lenders + borrower == V` ([`Payout::total`]). The
/// `rent_*`, `penalty*`, `keeper_topup`, `expiry_fee` and `lump` fields break the totals
/// down for events and the lender pool: a close, liquidation or expiry calls
/// [`pool::repay`](crate::pool::repay) with `rent_paid = rent` and
/// `Resolution { lump, .. }`, a settlement [`pool::settle`](crate::pool::settle) with
/// `sol_in = lenders`, `rent = R_due` and `Resolution { lump, .. }`. Either also vests the
/// rent paid above what was credited while the position was open
/// ([`pool::rent_top_up`](crate::pool::rent_top_up); finding M10-8C-1), which `lump` does
/// not include.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Payout {
    /// Measured venue spend `B` of the buyback (0 in a settlement).
    pub buyback: u64,
    /// Rent paid `r` (close: `R_due`; liquidation and expiry: `min(R_due, V − B)`; 0 in a
    /// settlement, whose lender take already includes rent through FV).
    pub rent: u64,
    /// Protocol part of `rent`: `⌊r·rent_protocol_bps/10⁴⌋`.
    pub rent_protocol: u64,
    /// Lender part of `rent`: the remainder.
    pub rent_lenders: u64,
    /// Liquidation penalty `π` actually paid.
    pub penalty: u64,
    /// Keeper part of `π`, rounded down.
    pub penalty_keeper: u64,
    /// Protocol part of `π`, rounded down.
    pub penalty_protocol: u64,
    /// Lender part of `π`: the remainder.
    pub penalty_lenders: u64,
    /// Expiry fee `e` actually paid, 100 % to lenders.
    pub expiry_fee: u64,
    /// Liquidation keeper floor top-up `min(max(0, K_s − ⌊π·keeper_split/10⁴⌋), W)`, paid
    /// from the borrower remainder `W` (N22, A74; part of `keeper`). 0 elsewhere.
    pub keeper_topup: u64,
    /// Keeper total: penalty share plus the floor top-up, expiry reward `K_e`, or
    /// settlement reward `K_s`.
    pub keeper: u64,
    /// Protocol total.
    pub protocol: u64,
    /// Lender total (credited to `market_vault`).
    pub lenders: u64,
    /// The part of `lenders` that vests over `lump_vest_s` (ECON §9.4): the lenders' share
    /// of the penalty (liquidation), the expiry fee (expiry), or a settlement's take above
    /// `⌊Q·P_ema/WAD⌋ + R_due` (S1, S2 and S3; S1 since AIR-21), above `R_due` alone on a venue
    /// that cannot execute (AIR-09). 0 for a close.
    pub lump: u64,
    /// Borrower remainder (paid through the position close, A18).
    pub borrower: u64,
}

impl Payout {
    /// `buyback + keeper + protocol + lenders + borrower`, which equals `V` for every
    /// payout built here.
    ///
    /// Errors: `Overflow`.
    pub fn total(&self) -> Result<u64, MathError> {
        [self.keeper, self.protocol, self.lenders, self.borrower]
            .into_iter()
            .try_fold(self.buyback, u64::checked_add)
            .ok_or(MathError::Overflow)
    }

    /// Checks `total() == v` (I-E4). A failure is a bug in this module, never an input
    /// error: `Invariant`.
    fn conserved(self, v: u64) -> Result<Self, MathError> {
        if self.total()? == v {
            Ok(self)
        } else {
            Err(MathError::Invariant)
        }
    }
}

fn check_bps(bps: u32) -> Result<(), MathError> {
    if u64::from(bps) > BPS {
        Err(MathError::InvalidInput)
    } else {
        Ok(())
    }
}

/// `⌊x·bps/10⁴⌋` or `⌈x·bps/10⁴⌉`.
fn bps_of(x: u64, bps: u32, r: Rounding) -> Result<u64, MathError> {
    mul_bps_u64(x, u64::from(bps), r)
}

/// Splits paid rent 85/15-style with the position's locked share: protocol
/// `⌊r·rent_protocol_bps/10⁴⌋`, lenders the rest (SPEC §6.3; ECON §8.1; `pool_model.repay`).
fn split_rent(r: u64, rent_protocol_bps: u32) -> Result<(u64, u64), MathError> {
    let protocol = bps_of(r, rent_protocol_bps, Rounding::Floor)?;
    Ok((
        protocol,
        r.checked_sub(protocol).ok_or(MathError::Underflow)?,
    ))
}

/// What is left after the buyback and the rent: `(r, V − B − r)` with `r = min(R_due, V − B)`.
fn after_buyback_and_rent(v: u64, b: u64, r_due: u64) -> Result<(u64, u64), MathError> {
    let w = v.checked_sub(b).ok_or(MathError::Underflow)?;
    let r = r_due.min(w);
    Ok((r, w.saturating_sub(r)))
}

/// Borrower close (ECON §8.1; ARCH §3.5, §5.2): `B` → `R_due` (protocol
/// `⌊R·rent_protocol_bps/10⁴⌋` with the position's locked share, lenders the rest) →
/// remainder to the borrower. No penalty, even when liquidatable (G28); nothing vests.
///
/// `b` is the measured spend (incl. venue-side rent). The close precondition
/// (`max_in = min(max_buyback_cost, V − R − ert)`, `B ≤ max_in + ert`) guarantees
/// `B + R_due ≤ V`.
///
/// Errors: `Underflow` if `B + R_due > V`; `InvalidInput` if `rent_protocol_bps > 10⁴`.
pub fn close(v: u64, b: u64, r_due: u64, rent_protocol_bps: u32) -> Result<Payout, MathError> {
    check_bps(rent_protocol_bps)?;
    let (rent_protocol, rent_lenders) = split_rent(r_due, rent_protocol_bps)?;
    let borrower = v
        .checked_sub(b)
        .and_then(|w| w.checked_sub(r_due))
        .ok_or(MathError::Underflow)?;
    Payout {
        buyback: b,
        rent: r_due,
        rent_protocol,
        rent_lenders,
        keeper: 0,
        protocol: rent_protocol,
        lenders: rent_lenders,
        borrower,
        ..Payout::default()
    }
    .conserved(v)
}

/// Inputs shared by the keeper-initiated buyback waterfalls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExitInputs {
    /// Tracked vault lamports `V`.
    pub v: u64,
    /// Measured venue spend `B` (incl. venue-side rent), `≤ V`.
    pub b: u64,
    /// `R_due` ([`crate::rent::due`]).
    pub r_due: u64,
    /// `B̂_live,risk`: the live-state quote at the **risk** fee
    /// `min(φ_live, venue_fee_hard_cap_bps)` (ARCH §5.2's fee base; sim `qs_r`). The
    /// execution quote at the live fee decides `max_in` and whether the buyback runs, not
    /// the penalty or the expiry fee.
    pub b_live: u64,
    /// `B̂_ref`: the exit-reference quote (`bounds`).
    pub b_ref: u64,
    /// The position's locked `rent_protocol_bps` (R3A-5;
    /// [`PosRent::rent_protocol_bps`](crate::rent::PosRent::rent_protocol_bps)).
    pub rent_protocol_bps: u32,
}

impl ExitInputs {
    /// Fee base `min(B̂_live,risk, B̂_ref)`: quotes, never the measured spend (C0-1 (2),
    /// C0-14; ARCH §5.2).
    pub const fn base(&self) -> u64 {
        if self.b_live < self.b_ref {
            self.b_live
        } else {
            self.b_ref
        }
    }
}

/// Normal liquidation (SPEC §5.2, §6.3; ECON §8.3, §8.5; ARCH §3.6, §5.2; sim
/// `Market._buyback_exit`):
///
/// 1. `B` to the venue;
/// 2. `r = min(R_due, V − B)`: protocol `⌊r·rent_protocol_bps/10⁴⌋`, lenders the rest;
/// 3. `π = min(⌈p·min(B̂_live, B̂_ref)/10⁴⌉, V − B − r)`: keeper `kp = ⌊π·keeper/10⁴⌋`,
///    protocol `⌊π·protocol/10⁴⌋`, lenders the rest (which vests: [`Payout::lump`]);
/// 4. **keeper floor** (owner decision N22, A74, R3B-10): the keeper also gets
///    `top = min(max(0, K_s − kp), W)` from the borrower remainder `W = V − B − r − π`,
///    so a liquidation that leaves the borrower equity pays the keeper at least `K_s`
///    (I-E10) and never more than the borrower would have lost in S1;
/// 5. the remainder `W − top` to the borrower.
///
/// Rent and then the penalty are truncated when the vault is underwater. `k_s` is the
/// position's settlement keeper reward ([`risk::k_s`] on `V`, locked terms).
///
/// Errors: `Underflow` if `B > V`; `InvalidInput` if the split does not sum to 10⁴ or a
/// rate exceeds 10⁴ bps.
pub fn liquidate(
    x: &ExitInputs,
    penalty_bps: u32,
    split: &PenaltySplit,
    k_s: u64,
) -> Result<Payout, MathError> {
    split.validate()?;
    check_bps(x.rent_protocol_bps)?;
    let (rent, w) = after_buyback_and_rent(x.v, x.b, x.r_due)?;
    let (rent_protocol, rent_lenders) = split_rent(rent, x.rent_protocol_bps)?;
    let penalty = bps_of(x.base(), penalty_bps, Rounding::Ceil)?.min(w);
    let penalty_keeper = bps_of(penalty, split.keeper_bps, Rounding::Floor)?;
    let penalty_protocol = bps_of(penalty, split.protocol_bps, Rounding::Floor)?;
    let penalty_lenders = penalty
        .checked_sub(penalty_keeper)
        .and_then(|p| p.checked_sub(penalty_protocol))
        .ok_or(MathError::Underflow)?;
    let w = w.saturating_sub(penalty);
    let keeper_topup = k_s.saturating_sub(penalty_keeper).min(w);
    Payout {
        buyback: x.b,
        rent,
        rent_protocol,
        rent_lenders,
        penalty,
        penalty_keeper,
        penalty_protocol,
        penalty_lenders,
        expiry_fee: 0,
        keeper_topup,
        keeper: penalty_keeper
            .checked_add(keeper_topup)
            .ok_or(MathError::Overflow)?,
        protocol: rent_protocol
            .checked_add(penalty_protocol)
            .ok_or(MathError::Overflow)?,
        lenders: rent_lenders
            .checked_add(penalty_lenders)
            .ok_or(MathError::Overflow)?,
        lump: penalty_lenders,
        borrower: w.saturating_sub(keeper_topup),
    }
    .conserved(x.v)
}

/// Expiry of a position that is not CR-liquidatable (SPEC §4 "Duration"; ECON §8.4;
/// ARCH §3.7, §5.2):
///
/// 1. `B` to the venue;
/// 2. `r = min(R_due, V − B)`, split as in [`close`];
/// 3. expiry fee `e = min(⌈expiry_fee_bps·min(B̂_live, B̂_ref)/10⁴⌉, V − B − r)`, 100 %
///    lenders (it vests: [`Payout::lump`]);
/// 4. keeper `K_e = min(fixed + ⌊bps·B̂_ref/10⁴⌋, cap, V − B − r − e)` ([`risk::k_e`]);
/// 5. the remainder to the borrower.
///
/// Unchanged in v0.4.1: the fee's lender share vests in `pool`.
///
/// Errors: `Underflow` if `B > V`; `InvalidInput` if a rate exceeds 10⁴ bps.
pub fn expire(
    x: &ExitInputs,
    expiry_fee_bps: u32,
    keeper: &ExpiryKeeperTerms,
) -> Result<Payout, MathError> {
    check_bps(x.rent_protocol_bps)?;
    let (rent, w) = after_buyback_and_rent(x.v, x.b, x.r_due)?;
    let (rent_protocol, rent_lenders) = split_rent(rent, x.rent_protocol_bps)?;
    let expiry_fee = bps_of(x.base(), expiry_fee_bps, Rounding::Ceil)?.min(w);
    let w = w.saturating_sub(expiry_fee);
    let k_e = risk::k_e(x.b_ref, keeper)?.min(w);
    Payout {
        buyback: x.b,
        rent,
        rent_protocol,
        rent_lenders,
        expiry_fee,
        keeper: k_e,
        protocol: rent_protocol,
        lenders: rent_lenders
            .checked_add(expiry_fee)
            .ok_or(MathError::Overflow)?,
        lump: expiry_fee,
        borrower: w.saturating_sub(k_e),
        ..Payout::default()
    }
    .conserved(x.v)
}

/// SOL settlement (SPEC §5.3, §6.3; D3; ECON §2.1, §8.6; ARCH §3.8, §5.2; sim
/// `Market._settle`):
///
/// 1. `K_s` to the keeper;
/// 2. lenders `x`: S1 `V − K_s` (never capped); S2/S3 `x = min(V − K_s, FV)` with `fv` = FV
///    at `(P_ema, L_ema)` ([`risk::fair_value`]); on every kind the part of `x` above the
///    zero-impact value of the extinguished tokens plus the rent due vests:
///    `lump = x − min(x, ⌊q·p_ema/WAD⌋ + r_due)` ([`Payout::lump`]; v0.4.1, R4B-6, no
///    `B̂_FV` reconstruction, R4A-5);
/// 3. `V − K_s − x` to the borrower. The protocol gets nothing.
///
/// **S1 vests its excess too (AIR-21).** The S1 legs require only `B̂ + ert > V − K_s`, and
/// `B̂` carries the buyback's impact and fees, so an S1 take can exceed the tokens' value at
/// `P_ema` plus the rent; credited at once, a just-in-time deposit bundled around the S1 took
/// its pro-rata share of that excess. A genuine S1 loss (`V − K_s ≤ ⌊q·p_ema/WAD⌋ + r_due`)
/// vests nothing and stays immediate.
///
/// **`p_ema` is the price the tokens count at once:** `P_ema` while the venue executes, and 0
/// when it cannot (`sol_settle`, AIR-09): nothing is observed then, `P_ema` is the pre-halt
/// mark and the settlement box its point, so an immediate `⌊q·P_ema/WAD⌋` would let a
/// just-in-time deposit swap tokens for SOL at a stale price. At 0 only `r_due` is immediate.
///
/// `fv` of `None` (undefined `B̂_FV`) means the cap does not bind; `fv` is ignored for S1.
/// Whether an S2/S3 amount may be paid at all (the settlement box's `fv_dev_bps` check) is
/// `settle`'s decision, not this function's.
///
/// Errors: `InvalidInput` if `K_s > V`.
pub fn settle(
    v: u64,
    kind: SettleKind,
    k_s: u64,
    fv: Quote,
    q: u64,
    p_ema: u128,
    r_due: u64,
) -> Result<Payout, MathError> {
    let take = v.checked_sub(k_s).ok_or(MathError::InvalidInput)?;
    let lenders = match kind {
        SettleKind::S1 => take,
        SettleKind::S2 | SettleKind::S3 => fv.map_or(take, |fv| take.min(fv)),
    };
    let immediate = to_u64(immediate_cap(q, p_ema, r_due)?.min(u128::from(lenders)))?;
    let lump = lenders.checked_sub(immediate).ok_or(MathError::Underflow)?;
    Payout {
        keeper: k_s,
        lenders,
        lump,
        borrower: take.saturating_sub(lenders),
        ..Payout::default()
    }
    .conserved(v)
}

/// `⌊q·p_ema/WAD⌋ + r_due` (ECON §2.1 "S2/S3 immediate part", S1's too since AIR-21),
/// saturated: when `q·p_ema` leaves `u128` the value exceeds `2¹²⁸/10¹⁸ > u64::MAX`, above
/// any take, so the saturated bound gives the same `min(x, ·)` without a 256-bit
/// intermediate.
fn immediate_cap(q: u64, p_ema: u128, r_due: u64) -> Result<u128, MathError> {
    let v_ema = match u128::from(q).checked_mul(p_ema) {
        Some(n) => div(n, WAD, Rounding::Floor)?,
        None => u128::MAX,
    };
    Ok(v_ema.saturating_add(u128::from(r_due)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn exit(v: u64, b: u64, r: u64, b_live: u64, b_ref: u64) -> ExitInputs {
        ExitInputs {
            v,
            b,
            r_due: r,
            b_live,
            b_ref,
            rent_protocol_bps: 1_500,
        }
    }

    #[test]
    fn close_splits_rent() {
        let p = close(1_000, 700, 101, 1_500).unwrap();
        assert_eq!((p.rent_protocol, p.rent_lenders, p.borrower), (15, 86, 199));
        assert_eq!(p.total(), Ok(1_000));
        assert_eq!(close(1_000, 900, 101, 1_500), Err(MathError::Underflow));
    }

    #[test]
    fn liquidation_normal_and_underwater() {
        let x = exit(10_000, 6_000, 100, 6_010, 5_990);
        let p = liquidate(&x, 500, &PenaltySplit::DEFAULT, 0).unwrap();
        // π = ⌈5·5,990/100⌉ = 300: keeper 90, protocol 60, lenders 150.
        assert_eq!(p.penalty, 300);
        assert_eq!(
            (p.penalty_keeper, p.penalty_protocol, p.penalty_lenders),
            (90, 60, 150)
        );
        assert_eq!(p.protocol, 15 + 60);
        assert_eq!(p.lenders, 85 + 150);
        assert_eq!((p.lump, p.keeper_topup, p.keeper), (150, 0, 90));
        assert_eq!(p.borrower, 10_000 - 6_000 - 100 - 300);
        // Keeper floor (N22): K_s = 1,000 tops the keeper up from the borrower's 3,600.
        let f = liquidate(&x, 500, &PenaltySplit::DEFAULT, 1_000).unwrap();
        assert_eq!((f.keeper_topup, f.keeper), (910, 1_000));
        assert_eq!(f.borrower, p.borrower - 910);
        assert_eq!(
            (f.lenders, f.protocol, f.lump),
            (p.lenders, p.protocol, 150)
        );
        // ... truncated by the borrower remainder, and 0 once the penalty share covers K_s.
        let f = liquidate(&x, 500, &PenaltySplit::DEFAULT, 10_000).unwrap();
        assert_eq!((f.keeper, f.borrower), (90 + 3_600, 0));
        let f = liquidate(&x, 500, &PenaltySplit::DEFAULT, 90).unwrap();
        assert_eq!((f.keeper_topup, f.borrower), (0, p.borrower));
        // Penalty truncated.
        let p = liquidate(
            &exit(6_200, 6_000, 100, 6_010, 5_990),
            500,
            &PenaltySplit::DEFAULT,
            1_000,
        )
        .unwrap();
        assert_eq!((p.rent, p.penalty, p.borrower), (100, 100, 0));
        // Rent truncated, no penalty.
        let p = liquidate(
            &exit(6_050, 6_000, 100, 6_010, 5_990),
            500,
            &PenaltySplit::DEFAULT,
            1_000,
        )
        .unwrap();
        assert_eq!((p.rent, p.penalty, p.borrower), (50, 0, 0));
        assert_eq!(p.total(), Ok(6_050));
        let bad = PenaltySplit {
            lender_bps: 5_001,
            ..PenaltySplit::DEFAULT
        };
        assert_eq!(liquidate(&x, 500, &bad, 0), Err(MathError::InvalidInput));
        assert_eq!(
            liquidate(&exit(5, 6, 0, 1, 1), 500, &PenaltySplit::DEFAULT, 0),
            Err(MathError::Underflow)
        );
    }

    #[test]
    fn expiry_pays_fee_then_keeper() {
        let terms = ExpiryKeeperTerms::DEFAULT;
        let x = exit(2_000_000_000, 1_000_000_000, 10, 1_000_000_000, 999_000_000);
        let p = expire(&x, 200, &terms).unwrap();
        assert_eq!(p.expiry_fee, 19_980_000);
        assert_eq!(p.keeper, 1_000_000 + 999_000);
        assert_eq!(p.lenders, 9 + 19_980_000);
        assert_eq!(p.protocol, 1);
        assert_eq!(p.lump, 19_980_000);
        assert_eq!(p.total(), Ok(x.v));
        // Keeper reward truncated by the borrower's remainder.
        let x = exit(1_020_500_000, 1_000_000_000, 0, 1_000_000_000, 999_000_000);
        let p = expire(&x, 200, &terms).unwrap();
        assert_eq!(
            (p.expiry_fee, p.keeper, p.borrower),
            (19_980_000, 520_000, 0)
        );
    }

    #[test]
    fn settlement_caps_and_lump() {
        const Q: u64 = 1_000;
        let p_ema = 300_000_000_000_000_000; // 0.3 lamport per raw unit: ⌊Q·P/WAD⌋ = 300
        // S1 is never capped; its take above ⌊Q·P_ema/WAD⌋ + R_due vests (AIR-21) ...
        let p = settle(1_000, SettleKind::S1, 20, Some(500), Q, p_ema, 7).unwrap();
        assert_eq!(
            (p.keeper, p.lenders, p.borrower, p.lump),
            (20, 980, 0, 980 - 307)
        );
        // ... and a genuine S1 loss (the take at most the tokens' value plus rent) stays
        // immediate.
        let p = settle(300, SettleKind::S1, 20, None, Q, p_ema, 7).unwrap();
        assert_eq!((p.lenders, p.borrower, p.lump), (280, 0, 0));
        let p = settle(1_000, SettleKind::S2, 20, Some(500), Q, p_ema, 7).unwrap();
        assert_eq!((p.keeper, p.lenders, p.borrower), (20, 500, 480));
        assert_eq!(p.lump, 500 - 307);
        let p = settle(1_000, SettleKind::S3, 20, Some(5_000), Q, p_ema, 7).unwrap();
        assert_eq!((p.lenders, p.borrower, p.lump), (980, 0, 980 - 307));
        let p = settle(1_000, SettleKind::S2, 20, None, Q, p_ema, 7).unwrap();
        assert_eq!((p.lenders, p.borrower, p.lump), (980, 0, 673));
        // Nothing vests when the take is at most the market value plus rent ...
        let p = settle(1_000, SettleKind::S3, 20, Some(300), Q, p_ema, 7).unwrap();
        assert_eq!((p.lenders, p.lump), (300, 0));
        // ... unless the venue cannot execute: the tokens count at 0, only R_due is immediate
        // (AIR-09).
        let p = settle(1_000, SettleKind::S3, 20, Some(300), Q, 0, 7).unwrap();
        assert_eq!((p.lenders, p.lump), (300, 300 - 7));
        // ... nor when Q·P_ema leaves u128 (its value is above any u64 take).
        let p = settle(1_000, SettleKind::S3, 20, Some(900), u64::MAX, u128::MAX, 0).unwrap();
        assert_eq!((p.lenders, p.lump), (900, 0));
        assert_eq!(p.total(), Ok(1_000));
        assert_eq!(
            settle(10, SettleKind::S1, 11, None, Q, p_ema, 0),
            Err(MathError::InvalidInput)
        );
    }
}
