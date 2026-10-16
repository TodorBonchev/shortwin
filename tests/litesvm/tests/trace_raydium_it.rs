//! T-RAY-TRACE and T-TRACE on Raydium, with the stack depth (ARCH §6.2, §6.3, §8.2; C1-1, C1-8;
//! U4): every Raydium-path instruction of the release short.win, on the dumped AMM v4 and CPMM
//! programs (SPL and Token-2022 bases), read off the transaction's own inner instructions.
//!
//! - **Per instruction**: the exact instruction-trace entries (the instruction plus every CPI at
//!   any depth) and the exact call shape — which program at which stack height — of
//!   `create_market`, `crank_price` (roll and record; a halted pool starting both route chains),
//!   `open_short` (a wallet's first open, a repeat borrower, with normalisation), `close_short`
//!   (plain, with normalisation), `liquidate`, `expire`, `sol_settle` (no-op; S3 on a halted
//!   pool), `flag_venue_upgraded` and `revalidate_venue`. Every Raydium leg is one venue CPI at
//!   height 2 whose two token transfers sit at height 3, the peak: short.win keeps two spare
//!   levels (ARCH §6.3; the bench `tests/bench/m3_cu` runs them through one and two wrapper
//!   programs).
//! - **Worst-case transactions** (ComputeBudget included): the largest open and close with
//!   normalisation, an atomic sandwich of an open with real Raydium swaps, the most opens of one
//!   wallet in one transaction, a batch of six cranks that all start a route chain, the keeper's
//!   `[crank, liquidate | expire | sol_settle S3]` and both latch flags — each ≤ 64
//!   (`MAX_INSTRUCTION_TRACE_LENGTH`); one open more is refused by the runtime itself.
//!
//! Figures against ARCH §6.2's Raydium column: an open is 15 entries for a wallet's first
//! position (the table's 13 omits the position `init` and the exposure `init_if_needed`, as M2
//! found on the mock), 14 for a repeat borrower, 18 with normalisation; exits are 9 (12 with
//! normalisation); a crank that starts a route chain is 3 (its `RouteChainChanged` event).
//!
//! Every transaction goes through `Env::send`: the invariant hooks run after each instruction.
//! A keeper's decision to send is a `simulate_transaction` on the current state (as a keeper
//! simulates), never a committed probe.

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::types::TransactionMetadata;
use shortwin::constants::{
    GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4, RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_V4_PROGRAM_ID,
    TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use shortwin::events::{Expire, Liquidate, SolSettle};
use shortwin::state::VenueKind;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::errors::ok;
use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, RaydiumProgram, V4Fixture};
use shortwin_litesvm_tests::raydium::{
    RayFixture, RayMarket, RaySpec, create_ray_market_ix, lp_mint, set_mint_supply,
};
use shortwin_litesvm_tests::{Env, events, ix};
use shortwin_math::fees::Direction;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;

const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);
const CREATOR_BOTH: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorBoth);
const WSOL_T1: RayFixture = RayFixture::Cpmm(CpmmFixture::WsolToken1);

