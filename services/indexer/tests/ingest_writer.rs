//! The writer on synthetic transactions (INDEXER.md §3.1, §3.6, §7.3): what no recorded session
//! exercises — quarantined payloads next to good rows, venue program upgrades, failed and
//! truncated transactions, a different transaction at an occupied block position, and the
//! refresher's token-metadata and venue-admin rows.

mod ingest_common;

use anchor_lang::Discriminator;
use ingest_common::pg;
use shortwin_indexer::ingest::decode::idl;
use shortwin_indexer::ingest::raw::{Commitment, RawIx, RawTopIx, RawTx};
use shortwin_indexer::ingest::writer::{WriteOutcome, Writer};
use shortwin_indexer::ingest::{ProgramIds, refresher};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use sqlx::PgPool;

fn k(b: u8) -> Pubkey {
    Pubkey::new_from_array([b; 32])
}

fn ix(program: Pubkey, h: u32, data: Vec<u8>, accounts: Vec<Pubkey>) -> RawIx {
    RawIx {
        program_id: program,
        accounts,
        data,
        stack_height: h,
    }
}

fn tx(
    sig: u8,
    slot: u64,
    tx_index: u32,
    commitment: Commitment,
    instructions: Vec<RawTopIx>,
) -> RawTx {
    RawTx {
        signature: Signature::from([sig; 64]),
        slot,
        tx_index,
        block_time: Some(1_790_000_000 + i64::try_from(slot).expect("slot")),
        commitment,
        fee_payer: k(50),
        fee: 5_000,
        compute_units: Some(10),
        err: None,
        account_keys: vec![k(50)],
        instructions,
        logs: vec![],
    }
}

fn crank_price() -> RawIx {
    let data = idl::idl()
        .instruction("crank_price")
        .expect("crank_price")
        .discriminator
        .clone();
    ix(
        shortwin::ID,
        1,
        data,
        vec![k(9), k(1), k(10), k(11), k(200), shortwin::ID],
    )
}

fn event_cpi(payload: &[u8]) -> RawIx {
    let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
    data.extend_from_slice(payload);
    ix(shortwin::ID, 2, data, vec![k(200)])
}

fn mint_safety_tripped() -> Vec<u8> {
    let ev = shortwin::events::MintSafetyTripped {
        market: k(1),
        slot: 3,
        ts: 4,
        mint: k(4),
    };
    let mut p = <shortwin::events::MintSafetyTripped as Discriminator>::DISCRIMINATOR.to_vec();
    anchor_lang::AnchorSerialize::serialize(&ev, &mut p).expect("borsh");
    p
}

async fn one(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quarantine_upgrades_failures_and_truncation() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "rpc_backfill")
        .await
        .expect("writer");

    // A good event, an undecodable one (quarantined, never dropped) and a BPF loader `Upgrade` of
    // the CPMM program in one successful transaction.
    let mut upgrade = 3u32.to_le_bytes().to_vec();
    upgrade.extend([0u8; 4]);
    let loader = solana_pubkey::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
    let t = tx(
        1,
        10,
        0,
        Commitment::Finalized,
        vec![
            RawTopIx {
                ix: crank_price(),
                inner: vec![event_cpi(&mint_safety_tripped()), event_cpi(&[9; 12])],
            },
            RawTopIx {
                ix: ix(
                    loader,
                    1,
                    upgrade,
                    vec![k(77), shortwin::constants::RAYDIUM_CPMM_PROGRAM_ID],
                ),
                inner: vec![],
            },
        ],
    );
    assert_eq!(
        w.write(&t).await.expect("write"),
        WriteOutcome::Written { events: 1 }
    );
    assert_eq!(w.write(&t).await.expect("again"), WriteOutcome::Duplicate);
    assert_eq!(one(&pool, "SELECT count(*) FROM events").await, 1);
    assert_eq!(
        one(
            &pool,
            "SELECT count(*) FROM ingest_quarantine WHERE ev_source = 0"
        )
        .await,
        1
    );
    assert_eq!(
        one(
            &pool,
            "SELECT count(*) FROM invocations WHERE name = 'crank_price'"
        )
        .await,
        1
    );
    assert_eq!(
        one(&pool, "SELECT shortwin_ixs::bigint FROM ingest_txs").await,
        1
    );
    let upgrades: Vec<(i16, String)> =
        sqlx::query_as("SELECT venue::smallint, programdata::text FROM venue_program_upgrades")
            .fetch_all(&pool)
            .await
            .expect("upgrades");
    // GUARD_RAYDIUM_CPMM = 4.
    assert_eq!(upgrades, vec![(4, k(77).to_string())]);

    // A failed transaction: its invocations (failed), its error, no event.
    let mut f = tx(
        2,
        11,
        0,
        Commitment::Finalized,
        vec![RawTopIx {
            ix: crank_price(),
            inner: vec![event_cpi(&mint_safety_tripped())],
        }],
    );
    f.err = Some(serde_json::json!({"InstructionError": [0, {"Custom": 6042}]}));
    w.write(&f).await.expect("failed tx");
    assert_eq!(one(&pool, "SELECT count(*) FROM events").await, 1);
    assert_eq!(
        one(&pool, "SELECT err_code FROM ingest_txs WHERE slot = 11").await,
        6042
    );
    assert_eq!(
        one(
            &pool,
            "SELECT err_ix_index::bigint FROM ingest_txs WHERE slot = 11"
        )
        .await,
        0
    );
    assert_eq!(
        one(&pool, "SELECT count(*) FROM invocations WHERE failed").await,
        1
    );
    assert_eq!(w.report.txs_failed, 1);

    // A truncated log stream is flagged.
    let mut trunc = tx(
        3,
        12,
        0,
        Commitment::Finalized,
        vec![RawTopIx {
            ix: crank_price(),
            inner: vec![],
        }],
    );
    trunc.logs = vec![
        format!("Program {} invoke [1]", shortwin::ID),
        "Log truncated".into(),
    ];
    w.write(&trunc).await.expect("truncated");
    assert_eq!(
        one(
            &pool,
            "SELECT count(*) FROM ingest_txs WHERE logs_truncated"
        )
        .await,
        1
    );
    // The chain clock and the finalized watermark follow finalized writes.
    assert_eq!(
        one(&pool, "SELECT finalized_slot FROM ingest_state").await,
        12
    );
    assert_eq!(
        one(&pool, "SELECT latest_block_time FROM ingest_state").await,
        1_790_000_012
    );
    w.close().await.expect("close");
    pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_transaction_at_an_occupied_position_replaces_a_confirmed_one_only() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "ws_live")
        .await
        .expect("writer");
    let a = tx(
        1,
        20,
        0,
        Commitment::Confirmed,
        vec![RawTopIx {
            ix: crank_price(),
            inner: vec![],
        }],
    );
    let b = tx(
        2,
        20,
        0,
        Commitment::Confirmed,
        vec![RawTopIx {
            ix: crank_price(),
            inner: vec![],
        }],
    );
    w.write(&a).await.expect("a");
    // A fork switch the tracker has not seen yet: the newer observation replaces it.
    w.write(&b).await.expect("b replaces a");
    let sigs: Vec<String> = sqlx::query_scalar("SELECT signature::text FROM ingest_txs")
        .fetch_all(&pool)
        .await
        .expect("sigs");
    assert_eq!(sigs, vec![b.signature.to_string()]);
    assert_eq!(one(&pool, "SELECT count(*) FROM ingest_rollbacks").await, 1);
    // A finalized transaction is never replaced.
    let c = tx(
        3,
        21,
        0,
        Commitment::Finalized,
        vec![RawTopIx {
            ix: crank_price(),
            inner: vec![],
        }],
    );
    let d = tx(
        4,
        21,
        0,
        Commitment::Finalized,
        vec![RawTopIx {
            ix: crank_price(),
            inner: vec![],
        }],
    );
    w.write(&c).await.expect("c");
    assert!(
        w.write(&d).await.is_err(),
        "two finalized transactions at one position"
    );
    w.close().await.expect("close");
    pool.close().await;
}

