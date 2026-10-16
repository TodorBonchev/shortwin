# Third-party notices

This repository includes or adapts the third-party material below. [`REUSE.toml`](REUSE.toml)
gives each file's licence, and the licence texts are in [`LICENSES/`](LICENSES/). Dependencies
that Cargo and pnpm download are not part of the repository; they come under their own licences
(all permissive; `deny.toml` enforces the Rust allow-list).

## Program binaries (`tests/fixtures/programs/`)

Unmodified copies of programs deployed on Solana mainnet, each stored with its loader header
([`tests/fixtures/README.md`](tests/fixtures/README.md)), used as test fixtures.

| File | Program | Source | Licence |
|---|---|---|---|
| `raydium_amm_v4.so` | Raydium AMM v4 (`675kPX9M…`) | [raydium-io/raydium-amm](https://github.com/raydium-io/raydium-amm) @ `d26944b` | Apache-2.0 |
| `raydium_cpmm.so` | Raydium CPMM (`CPMMoo8L…`), the 445,763,504 deploy | [raydium-io/raydium-cp-swap](https://github.com/raydium-io/raydium-cp-swap) @ `59fb845` | Apache-2.0 |
| `raydium_cpmm_2026_10.so` | Raydium CPMM, the 452,376,460 deploy | raydium-cp-swap @ `bdeee23` (reproduced byte for byte) | Apache-2.0 |
| `token_2022.so` | SPL Token-2022 (`TokenzQd…`) | [solana-program/token-2022](https://github.com/solana-program/token-2022) @ `9bc02757` | Apache-2.0 |
| `spl_memo_v3.so` | SPL Memo v3 (`MemoSq4g…`) | [solana-program/memo](https://github.com/solana-program/memo) | Apache-2.0 |
| `squads_v4.so` | Squads Multisig v4 (`SQDS4ep6…`) | [Squads-Protocol/v4](https://github.com/Squads-Protocol/v4), probably @ `64af733` (unverified) | at `64af733`, the Business Source License 1.1 of Selimor Investments Limited ([text](LICENSES/LicenseRef-Squads-BUSL-1.1.txt)), which permits non-production use; the repository's main branch is AGPL-3.0 today |
| `pump*.so` | pump.fun's bonding curve (`6EF8rrec…`), PumpSwap (`pAMMBay6…`) and pump-fees (`pfeeUxB6…`) | no public source | **not included**: no licence permits redistributing them |

None of the Apache-2.0 sources above has a NOTICE file.

## Squads IDL (`tests/fixtures/idl/squads_multisig_program.json`)

The Squads Multisig v4 IDL 2.1.0. It is byte-identical (sha256 `cb9a0a29…9f9b9b`) to
`idl/squads_multisig_program.json` in the npm package `@sqds/multisig` 2.1.4, whose
`package.json` declares the MIT licence ([text](LICENSES/MIT.txt)). The package ships no
copyright line; the Squads v4 repository names Selimor Investments Limited as its licensor.

## Code adapted from Raydium (Apache-2.0)

These files reimplement Raydium's quote arithmetic so that short.win prices its legs exactly as
the venues do. Each file's header names its source. The adapted parts stay under Apache-2.0; the
rest of each file is under BUSL-1.1.

- `crates/shortwin-math/src/quote/cpmm.rs`: from raydium-cp-swap @ `59fb845`
  (`curve/calculator.rs`, `curve/fees.rs`, `instructions/swap_base_{input,output}.rs`).
- `crates/shortwin-math/src/quote/ray_v4.rs`: from raydium-amm @ `d26944b` (`processor.rs`
  `process_swap_base_{in,out}_v2`, `math.rs` `swap_token_amount_base_{in,out}`).
- `crates/shortwin-math/tests/vectors/quotes/gen_port.py`: a port of raydium-cp-swap @ `59fb845`
  (`calculator.rs`, `fees.rs`) that generates test vectors.

## Public chain data

`tests/fixtures/accounts/`, `scripts/gapcal/tests/fixtures/` and `services/keeper/tests/data/*.bin`
are snapshots of public Solana mainnet state: account data and recorded RPC responses
([notice](LICENSES/LicenseRef-Public-Chain-Data.txt)).

## Credits

`crates/shortwin-math/src/num/u256.rs` implements Knuth's Algorithm D (The Art of Computer
Programming, Vol. 2, §4.3.1). Some of its test vectors are scaled from the `divmnu` test vectors
of Hacker's Delight (Henry S. Warren, Jr.).
