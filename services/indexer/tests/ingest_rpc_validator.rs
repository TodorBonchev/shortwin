//! The RPC backfill, the live `logsSubscribe` source and the finality tracker against a real
//! node (INDEXER.md §2.2, §2.3, §3.2): a local `solana-test-validator` (offline, no program
//! needed). The tracked "program" address is a fresh key the faucet airdrops to — every airdrop
//! is a real transaction that mentions it, so the RPC paths (`getSignaturesForAddress` paging and
//! cursors, `getTransaction` at version 1 through `to_raw_tx`, block positions, PubSub
//! notifications, `getSignatureStatuses`) run exactly as in production.
//!
//! Opt-in (it starts a validator, ~10 s each): `cargo test -p shortwin-indexer --test
//! ingest_rpc_validator -- --ignored` with `solana-test-validator` on `PATH`. The M8 security
//! review's two validator reproducers are here turned around (M8-SEC-1: history older than the
//! status cache survives the finality tracker; M8-SEC-5: a rolled-back newest signature does not
//! halt live ingest); they run the validator with 50 ms slots (`--ticks-per-slot 8`).

mod ingest_common;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ingest_common::pg;
use shortwin_indexer::IndexerConfig;
use shortwin_indexer::ingest::live::RpcStatus;
use shortwin_indexer::ingest::raw::{Commitment, RawIx, RawTopIx, RawTx};
use shortwin_indexer::ingest::rpc::{self, BackfillSource, Rpc, Stream};
use shortwin_indexer::ingest::writer::{CursorAdvance, Writer};
use shortwin_indexer::ingest::{self, ProgramIds, finality};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;

struct Validator {
    child: Child,
    ledger: std::path::PathBuf,
    rpc: String,
    ws: String,
}

impl Drop for Validator {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.ledger);
    }
}

async fn start_validator() -> Validator {
    start_validator_with(0, None).await
}

