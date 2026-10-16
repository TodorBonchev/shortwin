//! **T-SIZE of the PumpSwap 4-extras transactions** through the lookup tables the keeper's own
//! `alt` module builds on chain (M7 K6; OWNER-M7-1; ARCH §6.4 row 56): since M6R-1's LP mint (P6,
//! a per-pool key inline) the open with the largest (4-entry) extras set is 6–7 B over 1,232 B
//! through A28 **exactly as ARCH §6.4 enumerates it** — an owner trade-off. This scenario measures
//! the options on the PumpSwap fork, every transaction built by the keeper's builder
//! (`shortwin_services_common::tx`, both ComputeBudget instructions) and **executed** under the
//! invariant hooks:
//!
//! - **(a)** A28 as enumerated (static keys, live pump / PumpSwap recipients and their WSOL ATAs);
//! - **(b)** the table `shortwin-build-alt create` builds: (a) plus the four `VenueExtras` PDAs
//!   (the flagged addition, `alt::static_entries`);
//! - **(m)** the market's own table (`alt::market_entries`, `shortwin-build-alt market`): the
//!   per-market keys the protocol table leaves inline, extendable; its rent is measured.
//!
//! Transactions: a first-time borrower's `open_short` with a separate fee payer and the largest
//! extras set (4 fixed read-only entries published for sell and buy, `current`); the keeper's
//! `liquidate` with that set (sent by the real keeper on v0 after a genuine pump) and its
//! `[crank_price, liquidate]` composite; the keeper's `crank_price` alone. `M7_COST_WRITE=1`
//! writes `tests/bench/m7_keeper/tsize_pumpswap.{json,md}`.

use std::fmt::Write as _;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::events::{Liquidate, Open};
use shortwin::state::{ENTRY_FIXED, EXTRAS_IX_BUY, EXTRAS_IX_SELL, EXTRAS_MAX_ENTRIES, VenueKind};
use shortwin::venues::{EXTRAS_SET_CURRENT, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_keeper::config::TxFormatPolicy;
use shortwin_keeper::{alt, events, ixs, venue, view};
use shortwin_keeper_litesvm::block_on;
use shortwin_litesvm_tests::engine::{VenueMarket, open_short_ix, set_venue_extras_ix};
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammUser};
use shortwin_litesvm_tests::pda;
use shortwin_services_common::Chain;
use shortwin_services_common::tx::{self, ComputeBudget, TxFormat, V0_MAX_TX_BYTES};
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;
use solana_signer::Signer;

use crate::common::*;

const SOL: u64 = 1_000_000_000;

/// The first slot of the `SlotHashes` sysvar: a `recent_slot` the lookup-table program accepts.
fn recent_slot(h: &Harness) -> u64 {
    let key: Pubkey = "SysvarS1otHashes111111111111111111111111111"
        .parse()
        .unwrap();
    let acc = block_on(h.chain().accounts(&[key])).unwrap()[0]
        .clone()
        .expect("SlotHashes");
    u64::from_le_bytes(acc.data[8..16].try_into().unwrap())
}

fn next_slot(h: &Harness) {
    h.with_env(|env| {
        let c = env.clock();
        env.set_clock(c.unix_timestamp, c.slot + 1);
    });
}

/// Creates a table with `addresses` through the keeper's `alt::create` (extendable).
fn table(h: &Harness, addresses: &[Pubkey]) -> AddressLookupTableAccount {
    let payer = h.with_env(|env| env.funded_keypair());
    let slot = recent_slot(h);
    let key = block_on(alt::create(
        h.chain(),
        &payer,
        &payer,
        slot,
        addresses,
        false,
        0,
    ))
    .expect("create the table");
    next_slot(h);
    let state = block_on(alt::read_table(h.chain(), &key)).expect("table");
    assert_eq!(state.addresses, addresses);
    state.account()
}

/// A position size inside every default cap on `p` now (1 % of the base reserve, at most ≈ 10 SOL
/// of notional).
fn q_for(env: &Env, p: &pamm::PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let by_notional = base * u128::from(10 * SOL) / r.effective_quote.max(1);
    u64::try_from((base / 100).min(by_notional)).expect("u64")
}

/// Cranks every 30 s until the boost window is over (opens wait for it).
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
        env.crank(pm);
    }
}

