# 🔐 Security Review — short.win-6-math-rest

---

## Scope

|  |  |
| --- | --- |
| **Mode** | filename |
| **Files reviewed** | `./crates/shortwin-math/src/bounds.rs` · `./crates/shortwin-math/src/caps.rs` · `./crates/shortwin-math/src/consts.rs`<br>`./crates/shortwin-math/src/error.rs` · `./crates/shortwin-math/src/fees.rs` · `./crates/shortwin-math/src/num/mod.rs`<br>`./crates/shortwin-math/src/num/u256.rs` · `./crates/shortwin-math/src/persist.rs` · `./crates/shortwin-math/src/pool_select.rs`<br>`./crates/shortwin-math/src/quote/cpmm.rs` · `./crates/shortwin-math/src/quote/mod.rs` · `./crates/shortwin-math/src/quote/pricing.rs`<br>`./crates/shortwin-math/src/quote/ray_v4.rs` · `./crates/shortwin-math/src/quote/reprice.rs` · `./crates/shortwin-math/src/quote/sol_leg.rs`<br>`./crates/shortwin-math/src/quote/straddle.rs` · `./crates/shortwin-math/src/rent.rs` · `./crates/shortwin-math/src/risk.rs`<br>`./crates/shortwin-math/src/settle.rs` · `./crates/shortwin-math/src/tier.rs` · `./crates/shortwin-math/src/waterfall.rs` |
| **Confidence threshold (1-100)** | 75 |
| **Passes** | 3 |
| **Memory** | 0 records before this scan · 56 after · `55b3e23` |

---

## Findings

[75] **1. Deposit-wrapped cranks raise L_hi, so S2 and S3 never settle**

`shortwin-math::box_of` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
A borrower who adds and removes liquidity around a crank in 4 of 8 seconds raises L_hi, so S2 and S3 never settle the position.

**Fix (Option A — take L_hi from the bucket lows, which only attacker-only seconds raise)**:

```diff
@@ crates/shortwin-math/src/settle.rs — box_of
-    let (p_mlo, p_mhi, l_mlo, l_mhi) = ring_medians(ring)?;
+    let (p_mlo, p_mhi, l_mlo, _l_mhi) = ring_medians(ring)?;
+    // A deposit around any record raises that bucket's `lhi`, and honest records in the same
+    // second cannot lower a maximum. Only an attacker-only second raises a bucket's `llo`.
+    let l_hi_med = ring_liq_upper_median_of_lows(ring)?;
     Ok(SettleBox {
         p_lo: ema.price.min(p_mlo),
         p_hi: ema.price.max(p_mhi),
         l_lo: ema.liq.min(l_mlo),
-        l_hi: ema.liq.max(l_mhi),
+        l_hi: ema.liq.max(l_hi_med),
     })
@@ crates/shortwin-math/src/observe.rs
+/// Upper median (index `⌊n/2⌋`) of the ring's bucket liquidity lows `llo`.
+pub fn ring_liq_upper_median_of_lows(ring: &Ring) -> Result<u128, MathError> {
+    ring.check()?;
+    let n = ring.len();
+    let mut v = [0u128; RING_LEN];
+    for (k, e) in ring.iter().enumerate() {
+        *v.get_mut(k).ok_or(MathError::InvalidInput)? = e.llo;
+    }
+    let vals = v.get_mut(..n).ok_or(MathError::InvalidInput)?;
+    vals.sort_unstable();
+    vals.get(n / 2).copied().ok_or(MathError::InvalidInput)
+}
```

**Fix (Option B — cap L_mhi at the M4F-1 depth cap, as ref_liquidity does)**:

```diff
@@ crates/shortwin-math/src/settle.rs — box_of
-pub fn box_of(ema: &Ema, ring: &Ring, now: i64, executable: bool) -> Result<SettleBox, MathError> {
+pub fn box_of(
+    ema: &Ema,
+    ring: &Ring,
+    now: i64,
+    executable: bool,
+    depth_cap: Option<u128>,
+) -> Result<SettleBox, MathError> {
@@
-        l_hi: ema.liq.max(l_mhi),
+        // `bounds::ref_depth_cap(pool_d, λ_min)`: a deposit-wrapped bucket high credits no
+        // more depth than sizing does.
+        l_hi: ema.liq.max(depth_cap.map_or(l_mhi, |cap| l_mhi.min(cap))),
```

---

[75] **2. A fixed K_s reserve blocks every buyback at the legal min_notional floor**

`shortwin-math::buyback_allowed` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
When governance sets min_notional to its legal floor of 10⁷, K_s blocks every buyback, so S1 gives the keeper half the vault.

**Fix (Option A — validate the params bound)**:

```diff
@@ crates/shortwin-math/src/params.rs
-    /// `min_notional` ∈ [10⁷, 10⁹].
+    /// `min_notional` ∈ [6·10⁷, 10⁹]: at the floor every tier keeps
+    /// `CR_open·N·(10⁴ − settle_keeper_bps)/10⁴ − settle_keeper_fixed − exec_rent_tol > CR_open·N/cr_liq`.
     MinNotional,
@@
-            Self::MinNotional => (10_000_000, 1_000_000_000),
+            Self::MinNotional => (60_000_000, 1_000_000_000),
```

**Fix (Option B — restrict the open)**:

```diff
@@ programs/shortwin/src/instructions/open_short.rs — after `let v = s.checked_add(m).or_math()?;`
+    // The vault at CR = L must still pay the buyback, ert and K_s, or no keeper exit can run.
+    let b_at_liq = mul_div(u128::from(v), BPS_U128, u128::from(tp.cr_liq_bps), Rounding::Floor).math()?;
+    let k_s = risk::k_s(v, &config.params.settle_keeper()).math()?;
+    require!(
+        b_at_liq + u128::from(ert) + u128::from(k_s) <= u128::from(v),
+        ShortwinError::ExitRoomTooSmall
+    );
```

**Fix (Option C — scale the fixed part of K_s with the vault)**:

```diff
@@ crates/shortwin-math/src/risk.rs — k_s
     let pct = mul_bps(u128::from(v), u128::from(terms.bps), Rounding::Floor)?;
-    let k = u128::from(terms.fixed)
+    // The fixed part takes at most 10 % of a small vault. Where ert > 0, pair this with Option A.
+    let fixed = u128::from(terms.fixed).min(u128::from(v) / 10);
+    let k = fixed
         .saturating_add(pct)
         .min(u128::from(terms.cap))
         .min(u128::from(v));
```

**Fix (Option D — cap the reserved K_s at V − B̂ − ert)**:

```diff
@@ crates/shortwin-math/src/risk.rs — buyback_allowed
 ) -> Result<bool, MathError> {
-    Ok(!budget.exceeded_by(b_live)? && sigma_live <= u64::from(sigma_max_bps))
+    // Reserve at most the K_s that the vault can spare after the buyback and ert: the
+    // liquidation waterfall pays the keeper floor from the borrower's remainder. max_in_liq
+    // and max_in_exp must use the same reduced reserve.
+    let fits = match b_live {
+        None => false,
+        Some(b) => u128::from(b).saturating_add(u128::from(budget.ert)) <= u128::from(budget.v),
+    };
+    Ok(fits && sigma_live <= u64::from(sigma_max_bps))
```