/// A validator on its own port range (`slot`: which of a test binary's validators) and, with
/// `ticks`, shorter slots.
async fn start_validator_with(slot: u16, ticks: Option<u32>) -> Validator {
    // Each validator takes ports base..base + 60: 100 apart per slot, 300 per process.
    let base = 20_000 + u16::try_from(std::process::id() % 130).expect("port") * 300 + slot * 100;
    let ledger = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("validator-{}-{slot}", std::process::id()));
    let mut cmd = Command::new("solana-test-validator");
    cmd.args(["--ledger"])
        .arg(&ledger)
        .args(["--reset", "--quiet"]);
    if let Some(t) = ticks {
        cmd.args(["--ticks-per-slot", &t.to_string()]);
    }
    let child = cmd
        .args(["--rpc-port", &base.to_string()])
        .args(["--faucet-port", &(base + 10).to_string()])
        .args(["--gossip-port", &(base + 11).to_string()])
        .args([
            "--dynamic-port-range",
            &format!("{}-{}", base + 20, base + 60),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("solana-test-validator on PATH");
    let v = Validator {
        child,
        ledger,
        rpc: format!("http://127.0.0.1:{base}"),
        ws: format!("ws://127.0.0.1:{}", base + 1),
    };
    let client = RpcClient::new(v.rpc.clone());
    let t0 = Instant::now();
    while client.get_health().await.is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(90),
            "the validator did not start"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    v
}

async fn airdrop(client: &RpcClient, to: &Pubkey, lamports: u64) {
    let sig = client.request_airdrop(to, lamports).await.expect("airdrop");
    let t0 = Instant::now();
    while !client
        .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
        .await
        .map(|r| r.value)
        .unwrap_or(false)
    {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "airdrop not confirmed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn count(pool: &sqlx::PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .expect("count")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "starts solana-test-validator (opt-in)"]
async fn backfill_live_and_finality_against_a_local_validator() {
    let v = start_validator().await;
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let client = RpcClient::new_with_commitment(v.rpc.clone(), CommitmentConfig::confirmed());
    let tracked = Pubkey::new_unique();
    for i in 0..5 {
        airdrop(&client, &tracked, 1_000_000 + i).await;
    }
    let mut cfg = IndexerConfig::default();
    cfg.ingest.rpc_url = v.rpc.clone();
    cfg.ingest.ws_url = v.ws.clone();
    cfg.ingest.program_id = tracked.to_string();
    cfg.ingest.backfill.page_limit = 2; // force paging
    cfg.ingest.finality.poll_ms = 500;
    // The backfill reads at finalized: wait until the airdrops are rooted.
    let t0 = Instant::now();
    loop {
        let sigs = client
            .get_signatures_for_address_with_config(
                &tracked,
                solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config {
                    commitment: Some(CommitmentConfig::finalized()),
                    ..Default::default()
                },
            )
            .await
            .expect("signatures");
        if sigs.len() == 5 {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "airdrops not finalized"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let report = ingest::run_backfill(&cfg, &pool).await.expect("backfill");
    assert_eq!(report.txs_written, 5);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM ingest_txs WHERE commitment = 'finalized'"
        )
        .await,
        5
    );
    let positions: Vec<(i64, i32)> =
        sqlx::query_as("SELECT slot, tx_index FROM ingest_txs ORDER BY slot")
            .fetch_all(&pool)
            .await
            .expect("positions");
    assert!(positions.iter().all(|(s, _)| *s > 0));
    let cursor: Option<String> =
        sqlx::query_scalar("SELECT newest_signature::text FROM ingest_cursors WHERE stream = $1")
            .bind(format!("program:{tracked}"))
            .fetch_one(&pool)
            .await
            .expect("cursor");
    assert!(
        cursor.is_some(),
        "the stream's cursor advanced with the rows"
    );
    // A second backfill finds nothing new (the cursor bounds the listing).
    assert_eq!(
        ingest::run_backfill(&cfg, &pool)
            .await
            .expect("again")
            .txs_written,
        0
    );

    // Live: subscribe, gap-fill, stream; the finality tracker promotes.
    let live_cfg = cfg.clone();
    let live_pool = pool.clone();
    let live = tokio::spawn(async move { ingest::run_live(&live_cfg, &live_pool).await });
    tokio::time::sleep(Duration::from_secs(3)).await;
    for i in 0..3 {
        airdrop(&client, &tracked, 2_000_000 + i).await;
    }
    let t0 = Instant::now();
    while count(&pool, "SELECT count(*) FROM ingest_txs").await < 8 {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "live notifications not ingested"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let t0 = Instant::now();
    while count(
        &pool,
        "SELECT count(*) FROM ingest_txs WHERE commitment = 'confirmed'",
    )
    .await
        > 0
    {
        assert!(
            t0.elapsed() < Duration::from_secs(90),
            "the finality tracker did not promote"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(count(&pool, "SELECT finalized_slot FROM ingest_state").await > 0);
    live.abort();
    let _ = live.await;
    pool.close().await;
    drop(v);
}

async fn airdrop_sig(client: &RpcClient, to: &Pubkey, lamports: u64) -> Signature {
    let sig = client.request_airdrop(to, lamports).await.expect("airdrop");
    let t0 = Instant::now();
    while !client
        .confirm_transaction_with_commitment(&sig, CommitmentConfig::confirmed())
        .await
        .map(|r| r.value)
        .unwrap_or(false)
    {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "airdrop not confirmed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    sig
}

async fn wait_finalized_past(client: &RpcClient, slot: u64, secs: u64) {
    let t0 = Instant::now();
    while client
        .get_slot_with_commitment(CommitmentConfig::finalized())
        .await
        .expect("slot")
        <= slot
    {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "not finalized past {slot}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// M8-SEC-1: transactions ingested at `confirmed` and checked by the finality tracker only after
/// the node's status cache (~300 slots) no longer holds them are promoted, never rolled back:
/// the tracker asks with `searchTransactionHistory`, and "no status" alone proves nothing. The
/// live gap-fill writes what the node reports finalized at `finalized` (nothing left for the
/// tracker).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "starts solana-test-validator (opt-in)"]
async fn finalized_history_outlives_the_status_cache() {
    let v = start_validator_with(1, Some(8)).await;
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let client = RpcClient::new_with_commitment(v.rpc.clone(), CommitmentConfig::confirmed());
    let tracked = Pubkey::new_unique();
    let rpc = Rpc::new(&v.rpc, 1_000);
    let ids = ProgramIds { shortwin: tracked };
    let streams = vec![Stream {
        name: format!("program:{tracked}"),
        address: tracked,
    }];
    let mut w = Writer::open(&pool, ids, "ws_live").await.expect("writer");
    // Live ingest at confirmed: five airdrops written as the live source writes them.
    let mut last = 0;
    for i in 0..5 {
        let sig = airdrop_sig(&client, &tracked, 1_000_000 + i).await;
        let raw = rpc
            .raw_tx_patient(&sig, Commitment::Confirmed, 6)
            .await
            .expect("getTransaction")
            .expect("served at confirmed");
        last = last.max(raw.slot);
        rpc::apply_item(
            &mut w,
            &streams,
            ingest::source::SourceItem::Tx(Box::new(raw)),
        )
        .await
        .expect("write");
    }
    // The indexer was down for 450 slots: the status cache forgot them.
    wait_finalized_past(&client, last + 450, 120).await;
    let sigs: Vec<Signature> =
        sqlx::query_scalar::<_, String>("SELECT signature::text FROM ingest_txs")
            .fetch_all(&pool)
            .await
            .expect("sigs")
            .iter()
            .map(|s| s.parse().expect("sig"))
            .collect();
    let cache = client
        .get_signature_statuses(&sigs)
        .await
        .expect("statuses")
        .value;
    eprintln!(
        "status cache after 450 slots: {:?}",
        cache.iter().map(Option::is_some).collect::<Vec<_>>()
    );
    let r = finality::run_once(&mut w, &RpcStatus { rpc: &rpc }, 256)
        .await
        .expect("finality");
    assert_eq!(
        (r.promoted, r.rolled_back, r.unresolved),
        (5, 0, 0),
        "{r:?}"
    );
    let rollbacks: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_rollbacks")
        .fetch_one(&pool)
        .await
        .expect("rollbacks");
    assert_eq!(rollbacks, 0);
    // The live gap-fill (listed at confirmed) writes the finalized history at finalized.
    let pool2 = pg.fresh_db().await.expect("db");
    let mut w2 = Writer::open(&pool2, ids, "ws_live").await.expect("writer");
    let mut gap = BackfillSource::new(
        &rpc,
        pool2.clone(),
        streams.clone(),
        Default::default(),
        Commitment::Confirmed,
        false,
    );
    rpc::drive(&mut gap, &mut w2, &streams)
        .await
        .expect("gap-fill");
    let by: Vec<(String, i64)> =
        sqlx::query_as("SELECT commitment, count(*) FROM ingest_txs GROUP BY 1")
            .fetch_all(&pool2)
            .await
            .expect("rows");
    assert_eq!(by, vec![("finalized".to_string(), 5)]);
    w.close().await.expect("close");
    w2.close().await.expect("close");
    pool.close().await;
    pool2.close().await;
    drop(v);
}

/// M8-SEC-5: a stream's newest transaction rolled back (here a confirmed transaction the cluster
/// never had: a fork it abandoned) leaves the cursor on the stream's newest finalized one, so
/// `live` keeps ingesting: the gap-fill lists from a signature the node knows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "starts solana-test-validator (opt-in)"]
async fn a_rolled_back_newest_signature_does_not_halt_live_ingest() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("shortwin_indexer=info")
        .with_test_writer()
        .try_init();
    let v = start_validator_with(2, Some(8)).await;
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let client = RpcClient::new_with_commitment(v.rpc.clone(), CommitmentConfig::confirmed());
    let tracked = Pubkey::new_unique();
    for i in 0..4 {
        airdrop_sig(&client, &tracked, 1_000_000 + i).await;
    }
    let rpc = Rpc::new(&v.rpc, 1_000);
    let ids = ProgramIds { shortwin: tracked };
    let streams = vec![Stream {
        name: format!("program:{tracked}"),
        address: tracked,
    }];
    let mut w = Writer::open(&pool, ids, "ws_live").await.expect("writer");
    let slot = client.get_slot().await.expect("slot");
    wait_finalized_past(&client, slot, 60).await;
    let mut gap = BackfillSource::new(
        &rpc,
        pool.clone(),
        streams.clone(),
        Default::default(),
        Commitment::Confirmed,
        false,
    );
    rpc::drive(&mut gap, &mut w, &streams)
        .await
        .expect("gap-fill");
    // A confirmed transaction of the stream on a fork the cluster abandons, written with its
    // cursor advance like any live transaction.
    let unknown = Signature::from([0x11; 64]);
    let fork_slot = client.get_slot().await.expect("slot") + 5;
    let fork_tx = RawTx {
        signature: unknown,
        slot: fork_slot,
        tx_index: 0,
        block_time: Some(1_790_000_000),
        commitment: Commitment::Confirmed,
        fee_payer: Pubkey::new_unique(),
        fee: 5_000,
        compute_units: None,
        err: None,
        account_keys: vec![Pubkey::new_unique(), tracked],
        instructions: vec![RawTopIx {
            ix: RawIx {
                program_id: solana_pubkey::pubkey!("11111111111111111111111111111111"),
                accounts: vec![tracked],
                data: vec![2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0],
                stack_height: 1,
            },
            inner: vec![],
        }],
        logs: vec![],
    };
    w.write_with(
        &fork_tx,
        &[CursorAdvance {
            stream: streams[0].name.clone(),
            address: tracked,
        }],
    )
    .await
    .expect("fork tx");
    wait_finalized_past(&client, fork_slot, 60).await;
    let r = finality::run_once(&mut w, &RpcStatus { rpc: &rpc }, 256)
        .await
        .expect("finality");
    assert_eq!(r.rolled_back, 1, "{r:?}");
    let cursor: Option<String> =
        sqlx::query_scalar("SELECT newest_signature::text FROM ingest_cursors WHERE stream = $1")
            .bind(&streams[0].name)
            .fetch_one(&pool)
            .await
            .expect("cursor");
    assert_ne!(
        cursor,
        Some(unknown.to_string()),
        "the cursor left the rolled-back signature"
    );
    w.close().await.expect("close");
    let before: i64 = count(&pool, "SELECT count(*) FROM ingest_txs").await;
    let mut cfg = IndexerConfig::default();
    cfg.ingest.rpc_url = v.rpc.clone();
    cfg.ingest.ws_url = v.ws.clone();
    cfg.ingest.program_id = tracked.to_string();
    cfg.ingest.refresher.enabled = false;
    cfg.ingest.finality.poll_ms = 500;
    let live_pool = pool.clone();
    let live = tokio::spawn(async move { ingest::run_live(&cfg, &live_pool).await });
    tokio::time::sleep(Duration::from_secs(3)).await;
    for i in 0..3 {
        airdrop_sig(&client, &tracked, 3_000_000 + i).await;
    }
    let t0 = Instant::now();
    while count(&pool, "SELECT count(*) FROM ingest_txs").await < before + 3 {
        assert!(
            !live.is_finished(),
            "live stopped: {:?}",
            live.await.expect("join")
        );
        assert!(t0.elapsed() < Duration::from_secs(60), "live ingest halted");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    live.abort();
    let _ = live.await;
    pool.close().await;
    drop(v);
}
