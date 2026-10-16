//! The PumpSwap adapter and graduation (M6; ARCH §2.4.1 PumpSwap row, §2.5 "On → PumpSwap", §3.2,
//! §3.4, §3.9, §3.12, §5.3–§5.5, §8.2) on the mainnet fork: the release short.win trading against
//! the **dumped** PumpSwap, pump and pump-fees programs and the canonical pools of
//! `tests/fixtures/` (groups `pamm_state`, `pamm_synthetic`, `pump_state`; `shortwin_litesvm_tests::
//! pamm`). Every instruction goes through `Env::send`, so the invariant hooks run after each one —
//! on PumpSwap markets with the harness's own venue view (`invariants::venue_view`: I13 re-quotes,
//! I16 route rules, I18 the post-change confirmation, the graduation's effects).
//!
//! | Module | What |
//! |---|---|
//! | `listing` | `create_market(Auto)` of a PumpSwap-class coin (SPL Token and Token-2022): the PumpSwap cache, the trader's base ATA and pAMM UVA, no price until the first PumpSwap observation, which starts it marked for the warm-up; a `None →` PumpSwap re-detection |
//! | `flows` | open → close to the lamport on every pool role (Token-2022 301 B: fresh after its boost window, mature, cashback; SPL Token 261 B and 211 B), every delta against `shortwin-math` on the readers' view; expiry and liquidation after a real pump on an SPL and a Token-2022 pool; `sol_settle`'s no-op and S1 after a held ×10 pump (S3 under the buy halt is `gates`') |
//! | `refusals` | every PumpSwap open refusal in the program's order: the warm-up, `disable_flags` (`VenueFlagged`, the buy bit `NoRoute`), the boost gate, the latch, the existence requirement, account substitution |
//! | `structure` | I14 (M10 G-A1a): every ReadSet (P0–P6) and TradeExtras (T0–T18) slot omitted, foreign, swapped, or a trailing account, on `open_short`, `close_short`, `liquidate`, `expire`, `crank_price` and `sol_settle` (SPL Token and a cashback coin) |
//! | `wsol` | trader griefing (M10 G-B): lamports donated to `trader` / `trader_wsol` before an open, a close and in the open's transaction; a pre-created `ATA(trader, WSOL)`; tokens donated to `trader_base_ata` before an open, a close, an expiry and a liquidation — all normalised, the legs exact, `T` unchanged |
//! | `gates` | T-DISABLE, T-ATA, T-PDA-USER (pAMM buy), T-RENT (pAMM), T-CASHBACK (pAMM); `sweep_trader`'s six-account token leg key-checked and its cashback slot failing closed on a non-WSOL account (M10 G-A1c) |
//! | `negvqr` | the synthetic negative-VQR pool: pricing on `vault + VQR`, trades to the lamport, the tier inputs at `min(real, effective)` |
//! | `buckets` | M10 pump revalidation, on the 2026-10-02 redeploy: v2 trades keep their fees in the pool's buckets (@271/@279, off VQR), a v1 sale is paid from `vault − buckets` (6063; the 2026-09 build pays it), the sweeps leave the price, short.win's v1 open and close on a pool carrying buckets to the lamport; since M10 program batch 4 (PUMP-2026-10-1) `R` and the tier read the payable `vault − buckets` and the boost `max(VQR + buckets, 0)`: the open's sale cap to the lamport, the keeper exits, a negative-VQR pool with buckets, buckets above the vault not executable |
//! | `lock` | ★T-PAMM-LOCK (M6R-1, the owner's decision of 2026-09-30): the pool's locked depth `D` from its burned LP (the LP mint P6), key-checked; third-party `deposit` / `withdraw` against the size cap, the tier, the reference cap and the warm-up's `L_slow ≤ D` (`attacks/pumpswap_lp.py` parts 2 and 3); FITS-2 (a) and FITX-1 on a pool with third-party LP; M6D-1 as decided (OWNER-M6D-1, ARCH row 57, ECON #106: PumpSwap credited on its burned LP only — part 3 (b): a held deposit lifts no open; part 3 (e): the pulled pump liquidates, the lenders whole; part 3 (f), M6E-1: the reference cap at `D`, so a deposit pulled at a deadline holds no expiry up); the capacity the rule costs on every pool fixture |
//! | `admin` | venue admin with a position open (M10): PumpSwap `FeeConfig` hikes between the caps and above the hard cap (G-G4: opens refused, S3 evidence, risk and FV at the locked cap, closes and liquidations at the live fee), mayhem at listing and mid-position and a non-WSOL quote plant (G-H9), a pool layout change (G-H3: not executable, S3 at the cap), the Emergency route-broken flag (G-D6: S3 after the timer, liquidations identical second by second, O15's race against an honest expiry), extras on the sell leg (G-H2: appended to the `sell` CPI); the pool and its vaults on every extras site and the borrower on the keeper exits are `DeniedAccount` (M10 batch 3) |
//! | `hooks` | M10: the PumpSwap clauses of I18 — (a) the price start's anchor at the burned depth, (d) the exit reference capped at `D`, (f) `L_slow ≤ pool_d` through the warm-up — and I15's burned-LP sizing and tier (OWNER-M6D-1) fire on a mutant's state after a real instruction, which passes |
//! | `v3_era` | the 2026-10-13 re-pin (pump and PumpSwap of 2026-10-08, pump-fees of 2026-10-12): T-PFEE-CPI — inside short.win's PumpSwap sale and buy the 2026-10-12 pump-fees only answers `get_fees` / `get_fees_with_quote_mint` on `[PumpSwap's FeeConfig, PumpSwap]`, never `withdraw`, seeing no short.win account |
//! | `graduation` | a curve market with open positions through completion, `PumpMigrating`, `migrate_v2`, `→ PumpSwap`, the warm-up and exits on the pool; a graduation without positions, then trading on the pool. The atomic `[buy_v2(rT), migrate_v2]` is `m5_sec_curve_it::m5_sec_4_an_atomic_complete_and_migrate_graduates_the_market_onto_its_pool`, a borrower's self-made straddle (liquidated on the curve in one instruction since pump-final P2) `synthetic_exit_it::a_self_made_straddle_is_liquidated_at_once`, the real `migrate_v2` scenarios `pump_curve_e2e` |
//!
//! Opens and closes are sent with a 300 k CU limit (`VenueMarket::cu_limit`), keeper exits with
//! 400 k.

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

mod admin;
mod buckets;
mod common;
mod creator;
mod flows;
mod gates;
mod graduation;
mod hooks;
mod listing;
mod lock;
mod negvqr;
mod refusals;
mod structure;
mod v3_era;
mod wsol;

/// The graduation E2E's machinery (`graduation_support.rs`): the keeper's view of a PumpSwap
/// position (`pool_status`), for `lock`.
#[path = "../graduation_support.rs"]
mod support;
