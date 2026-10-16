# AI review of short.win v1.0.0

An AI-assisted security review of short.win's on-chain core, the triage of its leads, and a
branch of proposed fixes. The review used Claude Opus 5.5 with the `rust-auditor` Claude Code
skill.

**This is not a formal audit.** The fixes on `ai-review-fixes`, and their follow-ups on
`air-followups`, were proposals for the owner's review; the owner decided them on 2026-10-12
and 2026-10-13 ("Decisions for the owner"). They passed local runs of the CI gate (below), but
none has run in CI or had an external fix review, which [`../findings.md`](../findings.md)
requires before a finding counts as fixed. The ≥ 24 h fuzz run that file also requires was
waived by the owner for this batch on 2026-10-13; none was run on the fixed program.

## Scope and method

- **Reviewed commit: `55b3e23`** (2026-10-08). Six scans covered the 106 core files of
  [AUDIT.md §2](../../AUDIT.md): instructions; state and engine; venues; the venue-state crate;
  math core; math rest. Each scan ran 3 passes of 12 agents. The findings were merged by root
  cause. Two independent reviewers then re-checked each merged issue, one for reachability and
  one for impact ([`SUMMARY.md`](SUMMARY.md), [`verification.json`](verification.json)).
- **The audit tag.** `audit-1` / `v1.0.0` (`e4895c2`) is 8 commits after `55b3e23`. None of them
  touches `programs/`, `crates/` or a file listed in [`scope.txt`](../scope.txt):
  `git diff 55b3e23 v1.0.0 -- programs crates` is empty. Every issue therefore applies to v1.0.0
  as reported, at the same file and line.
- **The build.** A local build of `main` @ `9aade3e` (v1.0.0 plus one docs commit), with the
  pinned toolchain (Rust 1.98.1, Agave 4.1.2, Anchor 1.2.0, platform-tools v1.57, SBPF v3),
  reproduces AUDIT.md §0's hashes byte for byte: `shortwin.so` `4d19c582…`, `complete_curve.so`
  `f3a47201…`. The regression tests below fail on that code.
- **Leads.** The summary listed eight "leads worth a look" that its reviewers had not checked.
  Each was triaged by hand against the code, the Agave source and the normative sims
  ([`leads.md`](leads.md)). Seven are confirmed (AIR-18 … AIR-24) and one is refuted (L-1).
- **Not covered.** The keeper core is in scope in AUDIT.md, but it was not scanned: the skill
  targets on-chain code. Where a fix changes what the keeper must do, the follow-ups below say
  so.

**IDs.** AIR-01 … AIR-17 are the summary's I-01 … I-17. AIR-18 … AIR-24 are leads L-2 … L-8.

## Status

