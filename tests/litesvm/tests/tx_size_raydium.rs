//! T-SIZE on Raydium (ARCH §6.4, §6.2, §8.2; M3 exit criterion "T-SIZE … for Raydium"): every
//! Raydium transaction the protocol and its keeper send — `open_short`, `close_short`,
//! `liquidate`, `expire`, `sol_settle`, `crank_price` batches and (M4) the pool-selection
//! instructions `propose_pool` / `execute_pool_switch` — fits **1,232 B and 64
//! accounts** as a v0 transaction through the protocol ALT (A28) in its **worst case**, and
//! **executes** that way on the mainnet fork (the dumped AMM v4 and CPMM programs and pools),
//! with the invariant hooks after every instruction.
//!
//! **Worst case** of each transaction:
//! - the compute budget both ways the protocol sends it: `SetComputeUnitLimit` and
//!   `SetComputeUnitPrice` (ARCH §6.1: clients always prepend the limit; the keeper adds a price);
//! - a **separate fee payer** (two signatures, the payer's key inline) — a wallet paying for itself
//!   is one signature and one key smaller; both are measured, the separate payer is asserted;
//! - a first-time borrower (the position and the exposure records created by the open);
//! - the **largest extras set** on every trading instruction (A41): the venue's `VenueExtras`
//!   account and `EXTRAS_MAX_ENTRIES` = 4 fixed entries, all distinct keys, published by the
//!   Emergency key for the sell and the buy kinds and selected as `current` — AMM v4 v2 and
//!   CPMM accept trailing accounts (R-RAY §9.6), so these transactions trade normally;
//! - both pool programs and both token programs: AMM v4 (Fartcoin), CPMM on SPL Token
//!   (USELESS) and on Token-2022, WSOL as token 0 and as token 1.
//!
//! **Two tables** (both asserted): **(a)** A28 exactly as ARCH §6.4 enumerates it — the harness's
//! `raydium::protocol_alt_addresses()` (programs, authorities, SPL Token, Token-2022, ATA,
//! System, WSOL, the 21 CPMM `AmmConfig`s, short.win's `config`, `venue_guard`, `oracle_cache`
//! and event authority; the pump part adds nothing a Raydium transaction names), where the
//! `VenueExtras` account stays inline; **(b)** the table `scripts/build-alt.sh` builds
//! (`shortwin_keeper::alt`), which also holds the four `VenueExtras` PDAs. The keeper's scenario
//! `raydium_alt` builds (b) on chain with the script's own code and checks that it holds every
//! address of (a) and those PDAs, so these sizes are the real table's.
//!
//! **Crank batches**: the largest `k` whose v0 batch fits, for six markets on the fork (one AMM v4,
//! five CPMM), CPMM first (their ReadSet is a key longer): at least the keeper's v0 batch size
//! (4, ARCH §7.1 duty 7), executed as one transaction. Also measured: the keeper's composite
//! `ComputeBudget ×2 + crank_price + liquidate` (ARCH §6.2) and an exit carrying a Jito tip
//! transfer (duty 3's bundle path).
//!
//! **Pool selection** (M4; ARCH §3.9, §7.1 duties 5 and 12): the keeper's `propose_pool` and
//! `execute_pool_switch` in their largest shapes — a CPMM incumbent's ReadSet and a CPMM
//! candidate's (5 + 5 remaining accounts; AMM v4 ReadSets are a key shorter), and a list-only
//! market's detection set with a CPMM candidate — with both ComputeBudget instructions and a
//! separate fee payer; each executes through the table (a list-only adoption, a zero-position
//! adoption, a fast-path window opened on a halted incumbent and its execution).
//!
//! Run with `-- --nocapture` for the table.

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::types::TransactionMetadata;
use shortwin::errors::ShortwinError as E;
use shortwin::events::{
    Close, Expire, Liquidate, Open, POOL_KIND_FAST, POOL_KIND_LISTING, POOL_KIND_ZERO_POSITION,
    PoolProposed, PoolSwitched, PriceCranked, SolSettle,
};
use shortwin::state::{ENTRY_FIXED, EXTRAS_IX_BUY, EXTRAS_IX_SELL, EXTRAS_MAX_ENTRIES, VenueKind};
use shortwin::venues::{EXTRAS_SET_CURRENT, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, open_short_ix, set_venue_extras_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{custom_code, ok};
use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, V4Fixture};
use shortwin_litesvm_tests::pool_select::{
    detection_set, execute_pool_switch_ix, plant_cpmm, propose_pool_ix, spec_like,
};
use shortwin_litesvm_tests::raydium::{
    self, RayFixture, RayMarket, RaySpec, install_alt, protocol_alt_addresses,
};
use shortwin_litesvm_tests::{Env, TxResult, events, invariants, pda, read};
use shortwin_math::fees::Direction;
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, Hash, Message, VersionedMessage, v0};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

