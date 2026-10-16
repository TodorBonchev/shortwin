# m6_cu — M6 CU bench, T-TRACE, T-SIZE, T-DEPTH and the graduation transition on the PumpSwap venue

This is the M6 compute, trace, size and depth gate for the PumpSwap venue and the `→ PumpSwap`
graduation transition (ARCHITECTURE §6.1–§6.4, §8.2 T-CU / T-TRACE / T-SIZE / T-DEPTH, §9 M6).
Everything runs the **release** short.win (`anchor build --tools-version v1.57 --arch v3`) against
the **dumped mainnet** PumpSwap, pump and pump-fees programs and the canonical pools of
`tests/fixtures/` (groups `pamm_state`, `pamm_synthetic`, and the graduated coins of `pump_state`),
on the LiteSVM harness's PumpSwap fork (`shortwin_litesvm_tests::pamm` / `fork`).

- **CU bench** (`tests/m6_cu.rs` → `results.md`): every PumpSwap-path instruction —
  `create_market(Auto)` (a PumpSwap listing with the pAMM init set on the 261- and 301-byte pools),
  `open_short` (a wallet's first open on the 301 B / 261 B / 211 B / cashback pools, a repeat
  borrower, normalising, the costliest path), `close_short` (plain, normalising, the costliest
  path), `liquidate` (after a real ×2.8 pump, and on the migrated pool after the warm-up), `expire`,
  `sol_settle` (the no-op, S3 under the buy halt), `crank_price` (roll and record, record only,
  the first crank after a coin-creator change, which re-caches the creator (M6-SEC-4),
  the **`PumpCurve → PumpSwap` graduation transition** that rewrites the ring, and the same
  transition on a market re-detected onto the curve, which derives the PumpSwap cache there),
  `sweep_trader`'s 6-account token leg, and `flag_venue_upgraded` / `revalidate_venue` on the dumped 10 MiB PumpSwap
  ProgramData — on SPL Token and Token-2022 pools. Mollusk 0.15.1 gives the figure the budget
  applies to; LiteSVM 0.16.0 runs the same instruction behind a 1.4 M `SetComputeUnitLimit` as a
  cross-check that must agree to the unit, and gives its trace entries, its peak stack height and
  PumpSwap's own CU. Budgets are ARCH §6.1's PROPOSED transaction limits.
- **T-TRACE / T-SIZE** (`tests/t_trace.rs` → `trace.md`): the worst-case PumpSwap transactions —
  the largest client open and close (normalising and costliest), the keeper's `[crank, liquidate |
  expire | sol_settle S3]`, the **graduation transition crank**, the **transition with a
  borrower's first close** (the transition crank plus the borrower's own close on the pool in one
  transaction; the retired straddle bundle's tx3, pump-final D2), a PumpSwap listing,
  `sweep_trader`, the **post-wipe reward claim bundle** (`[create_idempotent ATA(trader, WSOL),
  pAMM claim_cashback, sweep_trader]` — `claim_venue_rewards` was not built, ARCH §3.9), and the
  two-flag latch batch — each within `MAX_INSTRUCTION_TRACE_LENGTH = 64` and at its exact expected
  count, within ARCH §6.1's CU limit, with its wire sizes (legacy; v0 through the protocol ALT of
  A28 with its pump part). The per-instruction call shapes are pinned by
  `tests/litesvm/tests/trace_pumpswap_it.rs`; the full T-SIZE (largest extras, a separate fee
  payer, both tables, executed as v0) by `tests/litesvm/tests/tx_size_pumpswap_it.rs`; T-LATCH
  (real PumpSwap loader upgrades) by `tests/litesvm/tests/latch_pumpswap_it.rs`; and T-REPLAY-0428
  (the 2026-04-28 added-trailing-account interface change recovered through the extras sets) by
  `tests/litesvm/tests/replay_0428_it.rs`.
- **Stack depth** (`tests/t_depth.rs` → `depth.md`): every PumpSwap-path instruction called
  directly and through one, two and three levels of the TEST-ONLY CPI wrapper (`wrapper/`), on
  LiteSVM's **mainnet** feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`; Mollusk is not used here —
  it enables SIMD-0268's limit of 9).

