//! The Raydium adapters (M3; ARCH §2.4.1, §2.6, §3.2, §3.12, §5.3, §8.2) on the mainnet fork:
//! the release short.win trading against the dumped AMM v4 and CPMM programs and pools
//! (`tests/fixtures/`, `shortwin_litesvm_tests::fork`, `shortwin_litesvm_tests::raydium`).
//! Every instruction goes through `Env::send` (or `Env::send_v0`), so the invariant hooks run
//! after each one — on Raydium markets with the harness's own venue view
//! (`invariants::venue_view`: I13 re-quotes, I16 route rules).
//!
//! | Module | What |
//! |---|---|
//! | `smoke` | list, open and close on every eligible dumped pool |
//! | `listing` | `create_market(RaydiumV4 / RaydiumCpmm)`: exact state, O13 ages and tiers, every verification and eligibility refusal, D1 precedence (live / migrating curve, canonical pool; Fartcoin's real graduated curve) |
//! | `flows` | open → close to the lamport on both programs, SPL and Token-2022, WSOL on either side, every fee variant (AMM v4 fee and `need_take_pnl`; every eligible CPMM config; creator fee off, on the SOL side, on the token side — U11 — and on both) |
//! | `refusals` | the open refusals Raydium adds or reaches: listing window, `R_eff` / locked depth, caps on locked depth, fee caps, halts, flags, disabled venues, spot vs EMA, the venue's own slippage bound |
//! | `latch` | T-LATCH on the dumped ProgramData: either Raydium program's upgrade blocks opens on both; exits unaffected; revalidation |
//! | `pda` | T-RAY-PDA: the delegate spend out of the PDA-owned lending vault and the direct delivery into it, read off the inner instructions (U10 on Token-2022) |
//! | `wsol` | T-RAY-WSOL: pre-funded `trader_wsol` address, donations to the trader (also in the open's transaction and before a keeper's CPMM expiry), a pre-created `ATA(trader, WSOL)`, on both programs (M10 G-B on CPMM) |
//! | `route` | T-RAY-DISABLE (every AMM v4 status, CPMM status bit and open time; S3 at the deadline on a halted pool) and T-RAY-FEE (fees between and above the caps; risk and FV at the locked cap, also on the Token-2022 and creator-fee CPMM pools); M10 venue admin with a position open, `amm_v4_*` / `cpmm_*`: the Emergency flag's S3 timing and FV, liquidations identical with and without it, O15's race (G-D6), the liquidatable set at the cap and at 70 % (G-G5), a pool layout change (G-H3) |
//! | `exits` | liquidation after a real whale pump, expiry, the zero-lamport borrower (M10 G-H10: close, expiry, liquidation, S3 under the Emergency flag), on both programs |
//! | `structure` | I14: every ReadSet / TradeExtras slot mandatory and key-checked on every instruction; a thin pool in place of the adopted one on every instruction (M10 G-A6: the reversed Fartcoin pool, a planted same-mint CPMM pool); extras ride along; a denylisted extra bypassed with `previous` on both programs (G-A5); the pool and its vaults on every extras site and the borrower on the keeper exits are `DeniedAccount`, also as Emergency-template aliases of the CPMM vaults (M10 batch 3) |
//! | `trace_cu` | T-RAY-TRACE / T-RAY-CU and T-SIZE (v0 with the protocol ALT, executed) |

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

mod common;
mod exits;
mod flows;
mod latch;
mod listing;
mod pda;
mod refusals;
mod route;
mod smoke;
mod structure;
mod trace_cu;
mod wsol;
