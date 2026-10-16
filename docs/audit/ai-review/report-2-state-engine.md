# 🔐 Security Review — short.win-2-state-engine

---

## Scope

|  |  |
| --- | --- |
| **Mode** | filename |
| **Files reviewed** | `./programs/shortwin/src/state/config.rs` · `./programs/shortwin/src/state/epoch.rs` · `./programs/shortwin/src/state/exposure.rs`<br>`./programs/shortwin/src/state/layout_asserts.rs` · `./programs/shortwin/src/state/lender.rs` · `./programs/shortwin/src/state/market.rs`<br>`./programs/shortwin/src/state/mod.rs` · `./programs/shortwin/src/state/oracle.rs` · `./programs/shortwin/src/state/pod.rs`<br>`./programs/shortwin/src/state/position.rs` · `./programs/shortwin/src/state/vault.rs` · `./programs/shortwin/src/state/venue_extras.rs`<br>`./programs/shortwin/src/state/venue_guard.rs` · `./programs/shortwin/src/constants.rs` · `./programs/shortwin/src/errors.rs`<br>`./programs/shortwin/src/events.rs` · `./programs/shortwin/src/lamports.rs` · `./programs/shortwin/src/mint_filter.rs`<br>`./programs/shortwin/src/engine/arming.rs` · `./programs/shortwin/src/engine/bounds.rs` · `./programs/shortwin/src/engine/caps.rs`<br>`./programs/shortwin/src/engine/guard.rs` · `./programs/shortwin/src/engine/lender.rs` · `./programs/shortwin/src/engine/mod.rs`<br>`./programs/shortwin/src/engine/observe.rs` · `./programs/shortwin/src/engine/pool_select.rs` · `./programs/shortwin/src/engine/prologue.rs`<br>`./programs/shortwin/src/engine/route.rs` · `./programs/shortwin/src/engine/tier.rs` · `./programs/shortwin/src/engine/trader.rs`<br>`./programs/shortwin/src/engine/waterfall.rs` · `./programs/complete_curve/src/lib.rs` |
| **Confidence threshold (1-100)** | 75 |
| **Passes** | 3 |
| **Memory** | 0 records before this scan · 39 after · `55b3e23` |

---

## Findings

[85] **1. Many 1-lamport sweeps in one transaction keep moving vest_end later**

`shortwin_math::vest_in` · Confidence: 85 · seen in 2/3 runs · NEW

**Description**
Any caller sends 1-lamport sweeps, and vest_in rounds each new end up a full second, so lenders who exit forfeit more of the vest.

**Fix (Option A — restrict: round the weighted end down)**:

```diff
 // crates/shortwin-math/src/pool.rs, vest_in
         let num = add_u128(
             ovf(bal.checked_mul(left))?,
             ovf(x.checked_mul(u128::from(lump_vest_s)))?,
         )?;
-        mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        // A lump smaller than the vest balance no longer buys a full second (R4B-5).
+        mul_div(num, 1, add_u128(bal, x)?, Rounding::Floor)?
```

**Fix (Option B — allow-and-handle: keep vest_end for a sub-second shift)**:

```diff
 // crates/shortwin-math/src/pool.rs, vest_in
         let bal = u128::from(p.vest_bal);
-        let num = add_u128(
-            ovf(bal.checked_mul(left))?,
-            ovf(x.checked_mul(u128::from(lump_vest_s)))?,
-        )?;
-        mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        // A lump whose exact shift x·(lump_vest_s − left)/(bal + x) is below 1 s keeps vest_end.
+        let shift = ovf(x.checked_mul(u128::from(lump_vest_s).saturating_sub(left)))?;
+        if shift < add_u128(bal, x)? {
+            left
+        } else {
+            let num = add_u128(
+                ovf(bal.checked_mul(left))?,
+                ovf(x.checked_mul(u128::from(lump_vest_s)))?,
+            )?;
+            mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        }
```

---

[75] **2. A rebase roll leaves the unreleased vest to a dust depositor**

`shortwin_math::maybe_roll` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
An attacker deposits one token unit before the last SOL settlement, so maybe_roll skips the vest flush and a later dust deposit takes the vest.

**Fix**