| ID | Issue | Severity | Status | Commits | Regression test |
|---|---|---|---|---|---|
| AIR-01 | The venue latch keys on the ProgramData deploy slot, which a permissionless `ExtendProgram` rewrites | Medium | Fixed; residuals (decision 5) | `86f4004` | `latch_raydium_it`, `latch_pump_it`: `air01_*` |
| AIR-02 | `vest_in` rounds the vest end up, so dust sweeps keep pushing `vest_end` later | Low | Fixed | `c1ce39f` | `pool::tests::dust_lumps_do_not_push_the_vest_end` |
| AIR-03 | `lender_credit` floors each accrual's rent credit and carries no remainder | Low | Fixed | `2a4e107` | `engine_it` `prologue::per_second_cranks_credit_a_small_positions_rent` |
| AIR-04 | `apply_config` installs new governance and pause keys without their signatures | Informational | No change; known-issues row (decision 7) | — | — |
| AIR-05 | `rewards_required` reads the current venue kind, so the wipe's cashback gate drops after a market leaves pump | Low | Fixed | `1e938e8` | `pump_it` `graduation::air05_a_full_wipe_after_the_market_left_pump_waits_for_the_curve_cashback` (since pump-final P2, short.win 1.0.1, the test runs on a non-cashback curve with lamports seeded into the trader's pump UVA, which pump's `claim_cashback` pays out: v3 refuses live cashback curves, owner D1 (a); the name and every wipe-gate assertion kept, T-WIPE-GATE) |
| AIR-06 | The `min_notional` floor admits positions with no buyback room at the liquidation boundary | Low | Fixed | `5bb8d87` | `params::tests::min_notional_floor_keeps_buyback_room_at_the_liquidation_boundary`; `it` `admin::min_notional_below_the_buyback_room_floor_is_refused` |
| AIR-07 | `maybe_roll_in` flushes the unreleased vest only on a full wipe, not on a rebase roll | Medium | Fixed; the drained-pool gate is separate (decision 2) | `e1f873a`; the drained-pool gate `00d3772`; indexer `b093545` | `pool_econ` `a_dust_deposit_before_the_last_settlement_takes_no_vest`, `a_deposit_above_a_thousandth_cannot_keep_the_vest_from_the_old_lenders`; `lender_it` `air_07_*` |
| AIR-08 | On an emptied non-wipe market, deposit-sweep-withdraw takes swept SOL owed to the protocol | Low | Fixed (keeper too) | `20009fd`; keeper `21229b6` | `pool_econ` `a_deposit_into_an_emptied_pool_waits_for_the_sweep`; `lender_it` `air_08_*`; `m5_sec_curve_it` `air_08_*` (since pump-final P2 on a non-cashback curve with lamports seeded into the trader's pump UVA after the close, as AIR-05's; T-WIPE-GATE) |
| AIR-09 | S2/S3 settlement credits `Q·P_ema` at once, at a frozen pre-halt price | Low | Fixed | `907994e` | `lender_real_it` `jit::air_09_a_jit_whale_around_an_s3_on_a_halted_venue_realises_nothing` |
| AIR-10 | The liquidation risk view prices at the live fee tier, which a same-transaction sale raises | Low | Fixed for sales; burns remain (decision 3) | `cece6fa`; fuzz oracle `84cc9c9` | `m7_sec_it` `pumpswap::air_10_a_same_transaction_sale_does_not_raise_the_exit_risk_fee` |
| AIR-11 | `withdraw_in` resets `b_min` only when `lock_norm == 0`, so later locks keep a stale low `b_min` | Informational | No change: intended design; doc wording (decision 7) | — | — |
| AIR-12 | A new lock era resets `b_min` but not Π, so Π compounds across eras down to 1 | Low | Fixed | `e153169` | `pool_econ` `lock_decay_restarts_with_every_lock_era` |
| AIR-13 | `new_raydium` seeds `L_slow` at the 2D anchor instead of capping it at the locked depth D | Low | Fixed | `07c457b`; tests `e34e285`, keeper scenarios `3fc5e78` | `engine::pool_select::tests::a_price_start_seeds_l_slow_at_most_the_locked_depth` |
| AIR-14 | Raydium's `ref_depth_cap` credits 2D, so a held-then-pulled deposit lifts `L_b` over the live pool | Low | Fixed | `ead059f` | `m4r_it` `air14_a_deposit_pulled_at_the_deadline_does_not_hold_a_raydium_expiry` |
| AIR-15 | The settlement box takes `L_hi` from the upper median of bucket highs, which deposit-wrapped cranks raise | Low | Fixed | `1ab0928` | `settle::tests::deposit_wrapped_highs_widen_l_hi_only_up_to_the_depth_cap`; the G-E4 LiteSVM tests (`lp_around_settle`) |
| AIR-16 | `evaluate` step 2 refreshes a lapsed arming clock instead of letting it re-arm | Low | Fixed | `6977467` | `settle::tests::step_2_does_not_revive_a_lapsed_clock`; `settle_it` `more::a_stale_bucket_keeps_the_clock_and_a_long_gap_re_arms` |
| AIR-17 | Under the post-change guard, the spot-leg floor reads bucket lows that withdraw-wrapped cranks lower | Low | Fixed | `7343a63`; tests `9323f98` | `bounds::tests::withdraw_wrapped_cranks_do_not_lower_the_guarded_spot_floor` |
| AIR-18 | L-2: `ExtendProgram` blocks a program for the rest of its slot | Low | Operational (decision 4) | — | — |
| AIR-19 | L-3: a live fee between a position's locked cap and the global hard cap leaves it with no forced exit | Low | Fixed; conservative mark (decision 6) | `7bd84e7` | `route_it` `hard_cap::air_19_a_fee_above_the_locked_cap_is_no_route_for_that_position` |
| AIR-20 | L-4: an LP withdrawal inside `sol_settle` removes the live veto on S1/S2 | Informational | Documented and pinned; no behaviour change (decision 7) | `eb02191` | `settle::tests::a_drained_live_state_leaves_the_box_corner_as_the_only_s1_veto` (pins the residual) |
| AIR-21 | L-5: S1 credits a take above market value at once, with no vest | Low | Fixed | `e05315f` | `lender_real_it` `jit::air_21_a_jit_whale_around_an_s1_takes_none_of_its_excess` |
| AIR-22 | L-6: one flat price step every few seconds zeroes the sustained growth and blocks liquidation in a fast pump | Low | Fixed; trade-off (decision 1) | `6c39e58`; tests `03b7762`, `8a4df78` | `observe::tests::one_flat_step_in_three_keeps_the_sustained_growth` |
| AIR-23 | L-7: exits read `pool_lock_min_bps` live, not the value locked at open | Low | Fixed by AIR-14; pinned | `f726bc6` | `m4r_it` `air23_a_governed_pool_lock_min_bps_moves_no_open_positions_exit_bound` |
| AIR-24 | L-8: a pending `EnableVenues` change undoes `disable_venue` | Low | Fixed | `a113886` | `it` `admin::air24_disable_venue_cancels_a_pending_enable_of_that_venue` |
| — | L-1: a borrower whose address refuses lamports blocks its own liquidation | — | Refuted ([`leads.md`](leads.md)) | — | — |

