//! The analytics formulas of INDEXER.md §8 in Rust: the reference the SQL views and the writer
//! (settlement and fee rows) must agree with, and what the replay harness recomputes from chain
//! accounts (§11.4). Owner: ANALYTICS (the writer calls [`p_ref`], [`settlement_outcome`],
//! [`rent_protocol_share`] and [`borrower_pnl`]; their results are part of the schema contract).

use crate::numeric::mul_wad_floor;

/// OI at mark (§8.1): `⌊ΣB_Q · P_ema / WAD⌋` lamports.
pub fn oi_lamports(borrowed_q: u64, ema_price: u128) -> u128 {
    mul_wad_floor(u128::from(borrowed_q), ema_price).unwrap_or(u128::MAX)
}

/// TVL (§8.1): unborrowed tokens at mark, the SOL held for lenders and the open positions'
/// vaults — `⌊T · P_ema / WAD⌋ + lender_sol + Σ v_open`. Borrowed tokens are not counted: they
/// were sold, and their proceeds sit in the position vaults.
pub fn tvl_lamports(tokens_free: u64, ema_price: u128, lender_sol: u64, open_vaults: u64) -> u128 {
    mul_wad_floor(u128::from(tokens_free), ema_price)
        .unwrap_or(u128::MAX)
        .saturating_add(u128::from(lender_sol))
        .saturating_add(u128::from(open_vaults))
}

/// The lenders' claim at mark (§8.1): `⌊(T + ΣB_Q) · P_ema / WAD⌋ + lender_sol`.
pub fn lender_value_lamports(
    tokens_free: u64,
    borrowed_q: u64,
    ema_price: u128,
    lender_sol: u64,
) -> u128 {
    mul_wad_floor(u128::from(tokens_free) + u128::from(borrowed_q), ema_price)
        .unwrap_or(u128::MAX)
        .saturating_add(u128::from(lender_sol))
}

/// How a settlement's reference price was obtained (`settlements.p_ref_source`, §8.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PRefSource {
    /// The box is a point (`p_lo == p_hi`): that point is `P_ema` exactly.
    BoxPoint,
    /// The market's last `P_ema` seen before the settlement (PriceCranked or Open), clamped into
    /// `[p_lo, p_hi]` — the box contains the settlement's own `P_ema` by construction.
    CrankClamped,
    /// No price seen: the box midpoint `⌊(p_lo + p_hi)/2⌋`.
    BoxMid,
}

impl PRefSource {
    /// The column value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BoxPoint => "box_point",
            Self::CrankClamped => "crank_clamped",
            Self::BoxMid => "box_mid",
        }
    }
}

/// `P_ref` of a settlement (§8.4) from its box corners and the last `P_ema` seen.
pub fn p_ref(last_ema: Option<u128>, p_lo: u128, p_hi: u128) -> (u128, PRefSource) {
    let (lo, hi) = (p_lo.min(p_hi), p_lo.max(p_hi));
    if lo == hi {
        return (lo, PRefSource::BoxPoint);
    }
    match last_ema {
        Some(p) => (p.clamp(lo, hi), PRefSource::CrankClamped),
        None => (lo + (hi - lo) / 2, PRefSource::BoxMid),
    }
}

/// A settlement's outcome for its lenders (§8.4; SPEC §13 "realised lender loss"; the
/// simulator's `lender_pnl`, `docs/design/sim/shortwin_sim.py` `summarize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettlementOutcome {
    /// `⌊q · P_ref / WAD⌋`: the value of the extinguished tokens at settlement.
    pub token_value: u128,
    /// `lender_amount − token_value` (positive: the lenders received more SOL than the tokens
    /// were worth).
    pub lender_pnl: i128,
    /// `max(0, −lender_pnl)`.
    pub realised_lender_loss: u128,
}

/// The lenders' outcome of a `SolSettle` with `lender_amount` for `q` tokens at `p_ref`.
pub fn settlement_outcome(lender_amount: u64, q: u64, p_ref: u128) -> SettlementOutcome {
    let token_value = mul_wad_floor(u128::from(q), p_ref).unwrap_or(u128::MAX);
    let tv = i128::try_from(token_value).unwrap_or(i128::MAX);
    let lender_pnl = i128::from(lender_amount).saturating_sub(tv);
    SettlementOutcome {
        token_value,
        lender_pnl,
        realised_lender_loss: u128::try_from(lender_pnl.saturating_neg().max(0)).unwrap_or(0),
    }
}