const TX_MAX: usize = 1_232;
const ACCOUNTS_MAX: usize = 64;
/// The keeper's v0 crank batch (ARCH §7.1 duty 7; `KeeperConfig::batch_v0`).
const KEEPER_BATCH_V0: usize = 4;

const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);
const CREATOR_BOTH: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorBoth);
const WSOL_T1: RayFixture = RayFixture::Cpmm(CpmmFixture::WsolToken1);

const COMPUTE_BUDGET: Pubkey =
    anchor_lang::prelude::pubkey!("ComputeBudget111111111111111111111111111111");

// ---- transactions ------------------------------------------------------------------------------

/// `SetComputeUnitLimit(1.4 M)` and `SetComputeUnitPrice` (fixed-width data: any value has the
/// same size).
fn compute_budget() -> [Instruction; 2] {
    let mut limit = vec![2u8];
    limit.extend_from_slice(&1_400_000u32.to_le_bytes());
    let mut price = vec![3u8];
    price.extend_from_slice(&50_000u64.to_le_bytes());
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

/// `ComputeBudget ×2 ++ ixs`.
fn with_budget(ixs: &[Instruction]) -> Vec<Instruction> {
    let mut v = compute_budget().to_vec();
    v.extend_from_slice(ixs);
    v
}

/// One of the two tables (module docs).
#[derive(Clone)]
struct Alt {
    name: &'static str,
    table: AddressLookupTableAccount,
}

/// (a) A28 as ARCH §6.4 enumerates it; (b) the script's table: (a) + the four `VenueExtras`.
fn tables() -> [Alt; 2] {
    let a = protocol_alt_addresses();
    let mut b = a.clone();
    for kind in VenueKind::WITH_EXTRAS {
        b.push(pda::venue_extras(kind));
    }
    [
        Alt {
            name: "A28",
            table: AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses: a,
            },
        },
        Alt {
            name: "build-alt",
            table: AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses: b,
            },
        },
    ]
}

fn shortvec_len(mut n: usize) -> usize {
    let mut len = 1;
    while n >= 0x80 {
        n >>= 7;
        len += 1;
    }
    len
}

/// A signed v0 transaction of `ixs` paid by `payer` through `alt`.
fn v0_tx(
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    alt: &Alt,
    blockhash: Hash,
) -> VersionedTransaction {
    let msg = v0::Message::try_compile(
        &payer.pubkey(),
        ixs,
        std::slice::from_ref(&alt.table),
        blockhash,
    )
    .expect("compile v0");
    let mut all: Vec<&Keypair> = vec![payer];
    all.extend(
        signers
            .iter()
            .copied()
            .filter(|s| s.pubkey() != payer.pubkey()),
    );
    VersionedTransaction::try_new(VersionedMessage::V0(msg), &all).expect("sign v0")
}

/// Wire size and locked accounts of a v0 transaction.
fn measure(tx: &VersionedTransaction) -> (usize, usize) {
    let sigs = tx.signatures.len();
    let size = shortvec_len(sigs) + 64 * sigs + tx.message.serialize().len();
    let loaded = tx.message.address_table_lookups().map_or(0, |l| {
        l.iter()
            .map(|t| t.writable_indexes.len() + t.readonly_indexes.len())
            .sum()
    });
    (size, tx.message.static_account_keys().len() + loaded)
}

