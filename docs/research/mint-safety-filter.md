# Mint safety filter (spec §3) and token vaults: research

*Research note for short.win. Written 2026-09-27. Baseline: `docs/SPEC.md` v0.3 (owner-approved, not modified here).*
*Status: research input for the protocol and security agents. This is not a spec change. Items that need an owner decision are listed in §10.*

---

## 0. TL;DR

- **Use an allowlist, not a denylist.** A mint is listable only if all of these hold:
  1. it is owned by SPL Token (`Tokenkeg…`) or Token-2022 (`Tokenz…`);
  2. it is not a native (wrapped-SOL) mint;
  3. `freeze_authority` is `None`;
  4. every TLV extension on the mint is one of `MetadataPointer`, `TokenMetadata`, `GroupPointer`, `TokenGroup`, `GroupMemberPointer`, `TokenGroupMember`.

  Everything else is rejected, including extension types the program does not recognise and any parse error. That covers the four spec items (freeze authority, PermanentDelegate, TransferHook, TransferFeeConfig) plus Pausable, NonTransferable, DefaultAccountState, MintCloseAuthority, the confidential extensions, InterestBearing, ScaledUiAmount and the new PermissionedBurn.
- **Token-2022 is still adding extensions.** Mainnet runs Token-2022 **v11.0.0** (deployed 2026-06-17; the on-chain hash matches the release). That version added **`PermissionedBurn` (type 28)**. The `spl-token-2022-interface` **2.x** that `anchor-spl 1.2.0` depends on does not know type 28. With the official parser, an unknown type is a hard error, so an allowlist fails closed automatically.
- **Real pump.fun mints pass the filter.** In 123 of 123 consecutive `create_v2` coins on mainnet (2026-09-26 21:45–21:50 UTC), each mint:
  - is Token-2022, with `mint_authority = None` and `freeze_authority = None`;
  - has exactly two extensions: `MetadataPointer` (authority `None`, pointing at the mint itself) and `TokenMetadata` (update authority `None`).

  Older pump coins are SPL Token, 82 bytes, with both authorities `None`, and also pass.
- **Re-checking on every interaction is cheap.** `transfer_checked` needs the mint account anyway. Under current Token-2022 rules a mint that passes cannot later fail:
  - the freeze authority can never be re-enabled;
  - the dangerous extensions can only be added before `InitializeMint`;
  - only metadata and group entries can be appended later.

  So the re-check works as a tripwire. It protects against Token-2022 upgrades, mint close-and-reinit, and bugs on our side. Enforce it strictly on entry paths (create, deposit, open), but do not let it block exits (spec §11).
- **Vault accounts:**
  - Allowed mints need no account extensions.
  - Use **`transfer_checked` everywhere**.
  - Prefer **PDA-addressed vault token accounts** over ATAs, because an Anchor `init` ATA can be blocked by an attacker who pre-creates it.
  - Always pass `mint.owner` as the token program. ATA addresses depend on the token program.
  - For pump buys, the pump `user` must be a system-owned PDA with no data.

---

## 1. Spec baseline

SPEC §3: reject mints with an **active freeze authority**, a Token-2022 **permanent delegate**, **transfer hooks** or **transfer fees**. The rule is automatic (not curation), and "standard Pump.fun tokens pass this filter". SPEC §3/§4: the risk tier is "re-evaluated on every interaction". SPEC §11: never pause closes or liquidations except during a detected venue migration. SPEC §15: any new parameter must be configurable and recorded in the decision log.

This note implements §3. It extends the filter to other extensions that have the same effect (freeze, seize, block, or break accounting), which is what §3 says the filter is for: "mints that could make the lender vault unsafe regardless of price". Those extensions are marked as recommendations for the decision log (§10).

---

## 2. Verified environment (as of 2026-09-26/27)

