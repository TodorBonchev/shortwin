//! T-TRACE on the pump curve, with the stack depth (ARCH §6.2, §6.3, §8.2; C1-1, C1-8; §9 M5):
//! every curve-path instruction of the release short.win, on the dumped pump, pump-fees and
//! PumpSwap programs (SPL Token and Token-2022 curves), read off the transaction's own inner
//! instructions.
//!
//! - **Per instruction**: the exact instruction-trace entries (the instruction plus every CPI at
//!   any depth) and the exact call shape — which program at which stack height — of
//!   `create_market(Auto)` (a curve listing with the init set; a PumpSwap listing with the pAMM
//!   init set; a list-only listing),
//!   `crank_price` (roll and record; `PumpCurve → PumpMigrating`, `PumpMigrating → None`,
//!   `None → PumpCurve`), `open_short` (a wallet's first open, a repeat borrower, with
//!   normalisation, with a short curve's growth), `close_short` (plain, normalising, a completing
//!   buyback, a synthetic completion above `rT`), `liquidate` and `expire` (each also a synthetic
//!   completion above `rT`), `sol_settle` (no-op; S3
//!   on `PumpMigrating` and on `None`), `sweep_trader`'s token leg, `flag_venue_upgraded` and
//!   `revalidate_venue`. Every curve leg is one pump CPI at height 2 (`sell_v3` / `buy_v3`,
//!   pump-final §6.1) whose token, System and event CPIs sit at height 3, the peak: short.win
//!   keeps two spare levels (ARCH §6.3; the bench `tests/bench/m5_cu` runs them through one and
//!   two wrapper programs). v3 makes no pump-fees CPI and names no creator vault: a curve with a
//!   creator traces like one without, and an empty creator vault costs nothing.
//! - **Worst-case transactions** (ComputeBudget included): the largest open and close, an atomic
//!   sandwich of an open with the curve's own trades, the most opens of one wallet in one
//!   transaction, a batch of four cranks that all move their market to `PumpMigrating`, the
//!   keeper's `[crank, liquidate | expire | sol_settle S3]` (the exits also past `rT`), the three
//!   pump latch flags — each ≤ 64
//!   (`MAX_INSTRUCTION_TRACE_LENGTH`); one open more is refused by the runtime itself.
//!
//! Figures against ARCH §6.2's curve column (open 9 + 1 + 2 = 12, ≤ 15; exits 11 + 1 = 12,
//! ≤ 15) and pump-final §6.6 (sell leg 1 + 3 + 2, buy leg 1 + 4 + 2, a synthetic buy + 2): an
//! open is 13 entries for a wallet's first position (the table omits the position `init` and the
//! exposure `init_if_needed`, as on Raydium), 12 for a repeat borrower, 16 with normalisation (17
//! when the sale is also a 151-byte curve's first trade on pump's 2026-10-08 build, which grows
//! it to 166 bytes); an exit is 9, 12 normalising, 11 with a completing buyback (pump's
//! `CompleteEvent` and short.win's `VenueStateChanged`) and 12 with a synthetic completion above
//! `rT` (pump's `CompleteEvent` and `PostCompleteBuyEvent` too); a crank that moves the venue is
//! 3–4 (its `VenueStateChanged` and chain events). Every path is within the table's maxima of 15.
//!
//! Every transaction goes through `Env::send`: the invariant hooks run after each instruction.
//! A keeper's decision to send is a `simulate_transaction` on the current state, never a
//! committed probe. The only planted state is the venue's (pump's curve account: reserves zeroed
//! by a migration elsewhere, a curve removed before a listing; the header of a ProgramData for
//! the latch).

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::types::TransactionMetadata;
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, PUMP_FEES_PROGRAM_ID,
    PUMP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use shortwin::events::{
    Close, Expire, Liquidate, Open, PriceCranked, RouteChainChanged, SolSettle, TraderSwept,
    VenueStateChanged,
};
use shortwin::state::VenueKind;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::errors::ok;
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve, PumpProgram, PumpV2};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::{Env, events, ix, pda};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;

/// A live legacy SPL-Token curve of 115 bytes without a creator.
const SPL: PumpCoin = PumpCoin::Legacy115;
/// A live Token-2022 curve of 115 bytes with a creator.
const T22: PumpCoin = PumpCoin::T22_115;
/// A live Token-2022 curve of 151 bytes near completion.
const NEAR: PumpCoin = PumpCoin::Near;
/// Both token programs.
const VARIANTS: [PumpCoin; 2] = [SPL, T22];

/// `MAX_INSTRUCTION_TRACE_LENGTH` (agave `transaction-context`; ARCH §6.2).
const MAX_TRACE: usize = 64;
/// `MAX_INSTRUCTION_STACK_DEPTH` on mainnet (SIMD-0268 inactive; ARCH §6.3).
const MAX_DEPTH: u8 = 5;

/// `ComputeBudget111111111111111111111111111111`.
const COMPUTE_BUDGET: Pubkey =
    Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");

// ---- the shape of a transaction -------------------------------------------------------------

/// A program as the shapes name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum P {
    /// short.win itself (its `emit_cpi!` event self-CPI).
    Own,
    System,
    /// SPL Token or Token-2022.
    Token,
    /// pump (`6EF8rr…`), and its event self-CPI.
    Pump,
    /// pump-fees (`get_fees_with_quote_mint`).
    Pfee,
    /// The ATA program (the listing's `create_idempotent`).
    Ata,
    /// PumpSwap (`pAMMBay6…`), and its event self-CPI.
    Pamm,
    Other,
}

