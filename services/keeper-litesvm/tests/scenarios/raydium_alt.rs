//! The protocol ALT (ARCH §6.4, A28) end to end: `shortwin_keeper::alt` — the code behind
//! `shortwin-build-alt` / `scripts/build-alt.sh` — creates, extends and freezes the table through
//! the keeper's own chain backend on the Raydium fork (the address-lookup-table program LiteSVM
//! ships), and the keeper's v0 transactions (crank batches and a liquidation) then run through it.
//!
//! It also pins the chain of assumptions of T-SIZE: every address the program tests put in
//! their ALT (`shortwin_litesvm_tests::raydium::protocol_alt_addresses`, used by
//! `tests/litesvm/tests/tx_size_raydium.rs` and `ray_it`) is in the table the script builds, so
//! the sizes measured there are the sizes the real table gives.

use anchor_lang::prelude::Pubkey;
use shortwin::events::Liquidate;
use shortwin_keeper::alt;
use shortwin_keeper::config::TxFormatPolicy;
use shortwin_keeper::events;
use shortwin_keeper_litesvm::block_on;
use shortwin_litesvm_tests::engine::VenueMarket;
use shortwin_litesvm_tests::raydium::protocol_alt_addresses;
use shortwin_services_common::tx::{TxFormat, V0_MAX_TX_BYTES};
use shortwin_services_common::{AccountData, Chain};
use solana_signer::Signer;

use crate::common::*;
use crate::raydium::*;

/// The first slot of the `SlotHashes` sysvar (bincode: `u64` length, then `(slot, hash)`
/// entries): a `recent_slot` the lookup-table program accepts.
fn recent_slot(h: &Harness) -> u64 {
    let key: Pubkey = "SysvarS1otHashes111111111111111111111111111"
        .parse()
        .unwrap();
    let acc = block_on(h.chain().accounts(&[key])).unwrap()[0]
        .clone()
        .expect("SlotHashes");
    assert!(u64::from_le_bytes(acc.data[..8].try_into().unwrap()) >= 1);
    u64::from_le_bytes(acc.data[8..16].try_into().unwrap())
}

fn dump(owner: Pubkey, data: &[u8]) -> AccountData {
    AccountData {
        lamports: 1,
        owner,
        executable: false,
        data: data.to_vec(),
    }
}

#[test]
fn the_protocol_alt_is_built_on_chain_and_carries_the_keepers_v0_transactions() {
    let (mut h, ms) = ray_setup(&[FART, USELESS, T22], KeeperConfig::default());
    let payer = h.with_env(|env| env.funded_keypair());
    let authority = h.with_env(|env| env.funded_keypair());
    let statics = alt::keys(&alt::static_entries());

    // create + extend with the static keys (the script's `create`).
    let slot = recent_slot(&h);
    let table = block_on(alt::create(
        h.chain(),
        &payer,
        &authority,
        slot,
        &statics,
        false,
        0,
    ))
    .expect("create the table");
    h.with_env(|env| {
        let c = env.clock();
        env.set_clock(c.unix_timestamp, c.slot + 1);
    });
    // `extend`: the live recipients (here from the mainnet dumps the unit tests pin).
    let recipients = alt::recipient_entries(
        &dump(
            shortwin::constants::PUMP_PROGRAM_ID,
            include_bytes!("../../../keeper/tests/data/pump_global.bin"),
        ),
        &dump(
            shortwin::constants::PUMP_AMM_PROGRAM_ID,
            include_bytes!("../../../keeper/tests/data/pamm_global_config.bin"),
        ),
    )
    .unwrap();
    let all = alt::keys(&alt::protocol_entries(alt::static_entries(), recipients).unwrap());
    let added = block_on(alt::extend(h.chain(), &payer, &authority, &table, &all, 0)).unwrap();
    assert_eq!(added, 2 * 17, "17 distinct recipients and their WSOL ATAs");
    // Nothing left to add; then freeze.
    assert_eq!(
        block_on(alt::extend(h.chain(), &payer, &authority, &table, &all, 0)).unwrap(),
        0
    );
    block_on(alt::send(
        h.chain(),
        &payer,
        &[&authority],
        &[alt::freeze_ix(&table, &authority.pubkey())],
        0,
    ))
    .expect("freeze");
    h.with_env(|env| {
        let c = env.clock();
        env.set_clock(c.unix_timestamp, c.slot + 1);
    });
    let state = block_on(alt::read_table(h.chain(), &table)).unwrap();
    assert_eq!(state.addresses, all, "in the script's order");
    assert_eq!(state.authority, None, "frozen");
    assert_eq!(state.deactivation_slot, u64::MAX);
    assert!(state.missing(&all).is_empty());
    assert!(
        block_on(alt::extend(
            h.chain(),
            &payer,
            &authority,
            &table,
            &[Pubkey::new_unique()],
            0
        ))
        .is_err(),
        "a frozen table cannot be extended"
    );
    // T-SIZE's table is a subset of this one.
    let missing: Vec<Pubkey> = protocol_alt_addresses()
        .into_iter()
        .filter(|k| !state.addresses.contains(k))
        .collect();
    assert!(
        missing.is_empty(),
        "the T-SIZE ALT assumes keys the real one lacks: {missing:?}"
    );
    println!("protocol ALT {table}: {} addresses", state.addresses.len());

    // The keeper on v0 with the table: a crank batch of the three markets through it.
    {
        let cfg = h.keeper.config_mut();
        cfg.tx_format = TxFormatPolicy::V0;
        cfg.lookup_tables = vec![table.to_string()];
    }
    let positions: Vec<_> = ms.iter().map(|(rm, q)| ray_open(&h, rm, *q)).collect();
    let before = h.chain().sent().len();
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(h.keeper.lookup_tables().len(), 1);
    assert_eq!(r.cranked.len(), 3, "{r:?}");
    assert_eq!(r.batches.len(), 1);
    assert_eq!(r.batches[0].format, TxFormat::V0);
    let s = &h.chain().sent()[before];
    assert!(s.ok && s.bytes <= V0_MAX_TX_BYTES, "{s:?}");
    println!(
        "v0 batch of 3 Raydium cranks through the built ALT: {} B, {} accounts",
        s.bytes, s.accounts
    );

    // A liquidation over the table (v0): the whale pumps the AMM v4 pool.
    let (fart, _) = &ms[0];
    let (b, nonce) = &positions[0];
    let pos = position_of(fart, &b.key(), *nonce);
    let (_, y) = fart.reserves(&h.env.borrow().svm).unwrap();
    whale_buy(&h, fart, y / 1_000 * 484);
    let mut done = false;
    for s in 0..60 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if let Some(x) = r.exits.iter().find(|e| {
            e.position == pos && e.kind == ExitKind::Liquidate && e.result.landed().is_some()
        }) {
            let ExitResult::Landed {
                outcome, format, ..
            } = &x.result
            else {
                unreachable!()
            };
            assert_eq!(*format, TxFormat::V0);
            let l: Liquidate = events::of(outcome).into_iter().next().expect("Liquidate");
            assert_eq!(l.position, pos);
            let sent = h.chain().sent();
            let tx = sent
                .iter()
                .rev()
                .find(|t| t.instructions == 1 && t.ok)
                .unwrap();
            println!(
                "liquidate (AMM v4) as v0 through the built ALT: {} B, {} accounts",
                tx.bytes, tx.accounts
            );
            assert!(tx.bytes <= V0_MAX_TX_BYTES);
            done = true;
            break;
        }
    }
    assert!(done, "no liquidation");
    assert!(!is_open(&h, fart, &b.key(), *nonce));
    let _ = fart.market();
}
