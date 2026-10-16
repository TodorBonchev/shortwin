# shortwin-fuzz

short.win's invariant fuzz harness (ARCHITECTURE §8.3, §8.4; M7): seeded **action sequences** on
LiteSVM 0.16 through the program-test harness (`shortwin-litesvm-tests`), so every transaction
runs the harness's invariant hooks — I1–I13, I15–I19 and A26 after every instruction and after
every prefix of a multi-instruction transaction (I14 is structural, plus the fuzzer's omission
and replacement checks) — against the test-venue build and the sha-checked mainnet-fork
fixtures.

```sh
anchor build --tools-version v1.57 --arch v3 --ignore-keys   # target/deploy/{shortwin,mock_cpamm}.so
scripts/build-test-venue.sh                                  # target/test-venue/deploy/shortwin.so
cargo run -p shortwin-fuzz --release -- run --profile all --duration 1h --jobs 8 --out fuzz-out --minimize
cargo run -p shortwin-fuzz --release -- replay fuzz-out/crash-<profile>-<seed>-<sig>.json
cargo run -p shortwin-fuzz --release -- tmin   fuzz-out/crash-<profile>-<seed>-<sig>.json
cargo run -p shortwin-fuzz --release -- one --profile curve --seed 42 --actions 400   # `run --actions 400`'s run of that seed, full summary
cargo run -p shortwin-fuzz --release -- one --profile curve --seed 42 --length 280    # exactly 280 actions
cargo test -p shortwin-fuzz --release                                                  # smoke, corpus replays, self-tests
```

The program paths are the harness's (`SHORTWIN_SO`, `SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`).
The fuzzer needs speed, not the deploy profile's fat LTO:
`CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` builds it in ~2 min
(CI does the same). `run` without `--duration` does one run; `--runs N` caps the count; each job
(`--jobs`, default: every core) owns one seed stream (`seed + job + k·jobs`: runs at `--seed s` and
`s + 1` on the same jobs share all but one stream's first seed, so a run meant to add coverage takes
a seed far from earlier runs'; the 24 h runs 2 and 4 re-ran runs 1's and 3's); a run's length varies
with its seed from a quarter of `--actions` to all of it (so the wind-down's liveness check meets
states at every depth: `runner::run_length`, `--actions/4 + splitmix64(seed ^ 0x1e57) % (3·--actions/4
+ 1)`); since M10 `one` derives the same length from its own `--actions` (default 300, as `run`), so
`one --profile P --seed S --actions N` replays exactly the run `run --actions N` made of seed `S`
(`--length N` runs exactly `N` actions instead), and an anomaly's `first` cursor reads `profile seed
#i/n \`action\`` (or `… wind-down after n actions`) with the run's length `n`; the exit status is 1
if any run failed. `run` prints progress every
minute and a summary (also `OUT/summary.json`).

## Why not Crucible

ARCH §8.4 named Crucible (`anchor fuzz`). Anchor 1.2.0 embeds crucible-fuzz-cli 0.2.1, whose
harness crates (crucible-test-context / crucible-fuzzer 0.3.0-alpha.1) pin `litesvm =0.15.2` and
`solana-program-runtime ~4.1.1`; this repository pins `litesvm =0.16.0` and the Agave line at
`~4.2.2` in one lockfile (C1-14), and the 4.1 and 4.2 runtime crates cannot coexist in it. A
Crucible harness could therefore not link `tests/litesvm` — the ~5,200 lines of I1–I18 hooks
and the fork fixtures — without a rewrite on a second Agave line. Crucible's stateful depth (15
actions by default) is also short for the protocol's clock-driven paths (a 3,600 s S3 wait,
72 h–30 d deadlines, 6 h vesting); a run here is 400 actions and warps up to three days per
action. LiteSVM 0.16's `register-tracing` feature can later add sBPF PC-coverage feedback without
a second runtime (not enabled: it would change the shared feature set).

## Runs, seeds, crash files

A **run** is `(profile, seed, action list)`: the world is set up (its choices drawn from
`seed ^ SETUP_SALT`: token program, fee, fixture, pool depth, oracle, the per-market notional
limit at 25 SOL / 1,000 SOL / its 10⁵ SOL bound, the wallet cap; and from `seed ^ M7_SALT`
whether O17's `coverage_authority` is installed — three runs in four, planted as an applied
`Authorities` change leaves it; inert otherwise), every action is applied, then the
**wind-down** (the liveness oracle below). The action list is a pure function of
`(profile, seed)` (`action::stream`); an action is `kind m a b` — the market slot and two numbers
the action interprets modulo the live state (positions, lenders, candidates), so every
subsequence of a recorded list is itself a valid list.