| Fact | Value | Source |
|---|---|---|
| Token-2022 program on mainnet | `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`. Programdata `DoU57AYuPFu2QU514RktNPG22QhApEjnKxnBcu4BHDTY`, last deployed at slot 427,147,035 (2026-06-17 20:57 UTC). Upgrade authority `AeLmXCbPaQHGWRLr2saFsEVfmMNuKnxRAbWCT9P5twgz`. | RPC `getAccountInfo` / `getBlockTime` |
| Which Token-2022 build is live | sha256 of the on-chain ELF (trailing zeros stripped) is `7ea94a02…2eb1947`. This equals the published executable hash of **program@v11.0.0**. The newer v11.1.0 (2026-09-23) is "available on devnet and testnet" only. | RPC plus the release notes of [program@v11.0.0](https://github.com/solana-program/token-2022/releases/tag/program%40v11.0.0) and [program@v11.1.0](https://github.com/solana-program/token-2022/releases/tag/program%40v11.1.0) |
| v11.0.0 contents | "confidential transfers re-enabled, the new permissioned-burn extension, and the batch instruction from p-token" | [program@v11.0.0 release notes](https://github.com/solana-program/token-2022/releases/tag/program%40v11.0.0) |
| SPL Token program on mainnet | `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`. Programdata `3gvYRKWyXRR9xKWe1ZjPhLY5ZJRN7KDB4rFZFGoJfFk2`, deployed at slot 419,472,000 (2026-05-13), upgrade authority `None`. The ELF sha256 `435df1e7…3bf478075` equals the `spl_p_token.so` asset of **p-token@v1.0.0**. So SPL Token on mainnet is the p-token (Pinocchio) reimplementation. The mint layout is unchanged. | RPC plus the [p-token@v1.0.0](https://github.com/solana-program/token/releases/tag/p-token%40v1.0.0) asset (hashed locally) |
| Associated Token Account program | `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`, owned by `BPFLoader2111…` (not upgradeable) | RPC |
| pump program | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`, last deployed at slot 449,734,335 (2026-09-23) | RPC; [pump IDL](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/idl/pump.json) |
| pump-amm (PumpSwap) program | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`, last deployed at slot 446,462,733 (2026-09-12) | RPC; [pump_amm IDL](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/idl/pump_amm.json) |
| Rent-exempt minimums now | 82 B → 1,066,800; 165 B → 1,488,440; 170 B → 1,513,840; 390 B → 2,631,440 lamports. These are **lower than the historical 2,039,280 for 165 B**, so never hard-code rent. | RPC `getMinimumBalanceForRentExemption` |

### Crates (crates.io API, 2026-09-27)

| Crate | Latest stable | Notes |
|---|---|---|
| `anchor-lang` / `anchor-spl` | **1.2.0** (2026-09-04). 2.0.0-rc.1 exists; 0.32.2 and 0.31.2 were patched on 2026-09-14. | `anchor-spl 1.2.0` depends on **`spl-token-2022-interface ^2`**, `spl-token-interface ^2` and `spl-associated-token-account-interface ^2` ([spl/Cargo.toml@v1.2.0](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/spl/Cargo.toml)). `anchor-spl 2.0.0-rc.1` still uses `spl-token-2022-interface ^2` plus `pinocchio-token-2022 ^0.3`. The GitHub API resolves `solana-foundation/anchor` to `otter-sec/anchor`. |
| `spl-token-2022-interface` | **3.1.2** (2026-09-23). The last 2.x is 2.1.0 (2025-11-03). | 3.x includes `PermissionedBurn`. 2.1.0 does not ([ExtensionType enum @ interface@v2.1.0](https://github.com/solana-program/token-2022/blob/interface%40v2.1.0/interface/src/extension/mod.rs) ends at `PausableAccount` = 27). |
| `spl-token-2022` (program crate) | 11.1.0 | Programs should depend on `-interface`, not on this crate. The program crate itself deprecates re-exports "Use spl_token_2022_interface instead". |
| `spl-token-interface` | 3.0.0 (`anchor-spl` uses ^2) | |
| `pinocchio-token-2022` | 0.4.0 | Only relevant if we move off Anchor. |

`anchor_spl::token_2022::spl_token_2022` is a re-export of `spl_token_2022_interface` ([token_2022.rs:8](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/spl/src/token_2022.rs)). `anchor_spl::token::spl_token` is a re-export of `spl_token_interface` ([token.rs:11](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/spl/src/token.rs)).

---

## 3. Mint layouts and how to read them in Anchor

### 3.1 Base mint (both programs, 82 bytes)

Byte offsets are identical for SPL Token and Token-2022 ([T22 interface/src/state.rs](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/state.rs) `struct Mint`). They were confirmed on-chain against `DM3hTLQYRuejhQsSim3XFVV5wYF3VKB1b9UWsEsnpump`.

| Offset | Size | Field | Encoding |
|---|---|---|---|
| 0 | 36 | `mint_authority` | `COption<Pubkey>`: u32 LE tag (0 = None, 1 = Some) followed by 32 bytes |
| 36 | 8 | `supply` | u64 LE |
| 44 | 1 | `decimals` | u8 |
| 45 | 1 | `is_initialized` | bool |
| 46 | 36 | `freeze_authority` | `COption<Pubkey>` (tag at 46..50, key at 50..82) |

### 3.2 Token-2022 extended mint

Source: [interface/src/extension/mod.rs L286-L314, L205-L240](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/mod.rs).

- Bytes 82..165 are **zero padding**. Mints are padded to `Account::LEN` = 165 so a mint can never be confused with a token account. The parser rejects non-zero padding (`type_and_tlv_indices`).
- Byte 165 is the `AccountType` (1 = Mint, 2 = Account).
- From byte 166 the data is TLV: `u16 LE type | u16 LE length | value`. The walk stops at type 0 (`Uninitialized`) or when fewer than 2 bytes remain.
- A data length of exactly 355 (`Multisig::LEN`) is always rejected (`check_min_len_and_not_multisig`). The program pads such accounts with an `Uninitialized` entry instead.
- **An unknown type is a hard error.** `try_for_each_tlv_extension_type` calls `ExtensionType::try_from(&[u8])`, which maps any unknown discriminant to `InvalidAccountData`. The 2.1.0 crate does the same ([mod.rs L1135-L1143 @ interface@v2.1.0](https://github.com/solana-program/token-2022/blob/interface%40v2.1.0/interface/src/extension/mod.rs)).

Worked example: pump mint `DM3hTLQYRuejhQsSim3XFVV5wYF3VKB1b9UWsEsnpump` (390 bytes). Bytes 82..165 are all zero, byte 165 = 1, then TLV type 18 `MetadataPointer` (len 64) at offset 166, then type 19 `TokenMetadata` (len 152) at offset 234, ending at 390 (RPC).

### 3.3 Reading it inside an Anchor program

| API | What it does | Source |
|---|---|---|
| `InterfaceAccount<'info, anchor_spl::token_interface::Mint>` | Owner check against {Tokenkeg, Tokenz}, then `StateWithExtensions::<Mint>::unpack`. This parses the **base only**. It does not iterate TLV, so it does **not** reject unknown extensions. | [token_interface.rs](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/spl/src/token_interface.rs) |
| `StateWithExtensions::<Mint>::unpack(&data)` | Works for both 82-byte SPL mints (empty TLV) and extended mints. Rejects multisig length, bad padding, wrong account type, and uninitialized mints (`Pack::unpack`). | [mod.rs L525-L550](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/mod.rs) |
| `PodStateWithExtensions::<PodMint>::unpack` | Zero-copy variant. This is what the Token-2022 program uses internally. | same, L552-L575 |
| `.get_extension_types()` | Walks every TLV entry and returns `Vec<ExtensionType>`. **Errors on unknown types.** This is the call the filter needs. | same, L441-L443 |
| `.get_extension::<T>()` | Reads one fixed-size extension. The filter does not need it, because v1 rejects by presence. | same, L427-L431 |
| `anchor_spl::token_interface::get_mint_extension_data::<T>(&AccountInfo)` | Convenience wrapper over `get_extension`. | [token_interface.rs](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/spl/src/token_interface.rs) |
| Anchor constraints `mint::token_program = …`, `mint::freeze_authority = …`, `token::token_program = …`, `associated_token::token_program = …`, `extensions::permanent_delegate::…`, `extensions::transfer_hook::…` | Declarative checks. `mint::freeze_authority` only compares against a given key. It cannot express "is None", so use a `constraint =` or a handler check. | [lang/syn/src/parser/accounts/constraints.rs@v1.2.0](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/lang/syn/src/parser/accounts/constraints.rs) |

**Version consequence.** With `anchor-spl 1.2.0` the parser comes from interface 2.1.0. Calling `get_extension_types()` on a mint that carries `PermissionedBurn` (type 28, live on mainnet since v11.0.0) returns `Err(InvalidAccountData)`. In an allowlist design that error simply means reject, which is correct. Do **not** use `unwrap_or_default()` or any code that treats a parse error as "no extensions".

Adding a direct `spl-token-2022-interface = "3.1"` dependency next to `anchor-spl 1.2.0` would put two copies of the crate in the build. That is fine for byte parsing, but whether it resolves cleanly with `anchor-lang 1.2.0`'s Solana crate versions is **UNVERIFIED** (the build toolchain was not run here). It is also unnecessary for correctness.

---

## 4. Classification of every Token-2022 extension (token-2022 `main` @ 23b03f8, 2026-09-26)

The full enum is in [interface/src/extension/mod.rs L1074-L1142](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/mod.rs). Discriminants were derived from the enum order. Mint versus account kind comes from `get_account_type()` (L1254-L1291).

**Mutability rules that apply to every row:**
- **The set of dangerous mint extensions is fixed at mint creation.** Every mint-extension `Initialize*` handler (TransferFee, TransferHook, PermanentDelegate, MintCloseAuthority, NonTransferable, DefaultAccountState, InterestBearing, ScaledUiAmount, Pausable, PermissionedBurn, ConfidentialTransfer, ConfidentialMintBurn and the pointer extensions) calls `unpack_uninitialized`. That function fails with `AlreadyInUse` once the mint is initialized (mod.rs L940-L956). `Reallocate` only accepts **account** extensions ([reallocate.rs L54-L56](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/reallocate.rs)).
- Only `TokenMetadata`, `TokenGroup` and `TokenGroupMember` can be **appended after** init, and only with the mint authority's signature ([token_metadata/processor.rs L44-L97](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/token_metadata/processor.rs), [token_group/processor.rs L45-L88](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/token_group/processor.rs)). All three are in the allowlist.
- **Once any authority is `None`, it cannot be set again.** Every branch of `process_set_authority` does `…authority.ok_or(…)?` before `validate_owner` ([processor.rs L709-L1026](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/processor.rs); freeze authority at L810-L824). A config whose authority is `None` is permanently fixed.
- **The contents of an extension stay mutable while its authority is `Some`.** The table lists each case.

| # | Extension | Kind | What it can do to a short.win vault / accounting | Mutable after creation (while authority is Some) | **v1 decision** |
|---|---|---|---|---|---|
| — | base `freeze_authority` | mint field | Freezes any token account, including our vault, which blocks every transfer ([processor.rs L1400-L1452](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/processor.rs)). | Can only go Some → other/None; **can never go None → Some** (L810-L815). | **REJECT if Some** (spec) |
| 0 | Uninitialized | padding | End-of-TLV marker | — | n/a (terminator) |
| 1 | TransferFeeConfig | mint | Fee withheld from every `transfer_checked`, up to **10,000 bps (100%)** (`MAX_FEE_BASIS_POINTS`, [transfer_fee/mod.rs L26](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/transfer_fee/mod.rs)). Borrowed `Q` would no longer equal returned `Q`. | Fee authority can raise it at any time from 0; takes effect **2 epochs later** ([transfer_fee/processor.rs L64-L113](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/transfer_fee/processor.rs)). The 0-bps case can still become non-zero. | **REJECT on presence** (spec) |
| 3 | MintCloseAuthority | mint | Mint can be closed only when `supply == 0` (processor.rs L1375). It can then be **recreated at the same address with different extensions**, and our cached filter result would be stale. | Close authority transferable/revocable (L825-L837). | **REJECT** (allowlist). Harmless if authority is None; candidate to relax (§10) |
| 4 | ConfidentialTransferMint | mint | Enables encrypted balances. Our vault would never configure `ConfidentialTransferAccount`, so it only receives public credits. The risk is to accounting and opacity (hidden flows; prerequisite for #24). No memecoin needs it. | Authority can change auto-approve and auditor. | **REJECT** |
| 6 | DefaultAccountState | mint | If `Frozen`, every new account, **including our vault**, is created frozen ([processor.rs L227-L234](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/processor.rs)). A frozen default requires a freeze authority at `InitializeMint` (L131-L137). | `Update` needs the freeze authority ([default_account_state/processor.rs L45-L77](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/default_account_state/processor.rs)). If the freeze authority was revoked after setting `Frozen`, accounts are born permanently frozen. | **REJECT** (allowlist). With freeze authority None and state = Initialized it is harmless; candidate to relax |
| 9 | NonTransferable | mint | Soulbound: any transfer out fails (`NonTransferableAccount`, processor.rs L369-L374). Cannot be lent or sold. | Fixed. | **REJECT** |
| 10 | InterestBearingConfig | mint | Changes only the **UI amount** (raw u64 unchanged). Lenders and UI see a drifting number. The issuer controls "value per raw unit". | `rate_authority` can change the rate. | **REJECT** |
| 12 | PermanentDelegate | mint | Delegate may `transfer_checked` or burn **from any account, including our vault** (processor.rs L462-L469; the program even logs "tokens in this account may be seized at any time", L210). | Delegate can hand the role to another key or revoke it (L883-L895). | **REJECT on presence** (spec) |
| 14 | TransferHook | mint | Every `transfer_checked` CPIs into an arbitrary program (processor.rs L579-L600). It can block **only our vault** (holding liquidations hostage), needs extra accounts that pump/pump-amm/Jupiter CPIs won't pass, and the program can change. | Hook **program_id can be set or changed at any time, even from None**, by the hook authority ([transfer_hook/processor.rs L53-L86](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/transfer_hook/processor.rs)). Mainnet example: PYUSD `2b1kV6Dk…` has `TransferHook{authority: 2apBGM…, program_id: None}`. It is inactive today but can be switched on (RPC). | **REJECT on presence** (spec) |
| 16 | ConfidentialTransferFeeConfig | mint | Only valid together with #1 and #4 (combination rules at mod.rs L1326-L1370). | Authority. | **REJECT** |
| 18 | MetadataPointer | mint | Display data only. | Authority can repoint it. | **ALLOW** |
| 19 | TokenMetadata | mint (variable length) | Display data only. It can be appended after init by the mint authority. | Update authority can change name/symbol/uri. **Never use metadata as identity.** | **ALLOW** |
| 20 | GroupPointer | mint | Display/collection data. | Authority. | **ALLOW** |
| 21 | TokenGroup | mint | Collection data (max_size, update authority). Can be appended post-init by the mint authority. | Update authority. | **ALLOW** |
| 22 | GroupMemberPointer | mint | Display data. | Authority. | **ALLOW** |
| 23 | TokenGroupMember | mint | Collection membership. Can be appended post-init. | — | **ALLOW** |
| 24 | ConfidentialMintBurn | mint | Mint/burn into encrypted supply. `mint.supply` then **excludes the confidential supply** ([confidential_mint_burn/mod.rs L20-L29](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/confidential_mint_burn/mod.rs)). Hidden inflation breaks any supply-based metric. | Mint authority mints confidentially. | **REJECT** |
| 25 | ScaledUiAmount | mint | Scales the UI amount by an f64 multiplier, with a **scheduled `new_multiplier` at a timestamp** ([scaled_ui_amount/mod.rs L53-L63](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/scaled_ui_amount/mod.rs)). This is a split/rebase lever controlled by the issuer. The price per raw unit can jump for non-market reasons, causing forced liquidations or S1 settlements. | Authority updates the multiplier ([scaled_ui_amount/processor.rs L59-L100](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/scaled_ui_amount/processor.rs)). | **REJECT** |
| 26 | Pausable | mint | When `paused`, `transfer_checked` fails with `MintPaused` (processor.rs L399-L404), as do mint_to (L1079) and burn (L1211). Unchecked `Transfer` also fails, because `PausableAccount` forces the mint to be supplied (L434-L438). **All token movement stops**, so closes and liquidations are impossible. | **Toggled at any time** by the pause authority ([pausable/processor.rs L43-L73](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/pausable/processor.rs)). | **REJECT** |
| 28 | PermissionedBurn *(new in v11.0.0, live on mainnet)* | mint | While an authority is set, a standard `Burn` fails. Burns need a co-signature from the authority ([processor.rs L1150-L1181](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/processor.rs); [permissioned_burn/mod.rs](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/permissioned_burn/mod.rs)). It **cannot move or freeze** anyone's tokens, and short.win never burns. | Authority transferable/revocable (processor.rs L1003-L1016). | **REJECT** (allowlist default; also unparseable by interface 2.x). Low risk; candidate to promote to ALLOW after review |
| 2, 5, 7, 8, 11, 13, 15, 17, 27 | TransferFeeAmount, ConfidentialTransferAccount, ImmutableOwner, MemoTransfer, CpiGuard, NonTransferableAccount, TransferHookAccount, ConfidentialTransferFeeAmount, PausableAccount | **account** | Cannot legitimately appear in a mint's TLV. §8 covers what they mean on our vault and on counterparties' accounts. | — | **REJECT if found in a mint** (malformed) |
| ≥ 29 / unknown | future types | ? | Unknown semantics | — | **REJECT** (parse error) |

Example of a mint the filter rejects, from mainnet: PYUSD `2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo`. It has a freeze authority and carries MintCloseAuthority, PermanentDelegate, TransferFeeConfig, ConfidentialTransferMint, ConfidentialTransferFeeConfig, TransferHook, MetadataPointer and TokenMetadata (RPC). USDC `EPjFWdd5…` (SPL Token) is rejected for its freeze authority `7dGbd2QZ…` (RPC).

---

## 5. Allowlist versus denylist

**Recommendation: allowlist.** Reasons:

1. **The extension set keeps growing, and the Token-2022 program is upgradeable.** Pausable and ScaledUiAmount were added in the 2.x interface line. PermissionedBurn arrived in v11.0.0 and has been live on mainnet since 2026-06-17 (§2). The upgrade authority is `AeLmXC…`. A denylist compiled into our program would **silently accept** the next dangerous extension. An allowlist rejects it.
2. **Fail-closed comes for free.** The official parser already errors on unknown discriminants (§3.2). With an allowlist, "unknown", "parse error" and "not allowed" all lead to the same outcome.
3. **Nothing legitimate is lost.** Standard pump.fun tokens use exactly `{MetadataPointer, TokenMetadata}`, or no extensions at all (§7). The allowlist is a strict superset of what the target market needs.
4. **Smaller audit surface.** Six benign types to reason about instead of 29 and counting.
5. **Cost:** some exotic but harmless mints are refused, for example a mint whose TransferHook authority and program are both None. That trade is acceptable for a permissionless lending venue where lenders bear bad debt (spec §5.4). Each relaxation can be proposed individually through the decision log (§10).

**Presence versus configuration.** v1 rejects on **presence** of TransferFeeConfig, TransferHook and PermanentDelegate, not on current values. A zero fee or a hook with no program id can become active later while their authority exists (§4). A presence check is simpler than parsing each configuration.

---

## 6. When to check: market creation versus every interaction

**What can change after market creation, given current Token-2022 semantics (§4)?**
- The base `freeze_authority` can only move toward None. A mint that passed stays passed.
- No rejected extension can be *added* to an initialized mint. Only TokenMetadata, TokenGroup and TokenGroupMember can be appended, and they are allowed.
- **Configs of rejected extensions** (fee rate, hook program, pause flag, default state, multiplier, rate) are mutable, but none of those extensions can be present on an accepted mint.
- **Mint close and re-init:** only possible with MintCloseAuthority and `supply == 0`. That extension is rejected, and while our vault holds tokens the supply is above zero.
- **Program upgrades** of Token-2022 (or p-token) could change any of the above. This is outside our control.

**Therefore:**
- `create_market`: hard filter. This is the authoritative check.
- **Every instruction that moves tokens** (deposit, withdraw, open_short, close_short, liquidate, sol_settle, expire): re-run the filter. It costs one TLV walk over roughly 400 bytes of an account we already pass for `transfer_checked`. Measured CU is **UNVERIFIED** but expected to be small.
  - **Entry paths** (deposit, open_short) must `require!` it.
  - **Exit paths** (withdraw, close, liquidate, settle, expire) should not revert *because of the filter alone*. Spec §11 says closes and liquidations are never paused. If the token program still lets tokens move, exiting is the best outcome for lenders. If it does not (for example a hypothetical freeze), the transfer itself fails and spec §5.3 S3 ("no executable route ≥ 10 min") governs.
- Because a revert would also roll back any state write, a failing re-check on an exit path cannot persist a "mint unsafe" flag by itself. Two options: add a permissionless `flag_unsafe_mint` instruction that sets `market.paused_opens = true` when the filter fails, or let keepers call the existing per-market pause. This needs a decision (§10).
- Every instruction must also assert `mint.key() == market.mint`, `*mint.owner == market.token_program` and `token_program.key() == market.token_program`.

---

## 7. Pump.fun mints on mainnet

### 7.1 Method

- `getSignaturesForAddress` on the pump mint-authority PDA `TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM`. It is derived here as `find_program_address([b"mint-authority"], pump)`, bump 255; it signs every coin creation.
- `getTransaction` on each signature, decoding pump instructions by Anchor discriminator from the IDL: `create_v2 = [214,144,76,236,95,139,49,180]`, `create = [24,30,200,40,5,28,7,119]`.
- `getMultipleAccounts` on the created mints, decoded with the layout in §3.

The scripts live in the session scratchpad and are not committed.

### 7.2 Results

**Survey:** 123 of 123 successful coin creations between slots 450,799,308 and 450,800,312 (2026-09-26 21:45–21:50 UTC) used **`create_v2`**. In every case:
- owner = Token-2022;
- `mint_authority = None`, `freeze_authority = None`;
- extensions = exactly `[MetadataPointer{authority: None, metadata_address: <the mint itself>}, TokenMetadata{update_authority: None}]`.

Supply was 1,000,000,000.000000 (70 coins), 2,000,000,000.000000 (51 coins; each mayhem-mode coin inspected individually had 2B supply, so these are presumably mayhem coins, but the flag was not decoded for all 51), or slightly below 1B (2 coins with burns). **38 of the 123 mints do not end in `pump`.** Range endpoints: first `3Bievo2zZVGnhKNFZUXp5LMKnQEBd1Xa9gckjLPeoFF1Lg1La9ZyT4uiPy9QeYiDGwxQJ55goRxGCLF36UdKBGip`, last `3thyZArVNYQzf9MT28htohFP4tdGDfrcaKy2FDMdNab5boz1keykypMsty5WiYxuTKCBZDMDVtprCDwoEbWzbP5C` (RPC).

| Mint | Created by (tx) | Program | Len | mint_auth | freeze_auth | Extensions | Notes | Filter |
|---|---|---|---|---|---|---|---|---|
| `DM3hTLQYRuejhQsSim3XFVV5wYF3VKB1b9UWsEsnpump` | `3V6j6WYz8sfcDJWkVp2jwiAVXt5uLLuzkQRKDUTnpmT8M3tSgjryMAVQChcxANPg1K4nMws3nHSzA1zp9GMi8NAg` (`create_v2`) | Token-2022 | 390 | None | None | MetadataPointer (auth None → self), TokenMetadata (update auth None) | regular coin; bonding-curve ATA `FjYEXzSh…` is 170 B with `ImmutableOwner` | PASS |
| `HWifwKJHfttJTT8TmYz2iV969tNvfy21nPn8gLKSpump` | `5FWUcXaea5TZoL7NP4Vbaqy7KWretmi8CyRSQtPJDsiqFSiuBaryDAaFKdDiuTGAUMAMJMAoCPvxjSQvvGmBH5CY` | Token-2022 | 399 | None | None | same | `is_mayhem_mode = true`, supply 2B | PASS |
| `BKb9e5Tn5BVMp4CawsnH2KYpH8uHtpxQCTsMsLGJuBqi` | `4vno9fgMMEfp5wbgnpEMqNkkT78S4xmETqiHZxSULeFGF6PuBqShVsiE9PZjdYtwCVAzstwXoD8ckq1EBYabKL5j` | Token-2022 | 388 | None | None | same | holder-rewards coin (`is_holder_reward = [1]`), no `pump` suffix | PASS |
| `3gMvW4Varg85oLBHG49EKfJWRT3muyGrv9AnQsYQpump` | `4nVWCu7MFPquGxdswVjBjV8eJFpjk1B7HUxoF3tYcTjALMAFxiNc3ZFpSEKHPaZZD3xLUMPjZWA9fGpWWQJx8nFV` | Token-2022 | 409 | None | None | same | mayhem + holder rewards | PASS |
| `EvT3XpN1SqKPZwUvjWPZsRfMdKATxvE4Ge9szQbL8pFm` | `3hknTudc5uDLS5AkvuYF833gUzMECaNF92dCx4ifa78sAMvZniz4XiwnPRVJSPoDNLvs9y6MK3p9XELTS7DGZnUZ` | Token-2022 | 398 | None | None | same | no suffix; bonding curve `3rVjLHsX…` owned by pump, `complete = 0` | PASS |
| `2PNTw3x5Gw2mbAKhnv636gWDUpdT1iS7pNk9TzpApump` | graduated; PumpSwap sell `A6i1MXZQ2htS71VmMfk8XbaJr1gGBNqQcCD4yQuyKu5Cd6Tj1ng55CwqJ8gHzSNNcoGB8deXVWBU3csVN7dU9G9` | Token-2022 | 410 | None | None | same | pool `9UTi1bAw…`; pool base account `rpCddoa…` is the pool's Token-2022 ATA (170 B, `ImmutableOwner`) | PASS |
| `9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump` | legacy pump coin (bonding curve `TBHe5tJn…`, `complete = 1`) | **SPL Token** | 82 | None | None | — | graduated legacy coin | PASS |

The trailing `create_v2` arguments decode per the IDL as `is_cashback_enabled: OptionBool (1 B)`, `creator_fee_bps: OptionU64 (8 B)`, `is_holder_reward: OptionBool (1 B)` ([pump.json](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/idl/pump.json)). The pump docs state that `create_v2` makes "a new Token-2022 mint … metadata pointer set to `mint`" ([COIN_CREATION.md](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/docs/instructions/COIN_CREATION.md)).

**Conclusion:** both the current pump pipeline (Token-2022 via `create_v2`) and legacy SPL Token pump coins pass the filter. The spec statement "Standard Pump.fun tokens pass this filter" holds.

**Caveats:**
- The legacy `create` instruction is still in the IDL. Whether pump still accepts it on mainnet is **UNVERIFIED**; 0 of 123 sampled creates used it.
- The claim that *all* historical legacy pump mints lack a freeze authority rests on a sample of one plus the pump design, and is **UNVERIFIED** in aggregate. The filter checks each mint anyway.

**Do not detect pump coins by the `pump` suffix** (38 of 123 lacked it). Derive `bonding_curve = PDA([b"bonding-curve", mint], pump)` and check that its owner is the pump program. That PDA was verified on-chain for all three sampled coins above.

---

## 8. Vault token accounts and CPI pitfalls

### 8.1 Which account extensions our vaults need

- Token-2022 initialises only the account extensions that the mint's extensions require (`required_init_account_extensions`, [mod.rs L1296-L1310](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/interface/src/extension/mod.rs)):
  - TransferFeeConfig → TransferFeeAmount
  - NonTransferable → NonTransferableAccount + ImmutableOwner
  - TransferHook → TransferHookAccount
  - Pausable → PausableAccount
- **None of these apply to allowlisted mints.** A vault can therefore be a plain 165-byte Token-2022 account.
- The ATA program **always** adds `ImmutableOwner`: `get_account_len(…, &[ExtensionType::ImmutableOwner])`, then `initialize_immutable_owner`, then `initialize_account3` ([ATA processor.rs L117-L160](https://github.com/solana-program/associated-token-account/blob/4e58a1098c8ce9bb256c0aa8d768e34d690918e9/program/src/processor.rs)). The result is 170 bytes, as observed for the pump bonding-curve and PumpSwap pool ATAs (§7).
- `ImmutableOwner` blocks `SetAuthority(AccountOwner)` (processor.rs L740-L742). For a vault owned by our PDA it is defence in depth only, since only our program could sign an owner change and it never will. It is **recommended but not required**. Anchor's `token::` init does **not** add it; it computes the size from the mint's required extensions and calls `initialize_account3` ([codegen constraints.rs L649-L684, L1742-L1758](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/lang/syn/src/codegen/accounts/constraints.rs)).
- Rent: compute with `Rent::get()?.minimum_balance(len)`. Never hard-code 165 bytes or the historical 2,039,280 lamports (§2).

### 8.2 PDA vault versus ATA vault

- **Anchor `init` with `associated_token::…` calls the non-idempotent `associated_token::create`** (codegen constraints.rs L743-L756). The ATA of a deterministic market PDA can be created by anyone, because the ATA program accepts any funder. An attacker who pre-creates `ATA(market_pda, mint)` makes `create_market` fail **permanently for that mint**. That is a listing DoS against a permissionless design.
- **Recommended:** pool and position token vaults at **our own PDA addresses** (for example `[b"pool_vault", market]`), using Anchor `init` with `token::mint`, `token::authority`, `token::token_program`. Only our program can sign for those addresses. Anchor's create path also handles a PDA that someone pre-funded with lamports (fund/allocate/assign branch).
- If an ATA is ever needed (for example a venue that insists on the user's ATA, §8.5), use `create_idempotent` and then validate mint, owner and token program. Anchor `init_if_needed` does the same (L758-L770).

### 8.3 `transfer_checked`

- For allowlisted mints, the legacy unchecked `Transfer` would technically still work on Token-2022: it only fails when the source account carries TransferHookAccount, TransferFeeAmount or PausableAccount (processor.rs L415-L439).
- **Use `transfer_checked` for every movement anyway:**
  - `anchor_spl::token_2022::transfer` is `#[deprecated]` in favour of `transfer_checked` ([token_2022.rs L10-L13](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/spl/src/token_2022.rs));
  - it verifies the mint and decimals;
  - it is mandatory for any mint with fee, hook or pause extensions;
  - it forces the mint account into the instruction, which is exactly what the per-interaction re-check (§6) needs.
- `anchor_spl::token_interface::transfer_checked` works for both token programs. It dispatches on `CpiContext.program_id`; in Anchor 1.x `CpiContext::new` takes a `Pubkey` program id ([lang/src/context.rs L173-L205](https://github.com/otter-sec/anchor/blob/84a63f9f7112b23581816436fbcf3f6c515779f9/lang/src/context.rs)).
- After venue CPIs, `reload()` the vault accounts and account for **measured balance deltas**, not requested amounts.

### 8.4 Counterparty token-account extensions (lenders' and borrowers' own accounts)

These apply even to allowlisted mints, because they are account-level extensions the *holder* can enable:

- **CpiGuard (lender deposit).** When `lock_cpi` is on, a transfer signed by the **owner** inside a CPI fails with `CpiGuardTransferBlocked` (processor.rs L450-L460). An `Approve` inside a CPI also fails (L662). A lender with CPI Guard therefore cannot deposit through our `deposit` CPI. Workaround: the lender signs a **top-level** `Approve` to our program's PDA in the same transaction, and our program transfers as delegate. That path is allowed because the authority is the delegate, not the owner. Alternatively the UI asks the user to disable the guard. The frontend should detect `CpiGuard.lock_cpi` before building the transaction.
- **MemoTransfer (withdrawals and payouts to user accounts).** If the destination requires memos, the transfer must be preceded by a memo instruction as its **previous sibling** (processor.rs L530-L532; [memo_transfer/mod.rs](https://github.com/solana-program/token-2022/blob/23b03f88e44a9ef2587e5a08206a09eca0c8f17b/program/src/extension/memo_transfer/mod.rs)). Inside our program that means a CPI to the memo program (v1 `Memo1Uhk…`, v3 `MemoSq4g…`, or v4 `Memo4c2pN8afCj432Lb7RMVKi9PbQnnW7ewFFaV3oAH`, [memo interface](https://github.com/solana-program/memo/blob/2236266de426623a97426537849e5426c08c0a0d/interface/src/lib.rs)) immediately before the `transfer_checked` CPI. Recommendation: check the destination's TLV for `MemoTransfer.require_incoming_transfer_memos` and add a memo CPI when it is set, or always add one. Whether a CPI'd memo satisfies the sibling check was **UNVERIFIED** here; the Token-2022 code implies it (it uses `sol_get_processed_sibling_instruction` at the current stack height) and it should be tested. *M10 (T-MEMO, U17): verified on the mainnet build.* The LiteSVM suite runs the deployed Token-2022 (ProgramData `DoU57AYu…`, deploy slot 427,147,035, trimmed ELF `7ea94a02…` = OtterSec's verified build of `solana-program/token-2022@9bc02757`, v11.0.0) and spl-memo v3 (BPFLoader2, `f520eaf0…`): `tests/litesvm/tests/security.rs` `sec_7_memo_required_destination_is_served` shows short.win's memo CPI (7,928 CU) right before Token-2022's `transfer_checked` (2,487 CU) into a memo-required account, and `t_memo_control_top_level_sibling_check` shows the check is live in that ELF (a top-level `transfer_checked` fails `NoMemo` alone or with an instruction between, and passes right after a v3 or v1 memo). The web E2E validator runs the same ELFs. A mainnet transaction is M11's (TMEMO-10). Since M10 the frontend passes spl-memo v3 on every Token-2022 withdrawal (TMEMO-9); the program CPIs it only when the destination requires memos.
- **ConfidentialTransferAccount with `allow_non_confidential_credits = false`** would block credits (processor.rs L535-L538). It is only possible for mints with ConfidentialTransferMint, which are rejected.
- **Frozen accounts:** impossible for allowlisted mints (no freeze authority).

### 8.5 Pump / PumpSwap CPIs with Token-2022 mints

- **Token program per mint.**
  - `create_v2` coins: `base_token_program` = **Token-2022**.
  - Legacy coins: SPL Token.
  - Quote (wSOL) accounts use SPL Token. Docs: "`quote_token_program` … is not necessarily the same as `base_token_program`" ([BUY.md](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/docs/instructions/BUY.md), [SELL.md](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/docs/instructions/SELL.md)).

  Always pass `mint.owner`; do not pass a constant. The legacy `buy`/`sell` take a single `token_program`; `*_v2` take both.
- **ATA addresses depend on the token program.** ATA seeds are `[wallet, token_program_id, mint]` (ATA processor.rs L109-L114). In the pump IDL, `associated_bonding_curve` / `associated_base_bonding_curve` are PDA-constrained on `token_program` / `base_token_program`. Passing the wrong program yields a different address and a seeds failure. Use `get_associated_token_address_with_program_id`, never the SPL-Token-only helper.
- **Mixed programs in one instruction.** PumpSwap `buy`/`sell` take `base_token_program` and `quote_token_program` separately. On-chain we observed canonical pools with Token-2022 base and SPL Token wSOL quote (for example pool `2wWnS6pq…`, tx `4DAUibrh…`). We also observed **non-canonical pools where wSOL is the *base*** and a Token-2022 mint is the quote (pool `7t3BcgUj…`, tx `2CveWtSY…`) (RPC). Pool selection must not assume the orientation. Derive or verify the canonical migration pool; that is the venue research topic.
- **The pump `user` must be a system-owned, zero-data PDA for buys.** Pump pays SOL from `user` via a System Program transfer. The System Program rejects any `from` that carries data: "Transfer: `from` must not carry data" ([agave system_processor.rs L223-L227](https://github.com/anza-xyz/agave/blob/f77165963c709934e8ea8564187c00f081b597d1/programs/system/src/system_processor.rs)). The signer therefore cannot be a `Market` or `Position` data account. Use a dedicated PDA that is never allocated and signs with `invoke_signed`, and keep its balance at 0 or at least the rent minimum.
- **User token account passed to pump.** The pump and PumpSwap IDLs put **no** seeds constraint on `associated_user` / `associated_base_user` / `user_base_token_account`. The docs describe it as "User's associated token account". Whether pump enforces the ATA in code is **UNVERIFIED**; decide via a mainnet-fork or LiteSVM test. If it is enforced, route borrowed tokens through `ATA(trader_pda, mint, token_program)` (created idempotently) rather than handing the pool vault to pump.
- **Token-2022 metadata-only mints need no extra accounts** in pump transfers. A hook mint would need extra accounts that pump does not forward, so its liquidations would fail. That is a concrete reason the filter must run before any curve CPI.
- **PumpSwap uses wSOL token accounts for the quote side** (`user_quote_token_account`, `quote_token_program` = SPL Token). For native-SOL-paired bonding-curve coins, pump moves native SOL and only seed-constrains the quote ATAs ([README](https://github.com/pump-fun/pump-public-docs/blob/81091419e4457566469d4e2a27f64ed84d42419c/README.md)). Wrapping and unwrapping belong to the venue adapter design.

---

## 9. Rust code sketches (Anchor 1.2.0, `anchor-spl 1.2.0` → `spl-token-2022-interface 2.1.x`)

These are sketches. They were not compiled here, because the toolchain is being installed by someone else.

### 9.1 Dependencies

```toml
[dependencies]
anchor-lang = "1.2.0"
anchor-spl  = { version = "1.2.0", default-features = false, features = ["token", "token_2022", "associated_token"] }
# Parsing goes through anchor_spl::token_2022::spl_token_2022 (= spl-token-2022-interface 2.x).
# Do NOT add a second token-2022 interface just for the filter: unknown types (e.g. PermissionedBurn=28)
# already fail closed.

[dev-dependencies]
# Only to keep the classification table in sync with the newest program (see 9.5).
spl-token-2022-interface-latest = { package = "spl-token-2022-interface", version = "=3.1.2" }
```

### 9.2 The filter

```rust
use anchor_lang::prelude::*;
use anchor_spl::token::spl_token;                 // spl-token-interface
use anchor_spl::token_2022::spl_token_2022 as t22; // spl-token-2022-interface
use t22::extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions};

#[error_code]
pub enum MintFilterError {
    #[msg("Mint is not owned by SPL Token or Token-2022")]
    UnsupportedTokenProgram,
    #[msg("Native (wrapped SOL) mints cannot be listed")]
    NativeMint,
    #[msg("Mint account data is malformed or uninitialized")]
    MalformedMint,
    #[msg("Mint has an active freeze authority")]
    FreezeAuthority,
    #[msg("Mint carries a Token-2022 extension that is not allowlisted (or unknown)")]
    DisallowedExtension,
}

/// v1 allowlist: extensions that cannot freeze, seize, block, tax or rescale balances.
/// Everything else, including types unknown to the linked interface crate, is rejected.
#[inline]
fn is_allowlisted(t: ExtensionType) -> bool {
    matches!(
        t,
        ExtensionType::MetadataPointer
            | ExtensionType::TokenMetadata
            | ExtensionType::GroupPointer
            | ExtensionType::TokenGroup
            | ExtensionType::GroupMemberPointer
            | ExtensionType::TokenGroupMember
    )
}

/// Spec §3 mint safety filter. Pure read, no allocation except the small Vec of types.
pub fn check_mint_safety(mint: &AccountInfo) -> Result<()> {
    let owner = *mint.owner;
    let is_legacy = owner == spl_token::ID;
    require!(is_legacy || owner == t22::ID, MintFilterError::UnsupportedTokenProgram);
    require!(
        mint.key() != spl_token::native_mint::ID && mint.key() != t22::native_mint::ID,
        MintFilterError::NativeMint
    );

    let data = mint.try_borrow_data()?;
    if is_legacy {
        // SPL Token mints are exactly 82 bytes and have no TLV.
        require!(data.len() == t22::state::Mint::LEN, MintFilterError::MalformedMint);
    }
    // Same base layout for both programs. unpack(): rejects len==355, non-zero padding,
    // wrong AccountType, and uninitialized mints.
    let state = StateWithExtensions::<t22::state::Mint>::unpack(&data)
        .map_err(|_| error!(MintFilterError::MalformedMint))?;

    require!(state.base.freeze_authority.is_none(), MintFilterError::FreezeAuthority);

    // Walks every TLV entry; Err on unknown discriminant (fail closed).
    let types = state
        .get_extension_types()
        .map_err(|_| error!(MintFilterError::DisallowedExtension))?;
    for t in types {
        require!(is_allowlisted(t), MintFilterError::DisallowedExtension);
    }
    Ok(())
}
```

Notes:
- Do not gate on `data.len() > 82` alone. A Token-2022 mint may be exactly 82 bytes with no extensions; that is valid and allowed.
- `mint_authority` is deliberately **not** checked. The spec does not ask for it. Whether it should affect the tier is an open question (§10).

### 9.3 Market creation

```rust
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

#[derive(Accounts)]
pub struct CreateMarket<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(mint::token_program = token_program)] // mint.owner == token_program
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(init, payer = payer, space = 8 + Market::INIT_SPACE,
              seeds = [b"market", mint.key().as_ref()], bump)]
    pub market: Account<'info, Market>,

    // PDA-addressed vault (not an ATA): cannot be pre-created by a griefer (§8.2).
    #[account(init, payer = payer,
              seeds = [b"pool_vault", market.key().as_ref()], bump,
              token::mint = mint, token::authority = market, token::token_program = token_program)]
    pub pool_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn create_market(ctx: Context<CreateMarket>) -> Result<()> {
    check_mint_safety(&ctx.accounts.mint.to_account_info())?; // hard filter
    let m = &mut ctx.accounts.market;
    m.mint = ctx.accounts.mint.key();
    m.token_program = ctx.accounts.token_program.key(); // pinned for all later instructions
    m.decimals = ctx.accounts.mint.decimals;
    m.pool_vault = ctx.accounts.pool_vault.key();
    // ... venue detection via bonding-curve PDA owner, tier, etc.
    Ok(())
}
```

Caveat: Anchor's `token::` init for `pool_vault` calls `get_extension_types()?` to size the account (§8.1). For a mint with an unknown extension, the transaction therefore fails **before** the handler runs, with a generic error instead of `DisallowedExtension`. It still fails closed. For better error messages, run `check_mint_safety` in a `constraint =` on `mint` or in a preceding instruction. The evaluation order of Anchor constraints relative to `init` fields is **UNVERIFIED** and should not be relied on for safety.

### 9.4 Per-interaction guard and token movement

```rust
use anchor_spl::token_interface::{self, TransferChecked};

pub enum PathKind { Entry, Exit }

/// Call at the top of every instruction that touches the market's tokens.
pub fn guard_market_mint(
    market: &Market,
    mint: &InterfaceAccount<Mint>,
    token_program: &Interface<TokenInterface>,
    path: PathKind,
) -> Result<()> {
    require_keys_eq!(mint.key(), market.mint);
    require_keys_eq!(*mint.to_account_info().owner, market.token_program);
    require_keys_eq!(token_program.key(), market.token_program);
    match path {
        PathKind::Entry => check_mint_safety(&mint.to_account_info()),
        PathKind::Exit => {
            // Spec §11: never block exits on the filter alone. Emit an event for keepers/indexer.
            if check_mint_safety(&mint.to_account_info()).is_err() {
                emit!(MintSafetyTripped { market: market.key(), mint: market.mint });
            }
            Ok(())
        }
    }
}

pub fn vault_transfer<'info>(
    token_program: &Interface<'info, TokenInterface>,
    from: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    to: &InterfaceAccount<'info, TokenAccount>,
    authority: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(), // Anchor 1.x: program id, not AccountInfo
            TransferChecked {
                from: from.to_account_info(),
                mint: mint.to_account_info(),
                to: to.to_account_info(),
                authority,
            },
            signer_seeds,
        ),
        amount,
        mint.decimals,
    )
}
// After any venue CPI: from.reload()?; to.reload()?; and book the measured deltas.
```

### 9.5 CI guard: keep the classification in sync with the newest Token-2022

```rust
#[cfg(test)]
mod tests {
    use spl_token_2022_interface_latest::extension::ExtensionType as Latest;

    /// Every discriminant we have *reviewed* (0..=28 as of token-2022 main@23b03f8 / program v11.x).
    const REVIEWED: core::ops::RangeInclusive<u16> = 0..=28;

    #[test]
    fn every_token2022_extension_is_reviewed() {
        for raw in 0u16..=1024 {
            if Latest::try_from(raw).is_ok() {
                assert!(
                    REVIEWED.contains(&raw),
                    "New Token-2022 extension type {raw}: classify it in docs/research/mint-safety-filter.md §4"
                );
            }
        }
    }
}
```

Bumping `spl-token-2022-interface-latest` in the dev-dependencies (Dependabot or a manual bump) then forces a review whenever a new extension ships. Also add LiteSVM or Mollusk fixtures: one mint per extension type, built with the real Token-2022 `.so`. Assert that exactly the six allowlisted types pass, and that a freeze-authority SPL mint, a clean SPL mint, and a pump-shaped Token-2022 mint (MetadataPointer→self plus TokenMetadata) behave as expected.

---

## 10. Design implications for short.win

1. **Filter definition (implements §3):**
   - owner ∈ {SPL Token, Token-2022};
   - not `So111…112` or `9pan9bMn5HatX4EJdBwg9VgCa7Uz5HL8N1m5D3NdXejP` (both native mints otherwise pass, per RPC);
   - `freeze_authority == None`;
   - every mint extension ∈ {MetadataPointer, TokenMetadata, GroupPointer, TokenGroup, GroupMemberPointer, TokenGroupMember};
   - any parse error means reject.

   **Decision log entry needed:** the spec names four rejections. The additional ones (Pausable, NonTransferable, DefaultAccountState, MintCloseAuthority, confidential extensions, InterestBearing, ScaledUiAmount, PermissionedBurn, unknown types) follow the spec's stated purpose, but they should be recorded as "✅ Delegated, proposed".
2. **Presence-based rejection for the spec's four items.** Do not add "inert configuration" exceptions (for example a 0-bps fee with no authority) in v1. If the owner wants them later, add each as its own decision-log item.
3. **Market state:** store `mint`, `token_program`, `decimals` and the vault addresses. Every instruction checks `mint.key()`, `mint.owner` and `token_program` against them.
4. **Re-check everywhere, but only enforce on entry.** Hard-fail on create, deposit and open. On withdraw, close, liquidate, settle and expire, emit `MintSafetyTripped` and continue (spec §11). **Decision needed:** a permissionless `flag_unsafe_mint` instruction that sets the market's paused-opens flag when the filter fails. This is new state, not an economic parameter, but spec §15 asks for it to be logged.
5. **Always use `transfer_checked`** through `anchor_spl::token_interface`, and book balance deltas after venue CPIs.
6. **Vaults are PDA-addressed token accounts**, created by our program. Do not use `init` on ATAs of deterministic PDAs, because pre-creation would block listing (§8.2). Calling `initialize_immutable_owner` on Token-2022 vaults is optional hardening.
7. **Compute account sizes and rent dynamically.** Current mainnet rent is lower than the historical values; a Token-2022 ATA is 170 bytes.
8. **Venue adapters:**
   - pass `mint.owner` as the base token program and SPL Token for wSOL;
   - derive ATAs with the program id;
   - the pump `user` is a system-owned, never-allocated PDA;
   - test whether pump requires the user's ATA;
   - do not assume PumpSwap base/quote orientation.
9. **Identify pump coins by the bonding-curve PDA, not the mint suffix.** 31% of fresh pump mints lack the `pump` suffix.
10. **Lender UX:**
    - Detect CPI Guard on the source account and use the top-level `Approve` plus delegate-transfer path, or ask the user to disable the guard.
    - For payouts to accounts that require memos, add a memo CPI before the transfer.
    - Show the filter verdict and reasons on the market page. The frontend and indexer should run the same byte-level check (§3) before offering "Create market".
11. **Identity is the mint address.** TokenMetadata name and symbol are mutable whenever an update authority exists. Pump's are `None`, but other mints may differ, so never key markets by name or symbol.
12. **Dependency pinning:** `anchor-spl 1.2.0` with interface 2.1.x is safe for this filter, because unknown types fail closed. Add the CI test in §9.5 so new Token-2022 extensions force a review.
13. **Systemic risk to disclose:** Token-2022 (upgrade authority `AeLmXC…`), pump and pump-amm are upgradeable programs. The filter cannot protect against upgrades that change semantics. That belongs in the §12 risk table (R5-like).

### Open questions for the owner

- Should an active `mint_authority` (unlimited inflation) affect the tier or the listing? It is not in the spec. Pump coins have none.
- Should "inert" variants be allowed later (TransferHook with no authority and no program; zero fee with no authority; MintCloseAuthority=None; DefaultAccountState=Initialized; PermissionedBurn)? The recommendation is no for v1.
- Should the `flag_unsafe_mint` / auto-pause instruction from implication 4 be added?

---

## 11. Unverified items

- Whether pump or PumpSwap enforce that the user's token account is the ATA of `user` (the IDLs have no seeds constraint; the docs say "associated token account").
- Whether pump still accepts the legacy SPL-Token `create` on mainnet (0 of 123 recent creates used it).
- That every historical legacy pump mint has `freeze_authority = None`. Checked on a sample of one; the filter checks each mint regardless.
- ~~Whether a memo emitted by CPI satisfies Token-2022's `MemoTransfer` previous-sibling check inside a CPI.~~ Verified on the mainnet Token-2022 and spl-memo v3 ELFs in M10 (§8.4); a mainnet transaction is pending (M11, TMEMO-10).
- The CU cost of `check_mint_safety`, and whether the §9 sketches compile unchanged against `anchor-lang/anchor-spl 1.2.0` (not built here).
- Whether a direct `spl-token-2022-interface 3.x` dependency resolves cleanly next to `anchor-spl 1.2.0`.
- The evaluation order of Anchor account constraints relative to `init` fields.

---

## 12. Sources

- Token-2022 source, `main` @ `23b03f88e44a9ef2587e5a08206a09eca0c8f17b` (2026-09-26): https://github.com/solana-program/token-2022/tree/23b03f88e44a9ef2587e5a08206a09eca0c8f17b. Files: `interface/src/extension/mod.rs`, `interface/src/state.rs`, `interface/src/extension/*`, `program/src/processor.rs`, `program/src/extension/*/processor.rs`, `program/src/extension/reallocate.rs`.
- Token-2022 interface 2.1.0 (commit `4fae3ba6516db0c6e4b2fe698ba2cfdb92901b93`): https://github.com/solana-program/token-2022/tree/interface%40v2.1.0
- Token-2022 releases: https://github.com/solana-program/token-2022/releases (program@v11.0.0, program@v11.1.0, interface@v3.1.2).
- p-token release: https://github.com/solana-program/token/releases/tag/p-token%40v1.0.0
- Anchor v1.2.0 (commit `84a63f9f7112b23581816436fbcf3f6c515779f9`): https://github.com/otter-sec/anchor/tree/84a63f9f7112b23581816436fbcf3f6c515779f9. Files: `spl/Cargo.toml`, `spl/src/{token.rs, token_2022.rs, token_interface.rs, associated_token.rs}`, `lang/src/context.rs`, `lang/syn/src/codegen/accounts/constraints.rs`, `lang/syn/src/parser/accounts/constraints.rs`.
- Associated Token Account program @ `4e58a1098c8ce9bb256c0aa8d768e34d690918e9`: https://github.com/solana-program/associated-token-account/blob/4e58a1098c8ce9bb256c0aa8d768e34d690918e9/program/src/processor.rs
- Memo interface @ `2236266de426623a97426537849e5426c08c0a0d`: https://github.com/solana-program/memo/blob/2236266de426623a97426537849e5426c08c0a0d/interface/src/lib.rs
- Agave system program @ `f77165963c709934e8ea8564187c00f081b597d1`: https://github.com/anza-xyz/agave/blob/f77165963c709934e8ea8564187c00f081b597d1/programs/system/src/system_processor.rs
- pump public docs @ `81091419e4457566469d4e2a27f64ed84d42419c`: https://github.com/pump-fun/pump-public-docs/tree/81091419e4457566469d4e2a27f64ed84d42419c (README, `docs/instructions/{COIN_CREATION,BUY,SELL}.md`, `docs/PUMP_PROGRAM_README.md`, `docs/CPI_README.md`, `idl/pump.json`, `idl/pump_amm.json`).
- crates.io API (`https://crates.io/api/v1/crates/<name>`), queried 2026-09-27: anchor-lang, anchor-spl, spl-token-2022-interface, spl-token-2022, spl-token-interface, spl-token, spl-associated-token-account(-interface), pinocchio-token-2022.
- Solana mainnet JSON-RPC (`api.mainnet-beta.solana.com`, `solana-rpc.publicnode.com`), 2026-09-26/27, around slot 450.8M. Accounts and transactions cited inline: program and programdata accounts in §2, mints and transactions in §7, PYUSD `2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo`, USDC `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`, native mints, and PumpSwap pools `2wWnS6pqU4jf5sgu6pd515fUq8pN7cKwq4Ry864FMPbh`, `9UTi1bAwVoSooNHeTFR2MPt5MBRECVVPCSfayAj54XTx`, `7t3BcgUjoU1eSoXPrxPfVftr1hJ6k6hDmeLsyGgKRegg`.
