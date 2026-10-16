# 🔐 Security Review — short.win-1-instructions

---

## Scope

|  |  |
| --- | --- |
| **Mode** | filename |
| **Files reviewed** | `./programs/shortwin/src/lib.rs` · `./programs/shortwin/src/instructions/add_margin.rs` · `./programs/shortwin/src/instructions/apply_config.rs`<br>`./programs/shortwin/src/instructions/cancel_config.rs` · `./programs/shortwin/src/instructions/claim_sol.rs` · `./programs/shortwin/src/instructions/close_short.rs`<br>`./programs/shortwin/src/instructions/cover_deficit.rs` · `./programs/shortwin/src/instructions/crank_price.rs` · `./programs/shortwin/src/instructions/crank_sol_usd.rs`<br>`./programs/shortwin/src/instructions/create_market.rs` · `./programs/shortwin/src/instructions/deposit.rs` · `./programs/shortwin/src/instructions/disable_venue.rs`<br>`./programs/shortwin/src/instructions/execute_pool_switch.rs` · `./programs/shortwin/src/instructions/expire.rs` · `./programs/shortwin/src/instructions/flag_unsafe_mint.rs`<br>`./programs/shortwin/src/instructions/flag_venue_upgraded.rs` · `./programs/shortwin/src/instructions/initialize_config.rs` · `./programs/shortwin/src/instructions/liquidate.rs`<br>`./programs/shortwin/src/instructions/mod.rs` · `./programs/shortwin/src/instructions/open_short.rs` · `./programs/shortwin/src/instructions/propose_config.rs`<br>`./programs/shortwin/src/instructions/propose_pool.rs` · `./programs/shortwin/src/instructions/revalidate_venue.rs` · `./programs/shortwin/src/instructions/set_coverage_pause.rs`<br>`./programs/shortwin/src/instructions/set_pause.rs` · `./programs/shortwin/src/instructions/set_route_broken.rs` · `./programs/shortwin/src/instructions/set_venue_extras.rs`<br>`./programs/shortwin/src/instructions/sol_settle.rs` · `./programs/shortwin/src/instructions/sweep_protocol_fees.rs` · `./programs/shortwin/src/instructions/sweep_trader.rs`<br>`./programs/shortwin/src/instructions/sync_lender.rs` · `./programs/shortwin/src/instructions/withdraw.rs` |
| **Confidence threshold (1-100)** | 75 |
| **Passes** | 3 |
| **Memory** | 0 records before this scan · 45 after · `55b3e23` |

---

## Findings

[85] **1. flag_venue_upgraded treats a ProgramData extension as a venue upgrade**

`shortwin::flag_venue_upgraded` · Confidence: 85 · seen in 2/3 runs · NEW

**Description**
Any caller who extends a venue's ProgramData changes its deploy slot, so flag_venue_upgraded blocks open_short on that venue and revalidate_venue cannot clear the latch.

**Fix (Option A — validate: latch and revalidate on a hash of the ELF bytes)**:

```diff
 // programs/shortwin/src/instructions/flag_venue_upgraded.rs
 pub fn handler(ctx: Context<FlagVenueUpgraded>, venue: u8) -> Result<()> {
     let slot = read_deploy_slot(&ctx.accounts.program_data)?;
+    // A permissionless loader ExtendProgram also rewrites the deploy slot (Agave 4.2, KI-63).
+    // Latch only a change of the ELF bytes: compare their hash with the one revalidate_venue stored.
+    let elf_hash = elf_hash(&ctx.accounts.program_data)?; // hash of data[45..] without the zero tail
@@
         if g.upgraded & bit == 0 {
-            require!(slot != guard_slot, ShortwinError::VenueNotUpgraded);
+            require!(
+                slot != guard_slot && Some(&elf_hash) != g.guard_hash.get(i),
+                ShortwinError::VenueNotUpgraded
+            );
             g.upgraded |= bit;

 // programs/shortwin/src/instructions/revalidate_venue.rs (also in revalidate_venue)
-pub fn handler(ctx: Context<RevalidateVenue>, venue: u8, expected_slot: u64) -> Result<()> {
+pub fn handler(
+    ctx: Context<RevalidateVenue>,
+    venue: u8,
+    expected_slot: u64,
+    expected_hash: [u8; 32],
+) -> Result<()> {
     let slot = read_deploy_slot(&ctx.accounts.program_data)?;
-    require_eq!(slot, expected_slot, ShortwinError::VenueSlotMismatch);
+    // An ExtendProgram after the review moves the slot but not the ELF bytes: accept a later
+    // slot when the bytes still hash to the value the Emergency vault reviewed.
+    require_gte!(slot, expected_slot, ShortwinError::VenueSlotMismatch);
+    let elf_hash = elf_hash(&ctx.accounts.program_data)?;
+    require!(elf_hash == expected_hash, ShortwinError::VenueSlotMismatch);
@@
-        *slot_ref = expected_slot;
+        *slot_ref = slot;
+        if let Some(h) = g.guard_hash.get_mut(i) {
+            *h = elf_hash;
+        }
         g.upgraded &= !bit;
```

**Fix (Option B — restrict: a keeper that compared the ELF hash off chain signs the flag)**:

```diff
 // programs/shortwin/src/instructions/flag_venue_upgraded.rs
 pub struct FlagVenueUpgraded<'info> {
+    /// The keeper that compared the venue ELF hash off chain before it flags the venue.
+    pub keeper: Signer<'info>,
+    #[account(
+        seeds = [GlobalConfig::SEED],
+        bump = config.load()?.bump,
+        constraint = config.load()?.latch_keeper == keeper.key() @ ShortwinError::Unauthorized
+    )]
+    pub config: AccountLoader<'info, GlobalConfig>,
     #[account(
         mut,
         seeds = [VenueGuard::SEED],

 // programs/shortwin/src/instructions/revalidate_venue.rs (also in revalidate_venue)
     let slot = read_deploy_slot(&ctx.accounts.program_data)?;
-    require_eq!(slot, expected_slot, ShortwinError::VenueSlotMismatch);
+    // An extension after the review moves the slot: accept a later slot when the ELF bytes
+    // still hash to the value the Emergency vault reviewed.
+    require_gte!(slot, expected_slot, ShortwinError::VenueSlotMismatch);
+    require!(
+        elf_hash(&ctx.accounts.program_data)? == expected_hash,
+        ShortwinError::VenueSlotMismatch
+    );
```

