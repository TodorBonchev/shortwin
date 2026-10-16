# m2_cu — M2 CU bench and T-TRACE

This is the M2 compute and trace gate (ARCHITECTURE §6.1, §6.2, §8.2 T-CU / T-TRACE, §9 M2).

- **CU bench** (`tests/m2_cu.rs` → `results.md`): every M2 instruction — `open_short`,
  `close_short`, `add_margin`, `liquidate`, `expire`, `sol_settle`, `crank_price`,
  `set_route_broken`, `set_venue_extras`, `sweep_trader` (brought forward from M6 by
  M2-SEC-4) — on short.win's **test-venue** SBF build against the
  TEST-ONLY mock venue, with Mollusk 0.15.1 (the figure the budget applies to) and LiteSVM
  0.16.0 (a cross-check that must agree to the unit, plus the instruction's trace entries).
  Budgets are ARCH §6.1's transaction limits where §6.1 has a row, and a class limit where it
  does not; `results.md` names the source of each.
- **T-TRACE** (`tests/t_trace.rs` → `trace.md`): the worst-case M2 transactions — client
  transactions with their two ComputeBudget instructions and normalisation, an atomic sandwich
  bundle, two opens in one transaction, a batch of six cranks that all start a route chain,
  twenty top-ups, and the keeper's `[crank, liquidate | expire | sol_settle]` — each within
  `MAX_INSTRUCTION_TRACE_LENGTH = 64` entries (every top-level instruction plus every CPI at
  any depth). A second test proves the counting against the runtime: 62 entries land, 65 are
  refused with `MaxInstructionTraceLengthExceeded`.

The mock trades like ARCH §2.4.1's Raydium row (an SPL-Token WSOL side, one venue CPI with two
`transfer_checked`, no venue-side rent), so the Raydium budgets and trace figures apply.

```sh
# from the repository root
anchor build --tools-version v1.57 --arch v3   # target/deploy/mock_cpamm.so (and the release shortwin.so)
scripts/build-test-venue.sh                    # target/test-venue/deploy/shortwin.so
cargo test --manifest-path tests/bench/m2_cu/Cargo.toml --locked -- --nocapture
# SHORTWIN_TEST_VENUE_SO / MOCK_CPAMM_SO override the ELFs;
# M2_CU_WRITE=1 rewrites results.md, M2_TRACE_WRITE=1 rewrites trace.md.
```

Every M2 handler has landed, so every case and every transaction must execute: a refusal fails
the bench (the M2 merge removed the tracks' temporary `NotImplemented` and the bench's
"pending" rows with it).

## The keeper-exit cases

Their pre-states are **self-checked with `shortwin-math`**, so they measure the executing path:

- `liquidate`: the price ×2.3 held for 40 cranked seconds — the spot leg and the EMA leg (the
  newest `liq_persist_n` ring entries at the capped fee) are below `cr_liq`, the reference is
  fresh, the buyback is allowed (`B̂_live + ert ≤ V − K_s`, `σ ≤ σ_max`) and `B̂_live ≤
  max_in_liq` (`bounds::liq_reference` on `L_b = max(L_live, L_ema, L_slow)`; the mock has no
  Raydium depth cap).
- `expire`: past the deadline, not CR-liquidatable, a fresh reference, `B̂_live ≤
  ⌊B̂_ref·(1 + δ_exp)⌋` (`bounds::expiry_reference`).
- `sol_settle`: the no-change call on a healthy position (the keeper's per-second crank), and
  S3 on an expired position whose route Emergency flagged, the chain unbroken (cranks every
  30 s) for `s3_wait_s` + 30 s.

A `sol_settle` that creates a `ShareEpoch` record (6 trace entries, 7 with a pre-funded record
address; ≈ 93–96 k CU) is measured by `tests/litesvm/tests/settle_it.rs` (the full wind-down
scenario), not here.

## Workspace and pins

A standalone workspace with its own `Cargo.lock`, like `tests/bench/m1_cu`, so Mollusk does not
enter the on-chain workspace's lockfile. The lockfile was seeded from `tests/bench/m1_cu`'s:
anchor 1.2.0, LiteSVM 0.16.0 and the Agave runtime crates resolve to the same versions as the
program tests, held at 4.2.2 (C1-14). The only difference from the M1 bench's resolution is
this package's own dependency list (`shortwin-math` for the scenario self-checks).

## Findings (see `results.md` and `trace.md` for the figures)

- Mollusk and LiteSVM report the same CU for every measured case (the bench fails otherwise).
- `open_short` varies by ≈ 1.5 k CU per extra bump iteration from run to run: it derives the
  new `ShortPosition` (and a first `WalletExposure`) PDA with a canonical bump search for a
  fresh random borrower key (as the M1 bench notes for its PDA-creating cases), and so do the
  traces that hold an open (the sandwich, four opens of one wallet). Some rows also vary by
  19–21 CU between runs (the closes, `expire`; an open on top of its bump search); the others
  are stable. `results.md` and `trace.md` hold one run's figures: a re-run that differs from them
  only by this noise is not committed (AUDIT.md §10).
- Every measured instruction is far inside its §6.1 budget.
- **ARCH §6.2 is low for `open_short` on the mock/Raydium path**: 15 entries for a wallet's
  first open (the table's 13 omits the `ShortPosition` `init` and the `WalletExposure`
  `init_if_needed`, one System CPI each), 14 for a later open, 18 with normalisation against
  the table's maximum of 15; `close_short` with normalisation is 12 against 11 (the
  `TraderSwept` event). All far below 64.
- `liquidate` and `expire` are 9 entries each (12 with a trader sweep), ≈ 94 k and ≈ 89 k CU;
  `sol_settle` is 1 entry as a no-op (≈ 38 k CU) and 2 when it settles S3 (≈ 58 k CU). The
  keeper's `[ComputeBudget ×2, crank_price, liquidate | expire]` is 13 entries, `[…,
  sol_settle S3]` 6.
- **`crank_price` is 3 entries when a route chain starts or clears** (its second event,
  `RouteChainChanged`), against §6.2's 2; a keeper batch of `k` cranks is then up to `2 + 3k`
  (k = 6: 20), not `2 + 2k`.
- `add_margin` is 3 entries (the instruction, its System transfer, `MarginAdded`); twenty
  top-ups fit one transaction (62 entries), twenty-one do not.
