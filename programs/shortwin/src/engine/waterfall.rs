//! Waterfalls and position accounting (ARCH §2.1 `engine/waterfall.rs`, §3.4 steps 12–13,
//! §3.5 steps 6–7, §5.2): applies the pure `shortwin_math::waterfall` payouts as lamport moves
//! and lender-pool calls, and keeps the market's position aggregates (`ΣB_Q` by tier,
//! `A_j`, `open_positions`, `total_notional_open`; I6), the high-water mark `max_q_hwm` and the
//! low-water mark `min_open_fee_ppm` ("switch only if all fit" reads both), and the low-water
//! mark `min_open_hard_cap_bps` (the S3 chain reads it, AIR-19).
//!
//! The math crate decides every amount (`waterfall::{close, liquidate, expire, settle}`: legs
//! non-negative, summing to `V`, I9); this module only moves them:
//!
//! - lender and protocol legs: position → `market_vault` by direct debit ([`pay_vault`]), then
//!   `vault.protocol_fees += protocol` and `pool::repay` (rent true-up against `receivable`,
//!   the lender rent realised, the lump vested; [`book_repay`]) or `pool::settle`;
//! - the keeper leg: position → keeper by direct debit (the tracks' `liquidate` / `expire` /
//!   `sol_settle`);
//! - the borrower leg stays on the position, which is closed to the borrower (A18).

use anchor_lang::prelude::*;
use shortwin_math::caps;
use shortwin_math::pool::{self, Pool, Resolution, Vault};
use shortwin_math::rent::{self, Aggr, Basis};
use shortwin_math::waterfall::Payout;

use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::lamports;
use crate::state::{Market, ShortPosition};

/// ARCH §3.4 step 13 (open): `ΣB_Q` by tier, `A_j += q·coef_j·(10⁴ − p)` with the position's
/// snapshot share (R3A-5), `open_positions += 1`, `total_notional_open += n_open`,
/// `max_q_hwm = max(max_q_hwm, q)` (the high-water mark "switch only if all fit" bounds every
/// open `q` with; owner decision of 2026-09-29) and `min_open_fee_ppm` lowered to `phi_ppm`, the
/// venue's one-way fee the open's caps were computed at (set to it by the first open after the
/// market emptied; `shortwin_math::caps::open_fee_mark`): the fee the open positions posted
/// their margin at, which the carried fit's fee-scaled caps take when it is below the candidate's
/// (FIT-2; `shortwin_math::caps::carried_phi`); and `min_open_hard_cap_bps` lowered to the
/// position's locked `venue_fee_hard_cap_bps` (set to it by the first open after the market
/// emptied): the cap the market's S3 chain judges the live fee against, with the global one
/// (AIR-19; `engine::route::s3_no_route`). `pool::borrow` (`T`, `ΣB_Q`, `max_release`) is the
/// caller's.
pub fn add_position(
    market: &mut Market,
    aggr: &mut Aggr,
    pos: &ShortPosition,
    phi_ppm: u32,
) -> Result<()> {
    aggr.add(pos.q, &pos.rent_terms(), u32::from(pos.rent_protocol_bps))
        .math()?;
    let t = market
        .borrowed_q_by_tier
        .get_mut(usize::from(pos.tier))
        .ok_or(ShortwinError::MathOverflow)?;
    *t = t.checked_add(pos.q).or_math()?;
    market.open_positions = market.open_positions.checked_add(1).or_math()?;
    market.total_notional_open = market
        .total_notional_open
        .checked_add(pos.n_open)
        .or_math()?;
    market.max_q_hwm = market.max_q_hwm.max(pos.q);
    market.min_open_fee_ppm = caps::open_fee_mark(market.min_open_fee_ppm, phi_ppm);
    let cap = pos.venue_fee_hard_cap_bps;
    market.min_open_hard_cap_bps = match market.min_open_hard_cap_bps {
        0 => cap,
        mark => mark.min(cap),
    };
    Ok(())
}

/// The inverse of [`add_position`] at every exit (ARCH §3.5 step 7, §3.8 "Effects"): the exit
/// that takes `open_positions` to 0 also resets `max_q_hwm`, `min_open_fee_ppm` and
/// `min_open_hard_cap_bps` to 0 (the marks change nowhere else, so `max_q_hwm` stays at or above
/// every open `q`, `min_open_fee_ppm` at or below every open position's open fee and
/// `min_open_hard_cap_bps` at or below every open position's locked hard cap).
pub fn remove_position(market: &mut Market, aggr: &mut Aggr, pos: &ShortPosition) -> Result<()> {
    aggr.sub(pos.q, &pos.rent_terms(), u32::from(pos.rent_protocol_bps))
        .math()?;
    let t = market
        .borrowed_q_by_tier
        .get_mut(usize::from(pos.tier))
        .ok_or(ShortwinError::MathOverflow)?;
    *t = t.checked_sub(pos.q).or_math()?;
    market.open_positions = market.open_positions.checked_sub(1).or_math()?;
    market.total_notional_open = market
        .total_notional_open
        .checked_sub(pos.n_open)
        .or_math()?;
    if market.open_positions == 0 {
        market.max_q_hwm = 0;
        market.min_open_fee_ppm = 0;
        market.min_open_hard_cap_bps = 0;
    }
    Ok(())
}

/// Books a repayment (close, liquidation, expiry; ARCH §3.3 "borrower-side pool calls"):
/// `pool::repay` with the position's `q`, the lender rent it was credited continuously
/// (`rent::credited_for` on the current basis, ceil) and the payout's vesting lump; the rent
/// paid and the position's locked share (the lender rent above the credited rent — `R_min`
/// within `min_rent_s` of the open — vests with the lump, `pool::rent_top_up`, M10-8C-1);
/// then `protocol_fees += payout.protocol`. Returns the lender rent. The lamports are
/// [`pay_vault`]'s.
pub fn book_repay(
    pool: &mut Pool,
    vault: &mut Vault,
    now: i64,
    lump_vest_s: u32,
    pos: &ShortPosition,
    basis: &Basis,
    payout: &Payout,
) -> Result<u64> {
    let credited = rent::credited_for(&pos.pos_rent(), basis).math()?;
    let res = Resolution {
        q: pos.q,
        credited,
        lump: payout.lump,
    };
    let lender_rent = pool::repay(
        pool,
        vault,
        now,
        lump_vest_s,
        &res,
        payout.rent,
        u32::from(pos.rent_protocol_bps),
    )
    .math()?;
    vault.protocol_fees = vault.protocol_fees.checked_add(payout.protocol).or_math()?;
    Ok(lender_rent)
}

/// Moves a payout's lender and protocol legs from the position to `market_vault` (direct
/// debit; the position stays at or above its rent-exempt minimum).
pub fn pay_vault(
    position: &AccountInfo,
    market_vault: &AccountInfo,
    payout: &Payout,
) -> Result<()> {
    let amount = payout.lenders.checked_add(payout.protocol).or_math()?;
    lamports::move_lamports(position, market_vault, amount)
}
