//! Raydium and PumpSwap pool selection: the locked LP share `λ`, the locked depth `D`, its
//! SOL side and the listing floor (`shortwin_math::{pool_select, tier::locked_sol}`; ECON
//! §2.1, §6.2; ARCH §2.6, §4.7). The create-market candidates and the venue state read them.

use shortwin_math::{pool_select, tier};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

/// Locked LP share `λ_bps = ⌊(lpCounter − lpSupply)·10⁴/lpCounter⌋` (ECON §2.1): `lpCounter`
/// the venue's own LP count (AMM v4 `lp_amount`, CPMM and PumpSwap `lp_supply`), `lpSupply`
/// the LP mint's circulating supply; 0 with no counter or a supply at or above it.
#[wasm_bindgen(js_name = lambdaBps)]
pub fn lambda_bps(lp_counter: u64, lp_supply: u64) -> u32 {
    pool_select::lambda_bps(lp_counter, lp_supply)
}

/// Locked constant-product depth `D = ⌊λ_bps·isqrt(X·Y)/10⁴⌋` on the pricing reserves
/// (ECON §2.1; ARCH §2.6), λ floored to whole bps first.
#[wasm_bindgen(js_name = lockedDepth)]
pub fn locked_depth(lp_counter: u64, lp_supply: u64, x: u64, y: u64) -> Result<u128, JsError> {
    Ok(pool_select::locked_depth(lp_counter, lp_supply, x, y)?)
}

/// Locked SOL of a pool at the price `P` (WAD), `⌊D·isqrt(P)/10⁹⌋` lamports (ECON §6.2;
/// ARCH §4.7), saturating.
#[wasm_bindgen(js_name = lockedSol)]
pub fn locked_sol(locked_depth: u128, p: u128) -> u128 {
    tier::locked_sol(locked_depth, p)
}

/// The listing floor on locked depth (M4-SEC-1, M4-SEC-3; ARCH §2.6 "Eligibility"):
/// `poolMinRealSol·λ_min ≤ lockedSol(D, P_ref)·10⁴`; `false` for `λ_min = 0`. A new market
/// passes its candidate's own spot as `P_ref`.
#[wasm_bindgen(js_name = lockedFloorOk)]
pub fn locked_floor_ok(
    locked_depth: u128,
    p_ref: u128,
    lambda_min_bps: u32,
    pool_min_real_sol: u64,
) -> bool {
    pool_select::locked_floor_ok(
        locked_depth,
        p_ref,
        lambda_min_bps.into(),
        pool_min_real_sol,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const WAD: u128 = 1_000_000_000_000_000_000;

    #[test]
    fn lambda_and_depth() {
        // The Fartcoin AMM v4 pool of the pool_select tests: 9,978 bps.
        assert_eq!(lambda_bps(10_000, 22), 9_978);
        assert_eq!(lambda_bps(0, 0), 0);
        assert_eq!(lambda_bps(5, 7), 0);
        assert_eq!(lambda_bps(u64::MAX, 0), 10_000);
        // λ floored first: ⌊9,978·isqrt(10¹²)/10⁴⌋ = 997,800.
        assert_eq!(
            locked_depth(10_000, 22, 1_000_000, 1_000_000).unwrap(),
            997_800
        );
        assert_eq!(locked_depth(0, 0, 1_000_000, 1_000_000).unwrap(), 0);
        assert_eq!(
            locked_depth(1, 0, u64::MAX, u64::MAX).unwrap(),
            u128::from(u64::MAX)
        );
    }

    #[test]
    fn locked_sol_and_floor() {
        // D = 10¹³ at P = 1 WAD: isqrt(P) = 10⁹, so the locked SOL is D itself.
        assert_eq!(locked_sol(10_000_000_000_000, WAD), 10_000_000_000_000);
        assert_eq!(
            locked_sol(10_000_000_000_000, 4_000_000_000_000_000_000),
            20_000_000_000_000
        );
        assert_eq!(locked_sol(u128::MAX, u128::MAX), u128::MAX);
        // 25 SOL floor at λ_min 50 %: 12.5 SOL locked passes, a lamport less does not.
        let floor = 25_000_000_000u64;
        assert!(locked_floor_ok(12_500_000_000, WAD, 5_000, floor));
        assert!(!locked_floor_ok(12_499_999_999, WAD, 5_000, floor));
        assert!(!locked_floor_ok(u128::MAX, WAD, 0, floor));
        for (d, p, lam, min) in [
            (12_500_000_000u128, WAD, 5_000u32, floor),
            (1, 1, 1, 0),
            (997_800, 2_333_333_333_333_333_333, 2_000, 1_000_000),
        ] {
            assert_eq!(
                locked_floor_ok(d, p, lam, min),
                pool_select::locked_floor_ok(d, p, u64::from(lam), min)
            );
        }
    }
}