```diff
 // crates/shortwin-math/src/pool.rs, maybe_roll_in
     } else {
         let cap = ovf(R_MAX.checked_mul(add_u128(claims, VIRTUAL_ASSETS)?))?;
         if shares_vs(p)? <= cap {
             return Ok(None);
         }
+        // A rebase leaves the old lenders a tiny part of S: pay them the unreleased vest first,
+        // as the full wipe does ("the wiped lenders keep their unreleased lumps", ECON §9.5).
+        release_in(p, now)?;
+        let vb = p.vest_bal;
+        p.vest_bal = 0;
+        if vb != 0 {
+            credit_pos(p, i128::from(vb), true)?;
+        }
         let mut k: u8 = 1;
```

---

[75] **3. accrue floors each lender rent credit and drops the remainder**

`shortwin::accrue` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
Any caller who sends crank_price each second makes accrue floor every lender credit, and no exit adds the remainder back, so lenders lose rent.

**Fix**

```diff
 // crates/shortwin-math/src/rent.rs
-pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3]) -> Result<u64, MathError> {
-    dot_div(aggr.a, *delta, 1, RENT_DEN_BPS, Rounding::Floor)
+/// Carries `Σ A_j·δJ_j mod RENT_DEN_BPS` into the next accrual, as `credit_pos` does with `carry`.
+pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3], carry: &mut u128) -> Result<u64, MathError> {
+    let n = aggr
+        .a
+        .iter()
+        .zip(delta)
+        .try_fold(U256::from_u128(*carry), |acc, (&a, &d)| {
+            acc.checked_add(U256::from_product(a, d))
+        })
+        .ok_or(MathError::Overflow)?;
+    let (q, rem) = n.div_rem_u128(RENT_DEN_BPS).ok_or(MathError::DivByZero)?;
+    *carry = rem;
+    q.to_u64().ok_or(MathError::Overflow)
 }
 // programs/shortwin/src/engine/prologue.rs, accrue
-    let credit = rent::lender_credit(&market.aggr(), &delta).math()?;
+    // `rent_credit_carry`: a new u128 Market field (from `_reserved`), so no accrual drops its remainder.
+    let mut carry = market.rent_credit_carry;
+    let credit = rent::lender_credit(&market.aggr(), &delta, &mut carry).math()?;
+    market.rent_credit_carry = carry;
```

---

[75] **4. apply_config installs a new governance key without its signature**

`shortwin::apply_config` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
apply_config writes the new governance and pause keys without their signatures, so one wrong governance key stops every later config change.

**Fix (Option A — validate: the new keys sign apply_config)**:

```diff
 // programs/shortwin/src/instructions/apply_config.rs
 pub struct ApplyConfig<'info> {
+    /// `Authorities` only: the new governance and pause keys sign, so they can sign later changes.
+    pub new_governance: Option<Signer<'info>>,
+    pub new_pause_authority: Option<Signer<'info>>,
     #[account(
 ...
     match &change {
-        ConfigChange::Authorities { fee_recipient, .. } => {
+        ConfigChange::Authorities { governance, pause_authority, fee_recipient, .. } => {
+            let g = ctx
+                .accounts
+                .new_governance
+                .as_ref()
+                .ok_or_else(|| error!(ShortwinError::Unauthorized))?;
+            require_keys_eq!(g.key(), *governance, ShortwinError::Unauthorized);
+            let p = ctx
+                .accounts
+                .new_pause_authority
+                .as_ref()
+                .ok_or_else(|| error!(ShortwinError::Unauthorized))?;
+            require_keys_eq!(p.key(), *pause_authority, ShortwinError::Unauthorized);
             let recipient = ctx
```

**Fix (Option B — two-step: the new keys accept)**:

```diff
 // programs/shortwin/src/state/config.rs, GlobalConfig::apply
             ConfigChange::Authorities {
                 governance,
                 pause_authority,
                 coverage_authority,
                 fee_recipient,
             } => {
-                self.governance = *governance;
-                self.pause_authority = *pause_authority;
+                // An `accept_authorities` instruction that each new key signs moves these in.
+                self.pending_governance = *governance;
+                self.pending_pause_authority = *pause_authority;
                 self.coverage_authority = *coverage_authority;
                 self.fee_recipient = *fee_recipient;
             }
```

---

[75] **5. A dust deposit into an empty market takes the trader cashback**

`shortwin::deposit` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
deposit skips the sweep gate on an empty market, so an attacker deposits dust, sweeps, withdraws and takes SOL that KI-10 gives the protocol.