/// One venue admin watch observation of `k(6)`'s `field`: whether it recorded a row.
async fn watch(
    pool: &PgPool,
    known: &mut refresher::AdminValues,
    field: &'static str,
    slot: u64,
    value: &str,
) -> bool {
    refresher::observe_admin_value(pool, known, &k(6), field, slot, "raydium_v4", value.into())
        .await
        .expect("observe")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresher_rows_token_metadata_and_venue_admin_changes() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mint = k(5);
    let long = "x".repeat(300);
    refresher::insert_token_metadata(
        &pool,
        &mint,
        6,
        Some(("Name".into(), "SYMBOLSYMBOLSYMBOL1".into(), long)),
        9,
    )
    .await
    .expect("metadata");
    // The first observation wins.
    refresher::insert_token_metadata(&pool, &mint, 9, None, 10)
        .await
        .expect("again");
    let (dec, sym, uri_len, src): (i16, String, i32, String) = sqlx::query_as(
        "SELECT decimals::smallint, symbol, length(uri), source FROM token_metadata WHERE mint = $1",
    )
    .bind(mint.to_string())
    .fetch_one(&pool)
    .await
    .expect("row");
    assert_eq!(
        (dec, sym.as_str(), uri_len, src.as_str()),
        (6, "SYMBOLSYMBOLSYMB", 200, "metaplex")
    );
    refresher::record_admin_value(&pool, &k(6), "status", 100, "raydium_v4", None, "6")
        .await
        .expect("first");
    refresher::record_admin_value(&pool, &k(6), "status", 200, "raydium_v4", Some("6"), "4")
        .await
        .expect("change");
    assert_eq!(
        one(&pool, "SELECT count(*) FROM venue_admin_states").await,
        2
    );

    // The watch after a restart (an empty cache) reads the last value back from the rows: an
    // unchanged value records nothing, a changed one records its stored old value.
    let mut known = refresher::AdminValues::new();
    assert!(!watch(&pool, &mut known, "status", 300, "4").await);
    assert!(!watch(&pool, &mut known, "status", 310, "4").await);
    assert!(watch(&pool, &mut known, "status", 320, "1").await);
    // A field never stored before: its first value, with no old value.
    assert!(watch(&pool, &mut known, "trade_fee_rate", 320, "2500").await);
    assert!(!watch(&pool, &mut known, "trade_fee_rate", 330, "2500").await);
    // A second restart sees the latest row, not the first one.
    let mut known = refresher::AdminValues::new();
    assert!(!watch(&pool, &mut known, "status", 400, "1").await);
    assert!(watch(&pool, &mut known, "status", 410, "6").await);
    let rows: Vec<(String, i64, Option<String>, String)> = sqlx::query_as(
        "SELECT field, observed_slot, old_value, new_value FROM venue_admin_states \
         WHERE observed_slot > 200 ORDER BY observed_slot, field",
    )
    .fetch_all(&pool)
    .await
    .expect("rows");
    let s = |v: &str| v.to_string();
    assert_eq!(
        rows,
        [
            (s("status"), 320, Some(s("4")), s("1")),
            (s("trade_fee_rate"), 320, None, s("2500")),
            (s("status"), 410, Some(s("1")), s("6")),
        ]
    );
    pool.close().await;
}