---

[75] **2. accrue floors each lender rent credit and keeps no remainder**

`shortwin::accrue` · Confidence: 75 · seen in 1/3 runs · NEW

**Description**
Any caller who sends crank_price every second makes accrue floor each lender credit to 0, so lenders never get the rent of small positions.

**Fix**

```diff
 // crates/shortwin-math/src/rent.rs
-pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3]) -> Result<u64, MathError> {
-    dot_div(aggr.a, *delta, 1, RENT_DEN_BPS, Rounding::Floor)
+/// `rem` carries the remainder of the division (`< RENT_DEN_BPS`) to the next accrual, so a
+/// credit below 1 lamport per accrual still reaches `acc` instead of the pot.
+pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3], rem: &mut u128) -> Result<u64, MathError> {
+    // ⌊(Σ A_j·δJ_j + rem)/RENT_DEN_BPS⌋, and the new remainder
+    let (credit, r) = dot_div_rem(aggr.a, *delta, *rem, RENT_DEN_BPS)?;
+    *rem = r;
+    Ok(credit)
 }

 // programs/shortwin/src/engine/prologue.rs (accrue)
-    let credit = rent::lender_credit(&market.aggr(), &delta).math()?;
+    let mut rem = market.lender_credit_rem;
+    let credit = rent::lender_credit(&market.aggr(), &delta, &mut rem).math()?;
+    // No position open: nothing accrues against the carry, and `receivable` returns to 0 (I-P7).
+    market.lender_credit_rem = if market.open_positions == 0 { 0 } else { rem };
```

---

[75] **3. apply_config installs a new governance key that never signs**

`shortwin::apply_config` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
Any caller of apply_config installs the proposed governance, pause and coverage keys without their signatures, so one wrong key loses that role forever.

**Fix (Option A — validate: the new keys sign before apply_config writes them)**:

```diff
 // programs/shortwin/src/instructions/apply_config.rs
     pub oracle_cache: Option<Box<Account<'info, OracleCache>>>,
+    /// `Authorities` only: the new governance and pause_authority keys must sign.
+    pub new_governance: Option<Signer<'info>>,
+    pub new_pause_authority: Option<Signer<'info>>,
 }
@@
-        ConfigChange::Authorities { fee_recipient, .. } => {
+        ConfigChange::Authorities { governance, pause_authority, fee_recipient, .. } => {
+            let g = ctx.accounts.new_governance.as_ref().ok_or_else(|| error!(ShortwinError::Unauthorized))?;
+            require_keys_eq!(g.key(), *governance, ShortwinError::Unauthorized);
+            let p = ctx.accounts.new_pause_authority.as_ref().ok_or_else(|| error!(ShortwinError::Unauthorized))?;
+            require_keys_eq!(p.key(), *pause_authority, ShortwinError::Unauthorized);
             let recipient = ctx
```

**Fix (Option B — two-step: an accept instruction that the new keys sign)**:

```diff
 // programs/shortwin/src/state/config.rs (GlobalConfig::apply)
             } => {
-                self.governance = *governance;
-                self.pause_authority = *pause_authority;
+                // Two-step: the new keys take their roles only in `accept_authorities`, which they sign.
+                self.pending_governance = *governance;
+                self.pending_pause_authority = *pause_authority;
                 self.coverage_authority = *coverage_authority;
                 self.fee_recipient = *fee_recipient;
             }

 // programs/shortwin/src/instructions/accept_authorities.rs (new)
+pub fn handler(ctx: Context<AcceptAuthorities>) -> Result<()> {
+    let mut c = ctx.accounts.config.load_mut()?;
+    require_keys_eq!(ctx.accounts.new_governance.key(), c.pending_governance, ShortwinError::Unauthorized);
+    require_keys_eq!(ctx.accounts.new_pause_authority.key(), c.pending_pause_authority, ShortwinError::Unauthorized);
+    c.governance = c.pending_governance;
+    c.pause_authority = c.pending_pause_authority;
+    Ok(())
+}
```

---

[75] **4. deposit drops the cashback gate after a curve market leaves pump**

`shortwin::deposit` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
After a curve market moves to None, any depositor ends a full-wipe record without the cashback sweep, so new lenders take the wiped lenders' cashback.

**Fix (Option A — validate: store at the wipe that the reward sweep is required)**:

```diff
 // programs/shortwin/src/state/market.rs
 pub const MARKET_FLAG_WIPE_REWARDS_CLAIMED: u16 = 1 << 8;
+/// Set when a full wipe rolls on a pump venue: the wipe record needs the venue-reward sweep.
+pub const MARKET_FLAG_WIPE_NEEDS_REWARDS: u16 = 1 << 9;

 // programs/shortwin/src/instructions/sol_settle.rs (after the settlement is booked)
+    // Record at the roll whether this full wipe needs the venue-reward sweep, while the
+    // market is still on the venue that earned the cashback.
+    if pool.total_shares == 0 && pool.is_wipe_live() {
+        market.set_flag(MARKET_FLAG_WIPE_NEEDS_REWARDS, lender::rewards_required(&market));
+    }

 // programs/shortwin/src/instructions/deposit.rs
-            lender::rewards_required(&market),
+            market.has_flag(MARKET_FLAG_WIPE_NEEDS_REWARDS) || lender::rewards_required(&market),

 // programs/shortwin/src/instructions/sweep_trader.rs
-        let pump = matches!(
-            kind,
-            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
-        );
+        // A wipe that needs the reward sweep keeps the pump token leg after the market leaves pump.
+        let pump = m.has_flag(MARKET_FLAG_WIPE_NEEDS_REWARDS)
+            || matches!(
+                kind,
+                Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
+            );
```

