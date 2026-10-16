# m5_cu — M5 CU bench, T-TRACE, T-SIZE preview and stack depth on the pump curve

This is the M5 compute, trace, size and depth gate for the pump bonding-curve venue (ARCHITECTURE
§6.1–§6.4, §8.2 T-CU / T-TRACE / T-SIZE / T-DEPTH, §9 M5), and since pump-final P3
(2026-10-15) gate G1's budget rows (pump-final §6.6, §12): the curve adapter trades `sell_v3` /
`buy_v3` with 10 TradeExtras, and a buyback past `rT` completes the curve by pump's synthetic
migration in one instruction. Everything runs the **release** short.win (`anchor build
--tools-version v1.57 --arch v3`) against the **dumped mainnet** pump (2026-10-13), PumpSwap
(2026-10-08) and pump-fees (2026-10-12) programs and the pump coins of `tests/fixtures/` (group
`pump_state`), on the LiteSVM harness's pump fork (`shortwin_litesvm_tests::fork` / `pump`).

- **CU bench** (`tests/m5_cu.rs` → `results.md`): every curve-path instruction —
  `create_market(Auto)` (a curve listing with the init set on the 115-, 125- and 151-byte
  layouts, a `PumpMigrating` listing, both list-only `None` listings), `open_short` (a wallet's
  first open on every layout, a repeat borrower, normalising, the costliest path), `close_short`
  (plain, normalising, the costliest path, a completing buyback, a **synthetic** buyback past
  `rT` and the same with the **worst ring**), `liquidate` (after a real pump; a synthetic one on
  ECON's `D@80%` market, regular and worst ring), `expire` (at the deadline, the costliest path, a
  completing buyback, a synthetic one, regular and worst ring), `sol_settle` (the no-op, S3 on
  `PumpMigrating` at 70 min — without and with a synthetic pool part, `pc ≠ 0`, whose FV is
  ρ-scaled — and S3 on `None` after the chain, M2-SEC-5), `crank_price` (roll and record;
  `PumpCurve → PumpMigrating` after an exact and after a synthetic completion, a `PumpMigrating`
  roll after short.win's own synthetic close, `PumpMigrating → None`, a `None` market's crank,
  the `None → PumpCurve` re-detection), `sweep_trader`'s token leg, `flag_venue_upgraded` and
  `revalidate_venue` on the dumped 10 MiB pump and PumpSwap and the 0.95 MB pump-fees
  ProgramData — on SPL Token and Token-2022 curves. Mollusk 0.15.1 gives the figure the budget
  applies to; LiteSVM 0.16.0 runs the same instruction behind a 1.4 M `SetComputeUnitLimit` as a
  cross-check that must agree to the unit (the instruction's own `consumed` log line) and gives
  its trace entries, its peak stack height and pump's own CU (its outermost frames, its token,
  System and event CPIs included). Budgets are ARCH §6.1's PROPOSED transaction limits, which
  pump-final §6.6 keeps; `results.md` names the source of each.
- **The synthetic exits and the worst ring** (pump-final §5.2, §6.6): on the near-complete curve
  the crowd leaves `rT = q/2` (a real `buy_v2`), so the exit's `buy_v3(q)` buys `rT` on the curve
  and `q − rT` from the would-be pool and the market moves to `PumpMigrating` in the instruction;
  the synthetic liquidation is T-SYNTH-2's path on ECON's `D@80%` market (the crowd's buy-down
  lifts the price × 2.4, past Tier D's `CR_liq`). **Worst ring**: the keeper cranks until every
  entry of the EMA ring holds a curve state past the position's `q` (`ring_past_rt`, asserted at
  the measured second; ≈ 20 s, since the ring caches the EMA's reprojection), so every EMA-leg
  quote, box corner and `B̂_ref` the instruction takes is a `B̂_v3`.
- **T-TRACE** (`tests/t_trace.rs` → `trace.md`): the worst-case curve transactions — the largest
  client open and close with their two ComputeBudget instructions and normalisation, a completing
  close, a synthetic close, an atomic sandwich of an open with the curve's own trades, five opens
  of one wallet, a batch of four cranks that all move their market to `PumpMigrating`, the
  keeper's `[crank, liquidate | expire | sol_settle S3]` (synthetic exits past `rT`, regular and
  worst ring; S3 on `PumpMigrating` with and without a pool part, and on `None`), the keeper's
  top-level `migrate_v2` after a third party's exact completion (`pc = 0`: a `buy_v2` of exactly
  `rT`) and after a synthetic one (`pc ≠ 0`: a third party's `buy_v3` past `rT`, and short.win's
  own synthetic close), each held to its `MIGRATE_CU`, the three pump latch flags, a curve
  listing, `sweep_trader` — each
  within `MAX_INSTRUCTION_TRACE_LENGTH = 64`
  and at its exact expected count, within ARCH §6.1's CU limit, with its wire sizes (legacy; v0
  through the protocol ALT of A28 **with its pump part**: pump, PumpSwap and pump-fees, their
  statics, the live `Global` / `GlobalConfig` fee and buyback recipients with their WSOL ATAs).
  Three more tests: six opens of one wallet are refused by the runtime itself
  (`MaxInstructionTraceLengthExceeded`), how many opens of one wallet fit a v0 transaction, and the
  crank batches the protocol ALT holds (the four real curve markets and synthetic ones beyond
  them, sized only).