/// The inner instructions of top-level instruction `index` of the transaction `ixs` (paid by the
/// harness payer) as `(program, stack height)`.
fn shape(env: &Env, ixs: &[Instruction], meta: &TransactionMetadata, index: usize) -> Vec<(P, u8)> {
    let msg = Message::new(ixs, Some(&env.payer.pubkey()));
    let keys = msg.account_keys;
    meta.inner_instructions
        .get(index)
        .map(|list| {
            list.iter()
                .map(|i| {
                    let p = keys[usize::from(i.instruction.program_id_index)];
                    let kind = if p == shortwin::ID {
                        P::Own
                    } else if p == anchor_lang::system_program::ID {
                        P::System
                    } else if p == TOKEN_PROGRAM_ID || p == TOKEN_2022_PROGRAM_ID {
                        P::Token
                    } else if p == PUMP_PROGRAM_ID {
                        P::Pump
                    } else if p == PUMP_FEES_PROGRAM_ID {
                        P::Pfee
                    } else if p == ASSOCIATED_TOKEN_PROGRAM_ID {
                        P::Ata
                    } else if p == shortwin::constants::PUMP_AMM_PROGRAM_ID {
                        P::Pamm
                    } else {
                        P::Other
                    };
                    (kind, i.stack_height)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Entries of top-level instruction `index`: itself plus every CPI at any depth.
fn entries(meta: &TransactionMetadata, index: usize) -> usize {
    1 + meta.inner_instructions.get(index).map_or(0, Vec::len)
}

/// Entries of a whole transaction of `top_level` instructions (ARCH §6.2).
fn tx_entries(meta: &TransactionMetadata, top_level: usize) -> usize {
    top_level + meta.inner_instructions.iter().map(Vec::len).sum::<usize>()
}

/// The deepest stack height in the transaction (1 = top level).
fn peak(meta: &TransactionMetadata) -> u8 {
    meta.inner_instructions
        .iter()
        .flatten()
        .map(|i| i.stack_height)
        .max()
        .unwrap_or(1)
}

/// pump's `sell_v3` under short.win (ARCH §2.4.1 PumpCurve row; pump-final §6.1): pump at height
/// 2, the token transfer `trader_base_ata → curve_base_ata` and its `TradeEvent` at height 3 (the
/// proceeds are credited by direct lamport moves, the fees stay in the curve's buckets: no inner
/// instruction, no pump-fees lookup, no creator vault).
fn sell() -> Vec<(P, u8)> {
    vec![(P::Pump, 2), (P::Token, 3), (P::Pump, 3)]
}

/// The sell leg of an open: `q` from the lending vault into `trader_base_ata` (the market signs),
/// `ert` from the borrower to the trader, `sell_v3`, then `S` to the position and `ert` back to
/// the borrower (System transfers the trader signs).
fn sell_leg() -> Vec<(P, u8)> {
    let mut v = vec![(P::Token, 2), (P::System, 2)];
    v.extend(sell());
    v.extend([(P::System, 2), (P::System, 2)]);
    v
}

/// What a buy does to the curve: the `TradeEvent` alone, pump's `CompleteEvent` too when it buys
/// exactly `rT`, and its `PostCompleteBuyEvent` as well when it buys past `rT` (the synthetic
/// migration, pump-final §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Buy {
    Plain,
    Completing,
    Synthetic,
}

impl Buy {
    /// pump's events at height 3.
    fn events(self) -> usize {
        match self {
            Self::Plain => 1,
            Self::Completing => 2,
            Self::Synthetic => 3,
        }
    }
}

/// pump's `buy_v3` under short.win with the trader PDA as `user` (T-PDA-USER): pump at height 2,
/// the token transfer `curve_base_ata → trader_base_ata` and one System transfer **from the
/// trader** (the whole charge: v3 keeps its fees in the curve's buckets) at height 3, then its
/// events (`buy`).
fn buy(buy: Buy) -> Vec<(P, u8)> {
    let mut v = vec![(P::Pump, 2), (P::Token, 3), (P::System, 3)];
    v.extend(std::iter::repeat_n((P::Pump, 3), buy.events()));
    v
}

/// The buy leg of an exit: the position funds the trader by direct lamport moves, a 0-lamport
/// System transfer syncs both balances for the runtime's per-CPI check, `buy_v3`, the refund of
/// what is left to the position, and `q` from `trader_base_ata` to the lending vault.
fn buy_leg(b: Buy) -> Vec<(P, u8)> {
    let mut v = vec![(P::System, 2)];
    v.extend(buy(b));
    v.extend([(P::System, 2), (P::Token, 2)]);
    v
}

/// Trader normalisation on the pump venues (ARCH §2.4 rule 5): the donated lamports of the
/// trader PDA to `market_vault`, the donated tokens of `trader_base_ata` to the lending vault,
/// and `TraderSwept`.
const NORMALISE: [(P, u8); 3] = [(P::System, 2), (P::Token, 2), (P::Own, 2)];

/// An open: `inits` System creations (the position; a first open also the exposure record), the
/// normalisation if any, the sell leg, the margin and fee transfers, the event.
fn open_shape(inits: usize, normalise: bool) -> Vec<(P, u8)> {
    let mut v = vec![(P::System, 2); inits];
    if normalise {
        v.extend(NORMALISE);
    }
    v.extend(sell_leg());
    v.extend([(P::System, 2), (P::System, 2), (P::Own, 2)]);
    v
}

/// `s` with pump's growth of a short curve on its first trade (the 151-byte `near` curve to 166
/// bytes on the 2026-10-08 build, `PumpCurve::growth_top_up`): the trader's rent top-up of the
/// curve, one System transfer at height 3 right after pump's own entry, before its token transfer.
fn with_growth(mut s: Vec<(P, u8)>, grow: bool) -> Vec<(P, u8)> {
    if grow {
        let at = s
            .iter()
            .position(|&e| e == (P::Pump, 2))
            .expect("a pump CPI")
            + 1;
        s.insert(at, (P::System, 3));
    }
    s
}

/// An exit: the normalisation if any, the buy leg, a completing buyback's `VenueStateChanged`
/// (`→ PumpMigrating` in the instruction, after `rT` exactly or past it), the exit's event.
fn exit_shape(normalise: bool, b: Buy) -> Vec<(P, u8)> {
    let mut v = if normalise {
        NORMALISE.to_vec()
    } else {
        Vec::new()
    };
    v.extend(buy_leg(b));
    v.extend(std::iter::repeat_n(
        (P::Own, 2),
        1 + usize::from(b != Buy::Plain),
    ));
    v
}

/// Prints and returns the shape of instruction `index`.
fn shown(
    env: &Env,
    what: &str,
    ixs: &[Instruction],
    meta: &TransactionMetadata,
    index: usize,
) -> Vec<(P, u8)> {
    let s = shape(env, ixs, meta, index);
    println!(
        "{what}: {} entries, peak {}, {} CU: {s:?}",
        entries(meta, index),
        peak(meta),
        meta.compute_units_consumed
    );
    s
}

/// Every shape is short.win's own CPIs at height 2 and the venue's at 2 (pump, PumpSwap, the ATA
/// program) with theirs at 3; returns the number of pump CPIs short.win made.
fn pump_calls(s: &[(P, u8)]) -> usize {
    assert!(
        s.iter().all(|&(p, h)| match p {
            P::Own => h == 2,
            P::System | P::Token => h == 2 || h == 3,
            P::Pump | P::Pamm | P::Ata => h == 2 || h == 3,
            P::Pfee => h == 3,
            P::Other => false,
        }),
        "{s:?}"
    );
    s.iter().filter(|&&(p, h)| p == P::Pump && h == 2).count()
}

// ---- scenarios ------------------------------------------------------------------------------

/// `vT/200`, the `pump_it` position size (0.5 % of the virtual token reserve).
fn q_of(env: &Env, c: &PumpCurve) -> u64 {
    c.state(&env.svm).virtual_token_reserves / 200
}

/// Lists `c`, lends `40·Q`, waits out the warm-up and cranks once.
fn add_market(env: &mut Env, c: PumpCurve) -> (PumpMarket, u64) {
    ok(env.list_pump(&c));
    let q = q_of(env, &c);
    let pm = env.pump_market_listed(PumpSpec::new(c.coin, q.saturating_mul(40)), c);
    env.crank(&pm);
    (pm, q)
}

fn setup(coin: PumpCoin) -> (Env, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(coin);
    let (pm, q) = add_market(&mut env, c);
    (env, pm, q)
}

fn open_ix(pm: &impl VenueMarket, b: &Pubkey, nonce: u64, q: u64) -> Instruction {
    open_short_ix(pm, b, nonce, q, 0, u64::MAX, u64::MAX, ExtrasSel::NONE, &[])
}

fn close_ix(pm: &impl VenueMarket, b: &Pubkey, nonce: u64) -> Instruction {
    close_short_ix(pm, b, nonce, u64::MAX, ExtrasSel::NONE, &[])
}

/// The client's transaction: ARCH §6.1's CU limit for a curve trade, then `ix`.
fn client(ix: Instruction) -> Vec<Instruction> {
    vec![pump::cu_limit_ix(pump::CURVE_TRADE_CU), ix]
}

/// The keeper's transaction: ARCH §6.1's CU limit for a curve exit, then `ix`.
fn keeper_tx(ix: Instruction) -> Vec<Instruction> {
    vec![pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix]
}

/// A position of `q` two cranked seconds old.
fn with_position(coin: PumpCoin) -> (Env, PumpMarket, Borrower, u64, u64) {
    let (mut env, pm, q) = setup(coin);
    let mut b = env.borrower();
    let (res, nonce) = env.open(&pm, &mut b, q);
    ok(res);
    env.tick(&pm, 2);
    (env, pm, b, nonce, q)
}

/// A System transfer.
fn transfer(from: &Keypair, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            AccountMeta::new(from.pubkey(), true),
            AccountMeta::new(*to, false),
        ],
        data,
    }
}

/// Tokens from a curve user's (planted) balance to `to` (`TransferChecked`, the user signing).
fn donate_tokens(env: &mut Env, pm: &PumpMarket, to: &Pubkey, amount: u64) {
    let u = CurveUser::new(env, &pm.curve);
    fork::set_token_amount(&mut env.svm, &u.ata, amount);
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(6);
    let ix = Instruction {
        program_id: pm.keys.token_program,
        accounts: vec![
            AccountMeta::new(u.ata, false),
            AccountMeta::new_readonly(pm.keys.mint, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(u.kp.pubkey(), true),
        ],
        data,
    };
    ok(env.send(&[ix], &[&u.kp]));
}

/// Lamports to the trader PDA and tokens to `trader_base_ata`, so the next leg normalises.
fn donate_to_trader(env: &mut Env, pm: &PumpMarket) {
    let griefer = env.funded_keypair();
    ok(env.send(&[transfer(&griefer, &pm.trader(), 1_000_000)], &[&griefer]));
    donate_tokens(env, pm, &pm.trader_base_ata, 777);
}

/// `simulate_transaction` of `ixs` (nothing is committed): its metadata if it would succeed.
fn simulate(env: &Env, ixs: &[Instruction], signers: &[&Keypair]) -> Option<TransactionMetadata> {
    let mut all: Vec<&Keypair> = vec![&env.payer];
    all.extend(signers.iter().copied());
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&env.payer.pubkey()),
        &all,
        env.svm.latest_blockhash(),
    );
    env.svm.simulate_transaction(tx).ok().map(|s| s.meta)
}

/// A third party buys `rT − rt_left` on the curve (a real `buy_v2`, which stops at `rT`).
fn buy_down_to(env: &mut Env, c: &PumpCurve, rt_left: u64) {
    let u = CurveUser::new(env, c);
    let rt = c.state(&env.svm).real_token_reserves;
    ok(u.buy(env, c, rt - rt_left, u64::MAX));
}

/// A liquidatable position (40 % of `vT` bought, the keeper's simulated `[crank, liquidate]`
/// succeeds at the returned, uncranked second).
fn liquidatable(coin: PumpCoin) -> (Env, PumpMarket, Borrower, u64, Keypair) {
    let (mut env, pm, b, nonce, _) = with_position(coin);
    env.tick(&pm, 1);
    let u = CurveUser::new(&mut env, &pm.curve);
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    ok(u.buy(&mut env, &pm.curve, vt / 10 * 4, u64::MAX));
    let keeper = env.funded_keypair();
    for _ in 0..900 {
        env.warp(1);
        let ixs = [
            pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU),
            crank_price_ix(&pm),
            liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce),
        ];
        if simulate(&env, &ixs, &[&keeper]).is_some() {
            return (env, pm, b, nonce, keeper);
        }
        env.crank(&pm);
    }
    panic!("{coin:?}: never liquidatable");
}