**Fix (Option B — validate: every market with a pump UVA needs the reward sweep)**:

```diff
 // programs/shortwin/src/engine/lender.rs
 pub fn rewards_required(market: &Market) -> bool {
-    matches!(
-        market.venue_kind(),
-        Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
-    )
+    // Curve-era cashback stays in the pump UVA after the market leaves pump (→ None, → Raydium).
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
+        // Read the pump UVA on every market that has one, so the reward bit is set there too.
+        let pump = m.pump_uva != Pubkey::default()
+            || matches!(
+                kind,
+                Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
+            );
```

---

[75] **5. open_short accepts small shorts that no keeper can liquidate**

`shortwin::open_short` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
At a low allowed min_notional, a borrower opens a short with no buyback room at the liquidation boundary, so lenders get only V − K_s.

**Fix (Option A — validate: refuse the open without buyback room at the liquidation boundary)**:

```diff
 // programs/shortwin/src/instructions/open_short.rs
     pos.lock_terms(&config.params, &tp, ert)?;
+    // At the liquidation boundary the buyback costs ⌈V·10⁴/cr_liq_bps⌉. Without that room
+    // `plan()` refuses liquidate and expire with UseSolSettle, and the only exit is S1 at V − K_s.
+    let k_s = risk::k_s(v, &pos.settle_keeper_terms()).math()?;
+    let b_liq = mul_div(u128::from(v), 10_000, u128::from(pos.cr_liq_bps), Rounding::Ceil).math()?;
+    require!(
+        u128::from(v.saturating_sub(k_s)).saturating_sub(u128::from(ert)) >= b_liq,
+        ShortwinError::BelowMinNotional
+    );
     let release_at = pos.release_at()?;
```

**Fix (Option B — restrict: raise the compiled floor of min_notional)**:

```diff
 // crates/shortwin-math/src/params.rs (Rule bounds)
-            Self::MinNotional => (10_000_000, 1_000_000_000),
+            // Above the notional where V − K_s − ert ≥ ⌈V·10⁴/cr_liq_bps⌉ holds at the compiled
+            // extremes of the keeper and tier terms (about 2.63·10⁷ at the Tier A defaults).
+            Self::MinNotional => (MIN_NOTIONAL_FLOOR, 1_000_000_000),
```

---

[75] **6. A deposit before an S3 settlement buys SOL at an old P_ema**

`shortwin::sol_settle` · Confidence: 75 · seen in 3/3 runs · NEW

**Description**
An attacker deposits cheap tokens before an S3 sol_settle on a halted venue, so it takes the other lenders' SOL at the old P_ema.

**Fix (Option A — validate: vest the whole S2/S3 lender amount)**:

```diff
 // programs/shortwin/src/instructions/sol_settle.rs
         Some(s) => {
-            let payout = waterfall::settle(pos.v, s.kind, s.k_s, s.fv, pos.q, inp.p_ema, inp.r_due)
-                .math()?;
+            // Vest the whole S2/S3 take over lump_vest_s: an immediate ⌊Q·P_ema/WAD⌋ + R_due goes
+            // to the shares that hold at the settlement, and P_ema is the pre-halt price when the
+            // venue cannot execute. S1 ignores both arguments.
+            let payout = waterfall::settle(pos.v, s.kind, s.k_s, s.fv, pos.q, 0, 0).math()?;
```

**Fix (Option B — ban-path: refuse deposits while the S3 chain runs)**:

```diff
 // programs/shortwin/src/instructions/deposit.rs
         prologue::roll_only(&market_key, &mut market, now, clock.slot)?;
+        // An S3 settlement credits ⌊Q·P_ema/WAD⌋ at once, at a P_ema frozen while the venue
+        // cannot execute: no deposit may join the shares while that chain runs.
+        require!(market.no_route_since == 0, ShortwinError::SettlementPending);
```

---

[75] **7. Repeated 1-lamport sweeps keep moving the vest end later**

`shortwin::sweep_trader` · Confidence: 75 · seen in 2/3 runs · NEW

**Description**
Any caller repeats 1-lamport sweep_trader calls that each move vest_end 1 s later, so lenders who exit at vest_end lose SOL to those who stay.

**Fix (Option A — validate: round the value-weighted span down)**:

```diff
 // crates/shortwin-math/src/pool.rs (vest_in)
-        mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        // Round down: a lump moves the end only by its value share and a dust lump moves it 0 s,
+        // so repeated 1-lamport sweeps cannot hold vest_end at now + lump_vest_s.
+        mul_div(num, 1, add_u128(bal, x)?, Rounding::Floor)?
```

**Fix (Option B — allow-and-handle: a dust lump joins the running vest without moving its end)**:

```diff
 // crates/shortwin-math/src/pool.rs (vest_in)
     let x = u128::from(lamports);
+    // A lump below the dust floor joins the running vest and leaves vest_end where it is.
+    if p.vest_bal != 0 && p.vest_end > now && lamports < MIN_VEST_MOVE_LAMPORTS {
+        p.vest_bal = add_u64(p.vest_bal, lamports)?;
+        p.vest_last = now;
+        return Ok(());
+    }
     let span: u128 = if p.vest_bal != 0 && p.vest_end > now {
```

---

Findings List