Each regression test fails on v1.0.0 and passes with the fix, except AIR-20's, which pins the
residual. Each fix commit message gives the root cause, the fix, the options weighed and the
test's figures before and after. Where a fix changes a normative sim (`docs/design/sim/`), the
same commit regenerates the vectors, so every commit passes the vectors check on its own.

**The branch.** It holds 32 commits on `main` @ `9aade3e`, in a linear history: 30 fix and test
commits, one that refreshes the CU benchmark figures, and the one that adds this folder. The
fixes were written in six independent groups and then combined. Where two fixes met, the later
commit carries the reconciliation:
- AIR-19 (`7bd84e7`) places its field after AIR-03's `lender_credit_rem`.
- AIR-14 (`ead059f`) also moves AIR-15's settlement-box cap to D, with its tests and the
  PumpSwap fuzz oracle. On Raydium it gives up V2-14's `L_slow` credit above D. That credit
  was added against FITV-2, a forced liquidation wrapped in an unlocked LP's withdrawal. The
  keeper's FITV-2 contest still liquidates nothing on this branch: the honest keeper does not,
  and all 390 of the attacker's wrapped liquidations are refused.
- AIR-08 (`20009fd`, `21229b6`) keeps AIR-05's pump-UVA markets in the sweep gate.
- The drained-pool gate (`00d3772`) updates the stranding-table pins that it and AIR-03 move
  together. It also makes the pool-vector generator's random withdrawals keep the last share
  while tokens are borrowed, as the program requires (`LastSharesBorrowed`): with deposits
  refused in drained pools, one trace reached that burn and T-POT's full replay stopped there.
- The keeper's freeze scenarios (`3fc5e78`) wait for `L_slow` after a listing, as AIR-13's
  harness commit does for the LiteSVM tests. Without the wait, seven scenarios opened smaller
  positions in a riskier tier, or none.

## Decisions for the owner

The owner resolved decisions 1–7 on 2026-10-12 and decisions 9–12 on 2026-10-13. Each item
keeps its analysis and ends with the resolution; item 8 lists the interface changes and needs no
decision.

