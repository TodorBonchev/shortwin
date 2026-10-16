//! T-SIZE on the PumpSwap venue (ARCH §6.4, §6.2, §8.2; §9 M6 exit criterion "T-SIZE"): every
//! PumpSwap transaction the protocol and its keeper send — `create_market(Auto)`, `open_short`,
//! `close_short`, `liquidate`, `expire`, `sol_settle`, `sweep_trader`'s token leg, `crank_price`
//! batches, the `→ PumpSwap` graduation transition and the straddle's tx3 — fits **1,232 B and 64
//! accounts** as a v0 transaction through the protocol ALT (A28) in its **worst case**, and
//! **executes** that way on the PumpSwap fork (the dumped PumpSwap, pump and pump-fees programs and
//! canonical pools), with the invariant hooks after every instruction.
//!
//! **Worst case** of each transaction: the compute budget both ways (`SetComputeUnitLimit` +
//! `SetComputeUnitPrice`); a **separate fee payer** (two signatures); a first-time borrower; and
//! the **largest extras set** on every trading instruction (A41): the PumpSwap `VenueExtras`
//! account and `EXTRAS_MAX_ENTRIES` = 4 fixed entries, published for the sell and buy kinds and
//! selected as `current` — the pAMM ELF accepts trailing accounts (R-VEN Update H5), so these
//! transactions trade normally.
//!
//! **Two tables** (both asserted): **(a)** A28 exactly as ARCH §6.4 enumerates it (the Raydium part
//! the harness builds and the pump part — pump, PumpSwap and pump-fees, their statics, and the 8 + 8
//! pump and 8 + 8 PumpSwap fee and buyback recipients of the live `Global` / `GlobalConfig` with
//! their WSOL ATAs) where the `VenueExtras` account stays inline; **(b)** the table
//! `scripts/build-alt.sh` builds (`shortwin_keeper::alt`: (a) plus the four `VenueExtras` PDAs).
//!
//! **M6R-1 (owner decision of 2026-09-30; ARCH §6.4 row 56, changelog 56):** every PumpSwap ReadSet
//! carries the pool's LP mint (P6, a per-pool key inline: +33 B). Two worst cases no longer fit
//! 1,232 B through (a): the open with the largest (4-entry) extras set and the keeper's `[crank,
//! liquidate]` with it — an owner trade-off with its fallbacks (a v1 transaction, a per-market
//! table, or fewer extras; the keeper sends the liquidation alone). Both are pinned here: over by
//! at most [`P6_OVER_MAX`] B through (a), fitting — and the open **executing** — through (a) plus
//! a per-market lookup table ([`market_table`]); the open with 3 extras fits (a) alone. v0 crank
//! batches hold [`KEEPER_BATCH_V0`] = 3 PumpSwap cranks (4 until M6R-1; the 4-crank batch is
//! pinned over the limit).
//!
//! **M7 (OWNER-M7-1, measured by the keeper):** the composite is over only as the worst case here
//! builds it, with a separate fee payer (a second signature and key, ≈ 96 B). The keeper pays its
//! own fee (the "self-paid" column printed below): its `[crank, liquidate]` with 4 extras is
//! 1,156–1,157 B through (a) and fits; the separate-payer open stays the one transaction over (a)
//! (`tests/bench/m7_keeper/tsize_pumpswap.md`; ARCH §6.4).
//!
//! Run with `-- --nocapture` for the table.

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::types::TransactionMetadata;
use shortwin::constants::{
    PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG,
    PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, PUMP_AMM_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG,
    PUMP_FEES_PROGRAM_ID, PUMP_GLOBAL, PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID,
};
use shortwin::errors::ShortwinError as E;
use shortwin::events::{
    Close, Expire, Liquidate, Open, PriceCranked, SolSettle, VenueStateChanged,
};
use shortwin::state::{ENTRY_FIXED, EXTRAS_IX_BUY, EXTRAS_IX_SELL, EXTRAS_MAX_ENTRIES, VenueKind};
use shortwin::venues::{EXTRAS_SET_CURRENT, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, open_short_ix, set_venue_extras_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{custom_code, ok};
use shortwin_litesvm_tests::fork::PumpCoin;
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammPool, PammUser};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::raydium::install_alt;
use shortwin_litesvm_tests::{Env, TxResult, events, invariants, pda, read};
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, Hash, Message, VersionedMessage, v0};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