/// The protocol's rent share of a liquidation or an expiry (§8.2; ARCH §5.2 "r (split as in
/// close)"): `⌊rent · rent_protocol_bps / 10⁴⌋` with the position's locked share.
pub fn rent_protocol_share(rent: u64, rent_protocol_bps: u16) -> u64 {
    u64::try_from(u128::from(rent) * u128::from(rent_protocol_bps) / 10_000).unwrap_or(u64::MAX)
}

/// The borrower's P&L of an exited position (§8.3; the simulator's `borrower_pnl`):
/// `borrower_payout − (m + margin_added) − fee_open`.
pub fn borrower_pnl(borrower_payout: u64, m: u64, margin_added: u64, fee_open: u64) -> i128 {
    i128::from(borrower_payout) - i128::from(m) - i128::from(margin_added) - i128::from(fee_open)
}

/// A lender's shares across one epoch roll (§6.3; `pool::maybe_roll`): `⌊shares · num / den⌋`.
pub fn convert_shares(shares: u128, num: u8, den: u128) -> u128 {
    if den == 0 {
        return 0;
    }
    shortwin_math::num::mul_div(shares, u128::from(num), den, shortwin_math::Rounding::Floor)
        .unwrap_or(0)
}

/// Opening price impact in basis points (§8.3 "execution slippage"): the sale's shortfall from
/// the pre-trade spot value, `⌊(q·spot/WAD − s) · 10⁴ / (q·spot/WAD)⌋`, 0 when the sale got
/// at least the spot value.
pub fn open_impact_bps(q: u64, spot_wad: u128, s: u64) -> u64 {
    let spot_value = mul_wad_floor(u128::from(q), spot_wad).unwrap_or(0);
    if spot_value == 0 || u128::from(s) >= spot_value {
        return 0;
    }
    u64::try_from((spot_value - u128::from(s)) * 10_000 / spot_value).unwrap_or(10_000)
}

/// Exit execution against its quote (§8.3; SQL `sw_exit_slippage_bps`): `(buyback_cost −
/// b_live)·10⁴ / b_live` bps, truncated toward zero (negative: better than quoted); `None`
/// without a quote.
pub fn exit_slippage_bps(buyback_cost: u64, b_live: u64) -> Option<i128> {
    (b_live > 0)
        .then(|| (i128::from(buyback_cost) - i128::from(b_live)) * 10_000 / i128::from(b_live))
}

/// Lendable inventory at mark (§8.1, §8.7): `⌊(T + ΣB_Q) · P_ema / WAD⌋` lamports.
pub fn inventory_value_lamports(tokens_free: u64, borrowed_q: u64, ema_price: u128) -> u128 {
    mul_wad_floor(u128::from(tokens_free) + u128::from(borrowed_q), ema_price).unwrap_or(u128::MAX)
}

/// Global utilisation (§8.7): OI at mark over the inventory at mark, `⌈OI · 10⁴ / I⌉` bps
/// (rounded up as the program's per-market `utilization_bps`); `None` without a priced
/// inventory.
pub fn global_util_bps(oi_lamports: u128, inventory_value_lamports: u128) -> Option<u64> {
    if inventory_value_lamports == 0 {
        return None;
    }
    shortwin_math::num::mul_div(
        oi_lamports,
        10_000,
        inventory_value_lamports,
        shortwin_math::Rounding::Ceil,
    )
    .ok()
    .and_then(|v| u64::try_from(v).ok())
}

/// A count's share of `total` in bps, `⌊n · 10⁴ / total⌋` (the exit rates of §8.3, the loss
/// shares of §8.4); `None` for an empty total.
pub fn share_bps(n: u128, total: u128) -> Option<u128> {
    (total > 0).then(|| n.saturating_mul(10_000) / total)
}

