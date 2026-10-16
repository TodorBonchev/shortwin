# `mock_cpamm` — interface (binding for M2)

**TEST-ONLY. Never deployed** (ARCHITECTURE §1.2, §8.2, §9 M2; A49). A constant-product pool of
a base token (SPL Token or Token-2022) against WSOL (SPL Token), fee on the input, with the
knobs M2's venue tests need. short.win knows it only as `VenueKind::Mock` behind its
`test-venue` feature, which release builds never enable.

| | |
|---|---|
| Program id | `BPAZFPkmVtYEWEZHCzFUns7oybh1VWfAvmu3ofM2mQi7` (`declare_id!` in `src/lib.rs`, `Anchor.toml`) |
| Keypair | `tests/mock_cpamm/keys/mock_cpamm-keypair.json`, **not committed** (`*-keypair.json` is gitignored; the build takes the id from `declare_id!`, so a checkout without it builds the same program; copy it to `target/deploy/` to silence Anchor's "Program ID mismatch" warning) |
| Build | `anchor build --tools-version v1.57 --arch v3` builds it with short.win (Anchor workspace member) → `target/deploy/mock_cpamm.so`, `target/idl/mock_cpamm.json`. The test-venue build `anchor build … -- --features test-venue` also works: the crate declares a no-op `test-venue` feature |
| LiteSVM | `Env::load_mock_cpamm()` (`tests/litesvm/src/mock.rs`); `MOCK_CPAMM_SO=/path/to/mock_cpamm.so` overrides the path |
| Framework | Anchor 1.2.0; errors from 6000; Anchor 1.x also refuses a writable account passed twice among the named accounts (`ConstraintDuplicateMutableAccount`, 2040) |

## PDAs and accounts

| Account | Address | Owner / type |
|---|---|---|
| Pool | `["pool", base_mint, quote_mint]` (quote = WSOL, so one pool per base mint) | mock program; **384 bytes**; discriminator `3ac4d0f11fd8f330` = `sha256("account:MockPool")[..8]` (distinct from every mainnet `Pool`) |
| `pool_authority` | `["authority"]` (one for all pools) | never created; owns every vault; signs every output transfer |
| Base vault | `["vault", pool, base_mint]` | token account of `base_mint` under the base mint's program; owner `pool_authority`; created by `initialize_pool` |
| Quote vault | `["vault", pool, WSOL]` | SPL Token WSOL account; owner `pool_authority` |

WSOL is `So11111111111111111111111111111111111111112` under SPL Token
`Tokenkeg…`. Only WSOL can be the quote mint.

## Pool layout

Absolute byte offsets from the start of the account data (the discriminator included), all
integers little-endian, `bool` = one byte 0/1. The account never changes size. The constants
are `mock_cpamm::state::OFF_*` (`state::tests::offsets_are_contiguous` pins the table).

| Offset | Size | Field | Notes |
|---:|---:|---|---|
| 0 | 8 | discriminator | `3a c4 d0 f1 1f d8 f3 30` |
| 8 | 1 | `layout_version` | 0 or 1 (below) |
| 9 | 1 | `disabled` | swaps fail while 1 |
| 10 | 4 | `fee_ppm` u32 | fee on the input, `< 1,000,000` |
| 14 | 8 | `over_delivery` u64 | extra output units of every exact-out swap |
| 22 | 8 | `protocol_fee_base` u64 | **layout 0 only**; zero under layout 1 |
| 30 | 8 | `protocol_fee_quote` u64 | **layout 0 only**; zero under layout 1 |
| 38 | 1 | `required_extras` count | 0..=4 |
| 39 | 128 | `required_extras` `[Pubkey; 4]` | unused slots zero |
| 167 | 32 | `base_vault` | |
| 199 | 32 | `quote_vault` | |
| 231 | 1 | `bump` | pool PDA bump |
| 232 | 32 | `admin` | the pool creator; signs `set_knobs`, `remove_liquidity` |
| 264 | 32 | `base_mint` | |
| 296 | 32 | `quote_mint` | always WSOL |
| 328 | 32 | `base_token_program` | SPL Token or Token-2022 |
| 360 | 1 | `authority_bump` | bump of `["authority"]` |
| 361 | 1 | `base_vault_bump` | |
| 362 | 1 | `quote_vault_bump` | |
| 363 | 1 | reserved | zero |
| 364 | 8 | `protocol_fee_base` u64 | **layout 1 only**; zero under layout 0 |
| 372 | 8 | `protocol_fee_quote` u64 | **layout 1 only**; zero under layout 0 |
| 380 | 4 | reserved | zero |

**Pricing reserves** (as Raydium CPMM's `vault − counters`, ARCH §4.1):
`X = base_vault.amount − protocol_fee_base` (token), `Y = quote_vault.amount −
protocol_fee_quote` (SOL). Real SOL `R = Y`. A vault below its counter makes every swap fail
(`InsufficientVault`), the analogue of CPMM's `vault_i < counters_i`.

**Layout versions** (a simulated venue-state layout change; ARCH §5.3 "Venue interface
breakage", V1-8, T-S3-BROKEN). `set_knobs(layout_version = 1)` moves both counters from
22/30 to 364/372 and zeroes 22..38; `layout_version = 0` moves them back. The venue itself
keeps working under either layout (swaps, liquidity). A reader that still used the version-0
offsets would see zero counters and price on the raw vault balances, so **a reader must
require `layout_version == 0` and treat anything else as unparseable** (reader failure ⇒
`NotExecutable`, A63), the way a real venue's reader fails on a changed length or
discriminator.

### Reader contract (for `shortwin-venue-state`)

Owner `== BPAZFPkm…`, discriminator `== 3ac4d0f11fd8f330`, length-tolerant reads per ARCH §4.1,
`layout_version == 0` (else failure). Fields a short.win reader may use: `disabled` (9),
`fee_ppm` (10), the two version-0 counters (22, 30), `base_vault` (167), `quote_vault` (199),
`base_mint` (264), `quote_mint` (296), `base_token_program` (328). The knobs
`over_delivery` and `required_extras` model venue *behaviour* that short.win cannot observe
on a real venue: the program must not read them (it learns about over-delivery from the
measured deltas, and about new trailing accounts from the Emergency extras set, O6).

A natural venue predicate for `VenueKind::Mock` (ARCH §5.3 table): pool parses (as above)
`∧ !disabled ∧ base_vault ≥ protocol_fee_base ∧ quote_vault ≥ protocol_fee_quote ∧ X > 0 ∧ Y > 0`.

## Instructions

Discriminators are Anchor's `sha256("global:<name>")[..8]`; arguments are Borsh (`bool` 1
byte, integers LE, `Vec<Pubkey>` = u32 length + 32 bytes each). S = signer, W = writable.

| Instruction | Discriminator | Arguments |
|---|---|---|
| `initialize_pool` | `5fb40aac54aee828` | `fee_ppm: u32` |
| `add_liquidity` | `b59d59438fb63448` | `base: u64, quote: u64` |
| `swap_exact_in` | `68688356a1bdb4d8` | `amount_in: u64, min_out: u64` |
| `swap_exact_out` | `fa49652126cf4bb8` | `max_in: u64, amount_out: u64` |
| `set_knobs` | `4c733da062186c4f` | `disabled: bool, fee_ppm: u32, layout_version: u8, over_delivery: u64, required_extras: Vec<Pubkey>` |
| `remove_liquidity` ¹ | `5055d14818ceb16c` | `base: u64, quote: u64` |

¹ An addition to the M2 interface (admin-only LP pull for the T-BOX `L` variant and LP-pull
scenarios); nothing else depends on it.

### Swap accounts (`swap_exact_in`, `swap_exact_out`) — order is binding

| # | Account | Flags | Checked |
|---:|---|---|---|
| 0 | `authority` | S | owner **or delegate** of `source` (delegate: `source.delegate == authority ∧ delegated_amount ≥ amount_in`), else `SourceAuthority` |
| 1 | `pool` | W | mock-owned `MockPool` (Anchor owner + discriminator) |
| 2 | `pool_authority` | | `["authority"]` with the pool's recorded bump (`ConstraintSeeds`) |
| 3 | `source` | W | token account of `input_mint` under `input_token_program` (Anchor `token::` constraints) |
| 4 | `destination` | W | token account of `output_mint` under `output_token_program`; **owner not checked** (as CPMM and AMM v4) |
| 5 | `input_vault` | W | `pool.base_vault` ⇒ base in; else must be `pool.quote_vault` (`VaultMismatch`) |
| 6 | `output_vault` | W | the pool's other vault (`VaultMismatch`) |
| 7 | `input_mint` | | `== input_vault.mint` (`MintMismatch`); owned by `input_token_program` |
| 8 | `output_mint` | | `== output_vault.mint` (`MintMismatch`); owned by `output_token_program` |
| 9 | `input_token_program` | | SPL Token or Token-2022 (Anchor `Interface`) |
| 10 | `output_token_program` | | SPL Token or Token-2022 |
| 11… | remaining accounts | any | **exactly** `pool.required_extras`: same count, same keys, same order; flags free (`ExtrasMismatch`) |

**Direction** = whether `input_vault` is the base vault: base in (short.win's open leg: sell
tokens for WSOL) or quote in (the exit legs: buy tokens with WSOL). `source` and
`destination` may not be the pool's vaults (`VaultAsUserAccount`; Anchor's duplicate-account
check usually fires first).

**Order of checks**: Anchor account constraints, then `disabled` (`PoolDisabled`), vaults,
mints, extras, reserves (`InsufficientVault`), the arithmetic and the slippage bound, the
delegate check, then two `transfer_checked`: `source → input_vault` for `amount_in` signed by
`authority` (a CPI caller's PDA signature passes through), and `output_vault → destination`
for the output signed by `pool_authority`. Finally the input side's counter grows by the
protocol fee.

**Trace and CU**: one swap is 3 instruction-trace entries (the mock + its 2 `transfer_checked`),
as CPMM's; ≈ 18.4 k CU with an SPL base, ≈ 20.4 k with a Token-2022 base
(`tests/litesvm/tests/mock_cpamm.rs::swap_cost_and_trace`, bound 30 k).

### Other account lists

| Instruction | Accounts (in order) |
|---|---|
| `initialize_pool` | 0 `admin` (S, W; pays, becomes admin), 1 `base_mint` (under `base_token_program`), 2 `quote_mint` (== WSOL, `QuoteNotWsol`), 3 `pool` (W, init), 4 `pool_authority`, 5 `base_vault` (W, init), 6 `quote_vault` (W, init), 7 `base_token_program`, 8 `quote_token_program` (== SPL Token, `TokenProgramMismatch`), 9 System |
| `add_liquidity` | 0 `depositor` (S; owner or delegate of both sources), 1 `pool`, 2 `base_source` (W), 3 `quote_source` (W), 4 `base_vault` (W), 5 `quote_vault` (W), 6 `base_mint`, 7 `quote_mint`, 8 `base_token_program`, 9 `quote_token_program` |
| `remove_liquidity` | 0 `admin` (S), 1 `pool`, 2 `pool_authority`, 3 `base_vault` (W), 4 `quote_vault` (W), 5 `base_destination` (W), 6 `quote_destination` (W), 7 `base_mint`, 8 `quote_mint`, 9 `base_token_program`, 10 `quote_token_program` |
| `set_knobs` | 0 `admin` (S; `Unauthorized` unless the pool's admin), 1 `pool` (W) |

`add_liquidity` is permissionless and mints no LP token: liquidity is a donation to the
curve (an "unlocked LP add"). Either amount may be 0, not both (`ZeroAmount`).
`remove_liquidity` pays out at most the pricing reserves (`InsufficientLiquidity`).

## Swap arithmetic (`src/math.rs`)

Written as a venue writes it (after `raydium-cp-swap` `curve/calculator.rs`, `curve/fees.rs`),
not by calling `shortwin-math`. `D = 1,000,000`, `f = fee_ppm`, `p = PROTOCOL_FEE_RATE =
120,000` (12 % of the trade fee, like CPMM config 0's protocol share). `R_in`, `R_out` are the
pricing reserves of the input and output side.

- **Exact in** `a`: `trade = ⌈a·f/D⌉`, `net = a − trade`, `out = ⌊R_out·net/(R_in + net)⌋`;
  requires `a > 0`, `R_in, R_out > 0`, `out > 0` (`ZeroAmount`, `EmptyPool`, `ZeroOutput`) and
  `out ≥ min_out` (`SlippageMinOut`).
- **Exact out** `q`: requires `0 < q < R_out` (`InsufficientLiquidity`);
  `swapped = ⌈R_in·q/(R_out − q)⌉`, `amount_in = swapped` if `f = 0`, else
  `⌈swapped·D/(D − f)⌉`; `trade = amount_in − swapped`; requires `amount_in ≤ max_in`
  (`SlippageMaxIn`) and that it fits u64 (`MathOverflow`). Delivers `q + over_delivery`,
  which must stay below `R_out` (`InsufficientLiquidity`).
- **Protocol fee** `⌊trade·p/D⌋` moves from the input side's reserve into its counter.

### short.win's view (binding for the `VenueKind::Mock` quote)

Use `shortwin-math` `quote::cpmm` with

```rust
CpmmFees { trade: fee_ppm as u64, creator: 0, creator_on_input: false, protocol: 120_000, fund: 0 }
```

- open leg (base in, exact in): `cpmm::sell_exact_in(Q, X, Y, &fees)` → `proceeds`;
- buy leg (quote in, exact out): `cpmm::buy_exact_out(Q, Y, X, &fees)` → `cost`
  (arguments: input reserve, output reserve);
- one-way fee `φ = fee_ppm` ppm exactly.

The execution equals the quote **exactly** — output, cost and both post-trade pricing reserves
(`token_after`, `sol_after`) — and the swap succeeds iff the quote is defined:
`src/math.rs` proptests (the full u64 range) and
`tests/litesvm/tests/mock_cpamm.rs::execution_equals_the_cpmm_quote` (the SBF program on
LiteSVM, both directions, both kinds, SPL and Token-2022 bases, with counters and fee changes).
`creator_on_input: true` gives the same quotes for `fee_ppm > 0` but is undefined at
`fee_ppm = 0` (CPMM's fee split divides by `t + c`), hence `false`. `over_delivery > 0`
breaks the equality on purpose (exact out only).

## Knobs (`set_knobs`, admin only, every knob at once)

| Knob | Effect | Simulates |
|---|---|---|
| `disabled` | every swap fails `PoolDisabled`; liquidity still moves | an admin halt (CPMM status bit, AMM v4 status) |
| `fee_ppm` | the next swap's fee; `≥ 10⁶` refused (`BadFee`) | a fee hike |
| `layout_version` | 0 ↔ 1 moves the counters (above); other values `BadLayoutVersion` | a venue-state layout change (V1-8) |
| `over_delivery` | exact-out swaps deliver `amount_out + over_delivery`; exact in unaffected; the input is still the quote's | a venue that over-delivers on a buy (V2-10, A62) |
| `required_extras` | up to 4 keys (`TooManyExtras`); swaps need exactly these trailing accounts | an interface change adding accounts (C0-4, T-REPLAY-PDA with a per-mint PDA key) |

## Errors

| Code | Name | Code | Name |
|---:|---|---:|---|
| 6000 | `BadPoolData` | 6011 | `ExtrasMismatch` |
| 6001 | `BadLayoutVersion` | 6012 | `SourceAuthority` |
| 6002 | `BadFee` | 6013 | `InsufficientVault` |
| 6003 | `TooManyExtras` | 6014 | `EmptyPool` |
| 6004 | `QuoteNotWsol` | 6015 | `ZeroAmount` |
| 6005 | `SameMint` | 6016 | `ZeroOutput` |
| 6006 | `Unauthorized` | 6017 | `InsufficientLiquidity` |
| 6007 | `PoolDisabled` | 6018 | `SlippageMinOut` |
| 6008 | `VaultMismatch` | 6019 | `SlippageMaxIn` |
| 6009 | `MintMismatch` | 6020 | `MathOverflow` |
| 6010 | `TokenProgramMismatch` | 6021 | `VaultAsUserAccount` |

Codes are append-only.

## Differences from a real CPMM (by design)

No LP token, no creator or fund fee, no `open_time` / status word (only `disabled`), no
observation account, no AMM config account, one pool per base mint. Transfer-fee and
transfer-hook Token-2022 mints are not modelled (the SPEC §3 filter refuses them anyway):
the mock assumes the vault receives `amount_in`.

## LiteSVM helpers (`tests/litesvm/src/mock.rs`)

`Env::load_mock_cpamm`; `pool_address`, `authority`, `vault_address`; `MockPool` (keys);
`new_pool` (a new SPL or Token-2022 mint and its pool), `create_pool` (any existing base mint,
e.g. a Token-2022 fixture of `fixtures::mints`); `seed` / `add_liquidity`, `remove_liquidity`;
`set_knobs`, `update_knobs`, `knobs`; `state` (decoded), `reserves` (vaults, counters, `X`, `Y`,
layout-aware); `wsol_account`, `wrap`, `base_account`, `trader`; `swap_accounts`,
`swap_exact_in_ix`, `swap_exact_out_ix` (with remaining accounts); `cpmm_fees`,
`quote_exact_in`, `quote_exact_out`; `assert_mock_err`.