---

[75] **3. Step 2 of evaluate revives a lapsed clock with its old count**

`shortwin-math::evaluate` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
Any caller revives a lapsed arm with one sol_settle call in a calm second, so the next call settles S1 or S2 at once.

**Fix (Option A — leave a lapsed clock for step 3)**:

```diff
-/// 2. Not CR-liquidatable and not expired: an armed position is disarmed only if
-///    [`not_liquidatable`] holds on the box (`SettleDisarmed{NotLiquidatable}`); otherwise
-///    its clock keeps running (`last = now`). Nothing else happens.
+/// 2. Not CR-liquidatable and not expired: an armed position is disarmed only if
+///    [`not_liquidatable`] holds on the box (`SettleDisarmed{NotLiquidatable}`); otherwise
+///    a live clock keeps running (`last = now`) and a lapsed one is left for step 3 to
+///    re-arm. Nothing else happens.
@@ evaluate, step 2
         if state.armed != 0 {
             if not_liquidatable(quotes, expired, inp.ema_leg, v_eff, terms.cr_liq_bps, bx)? {
                 state.armed = 0;
                 out.arm = ArmChange::Disarmed(DisarmReason::NotLiquidatable);
-            } else {
+            } else if state.is_live(now, terms.arm_gap_s) {
                 state.last = now;
                 out.arm = ArmChange::Kept;
             }
         }
```

**Fix (Option B — clear a lapsed arm)**:

```diff
@@ evaluate, step 2
         if state.armed != 0 {
             if not_liquidatable(quotes, expired, inp.ema_leg, v_eff, terms.cr_liq_bps, bx)? {
                 state.armed = 0;
                 out.arm = ArmChange::Disarmed(DisarmReason::NotLiquidatable);
-            } else {
+            } else if state.is_live(now, terms.arm_gap_s) {
                 state.last = now;
                 out.arm = ArmChange::Kept;
+            } else {
+                // A lapsed clock must re-arm with a fresh count.
+                state.armed = 0;
             }
         }
```

---

[75] **4. lender_credit drops its remainder, so lenders lose rent on every accrual**

`shortwin-math::lender_credit` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
Each accrual floors the lender credit with no carry, and the true-up removes the exact share, so lenders lose up to a lamport per second.

**Fix**

```diff
@@ crates/shortwin-math/src/rent.rs — lender_credit
-pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3]) -> Result<u64, MathError> {
-    dot_div(aggr.a, *delta, 1, RENT_DEN_BPS, Rounding::Floor)
+/// `carry` is the market's stored remainder of the earlier accruals, in `[0, RENT_DEN_BPS)`.
+/// It is updated in place, so the credits of all accruals sum to the floor of their exact total.
+pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3], carry: &mut u128) -> Result<u64, MathError> {
+    let n = aggr
+        .a
+        .into_iter()
+        .zip(*delta)
+        .try_fold(U256::ZERO, |acc, (x, y)| acc.checked_add(U256::from_product(x, y)))
+        .and_then(|s| s.checked_add(U256::from_u128(*carry)))
+        .ok_or(MathError::Overflow)?;
+    let (q, rem) = n.div_rem_u128(RENT_DEN_BPS).ok_or(MathError::DivByZero)?;
+    *carry = rem;
+    q.to_u64().ok_or(MathError::Overflow)
 }
@@ programs/shortwin/src/engine/prologue.rs — accrue
-    let credit = rent::lender_credit(&market.aggr(), &delta).math()?;
+    let mut carry = market.rent_credit_carry.get();
+    let credit = rent::lender_credit(&market.aggr(), &delta, &mut carry).math()?;
+    market.rent_credit_carry.set(carry);
```

---

[75] **5. A pulled deposit blocks Raydium liquidate and expire for over an hour**

`shortwin-math::ref_depth_cap` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
A borrower who holds and pulls a deposit blocks Raydium exits for over an hour, because ref_depth_cap credits twice the locked depth; also in ref_liquidity.

**Fix (Option A — cap the Raydium EMA terms at D, as M6E-1 does on PumpSwap)**:

```diff
@@ programs/shortwin/src/engine/bounds.rs — ref_depth_cap
 pub fn ref_depth_cap(config: &GlobalConfig, market: &Market, kind: VenueKind) -> Option<u128> {
-    kind.lock_min_bps(config.params.pool_lock_min_bps)
-        .map(|lambda_min| bounds::ref_depth_cap(market.pool_d.get(), lambda_min))
+    // As M6E-1 on PumpSwap: the EMA terms of ref_liquidity credit no exit above the
+    // locked depth D, so a held and pulled deposit cannot lift L_b over the live pool.
+    kind.lock_min_bps(config.params.pool_lock_min_bps)
+        .map(|_| bounds::ref_depth_cap(market.pool_d.get(), 10_000))
 }
```