```sh
# from the repository root
anchor build --tools-version v1.57 --arch v3                      # target/deploy/shortwin.so (release)
cargo-build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path tests/bench/m6_cu/wrapper/Cargo.toml \
  --sbf-out-dir tests/bench/m6_cu/target/deploy -- --locked       # the wrapper (t_depth)
cargo test --manifest-path tests/bench/m6_cu/Cargo.toml --locked -- --nocapture
# SHORTWIN_SO / M6_WRAPPER_SO override the ELFs;
# M6_CU_WRITE=1, M6_TRACE_WRITE=1, M6_DEPTH_WRITE=1 rewrite results.md, trace.md, depth.md.
```

A case or transaction that does not execute, a Mollusk figure above its budget, a disagreement of
the two harnesses, a trace count other than the expected one or above 64, a peak stack height other
than the expected one (above 3 for an instruction), a transaction above its §6.1 CU limit, a client
or keeper transaction above 1,232 B as v0 + ALT, or a wrapper level that does not reach exactly
`direct + levels` fails the bench.

**M6R-1 (the owner's decision of 2026-09-30; ARCH changelog 56).** Every PumpSwap ReadSet carries
the pool's LP mint (P6), which the adapter key-checks against `pool.lp_mint` and reads for the
pool's locked depth `D` (the burned creation LP's; the prologue writes it into `pool_d` and, during
a warm-up, caps `L_slow` at it): every PumpSwap-ReadSet transaction is 33 B longer (the key inline,
its index) and its instruction ≈ 2.1–2.9 k CU dearer (the parse of the pool for P6's check, the
mint, `λ` and `D`; ≈ 4.7 k on an open, which reads the venue before and after its sale), all within
§6.1's limits. The position size is ≈ 8 SOL of notional (10 SOL before: on the `mature` pool, whose
third-party LP puts `D` at 92.98 % of `√k`, a wallet's second 10-SOL position is over its wallet
cap on `X₀ = 0.93·X`).

## Pre-states

- Every PumpSwap trade case lists its pool with the real `create_market(Auto)` and its pAMM init
  set, lends `40·q`, waits out `market_warmup_s` and the price's post-change warm-up, and — on a
  pool whose boost vault is still funded — waits out `boost_window_s` (the boost gate applies to
  opens only). `q = min(1 % of the base reserve, ≈ 10 SOL of notional)` — the `pamm_it` size.
- The pools: `mature` (Token-2022, 301 B, boost spent — the main 301-byte pool), `legacy261` (SPL
  Token, 261 B), `legacy211` (SPL Token, 211 B, the short-pool growth case), `cashback` (Token-2022,
  301 B, the cashback ATA path), and `fresh` (Token-2022, 301 B, boost active) for a listing.
- **The costliest paths** change the pool's `coin_creator` after the listing to a fixed key whose
  creator vault is **empty** — the adapter re-derives both creator-vault slots (T11, T12) from the
  live creator until a `crank_price` re-caches it (M6-SEC-4), so the costliest liquidation applies
  the change after its second's crank — pick the **last** protocol and buyback recipients of the live `GlobalConfig` (the
  longest key scans), create the accounts the existence requirement needs, and normalise (lamports
  donated to the trader PDA and tokens to `trader_base_ata`).
- **The graduation transition** builds a curve market on the near-complete fixture curve with two
  open positions, completes it, runs a real `migrate_v2`, and measures the next `crank_price`, which
  moves the market `PumpCurve → PumpSwap` and rewrites the ring to the pool's `lp_supply` (§2.5,
  M5-SEC-4). The **graduation-path** liquidation and the **transition with a borrower's first
  close** run after that.
- The keeper exits are prepared the way a keeper decides: `liquidate` after a real ×2.8 pump cranked
  second by second until the `[crank, liquidate]` simulates; `expire` two cranked seconds past the
  deadline; `sol_settle` S3 under the buy halt (`disable_flags` bit 3: the venue cannot execute, the
  crank starts the S3 chain, S3 settles `s3_wait_s` later, no venue CPI).
- The borrowers, the keeper and a changed creator have **fixed** keys, so the reports are
  byte-identical across runs, except on the graduation path: its two borrowers are fresh keys
  (`env.borrower()`), and its liquidation varies by 19 CU between runs.

## Findings (see `results.md`, `trace.md` and `depth.md` for the figures)