/// Publishes the largest extras set (4 fixed read-only entries) for PumpSwap's sell and buy
/// kinds; returns the selection and the trailing metas (`VenueExtras` first).
fn publish_worst_extras(env: &mut Env) -> (ExtrasSel, Vec<AccountMeta>) {
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

/// The size (bytes, accounts) of `ixs` as a v0 transaction paid by `payer` and signed by
/// `signers`, both ComputeBudget instructions set, through `alts`.
fn size(
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    alts: &[AddressLookupTableAccount],
) -> (usize, usize) {
    let budget = ComputeBudget {
        unit_limit: tx::MAX_COMPUTE_UNITS,
        unit_price: 1,
    };
    let t = tx::build(
        TxFormat::V0,
        payer,
        signers,
        ixs,
        &budget,
        solana_hash::Hash::default(),
        alts,
    )
    .expect("build");
    (tx::size(&t), tx::account_count(&t))
}

/// One row of the size table.
#[derive(Clone, Debug)]
struct Row {
    what: &'static str,
    /// Bytes through (a), (b), (a)+(m), (b)+(m).
    bytes: [usize; 4],
    accounts: [usize; 4],
}

const TABLES: [&str; 4] = ["A28 (a)", "build-alt (b)", "(a) + market", "(b) + market"];

fn row(
    what: &'static str,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    t: &[Vec<AddressLookupTableAccount>; 4],
) -> Row {
    let mut r = Row {
        what,
        bytes: [0; 4],
        accounts: [0; 4],
    };
    for (i, alts) in t.iter().enumerate() {
        (r.bytes[i], r.accounts[i]) = size(payer, signers, ixs, alts);
    }
    println!(
        "  {what:<46} {}",
        TABLES
            .iter()
            .zip(r.bytes)
            .map(|(n, b)| format!("{n}: {b} B"))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    r
}

/// What one pool measured.
struct Measured {
    coin: PammCoin,
    rows: Vec<Row>,
    market_table: usize,
    rent: u64,
    /// The keeper's liquidation as sent on v0 through (b) + (m): bytes, accounts.
    liq_sent: (usize, usize),
}

/// Lists `coin`'s pool (the curve account extended if the pool predates it, as pAMM needs),
/// lends `40·q`, warms up and waits out the boost.
fn listed(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let q = q_for(&env, &p);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < shortwin_venue_state::pump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 60);
    wait_out_boost(&mut env, &pm);
    let q = q_for(&env, &pm.pool);
    (env, pm, q)
}

/// Builds the tables on `coin`'s fork, sizes every transaction through them, executes the open
/// through (b) and (a) + (m), and has the real keeper liquidate on v0 through (b) + (m).
fn measure(coin: PammCoin) -> Measured {
    let (mut env, pm, q) = listed(coin);
    let (sel, extras) = publish_worst_extras(&mut env);
    let key = pm.market();
    // The position is below the 1-SOL value-at-risk floor: covered all the same (its liquidation
    // is what is measured; `common::covering_all`).
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));

    // The tables, built on chain by the keeper's code.
    let live = block_on(alt::live_entries(h.chain())).expect("live entries");
    let b_keys = alt::keys(&live);
    let extras_pdas: Vec<Pubkey> = live
        .iter()
        .filter(|e| e.label.contains("venue_extras"))
        .map(|e| e.key)
        .collect();
    assert_eq!(extras_pdas.len(), 4, "the four VenueExtras PDAs");
    let a_keys: Vec<Pubkey> = b_keys
        .iter()
        .copied()
        .filter(|k| !extras_pdas.contains(k))
        .collect();
    let ta = table(&h, &a_keys);
    let tb = table(&h, &b_keys);
    let v = block_on(view::load(h.chain(), &[key])).expect("view");
    let mv = v.market(&key).expect("the market reads");
    let m_entries = alt::market_entries(mv, &b_keys).expect("market entries");
    println!("PumpSwap ({coin:?}): the market table");
    for e in &m_entries {
        println!("  {} {}", e.key, e.label);
    }
    let tm = table(&h, &alt::keys(&m_entries));
    // The fork carries mainnet's Rent sysvar (5,080 lamports per byte): the table holds exactly
    // its rent-exempt minimum.
    let rent = h.with_env(|env| env.svm.get_account(&tm.key).expect("table").lamports);
    assert_eq!(rent, alt::table_rent_lamports(m_entries.len()));
    assert_eq!(
        rent,
        h.with_env(|env| env
            .svm
            .minimum_balance_for_rent_exemption(56 + 32 * m_entries.len()))
    );
    // The per-market keys the program's own T-SIZE table holds (tx_size_pumpswap_it
    // `market_table`) that the keeper's derivation must cover — the market's accounts and its
    // pool's, the trader's per-market venue accounts.
    for k in [
        key,
        pm.keys().market_vault,
        pm.keys().pool_vault,
        pm.keys().trader,
        pm.pool.pool,
        pm.pool.base_vault,
        pm.pool.quote_vault,
        pm.pool.lp_mint,
        pm.trader_uva(),
        pda::trader_wsol(&key).0,
    ] {
        assert!(
            tm.addresses.contains(&k),
            "{k} missing from the market table"
        );
    }
    let t: [Vec<AddressLookupTableAccount>; 4] = [
        vec![ta.clone()],
        vec![tb.clone()],
        vec![ta.clone(), tm.clone()],
        vec![tb.clone(), tm.clone()],
    ];

    // ---- the client open with the largest extras set (a first-time borrower) ----
    let fee_payer = h.with_env(|env| env.funded_keypair());
    let mut borrowers: Vec<Borrower> = (0..2).map(|_| h.with_env(|env| env.borrower())).collect();
    let open_ix = |b: &Borrower| {
        open_short_ix(
            &pm,
            &b.key(),
            b.next_nonce,
            q,
            0,
            u64::MAX,
            u64::MAX,
            sel,
            &extras,
        )
    };
    println!("PumpSwap ({coin:?}) with the largest (4-entry) extras set, v0 sizes:");
    let open_row = row(
        "open_short (first-time borrower, separate payer)",
        &fee_payer,
        &[&borrowers[0].kp],
        &[open_ix(&borrowers[0])],
        &t,
    );
    let open_self_row = row(
        "open_short (first-time borrower pays its fee)",
        &borrowers[0].kp,
        &[],
        &[open_ix(&borrowers[0])],
        &t,
    );
    // Executed through (b) and through (a) + (m).
    h.with_env(|env| env.crank(&pm));
    for (i, alts) in [(0usize, &t[1]), (1, &t[2])] {
        let b = &borrowers[i];
        let ix = open_ix(b);
        let bh = h.with_env(|env| env.svm.latest_blockhash());
        let budget = ComputeBudget {
            unit_limit: 1_400_000,
            unit_price: 1,
        };
        let tx =
            tx::build(TxFormat::V0, &fee_payer, &[&b.kp], &[ix], &budget, bh, alts).expect("build");
        assert!(tx::size(&tx) <= V0_MAX_TX_BYTES);
        let out = block_on(h.chain().send(&tx)).expect("send");
        assert!(out.ok(), "open {i}: {:?}\n{}", out.err, out.logs.join("\n"));
        assert_eq!(events::of::<Open>(&out).len(), 1);
    }
    for b in &mut borrowers {
        b.next_nonce += 1;
    }

    // ---- the keeper's liquidation and composite, sized on its own builder ----
    let keeper_kp = h.with_env(|env| env.funded_keypair());
    let v = block_on(view::load(h.chain(), &[key])).expect("view");
    let mv = v.market(&key).expect("market");
    let pv = mv.positions.first().expect("a position");
    let choice = venue::extras_candidates(mv.kind, EXTRAS_IX_BUY, &mv.market, mv.extras.as_deref())
        .into_iter()
        .find(|c| c.sel.set == EXTRAS_SET_CURRENT)
        .expect("the published set");
    assert_eq!(choice.metas.len(), 1 + EXTRAS_MAX_ENTRIES);
    let liq = ixs::liquidate(&keeper_kp.pubkey(), mv, pv, &choice).expect("liquidate");
    let crank = ixs::crank_price(mv);
    let liq_row = row(
        "keeper liquidate (4 extras)",
        &keeper_kp,
        &[],
        std::slice::from_ref(&liq),
        &t,
    );
    let comp_row = row(
        "keeper [crank_price, liquidate] (4 extras)",
        &keeper_kp,
        &[],
        &[crank.clone(), liq],
        &t,
    );
    let crank_row = row("keeper crank_price", &keeper_kp, &[], &[crank], &t);

    // ---- the real keeper on v0 through (b) + (m): its liquidation after a pump lands ----
    {
        let cfg = h.keeper.config_mut();
        cfg.tx_format = TxFormatPolicy::V0;
        cfg.lookup_tables = vec![tb.key.to_string(), tm.key.to_string()];
    }
    h.seconds(2);
    let whale = h.with_env(|env| PammUser::new(env, &pm.pool, 50_000 * SOL));
    let mut landed: Option<(usize, usize)> = None;
    for s in 0..600 {
        h.with_env(|env| {
            env.warp(1);
            ok(whale.push_price(env, &pm.pool, 104, 100));
        });
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if let Some(x) = r
            .exits
            .iter()
            .find(|e| e.kind == ExitKind::Liquidate && e.result.landed().is_some())
        {
            let ExitResult::Landed {
                outcome,
                format,
                bytes,
                ..
            } = &x.result
            else {
                unreachable!()
            };
            assert_eq!(*format, TxFormat::V0);
            assert_eq!(events::of::<Liquidate>(outcome).len(), 1);
            let sent = h.chain().sent();
            let tx = sent
                .iter()
                .rev()
                .find(|t| t.ok && t.instructions == 1 && t.bytes == *bytes)
                .expect("the liquidation's transaction");
            landed = Some((tx.bytes, tx.accounts));
            break;
        }
    }
    let liq_sent = landed.expect("the keeper liquidated on v0");
    println!(
        "  the keeper's liquidation, sent on v0 through (b) + market: {} B, {} accounts",
        liq_sent.0, liq_sent.1
    );

    // ---- the gate ----
    // Through A28 as enumerated the open with a separate fee payer does not fit (pinned: over by
    // at most 24 B, M6R-1); through the table build-alt builds it fits, and through a market
    // table with room to spare. A borrower paying its own fee fits every table. The keeper pays
    // its own fees (one signature, no separate payer key): its liquidation and its `[crank_price,
    // liquidate]` composite with the largest set fit every table, A28 as enumerated included.
    assert!(
        open_row.bytes[0] > V0_MAX_TX_BYTES && open_row.bytes[0] <= V0_MAX_TX_BYTES + 24,
        "{open_row:?}"
    );
    assert!(
        open_row.bytes[1..].iter().all(|b| *b <= V0_MAX_TX_BYTES),
        "{open_row:?}"
    );
    for r in [&open_row, &open_self_row, &liq_row, &comp_row, &crank_row] {
        assert!(r.accounts.iter().all(|a| *a <= 64), "{r:?}");
    }
    for r in [&open_self_row, &liq_row, &comp_row, &crank_row] {
        assert!(r.bytes.iter().all(|b| *b <= V0_MAX_TX_BYTES), "{r:?}");
    }
    // The separate payer costs its signature and its key: 64 + 32 B.
    assert_eq!(open_row.bytes[0] - open_self_row.bytes[0], 96);
    assert!(liq_sent.0 <= V0_MAX_TX_BYTES);
    Measured {
        coin,
        rows: vec![open_row, open_self_row, liq_row, comp_row, crank_row],
        market_table: m_entries.len(),
        rent,
        liq_sent,
    }
}