1. **AIR-22, sustained growth.** The fix ends the flat-step veto: a straddle crank every 3 s no
   longer blocks liquidation through a pump. The cost is that one non-rising step in three is
   now tolerated, so the sim's held-sandwich plan against a liquidatable borrower needs 2 held
   seconds instead of 3. On RA the attacker nets +2.839 SOL instead of +1.981. The borrower
   loses the same remainder (5.775 SOL), and lenders plus protocol lose less (0.049 / 1.257 SOL
   instead of 0.570 / 2.272). Keeping 3 s needs `kappa_growth_n` ≥ 5, which is outside its
   current box. To keep the old trade-off, revert `6c39e58`, `03b7762` and `8a4df78` and
   regenerate the vectors.

   **Resolved (owner, 2026-10-12): keep the lower median.** The trade-off is known-issues KI-76.

2. **AIR-07, the drained-pool deposit gate.** The base fix flushes the vest when a settlement
   leaves at most 1/1,000 of the token claims it found. That left a residual: an outsider who
   deposited more than that before the last settlement kept the vest vesting and diluted the
   old lenders (4.61 of a 5 SOL lump in the test). A separate commit closes it. `deposit` now
   refuses with `PoolDrained` (6108) while `S > 0`, `ΣB_Q > 0` and `1,000·T ≤ T + ΣB_Q`. Any
   repayment above 0.1 % of `ΣB_Q` reopens deposits. The cost falls on honest depositors: in
   the sims 26 of 6,653 deposits in the ECON §9.6 fuzz (90 % cap), 102 of 5,910 in the
   lock-heavy mix and 378 of 26,749 in the stranding table are refused, and while a pool is
   drained no newcomer can supply exit liquidity. To accept the residual instead, revert
   `00d3772` and regenerate the vectors.

   **Resolved (owner, 2026-10-12): keep the gate** (`PoolDrained` = 6108). Its cost to honest
   depositors is known-issues KI-71.

3. **AIR-10, burns.** A holder's burn earlier in the same transaction still raises the
   PumpSwap tier, because the fee at `P_ema` is resolved on the live mint supply. A test pins
   this. Closing it needs a per-second supply snapshot in `Market` (reserved bytes). Accept it
   with a known-issues row, or add the snapshot.

   **Resolved (owner, 2026-10-12): accept the burn residual** with known-issues KI-72; no
   snapshot.

