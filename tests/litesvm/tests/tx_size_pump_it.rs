//! T-SIZE on the pump curve (ARCH §6.4, §6.2, §8.2; §9 M5 exit criterion "T-SIZE"): every curve
//! transaction the protocol and its keeper send — `create_market(Auto)`, `open_short`,
//! `close_short`, `liquidate`, `expire`, `sol_settle` (the no-op, and S3 on `PumpMigrating` and
//! on `None`), `sweep_trader`'s token leg and `crank_price` batches — fits **1,232 B and 64
//! accounts** as a v0 transaction through the protocol ALT (A28) in its **worst case**, and
//! **executes** that way on the pump fork (the dumped pump, pump-fees and PumpSwap programs and
//! coins), with the invariant hooks after every instruction.
//!
//! **Worst case** of each transaction:
//! - the compute budget both ways the protocol sends it: `SetComputeUnitLimit` and
//!   `SetComputeUnitPrice` (ARCH §6.1);
//! - a **separate fee payer** (two signatures, the payer's key inline) — a wallet paying for itself
//!   is one signature and one key smaller; both are measured, the separate payer is asserted;
//! - a first-time borrower (the position and the exposure records created by the open);
//! - the **largest extras set** on every trading instruction (A41): the curve's `VenueExtras`
//!   account and `EXTRAS_MAX_ENTRIES` = 4 fixed entries, all distinct keys, published by the
//!   Emergency key for the sell and the buy kinds and selected as `current` — pump accepts
//!   trailing accounts (R-PUMP Update U4), so these transactions trade normally;
//! - SPL Token and Token-2022 curves (115 bytes; the Token-2022 one with a creator), and the
//!   151-byte curve near completion for the migration paths.
//!
//! **Two tables** (both asserted): **(a)** A28 exactly as ARCH §6.4 enumerates it — the Raydium
//! part the harness builds (`raydium::protocol_alt_addresses`: SPL Token, Token-2022, ATA,
//! System, WSOL, short.win's `config`, `venue_guard`, `oracle_cache` and event authority, the
//! Raydium programs, authorities and `AmmConfig`s) and the pump part: pump, PumpSwap and
//! pump-fees, pump `global`, `event_authority`, `fee_config` and global volume accumulator, the
//! same four of PumpSwap, and the 8 + 8 pump and 8 + 8 PumpSwap fee and buyback recipients of the
//! live `Global` / `GlobalConfig` with their WSOL ATAs — where the `VenueExtras` account stays
//! inline; **(b)** the table `scripts/build-alt.sh` builds (`shortwin_keeper::alt`: (a) plus the
//! four `VenueExtras` PDAs).
//!
//! **Crank batches**: the largest `k` whose v0 batch fits, on the four live curves of the dump,
//! executed as one transaction (at least the keeper's v0 batch of 4, ARCH §7.1 duty 7). Also
//! asserted (pump-final §6.6: the v3 TradeExtras are 10 keys, the v2 list's 20 did not fit): the
//! keeper's composite `ComputeBudget ×2 + crank_price + liquidate` (ARCH §6.2), a liquidation
//! carrying a Jito tip transfer (duty 3's bundle path) and the tipped composite, each with the
//! largest extras set; and the synthetic exits past `rT` (a client close; the keeper's tipped
//! `[crank_price, expire]`), executed through the tables.
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
    Close, Expire, Liquidate, Open, PriceCranked, SolSettle, TraderSwept, VenueStateChanged,
};
use shortwin::state::{ENTRY_FIXED, EXTRAS_IX_BUY, EXTRAS_IX_SELL, EXTRAS_MAX_ENTRIES, VenueKind};
use shortwin::venues::{EXTRAS_SET_CURRENT, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, open_short_ix, set_venue_extras_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{custom_code, ok};
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve};
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
/// The keeper's v0 crank batch (ARCH §7.1 duty 7; `KeeperConfig::batch_v0`).
const KEEPER_BATCH_V0: usize = 4;