/// An expired healthy position, three cranked seconds past its deadline.
fn expired(coin: PumpCoin) -> (Env, PumpMarket, Borrower, u64, Keypair) {
    let (mut env, pm, b, nonce, _) = with_position(coin);
    let deadline = env.position(&pm, &b.key(), nonce).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    env.tick(&pm, 3);
    let keeper = env.funded_keypair();
    (env, pm, b, nonce, keeper)
}

/// pump `Global`'s initial reserves (`vT`, `vQ`, `rT`: ECON's `PumpCurve.at_progress(0)`).
const PUMP_VT0: u64 = 1_073_000_000_000_000;
const PUMP_VS0: u64 = 30_000_000_000;
const PUMP_RT0: u64 = 793_100_000_000_000;
/// ECON's `D@80%` market (`synthetic_exit_it`'s T-SYNTH-2): the tokens the fresh curve sold.
const D80_SOLD: u64 = 634_480_000_000_000;

/// The keeper's simulated `[crank, exit]` retried every second (cranked when not yet) until it
/// succeeds; the clock at that second, uncranked.
fn until_exit(
    env: &mut Env,
    pm: &PumpMarket,
    keeper: &Keypair,
    exit: &Instruction,
    max_s: usize,
) -> bool {
    for _ in 0..max_s {
        env.warp(1);
        let ixs = [
            pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU),
            crank_price_ix(pm),
            exit.clone(),
        ];
        if simulate(env, &ixs, &[keeper]).is_some() {
            return true;
        }
        env.crank(pm);
    }
    false
}

/// A position past `rT` the keeper liquidates (T-SYNTH-2's path): ECON's `D@80%` market (the fresh
/// curve at pump's initial reserves, the crowd's `buy_v2` of 80 % of `rT₀`), a position of `vT/200`
/// 30 cranked seconds old, the crowd leaving `rT = q/2` (the price × 2.4); liquidatable at the
/// returned, uncranked second.
fn liquidatable_synthetic() -> (Env, PumpMarket, Borrower, u64, Keypair) {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
    fork::set_curve_reserves(&mut env.svm, &c.curve, PUMP_VT0, PUMP_VS0, PUMP_RT0, 0);
    let crowd = CurveUser::new(&mut env, &c);
    env.fund(&crowd.kp.pubkey(), 2_000_000_000_000);
    ok(crowd.buy(&mut env, &c, D80_SOLD, u64::MAX));
    let (pm, q) = add_market(&mut env, c);
    let mut b = env.borrower();
    let (res, nonce) = env.open(&pm, &mut b, q);
    ok(res);
    env.tick(&pm, 30);
    buy_down_to(&mut env, &pm.curve, q / 2);
    let keeper = env.funded_keypair();
    let liq = liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce);
    assert!(
        until_exit(&mut env, &pm, &keeper, &liq, 900),
        "never liquidatable"
    );
    (env, pm, b, nonce, keeper)
}