const TX_MAX: usize = 1_232;
const ACCOUNTS_MAX: usize = 64;
/// The keeper's v0 crank batch of PumpSwap markets (ARCH §7.1 duty 7): 3 since M6R-1's LP mint
/// (4 before; the keeper cuts its batches where a transaction stops fitting).
const KEEPER_BATCH_V0: usize = 3;
/// The most M6R-1's LP mint pushes a worst-case PumpSwap transaction over 1,232 B through A28
/// (ARCH §6.4 row 56 estimates 6–7 B for the 4-extras open, ≈ 20 B for the keeper's composite).
const P6_OVER_MAX: usize = 24;
const SOL: u64 = 1_000_000_000;

const MATURE: PammCoin = PammCoin::Mature;
const SPL261: PammCoin = PammCoin::Legacy261;

const COMPUTE_BUDGET: Pubkey =
    anchor_lang::prelude::pubkey!("ComputeBudget111111111111111111111111111111");

// ---- transactions ------------------------------------------------------------------------------

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

fn with_budget(ixs: &[Instruction]) -> Vec<Instruction> {
    let mut v = compute_budget().to_vec();
    v.extend_from_slice(ixs);
    v
}

#[derive(Clone)]
struct Alt {
    name: &'static str,
    table: AddressLookupTableAccount,
}

/// A28's pump part as ARCH §6.4 enumerates it, read from the fork's live `Global` and
/// `GlobalConfig`.
fn pump_part(svm: &litesvm::LiteSVM) -> Vec<Pubkey> {
    let mut v = vec![
        PUMP_PROGRAM_ID,
        PUMP_AMM_PROGRAM_ID,
        PUMP_FEES_PROGRAM_ID,
        PUMP_GLOBAL,
        PUMP_EVENT_AUTHORITY,
        PUMP_FEE_CONFIG,
        PUMP_GLOBAL_VOLUME_ACCUMULATOR,
        PUMP_AMM_GLOBAL_CONFIG,
        PUMP_AMM_EVENT_AUTHORITY,
        PUMP_AMM_FEE_CONFIG,
        PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
    ];
    let g = svm.get_account(&PUMP_GLOBAL).expect("pump Global");
    let global = vpump::read_global(&g.owner.to_bytes(), &g.data).expect("Global");
    let gc = svm
        .get_account(&PUMP_AMM_GLOBAL_CONFIG)
        .expect("GlobalConfig");
    let config = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data).expect("GlobalConfig");
    for list in [
        global.fee_recipients(),
        global.buyback_fee_recipients(),
        config.protocol_fee_recipients(),
        config.buyback_fee_recipients(),
    ] {
        for k in list {
            let k = Pubkey::new_from_array(k);
            if k == Pubkey::default() {
                continue;
            }
            for key in [k, pump::wsol_ata(&k)] {
                if !v.contains(&key) {
                    v.push(key);
                }
            }
        }
    }
    v
}