/// `lo–hi`, or one number.
fn span(v: impl Iterator<Item = usize>) -> (usize, usize) {
    v.fold((usize::MAX, 0), |(lo, hi), x| (lo.min(x), hi.max(x)))
}

fn fmt_span((lo, hi): (usize, usize)) -> String {
    if lo == hi {
        format!("{lo}")
    } else {
        format!("{lo}–{hi}")
    }
}

/// ★ OWNER-M7-1, measured (module docs) on three pools: an SPL coin (`Legacy261`), the mature
/// Token-2022 pool with third-party LP and a cashback coin.
#[test]
fn the_pumpswap_four_extras_transactions_through_the_keepers_tables() {
    let coins = [PammCoin::Legacy261, PammCoin::Mature, PammCoin::Cashback];
    let all: Vec<Measured> = coins.into_iter().map(measure).collect();
    let n_rows = all[0].rows.len();
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# m7_keeper — T-SIZE of the PumpSwap 4-extras transactions (OWNER-M7-1)\n\nGenerated by `services/keeper-litesvm` `pamm_alt::the_pumpswap_four_extras_transactions_through_the_keepers_tables` (`M7_COST_WRITE=1`) on the PumpSwap fork ({} pools). Every table is built on chain by the keeper's `alt` module — (a) A28 exactly as ARCH §6.4 enumerates it, (b) the table `shortwin-build-alt create` builds ((a) plus the four `VenueExtras` PDAs), (m) the market's own table (`shortwin-build-alt market`) —; every size is the keeper's v0 builder with both ComputeBudget instructions and the largest (4-entry, fixed) extras set published for sell and buy; the open is executed through (b) and through (a) + (m), and the real keeper's liquidation is sent on v0 through (b) + (m). Limit 1,232 B and 64 accounts; bytes as the range over the pools.\n\n| transaction | {} |\n|---|---|---|---|---|",
        all.iter()
            .map(|m| format!("{:?}", m.coin))
            .collect::<Vec<_>>()
            .join(", "),
        TABLES.join(" | ")
    );
    let mut j = String::new();
    let _ = writeln!(
        j,
        "{{\n  \"schema\": \"shortwin.m7.tsize_pumpswap/1\",\n  \"pools\": [{}],\n  \"limit_bytes\": {V0_MAX_TX_BYTES},\n  \"tables\": [\"a28\", \"build_alt\", \"a28_plus_market\", \"build_alt_plus_market\"],\n  \"rows\": [",
        all.iter()
            .map(|m| format!("\"{:?}\"", m.coin))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for i in 0..n_rows {
        let what = all[0].rows[i].what;
        let mut cells = Vec::new();
        let mut jb = Vec::new();
        for t in 0..4 {
            let b = span(all.iter().map(|m| m.rows[i].bytes[t]));
            let a = span(all.iter().map(|m| m.rows[i].accounts[t]));
            let flag = if b.1 > V0_MAX_TX_BYTES {
                " **over**"
            } else {
                ""
            };
            cells.push(format!(
                "{} B / {} accounts{flag}",
                fmt_span(b),
                fmt_span(a)
            ));
            jb.push(format!("[{}, {}]", b.0, b.1));
        }
        let _ = writeln!(md, "| {what} | {} |", cells.join(" | "));
        let _ = writeln!(
            j,
            "    {{\"what\": \"{what}\", \"bytes_min_max\": [{}]}}{}",
            jb.join(", "),
            if i + 1 < n_rows { "," } else { "" }
        );
    }
    let tables = span(all.iter().map(|m| m.market_table));
    let rents = (
        all.iter().map(|m| m.rent).min().unwrap_or(0),
        all.iter().map(|m| m.rent).max().unwrap_or(0),
    );
    let sent_b = span(all.iter().map(|m| m.liq_sent.0));
    let sent_a = span(all.iter().map(|m| m.liq_sent.1));
    let _ = writeln!(
        md,
        "\nThe market table (`alt::market_entries` against (b)): {} addresses (the market's accounts, its pool's ReadSet and trade extras, and the 4 extras keys, which a venue-wide table could hold instead), rent {}–{} lamports at mainnet's 5,080 lamports per byte (≈ {:.4} SOL; recovered on close). The keeper's liquidation sent on v0 through (b) + (m): {} B, {} accounts.",
        fmt_span(tables),
        rents.0,
        rents.1,
        rents.1 as f64 / 1e9,
        fmt_span(sent_b),
        fmt_span(sent_a)
    );
    let _ = writeln!(
        j,
        "  ],\n  \"market_table\": {{\"addresses_min_max\": [{}, {}], \"rent_lamports_min_max\": [{}, {}]}},\n  \"keeper_liquidation_sent_v0\": {{\"bytes_min_max\": [{}, {}], \"accounts_min_max\": [{}, {}]}}\n}}",
        tables.0, tables.1, rents.0, rents.1, sent_b.0, sent_b.1, sent_a.0, sent_a.1
    );
    println!("{md}\n{j}");
    if std::env::var_os("M7_COST_WRITE").is_some() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/bench/m7_keeper");
        std::fs::create_dir_all(&dir).expect("m7_keeper dir");
        std::fs::write(dir.join("tsize_pumpswap.md"), md).expect("md");
        std::fs::write(dir.join("tsize_pumpswap.json"), j).expect("json");
    }
}
