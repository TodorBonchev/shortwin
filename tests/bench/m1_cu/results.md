# M1 CU baseline

Compute units of every M1 instruction (ARCH §9 M1) on the SBF program (`anchor build --tools-version v1.57 --arch v3`), measured with Mollusk 0.15.1 on account states built by the LiteSVM harness, with the same SPL Token (p-token), Token-2022 and ATA ELFs; LiteSVM 0.16.0 runs the same instruction as a cross-check. Written by `M1_CU_WRITE=1 cargo test --manifest-path tests/bench/m1_cu/Cargo.toml` (README.md); CI fails when a Mollusk figure exceeds its budget or a case fails.

| Instruction | Case | Mollusk CU | LiteSVM CU | Budget | Budget source |
|---|---|---|---|---|---|
| `initialize_config` | defaults, 7 accounts created | 56145 | 56145 | 200000 | no §6.1 row; admin class (tests/litesvm cu.rs) |
| `propose_config` | `Params` | 12833 | 12833 | 200000 | no §6.1 row; admin class (tests/litesvm cu.rs) |
| `apply_config` | `Params` | 11375 | 11375 | 200000 | no §6.1 row; admin class (tests/litesvm cu.rs) |
| `cancel_config` | pending `Params` | 6576 | 6576 | 200000 | no §6.1 row; admin class (tests/litesvm cu.rs) |
| `set_pause` | global | 6089 | 6089 | 60000 | no §6.1 row; admin flag class (tests/litesvm cu.rs) |
| `set_pause` | one market | 8067 | 8067 | 60000 | no §6.1 row; admin flag class (tests/litesvm cu.rs) |
| `set_coverage_pause` | one market, role installed by the timelock (M7) | 8091 | 8091 | 60000 | no §6.1 row; admin flag class (tests/litesvm cu.rs) |
| `disable_venue` | pump curve bit | 5712 | 5712 | 60000 | no §6.1 row; admin flag class (tests/litesvm cu.rs) |
| `flag_venue_upgraded` | pump, 109 B mock ProgramData | 6036 | 6036 | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` |
| `flag_venue_upgraded` | pump, 10 MiB ProgramData (U5) | 6036 | 6036 | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` |
| `revalidate_venue` | pump, 109 B mock ProgramData | 8506 | 8506 | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` |
| `revalidate_venue` | pump, 10 MiB ProgramData (U5) | 8506 | 8506 | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` |
| `create_market` | SPL Token mint, `None` venue | 60545 | 60545 | 250000 | §6.1 `create_market` |
| `create_market` | Token-2022 mint, `None` venue | 51401 | 51401 | 250000 | §6.1 `create_market` |
| `flag_unsafe_mint` | freeze authority set | 6425 | 6425 | 60000 | no §6.1 row; crank class (§6.1 `crank_price`) |
| `deposit` | first deposit, SPL Token | 36327 | 36327 | 100000 | §6.1 `deposit` / `withdraw` |
| `deposit` | first deposit, Token-2022 | 36547 | 36547 | 100000 | §6.1 `deposit` / `withdraw` |
| `deposit` | second deposit, SPL Token | 28147 | 28147 | 100000 | §6.1 `deposit` / `withdraw` |
| `withdraw` | half the shares, SPL Token | 31631 | 31631 | 100000 | §6.1 `deposit` / `withdraw` |
| `claim_sol` | pays a covered balance | 16420 | 16420 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `cover_deficit` | 1,000,000 lamports | 16719 | 16719 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | lender on the current epoch | 14189 | 14189 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `deposit` | 1 rebase record behind, written | 34616 | 34616 | 100000 | §6.1 `deposit` / `withdraw` |
| `deposit` | 3 rebase records behind, written | 44184 | 44184 | 100000 | §6.1 `deposit` / `withdraw` |
| `deposit` | 8 rebase records behind, written | 68232 | 68232 | 100000 | §6.1 `deposit` / `withdraw` |
| `deposit` | 12 rebase records behind, written | 86744 | 86744 | 100000 | §6.1 `deposit` / `withdraw` |
| `withdraw` | 1 rebase record behind, written | 36635 | 36635 | 100000 | §6.1 `deposit` / `withdraw` |
| `withdraw` | 3 rebase records behind, written | 46222 | 46222 | 100000 | §6.1 `deposit` / `withdraw` |
| `withdraw` | 8 rebase records behind, written | 70251 | 70251 | 100000 | §6.1 `deposit` / `withdraw` |
| `withdraw` | 12 rebase records behind, written | 90263 | 90263 | 100000 | §6.1 `deposit` / `withdraw` |
| `claim_sol` | 1 rebase record behind, written | 21412 | 21412 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | 3 rebase records behind, written | 30980 | 30980 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | 8 rebase records behind, written | 55028 | 55028 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | 10 rebase records behind, written | 65440 | 65440 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `claim_sol` | 12 rebase records behind, written | 75040 | 75040 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `cover_deficit` | 1 rebase record behind, written | 21711 | 21711 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `cover_deficit` | 3 rebase records behind, written | 31279 | 31279 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `cover_deficit` | 8 rebase records behind, written | 55327 | 55327 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `cover_deficit` | 10 rebase records behind, written | 65739 | 65739 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `cover_deficit` | 12 rebase records behind, written | 75339 | 75339 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `sync_lender` | 1 rebase record behind, written | 19365 | 19365 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | 3 rebase records behind, written | 28933 | 28933 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | 8 rebase records behind, written | 52981 | 52981 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | 10 rebase records behind, written | 63393 | 63393 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `sync_lender` | 12 rebase records behind, written | 72993 | 72993 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `sync_lender` | a third party, 1 rebase record passed: the pot-only touch | 18509 | 18509 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | a third party, 8 rebase records passed: the pot-only touch | 47989 | 47989 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | KI-66 full: settles a forward chain, 8 records | 49654 | 49654 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `cover_deficit` | KI-66 full: settles a forward chain, 8 records | 49953 | 49953 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | KI-66 full: owner settles a forward chain, 8 records | 47607 | 47607 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | KI-66 full: FWD_OPEN, settles through 8 records | 50836 | 50836 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | KI-66 full: a third party continues a chain, 4 records | 31655 | 31655 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | KI-66 full: settles a chain a unit above it at every record (lag 1), 7 records | 57927 | 57927 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `claim_sol` | KI-66 full: settles a chain a unit above it at every record (lag 1), 8 records (the web caps a chained settlement at 7) | 64053 | 64053 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `cover_deficit` | KI-66 full: settles a chain a unit above it at every record (lag 1), 7 records | 58229 | 58229 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `cover_deficit` | KI-66 full: settles a chain a unit above it at every record (lag 1), 8 records (the web caps a chained settlement at 7) | 64355 | 64355 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `sync_lender` | KI-66 full: owner settles a chain a unit above it at every record (lag 1), 7 records | 55883 | 55883 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sync_lender` | KI-66 full: owner settles a chain a unit above it at every record (lag 1), 8 records (the web caps a chained settlement at 7) | 62009 | 62009 | 1400000 | transaction limit; beyond the web's 8-record cap (§6.1's 60,000 holds to 8) |
| `claim_sol` | KI-67: a wiped lender's claim that closes the position | 19536 | 19536 | 60000 | §6.1 `claim_sol` / `cover_deficit` / `sync_lender` |
| `sweep_protocol_fees` | the market creation fee | 11747 | 11747 | 60000 | no §6.1 row; crank class (§6.1 `crank_price`) |
| `crank_sol_usd` | mainnet dump, empty cache | 6146 | 6146 | 60000 | no §6.1 row; crank class (§6.1 `crank_price`) |
| `crank_sol_usd` | 55 s later, step checked | 6556 | 6556 | 60000 | no §6.1 row; crank class (§6.1 `crank_price`) |
