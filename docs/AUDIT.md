# short.win audit package

This is the auditors' entry point to short.win's on-chain code. It says what is in scope, how
to build and test the code, what the code relies on, and what we already know about it.

**Status:**
- This is the M10 final package (WP-10 final), written on the merged M10 tree (`m1-core`,
  §0). Every M10 work package has landed: program code is final for the audit (batch 4, then
  the KI-65 program batch of 2026-10-07 — no rebase orphans, known-issues KI-65 — and its
  follow-up of the same day — conversions through a rebase record are owner-signed,
  KI65-REV-SEC1 —, then the KI-66 batch — anyone else's `sync_lender` of a lender behind a
  rebase record is the pot-only touch, owner decision of 2026-10-07, seventh round; the
  release ELF `42b11bc4…` —, then the KI-66 full / KI-67 batch of 2026-10-08 (eighth round:
  the pot-only touch walks the lender's forward chain, `claim_sol` closes an empty position;
  `m10/ki67`, new hashes in §10's gate table; its full gate on the batch's head `36f43cc`,
  web included, is green and the verifiable build reproduces the hashes of §0 there)).
- The ≥ 24 h fuzz run and CI were clean on batch 4's head `4f4849e` (§10). The KI-65 batch,
  its follow-up, the KI-66 batch and the KI-66 full / KI-67 batch changed the program after
  them: their local gates are green (§10), and CI passed on the head that carries them,
  `55b3e23` (run 37816833577, 2026-10-08, as the owner's ninth round records it; §10). **The
  ≥ 24 h fuzz on the final program bytes is done** (release `4d19c582…`; §10 "Fuzz", the owner's
  seventh round asked for it): run C on `f48e75d` and run E on `9861c5a` (the fixed fuzzer) were
  clean, 0 failures and 0 anomalies; run D on `55b3e23` held every program invariant and found
  two fuzz-oracle bugs, FUZZ-24D-A and FUZZ-M10-24H-D-B, fixed in `9861c5a` (known-issues §6).
- After the program: the FE-30 batch (the web and the indexer), the fuzzer fix `9861c5a`, the
  M10-RESP batch (`acbe323`, phone and tablet layouts), the round-13 UX batch (`7a64dcd`) and the
  lint fix `7b2121d` — the web, the indexer, the fuzzer and docs only, the program bytes unchanged
  (§0, §10). **CI passed on `7b2121d`** (run 38125079869, 2026-10-11: all 24 jobs a push runs;
  the first push's run failed only the web lint, which `7b2121d` fixed; the commits after it are
  docs only; §10).
- **Tagged `audit-1` / `v1.0.0` on `e4895c2`** (2026-10-11, owner decision; §0). What is still
  pending is named where it applies: the owner's review of this package (§0), the open owner
  items (§7), the calibration results (KI-14) and the external audit itself (§12).
- **After the tag, the AI review of v1.0.0** ([`audit/ai-review/`](audit/ai-review/README.md);
  not a formal audit): 17 issues and 7 confirmed leads (AIR-01 … AIR-24), fixed by a program
  batch after the tag (with the math crate, the normative sims and vectors, the keeper and the
  indexer) or disclosed. The owner decided its open points on 2026-10-12 and confirmed the
  rest on 2026-10-13 (AIR-21's S1 vest, AIR-09's zero token value, AIR-14's widening of ECON
  §12.12 residual 10; §7); its residuals are known-issues KI-69 … KI-76 (and sentences in older
  rows), and the register's §7 maps every finding. Its follow-ups (branch `air-followups`,
  2026-10-12/13: `SolSettle.vest_flushed`, the keeper, the indexer, the web, the IDL and client,
  the fuzz corpus and the docs) change the program once more (§0's AI-review build). The fixes
  and their follow-ups have passed local runs of the CI gate (the latest on 2026-10-13, §10)
  but have not yet been through CI or an external fix review; the owner waived the ≥ 24 h fuzz
  run for this batch on 2026-10-13, so none was run on its program (§12).
- **After the AI review: short.win 1.0.1, pump v3** (branch `pump-v3`, 2026-10-13 … 16; the
  design `pump-final`, the owner's decisions D1 (a) … D4 of 2026-10-13; §13). pump redeployed
  three times after the tag; the curve adapter now trades pump's v3 instructions, a position past
  the curve's `rT` exits in one instruction by pump's synthetic migration, a stuck synthetic
  migration is priced on the pool `migrate_v2` creates, and the straddle helper `complete_curve`
  is retired (D2: **an audit-scope change the auditors must be told of**, with this addendum).
  The build's hashes (§0) are a **local build, not verified**: no verifiable build (its tools are
  not installed on the machine that built them, and that session's rules allowed no network
  beyond read-only Solana RPC to fetch them: §0) and no CI run; a local run of every CI job that
  runs on that machine passed on 2026-10-16 (§10 "The pump v3 gate").
- Normative sources:
  - the design documents `docs/SPEC.md`, `docs/design/ARCHITECTURE.md` (ARCH) and
    `docs/design/ECONOMICS.md` (ECON);
  - this file;
  - `docs/audit/`.
- Section numbers like "ARCH §8.3" refer to those documents.

| File | What it holds |
|---|---|
| `docs/AUDIT.md` | this guide |
| [`docs/audit/scope.txt`](audit/scope.txt) | the in-scope files, generated from `git ls-files` (`scripts/audit_scope.py`) |
| [`docs/audit/known-issues.md`](audit/known-issues.md) | the known-issues register (KI-01 …): accepted and disclosed residuals, each with its bound |
| [`docs/audit/ai-review/`](audit/ai-review/README.md) | the AI-assisted review of v1.0.0 (not a formal audit): its issues and leads (AIR-01 … AIR-24), their fixes and regression tests, and the owner's decisions of 2026-10-12 and 2026-10-13 |
| [`docs/audit/findings.md`](audit/findings.md) | the findings tracker (the auditors' findings and their resolution) |
| [`docs/audit/README.md`](audit/README.md) | the folder index and the RFQ log |
| [`SECURITY.md`](../SECURITY.md) | the vulnerability disclosure policy |

---

## 0. Commit, ids, hashes, contacts

| Item | Value |
|---|---|
| Audit commit / tag | **`e4895c2`, tagged `audit-1` / `v1.0.0`** (2026-10-11), the head of `m1-core` at the tag: the M10 tree (`98b1181`, batch 3 from `391aed3`, the doc sync and this package; program batch 4 — the program change `1e4a273`, `3b33ab7` bytecode-neutral — with its docs and gate up to `4f4849e`), then the final tree's fuzz fixes `997c730` and `945d45a` (`tests/fuzz` and one test-harness doc comment only) and this evidence sync; then the **KI-65 program batch** (branch `m10/ki65`, 2026-10-07: `ShareEpoch.conv_rem`, cumulative conversion rounding; its program commit `3c7c689` on `m10/ki65`), then the **KI-65 follow-up** (branch `m10/ki65b`, 2026-10-07: owner-signed conversions through a rebase record, `sync_lender`'s `owner` account and `RebaseNeedsOwner`, KI65-REV-SEC1; its program commit `79cacd9` on `m10/ki65b`), then the **KI-66 batch** (branch `m10/ki66`, 2026-10-07/08: the pot-only touch, `pool::touch_pot` and `sync_lender`'s path selection, owner decision of 2026-10-07, seventh round; its math and program commits `e639558` and `e19e0c8` on `m10/ki66`), then the **KI-66 full / KI-67 batch** (branch `m10/ki67`, 2026-10-08: the forward chain, `claim_sol`'s close, owner decisions of the eighth round; its math and program commits `6aad5a5` and `f47b0d8` on `m10/ki67`; the ids change if the branches are rebased at integration), then the **FE-30 batch** (branch `m10/fe30`, 2026-10-08: the web and the indexer only, owner decisions of the ninth round; the program bytes unchanged), then the fuzzer fix `9861c5a` (run D's two oracle findings, FUZZ-24D-A and FUZZ-M10-24H-D-B: `tests/fuzz` and docs), then the **M10-RESP batch** (m1-core `acbe323`, 2026-10-09/10: phone and tablet layouts, the desktop terminal's fixes; the owner's twelfth round; approved as built in the thirteenth round, 2026-10-10) and the **round-13 UX batch** (m1-core `7a64dcd`, 2026-10-11: O-1 lend order, O-2 coverage notice once, O-3 newcomer help, O-4 12 px floor, O-5 Claim wording; its 143 new strings and the `borrower.liquidation` ¶3 amendment approved by the owner on 2026-10-11) — the web only; then the evidence sync `04c5236` (docs), the lint fix `7b2121d` (the web's lint configuration, M10-CI-LINT) and the CI record `e4895c2` (docs). The program bytes are those of the KI-66 full / KI-67 batch. **Tagged on 2026-10-11** (the owner's decision of that day; owner decision of 2026-10-02: release 1.0.0 at the audit tag; post-audit fixes are 1.0.x); its program hashes are the table below (CI run 38125079869's verifiable build on `7b2121d`; `e4895c2` changes only docs) |
| Package version | At the tag `shortwin` and `complete_curve` were both 1.0.0 (`programs/*/Cargo.toml`; `security.txt`'s `source_release` `v1.0.0`). **Since 2026-10-16 `shortwin` is 1.0.1** (`source_release` `v1.0.1`; the owner's 1.0.x rule; §13) and `complete_curve` is retired |
| `shortwin` program id | `5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK` (`programs/shortwin/src/lib.rs` `declare_id!`) |
| `complete_curve` program id | `4B5mGykX9EWwaYdbTvsp8qXCUTbc7caQourz16Gat6LJ` (`programs/complete_curve/src/lib.rs`) |
| TEST-ONLY mock venue | `BPAZFPkmVtYEWEZHCzFUns7oybh1VWfAvmu3ofM2mQi7` (`tests/mock_cpamm`; never deployed; out of scope, §2) |
| Toolchain | Anchor CLI and `anchor-lang` 1.2.0; Agave CLI 4.1.2; platform-tools v1.57 (rustc 1.95), SBPF v3; host Rust 1.98.1 (`docs/DEVELOPMENT.md` "Pins") |
| Build | `anchor build --tools-version v1.57 --arch v3 --ignore-keys` (never a bare `cargo build-sbf`; never `anchor keys sync`, which would rewrite `declare_id!` and so the hashes below) |
| Verifiable build | solana-verify 0.5.2, image `solanafoundation/solana-verifiable-build@sha256:2e0b78f44ee76612e9260c7c988570c5e14de6fbd93e0ab07115ec7054473b4f`, `--arch v3 --cargo-build-sbf-args="--tools-version v1.57"`: `scripts/verifiable-build.sh` (Docker, or `--local`: a bubblewrap shim running the same digest-pinned image) and CI's `verifiable-build` job, which fail unless each output is byte-identical to the `anchor build` artifact (ARCH §1.1) |
| Contacts | Security reports: `security@short.win`; policy page `https://short.win/security` (`SECURITY.md`; the same values are in the program's `security.txt` (and were in `complete_curve`'s until 1.0.0) and the web's `/.well-known/security.txt`; owner decision Q3 of 2026-10-02). The auditors' point of contact is named at the engagement (§12) |

**Retired after the tag: `complete_curve`** (design pump-final §11, owner decision D2 of
2026-10-13; the auditors are to be told of this scope change). Since pump's v3 build a
short.win exit past the curve's `rT` buys `rT` on the curve and the rest from the would-be pool
in one instruction, so the straddle bundle and its helper have no caller, and the helper was
never deployed. Since PUMP-V3 P6 it is out of the workspace (`anchor build` builds `shortwin`
and the TEST-ONLY `mock_cpamm`), the release scripts and CI (`verifiable-build.sh`,
SECTXT-1, A49), Governance's upgrade set (`shortwin-gov` refuses a layout that names it), the
committed IDLs and the indexer; `scripts/audit_scope.py` no longer lists it (`scope.txt`). The
`complete_curve` rows of this section and §1–§4, §9's reproduce commands (run on the tag) and
§11's "1.0.0 only" focus item describe the audited 1.0.0 tree and stay as its record
(`git show v1.0.0:programs/complete_curve`).

**Verified hashes of this tree.** `scripts/verifiable-build.sh --local --compare` on `m10/ki67`
(2026-10-08, commit `36f43cc`, the batch's head with the web batch; its `verifiable-hashes.txt`
names the commit; first at `25e95f2`, the same bytes): solana-verify 0.5.2 in the pinned image
built both programs from a fresh clone of `HEAD`, and each output is byte-identical to the
`anchor build` artifact the tests ran on (the full gate's clean build, §10). The hashes:

| Program | sha256 of the `.so` file | executable hash (trailing zeros trimmed; the on-chain comparison) | bytes |
|---|---|---|---|
| `shortwin.so` | `4d19c582f399f4a217a6d24725ad5dc3cb6b6308ff346a8f24312d41a01c1f75` | `52a29a5155503e56f1592a722651925b6f5bd55fdf1e8f2b1bc3be35eb78186b` | 1,609,616 |
| `complete_curve.so` | `f3a47201738fd72d4dd02f756ba3c17f241dc2bedde1c9d25f63eccaaafc6d42` | `2f2ef7da92c7b8dffc57bd7ae9bf33f64c8dbc09dc552c401742ad85f45382ab` | 122,264 |

- The KI-66 full / KI-67 batch changed `shortwin` only (+20,720 B); at the KI-66 batch (`m10/ki66`,
  verified at `45327c1`) it was `42b11bc4…60cd` / `59e5df19…7930`, 1,588,896 B. The KI-66 batch
  changed `shortwin` only (+5,272 B); at the KI-65 follow-up (`m10/ki65b`,
  verified at `9612471`) it was `cdbd2e70…c50a` / `d4f938a8…386f`, 1,583,624 B, which changed
  `shortwin` only too (+1,600 B); at the KI-65 batch (`m10/ki65`,
  verified at `b48dbd7`) it was `386c5363…2895` / `92860b40…e6f3`, 1,582,024 B, which changed
  `shortwin` only too (+6,808 B); at batch 4 (`4f4849e`, verified on `m10/batch4` at `f08426d`,
  2026-10-04) it was `bbef1075…f289` / `3214661b…ee09`, 1,575,216 B. The test-venue build is
  `8cea17e1…bb98` (the KI-66 batch: `9296be90…542a`; the KI-65 follow-up: `0d8cc621…a1fd`; the KI-65 batch: `d5b313d3…6ca5`;
  batch 4: `b32f85a0…3e5b`).

- Both ELFs have `e_flags` 0x3 (SBPF v3) and carry exactly one `security.txt` (§10).
- Program batch 4 (PUMP-2026-10-1: PumpSwap's fee buckets) changed `shortwin` only; at batch 3
  (`391aed3`) it was `c4398d01…39ea` / `42f476fe…200b`, 1,574,648 B. `complete_curve` is
  unchanged since batch 3. ARCH §9 records both.
- The executable hash is what `solana-verify get-program-hash` and verify.osec.io compare with
  the deployed ProgramData. Verifying the deployment on chain is M11's (ARCH §9 M11).
- At the skeleton base `e857a2a` the hashes were `00011844…e1a1` / `117392ce…e469` and
  `1970661a…2bd` / `8cbeaea8…6f43`. Batches 1–3 changed both programs (`security.txt` and the
  version), and `shortwin` also by SQ-G8, M10-8C-1 and the extras denylist; batch 4 changed
  `shortwin`'s PumpSwap adapter.

**After the tag: the AI review's branch** (`air-followups` at `621f8e1`, 2026-10-13: the fixes of
`ai-review-fixes` and their follow-ups; §10's latest gate). A local `anchor build` with the
pinned toolchain gives the bytes below. **They are not verified:** no verifiable build ran
(Docker is not available on the machine that built them) and no CI run, so they are the
proposed fixes' bytes, not release hashes:

| Program | sha256 of the `.so` file | executable hash (trailing zeros trimmed) | bytes |
|---|---|---|---|
| `shortwin.so` | `fba9dd7567512dfcd96d81290a8d67ee10e8601afe232c3bce8d403b3c4fdf55` | `9e4f5ea5f805a16efbd9be2ab9bbaec3a15b74b3fe435cb031e55d7259ceb4b8` | 1,634,936 |
| `complete_curve.so` | `f3a47201738fd72d4dd02f756ba3c17f241dc2bedde1c9d25f63eccaaafc6d42` | `2f2ef7da92c7b8dffc57bd7ae9bf33f64c8dbc09dc552c401742ad85f45382ab` | 122,264 |

- `shortwin` grows by 25,320 B over v1.0.0; `complete_curve` is byte-identical to v1.0.0's. The
  test-venue build is `63ee71aa…b05c` (1,670,064 B) and `mock_cpamm.so` `4cf17007…7ab9`
  (281,240 B, unchanged). Both release ELFs have `e_flags` 0x3 and pass A49 and the
  `security.txt` check (`--source-release v1.0.0`).
- The program last changed in `cb756ca` (`SolSettle.vest_flushed`, AIR-07); the AI review's
  README records the build before it (`eafc9833…`). No later commit changes program source, and
  builds at `63b673c` and at `621f8e1` gave the same bytes.
- The package version is still 1.0.0. A release of these fixes as 1.0.x (owner decision of
  2026-10-02) changes the version, which `security.txt`'s `source_release` carries, and so
  these hashes.

**After the tag: short.win 1.0.1, pump v3** (branch `pump-v3`, 2026-10-16: the AI review's fixes
above, then pump-final P0 … P8; §13). `anchor build --tools-version v1.57 --arch v3
--ignore-keys` and `scripts/build-test-venue.sh` with the pinned toolchain, on `dd24af1` (the
version bump; no later commit changes program bytes: `venues/mod.rs`'s doc comment of
2026-10-16 keeps every line number, and a rebuild gave the same bytes). **A local build, not
verified:** no verifiable build ran and no CI run. The machine that built it has no Docker, and
the `--local` mode (no Docker daemon) lacks its prerequisites there too: solana-verify 0.5.2,
bubblewrap and the extracted image are not installed. The session's rules allowed no network
beyond read-only Solana RPC, so none of them could be fetched, nor the image from Docker Hub or
the crates and platform-tools v1.57 the build downloads.
`scripts/verifiable-build.sh --local --compare target/deploy` (pump-final §15 P8; or Docker
mode, or CI's `verifiable-build` job) is still to run. These are the bytes to verify, not yet
release hashes:

| Program | sha256 of the `.so` file | executable hash (trailing zeros trimmed) | bytes |
|---|---|---|---|
| `shortwin.so` 1.0.1 | `72c82879accac8323af6ef0ce777baea6b40c1ce312cefaf65aaa152a3f21429` | `0610d4196b645c6e5a3130184835289d290e07f8ec239d52bbf427f77ee62dfc` | 1,635,424 |

- The TEST-ONLY test-venue build is `fd46da2c6b907486fe0021a06b5a075965b0db0d59981749bd4ed85b2ca89db7`
  (1,666,464 B; 1.0.0's was `8cea17e1…bb98`, and the version bump alone moves it: below), and
  `mock_cpamm.so` is `4cf1700761782aece1a4e3463ce0b2b9f2afaed2ecf3d0a6362f115faf9c7ab9`
  (281,240 B, **unchanged** since 1.0.0). `e_flags` 0x3 on all three; A49 ok (`check-a49.py`:
  the release ELF carries neither the mock id nor the test-venue marker; `mock_cpamm.so` and the
  test-venue build carry the mock id, and the test-venue build the marker);
  `check-security-txt.py --forbid-placeholders --source-release v1.0.1`: ok, exactly one
  security.txt in the release ELF and none in the test-venue build.
- Deterministic: the same tree with the version set back to 1.0.0 gives `b3130ae2…8e31` /
  `1f10334c…6553` (the build pump-final P2 … P7 tested and measured, 1,635,424 / 1,670,584 B),
  and two recompiles at 1.0.1 gave the bytes above. The version is the only difference between
  the two; it enters `security.txt` and the crate's metadata hash, which moves the test-venue
  build's layout (it compiles security.txt out).
- `shortwin` grows by 25,808 B over v1.0.0 (`4d19c582…`, 1,609,616 B) and by 488 B over the AI
  review's build (`fba9dd75…`, 1,634,936 B). `complete_curve` is no longer built (retired, D2).
- Gates on these bytes: `dump-fixtures.py check --live` at slot 457,523,244 (2026-10-16
  05:38 UTC): every pinned program and account matches mainnet, the venue ELFs §13 lists
  included; `python3 scripts/audit_scope.py check`: ok.

---

## 1. The protocol on one page

short.win is permissionless peer-to-peer token lending on Solana, built for shorting memecoins
(SPEC §1).

- **Lenders** deposit tokens into a per-mint market and earn rent (SPEC §2.4).
- **Borrowers** open a short in one atomic transaction (SPEC §2.1). They post SOL margin, and
  the program borrows `q` tokens from the market's `pool_vault` and sells them on the token's
  venue. The proceeds and the margin stay on the position account.
- **Exits** buy the tokens back on the venue and return them to the vault:
  - close (by the borrower);
  - liquidation (permissionless, when the position is under its CR, SPEC §5);
  - expiry (at the tier's maximum duration).
- **SOL settlement.** When a buyback is impossible or unsafe, a position settles in SOL to the
  lenders under conditions S1–S3 (SPEC §5.3, ECON §8.6).

**Venues.** Every venue is a constant-product pool, priced with exact on-chain math (ARCH §0,
§4):

| Venue | Used for |
|---|---|
| pump.fun bonding curve | coins still on the curve |
| PumpSwap | the canonical pool after graduation |
| Raydium AMM v4 and CPMM | other tokens; the program adopts a pool by a permissionless rule, ARCH §2.6 |
| `None` | list-only markets (deposits and withdrawals only) |

A market moves between venues on graduation and on Raydium pool switches (ARCH §2.5).

**Key mechanisms:**
- **One external signer per market.** A system-owned, zero-data `trader` PDA is the only key
  the program signs with towards a venue. Every leg first normalises the trader's state,
  because anyone can donate to it (ARCH §2.4).
- **Manipulation resistance.** The protocol observes prices in per-second buckets. It keeps an
  EMA with an 8-entry ring, a liquidation persistence rule, and a fixed 2 % bound over a
  manipulation-resistant reference on every keeper buyback. Every settlement decision must
  come out the same on a "settlement box" of EMA and ring medians (ARCH §0 point 3, ECON §5).
- **Lender accounting.** Rent accrues continuously, with O(1) indices. One-off lender income
  vests over 6 h. Withdrawal locks expire with the positions they stand for (ECON §9).
- **Admin.** Governance changes parameters through a timelock (48 h by default) and only
  within compiled bounds. The Emergency multisig has no timelock: it can pause opens, disable
  a venue for opens, revalidate a venue after an upgrade, set a route-broken flag, publish
  bounded extra-account templates, and clear (never set) a keeper coverage pause. **No admin
  instruction can move pool or position funds** (I4; ARCH §3.1 "What admins cannot do").
- **Venue guard.** A permissionless latch records a venue program upgrade. New opens on that
  venue are refused until the Emergency multisig revalidates it; exits never check the guard
  (ARCH §3.1). Since AIR-01 a ProgramData extension alone does not count as an upgrade once a
  revalidation has recorded the venue's ELF fingerprint (known-issues KI-69).
- **Keepers.** Anyone can run the exits and the price cranks. The in-house keeper covers
  markets with open positions, within a global daily budget. A market it cannot cover has its
  opens paused through the `coverage_authority` (degraded mode; ARCH §7.1, ECON §5.8).
- **`complete_curve`** (1.0.0; **retired** after the tag, §0). A stateless helper that completed
  a pump curve inside a straddle bundle. It had its own signer, because short.win never forwards
  a user's signature to a venue (ARCH §5.4).

---

## 2. Scope

The in-scope file list is [`docs/audit/scope.txt`](audit/scope.txt). Regenerate it at the tag
with `python3 scripts/audit_scope.py write`. `python3 scripts/audit_scope.py check` fails in
two cases:
- the list and `git ls-files` differ;
- a tracked file under `programs/`, `crates/`, `services/` or a Cargo workspace member is
  in no unit and not on the script's explicit `EXCLUDED` list (listed at the end of
  `scope.txt`).

So a new program, crate or service fails `check` until a reviewed edit of the script puts it in
a unit or excludes it with a reason. On this tree WP-7's `crates/squads-v4-ix` and
`services/ops` (`shortwin-gov`) are classified as the `governance` context unit.

**What is in scope** (owner decisions of 2026-10-02, Q2 and "Audit keeper scope"): the on-chain
core and the keeper's core subset (its coverage budget, liquidation and settlement duties, and
transaction building; ≈ 4.7k non-test nLOC). Line counts on this tree (`m10/ki67`, the KI-66
full / KI-67 batch; release ELF `4d19c582…`; the tokei column is the KI-66 batch's, tokei was not
available for this batch — its core rows grow by about the nLOC delta):

| Class | Unit | `.rs` files | tokei 14.0.0 "code" | non-test nLOC |
|---|---|---|---|---|
| core | `programs/shortwin` (without the TEST-ONLY `venues/mock.rs`) | 70 | 16,245 | 13,293 |
| core | `programs/complete_curve` (1.0.0; retired after the tag, §0: not in `scope.txt` since PUMP-V3 P6) | 1 | 293 | 213 |
| core | `crates/shortwin-math` (`src/`) | 25 | 10,506 | 6,593 |
| core | `crates/shortwin-venue-state` (`src/`, without the TEST-ONLY `mock_cpamm.rs`) | 10 | 3,772 | 1,636 |
| core | build pins: `Cargo.toml`, `Cargo.lock`, `Anchor.toml`, `rust-toolchain.toml` | — | — | — |
| **core** | **total** | **106** | **30,816** | **21,735** |
| **keeper** (in scope) | keeper core: exit and crank decisions, coverage and budget, the instructions and account lists it sends (`decide`, `coverage`, `budget`, `fees`, `graduation`, `ixs`, `venue`, `view`, `pda`, `sweep`, `monitor`, `pool_monitor`; `services/common` `tx`, `chain`) | 14 | 7,759 | **4,692** |
| context | keeper rest: the loop (`keeper.rs`, 3,231 non-test nLOC), config, metrics, alerts, the RPC backend, the ALT and v1-probe tools | 12 | 6,316 | 5,821 |
| context | governance: `shortwin-gov` (`services/ops`) and its Squads v4 encoders (`crates/squads-v4-ix`); host-only, never on-chain | 16 | 4,661 | 4,124 |
| out | TEST-ONLY mock venue code (`venues/mock.rs`, `mock_cpamm.rs`, `tests/mock_cpamm`) | 6 | 1,422 | 1,188 |

Since the M10 skeleton (`e857a2a`: core 21,117 nLOC), the three program batches added 126
non-test nLOC to the core: `security.txt` and the AUD-10 lints (batch 1), SQ-G8, M10-8C-1 and
the version (batch 2), and the extras denylist (batch 3). The KI-65 batch adds 177 more
(`programs/shortwin` +45, `crates/shortwin-math` +132: `conv_rem`, `store_records`, the
commit pass; batch 4's tree counted 21,268), and its follow-up 19 more (`programs/shortwin`:
the `owner` account, `reads_rebase` and its check; tokei "code" +80 there, inline tests
included, counted with tokei 12.1.2 on the changed files). The KI-66 batch adds 61 more
(`programs/shortwin` +5: the path selection; `crates/shortwin-math` +56: `touch_pot`, the
`Hold` pass, the shared touch tail; tokei 14.0.0 "code" +19 / +56). The keeper core is unchanged since
the skeleton; batch 3's `DeniedAccount` fallback is in `keeper.rs` (keeper rest).

**How the lines were counted:**
- **tokei "code"**: tokei 14.0.0 over each unit's `.rs` files (`tokei <files> -t Rust`, the
  file list from `audit_scope.UNITS`). It counts non-blank, non-comment lines and keeps the
  inline `#[cfg(test)]` modules.
- **non-test nLOC**: `python3 scripts/audit_scope.py nloc`. It drops every `#[cfg(test)]` item,
  then counts the same way. The item's end is found by counting braces outside string, char
  and comment text. The script warns when a braced item does not close at its attribute's
  indentation; it gives no warning on this tree.
- The M10 audit-preparation research counted ≈ 21.4k on-chain at `e857a2a` (shortwin 13,702
  with the mock adapter; math 6,193; venue-state 1,686). The script does better on two files:
  - `mint_filter.rs`: the research kept a test module whose attribute spans several lines
    (+263 lines);
  - `u256.rs`: the research kept a test-only `static` (+3 lines).

  It also excludes the mock adapter.
- The research's "keeper subset ≈ 4.8k" had no file list. The keeper-core unit above (4,692)
  is the subset the owner put in scope.

**Out of scope** (read for context only):
- the frontend (`apps/web`), the indexer and API (`services/indexer*`), the generated client
  (`clients/js`) and the WASM facade (`crates/shortwin-math-wasm`, `packages/math-wasm`);
- the keeper outside its core subset, and the governance tool `shortwin-gov` with
  `crates/squads-v4-ix` (the `context` units above; members review and sign in the Squads app,
  the tool only stages and verifies: owner decision of 2026-10-03);
- the tests (`tests/**`, `crates/*/tests`, `services/*-litesvm`), the simulations
  (`docs/design/sim`) and the scripts;
- the external programs. short.win calls or reads them, and the fork tests run their dumped
  mainnet ELFs (`tests/fixtures/manifest.json`):
  - pump.fun, PumpSwap and pump-fees;
  - Raydium AMM v4 and CPMM;
  - SPL Token (p-token), Token-2022, the ATA program and spl-memo;
  - the Pyth SOL/USD feed;
  - Squads v4 (trusted, not reproduced: KI-59).

  Their pins (deploy slots, trimmed ELF hashes, sources) are in `tests/fixtures/README.md` and
  ARCH §1.1; the full trimmed-ELF hashes are `scripts/dump-fixtures.py`'s pins and
  `tests/fixtures/manifest.json`'s `elf_trimmed_sha256` (the fixtures README shortens them and
  gives none for AMM v4). M10 added the CPMM redeploy (`raydium_cpmm_2026_10`, slot
  452,376,460), the mainnet Token-2022 (v11.0.0, `7ea94a02…`) and spl-memo v3 (`f520eaf0…`),
  and Squads v4 (slot 302,582,236, `d4866083…2fac`), and the pump venues' 2026-10-02 redeploys
  (`pump_programs_2026_10`: pump 452,654,932 `b0b777c6…`, PumpSwap 452,654,882 `01557035…`,
  pump-fees 452,655,002 `f166c296…`; the `VenueGuard` baseline `VENUE_GUARD_SLOTS_2026_10_03`).
  `python3 scripts/dump-fixtures.py check --live` reports
  a pinned program that has moved on mainnet.

**The TEST-ONLY code is compiled out of the release ELF:**
- The mock venue exists only under the Cargo feature `test-venue`: 35 `cfg` sites in 6 files
  (`constants.rs`, `engine/tier.rs`, `instructions/create_market.rs`, `state/market.rs`,
  `venues/mod.rs`, and venue-state's `lib.rs`). Two more `cfg`s keep `security.txt` out of the
  test-venue (and `no-entrypoint`) builds of both programs.
- That feature goes only into a separate build, `scripts/build-test-venue.sh`, written to
  `target/test-venue/`.
- CI's anchor-build job (A49) checks that:
  - the release ELF `shortwin.so` does not embed the mock's program id (at 1.0.0 also
    `complete_curve.so`, retired since, §0);
  - the release `shortwin.so` lacks the marker `shortwin:test-venue:mock-cpamm`. The marker
    is a `shortwin` constant (`constants.rs`);
  - the test-venue `shortwin.so` contains both, which shows that the checks can see them;
  - the same checks run on the `verifiable-build` job's artifacts.
- Auditors should still confirm that no `test-venue` path reaches release behaviour.

---

## 3. Reading order

1. `README.md`, then SPEC §1–§11 (the product rules; §14 is the decision log).
2. ARCH §0 (summary), §2.3–§2.5 (accounts, funds and signer isolation, the venue state
   machine) and §3.0 (the common instruction structure and the remaining-accounts
   convention).
3. The program, in this order:
   1. `programs/shortwin/src/lib.rs`, the instruction list;
   2. `state/` (`config.rs`, `market.rs`, `position.rs`, `vault.rs`, `layout_asserts.rs`);
   3. `engine/prologue.rs`, then `engine/observe.rs`, `engine/guard.rs` and `engine/route.rs`;
   4. `instructions/open_short.rs`, `close_short.rs`, `liquidate.rs`, `expire.rs`,
      `sol_settle.rs`;
   5. `engine/waterfall.rs`, `engine/bounds.rs`, `engine/caps.rs` and `engine/lender.rs`;
   6. the lender instructions (`deposit.rs`, `withdraw.rs`, `claim_sol.rs`, `cover_deficit.rs`,
      `sync_lender.rs`);
   7. the venue adapters (`venues/mod.rs` and one file per venue);
   8. pool selection (`propose_pool.rs`, `execute_pool_switch.rs`, `engine/pool_select.rs`);
   9. the admin instructions (`initialize_config.rs`, `propose_config.rs`, `apply_config.rs`,
      `cancel_config.rs`, `set_*.rs`, `flag_venue_upgraded.rs`, `revalidate_venue.rs`,
      `sweep_*.rs`);
   10. `mint_filter.rs`.
4. `crates/shortwin-math` (its README lists the modules; ECON §2 gives the numeric
   conventions, ECON §3–§9 the formulas) and `crates/shortwin-venue-state` (the byte-level
   readers; ARCH §4.1).
5. (1.0.0 only) `programs/complete_curve/src/lib.rs` together with ARCH §5.4 (the straddle
   bundle); retired after the tag (§0).
6. If the keeper is in scope: `services/keeper/src/lib.rs`, then `decide.rs`, `view.rs`,
   `ixs.rs` and `venue.rs`, then `keeper.rs` (ARCH §7.1).
7. The invariant hooks (`tests/litesvm/src/invariants.rs` and `invariants/`; ARCH §8.3). They
   are the executable statement of what must always hold.

---

## 4. Trust and threat model

**Roles.** The owner's layout (decision Q4 of 2026-10-02; ARCH §2.3.2, §3.1, §7.4, §8.5). The
two Squads v4 multisigs are created on mainnet in M11 with the members' keys, so their
addresses are not known yet; `shortwin-gov create-multisigs` derives every vault from the
layout file (`services/ops/governance.example.toml`), and `squads_gov_it` builds this exact
layout on the dumped mainnet Squads ELF.

| Multisig | Squads v4 config | Its vaults' roles in short.win |
|---|---|---|
| **Governance** | 3-of-5, `time_lock` 172,800 s (48 h); autonomous (no config authority), no spending limits, rent collector its own vault 0 | vault 0: the upgrade authority of `shortwin` (at 1.0.0 also of `complete_curve`, retired since, §0), and `governance`; vault 1: `fee_recipient` |
| **Emergency** | 2-of-4, `time_lock` 0; autonomous, no spending limits | vault 0: `pause_authority` |
| (no multisig) | the keeper's hot key | `coverage_authority`, set at `initialize_config` or by a timelocked `Authorities` change |

**SQ-G8 (built in batch 2).** The program refuses a configuration in which two of
`governance`, `pause_authority`, `fee_recipient` and an installed `coverage_authority` share a
key (`config::check_authorities`, `AuthoritiesNotDistinct` = 6105; an unset coverage key is
skipped). It is checked in `initialize_config` and, for an `Authorities` change, at
`propose_config` and again at `apply_config`.

| Role | Key | Can | Cannot |
|---|---|---|---|
| Upgrade authority | Governance vault 0, after the deployer moves it with the checked `SetAuthorityChecked` (staged by `shortwin-gov`; the deployer runs `initialize_config` once, before) | upgrade `shortwin` (at 1.0.0 also `complete_curve`, retired since, §0) after the 48 h Squads time lock | — (the upgrade is the trust assumption; ARCH §8.5, KI-45, KI-62) |
| `governance` | Governance vault 0 | `propose_config` (timelocked, within compiled bounds, `params::validate` + `validate_econ`), `cancel_config`, `set_pause`, `set_route_broken`, `set_venue_extras` (pins `stable`) | move funds; change a venue program id; skip the timelock |
| `pause_authority` | Emergency vault 0 | `set_pause`, `disable_venue` (which also cancels a pending `EnableVenues` change that would set the venue's bit, AIR-24), `revalidate_venue(venue, expected_slot, expected_elf_hash)` (the hash of the reviewed trimmed ELF lets a later deploy slot from an extension through and records the venue's fingerprint, AIR-01; KI-69), `set_route_broken`, `set_venue_extras`, `cancel_config` (not `Authorities` or `Timelock` changes), clear (never set) `paused_coverage` | move funds; re-enable a venue (timelocked); veto an upgrade (KI-62) |
| `coverage_authority` | the keeper's hot key | set or clear one market's `paused_coverage` (opens only) | anything else (I4: byte for byte) |
| `fee_recipient` | Governance vault 1 (no spending limits) | receive `sweep_protocol_fees` | — |
| Anyone | — | `crank_price`, `crank_sol_usd`, `liquidate`, `expire`, `sol_settle`, `flag_venue_upgraded`, `flag_unsafe_mint`, `sweep_trader`, `sweep_protocol_fees`, `propose_pool`, `execute_pool_switch`, `create_market`, `apply_config` (after the timelock) | change locked position terms; take any amount but the bounded keeper reward |

**Disclosed Squads v4 semantics** (ARCH §8.5; `squads_gov_it` T-SQ-2, -4, -6, -7; known issues
KI-45, KI-59 … KI-63):
- A parameter change takes ≈ 96 h end to end: 48 h of Squads time lock from the threshold
  approval, then 48 h of `timelock_s`. An upgrade waits only for the 48 h Squads time lock.
- An Approved vault transaction **never expires**. It survives a member rotation and a
  threshold increase, and executes with the approvals it had. A time-lock increase delays it
  but does not stale it.
- Governance **cannot cancel reactively** inside the program window: its `cancel_config` lands
  at `not_before` at the earliest. Only the Emergency can cancel a parameter change in time,
  and never an `Authorities` or `Timelock` change.
- The Emergency **cannot veto an upgrade**; its members cannot vote on Governance proposals.
- A refused vault execution stays Approved and can be re-executed.
- `ExtendProgram` is permissionless: an upgrade to a larger ELF needs it first, and anyone's
  extension redeploys the program, which is unusable for the rest of that slot (KI-63, KI-73;
  the bounded griefing accepted by the owner on 2026-10-12, with no pre-extension to 10 MiB).
- **No spending limits** exist on either multisig (`shortwin-gov verify` checks the config
  authority is unset).
- Members approve and execute **in the Squads app**; `shortwin-gov` only stages and verifies,
  with no hardware-wallet support. The Emergency's worst-case `set_venue_extras` goes out as
  three single-signer transactions (owner decision of 2026-10-03).

**Attacker models** (ECON §12.1):
- **Atomic model.** Bundles inside one transaction or block.
- **Held model.** Prices held for whole seconds with nobody trading against them; this is also
  the exposure in degraded mode.
- **Other actors.** A keeper that sandwiches its own liquidations; lenders running before an
  S1; an LP holder who wraps cranks in deposits or withdrawals; junk listings that grief the
  keeper's budget; and a malicious or broken venue upgrade.

**External dependencies:**
- the venue programs and their admins (ARCH §5.5, §8.5; KI-44);
- the Token programs and memo (U17);
- the Pyth SOL/USD feed, read through `crank_sol_usd` into an oracle cache. Tiers fall back
  to C when the cache is older than `oracle_stale_s` (600 s);
- Squads v4: immutable since slot 302,582,236, trimmed ELF `d4866083…2fac`, IDL 2.1.0; the
  source (`Squads-Protocol/v4@64af733`) is UNVERIFIED and not rebuilt (ARCH §1.1, §12 U34;
  KI-59). The governance tests run this mainnet ELF itself.

What the design relies on and discloses: ARCH §8.5 and the register's §4.

---

## 5. Invariants and where they are checked

The invariants are ARCH §8.3: I1–I19, A26's locked terms, and ECON §13's economic
invariants.

**Where they are checked:**
- **The hooks.** `tests/litesvm/src/invariants.rs` and `invariants/{lender, positions,
  pool_select, confirm, open_terms, venue_view}.rs` run after **every instruction** of every
  LiteSVM test transaction. A transaction of several instructions is replayed prefix by
  prefix, so each instruction is checked against its own pre-state (ARCH §8.3 "The M1
  hooks").
- **The fuzzer.** `tests/fuzz` (ARCH §8.4) runs the same hooks along seeded action sequences:
  six profiles (`mock`, `mixed`, `curve`, `pumpswap`, `amm-v4`, `cpmm`), on the test-venue
  build and the sha-checked mainnet forks. Its extra oracles are:
  - the I10/I17 gates and I4 over the whole VM;
  - I14 omissions and replacements of every `ReadSet` / `TradeExtras` account (`close_short`
    included since M10; an accepted replacement fails the run);
  - the differential exit oracles: every keeper `liquidate` / `expire` recomputed from
    `shortwin-math` on the mock, AMM v4 and CPMM (every lamport, including I11's liveness
    clause), on the live curve (I-E11) and on PumpSwap (decision level);
  - the lender oracles (M10): `claim_sol ×N` in one transaction pays once; a JIT deposit
    around a landed liquidation, expiry or settlement gains at most its shares' part of the
    immediate rent credit; `[withdraw, sol_settle]` stays within the pre-bundle allowance;
  - the deficit tripwire and the second claim pass of the wind-down (M10; KI-53).
- **Mutation testing.** `scripts/mutants.sh` (M7-SEC-6) proves that the hooks catch seeded
  bugs in the open and coverage paths: 7 of 7 killed.
- **Hook mutants** (M10 WP-8d): tests that feed each hook a wrong instruction and expect it to
  fail, for I17 (`invariants_m2_hooks::i17_no_curve_open_while_graduation_halted`), I18 on
  PumpSwap (`pamm_it::hooks::i18_a_*`, `i18_d_*`, `i18_f_*`) and I15's burned-LP sizing and
  tier (`pamm_it::hooks::i15_an_open_is_sized_and_tiered_on_the_burned_lp`).

| Invariant | Statement (short; ARCH §8.3 is normative) |
|---|---|
| I1 | Tokens: `T + ΣB_Q` equals the lender claims; `pool_vault.amount ≥ T` |
| I2 | Rent-exempt floors plus the tracked SOL on the position and `MarketVault` accounts |
| I3 | Position SOL leaves only through close, liquidate, expire or sol_settle |
| I4 | No admin-signed instruction decreases fund balances, except the fee sweep; no venue id changes; `coverage_authority` touches only its flag |
| I5 | Utilisation ≤ `util_cap` after every open |
| I6 | Aggregates (`ΣB_Q`, rent aggregates, `open_positions`, `max_release`, `max_q_hwm`) match the positions |
| I7 | The trader side is empty after every leg; `pool_vault` has no delegate |
| I8 | ECON I-P1–I-P7, lender SOL and locks (claims, pot, vest, allowances) |
| I9 | Every waterfall conserves `V` |
| I10 | Exits never read the oracle, the guard, the pause flags or the route-broken flag as a gate |
| I11 | Keeper buyback spend ≤ `B̂_ref·(1 + δ)`; the liveness clause |
| I12 | S1 / S2 / S3 takes, the box, persistence and the S3 chain |
| I13 | Venue charge within `[B̂ − 5, B̂]`; EMA single-step bounds |
| I14 | Every pricing, route, tier or guard account is present and key-checked |
| I15 | Raydium adoption eligibility and timing |
| I16 | Fee caps in every risk quote; the route chains are sound |
| I17 | No open under the venue guard; the graduation halt |
| I18 | The post-change confirmation (warm-up, guard, seeds) |
| I19 | An open's terms, recomputed differentially from `shortwin-math` |

---

## 6. Known issues

[`docs/audit/known-issues.md`](audit/known-issues.md) is the single register: 77 rows (KI-01 …
KI-77), each with its bound, the decision that covers it and its status, plus the M10
findings and their resolution (its §6, with pump v3's PUMP-2026-10-13-1 … -6 since 1.0.1) and
the AI review's findings and theirs (its §7). Do
not report these as findings unless the stated bound is wrong or can be exceeded. Summary:

| Section | Rows | What |
|---|---|---|
| §1 | KI-01 … KI-15, KI-52, KI-53, KI-65 … KI-68, KI-71, KI-72, KI-75 … KI-77 | Economic residuals (O19) (★1.0.1: KI-77, pump's synthetic fee on the live supply — a burn can raise it, and the fee gates keep 10¹⁵ —, accepted beside KI-72; no effect under pump's single flat curve tier) (★eighth round: KI-66 fixed in full by the forward chain, its latent-unit residual disclosed; KI-67 fixed in the program; KI-68, a touch's per-touch floor, disclosed; ninth round: KI-68 accepted): the 2 % exit bound and the held model, with the real-venue figures accepted on 2026-10-03 (AMM v4 +61.27, CPMM +37.33 / atomic extra +5.34, PumpSwap +10.20 SOL); no liquidation reserve; baskets; settlement push and deferral; degraded-mode S2→S1; the sale-wrapped open's notional (accepted); the run residual; gap risk; creator self-dealing; **M10-8C-1 (fixed)**; the **late-depositor shortfall** (accepted, with the deposit warning at 1 % / 1 SOL); the **rebase-orphan vest** (found by the final tree's 24 h fuzz; **fixed in the program**, KI-65: cumulative conversion rounding, no rebase orphans; KI-11 with it); the pot behind a rebase record (KI-66, the owner-signed rule's price; **fixed in the program** by the pot-only touch, its post-record residual disclosed); a position paid by `claim_sol` after its owner's exit stayed open with its rent (KI-67, latent since M1; **fixed**: `claim_sol` closes it); a touch of a current or parked lender floors per touch, ≤ 1 lamport (KI-68, predates KI-66; accepted 2026-10-08); the AI review's, accepted on 2026-10-12: the **drained-pool deposit gate**'s refusals of honest deposits (KI-71, AIR-07), a same-transaction burn's PumpSwap exit tier (KI-72, AIR-10), the box corner as the only S1/S2 veto inside an LP's withdrawal (KI-75, AIR-20), the sustained growth's trade-off (KI-76, AIR-22: a 2-s held sandwich, the two-in-three flat-step veto) |
| §2 | KI-16 … KI-29 | Pool switches and third-party LP (O18; ECON §12.12 residuals 1–17), graduation delays |
| §3 | KI-30 … KI-41 | Keeper coverage and budget (O17, OWNER-M7-*), transaction sizes |
| §4 | KI-42 … KI-46, KI-54 … KI-64, KI-69, KI-70, KI-73, KI-74 | Trust: the venue guard window, halted venues, upgradeable venues and programs (with the pump venues' 2026-10-02 redeploy and their 2026-10-08 / 10-12 / 10-13 redeploys, KI-44; the curve's 166-byte growth, KI-64), admin powers; the **extras-denylist residuals** (batch 3); the **CPMM creator-fee share**; the Squads source; the **Squads semantics** (KI-63 corrected: an extension blocks the program for the rest of its slot); the AI review's, accepted on 2026-10-12: the **venue latch's extension test** and its residuals (KI-69, AIR-01), new authority keys installed unsigned (KI-70, AIR-04), **`ExtendProgram`'s one-slot block** (KI-73, AIR-18), the conservative S3-chain mark (KI-74, AIR-19) |
| §5 | KI-47 … KI-51 | Platform facts relied on (U16's 40 M limit, U17, U25, U29, log events; since 1.0.1 no exit uses a Jito bundle, KI-51) |
| §6 | — | M10 findings: M10-8C-1, B2-2, the extras denylist, PUMP-2026-10-1 … -3 (the pump venues' redeploy; since 1.0.1 also **PUMP-2026-10-13-1 … -6**: the synthetic migration's pricing and its two residuals, the cashback scope cut D1 (a), `B̂_v3` above `rT` D3, the v3 dependency, pump-fees and R-PFEE, the curve's fee buckets), the 24 h fuzz findings (runs 1 and 3: FUZZ-M7-24H-1 … -3, FUZZ-M10-24H-A, -B; run D on the final program bytes: FUZZ-24D-A, FUZZ-M10-24H-D-B — all the harness's), FUZZ-M10-1, the KI-65 batch's review findings (KI65-REV-SEC1: owner-signed rebase conversions; KI65-REV-CU: `MAX_RECORDS` 8) and its follow-up's (KI65-REV-POT: the pot behind a rebase record, KI-66, fixed by the pot-only touch; KI65B-REV-LOW) and the KI-66 batch's (KI66-REV-LOW: the surplus move checked by the math properties, the fuzz's never-returns model, the sub-unit warning judged on the worst case, KI-67) and the KI-66 full / KI-67 batch's (KI67-REV: the web's read of a funded lender PDA, the 3012 retry, the fuzz's chain coverage and residual cross-check, the withdrawal close hook, the park mutant, KI-68), the WP-1 / WP-10 doc findings, GC-F1, M10-CI-LINT |
| §7 | — | The AI review of v1.0.0: AIR-01 … AIR-24 and the refuted lead L-1, each with its fix commit or the row that discloses it |

**Status by owner decision** (SPEC §14, ARCH §10):
- **Accepted by the owner:** the real-venue held sandwich (KI-01's and KI-02's figures,
  2026-10-03), the late-depositor shortfall as designed with the warning's 1 % / 1 SOL
  thresholds (KI-53, 2026-10-03), B2-2 kept as built (KI-52, 2026-10-03; AIR-21's change to S1
  confirmed on 2026-10-13), the extras-denylist
  residuals (KI-54 … KI-57, 2026-10-03), the first-trade venue rent on short pump accounts
  (KI-64, PUMP-2026-10-2, 2026-10-03), the Squads layout (KI-45, 2026-10-02), the per-touch
  floor of a current or parked lender (KI-68, 2026-10-08, ninth round: at most 1 lamport of
  entitlement and 1 of realised SOL per third-party touch, each touch costing its sender a
  transaction fee; the cumulative-floor alternative, a program change, not taken), and the earlier
  rows marked accepted (KI-12, KI-22 … KI-24 (KI-23 with AIR-14's widening accepted and KI-24
  with AIR-09's zero token value confirmed, both on 2026-10-13), KI-27, KI-28's M6R-4, KI-29,
  KI-30, KI-35), and
  the margin of a PumpSwap position opened while a deposit is held (KI-26, M6D residual 16,
  2026-10-09, eleventh round: as built; pricing it on the burned depth, a program change, not
  taken), and O18's residuals as disclosed (KI-16 … KI-21, KI-25, 2026-10-09, eleventh round,
  with O18's rules), and the AI review's residuals (KI-69 … KI-76, 2026-10-12, recorded in the
  review's README: the venue latch's extension residuals, new authority keys installed
  unsigned, the honest deposits the drained-pool gate refuses, a same-transaction burn's
  PumpSwap exit tier, `ExtendProgram`'s one-slot block with no pre-extension to 10 MiB, the
  conservative S3-chain mark, the box corner as the only S1/S2 veto inside an LP's withdrawal,
  the sustained growth's trade-off) and KI-13's lock decay (AIR-11, intended design,
  2026-10-12).
- **Fixed:** M10-8C-1 (KI-52), the extras denylist hardening (batch 3), PUMP-2026-10-1 (batch 4:
  PumpSwap's fee buckets are not payable SOL), the rebase-orphan vest and dust (KI-65, KI-11:
  the KI-65 program batch, cumulative conversion rounding through each rebase record), the
  third-party conversion order (KI65-REV-SEC1: only a position's owner converts it through a
  rebase record, the KI-65 follow-up batch, owner decision of 2026-10-07), the pot behind a
  rebase record (KI-66, KI65-REV-POT: anyone else's `sync_lender` of a lender behind a rebase
  record is the pot-only touch, the KI-66 batch, owner decision of 2026-10-07, seventh round;
  and what the absent lender's shares earn after the record, KI66-R3: the pot-only touch walks
  its forward chain, the KI-66 full batch, eighth round, 2026-10-08), the open empty position
  (KI-67: `claim_sol` closes it, eighth round; its `lock_term` detail confirmed as built on
  2026-10-09, eleventh round), and the harness and doc findings of the
  register's §6. After the tag: the AI review's findings but AIR-04, AIR-11, AIR-18 and AIR-20
  (the register's §7; the fixes' commits and regression tests are in the review's README),
  built and through a local run of the CI gate but not yet through CI or an external fix
  review; the ≥ 24 h fuzz run waived by the owner for this batch (2026-10-13; §12).
- **Disclosed** (owner decision of 2026-10-07): KI-65's remaining conversion-order residual — a
  party ordering its own conversions (Sybils placed before the roll, a tuned residue) and
  converting after honest lenders' own conversions, at any later time (no atomicity needed;
  an honest sub-unit lender loses its latent unit at its first own conversion during a vest) —
  with its figures, pinned in `ki65_it`; since the seventh round the web's review warns before
  the user's own conversion of a sub-one-share position to 0 shares during a vest and advises
  waiting for its end (`lender.subUnitConversion`; judged on the worst case, the position's
  floor through the records, since other lenders' conversions can land first and no
  instruction takes a minimum of shares — KI66-REV-LOW), and `lender.residuals` carries the
  residual. KI-66's residual since the eighth round (the forward chain moves what an absent
  lender's unconverted shares earn after the record; `pot_liveness_table` 0 of 2,000 pools above
  dust, was 8, worst 2,134,755,812 lamports): the latent rounding units — an owner's shares
  beyond its chain's floor, at most one per record, whose later earnings reach the pot only at
  its conversion (`pool_model.latent_residual_table`: worst 727,760 lamports on residue-tuned
  pools, 374,750 beyond every owner returning on adversarial ones; no general small bound when
  `S` is tiny) — and the chain's over-move, at most `(σ − q)·max(0, a − r)` per epoch (smaller
  than the held-only touch's); owner item (§7). KI-68 (predates KI-66; found by the KI-66 full
  security review: a touch of a current lender, or of one parked on a live full-wipe record,
  accrues `⌊s·(end − snap)/WAD⌋` per touch, so each third-party touch between two credits may
  floor away up to 1 lamport of that lender's entitlement and of its realised SOL, left in
  `lender_sol` as rounding) is accepted since the ninth round (above).
- **Open** (build default in force; §7): the rest of O19 (KI-03 … KI-10 are disclosed but not
  signed off), OWNER-M6SK-1 (KI-28) and the M7 keeper items. The three confirmations the AI
  review's fixes asked for (SPEC §14 fourteenth round) were given on 2026-10-13: AIR-21's S1
  vest, which changes B2-2 (KI-52); AIR-09's zero token value on a venue that cannot execute
  (KI-24); and AIR-14's widening of ECON §12.12 residual 10 (KI-23). O18 is decided (2026-10-09,
  eleventh round: its rules accepted as the v1.0.0 design and the §2 residuals it disclosed,
  KI-16 … KI-21 and KI-25, accepted as disclosed; §7).

---

## 7. Open owner items and their build defaults

Until the owner decides, the build default stays in force. **Program** marks items whose
decision, or whose alternative to the build default, changes program bytecode. After the tag
such a change is a 1.0.x fix batch with the §12 fix policy. Items marked keeper or web change
only that code.

**Decided in M10** (SPEC §14 and ARCH §10 record them with their dates): Q1 CI route, Q2 audit
shape, Q3 security contact, Q4 Squads layout and SQ-G8, Q5/Q6 defaults (2026-10-02);
M10-8C-1, the release version, the keeper scope, calibration data, CI minutes (2026-10-02);
O19's real-venue figures, the late-depositor warning, the extras denylist hardening, Squads
signing (2026-10-03, third round); B2-2 kept as built, the extras-denylist residuals KI-54 …
KI-57 accepted, the deposit-warning thresholds (1 % / 1 SOL) confirmed, the `SECURITY.md` /
`/security` policy defaults kept (2026-10-03, fourth round); PUMP-2026-10-1 fixed and
PUMP-2026-10-2 disclosed after the pump venues' 2026-10-02 redeploys were re-dumped and
revalidated (2026-10-03, fifth round); KI-65 fixed in the program (2026-10-07, sixth round:
cumulative conversion rounding), conversions through a rebase record owner-signed
(KI65-REV-SEC1) and `MAX_RECORDS` = 8 accepted (2026-10-07, sixth round); KI-66 fixed by the
pot-only touch and the web's sub-unit conversion warning, with the `lender.residuals` sentence
approved (2026-10-07, seventh round); KI-67 fixed in the program, KI-66 fixed in full (the
forward chain) and the web's claim-time auto-touch (2026-10-08, eighth round); KI-68 accepted
and disclosed, FE-30 (the claim's pot-sync candidates: the lenders behind the share epoch first,
paged further, ranked by the simulated gain — the indexer's `epoch` parameter and the bound of
4 pages × 50 lenders confirmed by the owner on 2026-10-09 (tenth round), with the indexer's
`max_page_limit` floor of 200; short.win's
3007 on a lender position rebuilt like 3012) and the four lender texts of the seventh and eighth rounds approved as written
(2026-10-08, ninth round); in the eleventh round (2026-10-09, the pre-freeze sign-off) O18
accepted as the v1.0.0 design (ECON §14's 37 PROPOSED rows ADOPTED, N25's keeper-budget defaults
among them, SPEC §14 rows 10, 13 and 18 adopted; the parameter values stay governance-tunable
within the compiled bounds; O18's residuals KI-16 … KI-21 and KI-25 accepted as disclosed), KI-67's
`lock_term` detail confirmed as built, M6D residual 16 accepted as built (KI-26) and IX-1 / IX-11
deferred to a 1.0.x batch after the audit tag; the web on phones (2026-10-09, twelfth round: the
M10-RESP redesign); in the thirteenth round (2026-10-10) the redesign approved as built and
O-1 … O-6 decided (O-1 … O-5 built in the round-13 UX batch; O-6, the Mobile Wallet Adapter, deferred
until after the audit, off by default; **lifted by the owner on 2026-10-13: built and on by default,
the owner's real-device test before the release** (its own row below)), and on 2026-10-11 the round's 143 strings approved word for
word, `borrower.liquidation` ¶3 amended and the copy review's questions decided (its ten items and
one follow-up; the table below). All the program-affecting ones are built (batches 2, 3 and 4, the
KI-65 batch, its follow-up, the KI-66 batch and the KI-66 full / KI-67 batch); the deferred IX-1 /
IX-11 are not.

**Decided after the tag** (2026-10-12 and 2026-10-13; the AI review's README, "Decisions for the
owner", records them): the AI review's open points (the table's first row). Its program fixes
are built. On 2026-10-13 the owner also lifted O-6 (the Mobile Wallet Adapter; its row follows the
thirteenth round's): web only. Also on 2026-10-13 the owner decided the pump v3 catch-up (the
design `pump-final`; its row below; §13): a program change, built as 1.0.1.

| Item | Decision needed | Build default (in force) | Changes program code? |
|---|---|---|---|
| **The AI review of v1.0.0** (AIR-01 … AIR-24; [`audit/ai-review/README.md`](audit/ai-review/README.md); known-issues §7) | **Decided (2026-10-12):** **AIR-22** keep the lower-median sustained growth (`6c39e58`, `03b7762`, `8a4df78`; its trade-off KI-76); **AIR-07** keep the drained-pool deposit gate (`PoolDrained` = 6108, `00d3772`; the refused honest deposits KI-71); **AIR-10** accept the burn residual (KI-72), no per-second supply snapshot; **AIR-18** accept the bounded `ExtendProgram` griefing, the keeper retrying in a later slot, no pre-extension of short.win's ProgramData to 10 MiB (KI-73; KI-63's "harmless" corrected); **AIR-19** accept the conservative `min_open_hard_cap_bps` mark (KI-74), no exact cap tracking; **AIR-01** the hashed `revalidate_venue` of pump-fees, AMM v4, CPMM and Jupiter after `initialize_config` (`services/README.md` "Governance"), its residuals accepted (KI-69); **AIR-04, AIR-11, AIR-20** no behaviour change, known-issues rows and doc wording only (KI-70, KI-13, KI-75; AIR-20's optional fix in `leads.md` not taken). **Decided (2026-10-13, SPEC §14 fourteenth round):** AIR-21's S1 vest confirmed (known-issues KI-52 and its B2-2 row), AIR-09's zero token value on a venue that cannot execute confirmed (KI-24), and AIR-14's widening of ECON §12.12 residual 10 accepted (KI-23); the ≥ 24 h fuzz run waived for this batch (§12) | As decided: the fixes built (the AI review's batch after the tag) | **Program** (built: the AIR fixes, a batch after the tag; the alternatives not taken — reverting AIR-22 or the gate, the supply snapshot, exact cap tracking, AIR-20's fix — would be program changes too; pre-extending ProgramData is a deployment step) |
| **O19** (the rest) | Sign off the disclosed residuals of known-issues §1 (KI-01, KI-03 … KI-11); the real-venue figures are already accepted | Accept and disclose; no liquidation reserve | No; the alternative (a rent-only liquidation reserve) is **program** |
| **Rebase-orphan vest** (KI-65; O19; FUZZ-M10-24H-A) and the conversion order (KI65-REV-SEC1) | **Decided (2026-10-07): fixed in the program by cumulative conversion rounding** — each rebase record carries `conv_rem` and a lender's conversion mints `⌊(conv_rem + s)/1,000^k⌋`, so the old epoch's lenders end with exactly the new `S` and the last real lender is paid the vest (ECON §9.5, KI-65). **Decided (2026-10-07, sixth round): owner-signed conversions** — `sync_lender` converts a position through a rebase record only with the owner's signature (unsigned it stops at the record: since the KI-66 batch the pot-only touch, with `RebaseNeedsOwner` 6107 its post-condition; plain catch-up, the V2-12 jump, the parking and `sync_lender(0)` stay permissionless; every other lender instruction is the owner's), so no third party orders an honest lender's conversion and a latent honest unit blocks the flush (W3). Disclosed, not closed: a party ordering its own conversions and converting after honest lenders' own conversions (at any later time) still collects up to one unit per position and, where every honest holder converted to 0 shares, the whole unreleased vest (KI-65's figures: +4,007,334,855 vs 369,862 lamports; a JIT depositor 3,000,000,000 of a 3,000,000,000 lump). **Decided (2026-10-07): `MAX_RECORDS` = 8** per lender transaction (worst case `cover_deficit` 54,549 CU of 60,000; a lender further behind catches up over several transactions) | Built (the KI-65 program batch and its follow-up) | **Program** (built; new hashes, §0) |
| **KI-66** (KI65-REV-POT; O19) | **Decided (2026-10-07, seventh round): the pot-only touch** — anyone's `sync_lender` without the owner's signature that reaches a rebase record accrues the lender's `pending` / `real` to the record's `acc_end` / `acc_r_end` and moves its surplus to the pot, converting no shares, writing no `conv_rem` and leaving the epoch (it cannot order conversions; idempotent; the owner's later conversion counts nothing twice). `ki65_it` pins the deficit lender paid in full without the holder's owner (pot 4,434,782,609 lamports; A's next claim 2,957,608,696, nothing left owed). Then disclosed: what the unconverted shares earn after the record waited for the owner (KI66-R3) — **fixed in full in the eighth round** (2026-10-08: the forward chain; the next row). **Decided (seventh round): the web's sub-unit conversion warning** (`lender.subUnitConversion`, the vest's end) and the `lender.residuals` sentence | Built (the KI-66 batch, `m10/ki66`) | **Program** (built; new hashes, §0) |
| **KI-67** (KI66-REV-LOW; latent since M1) | **Decided (2026-10-08, eighth round): `claim_sol` closes the position** when the claim leaves it current with no shares, `pending` 0 and no lock (and no forward chain); a second claim of it then fails with `AccountNotInitialized` (3012). **The `lock_term` detail confirmed by the owner (2026-10-09, eleventh round): kept as built** — the close ignores a dead lock term when `lock == 0`. The owner's text said "no lock term (withdraw's conditions)", but `withdraw` never checked `lock_term`, and a term with `lock == 0` is never read again (the calendar keeps its own copy); requiring `lock_term == 0` would leave KI-67's own "rebase floors a sub-unit position" case open (6 of 4,839 closable positions in 5,000 of `pool_model`'s differential pools once every lender has claimed; `pool_model.py`'s output, `out/pool_model.txt`) | Built without the `lock_term` condition (`pool::closable`); confirmed | **Program** (built; new hashes, §0) |
| **KI-66 full residual and the chained settlement cap** (KI66-R3; O19) | **Decided (2026-10-08, eighth round): the full fix** (the forward chain; built). **Accept** the latent-unit residual (worst 727,760 lamports measured on residue-tuned pools, 374,750 beyond every owner returning on adversarial ones; no general bound when `S` is tiny) and the chain's over-move bound `(σ − q)·max(0, a − r)` per epoch (smaller than the held-only touch's `σ·(…)`; measured: 4 of 16,056 pot-only touches moved more than the owner's own touch would, 1 lamport each, `pool_model.py`'s 5,000-pool differential line). **Accept** the CU consequence: an owner settling its forward chain at a lag of 1 at every record costs 64,355 CU over 8 records (60,000 budget) and 58,229 over 7, so the web caps a chained owner's settlement at 7 records per transaction; `MAX_RECORDS` stays 8 for every other lender transaction | Accept and disclose (known-issues KI-66; ARCH §6.1) | No (the cap is the web's) |
| **KI-68** (the per-touch floor of a current or parked lender; predates KI-66) | **Decided (2026-10-08, ninth round): accept and disclose** (known-issues KI-68, accepted): at most 1 lamport of entitlement and 1 of realised SOL per third-party touch, each touch costing its sender a 5,000-lamport fee. Not taken: accruing current and parked lenders by cumulative floor (`⌊s·end/WAD⌋ − ⌊s·snap/WAD⌋`, as the forward chain does), with exact parity in `pool_model` and regenerated vectors | As built (accepted) | No (the alternative not taken was **program**) |
| **O18** with the SPEC §14 delegated rows **10** (collateral and liquidation by tier), **13** (dynamic rent) and **18** (fee split) | **Decided (2026-10-09, eleventh round): accepted as the v1.0.0 design** — the PROPOSED parameters and venue rules (ARCH §2.3.3, §10 O18): ECON §14's 37 PROPOSED rows read ADOPTED (owner, 2026-10-09, O18), SPEC §14 rows 10, 13 and 18 are adopted by the owner. The parameter values stay governance-tunable within the compiled bounds (ECON §15, ARCH §2.3.3). N25's keeper-budget defaults are among the 37 (OWNER-M7-17's options (3) and (4) stay open, below). **O18's residuals accepted as disclosed** (2026-10-09, eleventh round): known-issues KI-16 … KI-21 and KI-25 (ECON §12.12 residuals 1–4 and 13, ARCH §2.6). OWNER-M6SK-1 is its own row below | As built (adopted; residuals accepted) | No |
| **O9** (ECON G37, U21) | The calibrated gap frequencies per tier and the pre-committed rule (if the 95 % upper bound of λ_up exceeds a tier's break-even, raise CR_open or rent, or cut caps), after ≥ 30 days of the public-RPC crawl | Uncalibrated Monte Carlo (ECON §11.6, illustrative); the crawl runs | Parameters through governance; **program** only if a value leaves its compiled bounds |
| **M6D residual 16** (ECON §12.12; KI-26) | **Decided (2026-10-09, eleventh round): accepted as built** — a position opened while a deposit is held is margined on the pool as it is (known-issues KI-26, accepted). Not taken: pricing the open's margin on the burned depth | As built (accepted) | No (the alternative not taken was **program**) |
| **OWNER-M6SK-1** (KI-28) | The keeper's tx3 records the new pool's first bucket (+9 → +16 s tracking after a ×3 pump) | As built | Keeper core |
| OWNER-M7-1 (KI-41) | The build-alt table as the normative A28 | Adopt; per-market table as fallback | No (ALT) |
| OWNER-M7-2 (KI-31) | (a) accept the free-pool drain, or (d) a per-market hourly cap on liquidity-anomaly coverage | (a) | Keeper core |
| OWNER-M7-4 (KI-37) | Isolated missed seconds and `liq_cover_s` | PROPOSED until production telemetry | Keeper core |
| OWNER-M7-5 | The LiteSVM action-sequence fuzzer instead of Crucible | As built | No (tooling) |
| OWNER-M7-6 (KI-32) | (a) the alerted overdraft, or (b) cap the overdraft | (a) | Keeper core |
| OWNER-M7-7 | `per_market_daily_lamports` | Superseded by OWNER-M7-17 (decision A) | — |
| OWNER-M7-8 (KI-36) | T-CASCADE defaults (12 M / 20 M) and the fee-payer residual | Confirm; separate crank payer not built | Keeper core |
| OWNER-M7-9 (KI-38) | Keeper load: sequential RPC | Accept for the beta | Keeper / ops |
| **OWNER-M7-17 (3), (4)** (KI-33, KI-34); its defaults adopted with O18 (2026-10-09) | (3) the drainable free pool, (a) accept or (b) confine bursts; (4) idle cadence below the floor. The defaults (`G` 1 SOL, floor 1 SOL, margin 10,000 bps, tail 2,000 bps) are adopted by the owner as v1.0.0's (2026-10-09, eleventh round, O18; ECON §14 N25) | As built, (a) | Keeper core (budget logic and config) |
| **M8: IX-1, IX-11** (INDEXER.md §14) | Add `rent_protocol`, `q` and `ema_price` to the `Liquidate` / `Expire` / `SolSettle` events, and `record_donation` to `EpochRolled`, so the indexer need not derive them. **Deferred (2026-10-09, eleventh round) to a 1.0.x batch** after the audit tag; not before the freeze | Not added; the indexer derives them | **Program** (IDL, CU, trace): a 1.0.x batch after the tag |
| M8: IX-9, IX-10, IX-14, IX-15, IX-18 | Off-chain indexer items (deployment, fee-sharing attribution, harness coverage, versioned decoders) | Open, out of scope | Indexer only |
| FE-21 | Refuse sending-only wallets for writes, or keep the post hoc check | Post hoc check | Web only |
| FE-23 | Source maps: upload to a private error tracker, or publish | Built hidden, not deployed | Web / ops only |
| FE-30 (FRONTEND §13; KI66-R4) | **Decided (2026-10-08, ninth round): prefer behind and chained lenders.** The claim-time candidates were the indexer's first page of active lenders, largest raw share count first (not comparable across share epochs: a large current lender could push an absent surplus holder out of the 24). Built: the indexer's lenders behind the share epoch first (its new additive `epoch=behind` / `current` filter of `/v1/markets/{mint}/lenders`; a lender with a forward chain is behind, except one whose owner's own conversion reached the share epoch inside the chain: that one is current and comes with the current lenders), then the current ones, 50 a page, paged further until the best ≤ 4 cover the shortfall, at most 4 pages (≤ 200 lenders: three sequential reads a page while the review waits), ranked by `previewTouchPot`'s gain, every row of either list read whatever its epoch (an indexer lagging or leading the RPC's share epoch only reorders the reads); the size trim stays the conservative bound (no lookup tables). A sync target or the claimant's own position closed meanwhile fails the review's simulation with 3012 — or, once someone funded the closed address, short.win's `AccountOwnedByWrongProgram` (3007) on the `lender` account — and the review's Retry rebuilds both. **Confirmed by the owner (2026-10-09, tenth round):** the builder's choices under the "Web only" decision, the indexer's `epoch` parameter (a client-side scan of the raw-share list would have no bound) and the bound of 4 pages × 50 lenders; the indexer also refuses to start with `max_page_limit` below 200, the web's largest page request (`WEB_MIN_PAGE_LIMIT`, INDEXER §9.1) | Built (FRONTEND §3.6, §13 FE-30); confirmed | Web, indexer (an additive query parameter; the page-limit floor) |
| The seventh and eighth rounds' web wording | **Decided (2026-10-08, ninth round): approved as written** — `lender.potSyncs`, `lender.potSyncsDeferred`, `lender.claimCloses` and `lender.subUnitConversion` (FRONTEND §4.2) | As written in FRONTEND §4.2 | Web only |
| The web on phones and the thirteenth round's UX items (M10-RESP; O-1 … O-6; FRONTEND §13) | **Decided (2026-10-10, thirteenth round): the M10-RESP redesign approved as built** (the twelfth round's request of 2026-10-09: phones, tablets and desktops in the dark terminal style), with **O-1** the lend deposit order, **O-2** the coverage notice once, **O-3** newcomer help (the intro strip, the glossary, How it works), **O-4** a 12 px text floor and **O-5** the Claim wording to build, and **O-6** the Mobile Wallet Adapter after the audit, off by default (the "Open in Backpack / Phantom / Solflare" links stay). **Decided (2026-10-11): the round's 143 new or changed strings approved word for word** (FRONTEND §4.3), with recommendations 6–10 (Tier C as How it works' example, the risk texts there without tick boxes, "short.win" in running text and "ShortWin" as the brand mark, the glossary cuts, "Lending page") approved; **`borrower.liquidation` ¶3 amended** (the expiry fee made conditional: "Expiry: after {maxDuration} the position can be closed by anyone. Unless it can be liquidated at that point, you pay a {expiryFee} expiry fee instead of the penalty."; SPEC §5.3, `expire.rs`'s `UseLiquidate`; FRONTEND §4.2); and the round's questions: a Claim shortfall under 10,000 lamports counts as none (`CLAIM_DUST_LAMPORTS`), the Lending page's Claim is the market panel's claim with its pot syncs, and a Claim is enabled with something claimable now or syncs that can pay, the short-phone breakpoint approved, the lone header ⓘ accepted until a design pass after the audit | Built (`acbe323`, `7a64dcd`); O-6 lifted 2026-10-13 (its own row) | Web only |
| **O-6, the Mobile Wallet Adapter** (FRONTEND §5.4 (6), §9.3, §13 FE-32) | **Decided (2026-10-13): O-6 lifted.** Built now and switched on (default on; `VITE_ENABLE_MOBILE_WALLET_ADAPTER=false` keeps an off switch). `@solana-mobile/wallet-standard-mobile` 0.7.0, with the protocol pinned to 3.0.0 through pnpm overrides; Kit stays 8.3.0. The mobile UX fixes ship with their strings as drafted. No WalletConnect / Reown. The security review's safeguards taken as decided: the adapter signs only through `solana:signTransaction`, a repo-wide 14-day `minimumReleaseAge` (the two packages excluded until 2026-10-21), Disconnect local only, the `ws://localhost` CSP residual accepted. This replaces the decision of 2026-10-10 (after the audit, off by default). | Built, on by default; **the owner's real-device test pending** (it precedes the release; DEVELOPMENT "Mobile wallets on a real device") | No. Web only: CSP `connect-src` + `ws://localhost:*`, `http://localhost` and two style hashes, in flag-on builds |
| **pump v3** (the design `pump-final`; known-issues KI-77, PUMP-2026-10-13-1 … -6; §13) | **Decided (2026-10-13):** change the program — `PumpMigrating` priced on the post-completion fields, v3 for every curve leg; **D1 (a)** refuse live cashback curves (`CurveCashbackCoin` 6109); **D2** retire `complete_curve` and the 3-transaction straddle bundle (an audit-scope change: **the auditors must be told**, with this addendum); **D3** `B̂_v3` is the risk and exit price above `rT`; **D4** the two strings (6109's and 6031's) as drafted; every residual of the design's §16 / §17 accepted; the release order keeper (both TradeExtras formats) → program upgrade → web; version **1.0.1**; the re-pin to pump's 2026-10-13 build (slot 456,494,698). **Decided after P1 (which introduced `φ_s` on 2026-10-14), confirmed by the owner on 2026-10-16:** the live-supply synthetic fee `φ_s` (a burn can raise it under a multi-tier curve `FeeConfig`) and the fee gates' 10¹⁵ `φ` disclosed beside KI-72 and accepted under KI-72's decision of 2026-10-12 (AI review decision 3; KI-77); no code change | Built (P0 … P8, `pump-v3`); hashes §0 (local, not verified) | **Program** (built: 1.0.1; the `complete_curve` program removed) |
| Other M10 items | The G-H10 reading of a zero-lamport borrower ((A) reaped after the open, (B) emptied inside the exit's transaction; both tested); the fuzzer's PumpSwap weights (`fee_shock` 3, `open_near_cap` 6); whether to publish the private audit report (recommended, before the beta) | As built; publication undecided | No |
| Scope note | `shortwin-gov` and `crates/squads-v4-ix` are out of scope under the owner's scope decision (programs plus keeper core). They prepare the transactions the members sign; add them to the RFQ if the owner wants them reviewed | Out of scope (§2) | No |

**Closed since the skeleton:** the M10 plan's corrections (WP10-PLAN-Q2) and the four items
tagged M10 without a work package: FE-15 is done in M10 (frontend only, no program bytecode);
FE-18 is partly closed (the mock-venue bundle, claim-time sync and venue claim encodings on a
validator; the pump / PumpSwap composite bundle is M11's); FE-22 moved to M11 (it needs
mainnet latency); IX-16's decoder half is closed (GC-7 / GC-F1), its refresher and admin
watch are M11's.

---

## 8. UNVERIFIED status

ARCH §12 is the **normative** UNVERIFIED register. The research notes (`docs/research/*.md`)
are historical: where they disagree with ARCH §12 or the code, ARCH §12 and the code win.

**Status on this tree** (ARCH §12 after the M10 doc sync):

| U | Item | Status | Left for |
|---|---|---|---|
| U1 | PDA `user` under CPI (pump buy, PumpSwap buy) | VERIFIED on the dumped ELFs (M5, M6). A PumpSwap buy by a third-party PDA is unseen on mainnet | — |
| U2, U3, U6, U7, U9, U11, U24 | Non-ATA WSOL, `disable_flags` bits, rent debits, ATA depth, unsigned `claim_cashback`, CPMM exact-out creator fee, trailing accounts | VERIFIED / RESOLVED (M3–M6) | — |
| U4 | Whole-instruction CU, trace and size figures | **VERIFIED (measured)** for every instruction class (`m1_cu`, `m2_cu`, `m3_cu`, `m5_cu`, `m6_cu`, `m7_keeper`), within the PROPOSED limits, re-baselined after each M10 batch and for pump v3 (the curve and PumpSwap benches on `b3130ae2…`, pump-final P3; `m7_keeper` / `m7_cascade` on 1.0.1, P8; §13) | — |
| U5 | Copying a 10 MiB ProgramData | VERIFIED for CU (not metered). Latency and fee are unmeasured. Since AIR-01 a flag or a revalidation after an extension also hashes the trimmed ELF: ≤ 0.44 M CU measured on pump-fees, CPMM and AMM v4 (`latch_raydium_it`, `latch_pump_it` `air01_*`; pump-fees and CPMM after one 10 KiB extension), and ≈ 0.48 M estimated for pump-fees grown near the 10 MiB cap, so its sender requests ≥ 0.6 M CU or sizes the limit by simulation; Jupiter's is not measured and is sized by simulation (KI-69) | M11 (mainnet) |
| U8, U22, U25 | The straddle bundle's pre-flight and a live block engine; Jito semantics; v1 transactions landing | Fork parts VERIFIED; the live parts **UNVERIFIED**: no mainnet spend before the audit (owner default Q6). **Since 1.0.1 (pump-final, D2) no exit uses a bundle** (a position past `rT` exits in one `buy_v3`): U8 and U22 gate nothing any more; U25 stays for the keeper's v1 crank batches (KI-51) | M11 probes (U25) |
| U10 | Delegated `pool_vault` sell; direct delivery for Token-2022 CPMM | **VERIFIED** on the dumped ELFs, re-run on the 452,376,460 CPMM ELF | a mainnet example in M11 |
| U12 | Raydium deployed binaries vs public source | **CPMM closed:** the redeploy is `raydium-cp-swap@bdeee23`, rebuilt byte for byte (the manifest's `source.status` text still says UNVERIFIED; ARCH §12 is normative). **AMM v4:** a behavioural match only | — |
| U13, U14 | Raydium admin history; the `open_time` clamp deploy | Keeper monitor; a conservative constant | — |
| U15, U30 | VQR drift; pump's announced upgrade | Third-party LP explains the drift. The upgrade **was deployed on 2026-10-02** (the `pump-rust-client` 0.2.0 release; what its added `.text` does is UNVERIFIED, coverage is behavioural); re-dumped as `pump_programs_2026_10`, every pump suite and the T-LATCH replay re-run on it, the fee buckets read since batch 4 (PUMP-2026-10-1). **Successors** (pump-final, §13): pump and PumpSwap 2026-10-08, pump-fees 2026-10-12, pump 2026-10-13, through the addendum's steps 1–4 — re-dumped (`pump_programs_2026_10_13`, `_13b`, `pump_state_2026_10_13`), interface-diffed, gates G0 / G0b on the ELFs, and the adapter moved to v3 (1.0.1); what the new `.text` does beyond the paths short.win and its tests exercise stays UNVERIFIED (behavioural coverage) | Closed for 1.0.1; a later pump upgrade goes through the addendum again (§12) |
| U16 | The per-writable-account limit | **Desk-checked:** 40 M CU on mainnet in Agave master and Firedancer (below); other clients not checked | monitoring |
| U17 | CPI'd memo satisfies Token-2022 MemoTransfer | **VERIFIED on the mainnet build** (Token-2022 v11.0.0 `7ea94a02…`, spl-memo v3) | a mainnet transaction (M11, TMEMO-10) |
| U18 | `declare_program!`, the lockfile resolution, solana-verify vs `anchor build` | **RESOLVED** for 1.0.0: byte-identical for both programs (§0). 1.0.1: not yet run (a local build only; §0, §13) | CI and the verifiable build on `pump-v3` (TODO.md) |
| U19, U23 | Surfpool template fidelity; the Switchboard shutdown | **Closed: not relied on** | — |
| U20, U21, U28 | Arbitrage of held pumps; gap frequencies; price noise | Uncalibrated. U21: method and public-RPC crawl since 2026-10-02, results pending | ≥ 30 days of tapes (O9); U28 from production |
| U26 | The latch's reaction time | Monitoring; LiteSVM cannot model it | — |
| U27 | Keeper cost inputs | Closed (M7) | — |
| U29, U31, U32, U33 | pump's cap byte; pAMM `claim_cashback` and `init_user_volume_accumulator`; `set_coin_creator`; the LP accounting behind `D` | Pinned on the dumped ELFs | Re-verify on every pump / pAMM upgrade (U30) |
| U34 | The Squads v4 program's source | Trusted, not reproduced (`64af733` probable, not rebuilt; KI-59) | optional rebuild |

**The re-grep** (`grep -rn UNVERIFIED docs programs tests crates services` on this tree) has no
contradiction with ARCH §12. Every marker outside ARCH §12 maps to a row above or is
historical:

| Where | Marker | Maps to |
|---|---|---|
| `docs/research/*.md` (100 lines) | the research notes' own markers | historical; ARCH §12 wins (the M10 doc sync corrected R-TOOL, R-MINT, R-RAY and the venue notes) |
| `docs/design/ECONOMICS.md` (§17, the boost rule, v1 batches) | ECON's own register | U15, U25, U16 (struck through and restated as desk-checked) |
| `docs/DEVELOPMENT.md`, `tests/fixtures/README.md`, `tests/fixtures/manifest.json`, `tests/litesvm/src/fork.rs`, `crates/squads-v4-ix/src/lib.rs` | the CPMM and Squads fixture sources | U12 (CPMM: rebuilt since; the manifest's status text predates it) and U34 |
| `docs/design/FRONTEND.md` FE-3 | browser bundle submission | U22 |
| `services/common/src/rpc.rs` | live bundle submission | U22 |
| `services/keeper-litesvm/tests/scenarios/m7_cost.rs`, `tests/bench/m7_keeper/results.*`, `services/README.md` | the keeper's live-RPC capacity | OWNER-M7-9 (KI-38) |
| `tests/litesvm/tests/graduation_e2e.rs:1188` | how a bot spends a donation | R-VEN §1.4 (not relied on) |
| `crates/shortwin-math/README.md`, `crates/shortwin-math/tests/quotes_mainnet.rs` | five fee branches with no mainnet trade | covered by the reference-model proptests; the re-scan is deferred (below) |
| `tests/bench/cu_math/README.md` | ARCH §1.1's old Agave resolver question | settled (U18) |
| `docs/design/sim/shortwin_sim.py`, `docs/design/sim/legacy/v0_3/shortwin_sim.py` (historical, the v0.3 sim) | the proportional boost rule | U15 |
| `docs/design/ARCHITECTURE.md` outside §12 (the changelog, the conventions, §1.1, §4.7, §5.4, §6.4, §7.1, §8.2, §10, Appendix A) | the conventions' definition, and the register's items where they are used | the §12 rows each cites: U5, U8, U15, U16, U22, U25, U34; the keeper's live-RPC load is OWNER-M7-9 (KI-38) |
| `docs/AUDIT.md`, `docs/audit/*` | this package | this section |

The stale markers the M10 plan named (ARCH lines 598, 917, 1389, 1736, 1804, 1824, 2448 at
`e857a2a`), the two U5 test-file markers and `withdraw.rs`'s U17 marker are gone: no marker is
left under `programs/`.

**AUD-12 cheap checks** (M10 audit preparation; M10 plan review C3):
- **U4 label.** Done (ARCH §12 U4). Every bench has a committed `results.md` from a passing
  run, and the benches fail CI on any figure over its budget.
- **U16, the Firedancer desk check.** Done, read-only:
  - Firedancer at `firedancer-io/firedancer@e8f25f34` (2026-10-01):
    - `src/flamenco/runtime/fd_slot_params.h` sets `max_writable_account_units` to
      24,000,000 and `max_block_units` to 60,000,000.
    - `fd_cost_tracker.c` multiplies both by 100/60 when the feature
      `raise_block_limits_to_100m` (`P1BCUMpAC7V2GRBRiJCNUgpMyWZhoqt3LKo712ePqsz`) is active:
      40,000,000 per writable account.
    - The pack tile takes the replay limits; `FD_PACK_MAX_WRITE_COST_PER_ACCT_UPPER_BOUND`
      is 100 M × 4/10 = 40 M (SIMD-0306), and the lower bound is 12 M.
  - Agave master at `anza-xyz/agave@e9035015` (2026-10-02):
    - `cost-model/src/block_cost_limits.rs` still has `MAX_WRITABLE_ACCOUNT_UNITS` = 24 M.
    - `runtime/src/slot_params.rs` `cost_limits(raise_block_limits_to_100m)` applies the same
      ×100/60, and `Bank::apply_cost_tracker_limits_for_active_features` installs it.
  - On mainnet (getMultipleAccounts at slot 452,488,037) the feature account says the feature
    was activated at slot 435,888,000.
  - Result: the effective limit is **40 M CU per writable account** (100 M per block) in both
    clients, not the 24 M that ARCH §0 [V10] and §6.6 state.
  - Consequences, none a program change (ARCH §0 [V10], §6.6 and ECON §17 now state 40 M):
    - censoring a market costs ×1.67 more than the 24 M assumption gave;
    - the keeper's `market_cu_per_slot` (12 M) is 30 % of the per-account budget;
    - the OWNER-M7-8 payer residual has more room.
- **The five fee branches with no mainnet trade** (`crates/shortwin-math/README.md:326`,
  `quotes_mainnet.rs:426`): **deferred**. A re-scan needs a crawl that looks for curve
  default-creator and per-curve or per-pool `creator_fee_bps` overrides, and CPMM exact-out
  trades with the creator fee on either side. The calibration crawler's decoders (GC-7) can
  flag them; otherwise M11.

---

## 9. Reproduce

These commands are for a Linux host with the pinned toolchain (`docs/DEVELOPMENT.md` "Setup":
rustup, Agave 4.1.2, Anchor 1.2.0, mise for Node and pnpm). The target is 30 minutes after
the toolchain is installed; the times in the comments are this package's gate (§10) on a
3-job build limit, with a warm `target/`.

They reproduce the audited tree: run them on a checkout of the tag `audit-1` (`v1.0.0`), whose
hashes are §0's. On a tree after PUMP-V3 P6 the workspace builds `shortwin` alone (the
`complete_curve` helper is retired, §0): drop `complete_curve.so` from step 2, and compare
`shortwin.so` with the hashes recorded for that tree, not the tag's (1.0.1: §0's last table,
§13).

```sh
# 1. Build both programs (release) and the TEST-ONLY test-venue build; check SBPF v3.
#    A clone has no program keypair: --ignore-keys silences Anchor's "Program ID mismatch"
#    warning (the id comes from declare_id!). Never run `anchor keys sync`.
anchor build --tools-version v1.57 --arch v3 --ignore-keys && scripts/build-test-venue.sh
od -An -t x4 -j 48 -N 4 target/deploy/shortwin.so            # 00000003
# 2. Hashes, compared with §0
sha256sum target/deploy/shortwin.so target/deploy/complete_curve.so
#    and the verifiable build, byte for byte (Docker; --local runs the same image without one)
scripts/verifiable-build.sh --compare target/deploy            # ≈ 4 min (a fresh clone)
# 3. Fixtures: every dumped mainnet account and ELF matches its checksum
(cd tests/fixtures && sha256sum -c SHA256SUMS) && python3 scripts/dump-fixtures.py check
# 4. The math crate (proptests at the CI count)
PROPTEST_CASES=2000 cargo test -p shortwin-math --locked
# 5. The program tests with the invariant hooks (≈ 20 min debug): the engine on the
#    test-venue build, every venue on the release build against the dumped mainnet ELFs
cargo test -p shortwin-litesvm-tests --locked
# 6. The fuzzer's smoke tests and corpus replays, then a short run on every profile
cargo test --release -p shortwin-fuzz --locked
cargo run -p shortwin-fuzz --release -- run --profile all --duration 10m --jobs 4 --out fuzz-out
```

The full gate (G-full in the M10 plan; CI's `ci.yml`) also runs fmt and clippy with
`-D warnings`, the CU benches, the services tests, mutants, `cargo deny` and `cargo audit`.

---

## 10. Evidence

**CI.** `.github/workflows/ci.yml` has these jobs:
- fmt, clippy, test (with `squads-v4-ix`), vectors, deny, wasm, cu-math;
- anchor-build, including A49 (the release-vs-test-venue check) and the release gate
  `check-security-txt.py --forbid-placeholders` on the release ELF (until PUMP-V3 P6 on both,
  `complete_curve.so` too);
- verifiable-build (solana-verify 0.5.2 in the pinned image, byte equality with
  anchor-build's artifacts, A49 on its output, the hashes in the job summary);
- program-tests, t-pot, codama (IDL and client diffs; both IDLs until PUMP-V3 P6), web, web-e2e;
- the benches m1-cu, m2-cu, m3-cu, m5-cu, m6-cu, m7-keeper-bench, and t-size;
- services (with `shortwin-gov`) and indexer;
- fuzz-smoke on every push; fuzz-nightly and mutants are opt-in. A `schedule` run (main only)
  runs them only when the repository variable `FUZZ_NIGHTLY` is `true` (owner decision
  2026-10-02, `4cb7729`). Otherwise they run on `workflow_dispatch`: fuzz-nightly only with a
  `fuzz_duration` input, mutants on every dispatch.

**CI runs:** the first green run on `main` is at `e857a2a` (run 36972628387).
- **The program-final head `4f4849e`:** run 37161377613
  (the development repository's CI, 2026-10-03), green.
  Every job a push runs succeeded (24 of 26; `fuzz-nightly` and `mutants` are opt-in and were
  skipped). `verifiable-build` ran in Docker mode and found both programs byte-identical to
  anchor-build's artifacts, with the hashes of §0: `shortwin` `bbef1075…f289` / executable
  `3214661b…ee09`, 1,575,216 B; `complete_curve` `f3a47201…6d42` / `2f2ef7da…82ab`, 122,264 B.
  `indexer` passed after the runner-disk fix `3d341c8`; the run before it (37130341616, on
  `cbde0ec`) failed only that job, on a full runner disk.
- **The head that carries this file:** pending. The commits after `4f4849e` are `997c730`
  and `945d45a` (`tests/fuzz` and one test-harness doc comment), the evidence sync (docs
  only) and `c6a9480` (`tests/fuzz`: the summary records every orphan-credit wind-down) — CI on
  `c6a9480`: run 37402221870 (2026-10-06), green, all 24 jobs a push runs succeeded (`fuzz-nightly`
  and `mutants` are opt-in) —, and then the **KI-65 batch**, which changes the program
  (`ShareEpoch.conv_rem`, `store_records`, error 6106; the new hashes of §0), the math crate, the
  normative sim and vectors, the JS client (writable records, the 143-byte codec), the web
  (`convRem` in the previews, `MAX_RECORDS`), the indexer's recorded sessions, the tests and the
  benches; then the **KI-65 follow-up** (`m10/ki65b`), which changes the program again
  (`sync_lender`'s `owner` account, error 6107 `RebaseNeedsOwner`; the hashes of §0), the JS client
  (the IDL, a Codama `before` visitor), the web, the fuzzer, the tests and the benches; then the
  **KI-66 batch** (`m10/ki66`), which changes the program again (`sync_lender`'s path selection,
  the pot-only touch; the hashes of §0), the math crate (`pool::touch_pot`), the normative sim and
  vectors, the JS client (IDL docs only), the web (`lender.subUnitConversion`), the fuzzer's
  oracle, the tests, the benches and the indexer's recorded sessions; then the **KI-66 full / KI-67
  batch** (`m10/ki67`), which changes the program again (the forward chain in `LenderPosition`,
  `claim_sol`'s close; the hashes of §0), the math crate, the normative sim and vectors, the JS
  client (the IDL's `fwd*` fields), the WASM facade and the web (the claim's pot-only syncs, the
  closing claim), the fuzzer, the tests, the benches and the indexer's recorded sessions. CI on
  that batch's head `55b3e23`: run 37816833577 (2026-10-08), all 24 jobs a push runs succeeded (as
  the owner's ninth round records it; `fuzz-nightly` and `mutants` are opt-in). Then the **FE-30
  batch** (`m10/fe30`: the web's claim candidates and its 3007 mapping, the indexer's additive
  `epoch` parameter, the docs; no program, IDL, math, sim or client change), the owner's
  eleventh-round docs, the fuzzer fix `9861c5a` (run D's FUZZ-24D-A and FUZZ-M10-24H-D-B;
  `tests/fuzz` and docs), the **M10-RESP batch** (`acbe323`: the web's phone and tablet layouts)
  and the **round-13 UX batch** (`7a64dcd`: the web) — no program change after `55b3e23`. CI on
  them (owner decision Q1: `main` is fast-forwarded after each verified step): the first push
  (`04c5236`, run 38123721574, 2026-10-11) failed only the `web` job's lint, on 7 errors in the
  M10-RESP and round-13 code (6 `noArrayIndexKey`, 1 `useJsxKeyInIterable`) that the local gates
  had passed — Biome 2.5.14 does not apply its `react` and `test` domains under a path with a
  `.cache` segment, where the work clones live, and the M10-RESP batch had dropped two suppressions
  `55b3e23` carried (M10-CI-LINT, known-issues §6); every other job that finished was green
  (`web-e2e` and `indexer` among them), and the run was cancelled after `web-e2e` to save Actions
  minutes (`program-tests` and `services` stopped). The **lint fix `7b2121d`**: `biome.json`
  enables both domains, so every checkout lints as CI does; the suppressions are back with their
  reasons, and one missing key is added (no rendering or text change). **CI on `7b2121d`: run
  38125079869 (2026-10-11), green, all 24 jobs a push runs succeeded** (`fuzz-nightly` and
  `mutants` are opt-in); the commits after it are docs only.
- **After the tag, the AI review's fixes and follow-ups** (`ai-review-fixes` = `main` @
  `4497ea6`; `air-followups`): no CI run; the owner asked for the branch to be pushed with
  `[skip ci]` (2026-10-13). Their local gates: the review's README "Local gate", and the integrated
  branch's below.
- **`pump-v3`** (short.win 1.0.1, pump v3 and the Mobile Wallet Adapter, 2026-10-13 … 16; §13):
  no CI run (not pushed). Its local gate follows.

**The pump v3 gate** (2026-10-16, a local run, not CI; `pump-v3` at `0879185`, the P8 docs head;
the m2-cu figures it re-wrote are committed in `b44908b`; WSL Ubuntu 24.04 on a 6 GB machine,
test binaries at 4 threads; a stand-in for every CI job that can run there; the logs are not
committed). The SBF builds ran in place, and every test loaded them (`SHORTWIN_SO`,
`SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`):

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `72c82879…1429` (executable `0610d419…2dfc`, 1,635,424 B), test-venue `shortwin.so` `fd46da2c…9db7` (1,666,464 B), `mock_cpamm.so` `4cf17007…7ab9` (281,240 B): §0's 1.0.1 table; the IDL equals the committed copy |
| SBPF v3; A49; security.txt | `e_flags` 0x3 on `shortwin.so`, `mock_cpamm.so` and the test-venue build; A49 ok (the release ELF is free of the mock id; `mock_cpamm.so` and the test-venue build carry it); `check-security-txt.py --forbid-placeholders --source-release v1.0.1` ok |
| fmt (with cu-math's); `shortwin-math`'s deny lints, `no_std`, no runtime dependency; clippy `-D warnings` on `-p shortwin-math`, the workspace and `-p shortwin` (the release feature set) | clean |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix` (`PROPTEST_CASES=2000`) | 399 passed, 0 failed |
| vectors; T-CONFIG; wasm; T-LAYOUT | every `gen_*.py` regenerates the committed vectors with no diff; `config_check.py` OK, `test_config_gate.py` 8 tests; the math crates build for `wasm32-unknown-unknown`, and `cargo check -p shortwin` there |
| fixtures; `cargo test -p shortwin` (the release feature set) | `SHA256SUMS` OK, `dump-fixtures.py check` 340 entries; 80 passed |
| **program-tests: `cargo test -p shortwin -p shortwin-venue-state -p shortwin-litesvm-tests`** | **1,289 passed, 0 failed, 5 ignored** (79 result groups: 76 test binaries and 3 doc-test runs) |
| T-SIZE; `build-alt.sh print` | `tx_size_raydium` 3, `tx_size_pump_it` 5, `tx_size_pumpswap_it` 4 passed; the ALT's static part 49 addresses |
| T-POT (`SHORTWIN_TRACES=full`, release) | 5 passed (stranding table: 40 seeds, 15,044 lamports, worst 435) |
| **fuzz-smoke** (`cargo test --release --locked -p shortwin-fuzz`, LTO off and 16 codegen units as CI) | 32 passed |
| benches cu-math, m1, m2, m3, m5, m6 (after the CPI wrappers' SBF builds, v1.57, SBPF v3) | 2 / 2 / 4 / 5 / 7 / 3 passed, every figure within its budget; fmt and clippy clean. cu-math, m3 and m5 re-write their committed files unchanged. m2's re-written `results.md` / `trace.md` are committed in `b44908b`: the version bump moved five stable rows by 9–10 CU, the rest is key noise (a second run compared). m1's and m6's differ from the committed files by the documented noise only (m6's second run equals them), so they are not committed |
| **services**: fmt, clippy (the workspace; the keeper alone); tests | clean; **264 passed, 0 failed, 3 ignored** (keeper scenarios 146, 2 ignored: the release measurements; T-OPS `gov_e2e` 2; `rpc_local` ignored); keeper A49 (the scenario binary embeds the mock id, the release keeper does not) |
| m7-keeper-bench (cost, PumpSwap ALT, cascade; release, the WRITE switches) | 3 passed. The re-written figures differ from `1710890`'s by run-to-run noise only (wall-clock times, the mock `expire`'s 19 CU, the cascade's per-exit requested-CU ranges, e.g. the mock `liquidate` 104,739–111,212 → 104,737–111,208); `keeper_cost.py` reproduces `out/keeper_cost.txt` on both, so they are not committed |
| **indexer** (embedded PostgreSQL as uid 1 through `unshare`: it refuses root) | `pg_fixture` 2; `shortwin-indexer` **127 passed, 3 ignored** (`ingest_sessions` 8, `api_replayed` 1, `security_ingest` 11 among them; the 3 ignored are `ingest_rpc_validator`'s); the replay harness **8 passed** on nine sessions (2,371 transactions; the projections equal the chain at all 178 checkpoints); `ingest_rpc_validator --ignored` against `solana-test-validator` 3 passed; the production indexer links no embedded Postgres |
| codama; client check | the IDL equals the build's; `generate` leaves no diff; typecheck; 29 client tests |
| web: `pnpm install --frozen-lockfile --offline`, the math-wasm build, the facade's Node smoke test, lint, typecheck, Vitest, the build | install ok; smoke ok; Biome 358 files clean; typecheck clean; **1,021 passed, 1 skipped** (87 files, 1 skipped); built |
| **web-e2e** (Playwright on `solana-test-validator`, the test-venue build) | the whole suite (projects chain, chain-phone, chain-android) **122 passed, 2 skipped** (18.3 min); the Mobile Wallet Adapter built off (`SHORTWIN_E2E_MWA=0`, chain-android) 8 passed, 19 skipped; the responsive gate (`SHORTWIN_RESPONSIVE_GATE=1`) **96 passed** |

Not run: CI (nothing pushed); `cargo deny` and `cargo audit` (not installed, and the advisory
database needs a download; since v1.0.0 the Cargo lockfiles change only workspace packages,
`complete-curve` removed and `shortwin` 1.0.1, and `pnpm-lock.yaml` gained the Mobile Wallet
Adapter's npm packages, which neither tool covers; CI's online `pnpm install --frozen-lockfile`
still has to pass the 14-day release-age policy); the verifiable build (§0, §13); `rpc_local`
(manual); `mutants`; a ≥ 24 h fuzz run (§13).

After this gate: `b44908b` (the m2 figures, above), then the final review's fixes (2026-10-16:
the indexer, the tests and the docs; no program, IDL or web change), each with its own runs in
its commit message (TODO.md names them).

**The AI review follow-ups' gate** (2026-10-13, a local run, not CI; `air-followups` at
`621f8e1`: the AI review's fixes and their follow-ups `cb756ca` … `621f8e1`, the program from
`cb756ca`; WSL Ubuntu 24.04 on a 12-core, 6 GB machine, test binaries at 4 threads; a local
stand-in for CI's jobs, `gate.sh`, plus the codama, web, fuzz-smoke, m7-keeper-bench and
indexer steps; the logs are not committed). The SBF builds ran in place (`shortwin`,
`complete_curve`, `mock_cpamm` and the test-venue build all recompiled; `complete_curve` came
out byte-identical to v1.0.0's) and every test loaded them:

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `fba9dd75…4fdf55` (1,634,936 B), `complete_curve.so` `f3a47201…6d42` (unchanged), test-venue `shortwin.so` `63ee71aa…b05c` (1,670,064 B), `mock_cpamm.so` `4cf17007…7ab9`: each byte-identical to the build at `63b673c`; both IDLs equal the committed copies (§0's AI-review table) |
| SBPF v3; A49; security.txt | `e_flags` 0x3 on both release ELFs and on `mock_cpamm.so`; A49 ok (the test-venue build carries its marker and the mock id, the release builds neither); `security.txt` valid (`--forbid-placeholders --source-release v1.0.0`), none in the test-venue build |
| fmt (with cu-math's); `shortwin-math`'s deny lints, `no_std`, no runtime dependency; clippy `-D warnings` on `-p shortwin-math`, the workspace, `-p shortwin`, the services and the keeper alone, the benches | clean |
| lockfiles | no change since v1.0.0 (`Cargo.lock`, `services/Cargo.lock`, the six bench lockfiles, `pnpm-lock.yaml`) |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix` (`PROPTEST_CASES=2000`, as CI's test job) | 376 passed, 0 failed |
| vectors; T-CONFIG; wasm | every `gen_*.py` regenerates the committed vectors with no diff; `config_check.py` OK, `test_config_gate.py` 8 tests; `wasm32-unknown-unknown` builds and `cargo check -p shortwin` there |
| audit scope; fixtures | `scope.txt` matches; `SHA256SUMS` OK, `dump-fixtures.py check` 265 entries |
| `cargo test -p shortwin` (the release feature set) | 75 passed |
| **program-tests: `cargo test -p shortwin -p shortwin-venue-state -p shortwin-litesvm-tests`** (T-SIZE's 11 among them) | **1,252 passed, 0 failed, 4 ignored** (78 result blocks; the four ignored on `main` too) |
| T-POT (`SHORTWIN_TRACES=full`); `build-alt.sh print` | 5 passed (stranding table: 40 seeds, 15,044 lamports, worst 435; `ext_pot_fwd_dust_100`'s 4 withdrawals replayed as `SolBelowMin` refusals, `251d6e8`); the ALT's static part 49 addresses |
| **services tests** | **252 passed, 0 failed, 3 ignored**: keeper lib 86, keeper scenarios 140 (2 ignored: the release measurements), `shortwin-ops` 15, T-OPS `gov_e2e` 2, services-common 9 (`rpc_local` ignored); keeper A49 (the scenario binary embeds the mock id, the release keeper does not) |
| indexer, without PostgreSQL | the unit and decoder tests 66 passed (`the_committed_sessions_decode_with_the_current_layouts` among them); the replay harness's 5 tests that need no database passed; the production indexer links no embedded Postgres |
| benches cu-math, m1, m2, m3, m5, m6 (after the CPI wrappers' SBF builds) | every figure within its budget; fmt and clippy clean. The re-written `results.md` / `trace.md` of m1, m2 and m6 differ by run-to-run noise on the bytes `fb60d70` measured: multiples of ≈ 1.5 k CU (the PDA bump search of fresh keys) and 19–40 CU on other rows; committed in `f33edf0`. cu-math, m3 and m5 unchanged |
| m7-keeper-bench (cost, PumpSwap ALT, cascade; release) | 3 passed. The committed `tests/bench/m7_keeper` and `m7_cascade` figures still name v1.0.0's build (`4d19c582…`); the re-written ones move the requested CU by up to +2,547 (PumpSwap's settling `sol_settle`, 74,445 → 76,992) and fail `keeper_cost.py`'s cross-check of its constants, so they are not committed: the sim, `out/keeper_cost.txt` and ECON §5.8 must move with them (TODO.md). The committed figures reproduce `out/keeper_cost.txt` |
| **fuzz-smoke** (`cargo test --release --locked -p shortwin-fuzz`, LTO off and 16 codegen units as CI) | **12 + 16 passed** (`corpus_replays` included); with `FUZZ_VERBOSE=1` FUZZ-M7-24H-3's deficit is 1,028,407,941,046 lamports against a bound of 1,028,409,223,206 (999,998 ppm), and FUZZ-M10-24H-A's CPMM lender stays owed 2,149 lamports (`lender_sol` 2,332), the shapes `621f8e1` and `70ac76e` revised the reproducers for |
| codama; client check | both IDLs equal the build's; `generate` leaves no diff and no untracked file; typecheck; 29 client tests |
| web: `pnpm install --frozen-lockfile --offline`, the math-wasm build, lint, typecheck, Vitest, the build | no lockfile change; the math-wasm `pkg/` up to date with its inputs' hash; Biome 334 files clean; typecheck clean; **769 passed, 1 skipped** (78 files, 1 skipped); built |

Not run: CI (nothing pushed); `cargo deny` and `cargo audit` (not installed here, and the
advisory database needs a download; no lockfile changed since v1.0.0, whose CI run 38125079869
passed both); the verifiable build (Docker is not available here: §0's AI-review hashes are not
verified); the `indexer` job's PostgreSQL tests (no embedded PostgreSQL here) and its
`ingest_rpc_validator` (a validator and PostgreSQL); `rpc_local` (manual: no CI job runs it; the
keeper track ran it against a local `solana-test-validator` at `8945087`, 1 passed, and
`services/common` has not changed since); `web-e2e` (no Playwright browser); a
timed fuzz run (the ≥ 24 h run on these bytes is not done: the owner did not ask for one on
2026-10-12 and waived it for this batch on 2026-10-13) and `mutants`.

After this gate, the final review of the follow-ups changed only the web, the WASM facade and
CI's `web` job (`ed014ce` AIR-10's exit risk cap in a position's risk view, `b1d86d4` AIR-03's
remainder in the unpaid-rent preview, `33491a3` the facade's Node smoke test in CI; the program
bytes, the IDL and the client unchanged). Their own runs, as their commit messages record: CI's
`web` job locally (Biome 334 files, typecheck, **771 passed, 1 skipped**, the build), the
facade's tests (26 passed), its clippy and fmt, and `js/smoke.cjs` on a Node build.

**The web batches after FE-30** (the web only; the program bytes, the IDL, the client and the
indexer unchanged; each batch's gate as its commit message records it):
- **The M10-RESP batch** (m1-core `acbe323`, 2026-10-09/10, the tree of `m10/resp` `2cff70b`: phone
  and tablet layouts and the desktop terminal's fixes, the owner's twelfth-round request; approved as
  built by the owner in the thirteenth round, 2026-10-10): biome (in a work clone under `~/.cache`,
  which skips Biome's React rules: the lint fix above), typecheck, Vitest **681 passed**,
  the build (0 `<style>` tags, 0 `style` attributes); the web **E2E 77 / 77** (the desktop and phone
  projects); the **responsive gate 80 / 80** at 8 viewports (320 × 693 to 1440 × 900): 0 document
  overflow, 0 targets under 24 px, 0 phone text under 12 px, 0 axe violations. The approved
  disclosure texts, every `data-testid`, simulate → review → sign and the CSP unchanged.
- **The round-13 UX batch** (m1-core `7a64dcd`, 2026-10-11, the tree of `m10/ux` `1405e74`: O-1 lend
  order, O-2 coverage notice once, O-3 newcomer help, O-4 12 px floor, O-5 Claim wording; the
  owner's approval and decisions of 2026-10-11 — the 143 new strings approved word for word, one
  sentence of the approved disclosure `borrower.liquidation` ¶3 amended (the expiry fee made
  conditional, SPEC §5.3), the Claim's 10,000-lamport dust threshold, the Lending page's Claim
  with the pot syncs): biome (in work clones under `~/.cache`, which skip Biome's React rules: the
  lint fix above), typecheck, Vitest **760 passed** (78 files, 1 opt-in skipped), the build (0
  `<style>` tags, 0 `style` attributes); the web
  **E2E 95 / 95** (69 desktop, 26 phone) on `59ea55c`'s code (the commits after it change no app
  code); the **responsive gate 96 / 96** at 8 viewports on `487e737`'s code (63–65 states each: 0
  document overflow, 0 off screen, axe 0, 0 targets under 24 px, 0 text under 12 px, 0 tap covers;
  11,520 controls judged). Every `data-testid` (one added, `tx-sign-hold`), simulate → review →
  sign, the approved disclosure texts but ¶3, the CSP and the breakpoint set unchanged; no new
  dependency.

CI on both: green on `7b2121d` (run 38125079869, above), after the lint fix.

**The review fix round's gate** (2026-10-08, on `m10/ki67` at `6e724b5`, then `182e104` /
`b72b3a3` for the steps after the new park test and the fuzzer's overflow fix; logs
`~/.cache/shortwin-work/m10/ki67-gate/fix/logs`, drivers `chainA.sh` … `chainD.sh`, `SUMMARY`).
The three reviews of the batch (security, spec, web) found no program defect; the fix round
changes the web, the harness (hooks, fuzzer, tests, mutants), the sim's printout and the docs,
not the program, so the full gate's SBF builds (`target/gate-sbf`, `target/gate-tv`; release
`4d19c582…1c1f75`) were loaded and the verifiable build and the benches were not re-run
(known-issues KI67-REV, FUZZ-KI67-FIX-1; KI-68 disclosed):

| Step | Result |
|---|---|
| fmt; clippy `-D warnings` on the workspace (all targets) and on the LiteSVM tests and the fuzzer after the last two commits | clean |
| `PROPTEST_CASES=2000 cargo test -p shortwin-math --release`; `cargo test -p shortwin-math -p shortwin-math-wasm` | 323 and 346 passed (the security review's `fwd_dynamic_props` adopted: 2 more); 0 failed |
| `fwd_dynamic_props` at `PROPTEST_CASES=200000` | passed: 1,317,259 third-party syncs, 643,627 left a held chain, 33,257 an open one, 29,867 owners stopping inside a chain |
| **`cargo test --release -p shortwin-litesvm-tests`** (every LiteSVM test: the close hook now replays a withdrawal's prologue, `pool::withdraw` and `pool::claim`) | **1,080 passed, 0 failed, 4 ignored** (74 result blocks; `ki67_sec_it` 6 adopted); then `ki66_sec_it` 14 passed with the new park test; T-POT 5 passed; a probe that fed the replay one share too few failed 17 `lender_it` tests (the replay is exercised) |
| vectors; sim | every `gen_*.py` reproduces the committed vectors; `pool_model.py` reproduces `out/pool_model.txt`, which changed only by the new measurements (the differential line's counters, and the 5,000-pool line: 4 of 16,056 touches over-moving 4 lamports; 6 of 4,839 closable positions with a dead lock term) |
| fuzz crate tests | 26 passed (twice: before and after the overflow fix); `corpus_replays` with the new corpus file |
| **fuzz, 10 min each** (fresh seeds: all `67800000`, 4 jobs; mock `67800100`, cpmm `67800200`, pumpswap `67800300`, 3 jobs) | 295 / 363 / 445 / 203 runs, 543,641 / 522,627 / 698,380 / 463,739 transactions; **0 anomalies**; failures 0 / 0 / 1 / 0 — the cpmm one is FUZZ-KI67-FIX-1, the fuzzer's own `u128` overflow in the wind-down's dust test (pre-existing: the `f48e75d` fuzzer fails its reproducer too), fixed; the cpmm run again after the fix (same seed): 379 runs, 594,845 transactions, 0 failures, 0 anomalies. Chains started 0 / 1 / 4 (2 in the rerun) / 2, walked further by a third party 0, owners stopping inside one 0 (KI66F-FUZZ-COVERAGE stays open in the fuzz); `absent-residual-differs` 0; no present lender owed above dust; `claim_sol` closed 8 / 3 / 11 / 5 positions |
| **mutant `ki66full-park-keeps-chain`** (`scripts/mutants.sh`, 4 jobs) | first **survived** `ki66_sec_it` (no test had an owner park inside its chain); **killed** by the new `an_owner_parking_inside_its_chain_on_a_live_wipe_settles_it`. The other 18 were killed at `36f43cc` (below) and not re-run |
| web: lint, typecheck, build; Vitest | clean (the 2 existing warnings); **599 passed, 1 skipped** (60 files; 3 new: the funded lender PDA in `readLendChain` and `readOtherLenders`, short.win's 3012 → rebuild) |
| **web E2E** (all specs, under the E2E lock, the test-venue build) | **60 passed** (11.3 min); no validator left running; the ledger's ephemeral keypairs deleted |
| `python3 scripts/audit_scope.py check` | ok |

Not run in the fix round: CI (nothing pushed); the verifiable build, A49, the benches, services,
codama, `cargo deny` / `cargo audit` (no program, IDL, service or lockfile change); the other 18
mutants. **The ≥ 24 h fuzz then running on `f48e75d` covers this head's program bytes but not its
fuzzer or hooks** (the windowed syncs, the residual cross-check, the withdrawal replay, the
overflow fix: FUZZ-KI67-FIX-1 can stop one of its runs); a ≥ 24 h run on this head was owed
(since done: run D on `55b3e23`, then run E on `9861c5a` after run D's fixes; "Fuzz" below).

**The batch's full gate** (2026-10-08, on `m10/ki67` at `36f43cc`: the KI-66 full / KI-67
batch and the web batch together, every step of CI's jobs, web and E2E included; the machine's
3-slot wrapper; logs `~/.cache/shortwin-work/m10/ki67-gate/full/logs`, the drivers
`chain1.sh` … `chain3.sh` and `SUMMARY`). The SBF builds went to a fresh `target/gate-sbf` and
`target/gate-tv`, and every test loaded them (`SHORTWIN_SO` and the others), because the
≥ 24 h fuzz running on `f48e75d` re-reads `target/deploy`; the clean builds are byte-identical
to those ELFs:

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` (clean target directories) | `shortwin.so` `4d19c582…1c1f75` (1,609,616 B), `complete_curve.so` `f3a47201…6d42`, test-venue `shortwin.so` `8cea17e1…bb98`, `mock_cpamm.so` `4cf17007…7ab9`: each byte-identical to the batch's earlier build; the IDLs equal the committed ones |
| A49; security.txt | `A49-OK`; `SECTXT-OK` (`--forbid-placeholders --source-release v1.0.0`) |
| fmt (with cu-math's); `shortwin-math`'s deny lints, `no_std`, no runtime dependency; clippy `-D warnings` on `-p shortwin-math`, the workspace, `-p shortwin`, the benches, the services and the keeper alone | clean |
| deny / audit | `cargo deny check` (root and services): advisories, bans, licenses, sources ok; `cargo audit --deny warnings` (CI's five exceptions) on `Cargo.lock`, the six bench lockfiles and `services/Cargo.lock`: clean |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix`; `PROPTEST_CASES=2000 cargo test -p shortwin-math --release` | 357 passed (the facade's 23 included); 321 passed; 0 failed |
| vectors / sim; T-CONFIG; wasm | every `gen_*.py` regenerates the committed vectors with no diff; `pool_model.py` reproduces `out/pool_model.txt`; `config_check.py` OK, `test_config_gate.py` 8 tests; `wasm32-unknown-unknown` builds and `cargo check -p shortwin` there |
| `cargo test -p shortwin` (the release feature set) | 69 passed |
| **`cargo test --workspace --locked`** | **1,604 passed, 0 failed, 4 ignored** (114 result blocks) |
| T-POT (`SHORTWIN_TRACES=full`); T-SIZE and `build-alt.sh print`; cu-math | 5; 11 (the ALT's static part 49 addresses); 2 passed |
| benches m1, m2, m3, m5, m6 (after the CPI wrappers) | 2 / 4 / 5 / 7 / 3 passed, every figure within its budget; the re-written `results.md` / `trace.md` differ from the committed ones only by PDA-bump noise (`create_market`, the deposit rows, the m2 opens, one liquidate), so the committed figures, measured on these bytes, were kept |
| m7-keeper-bench (cost, PumpSwap ALT, cascade); `keeper_cost.py` | 3 passed; `results.json` names `4d19c582…` and `8cea17e1…` as committed; the rest differs by wall-clock and CU noise only (kept as committed); `out/keeper_cost.txt` reproduces on both the re-written and the committed figures |
| codama; client check; fixtures | the IDLs equal the build's; the regenerated client has no diff; typecheck; 29 client tests; `SHA256SUMS` OK, `dump-fixtures.py check` 265 entries |
| **services tests** | **351 passed, 0 failed, 6 ignored**; the ignored RPC test `ingest_rpc_validator` against `solana-test-validator` 3 passed; the production indexer links no embedded Postgres; keeper A49 (the scenario binary embeds the mock id, the release keeper does not) |
| the indexer sessions | recorded again (`SHORTWIN_RECORD_SESSIONS`): 141 and 840 records, as committed; the bytes differ by fresh keys, signatures, event data and PDA-bump CU only — with those masked, 0 records differ — so the committed sessions stay |
| fuzz crate tests | 26 passed (4 result blocks) |
| **fuzz, 10 min each** (fresh seeds: all `67500000`, 4 jobs; mock `67500100`, cpmm `67500200`, pumpswap `67500300`, 3 jobs; the machine loaded by the 24 h run and other jobs) | 247 / 352 / 341 / 160 runs, 472,725 / 520,212 / 545,801 / 355,629 transactions; **0 failures, 0 anomalies**; no present lender owed above dust; chains started 0 / 1 / 8 / 2, settled by their owners 0 / 0 / 8 / 2; `claim_sol` closed 3 / 3 / 10 / 2 positions, 3 / 3 / 7 / 1 bundles refused after the close |
| **mutants** (`scripts/mutants.sh`, `MUTANTS_JOBS=3`, 4,173 s) | **18 / 18 killed by their tests**; the seven fuzz-checked ones killed by a 3-minute fuzz each (10 / 5 / 10 / 111 / 33 failures on `mock`, 15 on `curve`) except `pump-caps-live-fee`, whose `pumpswap` run survived twice at 3 jobs (78 and 73 runs: its first failing seed is the 83rd of the stream, which a loaded machine does not reach in 3 minutes) and was killed at the script's default `MUTANTS_JOBS=4` (101 runs, seeds `20261083` and `20261089`) |
| verifiable build (`--local --compare` against the clean build) | **byte-identical for both programs at `36f43cc`** (`shortwin` `4d19c582…1c1f75` / executable `52a29a51…186b`, 1,609,616 B; `complete_curve` `f3a47201…6d42` / `2f2ef7da…82ab`, 122,264 B); A49 on its output |
| web: `pnpm install --frozen-lockfile`, `pnpm -r build`, lint, typecheck, Vitest; the lend vectors | no lockfile change; built; clean (the 2 existing warnings); **596 passed, 1 skipped** (60 files); `gen_lend_vectors.py` regenerates `lend_vectors.json` with no diff |
| **web E2E** (all specs, under the E2E lock, the test-venue build above) | **60 passed** (12.0 min); no validator left running |
| `python3 scripts/audit_scope.py check` | ok |

Not run: CI (nothing pushed); the ≥ 24 h fuzz finished after this gate (run C, "Fuzz" below: on
`f48e75d`, whose program, fuzzer and harness bytes are this head's: the commits after it change the
WASM facade, the web, the E2E and docs).

**The KI-66 full / KI-67 batch's gate** (2026-10-08, on `m10/ki67`, the program from `f47b0d8`
and the math crate from `6aad5a5`: owner decisions of 2026-10-08, eighth round — a third party's
pot-only touch walks the lender's forward chain, `claim_sol` closes an empty position; the
machine's 3-slot wrapper; logs `~/.cache/shortwin-work/m10/ki67-gate/gate/logs`, the drivers
`gate1.sh` … `gate5.sh` and `SUMMARY`). Every step of CI's jobs except the web's (the web batch,
KI66-R4, gated below):

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `4d19c582…1c1f75` (1,609,616 B, +20,720), `complete_curve.so` `f3a47201…6d42` (unchanged), test-venue `shortwin.so` `8cea17e1…bb98`, `mock_cpamm.so` `4cf17007…7ab9` |
| A49; security.txt | `A49-OK`; `SECTXT-OK` (`--forbid-placeholders --source-release v1.0.0`) |
| fmt; clippy `-D warnings` on the workspace, `-p shortwin`, `-p shortwin-math`, the benches, the services | clean |
| lockfiles | no change |
| vectors / sim; T-CONFIG | every `gen_*.py` regenerates the committed vectors with no diff (`pool_*`: the lender state's four `fwd*` fields, op `y U N S`, `ext_pot_fwd_{100,deep_100,dust_100,open_100}` — the last: 526 `FWD_OPEN` states, 148 at lag 1, 128 at `fwd_epoch` before the current epoch —, `pot_chain_demo`); `pool_model.py` reproduces `out/pool_model.txt` byte-identically (only the liveness row changed — 0 of 668 above dust —, plus `pot_chain_demo`, `fwd_differential(2000)` and `latent_residual_table`); `config_check.py` OK, `test_config_gate.py` 8 tests |
| wasm | `shortwin-math` and the facade on `wasm32-unknown-unknown`; `cargo check -p shortwin` there |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix`, then `PROPTEST_CASES=2000 cargo test -p shortwin-math`, then `pool_props` at 20,000 | 355 passed (after `1e46fa0`: a debug test thread's stack overflowed on `lag_part`'s case array, moved to the heap), then 321 at 2,000 cases, then 7 at 20,000; 0 failed (new: `fwd_lag_at_most_one`, `lag_part_is_the_difference_of_two_floors`, `windowed_syncs_equal_owner_alone`, `pot_only_touch_is_sound` with the chain) |
| **`cargo test --workspace --locked`** | **1,601 passed, 0 failed, 4 ignored** (114 result blocks) |
| lender suites (release, at the final head `66accb3`) | `ki65_it` 19, `ki66_sec_it` 13, `lender_it` 51 passed (and in the gate `lender_real_it` 14, `pool_attacks_it` 17): every `order_*`, W1, W2 and W3 pin unchanged (the `order_*` measured as SOL plus the rent their `withdraw` refunded); `pot_chain_demo`'s figures |
| T-POT (`SHORTWIN_TRACES=full`), T-SIZE, cu-math | 5 (the trace replay runs op `y` and `ext_pot_fwd_*`; the R3A-2 row exact); 11; 2 passed |
| benches m1, m2, m3, m5, m6 | every figure within its budget; m1's new rows (ARCH §6.1): the owner settling a chain over 8 records 49,654 / 49,953 / 47,607, the unsigned 8-record sync 47,989, the lag-1 worst case 57,927 / 58,229 / 55,883 at 7 records and 64,053 / 64,355 / 62,009 at 8 (reported against the transaction limit: the web caps a chained settlement at 7), `claim_sol` closing a wiped position 19,536; the others PDA-bump noise (m3, m5, m6 after building their CPI wrappers) |
| m7-keeper-bench; `keeper_cost.py` | 3 passed (re-written, noise; `results.json` names `4d19c582`); `out/keeper_cost.txt` regenerated (its cross-check comment) |
| codama; client check; fixtures | the IDL gains `LenderPosition.fwd_shares`, `fwd_epoch`, `fwd`, `fwd_lag` (`_reserved` 32 → 10) and docs; the regenerated client has no diff; 29 client tests; `dump-fixtures.py check` 265 entries, `SHA256SUMS` OK |
| **services tests** | fmt, clippy clean; **351 passed, 0 failed, 6 ignored**; the mock lifecycle's wiped lender now claims and its position closes (KI-67; the replay test requires it); the two committed sessions re-recorded on this program, and the suite passes on them again (351 / 0 / 6) |
| fuzz crate tests | 26 passed (12 + 14; `absent_owners_at_the_wind_down`'s three seeds each start a chain; the KI-65 reproducers unchanged) |
| **fuzz, 10 min each, round 1** (`--seed 67100000` all, 4 jobs; mock `67100100`, cpmm `67100200`, pumpswap `67100300`, 3 jobs) | 521 / 667 / 733 / 360 runs, 979,953 / 969,042 / 1,148,946 / 801,641 transactions, **0 failures**; anomalies 4 / 4 / 9 / 0, all `framework-error:claim_sol:AccountNotInitialized` — the claim action's own KI-67 bundle (a second claim of the position the first one closed), which its oracle requires; reclassified in `4898947` (`claim-after-close-refused`) |
| **fuzz, 10 min each, round 2** (after `4898947`; `--seed 67200000` all, 4 jobs; mock `67200100`, cpmm `67200200`, pumpswap `67200300`, 3 jobs) | 513 / 741 / 835 / 299 runs, 1,024,512 / 1,129,075 / 1,318,350 / 735,986 transactions; **0 failures, 0 anomalies**; the never-returns wind-down on 5 / 1 / 5 / 0 markets, no present lender owed above dust; chains started 11 / 2 / 9 / 0, settled by their owners 11 / 1 / 9 / 0 (no third-party walk past a second record and no owner stopping inside a chain: the fuzz reaches one rebase per epoch rarely, so `FWD_OPEN` is covered by the vectors, the properties and `ki66_sec_it`); `claim_sol` closed 8 / 11 / 16 / 6 positions, 6 / 8 / 10 / 6 bundles refused after the close |
| verifiable build (`--local --compare`) | byte-identical for both programs at `25e95f2` (the hashes of §0; solana-verify 0.5.2, the pinned image) |
| **mutants** (`scripts/mutants.sh`) | **18 / 18 killed** after `66accb3` (`MUTANTS_JOBS=3`, 2,621 s, then the one rerun): the seven by their tests and a 3-minute fuzz each (10 / 6 / 10 / 127 failures on `mock`, 37 for the clear delay, 16 on `curve`, 2 on `pumpswap`); by their tests alone the KI-65 / KI-66 three and the eight of this batch — `ki66full-jump-keeps-chain` first survived (every jump the tests reached had the chain at the current epoch, where the touch after it drops it the same way) and is killed by `ki66_sec_it` `a_jump_inside_a_stale_chain_drops_it` |
| `python3 scripts/audit_scope.py check` | ok |

Not run: CI (nothing pushed), the ≥ 24 h fuzz on this head (started after the gate; §10's run
list), the web's steps (the web batch, below).

**The web batch's gate** (KI66-R4 and KI-67 in the web, 2026-10-08, on `m10/ki67` at `a3b678d`;
no program, math-crate or client change — the WASM facade only; logs
`~/.cache/shortwin-work/m10/ki67-gate/web-logs`, driver `webgate.sh`, `GATE`):

| Step (CI job) | Result |
|---|---|
| `pnpm install --frozen-lockfile`; `pnpm -r build` | no lockfile change; built |
| web: lint, typecheck, Vitest | clean (the 2 existing lint warnings); **596 passed, 1 skipped** (60 files; new `build.test.ts`; `previews.test.ts`'s vectors add `touchPot` and `claimOutcome` against `pool_model` — `gen_lend_vectors.py` regenerates `lend_vectors.json` with no diff) |
| codama / client check | the IDL equals the build's; the regenerated client has no diff; typecheck; 29 tests |
| the facade | fmt, clippy `-D warnings`, 23 tests (`the_pot_only_touch_holds_and_walks_on_copies`, `a_claim_that_empties_the_position_closes_it`), `wasm32-unknown-unknown` check |
| **web E2E** (all specs, under the E2E lock) | **60 passed** (9.9 min; the test-venue `shortwin.so` `8cea17e1…bb98`): `lend-claim-pot-chain` lands `[sync_lender(absent, 1), claim_sol]` after an organic rebase — the claim alone 10,345 lamports, the absent lender's surplus 493, the pot draw 471, paid 10,816, nothing left owed — and `lend-post-wipe`'s last claim closes the position (its 2,568,240 lamports of rent back) |

**The KI-66 batch's gate** (2026-10-07/08, on `m10/ki66`, the program from `e19e0c8` and the
math crate from `e639558`: owner decision of 2026-10-07, seventh round — anyone else's
`sync_lender` of a lender behind a rebase record is the pot-only touch; the machine's 3-slot
wrapper; logs `~/.cache/shortwin-work/m10/ki66/logs`, the drivers `gate1.sh`, `web.sh`,
`gate2.sh` and, after a machine reboot during gate 2, `rest.sh`: the rebuild reproduced the
gate's ELFs byte for byte before the remaining steps ran). Every step of CI's jobs:

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `42b11bc4…60cd` (1,588,896 B, +5,272), `complete_curve.so` `f3a47201…6d42` (unchanged), test-venue `shortwin.so` `9296be90…542a`, `mock_cpamm.so` `4cf17007…7ab9` |
| A49; security.txt | `A49-OK`; `SECTXT-OK` (`--forbid-placeholders --source-release v1.0.0`) |
| fmt; clippy `-D warnings` on the workspace, `-p shortwin`, `-p shortwin-math`, the benches, the services | clean |
| lockfiles | no change; `cargo deny` / `cargo audit` not re-run (tools absent locally) |
| vectors / sim; T-CONFIG | every `gen_*.py` regenerates the committed vectors with no diff (new: op `h`, `pool_ext_pot` — 3 profiles —, `pot_touch_demo` in `pool_scenarios`; every older vector byte-identical); `pool_model.py` reproduces `out/pool_model.txt` byte-identically (new: the KI-66 `pot_touch_demo` and `pot_liveness_table`); `config_check.py` OK, `test_config_gate.py` 8 tests |
| wasm | `shortwin-math` and the facade on `wasm32-unknown-unknown`; `cargo check -p shortwin` there |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix`, then `PROPTEST_CASES=2000 cargo test -p shortwin-math` | 352 passed, then 318 at 2,000 cases; 0 failed (new: `ext_pot_only_touch`, `pot_only_touch_holds_at_a_rebase_record`, `TouchPot` in the random-op mix: held unconverted at the record's ends, idempotent, the owner's later conversion counting nothing twice) |
| **`cargo test --workspace --locked`** | **1,581 passed, 0 failed, 4 ignored** (113 result blocks; incl. `ki65_it` 18/18 — every `order_*`, W1, W2 and W3 figure unchanged and pinned, `a_surplus_behind_a_rebase_record_reaches_the_pot_without_its_owner`: pot 4,434,782,609, A's next claim 2,957,608,696, A owed 0, C later 2,000 shares and 579,710,144 —, the program's `reads_rebase` test, which also checks `touch_pot` never reads a rebase record, `lender_it` 49/49), 1,570 s |
| T-POT (`SHORTWIN_TRACES=full`), T-SIZE, cu-math | 5 (the R3A-2 row exact: 40 seeds, 19,887 lamports, worst 582; the trace replay runs op `h` and `pool_ext_pot`); 11; 2 passed |
| benches m1, m2, m3, m5, m6 | every figure within its budget; m1 rewritten (`results.md` is one run on this program, so its other rows carry PDA-bump noise): new rows `sync_lender` by a third party, the pot-only touch, 16,673 / 31,575 CU with 1 / 8 rebase records passed; the owner's conversions +32–64 CU (the shared touch tail: `claim_sol` 54,258, `cover_deficit` 54,581, `sync_lender` 52,240 at 8 records); the unsigned plain touch of a current lender 13,950 (−177); m2's rows noise only (not committed) |
| m7-keeper-bench; `keeper_cost.py` | 3 passed (re-written, noise; `results.json` names `42b11bc4`); `out/keeper_cost.txt` regenerated (its cross-check comment names the build) |
| codama; client check; fixtures | the IDL changes only in `sync_lender`'s docs; the regenerated client has no diff; 29 client tests; `dump-fixtures.py check` 265 entries, `SHA256SUMS` OK |
| web: build, lint, typecheck, Vitest; **web E2E** (all specs, under the E2E lock) | clean (2 existing lint warnings); 577 passed, 1 skipped (`previewSubUnitConversion`, the plans' `lender.subUnitConversion` last with `{vestEnd}`, a `LendPanel` + `TxReviewModal` component test of the claim's review; after the FRONTEND sync `disclosures.test.ts` checks both new texts verbatim); **58 passed** (6.9 min) |
| **services tests** | fmt, clippy clean; `cargo test --manifest-path services/Cargo.toml`: **351 passed, 0 failed, 6 ignored** (33 result blocks); the two committed sessions re-recorded on this program (same 836 / 138 transactions; only the program account's balance and the signatures change; the indexer's 117 tests pass on them) |
| fuzz crate tests | 25 passed (the KI-65 regression unchanged: run B's three reproducers and the curve corpus file at 125 / 198 / 495 / 50 lamports) |
| **fuzz, 10 min each** (the KI-66 oracle: nobody's `sync_lender` is refused `RebaseNeedsOwner`; a third party's that lands leaves exactly the lender and `MarketVault` of `pool::touch_pot` on the pre-state and no record changed) | `--profile all --seed 65107000 --jobs 4`: 543 runs, 1,023,084 transactions; mock `--seed 31013600`, cpmm `--seed 31005900`, pumpswap `--seed 31016900` (`--jobs 3`): 749 / 780 / 350 runs; **0 failures, 0 anomalies**; 25 / 27 / 31 / 20 epoch rolls; `sync_lender` landed 534 / 663 / 674 / 403, refused 0; at most 1,615 lamports of `lender_sol` and 0 owed after the second claim pass |
| verifiable build (`--local --compare`) | byte-identical for both programs at `45327c1` (the hashes of §0; solana-verify 0.5.2, the pinned image) |
| **mutants** (`scripts/mutants.sh`) | **10 / 10 killed** (`MUTANTS_JOBS=3`, 2,080 s): the seven by their tests and a 3-minute fuzz each (10 / 6 / 10 / 123 failures on `mock`, 37 for the clear delay, 16 on `curve`, 2 on `pumpswap`); by `ki65_it` alone `ki65-skip-store-records` (12 of 18 fail), `ki65-rebase-without-owner` — since KI-66 the path selection taking every caller for the owner — (6 of 18, the pot test among them) and the new `ki66-pot-skip-surplus`, the pot-only touch skipping the surplus move (`a_surplus_behind_a_rebase_record_reaches_the_pot_without_its_owner` fails) |
| `python3 scripts/audit_scope.py check` | ok (nLOC in §2) |

**The KI-66 reviews' fix round** (2026-10-08, on `m10/ki66` after `9ffb7cd`; KI66-REV-LOW in
the register; logs `~/.cache/shortwin-work/m10/ki66/fix`). No program, math-crate source, sim,
vector, client or lockfile change, so the release hashes of §0 stand (`shortwin.so`
`42b11bc4…60cd` checked before the runs); the changed parts re-ran:

| Step | Result |
|---|---|
| fmt; clippy `-D warnings` on the workspace (all targets) | clean, at the final head |
| `PROPTEST_CASES=2000 cargo test -p shortwin-math` | all pass; `pot_only_touch_is_sound` now recomputes the held lender's accrual and requires the exact surplus move: with `ki66-pot-skip-surplus` applied, `pot_only_touch_holds_at_a_rebase_record` and `invariants_hold_after_every_operation` fail (before: only the vectors) |
| `ki66_sec_it` (the security review's adversarial tests, adopted) and `ki65_it` | 6 / 6 and 18 / 18 passed |
| web: lint, typecheck, Vitest; **web E2E** (all specs, under the E2E lock) | clean (2 existing lint warnings); 578 passed, 1 skipped (`lender.subUnitConversion` on the worst case: a remainder that would keep a share now is flagged, two chained records); **58 passed** (7.8 min) |
| fuzz crate tests (release, at the final head) | 26 passed (12 + 14; new `absent_owners_at_the_wind_down`: three mock seeds held at the wind-down, a surplus moved by each first third-party sync, no anomaly; the KI-65 reproducers unchanged at 125 / 198 / 495 / 50 lamports) |
| **fuzz, 10 min each** (the KI-66 gate's seeds; the never-returns wind-down in one run in two) | all `--seed 65107000 --jobs 4`, mock, cpmm, pumpswap: 443 / 513 / 682 / 351 runs (a loaded machine), 831,104 / 772,569 / 1,100,358 / 806,702 transactions; **0 failures, 0 anomalies**; the never-returns wind-down on 4 / 7 / 4 / 2 markets (35 absent lenders), `sync-lender-held` 16 / 31 / 21 / 8 (6 mid-run), 25 moving a surplus; present lenders owed above dust 0 times; at most 1,615 lamports of `lender_sol` and 0 owed after the second claim pass |

Not re-run (inputs unchanged): the build, verifiable build, benches, services, codama, vectors
and sim, mutants (`scripts/mutants.sh`'s ten stand; `ki66-pot-skip-surplus` is now also killed
by `pool_props` and by the fuzz smoke, checked by hand).

**The KI-65 follow-up's gate** (2026-10-07, on `m10/ki65b`, the program from `79cacd9`: owner
decision of 2026-10-07, KI65-REV-SEC1 — only a position's owner converts it through a rebase
record; the machine's 3-slot wrapper; logs `~/.cache/shortwin-work/m10/ki65/gateb/logs`). Every
step of CI's jobs:

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `cdbd2e70…c50a` (1,583,624 B, +1,600), `complete_curve.so` `f3a47201…6d42` (unchanged), test-venue `shortwin.so` `0d8cc621…a1fd`, `mock_cpamm.so` `4cf17007…7ab9` |
| A49; security.txt | `A49-OK`; `SECTXT-OK` (`--forbid-placeholders --source-release v1.0.0`) |
| fmt; clippy `-D warnings` on the workspace, `-p shortwin`, `-p shortwin-math`, the benches | clean |
| lockfiles | no change (no dependency added; the Codama visitor is the pinned `codama` package's own); `cargo deny` / `cargo audit` not re-run (tools absent locally; the lockfiles are those of batch 4's results) |
| vectors / sim; T-CONFIG | `pool_model.py` reproduces `out/pool_model.txt` byte-identically (its edits are comments); `config_check.py` OK, `test_config_gate.py` 8 tests |
| wasm | `shortwin-math` and the facade on `wasm32-unknown-unknown`; `cargo check -p shortwin` there |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix`, then `PROPTEST_CASES=2000 cargo test -p shortwin-math` | 350 passed, then 316 at 2,000 cases; 0 failed |
| **`cargo test --workspace --locked`** | **1,577 passed, 0 failed, 4 ignored** (113 result blocks; incl. `ki65_it` 16/16, the program's `reads_rebase` test against the math crate, `lender_it` 49/49), 1,457 s |
| T-POT (`SHORTWIN_TRACES=full`), T-SIZE, cu-math | 5 (the R3A-2 row exact: 40 seeds, 19,887 lamports, worst 582); 11; 2 passed |
| benches m1, m2, m3, m5, m6 | every figure within its budget; m1's `sync_lender` rows re-measured (owner-signed behind records): 14,127 on the current epoch (+341), 19,017 / 28,463 / 52,206 at 1 / 3 / 8 rebase records (+326), 62,496 / 71,974 at 10 / 12 (beyond the web's cap of 8); `claim_sol` / `cover_deficit` unchanged (54,194 / 54,549 at 8); the other rows PDA-bump noise only (not committed) |
| m7-keeper-bench; `keeper_cost.py` | 3 passed (re-written, noise; `results.json` names `cdbd2e70`); `out/keeper_cost.txt` regenerated (its cross-check comment names the build) |
| codama; client check; fixtures | the IDL gains `sync_lender`'s `owner` and error 6107; the regenerated client (with `codama.json`'s `before` visitor: `owner` an either-signer) has no diff; 29 client tests; `dump-fixtures.py check` 265 entries, `SHA256SUMS` OK |
| web: build, lint, typecheck, Vitest; **web E2E** (all specs, under the E2E lock) | clean (2 existing lint warnings); 570 passed, 1 skipped (the own sync's `owner` a `READONLY_SIGNER`, the claim-time syncs' owners `READONLY`, error 6107's copy); **58 passed** (6.4 min) |
| **services tests** | fmt, clippy clean; `cargo test --manifest-path services/Cargo.toml --workspace`: **351 passed, 0 failed, 6 ignored** (keeper, LiteSVM scenarios with the M5 rebase sync now signed by the lender, T-OPS, indexer with embedded Postgres); the two committed sessions re-recorded on this program (same 836 / 138 transactions; the indexer's 117 tests pass on them); the `ingest_rpc_validator` test (`--ignored`, a local validator) not run |
| fuzz crate tests | 25 passed (the KI-65 regression: run B's three reproducers and the curve corpus file at 126 / 198 / 495 / 50 lamports) |
| **fuzz, 10 min each** (`sync_lender` now by the owner or a third party; the `KI65-REV-SEC1` oracle) | `--profile all --seed 65107000 --jobs 4`: 582 runs, 1,092,707 transactions; mock `--seed 31013600`, cpmm `--seed 31005900`, pumpswap `--seed 31016900` (`--jobs 3`): 747 / 776 / 345 runs; **0 failures, 0 anomalies**; 25 / 27 / 31 / 19 epoch rolls, `sync_lender` landed 560 / 658 / 672 / 392 and refused `RebaseNeedsOwner` 2 / 1 / 1 / 2 times (each as the oracle predicted); at most 1,615 lamports of `lender_sol` and 0 owed after the second claim pass |
| the owner-check mutant (`sync_lender`'s `require!` made `true`) | `ki65_it` fails 5 of 16 (the refusal test, W2 and three `order_*`), `lender_it` `flows::` 1 of 15; the 10-minute mock fuzz (`--seed 31013600`, the test-venue mutant) fails `FUZZ-ORACLE KI65-REV-SEC1` once (run 31013907) |
| verifiable build (`--local --compare`) | byte-identical for both programs at `9612471` (the hashes of §0; solana-verify 0.5.2, the pinned image) |
| **mutants** (`scripts/mutants.sh`) | **7 / 7 killed**, by the tests and by a 3-minute fuzz each (1,868 s) |
| `python3 scripts/audit_scope.py check` | ok |

**The KI-65 follow-up's review round** (2026-10-07, same branch; no program, math-crate or client
change, so the release ELF is unchanged, `cdbd2e70…`): the security review confirmed the rule
(no path by which a third party converts another owner's position through a rebase record) and
found that the residual needs no atomicity — a Sybil converted an hour after the honest claim
takes the same, so KI-65 now says "converts after", and an honest sub-unit lender loses its
latent unit at its first own conversion during a vest; the spec review found the rule's price
for the pot (KI-66, KI65-REV-POT: a surplus holder behind a rebase record that never returns
can no longer be touched by a third party; disclosed, owner item open in §7). `ki65_it` now pins
every residual figure with `assert_eq!` and adds the delayed conversion and the pot case
(18 passed); the fuzz's `KI65-REV-SEC1` oracle predicts the conversion and the touch after it;
the claim-time E2E checks the other lender's `owner` rides unsigned; `scripts/mutants.sh` gains
the skip-`store_records` and owner-check mutants (killed by `ki65_it` alone). Re-run (logs
`~/.cache/shortwin-work/m10/ki65/gatec/logs`): fmt and clippy `--workspace --all-targets -D
warnings` clean; `ki65_it` 18 passed, `lender_it` 49; the fuzz crate's 25; two 10-minute fuzz runs
(`--profile all --seed 65107000 --jobs 4`: 608 runs, 26 epoch rolls, `sync_lender` 583 landed and
2 refused; mock `--seed 31013600 --jobs 3`: 783 runs, 27 rolls, 693 and 1), **0 failures, 0
anomalies**, at most 1,138 lamports of `lender_sol` and 0 owed after the second claim pass; web
lint (2 existing warnings) and typecheck clean, the `lend-` E2E specs 14 passed (4.0 min);
**`scripts/mutants.sh` 9 / 9 killed** (the seven by their tests and a 3-minute fuzz: 10 / 6 / 10 /
125 failures on `mock`, 37 for the clear delay, 16 on `curve`, 2 on `pumpswap`; the KI-65 two by
`ki65_it`, 12 and 6 of 18 tests; 2,121 s). Not re-run (no program, client or service change): the
workspace and services tests, the full E2E, the benches and the verifiable build.

**The KI-65 batch's gate** (2026-10-07, on `m10/ki65`, the program from `3c7c689`; the
machine's 3-slot wrapper; logs `~/.cache/shortwin-work/m10/ki65/logs`). Every step of CI's jobs:

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `386c5363…2895` (1,582,024 B), `complete_curve.so` `f3a47201…6d42` (unchanged), test-venue `shortwin.so` `d5b313d3…6ca5`, `mock_cpamm.so` `4cf17007…7ab9` |
| A49; security.txt | `A49-OK`; `SECTXT-OK` (`--forbid-placeholders --source-release v1.0.0`) |
| fmt; clippy `-D warnings` on the workspace, `-p shortwin`, `-p shortwin-math`, services, the benches | clean |
| lockfiles | no change (no dependency added); `cargo deny` / `cargo audit` not re-run (tools absent locally; batch 4's results stand for the same lockfiles) |
| vectors; T-CONFIG | every `gen_*.py` regenerated: only `vectors/pool/` changes (pool_model v0.4.2; new `pool_ext_orphans_1.json`), reproducible; `config_check.py` OK, `test_config_gate.py` 8 tests |
| wasm | `shortwin-math` and the facade on `wasm32-unknown-unknown`; `cargo check -p shortwin` there |
| `cargo test -p shortwin-math -p shortwin-math-wasm -p squads-v4-ix`, then `PROPTEST_CASES=2000 cargo test -p shortwin-math` | 350 passed, then 316 at 2,000 cases; 0 failed |
| **`cargo test --workspace --locked`** | **1,571 passed, 0 failed, 4 ignored** (incl. `ki65_it` 11/11; 14/14 after the review round below), 1,560 s (the doc tests rerun alone: a concurrent build had replaced an rlib) |
| T-POT (`SHORTWIN_TRACES=full`), T-SIZE, cu-math | 5 (the R3A-2 row exact: 40 seeds, 19,887 lamports, worst 582; the new orphans group through the program); 11; 2 passed |
| benches m1, m2, m3, m5, m6 | every figure within its budget; m1's KI-65 rows as re-measured in the review round below (records that credit the lender: `claim_sol` / `cover_deficit` / `sync_lender` within 60,000 CU up to 8 rebase records, 54,194 / 54,549 / 51,880; 64,484 / 64,839 / 62,170 at 10, beyond the web's cap of 8); m6 +77 CU on the post-wipe reward claim; m2 / m3 / m5 noise only |
| m7-keeper-bench; `keeper_cost.py` | 3 passed (re-written); the constants still equal `results.json` |
| codama; client check; fixtures | the IDL gains error 6106, the regenerated client has no diff, 28 client tests; `dump-fixtures.py check` 265 entries, `SHA256SUMS` OK |
| web: build, lint, typecheck, Vitest; **web E2E** (all specs, under the E2E lock) | clean; 570 passed, 1 skipped; **58 passed** (6.9 min) |
| **services tests** | keeper, LiteSVM scenarios, T-OPS, indexer with Postgres: 0 failed (the indexer's two committed sessions re-recorded: the 127-byte `ShareEpoch` records no longer decode) |
| fuzz crate tests | 25 passed (incl. the KI-65 regression: run B's three reproducers and the curve corpus file at 125 / 198 / 495 / 50 lamports) |
| **fuzz, 10 min each** | `--profile all --jobs 4`: 527 runs, 1,053,129 transactions; mock `--seed 31013600`, cpmm `--seed 31005900`, pumpswap `--seed 31016900` (the reproducers' neighbourhoods, `--jobs 3`): 683 / 779 / 349 runs; **0 failures, 0 anomalies**; at most 1,615 lamports of `lender_sol` and 0 owed after the second claim pass; 25 / 27 / 31 / 20 epoch rolls |
| the `store_records` mutant (every handler's write removed) | `ki65_it` fails 10 of 11 (I1: "Σ lender shares 7199 != S 7200"; the lab's record comparison; run before the review round's three `order_*` tests) |
| verifiable build (`--local --compare`) | byte-identical for both programs at `b48dbd7` (the hashes of §0; solana-verify 0.5.2, the pinned image) |
| **mutants** (`scripts/mutants.sh`) | **7 / 7 killed**, by the tests and by a 3-minute fuzz each (1,914 s) |

**The KI-65 review round** (2026-10-07, same branch; the release ELF is unchanged, `386c5363…`,
rebuilt and compared after the only crate edit, a doc comment): the security review found the
conversion-order residual larger than first stated (a party collects one rounding unit per
position it holds, and with every other holder below one post-rebase share it is paid the whole
vest — a JIT depositor too, with one Sybil placed before the roll; KI-65 and §1's decision row
now say so, and `ki65_it`'s three `order_*` tests pin it: 14 passed). The m1 KI-65 rows were
re-measured on records that credit the lender about 1 SOL each (they had zero accrual deltas,
so `claim_sol` paid nothing): `claim_sol` / `cover_deficit` / `sync_lender` cost 54,194 /
54,549 / 51,880 CU at 8 records, 59,339 / 59,694 / 57,025 at 9 and 64,484 / 64,839 / 62,170 at
10 (over §6.1's 60,000), so the web's `MAX_RECORDS` is now 8; `cover_deficit` gained rows.
`EpochRecordReadOnly` is now also tested on `deposit` and `withdraw`. Re-run: `cargo fmt`,
clippy (`--workspace`, `-p shortwin-math`, the m1 bench), `cargo test -p shortwin-math` (316),
`pool_vectors` (12; `gen_pool.py` reproduces every file), `ki65_it` 14/14, the m1 bench, and
the web build, lint, typecheck, Vitest (570 passed, 1 skipped) and the `@lend` E2E specs (23
passed). Not re-run: the workspace tests, fuzz, mutants and verifiable build (no program,
harness or fuzz change).

**This package's gate** (re-run for program batch 4, 2026-10-03/04, on `m10/batch4` at
`09778df` … `f08426d`: the pump venues' 2026-10-02 redeploys, PUMP-2026-10-1 and -2, then docs
and the bench re-baselines; the program is the same from `e4f516b` on). Every step of CI's jobs,
run locally through the machine's 3-slot wrapper (logs:
`~/.cache/shortwin-work/m10/b4/logs/SUMMARY`):

| Step (CI job) | Result |
|---|---|
| anchor build + `scripts/build-test-venue.sh` | `shortwin.so` `bbef1075…f289`, `complete_curve.so` `f3a47201…6d42`, test-venue `shortwin.so` `b32f85a0…3e5b`, `mock_cpamm.so` `4cf17007…7ab9` |
| A49 (anchor-build) | `e_flags` 0x3 on every ELF; the mock id in neither release ELF and in the test-venue one; the marker only in the test-venue `shortwin.so`; release ≠ test-venue: `A49-OK` |
| security.txt (anchor-build) | the checker's unit tests; both release ELFs valid with `--forbid-placeholders --source-release v1.0.0`; one marker each, none in the test-venue ELF: `SECTXT-OK` |
| fmt; clippy `-D warnings` on the workspace, `-p shortwin`, `-p shortwin-math` | clean |
| lockfiles; `cargo deny` (root, services); `cargo audit` (8 lockfiles, CI's ignore list) | no lockfile change; `advisories ok, bans ok, licenses ok, sources ok`; no vulnerability |
| vectors; T-CONFIG | regenerated, no diff; `config_check.py` OK and `test_config_gate.py` 8 tests |
| wasm | `shortwin-math` and the facade on `wasm32-unknown-unknown`, `cargo check -p shortwin` there; the facade smoke test |
| fixtures; `check --live` | `dump-fixtures.py check`: 265 entries; `SHA256SUMS` OK; **`check --live` at slot 453,037,762: every pinned program and account matches mainnet** (the pump venues on `pump_programs_2026_10`, `VENUE_GUARD_SLOTS_2026_10_03`) |
| `cargo test -p shortwin-math -p shortwin-math-wasm`, then `PROPTEST_CASES=2000 cargo test -p shortwin-math` | 640 passed (310 + 20, then 310 at 2,000 cases); 0 failed |
| `cargo test -p squads-v4-ix` (test) | 13 passed |
| **`cargo test --workspace --locked`** (test, program-tests) | **1,551 passed, 0 failed, 4 ignored** (112 result blocks incl. the doc tests; the whole LiteSVM suite with the hooks, the pump suites on the redeploys), 1,385 s |
| verifiable build (`--local --compare`) | byte-identical for both programs (§0), 284 s |
| codama; client check | both IDLs equal the build's (no IDL change in batch 4); the regenerated client has no diff |
| web: build, lint, typecheck, Vitest | clean; **570 passed, 1 skipped** |
| **web E2E** (all specs, under the E2E lock) | **58 passed** (6.5 min) |
| services: fmt, clippy (workspace, keeper), build | clean |
| **services tests** (services, indexer) | **351 passed, 0 failed, 6 ignored** (33 result blocks; keeper, LiteSVM scenarios, T-OPS, indexer with Postgres) |
| keeper A49 | the release `shortwin-keeper` is free of the mock id; the scenarios binary embeds it |
| m7-keeper-bench; `keeper_cost.py` | 3 passed; the cost model's constants follow the re-measured PumpSwap and curve exits, and its output equals `docs/design/sim/out/keeper_cost.txt` |
| fuzz crate tests (fuzz-smoke) | 24 passed (release: smoke tests and corpus replays) |
| **fuzz, 10 min each**, `--jobs 3`: `--profile all --seed 20261081`; `--profile curve --seed 20261082`; `--profile pumpswap --seed 20261083` | all: 441 runs, 64,676 actions, 866,755 transactions, 1,702,677 hook checkpoints; curve: 597 runs, 77,021 actions, 1,031,838 transactions, 1,917,894 checkpoints; pumpswap: 335 runs, 49,187 actions, 757,773 transactions, 1,428,899 checkpoints; **0 failures, 0 anomalies** in each; the new `fee_buckets` action (v2 PumpSwap trades and the sweeps) landed 241 / 258 / 933 bucket-carrying v2 trades |
| **mutants** (`scripts/mutants.sh`) | **7 / 7 killed**, by the tests and by a 3-minute fuzz each |
| cu-math; t-pot; t-size | 2; 5; 11 passed |
| benches m1, m2, m3, m5, m6 (fmt, clippy, run, re-baselined) | 1, 4, 5, 7, 3 passed: every figure within its budget; m6 (PumpSwap) +120 … +379 CU from the bucket reads, m5's graduated listing +120; m3 unchanged; m1 / m2 only per-run PDA-bump noise (not committed) |
| `python3 scripts/audit_scope.py check` | ok |
| relative links of `docs/`, `SECURITY.md`, `README.md`, the test READMEs | 23 files, 72 relative links, 0 broken (a script checks every relative markdown link and anchor) |

**Which ELF each suite runs on.** Know this before reading test results:
- The **M2 engine suites run on the TEST-ONLY test-venue ELF**
  (`target/test-venue/deploy/shortwin.so`) against the mock venue. That covers every suite
  built on `Env::test_venue` / `mock_market`, for example:
  - `engine_it`, `liquidate_it`, `expire_it`, `add_margin_it`;
  - the settle suites, `caps_boundary_it`, `security_m2`, `attacks_exit_it`;
  - parts of `m7_sec_it` and `switch_confirm_it`;
  - the `mock` and `mixed` fuzz profiles.

  `tests/litesvm/README.md` lists each binary and its build.
- The **fork suites run on the release ELF** (`target/deploy/shortwin.so`) against the dumped
  mainnet venue ELFs: `ray_*`, `pump_*`, `pamm_it`, `graduation_*`, the `curve`, `pumpswap`,
  `amm-v4` and `cpmm` fuzz profiles, and the m3, m5 and m6 benches.
- A finding in mock-only code paths is out of scope (§2). An engine finding reproduced on the
  mock should be re-checked on a fork.

**Measured figures:** `tests/bench/*/results.md`, `trace.md` and `depth.md` (CU, instruction
trace, stack depth); `tests/litesvm/README.md` (the suites and their figures);
`tests/bench/m7_keeper` and `m7_cascade` (keeper cost and T-CASCADE).

**Fuzz:**
- **24 h run 1**, at `eb82a2a`: seed 20261001, 4 jobs, 86,405 s; 84,053 runs, 16,098,571
  actions, 207,549,074 transactions, 318,356,256 hook checkpoints. 4 failures in 3 harness
  signatures (FUZZ-M7-24H-1, -2; fixed in `e857a2a`) and 1 anomaly (FUZZ-M7-24H-3; fixed in
  `22961b0`, the economics are KI-53). Program invariant violations: 0.
- **24 h run 2** (the confirmation), at `4cb7729` (its `shortwin.so` equals `e857a2a`'s and
  `eb82a2a`'s, `00011844…`): seed 20261002, 4 jobs, 86,401.8 s (2026-10-02T10:12:42Z →
  2026-10-03T10:13Z); 72,149 runs, 13,821,419 actions, 178,365,479 transactions, 273,589,118
  hook checkpoints, 72,149 wind-downs. **0 failures, 0 anomalies**: M7's exit criterion
  "fuzz clean ≥ 24 h" is met.
- **24 h run 3** (the final tree), at `4f4849e` (release `shortwin.so` `bbef1075…`; the M10
  program batches changed both programs after run 2's commit): seed 20261004, 4 jobs,
  86,408.5 s (2026-10-03T23:19:34Z → 2026-10-04T23:20Z); 52,272 runs, 10,162,831 actions,
  132,425,511 transactions, 255,722,249 hook checkpoints, 0 anomalies. It failed two of the
  fuzzer's liveness oracles, both harness bugs with the program unchanged:
  - FUZZ-M10-24H-A (6 runs; crash files for 5, as the fuzzer keeps 5 per signature): the
    second claim pass's oracle was stricter than ECON §9.4 / §9.5's I-P6 residue (the
    accumulators' rounding carries and rebase-orphan credits). Fixed in `997c730` with exact
    state-derived bounds, which still catch the program mutants mutL, mutS and mutC. Its
    spec-level residual is KI-65, fixed in the program since (the KI-65 batch: no rebase
    orphans; the oracle's orphan-credit acceptance is gone).
  - FUZZ-M10-24H-B (1 run): the wind-down sized its per-second coverage from the config's
    `settle_persist_n` instead of the position's locked one (latent since M7). Fixed in
    `945d45a`.
- **24 h run 4** (the final tree's confirmation), at `945d45a` (the fixed fuzzer; the program
  bytes of `4f4849e`, `bbef1075…`): seed 20261005, 4 jobs, 86,404.9 s
  (2026-10-05T01:38:12Z → 2026-10-06T01:38Z); 52,274 runs, 10,163,360 actions, 132,445,714
  transactions, 255,725,412 hook checkpoints, 52,274 wind-downs. **0 failures, 0 anomalies**:
  M10's "fuzz clean ≥ 24 h on the final tree" is met. The second claim pass left at most 445
  lamports owed and at most 217,508,010 lamports (≈ 0.218 SOL) of `lender_sol` (5 wind-downs
  above the 0.01-SOL dust cap, each within the rebase-orphan shares' realised SOL); it paid
  13,618 claims. That largest orphan credit is run 3's sixth FUZZ-M10-24H-A failure (run 3's
  summary records the same figure, and there any `lender_sol` above the cap failed), which
  got no crash file, so its shape is not yet classified (KI-65); the triaged reproducers'
  0.054 and 0.012–0.100 SOL are examples, not the bound. KI-65 is fixed in the program since
  (no rebase orphans): run B's three largest orphan-credit wind-downs (19,131,900,419 /
  890,963,742 / 552,350,825 lamports on the mock / CPMM / PumpSwap) and the curve corpus file
  now end with 126 / 198 / 495 / 50 lamports on the KI-65 follow-up (125 / 198 / 495 / 50 on
  the KI-65 batch; `tests/fuzz/tests/smoke.rs`). Runs 3 and 4 predate
  the fix: the final tree's ≥ 24 h fuzz must be repeated on it (done: runs C, D and E below).
  Run 4 is not independent coverage: seeds are `base + job + k·jobs` per job and a run's
  profile and length derive from its seed, so on 4 jobs run 4's stream (base 20261005) is
  run 3's (base 20261004) shifted by one. It re-executed essentially all of run 3's 52,272
  cases, the six failing seeds included, with the fixed fuzzer (the two summaries' largest
  `lender_sol`, deficit, bound and deficit/bound figures are identical). It confirms the fixes
  on run 3's cases and adds almost no new coverage. Runs 1 and 2 (bases 20261001 and
  20261002) overlap the same way. A confirmation run meant to add coverage takes a widely
  spaced base seed.
- **The final program bytes** (release `shortwin.so` `4d19c582…`; KI-65 fixed, so no rebase orphans):
  three more 24 h runs, `run --profile all --duration 24h --jobs 4 --minimize`, named by letter after
  run 4 as run D's finding ids are (`tests/fuzz/README.md` "Runs so far" has their rows; base seeds
  far apart, so each adds coverage):
  - **24 h run C**, at `f48e75d` (`m10/ki67`: the final program bytes; the fuzzer before the KI-67
    review round's harness changes): seed 67300000, `--actions` 300 (the default), 86,418 s
    (2026-10-08T11:50:02Z → 2026-10-09T11:50:20Z); 43,833 runs, 6,405,217 actions, 84,950,438
    transactions, 165,759,524 hook checkpoints, 43,833 wind-downs. **0 failures, 0 anomalies.**
  - **24 h run D**, at `55b3e23` (the final head then): seed 71000000, `--actions 400`, 86,445 s
    (2026-10-08T17:29:26Z → 2026-10-09T17:30Z); 31,389 runs, 6,083,510 actions, 78,948,599
    transactions, 152,298,767 hook checkpoints. The program's invariant hooks held; two fuzz-oracle
    findings, both harness bugs, fixed in `9861c5a` (known-issues §6, `tests/fuzz/README.md`
    "Findings"): **FUZZ-24D-A** (1 failure: the PumpSwap exit oracle reused a stale expiry
    prediction after a direct VM write lifted a venue halt) and **FUZZ-M10-24H-D-B** (1 anomaly:
    the never-returns oracle ignored the absent lenders' KI-53 deficits; present lenders owed
    73,744,534,723 lamports backed only by an absent lender's `cover_deficit` — the economics are
    KI-53's, accepted by the owner on 2026-10-03).
  - **24 h run E**, at `9861c5a` (the final program bytes and the fixed fuzzer): seed 75000000,
    `--actions 400`, 86,415 s (2026-10-09T19:45:29Z → 2026-10-10T19:45:44Z); 44,988 runs,
    8,773,451 actions, 114,792,804 transactions (14,965,581 failed), 221,932,740 hook
    checkpoints, 44,988 wind-downs; 28 of 30 instructions landed, 69 of 108 error codes returned.
    **0 failures, 0 anomalies: M10's "fuzz clean ≥ 24 h" is met on the final program bytes.**
    The deficit tripwire checked 634 deficits in 621 runs (the largest share of its bound
    1,000,000 ppm, as run C); the second claim pass left at most 112 lamports owed and
    `lender_sol` at most 2,335 lamports; the never-returns wind-down reached 310 markets (621
    absent lenders), no present lender owed above dust, no absent lender left owing a deficit.
- This gate's 10-minute smoke on this tree is in the table above.

**M10 evidence** (ARCH §9 M10 row and §8.2 are normative):
- **CPMM on the live binary** (WP-2): fixture group `raydium_cpmm_2026_10` (slot 452,376,460),
  `raydium-cp-swap@bdeee23` rebuilt byte for byte; every CPMM suite, the m3 / m5 / m6 and M7
  benches and the web E2E validator on it; no adapter change.
- **T-MEMO** (WP-3, U17): `security::sec_7_*` and `t_memo_control_top_level_sibling_check` on
  the mainnet Token-2022 and spl-memo v3 ELFs; the whole LiteSVM suite runs on them.
- **Adversarial suite** (WP-5, WP-8, the CPMM-deferred cases): the M10 matrix's 62 sub-cases
  per venue (ARCH §8.2 table); new binaries and modules `pamm_it::{structure, wsol, admin,
  hooks}`, `ray_it::route`, `pump_it::fees`, `engine_it::zero_lamport`, `route_it::hard_cap`,
  `raydium_cpmm_attacks`, `pumpswap_attacks`, `raydium_cpmm_exits_it`, `lender_real_it`; ECON
  §12 figures per venue in `tests/litesvm/README.md`.
- **Extras denylist** (batch 3): `engine_it::o6::the_denylist_holds_on_every_leg_instruction`,
  `ray_it::structure::the_pool_and_the_borrower_are_denied_extras_on_both_programs`,
  `pump_it::structure::the_curve_and_the_borrower_are_denied_extras`,
  `pamm_it::admin::the_pool_and_the_borrower_are_denied_extras_on_pumpswap`, the three
  `template_aliases_*` tests, and the keeper scenario
  `extras::liquidations_bypass_a_current_set_the_program_denies`.
- **M10-8C-1** (batch 2): `lender_real_it::finding_m10_8c_1_*` (un-ignored, passing).
- **Squads governance** (WP-7): `crates/squads-v4-ix` (pinned to IDL 2.1.0), `squads_gov_it`
  T-SQ-1..9 on the mainnet Squads ELF, `shortwin-gov` with its T-OPS end-to-end tests
  (`services/ops/tests/gov_e2e.rs`).
- **security.txt** (batches 1–2): the values in §0; `check-security-txt.py
  --forbid-placeholders` on both ELFs in the gate; none in the test-venue ELF; the web's
  `/.well-known/security.txt` and `/security` page; `SECURITY.md`.
- **Gap calibration** (WP-9): the method (`docs/research/gap-calibration.md`) and the
  public-RPC crawler (`scripts/gapcal`), crawling since 2026-10-02; results after ≥ 30 days
  (KI-14, §7 O9).

---

## 11. Focus areas

These are where we would look first. They are not limits on the scope.

1. **Remaining accounts.** Every instruction that prices or trades takes its venue accounts as
   a `ReadSet` / `TradeExtras` tail (ARCH §3.0). Check:
   - key checks against market state (I14);
   - the extras templates and the denylist (A41; `set_venue_extras`, `venues/mod.rs`), including
     M10 batch 3's `venues::pool_denylist` (the adopted pool, its reserve vaults and the
     borrower, checked after a template entry is re-derived);
   - account substitution and omission on every venue.
2. **The trader PDA and the vault delegate.** Check the normalisation before each leg
   (donations of lamports, tokens or accounts), and the `ApproveChecked` of exactly `q` to the
   trader with the delegate auto-cleared at 0 (ARCH §2.4, I7).
3. **Venue CPIs:**
   - the sell and buy legs on pump (`buy_v2` / `sell_v2` with the trailing cap byte, U29; since
     1.0.1 `sell_v3` / `buy_v3` with 10 TradeExtras, the synthetic exit past `rT` and the
     `PumpMigrating` pricing on the pool `migrate_v2` creates, §13),
     PumpSwap (`buy` / `sell` with fee ATAs and UVA existence), AMM v4 (`swap_base_in_v2` /
     `swap_base_out_v2`) and CPMM;
   - measuring every leg against the post-normalisation baseline (I13);
   - (1.0.0 only; retired after the tag, §0) `complete_curve`'s single CPI.
4. **The observation layer.** Bucket integration, the EMA, the ring and the settlement box
   (`engine/observe.rs`, `shortwin-math` `observe`, `persist`, `settle`). Rounding direction
   at every step (ECON §2).
5. **Exit math and waterfalls.** The fixed-δ bound, the keeper floor `K_s`, S1 / S2 / S3 and
   fair value (`engine/bounds.rs`, `engine/waterfall.rs`, `shortwin-math` `bounds`,
   `waterfall`, `risk`).
6. **Lender accounting.** Entitlement and realised SOL, the pot, vesting (since M10 also the
   rent top-up, `pool::rent_top_up`), the lock calendar, share epochs after a wipe, the
   shortfall debit on current shares (KI-53), and repeated or bundled claims (I8; ECON §9).
7. **Pool selection and graduation.** `propose_pool` and `execute_pool_switch` (eligibility,
   the window, the fast path, the carried-fit rule); the curve → PumpSwap transition and its
   warm-up (I15, I18).
8. **Admin and config:**
   - the timelock and the apply window;
   - `Authorities` changes, the fee-recipient rule and SQ-G8's distinct-authorities check
     (`config::check_authorities`, at `initialize_config`, `propose_config` and `apply_config`);
   - Emergency limits (what it cannot cancel), and `disable_venue`'s cancel of a pending
     `EnableVenues` change (AIR-24);
   - `revalidate_venue`'s `expected_slot` and `expected_elf_hash`, and `flag_venue_upgraded`'s
     test of an extension (AIR-01: the ProgramData's length and the trimmed-ELF fingerprint,
     `shortwin-venue-state`'s `program_data::trimmed_elf`; KI-69);
   - the coverage pause and its 5-s clear delay;
   - compiled parameter bounds (`params::validate`, `validate_econ`).
9. **The mint filter** (`mint_filter.rs`, venue-state `mint.rs`). Check Token-2022 extensions,
   the freeze authority and permanent delegate, and that the filter can never block an exit or
   a withdrawal.
10. **Numerics.** The U256 fallbacks, the `as` casts (58 non-test sites in the audit-prep
    scan) and `wrapping_` / `overflowing_` uses, and the zero-copy layouts (`layout_asserts.rs`,
    T-LAYOUT).
11. **Keeper subset** (if in scope). The account lists it builds (`venue.rs`, `ixs.rs`): a
    wrong list is an exit that does not land. Also the exit decision against the program's
    own predicates (`decide.rs`, `view.rs`) and the budget's exit-critical path
    (`budget.rs`).

---

## 12. Logistics and change policy

**Engagement** (owner decision Q2 of 2026-10-02): one full private audit, then a public
contest (Sherlock or Cantina) on the fixed code, then a bug bounty at the beta. The owner
books the engagements. The firm, the dates, the 2 h kickoff walkthrough and the communication
channel are recorded in [`docs/audit/findings.md`](audit/findings.md) when booked.

**Freeze:**
- The audit commit is the tag named in §0.
- Between the tag and the report, the in-scope files change only through fix batches.
- `python3 scripts/audit_scope.py check` and the §0 hashes identify any drift.
- `check` passes on this tree: WP-7's `services/ops` and `crates/squads-v4-ix` were
  classified (the `governance` context unit) and `write` was run. Any later new crate,
  program or service is classified the same way before a fix batch's tag.
- The tag is `audit-1` together with `v1.0.0` (owner decision of 2026-10-02); fix batches
  after it are 1.0.x, each with its own §0 hashes.

**Findings.** Each finding is logged in [`docs/audit/findings.md`](audit/findings.md) and fixed
in a program batch, one commit per finding where practical. Each fix comes with:
- a regression test that fails before the fix: a LiteSVM test or a fuzz corpus file;
- the full gate (G-full) green on the fix commit;
- a ≥ 24 h fuzz run on the fix commit;
- the auditors' fix review.

*(2026-10-13: the owner waived the ≥ 24 h fuzz run for the AI review's fix batch, AIR-01 …
AIR-24; no such run was made on its program. The rule stands for the audit's findings. That
batch still needs CI and an external fix review.)*

A finding may instead be acknowledged with a written rationale; it then becomes a row in
`known-issues.md`.

**Venue-upgrade addendum.** The venues upgrade on their own schedule (KI-44); CPMM did so on
2026-09-30 / 10-01. If a venue program changes between the tag and the end of the engagement:
1. Re-dump it as a new fixture group, and confirm the drift with
   `python3 scripts/dump-fixtures.py check --live`. The pump venues' 2026-10-02 redeploys went
   through these steps before the freeze (ARCH changelog 73: `pump_programs_2026_10`, the
   T-LATCH replay, PUMP-2026-10-1 fixed in batch 4).
2. Diff its interface and layouts against the previous dump.
3. Re-run the fork suites, benches and fuzz profiles of that venue on the new ELF.
4. If short.win's adapter has to change, the change is an addendum, with its own scope
   (the adapter diff plus the venue's interface diff), its own hashes, and the same fix
   policy.

pump's 2026-10-08 / 10-12 / 10-13 redeploys went through all four steps after the tag; the
adapter change is short.win 1.0.1, and §13 is its addendum.

**Doc changes.** A change to ARCH, ECON or SPEC that alters a bound in `known-issues.md`
updates the register in the same commit.

**Publication.** Open (§7): we recommend publishing the private audit's report after the
fixes and before the beta. The public contest follows the fixes; the bug bounty starts at the
beta (M11). Published reports are listed on `https://short.win/security`, and short.win's
`security.txt` then gains its `auditors` field (a program upgrade).

---

## 13. Addendum: pump v3 (short.win 1.0.1)

The §12 venue-upgrade addendum for pump's redeploys after the tag (the design `pump-final`,
revision 2 of 2026-10-13; the owner's decisions of 2026-10-13, §7's "pump v3" row). Built on
the branch `pump-v3` (2026-10-13 … 16, phases P0 … P8, each commit with its tests in its
message). **Not yet through CI, a verifiable build or an external review.**

**For the auditors (owner decision D2):** this addendum changes the audit scope. The
`complete_curve` helper of the tagged `audit-1` scope (§0, §1, §2) is retired: removed from the
workspace, the release, Governance's upgrade set and `scope.txt` (4 lines out, 2 reworded); it
was never deployed. The curve adapter is rewritten (below). The owner is to tell the auditors of
both together with this addendum.

### 13.1 Steps 1–3: the re-pin

| Program | Build | Deploy slot | Trimmed-ELF sha256 (the on-chain comparison) | Fixture group |
|---|---|---|---|---|
| pump | 2026-10-13 15:40 UTC (in force) | 456,494,698 | `e202096978d8f20935153a4271938fd9f0a421d64c1749986b4c5c20ca2a52be` | `pump_programs_2026_10_13b` |
| pump | 2026-10-08 (superseded the same week) | 454,596,459 | `6956c2f83297b6f7f06bbe8033241589c39da9650597dc5c59fb5a431906bf6f` | `pump_programs_2026_10_13` |
| PumpSwap | 2026-10-08 | 454,596,406 | `3ffd5e3b5a31ccc86b434405e8a1876926da1e8685f891dc964b7f919c172ee1` | `pump_programs_2026_10_13` |
| pump-fees | 2026-10-12 (a 10-08 build at 454,596,501 preceded it, never examined: it can no longer be fetched) | 456,132,171 | `8ad2ca93c7ae97a7fb0de8a99de4d8825ccfdf89f73138cd49bda948e124b8b8` | `pump_programs_2026_10_13` |
| Jupiter | — | 454,465,850 | no fixture (held to the constant) | — |

- **Baseline** `VENUE_GUARD_SLOTS_2026_10_13` (`constants.rs`; a test and `check --live` baseline,
  the program never reads it). **`check --live` at slot 457,523,244** (2026-10-16 05:38 UTC,
  read-only RPC): every pinned program and account matches mainnet. The state of the new builds
  is `pump_state_2026_10_13` (slot 456,395,752: the 1,088-byte `Global`, ten coin roles).
- **Interface diff** (`tests/fixtures/README.md`): the 10-08 build adds pump's
  `multi_hop_curve_swap` and `set_max_curve_depth`, PumpSwap's `multi_hop_swap` and pump-fees'
  `withdraw`; nothing is removed; errors are appended; `BondingCurve` grows to 166 bytes
  (`creator_fee` @125, `protocol_fees` @133, `depth` @141, `ivqr` @142, `pcB` @150, `pcQ` @158),
  `Global` to 1,088 (`max_curve_depth` @1087); `buy_v3` past `rT` completes a curve by
  **synthetic migration** (the rest from the would-be pool). The 10-13 build changes only
  `create_v2`'s quote chain (pump's own new error 6109 `QuoteChainAccountsRequired`, unrelated to
  short.win's 6109); every path short.win uses is the same code (gate G0b: 180 transactions on
  both builds in lockstep, equal logs, accounts and CU).
- **Gates G0 / G0b on the dumped ELFs, the adapter unchanged:** T-PUMP-MATH (v2 cost, proceeds
  and error codes as before) and T-PUMP-MATH-V3 (a)–(g), (b′) (26 tests: the v3 charge model, the
  synthetic charge and events, the donation-inclusive pool part, `migrate_v2`'s pool and boost
  `⌊X₁·Y₁/10¹⁵⌋`, the tier tripwire, 6094 on a cashback coin, 6021 on the 10-02 build); T-REPIN-1
  … -3 (the full LiteSVM suite on the new builds with the AI review's v2 adapter: 1,117 passed);
  T-PFEE-CPI. Corrections they made to the design: the boost is `⌊X₁·Y₁/10¹⁵⌋` (the design's
  `⌊(Y − pcQ)·2069/10⁴⌋` holds only without a donation); on an exact division pump charges the
  curve part `⌊⌋ + 1` and the pool part `⌈⌉`; the 49-byte legacy curve's growth costs 261,600
  lamports (not 594,360).
- **R-PFEE** (read from the 10-12 ELF): `withdraw` writes only its signer `user`, the
  `referral_vault` PDA derived from it, that PDA's ATA and an ATA `user` owns — never a
  `FeeConfig`, a pool or a curve; the stop rule does not trigger (PUMP-2026-10-13-5). Pinning or
  revalidating pump-fees' 10-12 build on mainnet stays the owner's step.

### 13.2 Step 4: the adapter change (1.0.1)

**Scope** (against the AI review's tree `c0e0900`; `git diff --stat c0e0900..dd24af1`):
`programs/shortwin/src/venues/{pump_curve,mod,detect}.rs`, `errors.rs`, `constants.rs`,
`instructions/close_short.rs` (the close's venue check, `close_venue_checks`),
`instructions/liquidate.rs` (**code**: the keeper exits' `exit_venue_checks` no longer takes
`q` and no longer refuses `q > rT` with `CurveStraddle` before the CPI; such an exit is an
ordinary synthetic one, and one the curve and its would-be pool cannot deliver has no execution
quote and fails precondition 4, `UseSolSettle`), the doc text of
`instructions/{create_market,propose_pool}.rs`, `Cargo.toml` (1.0.1);
`crates/shortwin-math/src/quote/{straddle,pricing,mod}.rs`, `fees.rs`;
`crates/shortwin-venue-state/src/pump.rs` — 14 source files, +1,939 / −356 lines (P8's doc fix
in `venues/mod.rs` rewords three comment lines and changes no byte); `programs/complete_curve` removed
(−471). Non-test nLOC (`audit_scope.py nloc`): `shortwin`
13,597, `shortwin-math` 6,807, `shortwin-venue-state` 1,705 (§2's audited figures: 13,293 /
6,593 / 1,636, before the AI review's batch too).

**What changed** (ARCH §2.4.1, §2.5, §3.12, §4.3, §4.5, §5.4 and changelog 81; ECON #119):
- **Reader:** the 166-byte curve's six fields (0 on a shorter curve) and pump's invariant
  `vQ − rQ = ivqr` (`quote_offset_consistent`).
- **Curve legs on v3:** opens `sell_v3`, every exit `buy_v3` — 10 TradeExtras (were 20), the
  IDL's 17 accounts, exactly 24 data bytes (never `partial_fill`), no pump-fees CPI, no creator
  vault, the trader PDA the only signer. A buyback past `rT` buys `rT` on the curve and the rest
  from the would-be pool in one instruction; `sync_after_leg` moves the market to `PumpMigrating`
  in it. `CurveStraddle` (6031, D4's message) is now only a close past `rT` that the curve and
  its would-be pool cannot deliver (unreachable under the caps) or on a curve with no synthetic
  route (`φ_s` not resolved on the mint's supply; `close_venue_checks`); keeper exits there get
  `UseSolSettle`.
- **Pricing past `rT` (D3):** `B̂_v3`, the curve leg and the would-be pool's leg at the fee pump
  charges, the curve tier at its market cap on the mint's **live** supply (`φ_s`, KI-77), `+1`
  on both legs; `B̂_str` only for a `PumpMigrating` market's states.
- **`PumpMigrating`:** priced on the pool `migrate_v2` creates — after a synthetic pool part the
  live `curve_base_ata` and `rQ − fee + pcQ`, without one the deterministic 206.9e12 (bit-identical)
  — with FV ρ-scaled; a migrating curve whose post-complete fields are not known has no FV (S3
  gives the lenders the whole take: the documented exception to A63's cap-only rule); a live
  curve carrying them is not executable (PUMP-2026-10-13-1).
- **Listing (D1 (a)):** a live cashback curve is refused `CurveCashbackCoin` (6109, D4's
  message) and never executable; a complete one is observed and priced; a live curve of `depth`
  ≠ 0 is `CurveQuoteNotSol`.
- Unchanged: account layouts (`Market` 3,104 B, `ShortPosition`), instruction signatures,
  events, the ReadSet (7 slots), the PumpSwap adapter, the latch masks.

**Measured on the 1.0.1 code** (gate G1, `tests/bench/m5_cu` on `b3130ae2…`, the same code as
1.0.1 but for the version; Mollusk and LiteSVM agree on all 65 cases; limits ARCH §6.1):

| Row | Measured | Limit |
|---|---|---|
| `open_short` curve | 139.1–154.6 k (the v2 adapter: 172.3–202.0 k on the AI review's build, 172.4–202.1 k at the 1.0.0 tag) | 250 k |
| `close_short`, `q ≤ rT` / completing / synthetic (`q > rT`) | 108.2–123.3 k / 125.5 k / 135.8 k | 250 k |
| `liquidate` / `expire` (synthetic and worst ring included) | 126.3–159.2 k | 300 k |
| the keeper's `[CB ×2, crank_price, liquidate]` past `rT` | 184,881 CU (worst ring 192,601) | 300 k |
| the keeper's `[CB ×2, crank_price, expire]` past `rT` | **188,021** CU (worst ring 189,585) | 300 k |
| `sol_settle` S3 on `PumpMigrating` after a pool part (ρ) | 69.3 k (64.8 k without one) | 150 k |
| the keeper's top-level `migrate_v2` | **330,859 CU** after a third party's exact completion (`pc = 0`) and after a synthetic one (`pc ≠ 0`, a third party's or short.win's own) alike; 53 trace entries, peak 4, 912 B v0 | `MIGRATE_CU` 400 k |
| the tipped `[crank_price, liquidate]` with 4 extras (v0) | **1,159–1,160 B** through A28 (1,128–1,129 through the build-alt table), now asserted (`tx_size_pump_it`); the tipped liquidation alone 1,134–1,135 B | 1,232 B |

Every curve transaction is 144 B smaller than 1.0.0's (a client close 864–865 B, was
1,008–1,009); every leg peaks at stack height 3; every exit is ≤ 15 trace entries (a synthetic
one 12). On 1.0.1 itself (`tests/bench/m7_keeper`, P8): the keeper's curve `liquidate` /
`expire` 147,131 / 144,314 requested CU at 1,122 B (191,837 / 189,015 at 1,452 B before, −23.3 %;
ECON §5.8 re-baselined).

**Tests** (each phase's commit messages give the runs): T-READER, T-MATH-1 … -5 (the math
crate's properties over the full domain, the 11 mainnet synthetic completions); T-V3-ADAPTER,
T-OPEN-V3, T-SYNTH-1 … -6, T-SIGMA, T-MIG-PRICE / -S3 / -HALT / -DONATION / -GUARD,
T-LIVE-PC-GUARD, T-LISTING, T-LISTING-CASHBACK, T-WIPE-GATE (AIR-05, AIR-08, M5-SEC-1 and
M6-SEC-5's tests re-expressed on a non-cashback curve, names and assertions kept), T-SIZEOF,
T-LATCH, T-REPLAY, T-CU / -TRACE / -SIZE / -DEPTH; the whole LiteSVM suite at P6 (79 result
groups: 76 test binaries and 3 doc-test runs; 1,289 passed, 5 ignored); the keeper's
`upgrade_window` scenarios (the keeper liquidates on 1.0.0 with the 20-key list and on the
upgraded build with the 10-key one, no failed send); the fuzzer's v3 crowd and `migrating` oracle
(P7: 10-minute smokes and ≈ 1 h each of the curve and PumpSwap profiles, 0 failures, 0
anomalies). **The final local gate on 1.0.1** (2026-10-16, every CI job that runs on the
machine, web-e2e and the indexer's PostgreSQL and validator tests included; not CI): §10 "The
pump v3 gate". Since the final review: T-PUMP-MATH-V3 (b″) (both synthetic parts under the
curve's own `creator_fee_bps`, `Global.creator_fee_configurable` on and off, and for a default
creator: pump charges `φ_s`'s creator rule on both, and `B̂_v3` covers the charge by 2–3), KI-77
(c)'s step pinned (`quotes_synthetic_g0`), and the indexer stores no pump `TradeEvent` of a curve
quoted in another token (INDEXER §7.2).

**T-SELF-UPGRADE's 1.0.0 fixture.** `tests/fixtures/shortwin/shortwin_1_0_0.so` is the `v1.0.0`
tag (`e4895c2` = `audit-1`) built on 2026-10-15 in a worktree of its own with the pinned
`anchor build`: sha256 `4d19c582…1f75`, trimmed `52a29a51…186b`, 1,609,616 B — equal to §0's
**verified** 1.0.0 hashes (and so were that build's `complete_curve.so` and `mock_cpamm.so`).
The harness refuses a file whose hash differs. Its bytes are the verified build's because they
are equal; no solana-verify run produced them (neither Docker nor solana-verify and bubblewrap
for `--local` on that machine).
`upgrade_it::v1_0_0_positions_close_and_liquidate_after_the_v3_upgrade` opens curve positions on
it with the 20-key list, upgrades through the loader and closes and liquidates them on the new
build (a 20-key close refused cleanly).

**Release coordination** (owner, 2026-10-13): the keeper first, with both TradeExtras formats
chosen per short.win deploy slot; then the Governance upgrade (48 h); then the web, which builds
the 10-key list only; the next release deletes the keeper's 20-key builder. PumpCurve
`VenueExtras` sets must be empty at the upgrade (none exist on mainnet).

**Not verified, or not run:**
- the 1.0.1 hashes (§0): a local build only, no verifiable build (neither Docker nor the
  `--local` mode's solana-verify and bubblewrap installed, and the session's rules allowed no
  network to fetch them), no CI run on `pump-v3`;
- no external review of this change; no ≥ 24 h fuzz run on 1.0.1 (P7's runs above);
- the Surfpool fork run of pump-final §14 (`tests/litesvm/tests/graduation_surfpool.rs`, ignored;
  it still names the 2026-10-02 builds): not run, no Surfpool on the machine;
- pump-fees' 2026-10-08 build: never examined (it was replaced before it could be dumped);
- what the new pump `.text` does beyond the paths short.win and its tests exercise (U30);
- a multi-tier curve `FeeConfig` (KI-77): only the tier tripwire on a synthetic `FeeConfig`;
- the keeper's format selection across a real Governance upgrade: LiteSVM only (`upgrade_window`).
