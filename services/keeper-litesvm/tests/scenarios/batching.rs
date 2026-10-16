//! Duty 7 (ARCH §7.1, §6.4): the cranks of one second share transactions — v1 batches of 6,
//! v0 batches of 4 where the cluster refuses v1 — each simulated first; a market whose crank
//! fails simulation is dropped from its batch and cranked alone.

use shortwin_services_common::tx::{TxFormat, V0_MAX_TX_BYTES, V1_MAX_TX_BYTES};

use crate::common::*;

fn seven_markets(h: &Harness, mms: &[MockMarket]) {
    for mm in mms {
        let _ = open(h, mm, Q);
    }
}

#[test]
fn seven_markets_share_two_v1_transactions() {
    let (mut h, mms) = setup(7, KeeperConfig::default());
    seven_markets(&h, &mms);
    let r = h.second().clone();
    assert_eq!(r.cranked.len(), 7, "{r:?}");
    let sizes: Vec<(usize, TxFormat)> = r
        .batches
        .iter()
        .map(|b| (b.markets.len(), b.format))
        .collect();
    assert_eq!(sizes, vec![(6, TxFormat::V1), (1, TxFormat::V1)]);
    let sent = h.chain().sent();
    assert_eq!(sent.len(), 2);
    for (b, s) in r.batches.iter().zip(&sent) {
        assert_eq!(s.instructions, b.markets.len());
        assert!(s.bytes <= V1_MAX_TX_BYTES && s.accounts <= 64, "{s:?}");
        // One signature (the keeper), the compute budget in the v1 config.
        assert!(b.fee >= 5_000);
        assert!(b.unit_limit as u64 >= b.units);
    }
    // T-SIZE input (ARCH §6.4): the mock's ReadSet has 3 keys (PumpSwap / CPMM: 6).
    println!(
        "v1 batch of 6 mock cranks: {} B, {} accounts, {} CU",
        sent[0].bytes, sent[0].accounts, r.batches[0].units
    );
    assert_eq!(h.keeper.metrics.batches_v1, 2);
}

#[test]
fn a_cluster_without_v1_gets_v0_batches_of_four() {
    let (mut h, mms) = setup(7, KeeperConfig::default());
    seven_markets(&h, &mms);
    h.chain().set_v1(false);
    let r = h.second().clone();
    assert_eq!(r.cranked.len(), 7, "{r:?}");
    assert!(
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::V1Unsupported { .. })),
        "{:?}",
        r.alerts
    );
    let sizes: Vec<(usize, TxFormat)> = r
        .batches
        .iter()
        .map(|b| (b.markets.len(), b.format))
        .collect();
    assert_eq!(sizes, vec![(4, TxFormat::V0), (3, TxFormat::V0)]);
    for s in h.chain().sent() {
        assert_eq!(s.format, Some(TxFormat::V0));
        assert!(s.bytes <= V0_MAX_TX_BYTES, "{s:?}");
    }
    println!(
        "v0 batch of 4 mock cranks (no ALT): {} B, {} accounts",
        h.chain().sent()[0].bytes,
        h.chain().sent()[0].accounts
    );
    assert!(matches!(
        h.keeper.v1_support(),
        shortwin_keeper::V1Support::No { .. }
    ));
    // Later ticks stay on v0 without re-probing (until v1_reprobe_s).
    h.seconds(31);
    assert!(
        h.chain()
            .sent()
            .iter()
            .all(|s| s.format == Some(TxFormat::V0))
    );
    assert_eq!(h.keeper.metrics.v1_rejections, 1);
}

#[test]
fn a_failing_crank_is_dropped_from_its_batch_and_sent_alone() {
    let (mut h, mms) = setup(3, KeeperConfig::default());
    seven_markets(&h, &mms);
    // Corrupt the middle market's layout version: its crank fails (`ConfigVersion`).
    let bad = mms[1].market();
    h.with_env(|env| {
        let mut acc = env.svm.get_account(&bad).unwrap();
        acc.data[8] = 99;
        env.svm.set_account(bad, acc).unwrap();
    });
    let r = h.second().clone();
    let mut cranked = r.cranked.clone();
    cranked.sort();
    let mut want = vec![mms[0].market(), mms[2].market()];
    want.sort();
    assert_eq!(cranked, want, "{r:?}");
    assert_eq!(r.batches.len(), 1);
    assert_eq!(r.batches[0].markets.len(), 2);
    assert!(
        r.crank_failed.iter().any(|(k, _)| *k == bad),
        "{:?}",
        r.crank_failed
    );
}

#[test]
fn v0_batches_use_the_protocol_lookup_table() {
    // A28: the static accounts live in the protocol ALT; a v0 batch then carries them as
    // one-byte indexes.
    use anchor_lang::prelude::Pubkey;
    use shortwin_keeper::pda;
    let alt = Pubkey::new_unique();
    let cfg = KeeperConfig {
        tx_format: shortwin_keeper::config::TxFormatPolicy::V0,
        lookup_tables: vec![alt.to_string()],
        ..KeeperConfig::default()
    };
    let (mut h, mms) = setup(4, cfg);
    h.with_env(|env| {
        shortwin_keeper_litesvm::install_lookup_table(
            env,
            alt,
            &[pda::config(), pda::oracle_cache(), pda::event_authority()],
        )
    });
    seven_markets(&h, &mms);
    let r = h.second().clone();
    assert_eq!(r.cranked.len(), 4, "{r:?}");
    assert_eq!(r.batches.len(), 1);
    let with_alt = h.chain().sent()[0].clone();
    assert_eq!(with_alt.format, Some(TxFormat::V0));
    assert_eq!(with_alt.instructions, 4);
    // The same batch without the table is 1,024 B (`a_cluster_without_v1_gets_v0_batches_of_four`):
    // three 32-byte keys become three index bytes, plus the table's key and two list lengths.
    println!(
        "v0 batch of 4 mock cranks with the protocol ALT: {} B",
        with_alt.bytes
    );
    assert_eq!(
        with_alt.bytes,
        1_024 - 3 * 32 + (32 + 2 + 3),
        "{with_alt:?}"
    );
}