/// An expired position past `rT` on the near-complete curve: three cranked seconds past its
/// deadline the crowd leaves `rT = q/2`; the keeper's `[crank, expire]` succeeds at the returned,
/// uncranked second (the EMA has followed).
fn expired_synthetic() -> (Env, PumpMarket, Borrower, u64, Keypair) {
    let (mut env, pm, b, nonce, keeper) = expired(NEAR);
    let q = env.position(&pm, &b.key(), nonce).q;
    buy_down_to(&mut env, &pm.curve, q / 2);
    let exp = expire_ix(&pm, &keeper.pubkey(), &b.key(), nonce);
    assert!(
        until_exit(&mut env, &pm, &keeper, &exp, 600),
        "never expired"
    );
    (env, pm, b, nonce, keeper)
}

/// A list-only view of a pump market (the detection set as ReadSet).
struct AsNone<'a>(&'a PumpMarket);

impl VenueMarket for AsNone<'_> {
    fn keys(&self) -> &MarketKeys {
        &self.0.keys
    }
    fn read_set(&self, _writable: bool) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.0.curve.curve, false),
            AccountMeta::new_readonly(pda::canonical_pool(&self.0.keys.mint), false),
        ]
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        Vec::new()
    }
}

/// The keeper's `[crank, sol_settle]` every 30 s until the simulation settles S3; the clock at
/// that second, uncranked.
fn until_s3(env: &mut Env, mm: &impl VenueMarket, keeper: &Keypair, b: &Pubkey, nonce: u64) {
    for _ in 0..200 {
        env.warp(30);
        let ixs = [
            crank_price_ix(mm),
            sol_settle_ix(env, mm, &keeper.pubkey(), b, nonce),
        ];
        if let Some(meta) = simulate(env, &ixs, &[keeper])
            && events::of::<SolSettle>(&meta)
                .first()
                .is_some_and(|e| e.condition == 3)
        {
            return;
        }
        ok(env.send(&[crank_price_ix(mm)], &[]));
    }
    panic!("S3 never due");
}

/// A stuck migration with a position: S3 due (70 min after `migration_seen_at`).
fn s3_migrating() -> (Env, PumpMarket, Borrower, u64, Keypair) {
    let (mut env, pm, b, nonce, _) = with_position(NEAR);
    let keeper = env.funded_keypair();
    let deadline = env.position(&pm, &b.key(), nonce).deadline;
    let now = env.now();
    env.warp(deadline - 1_800 - now);
    env.crank(&pm);
    env.warp(1);
    buy_down_to(&mut env, &pm.curve, 0);
    env.warp(1);
    env.crank(&pm);
    until_s3(&mut env, &pm, &keeper, &b.key(), nonce);
    (env, pm, b, nonce, keeper)
}

/// A curve that migrated elsewhere (`→ None`) with an expired position: S3 due on the detection
/// set.
fn s3_none() -> (Env, PumpMarket, Borrower, u64, Keypair) {
    let (mut env, pm, b, nonce, _) = with_position(NEAR);
    let keeper = env.funded_keypair();
    let deadline = env.position(&pm, &b.key(), nonce).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    env.crank(&pm);
    env.warp(1);
    buy_down_to(&mut env, &pm.curve, 0);
    env.warp(1);
    env.crank(&pm);
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    env.crank(&pm);
    assert_eq!(env.market(&pm.market()).venue_kind(), Some(VenueKind::None));
    until_s3(&mut env, &AsNone(&pm), &keeper, &b.key(), nonce);
    (env, pm, b, nonce, keeper)
}

fn send(env: &mut Env, ixs: &[Instruction], signers: &[&Keypair]) -> TransactionMetadata {
    ok(env.send(ixs, signers))
}

fn with_cb(ixs: Vec<Instruction>) -> Vec<Instruction> {
    let mut limit = vec![2u8];
    limit.extend_from_slice(&1_400_000u32.to_le_bytes());
    let mut price = vec![3u8];
    price.extend_from_slice(&1u64.to_le_bytes());
    let mut v = vec![
        Instruction {
            program_id: COMPUTE_BUDGET,
            accounts: vec![],
            data: limit,
        },
        Instruction {
            program_id: COMPUTE_BUDGET,
            accounts: vec![],
            data: price,
        },
    ];
    v.extend(ixs);
    v
}

/// Whether `coin`'s curve charges a creator fee (a creator is set): a user's `buy_v2` makes one
/// more System transfer for it (`buy_v3` makes one transfer either way).
fn has_creator(env: &Env, pm: &PumpMarket) -> bool {
    !pm.curve.state(&env.svm).creator_is_default()
}

// ---- per instruction ------------------------------------------------------------------------