- **T-STRADDLE, curve part: retired** (pump-final P6, owner D2). The straddle bundle's tx1 rows
  (the `complete_curve` helper's completion at the live `rT`, after a trade, as a no-op) and the
  helper's depth row left with the helper (`git log` keeps them). A position past `rT` exits in
  one instruction (the synthetic rows). The bundle's tx2, the keeper's `migrate_v2` after a
  third party's exact completion (`pc = 0`), measures no helper and stays as a T-TRACE row next
  to the two after a synthetic completion (`pc ≠ 0`, a third party's or short.win's own), each
  held to the keeper's `MIGRATE_CU` (400 k, pump-final §6.6).
- **Stack depth** (`tests/t_depth.rs` → `depth.md`): every curve-path instruction called directly
  and through one, two and three levels of the TEST-ONLY CPI wrapper `wrapper/` (it forwards one
  instruction, as a Squads vault transaction or an integrator's program would call short.win), on
  LiteSVM's **mainnet** feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`; Mollusk is not used
  here: it enables SIMD-0268's limit of 9); and T-DEPTH's second half on the curve — the venue
  accounts pump would create one level deeper (the trader's pump UVA, `trader_base_ata`) are
  required, so a leg without them fails cleanly before pump runs (`VenueAccountMismatch`,
  `TraderStateInvalid`), direct and wrapped.

The per-instruction call shapes (which program at which height, in order) are pinned by
`tests/litesvm/tests/trace_pump_it.rs`, the full T-SIZE on the curve (the largest extras set, a
separate fee payer, both tables, executed as v0 under the invariant hooks) by
`tests/litesvm/tests/tx_size_pump_it.rs`, and T-LATCH on the dumped pump ProgramData (real
loader upgrades) by `tests/litesvm/tests/latch_pump_it.rs`; the three run in the program-tests
CI job.

```sh
# from the repository root
anchor build --tools-version v1.57 --arch v3                     # target/deploy/shortwin.so (release)
cargo-build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path tests/bench/m5_cu/wrapper/Cargo.toml \
  --sbf-out-dir tests/bench/m5_cu/target/deploy -- --locked      # the wrapper (t_depth, t_trace)
cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --locked -- --nocapture
# SHORTWIN_SO / M5_WRAPPER_SO override the ELFs;
# M5_CU_WRITE=1, M5_TRACE_WRITE=1, M5_DEPTH_WRITE=1 rewrite results.md, trace.md, depth.md.
```

A case or transaction that does not execute, a Mollusk figure above its budget, a disagreement
of the two harnesses, a trace count other than the expected one or above 64, a peak stack
height other than the expected one (above 3 for an instruction), a transaction above its §6.1
CU limit, a client or keeper transaction above 1,232 B as v0 + ALT (a transaction marked v1:
above 4,096 B or 64 keys), or a wrapper level that does not reach exactly `direct + levels`
fails the bench.

## Pre-states

- Every case builds a fresh pump fork (`Env::fork_pump_coin`), lists the curve with the real
  `create_market(Auto)` and its pump init set, lends `40·Q` and waits out the warm-up (60 s),
  then cranks once, where `Q = vT/200` (the `pump_it` size, within every default cap). The fresh
  125-byte curve first gets `vT/7` of buys by a normal user (its `rQ` is one lamport as dumped).
  Every transaction that builds the state goes through `Env::send`, so the invariant hooks run.
- The coins: `legacy115` (SPL Token, 115 B, no creator — its buys make one System transfer less),
  `t22_115` (Token-2022, 115 B, a creator), `fresh` (Token-2022, 125 B), `near` (Token-2022,
  151 B, `rT` ≈ 8.9·10¹² — the completing buyback, the migration and the synthetic exits), and the
  graduated coins (`graduated`, a PumpSwap-class coin, listed on its canonical pool with the pAMM
  init set since M6 — `tests/bench/m6_cu` measures every PumpSwap listing; `fartcoin`, graduated
  elsewhere, the list-only listing).
- **The costliest paths** (`costliest_legs`): the curve's creator changed after the listing to a
  fixed key whose creator vault is **empty** — under v2 the adapter re-derived the creator-vault
  slots and pump topped the new vault up from the trader; `sell_v3` / `buy_v3` name no creator
  vault (pump-final §6.1), so this now costs nothing and stays only to show it — the **last**
  buyback recipient of the live `Global` (TradeExtras T7: the longest key scans, short.win's and
  pump's), and normalisation (lamports donated to the trader PDA and tokens to
  `trader_base_ata`). The costliest open sells `vT/100`.
- The keeper exits are prepared the way a keeper decides: `liquidate` after a real `buy_v2` of
  40 % of `vT` (the price × 2.8), cranked second by second until a `simulate_transaction` of the
  keeper's `[crank_price, liquidate]` succeeds (the simulation commits nothing); `expire` three
  cranked seconds past the deadline; the completing and the synthetic expiry cranked until their
  simulated `[crank, expire]` succeeds (the third party's buy-down lifts the price above the
  bound's reference until the EMA follows). The synthetic liquidation: ECON's `D@80%` market (the
  fresh curve set to pump's initial reserves, the crowd's `buy_v2` of 80 % of `rT₀`), a position
  of `vT/200` 30 cranked seconds old, the crowd's buy-down to `rT = q/2`, then as `liquidate`.
  S3 on `PumpMigrating`: a third party completes the curve 30 min before the deadline — with a
  `buy_v2` of exactly `rT`, or (`pc ≠ 0`) a `buy_v3` of `rT + q` — the keeper cranks and
  simulates `[crank, sol_settle]` every 30 s until S3 settles (70 min after
  `migration_seen_at`). S3 on `None`: the complete curve's reserves zeroed by a migration
  elsewhere (planted on pump's account, as `pump_it::graduation`), the keeper on the detection set
  every 30 s until S3 settles (`s3_wait_s` after the chain started).
- The borrowers, the keeper, the sandwich attacker, the creator and the changed creator have
  **fixed** keys, so the canonical-bump searches of the position and exposure PDAs and the
  adapter's re-derivations cost the same on every run: the reports are byte-identical across
  runs.
- The latch cases change the dumped ProgramData's deploy slot in place
  (`fork::set_pump_deploy_slot`); `latch_pump_it` runs the loader's real `Upgrade`.

## Workspace and pins

A standalone workspace with its own `Cargo.lock`, like `tests/bench/m3_cu`, so Mollusk does not
enter the on-chain workspace's lockfile. The lockfile was seeded from `tests/bench/m3_cu`'s and
differs from it only in this workspace's own two packages (and the bench no longer names
`litesvm-token` directly): anchor 1.2.0, LiteSVM 0.16.0 and the Agave runtime crates resolve to
the same versions as the program tests, held at 4.2.2 (C1-14). `wrapper/` is a copy of
`tests/bench/m3_cu/wrapper` under its own id (`short.win-m5-cpi-wrapper-program`), built for SBF
with `cargo-build-sbf --tools-version v1.57 --arch v3` (never deployed) and a plain library on the
host.

## Findings (see `results.md`, `trace.md` and `depth.md` for the figures)

Figures of the build pump-final P2 produced (`shortwin.so` `b3130ae2…8e31`), measured at P3
(2026-10-15); the v2-adapter figures they replace are those of `fba9dd75…` (the AI review's
build).

- **Mollusk and LiteSVM agree to the unit on all 65 cases.**
- **CU, every row within its limit (gate G1, pump-final §6.6):**

  | §6.6 row | Measured | Estimate (§6.6) | Limit | v2 adapter |
  |---|---|---|---|---|
  | `open_short` curve | 139.1–154.6 k | 140–175 k | 250 k | 172.3–202.0 k |
  | `close_short`, `q ≤ rT` | 108.2–123.3 k | 125–155 k | 250 k | 150.3–181.4 k |
  | `close_short` completing (`q = rT`) | 125.5 k | ≈ 150 k | 250 k | 175.1 k |
  | `close_short` synthetic (`q > rT`; worst ring the same) | 135.8 k | 140–175 k | 250 k | n/a (bundle) |
  | `liquidate` / `expire` (synthetic and worst ring included) | 126.3–159.2 k | 140–200 k | 300 k | 167.8–198.7 k |
  | keeper `[crank_price, liquidate]` synthetic (`trace.md`, CB ×2 included) | 184.9 k; worst ring 192.6 k | ≤ 245 k | 300 k | n/a |
  | `sol_settle` S3 on `PumpMigrating` (`pc ≠ 0`, ρ) | 69.3 k (64.8 k without a pool part) | ≈ 66 k | 150 k | 64.2 k |
  | `migrate_v2` (keeper, top level; `trace.md`, CB ×2 included) | 330.9 k after a synthetic completion (`pc ≠ 0`: a third party's or short.win's own) and after a third party's exact one (`pc = 0`) alike | unchanged | `MIGRATE_CU` 400 k | 330.3 k |

  - pump's `sell_v3` is 42.5–49.1 k (`sell_v2` 69.9–85.7 k) and `buy_v3` 44.7–50.9 k plain,
    54.6 k completing and 63.4 k synthetic (`buy_v2` 80.7–98.2 k): no pump-fees CPI, no creator
    vault, fewer accounts. short.win's own part of an open is ≈ 92–106 k (v2: ≈ 102–121 k).
  - **The worst ring** costs a liquidation 7.7 k (151.5 → 159.2 k: its EMA-leg quotes and box
    corners on `B̂_v3`, 2,429 CU a call in `tests/bench/cu_math`), an expiry 1.6 k (155.1 →
    156.6 k) and a close nothing (135.8 k both: a close reads no ring) — within §6.6's "≤ +25 k
    from straddle-regime quotes".
  - `sol_settle` 43.5–43.9 k as a no-op, 64.8 k for S3 on `PumpMigrating` (FV on the pool
    `migrate_v2` will create), **69.3 k after a synthetic pool part** (FV ρ-scaled, pump-final
    §5.4; `quote::migrating_fv` 4,743 CU a call) and 44.9 k on `None` (limit 150 k).
  - `crank_price` 27.8–32.7 k for a roll and record, 37.0–37.1 k for `PumpCurve →
    PumpMigrating` (after an exact or a synthetic completion), 35.2 k for a `PumpMigrating` roll
    after short.win's own synthetic close, 26.4 k for `PumpMigrating → None`, 16.8 k on a `None`
    market, and **50.7 k for the `None → PumpCurve` re-detection** (it writes the curve cache and
    starts the price state): within the 60 k limit with 9.3 k of headroom.
  - `create_market(Auto)` 139.2–170.3 k for a curve or PumpSwap listing (the ATA program's
    creation of `trader_base_ata` and the venue's `init_user_volume_accumulator` included),
    58.1 k for a list-only listing (limit 250 k).
  - `sweep_trader`'s token leg 18.6–24.5 k (no §6.1 row; the 60 k lender class).
  - **U5 on the real accounts:** `flag_venue_upgraded` 6,036–6,046 CU and `revalidate_venue`
    8,506–8,521 CU on the dumped 10,485,760-byte pump and PumpSwap ProgramData and the
    948,989-byte pump-fees one, as on Raydium's: the input-region copy is not metered.
- **Trace** (ARCH §6.2's curve column: open 12, exits 12, each ≤ 15; pump-final §6.6: sell leg 1
  + 3 + 2, buy leg 1 + 4 + 2, a synthetic buy + 2): an open is **13** entries for a wallet's
  first position (the position and exposure inits, as on Raydium), 12 for a repeat borrower,
  **16 normalising**, 17 when the sale also grows the 151-byte curve to 166 bytes (`NEAR_GROWTH`);
  an exit is **9** (with or without a creator fee: v3 makes one System transfer either way),
  **12 normalising**, a completing buyback adds pump's `CompleteEvent` and short.win's
  `VenueStateChanged` (11), a **synthetic** one pump's `PostCompleteBuyEvent` too (**12**; the
  keeper's `[CB ×2, crank, liquidate | expire]` past `rT` is 16). Every exit is within the
  table's maximum of 15 (under v2 the normalising and top-up exits reached 16–17); a
  normalising open (16, 17 with the growth) still exceeds §6.2's 15, as under v2 (17, 18 with
  pump's creator-vault top-up), far within the 64 per transaction. A crank is 2, **4** when
  it moves the market to `PumpMigrating` or re-detects a curve, 3 for `PumpMigrating → None` and
  for a `PumpMigrating` roll after a synthetic close: a keeper batch of `k` curve cranks is up to
  `2 + 4k` entries. A curve listing is 15 (peak 3); `sol_settle` S3 is 2, the no-op 1;
  `sweep_trader`'s token leg 4. **Five opens of one wallet fill the trace exactly** (2 + 13 + 1 +
  4·12 = 64, the first sale growing the curve); a sixth (76) is refused by the runtime (v2: four
  fit, 56).
- **Sizes** (v0 through A28 with its pump part; the largest-extras worst case is
  `tx_size_pump_it`'s): the 10-key v3 list makes every curve transaction 144 B smaller (4 inline
  keys and 6 table indices fewer, pump-final §6.6). A client open is 900–901 B / 32–33 accounts
  with its two ComputeBudget instructions (v2: 1,044–1,045 B); a close 864–865 B (1,008–1,009 B);
  the keeper's `[crank, liquidate | expire]` 914–915 B, the same past `rT` (1,058–1,059 B); S3 on
  `PumpMigrating` 732 B, on `None` 686 B; a curve listing 685 B. **Three opens of one wallet fit
  a v0 transaction** (1,141 B; four are 1,261 B, five 1,381 B — v2: two); a v1 transaction holds
  the five the trace allows (1,810 B / 37 keys). The atomic sandwich of an open with the curve's
  own trades is 1,365–1,366 B: an attacker needs v1 (1,949–1,981 B / 47–48 keys) or its own
  table, which it has. A v0 transaction holds **five curve cranks** (1,181 B; six are 1,365 B), so
  the keeper's six-crank batch is a v1 transaction (1,546 B / 40 keys), as §6.4's table says.
- **Depth:** every curve leg peaks at height 3 (short.win 1 → pump 2 → Token / System / pump's
  event 3), a synthetic close, liquidation and expiry included, and so does a curve listing
  (short.win 1 → the ATA program / pump 2 → Token / System / pump's event 3): two wrapper levels
  fit (peak 5), a third fails with `CallDepth`. `sol_settle` (S3 after a synthetic pool part
  included), `crank_price`, `sweep_trader` and `flag_venue_upgraded` peak at 2 and fit three.
- **`migrate_v2`:** since pump-final a position past `rT` exits in one instruction (the synthetic
  rows above); `migrate_v2` stays the keeper's own transaction. After a third party's exact
  completion (`pc = 0`) and after a synthetic one (`pc ≠ 0`, a third party's or short.win's
  own) alike it is exactly **53** entries (51 + 2), peak **4** (A15), 330.9 k CU with its
  ComputeBudget instructions (within the keeper's `MIGRATE_CU` of 400 k and ECON T7/T8's
  322,371–358,072) and 912 B as v0 + ALT. The retired straddle bundle's tx1 (the
  `complete_curve` helper) measured 12 entries, 111.3 k CU, peak 3
  (`git log -- tests/bench/m5_cu/trace.md`).