/// AMM v4 with an SPL base, CPMM with an SPL base, CPMM with a Token-2022 base (AMM v4 is SPL
/// Token only, R-RAY §2.1).
const VARIANTS: [RayFixture; 3] = [FART, USELESS, T22];

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
    /// The Raydium program of the market.
    Venue,
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
                    } else if p == RAYDIUM_V4_PROGRAM_ID || p == RAYDIUM_CPMM_PROGRAM_ID {
                        P::Venue
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

/// A sell leg (ARCH §2.4.1, Raydium row): `ApproveChecked` of the trader on `pool_vault`, the
/// ephemeral WSOL account funded by the borrower (System `transfer`, `allocate`, `assign`, then
/// `InitializeAccount3`), the swap with its two transfers one level down, `CloseAccount`.
const SELL_LEG: [(P, u8); 9] = [
    (P::Token, 2),
    (P::System, 2),
    (P::System, 2),
    (P::System, 2),
    (P::Token, 2),
    (P::Venue, 2),
    (P::Token, 3),
    (P::Token, 3),
    (P::Token, 2),
];

/// A buy leg: the ephemeral WSOL account funded from the position by a direct lamport move
/// (`allocate` carrying the position, `assign`, `InitializeAccount3`), the swap with its two
/// transfers one level down, `CloseAccount`. The keeper's and the protocol's shares are direct
/// lamport moves too: an exit is its buy leg plus its event.
const BUY_LEG: [(P, u8); 7] = [
    (P::System, 2),
    (P::System, 2),
    (P::Token, 2),
    (P::Venue, 2),
    (P::Token, 3),
    (P::Token, 3),
    (P::Token, 2),
];

/// Trader normalisation (ARCH §2.4 rule 5): the donated lamports of the trader PDA and of the
/// `trader_wsol` address swept (two System transfers the PDAs sign) and `TraderSwept`.
const NORMALISE: [(P, u8); 3] = [(P::System, 2), (P::System, 2), (P::Own, 2)];

/// An open: `inits` System creations (the position; a first open also the exposure record), the
/// normalisation if any, the sell leg, the margin and fee transfers, the event.
fn open_shape(inits: usize, normalise: bool) -> Vec<(P, u8)> {
    let mut v = vec![(P::System, 2); inits];
    if normalise {
        v.extend(NORMALISE);
    }
    v.extend(SELL_LEG);
    v.extend([(P::System, 2), (P::System, 2), (P::Own, 2)]);
    v
}

/// An exit: the normalisation if any, the buy leg, the event.
fn exit_shape(normalise: bool) -> Vec<(P, u8)> {
    let mut v = if normalise {
        NORMALISE.to_vec()
    } else {
        Vec::new()
    };
    v.extend(BUY_LEG);
    v.push((P::Own, 2));
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

/// The number of venue CPIs of a shape, each at height 2 with its token CPIs at height 3.
fn venue_calls(s: &[(P, u8)]) -> usize {
    assert!(
        s.iter().all(|&(p, h)| match p {
            P::Venue | P::Own | P::System => h == 2,
            P::Token => h == 2 || h == 3,
            P::Other => false,
        }),
        "{s:?}"
    );
    s.iter().filter(|(p, _)| *p == P::Venue).count()
}

// ---- scenarios ------------------------------------------------------------------------------

/// `min(X/400, 2 SOL of notional)`, the `ray_it` position size.
fn q_of(env: &Env, pool: &AnyPool) -> u64 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture` (burning `WSOL_T1`'s LP first: it is not eligible as dumped), lends `40·Q`,
/// waits out the listing window, cranks once.
fn add_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    if fixture == WSOL_T1 {
        set_mint_supply(&mut env.svm, &lp_mint(&pool), 0);
    }
    ok(env.list_ray(&pool, fixture.hint()));
    let q = q_of(env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}

fn setup(fixture: RayFixture) -> (Env, RayMarket, u64) {
    let mut env = Env::ray_fork();
    let (rm, q) = add_market(&mut env, fixture);
    (env, rm, q)
}

fn open_ix(rm: &RayMarket, b: &Pubkey, nonce: u64, q: u64) -> Instruction {
    open_short_ix(rm, b, nonce, q, 0, u64::MAX, u64::MAX, ExtrasSel::NONE, &[])
}

fn close_ix(rm: &RayMarket, b: &Pubkey, nonce: u64) -> Instruction {
    close_short_ix(rm, b, nonce, u64::MAX, ExtrasSel::NONE, &[])
}

/// A position of `q` two cranked seconds old.
fn with_position(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, u64) {
    let (mut env, rm, q) = setup(fixture);
    let mut b = env.borrower();
    let (res, nonce) = env.open(&rm, &mut b, q);
    ok(res);
    env.tick(&rm, 2);
    (env, rm, b, nonce, q)
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

/// Donates to the trader PDA and the `trader_wsol` address, so the next leg normalises (ARCH
/// §2.4 rule 5).
fn donate_to_traders(env: &mut Env, rm: &RayMarket) {
    let griefer = env.funded_keypair();
    ok(env.send(
        &[
            transfer(&griefer, &rm.keys.trader, 1_000_000),
            transfer(&griefer, &rm.trader_wsol, 1_000_000),
        ],
        &[&griefer],
    ));
}

/// `SetComputeUnitLimit` and `SetComputeUnitPrice`.
fn compute_budget() -> [Instruction; 2] {
    let mut limit = vec![2u8];
    limit.extend_from_slice(&1_400_000u32.to_le_bytes());
    let mut price = vec![3u8];
    price.extend_from_slice(&1u64.to_le_bytes());
    [
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
    ]
}

fn with_cb(ixs: Vec<Instruction>) -> Vec<Instruction> {
    let mut v = compute_budget().to_vec();
    v.extend(ixs);
    v
}

/// `simulate_transaction` of `ixs` on the current state (nothing is committed): its metadata if
/// it would succeed.
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

/// A whale's real buy of `sol` lamports of the token.
fn whale_buy(env: &mut Env, rm: &RayMarket, sol: u64) {
    let (tok, program) = rm.pool.token();
    let whale = fork::trader(env, &tok, program, sol + 1_000_000_000, 0);
    ok(fork::swap(
        env,
        &rm.pool,
        &whale,
        Direction::Buy,
        fork::Swap::ExactIn(sol, 0),
    ));
}

/// A liquidatable position: the price pumped ≈ 2.2× by a real buy, then cranked second by
/// second until the keeper's simulated `[crank, liquidate]` of the next second succeeds. The
/// clock is at that next second, uncranked.
fn liquidatable(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, Keypair) {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    env.tick(&rm, 1);
    let (_, y) = rm.reserves(&env.svm).unwrap();
    whale_buy(&mut env, &rm, y / 1_000 * 484);
    let keeper = env.funded_keypair();
    for _ in 0..60 {
        env.warp(1);
        let ixs = [
            crank_price_ix(&rm),
            liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
        ];
        if simulate(&env, &ixs, &[&keeper]).is_some() {
            return (env, rm, b, nonce, keeper);
        }
        env.crank(&rm);
    }
    panic!("{fixture:?}: never liquidatable");
}

/// An expired healthy position, three cranked seconds past its deadline.
fn expired(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, Keypair) {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    let pos = env.position(&rm, &b.key(), nonce);
    let slot = env.clock().slot;
    env.set_clock(pos.deadline + 1, slot + 1_600_000);
    env.tick(&rm, 3);
    let keeper = env.funded_keypair();
    (env, rm, b, nonce, keeper)
}

/// Halts `rm`'s pool with the pool admin's own instruction (AMM v4 `SetParams` status 2, CPMM
/// `update_pool_status` 4 = swap disabled).
fn halt(env: &mut Env, rm: &RayMarket) {
    let ix = match &rm.pool {
        AnyPool::V4(p) => fork::v4_set_params_ix(p, 0, 2),
        AnyPool::Cpmm(p) => fork::cpmm_update_pool_status_ix(p, 4),
    };
    ok(fork::raydium_admin_send(env, ix));
}

/// An expired position on a halted pool whose S3 chain has run `s3_wait_s` (ARCH §5.3; as
/// `ray_it::route`): the clock at the second where the keeper's simulated `[crank, sol_settle]`
/// settles S3, uncranked.
fn s3_due(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, Keypair) {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    let pos = env.position(&rm, &b.key(), nonce);
    halt(&mut env, &rm);
    let slot = env.clock().slot;
    env.set_clock(pos.deadline - 5, slot + 1_500_000);
    env.crank(&rm);
    let keeper = env.funded_keypair();
    for _ in 0..40 {
        env.warp(30);
        let ixs = [
            crank_price_ix(&rm),
            sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce),
        ];
        if let Some(meta) = simulate(&env, &ixs, &[&keeper])
            && events::of::<SolSettle>(&meta)
                .first()
                .is_some_and(|e| e.condition == 3)
        {
            return (env, rm, b, nonce, keeper);
        }
        env.crank(&rm);
    }
    panic!("{fixture:?}: S3 never due");
}

/// Sends `ixs` (hooks on); returns the metadata.
fn send(env: &mut Env, ixs: &[Instruction], signers: &[&Keypair]) -> TransactionMetadata {
    ok(env.send(ixs, signers))
}

// ---- per instruction ------------------------------------------------------------------------

#[test]
fn create_market() {
    for fixture in VARIANTS {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(fixture);
        let creator = env.funded_keypair();
        let ixs = [create_ray_market_ix(
            &env.svm,
            &creator.pubkey(),
            &pool,
            fixture.hint(),
        )];
        let meta = send(&mut env, &ixs, &[&creator]);
        let s = shown(&env, &format!("{fixture:?} create_market"), &ixs, &meta, 0);
        // `Market` and `MarketVault` inits, the lending vault's creation and
        // `InitializeAccount3`, the creation fee, the event; the pool is only read.
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
            "{fixture:?}"
        );
        assert_eq!(venue_calls(&s), 0);
        assert_eq!((entries(&meta, 0), peak(&meta)), (7, 2));
    }
}

#[test]
fn crank_price() {
    for fixture in VARIANTS {
        let (mut env, rm, _) = setup(fixture);
        env.warp(1);
        let ixs = [crank_price_ix(&rm)];
        let meta = send(&mut env, &ixs, &[]);
        let s = shown(&env, &format!("{fixture:?} crank_price"), &ixs, &meta, 0);
        assert_eq!(s, [(P::Own, 2)], "roll + record: the event only");
        // Halted: the prologue starts the S3 and `pool_fail` chains (`RouteChainChanged`).
        halt(&mut env, &rm);
        env.warp(1);
        let meta = send(&mut env, &ixs, &[]);
        let s = shown(
            &env,
            &format!("{fixture:?} crank_price halted"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(s, [(P::Own, 2), (P::Own, 2)], "+ RouteChainChanged");
        let m = env.market(&rm.market());
        assert_eq!(
            (m.no_route_since, m.pool_fail_since),
            (env.now(), env.now())
        );
    }
}

#[test]
fn open_short() {
    for fixture in VARIANTS {
        let (mut env, rm, q) = setup(fixture);
        env.tick(&rm, 1);
        let b = env.borrower();
        let first = [open_ix(&rm, &b.key(), 0, q)];
        let meta = send(&mut env, &first, &[&b.kp]);
        let s = shown(
            &env,
            &format!("{fixture:?} open_short (first)"),
            &first,
            &meta,
            0,
        );
        assert_eq!(s, open_shape(2, false), "{fixture:?}");
        assert_eq!((entries(&meta, 0), peak(&meta)), (15, 3));

        let repeat = [open_ix(&rm, &b.key(), 1, q / 2)];
        let meta = send(&mut env, &repeat, &[&b.kp]);
        let s = shown(
            &env,
            &format!("{fixture:?} open_short (repeat)"),
            &repeat,
            &meta,
            0,
        );
        assert_eq!(s, open_shape(1, false), "{fixture:?}");
        assert_eq!(
            (entries(&meta, 0), peak(&meta)),
            (14, 3),
            "the exposure exists"
        );

        donate_to_traders(&mut env, &rm);
        let c = env.borrower();
        let normalising = [open_ix(&rm, &c.key(), 0, q / 2)];
        let meta = send(&mut env, &normalising, &[&c.kp]);
        let s = shown(
            &env,
            &format!("{fixture:?} open_short (normalising)"),
            &normalising,
            &meta,
            0,
        );
        assert_eq!(s, open_shape(2, true), "{fixture:?}");
        assert_eq!((entries(&meta, 0), peak(&meta)), (18, 3));
    }
}

#[test]
fn close_short() {
    for fixture in VARIANTS {
        let (mut env, rm, b, nonce, _) = with_position(fixture);
        let ixs = [close_ix(&rm, &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&b.kp]);
        let s = shown(&env, &format!("{fixture:?} close_short"), &ixs, &meta, 0);
        assert_eq!(s, exit_shape(false), "{fixture:?}");
        assert_eq!((entries(&meta, 0), peak(&meta)), (9, 3));

        let (mut env, rm, b, nonce, _) = with_position(fixture);
        donate_to_traders(&mut env, &rm);
        let ixs = [close_ix(&rm, &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&b.kp]);
        let s = shown(
            &env,
            &format!("{fixture:?} close_short (normalising)"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(s, exit_shape(true), "{fixture:?}");
        assert_eq!((entries(&meta, 0), peak(&meta)), (12, 3));
    }
}

#[test]
fn liquidate_and_expire() {
    for fixture in VARIANTS {
        let (mut env, rm, b, nonce, keeper) = liquidatable(fixture);
        env.crank(&rm);
        let ixs = [liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&keeper]);
        let _: Liquidate = events::one(&meta);
        let s = shown(&env, &format!("{fixture:?} liquidate"), &ixs, &meta, 0);
        assert_eq!(s, exit_shape(false), "{fixture:?}");
        assert_eq!((entries(&meta, 0), peak(&meta)), (9, 3));

        let (mut env, rm, b, nonce, keeper) = expired(fixture);
        let ixs = [expire_ix(&rm, &keeper.pubkey(), &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&keeper]);
        let _: Expire = events::one(&meta);
        let s = shown(&env, &format!("{fixture:?} expire"), &ixs, &meta, 0);
        assert_eq!(s, exit_shape(false), "{fixture:?}");
        assert_eq!((entries(&meta, 0), peak(&meta)), (9, 3));
    }
}

#[test]
fn sol_settle() {
    for fixture in VARIANTS {
        let (mut env, rm, b, nonce, _) = with_position(fixture);
        let keeper = env.funded_keypair();
        let ixs = [sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&keeper]);
        let s = shown(
            &env,
            &format!("{fixture:?} sol_settle (no-op)"),
            &ixs,
            &meta,
            0,
        );
        assert!(s.is_empty(), "the healthy no-op emits nothing: {s:?}");
        assert_eq!(peak(&meta), 1);

        let (mut env, rm, b, nonce, keeper) = s3_due(fixture);
        env.crank(&rm);
        let ixs = [sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce)];
        let meta = send(&mut env, &ixs, &[&keeper]);
        assert_eq!(events::one::<SolSettle>(&meta).condition, 3);
        let s = shown(
            &env,
            &format!("{fixture:?} sol_settle (S3)"),
            &ixs,
            &meta,
            0,
        );
        assert_eq!(venue_calls(&s), 0, "S3 pays in SOL: no venue");
        assert_eq!(peak(&meta), 2);
        assert_eq!(entries(&meta, 0), 2, "{fixture:?}: {s:?}");
    }
}

#[test]
fn latch_instructions() {
    let mut env = Env::ray_fork();
    let ea = env.pause_authority.insecure_clone();
    for (program, venue) in [
        (RaydiumProgram::AmmV4, GUARD_RAYDIUM_V4),
        (RaydiumProgram::Cpmm, GUARD_RAYDIUM_CPMM),
    ] {
        let dumped = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_deploy_slot(&mut env.svm, program, dumped + 1);
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

#[test]
fn worst_case_client_transactions() {
    for fixture in VARIANTS {
        // The largest open and close: a wallet's first open, and a close, both normalising.
        let (mut env, rm, q) = setup(fixture);
        env.tick(&rm, 1);
        donate_to_traders(&mut env, &rm);
        let b = env.borrower();
        let ixs = with_cb(vec![open_ix(&rm, &b.key(), 0, q)]);
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + open (normalising)"),
            &ixs,
            &[&b.kp],
            3,
        );
        assert_eq!(n, 2 + 18);
        env.tick(&rm, 1);
        donate_to_traders(&mut env, &rm);
        let ixs = with_cb(vec![close_ix(&rm, &b.key(), 0)]);
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + close (normalising)"),
            &ixs,
            &[&b.kp],
            3,
        );
        assert_eq!(n, 2 + 12);

        // An atomic sandwich of a normalising open with the venue's own swaps (3 entries each).
        let (tok, program) = rm.pool.token();
        let x = rm.reserves(&env.svm).unwrap().0;
        let attacker = fork::trader(&mut env, &tok, program, 1_000_000_000_000, x / 20);
        let sol_out = rm
            .pool
            .quote_exact_in(&env.svm, Direction::Sell, x / 50)
            .unwrap()
            .proceeds;
        let a = attacker.owner.pubkey();
        let front = rm.pool.swap_ix(
            &env.svm,
            &a,
            &attacker,
            Direction::Sell,
            fork::Swap::ExactIn(x / 50, 0),
        );
        let back = rm.pool.swap_ix(
            &env.svm,
            &a,
            &attacker,
            Direction::Buy,
            fork::Swap::ExactIn(sol_out, 0),
        );
        env.tick(&rm, 1);
        donate_to_traders(&mut env, &rm);
        let v = env.borrower();
        let ixs = with_cb(vec![front, open_ix(&rm, &v.key(), 0, q / 2), back]);
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + [swap, open (normalising), swap]"),
            &ixs,
            &[&attacker.owner, &v.kp],
            3,
        );
        assert_eq!(n, 2 + 3 + 18 + 3);
    }
}

/// Four opens of one wallet fit (2 + 15 + 3·14 = 59); a fifth (73) is refused by the runtime
/// itself with `MaxInstructionTraceLengthExceeded`, before any hook could blame short.win.
#[test]
fn the_most_opens_of_one_wallet() {
    for fixture in [FART, T22] {
        let (mut env, rm, q) = setup(fixture);
        env.tick(&rm, 1);
        let b = env.borrower();
        let opens = |k: u64| with_cb((0..k).map(|n| open_ix(&rm, &b.key(), n, q / 8)).collect());
        let res = env.send(&opens(5), &[&b.kp]);
        let err = res.expect_err("73 entries");
        assert!(
            format!("{:?}", err.err).contains("MaxInstructionTraceLengthExceeded"),
            "{:?}",
            err.err
        );
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + open ×4"),
            &opens(4),
            &[&b.kp],
            3,
        );
        assert_eq!(n, 2 + 15 + 3 * 14);
    }
}

/// A keeper batch of six cranks on six Raydium markets whose pools were all just halted: every
/// crank starts its route chains (3 entries): 2 + 3·6 = 20.
#[test]
fn a_crank_batch_starting_six_route_chains() {
    let mut env = Env::ray_fork();
    let markets: Vec<RayMarket> = [FART, USELESS, T22, CREATOR_FEE, CREATOR_BOTH, WSOL_T1]
        .into_iter()
        .map(|f| add_market(&mut env, f).0)
        .collect();
    for rm in &markets {
        env.crank(rm);
        halt(&mut env, rm);
    }
    env.warp(1);
    let ixs = with_cb(markets.iter().map(crank_price_ix).collect());
    let n = worst(&mut env, "CB×2 + crank_price ×6 (all halted)", &ixs, &[], 2);
    assert_eq!(n, 2 + 3 * 6);
    for rm in &markets {
        assert_eq!(env.market(&rm.market()).no_route_since, env.now());
    }
}

/// The keeper's exits with their crank: `[CB×2, crank, liquidate]` and `[CB×2, crank, expire]`
/// (2 + 2 + 9), `[CB×2, crank, sol_settle S3]` (2 + 2 + 2); and the latch duty (§7.1 duty
/// 10): both Raydium programs flagged in one transaction (2 + 2 + 2).
#[test]
fn worst_case_keeper_transactions() {
    for fixture in VARIANTS {
        let (mut env, rm, b, nonce, keeper) = liquidatable(fixture);
        let ixs = with_cb(vec![
            crank_price_ix(&rm),
            liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
        ]);
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + crank + liquidate"),
            &ixs,
            &[&keeper],
            3,
        );
        assert_eq!(n, 2 + 2 + 9);

        let (mut env, rm, b, nonce, keeper) = expired(fixture);
        env.warp(1);
        let ixs = with_cb(vec![
            crank_price_ix(&rm),
            expire_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
        ]);
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + crank + expire"),
            &ixs,
            &[&keeper],
            3,
        );
        assert_eq!(n, 2 + 2 + 9);

        let (mut env, rm, b, nonce, keeper) = s3_due(fixture);
        let ixs = with_cb(vec![
            crank_price_ix(&rm),
            sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce),
        ]);
        let n = worst(
            &mut env,
            &format!("{fixture:?} CB×2 + crank + sol_settle S3"),
            &ixs,
            &[&keeper],
            2,
        );
        assert_eq!(n, 2 + 2 + 2);
    }
    let mut env = Env::ray_fork();
    for (program, venue) in [
        (RaydiumProgram::AmmV4, GUARD_RAYDIUM_V4),
        (RaydiumProgram::Cpmm, GUARD_RAYDIUM_CPMM),
    ] {
        let dumped = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_deploy_slot(&mut env.svm, program, dumped + 1);
    }
    let ixs = with_cb(vec![
        ix::flag_venue_upgraded(GUARD_RAYDIUM_V4),
        ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM),
    ]);
    let n = worst(
        &mut env,
        "CB×2 + flag_venue_upgraded ×2 (both Raydium programs)",
        &ixs,
        &[],
        2,
    );
    assert_eq!(n, 2 + 2 + 2);
}

/// The venue kinds the shapes above name.
#[test]
fn variants_cover_both_programs_and_both_token_programs() {
    let kinds: Vec<VenueKind> = VARIANTS.iter().map(|f| f.kind()).collect();
    assert_eq!(
        kinds,
        [
            VenueKind::RaydiumV4,
            VenueKind::RaydiumCpmm,
            VenueKind::RaydiumCpmm
        ]
    );
    let mut env = Env::ray_fork();
    let programs: Vec<Pubkey> = VARIANTS
        .iter()
        .map(|f| env.load_ray_pool(*f).token().1.id())
        .collect();
    assert_eq!(
        programs,
        [TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID]
    );
}
