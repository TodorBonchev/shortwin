# Development

How to set up the toolchain, build and test short.win. The pins come from
[`design/ARCHITECTURE.md`](design/ARCHITECTURE.md) §1.1; the layout from §1.2.

## Pins

| Tool | Version | Pinned in |
|---|---|---|
| Host Rust | 1.98.1 (+ rustfmt, clippy, `wasm32-unknown-unknown`) | `rust-toolchain.toml` |
| Agave / Solana CLI | 4.1.2 | `Anchor.toml [toolchain]`, `Cargo.toml [workspace.metadata.cli]` |
| Anchor CLI and `anchor-lang` | 1.2.0 | `Anchor.toml [toolchain]`, `programs/shortwin/Cargo.toml` |
| Platform-tools / SBPF arch | v1.57 (rustc 1.95) / v3 | the build command (below) |
| Node | 24.21.0 LTS | `mise.toml` |
| pnpm | 12.7.0 | `mise.toml`, `package.json` `packageManager` (with sha512) |
| Python | 3.14, standard library only | test-vector generators, `scripts/config_gate.py`, `scripts/dump-fixtures.py`; since M10 `scripts/check-security-txt.py`, `scripts/audit_scope.py`, `scripts/gapcal/` |
| solana-verify (verifiable builds, M10) | 0.5.2, image `solanafoundation/solana-verifiable-build@sha256:2e0b78f4…` (Agave 4.1.2) | `ci.yml` `SOLANA_VERIFY_VERSION` / `SVB_IMAGE_DIGEST`, `scripts/verifiable-build.sh` |
| PostgreSQL (the indexer, M8) | 17.11: embedded in the tests (zonky builds through `postgresql_embedded` 0.21, user cache), `postgres:17.11` in the reference deployment | `services/indexer/src/testing/pg.rs` (`PG_VERSION`), `services/indexer/deploy/docker-compose.yml` |

On-chain crates declare `rust-version = "1.89"`. They are compiled by the platform-tools
rustc (1.95), not the host toolchain, and Cargo's resolver 3 uses that `rust-version` to
prefer dependency versions that still build there. The indexer crates (host only) declare
1.94 (sqlx 0.9, postgresql_embedded 0.21).

## Setup