4. **AIR-18, `ExtendProgram` griefing.** No program change can stop third-party extensions.
   Anyone can extend `shortwin` (or `complete_curve`) once per slot and block it for the rest
   of that slot, at ≈ 0.052 SOL of rent per slot at mainnet's 5,080 lamports/byte. The griefing
   ends for good once the program reaches the 10 MiB cap, after ≈ 870 slots for `shortwin`
   (≈ 45 SOL, paid by the attacker; ≈ 1,010 slots, ≈ 53 SOL, for `complete_curve`, each if
   deployed at its v1.0.0 ELF size). The options are:
   - extend `shortwin`'s ProgramData to 10 MiB at deployment (≈ 45 SOL of extra rent);
   - or accept the bounded griefing, have the keeper retry in the next slot, and correct
     KI-63 ("harmless").

   **Resolved (owner, 2026-10-12): accept the bounded griefing.** No pre-extension to 10 MiB.
   The keeper ticks every 400 ms by default, about one slot, so it sends a blocked exit again
   at its next tick, in a later slot (since `8945087` with no alert; a 30-s run of such
   refusals is paged once). KI-63 is corrected, the bounds are known-issues KI-73 (recomputed
   on 2026-10-13: `leads.md` L-2's erratum), and `services/README.md` "Governance" says what
   operators see.

5. **AIR-01, operations.** After `initialize_config`, run one `revalidate_venue` with the
   expected ELF hash for each venue whose ProgramData can still grow (pump-fees, AMM v4, CPMM,
   Jupiter). This records the fingerprints. Until then, one extension can still latch a venue.
   `shortwin-gov stage revalidate-venue` (package `shortwin-ops`) prints and stages the hash. A
   flag or revalidation after an extension hashes the trimmed ELF: request ≥ 0.5 M CU (≤ 0.44 M
   measured).

   **Resolved (owner, 2026-10-12): the runbook step**, in `services/README.md` "Governance".
   `shortwin-gov` sets no compute-unit limit, so it gets the runtime's default: 200,000 CU for
   a bare `execute`, 400,000 for `approve --execute` (two Squads instructions). Neither covers
   pump-fees' hash path, so an execution after an extension goes through a client that
   requests ≥ 0.5 M CU (Jupiter's cost is not measured: sized by simulation). A new staging at
   the current deploy slot also works, unless another extension lands before it executes. The
   residuals are known-issues KI-69 and are accepted.

   *2026-10-13:* the runbook now asks for ≥ 0.6 M CU, or a limit sized by simulation. The
   0.44 M were measured after one 10 KiB extension; the hash path also scans the zero tail
   (≈ 1 CU per 250 B), so pump-fees grown near the 10 MiB cap by a griefer's extensions needs
   ≈ 0.48 M before Squads' own cost (estimated with the tests' cost model; KI-69 (e)).

6. **AIR-19, the market mark.** `min_open_hard_cap_bps` stays at the lowest cap any position
   locked since the market last emptied. This is conservative: a closed position may have set
   it. Accept it, or track each cap exactly, which needs more state.

   **Resolved (owner, 2026-10-12): accept the conservative mark** (known-issues KI-74).

7. **No behaviour change: AIR-04, AIR-11, AIR-20.** [`DOC-FOLLOWUPS.md`](DOC-FOLLOWUPS.md) has
   the known-issues rows and doc wording for each. AIR-20 also has an optional code fix in
   [`leads.md`](leads.md).

   **Resolved (owner, 2026-10-12): no behaviour change.** Known-issues KI-70 (AIR-04), the
   lock-decay sentence of KI-13 (AIR-11) and KI-75 (AIR-20); AIR-20's optional fix is not
   taken, and its residual stays pinned.

8. **Interface changes.** The account sizes are unchanged; the new fields take reserved bytes.
   - `Market.lender_credit_rem` (AIR-03, @2,936) and `Market.min_open_hard_cap_bps` (AIR-19,
     @2,952).
   - `VenueGuard.guard_len` / `guard_elf` (AIR-01).
   - `revalidate_venue(venue, expected_slot, expected_elf_hash)` (AIR-01).
   - `min_notional` has a compiled floor of 53,000,000 lamports (AIR-06), so Governance can no
     longer set it below 0.053 SOL.
   - A new error, `PoolDrained` = 6108 (AIR-07); the existing codes keep their numbers.
   - `SolSettle` gains `vest_flushed: u64` (appended last, `cb756ca`): the vest the settlement
     flushed, with or without an epoch roll. `EpochRolled.vest_flushed` is also reported on a
     rebase that flushed (AIR-07).
   - `sweep_trader` takes the trader's pump UVA as a one-account token leg on a market that
     left pump (AIR-05).
   - `disable_venue` can now emit `ConfigCancelled` (AIR-24).

9. **SPEC §14 fourteenth round: AIR-21, AIR-09 and AIR-14's widening.** Three fixes change
   what an owner decision covered, and were drafted from the code and the sims, not from a
   decision. AIR-21 changes B2-2: an S1 now vests its take above `⌊Q·P_ema/WAD⌋ + R_due`; a
   genuine S1 loss still vests only its rent top-up. AIR-09 counts the tokens at 0 on a venue
   that cannot execute, so of an S3 there only `R_due` stays immediate (KI-24 was accepted on
   the behaviour before it). AIR-14 widens ECON §12.12 residual 10, which O18 accepted on the
   `9aade3e` figures: a held drain, frozen, forces 181 levels in the sim instead of 69, each
   still one the LP's withdrawal for good liquidates.

   **Resolved (owner, 2026-10-13): AIR-21 and AIR-09 confirmed; AIR-14's widening accepted**
   (known-issues KI-52 and its B2-2 row, KI-24, KI-23; SPEC §14 fourteenth round).

10. **FE-31, the `lender.residuals` copy.** The approved disclosure quoted the held model's
    Tier A loss to lenders and the protocol from before AIR-22 (about 2.06 SOL fresh, 8.02 SOL
    aged 25 days at 90 %). Under AIR-22's lower-median growth `sandwich.out` gives +3.340 and
    +4.543 SOL.

    **Resolved (owner, 2026-10-13): approved** with about 3.34 SOL fresh and 4.54 SOL aged
    (FRONTEND §13 FE-31).

11. **The ≥ 24 h fuzz run.** [`../findings.md`](../findings.md) counts a fix as done only after
    a ≥ 24 h fuzz run on it; the owner did not ask for one on 2026-10-12.

    **Resolved (owner, 2026-10-13): waived for this batch.** No such run was made on the fixed
    program; CI and an external fix review are still owed (AUDIT.md §12).

12. **Pre-AIR-01 `revalidate_venue`.** The indexer decodes instructions against the IDL it
    embeds, so it can no longer decode the two-argument `revalidate_venue` of a build before
    AIR-01; a backfill of such a build's history would quarantine that call.

    **Resolved (owner, 2026-10-13): accepted, no legacy decoder** (INDEXER §3.6). It matters
    only if such a build was deployed and its history must be indexed.

## Follow-ups

The fixes left the follow-ups below. They are done on the branch `air-followups` (`main` @
`4497ea6` plus the commits named here, 2026-10-12/13). The keeper, indexer, web, test-harness
and docs tracks went through adversarial reviews whose findings were fixed (the keeper's latest
in `a1edf78` and `bf037ef`, the docs' in `72103a5` … `c6a55b5`); the client regeneration, the
fuzz-corpus revisions and the integration's own commits had no separate review. What is still
open is at the end of this list and in [`TODO.md`](../../../TODO.md).

- **Indexer: done** (`cb756ca`, `fb60d70`, `590e724`). A flush without an epoch roll emitted no
  event, so `vest_bal` stayed stale until the next `PriceCranked`. `SolSettle` now reports
  every flush in `vest_flushed` (a program change: the new hashes in AUDIT.md §0; the CU
  figures refreshed in `fb60d70`), and the indexer zeroes `vest_bal` at every reported flush.
- **Test harness: done** (`251d6e8`). T-POT's replay stopped on KI-66 dust profile seed 66 at a
  withdrawal that the host model refused with `NegativePending` and the program paid (0, 0).
  The program pays a withdrawal's SOL part at once, so another lender's withdrawal had already
  drawn the pot, and an earlier claim paid this lender 266 lamports on chain against 741 in the
  model. A settlement shortfall then left its pending at 153 on chain and −322 in the model.
  The replay now sends such a withdrawal with `min_sol = u64::MAX` and asserts that the program
  runs it on its own state and reverts at `SolBelowMin`, so the token side stays the model's
  (`Stats.sol_gaps`). `ext_pot_fwd_dust_100` is back on seed 66, and its 4 such withdrawals
  are pinned.
- **Keeper: done.** `latch_action` no longer flags a pure `ExtendProgram` (AIR-01: `d2bb168`;
  then `a1edf78`, a `VenueNotUpgraded` counts only from a bank at or past the deploy slot, and
  a slot short.win refuses as not deployed is judged on the account read whole; `bf037ef`, a
  tick's moved venues are simulated together). The exit predictions use AIR-10's tier cap on
  PumpSwap (`2c1fce9`); the S3-chain predictions use AIR-19's market mark, and a position's S3
  timer its own locked cap (`6a46cde`). A program not deployed in its slot (AIR-18) is retried
  at the next tick without an alert (`8945087`).
- **Web app: done** (`3f224c2`, `3c0f602`, `2bda5f5`, `975f757`, `abce2b1`; `2fea2c5`).
  `PoolDrained` (6108) is in `errors.ts`, `previews.ts`' `MATH_ERRORS` and
  `gen_lend_vectors.py`'s `REFUSALS`, and `lend_vectors.json` is regenerated (AIR-07);
  `wipeSweepPending` tests `totalShares === 0n` (AIR-08); `sweep.ts` adds the pump-UVA leg off
  the pump venues (AIR-05); the WASM `previewSettlement` prices the tokens at 0 on a venue that
  cannot execute (AIR-09; AIR-21's S1 vest was already in it). After the final review of the
  follow-ups (2026-10-13): a position's risk view (its CR, the CR on the ring, the liquidation
  price) takes AIR-10's exit risk cap, the tier at `P_ema` on PumpSwap, as the program and the
  keeper do (`ed014ce`); the deposit's unpaid-rent preview carries AIR-03's remainder, through a
  `rem` argument of the WASM `RentBasis.lenderCredit` (`b1d86d4`); CI's `web` job runs the
  facade's Node smoke test (`33491a3`). These change only the web, the WASM facade and CI, after
  the local gate below; each commit message records its tests.
- **IDL and `clients/js`: done** (`caa5852`). Regenerated from this branch's build; CI's
  `codama` job passes locally. The indexer embeds the IDL, so the two committed recordings
  holding a `revalidate_venue` made before AIR-01 were migrated in the same commit (an all-zero
  `expected_elf_hash` appended, which is the old exact-slot check), and REPLAY's two committed
  sessions were re-recorded (`5c4488f`).
- **Fuzz smoke: done** (`70ac76e`, `621f8e1`). Two corpus regression tests failed since AIR-07
  (already on `4497ea6`): the drained-pool gate refused FUZZ-M7-24H-3's late deposit, and the
  vest flush left FUZZ-M10-24H-A's CPMM lender owed less than `CLAIM_DUST`. One action of each
  reproducer was revised so it reaches its finding's shape again; no program change.
- **Design docs and known issues: done** (`2c05eae`, `7618698`, `2aae952`, `18b65e2`,
  `f6e25be`, `34105c6`, `63b673c`, then `72103a5` … `c6a55b5`, `be1c47a` and `b19655f`):
  [`DOC-FOLLOWUPS.md`](DOC-FOLLOWUPS.md) applied, the sims' outputs regenerated,
  the register's rows for the owner's decisions, the AIR-18 figures recomputed (`leads.md`
  L-2's erratum).
- **Release evidence: partly done.** The integrated branch passed a local run of the CI gate on
  2026-10-13 (below), and AUDIT.md §0 records its build's hashes, marked as not verified. Still
  open: a CI run (the owner asked for the branch to be pushed with `[skip ci]`, 2026-10-13, so
  CI has not run), the verifiable build, the PostgreSQL and E2E jobs, and an external fix
  review. The ≥ 24 h fuzz run is waived by the owner for this batch (2026-10-13, decision 11);
  none was run.
- **The owner's items: decided** (2026-10-13, decisions 9–12): AIR-21 and AIR-09 confirmed and
  AIR-14's widening of residual 10 accepted (SPEC §14, fourteenth round; known-issues KI-23,
  KI-24, KI-52; AUDIT.md §7); FE-31's copy of `lender.residuals` approved (FRONTEND §13); the
  24 h fuzz waived; no legacy decoder for the pre-AIR-01 `revalidate_venue` (INDEXER §3.6).

## Local gate

CI has not run on these branches: nothing was pushed before 2026-10-13, and the owner asked for
`air-followups` to be pushed with `[skip ci]`. The CI jobs ran locally instead, in WSL (Ubuntu
24.04) with the pinned toolchain.

**The integrated branch** (2026-10-13, `air-followups` at `621f8e1`: the fixes and every
follow-up above; test binaries at 4 threads; AUDIT.md §10 has the same run):

| CI job or step | Result |
|---|---|
| anchor-build: release and test-venue SBF builds; SBPF v3 flags, A49, security.txt; the IDL copies | pass; the hashes below; both committed IDLs equal the build's |
| `fmt`; `clippy -D warnings` (the workspace, `shortwin-math` and `shortwin` alone); `shortwin-math`'s deny lints, `no_std`, no runtime dependency | pass |
| `shortwin-math`, `shortwin-math-wasm`, `squads-v4-ix` tests (`PROPTEST_CASES=2000`, as CI) | 376 passed |
| Vectors: every `gen_*.py` regenerated, no diff; `config_check.py` prints OK; the config-gate tests (8) | pass |
| The WASM build; the T-LAYOUT wasm32 check; audit scope; fixture checksums and `dump-fixtures.py check` (265 entries) | pass |
| `shortwin` unit tests | 75 passed |
| LiteSVM suite (`shortwin`, `shortwin-venue-state`, `shortwin-litesvm-tests`; T-SIZE's 11 among them) | 1,252 passed, 0 failed, 4 ignored ¹ |
| T-POT: every pool trace replayed in full | 5 passed (stranding table: 40 seeds, 15,044 lamports, worst 435; `ext_pot_fwd_dust_100`'s 4 `SolBelowMin` gaps) |
| Services: fmt, clippy, the keeper and `shortwin-gov` tests, T-OPS, the keeper LiteSVM scenarios, the keeper binary's A49 check; `build-alt.sh print` | 252 passed, 3 ignored (keeper lib 86; scenarios 140, 2 ignored; `rpc_local` ignored); A49 ok; the ALT's static part 49 addresses |
| Indexer without PostgreSQL: the indexer's unit tests; the replay harness's tests that need no database; the production indexer links no embedded Postgres | 66 passed; 5 passed; pass |
| CU benches (math, M1–M3, M5, M6) | every budget holds; M1, M2 and M6 re-wrote rows by run-to-run noise on the same bytes (committed in `f33edf0`) |
| m7-keeper-bench (cost, PumpSwap ALT, cascade; release) | 3 passed; its re-written figures are not committed: the committed ones still name v1.0.0's build, and the new ones fail `keeper_cost.py`'s cross-check (TODO.md) |
| fuzz-smoke (`cargo test --release -p shortwin-fuzz`, CI's profile) | 12 + 16 passed (FUZZ-M7-24H-3's deficit 1,028.408 SOL at 999,998 ppm of its bound; FUZZ-M10-24H-A's CPMM lender owed 2,149 lamports) |
| codama: IDL copies, `generate` leaves no diff, client typecheck and tests | pass; 29 client tests |
| web: math-wasm build, lint, typecheck, Vitest, build | pass (lint 334 files); Vitest 769 passed, 1 skipped |

¹ The four ignored tests are ignored on `main` too: a Surfpool fork, the pre-M4R builds (two),
and an exploration grid (`m4r_explore_liquidation_grid`).

Not run here:
- `cargo deny` and `cargo audit`: not installed, and their advisory database needs a download.
  No lockfile changed since v1.0.0 (`Cargo.lock`, `services/Cargo.lock`, the six bench
  lockfiles, `pnpm-lock.yaml`), whose lockfiles passed both in CI run 38125079869;
- the `indexer` job's PostgreSQL tests and its validator test (both need PostgreSQL);
- `rpc_local` (manual: no CI job runs it; the keeper track ran it against a local
  `solana-test-validator` at `8945087`, 1 passed, and `services/common` has not changed since);
- `web-e2e` (Playwright on a validator);
- the verifiable build (Docker);
- a fuzz run beyond the smoke tests (no ≥ 24 h run: the owner did not ask for one on
  2026-10-12 and waived it for this batch on 2026-10-13, decision 11), and the opt-in
  `mutants`.

The build's hashes: `shortwin.so` `fba9dd75…` (1,634,936 B); `complete_curve.so` `f3a47201…`
(unchanged from v1.0.0); AUDIT.md §0 has them in full, marked as not verified.

**The fixes' own gate** (`ai-review-fixes`, before the follow-ups): fmt, clippy, the math tests
(374), vectors, T-CONFIG, WASM, scope, fixtures, the SBF builds' checks, the unit tests (75),
the LiteSVM suite (1,250 passed, 4 ignored), T-POT (stranding table 15,044 lamports), the
services (226 passed, 3 ignored; 121 keeper scenarios), the indexer's unit tests (63) and the CU
benches (figures refreshed in `a8f9994`) passed; codama, web, web-e2e, fuzz-smoke and the
PostgreSQL tests did not run. Its `shortwin.so` was `eafc9833…`; `cb756ca` changed the program
since.

## Files

| File | Contents |
|---|---|
| [`SUMMARY.md`](SUMMARY.md) | The 17 merged issues, each with both reviewers' verdicts, the corrected impact and a fix direction |
| [`report-1-instructions.md`](report-1-instructions.md) … [`report-6-math-rest.md`](report-6-math-rest.md) | The six scan reports, unedited |
| [`verification.json`](verification.json) | Both reviewers' full reasoning for every issue |
| [`leads.md`](leads.md) | The triage of the eight leads (L-1 … L-8) |
| [`DOC-FOLLOWUPS.md`](DOC-FOLLOWUPS.md) | The design-doc and known-issues changes each fix implies, with line numbers against `9aade3e` |