const SPL: PumpCoin = PumpCoin::Legacy115;
const T22: PumpCoin = PumpCoin::T22_115;
const NEAR: PumpCoin = PumpCoin::Near;

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

/// A28's pump part as ARCH §6.4 enumerates it, read from the fork's live `Global` and
/// `GlobalConfig` (`shortwin_keeper::alt`'s order: statics, then the recipients with their WSOL
/// ATAs, each key once).
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

/// (a) A28 as ARCH §6.4 enumerates it (the Raydium part, then the pump part); (b) the script's
/// table: (a) + the four `VenueExtras`.
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

/// Installs both tables in the VM and returns them.
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
/// and self-paid by `signer`; asserts the separate-payer worst case fits; returns the sizes
/// through (a) and (b).
fn assert_fits(what: &str, env: &Env, signer: &Keypair, ixs: &[Instruction]) -> [usize; 2] {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    let mut out = [0; 2];
    for (i, alt) in tables(&env.svm).iter().enumerate() {
        let (sep, sep_keys) = measure(&v0_tx(&env.payer, &[signer], &all, alt, bh));
        let (own, own_keys) = measure(&v0_tx(signer, &[], &all, alt, bh));
        println!(
            "  {what:<52} {:<9} {sep:>5} B / {sep_keys:>2} accounts (self-paid {own:>5} B / {own_keys:>2}); margin {} B",
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

/// Measures `ixs` (with the compute budget) through both tables for instructions nobody but the
/// fee payer signs (`crank_price`, `sweep_trader`: the keeper is the payer); asserts it fits.
fn assert_fits_unsigned(what: &str, env: &Env, ixs: &[Instruction]) {
    let all = with_budget(ixs);
    let bh = env.svm.latest_blockhash();
    for alt in tables(&env.svm) {
        let (size, keys) = measure(&v0_tx(&env.payer, &[], &all, &alt, bh));
        println!(
            "  {what:<52} {:<9} {size:>5} B / {keys:>2} accounts; margin {} B",
            alt.name,
            TX_MAX as i64 - size as i64
        );
        assert!(size <= TX_MAX, "{what} through {}: {size} B", alt.name);
        assert!(keys <= ACCOUNTS_MAX, "{what}: {keys} accounts");
    }
}

/// Sends `ixs` (with the compute budget) as a v0 transaction through `alt` (installed in the VM),
/// paid by the harness payer and signed by `signers`, with the invariant hooks after **every**
/// instruction: every existing market the transaction names is tracked; each proper prefix is
/// replayed on a clone of the VM (signature checks off) and its last instruction checked
/// (`check_ix` against the state before it, then `check_all` on every tracked market); then the
/// transaction runs and its last instruction is checked the same way (as `Env::send` does for
/// legacy transactions; as `tx_size_raydium`).
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

/// `vT/200`, the `pump_it` position size.
fn q_of(env: &Env, c: &PumpCurve) -> u64 {
    c.state(&env.svm).virtual_token_reserves / 200
}

/// Lists `c`, lends `40·Q`, waits out the warm-up, cranks once (a fresh curve gets `vT/7` of
/// buys first, as `pump_it`).
fn add_market(env: &mut Env, c: PumpCurve) -> (PumpMarket, u64) {
    if c.coin == PumpCoin::Fresh {
        let u = CurveUser::new(env, &c);
        let q = u64::try_from(c.curve_state(&env.svm).vt / 7).unwrap();
        ok(u.buy(env, &c, q, u64::MAX));
    }
    ok(env.list_pump(&c));
    let q = q_of(env, &c);
    let pm = env.pump_market_listed(PumpSpec::new(c.coin, q.saturating_mul(40)), c);
    env.crank(&pm);
    (pm, q)
}

/// Publishes the largest extras set (4 fixed read-only entries, existing System accounts) for
/// the curve's sell and buy kinds (the Emergency key) and returns the selection and the
/// trailing metas (`VenueExtras` first).
fn worst_extras(env: &mut Env) -> (ExtrasSel, Vec<AccountMeta>) {
    let kind = VenueKind::PumpCurve;
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
    pm: &PumpMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = pm.market();
    let k = pm.keys();
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
        &pm.trade_remaining(extras),
    )
}

/// `expire(sel)` with `ReadSet ++ TradeExtras ++ extras`.
fn expire_x(
    pm: &PumpMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = pm.market();
    let k = pm.keys();
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
        &pm.trade_remaining(extras),
    )
}

fn open_x(
    pm: &PumpMarket,
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

// ---- tests ---------------------------------------------------------------------------------------

/// Every trading and settlement transaction of a curve market in its worst case (module docs),
/// measured through both tables and **executed** through them in turn: two opens (a first-time
/// borrower each), a no-op `sol_settle`, a close, an expiry at the deadline and a liquidation after
/// a real pump — all with the largest extras set; then `sweep_trader`'s token leg.
#[test]
fn every_curve_transaction_fits_and_executes_through_the_protocol_alt() {
    for coin in [SPL, T22] {
        println!("{coin:?}");
        let (mut env, c) = Env::fork_pump_coin(coin);
        let (pm, q) = add_market(&mut env, c);
        let [a, b] = install_tables(&mut env);
        let (sel, extras) = worst_extras(&mut env);
        let keeper = env.funded_keypair();

        // open_short, first-time borrowers, largest extras: through (a), then (b).
        let mut b1 = env.borrower();
        let mut b2 = env.borrower();
        assert_fits(
            "open_short (new borrower, 4 extras)",
            &env,
            &b1.kp,
            &[open_x(&pm, &b1, q, sel, &extras)],
        );
        env.warp(1);
        let meta = ok(send_v0_hooked(
            &mut env,
            &[open_x(&pm, &b1, q, sel, &extras)],
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
            &[open_x(&pm, &b2, q, sel, &extras)],
            &[&b2.kp],
            &b,
        ));
        let _: Open = one(&meta);
        let n2 = b2.next_nonce;
        b2.next_nonce += 1;

        // sol_settle (no extras; the ShareEpoch record named) — a no-op on a healthy position.
        env.tick(&pm, 1);
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
        env.tick(&pm, 3);
        let meta = ok(send_v0_hooked(&mut env, &[exp], &[&keeper], &b));
        let ev: Expire = one(&meta);
        assert!(ev.keeper_reward > 0);

        // liquidate after a real pump, largest extras; the keeper's composite and a tip
        // measured.
        let mut b3 = env.borrower();
        env.warp(1);
        ok(send_v0_hooked(
            &mut env,
            &[open_x(&pm, &b3, q, sel, &extras)],
            &[&b3.kp],
            &a,
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
        // pump-final §6.6: the v3 TradeExtras (10 keys, 4 fewer inline and 6 fewer table keys
        // than v2's 20) bring the keeper's composite, the tipped liquidation and the tipped
        // composite with the largest extras set within v0 (ARCH:1868 measured 1,223 B and
        // 1,278 B for the first two on the v2 list).
        assert_fits(
            "crank_price + liquidate (4 extras; keeper composite)",
            &env,
            &keeper,
            &[crank_price_ix(&pm), liq.clone()],
        );
        assert_fits(
            "liquidate (4 extras) + Jito tip transfer",
            &env,
            &keeper,
            &[liq.clone(), tip_transfer(&keeper.pubkey(), 1)],
        );
        assert_fits(
            "crank_price + liquidate (4 extras) + Jito tip",
            &env,
            &keeper,
            &[
                crank_price_ix(&pm),
                liq.clone(),
                tip_transfer(&keeper.pubkey(), 1),
            ],
        );
        // Without extras (the keeper's normal case) both fit through either table.
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
        env.tick(&pm, 3);
        let u = CurveUser::new(&mut env, &pm.curve);
        let vt = pm.curve.state(&env.svm).virtual_token_reserves;
        ok(u.buy(&mut env, &pm.curve, vt / 10 * 4, u64::MAX));
        let mut done = false;
        for _ in 0..900 {
            env.tick(&pm, 1);
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
        let sweep = pump::sweep_trader_pump_ix(&env, &pm);
        assert_fits_unsigned(
            "sweep_trader (token leg)",
            &env,
            std::slice::from_ref(&sweep),
        );
        let meta = ok(send_v0_hooked(&mut env, &[sweep], &[], &a));
        let ev: TraderSwept = one(&meta);
        assert_eq!(ev.lamports, 1_000_000);
    }
}

/// `create_market(Auto)` of a live curve with the pump init set (the listing a creator sends):
/// fits through both tables and executes through (a).
#[test]
fn a_curve_listing_fits_and_executes_through_the_protocol_alt() {
    for coin in [SPL, T22, NEAR] {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let [a, _] = install_tables(&mut env);
        let creator = env.funded_keypair();
        let ix = pump::create_pump_market_ix(&creator.pubkey(), &c);
        assert_fits(
            &format!("create_market (Auto, {coin:?}, init set)"),
            &env,
            &creator,
            std::slice::from_ref(&ix),
        );
        ok(send_v0_hooked(&mut env, &[ix], &[&creator], &a));
        assert_eq!(
            env.market(&pda::market(&c.mint).0).venue_kind(),
            Some(VenueKind::PumpCurve)
        );
    }
}

/// The migration paths (ARCH §5.3, §5.4; M2-SEC-5): the keeper's `[crank_price, sol_settle]`
/// that settles S3 on a stuck migration (`PumpMigrating`, the curve's ReadSet) and on a curve
/// that graduated elsewhere (`None`, the detection set), and the crank that moves a completed
/// curve to `PumpMigrating`: each fits through both tables and executes through them.
#[test]
fn migration_paths_fit_and_execute_through_the_protocol_alt() {
    // A stuck migration: S3 at migration_seen_at + 70 min.
    let (mut env, c) = Env::fork_pump_coin(NEAR);
    let (pm, q) = add_market(&mut env, c);
    let [a, b] = install_tables(&mut env);
    let mut bo = env.borrower();
    let (res, nonce) = env.open(&pm, &mut bo, q);
    ok(res);
    let keeper = env.funded_keypair();
    let deadline = env.position(&pm, &bo.key(), nonce).deadline;
    let now = env.now();
    env.warp(deadline - 1_800 - now);
    env.crank(&pm);
    env.warp(1);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt, u64::MAX));
    env.warp(1);
    let crank = crank_price_ix(&pm);
    assert_fits_unsigned(
        "crank_price (→ PumpMigrating)",
        &env,
        std::slice::from_ref(&crank),
    );
    let meta = ok(send_v0_hooked(&mut env, &[crank], &[], &a));
    let ev: VenueStateChanged = one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    let settle = |env: &Env| {
        [
            crank_price_ix(&pm),
            sol_settle_ix(env, &pm, &keeper.pubkey(), &bo.key(), nonce),
        ]
    };
    assert_fits(
        "crank_price + sol_settle (PumpMigrating)",
        &env,
        &keeper,
        &settle(&env),
    );
    let mut settled = false;
    for _ in 0..200 {
        env.warp(30);
        let ixs = settle(&env);
        let meta = ok(send_v0_hooked(&mut env, &ixs, &[&keeper], &b));
        if events::of::<SolSettle>(&meta)
            .first()
            .is_some_and(|e| e.condition == 3)
        {
            settled = true;
            break;
        }
    }
    assert!(settled, "S3 on the stuck migration");

    // Graduated elsewhere: `→ None`, then S3 on the detection set.
    let (mut env, c) = Env::fork_pump_coin(NEAR);
    let (pm, q) = add_market(&mut env, c);
    let [a, b] = install_tables(&mut env);
    let mut bo = env.borrower();
    let (res, nonce) = env.open(&pm, &mut bo, q);
    ok(res);
    let keeper = env.funded_keypair();
    let deadline = env.position(&pm, &bo.key(), nonce).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    env.crank(&pm);
    env.warp(1);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt, u64::MAX));
    env.warp(1);
    env.crank(&pm);
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    let meta = ok(send_v0_hooked(&mut env, &[crank_price_ix(&pm)], &[], &a));
    let ev: VenueStateChanged = one(&meta);
    assert_eq!(ev.to, VenueKind::None as u8);
    let none = AsNone(&pm);
    let settle = |env: &Env| {
        [
            crank_price_ix(&none),
            sol_settle_ix(env, &none, &keeper.pubkey(), &bo.key(), nonce),
        ]
    };
    assert_fits(
        "crank_price + sol_settle (None)",
        &env,
        &keeper,
        &settle(&env),
    );
    let mut settled = false;
    for _ in 0..200 {
        env.warp(30);
        let ixs = settle(&env);
        let meta = ok(send_v0_hooked(&mut env, &ixs, &[&keeper], &b));
        if events::of::<SolSettle>(&meta)
            .first()
            .is_some_and(|e| e.condition == 3)
        {
            settled = true;
            break;
        }
    }
    assert!(settled, "S3 on the graduated-elsewhere market");
}

/// `simulate_transaction` of `ixs` with the compute budget, paid by the harness payer (nothing is
/// committed): whether it would succeed.
fn would_land(env: &Env, ixs: &[Instruction], signers: &[&Keypair]) -> bool {
    let all = with_budget(ixs);
    let mut keys: Vec<&Keypair> = vec![&env.payer];
    keys.extend(signers.iter().copied());
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        &all,
        Some(&env.payer.pubkey()),
        &keys,
        env.svm.latest_blockhash(),
    );
    env.svm.simulate_transaction(tx).is_ok()
}

