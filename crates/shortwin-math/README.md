# shortwin-math

The protocol math of short.win, shared bit for bit by the Anchor program, the keeper and
the frontend (through [`shortwin-math-wasm`](../shortwin-math-wasm)): venue quotes, the
observation/EMA oracle with its cached reprojection, the settlement box and arming, exit
bounds, rent, risk predicates, caps, tiers, waterfalls, the lender pool and the parameter
checks (ARCHITECTURE §2.2, §9 M0 and M0b).

- `#![no_std]`, zero runtime dependencies, `#![forbid(unsafe_code)]`, and the ARCH §2.2
  denies `clippy::{arithmetic_side_effects, cast_possible_truncation, cast_sign_loss,
  unwrap_used}`. Builds for the host, `wasm32-unknown-unknown` and SBF v3 (platform-tools
  v1.57, rustc 1.95).
- Every fallible function returns `Result<_, MathError>`; quotes return `Option` (`None` =
  undefined, `B̂ = +∞`). `MathError` codes are stable (0–5 arithmetic and domain,
  6–12 lender-pool refusals matching ARCH §3.11's lender errors).
- Every division states its rounding (`num::Rounding`), per the ECONOMICS §2.1 numerics
  table: quotes, fees, rent, penalty, margin, FV and a lender's own lock up; shares,
  payouts, credits, claims, lock terms and the lock aggregate, keeper/protocol cuts, the
  EMA, `P_ref` and `max_in` down.
- u128 fast paths everywhere; a 256-bit intermediate only for the products ECON §2.2 lets
  exceed 2¹²⁸ (and, bit-identically, as a fallback beyond its bounds). No U256 square root
  on an on-chain path.
- Normative references (ARCH §2.2, §8.1): the ECONOMICS v0.4.1 formulas,
  `docs/design/sim/pool_model.py` v0.4.2 (lender pool; KI-65), `docs/design/sim/shortwin_sim.py`
  v0.4.1 (quotes, observation, exit bounds, settlement, waterfalls, `validate_params`) and
  its attack scripts `docs/design/sim/attacks/*.py`, all **with their default switches**
  (the analysis switches `box_obs`, `pprime_raw`, `ref_cap_lo = False`, `liq_reserve ≠
  "none"` only feed comparison rows); plus the venue math of `docs/research/`. The
  differential suites match the sims **exactly, with no tolerance or exception**.

## Modules: API → spec → tests

Section references: E = `docs/design/ECONOMICS.md`, A = `docs/design/ARCHITECTURE.md`,
R-* = `docs/research/*.md`. Test files are in `tests/`; vector files in
`tests/vectors/<track>/`.

| Module | Public API | Spec | Tests |
|---|---|---|---|
| `num` | `Rounding`, `U256`, `WAD`/`SQRT_WAD`/`BPS`/`PPM`, `div*`, `mul_div{,_u64,_u256,_i128}` (`mul_div_u64` native when `a·b` fits u64), `mul_wad`/`div_wad`, `mul_bps*`, `mul_ppm*`, `sqrt_u128`/`sqrt_u256` (floor and `isqrt⁺`), `mul_le`, `saturating_u64`, `overflow_to_max`, casts | A §2.2 (A30), E §2.1, §2.2 | `num_u256.rs`, `num_muldiv.rs` (vs num-bigint, incl. `mul_div_u64_native_path`); `num_vectors.rs` ← `num/num.json` |
| `error` | `MathError` (`code`, `from_code`, `as_str`, `ALL`) | A §2.2, §3.11 | unit |
| `consts` | `EMA_TAU_S`, `RING_LEN`, `EMA_WARMUP_BUCKETS` (`u8`, the post-change warm-up, PROPOSED, M4R), `REF_GUARD_TOL_BPS` (PROPOSED, M4R), `P_ACC_MAX`, `P_OPEN_MAX`, `S3_OBS_GAP_S`, `LOCK_BUCKET_S`, `LOCK_BUCKETS`, `COVERAGE_GAP_S` (40 s since the owner's decision of 2026-10-01, OWNER-M7-12; was 30), `MAX_RELEASE_SPAN_S`, `PUMPSWAP_LOCK_MIN_BPS` (10,000: PumpSwap's `λ_min` — sizing, the tier, and since M6E-1 the exit reference's cap and a price start's anchor — OWNER, OWNER-M6D-1 / ECON #106); compile-time asserts against the compiled bounds (`RING_LEN ≥ max(liq_persist_n, kappa_growth_n + 1)`, the lock-calendar horizon, `liq_persist_n ≤ EMA_WARMUP_BUCKETS = RING_LEN`) | E §2.2, §5.7, §15; A §2.3.3 | unit |
| `params` | `Params`, `ParamSet` (`DEFAULT`, `get`/`set` by ECON §15 name, `settle_terms`), `Rule` (`name`, `bounds`, `in_sim`, `per_tier`), `Violation`, `check_sim`/`validate` (= `shortwin_sim.validate_params`, every ECON §15 bound and check of the parameters the sim models), `check_econ`/`validate_econ` (the ECON §15 bounds of the parameters the sim does not model), `check` (both), `spec_cr`, `POOL_FAST_MAX_S`; projections `Params::{exit_params, settle_keeper, expiry_keeper, penalty_split, s3_terms}` | E §15; A §2.3.3 (A8, A73) | `params_vectors.rs` ← `params/params.json` (T-CONFIG host part); unit (every bound at its edges by name, the rule-set split) |
| `quote::sol_leg` | `buy_exact_out(q, X, Y, eps1, &FeeBps)` (ECON's quote), `curve_charge_exact_out(q, X, Y, &FeeBps)` (the curve's own charge), `sell_exact_in(q, X, Y, &FeeBps, real_quote_cap)` | E §3.1; A §4.3; R-PUMP §4, R-VEN §2.4 | `quotes_mainnet.rs` ← `quotes/mainnet.json` (curve, PumpSwap); `quotes_sim.rs` ← `quotes/sim.json`; `quotes_props.rs` |
| `quote::ray_v4` | `buy_exact_out(q, X_sol, Y_tok, num, den)`, `sell_exact_in(a, X_tok, Y_sol, num, den)` | E §3.1; A §4.3; R-RAY §3.3 | `quotes_mainnet.rs` (AMM v4), `quotes_sim.rs`, `quotes_props.rs` |
| `quote::cpmm` | `buy_exact_out(q, X_sol, Y_tok, &CpmmFees)`, `sell_exact_in(a, X_tok, Y_sol, &CpmmFees)` | E §3.1; A §4.3; R-RAY §3.3 | `quotes_mainnet.rs` (CPMM), `quotes_port.rs` ← `quotes/port.json`, `quotes_props.rs` |
| `quote::{reprice, …}` | `reprice(L, P)` (`sp = isqrt⁺(P)`, `X = ⌊L·10⁹/sp⌋`, `Y = ⌈L·sp/10⁹⌉`, `None` when `X = 0`), `reprice_token(L, P)`, `liquidity(X, Y) -> Result` (u128 `isqrt`, `Overflow` if `X·Y ≥ 2¹²⁸`), `curve_reprice(L, P, CurveOffsets)` | E §2.1, §5.3; A §4.3, §4.4 (R3B-7 c) | `quotes_sim.rs` (`reprice_matches_sim`, `liquidity_matches_sim`), `quotes_props.rs` (`reprice_is_exact`), `integration_props.rs` |
| `quote::{curve_straddle, …}` | `curve_synthetic` (`B̂_v3`, pump's synthetic migration), `curve_straddle` (`B̂_str`, a migrating market's states), `curve_two_leg` (v1.1 only), `migrated_pool` (with `post_complete_quote_in`), `migrating_fv_price` (ρ), `migrating_fv` (FV on a `PumpMigrating` market), `migration_boost` (`⌊X₁·Y₁/10¹⁵⌋`), `CurveState`, `MigrationParams`, `STANDARD_POOL_TOKENS` | E §3.1, §10.1–§10.2; A §4.3, §5.4 (V1-9); pump-final §5.1–§5.4 | `quotes_mainnet.rs` (migrations; `pump_synthetic_completions` ← `quotes/synthetic.json`), `quotes_synthetic_g0.rs` (G0's ELF figures), `quotes_sim.rs`, `quotes_props.rs` (`straddle_*`, `synthetic_*`, `migrating_fv_*`); unit |
| `quote::{Pricing, CurvePricing}` | `Pricing::{PumpSwap, RayV4, Cpmm}` and the pump curve's `CurvePricing` (live and repriced; above `rT` `B̂_v3` when `synthetic` holds the synthetic fee, else `B̂_str`): `buy`, `sell`, `buy_repriced` (live fee tier), `capped`, `oneway_ppm`, `oneway_bps` (display); `CurvePricing::liquidation_price` | A §4.2, §4.3 (two fee views, V2-1; `B̂_str`, V1-9); pump-final §5.3 | `quotes_sim.rs` (`repriced_quotes_match_sim`); `integration_props.rs` (`pricing_dispatch`, `curve_liquidation_price_*`, `straddle_dip_at_rt_is_bounded`); unit (`curve_pricing_is_synthetic`) |
| `fees` | `FeeBps` (`capped`: one combined fee at the cap above it), `FeeTier`, `fee_tier`, `pump_curve`, `pump_curve_at_supply` (the curve tier on the mint's live supply: the synthetic exit's fee), `pump_amm`, `ray_v4`, `cpmm`, `CurveFeeSchedule`, `PammFeeSchedule`, `RayV4Fee`, `CpmmConfig`, `CpmmPoolFees`, `CpmmFees`, `Direction`; `oneway_ppm` (φ, u32 ppm) / `oneway_bps` (display) / `capped` on each fee; `exceeds_cap(φ_ppm, cap_bps)` (the one gate) | E §2.1, §3.1; A §4.2, §4.3 (V2-1); R-PUMP §4, R-VEN §2.3, R-RAY §2.3, §2.5 | `quotes_mainnet.rs` (tier resolution on every trade), `quotes_sim.rs` (risk quotes at a cap), `quotes_props.rs` (`fee_resolution`, `capped_fees_never_quote_higher`) |
| `pool_select` | `lambda_bps`, `locked_depth` (`⌊λ_bps·isqrt(X·Y)/10⁴⌋`), `lock_share_ok` (no longer an eligibility clause, M4-SEC-1), `locked_floor_ok` (the eligibility floor `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol`, M4-SEC-1 / -3), `eligible`, `position_floor_ok` and `eligible_with_positions` (the locked SOL itself for an adoption that carries open positions, M4V-1), `depth_at_lambda` (also the adoption anchor `D/λ_min`, M4R), `dominates`, `fast_path_qualifies`, `PoolFacts` (`fee_ppm`, `locked_depth`, `p_ref`), `EligibilityParams` | E §2.1; A §2.6 (A42, V2-7, V2-8; the M4 security review) | `quotes_sim.rs` (`locked_depth_matches_sim`), `quotes_props.rs` (`locked_depth_properties`, `eligibility`, `locked_floor_keeps_the_pair_and_ignores_unlocked_lp`), `integration_props.rs` (`locked_depth_moves_only_by_retained_fees`) |
| `observe` | `Bucket`, `LastBucket`, `Ema` (with `warm`, `guard`, M4R), `Ring`, `RingEntry` (`p, l, x, y, lo, hi, llo, lhi, ts`; `reprojected`), `EmaParams`, `Observation` (`new_raydium`: a marked Raydium price start, M4R), `VenueChange`, `RING_LEN`; `record`, `integrate` (→ pre-roll gap; the post-change warm-up seed and the reference guard, M4R), `price_up_bound`, `price_down_bound` (M6-SEC-1), `mark_restart`, `warmed`, `coverage_gap`, `robust_spot`, `sustained_growth`, `ring_medians`, `ring_liq_lower_median`, `rewrite_liquidity` | E §5.1–§5.4; A §3.0, §4.4 | `oracle_vectors.rs` ← `oracle/units_{observe,ring}.json`; `oracle_props.rs` (incl. `warm_up_seed_properties`, P1–P6); unit |
| `persist` | `liq_ema_leg(&Ring, below(x, y), n)` | E §5.5; A §3.6, §4.5 | `oracle_vectors.rs` ← `oracle/units_persist.json`; `oracle_props.rs` (`liq_ema_leg_is_the_newest_n`) |
| `bounds` | `ExitParams`, `ref_liquidity` (`min(L_ema, L_mlo)` while the post-change guard is set, M4R; the EMA terms capped at `ref_depth_cap(D, λ_min)` = `⌊D·10⁴/λ_min⌋`, M4F-1 — on PumpSwap at `PUMPSWAP_LOCK_MIN_BPS`, i.e. `D`, M6E-1), `kappa_eff(g, params)`, `liq_reference` (with the `lo_x` cap), `expiry_reference`, `pi_ref`, `max_in_liq` (no reserve), `max_in_exp` (rent and fee senior) | E §8.2, §12.2, §12.9; A §3.6, §3.7, §5.2 | `oracle_vectors.rs` ← `oracle/units_bounds.json`; `oracle_props.rs` |
| `settle` | `SettleBox`, `box_of`, `BoxQuotes`, `s1_legs`, `s2_legs`, `refuted`, `not_liquidatable`, `pprime`, `CornerAmount`, `amount_at`, `amount_ok`, `settlement`, `S3Chain`, `S3Terms`, `s3_due`, `ArmState`, `ArmChange`, `DisarmReason`, `SettleTerms`, `SettleInputs` (with `warmed`, M4R), `Settlement`, `Evaluation`, `evaluate` (A §3.8's order; nothing while warming on an executable venue, M4R); the route predicates `s3_no_route`, `pool_fail` and the chain step `chain_step` (sim `Market._chain`; M2) | E §5.6, §8.6, §10.3; A §3.0 step 8, §3.8, §4.5, §5.3 | `settle_vectors.rs` ← `settle/*.json`; `settle_props.rs`; unit (`route_predicates_and_chain_step`) |
| `rent` | `RentTerms`, `Basis`, `Aggr` (`add`/`sub` fold `10⁴ − p`), `PosRent` (+`rent_protocol_bps`), `P_ACC_MAX`, `YEAR_S`; `utilization_bps`, `rate_bps` (display), `accrue` (clamps at `P_ACC_MAX`), `accrued`, `due`, `credited_for(&PosRent, &Basis)`, `lender_credit(&Aggr, δJ)`, `min_rent` | E §2.1, §7; A §4.5 | `risk_rent.rs` ← `risk/rent.json` |
| `risk` | `Quote`, `SettleKind`, `SettleKeeperTerms`, `ExpiryKeeperTerms`, `Budget`, `v_eff`, `cr_bps`, `cr_below`, `ring_below`, `liquidation_price`, `spot_leg`, `liquidatable`, `buyback_allowed`, `price_wad`, `sigma`, `sigma_xy`, `margin_d4` (cached `(x, y)`; the spot quote alone when the EMA quote is undefined), `open_fee`, `fv_rate_bps`, `fair_value`, `k_s`, `k_e` | E §3.3, §4, §5.5–§5.6, §8.6–§8.7; A §4.5 | `risk_predicates.rs` ← `risk/exits.json`; `risk_waterfall.rs` (FV, `p'`, keepers); `integration_props.rs` (liquidation price) |
| `caps` | `max_q_position`, `max_total_q` (the sim's integer form), `fee_size_cap`, `fee_oi_cap`, `oi_cap` (φ in ppm), `util_ok`, `curve_gamma_ok`, `curve_gamma_max_q`, `wallet_ok`, `notional_ok`, `conservative_x0` (`isqrt⁺(P_ema)`; the locked depth over `pool_lock_min_bps` on Raydium, over `PUMPSWAP_LOCK_MIN_BPS` = 100 % on PumpSwap since ECON #106 — `caps_sim.rs` checks the vectors' PumpSwap `λ_min` is the constant); the open guard `spot_near_ema` (`|P_spot − P_ema|·10⁴ ≤ dev·P_ema`, exact; M2) | E §2.1, §4, §6.1; A §2.6 rule 1, §3.4 steps 3–4, §4.6 | `caps_sim.rs` ← `quotes/caps.json`; unit (`spot_near_ema_is_exact`) |
| `tier` | `Tier`, `TierParams::DEFAULTS`, `TierVenue` (Raydium's and — since ECON #105, M6R-1 — PumpSwap's locked-depth bound, the latter at `λ_min` = `PUMPSWAP_LOCK_MIN_BPS` on the program's path since #106, OWNER-M6D-1; `Unlocked` for the program's test-venue mock), `OraclePrice`, `TierInputs`, `locked_sol`, `liquidity_lamports`, `assign`, `AGE_C_S`, `AGE_A_S` | E §6.2–§6.3; A §2.3.3, §4.7 | `tier_arch.rs` ← `quotes/caps.json`; unit (the threshold's u128 path equals 256 bits) |
| `waterfall` | `close`, `liquidate` (keeper floor at `K_s`), `expire`, `settle` (vesting lump) → `Payout` (+`keeper_topup`, `lump`); `ExitInputs`, `PenaltySplit` | E §8.1–§8.7 (I-E4, I-E10); A §5.2 | `risk_waterfall.rs` ← `risk/exits.json` |
| `pool` | `Pool` (Market part), `Vault` (MarketVault part), `Lender` (+`lock_until`), `ShareEpoch`, `Standing`, `SweepSource`, `Resolution`, `Settled`, `LOCK_BUCKET_S`, `LOCK_BUCKETS`, `WIPE_*`; `deposit`, `withdraw`, `allowance`, `allowance_view`, `claim`, `cover`, `touch`, `convert`, `release`, `vest`, `credit`, `credit_r`, `accrue`, `sweep`, `borrow`, `repay`, `settle`, `rent_top_up`, `maybe_roll`, `resolve`, `cal_advance`, `cal_add`, `cal_sub`, `lock_until_for`, `lock_of`, `locked_total`, share helpers | E §9; A §3.3, §2.3.4, §2.3.6; `pool_model.py` v0.4.2 (KI-65: `ShareEpoch.conv_rem`, cumulative conversion; `touch`/`convert`/`deposit`/`withdraw`/`claim`/`cover` take `&mut [ShareEpoch]`) | `pool_vectors.rs` ← `pool/*.json`; `pool_props.rs`; `pool_econ.rs`; shared `pool_harness.rs` |

The names follow the binding list of A §2.2. Where a signature differs from that list
(extra arguments, `Result`/`Option` wrappers), the reason is in the design notes below.

### ARCH §8.1 properties

| Property (A §8.1) | Test |
|---|---|
| Round trips never create value; `B̂` monotone in q for every family (the straddle on each side of `rT`, with a ≤ 3 bps dip across it; `B̂_v3` across it); `reprice` gives `Y/X ≥ P` | `quotes_props.rs`: `buy_covers_sell_and_round_trips`, `monotone_in_q`, `straddle_monotone`, `synthetic_monotone_and_continuous`, `*_never_favours_the_trader`, `reprice_is_exact`; `integration_props.rs`: `straddle_dip_at_rt_is_bounded` |
| Kinked-rate continuity; basis-index rent equals the direct integral; `receivable` returns to 0 under random `rent_protocol_bps` changes (I-P7) | `risk_rent.rs`: `kinked_rate_is_continuous_and_monotone`, `basis_rent_equals_direct_integral`, `receivable_returns_to_zero`, `pool_model_receivable_matches` |
| Fee and penalty splits sum exactly; waterfall legs non-negative and sum to `V` incl. the keeper top-up (I-E4, I-E10) | `risk_waterfall.rs`: `*_conserves` (incl. `liquidation_conserves`), `default_splits_exact` |
| Honest liquidation cascade on a market filled to the OI cap (`attacks/cascade.py`): each basket exit's waterfall, fixed-δ bound and `max_in_liq`, `B ≤ max_in` (I-E1) | `risk_waterfall.rs`: `cascade_exits_match_sim` |
| EMA step bounds, own observation never read (I-E7), directional aggregation; one update per second with the pre-roll gap | `oracle_props.rs`: `ema_step_is_clamped`, `ema_is_monotone`, `one_update_per_second`, `mixed_bucket_freezes_ema` |
| Cached `(x, y)` of every ring entry equals `reprice(l, p)` after every integration and every `rewrite_liquidity`; the EMA leg reads only the newest `liq_persist_n` entries | `oracle_props.rs`: `rewrite_liquidity_resets_the_ring`, `liq_ema_leg_is_the_newest_n`; `oracle_vectors.rs` (`integrate_matches_sim`, `rewrite_liquidity_matches_sim`) |
| Settlement box: medians of the bucket extremes (5 of 8), every predicate on the box equals its value at the named corner, one pushed second cannot change a box decision, in a sustained uptrend the S1 corner is `P_ema`, arming (I-E5, I-E8); the two fee views (F-S7) | `oracle_props.rs`: `medians_need_five_of_eight`; `settle_props.rs`: `wider_boxes_never_make_a_decision_easier`, `box_holds_the_ema_point`, `uptrend_s1_corner_is_the_ema`, `uptrend_claim_needs_a_sustained_ramp`, `arming_invariants`, `fee_above_the_hard_cap_is_s3_evidence_and_refutes`; `settle_vectors.rs` (58 attack, 17 scenario, 6 random full replays, incl. `fv_push`'s 5-vs-6 attacker-only seconds) |
| Exit bounds with the fixed δ, the `lo_x` cap and the expiry reserve; liveness I-E11 and I-E1–I-E3 | `oracle_props.rs`: `references_are_bounded`, `max_in_properties`; `oracle_vectors.rs` (`references_match_sim`, `max_in_matches_sim`) |
| S1 take = `V − K_s`, S1 whenever its legs hold on the box (I-E5); the 1,000-path exit fuzz of ECON §13 incl. S2/S3 and box decisions | `settle_vectors.rs`: `exit_fuzz_decisions_match_sim` (S1/S2/S3 = 174/8/28, exactly ECON §13; the one S2 at the expiry fee pinned, O15), `o15_reading_applies_to_an_expired_s2`; `risk_waterfall.rs` (`settlement_conserves`, `settlements_match_sim`) |
| `D = λ·√(X·Y)` moves under swaps only by the retained fee (V2-7), not under unlocked LP | `integration_props.rs`: `locked_depth_moves_only_by_retained_fees`; `quotes_props.rs`: `locked_depth_properties` |
| Lender pool vs `pool_model.py` v0.4.2: pot draws never exceed the own deficit, repeated claims pay 0, liveness after a full wind-down (I-P6), no lock effective at or after `lock_until`, vest release linear and complete at `vest_end`, credits with `S = 0` to the wipe record or the protocol; lock calendar; I-P1–I-P7 incl. the untouched-state I-P4; the exit rent top-up vests (M10-8C-1); no rebase orphans, `Σ shares = S` once every lender has converted, in any conversion order (KI-65) | `pool_vectors.rs` (350,067 trace lines, see Test vectors), `pool_props.rs` (`invariants_hold_after_every_operation`, `expired_locks_leave_the_aggregate_untouched`, `repeated_wipes_and_rebases`), `pool_econ.rs` (every ECON §9.3–§9.6 table row) |
| Cap closed forms against brute force | `caps_sim.rs`: `impact_caps_are_the_sim_forms`, `fee_and_wallet_caps`, `utilization_and_gamma` |
| `validate_params` equals `shortwin_sim.validate_params` on random configs, every compiled bound at its edges (per tier for the per-tier bounds), the per-tier SPEC CR minima and the config gate's configurations (T-CONFIG): the same violations, it accepts exactly what the sim accepts, and `validate_econ` accepts every one of them (the vectors move only parameters the sim models) | `params_vectors.rs`: `validate_equals_the_sim`, `validate_refuses_what_the_sim_refuses`, `defaults_are_the_sims` |
| U256 against num-bigint | `num_u256.rs`, `num_muldiv.rs` |
| Mainnet trades to the raw unit (curve, PumpSwap, AMM v4, CPMM; ECON T1–T11) | `quotes_mainnet.rs`: 969 trades (123 curve, 107 PumpSwap, 408 AMM v4, 331 CPMM) + 4 migrations; A §4.2 fee branches without a trade are listed UNVERIFIED (`fee_branch_coverage`, design notes) |
| Per-primitive CU within the §6.1 measured value + 20 % (T-CU-MATH) | [`tests/bench/cu_math`](../../tests/bench/cu_math) (Mollusk; `results.md`) |

## Running the tests

```sh
cargo test -p shortwin-math                         # every suite; proptests ≥ 2,000 cases each
PROPTEST_CASES=20000 cargo test -p shortwin-math    # deeper (can only raise the count)
cargo clippy -p shortwin-math --all-targets -- -D warnings
cargo fmt --all --check
cargo build -p shortwin-math -p shortwin-math-wasm --target wasm32-unknown-unknown --release
RUSTDOCFLAGS="-D warnings" cargo doc -p shortwin-math --no-deps
python3 docs/design/sim/config_check.py             # T-CONFIG: the defaults print OK
# T-CU-MATH (tests/bench/cu_math/README.md)
cargo-build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path tests/bench/cu_math/program/Cargo.toml --sbf-out-dir tests/bench/cu_math/target/deploy
cargo test --manifest-path tests/bench/cu_math/Cargo.toml --release
```

A failing property writes `tests/<file>.proptest-regressions`; commit it with the fix
(`tests/vectors/README.md`).

## Test vectors

Generators are Python 3.14, standard library only, deterministic (fixed seeds, sorted
JSON), import the v0.4.1 sims from `docs/design/sim` with their default switches (asserted)
without writing into `docs/`, and write next to themselves. Conventions:
[`tests/vectors/README.md`](tests/vectors/README.md). CI's `vectors` job re-runs every
`tests/vectors/*/gen_*.py` and fails on any diff.

| Track | Generator | Output | Source | Time |
|---|---|---|---|---|
| num | `num/gen_num.py` | `num.json` | `shortwin_sim.py` v0.4.1 helpers (`cdiv`, `fee_ceil`, `isqrt_ceil`) | < 1 s |
| quotes | `quotes/gen_sim.py` | `sim.json` (272 PumpSwap, 224 curve, 195 Raydium, 720 repriced, 400 reprice, 200 liquidity, 150 locked-depth cases) | `shortwin_sim.py` v0.4.1 quotes, risk quotes at a cap, `Market.quote_repriced`, `reprice` | < 1 s |
| quotes | `quotes/gen_caps.py` | `caps.json` (500 impact, 300 fee, 400 predicate, 300 `conservative_x0`, 32 example-market, 628 tier cases — the last 28 at `oracle_stale_s` = 600, the 600 / 601-s staleness boundary, since the owner's decision of 2026-10-01) | v0.4.1 cap functions, example markets, keeper rewards, ARCH §4.7 tier transcription | < 1 s |
| quotes | `quotes/gen_port.py` | `port.json` (506 cases) | the R-RAY port of `raydium-cp-swap` | < 1 s |
| quotes | `quotes/gen_mainnet.py` | `mainnet.json`, `venue_state.json` | raw mainnet `getTransaction` dumps (not committed) | — |
| oracle | `oracle/gen_oracle.py` | `units_{observe,ring,persist,bounds}.json` | `shortwin_sim.py` v0.4.1 `Market` methods (integrate, rewrite, growth, medians, EMA leg, references, `max_in`) | < 1 s |
| settle | `settle/gen_settle.py` | `units_settle.json`, `units_evaluate_1.json`, `trace_{attacks_1..3,scenarios_1,random_1}.json`, `fuzz_{1..3}.json` | `shortwin_sim.py` v0.4.1 `try_exit(settle_only=True)`, `attacks/` (arming, block_spot, refute_push, fv_push, fixed_delta, ptf_sandwich, venue_switch), ECON §11.2 scenarios, `fuzz_exits(1000, 20260927)` | ≈ 31 s |
| pool | `pool/gen_pool.py` | `pool_fuzz_continuous_{90,100}_{1..3}.json` (the option (a) ECON §9.6 runs, split), `pool_lock_fuzz_1.json`, `pool_stranded_{1..5}.json`, `pool_ext_{lazy,runs,dust,extreme}_1.json` (with the scripted model-note traces and M0B-A-1's empty lender with a lock term), `pool_ext_topup_1.json` (M10-8C-1: exits paying rent above what accrued, so an `R_min`-sized top-up vests, and a scripted JIT whale around an early repay and an early settlement), `pool_ext_orphans_1.json` (KI-65: lazy lenders converting in random orders through forced deep rebases, dust residues, whole exits during a vest, permissionless syncs in random orders; plus the `orphan_vest_demo` and `whale_order_demo` W1–W3 pools), `pool_scenarios_1.json` | `pool_model.py` v0.4.2, `attacks/pool_r3.py`, `attacks/pool_edge.py` | ≈ 39 s |
| risk | `risk/gen_rent.py` | `rent.json` | `shortwin_sim.py` v0.4.1 accrual (clamp included) and `pool_model.py` v0.4.2 `receivable` (unchanged since v0.4.1) | < 1 s |
| risk | `risk/gen_exits.py` | `exits.json` (incl. `margin_ema_undefined`: 30 `Market.open` calls whose EMA-leg quote is undefined) | `shortwin_sim.py` v0.4.1 scenarios, ECON §13's `fuzz_exits(1000)`, `attacks/{fvcap,sandwich,ptf_sandwich,cascade}.py` | ≈ 56 s |
| params | `params/gen_params.py` | `params.json` (3,448 configurations) | `shortwin_sim.py` v0.4.1 `validate_params` (with the M1 doc sync's split-part and per-tier bounds); `config_check.py` v0.4.1 box corners, scan, targeted and stress rows | < 1 s |

Regenerate everything and check that nothing changed:

```sh
for g in crates/shortwin-math/tests/vectors/*/gen_*.py; do python3 "$g"; done
git diff --exit-code -- crates/shortwin-math/tests/vectors
```

`gen_mainnet.py` needs the raw dumps: `python3 gen_mainnet.py --research <dir> [--extra
<dir> ...] [--fetch]` (`--fetch` refreshes `venue_state.json` over RPC). Without
`--research` it only prints a note. Every case carries its transaction signature, so the
dumps can be re-fetched; ARCH §1.2's `tests/fixtures/txs/` manifest is where they will
live once the program tests need them.

## WASM facade

`crates/shortwin-math-wasm` wraps the functions the frontend needs (quotes per venue and
repriced, CR and the liquidation price, D4 margin and open fee, FV, `p'` and keeper
rewards, the exit bounds `max_in_liq` / `max_in_exp` / `π_ref`, the settlement box of the
stored oracle state, rent due, waterfall previews with the keeper floor and the vesting
lump, caps, tiers, lender previews with `lock_until` and the vest, and the parameter
checks). Each export is one call into this crate; see that crate's README.

## Design notes

Where the design documents are ambiguous, disagree with each other or with the
normative sims, the crate implements the sims' behaviour (ARCH §2.2: the sims are
normative) and records it here. Items marked **owner** need a decision before the
dependent milestone.

### Integration (M0, M0b)

- **Error codes (A §2.2, §3.11).** `MathError` has `Invariant` (5, a failed internal
  check, e.g. waterfall conservation) and one variant per lender-pool refusal:
  `NegativePending` (6), `InsufficientShares` (7), `InsufficientInventory` (8),
  `AboveAllowance` (9), `LenderBehind` (10), `DepositTooSmall` (11) and, since M0b,
  `WipeSweepPending` (12; R4A-7: a deposit into a live full-wipe record before its
  post-wipe sweeps), matching A §3.11's lender errors so the program maps them one to one.
  `InvalidInput` remains for malformed arguments. The pool vectors carry the codes by name.
- **Additions beyond the A §2.2 list** (additive, for the program, keeper and frontend
  alike): `quote::Pricing` (venue dispatch with the (token, SOL) order handled once),
  `quote::{liquidity, reprice_token}`, `risk::ring_below`, `risk::liquidation_price`, the
  `num` helpers `mul_le`, `saturating_u64`, `overflow_to_max`; since M0b
  `observe::coverage_gap`, `settle::{BoxQuotes, CornerAmount, amount_at, settlement,
  S3Chain, S3Terms, s3_due}`, `pool::{Vault, Resolution, SweepSource}`,
  `params::{Rule, Violation, check, check_sim, check_econ, validate_econ}`,
  `fees::exceeds_cap`, `pool_select::lambda_bps`, `tier::locked_sol`.
- **Liquidation price (E §3.3, A §7.3).** ECON gives a real-valued closed form. The crate
  bisects the exact on-chain predicate `CR(V_eff, B̂(Q; reprice(L, P))) < L` over `P`, so
  the UI and keepers show the price at which the program's own test flips; the two
  agree up to rounding. A §2.2 lists no function for it.
- **Math types are not account layouts (A §2.2).** Every `#[repr(C)]` state type keeps
  its 128-bit fields at 16-byte offsets with explicit padding, asserted at compile time,
  and is identical on SBF, x86-64 and wasm32: `RingEntry` 144 B (native u128, `ts` last)
  against the 136-B `PodU128` account entry of A §2.3.4, `Ring` 1,168 B, `Observation`
  1,376 B; `Pool` 160 B, `Vault` 2,128 B (lock calendar first), `Lender` 144 B,
  `ShareEpoch` 96 B. The program converts field by field (T-LAYOUT is M1's).
- **Constants (A §2.1 `constants.rs`, §2.3.3 scope C)** live in `consts`, including the
  lock calendar's `LOCK_BUCKET_S` / `LOCK_BUCKETS` (re-exported by `pool`) and
  `P_ACC_MAX` (re-exported by `rent`); their compile-time asserts read the compiled bounds
  from `params::Rule::bounds`, the one table of them.
- **Parameters: one source of truth (A §2.3.3, §3.1, §9 M0b; E §15; review M0B-S-1,
  closed by the M1 doc sync).** `shortwin_sim.validate_params` checks every ECON §15 bound
  and relational check of the parameters the sim models — since the doc sync also the
  penalty split's protocol and keeper parts (≤ 5,000; sim `COMPILED_BOUNDS`) and every
  per-tier bound (`cr_open_bps ≤ 100,000`, penalty, duration, impacts, slip, rates,
  `min_rent_s`; sim `TIER_BOUNDS`) — and `validate` is that function exactly: `check_sim`
  reports the sim's violations (`Rule::in_sim`) and `validate` accepts exactly the
  configurations the sim accepts (all 3,448 configurations of `params.json`, every sim rule
  exercised, the per-tier bounds for every tier). `check_econ` / `validate_econ` keep only
  the ECON §15 bounds of the parameters the sim does not model (`min_notional`,
  `market_max_notional`, `market_creation_fee`, boost, `market_warmup_s ≥ 10·EMA_TAU_S`,
  `ema_force_delay_s`, `lump_vest_s`, `lock_grace_s`, θ and `θ_B < θ_A`, oracle, Raydium
  selection and `pool_fast_min_bps`; transcribed from the E §15 table and unit-tested at
  `lo − 1`, `lo`, `hi`, `hi + 1`). The vectors move only parameters the sim models, and
  `validate_econ` accepts every one of them, so the program's gate (`initialize_config` /
  `propose_config` run `validate`, then `validate_econ`; A §3.1) accepts a proposal
  exactly when `shortwin_sim.validate_params` (the first step of `config_check.py`) does
  and every other parameter is inside its bounds.
  History: M0b's crate first made the split-part and per-tier rules in `validate` and
  refused 69 configurations the v0.4.1 sim accepted (31 keeper parts, 32 protocol parts, 6
  `cr_open_bps > 100,000`); the review moved them to `validate_econ`; the doc sync moved
  them into the sim, so both now refuse them (`validate_refuses_what_the_sim_refuses`).
  Rule codes are unchanged: `Rule::in_sim` is an explicit list, not a code range. `check`
  reports both sets, the sim's first; the WASM facade's `validate` runs both and
  `validateSim` is the sim's gate. τ is a constant, not a parameter; the sim's per-tier
  `ema_dtmax_s` / `ema_clamp_bps` are the market-level `EmaParams` here (one value,
  reported without a tier); `util_kink_bps` is validated with the rest although the
  program stores it in `GlobalConfig.kink`. `pool_fast_route_s + pool_fast_window_s ≤ 540`
  is implied by the v0.4.1 bounds (420 + 120) and kept as the relational check.
- **T-CU-MATH (A §6.1).** `tests/bench/cu_math` measures every §6.1 primitive with
  Mollusk; all are within the §6.1 value + 20 % (`reprice` is the tightest: 985 against
  843, the cost of `isqrt⁺`). Two M0b changes came out of it, both bit-identical:
  `mul_div_u64` divides natively when `a·b` fits u64 (A §9 M0b's optional item; 278 → 96
  CU, the PumpSwap quote 1,458 → 944), and `rent`'s dot product stays in u128 when only the
  lender-share factor overflows (`credited_for` 3,239 → 2,991). The bench is a standalone
  workspace: Mollusk 0.15.1 does not compile against the Agave 4.3.0 crates resolver 3
  picks for its `^4.2.0`, so its lockfile holds Agave at 4.2.2 (C1-14 resolved here: 4.3.0
  is picked and must be avoided; A §1.1 and U18 record it since the M1 doc sync).
- **`gen_mainnet.py`** required `--research`, so CI's `vectors` job (which runs every
  generator without arguments) would fail; without it the extractor now prints a note
  and exits 0.
- `docs/design/sim/__pycache__` and `docs/design/sim/attacks/__pycache__` exist and
  predate M0's generators (the generators set `sys.dont_write_bytecode`); `.gitignore`
  excludes them.

### Quotes, fees, pool selection

- **Owner — curve buy rounding (E §2, §3.1, §3.2 vs A §4.3; review F-S2; still open in
  v0.4.1).** ECON's rounding table has quotes round up, E §3.1 defines the curve quote as
  `n = ⌈q·Y/(X − q)⌉ + ε₁` (the binding API's `eps1`) and says the curve *charges*
  `⌊·⌋ + 1`, and E §3.2 states both: charge 485,764,634, "short.win's conservative quote
  rounds up to 485,764,635". A §4.3's table gives the charge as the quote and claims the
  on-chain quote equals the venue's charge. The crate follows ECON and the sim:
  `sol_leg::buy_exact_out(eps1 = true)` is `⌈⌉ + 1` (every `B̂`: CR, S1/S2, bounds, FV,
  close affordability), `migrated_pool` uses that `n_curve(rT)` (as the sim's straddle),
  and `sol_leg::curve_charge_exact_out` is the program's `⌊⌋ + 1` with the venue's exact
  post-trade state (mainnet replays; the quote exceeds it by ≤ 1 + #fees lamports, so a
  `max_in ≥ B̂` always covers the charge). The owner should amend A §4.3's curve row and
  "the on-chain quote equals what the venue charges" (or ECON).
- **Fees in ppm (A §2.2, §4.2; E §2.1; R3A-3 iii).** A §2.2 listed `fees::{pump_curve,
  pump_amm, ray_v4, cpmm}` as returning a fee in ppm (fixed in the M1 doc sync), but the
  quotes must charge components
  separately (pump: three ceilings; CPMM: trade/creator, creator side, protocol/fund
  shares): the resolvers return the component structs and every fee type has
  `oneway_ppm()`, ECON §2.1's φ (pump bps × 100, AMM v4 `⌈num·10⁶/den⌉`, CPMM `t + c`). φ
  is u32 ppm; pump `FeeBps` fields are u64 (read from accounts), so `oneway_ppm` saturates
  at `u32::MAX` while `capped` compares unsaturated. Every gate goes through
  `fees::exceeds_cap(φ_ppm, cap_bps)` (`φ > cap·100`); `oneway_bps` is display only. The
  caps and `PoolFacts` take φ in ppm (`fee_ppm: u32`).
- **Capped fees (A §4.3, §9 M0b; V2-1).** Above the cap, pump venues now charge one
  combined fee at the cap, `net + ⌈net·cap/10⁴⌉` (`FeeBps::new(0, cap, 0)`; the sims'
  `Venue._risk_fees`), not v0.3's lp/protocol/creator split with three ceilings; at or
  below the cap the three components are unchanged. Raydium grosses up at `min(rate, cap)`
  (AMM v4 `(cap, 10⁴)`; CPMM `t + c = cap·100`). The curve and PumpSwap schedules take
  `cap_bps` because `curve_straddle` resolves the migrated pool's tier internally. Risk
  quotes never exceed live ones (proptested). For CPMM pools whose creator fee is on the
  output side, the risk quote keeps the venue structure at the capped rates (A §4.3's venue
  formula); the sim models the creator fee on the input only.
- **Hypothetical states at the live fee tier (R3B-7 b; E §3.1, A §4.2).** Repriced
  (EMA-leg, box-corner, reference) quotes use the live tier, which closes M0's open
  question: resolving the tier per repriced state would make `B̂` dip (≈ 0.05 %) at each
  threshold as `P` rises. The one exception, in the sim and A §4.3 alike: the straddle's
  post-migration pool resolves its PumpSwap tier at that pool's own market cap.
- **`reprice` (R3B-7 c).** `sp = isqrt⁺(P)`, `X = ⌊L·10⁹/sp⌋`, `Y = ⌈L·sp/10⁹⌉`, so
  `Y/X ≥ P`; `None` when `X = 0` (as the sim), while `reprice_token` returns `Ok(0)` there
  (what the conservative cap reserve needs). All of `reprice`, `curve_reprice`,
  `curve_straddle` and `curve_two_leg` return `Option` (A §2.2 lists `reprice` with it
  since v0.4.1; "all quotes return None when undefined"). `curve_straddle` also covers
  `q ≤ rT` (plain curve quote). `q = 0` and zero reserves are `None`.
- **`liquidity` returns `Result`** (u128 `isqrt`, `Overflow` if `X·Y ≥ 2¹²⁸`, outside ECON
  §2.2's bounds); `num::sqrt_u256` left the on-chain paths.
- **Curve LP fee (A §4.2).** pump-fees tiers carry `lp_fee_bps`, but the curve charges
  only protocol and creator fees (R-PUMP §4): `fees::pump_curve` sets `lp = 0`.
- **Curve dispatch (A §2.2, §4.3; review F-S3).** The curve's buyback needs `rT`
  (`B̂_str` above it), so it cannot be a `(X, Y)` quote: `Pricing` covers PumpSwap
  (ε₁ = 0), AMM v4 and CPMM only, and `CurvePricing` (fees, offsets, migration, pool
  schedule) prices the curve on a `CurveState` or `curve_reprice(L, P)`.
- **Two-leg `B̂₂` (v1.1 only)** is no longer in the v0.4.1 sim: `gen_sim.py` builds it from
  ECON §3.1's `B̂_curve(rT) + B̂_pool(q − rT)` with the sim's own primitives; its `exact_*`
  fields remain the sim's execution path.
- **Straddle market cap (A §4.3)** does not say which supply: `PammFeeSchedule.base_supply`
  is the live mint supply (10¹⁵ before graduation, as the sim).
- **EMA leg on the curve (A §4.3).** `rT_ema`, `rQ_ema` can go negative; both saturate at
  0 (the sim clamps `rT` only). `rT_ema = 0` prices all of `q` on the migrated pool.
- **Owner — straddle dip at `q = rT` (A §4.3, E §3.1 V1-9; review F-A2).** Above `rT`
  all of `q` is priced on the post-migration pool, whose `Y_p = rQ + n(rT) − fee` starts
  ≈ 0.023 % below the curve's completion price, so `B̂_str(rT + 1)` can be below
  `B̂_curve(rT)` (by ≤ 3 bps, proptested) and, on the EMA leg, `B̂` can fall as `P` rises
  past the price where `rT(P) = Q`. `risk::liquidation_price` is the first crossing only
  for quotes monotone in `P`, and `CurvePricing::liquidation_price` searches the curve and
  pool regimes separately (`integration_props.rs` pins the v0.4.1 numbers: `rT(P)` crosses
  `Q` at 410,837,995,801,322). The owner may prefer a monotone envelope
  `max(B̂_pool(q), B̂_curve(rT))` for `q > rT`; A §4.3 records the dip since the M1 doc
  sync. Since pump's v3 (pump-final §5.2, owner D3 of 2026-10-13) a live curve's `q > rT`
  executes by synthetic migration and is priced `B̂_v3` (`curve_synthetic`): no dip with one
  fee schedule, at most ≈ 2.4 bps above `B̂_str` just past `rT` and below it further out
  (`quotes_props.rs`). `B̂_str` stays the quote of a `PumpMigrating` market's states.
- **Synthetic migration (pump-final §5; P1).** `B̂_v3` charges both legs at the curve
  `FeeConfig` tier of the pre-trade market cap on the mint's **live** supply
  (`fees::pump_curve_at_supply`), which is what pump charges there (G0's tier tripwire);
  `fees::pump_curve` keeps the fixed 10¹⁵ for every other curve quote (ARCH §4.2's open item,
  unchanged). Both legs carry `ε₁ = 1`, so `B̂_v3` covers pump's charge (curve part `⌈⌉`, `⌊⌋ +
  1` on an exact division; pool part `⌈⌉`) by at most §5.2's bound, ≤ 7 lamports for `q − rT ≤
  X_s/2` (proptested on the full domain; 28ZvRwwN… +2 on mainnet, G0's buys +1 to +4).
  `migrated_pool` adds the curve's `post_complete_quote_in`; after a pool part the caller passes
  the live `curve_base_ata` as `pool_tokens`, which reproduces the pool `migrate_v2` created on
  all 11 sampled mainnet completions. pump-final §3.4 (d)'s boost `⌊(Y − pcQ)·2069/10⁴⌋` is
  `⌊X₁·Y₁/10¹⁵⌋` in fact (`migration_boost`; equal without a donation to `curve_base_ata`).
- **Locked depth (A §2.6; E §2.1).** `λ_bps = ⌊(counter − supply)·10⁴/counter⌋` first, then
  `D = ⌊λ_bps·isqrt(X·Y)/10⁴⌋` (v0.3's single floor on the raw ratio is larger); 0 if
  `counter = 0` or `supply ≥ counter`. `dominates` / `fast_path_qualifies` compare exactly.
- **Caps (E §6.1).** `max_total_q` is the sim's integer form `X − isqrt(⌊X²·10⁴/(10⁴ +
  ι)⌋)` (A §9 M0b settles M0's owner note in favour of the sim); it can exceed the real
  inequality `ΣQ ≤ X₁(1 − 1/√(1 + ι))` by one raw unit (proptested), which E §6.1 and A §4.6
  state next to the integer form since the M1 doc sync. `max_q_position` is the sim's
  `isqrt(⌊X²·(10⁴ + ι)/10⁴⌋) − X` with u128 square roots only: `X²·(10⁴ + ι)` exceeds u128
  above `X ≈ 1.8·10¹⁷` (now in E §2.2's overflow table), and for `X₀ > 2⁶⁴/√(1 + ι)` the
  radicand itself exceeds u128, handled by a root of `q/4^k` plus a k-bit correction
  (checked against the 256-bit root). `conservative_x0` uses `isqrt⁺(P_ema)` in both
  denominators and the Raydium bound `⌊D·10⁹·10⁴/(λ_min_bps·isqrt⁺(P_ema))⌋` (E §2.1, and
  E §6.1's prose since the M1 doc sync).
- **CPMM edge cases absent from R-RAY §3.3** (from `raydium-cp-swap@59fb845`):
  `creator_fee_on > 2` fails every swap (`InvalidInput`); creator fee on the input with
  `t + c = 0` divides by zero (`None`); config invariants `t + c < 10⁶`, `p + f ≤ 10⁶`;
  the venue's own post-swap u128 k-check overflow near 2⁶⁴ reserves is not modelled.
- **Mainnet findings.** 39 of 44 mayhem-curve trades do not price on their reported
  reserves (E [T12], R-PUMP §8); mayhem is out of scope (ADOPTED Q6) and excluded.
  Cashback coins report the creator slot as cashback with the same cost (R-VEN §1.5).
  No curve payout cap is documented; the curve passes `real_quote_cap = rQ`.
- **Mainnet coverage of the fee resolution (A §4.2, §8.1; review F-S5).** "Every amount
  to the raw unit" holds for the branches the trades exercise: curve schedule fees;
  PumpSwap canonical tiers, a non-canonical pool's `flat_fees` and a default coin creator;
  CPMM exact-in with the creator fee off, on the input and on the output, and exact-out
  without it. **UNVERIFIED**, each covered by the reference-model proptests
  (`fee_resolution`, `cpmm_equals_reference`) and for CPMM by `port.json`, with the gate
  that must replay a mainnet trade:
  - curve: creator is the default key; per-curve `creator_fee_bps` override — M5
    (proposed gate T-PUMP-FEES);
  - PumpSwap: per-pool `creator_fee_bps` override — M6 (T-PUMP-FEES);
  - CPMM exact-out with the creator fee on the input, and on the output (A §12 U11) —
    T-RAY-MATH (M3).

  `fee_branch_coverage` fails when a listed branch gains a trade, so this list stays
  accurate.
- **Proceeds are not strictly monotone on pump venues:** each non-zero fee component
  rounds up separately, so `S` can dip by up to `k − 1` lamports as `q` grows. Anything
  inverting `S(q)` must allow for it.

### Oracle: observation, persistence, bounds, settlement

- **Signatures (A §2.2).** `record` takes `now`; `integrate` takes `&mut Bucket` and `&mut
  LastBucket` (it must move the bucket, A §4.4), folds in the prologue guard and returns the
  pre-roll gap `now − ema_ts` when a bucket rolls (`CoverageGap` is `coverage_gap(gap,
  open_positions)`: `open_positions > 0 ∧ gap > COVERAGE_GAP_S`; the gap can be ≤ 0 in the
  sim's `dt ≤ 0` case). `integrate` takes **no venue-kind argument**: the sim caches
  `reprice(l, p)` for every venue (`Market._entry`) and `curve_reprice`'s `(vT, vQ)` are the
  same numbers with the curve offsets applied at quote time (A §3.0 step 5 and §9 M0b say so
  since the M1 doc sync). `liq_ema_leg` takes a `below(x, y)` closure over the cached
  reprojection (use the position's risk quote and `risk::cr_below`); `kappa_eff(g, params)`
  reads `EMA_TAU_S`; `liq_reference` / `expiry_reference` take the state and return `None`
  when the completed bucket is stale (`ReferenceStale`, A §3.6); `max_in_liq` / `max_in_exp`
  return `None` where the minimum is negative. Liquidation has no reserve, expiry reserves
  rent and the expiry fee (A §3.6, §3.7, the sim; A §2.1's `engine/bounds.rs` row, A §8.1
  and the A §9 M2 row say so since the M1 doc sync).
- **Rounding.** ECON §2.1 now specifies the floors of the EMA, the ±C clamp, `P_ref`, δ
  and `max_in` (M0's note closed); every value is exact against the sim.
- **EMA arithmetic.** ECON §2.2 says the EMA step fits u128; `weighted` takes the u128 path
  and falls back to U256 only beyond ECON's bounds, so an extreme state stays exact rather
  than returning `Overflow`, which would block the market's prologue (a denial of service).
- **Arming protocol (closed; A §3.8 = sim).** ARCH §3.8 now equals
  `try_exit(settle_only=True)` step for step and `settle::evaluate` implements it, with the
  S1 live leg at the risk fee and no "normal buyback allowed" step in `sol_settle` (M0's
  two owner notes on the arming order and the two fee views are closed). Behaviour the ARCH
  prose leaves implicit, reproduced from the sim: the refutation reads `s_armed`, not the
  live arm, so a lapsed arm can be refuted; a disarm clears only `armed` (count, since and
  last keep their values); an S2 whose amount waits falls through to the S3 check in the
  same call; S1 is chosen whenever its legs hold (S2's legs are evaluated only if S1's
  fail); legs are evaluated only with a fresh bucket, otherwise a live clock is kept; a
  non-liquidatable, unarmed evaluation writes nothing. `ArmChange` reports `None` / `Kept`
  / `Armed` (`SettleArmed`) / `Disarmed(reason)` (`SettleDisarmed`); `Settlement` carries
  `K_s`, lenders, borrower, FV, `p'` and both corner amounts for `SolSettle`. The vesting
  lump is `waterfall::settle`'s.
- **Settlement inputs.** Quotes on repriced states come through the `BoxQuotes` trait
  (`cost`, `sigma`, `fv_cost` keyed by `(P, L)` at `φ_risk = min(φ_live, cap)` and the
  fee-free σ); `fv_cost` differs from `cost` only for the stuck-migration S3, which prices
  the post-migration pool at `P` and ignores `L` (sim `fv_parts`). The program passes
  `SettleInputs.bx = box_of(&ema, &ring, now)` on the post-roll state, `b_live` = the risk
  quote on the live reserves (sim `qs_r`, not the execution quote), `sigma_live`, the EMA
  leg, CR-liquidatable (spot ∧ EMA legs), `executable`, `p_rs` (`None` = stale bucket) and
  the S3 chain (`s3_no_route` with the **global** hard cap). This keeps `settle`
  independent of the quote and risk modules; the settle vectors carry the sim's quotes only
  at the points its rule reads (corners `A = (P_lo, L_hi)`, `B = (P_hi, L_lo)`,
  `C = (P_hi, min(L_live, L_lo))`, `D = (p_rs, L_live)`, `E = (P_ema, L_ema)`), and the test
  quoter panics anywhere else, so the corner choice is checked too.
- **Amounts.** The box amount check is exact, `(x_hi − x_lo)·10⁴ ≤ fv_dev_bps·x_lo`, and a
  negative difference always agrees. `x = min(V − K_s, FV)` with FV from
  `risk::fair_value` (saturating); an undefined `B̂_FV` means no cap (`x = V − K_s`, `p'`
  `None`). `pprime(b_fv = 0)` reads "not below" (unreachable: `Q ≥ 1`). σ's `INF` in the
  vectors is encoded as `u64::MAX`; every comparison is against `σ_max < 2³²`.
- **Venue changes (closed; E §5.2, A §4.4 A72 = sim).** `rewrite_liquidity(ema, ring,
  bucket, l_new, VenueChange)` replaces `reset_liquidity` / `switch_liquidity`: graduation
  sets `L_ema = L_slow = l_new`, a Raydium adoption or fast path `L_ema = min(l_new,
  L_ema)` and `L_slow = min(l_new, L_slow)` (`VenueChange::Raydium`, the sims' `fast_switch`,
  kept for the differential vectors); then every entry gets `l = llo = lhi = L_ema` with
  `(x, y)` recomputed, and the bucket is cleared (the sims' `_rewrite_ring_liq`). M0's owner
  notes on graduation and the Raydium pool switch are closed. **Since the M4 security review
  the program uses `VenueChange::RaydiumLocked { slow_cap }`**: `L_ema = l_new` and `L_slow =
  min(L_slow, slow_cap)` with `slow_cap` the new pool's locked depth (never raised) — a
  deviation from ARCH v0.4.1 rule 4 and from the sims, which test only thinner pools. **Since
  M4R (post-change confirmation, PROPOSED, O18)** `l_new` is the anchor `⌊D·10⁴/λ_min⌋` (no
  sample of the new pool) and the adoption calls `mark_restart` (`Ema.warm = 8`, `Ema.guard =
  1`); the next 8 integrations re-seed `P_ema = max(min(min lo, price_up_bound(P_ema, dt, C)),
  price_down_bound(P_ema, dt, C))` (the largest v0.4.1 step bounds the rise, M4X-1, and — since
  the owner's decision of 2026-09-30, ECON #105 — the fall, M6-SEC-1: ×11/12 at `dt = 1`),
  `L_ema = min(max lhi, ⌊lomed(lhi)·(10⁴ + C)/10⁴⌋)`
  over the post-change entries (`integrate`'s warm-up branch), and the guard clears once `L_ema ≤
  1.01·L_mlo`; while it is set `L_slow` integrates `min(lobs, L_mlo)` (M4X-2). A never-marked
  state integrates exactly as before.
- **Encodings.** The creation entry is `(P, P, L, L)` for `(lo, hi, llo, lhi)`; `(x, y) =
  (0, 0)` where `reprice` is undefined (the sim's `reprice(l, p) or (0, 0)`; a quote on
  `x = 0` is undefined, i.e. below). `record` rejects `P = 0` or `L = 0` (the sim's
  `has_price` admits a floored-to-zero spot, which would pin the EMA at 0) and an open
  bucket of another second (the sim overwrites; this assumes the clock never goes back).
- **`sustained_growth`** saturates at `u64::MAX` bps/s (the sim is unbounded; unreachable
  for integrated rings).
- **Timestamps and scopes.** Ring timestamps are roll times (`now`), per the sim and
  A §4.4; spans and growth Δt are measured between rolls. τ is `EMA_TAU_S`; Δt_max, C and
  the slow EMA are market-level (`EmaParams`, A §2.3.3/§2.3.4) where the sim keeps them per
  tier with the same defaults and `TIER_BOUNDS`. The slow-EMA `dts` is floored at 0.
- **Closed by v0.4.1:** "parameter bounds allow δ_min > δ_max" and the relational-bound
  open item (the δ is fixed; `params` checks `κ_base ≤ κ_liq` and `δ_liq ≥ δ_exp`);
  `ref_lo_k` (gone).
- **"In a genuine uptrend the S1 corner is `P_ema`" (E §5.6, A §4.5, A §8.1)** holds for a
  sustained ramp (proptested for 0.05–5 %/s after two ring lengths, and at 25 %/s where the
  ±100 % clamp caps the EMA's growth at 1/6 per second), not for a ramp's first ≈ 8–11
  seconds (the EMA starts at the ramp's opening price), a step filling 4 of the 8 entries,
  or a steady ramp between ≈ 9.5 and ≈ 16.7 %/s (the EMA's steady lag `(1 + g)/(1 + 6g)`
  then exceeds the median's `(1 + g)^−4`): there the S1 corner is `P_mlo`, which delays S1
  but never makes it easier (`uptrend_claim_needs_a_sustained_ramp`). A doc precision, not
  a crate change; E §5.6, A §4.5 and A §8.1 say "sustained" since the M1 doc sync.
- **Two fee views (review F-S7), re-homed onto `evaluate`.** A live fee above the hard cap
  makes the live-fee buyback unaffordable while no S1/S2 leg holds at the risk fee; it is S3
  evidence (`s3_no_route` on the global cap), the risk-fee refutation still disarms a live
  arm while the timer runs, and S3 settles once due
  (`fee_above_the_hard_cap_is_s3_evidence_and_refutes`).
- **Coverage limits worth knowing.** The 1,000-path exit fuzz has no disarm at all (both
  reasons are covered by the synthetic units and the genuine-reversal scenario traces); the
  attack traces contain no expiry `max_in` (the scenarios and the fuzz do); the ECON §11.2
  stuck migration runs with `max_migration_pause_s` at its compiled minimum of 600 s and a
  keeper every 10 s, to keep the trace at 140 KB. The fuzz vectors are decision-only
  (self-contained lines; full replays of 1,000 paths would be ≈ 200 MB), and the first 6
  fuzz runs are replayed whole.

### Lender pool

- **State split (A §2.3.4, §2.3.6).** `Pool` is the Market part (T, ΣB_Q, S, the
  accumulators, `receivable`, `max_release`, the vest, `share_epoch`, `wipe_live`, the two
  wipe bits), `Vault` the MarketVault part (`lender_sol`, the pot `unmatched`,
  `protocol_fees`, Π, `pi_epoch`, `lock_norm`, `b_min`, `cal_day`, the 128 day buckets).
  `touch`, `claim`, `cover` and `allowance_view` take `&Pool`, so `claim_sol`,
  `cover_deficit` and `sync_lender` provably write only the lender and the vault (C0-17,
  A70). A §9 M0b's wording `Pool.{unmatched, lock_cal}` is satisfied by `Vault`.
- **Signatures that differed from A §2.2** (A §2.2 lists them since the M1 doc sync).
  `borrow(pool, now, q, release_at)` has no `p`
  (the rent aggregate is `rent::Aggr` state; `p` enters through `repay(…,
  rent_protocol_bps)`); `repay` and `settle` take a `Resolution { q, credited, lump }` with
  `credited` from `rent::credited_for`; every time-dependent function takes `now: i64`;
  `withdraw` takes `lock_grace_s` and `vest` / `sweep` / `repay` / `settle` take
  `lump_vest_s` (GlobalConfig parameters); `deposit` takes `rewards_required`; `sweep`
  takes a `SweepSource`. The program's `open_short` passes `release_at = deadline +
  max_migration_pause_s + grad_grace_s + s3_wait_s` (snapshot values).
- **Wipe gate (A §2.3.4, §3.3; R4A-7).** The sim has one `wipe_swept` flag set by any
  `sweep_sol` while `S == 0 ∧ wipe_live`; ARCH needs two bits (the trader sweep always, the
  venue-reward claim only on a pump venue that can execute). The crate keeps both, sets
  them only while `wipe_live` (ARCH says "while S = 0"), and clears both at every full
  wipe; `Pool::new` has both set (no wipe record yet). The vectors exercise the trader bit
  (`rewards_required = false`); unit and econ tests cover the rewards bit.
- **Pure views.** `allowance_view` and `locked_total` read the calendar as `cal_advance(now)`
  would leave it (the sim's `free_allowance` / `locked_total` advance its dict in place);
  `touch` still advances the calendar persistently, as `_touch` does.
- **Ring guard.** The program keeps a 128-slot calendar where the sim has an unbounded
  dict: `cal_add` refuses a day outside `[cal_day, cal_day + 128)` and `borrow` refuses a
  `release_at` beyond `now + MAX_RELEASE_SPAN_S` (90 d + 3 d); neither is reachable within
  the compiled bounds (the generator asserts every bucket lies inside the ring).
  `cal_advance` never decreases `cal_day` (A §2.3.6 set `cal_day = ⌊now/86,400⌋ + 1`
  unconditionally, which would move it backwards on a backwards clock; it says
  `max(cal_day, ·)` since the M1 doc sync).
- **`release`** returns `Invariant` if a vest runs while `S = 0` (unreachable: the vest is
  flushed whenever S reaches 0) and refuses `now < vest_last` while a vest runs.
- **Refusal pre-steps.** `pool_model` mutates state before some refusals (deposit and
  withdraw release and touch first; an open refused by the utilisation cap releases the
  vest); the vectors record these as committed steps (the pre-step of `d`/`w`, and an `R`
  op). On-chain a refused instruction reverts entirely, so this has no economic effect.
- **Owner — last shares while dust is borrowed (A §9 M0b note (4); replaces M0's "model
  defect 1").** With `S = 0 ∧ ΣB_Q > 0` the continuous rent credit moves `lender_sol`
  backing the departed lender's realised claim into `protocol_fees` (the model's own
  `check()` fails on those lines, I-P2 claim side, and the crate's checker agrees line for
  line). The later close of that position then debits the protocol by the ceiled
  `credited`, which exceeds the floor-credited amount the sink received, so `pool::repay`
  is refused with `Underflow` while `S = 0`: the close, liquidation or expiry cannot land
  until a deposit brings S above 0; an accrual larger than `lender_sol` is refused too.
  This supports ARCH's proposed rule that the last shares cannot be withdrawn while
  `ΣB_Q > 0`. Pinned in `ext_scripted_no_shares`; the proptests skip that interval.
- **Owner — lock-term overflow (A §9 M0b note (1), A §2.3.6, E §9.3).** Π never resets
  while `ΣB_Q > 0`; after a deep reduction a new lock's term `⌊lock·WAD/Π⌋` can exceed
  u128 and the withdrawal is refused (`Overflow`) until `ΣB_Q` reaches 0. Pinned in
  `ext_scripted_lock_terms`; proposed fix: roll the Π epoch when `lock_norm` reaches 0.
- **Closed by v0.4.1:** model defect 2 (ceiled `locked` above Σ locks: terms and the
  aggregate now floor, `ext_scripted_overlock` passes the model's check and the proptests
  use its I-P4 bound unchanged); `unassigned_sol` and its uses (removed: credits with
  `S = 0` and no live wipe go to the protocol sink, `Vault.protocol_fees`); `lock_term`
  after a rebase (now in the sim, M0's generator wrapper is gone); capturable lump credits
  (lumps, the expiry fee's lender share and the S2/S3 premium above `Q·P_ema + R_due`
  vest with a value-weighted end: `pool_econ.rs` `whale_*`); `cover_deficit` (A §3.3 now
  says `real += lamports` with the market read-only); lock rounding (E §2.1: a lender's
  own lock ceil, term and aggregate floor).
- **Option (b), the "release" mode, is not built:** O16 chose (a). ECON §9.6's third fuzz
  run (release, 90 %) therefore has no vectors; the vectors hold both option (a) runs
  whole (60,000 operations each, 62 liveness wind-downs), `lock_fuzz`, the 40-seed
  stranding table of `attacks/pool_r3.py`, the extended profiles and the round-3/round-4
  demo traces (`pool_vectors.rs` asserts the ECON §9.4, §9.6 totals).
- **U256 policy.** Every product goes through `num::mul_div` (u128 first, Knuth D
  fallback), reached only for ECON §2.2's share, accumulator and lock products; vest,
  calendar and time arithmetic are checked u128/i64.
- **Jump without records (V2-12; review M0B-A-1).** The model reads the lender's epoch
  record before jumping an empty lender and scales `lock_term` by it; the crate jumps
  without the record only when there is nothing to scale (`shares = lock = lock_term = 0`:
  identical state, no record account needed). A lender can reach `shares = lock = 0` with
  a non-zero term, since a rebase floors all three but `⌊lock·WAD/Π⌋ > lock` once Π < WAD;
  before the review the crate jumped such a lender without the record and kept a stale
  term (e.g. 1 where the model has 0; no economic effect, since the term is read only
  while `lock > 0`, but `LenderPosition.lock_term` differed from the model). Such a lender
  now needs its record (`LenderBehind` without it), as for any lender behind. Pinned by
  `pool::tests::empty_lender_with_a_term_scales_it` and the model trace
  `ext_scripted_empty_lock_term` (`pool_ext_extreme_1.json`, the reviewers' minimal
  18-operation reproducer); the reviewers' 210 k-line adversarial fuzz also replays
  exactly with the fix.
- **Value-weighted vest end (R4B-5; review M0B-A-2).** A lump `x` moves a running vest's
  end by `⌈x·(lump_vest_s − left)/(vest_bal + x)⌉` s (`left = vest_end − now`): never
  earlier while `lump_vest_s ≥ left`, and at most 1 s later for a dust lump. After a
  timelocked governance decrease of `lump_vest_s` (compiled bounds 3,600–604,800 s) one
  small lump pulls the end earlier, by days at the extremes
  (`vest_end_moves_earlier_after_a_governance_decrease`: 604,800 → 347,400 s). The crate
  and `pool_model` agree (the sim is normative); pulling the end earlier costs about the
  vest balance itself, so it is no practical grief. The model's own comment
  (`pool_model.py` `_vest`), E §9.4 and A §3.3 state the precondition since the M1 doc
  sync (before, they said "never earlier" without it, as the crate's comments did before
  the review).
- **API mapping** to the model: `touch` = `_touch(who)` (with `_to_pot` and the lender's
  own expired lock), `convert` = the partial step of `sync_lender(max_epochs)`,
  `allowance` = `free_allowance` after a touch, `allowance_view` = `free_allowance` on
  untouched state (T-POT's I-P4); `withdraw` does not pay SOL (the program calls
  `claim`); `repay` / `settle` / `accrue` take the rent amounts from `rent` / `waterfall`;
  `borrow` leaves the utilisation cap to `caps::util_ok`. `lock_epoch = −1` is `u32::MAX`
  (`NO_LOCK_EPOCH`).

### Rent, risk, caps, tiers, waterfalls

- **Rent is exact (closed).** The sims use the program's three basis indices, so M0's
  tolerance note is gone. `pool_model.py` keeps one index (`A = Σ q·(10⁴ − p)`); `gen_rent.py`
  sets up a case where the two coincide (base-only curves, accruals whose `c·ΔJ0` is a
  multiple of `10⁴·YEAR`), asserts it, and the Rust test replays `pool_model`'s `receivable`
  exactly. The slope terms of the lender credit are checked against ECON's formula, since
  `shortwin_sim.py` does not credit lenders. `r_min` rounding is specified by ECON §2.1 and
  matches the sim (closed).
- **`rate_bps`** (display only) is the sims' form, each slope term floored; rent itself
  integrates the exact rate through the basis indices and `min_rent` uses the exact rate.
- **`accrue`** takes `P_ema` and clamps at `P_ACC_MAX` inside (A §9 M0b; A §2.2 says so
  since the M1 doc sync). Passing a clamped value gives the same result.
- **D4 with an undefined EMA quote (E §4, A §3.4; review M0B-S-3; closes M0b's owner
  note).** `margin_d4` takes `B̂_ema` quoted on the newest ring entry's cached `(x, y)`
  (R3A-12); when that quote is undefined it prices the margin on `B̂_spot` alone, as the
  normative `shortwin_sim.Market.open` (`B_margin = max(qs[1], qe[1] if qe else qs[1])`). A
  §3.4 step 10 read with A §4.3's `+∞` rule would have refused the open instead; since the
  M1 doc sync A §3.4 step 10, A §4.3 and E §4 state the sim's reading. No scenario, fuzz or
  attack open reaches the branch (`gen_exits.py` asserts it); `exits.json`'s
  `margin_ema_undefined` reaches it through `Market.open` itself (a crash the EMA still
  lags, then an open at least as large as the cached token reserve, which the program's
  EMA-deviation guard and caps — not modelled by the sim's `open` — would refuse), 30 cases
  replayed exactly.
- **FV (A §4.5; O15; R4A-3).** `p'` is decided per evaluated state (`settle::pprime`: the
  expiry fee if expired and not CR-liquidatable at that corner, else the penalty); an
  undefined `B̂_FV` means no cap (the sim), FV saturates at `u64::MAX`.
  `risk::fv_rate_bps(penalty_applies, …)` is the per-state selector.
- **O15's scope (closed; SPEC §0, §5.3, §14; A §4.5, §10; E §2.1, §8.6; review M0B-S-2).**
  M0b found SPEC and A §10 recording the expiry-fee `p'` for **S3** only, while the
  normative sim (`Market.fv_parts`, used for the S2 and S3 amounts alike) and E §2.1 / §8.6
  take it for **any S2 or S3** of an expired position that is not CR-liquidatable at the
  evaluated state. The owner has since amended SPEC §0, §5.3 and §14 to S2 and S3 ("the
  same reasoning applies to S2 as to S3"), and the M1 doc sync aligned A §4.5, §10 and
  E §8.6, §14, §17: the crate's reading (`settle::pprime` at every corner and at the EMA
  point) is the recorded one. The difference was reachable with real money: ECON §13's
  exit fuzz run 52 (`settle/fuzz_1.json`, `fuzz/52`, op 119) is an S2 of an expired Tier A
  position paid at `p' = 200` (FV 69,021,636,745); the penalty reading (500) would have paid
  the lenders 70,977,058,967, i.e. 1.96 SOL more. It is the only S2 at the expiry fee in
  the 1,000 paths (`exit_fuzz_decisions_match_sim` pins it,
  `o15_reading_applies_to_an_expired_s2` pins both readings).
- **`ExitInputs.b_live`** is the live-state quote at the risk fee (A §5.2's `base =
  min(B̂_live,risk, B̂_ref)`, the sim's `min(qs_r, b_ref)`); the execution quote decides
  `max_in` and whether the buyback runs.
- **`V_eff`** is floored at 0 (the sim reports negative CRs); every predicate agrees.
- **σ (E §5.6 vs A §4.3; review F-S4)** is the sim's `slip_at` exactly, with `P` of the
  quoted state (`sigma_xy`); undefined or overflowing gives `u64::MAX`. E §5.6 defines σ
  as the *fee-free* premium with E §3.1's `n(q) = ⌈q·Y/(X − q)⌉`; A §4.3 says "`sw` on
  Raydium", which differs only on a CPMM pool whose creator fee is on the token side.
  `BuyQuote::net` is the fee-free `n(q)` there, so every σ follows ECON.
- **Tiers (A §2.3.4, §4.7).** `graduated_at ≤ 0` (0 = unset) tiers as C, as do a
  non-positive price, `|expo + 6| > 30` or `λ_min = 0`. Thresholds are compared exactly
  (`Λ·price·10^(expo+6)` vs `θ_usd_e6·10⁹`), in u128 (`θ·10⁹ < 2⁹⁴`; the left side fits for
  any realistic pool) with a bit-identical 256-bit fallback only when a side leaves u128
  (review M0B-S-4: the comparison was always 256-bit, on every open's tier evaluation;
  `threshold_fast_path_equals_256_bits` checks the two paths agree across the u128 edges).
  E §2.2's overflow table lists this product since the M1 doc sync. The Raydium `R_eff`
  term floors
  the locked SOL first (`tier::locked_sol = ⌊D·isqrt(P_ema)/10⁹⌋`, u128 saturating where
  ECON §2.1 types it u64: same values), then `⌊locked·10⁴/λ_min_bps⌋`; `y_slow` must come
  from the v0.4.1 `reprice`. Tier vectors come from an A §4.7 transcription (the sim has no
  tier function).
- **Signatures (A §2.2).** `credited_for(&PosRent, &Basis)` and `lender_credit(&Aggr, δJ)`
  read the position's snapshot of `rent_protocol_bps` / the folded aggregate;
  `liquidate(x, penalty_bps, split, k_s)` adds the keeper floor `kp + min(max(0, K_s − kp),
  W)` from the borrower remainder (N22; it does not reject `K_s > V`, the top-up is capped
  by the remainder); `settle(v, kind, k_s, fv, q, p_ema, r_due)` returns the vesting lump
  `x − min(x, ⌊q·p_ema/WAD⌋ + r_due)` for S2/S3 and 0 for S1, with no 256-bit intermediate
  (when `q·p_ema` overflows u128 the bound exceeds any take); `Payout` gains `keeper_topup`
  and `lump` (the penalty's lender share on a liquidation, the expiry fee on an expiry);
  waterfalls return `Result<Payout, _>`; `k_e` is the pre-truncation reward; `close`
  returns `Underflow` when `B + R > V`.
- **Caps saturate** at `u64::MAX` rather than erroring (a cap beyond `u64` never binds);
  `curve_gamma_max_q` too. Caps take `X₁` as an input: ECON's `X₀ + Q`; on CPMM the real
  post-sell reserve is slightly lower (fee counters), so the caller picks. The Tier D γ
  guard uses `rT` before the open, matching the sim's `max_open_q`.
- **Vector notes.** σ was never undefined in the v0.4.1 exit runs (the `None` case is
  unit-tested); a capped S2/S3 take always has a positive lump, because
  `FV = B̂_FV·(1 + p') + R_due > Q·P_ema + R_due`.
- **Enum encodings** (ARCH gives none): `Tier` A=0 … D=3 (`[TierParams; 4]` order),
  `SettleKind` S1=1, S2=2, S3=3.
- Only the SPEC-literal D3/D4 readings are built (A §9 owner gates; O1/O2 are not).

## Open decisions

The **owner** items above: curve buy rounding (F-S2: E §3.1 / §3.2 against A §4.3's curve
row and "the on-chain quote equals what the venue charges"), the straddle dip's envelope
(F-A2; A §4.3 records the dip itself since the M1 doc sync) and the pool's notes (1) and
(4) (A §9 M0b). Plus:

- **O16's wording (SPEC §2.4).** SPEC §2.4 vests "settlement proceeds above the market
  value of the debt"; the sim, A §3.3, §3.8, §9 M0b and E §9.4 vest only the premium of an
  S2/S3 take above `⌊Q·P_ema/WAD⌋ + R_due` and keep S1 immediate (E §9.4: S1 pays
  `V − K_s < B̂ + ert`, a loss against the buyback cost, and vesting it would shift S1 SOL
  towards lenders who stay through a loss event). The crate follows the sim; SPEC §2.4
  (the owner's document) could say "S2/S3 settlement proceeds".
- **WASM facade (M1 doc sync follow-up).** `crates/shortwin-math-wasm`'s
  `settle::tests::params_by_name` still asserts that the sim accepts tier A
  `cr_open_bps = 100,001` (`validateSim() == undefined`); since the doc sync the sim and
  `validate` refuse it (`"A:cr_open_bps"`), so that assertion (and its comment "an ECON §15
  bound the sim does not check") needs updating, e.g. to `min_notional = 9,999,999`, which
  only `validate_econ` refuses. Not edited from this crate.
- **M1 test pin (A §1.1, U18).** Resolver 3 picks Agave 4.3.0 for Mollusk's `^4.2.0`,
  against which Mollusk 0.15.1 does not compile; the program tests' lockfile must hold
  4.2.x (as `tests/bench/cu_math` does) or move Mollusk.
- Exact-in buys (pump `buy_exact_sol_in`, PumpSwap `buy_exact_quote_in`) have no quote
  API; add one if a keeper or frontend flow needs it (formulas in R-PUMP §4, R-VEN §2.4).

**Closed by the M1 doc sync** (`docs/design` edited to match the sims, which the crate
already followed; the only crate change is `params`, below):

- **One source of parameter validation (M0B-S-1).** `shortwin_sim.validate_params` now
  checks the penalty split's protocol and keeper parts and every per-tier bound, so
  `params::validate` is the sim again (3,448 configurations of `params.json`, every rule
  exercised) and `validate_econ` keeps only the bounds of the parameters the sim does not
  model; `config_check.py` takes its per-tier scan ranges from the sim's `TIER_BOUNDS`.
  E §15, A §2.3.3, §3.1, §8.1 and §9 M0b say so. Before, the v0.4.1 sim accepted 69
  configurations of the old `params.json` that the program refuses, so a proposal could
  pass `config_check.py` and still fail on-chain.
- **O15's scope (M0B-S-2):** the owner amended SPEC §0, §5.3 and §14 to S2 and S3; A §4.5,
  §10 and E §8.6, §14, §17 aligned.
- **D4 with an undefined EMA quote (M0B-S-3):** A §3.4 step 10, A §4.3 and E §4 state the
  sim's reading (the spot quote alone).
- A §3.0 step 5's ring entry `(ts, p, l, x, y, lo, hi, llo, lhi)`; A §9 M0b's `integrate`
  (no venue-kind argument) and generator wording (`gen_settle.py` for the box decisions,
  `gen_exits.py` for the waterfalls); the reserve wording of A §2.1 `engine/bounds.rs`,
  A §8.1 and the A §9 M2 row (liquidation none, expiry rent and fee); A §2.2's `fees`
  (components, `oneway_ppm`), `rent::accrue` and pool signatures; A §2.3.6 `cal_day`
  never backwards.
- E §2.2's overflow table (σ's numerator, the impact caps' `X²·(10⁴ + ι)`, the tier
  threshold) and A §2.2's fallback list; E §6.1's `conservative_x0` and, with A §4.6, the
  integer impact-cap forms.
- E §5.6, A §4.5, A §8.1: "sustained" uptrend.
- The vest-end precondition in `pool_model.py` `_vest`, E §9.4 and A §3.3 (M0B-A-2).
- `docs/design/arch/cu_math` (V18's bench, v0.3 API) is marked superseded by
  `tests/bench/cu_math`.