/// `create_market(Auto)` of a live curve: `Market`, `MarketVault` and the lending vault
/// (creation and `InitializeAccount3`), the creation fee, the trader's base ATA through the ATA
/// program (its size query, creation, immutable owner and initialisation one level down), the
/// trader's pump UVA through pump's `init_user_volume_accumulator` (its creation and event one
/// level down), the event: 15 entries, peak 3. A coin on its canonical PumpSwap pool (M6) has the
/// same shape with pAMM's `init_user_volume_accumulator` in pump's place: 15, peak 3. A list-only
/// listing (a coin graduated elsewhere): 7, peak 2.
#[test]
fn create_market() {
    for coin in VARIANTS {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let creator = env.funded_keypair();
        let ixs = [pump::create_pump_market_ix(&creator.pubkey(), &c)];
        let meta = send(&mut env, &ixs, &[&creator]);
        let s = shown(&env, &format!("{coin:?} create_market"), &ixs, &meta, 0);
        assert_eq!(
            s,
            [
                (P::System, 2),
                (P::System, 2),
                (P::System, 2),
                (P::Token, 2),
                (P::System, 2),
                (P::Ata, 2),
                (P::Token, 3),
                (P::System, 3),
                (P::Token, 3),
                (P::Token, 3),
                (P::Pump, 2),
                (P::System, 3),
                (P::Pump, 3),
                (P::Own, 2),
            ],
            "{coin:?}"
        );
        assert_eq!((entries(&meta, 0), peak(&meta)), (15, 3));
    }
    {
        let (mut env, c) = Env::fork_pump_coin(PumpCoin::Graduated);
        let creator = env.funded_keypair();
        let plain = ix::create_market(
            &creator.pubkey(),
            &c.mint,
            &c.token_program.id(),
            shortwin::instructions::create_market::VENUE_HINT_AUTO,
            &pda::canonical_pool(&c.mint),
        );
        let market = pda::market(&c.mint).0;
        let ixs = [ix::with_remaining(
            plain,
            &shortwin_litesvm_tests::pamm::pamm_init_set(&market),
        )];
        let meta = send(&mut env, &ixs, &[&creator]);
        let s = shown(&env, "Graduated create_market (PumpSwap)", &ixs, &meta, 0);
        assert_eq!(
            s,
            [
                (P::System, 2),
                (P::System, 2),
                (P::System, 2),
                (P::Token, 2),
                (P::System, 2),
                (P::Ata, 2),
                (P::Token, 3),
                (P::System, 3),
                (P::Token, 3),
                (P::Token, 3),
                (P::Pamm, 2),
                (P::System, 3),
                (P::Pamm, 3),
                (P::Own, 2),
            ]
        );
        assert_eq!((entries(&meta, 0), peak(&meta)), (15, 3));
    }
    {
        let coin = PumpCoin::Fartcoin;
        let (mut env, c) = Env::fork_pump_coin(coin);
        let creator = env.funded_keypair();
        let ixs = [ix::create_market(
            &creator.pubkey(),
            &c.mint,
            &c.token_program.id(),
            shortwin::instructions::create_market::VENUE_HINT_AUTO,
            &pda::canonical_pool(&c.mint),
        )];
        let meta = send(&mut env, &ixs, &[&creator]);
        let s = shown(
            &env,
            &format!("{coin:?} create_market (None)"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(
            s,
            [
                (P::System, 2),
                (P::System, 2),
                (P::System, 2),
                (P::Token, 2),
                (P::System, 2),
                (P::Own, 2)
            ],
            "{coin:?}: the list-only listing creates no venue account"
        );
        assert_eq!((entries(&meta, 0), peak(&meta)), (7, 2));
    }
}

/// `crank_price` on the curve: roll + record is its `PriceCranked` (2 entries); a transition adds
/// `VenueStateChanged` — `PumpCurve → PumpMigrating` also starts the chains (`RouteChainChanged`:
/// 4 entries), `PumpMigrating → None` keeps them running (3); the `None → PumpCurve`
/// re-detection 4; a `None` market's crank 2.
#[test]
fn crank_price() {
    for coin in VARIANTS {
        let (mut env, pm, _) = setup(coin);
        env.warp(1);
        let ixs = [crank_price_ix(&pm)];
        let meta = send(&mut env, &ixs, &[]);
        let s = shown(&env, &format!("{coin:?} crank_price"), &ixs, &meta, 0);
        assert_eq!(s, [(P::Own, 2)], "roll + record: the event only");
        let _: PriceCranked = events::one(&meta);
    }

    // PumpCurve → PumpMigrating (a third party completed the curve), a position open.
    let (mut env, pm, _, _, _) = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, 0);
    env.warp(1);
    let ixs = [crank_price_ix(&pm)];
    let meta = send(&mut env, &ixs, &[]);
    let s = shown(&env, "crank_price → PumpMigrating", &ixs, &meta, 0);
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    assert_eq!(
        events::of::<RouteChainChanged>(&meta).len(),
        1,
        "the chains start"
    );
    let _: PriceCranked = events::one(&meta);
    assert_eq!(s, [(P::Own, 2); 3], "{s:?}");
    assert_eq!((entries(&meta, 0), peak(&meta)), (4, 2));

    // PumpMigrating → None (migrated elsewhere).
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    let meta = send(&mut env, &ixs, &[]);
    let s = shown(&env, "crank_price → None", &ixs, &meta, 0);
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::None as u8);
    assert!(
        events::of::<RouteChainChanged>(&meta).is_empty(),
        "the chains run on"
    );
    let _: PriceCranked = events::one(&meta);
    assert_eq!(s, [(P::Own, 2); 2], "{s:?}");

    // A None market's crank on its detection set.
    env.warp(1);
    let none = [crank_price_ix(&AsNone(&pm))];
    let meta = send(&mut env, &none, &[]);
    let s = shown(&env, "crank_price (None)", &none, &meta, 0);
    assert_eq!(s, [(P::Own, 2)], "{s:?}");

    // None → PumpCurve: a mint listed before its curve was readable, re-detected by a crank.
    let (mut env, c) = Env::fork_pump_coin(T22);
    let saved = env.svm.get_account(&c.curve).unwrap();
    env.svm.set_account(c.curve, Account::default()).unwrap();
    let creator = env.funded_keypair();
    let list = ix::create_market(
        &creator.pubkey(),
        &c.mint,
        &c.token_program.id(),
        shortwin::instructions::create_market::VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    ok(env.send(&[list], &[&creator]));
    env.svm.set_account(c.curve, saved).unwrap();
    let vt = c.state(&env.svm).virtual_token_reserves;
    let pm = env.pump_market_listed(PumpSpec::new(T22, (vt / 200) * 40), c);
    let ixs = [crank_price_ix(&AsNone(&pm))];
    let meta = send(&mut env, &ixs, &[]);
    let s = shown(&env, "crank_price None → PumpCurve", &ixs, &meta, 0);
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpCurve as u8);
    let _: PriceCranked = events::one(&meta);
    assert_eq!(
        events::of::<RouteChainChanged>(&meta).len(),
        1,
        "the chains cleared"
    );
    assert_eq!(s, [(P::Own, 2); 3], "{s:?}");
    assert_eq!((entries(&meta, 0), peak(&meta)), (4, 2));
}

/// `open_short` on the curve: 13 entries for a wallet's first position, 12 for a repeat, 16
/// normalising, 17 when that sale is also the 151-byte curve's first trade on the 2026-10-08
/// build (its growth to 166 bytes); one pump CPI, peak 3. A curve with a creator (T22) traces
/// like one without (SPL): `sell_v3` names no creator vault.
#[test]
fn open_short() {
    for coin in VARIANTS {
        let (mut env, pm, q) = setup(coin);
        env.tick(&pm, 1);
        let b = env.borrower();
        let first = client(open_ix(&pm, &b.key(), 0, q));
        let meta = send(&mut env, &first, &[&b.kp]);
        let _: Open = events::one(&meta);
        let s = shown(&env, &format!("{coin:?} open (first)"), &first, &meta, 1);
        assert_eq!(s, open_shape(2, false), "{coin:?}");
        assert_eq!(pump_calls(&s), 1);
        assert_eq!((entries(&meta, 1), peak(&meta)), (13, 3));

        let repeat = client(open_ix(&pm, &b.key(), 1, q));
        let meta = send(&mut env, &repeat, &[&b.kp]);
        let s = shown(&env, &format!("{coin:?} open (repeat)"), &repeat, &meta, 1);
        assert_eq!(s, open_shape(1, false), "{coin:?}");
        assert_eq!(
            (entries(&meta, 1), peak(&meta)),
            (12, 3),
            "the exposure exists"
        );

        env.warp(1);
        donate_to_trader(&mut env, &pm);
        let c = env.borrower();
        let normalising = client(open_ix(&pm, &c.key(), 0, q));
        let meta = send(&mut env, &normalising, &[&c.kp]);
        let _: TraderSwept = events::one(&meta);
        let s = shown(
            &env,
            &format!("{coin:?} open (normalising)"),
            &normalising,
            &meta,
            1,
        );
        assert_eq!(s, open_shape(2, true), "{coin:?}");
        assert_eq!((entries(&meta, 1), peak(&meta)), (16, 3));
    }

    // The growth of a short curve inside the sale (normalising too): the 151-byte curve's first
    // trade since the dump.
    let (mut env, pm, q) = setup(NEAR);
    env.tick(&pm, 1);
    donate_to_trader(&mut env, &pm);
    let b = env.borrower();
    // The curve's first trade since the dump: the 2026-10-08 build grows it to 166 bytes.
    assert!(pm.curve.growth_top_up(&env.svm) > 0);
    let ixs = client(open_ix(&pm, &b.key(), 0, q));
    let meta = send(&mut env, &ixs, &[&b.kp]);
    let s = shown(&env, "open (normalising, growth)", &ixs, &meta, 1);
    assert_eq!(s, with_growth(open_shape(2, true), true));
    assert_eq!((entries(&meta, 1), peak(&meta)), (16 + 1, 3));
    assert_eq!(pm.curve.growth_top_up(&env.svm), 0, "grown");
}