**Fix (Option B — cap at the pool's own depth when it is locked above λ_min)**:

```diff
@@ crates/shortwin-math/src/bounds.rs — ref_depth_cap
-pub fn ref_depth_cap(locked_depth: u128, lambda_min_bps: u32) -> u128 {
-    depth_at_lambda(locked_depth, u64::from(lambda_min_bps))
+/// A pool locked above `λ_min` is credited with no more than its own depth `⌊D·10⁴/λ_live⌋`.
+pub fn ref_depth_cap(locked_depth: u128, lambda_min_bps: u32, lambda_live_bps: u32) -> u128 {
+    depth_at_lambda(locked_depth, u64::from(lambda_min_bps.max(lambda_live_bps)))
 }
@@ programs/shortwin/src/engine/bounds.rs — ref_depth_cap
-        .map(|lambda_min| bounds::ref_depth_cap(market.pool_d.get(), lambda_min))
+        // λ_live: `pool_select::lambda_bps` at the observation that refreshed `pool_d`.
+        .map(|lambda_min| bounds::ref_depth_cap(market.pool_d.get(), lambda_min, market.pool_lambda_bps))
```

---

[75] **6. Under the guard, an LP holder forces liquidation at a drained depth**

`shortwin-math::spot_liquidity` · Confidence: 75 · seen in 2/3 runs · NEW

**Description**
While the guard is set, the spot leg floor reads bucket lows that withdraw-wrapped cranks set, so an LP holder liquidates a healthy position.

**Fix (Option A — floor at the unguarded EMA depth)**:

```diff
@@ crates/shortwin-math/src/bounds.rs — spot_liquidity
-pub fn spot_liquidity(l_live: u128, ema: &Ema, ring: &Ring) -> Result<u128, MathError> {
-    Ok(l_live.max(guarded_ema_liquidity(ema, ring)?))
+pub fn spot_liquidity(l_live: u128, ema: &Ema, _ring: &Ring) -> Result<u128, MathError> {
+    // Withdraw-wrapped cranks lower `L_mlo`, never `L_ema`: floor the spot leg at the
+    // unguarded EMA depth, also while the post-change guard is set.
+    Ok(l_live.max(ema.liq))
 }
```

**Fix (Option B — floor at the upper median of the bucket highs under the guard)**:

```diff
@@ crates/shortwin-math/src/bounds.rs
-use crate::observe::{Ema, LastBucket, Ring, ring_liq_lower_median, sustained_growth};
+use crate::observe::{Ema, LastBucket, Ring, ring_liq_lower_median, ring_medians, sustained_growth};
@@ spot_liquidity
 pub fn spot_liquidity(l_live: u128, ema: &Ema, ring: &Ring) -> Result<u128, MathError> {
-    Ok(l_live.max(guarded_ema_liquidity(ema, ring)?))
+    // Under the guard, floor at a value a same-transaction withdrawal cannot lower: the upper
+    // median of the bucket highs `lhi`, not the lower median of the lows `llo`.
+    let floor = if ema.guard != 0 {
+        let (_, _, _, l_mhi) = ring_medians(ring)?;
+        ema.liq.min(l_mhi)
+    } else {
+        ema.liq
+    };
+    Ok(l_live.max(floor))
 }
```

---

Findings List

| # | Confidence | Title |
|---|---|---|
| 1 | [75] | Deposit-wrapped cranks raise L_hi, so S2 and S3 never settle |
| 2 | [75] | A fixed K_s reserve blocks every buyback at the legal min_notional floor |
| 3 | [75] | Step 2 of evaluate revives a lapsed clock with its old count |
| 4 | [75] | lender_credit drops its remainder, so lenders lose rent on every accrual |
| 5 | [75] | A pulled deposit blocks Raydium liquidate and expire for over an hour |
| 6 | [75] | Under the guard, an LP holder forces liquidation at a drained depth |

---

## Leads

_Vulnerability trails with concrete code smells where the full exploit path could not be completed in one analysis pass. These are not false positives — they are high-signal leads for manual review. Not scored._

- **assign treats a future oracle publish_time as fresh** — `shortwin-math::assign` · seen in 2/3 runs · NEW — Code smells: `let stale = inp.now.saturating_sub(inp.oracle.publish_time) > i64::from(inp.oracle_stale_s);` is false when `publish_time > now`, so a cache with a future date stays fresh; `assign` sets no upper limit on a future publish time; only `crank_sol_usd` refuses a publish time later than `now + oracle_max_age_s` (`OracleFutureDated`, M1-SEC-8, at most 600 s); the thresholds use the low end `price − conf`, so the tier stays conservative — assign treats an oracle publish_time later than now as fresh, so a future-dated cache stays usable after oracle_stale_s. Today only crank_sol_usd limits future dates, and we found no path around it.
- **A halted venue collapses the box onto a possibly pushed EMA** — `shortwin-math::box_of` · seen in 1/3 runs · NEW — Code smells: `if !executable && age > ring_len { return Ok(SettleBox::point(ema.price, ema.liq)); }`; the doc says "a venue that records no observations cannot have been pushed since", but the last integration before a halt can hold a bucket that an attacker-only second pushed; after the collapse both corners are equal, so `amount_ok` always passes — An attacker who pushes the last bucket before a halt makes S3 pay FV at the pushed EMA. We did not show that an attacker can time the halt.
- **box_of returns the EMA point with no ring check on a halted venue** — `shortwin-math::box_of` · seen in 1/3 runs · NEW — Code smells: the collapsed-point path (`!executable && age > RING_LEN`) returns `SettleBox::point` before `ring_medians` runs `ring.check()`; the doc says "Errors: InvalidInput for a corrupted or empty ring" — box_of skips the ring check on a halted venue, so a future caller that also reads the ring there trusts a corrupted ring. Today the point path reads only `ema`.
- **A same-transaction swap picks the fee tier of the robust quotes** — `shortwin-math::buy_repriced` · seen in 2/3 runs · NEW — Code smells: `Pricing::buy_repriced` and `CurvePricing` take the fee that `fees::pump_amm` and `fees::pump_curve` resolve at the live market cap (quote/mod.rs, R3B-7 b); that fee prices the ring-entry quotes of the EMA leg, the spot leg, the `BoxQuotes` corners, B̂_FV and B̂_ref; a swap earlier in the same transaction moves the pre-trade reserves across a pump-fees threshold; on the PumpSwap table the step at 98,240 SOL is 120 bps (20/5/95) below and 30 bps (20/5/5) above, both under the 500 bps cap, so a liquidator's sell that takes the live market cap just under the threshold, with P_live still at or above lb_lo, re-prices every risk quote by 1.012/1.003 (+0.9 %); M7-SEC-5 bounds the tier only for the open caps — A liquidator who sells under a PumpSwap fee threshold in the same transaction raises every risk quote by about 0.9 %, so positions with CR below 1.009·L become liquidatable. We did not check how often the threshold falls inside one bucket's price range.
- **RayV4Fee::capped can build a fee with num at or above den** — `shortwin-math::capped` · seen in 3/3 runs · NEW — Code smells: `RayV4Fee` has public fields, and `capped` returns `{num: cap_bps, den: BPS}` without the `num < den` check that `ray_v4` enforces; with `cap_bps ≥ 10⁴`, `ray_v4::buy_exact_out` returns `None` for every risk quote, and the predicates read `None` as +∞ (CR 0); the siblings `FeeBps::capped` and `CpmmFees::capped` stay defined; `VenueFeeHardCapBps` (300, 2,000) blocks this today — RayV4Fee::capped can build a fee with num at or above den, so every Raydium v4 risk quote becomes undefined and reads as liquidatable. This needs a hard cap of 10⁴ bps or more, and the compiled bounds block it.
- **carried_fit checks each carried position with the post-open impact formula** — `shortwin-math::carried_fit` · seen in 1/3 runs · NEW — Code smells: the market check uses `oi_cap(c.x0, ..)` because "the candidate's reserve holds none of them", but the per-position check uses `max_q_position(c.x0, ι_pos)`, whose formula assumes the reserve is `X₀ + q`; a carried buyback of q comes out of X₀, so the matching bound is `max_total_q(X₀, ι_pos)`, and the check is looser by a factor of `√(1 + ι_pos)`; at ι_pos 100 bps, where the fee cap at φ ≤ 2,500 ppm does not bind, q = 0.498 % of X₀ passes and its buyback moves the price by 1.0035 %, not 1 % — carried_fit admits a carried position whose buyback moves the price above ι_pos. The impact cap binds only when ι_pos is below about 250 bps.
- **carried_phi reads a zero mark as the candidate fee, the loosest φ** — `shortwin-math::carried_phi` · seen in 2/3 runs · NEW — Code smells: the two zero-mark fallbacks point in opposite directions in `carried_fit`: `carried_q_bound(0, ΣQ)` returns ΣQ, the strictest `q_max`, while `carried_phi(0, φ_cand)` returns `min(φ_cand, CARRIED_FEE_CAP_PPM)`, the most permissive φ; example: X₀ 150·10¹², x100 300, candidate 2,500 ppm — mark 500 gives a fee cap of 225·10⁹ and mark 0 gives 1,125·10⁹; a zero mark with open positions needs a legacy account or a 0-ppm open, and the fee-scaled cap blocks a 0-ppm open today — With a zero Market.min_open_fee_ppm, carried_phi uses the candidate fee, so carried_fit admits positions above their margin class. A migration or a 0-ppm open makes this reachable.
- **CARRIED_FEE_CAP_PPM keeps the 0.75 % class only at the default fee_size_cap_x100** — `shortwin-math::carried_phi` · seen in 3/3 runs · NEW — Code smells: `CARRIED_FEE_CAP_PPM = 2_500` is justified in consts.rs as keeping carried positions in "the ≤ 0.75 %-of-X₀ size class (fee_size_cap_x100 300 at 25 bps)"; `fee_cap` computes `⌊x100·φ·X/10⁸⌋`, so the class is 0.75 % only at `fee_size_cap_x100 = 300`; `Rule::FeeSizeCapX100` allows 100–500, and at 500 the class is 1.25 % of X₀; keeping 0.75 % needs `⌊750_000/fee_size_cap_x100⌋` ppm — When governance sets fee_size_cap_x100 to its legal 500, a pool switch carries positions of 1.25 % of X₀, not the 0.75 % that CARRIED_FEE_CAP_PPM targets. We did not prove that the larger class reaches S1 in the switch sims.
- **One routed observation clears the whole no-route chain** — `shortwin-math::chain_step` · seen in 2/3 runs · NEW — Code smells: `chain_step(.., hit = false)` returns `(0, 0)`, so one routed observation erases the chain with no minimum duration, and `s3_due` then needs another unbroken `s3_wait_s` (600 s); every prologue runs this step, including the `crank_price` instruction that any caller can send; the no-route inputs are `venue_executable`, the route-broken flag and `φ_live` above the hard cap; a venue admin who lifts the fee above the cap can drop it for one instruction in each 600-s window, crank, and lift it again — A venue admin who restores the route for one instruction in each 600-second window stops S3 for every liquidatable position. We found no party other than the venue admin that controls these inputs.
- **curve_gamma_ok accepts a γ of 10⁴ or more** — `shortwin-math::curve_gamma_ok` · seen in 2/3 runs · NEW — Code smells: `curve_gamma_max_q` returns `InvalidInput` for `γ ≥ 10⁴`, but `curve_gamma_ok` does not check γ; at γ = 10⁴ the test `(ΣQ + q)·10⁴ ≤ 10⁴·(rT + q)` reduces to `ΣQ ≤ rT`, so any `q` passes; the compiled `CurveOiGammaBps` bound [1,000, 9,000] blocks this today — curve_gamma_ok accepts γ ≥ 10⁴, which curve_gamma_max_q rejects, so at γ = 10⁴ the Tier D guard passes any q while ΣQ ≤ rT. A future bound change removes the graduation guard.
- **div_rem_limbs returns a zero quotient for a zero divisor** — `shortwin-math::div_rem_limbs` · seen in 1/3 runs · NEW — Code smells: `div_rem_limbs` returns an all-zero `(q, r)` when `significant(v) == 0` (`if n == 0 { return (q, r); }`), and again when `NonZeroU128::new` fails, instead of an error; its doc says "a zero divisor yields zero quotient and remainder; callers check first"; `div_rem`, `div_rem_u128` and `mul_div_floor` check the divisor first, and `isqrt` passes a divisor of at least 2⁶⁴ — div_rem_limbs returns 0 for a zero divisor instead of an error. A future caller that skips the divisor check gets a fee or a bound of 0.
- **A settlement clock counts EMA updates from before a pool restart** — `shortwin-math::evaluate` · seen in 3/3 runs · NEW — Code smells: `mark_restart` sets only `warm` and `guard`, and `rewrite_liquidity` keeps `ema.count`, so `Ema.count` and `ArmState.count` survive a pool adoption or a graduation; `evaluate` returns `Evaluation::default()` during the warm-up and leaves `ArmState` unchanged, while the warm-up integrations still increase `ema_count`; the warm-up takes at least 8 integrations, and `settle_arm_gap_s` is 10 s by default and up to 60 s, so a clock armed on the old pool is still live at the gate; `updates = ema_count − count` then holds old-pool and warm-up integrations — A clock armed before a pool restart keeps its count, so S1 or S2 can execute at the first warmed call. We did not prove that the program keeps such an arm live across a real adoption.
- **A widened box keeps an old S1/S2 clock armed through calm seconds** — `shortwin-math::evaluate` · seen in 2/3 runs · NEW — Code smells: only `refuted` (step 3) and `not_liquidatable` (step 2) reset an armed clock, and both read the corner `(P_hi, L_lo)`; `refuted` also reads σ at `min(L_live, L_lo)`; any caller can widen that corner without an attacker-only second: a pump-wrapped crank in 4 of 8 buckets raises `P_mhi`, an unlocked LP's `[withdraw, crank, deposit]` lowers `L_mlo` with no swap fee, and a withdrawal in the same transaction lowers `L_live`; while no reset fires, each call writes `last = now` (Kept) and `ema_count − count` keeps growing from the first arming; when the S1/S2 legs next hold, `updates ≥ settle_persist_n` already holds and the call settles at once; ECON §5.6 says widening "cannot flip a decision" — A lender who widens the box keeps an S1 or S2 clock armed through a calm period, so the next leg hold settles at once. We did not measure the gain against the cost of the widening.
- **Only a debug_assert! checks that S3 is not due during the warm-up** — `shortwin-math::evaluate` · seen in 2/3 runs · NEW — Code smells: `if inp.executable && !inp.warmed { debug_assert!(!s3_due(&inp.s3, &terms.s3, now, true)); return Ok(Evaluation::default()); }`; release builds remove `debug_assert!`, and the early return then skips step 4; today the claim holds because every restart clears `no_route_since` and the chain needs hits for at least `s3_wait_s` (≥ 600 s), against `EMA_WARMUP_BUCKETS` = 8 integrations; a new restart path that keeps the chain, or a lower `s3_wait_s` bound, makes a due S3 return `Evaluation::default()` with no error — In a release build, evaluate skips a due S3 during the warm-up without an error, because only debug_assert! checks that S3 cannot be due then. Today every restart clears the no-route chain.
- **evaluate settles on the arming call when persist_n is 0** — `shortwin-math::evaluate` · seen in 3/3 runs · NEW — Code smells: on the arming call `count = ema_count`, so `updates = 0`, and `updates >= u64::from(terms.persist_n)` is true when `persist_n = 0`; `evaluate` has no local check; only the compiled `Rule::SettlePersistN` bounds (40, 600) keep the value at 40 or more — evaluate settles S1 or S2 on the call that arms the clock when persist_n is 0. Today only the compiled SettlePersistN bounds block this value.
- **A halted warm-up venue lets S3 pay FV at the restart anchor** — `shortwin-math::evaluate` · seen in 2/3 runs · NEW — Code smells: `evaluate` gates only `inp.executable && !inp.warmed`, and its doc keeps a halted venue ungated on purpose so that S3 is never deadlocked; the prologue records buckets only on an executable venue, so a venue that halts during the warm-up keeps `warm > 0` and `L_ema` at the restart anchor (Raydium `⌊D·10⁴/λ_min⌋`, PumpSwap `lp_supply`); after `grad_grace_s + s3_wait_s`, `s3_due` holds, `box_of` falls back to the point `(P_ema, L_ema)` after `RING_LEN` seconds, and `settlement` pays FV on the anchor; with λ above λ_min the anchor is up to `λ/λ_min` times the real `√(X·Y)`, so B̂_FV and the lenders' take are lower — On a venue that halts during the warm-up, evaluate pays S3 at FV on the restart anchor, which can pay the lenders less. We did not prove that such a halt happens.
- **expire pays K_e after rent and the fee, so K_e can be 0** — `shortwin-math::expire` · seen in 1/3 runs · NEW — Code smells: `let k_e = risk::k_e(x.b_ref, keeper)?.min(w)` with `w` taken after rent and the expiry fee; `max_in_exp` reserves `K_s` (`s1_budget`) but not `K_e`; example: V 1e9, K_s 2e7, R 3e7, B̂_ref 9.5e8, fee 200 bps, δ_exp 100, ε 50 and ert 0 give max_in 954,750,000, B 954,000,000, r 3e7, e 1.6e7 and K_e 0, while S1 stays false because B̂ < V − K_s; `liquidate` has the N22 keeper floor, and `expire` has none — At the max_in_exp edge the expiry waterfall pays the keeper nothing, while S1 stays false. We did not prove that no keeper then runs the expiry.
- **expire does not check expiry_fee_bps against 10⁴ as its doc states** — `shortwin-math::expire` · seen in 3/3 runs · NEW — Code smells: the doc says "`InvalidInput` if a rate exceeds 10⁴ bps"; only `x.rent_protocol_bps` goes through `check_bps`; `expiry_fee_bps` goes unchecked into `bps_of(x.base(), expiry_fee_bps, Rounding::Ceil)?.min(w)`, and that amount vests to the lenders as `lump`; only the compiled `ExpiryFeeBps` bound (0–500) keeps the rate in range today — expire accepts an expiry_fee_bps above 10⁴ that its doc says it refuses, so a bad locked value gives the whole borrower remainder to the lenders. The compiled ExpiryFeeBps bound blocks this today.
- **fee_cap reads a zero multiplier as cap off** — `shortwin-math::fee_cap` · seen in 2/3 runs · NEW — Code smells: `if x100 == 0 { return Ok(None) }` turns `fee_size_cap` and `fee_oi_cap` off when the parameter is zero memory; the doc keeps 0 as "the sims' switch"; the compiled bounds [100, 500] and [300, 1,000] reject 0 today — fee_cap reads a zero fee_size_cap_x100 or fee_oi_cap_x100 as cap off, so a zeroed parameter removes the fee-scaled caps. The compiled bounds reject 0 today.
- **A zeroed tier byte decodes as Tier A, the most permissive tier** — `shortwin-math::from_u8` · seen in 3/3 runs · NEW — Code smells: `tier.rs` declares `#[repr(u8)] enum Tier { A = 0, B = 1, C = 2, D = 3 }` and `Tier::from_u8(0) => Some(Self::A)`; `Market.tier` and `ShortPosition.tier` are stored as `u8`; Tier A has the lowest `cr_open`, `cr_liq` and penalty and the longest duration; today `create_market` and `open_short` write the tier — Tier::from_u8 decodes a zeroed tier byte as Tier A, the most permissive tier. A future instruction that skips the tier write gives the position Tier A terms.
- **liq_ema_leg returns true for n = 0 with no ring evidence** — `shortwin-math::liq_ema_leg` · seen in 3/3 runs · NEW — Code smells: with `n = 0`, `ring.len() < n` is false and `take(0)` checks no entry, so `liq_ema_leg` returns `Ok(true)` without a call to `below` (the test asserts it); only the compiled `Rule::LiqPersistN` bounds (2, 3) keep a locked `liq_persist_n` above 0 — liq_ema_leg returns true for n = 0 with no ring evidence. A future LiqPersistN bound of 0 makes the EMA leg true for every position.
- **A zero ref_max_age_s makes every completed bucket old** — `shortwin-math::liq_reference` · seen in 1/3 runs · NEW — Code smells: `LastBucket::is_fresh` is `now − sec ≤ max_age_s`, and a completed bucket always has `sec < now`, so `ref_max_age_s = 0` is never fresh; `liq_reference` and `expiry_reference` then return `Ok(None)`, and `observe::robust_spot` returns `None`, which also blocks the spot leg and the S1/S2 legs; no function rejects 0, and only `Rule::RefMaxAgeS` [1, 3] guards it — A ref_max_age_s of 0 makes liq_reference and expiry_reference return None, so keepers cannot exit any position. Only the compiled bound blocks this today.
- **liquidate does not check penalty_bps against 10⁴ as its doc states** — `shortwin-math::liquidate` · seen in 3/3 runs · NEW — Code smells: the doc says "`InvalidInput` if the split does not sum to 10⁴ or a rate exceeds 10⁴ bps"; the code runs only `split.validate()?` and `check_bps(x.rent_protocol_bps)?`; `penalty_bps` goes straight into `bps_of(x.base(), penalty_bps, Rounding::Ceil)?.min(w)`, and `.min(w)` then gives the penalty the whole borrower remainder; only the compiled `TierPenaltyBps` bound (0–2,000) keeps the rate in range today — liquidate accepts a penalty_bps above 10⁴ that its doc says it refuses, so a bad locked value takes the whole borrower remainder. The compiled TierPenaltyBps bound blocks this today.
- **Rent due ranks ahead of the keeper top-up in liquidate** — `shortwin-math::liquidate` · seen in 3/3 runs · NEW — Code smells: `liquidate` pays rent `r = min(R_due, V − B)` first (`after_buyback_and_rent`), then `π = min(⌈p·base⌉, W)`, then `keeper_topup = k_s.saturating_sub(penalty_keeper).min(w)`; `max_in_liq` lets `B` reach `V − K_s − ert` and reserves nothing for `R_due`; when `R_due ≥ K_s + ert` the penalty and the top-up are 0; example: V 1,000,000,000, K_s 20,000,000, B 980,000,000 and R_due 25,000,000 give r 20,000,000, π 0 and keeper 0, while S1 at the same state pays the keeper K_s first and in full — liquidate pays rent and the penalty before the keeper top-up, so a keeper can get less than the K_s that S1 pays. We did not prove that keepers then skip such positions.
- **liquidity_lamports subtracts the whole VQR from the burned LP's share** — `shortwin-math::liquidity_lamports` · seen in 2/3 runs · NEW — Code smells: `locked_sol(D, P_ema) = λ·(Y + VQR)`, because `D` is taken on the effective reserve; `TierVenue::PumpSwap` subtracts the whole live VQR; VQR scales with third-party deposits, so the bound is `λ·Y − (1 − λ)·VQR`, not the burned LP's real SOL `λ·Y` that the doc states; example: λ 0.9, Y 100 SOL and VQR 50 SOL give 85 SOL against 90 SOL locked — liquidity_lamports subtracts the whole VQR from the burned LP's share, so a PumpSwap pool with third-party LP tiers on less SOL than it locks. The error is conservative, and we found no path that harms lenders.
- **A large deposit makes the whole-bps floor of λ cut D** — `shortwin-math::locked_depth` · seen in 2/3 runs · NEW — Code smells: `D = ⌊lambda_bps(c, s)·isqrt(X·Y)/10⁴⌋`; an unlocked deposit keeps `c − s` fixed and scales `c` and `√(X·Y)` together, so the loss is `frac(λ')/λ'` of the true D: with the test pool at λ 99.78 %, a deposit of 99× the pool gives 99 bps and D −0.78 %, and 999× gives 9 bps and D −9.8 %; `propose_pool` and `execute_pool_switch` judge eligibility on the candidate's D; the single floor `⌊(c − s)·isqrt(X·Y)/c⌋` never exceeds the true depth — A large unlocked deposit drops λ to a few bps, so the whole-bps floor cuts D by up to all of it in that instruction. The deposit must be about 100 to 10,000 times the locked LP.
- **margin_d4 falls back to the spot quote when the EMA quote is undefined** — `shortwin-math::margin_d4` · seen in 1/3 runs · NEW — Code smells: `let b_ema = b_ema.unwrap_or(b_spot);`; the margin grows without limit as `B̂_ema` rises when q nears the EMA state's x, then drops to `cr_open·B̂_spot` once q ≥ x and the quote is undefined; the doc calls this sim-normative and notes that ARCH would refuse the open; today `X₀` at or below the token side of the locked depth keeps q far below `x_ema` on Raydium and PumpSwap, and the curve L is constant — margin_d4 uses the spot quote alone when the EMA quote is undefined, so the margin drops at that edge. A sizing change that lets q reach the EMA reserve makes this reachable.
- **max_in prices B̂_ref at the capped fee but bounds the live-fee quote** — `shortwin-math::max_in_liq` · seen in 2/3 runs · NEW — Code smells: `B̂_ref` comes from `pr.capped(venue_fee_hard_cap_bps_pos).buy_repriced(..)` (`programs/shortwin/src/engine/bounds.rs`), but the keeper's check is `B̂_live ≤ max_in` at the live fee; example on PumpSwap: locked cap 500 bps, live fee 800 bps and global cap 1,000 bps (no S3), with `P_ref = P_live` and `L_b = L_live`, give `max_in = ⌊1.05n·1.02⌋ = 1.071n < B̂_live = 1.08n`, so `ExitBoundNotMet`; liquidation passes only when `κ_eff` lifts `P_ref` above `P_live`; expiry (`δ_exp` 1 %) is blocked once the live fee passes about 6.05 % at a 5 % cap — A live fee about δ_liq above the locked cap blocks every liquidation, because max_in uses B̂_ref at the capped fee. We did not prove that a venue fee can exceed the locked cap.
- **The box corners miss a dip of the curve straddle quote** — `shortwin-math::not_liquidatable` · seen in 3/3 runs · NEW — Code smells: the settle module doc says "`B̂` and σ increase with `P` and decrease with `L`", so `not_liquidatable` reads only `(P_hi, L_lo)`; the `curve_straddle` doc says `B̂_str` dips by about 0.02 % where `rT(P)` crosses `q`, so an interior point of a box that spans the crossing can cost more than the corner; for `q > rT(P)` the straddle quote can also rise with `L`, the opposite of the module rule — not_liquidatable reads only the (P_hi, L_lo) corner, but the curve straddle quote is not monotone, so a disarm can skip a liquidatable point. The dip is about 0.02 %, and we did not show a changed outcome.
- **Withdraw-wrapped cranks lower L_lo, so not_liquidatable cannot disarm the clock** — `shortwin-math::not_liquidatable` · seen in 2/3 runs · NEW — Code smells: `box_of` sets `l_lo = min(L_ema, L_mlo)`, and `L_mlo` is the lower median of the bucket liquidity lows; a bucket low is the minimum of every record in its second (`bucket.llo = bucket.llo.min(l)`), so withdraw-wrapped cranks in 4 of 8 seconds set `L_mlo` even when the keeper also cranks in those seconds; `not_liquidatable` judges CR at `(P_hi, L_lo)`, and when it fails, step 2 writes `last = now` (Kept) — An LP holder lowers L_lo with withdraw-wrapped cranks in 4 of 8 seconds, so not_liquidatable fails and the armed clock keeps running. We did not measure how much earlier S1 or S2 then executes.
- **notional_ok reads a zero market_max_notional as no limit** — `shortwin-math::notional_ok` · seen in 2/3 runs · NEW — Code smells: `market_max_notional == 0` means no market cap, and `min_notional = 0` lets every `n_open` pass; the compiled bounds [10⁹, 10¹⁴] and [10⁷, 10⁹] reject 0 today — notional_ok reads a zero market_max_notional as no limit, so a zeroed parameter removes the market notional cap. The compiled bounds reject 0 today.
- **The fee-scaled caps count the creator fee that the creator gets back** — `shortwin-math::oneway_ppm` · seen in 2/3 runs · NEW — Code smells: `FeeBps::oneway_ppm` returns `(lp + protocol + creator)·100`, which the program passes as the caps' φ (`VenueState::cap_fee_ppm` → `caps::fee_size_cap`, `caps::oi_cap`); `CpmmFees::oneway_ppm` also counts the creator part; `fees::pump_amm` reads `is_cashback_coin` nowhere; the fresh-graduate PumpSwap tier is `FeeBps::new(20, 5, 95)`, so φ = 12,000 ppm, of which 95 bps goes to the coin creator, or back to the paying trader on a cashback coin; at φ = 12,000 ppm `fee_size_cap` is 3.6 % of X₀ and `fee_oi_cap` is 9 % of X₁, against 0.75 % and 1.875 % at the real cost of 2,500 ppm — The fee-scaled caps count the 95 bps creator fee as a manipulation cost, but the coin creator and cashback traders get that fee back. We did not run the held-basket sim to prove a profit at the larger PumpSwap caps.
- **open_fee_mark uses 0 both as no mark and as a zero fee** — `shortwin-math::open_fee_mark` · seen in 3/3 runs · NEW — Code smells: `open_fee_mark(mark, phi_ppm)` returns `phi_ppm` when `mark == 0`, and an open at `phi_ppm = 0` writes 0 (the test asserts `open_fee_mark(2_500, 0) == 0`); the next open at a higher fee then replaces the low-water mark (`open_fee_mark(0, 10_000) == 10_000`); `carried_phi(0, ·)` reads 0 as no mark; today `fee_size_cap(x0, 0, x100)` returns `Some(0)`, so an open at φ = 0 fails `CapPositionImpact` — open_fee_mark uses 0 both for no mark and for a zero fee, so a later open erases a zero-fee mark. A zero-fee venue makes this reachable, and the fee-scaled cap blocks it today.
- **pump_curve picks the fee tier with a fixed 10¹⁵ supply** — `shortwin-math::pump_curve` · seen in 3/3 runs · NEW — Code smells: `pump_curve` computes `mcap = mul_div(vq, PUMP_CURVE_MCAP_SUPPLY, vt, Rounding::Floor)` with a fixed supply of 10¹⁵; the pump program picks the curve tier at `⌊vQ·mint.supply/vT⌋` with the live mint supply (ARCHITECTURE.md §4.2 "an open item"); any holder can burn tokens, so the math's `mcap` is at or above pump's and picks an equal or cheaper tier when a threshold lies between the two; B̂ then falls below the venue charge (I13) — A holder who burns tokens makes pump_curve quote a cheaper tier than pump charges, because pump_curve uses a fixed 10¹⁵ supply. Today pump's curve fee table has one tier, so no quote differs yet.
- **Open Raydium positions follow a governance change of pool_lock_min_bps** — `shortwin-math::ref_depth_cap` · seen in 1/3 runs · NEW — Code smells: `ref_depth_cap` takes `λ_min` from its caller; ExitParams and the locked exit-bound group of the position (state/position.rs, A26 "never changes") have no `λ_min` field; engine/bounds.rs reads `config.params.pool_lock_min_bps` at exit time; `apply_config` applies a Params change to every market at once, while the kink syncs only when `open_positions == 0`; `pool_lock_min_bps` ∈ [2,000, 10,000], so a change from 5,000 to 2,000 moves the cap of open Raydium positions from 2D to 5D; ECON §12.12 (12)(f)(v) measured 3D and 5D caps on PumpSwap at −31.1 % and −52.3 % of Q in S1 — When governance lowers pool_lock_min_bps, the exit reference cap of every open Raydium position rises with it, because positions do not lock λ_min. We did not simulate the Raydium S1 case.
- **A zero depth cap makes ref_liquidity judge the bound on L_live alone** — `shortwin-math::ref_depth_cap` · seen in 2/3 runs · NEW — Code smells: `ref_depth_cap(D, 0)` and `ref_depth_cap(0, λ)` both return 0 through `depth_at_lambda`; `ref_liquidity` then computes `credited.min(0) = 0` and `L_b = L_live`, which drops `L_ema'` and `L_slow`; a zero cap protects less than `None` (the crate test gives `Ok(50)` at `Some(0)` and `Ok(900)` with no cap); the siblings fail closed on the same inputs (`conservative_x0` returns `DivByZero` for λ_min 0); the compiled `PoolLockMinBps` bounds (2,000, 10,000) block it — A zero depth cap makes ref_liquidity use L_live alone, so an LP withdrawal loosens the exit bound, while sibling functions fail closed. A λ_min of 0, or a D that floors to 0, makes this reachable.
- **On PumpSwap, an LP withdrawal inside the exit raises B̂_ref and max_in** — `shortwin-math::ref_liquidity` · seen in 1/3 runs · NEW — Code smells: `ref_liquidity` returns `l_live.max(min(max(L_ema', L_slow), cap))`; on PumpSwap the program passes `cap = ref_depth_cap(D, PUMPSWAP_LOCK_MIN_BPS) = D ≤ L_live`, so `L_b = L_live`; `L_live` is the pre-trade `√(X·Y)`, which an unlocked LP lowers to D inside the exit transaction (`[withdraw, liquidate, deposit]`); the doc says "an LP pull in a quiet second cannot loosen the bound", which holds on Raydium but not on PumpSwap; example: a third-party deposit E = D and q = 2.47 % of x(D) raise B̂_ref by about 1.27 % after the drain, and max_in and the penalty base `min(B̂_live,risk, B̂_ref)` rise by the same factor — On PumpSwap a keeper who withdraws unlocked liquidity inside the liquidation raises B̂_ref, max_in and the penalty base, because L_b equals L_live there. We did not prove that the keeper profits.
- **refuted reads one corner, but the curve quote peaks inside the box** — `shortwin-math::refuted` · seen in 2/3 runs · NEW — Code smells: `refuted` takes `quotes.cost(bx.p_hi, bx.l_lo)` as the box maximum; on a curve market `B̂_str` differs by about 0.02 % from the curve quote at the crossing `rT(P) = Q` (straddle.rs doc, F-A2), so the highest cost in `[P_lo, P_hi]` can lie near the crossing, inside the box — refuted reads cost only at (P_hi, L_lo), so it can refute while a point near the curve crossing costs about 0.02 % more. We did not show a changed outcome.
- **A withdrawal inside sol_settle blocks the σ part of the refutation** — `shortwin-math::refuted` · seen in 2/3 runs · NEW — Code smells: `refuted` tests `quotes.sigma(bx.p_hi, l_live.min(bx.l_lo)) ≤ σ_max`; its doc considers only a deposit ("an atomic LP deposit cannot raise `min(L_live, ·)`"); a withdrawal earlier in the same transaction lowers `L_live`, raises σ and makes `refuted` false, so a live clock is not disarmed and keeps its old `count`; with the live-leg drain of `s1_legs` and `s2_legs`, only the box corners guard S1/S2 — An unlocked LP who withdraws inside sol_settle raises σ at the refutation corner, so evaluate keeps an arm that the honest pool refutes. We did not prove a profit for the LP.
- **s1_legs reads one box corner, but the curve quote is not monotone** — `shortwin-math::s1_legs` · seen in 1/3 runs · NEW — Code smells: `s1_legs` reads `quotes.cost(bx.p_lo, bx.l_hi)` as the lowest cost in the box; on a curve market `BoxQuotes::cost` is `CurvePricing::buy_repriced` → `curve_straddle`, which straddle.rs calls "Not monotone at q = rT"; `B̂_str` is up to about 0.02 % below the curve quote where `rT(P, L)` crosses q, so the lowest cost can lie inside the box; `CurvePricing::liquidation_price` searches the two regimes separately, and `s1_legs` does not — s1_legs tests the S1 budget at one corner, but on a curve market the cost can be about 0.02 % lower inside the box. We did not prove a state where this flips S1.
- **A live fee above the locked hard cap blocks the buyback and S1** — `shortwin-math::s1_legs` · seen in 3/3 runs · NEW — Code smells: `risk::buyback_allowed`, `max_in_liq` and `max_in_exp` read `B̂_live` at the live fee `φ_live`; `s1_legs` reads `b_live`, the robust-spot quote and the box corner at `φ_risk = min(φ_live, venue_fee_hard_cap_bps_pos)`; when `φ_live` is above the locked cap, the band `B̂_risk + ert ≤ V − K_s < B̂_exec + ert` fails both checks; `s3_no_route` reads the global cap, so it stays false while `φ_live` is at or below the global cap; S2 reads the fee-free σ; example: V 10 SOL, K_s 0.11 SOL, locked cap 500 bps and `φ_live` 700 bps give `B̂_risk` 9.765 ≤ 9.89 and `B̂_exec` 9.951 > 9.89; the `s1_legs` doc says `ert` leaves "no band where neither a buyback nor S1 is possible" — With the live fee above the locked hard cap, the buyback fails at the live fee and S1 fails at the capped fee. We did not prove that a venue fee reaches that band, or how long it stays there.
- **An LP withdrawal inside sol_settle removes the live veto on S1** — `shortwin-math::s1_legs` · seen in 3/3 runs · NEW — Code smells: `s1_legs` reads `budget.exceeded_by(b_live)` and `quotes.cost(p_rs, l_live)` on the live reserves and the raw `L_live`; `programs/shortwin/src/engine/arming.rs` passes `l_live = state.liquidity()`, which an LP withdrawal earlier in the same transaction lowers; FITS-2 moved only the CR spot leg to `bounds::spot_liquidity` (`max(L_live, L_ema')`); after a `[withdraw, sol_settle, deposit]` bundle only the box corner `(P_lo, L_hi)` vetoes S1; example: V 1,000, K_s 20, honest `b_live` 900 (buyback affordable), drained `b_live` 1,100 and robust-spot cost 1,050, both above 980, so S1 pays lenders 980 and the borrower 0 — An unlocked LP who withdraws inside sol_settle makes the live and robust-spot S1 legs true, and only the box corner then vetoes S1. We did not prove that the box corner holds while the honest pool can pay the buyback.
- **s2_legs reads σ at one corner, but curve σ dips inside the box** — `shortwin-math::s2_legs` · seen in 1/3 runs · NEW — Code smells: `s2_legs` reads σ only at `(P_lo, L_hi)`; for q > rT, σ is the pool's n over the curve's P (arming.rs `sigma_of`); at the edge `rT(P, L) = q` the quote n drops by about 0.02 %, so σ can be about 2 bps lower inside the box than at the corner — s2_legs tests σ at one corner, but on a curve market σ can be lower inside the box at the straddle edge. We did not prove a state where this flips S2.
- **An LP withdrawal inside sol_settle removes the live veto on S2** — `shortwin-math::s2_legs` · seen in 2/3 runs · NEW — Code smells: `s2_legs` reads `sigma_live` and `quotes.sigma(p_rs, l_live)` on the live reserves and the raw `L_live`; a withdrawal earlier in the same transaction raises both above `σ_max`, because σ falls with `L`; FITS-2's `max(L_live, L_ema')` floor is not applied here; only `σ(P_lo, L_hi)` at the box corner stays robust; S2 is capped at FV, unlike S1 — An unlocked LP who withdraws inside sol_settle makes the live and robust-spot S2 legs true, so only the box corner can stop S2. We did not prove that the box corner holds then.
- **s3_no_route reads the global hard cap, not the position's locked cap** — `shortwin-math::s3_no_route` · seen in 3/3 runs · NEW — Code smells: `s3_no_route(executable, route_broken, phi_ppm, hard_cap_bps)` gets the global `venue_fee_hard_cap_bps` (R3A-4 c); every risk quote, `B̂_ref` and the S1 legs use the position's locked cap; `pool_fail` reads the same global cap; a raised global cap removes the S3 exit for a fee between the two caps, and a lowered global cap starts S3 while the position still quotes at its old cap; example: locked cap 500 bps, global cap 1,500 bps, venue fee 1,000 bps — the buyback costs about 1.048·B̂_ref, above the 1.02 liquidation bound, S1 reads the 5 % quote, S2 reads the fee-free σ, and 100,000 ≤ 150,000 ppm keeps S3 false — After governance raises the global hard cap, a venue fee between the two caps blocks the buyback, S1 and S3. We did not prove that a real venue fee reaches that band.
- **settlement pays at the EMA point without a check against both corners** — `shortwin-math::settlement` · seen in 1/3 runs · NEW — Code smells: `settlement` checks `amount_ok(lo.x, hi.x, fv_dev_bps)` at `(P_lo, L_hi)` and `(P_hi, L_lo)` only, then pays `x(P_ema, L_ema)`; nothing checks `lo.x ≤ ema.x ≤ hi.x`, and `amount_ok` accepts any `x_hi < x_lo`; the sim's `_settle` raises `InvariantError("S2/S3 amount outside the box")` (I-E5, ±2 lamports), and the Rust code has no such check; on a curve market FV is not monotone, because `curve_straddle` dips by about 0.02 % where `rT(P)` crosses q — settlement pays the S2/S3 amount at the EMA point and does not check that it lies between the two corners. We did not prove a gap above about 0.02 % of the take.
- **With penalty_bps below expiry_fee_bps, the S2/S3 take exceeds both box corners** — `shortwin-math::settlement` · seen in 3/3 runs · NEW — Code smells: `pprime` picks `expiry_fee_bps` where the position is expired and CR ≥ L, else `penalty_bps`, decided at each state; `settlement` checks `amount_ok(lo.x, hi.x)` at `(P_lo, L_hi)` and `(P_hi, L_lo)`, then pays `at(p_ema, l_ema)`; `amount_ok` passes any negative difference (`x_hi.saturating_sub(x_lo)`); the params bounds allow `TierPenaltyBps` (0–2,000) below `ExpiryFeeBps` (0–500) with no ordering rule; example: V 12,500,000,000, L 12,500, K_s 135,000,000, penalty 0 and expiry fee 200 give x_lo 10,149,000,000 and x_hi 10,000,000,001, `amount_ok` passes, and the EMA point pays 10,200,000,000, which is 51,000,000 lamports above the higher corner — When governance sets penalty_bps below expiry_fee_bps, the S2/S3 take at (P_ema, L_ema) can exceed both box corners, and amount_ok still passes. The path needs governance to choose that order for new positions, and no params rule forbids it.
- **S1 credits a take above market value at once, with no vest** — `shortwin-math::settle` · seen in 1/3 runs · NEW — Code smells: `waterfall::settle` sets `lump = 0` for S1 and pays `lenders = V − K_s` at once; the S1 legs prove only `B̂ + ert > V − K_s` at the cheapest box corner, so `V − K_s` can exceed `⌊Q·P_ema/WAD⌋ + R_due` by up to that corner's impact plus fee; example: Q·P_ema 100 SOL, q 2.5 % of X, fee 0.25 % and K_s 0.25 SOL — S1 holds for V below 103.07 SOL and pays up to about 2.3 SOL above market value plus rent; ECON §9.4 keeps S1 immediate because S1 "pays V − K_s < B̂ + ert, a loss", which compares with B̂ and not with the market value; R4B-6 vests the same excess for S2/S3 against a token whale's [deposit → sol_settle → withdraw → claim_sol] — A lender who deposits just before an S1 and withdraws after takes a share of the take above market value, because S1 vests nothing. We did not prove that the pool's lock and allowance rules let that lender withdraw at once.
- **A pushed P_ema moves part of an S2/S3 take into immediate credit** — `shortwin-math::settle` · seen in 2/3 runs · NEW — Code smells: `waterfall::settle` vests `lump = x − min(x, ⌊q·p_ema/WAD⌋ + r_due)` and reads P_ema directly; when FV exceeds `V − K_s` at both box corners, both corner amounts equal the take, so `amount_ok` passes for any box width and nothing bounds the P_ema that splits the take; one attacker-only second lifts P_ema by up to 16.7 % (dt 1, C 100 %, `price_up_bound`); example: take 9.89 SOL, `⌊q·P/WAD⌋` 7.0 SOL, R_due 0.2 SOL — the lump falls from 2.69 to 1.52 SOL, and the immediate credit grows by 1.17 SOL — A pushed P_ema moves part of an S2 or S3 take from the vesting lump into immediate credit for lenders who hold shares now. We did not prove that a new lender can withdraw soon after.
- **wallet_ok limits one wallet, so a borrower with many wallets passes it** — `shortwin-math::wallet_ok` · seen in 2/3 runs · NEW — Code smells: `(q_open + q)·10⁴ ≤ wallet_cap_bps·OI_cap` reads the `q_open` of one wallet; nothing links wallets, and no doc line accepts that a borrower can split opens across wallets — wallet_ok limits the open quantity of one wallet only, so a borrower with several wallets opens past wallet_cap_bps. We did not check what the program does about this.

---

> ⚠️ This review was performed by an AI assistant. AI analysis can never verify the complete absence of vulnerabilities and no guarantee of security is given. Team security reviews, bug bounty programs, and on-chain monitoring are strongly recommended. For a consultation regarding your projects' security, visit [https://www.pashov.com](https://www.pashov.com)