| # | Confidence | Title |
|---|---|---|
| 1 | [85] | flag_venue_upgraded treats a ProgramData extension as a venue upgrade |
| 2 | [75] | accrue floors each lender rent credit and keeps no remainder |
| 3 | [75] | apply_config installs a new governance key that never signs |
| 4 | [75] | deposit drops the cashback gate after a curve market leaves pump |
| 5 | [75] | open_short accepts small shorts that no keeper can liquidate |
| 6 | [75] | A deposit before an S3 settlement buys SOL at an old P_ema |
| 7 | [75] | Repeated 1-lamport sweeps keep moving the vest end later |

---

## Leads

_Vulnerability trails with concrete code smells where the full exploit path could not be completed in one analysis pass. These are not false positives — they are high-signal leads for manual review. Not scored._

- **POSITION_OPEN is 0, so zeroed status bytes read as open** — `shortwin::add_margin` · seen in 2/3 runs · NEW — Code smells: `pub const POSITION_OPEN: u8 = 0` (state/position.rs:19) is the only status that `add_margin` accepts; every `ShortPosition` today is created by the `init` in `open_short` and fully written — A ShortPosition with zeroed status bytes reads as open to add_margin. No path writes a zeroed position today.
- **A pending EnableVenues change turns a disabled venue back on** — `shortwin::apply_config` · seen in 3/3 runs · NEW — Code smells: `disable_venue` clears a `venue_enabled` bit at once and does not touch `config.pending`; `GlobalConfig::apply` sets `self.venue_enabled |= *bits`; `apply_config` has no signer, and `check_change` checks only that the bits are known and enableable, not whether a bit was cleared after the proposal; `EnableVenues` is not in `GOVERNANCE_ONLY_CANCEL`, so the Emergency vault can cancel it — Any caller can apply an EnableVenues change that Governance proposed before a disable_venue call, so the disabled venue takes opens again. We assume the Emergency vault does not always cancel that pending change in time.
- **apply_config installs any OracleFeed account key without a check** — `shortwin::apply_config` · seen in 2/3 runs · NEW — Code smells: `check_shape` for `OracleFeed` requires only `account != Pubkey::default()`; `apply_config` empties `OracleCache` and checks nothing about the new account, while an `Authorities` change checks the new `fee_recipient` at apply time (`check_fee_recipient`); `crank_sol_usd` requires `owner = PYTH_RECEIVER_ID` and the feed id, so a wrong key makes every crank fail; `tier::assign` then gives tier C to every Raydium and PumpSwap market until another change lands about 96 h later — A wrong Pyth key in an honest OracleFeed change makes every crank_sol_usd fail, so pool markets tier as C for about 96 h.
- **assess fails when the live price rounds to zero** — `shortwin::assess` · seen in 3/3 runs · NEW — Code smells: `assess` calls `risk::sigma_xy(Some(b.net), q, x, y).math()?` and guards only `state.x > 0`; `sigma_xy` takes `P = ⌊y·WAD/x⌋`, which is 0 when `y·10¹⁸ < x`, and then divides by `q·P`; `arming::RiskQuotes::sigma_of`, which `sol_settle` uses, maps the same `DivByZero` to `u64::MAX`; the SOL reserve must be 18 lamports or less with `x` near `u64::MAX` — liquidate and expire fail when the live price rounds to zero, because assess does not map the σ division error to the maximum. No realistic pool reaches that state.
- **The four exit instructions do not check the position status** — `shortwin::close_short` · seen in 3/3 runs · NEW — Code smells: `AddMargin` requires `position.status == POSITION_OPEN`; the Accounts structs of `close_short`, `liquidate`, `expire` and `sol_settle` check `has_one` and the PDA but never `status`; `POSITION_EXIT_IN_PROGRESS = 1` is reserved for v1.1; today only `open_short` writes `status`, and it writes `POSITION_OPEN` — close_short, liquidate, expire and sol_settle accept a position with any status, but add_margin requires POSITION_OPEN. A future instruction that writes ExitInProgress lets a second exit run on a position that is already exiting.
- **crank_price moves a list-only market onto a disabled pump venue** — `shortwin::crank_price` · seen in 1/3 runs · NEW — Code smells: `redetect` and `redetect_pool` write `market.venue = to` (PumpCurve, PumpMigrating or PumpSwap) with no `config.venue_enabled` check; only `engine::guard::open_static` reads `venue_enabled`; the open positions of that market then exit on the new venue — Any caller's crank_price moves a list-only market onto a pump venue that the Emergency vault disabled. Opens still fail there, and we did not prove harm to the exits.
- **crank_price moves a list-only market onto a curve with no warm-up** — `shortwin::crank_price` · seen in 1/3 runs · NEW — Code smells: the pump listing in `create_market` sets `created_at = now`, so `open_short` waits `market_warmup_s` (50 s) while the EMA integrates; `redetect` (None → PumpCurve) writes `Observation::new(now, p, l)` (warm = 0) at one spot read that the crank caller picks, and leaves `created_at` at its old value; `open_short` has no `ema_warm` gate on PumpCurve, and `spot_near_ema` passes because P_ema equals that spot — crank_price sets P_ema from one spot read when it moves a list-only market onto a live curve, so opens can follow at once. We did not prove that a list-only market can later show a live curve.
- **One routed observation clears the S3 chain of a market** — `shortwin::crank_price` · seen in 2/3 runs · NEW — Code smells: `route::observe_chains` → `settle::chain_step(.., hit = false)` returns (0, 0), so one routed observation clears `no_route_since`, and `pool_fail_since` on the same path; every prologue with a ReadSet makes that observation from live venue state; on PumpSwap `fee_ppm` comes from the pre-trade reserves and the live supply; `crank_price` needs no signer; the fee caps (M7-SEC-5) and the spot leg (FITS-2) guard against state that the caller sets inside its own transaction, but the S3 and `pool_fail` chains have no such guard — A caller who makes the venue look routed inside its own transaction clears the S3 chain, so the S3 settlement waits another s3_wait_s. We did not prove that a reversible trade brings the live fee under venue_fee_hard_cap_bps.
- **crank_sol_usd refuses a large honest SOL/USD move until the cache expires** — `shortwin::crank_sol_usd` · seen in 2/3 runs · NEW — Code smells: `pyth::check` skips `step_ok` only when `now − cached.publish_time > oracle_stale_s`; `tier::assign` treats the same cache as fresh up to exactly that age; a move from $200 to $155 fails `OracleStep` (`|155 − 200|·10⁴ = 450,000 > 2,000·200 = 400,000`) until the cache is `oracle_stale_s` old; meanwhile `open_short` and `crank_price` tier on $200, so a market with Λ = 2,800 SOL tiers A in place of B for about 570 s; opens lock the Tier A terms for their whole life (A26); OWNER-M7-15 accepts a 600 s window for a stalled feed, not for a refused live price — crank_sol_usd refuses an honest SOL/USD move above oracle_max_step_bps until the cache goes stale, so new opens lock a tier from the old price. We did not show such a move inside one crank period on mainnet data.
- **create_market passes the creator's signer to an unreviewed pump upgrade** — `shortwin::create_market` · seen in 1/3 runs · NEW — Code smells: `pump_listing` calls `pump_curve::init_uva` / `pump_amm::init_uva`, which build `AccountMeta::new(payer.key(), true)` with payer = `creator`, a writable signer; the `VENUE_HINT_AUTO` branch never calls `guard::latch` (the module doc says "Pump listings are never latched"); every trading leg marks only `trader` as a signer, and short.win moves user lamports only by its own System transfers; so this is the only path where a user's own signature reaches a venue program, and it still runs while GUARD_PUMP or GUARD_PUMP_AMM is set — create_market passes the creator's signer to the pump program while its upgrade is unreviewed, so that upgrade can take the creator's lamports. We did not prove that a pump upgrade does this.
- **create_market lists a mint on a disabled venue** — `shortwin::create_market` · seen in 2/3 runs · NEW — Code smells: a Raydium hint runs `guard::latch(venue_guard, ..)` and the verify, but reads no `config.venue_enabled`; the `VENUE_HINT_AUTO` pump listing checks neither the latch nor `venue_enabled`; only `engine::guard::open_static` reads `venue_enabled`; the market PDA `["market", mint]` is `init`, so the listing fixes the venue, the pool, its SOL side and `graduated_at` for the only market the mint can have — Any caller lists a mint on a venue that disable_venue turned off, and the listing fixes the pool for good. Opens still fail there, and we did not prove other harm.
- **The first create_market caller picks the venue and pool of a mint** — `shortwin::create_market` · seen in 3/3 runs · NEW — Code smells: the module doc says a front-runner "can only create that same market", but the caller chooses `venue_hint` (Auto, RaydiumV4, RaydiumCpmm) and the Raydium candidate in `remaining_accounts`; that choice sets `m.venue`, the pool cache, the SOL side, `pool_d`, `graduated_at` and the first observation; `market` is `init` on `[Market::SEED, mint]`, so the first lister wins; a replacement goes only through `propose_pool`, under `pool_list_margin_bps` / `pool_switch_margin_bps` and with no open position — A front-runner who lists a mint first chooses its venue and Raydium pool, but the module doc says that only `creator` differs. We did not prove a loss beyond a later pool switch.
- **No instruction reads or clears MARKET_FLAG_GRADUATED_ELSEWHERE** — `shortwin::create_market` · seen in 1/3 runs · NEW — Code smells: `create_market`, `propose_pool`, `execute_pool_switch` and `pump_curve::sync` set `MARKET_FLAG_GRADUATED_ELSEWHERE`; no instruction in programs/shortwin reads it; no path clears it, including crank_price's redetect back onto a pump venue — Four paths set MARKET_FLAG_GRADUATED_ELSEWHERE, and nothing reads or clears it. A later change that reads the flag can trust a value that is no longer true.
- **The first listed pool's open_time fixes the market age for good** — `shortwin::create_market` · seen in 3/3 runs · NEW — Code smells: `Adoption::apply` sets `market.graduated_at = self.age_start`, which on CPMM is `min(now, open_time)` of the pool the lister chose; `pool_select::adopt` keeps it for every later Raydium switch; `tier::assign` gates Tier C (< 24 h) and Tier A (≥ 7 d) on it; the docs say age is "the token's time on an eligible pool", but `open_time` measures the pool's existence, not when it became eligible — A lister who picks an old CPMM pool that became eligible today gives the market and every later pool that old age. We did not prove that the earlier Tier A causes a loss for lenders.
- **execute_pool_switch moves open positions onto a disabled venue** — `shortwin::execute_pool_switch` · seen in 2/3 runs · NEW — Code smells: the handler checks `guard::candidate_latched` but never `config.venue_enabled` or `config.venue_route_broken` for the candidate kind (`adoptable = !inc.pump_venue && !latched && window_adoptable(..)`); only `engine::guard::open_static` reads `venue_enabled`; a window recorded before `disable_venue` still runs `pool_select::adopt`, which writes the candidate kind into `market.venue` and moves every open position there; the docs of `disable_venue` say "Opens only" — Any caller resolves a window onto a venue that the Emergency vault disabled, so the open positions then exit there. We assume that a disabled venue must not receive adopted markets.
- **A sale before execute_pool_switch cancels a valid pool-switch window** — `shortwin::execute_pool_switch` · seen in 2/3 runs · NEW — Code smells: the docs say `FitAt::Execution` judges "on the terms no transaction can move", and it drops `X_cand`; but `carried_fit` still evaluates at `window_price = min(P_ema, P_cand)`, and `candidate_tier` reads `r_now = a.state.r`, both from the candidate pool's live reserves; a sale of 40 SOL into a 2,100-SOL candidate moves Λ from about $509k to $499k, under θ_a = $500k, so the tier drops from A to B; a fit failure makes `clear_window` cancel the window, and the chain stays armed, so this repeats for each window; default `TierParams` give tiers A, B and C the same caps (500 / 2,000 bps) — A caller who sells into the candidate pool just before execute_pool_switch cancels a valid window when lower tiers have tighter impact caps. The default tier caps are equal, so the default configuration is not exposed.
- **execute_pool_switch takes the market age from the window candidate** — `shortwin::execute_pool_switch` · seen in 2/3 runs · NEW — Code smells: on a `None` market with open positions, `pool_select::adopt` sets `graduated_at = a.age_start` from the window candidate's CPMM `open_time`; proposers choose that candidate; `candidate_tier` uses the same `a.age_start` for the fit — Any caller adopts a window candidate on a list-only market, and the market takes that CPMM pool's open_time as its age. We did not measure the tier change.
- **execute_pool_switch checks the window floor at the frozen P_ema** — `shortwin::execute_pool_switch` · seen in 1/3 runs · NEW — Code smells: `propose_pool` checks the window floor at `window_price(p_ref, &adoption)` = min(P_ema, P_cand); `execute_pool_switch` checks it at `p_ref` = P_ema alone (`.filter(|a| pool_select::window_floor_ok(a, &config.params, p_ref))`); `carried_fit` in the same filter chain still uses min(P_ema, P_cand); the window adopts only while `pool_fail` holds, so P_ema stays frozen high; with pool_min_real_sol = 25 SOL, a candidate with 30 SOL locked at P_ema holds about 21.2 SOL after P_cand halves, and the execution still sees 30 SOL — After the candidate price falls, any caller of execute_pool_switch moves open positions onto a pool below pool_min_real_sol, because the floor uses the frozen P_ema. The doc says execution uses P_ema, and we did not prove that carried_fit passes.
- **expire fails when the borrower address refuses lamports** — `shortwin::expire` · seen in 1/3 runs · NEW — Code smells: `Expire.borrower` is an `UncheckedAccount` (`mut`); `close = borrower` on `position` and `close_exposure(.., borrower, pos.q)` credit only that account; there is no other recipient; `sol_settle` closes to the same account, so no other instruction exits an expired position on an executable venue — A borrower who makes its own address refuse lamports, for example with a program deployed there, makes every expire of its positions fail. We did not prove that the runtime still refuses that credit.
- **initialize_config accepts a fee recipient that apply_config refuses** — `shortwin::initialize_config` · seen in 1/3 runs · NEW — Code smells: `initialize_config` runs `check_authorities(.., &args.fee_recipient)` but never `check_fee_recipient`, which `apply_config` runs on the same key; `sweep_protocol_fees` needs a writable recipient (`move_lamports_above` requires `to.is_writable`); the runtime makes a sysvar or program key read-only — initialize_config does not run check_fee_recipient, so a sysvar or program key as fee recipient makes every sweep_protocol_fees fail until a timelocked Authorities change.
- **initialize_config stores guard_slot without the ProgramData check** — `shortwin::initialize_config` · seen in 3/3 runs · NEW — Code smells: `revalidate_venue` writes `guard_slot` only after `require_eq!(slot, expected_slot)` against the venue ProgramData; `initialize_config` writes `g.guard_slot = args.guard_slot` and takes no ProgramData account; `constants.rs` keeps several dated baselines (`VENUE_GUARD_SLOTS_2026_09_27`, `VENUE_GUARD_SLOTS_2026_10_02`), and CPMM was redeployed between them; `flag_venue_upgraded` needs no signer and sets the latch bit when `slot != guard_slot`, and never latches when `guard_slot` equals the deploy slot of an upgrade nobody reviewed — A wrong guard_slot at launch lets any caller latch that venue at once, or lets an unreviewed upgrade pass as reviewed. The harm needs a wrong value from the upgrade authority.
- **liquidate fails when the borrower address refuses lamports** — `shortwin::liquidate` · seen in 3/3 runs · NEW — Code smells: `Liquidate.borrower` is an `UncheckedAccount` (`mut`) that signed only at `open_short`; `close = borrower` on `position` and `close_exposure(.., borrower, ..)` credit only that account, with no fallback recipient; a borrower can empty its key and deploy a loader-v3 program at that address; Agave refuses a lamport change on an executable account (`ExecutableLamportChange`) unless SIMD-0162 is active, and it does not write-lock an executable or reserved key; if the credit fails, `ΣB_Q`, `open_positions` and `total_notional_open` never drop, and `withdraw` refuses the last shares (`LastSharesBorrowed`) — A borrower who deploys a program at its own address makes every liquidate of its positions fail, so lenders never get the borrowed tokens back. We did not prove that the cluster still refuses lamports into an executable account.
- **Exits read pool_lock_min_bps live, not the value locked at open** — `shortwin::liquidate` · seen in 3/3 runs · NEW — Code smells: `keeper_exit` (shared by `liquidate` and `expire`) calls `bounds::ref_depth_cap(&config, &market, kind)`, which reads `config.params.pool_lock_min_bps` at exit time; `ShortPosition::lock_terms` locks `delta_liq_bps`, the `kappa_*` terms, `ref_max_age_s` and `venue_fee_hard_cap_bps`, but not `λ_min`; the cap `⌊pool_d·10⁴/λ_min⌋` bounds `L_b`, so it sets `B̂_ref`, `max_in`, the penalty base and `K_e` on expiry; the `add_margin` docs say the exit bounds stay as the open locked them; any caller chooses the moment of `apply_config` — A timelocked Governance change of pool_lock_min_bps moves the exit bound and the expiry reward of positions that are already open, in liquidate and in expire. We did not prove a loss that users cannot avoid inside the timelock.
- **Rent takes the keeper floor on an aged position below CR 100 %** — `shortwin::liquidate` · seen in 2/3 runs · NEW — Code smells: `waterfall::liquidate` pays `r = min(R_due, V − B)` and the penalty before the keeper floor `min(K_s − kp, W)`; `max_in_liq` reserves `K_s` only against the buy; `plan` admits the buyback whenever `B̂_live + ert ≤ V − K_s`; for `V − B` in `[K_s + ert, R_due + K_s)` the keeper gets less than `K_s` while the S1 leg is false; with B = 100 SOL, R_due = 10 SOL, V = 105 SOL and K_s = 0.25 SOL the keeper gets 0; ECON §8.5 says liquidating never pays less than settling — On an aged position below CR 100 %, rent takes the vault remainder, so a keeper earns nothing from liquidate and waits for S1. We did not check whether the in-house keeper liquidates such positions at a loss.
- **open_short does not check the position lamports after the refund** — `shortwin::open_short` · seen in 1/3 runs · NEW — Code smells: `open_short` sets `pos.v = s + m` and later calls `move_lamports(position → borrower, sold.refund)`; that move keeps only `rent_min`; no post-condition checks `position.lamports ≥ rent_min + v`; every exit computes its payouts from `pos.v` and needs those lamports on the position — open_short does not check that the position holds rent plus v lamports after the refund. A future refund error in a venue adapter would pay the borrower from the position SOL.
- **open_short reads the mint supply from before the sell CPI** — `shortwin::open_short` · seen in 3/3 runs · NEW — Code smells: step 9 builds `post = venues::read_state_boxed(..., Some(accs.mint.supply))` after the `venues::sell` CPI; `accs.mint` was deserialized before the handler ran, and nothing calls `reload()`; `post` decides `n_open`, the `BelowMinNotional` and `MarketNotionalCap` checks, the D4 margin and the open fee, and on PumpSwap the supply picks the fee tier; no current adapter mints or burns the base token on a sell — open_short uses the mint supply cached before the sell CPI, so a venue that mints or burns on a sell gives a wrong notional. No current venue changes the supply on a sell.
- **Four instructions read the VenueGuard latch with no version check** — `shortwin::open_short` · seen in 3/3 runs · NEW — Code smells: `flag_venue_upgraded` and `revalidate_venue` check `venue_guard.load()?.version == VENUE_GUARD_VERSION`; `open_short`, `create_market`, `propose_pool` and `execute_pool_switch` bind `venue_guard` only by `seeds = [VenueGuard::SEED], bump = venue_guard.load()?.bump` and read `upgraded` through `guard::latch`, `guard::candidate_latch` or `guard::candidate_latched`; `config` and `market` are version-checked in every instruction that takes them — Four instructions read the VenueGuard latch without the version check that its writers make, so a later layout change gives them wrong latch bits.
- **propose_pool moves a list-only market onto a disabled venue** — `shortwin::propose_pool` · seen in 3/3 runs · NEW — Code smells: the program reads `config.venue_enabled` only in `engine::guard::open_static`; `propose_pool` checks `guard::candidate_latch` but not `config.venue_enabled & cand_kind.venue_bit()`; for a `None` market with `open_positions == 0`, `pool_select::decide` returns `Proposal::Adopt` for any eligible candidate, and `record_window` also records windows that `execute_pool_switch` later adopts; the source says disabling "affects opens only" — Any caller can move a list-only market onto a pool of a disabled venue, so open_short fails on that market until Governance enables the venue. We did not prove that a venue stays disabled long enough to matter.
- **propose_pool lets the proposer fix a list-only market's age** — `shortwin::propose_pool` · seen in 2/3 runs · NEW — Code smells: `pool_select::adopt` runs `if from == VenueKind::None { market.graduated_at = a.age_start; }`; on CPMM `age_start = min(now, open_time)` when `open_time ≥ T_CLAMP`; every later adoption keeps `graduated_at`; `tier::assign` reads it for the age gate; a list-only market with no position adopts the first eligible candidate at once — The proposer picks the CPMM pool that a list-only market adopts, so an old pool with new locked liquidity sets an old market age. We did not measure the tier change for later opens.
- **A venue that leaves the upgradeable loader keeps its latch forever** — `shortwin::revalidate_venue` · seen in 1/3 runs · NEW — Code smells: `RevalidateVenue.program_data` and `FlagVenueUpgraded.program_data` require `address = venue_program_data(venue)`, a compile-time `PDA(BPFLoaderUpgradeable, [program_id])`, and `owner = bpf_loader_upgradeable::ID`; `read_deploy_slot` returns `VenueAccountMismatch` for any other owner; `revalidate_venue` is the only instruction that clears a bit of `VenueGuard.upgraded`; a loader-v4 migration of the venue closes that ProgramData account, and a bit that is clear at that moment can never be set again — If a venue moves to loader-v4 with its latch bit set, revalidate_venue always fails, so its opens stay blocked until a short.win upgrade. We did not verify that loader-v4 migration is live on mainnet.
- **sol_settle fails when the borrower address refuses lamports** — `shortwin::sol_settle` · seen in 1/3 runs · NEW — Code smells: the executing branch calls `exposure.close(to)` and `position.close(to)` with `to = borrower`, an `UncheckedAccount` (`mut`); the no-op arming path never touches the borrower, but every S1, S2 and S3 execution credits it; `sol_settle` is the last exit on a None or PumpMigrating market — A borrower who makes its own address refuse lamports makes every executing sol_settle of its positions fail, so S1, S2 and S3 never run. We did not prove that the runtime still refuses that credit.
- **A raised global fee cap leaves some liquidatable positions with no exit** — `shortwin::sol_settle` · seen in 2/3 runs · NEW — Code smells: `liquidate`'s `buyback_allowed` reads the execution quote `state.buy(q)` at the live venue fee; the live S1 leg in `sol_settle` reads `RiskQuotes`, capped at the locked `pos.venue_fee_hard_cap_bps`; `s3_no_route` reads the global `config.params.venue_fee_hard_cap_bps`; Governance may raise the global cap within [300, 2,000] above a position's locked cap; on AMM v4 with a locked cap of 500, a global cap of 1,000 and a live fee of 800 bps, `B̂_exec ≈ 108.7`, `B̂_risk ≈ 105.3` and `V − K_s = 106`; the `s1_legs` doc says "no band exists where neither a buyback nor S1 is possible" — After Governance raises venue_fee_hard_cap_bps, a liquidatable position whose venue fee sits between its locked cap and the new cap has no exit. We did not measure how long a real fee rise keeps a position in that band.
- **sol_settle stores the vault only when three tracked fields change** — `shortwin::sol_settle` · seen in 2/3 runs · NEW — Code smells: the no-op branch calls `set_vault` only if `(lender_sol, unmatched, protocol_fees) != tracked_before`; `close_short` and `keeper_exit` always call `set_vault` after the prologue; `crank_price`, `propose_pool` and `execute_pool_switch` compare the same three fields on a scratch vault; `Vault` also holds `pi`, `b_min`, `cal_day`, `lock_cal` and `lock_norm`; today `prologue::accrue` writes only the three tracked fields — The no-op path of sol_settle drops a prologue write to any other Vault field. No prologue writes another field today.
- **An S1 settlement credits lenders at once above the token value** — `shortwin::sol_settle` · seen in 3/3 runs · NEW — Code smells: for S1 `waterfall::settle` returns `(take, 0)` with `take = V − K_s`, so nothing vests but the rent top-up; `pool::settle` credits `take` at once to the shares that exist at settlement; the S1 legs require only `V − K_s < B̂ + ert`, and `B̂` includes price impact and two fees; `deposit` needs no venue account, so a keeper can bundle a deposit before the S1 execution; S2 and S3 vest the part above `⌊q·P_ema/WAD⌋ + R_due` since R4B-6; `waterfall.rs` says S1 is "never capped, nothing vests" — A depositor who deposits just before an S1 settlement takes a share of the premium above the token value. The source says S1 stays immediate; we did not size the excess on a real S1.
- **A donation to the trader cashback accounts blocks deposits after a wipe** — `shortwin::sweep_trader` · seen in 3/3 runs · NEW — Code smells: `pump_cashback_claimable` returns the pump UVA lamports above rent, and `pump_amm::cashback_claimable` the raw WSOL of `ATA(pAMM UVA(trader), WSOL)`; `sweep_trader` sets `wipe_rewards_claimed` only when both are 0, and it is the only setter of that bit; any account can credit lamports to a pump-owned account or WSOL to that ATA; pAMM's `claim_cashback` moves the whole ATA balance, so that leg can be cleared, but the pump UVA leg depends on pump's `claim_cashback` paying out every lamport above rent; the UVA field `cashback_earned` (offset 74) is not read — An attacker who sends lamports to the trader pump UVA after a full wipe keeps wipe_rewards_claimed clear, so deposits fail until claim_cashback runs. We did not prove that pump's claim_cashback leaves a donation behind.
- **sync_lender floors each touch and resets the lender snapshot** — `shortwin::sync_lender` · seen in 1/3 runs · NEW — Code smells: `touch_tail` (crates/shortwin-math/src/pool.rs) accrues `⌊s·(acc − snap)/WAD⌋` and then sets `snap = acc`, so each touch drops the fraction; `fwd_walk` uses the cumulative floor `⌊q·end/WAD⌋ − ⌊q·snap/WAD⌋` for this reason; any caller can send `sync_lender` for any lender (`pool::touch_pot`); at 101 lamports/s of market credit and a touch after every accrual, a 0.1 % lender gets 0 of 8,726 lamports per day and a 1 % lender 86,400 of 87,264; the repository discloses this as KI-68 ("≤ 1 lamport per touch", owner decision pending) — Any caller who sends sync_lender after each accrual makes a small lender earn nothing, because each touch floors its accrual. The caller pays more in fees than the lender loses, and KI-68 discloses the floor.
- **One open position blocks a timelocked Kink change on its market** — `shortwin::sync_market_params` · seen in 2/3 runs · NEW — Code smells: `sync_market_params` (engine/prologue.rs) adopts the kink only `if market.kink_version != kink.version && market.open_positions == 0`; the EMA change has a `force_at` and the kink has none; overlapping `open_short` calls keep `open_positions ≥ 1`, so every open position keeps paying rent at the old kink — A borrower who keeps one position open on a market blocks the timelocked Kink change there for as long as the position stays open. We assume the source does not intend a change that never applies.
- **A dust deposit stops the last-lender vest flush in withdraw** — `shortwin::withdraw` · seen in 1/3 runs · NEW — Code smells: `withdraw_in` pays the unreleased vest only when `shares == p.total_shares` (pool.rs:1831); a dust deposit of about 1 token unit (≈1,000 shares) keeps that false; the `min_sol` guard (withdraw.rs:213) only reverts a front-run; when the real last lender exits, its unreleased vest flows to the dust holder, who takes the rest as the new last lender; the 1-lamport `sweep_trader` calls also keep the vest from finishing — An attacker who keeps a dust deposit in the pool stops the last real lender's vest flush, so the attacker later takes the unreleased vest. We did not weigh this gain against the cost of waiting.

---

> ⚠️ This review was performed by an AI assistant. AI analysis can never verify the complete absence of vulnerabilities and no guarantee of security is given. Team security reviews, bug bounty programs, and on-chain monitoring are strongly recommended. For a consultation regarding your projects' security, visit [https://www.pashov.com](https://www.pashov.com)