/// Measures `ixs` (with the compute budget) through both tables, with a separate fee payer
/// and self-paid by `signer`; asserts the separate-payer worst case fits; returns it for (b).
fn assert_fits(what: &str, env: &Env, signer: &Keypair, ixs: &[Instruction]) -> usize {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    let mut out = 0;
    for alt in tables() {
        let (sep, sep_keys) = measure(&v0_tx(&env.payer, &[signer], &all, &alt, bh));
        let (own, own_keys) = measure(&v0_tx(signer, &[], &all, &alt, bh));
        println!(
            "  {what:<44} {:<9} {sep:>5} B / {sep_keys:>2} accounts (self-paid {own:>5} B / {own_keys:>2}); margin {} B",
            alt.name,
            TX_MAX as i64 - sep as i64
        );
        assert!(sep <= TX_MAX, "{what} through {}: {sep} B", alt.name);
        assert!(sep_keys <= ACCOUNTS_MAX, "{what}: {sep_keys} accounts");
        assert!(own <= sep && own_keys <= sep_keys);
        out = sep;
    }
    out
}

/// Sends `ixs` (with the compute budget) as a v0 transaction through `alt` (installed in the VM),
/// paid by the harness payer and signed by `signers`, with the invariant hooks after **every**
/// instruction: every existing market the transaction names is tracked; each proper prefix is
/// replayed on a clone of the VM (signature checks off) and its last instruction checked
/// (`check_ix` against the state before it, then `check_all` on every tracked market); then the
/// transaction runs and its last instruction is checked the same way (as `Env::send` does for
/// legacy transactions).
fn send_v0_hooked(env: &mut Env, ixs: &[Instruction], signers: &[&Keypair], alt: &Alt) -> TxResult {
    let all = with_budget(ixs);
    for ix in &all {
        for m in &ix.accounts {
            if read::zc::<shortwin::state::Market>(&env.svm, &m.pubkey).is_some() {
                env.track_market(m.pubkey);
            }
        }
    }
    let admins = env.admin_keys();
    let payer = env.payer.insecure_clone();
    let mut pre = invariants::snapshot(&env.svm, env.tracked_markets());
    let mut prefix_ok = true;
    for k in 1..all.len() {
        let mut svm = env.svm.clone().with_sigverify(false);
        let msg =
            Message::new_with_blockhash(&all[..k], Some(&payer.pubkey()), &svm.latest_blockhash());
        let n_sig = usize::from(msg.header.num_required_signatures);
        let tx = VersionedTransaction {
            signatures: vec![Default::default(); n_sig],
            message: VersionedMessage::Legacy(msg),
        };
        let Ok(meta) = svm.send_transaction(tx) else {
            prefix_ok = false;
            break;
        };
        let ix = &all[k - 1];
        let ev = invariants::IxEvents::of_instruction(&meta, k - 1, ix);
        if let Err(v) =
            invariants::check_ix(&svm, &pre, invariants::is_admin_signed(ix, &admins), &ev)
        {
            panic!("{v}\n(after instruction {k})\n{}", meta.pretty_logs());
        }
        for m in env.tracked_markets() {
            if let Err(v) = invariants::check_all(&svm, m) {
                panic!("{v}\n(after instruction {k})\n{}", meta.pretty_logs());
            }
        }
        pre = invariants::snapshot(&svm, env.tracked_markets());
    }
    let tx = v0_tx(&payer, signers, &all, alt, env.svm.latest_blockhash());
    let (size, _) = measure(&tx);
    assert!(size <= TX_MAX);
    let res = env.svm.send_transaction(tx);
    env.svm.expire_blockhash();
    match &res {
        Ok(meta) => {
            assert!(prefix_ok, "a prefix failed but the transaction succeeded");
            let last = all.len() - 1;
            let ev = invariants::IxEvents::of_instruction(meta, last, &all[last]);
            if let Err(v) = invariants::check_ix(
                &env.svm,
                &pre,
                invariants::is_admin_signed(&all[last], &admins),
                &ev,
            ) {
                panic!("{v}\n{}", meta.pretty_logs());
            }
            env.assert_invariants();
        }
        Err(_) => env.assert_invariants(),
    }
    res
}

// ---- markets, extras, exits ----------------------------------------------------------------------

