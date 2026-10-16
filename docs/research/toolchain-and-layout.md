# short.win: toolchain, testing and repository layout (research)

*Research date: 2026-09-27. Baseline: `docs/SPEC.md` v0.3 (not modified).*
*Method: primary sources only (GitHub repos at pinned commits/tags, crates.io / npm registry APIs, Solana mainnet JSON-RPC). Mainnet reads were taken at slots ~450,800,700–450,803,548. Items I could not verify are marked **UNVERIFIED**. No `cargo`, `anchor` or `solana` command was run (per instructions), so nothing here was compiled.*

---

## 0. TL;DR: what to pin

| Layer | Pin | Why / source |
|---|---|---|
| Anchor (CLI + crates) | **1.2.0** (`anchor-lang`, `anchor-spl`, `anchor-client` = 1.2.0) | Latest stable release, 2026-09-04 ([release](https://github.com/otter-sec/anchor/releases/tag/v1.2.0); crates.io `anchor-lang` max_stable = 1.2.0) |
| Anchor repo | **github.com/otter-sec/anchor** | `solana-foundation/anchor` and `coral-xyz/anchor` both 301-redirect there (checked with `curl -sI`, 2026-09-27) |
| Solana/Agave CLI | **4.1.2** | The v1.2.0 tag's verifiable-build image pins `SOLANA_CLI=v4.1.2` ([docker/Makefile@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/docker/Makefile)). The sentence "The recommended Solana version is `4.1.2`" is **not** in the tagged release notes (at v1.2.0, `1-2-0.mdx` only links to the CHANGELOG); it was added on master on 2026-09-14 ([1-2-0.mdx@78ed411a40](https://github.com/otter-sec/anchor/blob/78ed411a406c0c210bddc7fb9e78bb686c64c374/docs/content/docs/updates/release-notes/1-2-0.mdx), commit 78ed411a40 "fix(avm): map Anchor 1.2 to Solana 4.1.2 (#5034)"), in the same commit that makes avm map 1.2.0→4.1.2 ([avm/anchor-solana-map.toml@master](https://github.com/otter-sec/anchor/blob/2b4b2fd22a881228e02c41b26bd23addb5b8b26f/avm/anchor-solana-map.toml)) |
| SBF platform-tools | **v1.57** (Rust 1.95.0, LLVM 22) | `anchor build` default `DEFAULT_TOOLS_VERSION = "v1.57"` ([cli/src/lib.rs#L82@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/lib.rs#L82)); [platform-tools v1.57 release notes](https://github.com/anza-xyz/platform-tools/releases/tag/v1.57) |
| SBPF target arch | **v3** | `DEFAULT_BUILD_ARCH = "v3"` ([cli/src/lib.rs#L83@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/lib.rs#L83)); SBPFv3 feature `5cC3foj77CWun58pC51ebHFUWavHWKarWyR5UUik7dnC` is active on mainnet since slot 428,976,000 (2026-06-26), per the mainnet feature account |
| Host Rust | **1.98.1** in `rust-toolchain.toml`; on-chain crates declare `rust-version = "1.89"` | Anchor MSRV 1.89 ([lang/Cargo.toml@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/lang/Cargo.toml)); off-chain Agave 4.3 client crates need 1.97.1 (crates.io `solana-rpc-client` 4.3.0 msrv 1.97.1); 1.98.1 is the current stable (install log) |
| Rust tests | **LiteSVM 0.16.0** (+ `litesvm-token` 0.16.0); Mollusk 0.15.1 optional | [LiteSVM v0.16.0](https://github.com/LiteSVM/litesvm/releases/tag/v0.16.0) (bundles the mainnet feature set as of 2026-08-24); [Mollusk 0.15.1](https://github.com/anza-xyz/mollusk/releases/tag/0.15.1) |
| Fork / E2E | **Surfpool 1.6.0** | [surfpool v1.6.0](https://github.com/solana-foundation/surfpool/releases/tag/v1.6.0); Anchor 1.x uses it for `anchor test` by default; "lowest recommended Surfpool version for use with Anchor is v1.1.2" ([1-0-0.mdx](https://github.com/otter-sec/anchor/blob/v1.2.0/docs/content/docs/updates/release-notes/1-0-0.mdx)) |
| Fuzz / property | **proptest 1.11.0** (math crate) + **Crucible 0.2.1** via `anchor fuzz` | crates.io; [asymmetric-research/crucible](https://github.com/asymmetric-research/crucible/releases/tag/v0.2.1); `Fuzz(crucible_fuzz_cli::Cli)` in [cli/src/lib.rs#L358@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/lib.rs#L358) |
| Verifiable build | **solana-verify 0.5.2** (Docker) | [release v0.5.2](https://github.com/solana-foundation/solana-verifiable-build/releases/tag/v0.5.2), 2026-09-21 |
| TS runtime | **Node 24 LTS (24.21.0)**, **pnpm 12.x** | [nodejs.org/dist/index.json](https://nodejs.org/dist/index.json) (v24 "Krypton" is LTS; v26 is Current); npm `pnpm` latest 12.6.0 and GitHub [v12.7.0](https://github.com/pnpm/pnpm/releases/tag/v12.7.0) (2026-09-25) |
| TS Solana SDK | **@solana/kit 8.3.0**, **@solana/react 8.3.0**, **@solana/kit-plugin-wallet 0.20.0**, **@solana/kit-plugin-rpc 0.19.0** | npm registry; [anza-xyz/kit v8.3.0](https://github.com/anza-xyz/kit/releases/tag/v8.3.0) |
| Client codegen | **codama 1.11.0**, **@codama/nodes-from-anchor 1.5.6**, **@codama/renderers-js 2.5.0** (requires kit ^8.3.0) | npm; [renderers-js CHANGELOG](https://github.com/codama-idl/renderers-js/blob/main/CHANGELOG.md) |
| Upgrade/admin | **Squads v4** `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` with `time_lock` | [Squads-Protocol/v4 README](https://github.com/Squads-Protocol/v4/blob/af94153ff77a28b6effe46b9c94baaa93742b48c/README.md) |

---

## 1. Anchor

### 1.1 Repository and releases
- **Canonical repo: `otter-sec/anchor`.** `https://github.com/solana-foundation/anchor` and `https://github.com/coral-xyz/anchor` both return `301 → https://github.com/otter-sec/anchor`, and the API redirects to repository id 325891672 (`curl -sI`, 2026-09-27). Release notes and docs now link to `otter-sec/anchor` (e.g. the [1.0.0 notes](https://github.com/otter-sec/anchor/blob/v1.2.0/docs/content/docs/updates/release-notes/1-0-0.mdx) say `cargo install avm --git https://github.com/otter-sec/anchor --locked`).
- **Latest stable: v1.2.0**, published 2026-09-04. Tag `v1.2.0` → commit `84a63f9f7112b23581816436fbcf3f6c515779f9` (`gh api repos/otter-sec/anchor/git/tags/bea40e2…`). Earlier 1.x releases: 1.0.0 (2026-04-02), 1.0.1–1.0.3, 1.1.0–1.1.2 (2026-06-25/26). On 2026-09-14 there were back-port patches 0.30.2 / 0.31.2 / 0.32.2 ([release list](https://github.com/otter-sec/anchor/releases)). 0.32.2 is a TS-only patch ([0-32-2.mdx](https://github.com/otter-sec/anchor/blob/v0.32.2/docs/content/docs/updates/release-notes/0-32-2.mdx)).
- **Anchor v2 is not ready.** `anchor-lang 2.0.0-rc.1` was published to crates.io on 2026-08-12. It is pinocchio-based and depends on `pinocchio ^0.11` and `wincode ^0.5` (crates.io dependency API). The v2 docs say: "This version is alpha software. It has not been audited … APIs may break between commits. Depend on it from git on the `anchor-next` branch" ([docs-v2 …/v2/index.mdx@master](https://github.com/otter-sec/anchor/blob/master/docs-v2/src/content/docs/v2/index.mdx), [alpha-limitations.mdx](https://github.com/otter-sec/anchor/blob/master/docs-v2/src/content/docs/v2/reference/alpha-limitations.mdx)). **Recommendation:** build on 1.2.0 and look at v2 again after it has a stable, audited release.

### 1.2 Required Solana / platform-tools / Rust
- **Solana CLI 4.1.2** is the recommended version for Anchor 1.2.0 (see §0). The Anchor verifiable-build image at tag v1.2.0 is built with `SOLANA_CLI=v4.1.2` ([docker/Makefile@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/docker/Makefile)). The explicit "recommended Solana version" sentence exists only on master, from commit 78ed411a40 (2026-09-14), not in the tagged 1.2.0 release notes. The install docs on master show `solana-cli 4.1.2` next to `anchor-cli 1.2.0` ([installation.mdx@master](https://github.com/otter-sec/anchor/blob/master/docs/content/docs/installation.mdx)).
  - Agave context: GitHub's "Latest" release is v4.3.0 (2026-09-18). The `stable` installer channel installed 4.2.2 (built from `e29e5d9`, which is one CI-only commit ahead of tag v4.2.2 `c9c6f32`, per `gh api repos/anza-xyz/agave/compare/v4.2.2...e29e5d9`). Mainnet RPC reports `apiVersion 4.3.0` (getAccountInfo `context`). The CLI version does not need to match the cluster version for build and deploy.
- **Platform-tools.** Anchor 1.2.0 always passes `--tools-version v1.57 --arch v3` to `cargo build-sbf` unless you override them ([cli/src/lib.rs#L82-L84, #L247-L251@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/lib.rs#L82-L84)). Environment override: `ANCHOR_BUILD_SBF_ARCH`.
  - `cargo-build-sbf` is now a separate crate ([anza-xyz/cargo-build-sbf](https://github.com/anza-xyz/cargo-build-sbf)). Agave 4.1.2 and 4.2.2 both ship `cargoBuildSbfVersion=4.1.0` ([scripts/cargo-build-sbf-version.sh@v4.1.2](https://github.com/anza-xyz/agave/blob/v4.1.2/scripts/cargo-build-sbf-version.sh)). Version 4.1.0 **defaults to platform-tools v1.54** (`DEFAULT_PLATFORM_TOOLS_VERSION = "v1.54"`, [cargo-build-sbf/src/lib.rs@cargo-build-sbf@v4.1.0](https://github.com/anza-xyz/cargo-build-sbf/blob/cargo-build-sbf%40v4.1.0/cargo-build-sbf/src/lib.rs#L23)). cargo-build-sbf 4.4.0 defaults to v1.57.
  - v1.54 = Rust 1.89.0 / LLVM 20 ([release](https://github.com/anza-xyz/platform-tools/releases/tag/v1.54)). v1.57 = Rust 1.95.0 / LLVM 22 ([release](https://github.com/anza-xyz/platform-tools/releases/tag/v1.57)).
  - ⇒ **Running plain `cargo build-sbf` produces a different binary (v1.54, arch v0) from `anchor build` (v1.57, arch v3).** Build only through `anchor build`, or always pass `--tools-version v1.57 --arch v3` explicitly (this matters for verifiable builds, §5.2).
- **Rust.** `anchor-lang` 1.2.0 has `rust-version = "1.89"` ([lang/Cargo.toml@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/lang/Cargo.toml)). `anchor init` writes `rust-toolchain.toml` with `channel = "1.89.0"` (`ANCHOR_MSRV`, [cli/src/template.rs#L21@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/template.rs#L21)). On-chain code is compiled by the platform-tools rustc (1.95.0 in v1.57), not the host toolchain, so **on-chain dependencies must build with rustc ≤ 1.95**. Host-side dependencies can need more: Agave 4.3 client crates declare MSRV 1.97.1 (crates.io `solana-rpc-client`/`solana-program-test` 4.3.0), `carbon-core` 2.0.0 declares 1.96.1 and `sqlx` 0.9.0 declares 1.94.0.
- **Solana SDK crate line.** Anchor 1.2.0 builds on the `solana-*` **3.x** component crates (`solana-account-info 3.1`, `solana-pubkey 3.0`, `solana-instruction 3.0`, `solana-sysvar 3.1.1`, …) and `spl-token-2022-interface 2`, `spl-token-interface 2` ([Cargo.toml@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/Cargo.toml), [spl/Cargo.toml@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/spl/Cargo.toml)). The newest upstream versions are `solana-program` 5.1.0 and `spl-token-2022-interface` 3.1.2 (crates.io). Do not pull 5.x SDK crates into the program crate.

### 1.3 Anchor 1.x breaking changes that affect short.win
All from the [1.0.0 release notes](https://github.com/otter-sec/anchor/blob/v1.2.0/docs/content/docs/updates/release-notes/1-0-0.mdx) and the [CHANGELOG](https://github.com/otter-sec/anchor/blob/v1.2.0/CHANGELOG.md) unless noted:
- The TS package is renamed `@coral-xyz/anchor` → **`@anchor-lang/core`**. Version 1.2.0 still depends on `@solana/web3.js ^1.69.1` (npm `@anchor-lang/core/latest` manifest), so it stays on the web3.js v1 world (see §3).
- **`CpiContext::new(program_id: Pubkey, accounts)`**: the program `AccountInfo` is no longer passed ([lang/src/context.rs#L188@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/lang/src/context.rs#L188)). Pump's official CPI example still uses the 0.3x form `CpiContext::new(ctx.accounts.program.to_account_info(), …)` ([pump-public-docs CPI_README.md@81091419](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/docs/CPI_README.md)), so it must be ported.
- **`declare_program!` renames:** `utils`→`parsers` (`try_from_bytes`→`parse`), `errors`→`error`, `ProgramError`→`<Name>Error`. A typed instruction parser is added, and `declare_program!` can be used with `anchor_client` alone.
- **Duplicate mutable accounts are rejected by default.** Opt in with the `dup` constraint.
- `Context<'info, T>` drops three lifetimes. `AccountInfo` inside `#[derive(Accounts)]` now warns (use `UncheckedAccount`). Only one `#[error_code]` block is allowed per program. `.reload()` re-checks the owner.
- **The legacy on-chain IDL instructions are replaced by the Program Metadata Program (PMP)**, `ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S` ([cli/src/fetch/pmp.rs#L23@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/fetch/pmp.rs#L23)).
- **`anchor test` / `anchor localnet` use Surfpool by default.** `--validator legacy` selects `solana-test-validator`. In-process templates (LiteSVM/Mollusk) set `skip_local_validator = true` ([cli/src/lib.rs#L1725-L1728@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/lib.rs#L1725)). `anchor init` now defaults to a **LiteSVM** Rust test template.
- IDL: unsupported field types (e.g. Rust tuples) are now a hard error. External accounts are no longer added to the IDL `accounts` array.
- **1.1.x:** `anchor-syn` moves to syn 2. A `[toolchain]` section and `Anchor.toml` `[scripts]` are added. **1.2.0:** `--arch`/`--tools-version`; a TS error-constants file is generated. (*Corrected M10, below:* the `security.json` template and `anchor deploy --security-metadata` are **not** in 1.2.0; they are under [Unreleased], PR #4177.) Fixes include `declare_program!` namespace collisions with prelude names (#4776), zero-copy undersized-buffer panics (#4555/#4633), and all-zero discriminators now being rejected (#4645) ([CHANGELOG 1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/CHANGELOG.md#120---2026-09-04)).
- Unreleased on master: `CreateAccountAllowPrefund` for `init` on pre-funded accounts, and Token-2022 extension checks for `init_if_needed` on existing mints ([CHANGELOG Unreleased](https://github.com/otter-sec/anchor/blob/2b4b2fd22a881228e02c41b26bd23addb5b8b26f/CHANGELOG.md)). The second one is worth watching for our mint handling.

### 1.4 Features to use

**`declare_program!` (CPI from vendored IDLs).**
- Mechanism: `declare_program!(pump);` reads `idls/pump.json` at the workspace root and generates `cpi`, `client`, `accounts`, `events`, `types`, `parsers`, `error` and `constants` modules ([features/declare-program.mdx@master](https://github.com/otter-sec/anchor/blob/master/docs/content/docs/features/declare-program.mdx); generator source in [lang/attribute/program/src/declare_program/](https://github.com/otter-sec/anchor/tree/v1.2.0/lang/attribute/program/src/declare_program)). It needs the new IDL spec (`metadata.spec = "0.1.0"`).
- **The Pump IDLs are compatible in format.** `idl/pump.json`, `idl/pump_amm.json` and `idl/pump_fees.json` at [pump-public-docs@81091419](https://github.com/pump-fun/pump-public-docs/tree/81091419e4457566469d4e2a27f64ed84d42419c/idl) (refreshed in commit `e0687ae9b7e0`, 2026-09-12) all have `spec: "0.1.0"`, 8-byte discriminators, and no generics. I analysed them with a local script:
  - pump: 47 instructions, 7 accounts, 28 events, 42 types.
  - pump_amm: 32 instructions, 7 accounts, 26 events, 39 types.
  - The only non-trivial constructs are two tuple structs (`OptionBool(bool)`, `OptionU64(u64)`), which the generator handles through `IdlDefinedFields::Tuple` ([common.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/lang/attribute/program/src/declare_program/common.rs)), plus instruction `returns` values.
  - pump and pump_fees define an account named `Global`. The prelude-collision fix #4776 in 1.2.0 addresses exactly this class of problem.
  - Pump itself documents `declare_program!(pump)` for CPI ([CPI_README.md](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/docs/CPI_README.md)).
  - **UNVERIFIED:** a real compile under anchor-lang 1.2.0 (not run).
- **Caveat from Pump:** do **not** decode Pump state with the IDL-generated account types: "The raw IDL type does not [zero-pad], and will fail on legacy accounts" (e.g. older, shorter `BondingCurve` accounts). They recommend `pump_rust_client::state` instead ([CPI_README.md](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/docs/CPI_README.md)). `pump-rust-client` 0.1.13 declares `anchor-lang >=0.31, <2` but also `solana-program >=2.1.21` and `anchor-spl` with the `metadata` feature (crates.io dependency API), which would pull a second SDK line into our program. ⇒ Write our own **length-tolerant, zero-padding readers** for `BondingCurve`, `Global`, `FeeConfig` and PumpSwap `Pool` in a `no_std` crate. Use `declare_program!` only for instruction builders and CPI.
- **Jupiter:** Jupiter's own example uses `declare_program!(jupiter_aggregator)` with `idls/jupiter_aggregator.json` on Anchor 0.30.1 ([jup-ag/jupiter-cpi-swap-example@b9bdc57](https://github.com/jup-ag/jupiter-cpi-swap-example/tree/b9bdc5707555c62b2b1154ea36d0f46b905c8a41/cpi-swap-program)). Surfpool vendors a v6 IDL (`spec 0.1.0`, camelCase instruction names such as `sharedAccountsRouteV2`) ([surfpool …/jupiter/v6/idl.json@v1.6.0](https://github.com/solana-foundation/surfpool/blob/v1.6.0/crates/core/src/scenarios/protocols/jupiter/v6/idl.json)). **UNVERIFIED:** compile under 1.2.0. Choosing the canonical Jupiter IDL is left to the venue research.
- **IDL drift is real.** Current mainnet programdata `slot` fields (last deploy): pump `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` → slot 449,734,335 (**2026-09-23**, after the 2026-09-12 IDL refresh); pump_amm `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` → 446,462,733 (2026-09-12); pump_fees `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` → 446,465,969; Jupiter `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` → 449,280,566 (2026-09-22). All three Pump programs share upgrade authority `7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8`. (Decoded from the programdata accounts `B5MvUwXd…`, `6naEzKeU…`, `75Uu23mq…`, `4Ec7ZxZS…` via getMultipleAccounts; timestamps via getBlockTime.) Surfpool's Pump integration was "verified 2026-08-07" at deploy slot 433,095,571 and says "A later deployment slot … means the program was upgraded and this integration must be revisited" ([surfpool pump README@v1.6.0](https://github.com/solana-foundation/surfpool/blob/v1.6.0/crates/core/src/scenarios/protocols/pump/README.md)).

**`InterfaceAccount` / `token_interface`.**
- `anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface}` accept both the SPL Token and Token-2022 owners and unpack through `StateWithExtensions`. There is a `get_mint_extension_data::<T>()` helper, and extension CPI modules include `permanent_delegate`, `transfer_hook`, `transfer_fee`, `pausable`, … ([spl/src/token_interface.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/spl/src/token_interface.rs), [spl/src/token_2022_extensions/](https://github.com/otter-sec/anchor/tree/v1.2.0/spl/src/token_2022_extensions)).
- Use `InterfaceAccount<'info, Mint>`, `InterfaceAccount<'info, TokenAccount>` and `Interface<'info, TokenInterface>` everywhere, and `transfer_checked` for movements.
- The §3 mint safety filter can be implemented as: `mint.freeze_authority.is_none()` plus checking the extension-type list for `PermanentDelegate`, `TransferHook`, `TransferFeeConfig` (and see open question Q1).
- Note: on mainnet the classic Token program `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` is now **p-token**. Feature `ptokFjwyJtrwCa9Kgo9xoDS59V4QccBGEaRFnRPnSdP` has been active since slot 419,472,000 (2026-05-13), per the mainnet feature account. Tokenkeg's programdata `3gvYRKWy…` was deployed at slot 419,472,000 and has no upgrade authority. This matters for CU numbers in tests (LiteSVM 0.12+ loads p-token when that feature is on, [litesvm programs/mod.rs@v0.16.0](https://github.com/LiteSVM/litesvm/blob/v0.16.0/crates/litesvm/src/programs/mod.rs)).

**`emit_cpi!`.** Enable the `event-cpi` feature of `anchor-lang` ([lang/Cargo.toml@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/lang/Cargo.toml)), add `#[event_cpi]` on the accounts struct, and call `emit_cpi!(Event{..})`. The docs say this was "introduced [as] an alternative to program logs, which can sometimes be truncated by data providers … this approach does incur additional compute costs" ([features/events.mdx@master](https://github.com/otter-sec/anchor/blob/master/docs/content/docs/features/events.mdx)). Recommendation: use `emit_cpi!` for all §10 events (the indexer depends on them), and keep `emit!` only for debug. Budget CU for it. Codama renderers-js 2.4+ generates event decoders ([CHANGELOG 2.4.0](https://github.com/codama-idl/renderers-js/blob/main/CHANGELOG.md)).

**Zero-copy.** `#[account(zero_copy)]` + `AccountLoader<T>`, `#[repr(C)]` Pod types, `load_init` / `load` / `load_mut`. `init` is limited to 10,240 bytes, and bigger accounts need the `zero` constraint with pre-allocation ([features/zero-copy.mdx@master](https://github.com/otter-sec/anchor/blob/master/docs/content/docs/features/zero-copy.mdx)). 1.2.0 fixed undersized-buffer panics and truncation (#4555, #4633). Recommended use: `Market` (read on every open/close/liquidate, includes the EMA state and rate index). `ShortPosition` and `LenderPosition` are small and can be Borsh `Account<T>`.

**IDL format.** JSON, `metadata.spec = "0.1.0"`, generated by the `idl-build` feature during `anchor build`. Uploaded on-chain through PMP. `anchor idl convert` converts legacy⇄current IDLs ([cli/src/lib.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/lib.rs), "Convert legacy IDLs (pre Anchor 0.30) to the new IDL spec").

**Codama integration inside Anchor.** `anchor codama convert|generate`, and an `Anchor.toml` `[clients]` section with `auto = true` that regenerates clients on `anchor build`. It runs `npx --yes codama` by default and pins `@codama/nodes` 1.6.0 semantics ([cli/src/codama.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/codama.rs), [config.rs `ClientsConfig`](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/config.rs)). Recommendation: generate with our own pinned `codama` devDependency (§3), not `npx --yes`, for supply-chain control.

---

## 2. Testing

### 2.1 Tool status (Sept 2026)

| Tool | Version / date | Status and fit | Source |
|---|---|---|---|
| **LiteSVM** (Rust) | 0.16.0, 2026-08-24; MSRV 1.89 | In-process SVM on Agave 4.2.1 crates (its Cargo.toml writes `"4.2.1"`, i.e. `^4.2.1`, so a fresh lockfile can also pick 4.3.x unless pinned; see the Mollusk row). Supports V1 transactions and bundles the **mainnet feature set** (`LiteSVM::new()` → `with_mainnet_features()`). Features: `add_program`/`add_program_from_file`/`add_program_with_loader`, `set_account`, `set_sysvar::<Clock>`, `warp_to_slot`, `simulate_transaction`, CU budget, register tracing and an SBPF debugger (0.13). **Primary program test harness.** | [release](https://github.com/LiteSVM/litesvm/releases/tag/v0.16.0), [lib.rs@v0.16.0](https://github.com/LiteSVM/litesvm/blob/v0.16.0/crates/litesvm/src/lib.rs) (L536, L761, L835, L1039, L1161, L1173), [CHANGELOG](https://github.com/LiteSVM/litesvm/blob/v0.16.0/CHANGELOG.md) |
| LiteSVM (npm `litesvm`) | 1.4.1, 2026-08-24; deps `@solana/kit ^8` | TS bindings (napi). Good for SDK and client tests with the Codama/Kit client. | npm registry |
| `@solana/kit-plugin-litesvm` | 0.19.0 | Kit client backed by LiteSVM | npm; [anza-xyz/kit-plugins](https://github.com/anza-xyz/kit-plugins) |
| Anchor's LiteSVM template | pins `litesvm = "0.10.0"` | Too old: mainnet feature set only arrived in 0.12.0 (2026-05-13) and p-token loading in 0.11/0.12. Bump to 0.16. | [template.rs#L605@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/template.rs#L605); LiteSVM CHANGELOG |
| `anchor-litesvm` / `litesvm-utils` | 0.4.0, 2026-04-09 | Depends on `litesvm ^0.11.0`, so it would hold us at 0.11. **Avoid** or fork. | crates.io dependency API |
| **Mollusk** | 0.15.1, 2026-08-29. Declares the Agave crates as `^4.2.0` (its Cargo.toml pins `solana-program-runtime = "4.2.0"`, `agave-feature-set = "4.2.0"`, …), so a fresh lockfile can resolve them to the newest 4.x (4.3.0 on 2026-09-27, MSRV 1.97.1; resolver 3 with an older workspace `rust-version` may prefer 4.2.x). It is not built against 4.3.0; the Agave 4.3 build is the separate pre-release `0.15.0-agave-4.3.0-beta.0` (`=4.3.0-beta.0` pins). | Single-instruction harness, no AccountsDB/Bank. `mollusk-svm-bencher` writes CU benchmark markdown. Use it for CU regression tests of hot instructions. **Pin** the resolved Agave crate versions in the on-chain workspace `Cargo.lock`, next to LiteSVM 0.16 (which declares `^4.2.1` for the same crates), so both harnesses run one Agave version. | [README](https://github.com/anza-xyz/mollusk/blob/main/README.md); [Cargo.toml@0.15.1](https://github.com/anza-xyz/mollusk/blob/0.15.1/Cargo.toml) (tag → `f432ef13`); crates.io `mollusk-svm/0.15.1/dependencies`, `mollusk-svm/0.15.0-agave-4.3.0-beta.0/dependencies`, `solana-program-runtime/4.3.0` (`rust_version` 1.97.1) |
| solana-program-test | 4.3.0 (MSRV 1.97.1) | Still published from the Agave monorepo, but monorepo crates outside the compatibility policy are now "Agave Unstable API" behind the `agave-unstable-api` feature (Agave 4.0 CHANGELOG). In 4.3.0 sysvar getters return `UnsupportedSysvar` in native-mode processors. **Don't use.** | [agave CHANGELOG@v4.3.0](https://github.com/anza-xyz/agave/blob/v4.3.0/CHANGELOG.md), [program-test/Cargo.toml@v4.3.0](https://github.com/anza-xyz/agave/blob/v4.3.0/program-test/Cargo.toml) |
| **Surfpool** | 1.6.0, 2026-09-18 | Drop-in replacement for `solana-test-validator` that lazily forks mainnet (`surfpool start`, `--rpc-url`, `--network devnet`, `--offline`, `--snapshot`). Cheatcodes include `surfnet_setAccount`, `surfnet_setTokenAccount`, `surfnet_timeTravel`, `surfnet_pauseClock`, `surfnet_cloneProgramAccount`, `surfnet_writeProgram`, `surfnet_registerScenario`, `surfnet_profileTransaction`, `surfnet_exportSnapshot`, … **v1.6.0 adds Pump/PumpSwap scenario templates** (`pump-bonding-curve-custom`, `pump-global`, `pump-amm-canonical-pool`, graduation builder). It also ships an `@solana/surfpool` kit plugin (npm 1.6.0). | [release notes](https://github.com/solana-foundation/surfpool/releases/tag/v1.6.0), [cli/mod.rs@v1.6.0](https://github.com/solana-foundation/surfpool/blob/v1.6.0/crates/cli/src/cli/mod.rs), [surfnet_cheatcodes.rs@v1.6.0](https://github.com/solana-foundation/surfpool/blob/v1.6.0/crates/core/src/rpc/surfnet_cheatcodes.rs), [pump README](https://github.com/solana-foundation/surfpool/blob/v1.6.0/crates/core/src/scenarios/protocols/pump/README.md) |
| `anchor test` | 1.2.0 | Runs the `[scripts] test` command. With the LiteSVM/Rust templates that is `cargo test` (Mollusk: `cargo test-sbf`) and no validator starts. Otherwise it uses Surfpool (config in `Anchor.toml [surfpool]`: `datasource_rpc_url`, `online`, `airdrop_addresses`, …) or `--validator legacy` (`[test.validator]` `clone`/`account`/`account_dir`/`genesis`). | [template.rs#L1499@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/template.rs#L1499), [config.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/config.rs) (`SurfpoolConfig`, `Validator`, `CloneEntry`, `AccountEntry`) |
| **Crucible** (`anchor fuzz`) | 0.2.1 (2026-08-04); 0.3.0-alpha.1 | Coverage-guided, stateful invariant fuzzer on LibAFL + LiteSVM 0.15.2 with sBPF edge coverage. Works on the compiled `.so`, uses the IDL for typed calls, supports RPC cloning, and has a `--mutate-accounts` constraint-check engine. Crucible 0.3.0-alpha depends on anchor-lang 1.0.1. Integrated as `anchor fuzz init/run/show/tmin`. | [crucible README](https://github.com/asymmetric-research/crucible/blob/main/README.md), [Cargo.toml](https://github.com/asymmetric-research/crucible/blob/main/Cargo.toml), [testing/fuzzing.mdx@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/docs/content/docs/testing/fuzzing.mdx) |
| Trident | stable 0.12.0 (2025-11-27); 0.13.0-rc.4 (2026-05-18) | Both depend on `solana-sdk ^2.3`, one SDK major behind Anchor 1.x (3.x). The repo was last pushed 2026-05-18. **Not recommended** for this stack. | [releases](https://github.com/Ackee-Blockchain/trident/releases), crates.io dependency API |
| proptest | 1.11.0 (MSRV 1.85) | Property tests on the pure math crate. | crates.io |

**Type compatibility note.** Anchor 1.2 uses `solana-pubkey 3` → `solana-address ^1.0`. LiteSVM 0.16 uses `solana-address ~2.6`. `solana-address 1.1.0` depends on `solana-address ^2.0` (a semver-trick re-export), so both should resolve to one `Address` type (crates.io dependency API for `solana-address/1.1.0`, `solana-pubkey/3.0.0`, `litesvm/0.16.0`). Crucible compiles anchor-lang 1.0.1 together with `solana-account 4.3` and `solana-pubkey 3.0` ([Cargo.toml](https://github.com/asymmetric-research/crucible/blob/main/Cargo.toml)). **UNVERIFIED by compilation here.**

### 2.2 Recommended test pyramid
1. **`shortwin-math` (host, `cargo test`)**
   - Unit tests plus **proptest** properties:
     - curve/AMM buy→sell round-trips never create value;
     - buyback cost is monotone in Q;
     - kinked-rate continuity at U = 80%;
     - fee splits sum exactly to the input (§6.3);
     - CR formula invariants;
     - cap computations stay bounded.
   - **Differential tests against real mainnet trades:** record pre/post reserves and amounts from real Pump/PumpSwap buy/sell transactions (`getTransaction` pre/post balances) and assert that our math reproduces them to the lamport.
2. **Program (LiteSVM 0.16, Rust)**
   - Load `target/deploy/shortwin.so` plus **dumped mainnet programs** (pump, pump_amm, pump_fees; Jupiter only where needed) plus account fixtures.
   - Drive rent accrual and expiry with `set_sysvar::<Clock>`/`warp_to_slot`.
   - Simulate 2×/5×/10× gaps, drains and "graduation mid-position" by rewriting curve/pool accounts with `set_account`.
   - Check the §10 invariants after every instruction.
   - Assert that the CPI depth and CU budgets hold under the **mainnet feature set**. SIMD-0268 ("raise CPI nesting limit from 4 to 8", feature `6TkHkRmP7JZy1fdM6fg5uXn76wChQBWGokHBJzrLB3mj`) has **no feature account on mainnet**, i.e. it is not activated. So our program → Jupiter → AMM → Token is already at depth 4 ([agave feature-set@v4.3.0 L1316/L2459](https://github.com/anza-xyz/agave/blob/v4.3.0/feature-set/src/lib.rs); mainnet getMultipleAccounts at slot 450,803,187).
3. **CU benchmarks**: `mollusk-svm-bencher` on open_short / close_short / liquidate / crank_price. Commit the markdown output and diff it in CI.
4. **Invariant fuzzing**: `anchor fuzz` (Crucible), with actions = all §15 instructions plus "price shock" actions that rewrite venue accounts. Encode the §10 invariants (token conservation, no admin fund movement, utilization ≤ cap after open) as fuzz invariants. Run as a nightly job.
5. **Fork/E2E (Surfpool)**: lazy mainnet fork, with Pump scenario templates for bonding-curve and graduation states. The keeper and frontend run against it, and `surfnet_timeTravel` exercises expiry and the S3 10-minute timer. Also the only practical place to test Jupiter routes end to end, since a route touches many AMM programs.

### 2.3 Loading real mainnet programs and accounts
- **Programs:** `solana program dump -u m <PROGRAM_ID> tests/fixtures/programs/<name>.so` (Agave CLI `program dump` subcommand, [cli/src/program.rs@v4.1.2](https://github.com/anza-xyz/agave/blob/v4.1.2/cli/src/program.rs)). For upgradeable programs this writes the ELF from programdata.
- **Accounts:** `solana account -u m <ADDR> --output json --output-file tests/fixtures/accounts/<addr>.json`. The JSON is a flattened `RpcKeyedAccount` (`pubkey` + `account{lamports,data,owner,executable,rentEpoch,space}`) ([cli-output `CliAccount`@v4.1.2](https://github.com/anza-xyz/agave/blob/v4.1.2/cli-output/src/cli_output.rs)). The same files work with `solana-test-validator --account <ADDR> <FILE>` / `--account-dir`, and with `--clone`, `--maybe-clone`, `--clone-upgradeable-program`, `--bpf-program`, `--upgradeable-program` ([validator/src/cli.rs@v4.1.2](https://github.com/anza-xyz/agave/blob/v4.1.2/validator/src/cli.rs)). In Anchor 1.2 these are `[[test.validator.account]]`, `[[test.validator.clone]]` and `[[test.genesis]]`.
- **LiteSVM:** `svm.add_program(id, &bytes)` (the upgradeable loader is the default) or `add_program_with_loader(...)` to match mainnet CU, then `svm.set_account(addr, Account{..})` after decoding the JSON (base64 `data`). A ~50-line fixture loader in `tests/litesvm/src/fixtures.rs` handles this.
- **Provenance:** keep a `tests/fixtures/manifest.json` with, for each file: source slot, programdata `slot`, sha256, and IDL commit. A scheduled CI job re-reads the programdata `slot` of pump / pump_amm / pump_fees / Jupiter and fails if it changed (upgrade risk R5).
- **Default programs:** LiteSVM already bundles SPL Token (or p-token under the feature), Token-2022 v11.0.0, ATA 1.1.1, Memo and ALT ([programs/mod.rs@v0.16.0](https://github.com/LiteSVM/litesvm/blob/v0.16.0/crates/litesvm/src/programs/mod.rs)). The mainnet Token-2022 programdata was deployed at slot 427,147,035. Dump it if byte-for-byte parity matters. (*M10:* done — the harness now runs the deployed Token-2022 and spl-memo v3, not the bundled builds; below.)

---

## 3. Clients and frontend

- **`@solana/web3.js` v1**: "Version 1.x is no longer actively maintained and receives critical fixes only. New development happens here, on the v3+" ([solana-web3.js README@main](https://github.com/solana-foundation/solana-web3.js/blob/main/README.md)). Advisory [#3947](https://github.com/solana-foundation/solana-web3.js/issues/3947) (2026-09-22) says `@solana/web3.js` 3.0 (a Kit-backed classic API, currently `3.0.0-rc.3` on npm tag `rc`) "will take the `latest` npm dist-tag in **Q4, 2026**". npm `latest` is still 1.99.0 (2026-09-08).
- **`@solana/kit`** (repo [anza-xyz/kit](https://github.com/anza-xyz/kit)) 8.3.0 (2026-09-09) is the successor. solana.com's migration page says `@solana/web3.js` v1, `@solana/wallet-adapter-*` and the earlier framework-kit packages (`@solana/client`, `@solana/react-hooks`, `@solana/web3-compat`) "are superseded", that "New apps build directly on `@solana/kit` and the kit plugins", and that wallets should go through `@solana/kit-plugin-wallet` (Wallet Standard) with `@solana/react` for React ([solana.com/docs/frontend/web3-compat](https://solana.com/docs/frontend/web3-compat), fetched 2026-09-27). The framework-kit packages were last published 2026-01-16 (npm).
- **Wallets:** `@solana/kit-plugin-wallet` 0.20.0 does Wallet Standard discovery, connection and signers (`walletSigner` / `walletPayer` / `walletIdentity`). Its peers are `@solana/kit ^8.2`, `@solana/react ^8.2` and `react ^19.2.8` ([README](https://github.com/anza-xyz/kit-plugins/blob/main/packages/kit-plugin-wallet/README.md); npm manifest). `@solana/wallet-adapter-react` 0.15.40 (2026-09-10) still ships, but new apps should not start on it.
- **Codama:** `codama` 1.11.0 CLI, `@codama/nodes-from-anchor` 1.5.6 (reads Anchor spec `v01`, including events), `@codama/renderers-js` 2.5.0 ("Generated code now requires `@solana/kit` `^8.3.0`"; events since 2.4.0), `@codama/renderers-rust` 3.1.3 (npm; [nodes-from-anchor src/v01](https://github.com/codama-idl/codama/tree/main/packages/nodes-from-anchor/src/v01); [renderers-js CHANGELOG](https://github.com/codama-idl/renderers-js/blob/main/CHANGELOG.md)). Note: 1.5.6 fixed `Vec<u8>` PDA seeds deriving from Borsh-prefixed bytes. Stay on ≥ 1.5.6.
- **Frontend recommendation:** Vite 8.3.1 SPA + React 19.3.0 + `@tanstack/react-query` 5.104.0 + TanStack Router 1.170.x + `@solana/kit` / `@solana/react` / `@solana/kit-plugin-wallet` / `@solana/kit-plugin-rpc` + the Codama client + `@shortwin/math-wasm` for quotes, CR and liquidation price (npm versions as of 2026-09-27). Reasoning: the product is a client-side trading UI (build tx → `simulateTransaction` → sign), so there is little to gain from SSR. §12 R6 geo-restriction can sit at the CDN/edge. Next.js 16.3.6 is the alternative if marketing/SEO pages grow.
- **Avoid `@anchor-lang/core` in the app** (it pulls in web3.js v1). Use it only for ad-hoc scripts, if at all.

---

## 4. Repository layout

### 4.1 Package manager: **pnpm workspaces**
- Anchor 1.2's `[toolchain] package_manager` accepts npm/yarn/pnpm/bun ([config.rs `PackageManager`@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/config.rs)).
- The Solana JS ecosystem repos we depend on are pnpm-based: the kit-plugins docs use `pnpm install` ([kit-plugins README](https://github.com/anza-xyz/kit-plugins/blob/main/README.md)), solana-web3.js "is a pnpm workspace" ([README](https://github.com/solana-foundation/solana-web3.js/blob/main/README.md)), and solana-foundation/github-actions examples use `pnpm/action-setup@v6` ([readme.md](https://github.com/solana-foundation/github-actions/blob/main/readme.md)).
- Bun 1.4.2 (2026-09-05) is viable but gains little here, since the services are Rust. Pin pnpm via `"packageManager": "pnpm@12.x"` and Node 24 LTS via `mise.toml` (the dev machine already uses mise: Node at `~/.local/share/mise/installs/node/26.8.2`).

### 4.2 Two Cargo workspaces
- The **on-chain workspace** (root) contains programs, `no_std` shared crates and program tests. It must resolve to crates that build with platform-tools rustc 1.95.
- The **services workspace** (`services/`) contains the keeper and indexer. It depends on the shared crates by `path` and is free to use Agave 4.3 client crates (MSRV 1.97.1), `sqlx` 0.9, etc.
- Separate lockfiles stop host-only dependencies from dragging shared transitive crates onto versions the SBF toolchain cannot compile. Anchor also refuses to nest a workspace inside an existing Cargo workspace (1.1.1 CHANGELOG, #4576).

### 4.3 Directory tree
```
short.win/
├── Anchor.toml                 # [toolchain] anchor_version="1.2.0", solana_version="4.1.2", package_manager="pnpm"
│                               # [programs.localnet|devnet|mainnet] shortwin=…; [surfpool] datasource_rpc_url=…
│                               # [scripts] test="cargo test -p shortwin-litesvm-tests"; [hooks] post_build="pnpm codegen"
├── Cargo.toml                  # on-chain workspace: members = programs/*, crates/*, tests/litesvm
│                               # resolver="3"; [profile.release] overflow-checks=true, lto="fat", codegen-units=1
│                               # [workspace.metadata.cli] solana = "4.1.2"   (for solana-verify image selection)
├── Cargo.lock
├── rust-toolchain.toml         # channel="1.98.1", components=["rustfmt","clippy"]
├── mise.toml                   # node=24, pnpm=12 (dev convenience)
├── package.json  pnpm-workspace.yaml  pnpm-lock.yaml  codama.json  biome.json  turbo.json(optional)
├── idls/                       # third-party IDLs consumed by declare_program!
│   ├── pump.json  pump_amm.json  pump_fees.json  jupiter_aggregator.json
│   └── PROVENANCE.md           # source repo@commit, program id, programdata slot at vendoring time
├── programs/
│   └── shortwin/               # Anchor 1.2 program (cdylib+lib); features: cpi, no-entrypoint, idl-build, event-cpi
│       └── src/ { lib.rs, state/, instructions/, venues/{pump_curve.rs,pump_amm.rs,jupiter.rs}, events.rs, errors.rs }
├── crates/
│   ├── shortwin-math/          # #![no_std], zero Solana deps: curve & CPMM math, fees, kinked rate, CR, tiers, caps, EMA
│   │                           # u128 / U256 checked arithmetic; #![deny(clippy::arithmetic_side_effects)]; proptest in dev-deps
│   ├── shortwin-venue-state/   # #![no_std] length-tolerant readers for Pump BondingCurve/Global/FeeConfig, PumpSwap Pool,
│   │                           # SPL/Token-2022 account & extension checks (bytemuck); shared by program, keeper, indexer
│   └── shortwin-math-wasm/     # wasm-bindgen facade over shortwin-math → built into packages/math-wasm
├── tests/
│   ├── litesvm/                # Rust integration tests (LiteSVM 0.16, litesvm-token 0.16), fixture loader, scenario builders
│   ├── bench/                  # mollusk-svm-bencher CU benchmarks → bench/*.md committed
│   └── fixtures/
│       ├── programs/*.so       # dumped mainnet ELFs (pump, pump_amm, pump_fees, jupiter, token-2022)
│       ├── accounts/*.json     # `solana account --output json` snapshots
│       ├── txs/*.json          # recorded mainnet trades for differential math tests
│       └── manifest.json       # slot, programdata slot, sha256 per fixture
├── fuzz/shortwin/              # `anchor fuzz init shortwin` (Crucible harness + invariants)
├── services/                   # separate Cargo workspace (host only)
│   ├── Cargo.toml  Cargo.lock
│   ├── common/                 # RPC/Yellowstone clients, declare_program!(shortwin) via anchor-client 1.2, config
│   ├── keeper/                 # liquidate / expire / sol_settle / crank_price; simulation first; Jito bundle submission
│   └── indexer/                # ingest (Yellowstone gRPC or RPC backfill) → Postgres; axum REST + WebSocket API
├── clients/js/                 # Codama-generated TS client (@shortwin/client), generated, committed, CI diff-checked
├── packages/
│   ├── math-wasm/              # wasm-pack output (@shortwin/math-wasm)
│   ├── sdk/                    # tx builders, PDAs, venue routing (Jupiter quote), preflight simulation, risk disclosures text
│   └── config/                 # program ids, cluster endpoints, tier tables mirrored from GlobalConfig (read-only)
├── apps/web/                   # Vite + React 19 + @solana/kit/@solana/react/@solana/kit-plugin-wallet + TanStack
├── scripts/                    # dump-fixtures.ts, check-venue-upgrades.ts, deploy helpers
├── .github/workflows/          # ci.yml, nightly.yml, release.yml
└── docs/ { SPEC.md, research/ }
```
Keeper language: **Rust**. It reuses `shortwin-math` and `shortwin-venue-state` natively, so the keeper's CR and executable-cost numbers match on-chain exactly, which is the §5.1 trigger. Rust service crates, versions from crates.io 2026-09-27: `tokio` 1.53.1, `axum` 0.8.9, `sqlx` 0.9.0, `yellowstone-grpc-client` 14.0.0 (`carbon-core` 2.0.0 is optional).

### 4.4 CI outline (GitHub Actions)
- **`ci.yml`** (PRs):
  1. **rust-lint**: `cargo fmt --check` and `cargo clippy -D warnings` for both workspaces, plus `cargo deny check` (0.20.2) and `cargo audit` (0.22.2).
  2. **math**: `cargo test -p shortwin-math` with `PROPTEST_CASES=2000`.
  3. **build-program**:
     - Install the toolchain with `solana-foundation/github-actions/setup-all` (v0.2.15; inputs include `anchor_version` and `verify_version`), or run in the container `quay.io/ottersec/anchor:v1.2.0` ([verifiable-builds.mdx](https://github.com/otter-sec/anchor/blob/master/docs/content/docs/references/verifiable-builds.mdx), [docker/build/Dockerfile@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/docker/build/Dockerfile)).
     - `anchor build --tools-version v1.57 --arch v3`.
     - Upload `.so` + IDL as artifacts.
     - Fail if `target/idl/shortwin.json` or `clients/js` differ from the committed copies.
  4. **program-tests**: `cargo test -p shortwin-litesvm-tests` using the built `.so` and cached fixtures, plus the Mollusk CU bench diff.
  5. **ts**: `pnpm install --frozen-lockfile`, `pnpm -r typecheck test build` (Vitest 5, litesvm npm tests).
  6. **static-analysis**: Radar GitHub Action (see §5.1).
- **`nightly.yml`**: `anchor fuzz run shortwin <tests> --release --stateful --cores N --timeout …`; the venue-upgrade drift check (programdata slots); Surfpool E2E (keeper + web e2e with Playwright 1.63).
- **`release.yml`** (tags):
  1. `solana-verify build --library-name shortwin --arch v3 --cargo-build-sbf-args="--tools-version v1.57"`, and assert the hash equals the CI `anchor build` hash. The `=` is required (see §5.2): the space-separated form fails argument parsing.
  2. `write-program-buffer` → `prepare-squads-release` (creates the Squads proposal) → `idl-upload` / `metadata-upload` (PMP) → after execution, `solana-verify verify-from-repo` and `solana-verify remote submit-job` (OtterSec worker). The action names come from [solana-foundation/github-actions](https://github.com/solana-foundation/github-actions) (moved from `solana-developers/github-actions`), whose readme says "It is highly recommended to use the squads integration for program deployments."

### 4.5 Full pin list (as of 2026-09-27)
- **Rust side:** anchor-lang/anchor-spl/anchor-client **1.2.0**; Agave CLI **4.1.2**; platform-tools **v1.57**; arch **v3**; host rustc **1.98.1**; litesvm / litesvm-token **0.16.0**; mollusk-svm / mollusk-svm-bencher **0.15.1** (plus the Agave `solana-program-runtime` / `agave-feature-set` / … versions they resolve to, pinned in `Cargo.lock`: a plain resolve would pick 4.3.0 on 2026-09-27, whose crates declare MSRV 1.97.1, while the MSRV-aware resolver 3 with a workspace `rust-version = "1.89"` may prefer 4.2.x; which one the lockfile gets is **UNVERIFIED** until the workspace exists); proptest **1.11.0**; Crucible **0.2.1** (through `anchor fuzz`); Surfpool **1.6.0**; solana-verify **0.5.2**; solana-security-txt **1.1.3**; cargo-deny **0.20.2**; cargo-audit **0.22.2**; cargo-nextest **0.9.146** (optional); wasm-bindgen **0.2.129**; wasm-pack **0.15.0** (crates.io).
- **JS side:** Node **24.21.0 LTS**; pnpm **12.x**; TypeScript **7.0.2** (fallback 6.0.3 if tooling breaks; **UNVERIFIED** ecosystem compatibility); @solana/kit / @solana/react **8.3.0**; @solana/kit-plugin-wallet **0.20.0**; @solana/kit-plugin-rpc **0.19.0**; @solana/kit-plugin-litesvm **0.19.0**; litesvm (npm) **1.4.1**; @solana/surfpool **1.6.0**; codama **1.11.0**; @codama/nodes-from-anchor **1.5.6**; @codama/renderers-js **2.5.0**; @solana-program/token **0.17.0**, token-2022 **0.19.0**, system **0.15.0**, compute-budget **0.19.0**; react **19.3.0**; vite **8.3.1**; @vitejs/plugin-react **6.1.1**; @tanstack/react-query **5.104.0**; vitest **5.0.2**; @biomejs/biome **2.5.14**; @playwright/test **1.63.0** (npm registry).

---

## 5. Security tooling

### 5.1 Static analysis and linters
- **Built into Anchor:**
  - `/// CHECK:` doc requirement on unchecked accounts (`--skip-lint` exists but should not be used).
  - Duplicate-mutable-account rejection by default (1.0).
  - `overflow-checks = true` in the release profile (Anchor workspace template, [template.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/cli/src/template.rs)).
  - [security-exploits reference](https://github.com/otter-sec/anchor/blob/master/docs/content/docs/references/security-exploits.mdx).
- **Clippy:** in `shortwin-math` and the program, deny `clippy::arithmetic_side_effects`, `clippy::cast_possible_truncation`, `clippy::cast_sign_loss`, `clippy::unwrap_used`. This enforces §9 "checked math".
- **Radar** (Auditware): a template-based static analyzer for Rust/Anchor that runs in Docker and has a GitHub Action ([Auditware/radar](https://github.com/Auditware/radar), pushed 2026-09-07; [radar-action](https://github.com/Auditware/radar-action)).
- **Trail of Bits `solana-lints`** (Dylint lints from the Sealevel attacks: missing signer/owner check, arbitrary CPI, insecure close, …) ([crytic/solana-lints](https://github.com/crytic/solana-lints), last push 2026-02-19; `cargo-dylint` 6.1.0 on crates.io). **UNVERIFIED:** whether it builds against the current nightly and Anchor 1.2.
- **Dependency hygiene:** `cargo deny` (licenses, bans, duplicate SDK majors) and `cargo audit` (RustSec).
- **Program metadata:** `solana-security-txt` 1.1.3 ([neodyme-labs](https://github.com/neodyme-labs/solana-security-txt)), and/or a `security` JSON uploaded through the Program Metadata Program. (*Corrected M10:* Anchor 1.2.0 has neither a `security.json` template nor `anchor deploy --security-metadata`; the upload goes through the program-metadata CLI, below.)
- A web search also turns up newer third-party scanners (e.g. solana-fender, Solazy, SolAudit; [dev.to overview](https://dev.to/ohmygod/the-solana-security-toolbox-in-2026-a-practitioners-guide-to-fuzzing-static-analysis-and-5h7f)). None was evaluated here (**UNVERIFIED** quality). The external audit in §9 remains mandatory.

### 5.2 Verifiable builds
- `solana-verify` 0.5.2 builds inside pinned-digest Docker images, one per Agave version: `docker/v4.1.2.Dockerfile`, `v4.2.2`, `v4.3.0` all exist ([repo tree](https://github.com/solana-foundation/solana-verifiable-build/tree/master/docker)).
- The v4.1.2 image installs `cargo-build-sbf 4.1.0` and pre-installs **platform-tools v1.54** ([v4.1.2.Dockerfile](https://github.com/solana-foundation/solana-verifiable-build/blob/master/docker/v4.1.2.Dockerfile)). `--arch` defaults to **v0** ([src/main.rs@v0.5.2](https://github.com/solana-foundation/solana-verifiable-build/blob/v0.5.2/src/main.rs#L157-L161)).
- The image is chosen from `[workspace.metadata.cli] solana = "…"` and otherwise from the `Cargo.lock` versions of solana-program / solana-program-error / solana-account-info ([README](https://github.com/solana-foundation/solana-verifiable-build/blob/master/README.md)). For an Anchor 1.2 program that fallback would find a **3.x** crate version and pick an **old image**.
- ⇒ **Must do:**
  - set `[workspace.metadata.cli] solana = "4.1.2"`;
  - pass `--arch v3 --cargo-build-sbf-args="--tools-version v1.57"` so the result matches `anchor build`. The option is declared with clap 2 `.require_equals(true)` on `build`, `verify-from-repo` and `export-pda-tx` (*corrected M10*; `remote submit-job` takes no build arguments) ([src/main.rs#L162-L167@v0.5.2](https://github.com/solana-foundation/solana-verifiable-build/blob/v0.5.2/src/main.rs#L162-L167), also L268–273 and L334–339; `Cargo.lock` resolves clap 2.34.0). A local clap 2.34.0 reproduction of that declaration rejects `--cargo-build-sbf-args "--tools-version v1.57"` with "requires a value but none was supplied" and accepts the `=` form. solana-verify splits the value on whitespace before passing it to `cargo build-sbf` (L1101–1102). Use the same `=` form in `verify-from-repo`;
  - check hash equality in CI. (*M10: verified* — `scripts/verifiable-build.sh` builds both programs byte-identical to `anchor build`, locally and in CI's `verifiable-build` job; below.)
- Alternative: `anchor build --verifiable` (image `quay.io/ottersec/anchor:v1.2.0`) and `anchor verify`. Explorer/OtterSec verification ([verify.osec.io](https://verify.osec.io), used by `@anchor-lang/core` since 1.1.1) consumes the solana-verify PDA flow: `verify-from-repo` uploads the PDA with the upgrade authority, then `remote submit-job` (*M10:* `verify-from-repo --remote` is deprecated and returns an error pointing to that flow; with Squads the PDA upload is `export-pda-tx`, executed by the vault). With Squads, the PDA upload is a multisig transaction; solana-foundation/github-actions v0.2.14 notes "Prevent squads system transfers" ([releases](https://github.com/solana-foundation/github-actions/releases)).

### 5.3 Upgrade authority via Squads v4, and timelocks
- Squads v4 `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` is live and immutable on mainnet (programdata `Fy3YMJCv…`, upgrade authority `None`, deployed at slot 302,582,236). It has been audited by OtterSec, Neodyme, Certora (with formal verification) and Trail of Bits ([README](https://github.com/Squads-Protocol/v4/blob/af94153ff77a28b6effe46b9c94baaa93742b48c/README.md)).
- **Timelock:** `Multisig.time_lock: u32` = "How many seconds must pass between transaction voting settlement and execution", with `MAX_TIME_LOCK = 3 months` ([state/multisig.rs@af94153](https://github.com/Squads-Protocol/v4/blob/af94153ff77a28b6effe46b9c94baaa93742b48c/programs/squads_multisig_program/src/state/multisig.rs#L9-L28)). It is enforced for both vault transactions and config transactions ([vault_transaction_execute.rs#L71](https://github.com/Squads-Protocol/v4/blob/af94153ff77a28b6effe46b9c94baaa93742b48c/programs/squads_multisig_program/src/instructions/vault_transaction_execute.rs#L71), [config_transaction_execute.rs#L83](https://github.com/Squads-Protocol/v4/blob/af94153ff77a28b6effe46b9c94baaa93742b48c/programs/squads_multisig_program/src/instructions/config_transaction_execute.rs#L83)). A *controlled* multisig (non-default `config_authority`) lets that authority change the config directly, so use **autonomous** multisigs.
- **Recommended setup (implements §9 and §11):**
  - **Governance multisig** (e.g. 3-of-5, `time_lock = 172800` s = 48h): the program **upgrade authority** plus the `GlobalConfig` **parameter admin** (fees, tier table, venue program IDs).
  - **Emergency multisig** (e.g. 2-of-4, `time_lock = 0`): only the **pause authority** (pause new opens globally or per market). It has no power over funds or parameters (§9/§11).
  - **Defense in depth:** an in-program two-step `propose_config` / `apply_config` with an on-chain `not_before = now + 48h`. The 48h guarantee then lives in our program's code and holds even if the multisig membership changes. Its delay value must itself be a timelocked parameter (decision-log item per §15).
  - The Squads Smart Account program `SMRTzfY6DfH5ik3TKiyLFfXexV8uSG3d2UksSCYdunG` is labelled "v0.1 … in active development" ([smart-account-program README](https://github.com/Squads-Protocol/smart-account-program/blob/main/README.md)). Stay on v4.

---

## 6. Audit of the background install (`scratchpad/install-toolchain.log`)
The install finished 2026-09-27 00:42:51. Observed state (from the log, `~/.local/share/solana/install/*`, `~/.config/solana/install/config.yml`, `~/.avm`, `~/.cache/solana`):

| # | Finding | Impact | Action |
|---|---|---|---|
| M1 | The script installed Agave **stable = 4.2.2** (`src:e29e5d91`). `avm use latest` then switched the active release to **4.1.2**: the log shows `✨ 4.1.2 initialized`, `active_release → releases/4.1.2` and `config.yml explicit_release: 4.1.2`. The log line `solana-cli 4.2.2` is therefore stale. | The active version (4.1.2) **matches** the Anchor 1.2.0 recommendation. The 4.2.2 release dir is just leftover. | Pin `solana_version = "4.1.2"` in `Anchor.toml [toolchain]` so avm resolution is deterministic. |
| M2 | avm was built from **master `2b4b2fd2`** (`avm v1.2.0 (…#2b4b2fd2)`), not from tag v1.2.0 (`84a63f9`). The 1.2.0→4.1.2 mapping only landed on master on 2026-09-14 (#5034). At tag v1.2.0, `anchor-solana-map.toml` ends at `1.0.2 → 3.1.10` and the platform-tools map says `4.0.0 → v1.56`. | An avm built from the tag would probably resolve Anchor 1.2.0 to Solana 3.1.10 (floor lookup; **UNVERIFIED** behaviour). | Keep the explicit `[toolchain]` pins, and install avm from a tag in CI. |
| M3 | Log warning: "The latest cargo build-sbf platform-tools (v1.57) is not installed". Afterwards `~/.cache/solana/v1.57/platform-tools/{llvm,rust,version.md}` exists (00:41–00:42). | `anchor build` uses v1.57, so this is fine if the download completed (**UNVERIFIED**; nothing was executed). Agave 4.1.2's bundled `cargo-build-sbf 4.1.0` defaults to **v1.54**, which is **not** installed, so a bare `cargo build-sbf` would download v1.54 and build a different binary. | Always build with `anchor build` or with explicit `--tools-version v1.57 --arch v3`. |
| M4 | The script used `--git https://github.com/solana-foundation/anchor`, which is now a redirect to `otter-sec/anchor`. | Works through the redirect. | Use the canonical `otter-sec/anchor` URL in docs and CI. |
| M5 | Host rustc **1.98.1** (stable, 2026-09-01). `anchor init` would write `rust-toolchain.toml` with 1.89.0. | No conflict. The repo should pin its own `rust-toolchain.toml` (1.98.1 recommended, §4.2). | Commit `rust-toolchain.toml`. |
| M6 | **Surfpool is not installed** (`surfpool: not found`), and neither is `solana-verify`. | `anchor test` without an in-process template, and `anchor localnet`, need Surfpool (lowest recommended 1.1.2). | Install Surfpool 1.6.0 (`curl -sL https://run.surfpool.run/ \| bash`, [README](https://github.com/solana-foundation/surfpool/blob/v1.6.0/README.md)) and `cargo install solana-verify --locked` (0.5.2). |
| M7 | Node on the machine is **v26.8.2** (Current, via mise). pnpm is not installed. Docker 29.7.2 is present. | Node 26 is not LTS. CI and the Anchor image use Node 22/24 (Anchor image `NODE_VERSION="v22.22.3"`, [Dockerfile@v1.2.0](https://github.com/otter-sec/anchor/blob/v1.2.0/docker/build/Dockerfile)). | Pin Node 24 LTS and pnpm 12 in `mise.toml` and CI. |

---

## 7. Open questions
1. **Mint filter scope (owner):** should Token-2022 **Pausable** (supported by anchor-spl since 1.2.0), **DefaultAccountState=Frozen**, **NonTransferable** and **MintCloseAuthority** also be rejected? Pausable and default-frozen can block vault transfers much like a freeze authority. The spec lists only freeze authority, permanent delegate, transfer hooks and transfer fees.
2. **Pump IDL freshness:** pump was redeployed 2026-09-23 (slot 449,734,335), after the pump-public-docs IDL refresh (2026-09-12). Before vendoring, compare against the on-chain IDL (PMP or legacy IDL account).
3. **Jupiter IDL source:** vendor Jupiter's `jupiter_aggregator.json` (from their CPI example) or Surfpool's camelCase v6 IDL? This affects `declare_program!` naming. It also needs a CPI-depth budget check, since SIMD-0268 is not active.
4. **Verifiable build path:** choose `solana-verify` (Explorer/OtterSec flow) or `anchor build --verifiable`, and prove bit-equality with the CI artifact.
5. **Indexer data source:** Yellowstone gRPC needs a provider (Triton/Helius/…); RPC polling plus `getSignaturesForAddress` backfill is the fallback. This is a cost and ops decision.
6. **TypeScript 7:** do the lint, build and test tools we pick all support the native TS 7 compiler? Otherwise stay on 6.0.3.

## 8. Not verified (summary)
- None of this was compiled: `declare_program!` on the pump / pump_amm / pump_fees / Jupiter IDLs under anchor-lang 1.2.0; anchor-lang 1.2 + LiteSVM 0.16 type unification (inferred from the solana-address 1.1→2.x semver trick); whether LiteSVM 0.10/0.11 (Anchor template / anchor-litesvm) load SBPFv3 ELFs by default.
- Completeness of the platform-tools v1.57 install in `~/.cache/solana/v1.57`.
- Hash equality between solana-verify (with the flags above) and `anchor build`. (*Verified in M10*, below.)
- Radar and solana-lints compatibility with Anchor 1.2; quality of the newer third-party scanners.
- Ecosystem compatibility of TypeScript 7.
- The npm `latest` for pnpm is 12.6.0 while GitHub shows v12.7.0 (2026-09-25). Pin whichever CI resolves reproducibly.
- How avm behaves when built from tag v1.2.0 (M2).

---

## Review corrections (2026-09-27)

These apply the first-pass review findings that target this note. Each was re-checked against primary sources, not taken from the review.

1. **C1-5: verifiable-build argument syntax (§4.4 `release.yml` step 1, §5.2 "Must do").**
   - **Change:** `--cargo-build-sbf-args "--tools-version v1.57"` becomes `--cargo-build-sbf-args="--tools-version v1.57"`.
   - **Why:** solana-verify 0.5.2 declares the option with clap 2 `.takes_value(true).require_equals(true)` ([src/main.rs L162–167 @ v0.5.2](https://github.com/solana-foundation/solana-verifiable-build/blob/v0.5.2/src/main.rs#L162-L167), tag → `f8cfe2f8`). The same holds for `verify-from-repo` (L268–273) and a third subcommand (L334–339). `Cargo.lock` resolves clap 2.34.0.
   - **Reproduction:** a scratch binary with clap 2.34.0 and the same declaration rejects the space form ("The argument '--cargo-build-sbf-args=<ARGS>' requires a value but none was supplied") and accepts the `=` form. The value is split on whitespace before `cargo build-sbf` (L1101–1102). `--arch` accepts v0–v3 with default v0, as stated.
2. **C1-11: toolchain citation (§0 "Solana/Agave CLI" row, §1.2).**
   - "The recommended Solana version is `4.1.2`" is **not** in `1-2-0.mdx` at tag v1.2.0, which only links the CHANGELOG. It was added on master in commit `78ed411a406c0c210bddc7fb9e78bb686c64c374` (2026-09-14, #5034, the same commit as the avm 1.2.0→4.1.2 mapping).
   - The citation now points to that commit and to `docker/Makefile@v1.2.0` (`SOLANA_CLI=v4.1.2`). The 4.1.2 pin itself is unchanged.
3. **C1-14: Mollusk crate versions (§2.1 Mollusk and LiteSVM rows, §4.5).**
   - `mollusk-svm` 0.15.1 declares its Agave crates as `^4.2.0`: crates.io `…/0.15.1/dependencies`, and `Cargo.toml` at tag 0.15.1 (`f432ef13`) writes `"4.2.0"`. It is not "Agave 4.3.0 crates".
   - The Agave 4.3 build is the separate pre-release `0.15.0-agave-4.3.0-beta.0` (`=4.3.0-beta.0`). LiteSVM 0.16.0 declares `^4.2.1`.
   - A plain (non-MSRV-aware) resolve would pick the newest compatible 4.x, which on 2026-09-27 is 4.3.0 (`solana-program-runtime` 4.3.0, `rust_version` 1.97.1); no resolve was run here. The MSRV-aware resolver 3 may prefer 4.2.x, so which one lands in the lockfile is **UNVERIFIED** until the workspace exists.
   - **Added:** pin the resolved Agave versions in the on-chain workspace `Cargo.lock`, so LiteSVM and Mollusk share one Agave version.

No review finding against this note was rejected.

---

## M10 corrections and updates (2026-10-03)

Checked against primary sources and the repository's M10 tree; each item names its evidence.

1. **Anchor 1.2.0 has no `security.json` / `--security-metadata` (§1.3, §5.1).**
   - Anchor's CHANGELOG lists them under [Unreleased] (PR #4177, merged 2026-09-17; v1.2.0 shipped
     2026-09-04); `cli/src/template.rs@v1.2.0` contains no "security", and the installed
     `anchor-1.2.0` has no such deploy flag (M10 security.txt research).
   - What short.win does instead: both programs embed `solana-security-txt =1.1.3`'s
     `security_txt!` (compiled out of `no-entrypoint` and `test-venue` builds;
     `scripts/check-security-txt.py`, the CI release gate; ARCH §8.5). A Program Metadata
     `security` upload (`npx @solana-program/program-metadata write security <program-id> …`,
     through Squads with `--export`) is optional and M11's (SECTXT-7).
2. **`require_equals` scope (§5.2, review correction 1).** In solana-verify 0.5.2
   (`src/main.rs` at `f8cfe2f8`), `--cargo-build-sbf-args` is `.require_equals(true)` on `build`
   (L165), `verify-from-repo` (L271) and `export-pda-tx` (L337). `remote submit-job` takes only
   `--program-id` and `--uploader`. The value is split on whitespace before `cargo build-sbf`
   (L1102).
3. **`--remote` is deprecated (§5.2).** `verify-from-repo --remote` returns an error ("The --remote
   flag has been deprecated. Upload your verify PDA with programs upgrade authority, then queue the
   remote worker with `solana-verify remote submit-job --program-id … --uploader …`", L500). The
   flow is the PDA upload (with Squads: `export-pda-tx`, executed by the vault), then
   `remote submit-job`; verify.osec.io requires public source. On-chain verification is M11.
4. **Verifiable build: done and byte-equal (§5.2, §8).**
   - `[workspace.metadata.cli] solana = "4.1.2"` selects
     `solanafoundation/solana-verifiable-build@sha256:2e0b78f4…` (IMAGE_MAP entry (4,1,2) at
     v0.5.2); `--arch v3 --cargo-build-sbf-args="--tools-version v1.57"` reproduces `anchor build`
     byte for byte for `shortwin` and `complete_curve` (`scripts/verifiable-build.sh --compare`, CI
     job `verifiable-build`).
   - Without Docker, `scripts/verifiable-build.sh --local` runs the same image's rootfs (fetched by
     digest, every blob checked) under bubblewrap (docs/DEVELOPMENT.md "The build rule").
   - IMAGE_MAP at v0.5.2 has 291 entries from 1.10.1 up (including (1,18,16) and (3,1,10)), so the
     shim can also rebuild third-party programs: Raydium CPMM's 2026-10 redeploy
     (`raydium-cp-swap@bdeee23`, (3,1,10)) was reproduced byte for byte (`tests/fixtures/README.md`).
     Squads v4 (1.18.16) was not attempted.
5. **The LiteSVM harness runs mainnet's SPL programs (§2.3).** Since M10 every `Env` replaces the
   bundled Token-2022 and spl-memo v3 with the deployed builds (fixture group `spl_programs`):
   Token-2022 at deploy slot 427,147,035, trimmed ELF `7ea94a02…` = OtterSec's verified build of
   `solana-program/token-2022@9bc02757` (program v11.0.0, the same version LiteSVM bundles, a
   different build), and spl-memo v3 (BPFLoader2, `f520eaf0…`); `SHORTWIN_BUNDLED_SPL=1` opts out.
   Token-2022 CPIs cost about 15 CU more on the mainnet ELF.
6. **Squads v4 facts (§5.3), as M10 pinned and tested them.**
   - Program `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf`: immutable since slot 302,582,236 (no
     upgrade authority), trimmed ELF `d4866083…2fac`; IDL 2.1.0 vendored from
     `Squads-Protocol/v4@64af733` (sha256 `cb9a0a29…`; fixture group `squads_programs`). The source
     of the deployed ELF is **UNVERIFIED** (probably `64af733`, the newest default-branch commit before the
     deploy; not rebuilt, and verify.osec.io lists it as not verified, registered commit `3742e552`,
     checked 2026-10-03).
   - `MAX_TIME_LOCK` = 7,776,000 s (90 days; `crates/squads-v4-ix`). The time lock runs from the
     approval that reaches the threshold and is checked at execution against the multisig's
     **current** time lock, so a later increase delays an approved transaction. An `Approved` vault
     transaction never expires and survives a member rotation and a threshold or time-lock change
     (only `Active` proposals and config transactions go stale); cancel unwanted ones with
     `proposal_cancel_v2` (T-SQ-7 in `tests/litesvm/tests/squads_gov_it.rs`).
   - The owner's layout (2026-10-02): Governance 3-of-5 with time lock 172,800 s (the upgrade
     authority and `governance`, its vault 0; `fee_recipient`, its vault 1), Emergency 2-of-4 with
     time lock 0 (`pause_authority`); short.win requires the authorities pairwise distinct (SQ-G8).
     A program change takes 96 h end to end (48 h Squads + 48 h program). Members sign in the Squads
     app; the repository's `shortwin-gov` (`services/ops`) stages and verifies only (2026-10-03).
   - Governance cannot cancel a parameter change reactively inside the program window (its cancel
     executes at `not_before` at the earliest); the Emergency has no veto over upgrades and its
     members cannot vote on Governance proposals. `ExtendProgram` is permissionless, so an upgrade
     to a larger ELF needs it before execution.
   - The multisigs are created on mainnet in M11. Spending limits are not used.

