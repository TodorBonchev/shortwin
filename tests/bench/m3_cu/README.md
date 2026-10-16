# m3_cu — M3 CU bench, T-TRACE and stack depth on Raydium (with M4 pool selection)

This is the M3 compute, trace and depth gate for the Raydium venues (ARCHITECTURE §6.1–§6.4,
§8.2 T-RAY-CU / T-RAY-TRACE / T-CU / T-TRACE, §9 M3), and since M4 also the gate of the Raydium
pool-selection instructions `propose_pool` and `execute_pool_switch` (§9 M4; §6.1 budget
100 k). Everything runs the **release** short.win
(`anchor build --tools-version v1.57 --arch v3`) against the **dumped mainnet** Raydium AMM v4
and CPMM programs, pools, mints and configs of `tests/fixtures/`, on the LiteSVM harness's
mainnet fork (`shortwin_litesvm_tests::fork` / `raydium`).

- **CU bench** (`tests/m3_cu.rs` → `results.md`): every Raydium-path instruction —
  `create_market`, `open_short`, `close_short`, `liquidate`, `expire`, `sol_settle`,
  `crank_price`, `flag_venue_upgraded`, `revalidate_venue`, and (M4) `propose_pool` /
  `execute_pool_switch` — on AMM v4 (SPL Token; AMM v4 is SPL
  Token only), CPMM with an SPL base and CPMM with a Token-2022 base, plus the CPMM variants that
  change the path (creator fee on the SOL side, WSOL as token 1). Mollusk 0.15.1 gives the figure
  the budget applies to; LiteSVM 0.16.0 runs the same instruction as a cross-check that must
  agree to the unit, and gives the instruction's trace entries, its peak stack height and the
  Raydium program's own CU (its token CPIs included). Budgets are ARCH §6.1's transaction limits;
  `results.md` names the source of each.
- **T-TRACE** (`tests/t_trace.rs` → `trace.md`): the worst-case Raydium transactions — the
  largest client open and close with their two ComputeBudget instructions and normalisation, an
  atomic sandwich of an open with the venue's own swaps, four opens of one wallet, crank batches
  whose every crank starts a route chain, the keeper's `[crank, liquidate | expire | sol_settle
  S3]`, both latch flags — each within `MAX_INSTRUCTION_TRACE_LENGTH = 64` and at its exact
  expected count, with their CU and wire sizes (legacy; v0 with the Raydium part of the protocol
  ALT, A28). Two more tests: five opens of one wallet are refused by the runtime itself
  (`MaxInstructionTraceLengthExceeded`), and four cranks fit a v0 + ALT transaction while five
  do not (U25).
- **Stack depth** (`tests/t_depth.rs` → `depth.md`): every Raydium-path instruction called
  directly and through one, two and three levels of the TEST-ONLY CPI wrapper `wrapper/` (it
  forwards one instruction, as a Squads vault transaction or an integrator's program would call
  short.win), on LiteSVM's **mainnet** feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`; Mollusk is
  not used here: it enables SIMD-0268's limit of 9).

The per-instruction call shapes (which program at which height, in order) are pinned by
`tests/litesvm/tests/trace_raydium_it.rs`, and T-LATCH on the dumped ProgramData (real loader
upgrades) by `tests/litesvm/tests/latch_raydium_it.rs`; both run in the program-tests CI job.

```sh
# from the repository root
anchor build --tools-version v1.57 --arch v3                     # target/deploy/shortwin.so (release)
cargo-build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path tests/bench/m3_cu/wrapper/Cargo.toml \
  --sbf-out-dir tests/bench/m3_cu/target/deploy                  # the wrapper (t_depth only)
cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --locked -- --nocapture
# SHORTWIN_SO / M3_WRAPPER_SO override the ELFs;
# M3_CU_WRITE=1, M3_TRACE_WRITE=1, M3_DEPTH_WRITE=1 rewrite results.md, trace.md, depth.md.
```

A case or transaction that does not execute, a Mollusk figure above its budget, a disagreement
of the two harnesses, a trace count other than the expected one or above 64, a peak stack
height above 3, a client or keeper transaction above 1,232 B as v0 + ALT (the v1 keeper batch:
above 4,096 B), or a wrapper level that does not reach exactly `direct + levels` fails the
bench.

## Pre-states

- Every case builds a fresh fork (`Env::ray_fork`), lists the pool with the real
  `create_market(RaydiumV4 | RaydiumCpmm)`, lends `40·Q` and waits out the listing window (its last
  `PRICE_WARMUP_CRANKS` = 10 seconds cranked every second: the price warm-up opens need since the
  M4 verification, M4V-5), where
  `Q = min(X/400, 2 SOL of notional)` (the `ray_it` size). Every transaction that builds the state
  goes through `Env::send`, so the invariant hooks run.