A failure — any panic: an invariant hook in `Env::send` (with the transaction's logs), a fuzzer
oracle (`FUZZ-ORACLE …`), the mock's differential exit oracle — is caught per action and written
as `crash-<profile>-<seed>-<hash>.json`: profile, seed, options, the action list up to and
including the failing action, the **signature** (panic location and first message line, digit
runs and keys masked) and the full message. `replay` re-runs it; `tmin` shrinks it by delta
debugging (ddmin, then single-action deletion) while the signature stays the same, writing
`min-….json`; `run --minimize` does that for the first failure of each signature.

Determinism: the actions are exactly reproducible; the keys a run creates (mints, borrowers) come
from the OS and differ between runs. Program logic does not depend on key bytes except through
PDA bumps (compute units), so a replay reproduces a failure's logic; the corpus reproducers were
each replayed repeatedly.

## Profiles (`--profile`)

| profile | build | venue(s) |
|---|---|---|
| `mock` | test-venue | one mock market (SPL or Token-2022, 0.05–1 % fee, thin / default / deep pool) with the mock's knobs: halt, fee, layout change, over-delivery, interface change |
| `mixed` | test-venue | two mock markets (SPL and Token-2022): global pauses, venue bits and config changes hit both |
| `curve` | release | the pump fork: `fresh` taken to 0–95 % by a real `buy_v2`, `legacy115`, `t22_115`, `near`; since pump-final §12 (P7) the crowd trades pump's v2 and v3 (`buy_v2` / `sell_v2`, `buy_v3` / `sell_v3` / `buy_exact_quote_in_v3`); completion (a `buy_v2` of `rT`, or a `buy_v3` overshoot that completes the curve by pump's synthetic migration), token donations to `curve_base_ata`, real `migrate_v2`, the `→ PumpSwap` transition, then the PumpSwap venue on the new pool |
| `pumpswap` | release | the PumpSwap fork: `fresh` (boost running), `mature` (third-party LP), `cashback`, `legacy261`; a third-party LP; since M10's fuzz follow-ups `fee_shock` (weight 3) and `open_near_cap` (6) three times as dense as elsewhere — M7-SEC-5's tier-boundary shape |
| `amm-v4` | release | Fartcoin's AMM v4 pool; planted AMM v4 / CPMM candidates of the token |
| `cpmm` | release | USELESS, the Token-2022 pool or the creator-fee pool; an LP provider (the program's own `deposit` / `withdraw`); planted candidates |

## Actions (ARCH §8.4)

| group | actions |
|---|---|
| trading | `open` (log-uniform from just below 0.1 SOL of notional to just above `min(X/20, T)`; one in 32 with `min_proceeds = u64::MAX`, `max_margin = 0` or `max_open_fee = 0`), `open_near_cap` (the largest `q` accepted, by bisection on clones from 5 % above the minimum notional — since M10; from `X/10⁵` it started below it on every fork pool and was skipped —; the refusal one unit above is counted), `close` (one in 32 with `max_buyback_cost = 1`), `add_margin`; on the mock, opens and closes pick an extras selection (`none` / `current` / `previous` / `stable`) |
| lenders | `deposit` (one in 101 of zero tokens), `withdraw` (a fraction, all shares, or exactly the pro-rata allowance), `claim_sol` ×1–4 **in one transaction** (M10), `cover_deficit`, `sync_lender` (signed by the lender's owner or sent by a third party; the KI-66 pot-only touch oracle below), `sweep_trader`, `dust_sweeps` (1 lamport to the trader and a sweep every minute), `lenders_out` (all out, optionally a dust holder stays, then a sweep), `jit_deposit` (`[deposit, exit, withdraw]`; since M10 the exit a liquidation or an expiry that lands, else a `sol_settle` that settles — with `b` odd after arming the settlement clock and up to 30 s, with `b` even at the position's deadline — and the withdrawal the minted shares within the allowance after the exit), ★`withdraw_then_settle` (M10, C0-9: `[withdraw, sol_settle]` in one transaction), ★`stale_receivable` (M10's fuzz follow-ups, FUZZ-M7-24H-3's shape: a position — opened near the cap if none is — left without a keeper for 1–3 days, one crank, a late lender depositing 1–3× the pool's token claims, then — with `b` even — the price steered in cranked steps until the position's `B̂_live` lies in `(V − R_due, V − K_s − ert]`, the only window where a liquidation pays less rent than it was credited (`rent = min(R_due, V − B)` while the buyback is allowed), and 10–59 keeper seconds: the late depositor is debited its share, the deficit the wind-down covers and the tripwire bounds; it reaches deficits on AMM v4 and CPMM, where `R_due` can exceed `K_s + ert`, and — rarely — on PumpSwap (a 5-minute pumpswap run: the window 3 times, 4 short exits, 1 deficit checked); on the mock and the curve the window stays empty) |
| keeper and time | `keeper_seconds` (crank every market, then liquidate / expire / sol_settle every position, each second), `crank`, `keeper_exit`, `silence` (41–940 s without observation — past `COVERAGE_GAP_S`, 40 s since the owner's decision of 2026-10-01 (OWNER-M7-12; was 31–930 s against 30 s): `CoverageGap`; with the role installed, two in three are **degraded mode** (O17): the keeper's `set_coverage_pause(true)` on every market, cleared 1–8 actions later), `warp` (1 s – 3 d), `warp_to_deadline` (±, or past the S3 tail), `outage` (venue halted, fee above the hard cap, or halted and flagged, 130 s – 2 h with the keeper settling: the no-route chain, S3, T-S3-BROKEN) |
| prices and liquidity | `trade` (on the live curve, since P7, one in three a `buy_v2` / `sell_v2`, the others `buy_v3` or `buy_exact_quote_in_v3` / `sell_v3`, a buy never above 98 % of `rT`), `shock` (×/÷ 1.05–10, held; on the curve one push in two on v3), `ramp`, `liquidity` (add / remove), `lp_wrapped_crank` (`[withdraw, crank, deposit]` and `[deposit, crank, withdraw]`), `deposit_wrapped_open`, `withdraw_wrapped_exit` |
| atomic bundles (ECON §12) | `pump_crank_dump` (`[push, crank, unwind]` 1–20 s in a row), `contrary_low` (a contrary low observation every second, with or without the keeper's, then `sol_settle`), `attacker_seconds` (1–6 attacker-only seconds up / down / flip before a settlement), `push_then_freeze`, `sandwich_exit` (`[front, close / liquidate / expire / sol_settle, back]`), `held_sandwich`, `dump_around_settle` (a dump or an LP deposit around `sol_settle`) |
| pauses and flags | `gates` (opens paused / route broken / venue halted on the market, I10's fuzz action), `global_pause`, `disable_venue` (re-enabled by Governance after the timelock), `route_broken_venue`, `venue_halt`, `fee_shock` (the mock's fee, AMM v4's swap fee, CPMM's config rates and since M10 the pump-fees `FeeConfig` of the curve — pump's `8Wf5Ti…` — and of PumpSwap — `5PHirr…` — every tier's protocol fee at 0 / 10 / 50 / 150 / 300 / 1,000 bps: below `venue_fee_cap_bps`, between the caps and above the hard cap; also `outage`'s fee branch; on PumpSwap half the shocks instead plant a tier boundary between the market caps at the spot and at `P_ema` — the live fee below it, a cheap tier above it — M7-SEC-5's shape, where the fee-scaled caps must take the fee at `P_ema`), `layout_shock`, `over_delivery`, `extras_shock` (the mock requires the per-mint PDA a template derives, or fresh keys no template names); each disturbance lasts 1–8 further actions |
| degraded mode (O17, M7) | `coverage_pause`: `set_coverage_pause` pausing or clearing one market, signed by the role's key, Governance, the Emergency vault or a stranger, with the keeper's reason or a bad one — the outcome predicted exactly (`CoverageAuthorityDisabled` while the role is inert, whoever signs — except the Emergency's clear; `Unauthorized`; `InvalidParams`); or the role rotated by Governance's timelocked `Authorities` change (a new key, or the default key: inert); ★`coverage_bracket` (M7-SEC-3): `[set_coverage_pause(false), open_short, set_coverage_pause(true)]` by the role's key — refused `CoveragePaused` on a paused market (the clear delay), ordinary gates otherwise; ★`emergency_clear` (OWNER-M7-3 (b), owner decision of 2026-10-01): the Emergency vault clears a market's coverage pause (after a hijacked key's set, or a no-op) with the role installed, unset or rotated — the whole VM unchanged but the flag and its clock (I4's coverage clause), an open in the clear delay refused `CoveragePaused` —, its set refused (`Unauthorized`, or `CoverageAuthorityDisabled` while the role is unset), and `[Emergency clear, open_short]` refused `CoveragePaused` on a paused market |
| price-wrapped opens (M7-SEC-6) | ★`wrapped_open`: a sale or a buy of the venue within the open's spot–EMA guard, `open_short`, unwound or not, in one transaction (the M5-SEC-3 / M7-SEC-4 / M7-SEC-5 class); the open's terms are I19's |
| extras sets (mock) | `extras_set`: the Emergency's `set_venue_extras` on the sell leg, the buy leg or both — the template the interface change needs, a hostile fixed-key entry, or an empty set |
| mint filter | `mint_shock`: a freeze authority planted on the market's mint for 1–8 actions, `flag_unsafe_mint` by anyone on one in eight (one-way), or on a safe mint (`MintSafe` / `MintUnsafeFlagged`) |
| fees | `sweep_protocol_fees` (anyone, to `config.fee_recipient`) |
| upgrades | `upgrade_shock` (a venue program's ProgramData deploy slot changes, with or without `flag_venue_upgraded`), `revalidate` |
| donations | `donate` to the trader, the `trader_wsol` address, `ATA(trader, WSOL)`, the PumpSwap boost ATA, the pool vault (tokens) or — on a pump venue, half of the token donations since M10 — `trader_base_ata` (tokens the next curve or PumpSwap leg moves to the pool vault, I7), or — on a curve not yet graduated, a quarter of them since P7 — `curve_base_ata` (0.1–100 % of 206.9e12 tokens, before or after a completion: the would-be pool a synthetic buy prices its pool part on, and the pool `migrate_v2` seeds; `donate-curve-base-ata`, `…-after-completion`), the market vault, a position |
| governance and oracle | `config_change` (a parameter of `shortwin_math::params::Rule` at its lower or upper bound or midway, valid, proposed, timelock waited out, applied — as a `Params`, an `EmaParams` (new version: adopted at once without positions, forced after `ema_force_delay_s`) or a `Kink` change; or an `OracleFeed` change to a second account and back, which empties the cache), `config_cancel` (a change cancelled by Governance or the Emergency vault, a `Timelock` change the Emergency may not cancel, or one left past `CONFIG_APPLY_WINDOW_S`), `oracle_crank` (`crank_sol_usd` on a re-planted mainnet `PriceUpdateV2` at the configured account: ±30 % against a 20 % step bound, 0–4 min old; or future-dated, too wide a confidence interval, not owned by the receiver) |
| pool selection | `plant_candidate`, `propose_pool`, `execute_switch`, `fast_path` (incumbent halted or over the fee cap past `pool_fast_route_s`, a window proposal, the window waited out, the execution) |
| graduation | `crowd_completes` (another user's `buy_v2` of exactly `rT` with `a` odd — `complete_curve` before P7, still parsed, with `a` made odd, so the crash files and corpus entries that name it replay the `buy_v2` they recorded — else a `buy_v3(rT + δ)` overshoot, `δ` 0.1–50 % of the would-be pool `X_s = curve_base_ata − rT`, that completes the curve by pump's synthetic migration; `fuzz:crowd-completes-v2-exact-…` / `-v3-overshoot-…`), `migrate` (checked: the `migrating_fv` oracle below), `graduation_halt` (PumpSwap `disable_flags` bit 0, M6-SEC-3) |
| PumpSwap fee buckets (M10 program batch 4) | ★`fee_buckets`: the trader's `buy_v2` / `sell_v2` of pump's 2026-10-02 redeploy (fees kept in the quote vault as `protocol_fees` @271 / `creator_fees` @279, off the VQR), or both permissionless sweeps; on `pumpswap` and a graduated `curve`, so the hooks meet pools whose payable SOL is `vault − buckets` (PUMP-2026-10-1); outcomes counted as `fuzz:fee_buckets-v2-landed` / `-v2-refused` (a cashback coin's or a halted pool's) / `-swept` / `-sweep-refused` |
| I14 | `omit_account`, `replace_account` (a remaining account of open / liquidate / expire / sol_settle / crank and — since M10 — close omitted or replaced by a fresh key; the refusals counted by instruction and code, `i14-*-refused:…`) |

## Oracles

- **The hooks** (`Env::send`): I1–I13, I15–I19 and A26 after every instruction, each proper prefix of
  a bundle replayed on a clone; I14's structural part is the harness's. ★I19 (M7-SEC-6) recomputes
  every open's terms — locked terms, deadline, tier, every cap, `N_open` and the notional limits, the
  D4 margin, the fee — from the pre-state and the harness's own venue views.
- **The gate oracle knows the coverage clear delay** (M7-SEC-3): an open within
  `COVERAGE_CLEAR_DELAY_S` of a clear must be refused.
- **I10 / I17 gates** (`world::open_gate`): an `open_short` landing while the market's or the global
  opens pause, the market's or venue's route-broken flag, a disabled venue bit, the upgrade latch or a
  non-executable venue (the harness's view) holds just before it; an exit or top-up refused with an
  open-only code (paused, flagged, latched, disabled, caps, notional, oracle, listing window).
- **Differential exits (mock, and since M10 AMM v4 and CPMM)**: every keeper `liquidate` / `expire`
  on an undisturbed mock market goes through `tests/litesvm/tests/liquidate_support.rs`'s oracle, and
  on a market whose adopted pool is an AMM v4 pool — or, since M10's fuzz follow-ups, a CPMM pool (the
  oracle is venue-generic: CPMM's fees per direction and creator-fee side, U11's exact out; held to
  the dumped ELF by `tests/litesvm/tests/raydium_cpmm_exits_it.rs`) — through
  `tests/litesvm/tests/raydium_v4_support.rs`'s (both included with `#[path]`): the decision — including `ExitBoundNotMet` against
  `⌊B̂_ref·(10⁴ + δ)/10⁴⌋`, so I11's liveness clause (I-E11) — and every lamport and token of the
  waterfall recomputed from `shortwin-math` on the pre-state.
- **Differential exits (the curve, M10)**: every keeper `liquidate` / `expire` on a market still on
  its live curve is first decided by `curve_exit` from `shortwin-math` and the readers alone —
  `pump_curve_support.rs`'s keeper view (included with `#[path]`; the legs, `B̂_live`, buyback
  allowed) and the references of `shortwin_math::bounds` on the curve's pricing at the position's
  hard cap, on a clone after a same-second crank, in the program's order (`NoRoute`,
  `EmaWarmingUp`, `ReferenceStale`, `NotLiquidatable` / `NotExpired`, …, `ExitBoundNotMet`; since
  pump-final §6 no `CurveStraddle`: past `rT` the exit is pump's synthetic migration, `B̂_live =
  B̂_v3` on the live would-be pool `X_s = curve_base_ata − rT`, and one the would-be pool cannot
  deliver has no `B̂_live`, so `UseSolSettle`, as does σ above `σ_max`) — and must land with the
  predicted `b_live`, `b_ref` and `max_in` or be refused
  with the predicted code: no `ExitBoundNotMet` once `B̂_live ≤ max_in` (I-E11), and no failure of
  pump's buyback leg when the exit was predicted to execute (only a compute-budget exhaustion is
  exempt, and it is an anomaly). The waterfall's amounts there are the hooks' (I9, I11, I12).
  **Since P7** an exit of `q ≥ rT` that lands must have completed the curve in its own instruction
  (`curve_exit::check_completion`): the market `PumpMigrating` with `migration_seen_at = now`, the
  curve complete, and pump's post-complete fields the synthetic buy's — `pcB = q − rT`, `pcQ =
  ⌈Y_sᵖ·pcB/(X_s − pcB)⌉` (`Y_sᵖ = rQ + n₁ᵖ − fee_m`, `n₁ᵖ` the curve part pump took: `⌈⌉`, or
  `⌊⌋ + 1` on an exact division, which the pool part may also take) — or both 0 for `q = rT`
  (`…:curve-differential-completes-synthetic` / `-exact`).
- **`migrating_fv` (pump-final §12, G-7; P7)**: every `SolSettle(S3)` of a market that was and
  stays `PumpMigrating` (`src/migrating.rs`) is recomputed host-side from the dumped accounts and
  `shortwin-math`'s primitives (not `quote::migrating_fv`): the pool `migrate_v2` will create, `(X₁,
  Y₁)` = (the live `curve_base_ata` after a pool part — donations included —, else 206.9e12; `rQ −
  fee_m + pcQ`), `p' = ⌈⌈p·Y₁/Y₀⌉·X₀/X₁⌉` after a pool part (`(X₀, Y₀)` the same curve's standard
  pool), `B̂_FV = buy_exact_out(q, reprice(isqrt(X₁·Y₁), p'))` at that pool's PumpSwap tier capped at
  the position's hard cap, `FV = B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due`: the event's `fv` at the stored `P_ema`,
  both box corners' `x` and `p'`, `K_s` and the lenders' `min(FV, V − K_s)` must be exactly that
  (an undefined FV — post-complete fields pump's invariants reject, §6.1.5 — `fv` 0 and the whole
  take). Every `migrate_v2` that lands must then create `(X₁, Y₁)`: its base vault the live
  `curve_base_ata` it found (`X₁` itself after a pool part; without one 206.9e12 or more, a donation
  seeded but not priced, ARCH §4.3) and its effective quote `Y₁` (at most 1 lamport below).
  Counters: `fuzz:migrating-fv-pool-part` / `-standard` / `-undefined`,
  `fuzz:migrate-pool-checked:pool-part` / `:standard` / `:donated`; `smoke::migrating_fv_after_a_
  synthetic_completion_and_a_donation` shows a 1-lamport FV difference fails the run
  (`migrating::FV_SKEW`).
- **Differential exits (PumpSwap, M10's fuzz follow-ups; decision level)**: every keeper `liquidate` /
  `expire` on a PumpSwap market is decided first by `raydium_v4_support`'s venue-generic predictor on a
  view built from the hooks' PumpSwap venue view (pricing reserves `(base vault, quote vault + VQR)`,
  `Pricing::PumpSwap` at the live tier) with the reference liquidity capped at the burned-LP depth `D`
  itself (`λ_min` = `PUMPSWAP_LOCK_MIN_BPS`, M6R-1), after a same-second crank on a clone
  (`src/pswap_exit.rs`), and must land with the predicted `b_live`, `b_ref`, `max_in` or be refused
  with the predicted code, as on the curve. The lamports are the hooks'.
- **I14 omissions and replacements**: an instruction with a remaining account omitted or replaced by a
  fresh key must fail (the hooks check nothing changed); since M10 an accepted replacement fails the
  run as an accepted omission does (before, an anomaly for review: no run ever reported one).
- **Lenders (M10)**: `claim_sol ×N` in one transaction: every claim after the first pays 0 and the
  lender's wallet gains exactly the first (V2-2), plus the position's rent when the claim closes it:
  **KI-67** (owner decision of 2026-10-08) — a claim closes the position exactly when the math
  crate's `touch` and `claim` on the pre-state leave it `pool::closable` (else `FUZZ-ORACLE KI-67`;
  `claim-closes`), and a bundle whose first claim closes it must fail atomically with
  `AccountNotInitialized` (`claim-bundle-closes`; the claim is then sent alone; such a refusal of a
  `claim_sol` after a `claim_sol` of the same position counts `claim-after-close-refused`, not a
  `framework-error` anomaly); a deposit that
  re-creates a closed position starts clean (`real`, `claimed`, chain and lock 0;
  `deposit-recreates`). The hooks' close check (`invariants::lender::closes_only_when_empty`)
  runs after every instruction. The JIT capture oracle (R3B-2, ECON §9.4): around
  a landed liquidation, expiry or settlement, while another lender holds shares, the JIT lender's
  SOL (wallet + its `LenderPosition`'s lamports + touched balance) gains over a clone in which the
  same exit lands without the deposit at most 2 lamports beyond its minted shares' part of the
  settlement's token part (`principal − top-up − credited`; 0 for a liquidation or an expiry) —
  none of the lump and, since M10-8C-1, none of the rent top-up
  `max(0, min(principal, rent) − credited_for(pos))`, which vest — and its tokens come back at most
  at the deposit's rate (a last lender is paid the unreleased vest by design, R4B-4; a settlement
  that rolls the share epoch is not checked, `jit-unchecked:epoch-rolled`).
  `[withdraw, sol_settle]` (C0-9): the withdrawal stays within the pro-rata allowance the lender had
  before the bundle, and the bundle never returns an open-only or mint-filter code.
- **The deficit tripwire (M10's fuzz follow-ups; ECON §9.4)**: before a lender covers a negative SOL
  balance (`cover_negative`, which since FUZZ-M7-24H-3 funds a deficit of any size), the deficit must
  be at most its **pro-rata share of the shortfall the landed exits realised** plus `DEFICIT_DUST`
  (0.001 SOL) — a bound kept from the exits' events and the observed receivable, not from the pool
  accounting that computed the deficit (`src/shortfall.rs`): for each landed transaction with exits on
  a market, `credited` = the receivable the transaction started from minus the receivable after it —
  stored, when the market had accrued in that second (after the keeper's crank: the transaction
  accrues nothing more), else after a same-second crank on a clone of the pre-state; each exit's lender part from its event (`Close.rent − rent_protocol`;
  `Liquidate` / `Expire` `rent` less the position's locked `rent_protocol_bps` read before it (a
  position it does not list: all protocol, the lender part 0); a settlement's
  principal `lender_amount − min(lump, lender_amount)`); each lender's `shares/S` at the exit walked
  from the pre-state through the transaction's `Deposit` / `Withdraw` events (a lender behind the
  share epoch counts every share). A lender's bound is the sum of its charges **since its balance was
  last seen not negative** (a touch on each exit transaction's pre-state restarts it: a deficit
  grows only by debits, so the earlier ones bound nothing left); the run's cumulative sum is kept
  beside it. A deficit of a lender behind the share epoch (parked on a full-wipe record) is read
  through the records and checked like any other. `credited` is the program's own receivable, so
  it is **cross-checked** on every transaction whose exits' positions the pre-state lists: it must
  equal the positions' `rent::credited_for` on their pre-state terms at the basis after the
  transaction, each clamped by what is left of the receivable (`shortfall-credited-checked`, else
  `shortfall-credited-unchecked`); a program over-crediting the receivable cannot widen the bound
  with the deficit. `summary.json`'s `wind_down_deficits` records the deficits checked, the
  largest with its bound and its cumulative bound, and the largest and smallest deficit/bound. On
  FUZZ-M7-24H-3's reproducer the 1,028.408-SOL deficit (1,044.467 SOL before AIR-07's drain gate,
  which made the reproducer leave 1.98 ‰ of the claims unborrowed) is 999,998 ppm of its bound;
  scaled to 99 % the run fails, and so does a receivable over-credited by 1 SOL
  (`smoke::deficit_tripwire_on_fuzz_m7_24h_3`).
- **`add_margin`** is never refused (ARCH §3.5).
- **I4's coverage clause (O17)**: every `set_coverage_pause` that lands changes nothing on the whole VM
  (every account short.win, the token programs, the venues and the System program own, lamports
  included; the fee payer aside) but bit 6 of that market's `flags`, which then equals `paused`; every
  refusal is the predicted code. (The harness's I4 hook checks the same clause on every short.win
  account and the tracked pool vaults, `invariants::i4_coverage_authority_changes_only_its_flag`;
  the fuzzer's check is wider: the whole VM.) Opens are gated on the flag (the gate oracle above),
  exits never are.
- **The mint filter**: exits, top-ups and a lender's `withdraw` never return a mint-filter code
  (`MintFreezeAuthority`, `MintDisallowedExtension`, `MintMalformed`, `MintUnsafeFlagged`; ARCH §3.0
  step 3); opens are refused while a freeze authority is planted or the market is flagged;
  `flag_unsafe_mint` lands exactly when the mint fails the filter and the market is not yet flagged.
- **Governance**: a proposed change never applies before its timelock (`TimelockNotElapsed`) or after
  its apply window (`PendingChangeExpired`), never after a cancel (`NoPendingChange`); the Emergency
  vault cannot cancel a `Timelock` change (`EmergencyCannotCancel`); a cancelled or lapsed change leaves
  the parameters and the timelock as they were.
- **The oracle**: `crank_sol_usd` never caches a future-dated update, one with too wide a confidence
  interval, or one from an account the receiver does not own.
- **Client bounds**: an open with `min_proceeds = u64::MAX` or a close with `max_buyback_cost = 1`
  never lands; a deposit of zero tokens never lands.
- **Liveness (I8 / I-P6, I-E11)**: the wind-down clears every gate and disturbance, restores prices,
  closes or exits every position — with the keeper's per-second coverage for `settle_persist_n` + 30 s
  (an armed settlement's persistence), then past every deadline with the keeper's 30-second cadence
  (inside `S3_OBS_GAP_S`, so the S3 chain never restarts) for `s3_wait_s` after the migration pause
  and the graduation grace (each the largest of the config's and the open positions' terms locked at
  open, FUZZ-M10-24H-B), and two more days — waits out the lenders' locks, syncs lenders behind
  the share epoch, covers a lender's negative SOL balance with `cover_deficit` (first topping the
  lender up to the deficit when its 1,000 SOL of funding falls short: `wind-down-deficit-funded`,
  FUZZ-M7-24H-3) and takes every lender out at its allowance; the market must end with no position,
  no receivable, no rent aggregate and nothing borrowed, and the hooks' dust bound holds throughout.
  Since M10's fuzz follow-ups a **second claim pass** follows (every lender claims again, past the
  latest vest end and a crank, in two sequential rounds as the hooks' wind-down: a lender's surplus
  reaches the pot only at its own touch): `lenders_out` takes the lenders out one by one, so a
  lender's claim can find `lender_sol` short until a later lender covers its deficit
  (FUZZ-M7-24H-3's seeding lender). After it the residue must be ECON §9.4's I-P6 — rounding, as
  the hooks' liveness check reads it (FUZZ-M10-24H-A): a lender owed more than `CLAIM_DUST` (1,000
  lamports) must be owed only the accumulators' carry rounding — no realised SOL of its own
  unclaimed, an empty pot, at most `CLAIM_DUST` plus the carries' bound (each share epoch's
  largest `S` over `WAD`, sampled after every transaction), within the hooks' liveness dust cap
  (0.01 SOL; `wind-down-owed-rounding`) — and `lender_sol` must be within that cap. Until M10's
  KI-65 fix the oracle also accepted the realised SOL rebase-orphan shares held
  (`wind-down-orphan-credit`); since the fix a lender's conversion rounds cumulatively through
  each rebase record (ECON §9.5), every share of `S` has a holder — the KI-65 oracle
  (`World::assert_no_orphans`) checks it at every roll and at the second claim pass, as the hooks'
  I1 does after every transaction — and that acceptance is gone. **Owner-signed rebase
  conversions** (KI65-REV-SEC1, owner decision of 2026-10-07) and **the pot-only touch** (KI-66,
  seventh round): the `sync_lender` action is signed by the lender's owner or sent by a third
  party (bit 16 of its argument); nobody's call is refused `RebaseNeedsOwner`, and a third party's
  that lands must leave exactly the lender and `MarketVault` of `pool::touch_pot` on the
  pre-state (`invariants::lender::third_party_sync`: behind a rebase record the lender accrued to
  the record's ends and unconverted, its surplus in the pot; elsewhere the plain touch) and no
  `ShareEpoch` record changed, else `FUZZ-ORACLE KI-66`. A third party's sync that holds the
  lender at a rebase record counts `sync-lender-held` (`sync-lender-held-to-pot` when it moved a
  surplus): the hold's coverage. **KI-66 full (the forward chain, eighth round):** a third party's
  sync passes its records from the chain's `fwd_epoch` once there is one (`World::pot_metas`, as
  the web does); `pot-chain-hold` counts the chains started, `pot-chain-walk` the third-party syncs
  that advanced one, `owner-stops-in-chain` / `owner-settles-chain` the owners' syncs that stopped
  inside a chain (`FWD_OPEN`) or settled it. Bits 17–18 of the argument pick the sync's window —
  0, every record up to 16 (one sync goes all the way, as before); otherwise 1 or 2 records
  (bit 19) — so a third party's walk stops inside a chain and a later one continues it across
  further rolls, and an owner's own sync stops inside its chain; bit 20 aims the sync at a lender
  behind the share epoch or with a chain (the KI-66 full security review's
  KI66F-FUZZ-COVERAGE: with every sync going all the way, a 20-minute run started 11 chains and
  walked none further). This alone does not reach the walks: a chain moves only across a further
  roll, and a second rebase while a lender is chained stays rare — the fix round's four 10-minute
  runs started 0 / 1 / 2 / 2 chains (all, mock, cpmm, pumpswap) and walked none further, and no
  owner stopped inside one — so the chain's dynamics rest on the math crate's `fwd_dynamic_props`,
  `ki66_sec_it`, `ki67_sec_it` and the vectors (an action that rolls a rebase while a lender is
  chained is open). **The never-returns model** (KI-66 review): in one run
  in two (from the seed, `setup::KI66_SALT`) the wind-down takes the lenders held behind a rebase
  record (shares, a lock or a lock term at its epoch) or with a forward chain for absent owners —
  third-party syncs each, 8 records at a time, until the chain reaches the current epoch or a sync
  moves nothing, then one more that must move nothing at all (else the anomaly
  `pot-touch-not-idempotent`), all under the oracle (`wind-down-absent-owner`); then every other
  lender exits and claims in two more rounds — and what the present lenders are still owed must be
  KI-66 full's residual, the latent rounding units: at most what the absent lenders' own touches
  would still move to the pot after their chains (`pool::touch` of the chained lenders on copies:
  the owner's conversion takes the chain's accruals back, so it moves exactly what the chain could
  not), plus **the deficits those touches leave the absent lenders** (`max(0, −pending)`; since
  FUZZ-M10-24H-D-B: KI-53's shortfall is debited pro rata to the shares holding when it lands, so
  a lender behind a rebase record can owe SOL that backs the present lenders' positive `pending`,
  and only its owner's `cover_deficit` pays that in — the cover reaches the pot at the owner's
  next touch) and what the pot holds, less what the absent lenders are owed beyond their own
  realised SOL (their claims draw on the same inflows), plus `CLAIM_DUST` per lender and the
  carries' bound, else the anomaly
  `wind-down-absent-owed-above-residual` (`wind-down-absent-deficit` counts the markets whose
  bound holds a deficit; `summary.json`'s `wind_down_claims.absent_deficit_markets`). That bound
  is what the absent owners' return brings to the pot, so a present lender owed beyond it is SOL
  no one will pay in: on FUZZ-M10-24H-D-B's reproducer (0.32 SOL owed to the present lenders,
  two absent lenders owing 0.349 SOL and owed 0.027 SOL) the bound is 3,111 lamports above the
  owed total — three `CLAIM_DUST`, the carry gap and 109 lamports of rounding — and a leak of
  10,000 lamports fires it
  (`smoke::absent_deficit_backs_the_present_lenders`, `world::ABSENT_OWED_LEAK`). A deficit
  itself is not taken on trust: when the owners return, the deficit tripwire bounds it by the
  lender's pro-rata share of the realised shortfall, and the second claim pass requires every
  lender paid and `lender_sol` within the dust cap. That take-back is cross-checked right after the
  chains, without assuming it: Σ max(0, the owner's touch on the state before any chain − what
  its chain moved) must equal what the owners' touches would still move (anomaly
  `absent-residual-mismatch` above `CLAIM_DUST`, counter `absent-residual-differs` at any
  difference)
  (`wind-down-absent-present-owed` counts the markets owed above dust;
  `wind_down_claims.largest_absent_owed_lamports` the largest total). The absent owners then
  return and the wind-down ends as in every run. Both are rare: the KI-66 review's four
  10-minute runs (the seeds of the KI-66 gate, 1,989 runs) reached the never-returns wind-down
  on 17 markets (35 absent lenders, 70 held syncs, 25 of them moving a surplus) and a held
  third-party sync mid-run 6 times, and no present lender was ever owed above dust, so the
  residual bound itself was not exercised. `tests/smoke.rs`
  `absent_owners_at_the_wind_down` pins three mock seeds that reach the hold (absent owners
  forced on, `world::ABSENT_OWNERS`; the `ki66-pot-skip-surplus` mutant fails it), and
  `ki65_it`, `ki66_sec_it` and the math crate's properties (`pot_only_touch_is_sound`: the
  exact surplus move) are the rule's primary guards, and the mutants of `scripts/mutants.sh` that need a
  rebase are killed by their tests alone. The hooks' liveness check accepts
  any `lender_sol` once nothing is owed, so this bound is the only guard against SOL stranded
  with every lender paid. `summary.json`'s `wind_down_claims` records the largest unpaid balance and
  `lender_sol`. Every lender counts: one parked on a live full-wipe record stays behind the share
  epoch after any `sync_lender` (V2-12), so its balance is read through the record
  (`wind-down-claim-parked`), and a lender that cannot be touched while it holds shares or a
  balance fails the run.
  The keeper never migrates a completed curve in the wind-down: the S3 path must settle a market left
  `PumpMigrating`. Until P7 a live curve whose real token reserves could not buy a position back
  (`Q > rT`: every exit refused `CurveStraddle`) was completed first and settled the same way
  (`fuzz:wind-down-curve-completed`; FUZZ-M7D-1); since pump-final §6 the exit itself completes the
  curve by pump's synthetic migration (`fuzz:exit-completes-curve-synthetic`), and that step is gone.

**Anomalies** (in the summary, not failures): a panic, compute exhaustion or framework error (an
Anchor code of short.win's own, not a venue's error surfacing through its CPI — those are counted as
`venue-cpi-refusal:…` events) inside a short.win instruction the fuzzer built well-formed, a
wind-down withdrawal refused even at the allowance (shares worth at most one raw token are counted as
`wind-down-dust-lender` instead). An accepted I14 replacement was one until M10; it fails now.

## Coverage

The summary counts actions by kind (and skips), short.win instructions landed and refused, refusals
by `instruction:Error`, and events (`Liquidate`, `Expire`, `SolSettle(S1/S2/S3)`, `SettleArmed`,
`CoverageGap`, `CoveragePauseSet`, `PoolSwitched`, `VenueStateChanged`, `MintSafetyTripped`,
`fuzz:graduated`, `fuzz:fee_buckets-…` (M10 program batch 4), the differential oracles' decisions — `…:differential-…` on the mock and, since
M10, `…:v4-differential-…` and `…:curve-differential-…` —, near-cap refusals by cap, venue CPI
refusals; since M10 the I14 refusals by instruction and code (`i14-omit-refused:…`,
`i14-replace-refused:…`), the lender oracles' reach (`claim-bundle-landed`, `jit-capture-checked`,
`jit-capture-checked:settle`, `jit-settle-clock:<settles|none>`, `jit-unchecked:<why>`,
`jit-old-shape:<why>`, `withdraw-then-settle-landed`) and
`donate-trader-base-ata`; since M10's fuzz follow-ups `…:cpmm-differential-…`, `…:pswap-differential-…`,
the shortfall ledger's `shortfall-exits-measured` / `shortfall-exits-short` / `shortfall-unmeasured`
and `shortfall-credited-checked` / `-unchecked`,
`stale-receivable-in-window`,
`wind-down-deficit-covered` / `-funded`, `wind-down-second-claim-paid` / `-nothing`,
`wind-down-claim-parked`; since P7 the crowd's curve trades by instruction (`fuzz:crowd-buy_v3-landed`,
…), its completions (`fuzz:crowd-completes-v2-exact-…` / `-v3-overshoot-…`), the exits that complete
a curve (`fuzz:exit-completes-curve-synthetic` / `-exact`), `donate-curve-base-ata` (`…-after-completion`)
and the `migrating_fv` oracle's `fuzz:migrating-fv-…` and `fuzz:migrate-pool-checked:…`), and prints **coverage proxies**: short.win instructions that landed (of 30), events emitted, and
short.win error codes returned (of the codes `errors.rs` declares), with the codes never returned —
a branch proxy (`summary.json`'s `coverage`). sBPF PC coverage is not measured (LiteSVM 0.16's
`register-tracing` would add it without a second runtime; it changes the shared feature set).

Not fuzzed: `create_market` and `initialize_config` (setup only), AMM v4 third-party liquidity
(the harness has CPMM LP builders only: `tests/litesvm/src/lp.rs`), `revalidate_venue` with a wrong
slot, extras sets, venue-state layout changes and over-delivery on the real venues (the dumped ELFs
cannot over-deliver; a stub ELF was not built, M10 G-E7 N/A). Not checked by an oracle (the hooks
still run): the waterfall amounts of curve exits beyond the hooks (the curve's differential oracle
predicts the decision and the bound, not every lamport), the waterfall amounts of PumpSwap exits
beyond the hooks (its oracle is decision level: no support module recomputes PumpSwap's creator
vault, cashback and fee recipients), sandwich and bundle exits (only the keeper's own exits go through the
differential oracles), the JIT capture around a settlement that rolls the share epoch, and any
economic outcome of the ECON §12 bundles (attacker P&L, lenders whole, deferral seconds), which
are `tests/litesvm`'s attack suites.

## Findings

`--include-known` turns off the avoidance of actions that only re-trigger an open finding (none is
open today); replays use the crash file's own options. A corpus file marked `"status": "fixed"` is a
regression: `corpus_replays` requires it to pass. A finding that was an anomaly rather than a failure
names its class in `"anomaly"` (with an empty `"signature"`): fixed, its replay must also not report
that class.

| id | reproducer | summary | status |
|---|---|---|---|
| FUZZ-M7-1 | `corpus/fuzz-m7-1-curve-trader-wsol.json` (2 actions) | a System transfer to a curve market's `trader_wsol` address survived a curve leg: the curve never names that address (ARCH §2.4 rule 2 normalises it on the WSOL venues only; `TraderAccounts::post_conditions` checks it there) while I7's hook required it closed after every leg; `sweep_trader` recovers the lamports | **resolved in the M7 integration, no program change**: I7's `trader_wsol` clause restated for the curve as a delta (ARCH §2.4 rule 5, §8.3 — the leg leaves the address's lamports unchanged, System-owned, no data; after a WSOL leg it holds nothing), the hook and its mutant test (`invariants_m2_hooks::i7_trader_wsol_on_a_curve_leg_is_a_delta`), a fork regression (`pump_it::exits::a_donation_to_the_trader_wsol_address_survives_curve_legs_until_swept`); the reproducer passes; the fuzzer donates to the address on curve markets again |
| FUZZ-M7D-1 | `corpus/fuzz-m7d-1-curve-straddle-wind-down.json` (3 actions) | the liveness oracle on a fresh curve: two `shock`s buying 98 % of `rT` around an open left `Q > rT`, so `close_short`, `liquidate` and `expire` refused `CurveStraddle`; the wind-down's price restore (one push, at most 50×) did not bring `rT` back above `Q` and its keeper never completed the curve, so the position stayed open (found by the M7D fixes' 10-minute smoke, seed 1790845047; the harness's gap, not the program's) | **resolved in the fuzzer, no program change**: the wind-down completes such a curve (the straddle's first step, ARCH §5.4) and leaves it `PumpMigrating`, as every completed curve, for the S3 path to settle (`SolSettle(S3)`); the liveness oracle is unchanged; the reproducer passes. **Closed by itself on pump's v3 (pump-final P2; P7)**: an exit of `Q > rT` completes the curve by synthetic migration in its own instruction, so P7 removed the wind-down's completion step; the reproducer, kept, now ends with the keeper's liquidation completing the curve (`smoke::fuzz_m7d_1_closes_by_synthetic_migration`) |
| FUZZ-M7-24H-1 | `corpus/fuzz-m7-24h-1-pumpswap-wrapped-open-tier.json` (152 actions) | I19 on a PumpSwap `wrapped_open` (`[ComputeBudget, sale within open_ema_dev_bps, open_short]`, the market ≈ 4 d after its listing: B iff `Λ ≥ θ_B`): the open landed at Tier B and the hook recomputed C — its tier inputs read the WSOL quote vault and the pool's VQR from the VM **after** the open's instruction, so after the open's own sale had drained 1.65 SOL from the vault, while the program (`engine::tier::inputs`, `r_now = state.r`) and ARCH §3.4 step 5 / §4.7 tier on the venue before the trade; with `R_now` binding `R_eff` (the wrapped sale had moved the spot below `P_ema`) the pre-sale Λ 543.52 SOL reached θ_B (541.58 SOL at `price − conf`, M7-SEC-7) and the post-sale 540.22 SOL did not (the first 24-hour run, profile `all`, seed base 20261001; the harness's mistake, not the program's) | **resolved in the harness, no program change**: the PumpSwap view carries the tier's `R_now` and `max(VQR, 0)` (`VenueView::pump_tier`) and I19 — as I15's burned-LP tier bound (`pool_select::pumpswap_open_sized_on_burned_depth`, the same post-instruction VQR read; latent: a swap leaves VQR unchanged) — takes them from the snapshot before the instruction; the other open-term inputs (caps on the pre-trade view, `N_open` and `B̂_ema` on the post-sale venue, as the program) and the other hooks were audited for the same mistake on every venue; a fork regression (`tier_transition_it::pump::pumpswap_open_tiers_before_its_own_sale`: `R_now` binding, the oracle at exactly `p*_B`, fails on the unfixed hook); the reproducer passes |
| FUZZ-M7-24H-2 | `corpus/fuzz-m7-24h-2-mixed-mint-to-duplicate.json` (2 actions), `corpus/fuzz-m7-24h-2-mock-sync-native-duplicate.json` (4 actions) | LiteSVM refused a harness helper's transaction as `AlreadyProcessed` (found by the 24 h run at eb82a2a): the helpers that send around `Env::send` — the litesvm-token builders (`tokens::mint_to`, `SyncNative` in `mock::wrap`), the airdrops — did not expire the blockhash, and their transactions are functions of their arguments (a `SyncNative` carries no amount), so two `donate`s topping up the pool vault by the same amount in a row sent the same `MintTo` twice, and a `wrap` repeated after `wrapped_open` restored the VM from a clone taken just after one sent the same `SyncNative` under the same blockhash (the harness's gap, not the program's: no program ran) | **resolved in the harness, no program change**: every such helper sends through `env::send_direct`, which expires the blockhash afterwards as `Env::send` does (`tokens.rs`, `mock::wrap`, `fork::{wrap, fund_wsol, fund_token}`, `Env::fund`); `Env::send` panics on `AlreadyProcessed`, so a duplicate can never pass as a refusal an oracle counts (an I14 omission, a gated open); no invariant changed; the reproducers pass; a harness test (`it` `harness::identical_helper_calls_in_a_row_are_not_duplicates`) |
| FUZZ-M7-24H-3 | `corpus/fuzz-m7-24h-3-amm-v4-wind-down-deficit-unfunded.json` (12 actions; an anomaly) | `wind-down-withdraw-refused` (`NegativePending`, 6082) in 1 of 84,049 wind-downs of the first 24 h run (run 1) at eb82a2a (amm-v4 seed 20333958; `one --profile amm-v4 --seed 20333958 --actions 400` replays it — `run` draws the length `400/4 + splitmix64(seed ^ 0x1e57) % 301` = 280 per seed, and since M10's fuzz follow-ups `one` derives it too; `--length 280` as well): a near-cap short accrued ≈ 3,640 SOL of rent receivable to the seeding lender, a second lender's deposit then took ≈ 59 % of the shares, and the position exited in the wind-down paying less than it was credited — the true-up debits the shortfall pro-rata to the current shares (ECON §9.4, disclosed), so the late depositor's `pending` was ≈ −1,044 SOL (−1,018 SOL in the full run). The wind-down's exact `cover_deficit` failed in its System transfer (insufficient funds: every lender is funded with 1,000 SOL), so the withdraw refused (the harness's gap, not the program's: the entitlement identity Σ pending = `lender_sol` held, within 261 lamports of dust) | **resolved in the harness, no program change**: `cover_negative` tops the lender up to the deficit plus 1 SOL before the cover (`wind-down-deficit-funded`); the cover lands and the withdraw lands in the same state; `corpus_replays` checks an anomaly-class regression; the reproducer passes. **Revised for AIR-07's drain gate (`00d3772`; `621f8e1`, 2026-10-13):** `lenders_out` took the unborrowed tokens to 0, so the late deposit was refused `PoolDrained` (6108) and the replay passed without a deficit. Action 9 is now `withdraw 0 943018557 451108468`: 668 ‰ of the seeding lender's shares, the largest withdrawal that lands, leaving 1.98 ‰ of the claims unborrowed. The near-cap short then accrues 3,614.36 SOL of receivable at 99.8 % utilisation; the late depositor holds 58.6 % of the shares and owes 1,028.408 SOL, above its 1,000 SOL funding, so the replay needs the top-up again |
| FUZZ-M10-1 | `corpus/fuzz-m10-1-amm-v4-jit-lender-rent.json` (27 actions) | the new JIT capture oracle (R3B-2, M10) read +2,568,240 lamports for a JIT lender around a landed expiry on AMM v4: the lender's `LenderPosition` held no shares, the bundle's withdrawal emptied it and the program closed it, refunding its rent to the wallet, while the control (the expiry without the deposit) left the account open — the lender's own account rent, not SOL of the exit (found by WP-8d's 10-minute all-profile run; the harness's mistake, not the program's) | **resolved in the fuzzer, no program change**: the oracle's lender SOL counts the `LenderPosition`'s lamports (wallet + account + touched balance); the bound is unchanged; the reproducer passes |
| FUZZ-M10-24H-A | `corpus/fuzz-m10-24h-a-cpmm-carry-owed.json` (96 actions), `corpus/fuzz-m10-24h-a-curve-orphan-credit.json` (54 actions) | the second claim pass's liveness oracle (M10) in 6 of 52,272 runs of the final-tree 24 h run at 4f4849e (`--seed 20261004`, signature 561410b6; amm-v4 20285252, curve 20277239, cpmm 20265774 and 20285226, mixed 20295515 — crash files for these 5 only, as `run` keeps `CRASH_FILES_PER_SIGNATURE` = 5 per signature; the sixth is uncaptured: its `lender_sol` was 217,508,010 lamports, the largest of run 3's summary, where any `lender_sol` above the cap failed, and of run 4's, which accepted it within the orphans' realised SOL; whether a lender stayed or every lender left is not yet classified), in the five triaged three shapes: (1) the last lender, all its shares withdrawn, owed 1,033 / 2,646 lamports with `real = claimed`, `lender_sol` 1,913 / 3,335 — the entitlement (`acc`) and realised (`acc_r`) accumulators carry different remainders, up to `S/WAD` lamports (thousands once `S` > 10²¹), and once the last shares leave the realised side stays in `lender_sol` allocated to no lender; (2) a lender owed that carry claimed before a later lender's touch moved its surplus (3,298 lamports) to the pot; (3) `lender_sol` of 11.7–100 M lamports with nothing owed, credited to one or two rebase-orphan shares (a rebase floors each lender's shares and `S` separately, ECON §9.5) — SOL claimable by nobody: on AMM v4, CPMM 20285226 and the mixed profile one orphan of 1,621–4,294 shares took its `1/S` of a vest the crank released (or of `acc_r` built while `S` was smaller still) while a lender kept the dust shares it could not withdraw; on the curve every lender had left during a vest, so the two orphans were all of `S` and the vest the lenders forfeited (R3B-2 releases it to the last lender only when its burn takes `S` to 0) went to them. The vest was released in full in every case (`vest_bal` 0 after the pass's crank); the oracle, written before batch 2's vesting rent top-ups (M10-8C-1, which make a running vest at the lenders' exit more likely), was stricter than ECON §9.4's I-P6 and the hooks' check (the harness's mistake, not the program's) | **resolved in the fuzzer, no program change**: the pass claims in two rounds, accepts an owed balance above `CLAIM_DUST` only as carry rounding (none of the lender's realised SOL unclaimed, an empty pot, ≤ `CLAIM_DUST` + the carries' `ΣS_peak/WAD` + the orphans' realised SOL, within the dust cap: a flat dust cap let a program mutant that leaks 2,000 lamports of each realised allocation pass in 22 of 22 wind-downs) and a `lender_sol` above the dust cap only up to the orphan shares' realised SOL; a regression test (`smoke::wind_down_residue_on_fuzz_m10_24h_a`) asserts each reproducer passes through its own branch; the reproducers pass; ECON §9.5's bound on orphan credit (`(lenders)/S_new` of later credits) did not hold once every lender leaves during a vest — ECON §9.5 now states it, with the lender guidance (wait for the market's vest end), and `docs/audit/known-issues.md` KI-65 discloses it. **Fixed in the program since (M10 KI-65: cumulative conversion, no rebase orphans)**: the orphan-credit acceptance is gone, the curve reproducer now replays to dust with every share held (`World::assert_no_orphans`), and the regression test asserts it with run B's three largest orphan-credit cases. **Revised for AIR-07's vest flush (`e1f873a`; `70ac76e`, 2026-10-13):** in the CPMM list's action 40 the settlement that leaves T = 2 now flushes the 1,206,153,452-lamport vest to the epoch-0 shares; after that the recorded list left its last lender owed 943 lamports, not above `CLAIM_DUST`. Action 45's `trade` now sells (`trade 0 458876573 3672211568`): 2,149 lamports owed, `lender_sol` 2,332, and the run takes `wind-down-owed-rounding` again |
| FUZZ-M10-24H-B | `corpus/fuzz-m10-24h-b-mock-locked-persist.json` (5 actions) | the liveness oracle (found by the final-tree 24 h run at 4f4849e, `--seed 20261004`, mock seed 20301612; `one --profile mock --seed 20301612 --actions 400` replays it, 338 actions; 1 in 52,272 wind-downs): a `config_change` raised `settle_persist_n` to 320, a short opened and locked it (`sol_settle` reads the position's terms, `engine::arming::terms`), a second change lowered the config's to 40; in the wind-down the position stayed armed on the S1/S2 legs of the undisturbed mock (`close_short` `InsufficientVault`, `liquidate` `UseSolSettle`, `expire` `UseLiquidate`, every `sol_settle` a landed keep), but `persist_seconds` sized the per-second coverage from the config (70 s) and no run of keeper seconds reached 320 before a deadline or day warp broke the arm gap (69 of 320 updates at the end); the in-house keeper keeps an armed position on the per-second cadence until it settles (the harness's gap since M7, not the program's) | **resolved in the fuzzer, no program change**: `persist_seconds` covers the largest of the config's and the open positions' locked `settle_persist_n`, `s3_cadence` the largest locked S3 timers (the same staleness, latent); the liveness oracle is unchanged; the reproducer passes |
| FUZZ-KI67-FIX-1 | `corpus/fuzz-ki67-fix-1-cpmm-dust-worth-overflow.json` (6 actions) | the release fuzzer's overflow check panicked in the wind-down (found by the KI-66 full / KI-67 fix round's 10-minute run, `run --profile cpmm --seed 67800200 --jobs 3`, cpmm seed 67800610; the 24 h run's fuzzer at `f48e75d` fails the same reproducer at `world.rs:2297`): the dust test for a refused full withdrawal computed a lender's worth as `shares · T / S` in `u128`, and six `stale_receivable` actions leave shares and `T` whose product exceeds 2^128 (the harness's bug, not the program's) | **resolved in the fuzzer, no program change**: the worth is `shortwin_math::num::mul_div` (a 256-bit product, floor); the reproducer passes (`corpus_replays`) |
| FUZZ-24D-A | `corpus/fuzz-24d-a-pumpswap-expire-stale-prediction.json` (4 actions) | the PumpSwap differential exit oracle in 1 of 31,389 runs of the KI-66 full / KI-67 head's 24 h run D (`55b3e23`, `--seed 71000000`; pumpswap seed 71023945, `world.rs:1145`: `expire` refused `NotExpired`, predicted `NoRoute`; the crash file 25 actions, minimised to 4): the oracle reused, for the expiry, the prediction it had made with a refused liquidation of the same position in the same second (`World::pswap_expire`, keyed `(now, position, transactions landed)`). A `venue_halt` made PumpSwap non-executable, a `keeper_exit`'s liquidation was refused `NoRoute` as predicted and cached the expiry's `NoRoute`; the halt's undo then cleared the flags by a direct VM write (no transaction, so the key did not change), and the next `keeper_exit`'s expiry reused the stale `NoRoute` while the program, on an executable pool again, correctly refused `NotExpired` (ARCH §3.7's order; the harness's bug, not the program's) | **resolved in the fuzzer (`9861c5a`), no program change**: the cached expiry prediction serves only the next keeper exit of the same action and is dropped at every action boundary, before the due undos run; the reproducer replays as a regression (`corpus_replays`, `smoke::pswap_expire_cache_fuzz24d_a`) |
| FUZZ-M10-24H-D-B | `corpus/fuzz-m10-24h-d-b-curve-absent-deficit.json` (63 actions; an anomaly) | `wind-down-absent-owed-above-residual` in 1 of 31,389 runs of the KI-66 full / KI-67 head's 24 h run D (`55b3e23`, `--seed 71000000`; curve seed 71017971, `one --profile curve --seed 71017971 --actions 400`, 378 actions): with an absent lender behind the epoch-0 rebase record the present lenders were owed 73,744,534,723 lamports, the oracle's bound 2,002 (the absent lender's surplus moves 0, dust, the carry gap). The absent lender's `pending` was −73,744,535,218 — the record's own shortfall debit: an S3 settlement realised ≈ 4.8 SOL of a ≈ 930 SOL credited receivable, debited pro rata to the shares holding then, so the lenders that had withdrawn more shares earlier kept a positive `pending` only its cover pays in (KI-53). The hold, the owner's signed conversion (VM copies, before and after the chain) and `pool_model` book it identically; the chain's epoch-1 accrual was 0. The oracle's bound ignored deficits (the harness's gap, not the program's) | **resolved in the fuzzer, no program change**: the bound is what the absent owners' return brings to the pot — the surplus moves plus the deficits their touches leave, plus the pot — less their own needs beyond their realised SOL (`World::absent_returns`; `wind-down-absent-deficit`), plus `CLAIM_DUST` per lender and the carry gap; on the reproducer it is 3,111 lamports above the owed total and a 10,000-lamport leak fires it (`smoke::absent_deficit_backs_the_present_lenders`); the reproducer replays as a regression (`corpus_replays`) |

The M10 fuzz follow-ups add no finding (nor do their review fixes: every `credited` cross-check matched to the lamport). Their checks reproduce on the corpus. FUZZ-M7-24H-3's reproducer covers a 1,028.408-SOL deficit (1,044.467 SOL at 99.9998 % before its AIR-07 revision) at 999,998 ppm of the tripwire's bound, and the seeding lender's positive balance, short in the first pass, is paid by the second claim pass. In the 10-minute cpmm and amm-v4 runs, no CPMM or AMM v4 differential exit, no deficit and no second-pass claim disagreed with the program. PumpSwap's decision-level oracle ran only in the smoke (`smoke_pumpswap`), where it found no disagreement.

## Runs so far (M7, M10, pump-final P7)

| run | build | wall | jobs | runs | actions | transactions | hook checkpoints | failures | anomalies |
|---|---|---|---|---|---|---|---|---|---|
| fuzz track, `--seed 20261001` | the M7 program | 3,903 s | 5 | 5,598 | 1,055,276 | 13,591,797 | 20,488,740 | 0 | 4 (the wind-down's `NegativePending`, fixed in the wind-down) |
| fuzz track, confirmation | the M7 program | 724 s | 5 | 1,190 | 225,106 | 2,879,543 | 4,327,270 | 0 | 0 |
| integration, `--seed 20261010` | the final tree (FUZZ-M7-1 resolved) | 605 s | 5 | 822 | 155,022 | 1,961,976 | 2,944,773 | 0 | 0 |
| M7 security review fixes, `--seed 20261011` | the fixed tree (M7-SEC-1 … -9; I19, `wrapped_open`, `coverage_bracket`) | 617 s | 5 | 806 | 155,055 | 2,036,820 | 3,109,137 | 0 | 0 |
| the owner's decisions of 2026-10-01 (default seed) | `COVERAGE_GAP_S` 40, `oracle_stale_s` 600, the Emergency's clear (`emergency_clear`; `silence` past 40 s) | 606 s | 4 | 1,176 | 167,812 | 2,181,234 | 3,342,239 | 0 | 0 |
| the M7D fixes (keeper only; the program byte-identical), default seed | the decisions' tree | 640 s | 4 | 1,103 | 157,856 | 2,113,224 | 3,276,171 | 1 (FUZZ-M7D-1: the wind-down's missing curve completion) | 0 |
| the M7D fixes, after FUZZ-M7D-1 (an interim wind-down that also migrated) | — | 601 s | 4 | 1,188 | 171,370 | 2,289,531 | 3,486,368 | 0 | 0 |
| the M7D fixes, final (default seed) | the wind-down completes a straddled curve and leaves it to S3 | 603 s | 4 | 1,215 | 172,918 | 2,303,424 | 3,513,992 | 0 | 0 |
| 24 h run 1, `--seed 20261001` | `eb82a2a` (the owner's decisions of 2026-10-01) | 86,405 s | 4 | 84,053 | 16,098,571 | 207,549,074 | 318,356,256 | 4 (FUZZ-M7-24H-1, -2: three harness signatures, fixed in `e857a2a`) | 1 (FUZZ-M7-24H-3, fixed in `22961b0`) |
| FUZZ-M7-24H-2's fix (harness only; the program byte-identical), default seed | helpers through `env::send_direct`; `Env::send` panics on `AlreadyProcessed` | 635 s | 3 | 371 | 53,727 | 688,097 | 1,047,207 | 0 | 0 |
| 24 h run 2 (the confirmation), `--seed 20261002` | `4cb7729` (the program bytes of `e857a2a`, `shortwin.so` `00011844…`) | 86,402 s | 4 | 72,149 | 13,821,419 | 178,365,479 | 273,589,118 | 0 | 0 |
| M10 WP-8d, `--profile curve` (time-based seed, not recorded) | the new oracles (AMM v4 / curve exits, JIT capture, claim bundles, `withdraw_then_settle`), pump `fee_shock` | 602 s | 3 | 415 | 52,109 | 606,351 | 1,048,883 | 0 | 0 |
| M10 WP-8d, `--profile pumpswap` (time-based seed, not recorded) | the same | 605 s | 3 | 416 | 58,169 | 743,898 | 1,266,504 | 0 | 0 |
| M10 WP-8d, `--profile all` (time-based seed, not recorded) | the same | 710 s | 3 | 245 | 34,395 | 435,608 | 720,022 | 1 (FUZZ-M10-1: the JIT oracle's lender rent, a harness mistake) | 0 |
| M10 WP-8d, `--profile all`, after FUZZ-M10-1 (time-based seed, not recorded) | the oracle counts the `LenderPosition`'s lamports | 625 s | 3 | 336 | 49,136 | 632,667 | 1,040,410 | 0 | 0 |
| M10 WP-8d final (the tier-boundary shock, `open_near_cap` from the minimum notional), `--profile curve` (time-based seed, not recorded) | the final tree | 615 s | 3 | 219 | 27,929 | 386,258 | 741,671 | 0 | 0 |
| the same, `--profile pumpswap` | the final tree | 606 s | 3 | 275 | 39,439 | 589,434 | 1,067,886 | 0 | 0 |
| the same, `--profile all` | the final tree | 613 s | 3 | 274 | 40,830 | 537,596 | 955,114 | 0 | 0 |
| M10 WP-8d review fixes (the curve oracle fails a refused buyback), `--profile curve`, `--seed 20261021` | the reviewed tree | 602 s | 3 | 374 | 47,961 | 613,345 | 1,120,779 | 0 | 0 |
| the same, `--profile pumpswap`, `--seed 20261022` | the reviewed tree | 602 s | 3 | 334 | 47,511 | 659,789 | 1,186,910 | 0 | 0 |
| the same, `--profile all`, `--seed 20261023` | the reviewed tree | 602 s | 3 | 348 | 49,722 | 637,343 | 1,126,933 | 0 | 0 |
| M10 fuzz follow-ups, `--profile cpmm`, `--seed 20261031` | the deficit tripwire, the second claim pass, `stale_receivable`, CPMM / PumpSwap exit oracles | 628 s | 3 | 245 | 34,477 | 376,737 | 722,843 | 0 | 0 |
| the same, `--profile amm-v4`, `--seed 20261032` | the same | 604 s | 3 | 222 | 31,770 | 360,668 | 689,103 | 0 | 0 |
| M10 fuzz follow-ups final, `--profile cpmm`, `--seed 20261041` | exit pre-states cloned only when unaccrued, PumpSwap's M7-SEC-5 shape denser | 611 s | 3 | 334 | 47,582 | 523,117 | 1,001,063 | 0 | 0 |
| the same, `--profile amm-v4`, `--seed 20261042` | the same | 606 s | 3 | 343 | 49,618 | 554,578 | 1,048,735 | 0 | 0 |
| M10 fuzz follow-ups review fixes, `--profile cpmm`, `--seed 20261041` | parked lenders in the claim liveness and the tripwire, bound restarts, the `credited` cross-check | 614 s | 3 | 345 | 49,327 | 546,459 | 1,043,562 | 0 | 0 |
| the same, `--profile amm-v4`, `--seed 20261042` | the same | 608 s | 3 | 319 | 46,026 | 516,524 | 981,250 | 0 | 0 |
| the same, `--profile pumpswap`, `--seed 20261053` (5 minutes) | the same | 302 s | 3 | 72 | 10,483 | 165,116 | 311,162 | 0 | 0 |
| 24 h run 3 (the M10 final tree), `--seed 20261004` | `4f4849e` (program batch 4, release `bbef1075…`) | 86,409 s | 4 | 52,272 | 10,162,831 | 132,425,511 | 255,722,249 | 7 (FUZZ-M10-24H-A ×6, FUZZ-M10-24H-B ×1: the fuzzer's liveness oracles, fixed in `997c730` and `945d45a`) | 0 |
| FUZZ-M10-24H-B's fix (harness only; the program byte-identical, release `bbef1075…`), `--profile mock,mixed`, `--seed 20261051` | the wind-down's persistence and S3 waits read the positions' locked terms | 1,226 s | 3 | 382 | 76,564 | 1,050,827 | 2,091,557 | 1 (the final-tree run's open signature (A), `world.rs:4148` second-claim-pass `lender_sol` above the cap, mock seed 20261070; it fails the same way on the unfixed fuzzer; FUZZ-M10-24H-B's signature did not recur) | 0 |
| 24 h run 4 (the final tree, the fixed fuzzer), `--seed 20261005` | `945d45a` (the program bytes of `4f4849e`, release `bbef1075…`); run 3's seed stream shifted by one, so essentially run 3's cases (below) | 86,405 s | 4 | 52,274 | 10,163,360 | 132,445,714 | 255,725,412 | 0 | 0 |
| 24 h run C (the KI-66 full / KI-67 program bytes), `--seed 67300000` (`--actions` the default 300) | `f48e75d` (`m10/ki67`; release `4d19c582…`, the final program bytes; the fuzzer before the KI-67 review round's harness changes) | 86,418 s | 4 | 43,833 | 6,405,217 | 84,950,438 | 165,759,524 | 0 | 0 |
| 24 h run D (the final head then), `--seed 71000000`, `--actions 400` | `55b3e23` (release `4d19c582…`; the fuzzer of the KI-67 review round) | 86,445 s | 4 | 31,389 | 6,083,510 | 78,948,599 | 152,298,767 | 1 (FUZZ-24D-A: the PumpSwap exit oracle's stale expiry prediction, fixed in `9861c5a`) | 1 (FUZZ-M10-24H-D-B: the never-returns bound ignored the absent lenders' KI-53 deficits, fixed in `9861c5a`) |
| FUZZ-M10-24H-D-B's fix (harness only; the program of run D, `55b3e23`, release `4d19c582…`), `--profile curve`, `--seed 72500000` | the never-returns bound: surplus moves + the absent lenders' deficits + the pot − their own needs | 1,207 s | 3 | 355 | 61,473 | 825,698 | 1,535,486 | 0 | 0 |
| the same, `--profile all`, `--seed 72600000` | the same | 1,215 s | 3 | 347 | 65,859 | 850,200 | 1,644,098 | 0 | 0 |
| 24 h run E (the final program bytes, the fixed fuzzer), `--seed 75000000`, `--actions 400` | `9861c5a` (the program bytes of `55b3e23`, release `4d19c582…`; run D's two findings fixed) | 86,415 s | 4 | 44,988 | 8,773,451 | 114,792,804 | 221,932,740 | 0 | 0 |
| P7 smoke, `--profile curve`, `--seed 77100000`, `--actions 400` (10 minutes) | `3e3b43b` (pump-final P2–P6 program, release `b3130ae2…`, test-venue `1f10334c…`; pump's 2026-10-13 ELFs; the v3 crowd and the `migrating_fv` oracle; run on its build before the P7 review's fixes, which change no generated action) | 657 s | 4 | 314 | 55,849 | 715,787 | 1,288,544 | 0 | 0 |
| the same, `--profile pumpswap`, `--seed 77200000` | the same | 668 s | 4 | 104 | 21,842 | 366,276 | 683,304 | 0 | 0 |
| the same, `--profile all`, `--seed 77300000` | the same | 615 s | 4 | 213 | 41,479 | 535,957 | 1,026,097 | 0 | 0 |
| P7, `--profile curve`, `--seed 77400000`, `--actions 400` (1 h) | the same | 3,605 s | 4 | 1,804 | 312,131 | 4,048,870 | 7,359,413 | 0 | 0 |
| P7, `--profile pumpswap`, `--seed 77500000`, `--actions 400` (1 h) | the same | 3,678 s | 4 | 1,121 | 221,011 | 3,241,966 | 6,015,979 | 0 | 0 |
| P7 review fixes, `--profile curve`, `--seed 77600000`, `--actions 400` (10 minutes) | `3e3b43b` as committed (the legacy `complete_curve` parsed with `a` odd; `crowd_v3_trades_land`) | 652 s | 4 | 327 | 58,948 | 779,003 | 1,414,529 | 0 | 0 |

In the M10 fuzz follow-ups' final runs: on `cpmm`, 180 liquidations and 337 expiries landed through
the CPMM differential oracle (every lamport; 25 / 31 more through AMM v4's on adopted candidates),
1,803 exits fed the shortfall ledger (18 realised a shortfall; `stale_receivable` hit its window 14
times), 4 deficits were checked against their bounds (the largest 2.104 SOL, the tightest at 99.98 %
of its bound), the second claim pass paid 68 claims and left nothing owed (`lender_sol` at most
5,731,356 lamports, within the 0.01-SOL cap); on `amm-v4`, 170 liquidations and 346 expiries through
the AMM v4 oracle and 15 / 32 through CPMM's, 1,822 exits measured (36 short), 8 deficits checked (the
largest 6.700 SOL, the tightest at 99.95 %), 69 second-pass claims paid, `lender_sol` at most 12,071
lamports.

After the review's fixes (same seeds): on `cpmm`, 195 / 344 CPMM differential exits, 1,862
transactions' `credited` equal to their positions' `credited_for` (none unchecked), 19 short
exits, 4 deficits within their bounds (the largest 2.104 SOL at 99.98 %, the smallest at 8.5 %),
3 lenders parked on a full-wipe record read through it in the second claim pass (the 3 the
first version skipped as `wind-down-claim-behind-epoch`), nothing owed after it; on `amm-v4`,
159 / 316 through AMM v4's oracle and 15 / 32 through CPMM's, 1,702 `credited` cross-checks, 30
short exits, 7 deficits (the largest 6.700 SOL at 37.5 % of its bound, the tightest at 99.92 %), 1
parked lender; on `pumpswap`, 36 / 100 decision-level exits, 467 cross-checks, `stale_receivable` in
its window 3 times, 4 short exits and 1 deficit (0.010 SOL at 8.9 % of its bound). In the `amm-v4` and
`pumpswap` reruns (which count it; the `cpmm` rerun predates the counter) no short exit was charged
at every share or with several exits in its transaction (`shortfall-short-every-share` /
`-several-exits` 0), and the restarts left every largest bound at its cumulative value: where a
deficit sits far below its bound, rent credited after the exit (the wind-down's warps accrue the
other positions' rent) repaid part of it — the bound ignores later credits, so it is loose there by
construction, not by a conservative charge.

≈ 4.9 h of wall clock (≈ 18.5 job-hours) in all, plus the M7 security review's runs (mutant builds)
and the mutation runs (below). In the fixes' run, 4,327 `wrapped_open` and 1,127 `coverage_bracket`
actions: 832 wrapped opens landed with I19 recomputing their terms, 585 brackets refused at the clear delay.
In the owner's-decisions run, 3,104 `emergency_clear` actions: 541 clears of a set flag and 780
no-op clears landed under I4's coverage clause (whole VM), 875 opens in the clear delay after an
Emergency clear (alone or bundled) refused `CoveragePaused`; 601 `coverage_bracket`s refused;
5,840 `CoverageGap` events.

**The exit criterion "fuzz clean ≥ 24 h" is met** (ARCHITECTURE §9, OWNER-M7-16): for M7 by 24 h
run 2 at `4cb7729`, and for M10 by run 4 on the then final tree at `945d45a` (the program bytes
of `4f4849e`, release `bbef1075…`), both with 0 failures and 0 anomalies (the table above; each a
local `run` of all six profiles on 4 jobs; `fuzz-nightly` is opt-in, below), and on the final
program bytes (release `4d19c582…`) by run E (below). Runs 1 and 3 failed
only the fuzzer's own checks (FUZZ-M7-24H-1 … -3, FUZZ-M10-24H-A, -B; no program invariant was
violated), and each was fixed before the next run. Runs 2 and 4 are confirmations, not independent
coverage: their base seeds are runs 1's and 3's plus one, so on 4 jobs each stream is the earlier
run's shifted by one (a run's profile and length derive from its seed). Run 4 re-executed
essentially all of run 3's 52,272 cases, the six failing seeds included, with the fixed fuzzer (the
two summaries' largest `lender_sol`, deficit, bound and deficit/bound figures are identical), and
run 2 re-ran run 1's seeds. In run 4 every one of the 52,274 runs ended in
the liveness wind-down: the deficit tripwire checked 834 deficits in 812 runs (the largest
1,468.99 SOL, within its bound); the second claim pass paid 13,618 claims in its first round and
1 more in its second round, and left at most 445 lamports owed; `lender_sol` was at most
217,508,010 lamports (≈ 0.218 SOL), above the 0.01-SOL dust cap in 5 wind-downs, each within the
rebase-orphan shares' realised SOL (`wind-down-orphan-credit`; ECON §9.5, KI-65 in
`docs/audit/known-issues.md`). The largest is run 3's uncaptured sixth FUZZ-M10-24H-A failure
(the findings table), so the triaged reproducers' 0.054 and 0.012–0.100 SOL are examples, not the
bound. KI-65 is fixed in the program since (no rebase orphans): run B's three largest
orphan-credit wind-downs (19,131,900,419 lamports on the mock, `one --profile mock --seed 31013687
--length 398`; 890,963,742 on CPMM, 31005995 / 390; 552,350,825 on PumpSwap, 31016959 / 386) and
the curve corpus file replay to dust (`tests/smoke.rs`, `wind_down_residue_on_fuzz_m10_24h_a`),
and the oracle no longer accepts orphan credit; these runs predate the fix and must be repeated on
the final tree (done: runs C, D and E, below).

**On the final program bytes** (release `shortwin.so` `4d19c582…`, the KI-66 full / KI-67 batch's; the
later commits change the fuzzer, the web, the indexer and docs, not the program), three more ≥ 24 h
`run`s of all six profiles on 4 jobs with `--minimize`, named by letter after run 4 as run D's finding
ids are (the table above; each its `summary.json` and log). Their base seeds are far apart, so unlike
runs 2 and 4 each adds coverage:
- **Run C** at `f48e75d` (`m10/ki67`, the final program bytes; its fuzzer predates the KI-67 review
  round's harness changes: the windowed syncs, the residual cross-check, the withdrawal replay and
  FUZZ-KI67-FIX-1's overflow fix), `--seed 67300000`, `--actions` 300 (2026-10-08T11:50:02Z →
  2026-10-09T11:50:20Z): **0 failures, 0 anomalies**; 43,833 wind-downs; the deficit tripwire checked
  517 deficits in 506 runs (the largest 6,315,964,217,539 lamports, the largest share of its bound
  1,000,000 ppm); after the second claim pass at most 1 lamport owed and `lender_sol` at most 2,771
  lamports; the never-returns wind-down on 234 markets (465 absent lenders, 995 held third-party
  syncs), no present lender owed above dust; 516 forward chains started, 492 settled by their owners,
  none walked further by a third party; `claim_sol` closed 606 positions.
- **Run D** at `55b3e23` (the final head then), `--seed 71000000`, `--actions 400`
  (2026-10-08T17:29:26Z → 2026-10-09T17:30Z): 31,389 runs, 31,388 wind-downs; the program's
  invariant hooks held. Two fuzz-oracle findings, both the harness's and fixed in `9861c5a` (the
  findings table): **FUZZ-24D-A** (1 failure: a stale PumpSwap expiry prediction after a direct VM
  write) and **FUZZ-M10-24H-D-B** (1 anomaly: the never-returns oracle ignored the absent lenders'
  KI-53 deficits — present lenders owed 73,744,534,723 lamports that only an absent owner's
  `cover_deficit` pays in). Otherwise: 490 deficits in 481 runs (the largest 327,191,934,426
  lamports; the largest share of its bound 1,000,024 ppm, within the tripwire's 0.001-SOL
  allowance); at most 1 lamport owed and `lender_sol` at most 1,418 lamports after the second claim
  pass; the never-returns wind-down on 181 markets (347 lenders, 803 held syncs); 438 chains
  started, 385 settled by their owners; `claim_sol` closed 697 positions.
- **Run E** at `9861c5a` (the final program bytes and the fixed fuzzer), `--seed 75000000`,
  `--actions 400` (2026-10-09T19:45:29Z → 2026-10-10T19:45:44Z): **0 failures, 0 anomalies**;
  44,988 runs, 8,773,451 actions, 114,792,804 transactions (14,965,581 failed), 221,932,740 hook
  checkpoints, 44,988 wind-downs, 86,415 s; 28 of 30 instructions landed, 69 of 108 error codes
  returned (runs C and D: 28 / 30 and 68 / 108). The deficit tripwire checked 634 deficits in 621
  runs (the largest 896,613,324,309 lamports against its bound 1,139,151,177,586; the largest share
  of its bound 1,000,000 ppm, as run C, the smallest 5,365 ppm); after the second claim pass at most
  112 lamports owed and `lender_sol` at most 2,335 lamports; the never-returns wind-down on 310
  markets (621 absent lenders, 1,404 held third-party syncs), no present lender owed above dust and no
  absent lender's deficit in a bound (`absent_deficit_markets` 0); 756 forward chains started, 690
  settled by their owners, none walked further by a third party; `claim_sol` closed 981 positions
  (743 claim bundles refused after the close), 25,307 deposits re-created one.

**M10's "fuzz clean ≥ 24 h" on the final program bytes is met** by run E (and by run C with the
earlier fuzzer); run D's program invariants held, and its two findings were the fuzzer's.

**pump-final P7 (§12; the curve adapter on pump's v3, release `b3130ae2…`).** The fuzzer at `3e3b43b`
(the crowd's v2 and v3 trades and synthetic completions, the donations to `curve_base_ata`, the
completion check of the curve's differential oracle and the `migrating_fv` oracle; these runs used
its build before the P7 review's fixes, which change only how the legacy name `complete_curve`
parses and add smoke tests, so every generated action stream is the same) ran a 10-minute
smoke of the curve, PumpSwap and all six profiles, then an hour each of the curve and PumpSwap
profiles on 4 jobs, `--minimize` (the table above): **0 failures, 0 anomalies**, no oracle failure.
In the hour on the curve (1,804 runs, every one ending in the wind-down): the crowd landed 10,864
`buy_v3`, 726 `buy_exact_quote_in_v3` and 11,592 `sell_v3` beside 11,516 `buy_v2` and 10,100
`sell_v2`, and completed 1,536 curves (773 by a `buy_v2` of `rT`, 763 by a `buy_v3` overshoot);
212 short.win exits completed a curve in their own instruction (192 by synthetic migration), 65 of
them keeper exits whose completion — `PumpMigrating` since now, `pcB`, `pcQ` — the curve oracle
checked (24 liquidations, 41 expiries); 125 token donations reached `curve_base_ata` (77 after a
completion); the `migrating_fv` oracle checked 210 S3 settlements of a `PumpMigrating` market (111
after a pool part, 99 without) and 1,436 `migrate_v2` pools (775 after a pool part, 627 standard,
34 with a donation seeded but not priced); 1,436 markets graduated to PumpSwap. `CurveStraddle`
was never returned (C8: past the would-be pool only). The PumpSwap hour (1,121 runs) checked 942
liquidations and 1,889 expiries through its differential oracle and 86 S3 settlements. Earlier
runs of the P7 fuzzer: a 4-minute curve trial (137 runs, 19 S3 and 115 `migrate_v2` checks) and
the smoke (`cargo test --release -p shortwin-fuzz`: 12 + 19 passed, among them
`migrating_fv_after_a_synthetic_completion_and_a_donation` — a 1-lamport FV skew fails it —,
`migrate_v2_creates_the_priced_pool_after_a_donation` and
`fuzz_m7d_1_closes_by_synthetic_migration`). After the P7 review the smoke is 12 + 20:
`crowd_v3_trades_land` requires a scripted run to land `buy_v3`, `buy_exact_quote_in_v3` and
`sell_v3` with none refused (it fails with `min_tokens_out` 0, or with the crowd on v2 only), and
`smoke_curve` requires each to land in its seeds. The review's 10-minute curve run on `3e3b43b`
as committed (the table): 327 runs, 779,003 transactions, **0 failures, 0 anomalies**; the crowd
landed 2,247 `buy_v3`, 136 `buy_exact_quote_in_v3` (none refused) and 1,710 `sell_v3`, and 141
`buy_v3` overshoots completed a curve; 33 short.win exits completed a curve by synthetic migration
(16 keeper exits checked by the curve oracle); 26 token donations reached `curve_base_ata` (13
after a completion); the `migrating_fv` oracle checked 50 S3 settlements (27 after a pool part)
and 271 `migrate_v2` pools; `CurveStraddle` was never returned.

**Mutation testing (M7-SEC-6).** `scripts/mutants.sh` builds this checkout's tests and fuzzer once
(the oracle) and each of the review's seven one-line mutants of the program (D4 without its EMA
leg, the fee-scaled size cap doubled, the wallet cap removed, Tier C as B, the coverage clear
immediate, the curve caps on the live reserves, the pump caps at the live fee), and requires its
named tests and a short fuzz of its profile to fail against each: a surviving mutant is a missing
oracle. Before I19, the first four survived 15 minutes of `mock,mixed` fuzzing (562 runs) and
M5-SEC-3's revert 12 minutes of `curve` (1,118 runs); with it, every one of the seven is killed by
its tests and by a 3-minute fuzz of its profile (2026-10-01, on the final tree: 10 / 10 / 10 / 133
failures on `mock` for the margin, fee-cap, wallet and tier mutants, 27 for the clear delay, 16 on
`curve`, 1 on `pumpswap`; the wallet mutant also fails `caps_boundary_it`'s M7-SEC-9 regression). **M10 (WP-8d):** on the new action stream the PumpSwap mutant (the caps at the live fee, M7-SEC-5) first survived its 3-minute fuzz — its kill had been a single failure, and `open_near_cap` was skipped on every fork pool —; with PumpSwap's tier-boundary fee shock and `open_near_cap` from the minimum notional all seven are killed again (2026-10-02, `MUTANTS_JOBS=3`, the script's default `MUTANTS_SEED` 20261001: 10 / 7 / 8 / 117 failures on `mock`, 32 for the clear delay, 16 on `curve`, 2 on `pumpswap`); again after the review's fixes: 10 / 7 / 8 / 113, 30, 13, 2). **M10 fuzz follow-ups:** the new action stream (`stale_receivable`) and the per-exit oracles on PumpSwap left the PumpSwap mutant alive again: its 3-minute fuzz reached 42 runs on a loaded machine, against WP-8d's 114, and WP-8d's kill had been 2 failures, both at seeds about 24 runs deep per job. PumpSwap's `fee_shock` and `open_near_cap` weights are now 3 and 6, three times as dense, and all seven are killed again (2026-10-02, `MUTANTS_JOBS=3`, seed 20261001: 10 / 5 / 9 / 110 failures on `mock`, 27 for the clear delay, 15 on `curve`, 5 on `pumpswap`). **KI-65 (M10):** two more mutants, checked by their tests alone (profile `-`: their fuzz oracles need a share rebase, which a 3-minute run reaches too rarely) — `claim_sol` converting through a rebase record without storing `conv_rem` (`ki65-skip-store-records`) and `sync_lender`'s owner check made `true` (`ki65-rebase-without-owner`, KI65-REV-SEC1; since KI-66 its path selection takes every caller for the owner); `ki65_it` kills both. **KI-66 (M10):** a third, also tests only — the pot-only touch skipping the surplus move to the pot (`ki66-pot-skip-surplus`, in `shortwin_math::pool`); `ki65_it` kills it. **KI-66 full and KI-67 (M10, eighth round):** eight more, tests only — the forward chain accruing nothing after the record (`ki66full-walk-accrues-nothing`), the owner adding its whole accrual inside the chain instead of the lag's part (`ki66full-open-full-accrual`, a double count), `FWD_HOLD` accruing the held epoch again (`ki66full-hold-accrues-again`), the chain flooring per touch instead of cumulatively (`ki66full-walk-per-touch-floor`), a chained lender re-entering the hold (`ki66full-rehold`), the V2-12 jump keeping the chain (`ki66full-jump-keeps-chain`), `claim_sol` closing with `pending` owed (`ki67-close-with-pending`) and `closable` without its epoch check (`ki67-close-parked`, a parked lender closed); `ki65_it`, `ki66_sec_it` and `lender_it` kill them.

## CI

`.github/workflows/ci.yml`'s `fuzz-nightly` (the `schedule` trigger on main, or `workflow_dispatch`
with `fuzz_duration`): one 5 h shard per profile on every core of its runner, `--minimize`, crash
files uploaded, the job failing on any crash; the `mock` shard first runs `cargo test --release -p
shortwin-fuzz` (the fixed-seed smoke of every profile, ≈ 15 s after the build, the corpus replays and
the harness's self-tests). On every push and pull request the `fuzz-smoke` job runs the same
`cargo test --release -p shortwin-fuzz` (the smoke, the corpus replays — FUZZ-M7-1's reproducer must
pass — and the self-tests); a failed smoke uploads its crash files.