**Fix**

```diff
 // crates/shortwin-math/src/pool.rs, deposit_in
-    if p.total_shares == 0 && p.is_wipe_live() {
+    // KI-10 / R3A-10: SOL owed to the trader while S = 0 is the protocol's; sweep it first (R4A-7).
+    if p.total_shares == 0 {
         let need = if rewards_required {
             WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
         } else {
             WIPE_TRADER_SWEPT
         };
         if p.wipe_swept & need != need {
             return Err(MathError::WipeSweepPending);
         }
-        // V2-12: the full-wipe record is final from now on; a parked depositor converts.
-        p.wipe_live = 0;
-        touch_in(p, v, l, records, now)?;
+        if p.is_wipe_live() {
+            // V2-12: the full-wipe record is final from now on; a parked depositor converts.
+            p.wipe_live = 0;
+            touch_in(p, v, l, records, now)?;
+        }
     }
 // crates/shortwin-math/src/pool.rs, sweep
-    if p.total_shares == 0 && p.is_wipe_live() {
+    if p.total_shares == 0 {
         p.wipe_swept |= source.bit();
     }
 // crates/shortwin-math/src/pool.rs, withdraw_in
     p.total_shares = sub_u128(p.total_shares, shares)?;
+    if p.total_shares == 0 {
+        // The next deposit waits for the post-exit sweeps, as after a full wipe.
+        p.wipe_swept = 0;
+    }
```

---

[75] **6. Anyone trips the venue latch with a one-byte ExtendProgram**

`shortwin::flag_venue_upgraded` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
Any caller extends a venue ProgramData by one byte, which moves its deploy slot, so flag_venue_upgraded blocks opens on every market of that venue.

**Fix (Option A — validate: compare the ELF bytes, not the slot)**:

```diff
 // programs/shortwin/src/instructions/flag_venue_upgraded.rs, handler
     let slot = read_deploy_slot(&ctx.accounts.program_data)?;
+    // ExtendProgram moves the deploy slot but only appends zero bytes: compare the ELF itself
+    // (sha256 of the data after the header, trailing zero bytes trimmed).
+    let elf = elf_hash(&ctx.accounts.program_data)?;
 ...
         if g.upgraded & bit == 0 {
-            require!(slot != guard_slot, ShortwinError::VenueNotUpgraded);
+            require!(
+                slot != guard_slot && elf != g.guard_elf_hash[i],
+                ShortwinError::VenueNotUpgraded
+            );
             g.upgraded |= bit;
 // programs/shortwin/src/instructions/revalidate_venue.rs, handler
+    // Record the reviewed ELF, so a later ExtendProgram alone cannot set the latch again.
+    g.guard_elf_hash[i] = elf_hash(&ctx.accounts.program_data)?;
```

**Fix (Option B — restrict: a trusted key flags)**:

```diff
 // programs/shortwin/src/instructions/flag_venue_upgraded.rs
 pub struct FlagVenueUpgraded<'info> {
+    /// ExtendProgram lets anyone move the deploy slot, so only a trusted key flags an upgrade.
+    pub pause_authority: Signer<'info>,
+    #[account(
+        seeds = [GlobalConfig::SEED],
+        bump = config.load()?.bump,
+        constraint = config.load()?.pause_authority == pause_authority.key()
+            @ ShortwinError::Unauthorized
+    )]
+    pub config: AccountLoader<'info, GlobalConfig>,
     #[account(
         mut,
         seeds = [VenueGuard::SEED],
```

---

[75] **7. A deposit ends the full-wipe record before the curve cashback arrives**

`shortwin::rewards_required` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
rewards_required returns false on a market that left the pump curve, so a depositor ends the wipe record and takes the wiped lenders' cashback.

**Fix (Option A — validate: every market with a pump UVA)**:

```diff
 // programs/shortwin/src/engine/lender.rs
 pub fn rewards_required(market: &Market) -> bool {
-    matches!(
-        market.venue_kind(),
-        Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
-    )
+    // Curve-era cashback stays in the trader's pump UVA after the market leaves the curve.
+    market.pump_uva != Pubkey::default()
+        || matches!(
+            market.venue_kind(),
+            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
+        )
 }
 // programs/shortwin/src/instructions/sweep_trader.rs
-        let pump = matches!(
-            kind,
-            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
-        );
+        // Read the pump UVA, and set `wipe_rewards_claimed`, on every market that has one.
+        let pump = m.pump_uva != Pubkey::default()
+            || matches!(
+                kind,
+                Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
+            );
```

