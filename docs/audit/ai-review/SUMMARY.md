# short.win — rust-auditor summary

Commit `55b3e23` · run 2026-10-09/10 · scope: the 106 core files of docs/AUDIT.md §2, in six scans, 3 passes each, 12 Opus agents per pass · this is an AI review, not a formal audit.

## How to read this

The six scan reports were merged by root cause, so one issue below can combine findings from several scans. Two independent reviewers then re-checked each issue: one asked whether the path is reachable, the other what it actually costs. Status is **Confirmed** when both confirm, **Disputed** when their verdicts differ, and **Refuted** when both refute (or one refutes with concrete code and the other is uncertain). Severity is the reviewers' fair rating. Where they differ, the table shows the lower one with a note. Confirmed means the mechanism is real. In most cases the reviewers also found the impact smaller than the original finding claimed, and the sections below give their corrected impact.

All 17 issues came back Confirmed. None is Disputed or Refuted.

## Issues

| ID | Title | Severity | Status | Primary location | Reported by (scan #finding, confidence) |
|---|---|---|---|---|---|
| I-01 | Venue upgrade latch keys on the ProgramData deploy slot, which a permissionless ExtendProgram rewrites | Medium | Confirmed | `programs/shortwin/src/instructions/flag_venue_upgraded.rs:51-68` | S1 #1 (85), S2 #6 (75), S4 #1 (85) |
| I-07 | maybe_roll_in flushes the unreleased vest only on a full wipe, not on a rebase roll | Medium | Confirmed | `crates/shortwin-math/src/pool.rs:2316-2330` | S2 #2 (75), S5 #2 (75) |
| I-02 | vest_in rounds the vest end up, so dust sweeps keep pushing vest_end later | Low | Confirmed | `crates/shortwin-math/src/pool.rs:937` | S1 #7 (75), S2 #1 (85), S5 #1 (85) |
| I-03 | lender_credit floors each accrual's rent credit and carries no remainder | Low | Confirmed | `crates/shortwin-math/src/rent.rs:373-374` | S1 #2 (75), S2 #3 (75), S6 #4 (75) |
| I-05 | rewards_required reads the current venue kind, so the full-wipe cashback gate drops after a market leaves pump | Low | Confirmed | `programs/shortwin/src/engine/lender.rs:185-190` | S1 #4 (75), S2 #7 (75) |
| I-06 | The min_notional floor admits positions with no buyback room at the liquidation boundary | Low | Confirmed | `crates/shortwin-math/src/params.rs:998` | S1 #5 (75), S6 #2 (75) |
| I-08 | On an empty non-wipe market, deposit-sweep-withdraw lets one lender take swept SOL owed to the protocol | Low | Confirmed | `crates/shortwin-math/src/pool.rs:1939` | S2 #5 (75), S5 #3 (75) |
| I-09 | S2/S3 settlement credits Q·P_ema at once, at a frozen pre-halt price, to whoever holds shares | Low | Confirmed | `programs/shortwin/src/instructions/sol_settle.rs:257` | S1 #6 (75) |
| I-10 | Liquidation risk view prices at the live fee tier, which a same-transaction sale or burn raises | Low | Confirmed | `programs/shortwin/src/instructions/liquidate.rs:333` | S3 #1 (75) |
| I-12 | A new lock era resets b_min but not Π, so Π compounds across eras down to 1 | Low | Confirmed | `crates/shortwin-math/src/pool.rs:1806-1808` | S5 #5 (75) |
| I-13 | new_raydium seeds L_slow at the 2D anchor instead of capping it at the locked depth D | Low | Confirmed | `crates/shortwin-math/src/observe.rs:398-402` | S5 #6 (70, below threshold) |
| I-14 | Raydium ref_depth_cap credits 2D, so a held-then-pulled deposit lifts L_b over the live pool | Low | Confirmed | `programs/shortwin/src/engine/bounds.rs:48-50` | S6 #5 (75) |
| I-15 | Settlement box takes L_hi from the upper median of bucket highs, which deposit-wrapped cranks raise | Low | Confirmed | `crates/shortwin-math/src/settle.rs:87-92` | S6 #1 (75) |
| I-16 | evaluate step 2 refreshes a lapsed arming clock instead of letting it re-arm | Low | Confirmed | `crates/shortwin-math/src/settle.rs:557-564` | S6 #3 (75) |
| I-17 | Under the post-change guard, the spot-leg liquidity floor reads bucket lows that withdraw-wrapped cranks lower | Low | Confirmed | `crates/shortwin-math/src/bounds.rs:156-158` | S6 #6 (75) |
| I-04 | apply_config installs new governance/pause keys without their signatures | Informational | Confirmed | `programs/shortwin/src/state/config.rs:187-196` | S1 #3 (75), S2 #4 (75) |
| I-11 | withdraw_in resets b_min only when lock_norm == 0, so later locks keep a stale low b_min | Informational¹ | Confirmed | `crates/shortwin-math/src/pool.rs:1806-1808` | S5 #4 (75) |

¹ The reachability reviewer rated I-11 Low and the impact reviewer rated it Informational. The table shows the lower rating.

Scans: S1 instructions · S2 state and engine · S3 venues · S4 venue-state crate · S5 math core · S6 math rest.

Totals: 17 Confirmed (2 Medium, 13 Low, 2 Informational), 0 Disputed, 0 Refuted.

---

## Confirmed issues

### I-01 · Venue upgrade latch keys on the ProgramData deploy slot, which a permissionless ExtendProgram rewrites

**Medium · Confirmed** · `flag_venue_upgraded.rs:51-68`, also `revalidate_venue.rs:52-53` and `crates/shortwin-venue-state/src/program_data.rs:68` · S1 #1, S2 #6, S4 #1

**Root cause.** `flag_venue_upgraded` needs no signer. It sets a venue's latch bit whenever the ProgramData header slot differs from the stored `guard_slot`, and it never compares the ELF. In Agave 4.2 anyone can send the loader's `ExtendProgram`, which rewrites that slot without changing the code. `revalidate_venue` requires the exact slot, so it has the same blind spot.

**Impact.** Anyone can block new opens on every market of a venue with one transaction, `[ExtendProgram(venue), flag_venue_upgraded(venue)]`. The latch also blocks Raydium listings and pool-switch candidates.
- Tripping pfee stops every PumpCurve and PumpSwap open.
- Tripping AMM v4 or CPMM stops every Raydium open.
- pump and pAMM themselves cannot be extended because they are already at the 10 MiB cap.
- Under SIMD-0431's 10 KiB minimum, each trip costs about 0.071 SOL of rent, locked in the venue's ProgramData.
- Exits, `add_margin` and withdrawals are not affected, and no funds are at risk.
- The in-house keeper flags on any slot change, so even a harmless pre-extension by a venue team trips the latch.

**Reviewers.**
- *Reachability (Medium):* confirmed that anyone can reach it. The claim that the latch "cannot be cleared" is overstated: Emergency signers can stage, approve and execute a revalidation in one atomic transaction, or Governance can ship an upgrade. But an attacker who watches for a staged revalidation can extend again so that it fails with `VenueSlotMismatch`.
- *Impact (Medium):* corrected the per-trip cost, and found it would take about 63-67 SOL to exhaust one venue. The only reliable fix is a program upgrade behind the 48 h time lock, so expect at least 48 h plus development time of frozen opens across the pump and/or Raydium markets.

**Disclosed?** No. KI-63 and ARCH §8.5(g) mention that ExtendProgram is permissionless but call it harmless. KI-42 covers only latch reaction time.

**Fix direction.** Option A: latch and revalidate on a hash of the ELF bytes, excluding the zero tail that an extension appends, and let `revalidate_venue` accept a later slot when the hash matches. Option B: require a trusted signer (a keeper key or `pause_authority`) for `flag_venue_upgraded`, and keep the hash check in revalidation.

```diff
-            require!(slot != guard_slot, ShortwinError::VenueNotUpgraded);
+            require!(elf_hash != g.guard_hash[i], ShortwinError::VenueNotUpgraded);
```

### I-07 · maybe_roll_in flushes the unreleased vest only on a full wipe, not on a rebase roll

**Medium · Confirmed** · `crates/shortwin-math/src/pool.rs:2316-2330` · S2 #2, S5 #2

**Root cause.** At the last SOL settlement, `maybe_roll_in` pays `vest_bal` to the old shares only when `token_claims() == 0`. Two things can leave 1 raw token behind: a dust deposit made before the settlement, or the allowance floor when lenders withdraw. Either way the epoch rebases by 1/1000^k instead of wiping, and `vest_bal` stays pending. `release_in` later pays that vest to whoever holds shares, and a depositor who joins after the rebase with a few thousand raw units holds nearly all of them.

**Impact.** The lenders of a wiped or nearly wiped market lose the vest that was unreleased at the last settlement: S2/S3 premium, penalty and expiry lumps, and rent top-ups, plus any sweeps until `vest_end`.
- **Reference model (`pool_model.py`), attack case:** a 5 SOL lump. An attacker who pays 2 raw units and holds for about 6 h took 4,999,990,010 lamports.
- **Organic case, no attacker:** in 300 random pools, T always ended at 1, never 0, so the vest was never flushed. A depositor of 10^6 raw units who joined right after the settlement took 3,999,992,005 of 4 SOL.
- **What it needs:** settlement principal is not at risk. It needs a market where nearly all lent tokens are SOL-settled while free tokens are at dust level.

**Reviewers.**
- *Reachability (Medium):* `deposit` and `sol_settle` are permissionless, so an attacker can bundle `[deposit(1), sol_settle, deposit(X)]`. The same leak happens without an attacker whenever T is left at dust. This contradicts ECONOMICS.md:1422 ("the wiped lenders keep their unreleased lumps") and decision #44.
- *Impact (Medium):* confirmed in the reference model, and the dust deposit before the settlement is often not even needed. It is Medium rather than High because it needs a run that settles nearly all lent tokens first, and the take is limited to the vest window.

**Disclosed?** No. R4B-4/KI-09 (forfeit on exit) and the KI-65 residual are different mechanisms.

**Fix direction.**
- S2 flushes the vest on a rebase as well: `release_in`, then credit `vest_bal` to the old shares before rescaling.
- S5 offers three options:
  - A: treat claims at or below a dust floor as a full wipe;
  - B: refuse deposits while every token is lent;
  - C: flush the vest at every roll.

The impact reviewer warns that flushing only on a rebase is incomplete, because dilution works without any roll once claims are tiny. Flushing at the settlement itself could bring back the R4B-6 just-in-time snipe, so check that before adopting it.

### I-02 · vest_in rounds the vest end up, so dust sweeps keep pushing vest_end later

**Low · Confirmed** · `crates/shortwin-math/src/pool.rs:937` · S1 #7, S2 #1, S5 #1

**Root cause.** `vest_in` computes the new vest span with `Rounding::Ceil`. Any lump of 1 lamport or more against a running vest therefore moves `vest_end` exactly 1 s later, up to `now + lump_vest_s`. `sweep_trader` is permissionless, has no minimum amount and no per-slot limit. Rent is checked only per transaction, so `[transfer 1 lamport to trader PDA, sweep_trader]` can repeat many times in one transaction.

**Impact.** Sustained sweeps hold `vest_end` at `now + 21,600 s`. Release then decays exponentially with a 6 h time constant: 63.2 % released at 6 h, 98.2 % at 24 h, about 99.97 % at 48 h. That is the v0.4 behaviour that R4B-5 marks as fixed.
- A lender who exits at the nominal 6 h end forfeits about 36.8 % of its share of recent lumps to the lenders who stay.
- KI-09's advice to wait until `vest_end` stops working, because `vest_end` stays 6 h away.
- Cost to the attacker: about 0.01-0.02 SOL a day in fees.
- No principal is lost.

**Reviewers.**
- *Reachability (Low):* no code blocks repeated dust sweeps. An integer re-implementation reproduces the repo's k=60 figures. The docs' bound ("a griefer buys at most 1 s per transaction") is wrong.
- *Impact (Low):* confirmed with an exact integer copy of `release_in`/`vest_in`. What moves between lenders is vest yield, not principal. The attacker profits only through its own shares when others exit early.

**Disclosed?** No. ECON R4B-5 documents the 1 s-per-lump ceiling but claims the attack is fixed. KI-09 covers the exit forfeit.

**Fix direction.** All three reports propose the same change, and both reviewers agree it keeps the "never earlier" property while `lump_vest_s ≥ left`. The alternatives are to let a dust lump leave `vest_end` unchanged, or to set a minimum lump size.

```diff
-        mul_div(num, 1, add_u128(bal, x)?, Rounding::Ceil)?
+        mul_div(num, 1, add_u128(bal, x)?, Rounding::Floor)?
```

### I-03 · lender_credit floors each accrual's rent credit and carries no remainder

**Low · Confirmed** · `crates/shortwin-math/src/rent.rs:373-374`, called from `programs/shortwin/src/engine/prologue.rs:99` · S1 #2, S2 #3, S6 #4

**Root cause.** Each accrual floors the aggregate lender rent credit and stores no remainder. At close, the true-up measures the top-up against the exact, ceiled per-position amount, so the floored units are never given back. The difference stays in `lender_sol` as surplus that nobody can claim.

**Impact.** The loss is under 1 lamport per market per second, because `dt` is whole seconds. That is under 86,400 lamports a day per market.
- It matters only where open interest is tiny. A 0.1 SOL Tier A position cranked every second loses all of its continuous rent, about 0.0031 SOL over 90 days. At 1 SOL and 15 %, about 1 % is lost.
- Griefing does not pay: each crank costs a 5,000-lamport fee to destroy under 1 lamport, and nobody profits.

**Reviewers.**
- *Reachability (Low):* real, and anyone can trigger it through the permissionless `crank_price`. It is bounded and mirrors the accepted KI-68.
- *Impact (Low):* systematic but dust-level. The floor is the normative design: floor the aggregate at each accrual and ceil per position at close, so that `receivable` never goes negative (I-P7). The docs never say that per-second cranks can wipe out all continuous rent on small markets.

**Disclosed?** Partly. The floor is documented design (ECON §2.1 and §7, ARCH §4.5). No known-issues entry covers the cumulative lender loss. KI-68 is the closest analogue.

**Fix direction.** Carry the remainder in a new per-market field, as `acc` already does with `carry`, and feed it into the next accrual. The impact reviewer confirms `receivable` still stays at or below Σ`credited_for`.

### I-05 · rewards_required reads the current venue kind, so the full-wipe cashback gate drops after a market leaves pump

**Low · Confirmed** · `programs/shortwin/src/engine/lender.rs:185-190`, used at `instructions/deposit.rs:141` · S1 #4, S2 #7

**Root cause.** `rewards_required` decides from the market's current venue kind. A curve market can move to None (curve complete, vT == 0, no canonical pool) and then to Raydium. Once it does, the `WIPE_REWARDS_CLAIMED` requirement disappears, and `sweep_trader` stops reading the pump UVA. Curve-era cashback is still in the trader's pump UVA, and anyone can claim it.

**Impact.** A depositor ends the wipe record before the cashback is swept, and the cashback then vests to the new lenders instead of the wiped ones. An attacker can force the order: `[sol_settle (wiping), sweep_trader, deposit(dust)]`, then `[claim_cashback, sweep_trader]`.
- The amount is bounded. Only legacy cashback coins are affected (30 bps of the trader's curve volume), and the keeper claims weekly once cashback reaches 0.01 SOL. Typically 0.01 to a few tenths of a SOL per event.
- It needs pump to change its migration target (an external event) and a full wipe.

**Reviewers.**
- *Reachability (Low):* nothing blocks the path. The keeper's post-wipe bundle protects the cashback only if it lands first. M6-SEC-5 fixed only the PumpMigrating variant of this bug.
- *Impact (Low):* `Market.pump_uva` survives the move to None and to Raydium. ECON §9.5's premise ("nothing can be claimed from it then") is false for the pump UVA.

**Disclosed?** No.

**Fix direction.** Three variants:
- S1 option A: record at the wipe whether the reward sweep is needed (a new market flag), and honour it in `deposit` and `sweep_trader`.
- S1 option B / S2 option A: require the reward sweep on every market whose `pump_uva` is set.
- S2 option B: also require it on graduated-elsewhere markets.

### I-06 · The min_notional floor admits positions with no buyback room at the liquidation boundary

**Low · Confirmed** · `crates/shortwin-math/src/params.rs:998`, with `risk.rs:263-270, 400-407` and `open_short.rs:606` · S1 #5, S6 #2

**Root cause.** The compiled bounds allow `min_notional` down to 10^7 lamports, and `check_econ` only range-checks it. K_s has a fixed part, 0.01 SOL by default. At small notionals the vault at `cr_liq` cannot cover B̂ + ert + K_s, so `buyback_allowed` is false and `liquidate`/`expire` refuse with `UseSolSettle`. `open_short` does not check for this.

**Impact.** These positions always end in S1, which pays lenders V − K_s, the borrower 0 and the keeper K_s (51 % of V for Tier A at the floor).
- At the Tier A boundary, about 0.0067 SOL per position moves from lenders to the keeper.
- A borrower who also acts as the keeper gets its margin back, which gives it a near-free option.
- Tiers A-C are affected. Tier D works at the floor.
- **Not reachable at the default** `min_notional` of 10^8. It needs Governance (3-of-5, about 96 h of timelocks) to set it below about 2.6·10^7 (Raydium) or 4.3·10^7 (PumpSwap) for Tier A.

**Reviewers.**
- *Reachability (Low):* the arithmetic is real but needs Governance to lower the value. "Always false" is slightly overstated: the check fails at the liquidation boundary but can pass at expiry after a price drop.
- *Impact (Low):* "every buyback" holds only for Tiers A-C. It is a bounds-derivation flaw against ECON §5.7/#77, which says the compiled bounds were re-derived so that no in-bounds value loses liquidations. The simulator does not model `min_notional`.

**Disclosed?** No. KI-12 and ECON's notes on small positions being S1-prone are related but different.

**Fix direction.** Options from the two reports:
- Raise the compiled floor: S1 says above about 2.63·10^7 at the Tier A defaults; S6 proposes 6·10^7.
- Refuse an open without buyback room at `cr_liq` (S1 option A, S6 option B).
- Scale or cap the fixed part of K_s (S6 options C and D).

### I-08 · On an empty non-wipe market, deposit-sweep-withdraw lets one lender take swept SOL owed to the protocol

**Low · Confirmed** · `crates/shortwin-math/src/pool.rs:1939`, payout at `pool.rs:1831-1837` · S2 #5, S5 #3

**Root cause.** `deposit_in` enforces the sweep gate only when `total_shares == 0` and a wipe is live. Take a market emptied by ordinary withdrawals: S = 0, no live wipe, ΣB_Q = 0. A dust deposit goes in first, then `sweep_trader` vests the swept SOL to the new shares instead of crediting `protocol_fees` (KI-10). A full withdraw then runs the last-lender flush, which pays out the whole unreleased vest in the same transaction.

**Impact.** Only `protocol_fees` loses.
- The amount is bounded by unclaimed venue cashback (legacy cashback coins, about 30 bps of volume, which the keeper claims weekly once it reaches 0.01 SOL) plus any donations.
- The attacker's cost is close to zero.
- No lender or borrower loses anything.
- Even an honest first depositor collects this SOL over 6 h.

**Reviewers.**
- *Reachability (Low):* the whole path runs in one transaction. The R3A-10 regression test only covers sweep-then-deposit, and the deposit-first gate (R4A-7) exists only for the post-wipe case.
- *Impact (Low):* a small, occasional loss of protocol revenue, about the size of R3A-10's original LOW finding.

**Disclosed?** No. KI-10 states the rule, and R3A-10 is marked FIXED.

**Fix direction.**
- S2: apply the sweep gate whenever `total_shares == 0`, and reset `wipe_swept` when the last shares leave.
- S5 option A: pay the last lender only the vest that existed before its latest deposit, and send the rest to `protocol_fees`.
- S5 option B: send the whole unreleased vest to `protocol_fees` when the last lender exits.

### I-09 · S2/S3 settlement credits Q·P_ema at once, at a frozen pre-halt price, to whoever holds shares

**Low · Confirmed** · `programs/shortwin/src/instructions/sol_settle.rs:257` · S1 #6. Scan 5 also raised it as a lead ("A deposit just before settle takes SOL at P_ema at once").

**Root cause.** `sol_settle` credits up to ⌊Q·P_ema/WAD⌋ + R_due immediately to the shares that exist at settlement. On a venue that cannot execute, P_ema is frozen at its pre-halt value, and the settlement box collapses to the EMA point, so nothing checks the price against the market. Deposits stay open during the S3 chain, `sol_settle` is permissionless, and the S3 time is public.

**Impact.** An attacker who holds cheap tokens bundles `[deposit, sol_settle, claim_sol, withdraw]` and swaps its tokens for SOL at the stale price. It gains f·Q·(P_ema − P_true), where f is its share of the pool.
- The honest lenders lose the same amount, but only out of a windfall. Their value never falls below the market value of their claims.
- The real harm is adverse selection: under KI-24 lenders carry the downside of the frozen mark, while outsiders can join only in the dump case and take the upside.
- ECON examples, assuming a 50 % dump and a whale at 9× the pool: about 8.3 SOL of a 9.2 SOL windfall in the frozen RA row, and up to about 225 SOL in the 500 SOL settlement example.

**Reviewers.**
- *Reachability (Low):* nothing in the code blocks it, but several conditions must hold together. The reviewer noted that the KI-24 asymmetry gives an argument for Medium.
- *Impact (Low):* the report overstates it. The attacker swaps its own tokens rather than taking SOL for free, and no principal is lost relative to market value.

**Disclosed?** Not as such. KI-24/KI-43 (frozen pre-halt P_ema) and R4B-6 are related.

**Fix direction.** Option A: vest the whole S2/S3 take (pass 0 for the immediate Q·P_ema and R_due parts). Option B: refuse deposits while the S3 chain runs (`no_route_since != 0`). The impact reviewer suggests vesting or capping the immediate part only when the venue is not executable.

### I-10 · Liquidation risk view prices at the live fee tier, which a same-transaction sale or burn raises

**Low · Confirmed** · `programs/shortwin/src/instructions/liquidate.rs:333`, read at `liquidate.rs:549` and `venues/mod.rs:842-912` · S3 #1. Scan 6 also raised it as a lead ("A same-transaction swap picks the fee tier of the robust quotes").

**Root cause.** Exit risk quotes in `liquidate::assess`, `keeper_exit`, `sol_settle` and `bounds::reference` are priced at min(φ_live, the position's hard cap).
- On PumpSwap, φ_live is chosen from a market cap computed on the live mint supply. The cap (500 bps by default) never binds, because the top tier is 125 bps.
- A burn or sale earlier in the same transaction therefore raises the tier.
- Opens resolve the fee at P_ema (M7-SEC-5); exits do not.

**Impact.** One tier step is 2-5 bps, about 0.05 % of B̂.
- Only positions within about 0.05 % of `cr_liq` on both legs (and on all 3 ring entries) can be flipped, and only while the market cap sits just above a tier threshold. An ordinary price tick would liquidate those positions anyway.
- The largest jump (25 → 95 bps) exists only at the 420 SOL boundary on some pools.
- PumpSwap only. The curve and Raydium are not affected.
- The borrower pays the penalty somewhat earlier. Lenders lose nothing.

**Reviewers.**
- *Reachability (Low):* reachable on PumpSwap. A burn raises the tier without moving price. A sale partly offsets itself, because the robust spot drops with it.
- *Impact (Low):* "liquidates a healthy position" is overstated. What is actually taken is an earlier liquidation and the borrower's small chance of recovering.

**Disclosed?** Partly. The design says exit risk quotes use the live tier (ARCH §4.3, I16, ECON R3B-7(b)). M7-SEC-5 fixed this only for opens. No known-issues entry covers it.

**Fix direction.** Option A: cap the exit risk fee at φ resolved at P_ema (`cap_fee_ppm`), using `read_state_boxed_for_open` in `keeper_exit` and `sol_settle` and the same cap in `bounds::reference`. Option B: in addition, resolve φ at P_ema on a supply snapshot stored at each EMA roll, so a burn cannot raise it either.

### I-12 · A new lock era resets b_min but not Π, so Π compounds across eras down to 1

**Low · Confirmed** · `crates/shortwin-math/src/pool.rs:1806-1808`, Π reset only at `pool.rs:1890-1896` · S5 #5. Scan 2 also raised it as a lead ("Π can fall to 1 and stop the decay of lender locks").

**Root cause.** When all locks have expired (`lock_norm` reaches 0) and a new lock starts, `withdraw_in` resets `b_min` to the current ΣB_Q but keeps Π. Π returns to WAD only on a full repay (ΣB_Q = 0). Each partial borrow/repay era multiplies Π by B'/b_min (rounded up), so Π compounds across eras down to 1. From then on `lock_of = ⌈lock·Π/lock_pi⌉` never shrinks.

**Impact.** Locks stop decaying as borrowers repay. They release only at `lock_until`: about 31 days by default, up to about 101 days.
- **Attacker path:** about 8 eras at the 25 SOL market cap reach Π = 1, with a min-notional position kept open throughout. The attacker pays fees and rent and gains nothing.
- **Organic drift:** much slower, about 53 eras at a ratio of 0.5.
- **Recovery:** one ΣB_Q = 0 event clears it completely.
- No funds are lost, and I-P4 holds.

**Reviewers.**
- *Reachability (Low):* reachable with permissionless roles. It matches `pool_model.py` exactly, so it is a spec-level flaw, not a porting slip.
- *Impact (Low):* smaller than the title says, because locks still expire at `lock_until`. It pushes the fading-interest case down to the lock_until-only worst case, which the design already accepts for busy markets.

**Disclosed?** Partly. ARCH §9 M0b note (1) proposes this same fix but discusses only the overflow in a single era and calls it unreachable. The issue is not in docs/AUDIT.md or the known-issues register.

**Fix direction.** Option A: set `v.pi = WAD` when `b_min` resets. Option B: roll the Π epoch whenever `lock_norm` reaches 0 (increment `pi_epoch`, set Π to WAD, clear `lock_cal`), as the ARCH note proposes.

### I-13 · new_raydium seeds L_slow at the 2D anchor instead of capping it at the locked depth D

**Low · Confirmed** · `crates/shortwin-math/src/observe.rs:398-402`, callers `programs/shortwin/src/engine/observe.rs:84` and `venues/pump_amm.rs:887` · S5 #6 (confidence 70, below the report's threshold). Scan 2 also raised it as a lead ("A Raydium price start lets L_slow reach twice the depth").

**Root cause.** A Raydium price start (a `create_market` listing, or the first adoption on a list-only market that never had a price) seeds L_ema = L_slow = min(live √(X·Y), ⌊D·10⁴/λ_min⌋). That is 2D at the default λ_min of 50 %. An adoption caps L_slow at D instead. Both callers are permissionless, so a deposit wrapped around the price start seeds L_slow at up to 2D on a pool whose honest depth is D, and nothing caps it later on Raydium.

**Impact.** At the first open, after the 3,600 s listing window and the warm-up, L_slow is about 1.37-1.42·D instead of D. The excess decays by about 1/e per hour.
- An attacker who wraps a deposit around its own open can size a position at about 1.4× the honest cap, or up to 2× if `pool_list_window_s` is set to 0.
- Lenders lose only if a genuine pump lands within the decay window (the position goes to S1).
- The credit never exceeds the already-accepted 2D cap.
- The PumpSwap price start is not affected.

**Reviewers.**
- *Reachability (Low):* reachable on both Raydium paths, with about 1× the pool's reserves in one transaction. The owner fixed the analogous PumpSwap residual as M6R-1.
- *Impact (Low):* it does not compound with the 2D reference cap (I-14), because every Raydium reader caps at the same 2D. The issue list says it compounds; this reviewer found it does not. An attacker can also reach most of this without the bug through deposit-wrapped cranks after the warm-up, and the two converge within a few hours.

**Disclosed?** Not in the known-issues register. The behaviour matches the spec (ECON §5.2), and the wider class is owner-accepted (SPEC §14 M6D-1). The docs never compare the price start with adoption's L_slow ≤ D cap.

**Fix direction.** The scan-5 report has no Fix block, because the finding was below its confidence threshold. The finding contrasts this path with adoption, which keeps L_slow ≤ D, and with PumpSwap's warm-up clamp.

### I-14 · Raydium ref_depth_cap credits 2D, so a held-then-pulled deposit lifts L_b over the live pool

**Low · Confirmed** · `programs/shortwin/src/engine/bounds.rs:48-50`, with `crates/shortwin-math/src/bounds.rs:100-111, 166-168` · S6 #5

**Root cause.** On Raydium, `ref_liquidity` caps its EMA terms at ⌊D·10⁴/λ_min⌋, which is 2D at the default. Since M6E-1, PumpSwap uses λ_min = 100 %, which caps them at D. On Raydium, a deposit held for about an hour feeds L_slow. Pulling it at the deadline leaves L_b near 2D while the live depth is D, so expire fails with `ExitBoundNotMet` until L_slow decays.

**Impact.** Expiry is delayed by tens of minutes to about an hour. The project measured +1,874 s and +3,326 s for 2× and 3× deposits on PumpSwap before M6E-1, and a delay over an hour needs more than 3× the pool. All of these are needed:
- λ_live of at least about 0.83-0.86;
- a fee of at least about 0.66 %, i.e. 1-4 % CPMM pools, not AMM v4 or 25 bps pools;
- a position at about 80 % or more of the impact cap;
- 2-3× the pool in both assets, held for about an hour.

Rent keeps accruing and no lender principal is lost. The liquidation half of the claim does not hold: δ_liq of 2 % covers the 1.27 % gap.

**Reviewers.**
- *Reachability (Low):* reachable for expiry. It is the same math as M6E-1, which the project reproduced on PumpSwap with 3,346 refused expiries.
- *Impact (Low):* a liveness delay that costs heavy capital to cause, with no fund loss.

**Disclosed?** Not in the known-issues register. ECON §8.2 discloses the up-to-2× Raydium lift as a deliberate trade-off, but the Raydium expiry hold is not spelled out.

**Fix direction.** Option A: cap the Raydium EMA terms at D, as M6E-1 does on PumpSwap (call `bounds::ref_depth_cap(pool_d, 10_000)`). Option B: cap at the pool's own depth, ⌊D·10⁴/max(λ_min, λ_live)⌋.

### I-15 · Settlement box takes L_hi from the upper median of bucket highs, which deposit-wrapped cranks raise

**Low · Confirmed** · `crates/shortwin-math/src/settle.rs:87-92` · S6 #1

**Root cause.** `box_of` sets `l_hi = max(L_ema, L_mhi)`, where L_mhi is the upper median of the bucket liquidity highs, with no depth cap. A deposit wrapped around any crank raises that second's high, and honest records in the same second cannot lower a maximum. Wrapping cranks in 4 of every 8 seconds sets L_mhi at will. That widens the box, so the S1/S2 legs and the S2/S3 amount check fail.

**Impact.** It only defers settlement, and only while the attacker keeps wrapping (about 2,500 lamports a second in fees, plus pool-sized capital in both assets each time).
- **S2:** unreachable before S1 at default parameters anyway.
- **S1:** blockable only in a narrow price band, about the position's price impact (roughly 1.5-6 %).
- **S3:** not affected on a halted venue, where the box collapses after 8 s.
- **Who loses:** the borrower gets a free option, and lenders carry the price risk while the deferral lasts. The attacker cannot force or misprice a settlement.

**Reviewers.**
- *Reachability (Low):* no code blocks it, but the impact is smaller than the title. The cost is below KI-07's figure.
- *Impact (Low):* a disclosed, bounded residual.

**Disclosed?** Mostly. It is covered by KI-07 "Settlement deferral" and ECON §5.6 "Residual — deferral by widening", whose M10 G-E4 measurement shows wraps doubling L_hi. ECON §10.5 and §12.11 also cover it. KI-07's cost figure understates the LP-wrap variant.

**Fix direction.** Option A: take L_hi from the upper median of the bucket lows, which only attacker-only seconds can raise. Option B: cap L_mhi at the M4F-1 depth cap, as `ref_liquidity` does.

### I-16 · evaluate step 2 refreshes a lapsed arming clock instead of letting it re-arm

**Low · Confirmed** · `crates/shortwin-math/src/settle.rs:557-564` · S6 #3

**Root cause.** In step 2 of `settle::evaluate` (not liquidatable, not expired), an armed position that fails `not_liquidatable` gets `last = now` with no `is_live` check. A clock that has already lapsed (no evaluation for more than `arm_gap_s`) comes back with its old count. The next step-3 call then sees `updates ≥ persist_n` and settles S1/S2 at once. Step 3 handles a lapsed clock correctly.

**Impact.** It skips the persistence window: 40 EMA updates by default, about 39 s.
- Nothing is fabricated. The settlement still needs the S1/S2 legs to hold on the box, unrefuted.
- The borrower loses the chance of a reversal during the skipped window. In S1 territory its equity is already below K_s + ert (at most about 0.26 SOL).
- Lapses are rare: a keeper outage, a halted venue, or a long warm-up. The honest keeper itself sends the call that revives the clock.

**Reviewers.**
- *Reachability (Low):* no code blocks it. An attacker can force step 2 in one transaction with a dump or an LP deposit that makes the spot leg false. The I12 invariant hook does not catch it.
- *Impact (Low):* it weakens timing; there is no theft path.

**Disclosed?** No. The code matches ARCH §3.8 step 2 and the sim, but contradicts ECON §5.6, which says evaluations more than `settle_arm_gap_s` apart re-arm the clock.

**Fix direction.** Option A: leave a lapsed clock for step 3. Option B: also clear a lapsed arm. The sim and ARCH §3.8 need the same change.

```diff
-            } else {
+            } else if state.is_live(now, terms.arm_gap_s) {
                 state.last = now;
```

### I-17 · Under the post-change guard, the spot-leg liquidity floor reads bucket lows that withdraw-wrapped cranks lower

**Low · Confirmed** · `crates/shortwin-math/src/bounds.rs:156-158`, guard branch at `bounds.rs:117-122` · S6 #6. Scan 5 raised a related lead ("An LP who wraps cranks with withdrawals keeps ema.guard set").

**Root cause.** After a venue restart, while `ema.guard` is set, `spot_liquidity` floors L_live at min(L_ema, lower median of the bucket lows). Bucket lows are per-second minimums. An unlocked LP who runs `[withdraw, crank, deposit]` in 4 of every 8 seconds sets that median to the drained depth D, even when the keeper also cranks in those seconds. The same wraps keep the guard from clearing, so the spot leg is judged at D.

**Impact.** Liquidation still needs the EMA leg as well, and that leg stays near the honest depth. So the only positions exposed are those in the spike-and-retreat band (in the sims, about 2.01-2.07× against a 2.078× threshold).
- The borrower pays the penalty (0.64-1.00 SOL on the fork reproducer positions, 30 % of it to the attacker as keeper) plus an overpay of at most about 4 %.
- Lenders are made whole.
- It needs a venue restart and an attacker who owns unlocked LP.
- The outcome stays inside the owner-accepted KI-23 (FITX-1) envelope.

**Reviewers.**
- *Reachability (Low):* a cheaper route into the accepted FITX-1 class. It contradicts the stated claim that nothing is forced while the guard is set, and the `fits2_a_*` test does not catch it.
- *Impact (Low):* the new exposure is small, but the docs (ECON §12.12 residual 3, ARCHITECTURE.md:909) say this cannot happen.

**Disclosed?** Partly. KI-20 describes the same setup but says it "forces no exit", which is wrong for the spike-and-retreat band. The result is within KI-23, which the owner accepted on 2026-09-29.

**Fix direction.** Option A: floor at the unguarded `ema.liq`. Option B: while the guard is set, floor at min(L_ema, upper median of the bucket highs). The impact reviewer prefers Option B, because it keeps the protection against deposit pushes, which Option A drops.

### I-04 · apply_config installs new governance/pause keys without their signatures

**Informational · Confirmed** · `programs/shortwin/src/state/config.rs:187-196`, handler `instructions/apply_config.rs:85` · S1 #3, S2 #4

**Root cause.** For an `Authorities` change, the permissionless `apply_config` checks only the new `fee_recipient` account. `GlobalConfig::apply` then writes `governance`, `pause_authority` and `coverage_authority` directly. The only checks on those keys are non-zero and pairwise distinct. No new key signs, and there is no accept step.

**Impact.** If Governance approves a mistyped key, a key nobody controls gets installed. The loss is not permanent:
- The program upgrade authority is Governance vault 0, held in ProgramData, which this change does not touch. A recovery upgrade after the 48 h Squads time lock restores the config.
- A wrong pause or coverage key can be rotated by the still-working Governance.
- No funds are at risk.

It is also unlikely: the change must pass a 3-of-5 approval and about 96 h of public visibility, and the supported `stage-authorities` tool derives the keys from the layout instead of taking hand-typed ones.

**Reviewers.**
- *Reachability (Informational):* only Governance's own mistake can trigger it, and the upgrade authority recovers from it.
- *Impact (Informational):* at worst a temporary freeze of governance-only actions. It is a hardening note about two-step acceptance.

**Disclosed?** Not as such. KI-61 covers only that nobody can cancel a pending `Authorities` change in time.

**Fix direction.** Option A: require the new governance and pause keys to sign `apply_config`. Option B: two steps. `apply` writes pending keys, and a new `accept_authorities` instruction, signed by the new keys, moves them in.

### I-11 · withdraw_in resets b_min only when lock_norm == 0, so later locks keep a stale low b_min

**Informational (reviewers split Low / Informational) · Confirmed** · `crates/shortwin-math/src/pool.rs:1806-1808`, decay test at `pool.rs:1898-1901` · S5 #4

**Root cause.** `withdraw_in` resets `b_min` to ΣB_Q only when `lock_norm == 0`, and it tests this before the lender's own live term leaves the calendar. While any lock is live (another lender's, the lender's own, or rebase residue), a new lock set at a higher ΣB_Q inherits the earlier low `b_min`. `resolve_in` decays Π only when ΣB_Q falls below that `b_min`.

**Impact.** Later lockers stay over-locked until ΣB_Q drops below the old `b_min`, or until `lock_until` (about 31 days by default, at most about 101). Over-locking is the conservative direction: I-P4 holds, and no tokens or SOL are taken. The only value effect is conditional. Unlocked co-lenders can exit first and push more of a later settlement loss onto the over-locked lender; this is the run residual already disclosed as KI-13. In the reachability reviewer's example, about 33 of a 100-token loss moves this way.

**Reviewers.**
- *Reachability (Low):* any lender reaches the mechanics, but this is the specified single-index design, not a coding slip.
- *Impact (Informational):* intended design, and "never decay" is wrong. The report's Option B would reopen V2-3 (a runner re-basing its own lock with a dust re-lock).

**Disclosed?** The behaviour is intended design: ARCH A57, ECON §9.3, V2-3, and the `scripted_overlock` test vector. Its value side is KI-13. The specific case of later locks inheriting an earlier lock's `b_min` has no known-issues row. The impact reviewer says the real gap is in the docs: ECON's "94 % within 24 h" holds only for the lender that set `b_min`.

**Fix direction.** S5 proposes four options:
- A: a decay base per lock or per cohort;
- B: re-base on a higher lock;
- C: reset `b_min` from a count of live locks;
- D: test `lock_norm` after the lender's own term leaves.

The impact reviewer warns that Option B reopens V2-3. It also notes that the current ordering (testing before the lender's own term leaves) is what stops a lender re-basing its own lock, and Option D changes that ordering. A doc fix to ECON §9.3 is the minimum.

---

## Disputed and refuted issues

None. Both reviewers confirmed every issue.

---

## Leads worth a look

**Unverified.** These leads come from the reports' Leads sections and were not re-checked by the reviewers. Each report says it could not complete the full exploit path for its leads.

1. **A borrower whose address refuses lamports blocks its own liquidation.** `shortwin::liquidate`, also `expire` and `sol_settle` (S1; 3/3 runs for liquidate). Exits credit only the borrower account. A borrower who deploys a program at its own address may make every liquidate, expire and executing `sol_settle` fail, so lenders never get the tokens back. The report did not prove that the cluster still refuses lamports into an executable account.
2. **ExtendProgram in every slot may block keeper exits on a venue.** `shortwin::liquidate` (S4; 3/3 runs). The loader allows one extension per program per slot, at about 0.071 SOL each under SIMD-0431. With delay visibility, every later CPI to that venue in the same slot fails. Not verified on mainnet.
3. **A live fee between the locked and global hard caps leaves positions with no exit.** `route::s3_no_route`, `risk::buyback_allowed` and `settle::s1_legs` (S1, S2, S6). Suppose Governance raises `venue_fee_hard_cap_bps` and a venue fee lands between a position's locked cap and the new global cap. The buyback then fails at the live fee, S1 fails at the capped fee, and S3 stays off. No report showed a real fee reaching that band.
4. **An LP withdrawal inside sol_settle removes the live veto on S1 and S2.** `shortwin-math::s1_legs` / `s2_legs` (S6; 3/3 and 2/3 runs). Both read the raw L_live, which a withdrawal in the same transaction drains, leaving only the box corner to veto S1 (where the borrower gets 0).
5. **S1 credits a take above market value at once, with no vest.** `shortwin::sol_settle` / `shortwin-math::settle` (S1 3/3, S6 1/3). V − K_s can exceed ⌊Q·P_ema⌋ + R_due by up to the corner's impact plus fee (about 2.3 SOL in the S6 example). A deposit made just before an S1 therefore takes a share. R4B-6 vests the same excess, but only for S2 and S3.
6. **One flat price step every few seconds blocks liquidation during a fast pump.** `shortwin-math::sustained_growth` (S5; 1/3 runs). Recording one low price every `kappa_growth_n` seconds keeps the growth term at 0. κ_eff then falls to its base value, so in a pump faster than about 34 bps/s P_ema lags spot by more than δ_liq. The cost was not compared with KI-05.
7. **Exits read pool_lock_min_bps live, not the value locked at open.** `engine/bounds.rs::ref_depth_cap` (S1 3/3, S2 3/3, S6 1/3). A Governance change from 5,000 to 2,000 bps moves the Raydium exit cap of open positions from 2D to 5D. ECON measured 3D and 5D caps on PumpSwap at −31.1 % and −52.3 % of Q in S1.
8. **A pending EnableVenues change undoes disable_venue.** `shortwin::apply_config` (S1 and S2; 3/3 runs each). `disable_venue` leaves the pending change in place, and anyone can apply it once the timelock passes. Opens then resume on a venue the Emergency just disabled, unless the Emergency cancels the change in time.

---

## Coverage notes

- The six scans cover the 106 core files of docs/AUDIT.md §2: instructions (32 files), state and engine (32, including `programs/complete_curve`), venues (7), the venue-state crate (10), math core (4) and math rest (21).
- The keeper core is in scope in AUDIT.md but was not covered, because the skill targets on-chain code.
- `venues/mock.rs` and `mock_cpamm.rs` were excluded as test-only.
- Every scan ran 12/12 agents on all 3 passes. Each scan started with empty scan memory.
- The report confidence threshold was 75. I-13 came from a finding at 70 that the report lists below the threshold.

---

## Links

The six rust-auditor reports, next to this file:

1. Scan 1, instructions: [report-1-instructions.md](report-1-instructions.md)
2. Scan 2, state and engine: [report-2-state-engine.md](report-2-state-engine.md)
3. Scan 3, venues: [report-3-venues.md](report-3-venues.md)
4. Scan 4, venue-state crate: [report-4-venue-state.md](report-4-venue-state.md)
5. Scan 5, math core: [report-5-math-core.md](report-5-math-core.md)
6. Scan 6, math rest: [report-6-math-rest.md](report-6-math-rest.md)

`verification.json` holds both reviewers' full reasoning for every issue.