/// Exits on the curve: a close is its buy leg and event — 9 entries, with a creator fee or
/// without; 12 normalising; a completing buyback adds pump's `CompleteEvent` and short.win's
/// `VenueStateChanged` (11); a buyback past `rT` completes the curve by synthetic migration and
/// adds pump's `PostCompleteBuyEvent` too (12; pump-final §5.2, §6.6). One pump CPI, peak 3.
#[test]
fn close_short() {
    for coin in VARIANTS {
        let (mut env, pm, b, nonce, _) = with_position(coin);
        assert_eq!(
            has_creator(&env, &pm),
            coin != SPL,
            "legacy115 has no creator"
        );
        let ixs = client(close_ix(&pm, &b.key(), nonce));
        let meta = send(&mut env, &ixs, &[&b.kp]);
        let _: Close = events::one(&meta);
        let s = shown(&env, &format!("{coin:?} close"), &ixs, &meta, 1);
        assert_eq!(s, exit_shape(false, Buy::Plain), "{coin:?}");
        assert_eq!(pump_calls(&s), 1);
        assert_eq!((entries(&meta, 1), peak(&meta)), (9, 3));

        let (mut env, pm, b, nonce, _) = with_position(coin);
        donate_to_trader(&mut env, &pm);
        let ixs = client(close_ix(&pm, &b.key(), nonce));
        let meta = send(&mut env, &ixs, &[&b.kp]);
        let s = shown(
            &env,
            &format!("{coin:?} close (normalising)"),
            &ixs,
            &meta,
            1,
        );
        assert_eq!(s, exit_shape(true, Buy::Plain), "{coin:?}");
        assert_eq!(entries(&meta, 1), 12);
    }

    // A completing buyback: `→ PumpMigrating` in the instruction, its events before `Close`.
    let (mut env, pm, b, nonce, q) = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, q);
    env.warp(1);
    let ixs = client(close_ix(&pm, &b.key(), nonce));
    let meta = send(&mut env, &ixs, &[&b.kp]);
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    let s = shown(&env, "close (completing)", &ixs, &meta, 1);
    assert_eq!(s, exit_shape(false, Buy::Completing), "{s:?}");
    assert_eq!((entries(&meta, 1), peak(&meta)), (11, 3));

    // A buyback past `rT` (a straddle, `rT = q/2`): the synthetic completion, normalising too.
    let (mut env, pm, b, nonce, q) = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, q / 2);
    env.warp(1);
    donate_to_trader(&mut env, &pm);
    let ixs = client(close_ix(&pm, &b.key(), nonce));
    let meta = send(&mut env, &ixs, &[&b.kp]);
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    assert_eq!(fork::pump_post_complete_buy_events(&meta).len(), 1);
    let s = shown(&env, "close (normalising, synthetic)", &ixs, &meta, 1);
    assert_eq!(s, exit_shape(true, Buy::Synthetic), "{s:?}");
    assert_eq!(pump_calls(&s), 1);
    assert_eq!((entries(&meta, 1), peak(&meta)), (15, 3));
}

/// A liquidation after a real pump and an expiry at the deadline: the exit shape (9); past `rT`
/// (ECON's D@80 % position liquidated, an expiry on the near-complete curve) the synthetic
/// completion's (12; pump-final §6.6).
#[test]
fn liquidate_and_expire() {
    let (mut env, pm, b, nonce, keeper) = liquidatable_synthetic();
    env.crank(&pm);
    let ixs = keeper_tx(liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce));
    let meta = send(&mut env, &ixs, &[&keeper]);
    let _: Liquidate = events::one(&meta);
    assert_eq!(fork::pump_post_complete_buy_events(&meta).len(), 1);
    let s = shown(&env, "liquidate (synthetic)", &ixs, &meta, 1);
    assert_eq!(s, exit_shape(false, Buy::Synthetic), "{s:?}");
    assert_eq!(pump_calls(&s), 1);
    assert_eq!((entries(&meta, 1), peak(&meta)), (12, 3));

    let (mut env, pm, b, nonce, keeper) = expired_synthetic();
    env.crank(&pm);
    let ixs = keeper_tx(expire_ix(&pm, &keeper.pubkey(), &b.key(), nonce));
    let meta = send(&mut env, &ixs, &[&keeper]);
    let _: Expire = events::one(&meta);
    assert_eq!(fork::pump_post_complete_buy_events(&meta).len(), 1);
    let s = shown(&env, "expire (synthetic)", &ixs, &meta, 1);
    assert_eq!(s, exit_shape(false, Buy::Synthetic), "{s:?}");
    assert_eq!((entries(&meta, 1), peak(&meta)), (12, 3));

    for coin in VARIANTS {
        let (mut env, pm, b, nonce, keeper) = liquidatable(coin);
        env.crank(&pm);
        let ixs = keeper_tx(liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce));
        let meta = send(&mut env, &ixs, &[&keeper]);
        let _: Liquidate = events::one(&meta);
        let s = shown(&env, &format!("{coin:?} liquidate"), &ixs, &meta, 1);
        assert_eq!(s, exit_shape(false, Buy::Plain), "{coin:?}");
        assert_eq!((entries(&meta, 1), peak(&meta)), (9, 3));

        let (mut env, pm, b, nonce, keeper) = expired(coin);
        let ixs = keeper_tx(expire_ix(&pm, &keeper.pubkey(), &b.key(), nonce));
        let meta = send(&mut env, &ixs, &[&keeper]);
        let _: Expire = events::one(&meta);
        let s = shown(&env, &format!("{coin:?} expire"), &ixs, &meta, 1);
        assert_eq!(s, exit_shape(false, Buy::Plain), "{coin:?}");
        assert_eq!((entries(&meta, 1), peak(&meta)), (9, 3));
    }
}

/// `sol_settle`: the healthy no-op emits nothing (1 entry, peak 1); S3 on `PumpMigrating` (the
/// stuck migration at 70 min) and on `None` (graduated elsewhere, the detection set) pay in SOL:
/// their event only (2 entries, peak 2), no venue CPI.
#[test]
fn sol_settle() {
    for coin in VARIANTS {
        let (mut env, pm, b, nonce, _) = with_position(coin);
        let keeper = env.funded_keypair();
        let ixs = [sol_settle_ix(&env, &pm, &keeper.pubkey(), &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&keeper]);
        let s = shown(
            &env,
            &format!("{coin:?} sol_settle (no-op)"),
            &ixs,
            &meta,
            0,
        );
        assert!(s.is_empty(), "the healthy no-op emits nothing: {s:?}");
        assert_eq!(peak(&meta), 1);
    }
    let (mut env, pm, b, nonce, keeper) = s3_migrating();
    env.crank(&pm);
    let ixs = [sol_settle_ix(&env, &pm, &keeper.pubkey(), &b.key(), nonce)];
    let meta = send(&mut env, &ixs, &[&keeper]);
    assert_eq!(events::one::<SolSettle>(&meta).condition, 3);
    let s = shown(&env, "sol_settle S3 (PumpMigrating)", &ixs, &meta, 0);
    assert_eq!(s, [(P::Own, 2)]);
    assert_eq!((entries(&meta, 0), peak(&meta)), (2, 2));

    let (mut env, pm, b, nonce, keeper) = s3_none();
    let none = AsNone(&pm);
    env.crank(&none);
    let ixs = [sol_settle_ix(
        &env,
        &none,
        &keeper.pubkey(),
        &b.key(),
        nonce,
    )];
    let meta = send(&mut env, &ixs, &[&keeper]);
    assert_eq!(events::one::<SolSettle>(&meta).condition, 3);
    let s = shown(&env, "sol_settle S3 (None)", &ixs, &meta, 0);
    assert_eq!(s, [(P::Own, 2)]);
    assert_eq!((entries(&meta, 0), peak(&meta)), (2, 2));
}