fn q_of(env: &Env, pool: &AnyPool) -> u64 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture` (`WSOL_T1` with its LP burned, so it is eligible), lends `40·Q`, waits out
/// the listing window, cranks once.
fn add_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    if fixture == WSOL_T1 {
        raydium::set_mint_supply(&mut env.svm, &raydium::lp_mint(&pool), 0);
    }
    ok(env.list_ray(&pool, fixture.hint()));
    let q = q_of(env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}

/// Publishes the largest extras set (4 fixed read-only entries, existing System accounts) for
/// the sell and buy kinds of `kind` (the Emergency key) and returns the selection and the
/// trailing metas (`VenueExtras` first).
fn worst_extras(env: &mut Env, kind: VenueKind) -> (ExtrasSel, Vec<AccountMeta>) {
    let venue = kind as u8;
    let keys: Vec<Pubkey> = (0..EXTRAS_MAX_ENTRIES)
        .map(|_| env.funded_keypair().pubkey())
        .collect();
    let entries: Vec<ExtraEntryArg> = keys
        .iter()
        .map(|k| ExtraEntryArg {
            kind: ENTRY_FIXED,
            writable: false,
            key: *k,
            seeds: Vec::<SeedPartArg>::new(),
        })
        .collect();
    let ea = env.pause_authority.insecure_clone();
    for ix_kind in [EXTRAS_IX_SELL, EXTRAS_IX_BUY] {
        ok(env.send(
            &[set_venue_extras_ix(
                &ea.pubkey(),
                venue,
                ix_kind as u8,
                entries.clone(),
            )],
            &[&ea],
        ));
    }
    let mut metas = vec![AccountMeta::new_readonly(pda::venue_extras(kind), false)];
    metas.extend(keys.iter().map(|k| AccountMeta::new_readonly(*k, false)));
    (
        ExtrasSel {
            set: EXTRAS_SET_CURRENT,
            bumps: [0; 4],
        },
        metas,
    )
}

fn build(
    accounts: impl ToAccountMetas,
    data: impl InstructionData,
    rest: &[AccountMeta],
) -> Instruction {
    let mut metas = accounts.to_account_metas(None);
    metas.extend_from_slice(rest);
    Instruction {
        program_id: shortwin::ID,
        accounts: metas,
        data: data.data(),
    }
}

/// `liquidate(sel)` with `ReadSet ++ TradeExtras ++ extras`.
fn liquidate_x(
    rm: &RayMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = rm.market();
    let k = rm.keys();
    build(
        shortwin::accounts::Liquidate {
            keeper: *keeper,
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: k.mint,
            pool_vault: k.pool_vault,
            market_vault: k.market_vault,
            trader: k.trader,
            token_program: k.token_program,
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Liquidate { extras: sel },
        &rm.trade_remaining(extras),
    )
}

/// `expire(sel)` with `ReadSet ++ TradeExtras ++ extras`.
fn expire_x(
    rm: &RayMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = rm.market();
    let k = rm.keys();
    build(
        shortwin::accounts::Expire {
            keeper: *keeper,
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: k.mint,
            pool_vault: k.pool_vault,
            market_vault: k.market_vault,
            trader: k.trader,
            token_program: k.token_program,
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Expire { extras: sel },
        &rm.trade_remaining(extras),
    )
}

fn open_x(
    rm: &RayMarket,
    b: &Borrower,
    q: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    open_short_ix(
        rm,
        &b.key(),
        b.next_nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        sel,
        extras,
    )
}

fn one<T: anchor_lang::Event + anchor_lang::Discriminator + anchor_lang::AnchorDeserialize>(
    meta: &TransactionMetadata,
) -> T {
    events::one(meta)
}

/// A Jito tip: a System transfer of `lamports` from `from` to a tip account (a fresh key: the
/// eight tip accounts are not in the table either).
fn tip_transfer(from: &Pubkey, lamports: u64) -> Instruction {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            AccountMeta::new(*from, true),
            AccountMeta::new(Pubkey::new_unique(), false),
        ],
        data,
    }
}

/// A whale's real buy of `sol` lamports on `rm`'s pool.
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

// ---- tests ---------------------------------------------------------------------------------------

/// Every trading and settlement transaction of a Raydium market in its worst case (module
/// docs), measured through both tables and **executed** through them in turn: two opens (a
/// first-time borrower each), a no-op `sol_settle`, a close, an expiry at the deadline and a
/// liquidation after a whale's pump — all with the largest extras set.
#[test]
fn every_raydium_transaction_fits_and_executes_through_the_protocol_alt() {
    for fixture in [FART, USELESS, T22, WSOL_T1] {
        println!("{fixture:?}");
        let mut env = Env::ray_fork();
        let (rm, q) = add_market(&mut env, fixture);
        let [a, b] = tables();
        install_alt(&mut env.svm, &a.table.key, &a.table.addresses);
        install_alt(&mut env.svm, &b.table.key, &b.table.addresses);
        let (sel, extras) = worst_extras(&mut env, rm.kind());
        let keeper = env.funded_keypair();

        // open_short, first-time borrowers, largest extras: through (a), then (b).
        let mut b1 = env.borrower();
        let mut b2 = env.borrower();
        assert_fits(
            "open_short (new borrower, 4 extras)",
            &env,
            &b1.kp,
            &[open_x(&rm, &b1, q, sel, &extras)],
        );
        env.warp(1);
        let meta = ok(send_v0_hooked(
            &mut env,
            &[open_x(&rm, &b1, q, sel, &extras)],
            &[&b1.kp],
            &a,
        ));
        let ev: Open = one(&meta);
        assert_eq!(ev.q, q);
        let n1 = b1.next_nonce;
        b1.next_nonce += 1;
        env.warp(1);
        let meta = ok(send_v0_hooked(
            &mut env,
            &[open_x(&rm, &b2, q / 2, sel, &extras)],
            &[&b2.kp],
            &b,
        ));
        let _: Open = one(&meta);
        let n2 = b2.next_nonce;
        b2.next_nonce += 1;

        // sol_settle (no extras; the ShareEpoch record named) — a no-op on a healthy position.
        env.tick(&rm, 1);
        let settle = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b1.key(), n1);
        assert_fits("sol_settle", &env, &keeper, std::slice::from_ref(&settle));
        let meta = ok(send_v0_hooked(&mut env, &[settle], &[&keeper], &b));
        assert!(events::of::<SolSettle>(&meta).len() <= 1);

        // close_short, largest extras.
        let close = close_short_ix(&rm, &b2.key(), n2, u64::MAX, sel, &extras);
        assert_fits(
            "close_short (4 extras)",
            &env,
            &b2.kp,
            std::slice::from_ref(&close),
        );
        let meta = ok(send_v0_hooked(&mut env, &[close], &[&b2.kp], &a));
        let ev: Close = one(&meta);
        assert_eq!(ev.q, q / 2);

        // expire at the deadline, largest extras.
        let exp = expire_x(&rm, &keeper.pubkey(), &b1.key(), n1, sel, &extras);
        assert_fits(
            "expire (4 extras)",
            &env,
            &keeper,
            std::slice::from_ref(&exp),
        );
        let deadline = env.position(&rm, &b1.key(), n1).deadline;
        let slot = env.clock().slot;
        env.set_clock(deadline + 1, slot + 1_600_000);
        env.tick(&rm, 3);
        let meta = ok(send_v0_hooked(&mut env, &[exp], &[&keeper], &b));
        let ev: Expire = one(&meta);
        assert!(ev.keeper_reward > 0);

        // liquidate after a pump, largest extras; the keeper's composite and a tip measured.
        let mut b3 = env.borrower();
        env.crank(&rm);
        env.warp(1);
        ok(send_v0_hooked(
            &mut env,
            &[open_x(&rm, &b3, q, sel, &extras)],
            &[&b3.kp],
            &a,
        ));
        let n3 = b3.next_nonce;
        b3.next_nonce += 1;
        let liq = liquidate_x(&rm, &keeper.pubkey(), &b3.key(), n3, sel, &extras);
        assert_fits(
            "liquidate (4 extras)",
            &env,
            &keeper,
            std::slice::from_ref(&liq),
        );
        assert_fits(
            "crank_price + liquidate (keeper composite)",
            &env,
            &keeper,
            &[crank_price_ix(&rm), liq.clone()],
        );
        let tip = tip_transfer(&keeper.pubkey(), 1_000_000);
        let tipped = assert_fits(
            "liquidate + Jito tip transfer",
            &env,
            &keeper,
            &[liq.clone(), tip],
        );
        assert!(tipped <= TX_MAX);
        env.tick(&rm, 3);
        let (_, y) = rm.reserves(&env.svm).unwrap();
        whale_buy(&mut env, &rm, y / 1_000 * 484);
        let mut done = false;
        for _ in 0..30 {
            env.tick(&rm, 1);
            let res = send_v0_hooked(&mut env, std::slice::from_ref(&liq), &[&keeper], &b);
            match custom_code(&res) {
                None => {
                    let meta = ok(res);
                    let ev: Liquidate = one(&meta);
                    assert_eq!(ev.position, pda::position(&rm.market(), &b3.key(), n3).0);
                    done = true;
                    break;
                }
                Some(c)
                    if c == u32::from(E::NotLiquidatable) || c == u32::from(E::ExitBoundNotMet) => {
                }
                Some(c) => panic!("{fixture:?}: liquidate failed with {c}"),
            }
        }
        assert!(done, "{fixture:?}: never liquidated");
        assert_eq!(read::token_delegate(&env.svm, &rm.keys.pool_vault), None);
    }
}

/// Crank batches (ARCH §6.4 table, §7.1 duty 7): six markets on the fork, CPMM first. The
/// largest `k` whose v0 batch fits through each table is at least the keeper's v0 batch (4), and
/// that batch executes as one transaction (one `PriceCranked` per market).
#[test]
fn crank_batches_fit_through_the_protocol_alt() {
    let mut env = Env::ray_fork();
    let ms: Vec<(RayMarket, u64)> = [USELESS, T22, CREATOR_FEE, CREATOR_BOTH, WSOL_T1, FART]
        .into_iter()
        .map(|f| add_market(&mut env, f))
        .collect();
    let [a, b] = tables();
    install_alt(&mut env.svm, &a.table.key, &a.table.addresses);
    install_alt(&mut env.svm, &b.table.key, &b.table.addresses);
    env.warp(1);
    let cranks: Vec<Instruction> = ms.iter().map(|(rm, _)| crank_price_ix(rm)).collect();
    let keeper = env.funded_keypair();
    let bh = env.svm.latest_blockhash();
    for alt in [&a, &b] {
        let mut max = 0;
        for k in 1..=cranks.len() {
            let all = with_budget(&cranks[..k]);
            let (size, keys) = measure(&v0_tx(&keeper, &[], &all, alt, bh));
            println!(
                "  crank batch k = {k} (CPMM first) {:<9} {size:>5} B / {keys:>2} accounts",
                alt.name
            );
            if size <= TX_MAX && keys <= ACCOUNTS_MAX {
                max = k;
            }
        }
        println!("  largest v0 crank batch through {}: {max}", alt.name);
        assert!(
            max >= KEEPER_BATCH_V0,
            "{}: only {max} cranks fit",
            alt.name
        );
    }
    // The keeper's batch of 4 CPMM markets executes through (a), each crank checked.
    let meta = ok(send_v0_hooked(
        &mut env,
        &cranks[..KEEPER_BATCH_V0],
        &[],
        &a,
    ));
    let ev: Vec<PriceCranked> = events::of(&meta);
    assert_eq!(ev.len(), KEEPER_BATCH_V0);
    // And the rest (two, the AMM v4 market among them) through (b).
    env.warp(1);
    let meta = ok(send_v0_hooked(
        &mut env,
        &cranks[KEEPER_BATCH_V0..],
        &[],
        &b,
    ));
    assert_eq!(events::of::<PriceCranked>(&meta).len(), 2);
}

/// `λ_bps` of `pool` now.
fn lambda_of(env: &Env, pool: &AnyPool) -> u64 {
    let counter = raydium::lp_counter(&env.svm, pool);
    let supply = raydium::mint_supply(&env.svm, &raydium::lp_mint(pool));
    u64::from(shortwin_math::pool_select::lambda_bps(counter, supply))
}

/// A CPMM pool of `base`'s token planted with `base`'s reserves scaled by `num/den` and its
/// locked share (a real pool account cloned from USELESS's; `pool_select::plant_cpmm`).
fn plant_like(env: &mut Env, base: &AnyPool, num: u64, den: u64) -> AnyPool {
    let lam = lambda_of(env, base);
    let spec = spec_like(&env.svm, base, true, lam).scaled(num, den);
    AnyPool::Cpmm(plant_cpmm(env, CpmmFixture::Useless, spec))
}

/// Measures `ixs` (with the compute budget) through both tables for an instruction nobody but
/// the fee payer signs (`propose_pool` and `execute_pool_switch` take no signer: the keeper is
/// the payer); asserts it fits.
fn assert_fits_unsigned(what: &str, env: &Env, ixs: &[Instruction]) {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    for alt in tables() {
        let (size, keys) = measure(&v0_tx(&env.payer, &[], &all, &alt, bh));
        println!(
            "  {what:<44} {:<9} {size:>5} B / {keys:>2} accounts; margin {} B",
            alt.name,
            TX_MAX as i64 - size as i64
        );
        assert!(size <= TX_MAX, "{what} through {}: {size} B", alt.name);
        assert!(keys <= ACCOUNTS_MAX, "{what}: {keys} accounts");
    }
}

/// Pool selection (module docs; ARCH §3.9, §6.4): `propose_pool` and `execute_pool_switch` in
/// their largest shapes fit 1,232 B and 64 accounts through both tables, and execute that way —
/// a list-only adoption (the detection set + a CPMM candidate), a zero-position adoption and a
/// fast-path window between two CPMM pools (5 + 5 remaining accounts), and the window's
/// execution. Neither instruction has a signer, so the fee payer is the only signature.
#[test]
fn pool_selection_fits_and_executes_through_the_protocol_alt() {
    // A list-only market of USELESS adopts its CPMM pool.
    {
        let mut env = Env::ray_fork();
        let [a, b] = tables();
        install_alt(&mut env.svm, &a.table.key, &a.table.addresses);
        install_alt(&mut env.svm, &b.table.key, &b.table.addresses);
        let pool = env.load_ray_pool(USELESS);
        let (mint, program) = pool.token();
        ok(env.list_auto(&mint, program));
        let ix = propose_pool_ix(&env.svm, &mint, &detection_set(&mint), &pool);
        assert_fits_unsigned(
            "propose_pool (list-only, CPMM candidate)",
            &env,
            std::slice::from_ref(&ix),
        );
        let meta = ok(send_v0_hooked(&mut env, &[ix], &[], &a));
        let ev: PoolSwitched = one(&meta);
        assert_eq!(ev.kind, POOL_KIND_LISTING);
    }

    // A CPMM market (USELESS), no position: a zero-position adoption onto a planted CPMM pool
    // twice as deep, then a fast path onto a thinner one while the new incumbent is halted.
    let mut env = Env::ray_fork();
    let (rm, _) = add_market(&mut env, USELESS);
    let [a, b] = tables();
    install_alt(&mut env.svm, &a.table.key, &a.table.addresses);
    install_alt(&mut env.svm, &b.table.key, &b.table.addresses);
    let mint = rm.pool.token().0;
    let deep = plant_like(&mut env, &rm.pool, 2, 1);
    let incumbent = raydium::candidate_read_set(&env.svm, &rm.pool, false);
    let ix = propose_pool_ix(&env.svm, &mint, &incumbent, &deep);
    assert_fits_unsigned(
        "propose_pool (CPMM incumbent, CPMM candidate)",
        &env,
        std::slice::from_ref(&ix),
    );
    env.warp(1);
    let meta = ok(send_v0_hooked(&mut env, &[ix], &[], &b));
    let ev: PoolSwitched = one(&meta);
    assert_eq!(ev.kind, POOL_KIND_ZERO_POSITION);

    // The fast path: the new incumbent halted and observed for 150 s, a candidate at 60 %.
    let AnyPool::Cpmm(deep_cpmm) = &deep else {
        unreachable!("planted as CPMM")
    };
    fork::set_cpmm_status(&mut env.svm, deep_cpmm, 4);
    let rm = rm.on_pool(&env.svm, deep);
    for _ in 0..5 {
        env.warp(30);
        env.crank(&rm);
    }
    let thin = plant_like(&mut env, &deep, 60, 100);
    let incumbent = raydium::candidate_read_set(&env.svm, &deep, false);
    let ix = propose_pool_ix(&env.svm, &mint, &incumbent, &thin);
    assert_fits_unsigned(
        "propose_pool (fast-path window, CPMM → CPMM)",
        &env,
        std::slice::from_ref(&ix),
    );
    env.warp(1);
    let meta = ok(send_v0_hooked(&mut env, &[ix], &[], &a));
    let ev: PoolProposed = one(&meta);
    assert_eq!(ev.kind, POOL_KIND_FAST);
    env.warp(60);
    let ix = execute_pool_switch_ix(&env.svm, &mint, &deep, &thin);
    assert_fits_unsigned(
        "execute_pool_switch (CPMM → CPMM)",
        &env,
        std::slice::from_ref(&ix),
    );
    let meta = ok(send_v0_hooked(&mut env, &[ix], &[], &b));
    let ev: PoolSwitched = one(&meta);
    assert_eq!(ev.kind, POOL_KIND_FAST);
}
