//! Owner: oracle track. Liquidation persistence: the EMA leg on the newest ring entries'
//! cached reprojection (ECON §5.5; ARCH §3.6 step 3, §4.5). Normative reference: the ring
//! loop of `Market.status` in `sim/shortwin_sim.py` v0.4.1.
//!
//! v0.4 removed the Dutch exit clock (`exec_run`, the run dating of `liq_ema_leg` and its
//! `liq_persist_s`) and moved settlement arming to [`crate::settle`] (ECON "Removed in
//! v0.4"; ARCH §9 M0b). What remains needs no seconds condition: the ring integrates at
//! most once per second, so `n` entries always span at least `n − 1` seconds.

use crate::error::MathError;
use crate::observe::Ring;

/// Liquidation EMA leg (ECON §5.5; ARCH §4.5): the newest `n` (`liq_persist_n`) ring
/// entries all have `CR(V_eff, B̂(Q; x_i, y_i)) < L` on their cached reprojection
/// `(x_i, y_i)` (decision 3). `false` while the ring holds fewer than `n` entries; `n = 0`
/// holds vacuously (the sim's `run ≥ 0`; compiled bounds give `n ∈ [2, 3]`).
///
/// `below(x, y)` is the position's risk-quote predicate on the cached state, as
/// [`crate::risk::cr_below`] of the quote on `(x, y)` at the capped fee; an undefined
/// quote (including the entry `(0, 0)` that has no reprojection) counts as below. It is
/// called at most `n` times, newest entry first, and stops at the first entry above `L`.
///
/// Errors: those of `below`; `InvalidInput` for a corrupted ring.
pub fn liq_ema_leg<F>(ring: &Ring, mut below: F, n: u8) -> Result<bool, MathError>
where
    F: FnMut(u128, u128) -> Result<bool, MathError>,
{
    ring.check()?;
    let n = usize::from(n);
    if ring.len() < n {
        return Ok(false);
    }
    for e in ring.iter().take(n) {
        if !below(e.x, e.y)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::observe::RingEntry;

    fn ring(xs: &[u128]) -> Ring {
        let mut r = Ring::default();
        for (i, &x) in xs.iter().enumerate() {
            r.push(RingEntry {
                x,
                y: 2 * x,
                ts: i64::try_from(i).unwrap(),
                ..RingEntry::default()
            })
            .unwrap();
        }
        r
    }

    #[test]
    fn newest_n_entries_on_the_cached_state() {
        // below ⇔ x ≥ 10 (a smaller token reserve means a costlier buyback).
        let calls = core::cell::Cell::new(0);
        let below = |x: u128, y: u128| {
            calls.set(calls.get() + 1);
            assert_eq!(y, 2 * x);
            Ok(x >= 10)
        };
        let r = ring(&[5, 11, 12, 13]);
        assert_eq!(liq_ema_leg(&r, below, 3), Ok(true));
        assert_eq!(liq_ema_leg(&r, below, 4), Ok(false));
        assert_eq!(liq_ema_leg(&r, below, 0), Ok(true));
        assert_eq!(liq_ema_leg(&r, below, 5), Ok(false));
        // The newest entry fails: one call, no leg.
        calls.set(0);
        let r = ring(&[11, 12, 9]);
        assert_eq!(liq_ema_leg(&r, below, 2), Ok(false));
        assert_eq!(calls.get(), 1);
        let err = |_: u128, _: u128| Err(MathError::Overflow);
        assert_eq!(liq_ema_leg(&r, err, 2), Err(MathError::Overflow));
        let mut bad = r;
        bad.len = 9;
        assert_eq!(
            liq_ema_leg(&bad, |_, _| Ok(true), 2),
            Err(MathError::InvalidInput)
        );
    }
}
