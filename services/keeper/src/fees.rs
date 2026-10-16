//! Priority policy (ARCHITECTURE §7.1 duty 14; C0-17): the price starts from a percentile of
//! the recent per-account prioritization fees on the transaction's `Market`s, escalates when
//! the market missed required observations (a transaction that did not land in its slot), and
//! is capped at the O17 ceiling (observations) or the exit ceiling (liquidations, expiries,
//! settlements).

/// The price a missed observation escalates from when recent fees are all 0: 1,000
/// micro-lamports per CU, ECON §5.8's "normal" price.
pub const ESCALATION_SEED: u64 = 1_000;

/// The `bps` percentile (nearest rank) of `fees`; 0 when empty.
pub fn percentile(fees: &[u64], bps: u32) -> u64 {
    if fees.is_empty() {
        return 0;
    }
    let mut v = fees.to_vec();
    v.sort_unstable();
    let n = v.len();
    // Nearest rank: ⌈p·n⌉, at least 1.
    let rank = (u128::from(bps) * n as u128).div_ceil(10_000).max(1);
    let idx = usize::try_from(rank).unwrap_or(n).min(n) - 1;
    v[idx]
}

/// The priority price, micro-lamports per CU: `max(percentile(recent), floor)` (seeded at
/// [`ESCALATION_SEED`] when it is 0 and the market is missing observations), times
/// `(1 + escalate_bps/10⁴)^misses`, capped at `ceiling`.
pub fn priority_price(
    recent: &[u64],
    percentile_bps: u32,
    floor: u64,
    ceiling: u64,
    escalate_bps: u32,
    misses: u32,
) -> u64 {
    let mut price = percentile(recent, percentile_bps).max(floor);
    if misses > 0 && price == 0 {
        price = ESCALATION_SEED;
    }
    let num = 10_000u128 + u128::from(escalate_bps);
    let mut p = u128::from(price);
    for _ in 0..misses.min(64) {
        p = p.saturating_mul(num) / 10_000;
        if p >= u128::from(ceiling) {
            break;
        }
    }
    u64::try_from(p)
        .unwrap_or(u64::MAX)
        .min(ceiling)
        .max(floor.min(ceiling))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        assert_eq!(percentile(&[], 7_500), 0);
        assert_eq!(percentile(&[5], 7_500), 5);
        assert_eq!(percentile(&[4, 1, 3, 2], 7_500), 3);
        assert_eq!(percentile(&[4, 1, 3, 2], 10_000), 4);
        assert_eq!(percentile(&[4, 1, 3, 2], 0), 1);
    }

    #[test]
    fn price_escalates_and_is_capped() {
        // Quiet market: 0, or the floor.
        assert_eq!(priority_price(&[0, 0, 0], 7_500, 0, 10_000, 5_000, 0), 0);
        assert_eq!(
            priority_price(&[0, 0, 0], 7_500, 100, 10_000, 5_000, 0),
            100
        );
        // Contention: the p75.
        assert_eq!(
            priority_price(&[10, 200, 3_000, 50], 7_500, 0, 10_000, 5_000, 0),
            200
        );
        // Misses escalate ×1.5 each from the seed, up to the ceiling.
        assert_eq!(priority_price(&[], 7_500, 0, 10_000, 5_000, 1), 1_500);
        assert_eq!(priority_price(&[], 7_500, 0, 10_000, 5_000, 2), 2_250);
        assert_eq!(priority_price(&[], 7_500, 0, 10_000, 5_000, 10), 10_000);
        assert_eq!(
            priority_price(&[1_000_000], 7_500, 0, 10_000, 5_000, 0),
            10_000
        );
    }
}
