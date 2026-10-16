# Doc follow-ups for the AIR fixes

The fixes on this branch change code, the normative sims and their vectors, but **not** the design
documents. These are the statements each fix makes false and the replacement wording the fix
author proposed. Line numbers are against `main` @ 9aade3e. E = `docs/design/ECONOMICS.md`,
A = `docs/design/ARCHITECTURE.md`, K = `docs/audit/known-issues.md`, S = `docs/SPEC.md`,
AUD = `docs/AUDIT.md`.

## AIR-01 — venue latch tells an extension from an upgrade

- A:975, 976, 983, 372, 427: the content rule (a grown ProgramData latches only if its trimmed ELF
  changed; an unchanged length after a slot change is an upgrade); `revalidate_venue(venue,
  expected_slot, expected_elf_hash)`; the VenueGuard layout (`guard_len [u32;4]` @160,
  `guard_elf [[u8;12];4]` @176, from `_reserved`).
- A:1798, 1872, 2365 (§6.1, §6.5, U5): "no hash without an extension; after one ≈ 0.5 CU/B of
  trimmed ELF + 1 CU/~200 B of zero tail (≤ 0.44 M CU measured); size the limit by simulation,
  ≤ 1.4 M".
- AUD:317: `revalidate_venue` takes `expected_elf_hash`.
- `services/README.md` (Governance): run one revalidation with a hash per venue after
  `initialize_config` (it records the fingerprint); request ≥ 0.5 M CU when executing after an
  extension.
- K: new row with AIR-01's residuals (until a hashed revalidation records the fingerprint an
  extension can latch once; 96-bit truncated fingerprint; an exact-slot revalidation records the
  supplied hash unchecked; extensions refresh `flagged_slot` while flagged; the keeper still pages
  `VenueUpgraded` after an extension).
  *Superseded (2026-10-12/13): the runbook asks for ≥ 0.6 M CU or a limit sized by simulation
  (≈ 0.48 M estimated for pump-fees near the 10 MiB cap; KI-69 (e); `72103a5`, `eeee640`,
  `7a92078`); the zero-tail scan costs ≈ 1 CU per 250 B; since `d2bb168` the keeper neither flags
  nor pages a pure extension of a fingerprinted venue (`a1edf78`, `bf037ef`).*

## AIR-02 — the vest end rounds down

- E:35, E:258, E:1321, A:1030, A:2466: ⌈…⌉ → ⌊…⌋ in the value-weighted vest end.
- E:1347: "at most 1 s (the ceiling)" → "not at all (the floor)".
- E:35, E:1356, A:2466: 98 % → 100 %.

## AIR-03 — the lender rent credit carries its remainder

- E:252, E:1050, A:1537 → "⌊(Σ_j A_j·δJ_j + rem)/(10⁴·10⁴·31,536,000·WAD)⌋, rem carried in
  `Market.lender_credit_rem`".
- A:1542 → "Because the accruals' credits sum to the floor of their exact total and each true-up
  ceils per position, …".
- A:330: list `rent::lender_credit_carried`.
- A:545 and `docs/design/arch/layout.py`:71-72 → `lender_credit_rem` PodU128 @2,936.
- E:1363 → "0.000015 | 0.000000"; E:1367 → "31,438,356"; E:1331 → recompute with the final sims
  (AIR-03 and AIR-07 both move it); K:65 (KI-66) and K:66 (KI-67) figures likewise.
- Regenerate `docs/design/sim/out/pool_model.txt`. AUD:903 / 948 / 997: the next T-POT run's
  stranding figures (15,058 lamports, worst seed 440 with AIR-03 alone).

## AIR-05 — the cashback gate survives a market leaving pump

- A:1048, A:1255: the reward bit and the pump-UVA token leg also apply on any market whose
  `pump_uva` is set.
- E:1440: "a market that left pump keeps its UVA's claimable cashback; the bit stays required".

## AIR-06 — min_notional's compiled floor

- E:3570 and A:448 → "[53,000,000, 10⁹] (derived buyback-room floor, AIR-06)".

