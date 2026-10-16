# 🔐 Security Review — short.win-5-math-core

---

## Scope

|  |  |
| --- | --- |
| **Mode** | filename |
| **Files reviewed** | `./crates/shortwin-math/src/lib.rs` · `./crates/shortwin-math/src/pool.rs` · `./crates/shortwin-math/src/observe.rs`<br>`./crates/shortwin-math/src/params.rs` |
| **Confidence threshold (1-100)** | 75 |
| **Passes** | 3 |
| **Memory** | 0 records before this scan · 45 after · `55b3e23` |

---

## Findings

[85] **1. One-lamport sweeps hold vest_end six hours away**

`shortwin-math::vest_in` · Confidence: 85 · seen in 3/3 runs · NEW

**Description**
Any caller who sweeps one lamport each second holds vest_end six hours away, so lenders who exit after six hours forfeit 37% of the lump.

**Fix (Option A — allow-and-handle: round the span down)**:

```diff
-        mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        mul_div(num, 1, add_u128(bal, x)?, Rounding::Floor)?
```

**Fix (Option B — ban-path: a dust lump keeps vest_end)**:

```diff
-        mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        // A dust lump vests on the running schedule: vest_end stays where it is.
+        let gap = u128::from(lump_vest_s).saturating_sub(left);
+        if ovf(x.checked_mul(gap))? < add_u128(bal, x)? {
+            left
+        } else {
+            mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        }
```

**Fix (Option C — restrict: a minimum lump size)**:

```diff
-    let span: u128 = if p.vest_bal != 0 && p.vest_end > now {
+    let running = p.vest_bal != 0 && p.vest_end > now;
+    let span: u128 = if running && lamports < MIN_VEST_LUMP_LAMPORTS {
+        // A lump below the minimum joins the running vest and leaves vest_end alone.
+        u128::try_from(ovf(p.vest_end.checked_sub(now))?).map_err(|_| MathError::Underflow)?
+    } else if running {
```

---

[75] **2. A dust deposit before the last settlement skips the full-wipe vest flush**

`shortwin-math::maybe_roll_in` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
An attacker deposits even one raw token before the last settlement, so maybe_roll_in skips the full wipe and the attacker takes the old lenders' vest.

**Fix (Option A — validate: a dust floor counts as a full wipe)**:

```diff
     let claims = p.token_claims()?;
-    let (num, k) = if claims == 0 {
+    // Claims at or below a dust floor are a full wipe: a dust deposit before the
+    // last settlement cannot turn the wipe into a rebase that keeps the vest unflushed.
+    let (num, k) = if claims <= WIPE_DUST_CLAIMS {
         // The wiped epoch's lenders get the unreleased vest.
         release_in(p, now)?;
```

**Fix (Option B — restrict: refuse a deposit while every token is lent out)**:

```diff
     let minted = shares_for_tokens(p, amount)?;
     if minted == 0 {
         return Err(MathError::DepositTooSmall);
     }
+    // A deposit must not join an epoch whose every token is borrowed: its claim
+    // would turn the full wipe at the last settlement into a rebase.
+    if p.tokens_free == 0 && p.borrowed_q > 0 {
+        return Err(MathError::InvalidInput);
+    }
```

**Fix (Option C — allow-and-handle: flush the vest at every roll)**:

```diff
     } else {
         let cap = ovf(R_MAX.checked_mul(add_u128(claims, VIRTUAL_ASSETS)?))?;
         if shares_vs(p)? <= cap {
             return Ok(None);
         }
+        // A rebase closes the epoch too: its shares get the unreleased vest, so the
+        // shares minted after the rebase do not take it. Gate the next deposit on the
+        // post-roll sweeps as R4A-7 does after a full wipe.
+        release_in(p, now)?;
+        let vb = p.vest_bal;
+        p.vest_bal = 0;
+        if vb != 0 {
+            credit_pos(p, i128::from(vb), true)?;
+        }
         let mut k: u8 = 1;
```

---

[75] **3. A one-transaction lender takes swept SOL that belongs to protocol_fees**

`shortwin-math::withdraw` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
An attacker deposits, sweeps and withdraws in one transaction on a market with no lenders, so the last-lender flush pays it SOL meant for protocol_fees.

**Fix (Option A — restrict: pay only the vest older than the deposit)**:

```diff
     if shares == p.total_shares && p.vest_bal != 0 {
         let vb = p.vest_bal;
         p.vest_bal = 0;
-        credit_pos(p, i128::from(vb), true)?;
+        // Pay the last lender only the vest that existed before its latest
+        // deposit (recorded at deposit); the rest goes to protocol_fees.
+        let owed = vb.min(l.vest_at_deposit);
+        credit_pos(p, i128::from(owed), true)?;
+        let rest = sub_u64(vb, owed)?;
+        v.lender_sol = sub_u64(v.lender_sol, rest)?;
+        v.protocol_fees = add_u64(v.protocol_fees, rest)?;
         touch_in(p, v, l, records, now)?;
     }
```