/// A lender's shares carried through consecutive epoch records `(num, den)` (§6.3; SQL
/// `sw_shares_at_epoch`): [`convert_shares`] per record, in epoch order.
pub fn shares_at_epoch(shares: u128, records: impl IntoIterator<Item = (u8, u128)>) -> u128 {
    records
        .into_iter()
        .fold(shares, |s, (num, den)| convert_shares(s, num, den))
}

/// Seconds of `[from, to)` covered by the half-open intervals `[t0, t1)` (the rollups'
/// `positions_open_s` / `coverage_paused_s`, §8.6); overlapping intervals count once each, as
/// the SQL sums them (the projections' intervals never overlap).
pub fn overlap_seconds(intervals: &[(i64, i64)], from: i64, to: i64) -> i64 {
    intervals
        .iter()
        .map(|&(t0, t1)| (t1.min(to) - t0.max(from)).max(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::WAD;

    #[test]
    fn tvl_counts_free_tokens_lender_sol_and_vaults() {
        // 1,000 free tokens at 0.5 lamport each, 7 lamports for lenders, 100 in vaults.
        assert_eq!(tvl_lamports(1_000, WAD / 2, 7, 100), 607);
        assert_eq!(oi_lamports(10, 3 * WAD), 30);
        assert_eq!(lender_value_lamports(1_000, 10, WAD / 2, 7), 512);
    }

    #[test]
    fn p_ref_prefers_the_point_then_the_clamped_crank() {
        assert_eq!(p_ref(Some(5), 9, 9), (9, PRefSource::BoxPoint));
        assert_eq!(p_ref(Some(5), 9, 12), (9, PRefSource::CrankClamped));
        assert_eq!(p_ref(Some(10), 9, 12), (10, PRefSource::CrankClamped));
        assert_eq!(p_ref(None, 9, 12), (10, PRefSource::BoxMid));
    }

    #[test]
    fn a_gap_settlement_is_a_loss_and_a_capped_one_is_not() {
        // q = 100 tokens at 2 lamports: worth 200; S1 paid 150 → loss 50.
        let s1 = settlement_outcome(150, 100, 2 * WAD);
        assert_eq!(s1.token_value, 200);
        assert_eq!(s1.lender_pnl, -50);
        assert_eq!(s1.realised_lender_loss, 50);
        // S2 paid 230 (fair value incl. penalty and rent) → gain 30, no loss.
        let s2 = settlement_outcome(230, 100, 2 * WAD);
        assert_eq!((s2.lender_pnl, s2.realised_lender_loss), (30, 0));
    }

    #[test]
    fn slippage_utilisation_and_intervals() {
        assert_eq!(exit_slippage_bps(103, 100), Some(300));
        assert_eq!(exit_slippage_bps(99, 100), Some(-100));
        assert_eq!(exit_slippage_bps(1, 3), Some(-6_666));
        assert_eq!(exit_slippage_bps(5, 0), None);
        assert_eq!(inventory_value_lamports(1_000, 10, WAD / 2), 505);
        assert_eq!(global_util_bps(1, 3), Some(3_334));
        assert_eq!(global_util_bps(0, 0), None);
        assert_eq!(share_bps(1, 3), Some(3_333));
        assert_eq!(share_bps(1, 0), None);
        assert_eq!(shares_at_epoch(5_000_000, [(1, 1_000), (1, 1_000)]), 5);
        assert_eq!(shares_at_epoch(5_000_000, [(1, 1_000), (0, 1)]), 0);
        assert_eq!(shares_at_epoch(7, []), 7);
        assert_eq!(overlap_seconds(&[(10, 20), (30, 100)], 0, 50), 30);
        assert_eq!(overlap_seconds(&[(10, 20)], 20, 50), 0);
    }

    #[test]
    fn splits_and_conversions_floor() {
        assert_eq!(rent_protocol_share(999, 1_500), 149);
        assert_eq!(borrower_pnl(120, 100, 5, 1), 14);
        assert_eq!(convert_shares(1_999, 1, 1_000), 1);
        assert_eq!(convert_shares(5, 0, 1), 0);
        assert_eq!(open_impact_bps(100, WAD, 99), 100);
        assert_eq!(open_impact_bps(100, WAD, 101), 0);
    }
}