/// Synthetic exits past `rT` (pump-final §5.2, §6.6, §12's T-SIZE of the keeper's v3 exits): on the
/// near-complete curve the crowd leaves `rT = q/2`, and the exit's `buy_v3(q)` buys `rT` on the
/// curve and `q − rT` from the would-be pool. Its accounts are the ordinary exit's (the same 17 of
/// `buy_v3`), so it fits as any exit does; executed through the tables with the largest extras set:
/// a client close through (a), and — on a second fork, past the deadline — the keeper's tipped
/// `[crank_price, expire]` through (b), the largest keeper transaction. Each completes the curve
/// (`PumpMigrating` in the instruction, pump's `PostCompleteBuyEvent`).
#[test]
fn synthetic_exits_fit_and_execute_through_the_protocol_alt() {
    // The client's synthetic close.
    let (mut env, c) = Env::fork_pump_coin(NEAR);
    let (pm, q) = add_market(&mut env, c);
    let [a, b] = install_tables(&mut env);
    let (sel, extras) = worst_extras(&mut env);
    let mut bo = env.borrower();
    env.warp(1);
    ok(send_v0_hooked(
        &mut env,
        &[open_x(&pm, &bo, q, sel, &extras)],
        &[&bo.kp],
        &b,
    ));
    let n = bo.next_nonce;
    bo.next_nonce += 1;
    env.tick(&pm, 2);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt - q / 2, u64::MAX));
    env.warp(1);
    let close = close_short_ix(&pm, &bo.key(), n, u64::MAX, sel, &extras);
    assert_fits(
        "close_short past rT (synthetic; 4 extras)",
        &env,
        &bo.kp,
        std::slice::from_ref(&close),
    );
    let meta = ok(send_v0_hooked(&mut env, &[close], &[&bo.kp], &a));
    let ev: Close = one(&meta);
    assert_eq!(ev.q, q);
    assert_eq!(fork::pump_post_complete_buy_events(&meta).len(), 1);
    let ev: VenueStateChanged = one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);

    // The keeper's tipped synthetic expiry.
    let (mut env, c) = Env::fork_pump_coin(NEAR);
    let (pm, q) = add_market(&mut env, c);
    let [_, b] = install_tables(&mut env);
    let (sel, extras) = worst_extras(&mut env);
    let mut bo = env.borrower();
    let (res, n) = env.open(&pm, &mut bo, q);
    ok(res);
    let keeper = env.funded_keypair();
    let deadline = env.position(&pm, &bo.key(), n).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    env.tick(&pm, 3);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt - q / 2, u64::MAX));
    let exit = || {
        [
            crank_price_ix(&pm),
            expire_x(&pm, &keeper.pubkey(), &bo.key(), n, sel, &extras),
            tip_transfer(&keeper.pubkey(), 1_000_000),
        ]
    };
    assert_fits(
        "crank_price + expire past rT (4 extras) + Jito tip",
        &env,
        &keeper,
        &exit(),
    );
    let mut landed = None;
    for _ in 0..600 {
        env.warp(1);
        if would_land(&env, &exit(), &[&keeper]) {
            landed = Some(ok(send_v0_hooked(&mut env, &exit(), &[&keeper], &b)));
            break;
        }
        env.crank(&pm);
    }
    let meta = landed.expect("the synthetic expiry lands");
    let ev: Expire = one(&meta);
    assert!(ev.buyback_cost > 0);
    assert_eq!(fork::pump_post_complete_buy_events(&meta).len(), 1);
    assert!(pm.curve.state(&env.svm).complete);
    assert_eq!(
        env.market(&pm.market()).venue_kind(),
        Some(VenueKind::PumpMigrating)
    );
}