**Fix (Option B — ban-path: the unreleased vest goes to protocol_fees)**:

```diff
     if shares == p.total_shares && p.vest_bal != 0 {
         let vb = p.vest_bal;
         p.vest_bal = 0;
-        credit_pos(p, i128::from(vb), true)?;
-        touch_in(p, v, l, records, now)?;
+        // The leaving lender keeps only the release time has earned (release_in above);
+        // the unreleased vest has no lender left and goes to protocol_fees (R3A-10).
+        v.lender_sol = sub_u64(v.lender_sol, vb)?;
+        v.protocol_fees = add_u64(v.protocol_fees, vb)?;
     }
```

---

[75] **4. Later locks never decay while an earlier lock holds a low b_min**

`shortwin-math::withdraw` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
Another live lock, the lender's own lock or a rebase residue keeps lock_norm above 0, so new locks keep the old b_min and never decay.

**Fix (Option A — validate: a decay base per lock)**:

```diff
@@ withdraw_in
-    if new_lock != 0 && v.lock_norm == 0 {
-        v.b_min = p.borrowed_q;
-    }
+    // Each lock keeps its own decay base: ΣB_Q at the time the lock is set
+    // (or a Π index per lock cohort), so a lock set at a higher ΣB_Q decays
+    // when ΣB_Q falls below its own level.
+    if new_lock != 0 {
+        l.lock_b_min = p.borrowed_q;
+    }
@@ resolve_in
-    if v.lock_norm > 0 && bn < v.b_min {
+    // Decay each lock against its own base (l.lock_b_min, or its cohort's Π index),
+    // not only when ΣB_Q falls below the b_min of the first lock.
+    if v.lock_norm > 0 && bn < v.b_min {
```

**Fix (Option B — allow-and-handle: re-base on a higher lock)**:

```diff
-    if new_lock != 0 && v.lock_norm == 0 {
+    // A lock added while ΣB_Q is above b_min re-bases Π and b_min (and rescales the
+    // live locks), so each lock decays from the borrowed amount when it started.
+    if new_lock != 0 && (v.lock_norm == 0 || p.borrowed_q > v.b_min) {
         v.b_min = p.borrowed_q;
     }
```

**Fix (Option C — validate: count live locks, not lock_norm)**:

```diff
-    if new_lock != 0 && v.lock_norm == 0 {
+    // A rebase residue left in lock_norm is not a live lock: reset b_min from a
+    // live-lock count (or remove a lender's term when its conversion floors lock to 0).
+    if new_lock != 0 && v.live_locks == 0 {
         v.b_min = p.borrowed_q;
     }
```

**Fix (Option D — restrict: test lock_norm after the lender's own term leaves; covers the lender's own re-lock only)**:

```diff
     let new_lock = add_u128(cur, add)?.min(remaining);
-    if new_lock != 0 && v.lock_norm == 0 {
-        v.b_min = p.borrowed_q;
-    }
     if l.lock_epoch == v.pi_epoch && cur != 0 {
         // A live lock leaves its bucket (an expired one's bucket is already gone).
         cal_sub(v, l.lock_until, l.lock_term)?;
     }
+    // Test lock_norm only after the lender's own live term has left the calendar,
+    // so a lender that re-locks does not keep its own old, low b_min.
+    if new_lock != 0 && v.lock_norm == 0 {
+        v.b_min = p.borrowed_q;
+    }
```

---

[75] **5. Repeated lock eras drive Π to 1, so later locks never decay**

`shortwin-math::withdraw` · Confidence: 75 · seen in 2/3 runs · NEW

**Description**
withdraw resets b_min at each lock era but keeps Π, so repeated borrows and repays drive Π to 1 and later locks never decay.

**Fix (Option A — allow-and-handle: reset Π with b_min)**:

```diff
     if new_lock != 0 && v.lock_norm == 0 {
         v.b_min = p.borrowed_q;
+        // A new lock era: no live lock reads the old Π, so Π starts again at WAD.
+        v.pi = WAD;
     }
```

**Fix (Option B — allow-and-handle: roll the Π epoch, ARCH §9 M0b note 1)**:

```diff
     if new_lock != 0 && v.lock_norm == 0 {
+        // Roll the Π epoch whenever lock_norm reaches 0.
+        v.pi_epoch = ovf(v.pi_epoch.checked_add(1))?;
+        v.pi = WAD;
+        v.lock_cal = [0; LOCK_BUCKETS];
         v.b_min = p.borrowed_q;
     }
```