- **Mollusk and LiteSVM agree to the unit on all CU cases.**
- **CU, all within ARCH §6.1's PROPOSED limits:**
  - `open_short` 174.9–201.2 k (limit 250 k; §6.1 estimated 123–138 k). PumpSwap's own `sell` is
    64.0–82.9 k. The largest is the normalising Token-2022 open at 201.2 k; the costliest path
    (creator change, last recipients) is 199.9 k.
  - `close_short` 148.7–171.2 k (limit 250 k). PumpSwap's `buy` 73.0–88.0 k. `liquidate` /
    `expire` 167.3–192.9 k (limit 300 k), the graduation-path liquidation on the migrated pool the
    largest.
  - `sol_settle` 48.9 k as a no-op, 55.0 k for S3 under the buy halt (limit 150 k).
  - `crank_price` 27.1–31.3 k for a roll and record, **32.1 k for the `PumpCurve → PumpSwap`
    graduation transition** (it writes the pool cache and rewrites the 8-entry ring), within the
    60 k limit.
  - `create_market(Auto)` 113.4–134.4 k for a PumpSwap listing (the `trader_base_ata` and pAMM
    UVA created; limit 250 k).
  - `sweep_trader`'s 6-account token leg 21.9–24.5 k (18.5 k with nothing to sweep; no §6.1 row,
    the 60 k lender class). **`claim_venue_rewards` was not built** — PumpSwap's post-wipe reward
    claim is the top-level `claim_cashback` + `sweep_trader` UVA-reading pattern, measured as the
    bundle in `trace.md`.
  - **U5 on the real accounts:** `flag_venue_upgraded` 6,026 CU and `revalidate_venue` 8,438 CU on
    the dumped 10,485,760-byte PumpSwap ProgramData, as on pump's: the input-region copy is not
    metered.
- **Trace** (ARCH §6.2's PumpSwap column: open 20, exits 16, each an estimate): an open is **22–23**
  entries for a wallet's first position, **26 normalising** and **26 for the costliest path** — so
  the estimate's ≤ 23 is exceeded by the normalising and costliest paths; an exit is 16–17 plain,
  **20 normalising / costliest** (above the ≤ 19 estimate). The keeper's `[crank, liquidate]` is 21
  entries (24 for the costliest). The graduation transition crank is **6** entries in its
  transaction (the crank plus `VenueStateChanged` and `RouteChainChanged`), peak 2; that crank
  plus a borrower's close in one transaction is 23, peak 3. A PumpSwap listing is 17 (peak 3);
  `sol_settle` S3 is 6 in the keeper transaction; `sweep_trader`'s token leg is 6; the post-wipe
  reward-claim bundle is 14, peak 2.
- **Sizes** (v0 through A28 with its pump part; the largest-extras worst case is
  `tx_size_pumpswap_it`'s): a client open is 1,040–1,041 B / 40–41 accounts with its two
  ComputeBudget instructions; a close 1,004–1,005 B; the keeper's `[crank, liquidate | expire]`
  1,053–1,054 B; the transition crank with a close 1,096 B; a PumpSwap listing 685 B; the post-wipe claim 767 B;
  `sweep_trader`'s token leg 606 B. Every client and keeper transaction fits a v0 transaction
  through the protocol ALT (≤ 1,232 B, ≤ 64 accounts).
- **Depth:** every PumpSwap leg peaks at height 3 (short.win 1 → pAMM 2 → pump-fees / Token /
  System / pAMM's event 3), and so does a PumpSwap listing (short.win 1 → the ATA program / pAMM 2
  → Token / System / pAMM's event 3): two wrapper levels fit (peak 5), a third fails with
  `CallDepth`. `sol_settle`, the graduation transition crank, `sweep_trader` and
  `flag_venue_upgraded` peak at 2 and fit three wrapper levels.

## Workspace and pins

A standalone workspace with its own `Cargo.lock`, like `tests/bench/m5_cu` (whose lockfile seeded
this one), so Mollusk does not enter the on-chain workspace's lockfile: anchor 1.2.0, LiteSVM
0.16.0 and the Agave runtime crates resolve to the same versions as the program tests, held at
4.2.2 (C1-14). `wrapper/` is a copy of `tests/bench/m5_cu/wrapper` under its own id
(`short.win-m6-cpi-wrapper-program`), built for SBF with `cargo-build-sbf --tools-version v1.57
--arch v3` (never deployed) and a plain library on the host.