/// Crank batches (ARCH §6.4 table, §7.1 duty 7): the four live curves of the dump. Every batch
/// of 1–4 cranks fits v0 through both tables, and the keeper's batch of 4 executes as one
/// transaction (one `PriceCranked` per market). (The largest v0 batch — 5 curve cranks, the sixth
/// needs v1 — is sized with synthetic markets in `tests/bench/m5_cu`, `t_trace`.)
#[test]
fn crank_batches_fit_through_the_protocol_alt() {
    let (mut env, first) = Env::fork_pump_coin(SPL);
    let mut ms = vec![add_market(&mut env, first).0];
    for coin in [T22, PumpCoin::Fresh, NEAR] {
        let c = env.load_pump_coin(coin);
        ms.push(add_market(&mut env, c).0);
    }
    let [a, b] = install_tables(&mut env);
    env.warp(1);
    let cranks: Vec<Instruction> = ms.iter().map(crank_price_ix).collect();
    let keeper = env.funded_keypair();
    let bh = env.svm.latest_blockhash();
    for alt in [&a, &b] {
        for k in 1..=cranks.len() {
            let all = with_budget(&cranks[..k]);
            let (size, keys) = measure(&v0_tx(&keeper, &[], &all, alt, bh));
            println!(
                "  crank batch k = {k} (curve) {:<9} {size:>5} B / {keys:>2} accounts",
                alt.name
            );
            assert!(size <= TX_MAX && keys <= ACCOUNTS_MAX, "{k} cranks");
        }
    }
    let meta = ok(send_v0_hooked(
        &mut env,
        &cranks[..KEEPER_BATCH_V0],
        &[],
        &a,
    ));
    let ev: Vec<PriceCranked> = events::of(&meta);
    assert_eq!(ev.len(), KEEPER_BATCH_V0);
    env.warp(1);
    let meta = ok(send_v0_hooked(&mut env, &cranks, &[], &b));
    assert_eq!(events::of::<PriceCranked>(&meta).len(), 4);
}