/// (a) A28 (the Raydium part, then the pump part); (b) the script's table: (a) + the four
/// `VenueExtras`.
fn tables(svm: &litesvm::LiteSVM) -> [Alt; 2] {
    let mut a = shortwin_litesvm_tests::raydium::protocol_alt_addresses();
    for k in pump_part(svm) {
        if !a.contains(&k) {
            a.push(k);
        }
    }
    let mut b = a.clone();
    for kind in VenueKind::WITH_EXTRAS {
        b.push(pda::venue_extras(kind));
    }
    assert!(b.len() <= 256, "{} addresses", b.len());
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

fn install_tables(env: &mut Env) -> [Alt; 2] {
    let t = tables(&env.svm);
    for alt in &t {
        install_alt(&mut env.svm, &alt.table.key, &alt.table.addresses);
    }
    t
}

fn shortvec_len(mut n: usize) -> usize {
    let mut len = 1;
    while n >= 0x80 {
        n >>= 7;
        len += 1;
    }
    len
}

fn v0_tx(
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    alt: &Alt,
    blockhash: Hash,
) -> VersionedTransaction {
    v0_tx_with(payer, signers, ixs, &[alt], blockhash)
}

/// [`v0_tx`] through several lookup tables (A28 and a per-market table, M6R-1's fallback).
fn v0_tx_with(
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    alts: &[&Alt],
    blockhash: Hash,
) -> VersionedTransaction {
    let tables: Vec<AddressLookupTableAccount> = alts.iter().map(|a| a.table.clone()).collect();
    let msg =
        v0::Message::try_compile(&payer.pubkey(), ixs, &tables, blockhash).expect("compile v0");
    let mut all: Vec<&Keypair> = vec![payer];
    all.extend(
        signers
            .iter()
            .copied()
            .filter(|s| s.pubkey() != payer.pubkey()),
    );
    VersionedTransaction::try_new(VersionedMessage::V0(msg), &all).expect("sign v0")
}

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

/// Measures `ixs` (with the compute budget) through both tables, separate payer and self-paid;
/// asserts the separate-payer worst case fits; returns the separate-payer sizes through (a) and (b).
fn assert_fits(what: &str, env: &Env, signer: &Keypair, ixs: &[Instruction]) -> [usize; 2] {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    let mut out = [0; 2];
    for (i, alt) in tables(&env.svm).iter().enumerate() {
        let (sep, sep_keys) = measure(&v0_tx(&env.payer, &[signer], &all, alt, bh));
        let (own, own_keys) = measure(&v0_tx(signer, &[], &all, alt, bh));
        println!(
            "  {what:<48} {:<9} {sep:>5} B / {sep_keys:>2} accounts (self-paid {own:>5} B / {own_keys:>2}); margin {} B",
            alt.name,
            TX_MAX as i64 - sep as i64
        );
        assert!(sep <= TX_MAX, "{what} through {}: {sep} B", alt.name);
        assert!(sep_keys <= ACCOUNTS_MAX, "{what}: {sep_keys} accounts");
        assert!(own <= sep && own_keys <= sep_keys);
        out[i] = sep;
    }
    out
}

/// M6R-1 (module docs): `ixs` (with the compute budget) is over 1,232 B through (a) by at most
/// [`P6_OVER_MAX`] B — the LP mint P6's cost, an owner trade-off — and fits through (a) plus the
/// market's own table `mt`, separate payer and self-paid; returns the separate-payer sizes through
/// (a), (b) and (a) + `mt`.
fn assert_p6_over(
    what: &str,
    env: &Env,
    signer: &Keypair,
    ixs: &[Instruction],
    mt: &Alt,
) -> [usize; 3] {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    let t = tables(&env.svm);
    let mut out = [0; 3];
    for (i, alts) in [vec![&t[0]], vec![&t[1]], vec![&t[0], mt]]
        .iter()
        .enumerate()
    {
        let (sep, sep_keys) = measure(&v0_tx_with(&env.payer, &[signer], &all, alts, bh));
        let (own, _) = measure(&v0_tx_with(signer, &[], &all, alts, bh));
        let name: Vec<&str> = alts.iter().map(|a| a.name).collect();
        println!(
            "  {what:<48} {:<16} {sep:>5} B / {sep_keys:>2} accounts (self-paid {own:>5} B); margin {} B",
            name.join(" + "),
            TX_MAX as i64 - sep as i64
        );
        assert!(sep_keys <= ACCOUNTS_MAX, "{what}: {sep_keys} accounts");
        out[i] = sep;
    }
    assert!(
        out[0] > TX_MAX && out[0] <= TX_MAX + P6_OVER_MAX,
        "{what} through A28: {} B (M6R-1 pins it just over the limit)",
        out[0]
    );
    assert!(
        out[2] <= TX_MAX,
        "{what} through A28 + the market's table: {} B",
        out[2]
    );
    out
}

/// A per-market lookup table (ARCH §6.4's fallback for what M6R-1's LP mint pushes over v0): the
/// market's own accounts and its pool's, which A28 leaves inline.
fn market_table(env: &mut Env, pm: &PammMarket) -> Alt {
    let market = pm.market();
    let k = pm.keys();
    let addresses = vec![
        market,
        k.market_vault,
        k.pool_vault,
        k.trader,
        k.mint,
        pda::trader_wsol(&market).0,
        pm.trader_base_ata,
        pm.pool.pool,
        pm.pool.base_vault,
        pm.pool.quote_vault,
        pm.pool.boost_vault_ata,
        pm.pool.lp_mint,
        pm.trader_uva(),
        pm.cashback_ata(),
        pm.creator_vault().0,
        pm.creator_vault().1,
        pamm::pool_v2(&k.mint),
    ];
    let alt = Alt {
        name: "per-market",
        table: AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses,
        },
    };
    install_alt(&mut env.svm, &alt.table.key, &alt.table.addresses);
    alt
}