**Fix (Option B — restrict: graduated-elsewhere markets)**:

```diff
 // programs/shortwin/src/engine/lender.rs
 pub fn rewards_required(market: &Market) -> bool {
-    matches!(
-        market.venue_kind(),
-        Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
-    )
+    market.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE)
+        || matches!(
+            market.venue_kind(),
+            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
+        )
 }
 // programs/shortwin/src/instructions/sweep_trader.rs
-        let pump = matches!(
-            kind,
-            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
-        );
+        // Read the trader's pump UVA on graduated-elsewhere markets too.
+        let pump = m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE)
+            || matches!(
+                kind,
+                Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
+            );
```

---

Findings List

| # | Confidence | Title |
|---|---|---|
| 1 | [85] | Many 1-lamport sweeps in one transaction keep moving vest_end later |
| 2 | [75] | A rebase roll leaves the unreleased vest to a dust depositor |
| 3 | [75] | accrue floors each lender rent credit and drops the remainder |
| 4 | [75] | apply_config installs a new governance key without its signature |
| 5 | [75] | A dust deposit into an empty market takes the trader cashback |
| 6 | [75] | Anyone trips the venue latch with a one-byte ExtendProgram |
| 7 | [75] | A deposit ends the full-wipe record before the curve cashback arrives |

---

## Leads

_Vulnerability trails with concrete code smells where the full exploit path could not be completed in one analysis pass. These are not false positives — they are high-signal leads for manual review. Not scored._