**Rust.** Install [rustup](https://rustup.rs). The first `cargo` command in the repository
installs 1.98.1 with rustfmt, clippy and the wasm32 target from `rust-toolchain.toml`
(or run `rustup toolchain install`).

**Agave CLI 4.1.2.**

```sh
sh -c "$(curl -sSfL https://release.anza.xyz/v4.1.2/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
agave-install init 4.1.2
solana --version            # solana-cli 4.1.2
```

If another version is active, `anchor` switches to 4.1.2 through `agave-install` because of
`[toolchain] solana_version` (and back afterwards).

**Anchor CLI 1.2.0.** Use the canonical repository `github.com/otter-sec/anchor`:

```sh
cargo install --git https://github.com/otter-sec/anchor --tag v1.2.0 avm --locked
avm install 1.2.0 && avm use 1.2.0
anchor --version            # anchor-cli 1.2.0
```

Installing the CLI directly also works and is what CI does:
`cargo install --git https://github.com/otter-sec/anchor --tag v1.2.0 anchor-cli --locked`.

**Platform-tools v1.57** are downloaded to `~/.cache/solana/v1.57` by the first
`anchor build`.

**Node and pnpm.** `mise install` in the repository root (see [mise](https://mise.jdx.dev)),
then `pnpm install --frozen-lockfile`. The only JS package so far is the Codama client
`clients/js` (`@shortwin/client`); `pnpm-workspace.yaml` also lists `packages/*` and `apps/*`.

## The build rule

**Always build the program with `anchor build --tools-version v1.57 --arch v3`. Never run a
bare `cargo build-sbf`.**

Agave 4.1.2 ships `cargo-build-sbf` 4.1.0, which defaults to platform-tools **v1.54**
(rustc 1.89) and SBPF arch **v0**. A bare `cargo build-sbf` therefore produces a different
program binary from the one Anchor, CI and the verifiable build produce. Anchor 1.2.0
defaults to v1.57 / v3 already; we pass the flags anyway so every command line states them.

Check an artifact: `od -An -t x4 -j 48 -N 4 target/deploy/shortwin.so` prints `00000003`
(ELF `e_flags` = SBPF v3). Outputs: `target/deploy/shortwin.so`, `target/idl/shortwin.json`,
`target/types/shortwin.ts` (from M5 to 1.0.0 also the straddle helper's
`target/deploy/complete_curve.so` and its IDL; the helper was retired with pump's v3 exits,
design pump-final §11, owner decision D2).

Verifiable builds (M10) use solana-verify 0.5.2 with the same pins; the `=` form of the
option is required (clap 2 `require_equals` on `build`, `verify-from-repo` and `export-pda-tx`):
`solana-verify build --library-name shortwin --arch v3 --cargo-build-sbf-args="--tools-version v1.57"`.
solana-verify picks its image from `[workspace.metadata.cli] solana = "4.1.2"` (root `Cargo.toml`):
`solanafoundation/solana-verifiable-build@sha256:2e0b78f4…` (rust 1.95.0, `cargo-build-sbf` 4.1.0;
the `--tools-version v1.57` argument makes it fetch v1.57 inside the container).
`scripts/verifiable-build.sh` builds the deployed program, `shortwin`, that way (until 1.0.0 also
the retired `complete_curve` helper) and must be byte-identical to `anchor build`:

```sh
cargo install solana-verify --version =0.5.2 --locked
scripts/verifiable-build.sh --compare target/deploy          # Docker (CI's verifiable-build job)
scripts/verifiable-build.sh --local --compare target/deploy  # no Docker daemon: bubblewrap shim
# --out DIR (default target/verifiable): shortwin.so, the build logs and verifiable-hashes.txt
```

- `verifiable-hashes.txt` holds, per program, the file's sha256 and solana-verify's
  **executable hash** (the file with its trailing zero bytes trimmed: what `get-program-hash`,
  `verify-from-repo` and verify.osec.io compare with the on-chain ProgramData).
- `--compare DIR` fails unless each program equals `DIR/<program>.so` byte for byte. CI compares
  with the `anchor-build` artifact, so the binary that is deployed and verified is the tested one.
- `--local` runs the same image without Docker. `scripts/svb-shim/fetch-image` downloads it by
  digest from Docker Hub, checks every blob's sha256 and extracts it once into `$SVB_CACHE`
  (default `~/.cache/shortwin/svb`). `scripts/svb-shim/docker` stands in for `docker` and runs it
  under bubblewrap in an unprivileged user namespace (no daemon, no root). It builds a fresh clone
  of HEAD: committed state only; the checkout's `target/deploy` is not touched. A container that a
  failed build leaves behind is removed on exit. `fetch-image` refuses an image whose layers would
  delete, write or hard-link through a symlink that leads out of the extracted rootfs (the rootfs
  has absolute links such as `var/run -> /run`). Needs `bwrap`, unprivileged user namespaces,
  python3, ≈ 8 GB on disk (a reflink file system such as btrfs makes each container instant) and
  network (crates.io, platform-tools v1.57 from GitHub).
- The shim runs any digest-pinned image of solana-verify's IMAGE_MAP (`SVB_IMAGE_DIGEST` is the
  allow-list). M10 rebuilt Raydium CPMM's 2026-10 redeploy that way, byte for byte, in the 3.1.10
  image (`tests/fixtures/README.md`).
- The script refuses a build tree in which another `Cargo.toml` has the same library name
  (`--library-name` takes the first match).
- On `pull_request`, CI builds GitHub's merge commit; `verifiable-hashes.txt` names it and the PR
  head (`PR_HEAD_SHA`).

On-chain verification (the PDA upload through Squads and `solana-verify remote submit-job`; the
old `verify-from-repo --remote` is deprecated and returns an error) needs the deployed program and
public source: M11.

**A comment-only program edit is bytecode-neutral only if no line below it moves.** Anchor's error
origins embed source line numbers (`error!`, `require!`), so a comment that adds or removes a line
changes the ELF. Check by hash (`sha256sum target/deploy/*.so` before and after).

`anchor build` builds the two programs of the Anchor workspace: `shortwin`, the one deployed
program, and the TEST-ONLY mock venue `tests/mock_cpamm` (`target/deploy/mock_cpamm.so`,
below). The stateless straddle helper `programs/complete_curve` (M5; ARCH §5.4, A66) left the
workspace with pump-final P6 (owner decision D2: pump's v3 exits complete the curve themselves,
and the helper was never deployed); `git show v1.0.0:programs/complete_curve` keeps its audited
source. **Deploy by name** (`anchor deploy -p shortwin`, or the verified-build path): a bare
`anchor deploy` would also deploy the mock.

### The test-venue build (M2; A49)

The position engine is tested against a TEST-ONLY constant-product venue, `tests/mock_cpamm`
(program id `BPAZFPkmVtYEWEZHCzFUns7oybh1VWfAvmu3ofM2mQi7`; its interface, byte offsets and
the reader contract are in [`tests/mock_cpamm/INTERFACE.md`](../tests/mock_cpamm/INTERFACE.md)).
short.win reaches it only through `VenueKind::Mock`, which exists only with the Cargo feature
`test-venue`. That feature goes into a **separate build** that never replaces the release
artifact:

```sh
anchor build --tools-version v1.57 --arch v3   # release: target/deploy/shortwin.so (no test-venue), mock_cpamm.so
scripts/build-test-venue.sh                    # TEST-ONLY: target/test-venue/deploy/shortwin.so
```

The script runs `anchor build --tools-version v1.57 --arch v3 --ignore-keys --no-idl -p shortwin
-- --features test-venue` in its own target directory (`target/test-venue`, or
`SHORTWIN_TV_TARGET`), checks SBPF v3 and that the build carries the marker
`shortwin:test-venue:mock-cpamm`. CI checks the other direction: the release `shortwin.so`
contains neither that marker nor the mock's program id (as raw bytes, as its four 8-byte
words, or as the four `lddw` immediates it compiles to), and `mock_cpamm.so` and the
test-venue build do. The mock's keypair (`tests/mock_cpamm/keys/`) is gitignored like every
program keypair.

Because `tests/litesvm` enables `shortwin/test-venue`, a workspace-wide `cargo test` or
`cargo clippy` compiles the program **with** the feature (Cargo unifies features); run
`cargo clippy -p shortwin --all-targets` and `cargo test -p shortwin` on their own to cover the
release feature set (CI does both).

## Everyday commands

```sh
cargo fmt --all --check
cargo clippy -p shortwin-math --all-targets -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings
PROPTEST_CASES=2000 cargo test --workspace           # the CI count; LiteSVM tests need the .so (below)
PROPTEST_CASES=20000 cargo test -p shortwin-math     # deeper local run (can only raise the count)
cargo build -p shortwin-math-wasm --target wasm32-unknown-unknown --release
cargo check -p shortwin --target wasm32-unknown-unknown   # T-LAYOUT wasm32 leg
cargo test -p squads-v4-ix                           # M10: the Squads v4 encoders against the vendored IDL
anchor build --tools-version v1.57 --arch v3
python3 scripts/check-security-txt.py --forbid-placeholders --source-release v1.0.1 \
  target/deploy/shortwin.so "short.win"              # M10 SECTXT-1: the release gate (1.0.1)
python3 scripts/test_check_security_txt.py           # the checker's own tests
cargo deny check
cargo audit --deny warnings --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2021-0139 \
  --ignore RUSTSEC-2024-0388 --ignore RUSTSEC-2024-0436 --ignore RUSTSEC-2025-0161
```

(The `test` job runs `shortwin-math` at `PROPTEST_CASES=2000`; the M2 gate also runs the
whole workspace at that count.) The ignored advisories are the ones in `deny.toml`: all
"unmaintained", none a vulnerability;
bincode comes with anchor-lang 1.2.0, the other four only with the LiteSVM test harness.

**security.txt (M10, ARCH §8.5).** The deployed program embeds a
neodyme security.txt (`solana-security-txt =1.1.3`, `security_txt!` in
`programs/shortwin/src/lib.rs`; the retired `complete_curve` carried one too until 1.0.0): the
owner's values of 2026-10-02 (contact `security@short.win`, policy `https://short.win/security`)
and `source_release` = `v` + the package version. It is compiled out of `no-entrypoint` builds (the keeper,
the harness and the fuzzer link the programs as libraries) and out of the TEST-ONLY `test-venue`
build. `scripts/check-security-txt.py` mirrors the crate's and the Explorer's parser and requires
exactly one marker; without `--forbid-placeholders` an `OWNER-FILL` placeholder is reported, not
refused. CI runs it with `--forbid-placeholders --source-release v<Cargo version>` on the release
ELF and checks that the test-venue build carries no marker. Read a deployed program's with
`query-security-txt <program id>`.

Test vectors from the normative sims: `python3 crates/shortwin-math/tests/vectors/<track>/gen_<topic>.py`.
The conventions are in [`crates/shortwin-math/tests/vectors/README.md`](../crates/shortwin-math/tests/vectors/README.md).

On a machine with little RAM, `CARGO_BUILD_JOBS=4` keeps the LiteSVM and Mollusk builds within
about 6 GB.

## Program tests (LiteSVM)

The program tests (ARCHITECTURE §8.2, §8.3) are the crate `tests/litesvm`
(`shortwin-litesvm-tests`, LiteSVM 0.16.0 on the Agave 4.2.2 runtime crates pinned in the
workspace `Cargo.lock`, C1-14). They load the SBF builds, so build them first:

```sh
anchor build --tools-version v1.57 --arch v3          # target/deploy/shortwin.so and mock_cpamm.so
scripts/build-test-venue.sh                           # target/test-venue/deploy/shortwin.so (M2 tests)
cargo test -p shortwin-litesvm-tests                  # every binary below, ≈ 25 min (debug, M10)
cargo test -p shortwin -p shortwin-venue-state        # host unit tests of the program and readers
```

Other builds: `SHORTWIN_SO` (the release program: the M1 binaries and every Raydium and pump
binary), `SHORTWIN_TEST_VENUE_SO` (the test-venue program, M2 binaries), `MOCK_CPAMM_SO` (the mock
venue) (`COMPLETE_CURVE_SO`, the straddle helper's, is gone with it, pump-final D2);
`SHORTWIN_PRE_M4R_SO` and `SHORTWIN_PRE_M4R_TEST_VENUE_SO` name a
release and a test-venue build of the tree before M4R, for `switch_confirm_it`'s ignored lockstep
differentials only. `Env::test_venue()` loads the test-venue program and the mock; `Env::new()` the release
program only; `Env::ray_fork()` the release program and the dumped Raydium programs (below);
`Env::fork_pump_coin(coin)` the release program, the dumped pump, pump-fees and PumpSwap programs,
pump's statics, one coin's accounts, mainnet's Rent and the pump snapshot's clock (M5);
`Env::fork_pamm_coin(coin)` (M6) the same with PumpSwap's statics and one canonical pool role of
the `pamm_state` dump (`Env::plant_negative_vqr` swaps in the synthetic negative-VQR pool);
`Env::with_squads()` (M10) adds the immutable mainnet Squads v4 and its `ProgramConfig`, and
`Env::squads_governed()` the owner's layout on it (Governance 3-of-5 with a 48 h Squads time lock,
Emergency 2-of-4 with none, `initialize_config` with the vaults). A
stale `target/deploy/shortwin.so` fails the Raydium and pump binaries in confusing ways: rebuild
after any program change (or point `SHORTWIN_SO` at a private build).

**The mainnet SPL programs (M10, T-MEMO).** Every `Env` replaces LiteSVM 0.16's bundled Token-2022
and spl-memo v3 with the builds mainnet runs, right after `LiteSVM::new()`
(`fork::load_mainnet_spl`, fixture group `spl_programs`): Token-2022 from its dumped ProgramData
(deploy slot 427,147,035, the live upgrade authority; trimmed ELF `7ea94a02…`, OtterSec's verified
build of `solana-program/token-2022@9bc02757`, program v11.0.0) under the upgradeable loader, and
spl-memo v3 under BPFLoader2 (account sha256 `f520eaf0…`); both are held to their manifest
sha256 and pinned in `fork.rs`. `SHORTWIN_BUNDLED_SPL=1` keeps the bundled builds. SPL Token
(p-token), the ATA program and spl-memo v1 stay LiteSVM's. Token-2022 CPIs cost about 15 CU more
on the mainnet ELF than on the bundled one; the benches, which copy the `Env`'s ELFs into Mollusk,
were re-baselined on it. The memo-required withdrawal and its control:
`cargo test -p shortwin-litesvm-tests --test security -- sec_7 t_memo --nocapture` (prints the
memo's CU, 7,928, and the whole withdrawal's, ≈ 42,700).

**The curve venue bit in tests (M5-SEC-4).** The M5 build refuses to enable the pump-curve venue
(`constants::VENUE_BITS_UNFINISHED`; `initialize_config` and `EnableVenues` fail `InvalidParams`),
so no deployment of it takes curve opens before M6's `→ PumpSwap` transition. The harness's
`Env::initialize_config` therefore sends its arguments without that bit and then writes it into
`GlobalConfig.venue_enabled` directly (`Env::force_venue_bits`, TEST-ONLY), so the curve suites run
the venue as M6 will enable it; `Env::initialize_config_as_sent` sends the arguments unchanged
(the gate's own test). **M6 emptied the set** (`VENUE_BITS_UNFINISHED = 0`): the forcing is now inert and `initialize_config` sends its arguments unchanged; the mechanism stays for a later unfinished venue.

| Binary | Covers |
|---|---|
| `--test it` | admin and config (T-CONFIG on-chain: every compiled bound, the timelock), pause, T-LATCH (mock ProgramData), T-LAYOUT (runtime offsets), lender constraint cases, the harness, the admin CU table (`cu_baseline -- --nocapture`) |
| `--test market_create` | `create_market` (exact state, creation fee, griefing, detection keys, venue hints) and `flag_unsafe_mint`; M10: SQ-G8, the four authorities pairwise distinct (`AuthoritiesNotDistinct`, 6105) at `initialize_config` and at an `Authorities` proposal and its apply |
| `--test mint_filter_extensions` | the SPEC §3 allowlist filter: one fixture per Token-2022 extension, program vs byte-level filter; M10: spoofed owners at the detection PDAs (G-A3) |
| `--test lender_it` | the six lender-pool instructions, T-POT, the lender adversarial cases (C0-6, C0-9, C0-13, R3A-2, R3A-10, R3B-1, R3B-2, V2-2, V2-12), `pool_model` trace replays |
| `--test lender_real_it` | M10 (WP-8c): the lender cases on **real** exits — a JIT whale's `[deposit 9×, exit, claim_sol, withdraw]` around a real liquidation's penalty, an expiry's fee, a `sweep_trader` and (on the fork) PumpSwap's cashback lump realises 0, a whale holding `h` takes `h/lump_vest_s` of its share; `[withdraw, sol_settle]` in one transaction held to the pre-settlement allowance; `claim_sol ×N`; two consecutive real wipes; `rent_protocol_bps` changed with positions open; the same on Raydium CPMM; finding M10-8C-1's regressions (`finding_m10_8c_1_*`: the rent top-up of an early exit vests, the whale realises 0) |
| `--test oracle_crank` | `crank_sol_usd` on the committed mainnet `PriceUpdateV2` dump (`tests/fixtures/`, checked by `sha256sum -c SHA256SUMS` there) |
| `--test security` | the M1 security review's findings as regressions and its refutation tests |
| `--test security_m2` | the M2 security review's findings as regressions: M2-SEC-1 (no box collapse on an executable venue), M2-SEC-2 (the `stable` extras set), M2-SEC-3 (canonical template bumps), M2-SEC-4 (`sweep_trader` reopens lending after a full wipe), M2-SEC-5 **inverted by M5** (`m2_sec_5_sol_settle_reaches_s3_on_a_none_market`: `sol_settle` on a `None` market takes the detection set as ReadSet and settles S3 after the chain, FV at the cap-only view; a planted state, hooks off from the plant — the real transition runs with hooks on in `pump_it::graduation`), and A49 (the release build refuses the mock venue); since M10 on mainnet's Token-2022 and spl-memo v3: `sec_7_*` (the memo CPI right before `transfer_checked`, 7,928 CU) and `t_memo_control_top_level_sibling_check` (T-MEMO) |
| `--test mock_cpamm` | the mock venue itself: execution equals `shortwin-math`'s `quote::cpmm` exactly (a differential proptest, `MOCK_CPAMM_CASES`, default 96), the layout at its documented offsets, every knob (disable, fee, layout change, over-delivery, required extras), delegate-aware sources, Token-2022 bases |
| `--test engine_it` | M2 engine: `open_short` / `close_short` flows and every refusal reachable on the mock, the prologue, trader normalisation and griefing, over-delivery, sandwiches around opens and closes, the O6 powers and T-REPLAY-PDA, T-TRACE / CU, the structural I14 test, the hooks' own mutants, and the seeded random walk (`ENGINE_FUZZ_SEEDS`, default 4, and `ENGINE_FUZZ_STEPS`, default 120) over opens, closes, top-ups, keeper exits, shocks, deadlines and I10's pause-and-flag action; M10: the extras denylist on every leg instruction (`o6`, G-A5; since batch 3 also the borrower and the mock pool and its vaults; the template-alias refusals on the real venues are `ray_it`, `pump_it` and `pamm_it` tests) and the zero-lamport borrower (`zero_lamport`, G-H10) |
| `--test add_margin_it` | `add_margin` (every gate ignored, snapshots unchanged, I14 on its accounts, a random walk: `ADD_MARGIN_WALK_SEEDS`) |
| `--test crank_price_it` | `crank_price`: `CoverageGap`, same-second floods, the ring rewrite, forced EMA adoption |
| `--test liquidate_it`, `--test expire_it` | the keeper exits against an oracle recomputed from `shortwin-math` alone: every waterfall branch, every refusal, what exits ignore (I10), donations, over-delivery, extras, I14, liveness (I-E11), T-TRACE / CU, hook mutants |
| `--test attacks_exit_it` | ECON §12's exit attacks on ECON's `RA` market, to the lamport: `sandwich.py`, `fixed_delta.py`, `block_spot.py`, `ema_atomic.py`, `ema_near.py`, `ema_atomic_multi.py` (the OI-cap basket: atomic, held, and S1 forcing), `ptf_sandwich.py`, `fvcap.py` (the RA rows), `cascade.py` |
| `--test settle_it` | `sol_settle`: arming, S1 / S2 / S3, epoch records, T-BOX (seeded push / freeze / dump runs) |
| `--test route_it` | T-S3-BROKEN (the flag, a fee above the hard cap, a disabled venue, a layout change; liquidations and closes equal the unflagged run; the Emergency-flag S3 raced against an honest expiry) |
| `--test extras_it` | T-REPLAY-PDA end to end (a per-mint PDA extra recovered with a template; a hostile `current` bypassed); M10 G-G3: the global hard cap moved across the live fee with two positions open |
| `--test attacks_settle_it` | ECON §12.10–§12.11 on an `RC`-like pool: `arming.py`, `refute_push.py`, `fv_push.py` (incl. the S2 → S1 flip and its limit) |
| `--test invariants_m2_hooks` | the M2 hooks fire on deliberately broken states (I2, I3, A26, I5–I7, I9–I13, I16) |
| `--test ray_math_it` | M3, T-RAY-MATH (execution half): direct swaps on the dumped AMM v4 / CPMM ELFs (both directions, exact in and out, every fixture pool, all 21 CPMM configs × creator-fee sides, fee and `need_take_pnl` changes, seeded sequences: `RAY_MATH_SEQ`, default 40, and `RAY_MATH_SEED`) equal `quote::ray_v4` / `quote::cpmm` on the readers' reserves, the exact bound passing and one unit tighter failing; negative controls; fixture integrity and venue PDAs; both readers' route predicates against the ELFs; the byte-level admin builders against the real admin instructions; M10: I17's M6-SEC-3 clause on the pump fork |
| `--test ray_it` | M3, the Raydium adapters on the fork: listing (exact state, O13, every verification / eligibility refusal, D1 precedence on Fartcoin's real curve), open → close to the lamport on both programs (SPL, Token-2022, WSOL on either side, every fee variant incl. U11), the Raydium open refusals, T-LATCH (byte-patched slots), T-RAY-PDA, T-RAY-WSOL, T-RAY-DISABLE, T-RAY-FEE, liquidation and expiry, I14, the hooks' Raydium view, T-RAY-TRACE / CU and T-SIZE (`-- --nocapture` prints them) |
| `--test raydium_v4_e2e`, `--test raydium_v4_attacks` | M3 fork E2E on Fartcoin (AMM v4) and ECON §12 on the real pool, every short.win instruction against the differential oracle of `raydium_v4_support.rs`: lifecycle (listing window, Tier C → A, close, two liquidations after a real pump, expiry, the lender's exit), S3 after a real `SetParams` halt, S2 via LP removal (unreachable at the defaults; reached under an in-bound governance corner), S1 on a real gap; exit sandwiches, atomic and held pushes, `fv_push` and the flip bundle (`-- --nocapture` prints the figures); M10 (on the 2026-10 CPMM ELF): a thin pool in place of the adopted one on every instruction (G-A6), a denylisted Emergency extra bypassed with `previous` and, since batch 3, the pool, its vaults (template aliases of the CPMM `pool_vault` PDA included) and the borrower refused as extras (G-A5), griefing on CPMM (G-B), the zero-lamport borrower on both programs (G-H10), and venue admin with a position open (`route::amm_v4_*` / `cpmm_*`: the Emergency flag's S3, the liquidation differential, the O15 race, G-G5, G-H3) |
| `--test raydium_cpmm_e2e` | M3 fork E2E on CPMM: the USELESS, Token-2022 and creator-fee journeys (list → deposit → open → real price moves → close / liquidation / expiry → S3 on an admin halt and on a fee hike above the hard cap → S2 on an out-of-model drained pool → lender exit), the settlement matrix on every fixture pool, U11 through short.win's exits, O13 ages |
| `--test raydium_cpmm_exits_it` | M10 (fuzz follow-ups): `raydium_v4_support`'s keeper-exit oracle on the 2026-10 CPMM ELF (USELESS, the Token-2022 pool, the creator-fee pool with its fee on either side): refusals as predicted, a liquidation after a real pump and an expiry checked to the lamport |
| `--test raydium_cpmm_attacks`, `--test pumpswap_attacks` | M10 (WP-8b): ECON §12 on Raydium CPMM (USELESS, the creator-fee pool) and PumpSwap (`mature`), with the shared `venue_attacks_support.rs` (no oracle on these venues: the program's own decision read on a clone of the VM, the structural claims asserted on what lands — I11, the lenders' legs, never S1): sandwiches, held pushes (O19's figures), the OI basket, the held fresh push, the `p′` veto, LP round trips around `sol_settle` (G-E4); PumpSwap also round 3 (`-- --nocapture` prints the figures) |
| `--test latch_raydium_it` | M3, T-LATCH on the dumped ProgramData: real loader upgrades (the Squads vault's signature forged), opens refused on every Raydium market while exits keep working, revalidation only at the loader's slot, a second upgrade during review, both Raydium bits, `[flag, open]` vs `[open, flag]`, U5 (the same CU on 1.4 MB, 0.79 MB and 10 MiB) |
| `--test trace_raydium_it` | M3, T-RAY-TRACE: the exact call shape (program and stack height of every CPI) and trace entries of every Raydium-path instruction on AMM v4, CPMM and Token-2022 CPMM; peak height 3; the worst-case transactions ≤ 64 and the runtime's refusal of one open more; M10: the upgrade mainnet made, CPMM's 445,763,504 build upgraded by the loader to the 2026-10 redeploy and latched until revalidated |
| `--test ray_sec_it` | M3 security review regressions on the fork: M3-SEC-1 (a CPMM config trade fee of 0 is no route — S3 chain, healthy positions frozen, never an uncapped S1 — and a fee-free risk view; the fee-split predicate against the ELF), M3-SEC-2 (Raydium listings refused while the Raydium latch is set), M3-SEC-3 as inverted by M4 (`m3_sec_3_propose_pool_unpins_the_first_listing`: a list-only front-run of a Raydium token moved onto its main pool by `propose_pool`; a thin eligible listing replaced by the main pool inside `pool_list_window_s`, the window not extended) |
| `--test pool_select_it` | M4, Raydium pool selection on the fork (release build, dumped programs, **planted** candidate pools — `pool_select::plant_v4` / `plant_cpmm`, a dumped pool's bytes on fresh keys that the dumped ELFs trade): a list-only market's adoption (exact state, O13 / O13b ages, D1 on real and planted curves, base/quote order); the listing window's 5 % margin, never extended; zero-position adoption at 25 % and `PositionsOpen`; AMM v4 ↔ CPMM switches with trades on the new pool; the fast path with open positions (the `pool_fail` chain — halt, fee above the hard cap, never the Emergency flag —, 120 s, the 60 s window keeping the deepest candidate, locked terms and EMA continuity, the ring rewrite and a ≤ 1 % box from the first second, exits on the new pool), cancellations, the latch; every refusal; T-RAY-SWITCH's attacks; #99's switch only if all fit (`carried_fit`: per position and per market on both programs, cancellation at the execution, the dump guard at `P_ema`, a withdrawal-wrapped execution, the mark) and #100's FIT regressions (`carried_fit::fit1_*`: FIT-1's residual — a dump-wrapped proposal carries at most the execution's cap, ≤ `λ·√(P_cand/P_ema)` × a native open's cap, knobs `FIT_R_BPS`, `FIT_Q_PERMILLE`; `carried_fit::fit3_*`: FIT-3's residual — a closed max-size position keeps a fitting candidate refused while positions are open, and they settle S3 at their deadlines with the lenders at FV; the I15 hook `i15_the_carried_fee_and_the_marks`: the fee mark `min_open_fee_ppm` moves only at an open and the last exit); the I15 hook's mutants; T-TRACE / CU (`-- --nocapture` prints them) |
| `--test pool_attacks_it` | M4 pool-capture attacks on the fork (ARCH §2.6's attack table; the attacker's liquidity moves by the dumped CPMM ELF's real `deposit` / `withdraw`): unlocked-LP capture and pull (`D` unchanged), λ burn / locker / "unburn" games, hysteresis and the wash-trading cost of a flip, fast-path front-running, R4B-10 griefing and the pause remedy, mirrored and look-alike quotes, the latch (per candidate program, M4-SEC-8), `venue_switch.py`'s RA / RB / RC rows, no forced exit through a switch; the implementer's two tripwires are **inverted** by the M4 security review (`lambda_games::lp_dilution_no_longer_cancels_the_fast_path`, `…::a_round_trip_under_the_sol_floor_no_longer_cancels_the_fast_path`: an atomic `[deposit LP / sell, execute_pool_switch, withdraw / buy back]` no longer cancels the fast-path window, eligibility being the locked depth at `P_ema`) (`-- --nocapture` prints the figures) |
| `--test m4_sec_it` | the M4 security review's findings as regressions on the fork (each fails on the pre-fix build): M4-SEC-1 (a persistent unlocked deposit no longer blocks the fast path; the halted market adopts and the underwater position is liquidated with and without it), M4-SEC-2 (a window whose arming chain broke — flapping or stale — cancels; the new failure re-arms after 120 s), M4-SEC-3 (eligibility unmovable inside a transaction on a market with a price; the bounded residual on a list-only market), M4-SEC-4 (a list-only market with positions collects the deepest pool; a pump venue cancels the window), M4-SEC-5 (restated by M4R: the adoption writes the anchor `⌊D·10⁴/λ_min⌋` and reads no sample, so a deposit or an unlocked LP's withdrawal around the execution — or around the proposal and the execution — changes nothing; after the warm-up `L_ema` is the pool's depth), M4-SEC-6 (a deeper or thinner candidate: the box is ≤ 1 % wide in `L` from the gate on), M4-SEC-8 (the latch holds only the candidate's program). M4-SEC-7 is the keeper scenario `junk_pools_cost_a_bounded_ranking_after_the_cranks` (`-- --nocapture` prints the figures) |
| `--test m4v_it` | the M4 verification's findings as regressions on the fork: M4V-1 (a collection window's candidate needs its locked SOL alone at `P_ema` above `pool_min_real_sol`: 20- and 13-SOL pools refused `PoolTooThin`), M4V-2 (restated by M4R: deposits around the proposal, the execution or both change nothing; the underwater position is liquidated at adoption + 9 s; a deposit-wrapped crank every second does not block a big position's liquidation — the reference guard), M4V-3 (a `λ < λ_min` candidate: `L_ema = √(X·Y)` ±1 % at the gate, the box ≤ 1 % after it), M4V-4 (a latched window candidate cancels, the chain stays armed), M4V-5 (a captured list-only market opens nothing until `ema_warm = 0`, then `R_eff` refuses the pushed open; since M6-SEC-1 the seed is ≈ 2.73× the pool at the gate and on it a minute later, the opens still refused) |
| `--test m4r_it` | M4R, the post-change confirmation (adopted, O18, 2026-10-09), on the fork: M4R-1 (a withdraw-wrapped switch and per-second withdraw-wrapped cranks and liquidations force nothing; every liquidation before the gate fails `EmaWarmingUp`; the genuinely liquidatable control is liquidated at +9 / +10 s; the degraded mode with an attacker-only warm-up), M4R-2 (a dump during the halt keeps a thin pool out; the pump-wrapped proposal residual), ★T-SWITCH-CONFIRM (a genuine 2.2× pump liquidated at adoption + 9 s; a keeper back after 600 s liquidates 8–9 s later); the M4R verification's M4X-1 (attacker-only price bundles through the warm-up: the seed rises by at most the largest v0.4.1 step, checked per integration; a position with a ≈ 3× threshold is no longer liquidated at a flat 1.00×), M4X-2 (a deposit freeze held for 900 s: `L_slow` stays at the honest lows while the guard is set, the genuine pump ends in a liquidation, not S1; the I18 mutant) and M4X-3 (a dump inside the collection window is adopted at `P_ema`; the warm-up then lowers the seed by at most one v0.4.1 step per integration since M6-SEC-1 — 1.73× the pool at the gate, on it a minute later —, an open at the gate is refused `PoolListWindow`, after the adopted pool's listing window `PoolTooThin`; the disclosed residual); the final check's M4F-1, restated by #99 (`m4f1_a_deposit_freeze_on_a_fitting_carried_position_only_delays_the_liquidation`: the position sized to fit the candidate, `q/x` 0.71 %, the keeper's coverage rules modelled second by second, a deposit-wrapped crank every second from the adoption or from +40 s under the shipped keeper and under the finding's cadence — every run LIQUIDATED at +168 s at 2.247×, like the control; `m4f1_the_depth_cap_bounds_the_frozen_reference_at_lambda_over_lambda_min`: under the finding's cadence `L_b` held at the Raydium cap `⌊D·10⁴/λ_min⌋`, 1.995× / 1.000× the depth on λ 99.78 / 50 % candidates, both liquidated; `m4f1_an_oversized_carried_position_is_not_adopted_and_settles_in_s3`: the finding's size is refused, S3 at the deadline with lenders at FV); ARCH §8.2; `m4r_explore_liquidation_grid` is ignored (knobs `M4R_GRID`, `M4R_SECS`, `M4R_SOL`) (`-- --nocapture` prints the figures) |
| `--test switch_confirm_it` | M4R's gate and guard: `liquidate` / `expire` fail `EmaWarmingUp` and `sol_settle` evaluates nothing during the warm-up on an executable venue, and only there (a new pool that halts in the warm-up reaches S3); the guard clears at the gate on an honest pool; a market without a switch is never marked; the ignored `lockstep_*` differentials send every instruction of a 90-s scenario to the pre-M4R build too and compare the accounts byte for byte (they run when `SHORTWIN_PRE_M4R_SO` / `SHORTWIN_PRE_M4R_TEST_VENUE_SO` name the pre-M4R release and test-venue builds; `-- --ignored lockstep`) |
| `--test tx_size_raydium` | M3, T-SIZE: every Raydium transaction in its worst case (both ComputeBudget instructions, a separate fee payer, a first-time borrower, the largest extras set) fits 1,232 B and 64 accounts as v0 through the protocol ALT (A28 as enumerated, and the table `scripts/build-alt.sh` builds) and executes that way; crank batches (4 per v0 transaction); the keeper's composite and a Jito tip; M4's `propose_pool` / `execute_pool_switch` in their largest shapes (CPMM incumbent + CPMM candidate, a list-only market's detection set), executed through the table (`-- --nocapture` prints the table) |
| `--test pump_math_it` | M5, T-PUMP-MATH (execution half) on the pump fork: a normal user's `buy_v2` / `sell_v2` / `buy_exact_quote_in_v2` on every live SOL curve of the dump (125-, 151-, 115- and 49-byte layouts, SPL Token and Token-2022) over grids and seeded sequences equal `fees::pump_curve` and `quote::sol_leg` on the readers' state to the lamport — every account delta and the `TradeEvent` —, the exact bound passing and one unit tighter failing; the live and synthetic fee tiers, the creator override, a default creator; 6021 above `rT`, the completing buy, 6005 after completion; the undocumented cap byte; the creator-vault top-up; recipient refusals; the tripwire on the live-supply fee tier (R-PUMP Update U10). Since the re-pin on pump's 2026-10-13 build (a curve under 166 bytes grows to 166 on its next trade; `the_2026_10_13_build_runs_short_wins_paths_as_the_2026_10_08_build` replays every path on both builds, gate G0b), and **T-PUMP-MATH-V3** (`v3_*`, gate G0): pump's own v3 trades against the v3 model, the synthetic migration past `rT` (its charge and events, a donated `curve_base_ata`, the creator override and a default creator on both parts, `buy_exact_quote_in_v3`), `migrate_v2`'s pool and boost, the tier tripwire, 6094 on a cashback coin, no synthetic path on the 2026-10-02 build |
| `--test pump_it` | M5, the pump-curve adapter on the fork (release build, dumped pump / pump-fees / PumpSwap programs and coins): `listing` (`create_market(Auto)`: curve cache, trader base ATA and pump UVA, the list-only outcomes, every refusal), `flows` (open → close to the lamport on every curve layout), `refusals`, `structure` (I14), `rent` (T-RENT: since pump-final no creator-vault top-up — v3 names no vault —, the UVA created at listing, a short curve's growth to 166 B out of `S`, the net check), `pda` (T-PDA-USER: the trader PDA as pump's `user` under CPI), `extras` (T-V3-ADAPTER: the 10 TradeExtras and the 17-account `buy_v3` / `sell_v3` CPI; 1.0.0's 20-key list refused), `buckets` (the curve's fee buckets outside `rQ`), `straddle` (since pump-final: a buyback past `rT` completes the curve by synthetic migration in one instruction at `B̂_v3`, the live supply's fee; `B̂_str` only for a `PumpMigrating` market's states, still against a third party's completion + `migrate_v2` + PumpSwap `buy`), `graduation` (`→ PumpMigrating`, the migration pause and S3 at 70 min, a real `migrate_v2`, `→ None` and S3 on the detection set, `None → PumpCurve` re-detection; M4's carry-overs: a graduated-elsewhere market with a position adopts its Raydium pool through the collection window, and the re-detection clears a running window; since M6 a migrated market moves `→ PumpSwap` and a graduated coin lists on its pool; the M6 security review's `m6_sec_5_*` — a full wipe during the migration pause waits for the pump UVA's claimable balance (since pump-final P2 seeded as lamports on a non-cashback curve, T-WIPE-GATE: v3 refuses live cashback curves), without one `sweep_trader` alone unlocks the market — and M6-SEC-3 as the owner decided it on 2026-09-30 (ARCH §3.4, SPEC §14): `m6_sec_3_curve_opens_are_refused_exactly_while_create_pool_is_halted` — a curve open fails `GraduationHalted` (6104) exactly while PumpSwap's `disable_flags` bit 0 is set (0x01 / 0x03 / 0x09 / 0x19 / 0xff refused, 0xfe opens, clearing the bit re-enables opens, a close runs under the halt) — and `m6_sec_3_a_position_opened_before_the_create_pool_halt_stalls_until_s3` — a position opened before the halt keeps the stalled-graduation path: `migrate_v2` fails 6017 (`DisabledCreatePool`), the healthy position frozen past the 70-min pause until its deadline, then S3), `exits` (expiry, liquidation, `sweep_trader`'s token leg, the pump latch bits); since pump-final `graduation` also prices a synthetic completion on the pool `migrate_v2` creates (T-MIG-PRICE, T-MIG-S3, T-MIG-DONATION, T-MIG-GUARD, T-LIVE-PC-GUARD) and observes a complete cashback curve during the pause, and `listing` refuses a live cashback curve `CurveCashbackCoin` (6109, T-LISTING-CASHBACK). The synthetic exits themselves are `--test synthetic_exit_it` (T-SYNTH-1 … -6, T-SIGMA, T-MIG-HALT) |
| `--test synthetic_exit_it` | pump-final T-SYNTH-1..6, T-SIGMA, T-MIG-HALT (it replaces M5's `straddle_it`, retired with the straddle bundle and the `complete_curve` helper, owner D1 (a) and D2): short.win's own close, liquidation and expiry past the curve's `rT` complete the curve in one `buy_v3` (pump v3's synthetic migration), quoted `B̂_v3`; the keeper's `migrate_v2` and transition after it; trades and third-party completions before the exit; `max_in`; the forged `q − rT ≥ X_s` (`CurveStraddle`); a donated `curve_base_ata`; σ on `B̂_v3`; a synthetic close under PumpSwap's `create_pool` halt |
| `--test upgrade_it` | pump-final T-SELF-UPGRADE, on pump's 2026-10-13 build: short.win 1.0.0 (the fixture `shortwin/shortwin_1_0_0.so`, refused unless it carries AUDIT §0's verified 1.0.0 hashes) refuses a 10-key (v3) list with nothing written and opens curve positions with its 20-key v2 list; the loader's own upgrade to the build under test (`ExtendProgram`, a buffer, `Upgrade` signed by the upgrade authority) leaves every `Market`, `ShortPosition` and `MarketVault` byte-identical; after it a 20-key close fails with nothing written and the 10-key close lands, the keeper's `[crank, liquidate]` after a real pump and a synthetic `q > rT` `[crank, expire]` land, and a pre-upgrade position on the migrating market settles S3 at ARCH §5.4's ρ-scaled FV. The keeper's format choice across the upgrade is the keeper-litesvm `upgrade_window` scenario |
| `--test pump_curve_scenarios`, `--test pump_curve_attacks`, `--test pump_curve_e2e` | M5 fork E2E on the curve (shared `pump_curve_support.rs`, no tests of its own; ≈ 2.5 / 1.5 / 1.3 min debug): ECON §11.2 and §10.1 curve rows replayed second by second against `shortwin_sim` to the lamport (D@50 % / D@80 % / D@90 %, stuck migrations with S3 at +4,200 s; D@0 % refused, `ProceedsBelowMin`); ECON §12 on the curve (expiry and liquidation sandwiches, atomic and held pushes, the OI-cap basket); the lifecycle on a live curve (listing, lenders, opens, a close, liquidations, an expiry, the lenders' exit), completion with positions open (`→ PumpMigrating` → `None`, S3), a curve migrated elsewhere between two reads, a real `migrate_v2` moving the market onto the pool (M6), and a fast `migrate_v2` priced on the pool (M6; the M5-only EMA freeze is gone); §10.1's graduation rows are `graduation_scenarios`' (`-- --nocapture` prints the figures) |
| `--test trace_pump_it` | M5, T-TRACE on the curve: the exact call shape (program and stack height of every CPI) and entries of every curve-path instruction (listing, cranks incl. the venue transitions, opens, closes incl. a completing buyback, liquidate, expire, each exit also a synthetic completion above `rT`, `sol_settle` on `PumpMigrating` and `None`, `sweep_trader`, the latch), peak height 3 (since pump-final every leg is one pump CPI, `sell_v3` / `buy_v3`, at height 2 and no pump-fees CPI); the worst-case transactions ≤ 64 and the runtime's refusal of one open more |
| `--test tx_size_pump_it` | M5, T-SIZE on the curve: every curve transaction in its worst case (the largest extras set, a separate fee payer, a first-time borrower) as v0 through both tables (A28 with its pump part, and `scripts/build-alt.sh`'s), executed that way; crank batches (5 per v0 transaction); with the largest extras set the keeper's composite, a tipped liquidation and the tipped composite, asserted to fit v0 since pump-final's 10-key TradeExtras (on the v2 list's 20 the tipped liquidation did not); the synthetic exits past `rT` (a client close, the keeper's tipped `[crank_price, expire]`) executed through the tables (`-- --nocapture` prints the table) |
| `--test latch_pump_it` | M5, T-LATCH on the dumped pump / pump-fees / PumpSwap ProgramData: real loader upgrades, opens refused on curve markets while cranks, top-ups, `sol_settle`, `sweep_trader`, closes, liquidations and expiries keep working; revalidation only at the loader's slot; all three bits must clear; U5 on the 10 MiB accounts; the loader upgrade from pump's 2026-10-02 builds to the current ones replayed with positions open (since pump-final a synthetic exit among the exits; the 2026-09 builds have no v3 trades); AIR-01's pump-fees extension |
| `--test m5_sec_curve_it` | the M5 security review's findings on the curve as regressions (release build, pump fork; shares `pump_curve_support.rs`; the audit's reproducers, converted): M5-SEC-1 (a full wipe on a live curve reopens once `sweep_trader` has set the venue-reward bit, the trader's pump UVA holding no claimable cashback; with a claimable balance `[pump claim_cashback(user = trader), sweep_trader]` credits the wiped epoch — T-CASHBACK's pump part, since pump-final P2 on a non-cashback curve with lamports seeded into the pump UVA, T-WIPE-GATE: v3 refuses live cashback curves), M5-SEC-3 (a sale wrapped around the open no longer lifts the Tier D guard or `X₀`: the curve caps read the reserves at `P_ema`), M5-SEC-4 **inverted by M6** (`m5_sec_4_m6_can_enable_the_curve_venue`; `m5_sec_4_an_atomic_complete_and_migrate_graduates_the_market_onto_its_pool`: the atomic `[buy_v2(rT), migrate_v2]` with the pool pushed ≥ 3.5× the last curve EMA moves the market onto the pool, which liquidates only after the 8-integration warm-up), M5-SEC-5 (an unreadable canonical pool keeps the market `PumpMigrating` and fails Raydium adoptions closed; an empty address is graduated elsewhere), M5-SEC-6 (a low-bump market costs short.win no extra CU) |
| `--test pamm_it` | M6, the PumpSwap adapter on the fork (release build, the dumped pAMM / pump / pump-fees programs and the `pamm_state` pools): `listing` (a PumpSwap-class coin lists on its canonical pool with the pAMM init set — Token-2022 301 B, SPL 261 B, 211 B after `extend_account` —; the first observation starts the price marked for the warm-up; the init set key-checked; `None → PumpSwap` re-detection), `graduation` (completion → `PumpMigrating` → a real `migrate_v2` → `PumpSwap`: the cache, `graduated_at`, the ring at `lp_supply`, `(8, 1)`, exits and opens refused `EmaWarmingUp` until 8 integrations, the borrower's close on the pool), `flows` (open → close to the lamport on every pool role, expiry and liquidation after a ×2.8 pump, `sol_settle` no-op and S1 after a held ×10), `refusals` (the warm-up, every `disable_flags` bit, the boost gate, the pAMM / pump-fees latch, the existence requirement, every trade slot substituted), `gates` (T-DISABLE on the ELF, T-ATA / T-PDA-USER, T-RENT, T-CASHBACK pAMM part), `negvqr` (the synthetic pool prices on `vault + VQR`; the tier inputs), `creator` (M6-SEC-4: a stranger's `set_coin_creator` on the dumped ELF costs an exit one retry — `VenueAccountMismatch` until the new creator vault's WSOL ATA exists — and `crank_price` re-caches the creator), `lock` (★T-PAMM-LOCK, the owner's M6R-1 decision of 2026-09-30 and OWNER-M6D-1, ARCH rows 56–57, ECON #105–#106; ARCH §8.2, `attacks/pumpswap_lp.py` parts 2–3; 12 of `pamm_it`'s 41 M6 tests: U33 — `pool_d` is the burned LP's depth `D` on every pool role —, the LP mint P6 key-checked, a deposit and its withdrawal moving neither `D` nor the bounds, a deposit wrapped around the open inflating nothing (3 (c)), `part_3b_a_held_deposit_lifts_no_open_the_burned_lp_sizes_it` — a 3× deposit held 1 h: the next open's cap 1.0000× the honest one at `pool_lock_min_bps` 5,000 and 2,000 (2.000× / 2.842× as built at ARCH row 56) —, a held deposit pulled at a pump liquidating a position sized before it with the control (3 (a), +88 s, `L_b` the drained pool's depth), `[migrate_v2, deposit, crank_price]` capped at `D` by the first PumpSwap observation (part 2), FITS-2 (a) and FITX-1 on a pool with third-party LP (3 (d)); **M6D-1's pin, as decided** `part_3e_a_held_deposit_sizes_nothing_and_the_pulled_pump_is_liquidated` — the held cap 1.0000×, the position at 95 % of it liquidated at +59 s when the deposit is pulled at a genuine 5×/120-s pump (the control +61 s), the lenders `Q` back and 0.0632 SOL, +4.20 % of `Q`'s value, the same second at `pool_lock_min_bps` 2,000 (row 56: S1 at +124 s, −37.9 %); **M6E-1's pin** `part_3f_a_deposit_pulled_at_the_deadline_does_not_hold_up_the_expiry` — a 3× / 2× deposit held for the last ≈ 62 min before a position's deadline and pulled at it (3× also 10 min before): expired at +1 s like the control, `L_b = L_live`, while the reference at the cap `2D` would have refused the executed buyback (the `2D` build: +3,347 / +1,910 / +2,536 s); and `capacity_on_the_canonical_pool_fixtures` — the wholly burned pools lose nothing, e/acc's (λ 92.98 %) −0.12 % after the warm-up and −6.13 % after 2 h; ECON §12.12 (12) (e), (f)); ★M10: `buckets` (the 2026-10-02 redeploy's fee buckets: v2 trades, the sweeps, v1 sales paid from `vault − buckets`; since program batch 4, PUMP-2026-10-1, the program reads the payable SOL and the boost — the open's cap to the lamport, keeper exits, a negative-VQR pool with buckets, buckets above the vault not executable) |
| `--test graduation_scenarios`, `--test graduation_e2e` | M6 fork E2E through a real graduation (shared `graduation_support.rs`, no tests of its own, which includes `pump_curve_support.rs`; ≈ 19 / 14 s debug): ECON §10.1's twelve graduation rows through completion, `migrate_v2`, the `→ PumpSwap` transition, pump's real `boost_buy_and_burn` on the sim's schedule and PumpSwap ramps, to the lamport against the sim with M6's graduation rule (the module docs list the three differences from ECON's published table; since #105 the normative sim runs M6's graduation rule and the ELF's boost rounding, so only `R_min` at the pre-open 0 % remains — ECON §10.1's as-built column, §11.2); ★T-SEED-FALL (M6-SEC-1: on D@80 % a 0.25× low recorded in post-change second 8 / 4 / 1 delays the liquidation to +14 / +22 / +28 s against the honest +12 s, as the sim; a genuine dump to 0.25× forces nothing on D@80 % or D@97 %; `graduation_e2e::m6_sec_1_*`, the review's reproducer: a 0.1× low in second 1 / 4 / 8 after the atomic attack, the seed at the gate exactly the fall bound, liquidated at +23 / +17 / +12 s against +9 s); a lifecycle through a real graduation, T-VENUE-SWITCH (graduation), the M5-SEC-4 atomic attack, ECON §10.2's straddle close (since pump-final the borrower's own close completes the curve by synthetic migration at `B̂_v3`, equal to the sim's synthetic close to the lamport; the bundle, and the keeper's bundle for a self-made straddle, are retired), a completing close, the boost gate (`-- --nocapture` prints ECON-format rows); M10: `structure` and `wsol` (I14 on all six instructions, G-B on PumpSwap), `admin` (venue admin with a position open: `FeeConfig` hikes, mayhem, a layout change, the Emergency flag; since batch 3 the pool, its vaults and the borrower refused as extras), `hooks` (the I15 / I18 mutants); 67 tests since batch 3 |
| `--test pumpswap_negvqr_e2e` | M6, a lifecycle on the **synthetic** negative-VQR pool (`pamm_synthetic`): opens, a close, liquidations after a ×3 pump, an expiry, S1 after a ×10 gap, the lenders' exit; every quote on `vault + VQR`, the tier input `min(vault, vault + VQR)` |
| `--test graduation_surfpool` | M6, the Surfpool E2E through a real graduation on **live mainnet** (`#[ignore]`d; needs Surfpool 1.6 and network: `scripts/surfpool-e2e.sh`, below) |
| `--test trace_pumpswap_it` | M6, T-TRACE on PumpSwap: the exact call shape and entries of every PumpSwap-path instruction (open 22–23, normalising 26; close 16–17; liquidate / expire 17; crank 2; the `→ PumpSwap` transition 4; the listing 15; `sweep_trader`'s token leg 4), peak height 3 |
| `--test tx_size_pumpswap_it` | M6, T-SIZE on PumpSwap: every transaction in its worst case (the largest extras set, a separate fee payer, a first-time borrower) as v0 through both tables, executed that way; the listing; the transition and tx3; since M6R-1's LP mint (+33 B per PumpSwap ReadSet) 3 cranks per v0 batch (the 4-crank batch pinned over 1,232 B), and the open with the largest (4-entry) extras set and the keeper's `[crank, liquidate]` with it pinned over 1,232 B through A28 — fitting through the build-alt table, the open executed through A28 plus a per-market table (the owner trade-off of ARCH §6.4) (`-- --nocapture` prints the table) |
| `--test latch_pumpswap_it` | M6, T-LATCH on the dumped pAMM / pump-fees ProgramData: a real loader upgrade refuses PumpSwap opens while exits, cranks, `sweep_trader` and `sol_settle` keep working; the PumpSwap mask is pAMM + pump-fees only; U5 |
| `--test replay_0428_it` | M6, T-REPLAY-0428 on the test-venue build (`SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`): pump's 2026-04-28 added-account change, modelled on the mock's `required_extras` knob (the deployed ELFs only accept trailing accounts), recovered through the extras sets (`current`, `previous`, a pinned `stable`, `none`) |
| `--test coverage_pause_it`, `--test tier_transition_it`, `--test caps_boundary_it`, `--test launch_guard_it`, `--test m7_sec_it` | M7: T-COVERAGE's `set_coverage_pause` part (O17) and the Emergency's clear, T-TIER, T-CAP, the launch guard (`market_max_notional`, `min_notional`) and the M7 security review's program findings (details in `tests/litesvm/README.md`) |
| `--test squads_gov_it` | M10 (WP-7), T-SQ-1..9 on the mainnet Squads v4 ELF in the owner's layout (`Env::squads_governed`): the double timelock (Squads 172,800 s, then the program's `timelock_s`; 96 h end to end), every Emergency power in ≤ 2 transactions (the worst-case `set_venue_extras` through the transaction buffer), the Emergency's refusals, the checked upgrade-authority transfer and an upgrade through the vault, the disclosed Squads semantics (an `Approved` transaction never expires), sizes and CU, I4 on the vault path (`-- --nocapture` prints sizes and CU) |

Every instruction goes through `Env::send`, which runs the invariant hooks — I1, I2, I6, I7,
I8, I13 (state part) on every market a transaction names or creates; I3, I4, I5, I7, I9–I13,
I15 (M4: listings, adoptions, windows, Raydium opens on locked depth), I16, I17 (M4: also
adoptions and window candidates while the candidate's own Raydium program is flagged), A26
(snapshot rules) and the lender transaction bounds against a pre-instruction snapshot — after
the transaction and, for a transaction of several instructions, after each instruction
(prefixes replayed on a clone of the VM). I14 is structural (account lists). A violation panics
with the logs. Only a test that plants an impossible state on purpose (the hooks' own mutant
tests, a copied account, an overflowed field) turns them off. Details and conventions:
[`tests/litesvm/README.md`](../tests/litesvm/README.md).

The quick run replays a subset of the `pool_model` traces. All of them (the 40 stranding seeds,
the lock-heavy mix, both ECON §9.6 fuzz runs; about 4 minutes):

```sh
SHORTWIN_TRACES=full cargo test --release -p shortwin-litesvm-tests --test lender_it traces::
```

### The Raydium mainnet fork (M3) and its fixtures

The Raydium binaries run short.win against the **real** Raydium programs, dumped from mainnet
with the pools, vaults, mints, LP mints, observations and configs they trade on
(`tests/fixtures/`; the harness module `tests/litesvm/src/fork.rs`, markets in `src/raydium.rs`).
Each program is deployed from its dumped ProgramData (the real deploy slot and upgrade authority,
for T-LATCH), rebuilt to the mainnet account's sha256; every fixture file is checked against
`manifest.json` when it is loaded. The state accounts are one `getMultipleAccounts` snapshot
(slot 451,187,550, 2026-09-28), so pools, vaults and fee counters agree with each other.

| Fixture | What |
|---|---|
| `programs/raydium_amm_v4.so`, `programs/raydium_cpmm.so` (group `raydium_programs`) | the deployed ELFs (deploy slots 445,763,204 and 445,763,504). Every AMM v4 test loads the first; the CPMM build is T-LATCH's "before" since M10 (`fork::CpmmDeploy::Sep2026`) |
| `accounts/raydium/*.bin` | both program accounts and ProgramData headers; AMM v4: Fartcoin `Bzc9NZfM…` and its reversed-order sibling `9GWMrDgH…`, the `AmmConfig` and authority; CPMM: USELESS `Q2sPHPdU…`, the creator-fee pools `FQd8z3…` (WSOL token 0), `3Cv9uPpa…` (WSOL token 1) and `FrZnJAFt…` (both tokens), the Token-2022 pool `7ZRtFcgf…`, each with vaults, mints, LP mint and observation; the authority and all 21 `AmmConfig`s |
| `programs/pump.so`, `programs/pump_fees.so`, `programs/pump_amm.so` (M5, group `pump_programs`, slot 451,672,911) | the deployed pump, pump-fees and PumpSwap ELFs (deploy slots 449,734,335 / 446,465,969 / 446,462,733, `VENUE_GUARD_SLOTS_2026_09_27`) |
| `accounts/pump/*.bin` (M5, group `pump_state`: one `getMultipleAccounts` at slot 451,675,432, 100 entries — the call's cap —, 13 recorded `"absent": true`) | the three program accounts and ProgramData headers; pump's `Global`, both pump-fees `FeeConfig`s, PumpSwap's `GlobalConfig`, both volume accumulators, the fee / buyback / withdraw authorities, WSOL, USDC, PumpSwap's protocol-fee and buyback WSOL ATAs, the Rent sysvar; ten pinned coin **roles**: `fresh` (125 B), `near` (151 B, near completion), `t22_115`, `legacy115` (SPL Token, no creator), `legacy49` (2024 layout), `graduated` (canonical 301-B pool and boost vault), `mayhem`, `usdc`, `legacy150`, `fartcoin` (graduated elsewhere) |
| `accounts/pamm/*.bin` (M6, group `pamm_state`: slot 451,784,607, 76 entries, 18 absent) | PumpSwap's `GlobalConfig`, `FeeConfig`, global volume accumulator, WSOL, Rent, the 16 recipient WSOL ATAs; per pool **role** the pool, mint, vaults, LP mint, coin-creator vault authority and ATA, boost vault and ATA, pool authority and migrated curve: `fresh` (301 B, boost vault 13.98 SOL), `mature` (boost spent, 7 % third-party LP), `cashback`, `legacy211`, `legacy261` |
| `accounts/pamm/pamm_mature_pool_negative_vqr.bin` (M6, group `pamm_synthetic`) | **synthetic**: the `mature` pool with VQR = −⌊vault/2⌋ (no negative-VQR pool exists on mainnet yet), at the same address — load one or the other; `check` verifies it equals its source but for that patch |
| `programs/raydium_cpmm_2026_10.so`, `accounts/raydium_cpmm_2026_10/*.bin` (M10, group `raydium_cpmm_2026_10`) | CPMM as **redeployed at 452,376,460** (trimmed ELF `93cad245…a0d2`; `raydium-cp-swap@bdeee23`, `feat/creator-fee-share`, rebuilt byte for byte in solana-verify's 3.1.10 image, `tests/fixtures/README.md`; the manifest's pin text still says UNVERIFIED). The ELF only: the pools and configs stay `raydium_state`'s (layouts unchanged; the 21 live configs equal the fixture bytes at slot 452,488,420). It shares its two addresses with `raydium_programs`' CPMM entries, so the harness loads a build by name: `fork::CpmmDeploy::CURRENT` (= `Oct2026`), which every CPMM test, bench and the web E2E validator run, or `Sep2026` (T-LATCH's old → new upgrade). `fork::FIXTURE_GUARD_SLOTS` is the guard slot set the harness records (`VENUE_GUARD_SLOTS_2026_09_27` with CPMM at 452,376,460) |
| `programs/pump{,_fees,_amm}_2026_10.so`, `accounts/pump_2026_10/*` (M10, group `pump_programs_2026_10`) | pump, pump-fees and PumpSwap **as redeployed on 2026-10-02** (deploy slots 452,654,932 / 452,655,002 / 452,654,882; trimmed ELFs `b0b777c6…`, `f166c296…`, `01557035…`; the `pump-rust-client` 0.2.0 release, `tests/fixtures/README.md`'s interface diff). The ELFs only: the state stays `pump_state`'s and `pamm_state`'s (the redeploy only appends fee buckets, `BondingCurve` @125/@133, `Pool` @271/@279). T-LATCH's "before" since the 2026-10-13 re-pin; `VENUE_GUARD_SLOTS_2026_10_03` was its baseline |
| `programs/pump_2026_10_08.so`, `programs/pump_amm_2026_10_08.so`, `programs/pump_fees_2026_10_12.so`, `accounts/pump_2026_10_13/*_program*.bin` (pump-final P0, group `pump_programs_2026_10_13`) | pump and PumpSwap **as redeployed on 2026-10-08** (454,596,459 / 454,596,406) and pump-fees **as redeployed on 2026-10-12** (456,132,171; trimmed ELFs `6956c2f8…`, `3ffd5e3b…`, `8ad2ca93…`): v3 trades with synthetic migration past `rT`, multi-hop, the 166-byte curve. Every curve and PumpSwap test loads its PumpSwap and pump-fees builds; its pump build, superseded on mainnet the same week, is kept (never refreshed) as G0b's differential's other side |
| `programs/pump_2026_10_13.so`, `accounts/pump_2026_10_13b/*` (pump-final P0b, group `pump_programs_2026_10_13b`) | pump **as redeployed on 2026-10-13** (456,494,698; trimmed ELF `e2020969…52be`; `create_v2`'s quote chain only, every path short.win uses unchanged — gate G0b, `tests/fixtures/README.md`). Every curve test loads it (`fork::PumpDeploy::CURRENT`); `VENUE_GUARD_SLOTS_2026_10_13` is the baseline |
| `accounts/pump_2026_10_13/*` (pump-final P0, group `pump_state_2026_10_13`, slot 456,395,752) | the state those builds introduced: pump's 1,088-byte `Global` (`max_curve_depth` @1087), the `FeeConfig`s and `GlobalConfig`, and the coin roles of the 166-byte curve: `near166`, `fresh166`, `buckets166`, `grow141`, `grow151`, `synthetic_stuck`, `graduated_synthetic` (98u8RJ and its pool), `cashback166`, `mayhem166`, `wsol_pool_buckets`. An address both state groups hold resolves to this group |
| `shortwin/shortwin_1_0_0.so` (pump-final P3; `SHA256SUMS` pins it, `dump-fixtures.py` does not manage it) | short.win's own release 1.0.0, built from the `v1.0.0` tag with the pinned toolchain; it equals AUDIT §0's verified 1.0.0 hashes. T-SELF-UPGRADE (`upgrade_it`) and the keeper's `upgrade_window` scenarios upgrade it to the current build |
| `programs/token_2022.so`, `accounts/spl/*.bin`, `programs/spl_memo_v3.so` (M10, group `spl_programs`) | the deployed Token-2022 (upgradeable, deploy 427,147,035, authority `AeLmXCbP…`, trimmed ELF `7ea94a02…`: OtterSec's verified build of `solana-program/token-2022@9bc02757`) and spl-memo v3 (BPFLoader2: the account data is the ELF, sha256 `f520eaf0…`); every `Env` loads them (above) |
| `programs/squads_v4.so`, `accounts/squads/*.bin`, `idl/squads_multisig_program.json` (M10, group `squads_programs`) | Squads v4: immutable since 302,582,236, trimmed ELF `d4866083…`, source UNVERIFIED (probably `Squads-Protocol/v4@64af733`); its `ProgramConfig` (creation fee 0, treasury `5DH2e3…`) and the IDL 2.1.0 vendored at `64af733` (sha256 `cb9a0a29…`, the snapshot's `vendored` list), which `crates/squads-v4-ix` is pinned against |
| `manifest.json` | per account: address, owner, lamports, executable, rent epoch, space, slot, sha256, purpose, decoded fields; one snapshot record per group (`vendored`: files taken from a pinned source) |
| `SHA256SUMS` | every fixture file |

`scripts/dump-fixtures.py` (Python 3.14, standard library only) writes and checks them:

```sh
python3 scripts/dump-fixtures.py check           # offline (also the default): every file against the
                                                 #   manifest, the ProgramData rebuilds, cross-references
(cd tests/fixtures && sha256sum -c SHA256SUMS)
python3 scripts/dump-fixtures.py fetch           # fetch only what is missing or invalid (idempotent)
python3 scripts/dump-fixtures.py fetch --refresh # re-dump everything at a new slot
python3 scripts/dump-fixtures.py fetch --rpc URL # default: api.mainnet-beta, then publicnode
python3 scripts/dump-fixtures.py fetch --refresh --group pump_state --coin graduated=MINT
                                                 # re-dump one group (repeatable), a pump coin role
                                                 #   replaced (its expectation is checked at fetch)
python3 scripts/dump-fixtures.py candidates      # live mints that fit each pump coin role
python3 scripts/dump-fixtures.py fetch --group pamm_state --pool negative_vqr=MINT
                                                 # M6: add a PumpSwap pool role (e.g. a real
                                                 #   negative-VQR pool once one exists)
python3 scripts/dump-fixtures.py check --live    # M10, mainnet, read-only: exit 1 naming each redeployed
                                                 #   program (vs its newest fixture and, for the six
                                                 #   VenueGuard programs, the newest
                                                 #   VENUE_GUARD_SLOTS_YYYY_MM_DD of constants.rs)
```

**A venue redeploy (M10)** is dumped as a **new** program group, as `raydium_cpmm_2026_10` was.
Never refresh the old group: T-LATCH keeps loading it. Then pin the new group in
`dump-fixtures.py` and move the test baseline deliberately (a new
`VENUE_GUARD_SLOTS_YYYY_MM_DD`; the program never reads it, `initialize_config` takes the live
slots). A fetch that reads anything other than a pin stops before it writes a file: the script
stages every file and writes them, `manifest.json` and `SHA256SUMS` only once every group it
fetched has passed its checks. `check --live` (about ten paced calls) holds every program to its
newest group and lists the older ones as "kept"; Jupiter has no fixture and is held to the
constant only. Run it before every freeze and merge. On 2026-10-03 (slot 452,907,481) it reported
pump, PumpSwap and pump-fees redeployed (deploy slots 452,654,932 / 452,654,882 / 452,655,002,
2026-10-02 ≈ 15:47Z); the M10 pump revalidation dumped them as `pump_programs_2026_10` and moved
the baseline to `VENUE_GUARD_SLOTS_2026_10_03`, and since then **`check --live` is clean**
(slot 453,037,762 in program batch 4: every pinned program and account matches mainnet). Every
curve and PumpSwap test, bench and fuzz profile runs the redeploys (`fork::PumpDeploy::CURRENT`);
`pump_programs` stays as T-LATCH's "before" (`latch_pump_it` / `latch_pumpswap_it`
`::the_2026_10_*` replay the real loader upgrades). *pump-final (2026-10-13 … 16):* it found pump
and PumpSwap redeployed on 2026-10-08, pump-fees on 2026-10-12, Jupiter at 454,465,850 and then
pump once more on 2026-10-13; they are `pump_programs_2026_10_13`, `pump_programs_2026_10_13b`
and `pump_state_2026_10_13` (above), the baseline is `VENUE_GUARD_SLOTS_2026_10_13`, and `check
--live` is clean again (slot 457,523,244 on 2026-10-16, the 1.0.1 release evidence, AUDIT.md
"Addendum: pump v3").

A `--refresh` re-pins every byte: the reader unit tests (`crates/shortwin-venue-state/src/
raydium_{v4,cpmm}.rs`, `pump.rs`, `pump_amm.rs`) assert decoded values of this dump (vault
amounts, counters, λ, curve reserves) and must be updated from the manifest's new `decoded`
fields, and figures printed by the E2E and bench tests move with them. The `graduated` role's boost
vault closes on mainnet within the hour after its migration, so a `pump_state` refresh needs a
fresh `--coin graduated=MINT` (from `candidates`); `pump_state` is at the 100-account
`getMultipleAccounts` cap, so a new static needs a second call and a slot-equality rule. Scenario builders (`fork::set_*`, byte-level admin setters,
`set_deploy_slot`) change the fork only in ways the tests name; the real admin instructions run
through `fork::admin_send` / `raydium_admin_send` (the admin's signature forged for one
transaction, the invariant hooks included). `Env::fork_raydium_v4` / `fork_cpmm` start from the
release build; a test-venue `Env` calls `load_raydium_programs`, `load_ray_v4` / `load_cpmm` and
`set_fork_clock` itself (not `set_all_venue_program_data`, which would overwrite the real
ProgramData headers).

**The pump fork (M5).** `Env::fork_pump_coin(PumpCoin::…)` (`tests/litesvm/src/fork.rs`: the pump
groups load lazily, apart from the Raydium fixtures) deploys the three pump programs from their
dumped ProgramData, loads the statics and one coin role, installs mainnet's Rent sysvar (5,080
lamports per byte: pump's venue-side rent is mainnet's, not LiteSVM's default) and the snapshot's
clock. `fork::PumpCurve` gives the readers' state, the live fees, the venue's own charge and
short.win's quotes, and the IDL-exact `buy_v2` / `sell_v2` / `buy_exact_quote_in_v2`;
`tests/litesvm/src/pump.rs` lists markets on it (`Env::pump_market`, `PumpMarket`), trades the
curve as a normal user (`CurveUser`) and builds the graduation path on the real ELFs
(`migrate_v2_ix`, `NewPool`, PumpSwap `extend_account` / `buy`). Venue-side scenario builders (`fork::set_curve_reserves`,
`set_curve_complete`, `set_curve_creator`, `set_curve_creator_fee_bps`,
`set_global_creator_fee_configurable`, `set_fee_config`, `set_pump_deploy_slot`) change only
pump's accounts.

## The invariant fuzzer (M7; `tests/fuzz`)

`tests/fuzz` (package `shortwin-fuzz`, a member of the root workspace, ARCHITECTURE §8.4) runs
seeded **action sequences** on LiteSVM through the program-test harness, so every transaction
runs `Env::send`'s I1–I13, I15–I19 and A26 hooks, plus the fuzzer's own oracles (the I10 / I17
gates, the mock's differential exit oracle, I14 omissions, I4's coverage clause on the whole VM,
governance and oracle rules, and the end-of-run liveness wind-down; since M10 also the
differential exit oracles on AMM v4 and CPMM, the curve's and PumpSwap's decision-level exit
predictors, the lender oracles — `claim_sol ×N`, the JIT capture bound, `[withdraw, sol_settle]`
within the allowance —, the deficit tripwire and the second claim pass; `tests/fuzz/README.md`
"Oracles"). It loads the same builds as the program tests. Build it without the deploy profile's fat LTO:

```sh
anchor build --tools-version v1.57 --arch v3 --ignore-keys && scripts/build-test-venue.sh
export CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
cargo test -p shortwin-fuzz --release -- --nocapture       # the CI smoke: fixed seeds on every
                                                           # profile, corpus replays, self-tests
cargo run -p shortwin-fuzz --release -- run --profile all --duration 10m --jobs 8 --out fuzz-out --minimize
cargo run -p shortwin-fuzz --release -- one --profile curve --seed 42 --actions 400   # one run, full summary
                                     # (M10: exactly the run `run --actions 400` draws for that seed:
                                     #  N/4 + splitmix64(S ^ 0x1e57) % (3N/4 + 1) actions; `--length N`
                                     #  runs exactly N; anomaly cursors read `profile seed #i/n`)
cargo run -p shortwin-fuzz --release -- replay fuzz-out/crash-<profile>-<seed>-<sig>.json
cargo run -p shortwin-fuzz --release -- tmin   fuzz-out/crash-<profile>-<seed>-<sig>.json  # -> min-….json
```

Profiles: `mock`, `mixed` (test-venue build), `curve`, `pumpswap`, `amm-v4`, `cpmm` (release
build, mainnet forks), or `all`. Each job owns one seed stream; the action list is a pure function
of `(profile, seed)`, so `replay` and `tmin` are deterministic. `fuzz-out/` (gitignored) receives
crash files (profile, seed, action list, signature, logs) and `summary.json` (counts and the
coverage proxies: instructions landed, events, error codes returned). A minimised reproducer of a
finding goes into `tests/fuzz/corpus/`; mark it `"status": "fixed"` once fixed and `corpus_replays`
keeps it as a regression. **The M7 exit criterion "fuzz clean ≥ 24 h"** (ARCHITECTURE §9) is a
`run --profile all --duration 24h --jobs 8 --actions 400 --minimize` with `CARGO_TARGET_DIR`
outside the repository (≈ 270–310 actions/s on 5 jobs), or nights of CI's `fuzz-nightly`, which
since 2026-10-02 runs only when the repository variable `FUZZ_NIGHTLY` is `true` (below, "CI").
It is **met** by the second local 24 h run (2026-10-02/03, at `4cb7729`, seed 20261002, 4 jobs,
86,402 s: 72,149 runs, 13,821,419 actions, 0 failures, 0 anomalies); the first (at `eb82a2a`) found
four harness failures and one anomaly, all fixed (FUZZ-M7-24H-1, -2, -3). For the M10 tree of then
it is met by run 4 (2026-10-05/06, at `945d45a` with the program bytes of `4f4849e`, release
`bbef1075…`; seed 20261005, 4 jobs, 86,404.9 s: 52,274 runs, 10,163,360 actions, 0 failures,
0 anomalies); run 3 (at `4f4849e`, seed 20261004) failed only the fuzzer's own liveness oracles,
both fixed (FUZZ-M10-24H-A, -B). On the final program bytes (release `4d19c582…`, the KI-66 full /
KI-67 batch's) it is met by run E (2026-10-09/10, at `9861c5a`, seed 75000000, 4 jobs, 86,415 s:
44,988 runs, 8,773,451 actions, 0 failures, 0 anomalies); run C (at `f48e75d`) was clean too, and
run D's two findings (at `55b3e23`: FUZZ-24D-A, FUZZ-M10-24H-D-B) were the fuzzer's, fixed in
`9861c5a`. Seeds are `base + job + k·jobs`, so runs 2 and 4 re-ran runs 1's and 3's streams
shifted by one: confirmations, not new coverage. A run meant to add coverage takes a widely spaced
`--seed` (as runs C, D and E do). Details: [`tests/fuzz/README.md`](../tests/fuzz/README.md).

**Mutation testing** (M7-SEC-6): `scripts/mutants.sh [--fuzz 3m] [--work DIR] [mutant …]` builds the
tests and the fuzzer once from the checkout, then each one-line program mutant — the M7 security
review's seven, and since M10 twelve more (KI-65, KI-66, and the nine of KI-66 full and KI-67:
the forward chain accruing nothing, the owner's double count inside a chain, the held epoch accrued
twice, a per-touch floor, a re-entered hold, a jump keeping the chain, the park on a live wipe
record keeping the chain (`ki66full-park-keeps-chain`, the KI-66 full review), a close with
`pending` owed and a parked lender closed) — (the test-venue or the release build of a private copy), and requires
the mutant's named tests and, for the M7 ones, a short fuzz run of its profile to fail against it;
the M10 mutants are checked by their tests alone (`-`) — a surviving mutant is a missing oracle. It needs the unmutated builds the fork tests load (`SHORTWIN_SO`, `SHORTWIN_TEST_VENUE_SO`,
`MOCK_CPAMM_SO`, default `target/…`); `--work` outside the repository keeps
the per-mutant builds out of `target/`.

## Benches

Six standalone Cargo workspaces with their own lockfiles keep Mollusk 0.15.1 out of the
on-chain lockfile (all hold the Agave runtime line at 4.2.2, C1-14):

```sh
# T-CU-MATH (M0b): per-primitive CU of shortwin-math, budget = ARCH §6.1 + 20 %
cargo-build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path tests/bench/cu_math/program/Cargo.toml --sbf-out-dir tests/bench/cu_math/target/deploy
cargo test --manifest-path tests/bench/cu_math/Cargo.toml --release      # CU_MATH_WRITE=1 rewrites results.md

# M1 CU baseline: every M1 instruction on Mollusk, cross-checked on LiteSVM, within its budget
anchor build --tools-version v1.57 --arch v3
cargo test --manifest-path tests/bench/m1_cu/Cargo.toml -- --nocapture   # M1_CU_WRITE=1 rewrites results.md

# M2 CU bench and T-TRACE: every M2 instruction on the test-venue build against the mock
scripts/build-test-venue.sh
cargo test --manifest-path tests/bench/m2_cu/Cargo.toml -- --nocapture
# M2_CU_WRITE=1 rewrites results.md, M2_TRACE_WRITE=1 rewrites trace.md

# M3 CU bench, T-TRACE and stack depth on Raydium: the release build on the dumped AMM v4 / CPMM
# programs (Mollusk, cross-checked on LiteSVM), the worst-case Raydium transactions, and every
# path through 0–3 levels of the TEST-ONLY CPI wrapper (tests/bench/m3_cu/wrapper, never deployed);
# since M4 also propose_pool / execute_pool_switch (every writing outcome, ARCH §6.1's 100 k)
anchor build --tools-version v1.57 --arch v3
cargo-build-sbf --tools-version v1.57 --arch v3 --manifest-path tests/bench/m3_cu/wrapper/Cargo.toml \
  --sbf-out-dir tests/bench/m3_cu/target/deploy -- --locked       # the wrapper (t_depth only)
cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --locked -- --nocapture
# M3_CU_WRITE=1 rewrites results.md, M3_TRACE_WRITE=1 trace.md, M3_DEPTH_WRITE=1 depth.md;
# SHORTWIN_SO / M3_WRAPPER_SO override the ELFs

# M5 CU bench, T-TRACE, the T-SIZE preview and stack depth on the pump curve: the release build
# on the dumped pump / pump-fees / PumpSwap programs and coins (Mollusk, cross-checked on
# LiteSVM), the worst-case curve transactions with their v0 + ALT sizes, the synthetic exits past
# rT and the keeper's migrate_v2 after them and after an exact completion (the straddle bundle's
# tx1 rows left with the complete_curve helper, D2), every path through 0–3 levels of its own
# TEST-ONLY CPI wrapper
anchor build --tools-version v1.57 --arch v3
cargo-build-sbf --tools-version v1.57 --arch v3 --manifest-path tests/bench/m5_cu/wrapper/Cargo.toml \
  --sbf-out-dir tests/bench/m5_cu/target/deploy -- --locked       # the wrapper (t_depth)
cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --locked -- --nocapture
# M5_CU_WRITE=1 rewrites results.md, M5_TRACE_WRITE=1 trace.md, M5_DEPTH_WRITE=1 depth.md;
# SHORTWIN_SO / M5_WRAPPER_SO override the ELFs
# (since M6 its graduated coin lists on PumpSwap with the pAMM init set; Fartcoin stays list-only)

# M6 CU bench, T-TRACE, T-SIZE, T-DEPTH and the graduation transition on PumpSwap: the release
# build on the dumped pAMM / pump / pump-fees programs and the pamm_state pools (Mollusk,
# cross-checked on LiteSVM), the worst-case PumpSwap transactions with their v0 + ALT sizes (the
# transition crank, the straddle's tx3, the post-wipe claim bundle), every path through 0–3
# levels of its own TEST-ONLY CPI wrapper
anchor build --tools-version v1.57 --arch v3
cargo-build-sbf --tools-version v1.57 --arch v3 --manifest-path tests/bench/m6_cu/wrapper/Cargo.toml \
  --sbf-out-dir tests/bench/m6_cu/target/deploy -- --locked       # the wrapper (t_depth)
cargo test --manifest-path tests/bench/m6_cu/Cargo.toml --locked -- --nocapture
# M6_CU_WRITE=1 rewrites results.md, M6_TRACE_WRITE=1 trace.md, M6_DEPTH_WRITE=1 depth.md;
# SHORTWIN_SO / M6_WRAPPER_SO override the ELFs
```

The M7 keeper benches are output directories, not workspaces: the real keeper measured by the
services workspace's scenario tests, in release (`#[ignore]`d in the default suite):

```sh
anchor build --tools-version v1.57 --arch v3 --ignore-keys && scripts/build-test-venue.sh
# ECON §5.8's cost-model inputs (CU per instruction and venue, crank batches, cadence, cost per
# market-day, load at 50 / 200 markets) -> tests/bench/m7_keeper/results.{md,json}; the PumpSwap
# 4-extras T-SIZE through the keeper's tables (OWNER-M7-1) -> tests/bench/m7_keeper/tsize_pumpswap.*;
# T-CASCADE under the keeper's CU-per-slot policy -> tests/bench/m7_cascade/results.{md,json}
M7_COST_WRITE=1 M7_CASCADE_WRITE=1 cargo test --manifest-path services/Cargo.toml --release --locked \
  -p shortwin-keeper-litesvm --test scenarios -- --include-ignored --nocapture m7_cost:: pamm_alt:: cascade::
python3 docs/design/sim/keeper_cost.py > docs/design/sim/out/keeper_cost.txt   # ECON §5.8's tables (and
                                                  # the global budget's, #109); asserts its constants
                                                  # equal m7_keeper/results.json
```

`results.json` records the SHA-256 of the two program builds it measured; re-run after a program
change (the load section's wall times and RPC capacity vary run to run). See
[`tests/bench/m7_keeper/README.md`](../tests/bench/m7_keeper/README.md) and
[`tests/bench/m7_cascade/README.md`](../tests/bench/m7_cascade/README.md).

(The `cu_math` bench program and the `m3_cu` / `m5_cu` / `m6_cu` wrappers are separate crates, so their
build commands name their manifests; the no-bare-`cargo build-sbf` rule is about the short.win
program.) The figures are in each bench's `results.md` (and `trace.md`, `depth.md`); commit them
when they change. The M3, M5 and M6 benches use fixed borrower, keeper and attacker keys (M3 also
draws the keys of its planted M4 pools in a fixed order), so their reports are byte-identical
across runs (the M1 and M2 benches use random keys: their PDA bump searches move the figures by
≈ 1.5 k CU per iteration from run to run). **Not every row is deterministic (M10).** Re-runs on the
same ELFs move: the M1 / M2 rows that create random mints or keypairs (`create_market`, a first
`deposit`, `open_short` and the M2 bundles, with their trace rows) in multiples of 1,500 CU; M6's
graduation-path `liquidate` by a few tens of CU (+19 on M10's integrated tree, its random keys);
and the M7 cascade's sums and the load section's wall times. Compare CU across builds net of
that, and commit a re-baseline only with the ELF hashes it ran on. Since M10 the M1–M6 benches
(they take their ELFs from the program-test `Env`) and the keeper benches run mainnet's
Token-2022 (+15 CU per Token-2022 CPI; above). See
[`tests/bench/cu_math/README.md`](../tests/bench/cu_math/README.md),
[`tests/bench/m1_cu/README.md`](../tests/bench/m1_cu/README.md),
[`tests/bench/m2_cu/README.md`](../tests/bench/m2_cu/README.md),
[`tests/bench/m3_cu/README.md`](../tests/bench/m3_cu/README.md),
[`tests/bench/m5_cu/README.md`](../tests/bench/m5_cu/README.md) and
[`tests/bench/m6_cu/README.md`](../tests/bench/m6_cu/README.md).

## Services and the keeper (keeper v1, M3; pool selection, M4)

`services/` is a separate **host** workspace with its own `Cargo.lock` (ARCHITECTURE §1.2,
§7.1): `common` (the chain client: JSON-RPC backend, v1 / v0 transaction building),
`keeper` (keeper v1: duties 1–4, batching packed by size, degraded mode, priority fees,
M4F-1's post-switch coverage — per second while a switch's reference guard is set (its
per-slot bursts on a bucket's liquidity were reverted by the owner's rule of 2026-09-29, ECON #99;
since ECON #101, FITS-2, a foreign bucket liquidity deviation on a market with open positions
starts per-second coverage for `liq_cover_s`, extended while deviations recur) —,
duty 10's venue monitor — ProgramData upgrades flagged with `flag_venue_upgraded`, Raydium pool
admin actions alerted — and, since M4, duty 12's Raydium pool monitor and duty 5's Raydium fast
path — `getProgramAccounts` discovery of every Raydium SOL pool of a mint, the program's own
verifier and `pool_select::decide`, `propose_pool` / `execute_pool_switch`, since #99 only with a
candidate every open position fits (since #100 with the fee-scaled caps at `min(φ_inc, φ_cand)`,
since #101 at most 25 bps)
— on Raydium AMM v4,
CPMM and list-only markets; M4-SEC-7: discovered pools ranked by locked SOL, at most
`rank_keys_per_tick` (200) accounts read a tick and the best `max_candidates` (8) evaluated,
after the tick's exits and crank batch; config `[pool_select]` in `keeper.example.toml`; since M5
duty 6 on the pump curve — `migrate_v2` as soon as a covered curve completes (since pump-final P4
a position past `rT` exits in one instruction, and the straddle bundle with its `complete_curve`
helper and Jito block engine is retired) —, since M6 PumpSwap exits and account
preparation, the move onto the pool after anyone's migration, the post-wipe claim bundle and duty
13's `sweep_trader` (config `[sweep]`), `disable_flags` in duty 10 —,
curve exits with the adapter's own account lists, and list-only markets covered with their
detection set: a curve that graduated elsewhere with positions open is kept on the no-route
cadence and settled in S3, and a list-only market whose curve appears is cranked onto it and its
trader accounts created); the `shortwin-keeper` binary and `shortwin-build-alt`, the protocol
ALT) and the TEST-ONLY
`keeper-litesvm` (an in-process LiteSVM backend that runs the program-test hooks after every
instruction of every keeper transaction, and the scenario tests on the mock and on the Raydium
fork). Its lockfile holds the same Agave 4.2.2 line as the harness it links.

```sh
anchor build --tools-version v1.57 --arch v3 && scripts/build-test-venue.sh
cargo test --manifest-path services/Cargo.toml --workspace          # unit tests + LiteSVM scenarios
cargo clippy --manifest-path services/Cargo.toml --workspace --all-targets -- -D warnings
cargo clippy --manifest-path services/Cargo.toml -p shortwin-keeper --all-targets -- -D warnings  # without test-venue
cargo deny --manifest-path services/Cargo.toml check                # services/deny.toml
cargo audit -f services/Cargo.lock --deny warnings <the ignore list above>

# the RPC backend against a live local validator (ignored by default)
solana-test-validator --reset --quiet &
SHORTWIN_RPC_URL=http://127.0.0.1:8899 \
  cargo test --manifest-path services/Cargo.toml -p shortwin-services-common --test rpc_local -- --ignored

# run the keeper
cargo build --manifest-path services/Cargo.toml -p shortwin-keeper --release
services/target/release/shortwin-keeper --rpc <url> --keypair keeper.json \
  --config services/keeper/keeper.example.toml --metrics 127.0.0.1:9464   # --once, --tick-ms,
                                                  # --discovery-rpc <url> (M4 pool discovery)
```

The scenarios load the same builds as the program tests: the mock scenarios the test-venue
build and the mock (`SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`), the Raydium scenarios
(`raydium*.rs`, and M4's `pool_select.rs` with its planted sibling pools) and M5's pump-curve
scenarios (`graduation.rs`) and M6's PumpSwap and post-wipe
scenarios (`pamm.rs`, `pamm_lp.rs` — M6R-1: the keeper's per-second liquidity-anomaly coverage holds a PumpSwap withdraw drag at one step, one crank per wrap, and never runs on a market without positions —, `wipe.rs` — incl. `m6_sec_2_*`: a donated pAMM cashback ATA without the trader's pAMM UVA is unlocked by the keeper's `[init_user_volume_accumulator, …, sweep_trader]`) the release build
(`SHORTWIN_SO`) with the committed fork fixtures. The keeper
knows the Raydium venues through the adapters' own account lists and the TEST-ONLY mock venue
only with the feature `test-venue` of its scenario crate; the release keeper binary must not
contain the mock's id (CI checks it). What each duty does and what v1 leaves out:
[`services/README.md`](../services/README.md). **M7:** the O17 degraded mode — the designated
pauser (`[coverage_pause] enabled = true` on exactly one instance, `--coverage-keypair
coverage.json`, a key separate from the fee payer, installed on chain by Governance's timelocked
`Authorities` change) sends `set_coverage_pause` when a market with open positions loses coverage
and clears it 600 s after coverage returns; since the owner's decision of 2026-10-01 the Emergency
multisig can clear a coverage pause too (never set one: stop the pauser first to override its own
pause, ARCHITECTURE §3.1, §7.1 duty 9) —, the keeper budget (`[budget]`; since 2026-10-01 one
global daily budget funding markets in order of lenders' value at risk — `global_daily_lamports`,
`var_floor_lamports`, `reserve_margin_bps`, `tail_share_bps`; `per_market_daily_lamports` is
retired; ARCHITECTURE §7.1 duty 8), duty 11's `crank_sol_usd` (`[oracle]`; `aging_alert_s` 300
with `oracle_stale_s` 600), the T-CASCADE CU-per-slot policy, `shortwin-build-alt market`
(per-market lookup tables) and `shortwin-v1-probe` (U25: does a mainnet leader take v1?
`--rpc <url> --keypair <funded> --send`, two transaction fees). **The degraded-mode runbook and
drill** (install the role, run exactly one pauser, what pages, a lost or compromised key, the
five drill steps on devnet or a local validator): [`services/README.md` "Degraded mode: runbook
and drill"](../services/README.md#degraded-mode-runbook-and-drill-o17); the same steps run
in-process on every CI run (`keeper-litesvm` `coverage_pause`). Do not share one `CARGO_TARGET_DIR` between two
checkouts of the repository: Cargo keys path crates the same way in both and mixes their
artifacts.

### The Surfpool E2E (M6; `scripts/surfpool-e2e.sh`)

`tests/litesvm/tests/graduation_surfpool.rs` runs a real graduation on a **Surfpool 1.6 fork of live
mainnet** (ARCHITECTURE §8.4): only short.win, the harness keys, the protocol ALT and one fixture coin
are planted; pump, pump-fees, PumpSwap and every venue account are mainnet's. It is `#[ignore]`d in
the normal suites (it needs Surfpool and network access; not in CI). The public RPCs refuse Surfpool's
batched `getMultipleAccounts` (HTTP 403 on 2026-09-30), so the script starts a retrying forwarder
first:

```sh
anchor build --tools-version v1.57 --arch v3                 # the release shortwin.so it plants
SURFPOOL=/path/to/surfpool scripts/surfpool-e2e.sh            # forwarder :18999, Surfpool :18899, the test
# or by hand:
python3 scripts/surfpool-rpc-forwarder.py 18999 &           # retries publicnode / api.mainnet-beta
surfpool start --rpc-url http://127.0.0.1:18999 --no-tui --no-studio --no-deploy \
  --port 18899 --ws-port 18900 --airdrop-amount 0
SURFPOOL_RPC=127.0.0.1:18899 cargo test -p shortwin-litesvm-tests --test graduation_surfpool \
  -- --ignored --nocapture                                  # SURFPOOL_TRACE=1: every transaction
```

Tested with the v1.6.0 release binary (`surfpool-linux-x64.tar.gz`, sha256 `cb471b00…`); a run takes
≈ 30 s once built.

### The protocol ALT (A28; `scripts/build-alt.sh`)

One address lookup table holds the protocol's static accounts for every v0 transaction (the
keeper's v0 fallback, the frontend): ARCH §6.4 A28's list, the four `VenueExtras` PDAs, and the
live pump / PumpSwap fee and buyback recipients with their WSOL ATAs (83 addresses on
2026-09-28). The script wraps `shortwin-build-alt` (services workspace; `CARGO_TARGET_DIR` is
honoured):

```sh
scripts/build-alt.sh print                                   # the static part (49 keys), no RPC
scripts/build-alt.sh print  --rpc https://api.mainnet-beta.solana.com   # + the live recipients
scripts/build-alt.sh create --rpc <url> --keypair payer.json [--authority auth.json] [--freeze]
scripts/build-alt.sh extend --rpc <url> --keypair payer.json --table <alt>   # add what is missing
scripts/build-alt.sh check  --rpc <url> --table <alt>        # exit 1 if an address is missing
scripts/build-alt.sh market --rpc <url> --market <market> [--table <alt>] [--keypair payer.json]  # M7
```

Put the table's address into the keeper's `lookup_tables` (`keeper.example.toml`). **This table
(A28 plus the four `VenueExtras` PDAs) is what the M7 measurements recommend as the normative A28**
(OWNER-M7-1, ARCH §6.4): through it the PumpSwap open with the largest extras set and a separate
fee payer fits (1,207–1,208 B), which A28 as enumerated does not. `market` (M7) lists a market's
own table — 21 addresses on PumpSwap, 4,348,480 lamports of rent, recoverable — and creates it
with `--keypair`; it lifts v0 crank batches to 8–10 and is the fallback for the 4-extras open.
`--freeze` is
the owner's call: a frozen table can never be deactivated but can never be extended either (a new
pump recipient then rides inline, 31 B). Re-run `check` whenever pump rotates recipients.
`tests/litesvm/tests/tx_size_raydium.rs` sizes every Raydium transaction through this table, and
the keeper scenario `raydium_alt` builds it on chain with the script's own code.

### Governance: Squads v4 and `shortwin-gov` (M10)

The owner's layout (decisions of 2026-10-02 and 2026-10-03, ARCHITECTURE §1.1 "Admin", §8.5):
Governance, an autonomous Squads v4 3-of-5 multisig with a 48 h Squads time lock (172,800 s),
holds short.win's upgrade authority (its vault 0, which is also `governance`; the retired
`complete_curve` is not in the upgrade set, pump-final D2) and the fee recipient (its vault 1);
Emergency, an autonomous 2-of-4 with time lock 0, is `pause_authority` (its vault 0). Since SQ-G8 the program refuses a config in which
`governance`, `pause_authority`, `fee_recipient` and an installed `coverage_authority` are not
pairwise distinct (`AuthoritiesNotDistinct`, 6105). A program change takes 96 h end to end (48 h
Squads, then the program's 48 h timelock). Members sign in the Squads app; the repository's tool
only stages and verifies. The multisigs are created on mainnet in M11.

- **`crates/squads-v4-ix`** (root workspace, host-only): encoders, the Squads `TransactionMessage`
  compiler, PDAs and account decoders for the program mainnet runs, pinned against the vendored
  IDL 2.1.0 (`tests/fixtures/idl/squads_multisig_program.json`) by `tests/idl.rs`. The program
  tests (`src/squads.rs`) and `services/ops` link it.
- **`squads_gov_it`** (T-SQ-1..9) drives the governance through the mainnet Squads ELF (table
  above).
- **`shortwin-gov`** (`services/ops`, package `shortwin-ops`): creates the two multisigs,
  initializes the config with their vaults, stages vault transactions (the checked upgrade-authority
  transfer, `Authorities` changes, upgrades, every admin instruction; a transaction buffer for
  large messages, resumed after an interruption), decodes them for review (`status`), approves,
  executes, cancels, applies timelocked changes and `verify`s the whole layout. Every transaction is
  simulated first; `--dry-run` sends nothing; key files are read only to sign and never printed;
  hardware-wallet paths are refused (Ledger members approve and execute in the Squads app). By the
  owner's decision of 2026-10-03 the (M11) runbook does the Emergency's worst-case
  `set_venue_extras` as three single-signer transactions (`squads_gov_it` lands it in two, the
  second co-signed by two members). Commands: [`services/README.md` "Governance"](../services/README.md#governance-shortwin-gov-m10).

```sh
cargo test -p shortwin-litesvm-tests --test squads_gov_it -- --nocapture   # T-SQ-1..9 (sizes, CU)
cargo test -p squads-v4-ix                                                 # the IDL pins
cargo test --manifest-path services/Cargo.toml -p shortwin-ops             # unit tests, T-OPS (gov_e2e)
G="cargo run --release --manifest-path services/Cargo.toml -p shortwin-ops --bin shortwin-gov --"
$G verify --config gov.toml --rpc <url>     # every derived address and authority; exit 1 on a FAIL
```

`services/ops/governance.example.toml` names public keys only (the members, the two create keys,
the keeper's `coverage_authority`) and the launch values; its keys are placeholders and the tool
refuses to send with it. `init-config` refuses a venue guard slot that differs from the live
ProgramData: re-read the six slots on the day.

## Indexer and API (M8; `services/indexer`)

The indexer is two crates of the services workspace (contract and as-built notes:
[`docs/design/INDEXER.md`](design/INDEXER.md); crate guide:
[`services/README.md` "Indexer (M8)"](../services/README.md#indexer-m8)): `shortwin-indexer` (RPC
backfill and `logsSubscribe` live ingest of short.win and venue events into
Postgres, projections, analytics views and hourly rollups, axum REST + WebSocket per
`services/indexer/openapi.yaml`, one binary) and the TEST-ONLY `shortwin-indexer-litesvm` (records
LiteSVM sessions and replays them into a database, asserting it against the chain).

**Run it** against a Postgres you have (a local server, a managed one, or the reference
docker-compose below) and an RPC node that serves `getTransaction` with
`maxSupportedTransactionVersion: 1`:

```sh
cargo build --manifest-path services/Cargo.toml -p shortwin-indexer --release
cp services/indexer/indexer.example.toml indexer.toml          # every key, with its default
export DATABASE_URL=postgres://shortwin@localhost:5432/shortwin
export SHORTWIN_RPC_URL=http://127.0.0.1:8899 SHORTWIN_WS_URL=ws://127.0.0.1:8900
I=services/target/release/shortwin-indexer
$I --config indexer.toml migrate
$I --config indexer.toml backfill        # history, at finalized (one writer per database)
$I --config indexer.toml run             # live ingest + rollups + the API on [api] listen (:8080)
#   or as separate processes: live | rollups | serve
$I --config indexer.toml replay services/indexer/tests/data/sessions/mock_lifecycle.jsonl
curl -s localhost:8080/v1/status; curl -s 'localhost:8080/v1/markets?sort=oi'
```

**Tests on an embedded PostgreSQL (no Docker, no root, no system Postgres).** The crate's tests start
PostgreSQL 17.11 as the current user through `postgresql_embedded`, from the zonky builds (≈ 15 MB
from Maven Central, downloaded once into `~/.cache/shortwin/postgresql-zonky`; they bundle their
libraries, so they run on distributions without `libxml2.so.2`). Each test gets its own database
under `services/target/tmp`, removed afterwards. `SHORTWIN_PG_INSTALL_DIR` and `SHORTWIN_PG_TMP`
move the binaries and the data.

```sh
M=services/Cargo.toml
cargo test --manifest-path $M -p shortwin-indexer --test pg_fixture   # the fixture gate (~2 s cached)
cargo test --manifest-path $M -p shortwin-indexer                     # decoders, ingest, analytics, API contract
cargo test --manifest-path $M -p shortwin-indexer --test api_contract --test api_limits --test api_ws --test api_replayed
cargo test --manifest-path $M -p shortwin-indexer --test venue_mainnet   # M10 GC-F1: the venue decoders on recorded mainnet txs
solana-test-validator --version && \
  cargo test --manifest-path $M -p shortwin-indexer --test ingest_rpc_validator -- --ignored   # RPC paths, ~40 s
```

`venue_mainnet` (M10 GC-F1) runs the gap-calibration crawler's recorded mainnet transactions
(`scripts/gapcal/tests/fixtures`) through the production conversion, the decoders and, for CPMM,
the writer: the stored post-trade reserves are the venues' pricing reserves (INDEXER.md §7.2). A
test that reads `pg_stat_*` counters must first force a statistics flush on every backend that
could have touched them (`flush_stats` in `tests/security_api.rs`: `pg_stat_force_next_flush()`
on each connection), not sleep.

**The replay harness (the M8 exit criterion).** `cargo test --manifest-path services/Cargo.toml -p
shortwin-indexer-litesvm` (~80 s) records nine sessions from the current builds — so build first,
as for the keeper scenarios (`anchor build --tools-version v1.57 --arch v3 &&
scripts/build-test-venue.sh`; `SHORTWIN_SO`, `SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`
override the ELFs) — on the mock venue, the Raydium and pump forks and through
the keeper's scenarios, then ingests each through the real pipeline into a fresh database and
asserts INDEXER.md §11.4 against the chain at every checkpoint (projections, the SOL ledgers,
analytics and rollups, idempotence, rebuild, out-of-order delivery, a rollback through the finality
tracker, catalog coverage, the API's read model). The recording hook is `Env::start_recording` in
`tests/litesvm/src/record.rs`; `SHORTWIN_RECORD_SESSIONS=<dir>` writes the sessions (and
`<dir>/committed/` the thinned copies committed under `services/indexer/tests/data/sessions/`).
Surfpool sessions are optional (the format takes a `recorder = "surfpool"` header).

**Reference deployment (docker-compose; for real deployments, not run on this machine).**
`services/indexer/deploy/`: Postgres 17.11, a one-shot `migrate`, exactly one `ingest` (`live`),
`rollups`, two `api` replicas; database roles from `initdb/01-roles.sh` (the API's is read-only
with a 2 s statement timeout); the image builds from the repository root through
`Dockerfile.dockerignore` (no `target/`, no keys).

```sh
cd services/indexer/deploy
mkdir -p secrets && for r in admin ingest api; do openssl rand -hex 24 > secrets/pg_$r; done
export SHORTWIN_RPC_URL=https://… SHORTWIN_WS_URL=wss://…
docker compose up -d postgres migrate
docker compose run --rm ingest backfill
docker compose up -d
```

## Web frontend (M9; `apps/web`)

The contract is [`docs/design/FRONTEND.md`](design/FRONTEND.md) (as built: v1.1). The app is a
Vite 8 + React 19 SPA in the pnpm workspace (`apps/web`, package `web`); it uses
`@shortwin/client` and the WASM math package `@shortwin/math-wasm` (`packages/math-wasm`, built
from `crates/shortwin-math-wasm`). Every page is its own chunk (route-level code splitting).

```sh
eval "$(mise activate bash)"
cargo install wasm-bindgen-cli --version =0.2.129 --locked   # once: the crate's exact pin
pnpm install --frozen-lockfile
pnpm --filter @shortwin/math-wasm build   # cargo build (wasm32, release) + wasm-bindgen --target web -> packages/math-wasm/pkg
cargo build -p shortwin-math-wasm --target wasm32-unknown-unknown --release --locked   # the .wasm (the pnpm build skips cargo while pkg/ is current)
wasm-bindgen --target nodejs --out-dir target/wasm-pkg target/wasm32-unknown-unknown/release/shortwin_math_wasm.wasm   # under $CARGO_TARGET_DIR if set
node crates/shortwin-math-wasm/js/smoke.cjs target/wasm-pkg   # the facade's JS interface from Node (CI's web job)
pnpm --filter web lint                    # biome check . (root biome.json)
pnpm --filter web typecheck               # tsc --noEmit (strict; includes e2e/ and playwright.config.ts)
pnpm --filter web test                    # vitest run (jsdom)
pnpm --filter web build                   # wasm (skipped when current) + tsc + vite build -> apps/web/dist (+ _headers; source maps -> apps/web/dist-sourcemaps, not deployed)
pnpm -r build                             # every workspace package that has a build (math-wasm, web)
pnpm --filter web check:headers https://<origin>   # after a deploy: X-Frame-Options, frame-ancestors, nosniff, HSTS; no public source maps; /.well-known/security.txt
pnpm --filter web exec node scripts/owner-copy.ts <out.md>   # the thirteenth round's new copy, every string with its id and sources (the owner's approval file)
pnpm --filter web exec node scripts/owner-copy.ts --doc      # regenerate FRONTEND.md §4.3's catalogue after changing src/config/copy.ts or glossary.ts
```

**New copy (the owner's thirteenth round).** The intro strip, How it works, the glossary, the
coverage link and the Claim label have one home each — `src/config/copy.ts` (with `copyNav.ts`, the
shell's links) and `src/config/glossary.ts` (sources in `glossarySources.ts`, the text resolution in
`glossaryText.ts`) — and `copy.test.ts` / `glossary.test.ts` fail until FRONTEND.md §4.3 carries every
string verbatim: after a change run `node scripts/owner-copy.ts --doc` in the same change. Every
string there is *pending owner approval* until the owner signs it off word for word. The
mobile-wallet strings (O-6, approved as drafted on 2026-10-13) live in `src/config/copyWallet.ts`,
which `copy.ts` spreads into `COPY` (the owner file's section 8); the same §4.3 check and `--doc`
regeneration apply.

**security.txt (M10 SECTXT-4).** `apps/web/public/.well-known/security.txt` (RFC 9116: the owner's
contact and policy, `Expires` 2027-09-30, rotate it before 2027-08-31) is checked by
`src/config/securityTxt.ts` in every build (fields and their format), by its unit test (on a clock
pinned 90 days before its own `Expires`) and by the E2E `frame` spec. Only a `VITE_CLUSTER=mainnet`
build and `check:headers` (production checks) also refuse an `OWNER-FILL` placeholder and an
`Expires` 30 days or less, or more than a year, ahead; other builds never fail because the date
nears. `dist/_headers` serves it as `text/plain; charset=utf-8`. The policy it names is the app's
`/security` page (`SecurityPolicyPage`, FRONTEND.md §3.2); the repository's `SECURITY.md` carries the
same values. FE-15 (M10): the TypeScript ports of the mint verdict, the open guards
(`spot_near_ema`, `P_OPEN_MAX`) and the pool-selection comparisons are gone; the WASM facade
exports them (FRONTEND.md §13 FE-15).

`packages/math-wasm/pkg/` is generated and gitignored; `pkg/.inputs-sha256` stamps the crate
sources, so later builds skip cargo and wasm-bindgen until `crates/shortwin-math*` or
`Cargo.lock` change (`$WASM_BINDGEN` may point at a matching binary instead of `PATH`; cargo is
looked up in `~/.cargo/bin` when it is not on `PATH`). The app is configured by `VITE_*`
variables (`apps/web/.env.example`; a bare build is a localnet build, and an invalid value fails
the build). Never put a secret in a `VITE_*` value: it ships in the bundle.

`src/features/short/live.validator.test.ts` is an optional Vitest suite against a running
validator (skipped unless `SHORTWIN_LIVE_RPC` is set; start one with
`src/features/short/vectors/live-validator.sh`, ports 38899/38900/39900).

### Run the app against a local validator

The E2E bootstrap doubles as the local environment: a `solana-test-validator` with the test-venue
program, the mock venue and the dumped real venues, seeded with markets, lenders and open
positions (details under "Web E2E" below). On ports of its own (RPC 18899, WS 18900), so a
developer validator on 8899 can run alongside.

```sh
anchor build --tools-version v1.57 --arch v3 && scripts/build-test-venue.sh   # the ELFs (once per program change)
pnpm --filter @shortwin/math-wasm build
cd apps/web
node e2e/validator/start.ts --env      # validator + seed (≈ 100 s), left running; writes .env.e2e.local
node e2e/validator/fund.ts <your wallet address>   # 25 SOL and 1,000,000 of every seeded mint
pnpm dev:e2e                           # vite --mode e2e: http://localhost:5173 against the validator
node e2e/validator/stop.ts             # stop the validator (and the preview / indexer)
```

- **Wallet:** any Wallet Standard wallet (Phantom, Solflare, Backpack) with a custom RPC of
  `http://127.0.0.1:18899` signs on `solana:localnet`. `fund.ts` uses the seed's generated
  (throwaway) mint authority in `.e2e/keys/`; the seeded addresses are in `.e2e/state.json`.
- **Indexer:** none by default, so the app runs in its RPC fallback (banner, the market page from
  the chain, the markets list replaced by "open a mint"; FRONTEND.md §6.5). For the full app,
  start the real indexer on the same validator: `SHORTWIN_E2E_PROFILE=full
  SHORTWIN_INDEXER_BIN=<built shortwin-indexer> node e2e/validator/start.ts --env` (PostgreSQL
  from the zonky binaries, API on :18080; see "Profile `full`" below).
- **The production build** instead of the dev server: `node e2e/validator/start.ts --app` builds
  it with the E2E configuration (CSP meta, `_headers`, hashed WASM) and serves it with
  `vite preview` on http://127.0.0.1:14173.
- The mock venue (`VITE_ENABLE_TEST_VENUE=true`) exists only in the test-venue program build and
  on localnet; the configuration refuses it anywhere else (FE-D7).

### Web E2E (M9; `apps/web/e2e`)

Playwright 1.63 against the production build, a local `solana-test-validator` and an injected
Wallet Standard test wallet ([`FRONTEND.md`](design/FRONTEND.md) §10.2–§10.3). No Docker, no
network, nothing installed as root; everything the run writes is under `apps/web/.e2e/`
(gitignored: the ledger, generated keys, the seed's `state.json`, the app build, logs, the HTML
report, traces and the screenshots).

```sh
anchor build --tools-version v1.57 --arch v3 && scripts/build-test-venue.sh   # the ELFs the validator loads
pnpm --filter @shortwin/math-wasm build
pnpm --filter web e2e                       # every spec: ≈ 9 min (≈ 2 of them setup: validator, seed, build)
pnpm --filter web e2e e2e/specs/lend-cpi-guard.spec.ts e2e/specs/lend-withdraw-memo.spec.ts   # the §10.3 exit tests
pnpm --filter web e2e --grep @scaffold      # the harness, smoke, wallet and frame specs only
SHORTWIN_E2E_KEEP=1 pnpm --filter web e2e   # leave the validator and the preview running, then…
SHORTWIN_E2E_REUSE=1 pnpm --filter web e2e e2e/specs/smoke.spec.ts   # …rerun in seconds without reseeding
SHORTWIN_E2E_SCREENSHOTS=update pnpm --filter web e2e e2e/specs/screens.spec.ts   # refresh e2e/screenshots/
SHORTWIN_E2E_RESPONSIVE=1 pnpm --filter web e2e --grep @responsive   # eight viewports: .e2e/responsive/ PNGs + metrics.json
SHORTWIN_E2E_RESPONSIVE=1 SHORTWIN_RESPONSIVE_GATE=1 pnpm --filter web e2e --grep @responsive   # M10-RESP: the same as a gate
pnpm --filter web e2e --project chain-phone  # M10-RESP: the phone project alone (mobile-*.spec.ts but mobile-mwa)
SHORTWIN_E2E_SCREENSHOTS=update pnpm --filter web e2e --project chain-phone   # refresh e2e/screenshots/phone/
pnpm --filter web e2e --project chain-android   # O-6: the Android project (mobile-shell and mobile-mwa: the Mobile Wallet Adapter against the fake wallet app)
SHORTWIN_E2E_MWA=0 pnpm --filter web e2e --project chain-android   # the same with the adapter built off (the app rebuilds automatically)
SHORTWIN_E2E_SCREENSHOTS=update pnpm --filter web e2e --project chain-android   # refresh e2e/screenshots/android/
pnpm --filter web exec node e2e/responsive/summary.ts --pages        # its per-viewport counts and worst states
pnpm --filter web exec playwright show-report .e2e/report
```

With `SHORTWIN_E2E_REUSE=1` the app is rebuilt (and the preview restarted) whenever its sources,
build configuration, the WASM stamp or the E2E `VITE_*` values changed since the last build
(`sourceFingerprint` in `e2e/validator/app.ts`); the seed is reused as it is, so a spec that needs
a fresh chain needs a run without `REUSE`.

**Global setup** (`e2e/global-setup.ts`; the validator and the preview are started there rather
than by Playwright's `webServer`, because the build needs the seeded protocol ALT):

1. `e2e/validator/validator.ts`: `solana-test-validator --reset --ledger apps/web/.e2e/ledger
   --limit-ledger-size 200000000` on dedicated ports (RPC 18899, WS 18900, faucet 19900, gossip
   18901, dynamic 18910–18960), with short.win's **test-venue** build
   (`target/test-venue/deploy/shortwin.so`) as an upgradeable program whose upgrade authority is
   a generated key, `mock_cpamm.so` from `target/deploy` (*2026-10-15:* no longer
   `complete_curve.so`, retired with pump's v3 exits, design pump-final D2), the dumped
   pump / pump-fees / PumpSwap / Raydium AMM v4 / CPMM ELFs of `tests/fixtures/programs` with
   their mainnet upgrade authorities (CPMM: the 2026-10 redeploy, M10), mainnet's Token-2022
   (`--upgradeable-program` from the `spl_programs` group with its mainnet upgrade authority,
   replacing the built-in, which is another build; M10 T-MEMO), and genesis accounts
   (`--account-dir .e2e/accounts`): every generated key funded, the mock's empty `VenueExtras` (as
   the LiteSVM harness installs it) and the dumped venue state of `tests/fixtures` (deduplicated;
   sysvars, synthetic and absent entries and the program groups — `raydium_programs`,
   `pump_programs` and the M10 groups — skipped). spl-memo v3 is the validator's built-in, which
   in Agave 4.1.2 already is mainnet's BPFLoader2 account byte for byte; global setup
   (`checkSplPrograms`) holds both to their mainnet hashes. Boots in ≈ 1.5 s.
2. `e2e/validator/seed.ts` (Kit, `@shortwin/client`, `@solana-program/*`): `initialize_config`
   with the ECON §15 defaults read from the WASM `ParamSet`, the generated governance / Emergency /
   coverage / fee keys and the live ProgramData slots; five SPL mints with mock pools and markets
   (`main` for trading, `move` for the price-move specs, `paused` in coverage pause, and since
   M10 `wipe` for FE-18's full wipe and `pot` for the claim-time pot, each lent by one spec wallet
   alone) and a list-only Token-2022 market (`t22`, metadata only); a protocol ALT; one funded wallet per
   independent spec group with its token accounts (a CPI-Guarded and a guard-capable Token-2022
   account, memo-required ATAs, lender positions); create-market mints (safe, freeze authority,
   a permanent delegate); after the 50 s market warm-up, five open positions. ≈ 100 s.
3. `vite build` with the E2E configuration (`VITE_ENABLE_TEST_VENUE=true`, the E2E ports, the
   seeded ALT) into `.e2e/dist`, served by `vite preview` on :14173.

**In the browser** (`e2e/fixtures.ts`): `wallet/injected-wallet.js` registers the "ShortWin E2E"
wallet (Wallet Standard, `solana:localnet`, Ed25519 by WebCrypto with the spec's generated key)
with test controls (`rejectNext`, `tamperNext('data' | 'drop' | fn)`, `raiseFeeNext`, `signed`);
the indexer is a `page.route` / `page.routeWebSocket` mock that builds OpenAPI-shaped responses
from the chain at request time (`indexer-mock.ts`; `indexer: 'down'` simulates an outage); every
spec selects by the §11 `data-testid`s only, fails on any CSP violation and runs axe (WCAG 2.2
AA) on the pages it visits. Node-side helpers (`validator/actions.ts`) move a mock pool's price
and flip CPI Guard or required memos for the race tests; since M10 also a full wipe (`fullWipe`:
`withdrawAllFree`, a gap, `settleUntilClosed`), validator simulations of a lone `claim_sol` /
`sync_lender` (`simulateClaimSol`, `simulateSyncLender`), emit_cpi event decoding (`eventsOf`) and
an empty `sweep_trader` that credits a market's pending rent (`accrue`).

**Specs** (22 files, 58 tests at M10; *M10-RESP:* 28 files with the four `mobile-*` specs (`responsive` is opt-in) — and a default run of 77 tests, 60 desktop and 17 phone; *thirteenth round:* 30 files with `guide` and `mobile-guide`, a default run of 87 tests, 65 desktop and 22 phone; *O-6 (`playwright test --list`, 2026-10-13):* 32 files with `focus`, `mobile-focus` and `mobile-mwa` (33 with the opt-in `responsive`), a default run of 123 tests, 70 desktop, 26 phone and 27 Android — `mobile-mwa`'s 21 include its two flag-off tests, skipped in a flag-on run as its 19 flag-on tests are with `SHORTWIN_E2E_MWA=0`; *pump v3 (2026-10-15):* 33 files with `short-close-curve`, a default run of 124 tests, 71 desktop, 26 phone and 27 Android): `harness`, `smoke`, `wallet`, `frame` (`@scaffold`); `markets`,
`create-market`, `status`, `fallback` (`@markets`); `lend-deposit`, `lend-cpi-guard`,
`lend-withdraw-memo`, `lend-claim`, `keyboard`, and since M10 (FE-18) `lend-post-wipe`,
`lend-claim-sync`, `lend-sweep-cashback` (`@lend`); `short-open`, `short-close`,
`short-close-curve` (pump v3: a close past the dumped near166 curve's `rT` on pump's 2026-10-13
ELF, the curve completed by synthetic migration in one `close_short`; its lender's tokens are
planted at genesis, `curveLender`), `add-margin`, `positions`, `tx-errors` (`@short`); `screens` (`@screens`: every page and state as
a full-height PNG in `.e2e/screenshots/`, axe on each, 1024 px without sideways scroll;
`SHORTWIN_E2E_SCREENSHOTS=update` writes the reviewed set in `e2e/screenshots/`). *M10-RESP:* two
Playwright projects — `<profile>` (the desktop, 1440 × 900: every spec above) and `<profile>-phone`
(390 × 844, touch, DPR 3, an iPhone user agent: the `mobile-*.spec.ts` specs — `mobile-shell`
from the foundation; `mobile-markets`, `mobile-trade`, `mobile-lend` from the page tracks), both
in the default run (the phone project ≈ 6 min or less once complete); the phone specs tap, run axe
at 0 on every state, sign only with the seeded `phone` wallet and write `e2e/screenshots/phone/`
with `SHORTWIN_E2E_SCREENSHOTS=update` (an owner-reviewed set, like the desktop one). *O-6
(2026-10-13):* a third project, also in the default run, `<profile>-android` (a Pixel 7: 412 × 839,
an Android Chrome user agent), runs `mobile-shell` and `mobile-mwa` — the Mobile Wallet Adapter
against a protocol-level fake wallet app (`e2e/wallet/fake-mwa.ts`, behind `page.routeWebSocket`,
signing as the seeded `android` wallet) with init-script hooks standing in for Android's app
switch (`e2e/wallet/mwa-hooks.js`) — and writes `e2e/screenshots/android/`; `SHORTWIN_E2E_MWA=0`
builds the app with the adapter off for `mobile-mwa`'s flag-off tests (its `mobile-shell` PNGs go
to `.e2e/screenshots/android-mwa-off/` only; FRONTEND.md §10.2). The fake is also held to the real
library by Vitest (`e2e/wallet/fake-mwa.test.ts`, in `pnpm --filter web test`). *M3
(2026-10-13), the first browser runs:* the Android project 25 passed and 2 skipped (the flag-off
tests; ≈ 3.4 min with the seed and the build), with `SHORTWIN_E2E_MWA=0` 8 passed and 19
skipped (≈ 2.2 min), the whole default run 121 passed and 2 skipped (70 desktop, 26 phone, 25
Android; ≈ 18 min), the responsive gate 96 passed with no finding (≈ 21 min; FRONTEND.md §13
"Mobile wallets"). CI's
`web-e2e` job runs them all and keeps the screenshots as an artifact. Not in the default run:
`responsive` (`@responsive`; FRONTEND.md §10.5) — every page and state at eight viewports from a
320 px phone (and a landscape phone) to a 1440 px desktop, a PNG each in `$SHORTWIN_SHOTS_DIR/<viewport>/` and the layout
metrics (sideways overflow, touch targets under 44 px, text under 12 px, hover-only affordances,
axe per viewport) in `metrics.json`; it runs with `SHORTWIN_E2E_RESPONSIVE=1` or a
`SHORTWIN_SHOTS_DIR` (default `.e2e/responsive`), `SHORTWIN_SHOTS_VIEWPORTS=phone,desktop` limits
the viewports, ≈ 6 × the screens spec. *M10-RESP:* `SHORTWIN_RESPONSIVE_GATE=1` makes it the
responsive gate (FRONTEND.md §10.5: overflow, layout width, zoom, off-screen elements, axe, targets
under 24 px, primary controls under 44 px with a coarse pointer, text under 12 px — at every
viewport since the thirteenth round's O-4, on phones only before —, failed steps, DOM-click
fallbacks), required before each responsive phase merges.

**Running the responsive capture (M10-RESP).** A responsive change is checked in three steps, all
under the E2E lock (one run per host):

```sh
cd apps/web
pnpm --filter web e2e --project chain-phone                     # the phone flows (≈ 6 min)
SHORTWIN_E2E_RESPONSIVE=1 SHORTWIN_RESPONSIVE_GATE=1 SHORTWIN_SHOTS_DIR=$PWD/.e2e/after \
  pnpm --filter web e2e --grep @responsive                      # the gate at all eight viewports (≈ 30 min)
node e2e/responsive/summary.ts .e2e/after --pages               # per viewport: overflow, targets, text, axe; the worst states
node e2e/responsive/summary.ts .e2e/before .e2e/after           # two directories: BEFORE and AFTER side by side
```

A failing gate test lists each finding as `<viewport>/<state>: <what>`; the PNGs in
`<dir>/<viewport>/` (`<state>.png` the whole page, `<state>-screen.png` the first screen on a phone
or tablet) show it. Look at the PNGs, not only the counts: the gate cannot see a card whose values
collide or a sheet that hides its submit. `SHORTWIN_SHOTS_VIEWPORTS=phone-s,tablet` narrows a rerun
while fixing; the final run is always all viewports. After a reviewed visual change, refresh both
reference sets: `SHORTWIN_E2E_SCREENSHOTS=update pnpm --filter web e2e e2e/specs/screens.spec.ts`
(desktop) and `SHORTWIN_E2E_SCREENSHOTS=update pnpm --filter web e2e --project chain-phone` (phone).

**Variables.** `SHORTWIN_CHROMIUM` (browser; default `/usr/bin/chromium` when present, `playwright`
forces Playwright's own Chromium, installed once with `pnpm --filter web exec playwright install
chromium` into the user cache — never `--with-deps` as root); `SHORTWIN_E2E_PROGRAM=release`
(the release `shortwin.so`: list-only markets, the trading specs skip); `SHORTWIN_E2E_VENUES=0`
(no dumped venues); `SHORTWIN_BUNDLED_SPL=1` (M10: keep the validator's built-in Token-2022 and
skip the SPL pin check); `SOLANA_BIN`, `SHORTWIN_SO`, `MOCK_CPAMM_SO` (*2026-10-15:*
`COMPLETE_CURVE_SO` is gone with the helper, pump-final D2).

**On a shared or small host (M10).**
- Playwright puts Chromium's profile in `os.tmpdir()`. On a host where `/tmp` is a quota'd or
  full tmpfs, Chromium fails lazy chunk loads with `net::ERR_INSUFFICIENT_RESOURCES`, and the
  tests show the app's error boundary, "RPC node is unreachable" or "Target crashed". Run the
  suite with `TMPDIR=<a directory on disk>`.
- System Chromium 152 (Arch) renderer crashes (SIGTRAP) failed every page spec on the M10
  development machine; `SHORTWIN_CHROMIUM=playwright` (Playwright's own Chromium) is the
  workaround.
- Playwright's own Chromium (`pnpm --filter web exec playwright install chromium`, the user cache)
  still needs the system's NSS, NSPR and ALSA libraries. A bare Ubuntu 24.04 (a fresh WSL distro
  among them) lacks them, and every test then fails at launch with `error while loading shared
  libraries: libnspr4.so` (global setup, which needs no browser, still succeeds). The machine's
  administrator installs them once: `sudo apt-get install libnss3 libnspr4 libasound2t64` (or
  `sudo pnpm --filter web exec playwright install-deps chromium`, which installs Playwright's whole
  list); the hosted CI image already has them. `ldd ~/.cache/ms-playwright/chromium*/chrome-*/chrome*
  | grep 'not found'` lists what is missing.
- The E2E ports are fixed (above), so one run per host: a second run, or a run beside a
  `start.ts` session, collides. Serialize them, e.g.
  `flock ~/.cache/shortwin/e2e.lock pnpm --filter web e2e`, and stop a validator you started
  (`node e2e/validator/stop.ts`) before releasing the lock.
- Do not carry the program tests' `SHORTWIN_SO` (the **release** build there) into an E2E run: here
  it replaces the default **test-venue** build, the validator then has no mock venue, and the
  trading specs fail.

**Profile `full`** (local, optional): `SHORTWIN_E2E_PROFILE=full SHORTWIN_INDEXER_BIN=<built
shortwin-indexer> pnpm --filter web e2e` starts PostgreSQL 17 from the zonky binaries the indexer's
tests cache (`~/.cache/shortwin/postgresql-zonky`, or `SHORTWIN_PG_INSTALL_DIR`; data in
`.e2e/pg`, port 15432), runs `migrate`, `backfill` and `run` against the validator (API on :18080)
and points the app at it; the mock-specific checks skip.

**By hand.** `node e2e/validator/start.ts [--app] [--env]` boots and seeds the environment (and
serves the app, or writes `.env.e2e.local` for the dev server) and leaves it running;
`node e2e/validator/fund.ts <address>` funds a browser wallet; `node e2e/validator/stop.ts` stops
everything. The seeded addresses are in `.e2e/state.json`, the generated (throwaway) keys in
`.e2e/keys/`.

### Mobile wallets on a real device (O-6; the owner's checklist)

The Mobile Wallet Adapter is on by default since 2026-10-13 (FRONTEND.md §5.4 (6)); the owner's
real-device test precedes the release (FRONTEND.md §13 FE-32). Playwright covers it against a
protocol-level fake wallet (Web E2E above; every Android spec passed on 2026-10-13, FRONTEND.md
§13 "Mobile wallets"); only a phone shows the Android intent and the app switch, Chrome's "Apps on
device" prompt, `upgrade-insecure-requests` on `ws://localhost`, a discarded tab and what real
wallets do. This is the checklist: record each result in the Result
column (or a copy of the table, linked from FE-32), then the devices, versions and date in
FRONTEND.md §13 FE-32 and in the AUDIT.md §7 O-6 row ("device test passed on <date>").

**Prerequisites.** Who hosts the P1 and P2 builds, and where, is still the owner's decision
(FRONTEND.md §13 FE-32): until it is made, neither exists.
- **P1.** A `VITE_CLUSTER=devnet` build with the flag on, served over **HTTPS** (a preview
  deploy), with short.win deployed on devnet and one market seeded. The Sign tests need it.
- **P2.** A `VITE_CLUSTER=mainnet` build over HTTPS, for **connect only** (F1); no transaction is
  sent.
- **P3.** An Android phone with Chrome ≥ 147 (and one with Chrome 145–146 if available), Samsung
  Internet, Brave and Firefox.
- **P4.** Phantom, Solflare and Backpack set to devnet, plus Seed Vault on a Seeker or Saga if
  available.
- **P5.** USB remote debugging to read the console: on the phone, Developer options → USB
  debugging; on the computer, Chrome at `chrome://inspect/#devices`, the phone's tab → Inspect.
  Watch for CSP errors ("Refused to …"), "MWA not registered" and "Mobile Wallet Adapter not
  loaded" warnings. DevTools' Network panel there also throttles the phone's tab (E2: Slow 3G).

| # | Step | Expected | Result |
|---|---|---|---|
| A1 | Open the devnet build in Chrome and tap Connect | The sheet lists "Use installed wallet" / "Opens your wallet app on this phone", then "Or open this page in a wallet app" with three links and "Set your wallet to devnet first." | |
| A2 | Remote console | No CSP violation, no "MWA not registered" | |
| B1 | First visit (permission never decided): tap the entry | Chrome's local-network prompt appears **at once**, while still in Chrome; the entry's line reads "Allow 'Apps on device' so this page can reach your wallet app". **Record Chrome's exact name for the permission**; if it is not "Apps on device", report it to the owner (the string is approved as drafted) | |
| B2 | Allow | The line returns to "Opens your wallet app on this phone"; tap again → the connect flow (C1) | |
| B3 | In a fresh profile: Deny | The permission text stays; taps do nothing else; allow "Apps on device" in site settings → the entry is ready **without a reload** | |
| B4 | Chrome 145–146 (if available) | The same flow (`fetch`-gated permission) | |
| C1 | Connect with one MWA wallet installed; then with several | One wallet opens directly; with several, Android's chooser lists them. The authorize screen shows "ShortWin", the origin and the PNG icon. Approve → back in Chrome → the header shows the address | |
| C2 | Decline in the wallet | Back in Chrome: "Connect wallet", no dialog, no overlay (dimmed or not) | |
| C3 | Cancel Android's app chooser | About 10 s after Chrome is back (at most 30 s) the button is "Connect wallet" again | |
| C4 | A phone with no MWA wallet | About 3 s after the tap the wallet sheet reopens with the open-in links; no library modal, no redirect to solanamobile.com | |
| C5 | Tap the entry, then go back to Chrome **through Recents** without deciding in the wallet; tap anywhere on the page | The page is not dimmed and a tap does nothing unexpected (no library overlay); the header button is busy, and about 10 s after Chrome is back it reads "Connect wallet"; tapping the entry opens the wallet again (record whether the wallet still shows the first request) | |
| D1 | Reload after C1 | The address returns **without** the wallet app opening | |
| D2 | Disconnect, reload | Disconnected; Connect opens the wallet again. Record whether the wallet still lists short.win as connected (Disconnect is local only) | |
| D3 | Change the active account in the wallet, then reload | The reload shows the **old** address, with no notice (the silent reconnect reads only this browser's cache); the change appears at the next Sign (E10) | |
| E1 | Devnet deposit: Review → Sign in wallet | The wallet opens **immediately** and shows the transaction; approve → "Confirming…" → Confirmed; the explorer shows the signature | |
| E2 | The same with a throttled network (remote DevTools, Slow 3G) | The wallet still opens at once (the prefetch); no "Couldn't open your wallet app" | |
| E3 | Decline in the wallet | "Cancelled in your wallet. Nothing was sent." + Try again | |
| E4 | Leave the wallet open for more than 60 s, then approve | Either confirmed or "The transaction expired before it landed. Try again." (record which) | |
| E5 | After Sign, return to Chrome (back or Recents) without acting in the wallet | Either the wallet closes the session and the review shows "Your wallet could not sign: …" with Retry, or the review still waits: then Cancel is enabled (from 5 s after the tap) and closes the review. Nothing is sent either way. Record which | |
| E6 | Developer option "Don't keep activities" (Chrome discarded while in the wallet) | Record the behaviour; the app sends nothing it did not see signed | |
| E7 | A wallet that returns `wallet_uri_base` (second sign over an https App Link) | Chrome stays on the page (not navigated to the wallet's site) | |
| E8 | A wallet without `solana:signTransactions` (if any) | It connects; the forms show their no-signer state (as for a read-only wallet: e.g. "Connect a wallet to lend this token."); no "sends itself" badge or banner; nothing is sent | |
| E9 | Revoke short.win in the wallet's connected-apps list after C1 (or wait for the token to expire), then Sign | The wallet opens and refuses or asks again; back in Chrome the review shows "Connect a wallet that can sign." with "Close and review again", and the header shows "Connect wallet". Nothing is sent. Reconnect → a new authorization in the wallet | |
| E10 | After C1, switch the active account in the wallet, then Sign | Record what the wallet does: it signs with the connected account (then E1's result), or it answers with the new account, and the review then shows "Connect a wallet that can sign." with "Close and review again" while the header shows the new address (or, after a reload, the M9-SEC-8 notice). Nothing is signed for the wrong account | |
| E11 | For each wallet (Phantom, Solflare, Backpack, Seed Vault) | Record its signer path (FRONTEND.md §5.4 (6)): the forms are enabled after Connect and the review opens with no "sends itself" banner = the `solana:signTransaction` path. A no-signer state after a successful Connect (no `solana:signTransactions` in its capabilities, MWA 1.x without sign-and-send, or per-account features that hide `solana:signTransaction` from Kit) is a finding for the owner | |
| F1 | Mainnet build: connect through MWA (no signing) | Connect succeeds, so `upgrade-insecure-requests` did not upgrade `ws://localhost`. If it times out, look for `wss://localhost` in the remote console → the owner decides (drop the directive from flag-on mainnet builds, or release mainnet with the flag off) | |
| G1 | Samsung Internet, Brave | The entry is present; connect and one devnet sign; record the local-network behaviour | |
| G2 | Firefox for Android | No "Use installed wallet"; the open-in links work | |
| H1 | Telegram, X, Instagram, Facebook in-app browsers (Android and iOS): open a short.win link | The sheet starts with "Open this page in your browser" and Copy link; the pasted URL equals the page's (path, query, hash) | |
| H2 | From those, tap Open in Phantom / Solflare / Backpack | Record whether the app opens or the wallet's website loads | |
| H3 | iOS Safari, and SFSafariViewController (Telegram iOS) | Each open-in link opens the wallet's browser on the same URL | |
| H4 | Inside each wallet's in-app browser (iOS and Android) | Connect (on Android, the open-elsewhere block also shows), a devnet sign, `assertSameIntent` passes; with the wallet set to mainnet, record its mismatch behaviour | |
| I1 | TalkBack | The entry reads "Use installed wallet, Opens your wallet app on this phone"; the links are reachable; while signing, the enabled Cancel is announced | |
| I2 | An Android tablet (if available) | The same entry; record whether "Opens your wallet app on this phone" reads acceptably on a tablet (wording feedback for the owner; the string ships as approved) | |

**Release criterion.**
- A–E and I pass in Chrome with at least Phantom and Solflare.
- **E11: Phantom and Solflare both take the `solana:signTransaction` path.** If either does not, the
  adapter cannot sign with it and the owner decides: ship with the flag off until a follow-up
  app-built signer is reviewed, or allow the post hoc path for MWA (with its double-send hazard).
- F1 passes, or the owner decides the `upgrade-insecure-requests` fallback.
- G and H are recorded; they block only on a regression against today's links.
- A release that must not wait ships with `VITE_ENABLE_MOBILE_WALLET_ADAPTER=false` (the CSP then
  drops the adapter's sources too).

## Codama client

`clients/js` is generated from the Anchor IDL; the IDL copy and the generated code are
committed. After any change to the program's interface (accounts, args, events, errors):

```sh
anchor build --tools-version v1.57 --arch v3
pnpm install --frozen-lockfile
pnpm --filter @shortwin/client idl:sync     # target/idl/shortwin.json -> clients/js/idl/
pnpm --filter @shortwin/client generate     # -> clients/js/src/generated/
pnpm --filter @shortwin/client check        # tsc + node --test (incl. the T-LAYOUT Codama leg)
```

CI fails if the IDL copy or the generated code is stale (the retired straddle helper's
`complete_curve.json` left `clients/js/idl/` with it, pump-final §10). See
[`clients/js/README.md`](../clients/js/README.md).

## Governance config gate (A73)

Every governance proposal passes `scripts/config-gate.sh` before the multisig signs it: the
compiled bounds and relational checks (`shortwin_sim.validate_params`, the same function as
`params::validate`), then `docs/design/sim/config_check.py`'s 1,655-run liveness battery.

```sh
scripts/config-gate.sh kappa_liq_bps=3000 D:rate_base_bps=500     # ECON §15 names; T: = one tier
scripts/config-gate.sh --file proposal.txt --owner-signoff "SPEC §14 …"
python3 scripts/test_config_gate.py                              # the gate's own tests (~1 min)
```

Exit 0 accepted, 1 rejected (a liveness row), 2 rejected (bounds), 3 needs the owner's sign-off
(only rows that leave lenders more than 1 % of Q worse), 64 usage. Parameters the sim does not
model (`params::validate_econ`) are listed, not simulated; the program checks their bounds at
`propose_config`.

## Gap-frequency calibration crawler (M10; `scripts/gapcal`)

The jump parameters of ECON §11.6's Monte Carlo (`lam_up`, `lam_dn`, `alpha`, `sigma` per tier;
G37, U21, O9) are calibrated from a read-only crawl of public mainnet RPC, because the M8 indexer
sees only short.win's own markets until launch. Method and status:
[`docs/research/gap-calibration.md`](research/gap-calibration.md); results need at least 30 days of
tapes (the crawl started on 2026-10-02). Python 3.14, standard library only
([`scripts/gapcal/README.md`](../scripts/gapcal/README.md)):

```sh
python3 -m unittest discover scripts/gapcal/tests     # offline, a few seconds (recorded mainnet txs)
DATA=~/.cache/shortwin-work/gapcal-data               # the default data directory; outside git
setsid nohup python3 scripts/gapcal/fetch.py crawl --data $DATA > $DATA/crawl.log 2>&1 &
python3 scripts/gapcal/fetch.py status --data $DATA   # offline: pid, rates, coverage per tier
python3 scripts/gapcal/fetch.py stop --data $DATA     # SIGTERM; it saves its state and exits
python3 scripts/gapcal/fetch.py replay --data $DATA --day 2026-10-02   # re-decode a day: 0 RPC calls
```

- **Resume** by running the same `crawl` command: it continues from `state.json` (listing cursors,
  the transaction queue, discovery cursors) and the universe.
- **Rate.** Two rate-limited threads, `fg` (polls, state, listings, discovery; 1.9 requests/s) and
  `bg` (transactions, the weekly census, ages; 1.0 requests/s); `--fg-rps` / `--bg-rps` override
  them and `fg + bg ≤ 3` is enforced. 429 and 5xx back off. Every `getTransaction` is cached.
- **Data.** Per UTC day `<data>/tapes/<day>/` (poll, trades, activity and events tapes, gzip'd and
  hashed into `manifest.json` when the day closes).
- Do not run `universe.py census` beside a live crawl: it refuses, since both together would exceed
  3 requests/s. The crawl runs its own weekly census.
- publicnode keeps about 28 h of signatures and transactions: an outage longer than that loses
  activity and trade history for good. CI does not run these tests.

## Audit package (M10)

`docs/AUDIT.md` (scope, threat model, reproduce path), `docs/audit/{scope.txt, known-issues.md,
findings.md}` and `SECURITY.md`. The scope file is generated:

```sh
python3 scripts/audit_scope.py write   # regenerate docs/audit/scope.txt from `git ls-files`
python3 scripts/audit_scope.py check   # exit 1 if it differs, or a watched file is in no unit
python3 scripts/audit_scope.py nloc    # AUDIT.md §2's table: files and non-test nLOC per unit
```

`check` fails when a tracked file under `programs/`, `crates/`, `services/` or a Cargo workspace
member is in no unit and not on the script's `EXCLUDED` list: a new crate or service is classified
in the script (a unit, or `EXCLUDED` with a reason) before `write`. `nloc` drops `#[cfg(test)]`
items (braces inside literals and comments are ignored) and counts the lines that are neither blank
nor comments; AUDIT.md §2's tokei column is tokei 14.0.0's "code" (`tokei <paths> -t Rust`).
The script puts each unit in one of four classes: `core` and `keeper` (the keeper core subset) are
in scope, `context` (the keeper rest and the governance tool with its Squads encoders) is read for
context only, and `out` is the TEST-ONLY mock venue code.

## Workspaces

- **Cargo, on-chain** (root `Cargo.toml`, resolver 3, `Cargo.lock` committed):
  `crates/shortwin-math`, `crates/shortwin-math-wasm`, `crates/shortwin-venue-state`,
  `programs/shortwin` (the straddle helper `programs/complete_curve` of M5 left both workspaces
  with pump-final P6, owner decision D2) and the program tests `tests/litesvm` (which share this lockfile so the
  Agave line LiteSVM resolves is pinned next to the program's, C1-14), and since M7 the
  invariant fuzzer `tests/fuzz` (it links the program tests, so it shares the lockfile too), and
  since M10 the host-only Squads v4 encoders `crates/squads-v4-ix` (the program tests and
  `services/ops` link it). The release profile
  keeps `overflow-checks = true` (Anchor refuses to build without it), `lto = "fat"` and
  `codegen-units = 1`. The workspace also holds the TEST-ONLY mock venue `tests/mock_cpamm`
  (an Anchor program, also a member of `Anchor.toml [workspace]`; never deployed, A49).
- **Cargo, services** (`services/Cargo.toml`, own `Cargo.lock`): host-only crates — the keeper
  and its chain client, and since M8 the indexer and API (`services/indexer`, Rust 1.94+ for sqlx
  0.9) with its TEST-ONLY replay harness (`services/indexer-litesvm`, which links the program
  tests `tests/litesvm`), and since M10 the governance tool `services/ops` (`shortwin-gov`)
  (ARCHITECTURE §1.2, §7). **Lockfiles:** the five bench workspaces m1–m6 also depend on
  `shortwin-litesvm-tests`, so a dependency added to the root workspace or `tests/litesvm` changes
  seven lockfiles (root, `services/`, `tests/bench/{m1,m2,m3,m5,m6}_cu`): refresh each with
  `cargo metadata --format-version 1 --manifest-path <manifest> >/dev/null` and re-run
  `cargo deny` / `cargo audit` where CI does.
- **Cargo, benches**: `tests/bench/cu_math`, `tests/bench/m1_cu`, `tests/bench/m2_cu`,
  `tests/bench/m3_cu`, `tests/bench/m5_cu` and `tests/bench/m6_cu` (each of the last three with
  its own TEST-ONLY CPI wrapper program), standalone workspaces with their own lockfiles (Mollusk
  0.15.1). `tests/bench/m7_keeper` and `tests/bench/m7_cascade` (M7) are output only, written by
  the services workspace's scenario tests.
- **`shortwin-math`** (ARCHITECTURE §2.2): `#![no_std]`, zero runtime dependencies,
  `#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation,
  clippy::cast_sign_loss, clippy::unwrap_used)]` (also on the program and
  `shortwin-venue-state`). The program additionally denies `clippy::expect_used`,
  `clippy::panic` and `unsafe_code` (AUD-10, M10; so did the `complete_curve` helper until
  1.0.0); `unsafe_code` does not fire inside Anchor's or
  `security_txt!`'s external macro expansions, and a test module that needs `expect` allows it
  as it allows `unwrap_used`. Every fallible function returns `Result<_, MathError>`, or `Option`
  where the API says a value is undefined. Every division takes an explicit `num::Rounding`,
  chosen per the ECONOMICS §2 rounding rule (in the pool's and lenders' favour). Module docs
  name the owning track and the ECON/ARCH sections. The program calls it for all arithmetic.
- **pnpm**: root `package.json` is private; workspaces per `pnpm-workspace.yaml`
  (`clients/js`, and since M9 `apps/web` and `packages/math-wasm`); `pnpm-lock.yaml` committed.
  Biome (`biome.json` at the root) covers `apps/web` and `packages/math-wasm`.

## Program id and keypair

The program id `5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK` is set in
`programs/shortwin/src/lib.rs` (`declare_id!`) and `Anchor.toml`. Its keypair is **not
committed**: whoever holds it can create the program at that address on any cluster. The
owner keeps it in the gitignored `keys/shortwin-keypair.json`; copy it to
`target/deploy/shortwin-keypair.json` before `anchor deploy`.

Without that file, `anchor build` prints a "Program ID mismatch" warning and writes a
throwaway keypair to `target/deploy/`. The built program is unaffected (the id comes from
`declare_id!`); `--ignore-keys` silences the warning, and CI uses it. Do not run
`anchor keys sync`: it would rewrite `declare_id!` to the throwaway key.

The straddle helper of M5 (`4B5mGykX9EWwaYdbTvsp8qXCUTbc7caQourz16Gat6LJ`) is retired and never
deployed (pump-final §11, owner decision D2): its id is in no manifest any more, and its keypair
(`programs/complete_curve/keys/complete_curve-keypair.json` in the owner's working tree) is no
longer needed.

**`initialize_config` on mainnet (OWNER-E2E-1, M11 deploy runbook).** As a legacy transaction
with a separate fee payer `initialize_config` is 1,294 B, over the 1,232-B limit (the Surfpool
E2E, `graduation_surfpool`). Send it as a **v0 transaction**, or let the upgrade authority pay its
own fee (1,198 B as legacy). If a Squads vault is the authority, check the size budget of its
wrapped transaction first. Nothing in the program changes.

## CI

`.github/workflows/ci.yml`, actions pinned to commit SHAs. It runs on pushes to `main`, on pull
requests and on demand; pushes to other branches run nothing (since the owner's decision of
2026-10-02, `m1-core` is fast-forwarded into `main` after each verified commit). **Actions
minutes:** the private repository's minutes cannot carry the nightly fuzz (6 shards × 5 h), so it is
opt-in (`gh variable set FUZZ_NIGHTLY --body true`) and long fuzz runs are local; one push to
`main` costs about 243 runner minutes (M10).

| Job | Runs |
|---|---|
| `fmt` | `cargo fmt --all --check` (workspace and the `cu_math` bench) |
| `clippy` | guard that `shortwin-math` keeps `#![no_std]`, the §2.2 denies and zero runtime deps; clippy `-D warnings` on `shortwin-math`, on the workspace, and on `shortwin` alone (the release feature set, without `test-venue`) |
| `test` | `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix` with `PROPTEST_CASES=2000` (M10: the Squads encoders' IDL pins) |
| `vectors` | re-runs every `tests/vectors/*/gen_*.py` with Python 3.14 and fails on any diff; T-CONFIG host part: `config_check.py` prints `OK` for the defaults, and `scripts/test_config_gate.py` |
| `deny` | `cargo deny check` (`deny.toml`) and `cargo audit` on the workspace and the six bench lockfiles |
| `wasm` | wasm32 release build of `shortwin-math` and `shortwin-math-wasm` |
| `cu-math` | T-CU-MATH bench (`tests/bench/cu_math`) |
| `anchor-build` | Agave 4.1.2 + Anchor CLI 1.2.0, `anchor build --tools-version v1.57 --arch v3 --ignore-keys` (shortwin and the mock; the `complete_curve` helper left with pump-final P6) and `scripts/build-test-venue.sh`; asserts SBPF v3 on the artifacts; A49: the release `shortwin.so` carries neither the test-venue marker nor the mock id, the test-venue build and `mock_cpamm.so` do; M10 SECTXT-1: `scripts/test_check_security_txt.py`, then `scripts/check-security-txt.py --forbid-placeholders --source-release v<Cargo version>` on the release ELF (exactly one valid security.txt with the owner's values), and none in the test-venue build; uploads the `.so` files, the IDL and the TS types |
| `verifiable-build` | M10: `scripts/verifiable-build.sh --compare` in Docker — solana-verify 0.5.2 in the pinned image (`SVB_IMAGE_DIGEST`), `--arch v3 --cargo-build-sbf-args="--tools-version v1.57"` — must be byte-identical to the `anchor-build` artifact for `shortwin`; only that image used; A49 on the verifiable artifacts; the file and executable hashes in the job summary and the `verifiable-build` artifact |
| `program-tests` | on the `anchor-build` artifact: fixture checksums (`SHA256SUMS` and `scripts/dump-fixtures.py check`), `cargo check -p shortwin --target wasm32-unknown-unknown` (T-LAYOUT), `cargo test -p shortwin` on the release feature set, `cargo test -p shortwin -p shortwin-venue-state -p shortwin-litesvm-tests` (incl. every Raydium and pump fork binary, `synthetic_exit_it`) |
| `codama` | IDL copy and generated client are current; `tsc`; `node --test` (T-LAYOUT Codama leg) |
| `web` | M9: wasm-bindgen 0.2.129, `@shortwin/math-wasm` build, then `apps/web`'s Biome check, type check, Vitest and production build (FRONTEND.md §10.4) |
| `web-e2e` | M9: Playwright (profile `chain`) on `solana-test-validator` with the `anchor-build` artifact's test-venue `shortwin.so` and mock (since 2026-10-15 not the `complete_curve` helper, pump-final D2) plus the dumped venues, the seeded scenario, the production build and the injected test wallet; every spec (incl. the §10.3 CPI Guard and memo exit tests, axe on every page); the `screens` PNGs uploaded always, report, traces and logs on failure |
| `m1-cu` | M1 CU baseline bench (`tests/bench/m1_cu`) within budget |
| `m2-cu` | M2 CU bench and T-TRACE (`tests/bench/m2_cu`) within budget and 64 trace entries |
| `m3-cu` | M3 CU bench, T-TRACE and stack depth on Raydium (`tests/bench/m3_cu`): builds the TEST-ONLY CPI wrapper (SBPF v3 asserted), every Raydium-path instruction (M4's `propose_pool` / `execute_pool_switch` included) within budget, the worst-case transactions at their exact trace counts, every path through 0–3 wrapper levels |
| `m5-cu` | M5 CU bench, T-TRACE, the T-SIZE preview and stack depth on the pump curve (`tests/bench/m5_cu`): builds its TEST-ONLY CPI wrapper (SBPF v3 asserted), every curve-path instruction within ARCH §6.1's PROPOSED limits, the worst-case curve transactions at their exact trace counts and v0 + ALT sizes, the synthetic exits and the keeper's `migrate_v2` after an exact and after a synthetic completion, every path through 0–3 wrapper levels |
| `m6-cu` | M6 CU bench, T-TRACE, T-SIZE, T-DEPTH and the graduation transition on PumpSwap (`tests/bench/m6_cu`): its TEST-ONLY CPI wrapper, every PumpSwap-path instruction within budget, the worst-case transactions at their exact trace counts and v0 + ALT sizes |
| `t-pot` | T-POT: every `pool_model` trace replayed through the program in release (`lender_it traces::`, `SHORTWIN_TRACES=full`) |
| `services` | the services workspace: fmt, clippy (the indexer crates included; also the keeper without `test-venue`), cargo-deny, cargo-audit, the keeper's and (M10) `shortwin-gov`'s unit tests, T-OPS (`gov_e2e`: the governance sequence through the tool's command line on the mainnet Squads ELF) and the keeper's LiteSVM scenarios (mock, Raydium and pump fork; M7's degraded-mode drill, duty 11, the PumpSwap tables) on the `anchor-build` artifacts, and the A49 check on the release keeper binary |
| `indexer` | M8: the indexer's tests on an embedded PostgreSQL 17.11 (zonky binaries cached): the fixture gate, `-p shortwin-indexer` (decoders, ingest on the recorded sessions, analytics, the API contract on the seed and on replayed sessions, limits, WebSocket), the replay harness `-p shortwin-indexer-litesvm` on the `anchor-build` artifacts (the M8 exit criterion), the RPC paths against `solana-test-validator`, no `postgresql_embedded` in the production dependencies, and `docker compose config` on the reference deployment |
| `t-size` | T-SIZE on Raydium (`--test tx_size_raydium`), on the pump curve (`--test tx_size_pump_it`) and on PumpSwap (`--test tx_size_pumpswap_it`), the size tables in the log, and the protocol ALT's static part (`scripts/build-alt.sh print`, uploaded as an artifact) |
| `fuzz-smoke` | M7: `cargo test --release -p shortwin-fuzz` — the fuzzer's fixed-seed smoke on every profile, the corpus replays (a reproducer marked fixed must pass) and its self-tests; crash files uploaded on failure |
| `m7-keeper-bench` | M7: the keeper's cost inputs, the PumpSwap 4-extras T-SIZE and T-CASCADE in release (`m7_cost::`, `pamm_alt::`, `cascade::`; the tests' own gates), the regenerated `tests/bench/m7_*` compared with the committed ones (a warning) |
| `fuzz-nightly` | M7: on the `schedule` trigger (main only) **only while the repository variable `FUZZ_NIGHTLY` is `true`** (owner decision of 2026-10-02, `4cb7729`), or on `workflow_dispatch` with `fuzz_duration`: one 5 h `shortwin-fuzz run` shard per profile, failures minimised and uploaded, the job failing on any crash; every other job skips the `schedule` trigger (`anchor-build` runs for it only when the variable is set) |
| `mutants` | M7-SEC-6: on the `schedule` trigger (main, only while `FUZZ_NIGHTLY` is `true`) or `workflow_dispatch`: `scripts/mutants.sh` — the LiteSVM tests and the fuzzer built once from the checkout (the oracle), then each of the M7 security review's seven one-line program mutants built and run against its named tests and a 3-minute fuzz of its profile; a mutant that survives either fails the job (a missing oracle) |