/// Measures `ixs` for instructions nobody but the fee payer signs (`crank_price`, `sweep_trader`,
/// the transition crank); asserts it fits.
fn assert_fits_unsigned(what: &str, env: &Env, ixs: &[Instruction]) {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    for alt in tables(&env.svm) {
        let (size, keys) = measure(&v0_tx(&env.payer, &[], &all, &alt, bh));
        println!(
            "  {what:<48} {:<9} {size:>5} B / {keys:>2} accounts; margin {} B",
            alt.name,
            TX_MAX as i64 - size as i64
        );
        assert!(size <= TX_MAX, "{what} through {}: {size} B", alt.name);
        assert!(keys <= ACCOUNTS_MAX, "{what}: {keys} accounts");
    }
}

/// Sends `ixs` (with the compute budget) as a v0 transaction through `alt`, with the invariant
/// hooks after **every** instruction (as `tx_size_pump_it::send_v0_hooked`).
fn send_v0_hooked(env: &mut Env, ixs: &[Instruction], signers: &[&Keypair], alt: &Alt) -> TxResult {
    send_v0_hooked_with(env, ixs, signers, &[alt])
}

/// [`send_v0_hooked`] through several lookup tables.
fn send_v0_hooked_with(
    env: &mut Env,
    ixs: &[Instruction],
    signers: &[&Keypair],
    alts: &[&Alt],
) -> TxResult {
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
        let svm = env.svm.clone().with_sigverify(false);
        let msg =
            Message::new_with_blockhash(&all[..k], Some(&payer.pubkey()), &svm.latest_blockhash());
        let n_sig = usize::from(msg.header.num_required_signatures);
        let mut svm = svm;
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
    let tx = v0_tx_with(&payer, signers, &all, alts, env.svm.latest_blockhash());
    let (size, _) = measure(&tx);
    assert!(size <= TX_MAX, "{} B", size);
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

fn one<T: anchor_lang::Event + anchor_lang::Discriminator + anchor_lang::AnchorDeserialize>(
    meta: &TransactionMetadata,
) -> T {
    events::one(meta)
}

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

// ---- markets, extras, exits ----------------------------------------------------------------------

fn q_for(env: &Env, p: &PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let y = r.effective_quote.max(1);
    let q = (base / 100).min(base * u128::from(10 * SOL) / y);
    u64::try_from(q).expect("u64")
}

fn wait_out_boost(env: &mut Env, pm: &PammMarket) {
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            return;
        }
        env.warp(30);
        ok(env.send(&[crank_price_ix(pm)], &[]));
    }
}

/// Lists `coin`'s pool, lends `40·q`, warms up and waits out the boost.
fn list_market(env: &mut Env, p: &PammPool, boost: bool) -> (PammMarket, u64) {
    let q = q_for(env, p);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 60);
    if boost {
        wait_out_boost(env, &pm);
    }
    let q = q_for(env, &pm.pool);
    (pm, q)
}

fn setup(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let (pm, q) = list_market(&mut env, &p, true);
    (env, pm, q)
}