- **complete_curve passes 21 unchecked accounts to pump** — `complete_curve::complete_curve` · seen in 1/3 runs · NEW — Code smells: 21 `UncheckedAccount`s (`global`, the fee recipients, `creator_vault`, the curve ATAs, `associated_base_user`, `user_volume_accumulator`, `fee_program`, `event_authority` and others) go to `buy_v2` with a `/// CHECK` comment only; the helper checks the pump program address, the curve PDA and its owner, and the `user` signer; it holds no funds and writes no short.win state — complete_curve relies on pump to check those accounts. Only the signer's own funds depend on them, and we did not read the checks in pump.
- **The no-op path of complete_curve accepts a curve that is not complete** — `complete_curve::complete_curve` · seen in 1/3 runs · NEW — Code smells: the no-op path returns `Ok` with `CurveAlreadyComplete` when `curve.complete || curve.real_token_reserves == 0`; the buy path requires `after.complete && after.real_token_reserves == 0` — complete_curve returns Ok for a curve with zero real tokens that is not complete, while the buy path refuses that state. We did not find a pump state that reaches it.
- **complete_curve forwards the signer flag of every trailing account to pump** — `complete_curve::complete_curve` · seen in 3/3 runs · NEW — Code smells: `metas.push(meta(info, info.is_writable, info.is_signer))` keeps the signer flag of the caller for every remaining account (lib.rs:312); the 27 named accounts give pump only `user` as a signer; pump `buy_v2` needs no second signer — pump receives every extra signature in the transaction through the trailing accounts. Today buy_v2 uses none, so only a future pump upgrade can act with it.
- **complete_curve fails on curve flag bytes that it does not use** — `complete_curve::complete_curve` · seen in 2/3 runs · NEW — Code smells: the handler uses only `real_token_reserves` and `complete`; `shortwin_venue_state::pump::read_curve` returns `Malformed` when `is_mayhem_mode`, `is_cashback_coin`, `can_edit_creator_fee` or `is_holder_reward` is not 0 or 1, so the handler fails with `BadCurve` — A pump layout change that writes another value into one of those bytes makes the straddle helper fail for every caller. No such change exists today.
- **Π can fall to 1 and stop the decay of lender locks** — `shortwin_math::resolve` · seen in 1/3 runs · NEW — Code smells: `resolve` sets `Π ← ⌈Π·B'/b_min⌉` with no floor; in the pool model, three deep borrow and return cycles with a dust position that keeps ΣB_Q > 0 bring Π to 1; after that a later lock does not shrink even when 99.9999 % of its borrow comes back — A borrower who keeps a dust position open can drive Π to 1, so lender locks stop their decay until lock_until. We did not measure the delay to lenders.
- **Box corners that take a different p' make sol_settle wait** — `shortwin_math::settlement` · seen in 1/3 runs · NEW — Code smells: `amount_at` decides p' at each box corner (`pprime`); an expired position with V_eff·10⁴/cr_liq in [B̂(P_lo,L_hi), B̂(P_hi,L_lo)) gets the expiry fee at one corner and the penalty at the other; that gap, (penalty − expiry fee)·B̂, is 300 bps for Tier A and 800 bps for Tier D at the defaults, above `fv_dev_bps` 100, so `amount_ok` fails and `settlement` returns None; the settle.rs docs state the per-corner p' but not this wait — While the box spans the liquidation CR of an expired position, sol_settle waits on its S2 or S3. We did not measure how long a real box stays across that threshold, and add_margin ends the wait.
- **A position that a pool switch moves onto Raydium keeps its pump ert** — `shortwin::adopt` · seen in 2/3 runs · NEW — Code smells: `ShortPosition.ert` is documented as "Effective `exec_rent_tol` (0 on Raydium)"; `open_short` locks `ert = venues::effective_ert(PumpCurve, exec_rent_tol)` on a curve market; `pool_select::adopt` moves a `None` market with open positions to a Raydium pool and does not rewrite the positions; `bounds::close_max_in`, `max_in_liq`, `max_in_exp` and the S1 budget subtract `pos.ert`; the Raydium adapters never use `leg.ert` — Every exit budget of a carried position stays up to 10,000,000 lamports too small. We did not prove that this costs the borrower or the lenders value.
- **adopt keeps the route-broken flag after a pool change** — `shortwin::adopt` · seen in 2/3 runs · NEW — Code smells: `adopt` clears `no_route_since`, `pool_fail_since` and the window, but leaves `MARKET_FLAG_ROUTE_BROKEN` set; `route::s3_no_route` and `guard::open_static` read that flag at every later observation; no doc says the flag must survive a pool change; only `set_route_broken` clears it — The S3 chain restarts at once on the adopted pool, and the new pool refuses opens, while the Emergency flag stays set. We did not prove a loss, because S3 needs a liquidatable or expired position.
- **A Raydium price start lets L_slow reach twice the depth** — `shortwin::adopt` · seen in 1/3 runs · NEW — Code smells: a priced Raydium adoption sets `L_slow = min(L_slow, D)` (`RaydiumLocked{slow_cap: d}`); a PumpSwap start caps it at D, and `refresh_locked_depth` caps it again during the warm-up; `initial_raydium` (the unpriced branch of `adopt`, and `create_market`) sets `L_slow = min(√(X·Y), ⌊D·10⁴/λ_min⌋)`, which is 2·D at the default `pool_lock_min_bps` 5,000; no later step caps it at D; `conservative_x0` and the tier `R_slow` read `L_slow` — An LP deposit wrapped around a Raydium listing raises L_slow to 2·D, while a priced adoption caps it at D. We did not prove that an open gains caps from it.
- **A pending EnableVenues change undoes disable_venue at once** — `shortwin::apply_config` · seen in 3/3 runs · NEW — Code smells: `disable_venue` writes only `venue_enabled &= !mask` and leaves `pending` as it is; `GlobalConfig::apply` runs `venue_enabled |= *bits`; any caller can send `apply_config` once `now ≥ not_before`; `EnableVenues` is not in `GOVERNANCE_ONLY_CANCEL`, so only an Emergency `cancel_config` before or in the same transaction stops it — Any caller applies a pending EnableVenues change that names a bit the Emergency just cleared, so opens resume on that venue with no new timelock. We did not prove a loss, and the Emergency can disable the venue again at once.
- **candidate_tier reads the live candidate reserve at execute_pool_switch** — `shortwin::candidate_tier` · seen in 3/3 runs · NEW — Code smells: `FitAt::Execution` drops the live `X_cand`, but `candidate_tier` still passes `r_now: a.state.r`; `tier::liquidity_lamports` takes `min(R_now, R_slow, locked·10⁴/λ_min)`, so a sale below `P_ema` makes `R_now` the minimum; `execute_pool_switch` cancels the window on any fit miss; the doc of `FitAt::Execution` says the execution uses terms that no transaction can move — An attacker who sells into the candidate pool around execute_pool_switch lowers the tier and cancels the window, and can repeat it. The default tiers A to C have equal impact caps, so we did not prove an effect today.
- **carried_fit judges the switch at the live candidate price** — `shortwin::carried_fit` · seen in 3/3 runs · NEW — Code smells: `carried_fit` evaluates at `window_price(p_ref, a)` = `min(P_ema, P_cand)` with the live `P_cand`, also at `FitAt::Execution`; a sale in the same transaction lowers `P_cand`, and with it `y_slow` and the tier in `candidate_tier`; a buy that lifts `P_cand` to `P_ema` removes the lower-price evaluation; the doc of `FitAt::Execution` says the execution uses terms that no transaction can move — An attacker who trades the candidate pool around execute_pool_switch moves the price at which the fit is judged, so the attacker can cancel or force a switch. We did not prove harm under the default tier table.
- **propose_config accepts a fee_recipient that apply_config always refuses** — `shortwin::check_change` · seen in 1/3 runs · NEW — Code smells: `propose_config` checks the `Authorities` fee_recipient by key only (`check_authorities`: not the default key, pairwise distinct); `check_fee_recipient` (System-owned, not executable) runs only in `apply_config`; only one change can be pending at a time — Governance can propose a fee_recipient that apply_config always rejects, so a key rotation in the same change needs a new proposal and a new timelock. Governance can cancel the change and propose again.
- **An Authorities change fails when the stored parameter set no longer validates** — `shortwin::check_change` · seen in 2/3 runs · NEW — Code smells: for `Authorities`, `Timelock`, `EnableVenues` and `OracleFeed`, `param_set_after` returns `self.param_set()`, and `check_change` runs `validate_param_set` on the stored set at propose and at apply; the apply_config docs say that "an upgrade could have changed the rules"; each Params change validates the whole set, so a set that fails two components at once cannot be repaired one change at a time — Parameters that an upgrade made invalid block every key rotation. We did not prove that a planned upgrade tightens a bound.
- **check_change compares util_cap_bps with the config kink, not the market kinks** — `shortwin::check_change` · seen in 3/3 runs · NEW — Code smells: `param_set_after` checks `UtilKinkLeCap` against `self.kink.util_kink_bps` only; `sync_market_params` keeps an old `market.util_kink_bps` while positions are open; `rent::accrue` reads `market.util_kink_bps` — Governance can lower `util_cap_bps` below the old kink of a market with open positions, so slope2 never applies on that market. We did not measure the rent that lenders lose.
- **check_fee_recipient accepts the trader PDA of a market** — `shortwin::check_fee_recipient` · seen in 3/3 runs · NEW — Code smells: `check_fee_recipient` requires only `owner == system_program::ID && !executable`; the `trader` PDA, the `trader_wsol` address and a ShareEpoch address that does not exist yet are System-owned with no data, so all pass; the doc comment says that the check refuses every short.win account — If Governance installs a trader PDA as fee_recipient, sweep_protocol_fees pays it and the next sweep books the protocol fees as lender income. This needs a Governance error.
- **close_trader_ata sends the trader PDA signature to an unchecked token program** — `shortwin::close_trader_ata` · seen in 3/3 runs · NEW — Code smells: `close_trader_ata` checks `*ata.owner == TOKEN_PROGRAM_ID` but sends the CPI to `token_program.key()` with the trader PDA seeds; `open_wsol` and `close_wsol` use the constant `TOKEN_PROGRAM_ID`; `sweep_trader` pins `address = TOKEN_PROGRAM_ID` today — A future caller that does not pin `token_program` gives the trader PDA signature to any program. The only caller pins it today, and we did not check the anchor-spl 1.2.0 instruction builder.
- **create_pda_account signs with the PDA seeds for an unchecked system program** — `shortwin::create_pda_account` · seen in 1/3 runs · NEW — Code smells: the `transfer`, `allocate` and `assign` CPIs go to `system_program.key()` from a raw `&AccountInfo` with the ShareEpoch PDA seeds; the helper does not compare that key with `system_program::ID`; `sol_settle` passes `Program<System>` today — A future caller that passes an unchecked account gives the ShareEpoch PDA signature to another program, which can then take the record address. Today the only caller passes the System program.
- **create_pda_account returns the swept lamports but does not check where they went** — `shortwin::create_pda_account` · seen in 2/3 runs · NEW — Code smells: `create_pda_account` (engine/arming.rs) moves the pre-funded lamports of the new PDA to `sweep_to` and returns the amount for the caller to book; it does not check that `sweep_to` is the market vault or differs from `payer`; the only caller, `sol_settle`, passes `market_vault` today — A future caller that passes another `sweep_to` books lender SOL that the vault never received. Today the only caller passes the market vault.
- **Market::is_pda and ShortPosition::is_pda accept any stored bump** — `shortwin::is_pda` · seen in 1/3 runs · NEW — Code smells: `Market::is_pda` and `ShortPosition::is_pda` call `create_program_address` with the stored `bump` and do no canonical check; today the bump comes only from `ctx.bumps` at init; `load_records` has the same pattern — A future writer of `bump` lets a second address pass as the same market or position. Today every bump comes from init.
- **The exits do not check ShortPosition.status** — `shortwin::is_pda` · seen in 2/3 runs · NEW — Code smells: `POSITION_OPEN = 0` is the zero value and `POSITION_EXIT_IN_PROGRESS = 1` is reserved for v1.1; only `add_margin` checks `status == POSITION_OPEN`; `close_short`, `liquidate`, `expire` and `sol_settle` bind the position only through `has_one` and `ShortPosition::is_pda` — A v1.1 position in ExitInProgress can still be closed, liquidated or settled during its exit. Nothing writes that status today.
- **The open gates read VenueGuard.upgraded without a check of its version** — `shortwin::latch` · seen in 3/3 runs · NEW — Code smells: `guard::latch`, `guard::candidate_latch` and `guard::candidate_latched` read `upgraded` from a `VenueGuard` that `open_short`, `create_market`, `propose_pool` and `execute_pool_switch` load with `seeds + bump` only; `flag_venue_upgraded` and `revalidate_venue` check `VenueGuard.version` — After a later layout change moves `upgraded`, open_short reads the wrong byte and can allow opens on a venue whose upgrade nobody reviewed. No such layout change exists today.
- **load_records accepts a ShareEpoch at the bump that the record stores** — `shortwin::load_records` · seen in 2/3 runs · NEW — Code smells: `load_records` calls `create_program_address` with the stored `bump` of each record and does not check that the bump is canonical; today only `sol_settle` writes records, with the bump from `find_program_address` — A future writer that uses another bump can create two records for one epoch. Today every record has the canonical bump.
- **normalise_pdas signs as the trader PDA for an unchecked system program** — `shortwin::normalise_pdas` · seen in 1/3 runs · NEW — Code smells: `normalise_pdas`, `sweep_trader_lamports`, `TraderAccounts::open_wsol` and `pay_from_trader` build `CpiContext::new_with_signer(system_program.key(), ..)` from a raw `AccountInfo` with the trader or trader_wsol seeds; none of them compares the key with `System::id()`; every caller passes `Program<'info, System>` today — A future caller that passes a raw account gives the trader PDA signature to another program. Every caller passes the System program today.
- **open_short checks the pool_min_real_sol floor on Raydium only** — `shortwin::open_short` · seen in 3/3 runs · NEW — Code smells: `open_short` checks `r_eff ≥ pool_min_real_sol` (`PoolTooThin`) only under `kind.is_raydium()`; the PumpSwap branch checks only `disable_flags` and the warm-up; `tier::r_eff` also computes R_eff for PumpSwap (M6R-1); the `r_eff` doc and the `open_short` comment name the floor as a Raydium rule (ARCH §2.6 rule 5), and no source line says why PumpSwap needs none — A PumpSwap pool below pool_min_real_sol still takes opens. We assume that the floor is meant to apply to every venue with a locked depth.
- **ref_depth_cap reads the live pool_lock_min_bps for open positions** — `shortwin::ref_depth_cap` · seen in 3/3 runs · NEW — Code smells: `ref_depth_cap` divides `market.pool_d` by the live `config.params.pool_lock_min_bps` at each liquidation and expiry; `ShortPosition::lock_terms` locks `kappa_*`, `delta_*`, `ref_max_age_s` and `venue_fee_hard_cap_bps`, but not λ_min; a change from 5,000 to 2,000 bps raises the Raydium cap ⌊pool_d·10⁴/λ_min⌋ by 2.5×; A26 lists exit bounds as per-position terms that governance cannot reach — A timelocked Params change of pool_lock_min_bps moves the exit reference cap, and so `max_in_liq` and `max_in_exp`, of positions already open. We did not prove a loss beyond the shift of that bound.
- **A fee between the locked and global caps blocks keeper exits and S3** — `shortwin::s3_no_route` · seen in 2/3 runs · NEW — Code smells: `route::s3_no_route` and `route::pool_fail` compare the live fee with the global `config.params.venue_fee_hard_cap_bps`; `arming::inputs` and `bounds::reference` price exits at the cap locked in `pos.venue_fee_hard_cap_bps`; `liquidate::plan` compares `max_in` with the uncapped live buy; ARCH A26 says only that a change of the global cap can move S3 timing — After Governance raises venue_fee_hard_cap_bps, a live fee between the two caps makes expire fail and keeps S3 off, so expired positions stay open. We did not prove that a venue fee reaches that band.
- **Two Emergency writes remove a young working extras set from every slot** — `shortwin::set_venue_extras` · seen in 2/3 runs · NEW — Code smells: `set_venue_extras` moves `current` into `stable` only after `EXTRAS_STABLE_S` (604,800 s) or on a Governance write; the module docs of `state/venue_extras.rs` say that two quick writes cannot move a working set out of `stable` — Two Emergency writes after a working set W younger than seven days leave W in no slot. This needs a compromised or twice-mistaken Emergency key, and we did not prove that the old `stable` set then fails for exits.
- **store_records writes raw bytes without a check of the account** — `shortwin::store_records` · seen in 3/3 runs · NEW — Code smells: `store_records` writes `conv_rem` at `ShareEpoch::CONV_REM_OFFSET` (127) and checks only `is_writable`; it does not check the owner, the discriminator, the market or the PDA address; nothing ties `before` to `accounts`, so it trusts that `load_records` checked the same slice; all five callers (`deposit`, `withdraw`, `claim_sol`, `cover_deficit`, `sync_lender`) pass `ctx.remaining_accounts` to both — A future caller that passes a different slice from `load_records` writes 16 bytes into another short.win account, such as `real` and `claimed` of a `LenderPosition`. Every caller today passes the same slice.
- **sweep_base_ata trusts a token program that the caller supplies** — `shortwin::sweep_base_ata` · seen in 3/3 runs · NEW — Code smells: `base_ata_state` requires `trader_base_ata.owner == base.token_program.key()`, and the caller supplies both accounts; `sweep_base_ata` sends `transfer_checked` to `base.token_program.key()` with the trader PDA seeds; `sweep_trader` and `venues::parse` key-check both accounts today — A future caller that does not key-check both accounts gives the trader PDA signature to a program that the caller chooses. Today every caller checks them against the `Market` fields.
- **A donated cashback balance holds post-wipe deposits until someone claims it** — `shortwin::sweep_trader` · seen in 2/3 runs · NEW — Code smells: `pump_cashback_claimable` reads the raw lamports of the pump UVA above rent, and `pump_amm::cashback_claimable` reads the WSOL amount of ATA(pAMM UVA(trader), WSOL); anyone can send lamports or WSOL to either; only the venue's `claim_cashback` empties them, and shortwin signs no claim for the trader; the tests cover only organic cashback — After a full wipe on a pump market, a donation keeps WIPE_REWARDS_CLAIMED unset, so deposits wait. We did not check that any caller can always clear the donation through the claim, or that pump keeps that claim free of a signer.
- **One open position keeps a market on its old kink** — `shortwin::sync_market_params` · seen in 3/3 runs · NEW — Code smells: `sync_market_params` copies `config.kink` only when `market.open_positions == 0`; the kink has no forced adoption like `ema_params.force_at`; a borrower can open the next `min_notional` position before the last one exits; `UtilKinkLeCap` is checked on `GlobalConfig` only — A borrower who always keeps one position open keeps the market on its old kink, as A59 states. We did not prove harm, except that a later `util_cap_bps` change can fall below that old kink.

---

> ⚠️ This review was performed by an AI assistant. AI analysis can never verify the complete absence of vulnerabilities and no guarantee of security is given. Team security reviews, bug bounty programs, and on-chain monitoring are strongly recommended. For a consultation regarding your projects' security, visit [https://www.pashov.com](https://www.pashov.com)