- The keeper exits are prepared the way a keeper decides: `liquidate` after a real whale buy
  lifts the price ≈ 2.2× (√2.2 − 1 of the SOL reserve), cranked second by second until a
  `simulate_transaction` of the keeper's `[crank_price, liquidate]` succeeds (the simulation
  commits nothing); `expire` three cranked seconds past the deadline; S3 on a pool halted by its
  admin's own instruction (AMM v4 `SetParams` status 2, CPMM `update_pool_status` 4), cranked
  every 30 s from just before the deadline until the simulated `[crank_price, sol_settle]`
  settles S3. A case therefore measures the executing path, not a refusal.
- The borrower and the sandwich attacker have **fixed** keys, so the canonical-bump searches of
  the position and exposure PDAs and the venue's key comparisons cost the same on every run: the
  reports are reproducible (the M2 bench used random keys, and an open varied by ≈ 1.5 k CU per
  extra bump iteration).
- The latch cases change the dumped ProgramData's deploy slot in place
  (`fork::set_deploy_slot`); the U5 case writes a synthetic 10,485,760-byte ProgramData (pump's
  size; pump is not dumped) at pump's compile-time address.

- **M4 pool selection** (`pool_cases`): every `propose_pool` outcome that writes — a list-only
  market's adoption (Fartcoin with its real mainnet bonding curve planted, so the D1 detection
  reads a curve that graduated elsewhere; USELESS), a listing-window replacement (Fartcoin's
  reversed pool listed thin — LP burned to λ = 50 %, topped up to just over the locked-depth
  floor (`pool_min_real_sol` and its rounding, M4-SEC-1 / -3), as
  `ray_sec_it`'s M3-SEC-3 case — replaced by the main pool), a zero-position adoption after the
  window onto a CPMM pool twice as deep, and on a halted incumbent with a position open (halted
  by its admin's own instruction, observed every 30 s for 150 s) a fast-path window opened on a
  candidate at 40 % of `D` and improved by one at 60 % — and both `execute_pool_switch` outcomes
  after the 60 s window: the fast adoption with the position open, and a cancellation (the
  incumbent resumed by its admin); since M4R also the `crank_price` that ends the post-change
  warm-up after that fast adoption (eight per-second cranks of the new pool before it: the seed
  over the eight post-change entries and the guard cleared at the gate). The candidates the fixtures lack are **planted** CPMM pools
  (`shortwin_litesvm_tests::pool_select::plant_cpmm`: USELESS's dumped pool bytes on fresh keys,
  with the token's mint, their own vaults, LP mint and observation; the dumped ELF trades
  them). Before every adoption the observation ring is full (`RING_LEN` + 2 cranked seconds), so
  the A72 ring rewrite is measured at its largest. The planted keys come from
  `Pubkey::new_unique()` in a fixed order, so the figures are reproducible as well.

## Workspace and pins

A standalone workspace with its own `Cargo.lock`, like `tests/bench/m2_cu`, so Mollusk does not
enter the on-chain workspace's lockfile. The lockfile was seeded from `tests/bench/m2_cu`'s and
differs from it only in this workspace's own two packages: anchor 1.2.0, LiteSVM 0.16.0 and the
Agave runtime crates resolve to the same versions as the program tests, held at 4.2.2 (C1-14).
`wrapper/` is a workspace member whose dependencies (`solana-program-entrypoint`, `solana-cpi`
and their component crates) were already in that lockfile through anchor-lang; it is built for
SBF with `cargo-build-sbf --tools-version v1.57 --arch v3` (never deployed) and is a plain
library on the host.

## Findings (see `results.md`, `trace.md` and `depth.md` for the figures)

- **Mollusk and LiteSVM agree to the unit on all 67 cases** (46 M3, 21 M4 / M4R).
- **CU, all within ARCH §6.1:**
  - `open_short`: 98.7–130.6 k (limit 150 k). The venue CPI is 15.4 k (AMM v4) and 25.1 k /
    28.0 k (CPMM SPL / Token-2022), as R-RAY §4.3 measured, but short.win's own part is
    ≈ 83–102 k against §6.1's estimate of 44–59 k: the worst case (a normalising first open on
    Token-2022 CPMM) leaves 19.4 k of headroom. (The marks "switch only if all fit" reads,
    `max_q_hwm` and FIT-2's `min_open_fee_ppm`, cost ≈ 8 CU an open.)
  - `close_short` 68.3–89.3 k, `liquidate` 91.0–111.6 k, `expire` 86.6–105.1 k (limit 200 k;
    §6.1 estimated 70–107 k for the keeper exits; M4F-1's reference cap included; FITS-2 (a)'s
    spot leg at `max(L_live, L_ema')` ≈ +80 CU).
  - `sol_settle` 39.4–43.0 k as a no-op, 47.4–53.1 k for S3 (limit 150 k; FITS-2 (a) ≈ +37 CU).
  - `crank_price` 22.9–27.9 k (limit 60 k; §6.1 estimated 18–23 k); the last post-change
    warm-up integration (M4R: the seed over 8 post-change entries, its step bound — M4X-1,
    +0.9 k — and the guard check) 27.0 k, below a steady roll.
  - `create_market` 59.2–71.2 k (limit 250 k).
  - **U5**: `flag_venue_upgraded` costs 6,026 CU and `revalidate_venue` 8,438 CU on the
    1,406,429-byte AMM v4 and 793,869-byte CPMM ProgramData **and** on a 10 MiB one: copying the
    account into the input region is not metered in CU. §6.1's 400 k request is far more than
    needed.
  - The buy leg's venue CPI (`swap_base_out_v2` on AMM v4) is 15.9–16.0 k, a little above
    R-RAY's 15.2–15.5 k range for the v2 swaps.
- **M4, pool selection** (budget 100 k, ARCH §6.1's estimate 25–35 k; since M4R an adoption
  writes the anchor `D/λ_min` and the restart marks, reading no liquidity of the pool — ≈ 1.1 k
  less than M4V's two-sample minimum — and a window proposal also prices the candidate for the
  floor at `min(P_ema, P_cand)`, ≈ 0.5 k more; since "switch only if all fit", owner decision of
  2026-09-29, both instructions read the oracle cache, ≈ 2.0 k, and a window with a position open
  evaluates the fit — the candidate's conservative `X₀`, its tier, the per-position and OI caps —
  at `min(P_ema, P_cand)` and at `P_ema`, ≈ 6.8 k each, its fee-scaled caps at FIT-2's
  `min(min_open_fee_ppm, φ_cand)`, ≈ 8 CU each, and FITS-1's 25-bps ceiling, ≈ 4 CU): `propose_pool`
  27.1–27.4 k for a list-only market's first pool (its price state starts fresh and marked),
  39.2–40.3 k for a window opened
  or improved with a position open, 42.3–45.6 k for an adoption that rewrites a full ring (zero
  position; listing window); `execute_pool_switch` 47.9–48.8 k for the fast adoption (a position
  open, the fit judged, ring rewritten), 29.8–30.0 k for a cancellation (the fit not evaluated).
  So the fast adoption is ≈ 14 k above §6.1's estimate, with 51 k of headroom.
  Every path makes exactly one CPI (its `emit_cpi!` event): 2 trace entries, peak height 2,
  three wrapper levels fit; with both ComputeBudget instructions a pool-selection transaction is
  4 entries, ≤ 727 B legacy and ≤ 608 B as v0 + ALT (18 accounts with the oracle cache: CPMM
  incumbent and CPMM candidate).
- **Trace:** a wallet's first open is 15 entries, a repeat borrower's 14, a normalising open 18;
  every exit is 9 (12 normalising); a crank is 2, or 3 when the pool just halted
  (`RouteChainChanged`); S3 is 2, the no-op `sol_settle` 1; `create_market` 7; the latch
  instructions 2. As M2 found on the mock, ARCH §6.2's Raydium row is low: 13 (≤ 15) for an open
  omits the position `init` and the exposure `init_if_needed`, the normalising open is 18 and
  the normalising exit 12 against the table's maxima 15 and 11, and a crank that starts a route
  chain is 3, so a keeper batch of `k` cranks is up to `2 + 3k` entries. The worst transaction
  is four opens of one wallet at 59 entries; a fifth open (73) is refused by the runtime.
- **Depth:** every Raydium trading path peaks at height 3 (short.win 1 → venue 2 → token 3).
  Two wrapper levels fit (peak 5) and a third fails with `CallDepth` at the venue's token CPI;
  `sol_settle`, `crank_price`, `create_market` and `flag_venue_upgraded` peak at 2 and fit three.
  So short.win can be CPI'd **twice** on the Raydium paths, not only once as ARCH §6.3 words it.
- **Sizes** (a T-SIZE preview): a client open or close and a keeper's `[crank, exit]`, with
  their ComputeBudget instructions, fit 1,232 B even as legacy transactions (at most 1,166 B: a
  normalising Token-2022 CPMM open), and at most 873 B as v0 + ALT. The sandwich (≤ 1,406 B
  legacy) and four opens (≤ 1,514 B legacy) need v0 + ALT (≤ 1,209 B). A v0 + ALT transaction
  holds four Raydium cranks (1,113 B; five are 1,327 B), the byte estimate of ARCH §6.4; the
  six-crank batch is 1,542 B as v0 + ALT, so it is a v1 transaction (44 accounts).