/// Publishes the largest extras set (4 fixed read-only entries) for PumpSwap's sell and buy
/// kinds and returns the selection and the trailing metas (`VenueExtras` first).
fn worst_extras(env: &mut Env) -> (ExtrasSel, Vec<AccountMeta>) {
    let kind = VenueKind::PumpSwap;
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
                kind as u8,
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

fn build_ix(
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

fn open_x(
    pm: &PammMarket,
    b: &Borrower,
    q: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    open_short_ix(
        pm,
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

fn liquidate_x(
    mm: &PammMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = mm.market();
    let k = mm.keys();
    build_ix(
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
        &mm.trade_remaining(extras),
    )
}

fn expire_x(
    mm: &PammMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = mm.market();
    let k = mm.keys();
    build_ix(
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
        &mm.trade_remaining(extras),
    )
}

// ---- tests ---------------------------------------------------------------------------------------

/// Every trading and settlement transaction of a PumpSwap market in its worst case, measured
/// through both tables and **executed** through them: two opens (a first-time borrower each), a
/// no-op `sol_settle`, a close, an expiry and a liquidation after a real pump — all with the
/// largest extras set; then `sweep_trader`'s token leg.
#[test]
fn every_pumpswap_transaction_fits_and_executes_through_the_protocol_alt() {
    for coin in [SPL261, MATURE] {
        println!("{coin:?}");
        let (mut env, pm, q) = setup(coin);
        let [a, b] = install_tables(&mut env);
        let (sel, extras) = worst_extras(&mut env);
        let keeper = env.funded_keypair();

        let mt = market_table(&mut env, &pm);
        let mut b1 = env.borrower();
        let mut b2 = env.borrower();
        // M6R-1: the largest extras set pushes the open over v0 through A28 (the LP mint P6);
        // through A28 plus the market's own table it fits and executes, and with 3 extras it fits
        // A28 alone.
        assert_p6_over(
            "open_short (new borrower, 4 extras)",
            &env,
            &b1.kp,
            &[open_x(&pm, &b1, q, sel, &extras)],
            &mt,
        );
        assert_fits(
            "open_short (new borrower, 3 extras)",
            &env,
            &b1.kp,
            &[open_x(&pm, &b1, q, sel, &extras[..extras.len() - 1])],
        );
        let meta = ok(send_v0_hooked_with(
            &mut env,
            &[open_x(&pm, &b1, q, sel, &extras)],
            &[&b1.kp],
            &[&a, &mt],
        ));
        let ev: Open = one(&meta);
        assert_eq!(ev.q, q);
        let n1 = b1.next_nonce;
        b1.next_nonce += 1;
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let meta = ok(send_v0_hooked_with(
            &mut env,
            &[open_x(&pm, &b2, q, sel, &extras)],
            &[&b2.kp],
            &[&b, &mt],
        ));
        let _: Open = one(&meta);
        let n2 = b2.next_nonce;
        b2.next_nonce += 1;

        // sol_settle (no extras) — a no-op on a healthy position.
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let settle = sol_settle_ix(&env, &pm, &keeper.pubkey(), &b1.key(), n1);
        assert_fits("sol_settle", &env, &keeper, std::slice::from_ref(&settle));
        let meta = ok(send_v0_hooked(&mut env, &[settle], &[&keeper], &b));
        assert!(events::of::<SolSettle>(&meta).is_empty());

        // close_short, largest extras.
        let close = close_short_ix(&pm, &b2.key(), n2, u64::MAX, sel, &extras);
        assert_fits(
            "close_short (4 extras)",
            &env,
            &b2.kp,
            std::slice::from_ref(&close),
        );
        let meta = ok(send_v0_hooked(&mut env, &[close], &[&b2.kp], &a));
        let ev: Close = one(&meta);
        assert_eq!(ev.q, q);

        // expire at the deadline, largest extras.
        let exp = expire_x(&pm, &keeper.pubkey(), &b1.key(), n1, sel, &extras);
        assert_fits(
            "expire (4 extras)",
            &env,
            &keeper,
            std::slice::from_ref(&exp),
        );
        let deadline = env.position(&pm, &b1.key(), n1).deadline;
        let now = env.now();
        env.warp(deadline + 1 - now);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let meta = ok(send_v0_hooked(&mut env, &[exp], &[&keeper], &b));
        let ev: Expire = one(&meta);
        assert!(ev.keeper_reward > 0);

        // liquidate after a real pump, largest extras; the composite and a tip measured.
        let mut b3 = env.borrower();
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        ok(send_v0_hooked_with(
            &mut env,
            &[open_x(&pm, &b3, q, sel, &extras)],
            &[&b3.kp],
            &[&a, &mt],
        ));
        let n3 = b3.next_nonce;
        b3.next_nonce += 1;
        let liq = liquidate_x(&pm, &keeper.pubkey(), &b3.key(), n3, sel, &extras);
        assert_fits(
            "liquidate (4 extras)",
            &env,
            &keeper,
            std::slice::from_ref(&liq),
        );
        // M6R-1: the keeper's composite with the largest extras set no longer fits through A28
        // (the keeper then sends the liquidation alone, whose prologue is the second's
        // observation anyway: ARCH §7.1 item 7).
        assert_p6_over(
            "crank_price + liquidate (4 extras; keeper composite)",
            &env,
            &keeper,
            &[crank_price_ix(&pm), liq.clone()],
            &mt,
        );
        // Without extras (the keeper's normal case) the composite and a tip both fit.
        let plain = liquidate_x(&pm, &keeper.pubkey(), &b3.key(), n3, ExtrasSel::NONE, &[]);
        assert_fits(
            "crank_price + liquidate (keeper composite)",
            &env,
            &keeper,
            &[crank_price_ix(&pm), plain.clone()],
        );
        assert_fits(
            "liquidate + Jito tip transfer",
            &env,
            &keeper,
            &[plain, tip_transfer(&keeper.pubkey(), 1_000_000)],
        );
        let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
        ok(u.push_price(&mut env, &pm.pool, 28, 10));
        let mut done = false;
        for _ in 0..900 {
            env.warp(1);
            ok(env.send(&[crank_price_ix(&pm)], &[]));
            let res = send_v0_hooked(&mut env, std::slice::from_ref(&liq), &[&keeper], &b);
            match custom_code(&res) {
                None => {
                    let meta = ok(res);
                    let ev: Liquidate = one(&meta);
                    assert_eq!(ev.position, pda::position(&pm.market(), &b3.key(), n3).0);
                    done = true;
                    break;
                }
                Some(c) if c == u32::from(E::NotLiquidatable) => {}
                Some(c) => panic!("{coin:?}: liquidate failed with {c}"),
            }
        }
        assert!(done, "{coin:?}: never liquidated");

        // sweep_trader's token leg (a donation on both sides of the trader).
        let griefer = env.funded_keypair();
        let mut data = 2u32.to_le_bytes().to_vec();
        data.extend_from_slice(&1_000_000u64.to_le_bytes());
        ok(env.send(
            &[Instruction {
                program_id: anchor_lang::system_program::ID,
                accounts: vec![
                    AccountMeta::new(griefer.pubkey(), true),
                    AccountMeta::new(pm.trader(), false),
                ],
                data,
            }],
            &[&griefer],
        ));
        let sweep = pamm::sweep_trader_pamm_ix(&env, &pm);
        assert_fits_unsigned(
            "sweep_trader (token leg)",
            &env,
            std::slice::from_ref(&sweep),
        );
        ok(send_v0_hooked(&mut env, &[sweep], &[], &a));
    }
}

/// `create_market(Auto)` of a PumpSwap-class coin with the pAMM init set: fits through both tables
/// and executes through (a).
#[test]
fn a_pumpswap_listing_fits_and_executes_through_the_protocol_alt() {
    for coin in [SPL261, MATURE] {
        let (mut env, p) = Env::fork_pamm_coin(coin);
        let [a, _] = install_tables(&mut env);
        let creator = env.funded_keypair();
        let ix = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
        assert_fits(
            &format!("create_market (Auto, {coin:?}, PumpSwap)"),
            &env,
            &creator,
            std::slice::from_ref(&ix),
        );
        ok(send_v0_hooked(&mut env, &[ix], &[&creator], &a));
        assert_eq!(
            env.market(&pda::market(&p.mint).0).venue_kind(),
            Some(VenueKind::PumpSwap)
        );
    }
}

/// The graduation transition (§2.5) and the straddle's tx3 (§5.4): the transition crank alone, and
/// the transition crank plus the borrower's close on the pool in one transaction, both fit through
/// both tables and execute through them.
#[test]
fn the_graduation_transition_and_tx3_fit_and_execute_through_the_protocol_alt() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm: PumpMarket = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q * 40), c);
    let [a, b] = install_tables(&mut env);
    let mut bo = env.borrower();
    let (res, nonce) = env.open(&pm, &mut bo, q);
    ok(res);
    env.warp(1);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt, u64::MAX));
    let migrator = env.funded_keypair();
    ok(env.send(
        &[
            pump::cu_limit_ix(600_000),
            pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve),
        ],
        &[&migrator],
    ));
    env.warp(1);
    // The transition crank alone.
    let crank = crank_price_ix(&pm);
    assert_fits_unsigned(
        "crank_price (→ PumpSwap transition)",
        &env,
        std::slice::from_ref(&crank),
    );
    // tx3: the transition crank + the borrower's close on the pool.
    let pam = PammMarket::graduated(&env.svm, &pm);
    let payer = env.payer.pubkey();
    ok(env.send(&pamm::venue_account_ixs(&env.svm, &payer, &pam), &[]));
    let close = close_short_ix(&pam, &bo.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
    assert_fits(
        "crank_price (→ PumpSwap) + close_short (tx3)",
        &env,
        &bo.kp,
        &[crank.clone(), close.clone()],
    );
    let meta = ok(send_v0_hooked(&mut env, &[crank, close], &[&bo.kp], &a));
    let ev: VenueStateChanged = one(&meta);
    assert_eq!(
        (ev.from, ev.to),
        (VenueKind::PumpCurve as u8, VenueKind::PumpSwap as u8)
    );
    assert_eq!(env.market(&pm.market()).open_positions, 0);
    let _ = b;
}