## AIR-07 — a settlement that leaves dust claims flushes the vest; a drained pool takes no deposit

- E:111, A:1238, E:1424: add "a settlement that leaves ≤ 1/1,000 of the token claims it found pays
  the whole vest to the current shares".
- A:1274 (`vest_flushed`): "also on a rebase; a flush without a roll emits no event".
  *Superseded by `cb756ca` (`SolSettle.vest_flushed`, appended last), which reports every flush,
  with or without a roll:* A:1272 (`SolSettle` row) gains "vest_flushed (the vest this
  settlement flushed, with or without an epoch roll; else 0)"; A:1274 reads "the vest the
  settlement flushed: a full wipe, or a rebase after a settlement that left ≤ 1/1,000 of the
  token claims; equal to `SolSettle.vest_flushed`"; A:59, A:1236, A:1240 and E:91 name
  `SolSettle.vest_flushed` beside `EpochRolled.vest_flushed`.
- E:1429: add "deposit while S > 0, ΣB_Q > 0 and 1,000·T ≤ T + ΣB_Q: refused (`PoolDrained`)".
  A:1048: the same check in `deposit`'s effects. A:1299 (error table): 6108 `PoolDrained`.
- E:109 stays as written: with the drained-pool gate a just-in-time deposit captures nothing.
- K: a row for the drained-pool gate: honest deposits are refused while a pool is drained (the
  sims' rates are in the README's decisions).
- Sim figures. The pool fixes' author measured these on AIR-02, AIR-07, AIR-08 and AIR-12
  without AIR-03, which moves several of them too, so recompute them on this branch's sims:
  - E:1267 row → "954 | 87 | 3.00";
  - E:1363 inflow → 50,850.3;
  - E:1331 pot-liveness → 654 runs (without the pot-only touch: 13 runs and 1,830,229,138;
    "3 lamports in total");
  - E:1331 and K:65 over-move → "5 of 15,756 touches";
  - K:65 tuned pools → 31 runs, 1,937,082, worst 727,760;
  - K:65 adversarial pools → 17,599 runs, 1 run, 16,229 in total, worst 14,894;
  - K:66 → "3 of the 6,485 closable positions";
  - E:1463–1465 fuzz lines → epochs 1,021 / 1,941 / 1,021.

## AIR-08 — the sweep gate applies whenever no shares exist

- E:36, E:1429, E:3516, A:1048, A:1084, A:1255, A:2341: "while a full-wipe record is live" →
  "whenever S = 0".

## AIR-09 / AIR-21 — settlement takes above the token value vest

- E:1336 (the "S1 stays immediate…" rationale), E:1147, A:1030, A:1236, A:1580, A:2176, A:2341 →
  "lump = x − min(x, ⌊Q·P′/WAD⌋ + R_due) on S1, S2 and S3, with P′ = P_ema, or 0 when the venue
  cannot execute". Mark A:2467, A:2251, S:359, E:32, E:62 "superseded by AIR-21".
- E:1613 and K:81 (KI-24): "an S3 at the frozen mark vests all but R_due".

## AIR-10 — the PumpSwap exit risk fee is capped at the tier at P_ema

- E:106, E:316, A:1157, A:1450, A:2101, A:2169 → "min(φ_live, hard cap), on PumpSwap also at most
  the tier at P_ema's market cap".
- K: a row for the burn residual (a same-transaction burn still raises the live tier; closing it
  needs a per-second supply observation).

## AIR-11 — doc-only

- E:1284 → "Π returns 94 % within a day to the lender whose lock set `b_min`. A later lock in the
  same era inherits that `b_min` and decays only once ΣB_Q falls below it, otherwise at
  `lock_until`. This over-locks, so I-P4 holds; the value side is KI-13."
- The same qualifier at E:110, E:3768, A:1067, A:2491 and K:59.

## AIR-12 — Π restarts with every lock era

- A:1064 → "b_min = ΣB_Q and Π = WAD (first lock of a lock era)"; A:2328: "Π epoch" → "lock era";
  A:2191 after "Π resets only at ΣB_Q = 0": "and at each lock era's first lock"; E:1268 → "954 | 0 |
  0.00"; A:2453: note the 1.27·T figure was measured with Π kept across eras.

