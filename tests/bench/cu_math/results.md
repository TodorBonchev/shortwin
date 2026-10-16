# T-CU-MATH results

Per-call compute units of `shortwin-math` on SBF v3 (platform-tools v1.57, Mollusk 0.15.1; 50 iterations, loop overhead subtracted). Written by `CU_MATH_WRITE=1 cargo test --release` in `tests/bench/cu_math` (README.md); CI fails when a value exceeds its budget.

| Primitive | ARCH §6.1 (V18) | M0b measured | Budget |
|---|---|---|---|
| `mul_div` u128 fast path | 691 | 688 | 830 |
| `mul_div` 256-bit fallback (Knuth D) | 783 | 792 | 940 |
| `mul_div_u64` | 267 | 96 | 321 |
| native u64 `a·b` then `div_ceil` (reference) | 76 | 84 | 92 |
| PumpSwap `buy_exact_out` (3 fee components) | 1452 | 944 | 1743 |
| CPMM `buy_exact_out` | 1872 | 1892 | 2247 |
| AMM v4 `buy_exact_out` | 959 | 973 | 1151 |
| `reprice(L, P)` | 843 | 985 | 1012 |
| `liquidity` = `isqrt(X·Y)` u128 | 734 | 692 | 881 |
| `sqrt_u256` (off the on-chain paths since v0.4) | 624 | 670 | 749 |
| `rent::due` | 1287 | 1416 | 1545 |
| `rent::credited_for` | 2637 | 2988 | 3165 |
| `rent::accrue` | 1109 | 608 | 1331 |
| `record` + `integrate` (one EMA roll incl. the cached `reprice`) | 3833 | 4387 | 4600 |
| `ring_medians` (4 × 8 bucket extremes) | 3208 | 3484 | 3850 |
| `sigma_xy` | 1473 | 1201 | 1768 |
| EMA-leg entry, v0.4 (cached `(x, y)` + quote + CR) | 1468 | 951 | 1762 |
| repriced quote + CR (v0.3 EMA-leg entry; a box corner) | 2289 | 1910 | 2747 |
| `settle::box_of` (full ring) | — | 3575 | 4307 |
| `persist::liq_ema_leg(3)`, every entry below `L` (PumpSwap quote + CR each) | — | 2908 | 3490 |
| `sustained_growth(3)` (full ring, a pump) | — | 1518 | 1770 |
| `pool::cal_advance` (one day elapsed) | — | 261 | 314 |
| `quote::curve_synthetic` past `rT` (`B̂_v3`: the curve leg and the would-be pool's part; pump-final §5.2) | — | 2429 | 2915 |
| `quote::migrating_fv` after a synthetic pool part (ρ-scaled, at the pool's PumpSwap tier; pump-final §5.4) | — | 4743 | 5692 |