/// Crank batches (ARCH §6.4, §7.1 duty 7): PumpSwap markets. Every batch of 1–3 cranks fits v0
/// through both tables, and the keeper's batch of 3 executes as one transaction; since M6R-1's LP
/// mint (+33 B per crank) a batch of 4 no longer fits (1,251 B through A28 when measured; 1,119 B
/// before), so the keeper's size cut sends 3.
#[test]
fn crank_batches_fit_through_the_protocol_alt() {
    let (mut env, p) = Env::fork_pamm_coin(MATURE);
    let mut markets = vec![list_market(&mut env, &p, false).0];
    for coin in [SPL261, PammCoin::Legacy211, PammCoin::Cashback] {
        let p = env.load_pamm_coin(coin);
        markets.push(list_market(&mut env, &p, false).0);
    }
    let [a, b] = install_tables(&mut env);
    env.warp(1);
    let cranks: Vec<Instruction> = markets.iter().map(crank_price_ix).collect();
    let keeper = env.funded_keypair();
    let bh = env.svm.latest_blockhash();
    for alt in [&a, &b] {
        for k in 1..=cranks.len() {
            let all = with_budget(&cranks[..k]);
            let (size, keys) = measure(&v0_tx(&keeper, &[], &all, alt, bh));
            println!(
                "  crank batch k = {k} (PumpSwap) {:<9} {size:>5} B / {keys:>2} accounts",
                alt.name
            );
            assert!(keys <= ACCOUNTS_MAX, "{k} cranks");
            if k <= KEEPER_BATCH_V0 {
                assert!(size <= TX_MAX, "{k} cranks: {size} B");
            } else {
                assert!(
                    size > TX_MAX,
                    "{k} cranks fit: the keeper's batch is too small"
                );
            }
        }
    }
    let meta = ok(send_v0_hooked(
        &mut env,
        &cranks[..KEEPER_BATCH_V0],
        &[],
        &a,
    ));
    assert_eq!(events::of::<PriceCranked>(&meta).len(), KEEPER_BATCH_V0);
}