## AIR-13 / AIR-14 / AIR-23 — Raydium depth caps at D

- E:480, A:1014, A:1523 → a Raydium price start writes `L_slow = min(L, D)`.
- E:1074, E:1101, A:325 → the Raydium cap is D, so `L_b = L_live` above it.
- A26 (A:571-577): exits read no governed `pool_*` parameter (AIR-23).

## AIR-15 — the settlement box's L_hi is capped

- A:1207, A:1555, E:528: `L_hi = max(L_ema, L_mhi)` → `max(L_ema, min(L_mhi, cap))`, cap = the
  market's depth cap (D on Raydium and PumpSwap after AIR-14; none on the curve).
- E:563: deposit wraps widen `L_hi` at most to the cap. K:53 (KI-07): the LP variant is bounded by
  the cap.

## AIR-16 — a lapsed arming clock re-arms

- A:1220 "otherwise keep the clock (`s_last = now`)" → "otherwise keep a live clock (`s_last = now`
  if `now − s_last ≤ settle_arm_gap_s`); a lapsed one is left for step 3 to re-arm".
- E:551 "Every other evaluation … keeps the clock running" → "Every other evaluation within
  `settle_arm_gap_s` of the last …". A:2483 (R3A-6): "within `settle_arm_gap_s`; a lapsed clock
  re-arms".

## AIR-17 — the guarded spot floor reads the bucket highs

- E:508 → `L_spot = max(L_live, L_floor)`, with `L_floor = min(L_ema, L_mhi)` while the guard is
  set. K:20 (KI-20): no longer "forces no exit" by withdraw wraps.

## AIR-18 — ExtendProgram blocks a program for the rest of its slot

- K:123 (KI-63) and A:2120 (g): "harmless" → "redeploys the program, which is unusable for the
  rest of that slot, and moves the deploy slot".
- K: new row with the bounds in `leads.md` (≈ 0.052 SOL per blocked slot; ≈ 44–49 SOL to exhaust a
  venue; short.win itself once deployed below 10 MiB).
- `services/README.md` deploy runbook: extend short.win's and complete_curve's ProgramData to the
  10 MiB cap at deployment (≈ 45 SOL of rent), or accept the bounded griefing.

## AIR-19 — a fee above a position's locked cap is no-route for it

- A:484, 574, 957, 1104, 1228, 1659, 1703, 1705, 2004, 2342 and E:1614: "the S3 chain at min(global,
  mark); a position's no-route at min(global, its locked cap); `pool_fail` global". E:567 and A:1450:
  no band exists within the locked cap; above it, S3 settles.
- A:545 and `docs/design/arch/layout.py`: `min_open_hard_cap_bps` u16 @2,952.
- K: a row for the stale mark (it stays at the lowest cap since the market last emptied).

## AIR-20 — doc-only

- E:538-539 and A:1558-1559: the S1/S2 legs read raw `L_live`, so inside an LP's
  `[withdraw, sol_settle, deposit]` only the box corner vetoes.
- K: a row with the bound (≈ 1.2 % of B̂ only at `pool_lock_min_bps` 20 % and a 1 % fee).

## AIR-22 — sustained growth takes the lower median step

- E:228, A:1522 → `g = max(0, lower median (index ⌊(n−1)/2⌋) of the last kappa_growth_n step rates)`.
- E §12.6 (2232–2296), S:293/355, E:3549/3745: held plans now 2 s (RA +2.839 SOL); re-measure the
  real venues (O19). Regenerate `docs/design/sim/out/attacks/{sandwich,block_spot,ema_near,
  fixed_delta}.out`. E §13 fuzz counts → recompute on the final sims.

## AIR-24 — disable_venue cancels a pending re-enable

- A:974, A:406: "and cancels a pending EnableVenues that would set the bit"; AUD:317 likewise.

## AIR-04 — doc-only

- K: a row — `apply_config` installs new authority keys unsigned; recovery is an upgrade from
  vault 0; stage authority changes only with `shortwin-gov stage-authorities`.