---

[70] **6. A Raydium price start sets L_slow above the locked depth**

`shortwin-math::new_raydium` · Confidence: 70 · seen in 3/3 runs · NEW

**Description**
An attacker wraps a Raydium price start in a pool deposit, so new_raydium sets L_slow up to twice the locked depth D that adoption enforces.

---

Findings List

| # | Confidence | Title |
|---|---|---|
| 1 | [85] | One-lamport sweeps hold vest_end six hours away |
| 2 | [75] | A dust deposit before the last settlement skips the full-wipe vest flush |
| 3 | [75] | A one-transaction lender takes swept SOL that belongs to protocol_fees |
| 4 | [75] | Later locks never decay while an earlier lock holds a low b_min |
| 5 | [75] | Repeated lock eras drive Π to 1, so later locks never decay |
| | | **Below Confidence Threshold** |
| 6 | [70] | A Raydium price start sets L_slow above the locked depth |

---

## Leads

_Vulnerability trails with concrete code smells where the full exploit path could not be completed in one analysis pass. These are not false positives — they are high-signal leads for manual review. Not scored._

- **accrue refuses any credit while a wipe record is live** — `shortwin-math::accrue` · seen in 1/3 runs · NEW — Code smells: `accrue`, `true_up` and `repay`'s `vest_in` call `credit_in` with `None` for the wipe record; with S = 0 and `wipe_live` set, `wipe.ok_or(MathError::InvalidInput)` refuses; only "ΣB_Q = 0 at a full wipe, and `open_short` needs S > 0" keeps the lender credit at 0 — accrue refuses any credit while a wipe record is live. A future path that keeps a position open through a full wipe makes every Market-writing prologue fail.
- **pool::borrow lends tokens while total_shares is 0** — `shortwin-math::borrow` · seen in 2/3 runs · NEW — Code smells: `borrow` checks `q > 0`, the release horizon and `q ≤ tokens_free`, but not `total_shares > 0`; `tokens_for_shares` floors, so dust `T` can stay after the last exit; S = 0 with ΣB_Q > 0 then sends every `accrue` credit through `credit_in` to `lender_sol −= x`; only `open_short`'s `total_shares > 0` check stops it (ARCH §9 M0b note 4) — The math borrow accepts a loan while S is 0, so a caller without open_short's check creates the state where every rent credit underflows lender_sol. Only the program refuses it today.
- **cal_advance clamps lock_norm at 0 and returns no error** — `shortwin-math::cal_advance` · seen in 3/3 runs · NEW — Code smells: `cal_advance`, `norm_at` and `cal_sub` subtract with `lock_norm.saturating_sub(..)`; nothing checks `lock_norm == Σ lock_cal` between the rolls of `maybe_roll_in` — If a future change lets lock_norm fall below the bucket sum, cal_advance clamps it at 0 and locked_total reports a wrong value. The code returns no error, and b_min then resets at the wrong time.
- **cal_sub clamps a term to its bucket and returns no error** — `shortwin-math::cal_sub` · seen in 1/3 runs · NEW — Code smells: `let x = term.min(*b); *b = sub_u128(*b, x)?; vault.lock_norm = vault.lock_norm.saturating_sub(x);`; a term larger than its bucket, or a bucket outside `lock_norm`, is clamped; a day outside the ring returns `Ok` and removes nothing; today's callers pass the lender's own `(until, term)`, and its bucket holds at least that term — cal_sub removes at most the bucket and clamps lock_norm at 0, so a term or day that does not match the calendar returns no error. A future caller with a wrong day removes another lender's term and leaves its own term in place.
- **check_sim accepts min_rent_s above max_duration_s** — `shortwin-math::check_sim` · seen in 2/3 runs · NEW — Code smells: `TierMinRentS ∈ [0, 86,400]` and `TierMaxDurationS ∈ [3,600, 7,776,000]` are checked one by one; the relational checks in `check_sim` have no `min_rent_s ≤ max_duration_s` rule; `rent::min_rent` charges `r(U)·min_rent_s` at open — The validator accepts a tier whose min_rent_s is above max_duration_s, so every borrower of that tier pays more minimum rent than the position can accrue. Only a governance update can set these values.
- **Governance can lower util_cap_bps below a market's kept kink** — `shortwin-math::check_sim` · seen in 1/3 runs · NEW — Code smells: `UtilKinkLeCap` compares only the global kink with the global `util_cap_bps`; `sync_market_params` adopts a new kink only while `open_positions == 0`, but opens read `util_cap_bps` at once — A governance update can lower util_cap_bps below the kink that a busy market keeps, so that market runs with its kink above the cap. This assumes that ECON wants the kink ≤ cap rule held for each market.
- **closable takes pending == 0 as proof that real equals claimed** — `shortwin-math::closable` · seen in 1/3 runs · NEW — Code smells: `closable` checks epoch, shares, pending, lock and fwd but not `real == claimed`; its doc says the two are equal only after the touch's surplus move (`to_pot`); `withdraw` and `claim` call it after a touch today — closable treats pending == 0 as proof that real equals claimed. A future caller that skips the touch can close a position that still holds unclaimed realised SOL.
- **commit_rems writes conv_rem before its last check** — `shortwin-math::commit_rems` · seen in 3/3 runs · NEW — Code smells: `Commit::set_rem` writes `conv_rem` into the caller's records during the replay; the `c.end != seen` check runs only at the end; a replay error maps to `Invariant` after earlier writes; the module doc says a refused call writes nothing — commit_rems writes conv_rem into the caller's records before its final check, so an Invariant error returns with changed records. The program reverts the transaction, so only an off-chain caller that keeps the records reads wrong remainders.
- **convert_steps reads Lender.fwd with no range check** — `shortwin-math::convert_steps` · seen in 3/3 runs · NEW — Code smells: `Lender.fwd` has three valid values but no reader rejects others; the accrual `match l.fwd { FWD_HOLD, FWD_OPEN, _ }` sends any other value to the plain accrual, while the `chain` test `l.fwd == FWD_NONE` and `touch_pot` treat it as a chain; `touch_tail` then returns `Invariant` — The conversion reads Lender.fwd without a range check, so a value above 2 takes the plain accrual and the chain path together and accrues an epoch twice. Only this crate writes the field today.
- **credit_in moves unpaid rent out of lender_sol while S is 0** — `shortwin-math::credit_in` · seen in 3/3 runs · NEW — Code smells: `v.lender_sol = sub_u64(v.lender_sol, x)?` on rent that `accrue` holds only as `receivable`; `v.protocol_fees = sub_u64(v.protocol_fees, x)?` on a `true_up` shortfall after `sweep_protocol_fees` (any caller) empties the fees; both fields are `u64` where `pool_model` is signed; only the program's `LastSharesBorrowed` and `open_short` checks keep S = 0 with ΣB_Q > 0 away — With S at 0, an accrue credit above lender_sol makes every Market instruction fail, and a fee sweep makes a shortfall exit fail. Only program checks keep S = 0 with open borrows away.
- **deposit trusts the caller's rewards_required flag** — `shortwin-math::deposit` · seen in 2/3 runs · NEW — Code smells: `deposit_in` builds the post-wipe gate from the caller's `rewards_required`; with `false` it checks only `WIPE_TRADER_SWEPT`; nothing in `Pool` records the venue kind; the program derives the flag from the venue kind at deposit time, which can change between the wipe and the deposit; M6-SEC-5 was this bug class once (`PumpMigrating` did not require the bit); today `engine::lender::rewards_required` sets the flag for every pump venue — deposit trusts the caller's rewards_required flag, so a path that passes false lets the first depositor take the cashback of wiped lenders. We found no such path today.
- **Ring::get reads a corrupted ring that check refuses** — `shortwin-math::get` · seen in 1/3 runs · NEW — Code smells: `Ring::get`, `newest`, `iter` and `len` skip `check()`; `len()` clamps `len` to `RING_LEN`; `slot()` wraps `head ≥ RING_LEN` by modulo; every current caller calls `check()` first — Ring::get returns an entry from a corrupted ring that check refuses, because head wraps by modulo and len clamps. A future caller that skips check reads the wrong entry.
- **An LP who wraps cranks with withdrawals keeps ema.guard set** — `shortwin-math::integrate` · seen in 1/3 runs · NEW — Code smells: `integrate` clears `ema.guard` only when `L_ema·10⁴ ≤ L_mlo·(10⁴ + REF_GUARD_TOL_BPS)`; `L_mlo` is the lower median (index 3 of 8) of the buckets' `llo`, and `record` keeps `llo` as the minimum of the second; one `[Raydium withdraw, crank_price, deposit]` in 4 of every 8 seconds sets `L_mlo` to the locked depth D; with `L_ema` seeded from honest highs H > 1.01·D the guard never clears; `guarded_ema_liquidity` and the `L_slow` input then read D; at λ = 50 %, H = 2D and q = 2.5 % of x_H, B̂ is 2.63 % higher at D, above δ_liq = 2 %; ARCH §2.6 rejected a lows bound for this effect — An unlocked LP who removes liquidity around a crank in half of the seconds keeps ema.guard set, so the exit bound reads the locked depth. We did not prove that this goes beyond the accepted FITX-1 residual.
- **integrate steps an Ema with price or liquidity at 0** — `shortwin-math::integrate` · seen in 2/3 runs · NEW — Code smells: `Ema` derives `Default` (all zero); `Ema::new` rejects 0, but `integrate` does not check `ema.price` or `ema.liq`; with P_ema = 0, `aggregate` gives lower = upper = 0, so the observation is 0 and `weighted(0, τ, 0, dt) = 0`; the ring then pushes `reprice(0, 0) = (0, 0)`; only the program's `priced` checks (`ring_count > 0`, `ema_price > 0`) keep a zeroed state out — integrate accepts an Ema at price or liquidity 0 and keeps it at 0, because the ±C clamp around 0 is 0. Only the program's priced check keeps a zeroed state out.
- **is_fresh treats a last bucket from the future as fresh** — `shortwin-math::is_fresh` · seen in 1/3 runs · NEW — Code smells: `LastBucket::is_fresh` computes `now.saturating_sub(self.sec) <= max_age_s`; a bucket with `sec > now` gives a negative age and reads as fresh, so `robust_spot` returns `min(p_live, lb_lo)`; the program passes `market_now = max(clock, last_accrual_ts)`, which only moves forward; `claim_sol`, `cover_deficit` and `sync_lender` use the raw clock but read no observation — is_fresh treats a last bucket whose sec is after now as fresh. A future caller that passes an earlier clock reads that bucket as the current reference.
- **is_full_wipe and ratio disagree on a record with num 0** — `shortwin-math::is_full_wipe` · seen in 1/3 runs · NEW — Code smells: `is_full_wipe` returns `num == 0` and ignores `den_log1000`; `ratio` accepts only `(0, 0)` or `(1, k)`; `top_up` and the program's gates use `is_full_wipe`, while `convert_steps` and `fwd_walk` use `ratio` — top_up accepts a record with num 0 and den_log1000 above 0, which convert_steps then refuses. A future writer of such a record leaves parked lenders unable to convert.
- **lock_of and withdraw accept a vault with Π at 0** — `shortwin-math::lock_of` · seen in 2/3 runs · NEW — Code smells: `Vault::new` sets Π = WAD, but no reader rejects Π = 0; `lock_of` returns ⌈lock·0/lock_pi⌉ = 0 for every lock, and `lock_pi = 0` gives `DivByZero`; `withdraw_in`'s `mul_div(new_lock, WAD, v.pi, …)` returns `DivByZero`; `resolve_in` keeps Π at 0; only `create_market`'s write of `NEW_VAULT_PI` defends — A zeroed Vault has Π at 0, so every lock reads as 0 and a withdrawal that forms a lock fails with DivByZero. Only the program's initialisation writes Π today.
- **max_release keeps the release time of closed positions** — `shortwin-math::lock_until_for` · seen in 3/3 runs · NEW — Code smells: `borrow` sets `p.max_release = p.max_release.max(release_at)`; neither `close_pos`, `repay`, `settle` nor a Π-epoch roll lowers it; `lock_until_for` reads `max(now, max_release) + lock_grace_s` for every new or grown lock; a max-duration open at `min_notional`, closed at once, costs about the 250,000-lamport open fee plus `R_min`; the field doc says "of any position opened so far" — A borrower who opens and closes one long position keeps max_release at its release time, so every later lock lasts until then. The field doc counts every position opened, and we did not measure the cost to lenders.
- **A zeroed ShareEpoch reads as a full wipe** — `shortwin-math::ratio` · seen in 3/3 runs · NEW — Code smells: `ShareEpoch::ratio` maps `(num, den_log1000) == (0, 0)` to a full wipe `(0, 1)`, the value of zeroed memory; `s_old` is documented as `> 0` but no reader checks it, and `top_up` on a zeroed record divides by `s_old = 0`; `convert_steps` then sets `shares`, `lock` and `lock_term` to 0 — A zeroed ShareEpoch passes ratio() as a full wipe, so the conversion sets the lender's shares to 0. Only the program's discriminator and PDA checks stop this today.
- **record_idx accepts a ShareEpoch by its epoch field alone** — `shortwin-math::record_idx` · seen in 2/3 runs · NEW — Code smells: `convert`, `touch`, `touch_pot`, `deposit`, `withdraw`, `claim`, `cover` and `fwd_walk` find a record only by `r.epoch == epoch` at offset `epoch − first.epoch`; they take `acc_end`, `acc_r_end`, `num`, `den_log1000` and `conv_rem` as given; the math `ShareEpoch` has no market key; the conversion writes `conv_rem` back into that record — record_idx matches a ShareEpoch by epoch only, so a caller without the program's PDA key check can pass a fake record with a larger acc_end and claim SOL. Only the program's PDA key check stops this.
- **release_in fails while a vest runs with no shares** — `shortwin-math::release_in` · seen in 1/3 runs · NEW — Code smells: `release_in` returns `MathError::Invariant` when `rel != 0` and `p.total_shares == 0`; `pool::accrue` calls it first, and the program prologue runs `pool::accrue` in every instruction that writes `Market`; `deposit_in` also releases first; only the withdraw flush and the full-wipe flush in `maybe_roll_in` keep the vest empty when S reaches 0 — release_in fails while a vest runs with no shares, so a future path to S = 0 blocks every instruction that writes Market. A deposit cannot repair the state, because it also releases first.
- **The Graduation rewrite sets L_ema and L_slow with no cap** — `shortwin-math::rewrite_liquidity` · seen in 3/3 runs · NEW — Code smells: `VenueChange::Graduation` sets `(liq, liq_slow) = (l_new, l_new)` and every ring entry's `l`, `llo` and `lhi` with no cap, no `min` with the old `L_slow` and no `mark_restart`; `RaydiumLocked` never raises `L_slow` above `slow_cap`; today the only caller, `crank_price::redetect`, passes the bonding curve's `√(X·Y)`, which curve trades do not change — rewrite_liquidity with VenueChange::Graduation sets L_ema and L_slow to the raw sample with no cap and no warm-up. A future caller that passes an AMM pool sample lets one transaction raise both values.
- **A deposit just before settle takes SOL at P_ema at once** — `shortwin-math::settle` · seen in 3/3 runs · NEW — Code smells: `deposit_in` counts a borrowed token 1:1 with a free token; `settle` calls `true_up`, which credits `immediate − credited` to `acc` and `immediate` to `acc_r` at once, with `immediate ≤ ⌊Q·P_ema/WAD⌋ + R_due`; only the lump and the rent top-up vest; the test `rent_top_up_vests_like_a_lump` gives a same-second depositor 650·9/10 of the immediate part; deposit and `sol_settle` are open to any caller; ECON R4B-6 treats `⌊Q·P_ema⌋` as fair value — A depositor who joins just before settle receives its share of the immediate ⌊Q·P_ema/WAD⌋ at once, so it trades tokens for SOL at P_ema. We did not prove that P_ema exceeds the price the depositor pays for the tokens.
- **The first steady bucket after the warm-up carries the whole gap weight** — `shortwin-math::steady_step` · seen in 3/3 runs · NEW — Code smells: `warm_step` leaves `liq_slow` and `slow_ts` unchanged (`..*ema`) for all 8 warm-up integrations; the first steady step uses `dts = min(now − slow_ts, slow_dtmax_s)`, 60 s at the defaults (weight 60/3,660 against 1/3,601 per second); at the compiled bounds (τ_slow = 600, slow_dtmax_s = 600) that bucket weighs 50 %; while the guard is set the input is `min(lobs, L_mlo)` — The warm-up leaves slow_ts unchanged, so the first steady bucket after it moves L_slow with up to slow_dtmax_s seconds of weight. We did not prove that an attacker who owns that bucket gains from it.
- **One flat price step sets sustained_growth to 0 during a pump** — `shortwin-math::sustained_growth` · seen in 1/3 runs · NEW — Code smells: `sustained_growth` returns 0 when any of the newest `kappa_growth_n` steps has `new.p <= old.p`; a second with a pushed low below `P_ema` and an honest high above it aggregates to `P_ema`, so the step is flat; `kappa_eff` then falls to `kappa_base_bps` (0 by default) and `liq_reference` gives `min(P_ema, lb_lo)`; in a pump faster than about 34 bps/s, `P_ema` lags spot by more than `delta_liq_bps`; one push every 3 s (the default `kappa_growth_n`) keeps g at 0; KI-05 prices a veto that pays every second — A borrower who records one low price every kappa_growth_n seconds keeps sustained_growth at 0, so keepers cannot liquidate it during a fast pump. We did not measure the cost against the KI-05 figures.
- **sweep sets the wipe gate bit of any source the caller names** — `shortwin-math::sweep` · seen in 1/3 runs · NEW — Code smells: `if p.total_shares == 0 && p.is_wipe_live() { p.wipe_swept |= source.bit(); }` runs for any `SweepSource` the caller names, also when `lamports == 0`; nothing in the math links `SweepSource::VenueRewards` to a reward claim that happened; today `sweep_trader` sends a 0-lamport `VenueRewards` sweep only when `rewards_clear` holds — sweep sets the wipe gate bit of the SweepSource that its caller names, also for 0 lamports. A caller that names VenueRewards in error opens the deposit gate before the cashback arrives.
- **top_up identifies a ShareEpoch by its epoch alone** — `shortwin-math::top_up` · seen in 3/3 runs · NEW — Code smells: `top_up` accepts any record with `rec.is_full_wipe()` and `rec.epoch + 1 == pool.share_epoch`; it does not compare `s_old` or a market key; the math `ShareEpoch` has no `market` field; only the PDA check in `sweep_trader` and `sol_settle` binds the record — top_up accepts any full-wipe record whose epoch is share_epoch − 1, so a record of another market passes the math checks. Only the program's PDA checks bind the record today.
- **top_up lowers acc_r_end for a negative realised credit** — `shortwin-math::top_up` · seen in 3/3 runs · NEW — Code smells: `credit_pos` allocates realised SOL only under `realised && lamports > 0`; `top_up` runs `rec.acc_r_end = to_unsigned(add_i128(to_i128(rec.acc_r_end)?, d)?)?` for either sign; a parked lender's `real` then falls below 0 and `to_unsigned(real)` fails; today `vest_in` passes `u64` amounts and no caller sends a realised debit — The public credit treats a realised debit two ways: with shares it leaves acc_r alone, and on a live wipe record it lowers acc_r_end. A future caller that sends one lowers the real balances of parked lenders.
- **touch_pot reports Behind for a current lender with a forward chain** — `shortwin-math::touch_pot` · seen in 2/3 runs · NEW — Code smells: `let (st, held) = if l.fwd == FWD_NONE { .. } else { (Standing::Behind, true) }` never checks `l.epoch == pool.share_epoch`; `convert` can return Current with `fwd = FWD_OPEN` still set; `touch_tail(Behind, held)` then skips the expired-lock drop and `clear_fwd`; `closable` needs `l.fwd == FWD_NONE` — touch_pot returns Behind for a current lender that convert left with a forward chain, so the expired lock and the chain stay. Today sync_lender calls touch after convert, so no account keeps that state.
- **Each third-party touch floors a current lender's accrual** — `shortwin-math::touch_pot` · seen in 3/3 runs · NEW — Code smells: `touch_tail` adds `accrued(l.shares, pool.acc - l.snap)` and the same for `real`, each a floor, then sets `snap = acc`; a parked lender's `convert_steps` floors `accrued(l.shares, rec.acc_end − l.snap)` once per call too; the unsigned `sync_lender` reaches both for any lender; `fwd_walk` uses `⌊q·end⌋ − ⌊q·snap⌋` and loses nothing; KI-68 discloses it — Any caller can send sync_lender for a current or parked lender after each credit, and each call floors away up to 1 lamport of its accrual. Each call costs the caller more in fees than the lender loses.
- **touch_tail keeps an expired lock from an older Π epoch** — `shortwin-math::touch_tail` · seen in 1/3 runs · NEW — Code smells: `if l.lock != 0 && now >= l.lock_until && l.lock_epoch == v.pi_epoch` clears an expired lock only in the current Π epoch; after a Π epoch roll the old lock stays above 0 until withdraw rewrites it; `closable` reads `l.lock == 0`; only the invariant shares ≥ lock keeps a lender with no shares closable — touch_tail clears an expired lock only in the current Π epoch, so a lock from an older epoch stays above 0 and closable reads it. A future path that sets only shares to 0 leaves a position that cannot close.
- **pool::touch writes shared conv_rem with no owner-only marker** — `shortwin-math::touch` · seen in 3/3 runs · NEW — Code smells: `touch`, `convert`, `claim`, `cover`, `withdraw` and `deposit` run `convert_in` and then `commit_rems`, which writes `conv_rem` into the shared `ShareEpoch` records; only `touch_pot` takes `&[ShareEpoch]`; KI65-REV-SEC1 allows only the owner to convert through a rebase record; only the program's signer-derived lender PDAs and `sync_lender`'s `RebaseNeedsOwner` post-check enforce it — A future instruction that calls pool::touch for another user's lender lets that caller order rebase conversions and take their rounding units. Only owner-signed instructions call it today.
- **true_up allocates realised rent to nobody while S is 0** — `shortwin-math::true_up` · seen in 2/3 runs · NEW — Code smells: with S = 0, `true_up` adds `immediate` to `lender_sol`, but `credit_r_in(p, immediate)` returns at once (`p.total_shares == 0`); nothing sends the amount to `unmatched`; departed lenders keep `pending > real − claimed` and `claim` pays only from the pot; the books balance only because `accrue` moved `credited` earlier; `pool_model._credit_r` does the same; only the program's `LastSharesBorrowed` keeps S = 0 with ΣB_Q > 0 away — With S at 0, true_up keeps the realised rent in lender_sol but credits it to no lender, so departed lenders cannot claim it. The program blocks this state today.
- **Warm-up integrations count toward settlement persistence** — `shortwin-math::warm_step` · seen in 2/3 runs · NEW — Code smells: `warm_step` does `count = ema.count + 1` like `steady_step`, and the `Ema.count` doc says the settlement persistence counts it; `settle::evaluate` executes once `ema_count − state.count ≥ persist_n` and returns early while `executable && !warmed`, which leaves `ArmState.last` unchanged; `is_live` keeps the clock live for `arm_gap_s`; the 8 warm-up integrations end about 9 s after an adoption — A settlement clock armed before a pool switch counts the 8 warm-up integrations, so a keeper executes S1 after one new-pool evaluation. We did not prove that the clock stays live across a real fast-path adoption.
- **A pushed price start keeps P_ema high after the warm-up** — `shortwin-math::warm_step` · seen in 1/3 runs · NEW — Code smells: `warm_step` clamps the seed with `.max(price_down_bound(ema.price, dt, params.clamp_bps)?)`; after `Observation::new_raydium`, `ema.price` is the one spot sample `p` of the start transaction; with a 10× push and honest buckets at lo = P, P_ema is 4.985·P after 8 integrations at dt = 1 (2.914·P at dt = 2) and needs 24 (10) more steady steps to come within 10 % of spot; `pump_amm.rs` says a pushed spot seeds nothing an open reads before eight integrations; before M6-SEC-1 the first warm step dropped the pushed value — An attacker who pushes the spot price in the price-start transaction keeps P_ema several times too high after the warm-up, so opens stay refused longer. We did not find a path that profits from it.
- **One recorded low pulls the warm-up seed below the documented floor** — `shortwin-math::warm_step` · seen in 3/3 runs · NEW — Code smells: the `integrate` doc promises `≥ ×0.4986 over 8 integrations`, which is (11/12)⁸ at dt = 1; `warm_step` clamps each step only at `price_down_bound(ema.price, dt, clamp_bps)`, which is 6/7 at dt = 2 (the default `ema_dtmax_s`); the low stays in the running minimum until the gate; one dt = 2 step gives 0.466 (the crate test asserts 0.46–0.48), eight give 0.291 — An attacker records one low in the warm-up, and with 2-s integrations the seed falls to 0.29 of the price, not 0.4986. We did not prove that a settlement reads the low seed before it recovers.
- **A zeroed Ema passes warmed and skips the reference guard** — `shortwin-math::warmed` · seen in 1/3 runs · NEW — Code smells: `warm == 0` means confirmed and `guard == 0` means no reference guard; `Ema::new` writes the same zero bytes that a zeroed account holds — A zeroed Ema passes warmed and skips the reference guard, because 0 is the confirmed value of warm and of guard. A path that leaves the EMA fields unwritten lets opens read them at once.
- **The last-lender burn keeps carry and carry_r at zero shares** — `shortwin-math::withdraw` · seen in 1/3 runs · NEW — Code smells: `maybe_roll_in` moves `carry` and `carry_r` to `dust` and sets them to 0 when S changes; `withdraw_in` takes `p.total_shares` to 0 and leaves both unchanged; the documented range `[0, S)` breaks at S = 0; the first `credit_pos` or `credit_r_in` after the next deposit divides the old remainder among the new shares; `pool_model.withdraw` does the same — The last-lender burn leaves carry and carry_r in place at S = 0, so the next depositor receives the old lenders' rounding remainder. maybe_roll_in moves the same remainder to dust.
- **Math withdraw lets S reach 0 while tokens are borrowed** — `shortwin-math::withdraw` · seen in 3/3 runs · NEW — Code smells: `withdraw_in` accepts `shares == p.total_shares` while `p.borrowed_q > 0`; a burn for 0 tokens passes `amount > allow` when `allow = 0`; in `pool_model` the last lender burns S − 1 shares (the lock caps at the 1 remaining share) and then the last share for 0 tokens, which leaves S = 0 with ΣB_Q = 1; `accrue`, `credit_in` and `true_up` then take the S = 0 branch; only the program's `LastSharesBorrowed` check (M1-SEC-3) refuses it; the keeper, the WASM build and the fuzz call `pool::withdraw` directly — The math withdraw lets the last lender burn every share while tokens are lent out, so a later rent credit can underflow lender_sol. Only the program's LastSharesBorrowed check stops this.

---

> ⚠️ This review was performed by an AI assistant. AI analysis can never verify the complete absence of vulnerabilities and no guarantee of security is given. Team security reviews, bug bounty programs, and on-chain monitoring are strongly recommended. For a consultation regarding your projects' security, visit [https://www.pashov.com](https://www.pashov.com)