/// `sweep_trader`'s pump token leg (ARCH §3.9, M5): the trader's lamports to `market_vault`, the
/// tokens of `trader_base_ata` to the lending vault, the event (4 entries, peak 2); nothing to
/// sweep: the event only.
#[test]
fn sweep_trader() {
    for coin in VARIANTS {
        let (mut env, pm, _) = setup(coin);
        donate_to_trader(&mut env, &pm);
        let ixs = [pump::sweep_trader_pump_ix(&env, &pm)];
        let meta = send(&mut env, &ixs, &[]);
        let ev: TraderSwept = events::one(&meta);
        assert_eq!((ev.tokens, ev.lamports), (777, 1_000_000));
        let s = shown(&env, &format!("{coin:?} sweep_trader"), &ixs, &meta, 0);
        assert_eq!(s, [(P::System, 2), (P::Token, 2), (P::Own, 2)]);
        assert_eq!((entries(&meta, 0), peak(&meta)), (4, 2));
        env.warp(1);
        let ixs = [pump::sweep_trader_pump_ix(&env, &pm)];
        let meta = send(&mut env, &ixs, &[]);
        let s = shown(
            &env,
            &format!("{coin:?} sweep_trader (empty)"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(s, [(P::Own, 2)]);
    }
}

/// The latch instructions on the dumped pump, pump-fees and PumpSwap ProgramData: their event
/// only (2 entries, peak 2).
#[test]
fn latch_instructions() {
    let (mut env, _) = Env::fork_pump_coin(T22);
    let ea = env.pause_authority.insecure_clone();
    for (program, venue) in [
        (PumpProgram::Pump, GUARD_PUMP),
        (PumpProgram::Fees, GUARD_PUMP_FEES),
        (PumpProgram::Amm, GUARD_PUMP_AMM),
    ] {
        let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_pump_deploy_slot(&mut env.svm, program, dumped + 1);
        let ixs = [ix::flag_venue_upgraded(venue)];
        let meta = send(&mut env, &ixs, &[]);
        let s = shown(
            &env,
            &format!("{program:?} flag_venue_upgraded"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(s, [(P::Own, 2)]);
        let ixs = [ix::revalidate_venue(&ea.pubkey(), venue, dumped + 1)];
        let meta = send(&mut env, &ixs, &[&ea]);
        let s = shown(
            &env,
            &format!("{program:?} revalidate_venue"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(s, [(P::Own, 2)]);
    }
}

// ---- worst-case transactions ----------------------------------------------------------------

/// Sends a worst-case transaction; asserts ≤ 64 entries and the stack peak; returns the
/// entries.
fn worst(env: &mut Env, what: &str, ixs: &[Instruction], signers: &[&Keypair], depth: u8) -> usize {
    let meta = send(env, ixs, signers);
    let n = tx_entries(&meta, ixs.len());
    println!(
        "{what}: {n} entries, peak {}, {} CU",
        peak(&meta),
        meta.compute_units_consumed
    );
    assert!(n <= MAX_TRACE, "{what}: {n} entries");
    assert_eq!(peak(&meta), depth, "{what}");
    assert!(peak(&meta) <= MAX_DEPTH);
    n
}

/// The largest client transactions: a wallet's first open, normalising, with the 151-byte curve's
/// growth to 166 bytes (2 + 17) and a normalising close past `rT`, completing the curve by
/// synthetic migration (2 + 15); an atomic sandwich of a normalising open with the curve's own
/// trades (a user's `buy_v2` 7 entries, 8 with a creator fee; `sell_v2` 4: pump's 2026-10-13
/// build still has both, and they trace longer than its v3 pair).
#[test]
fn worst_case_client_transactions() {
    let (mut env, pm, q) = setup(NEAR);
    env.tick(&pm, 1);
    donate_to_trader(&mut env, &pm);
    assert!(
        pm.curve.growth_top_up(&env.svm) > 0,
        "the first trade grows it"
    );
    let b = env.borrower();
    let ixs = with_cb(vec![open_ix(&pm, &b.key(), 0, q)]);
    let n = worst(
        &mut env,
        "CB×2 + open (normalising, growth)",
        &ixs,
        &[&b.kp],
        3,
    );
    assert_eq!(n, 2 + 16 + 1);
    env.tick(&pm, 1);
    buy_down_to(&mut env, &pm.curve, q / 2);
    env.warp(1);
    donate_to_trader(&mut env, &pm);
    let ixs = with_cb(vec![close_ix(&pm, &b.key(), 0)]);
    let n = worst(
        &mut env,
        "CB×2 + close (normalising, synthetic completion)",
        &ixs,
        &[&b.kp],
        3,
    );
    assert_eq!(n, 2 + 15);

    for coin in VARIANTS {
        let (mut env, pm, q) = setup(coin);
        let attacker = env.funded_keypair();
        let payer = env.payer.pubkey();
        let program = pm.curve.token_program.id();
        ok(env.send(
            &[
                fork::create_ata_idempotent_ix(&payer, &attacker.pubkey(), &pm.keys.mint, &program),
                fork::pump_init_uva_ix(&payer, &attacker.pubkey()),
            ],
            &[],
        ));
        let ata = pda::ata(&attacker.pubkey(), &pm.keys.mint, &program);
        let x = pm.curve.state(&env.svm).virtual_token_reserves / 50;
        let fr = fork::pump_fee_recipient(&env.svm, 1);
        let bb = fork::pump_buyback_fee_recipient(&env.svm, 1);
        let a = attacker.pubkey();
        let front = pm
            .curve
            .v2_ix(&env.svm, PumpV2::Buy, &a, &ata, &fr, &bb, x, u64::MAX);
        let back = pm
            .curve
            .v2_ix(&env.svm, PumpV2::Sell, &a, &ata, &fr, &bb, x, 0);
        env.tick(&pm, 1);
        donate_to_trader(&mut env, &pm);
        let v = env.borrower();
        let creator = has_creator(&env, &pm);
        let ixs = with_cb(vec![front, open_ix(&pm, &v.key(), 0, q), back]);
        let n = worst(
            &mut env,
            &format!("{coin:?} CB×2 + [buy_v2, open (normalising), sell_v2]"),
            &ixs,
            &[&attacker, &v.kp],
            3,
        );
        assert_eq!(n, 2 + (7 + usize::from(creator)) + 16 + 4);
    }
}

/// Five opens of one wallet fill the trace exactly (2 + 13 + 4·12 = 63, + 1 for the 151-byte
/// curve's growth to 166 bytes in the first sale on the 2026-10-08 build: 64); a sixth (76) is
/// refused by the runtime itself with `MaxInstructionTraceLengthExceeded`, before any hook could
/// blame short.win. (Each of `vT/800` on the near-complete curve: five stay under the wallet cap.)
#[test]
fn the_most_opens_of_one_wallet() {
    let (mut env, pm, _) = setup(NEAR);
    let q = pm.curve.state(&env.svm).virtual_token_reserves / 800;
    env.tick(&pm, 1);
    let grow = usize::from(pm.curve.growth_top_up(&env.svm) > 0);
    assert_eq!(grow, 1);
    let b = env.borrower();
    let opens = |k: u64| with_cb((0..k).map(|n| open_ix(&pm, &b.key(), n, q)).collect());
    let res = env.send(&opens(6), &[&b.kp]);
    let err = res.expect_err("76 entries");
    assert!(
        format!("{:?}", err.err).contains("MaxInstructionTraceLengthExceeded"),
        "{:?}",
        err.err
    );
    let n = worst(&mut env, "CB×2 + open ×5", &opens(5), &[&b.kp], 3);
    assert_eq!(n, 2 + 13 + grow + 4 * 12);
    assert_eq!(n, MAX_TRACE);
}

/// A keeper batch of four cranks on the four live curves of the dump, each just completed by a
/// third party: every crank moves its market to `PumpMigrating` (4 entries): 2 + 4·4 = 18.
#[test]
fn a_crank_batch_of_four_transitions() {
    let (mut env, first) = Env::fork_pump_coin(SPL);
    let mut markets = vec![add_market(&mut env, first).0];
    for coin in [T22, PumpCoin::Fresh, NEAR] {
        let c = env.load_pump_coin(coin);
        if coin == PumpCoin::Fresh {
            // As `pump_it`: the fresh curve's `rQ` is one lamport; buys first.
            let u = CurveUser::new(&mut env, &c);
            let q = u64::try_from(c.curve_state(&env.svm).vt / 7).unwrap();
            ok(u.buy(&mut env, &c, q, u64::MAX));
        }
        markets.push(add_market(&mut env, c).0);
    }
    for pm in &markets {
        env.crank(pm);
        buy_down_to(&mut env, &pm.curve, 0);
    }
    env.warp(1);
    let ixs = with_cb(markets.iter().map(crank_price_ix).collect());
    let n = worst(
        &mut env,
        "CB×2 + crank_price ×4 (→ PumpMigrating)",
        &ixs,
        &[],
        2,
    );
    assert_eq!(n, 2 + 4 * 4);
    for pm in &markets {
        assert_eq!(
            env.market(&pm.market()).venue_kind(),
            Some(VenueKind::PumpMigrating)
        );
    }
}

/// The keeper's exits with their crank: `[CB×2, crank, liquidate | expire]` (2 + 2 + 9; past `rT`,
/// a synthetic completion, 2 + 2 + 12), `[CB×2, crank, sol_settle S3]` on `PumpMigrating` and on
/// `None` (2 + 2 + 2); the latch duty (§7.1 duty 10): the three pump programs flagged in one
/// transaction (2 + 3·2).
#[test]
fn worst_case_keeper_transactions() {
    let (mut env, pm, b, nonce, keeper) = liquidatable_synthetic();
    let ixs = with_cb(vec![
        crank_price_ix(&pm),
        liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce),
    ]);
    let n = worst(
        &mut env,
        "CB×2 + crank + liquidate (synthetic)",
        &ixs,
        &[&keeper],
        3,
    );
    assert_eq!(n, 2 + 2 + 12);
    let (mut env, pm, b, nonce, keeper) = expired_synthetic();
    let ixs = with_cb(vec![
        crank_price_ix(&pm),
        expire_ix(&pm, &keeper.pubkey(), &b.key(), nonce),
    ]);
    let n = worst(
        &mut env,
        "CB×2 + crank + expire (synthetic)",
        &ixs,
        &[&keeper],
        3,
    );
    assert_eq!(n, 2 + 2 + 12);

    for coin in VARIANTS {
        let (mut env, pm, b, nonce, keeper) = liquidatable(coin);
        let ixs = with_cb(vec![
            crank_price_ix(&pm),
            liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce),
        ]);
        let n = worst(
            &mut env,
            &format!("{coin:?} CB×2 + crank + liquidate"),
            &ixs,
            &[&keeper],
            3,
        );
        assert_eq!(n, 2 + 2 + 9);

        let (mut env, pm, b, nonce, keeper) = expired(coin);
        env.warp(1);
        let ixs = with_cb(vec![
            crank_price_ix(&pm),
            expire_ix(&pm, &keeper.pubkey(), &b.key(), nonce),
        ]);
        let n = worst(
            &mut env,
            &format!("{coin:?} CB×2 + crank + expire"),
            &ixs,
            &[&keeper],
            3,
        );
        assert_eq!(n, 2 + 2 + 9);
    }

    let (mut env, pm, b, nonce, keeper) = s3_migrating();
    let ixs = with_cb(vec![
        crank_price_ix(&pm),
        sol_settle_ix(&env, &pm, &keeper.pubkey(), &b.key(), nonce),
    ]);
    let n = worst(
        &mut env,
        "CB×2 + crank + sol_settle S3 (PumpMigrating)",
        &ixs,
        &[&keeper],
        2,
    );
    assert_eq!(n, 2 + 2 + 2);

    let (mut env, pm, b, nonce, keeper) = s3_none();
    let none = AsNone(&pm);
    let ixs = with_cb(vec![
        crank_price_ix(&none),
        sol_settle_ix(&env, &none, &keeper.pubkey(), &b.key(), nonce),
    ]);
    let n = worst(
        &mut env,
        "CB×2 + crank + sol_settle S3 (None)",
        &ixs,
        &[&keeper],
        2,
    );
    assert_eq!(n, 2 + 2 + 2);

    let (mut env, _) = Env::fork_pump_coin(T22);
    for (program, venue) in [
        (PumpProgram::Pump, GUARD_PUMP),
        (PumpProgram::Fees, GUARD_PUMP_FEES),
        (PumpProgram::Amm, GUARD_PUMP_AMM),
    ] {
        let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_pump_deploy_slot(&mut env.svm, program, dumped + 1);
    }
    let ixs = with_cb(vec![
        ix::flag_venue_upgraded(GUARD_PUMP),
        ix::flag_venue_upgraded(GUARD_PUMP_FEES),
        ix::flag_venue_upgraded(GUARD_PUMP_AMM),
    ]);
    let n = worst(
        &mut env,
        "CB×2 + flag_venue_upgraded ×3 (pump)",
        &ixs,
        &[],
        2,
    );
    assert_eq!(n, 2 + 3 * 2);
}

/// The coins the shapes name: SPL Token without a creator, Token-2022 with one.
#[test]
fn variants_cover_both_token_programs() {
    let programs: Vec<Pubkey> = VARIANTS
        .iter()
        .map(|c| Env::fork_pump_coin(*c).1.token_program.id())
        .collect();
    assert_eq!(programs, [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID]);
}
