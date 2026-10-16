//! The analytics of INDEXER.md §8 on the embedded Postgres (ANALYTICS track): every SQL helper
//! against its Rust twin in `analytics::formulas`, the views on a seeded history (exit rates and
//! slippage, keeper activity, coverage pauses, the coin creator's share across epochs, global
//! utilisation), the hourly rollups (state carried forward, flows, open and paused seconds,
//! observation gaps, idempotence, the late-data reset, the global hours) and the `invocations`
//! retention prune.

use shortwin_indexer::analytics::{self, formulas, rollups};
use shortwin_indexer::numeric::WAD;
use shortwin_indexer::testing::pg::TestPg;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use sqlx::PgPool;

fn key(b: u8) -> String {
    Pubkey::new_from_array([b; 32]).to_string()
}

fn sig(n: u32) -> String {
    let mut b = [7u8; 64];
    b[..4].copy_from_slice(&n.to_le_bytes());
    Signature::from(b).to_string()
}

async fn start() -> (TestPg, PgPool) {
    let pg = TestPg::start_in(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")))
        .await
        .expect("the embedded PostgreSQL starts");
    let pool = pg.fresh_db().await.expect("a migrated database");
    (pg, pool)
}

// ---- SQL twins ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_helpers_equal_their_rust_twins() {
    let (pg, pool) = start().await;
    let amounts: [u64; 7] = [
        0,
        1,
        999,
        3_000_000_000_000,
        1 << 53,
        u64::MAX - 1,
        u64::MAX,
    ];
    let prices: [u128; 6] = [0, 1, WAD / 3, WAD, 7 * WAD + 13, 1_000_000 * WAD];
    for &q in &amounts {
        for &p in &prices {
            for &s in &[0u64, 1, 5_000, u64::MAX / 2] {
                let (impact, oi): (String, String) = sqlx::query_as(
                    "SELECT sw_open_impact_bps($1::numeric, $2::numeric, $3::numeric)::text, \
                            sw_mul_wad($1::numeric, $2::numeric)::text",
                )
                .bind(q.to_string())
                .bind(p.to_string())
                .bind(s.to_string())
                .fetch_one(&pool)
                .await
                .expect("helpers");
                assert_eq!(
                    impact,
                    formulas::open_impact_bps(q, p, s).to_string(),
                    "{q} {p} {s}"
                );
                assert_eq!(oi, formulas::oi_lamports(q, p).to_string());
            }
        }
        for &b in &amounts {
            let (util, slip): (i64, Option<String>) = sqlx::query_as(
                "SELECT sw_util_bps($1::numeric, $2::numeric), \
                        sw_exit_slippage_bps($1::numeric, $2::numeric)::text",
            )
            .bind(q.to_string())
            .bind(b.to_string())
            .fetch_one(&pool)
            .await
            .expect("helpers");
            assert_eq!(util, i64::from(shortwin_indexer::numeric::util_bps(q, b)));
            assert_eq!(
                slip,
                formulas::exit_slippage_bps(q, b).map(|v| v.to_string()),
                "{q} {b}"
            );
        }
    }

    // Shares across epoch records: a rebase (1/1,000), another (1/10⁶), then a full wipe.
    let market = key(2);
    seed_market(
        &pool,
        &market,
        &key(20),
        1_790_000_000,
        State::default(),
        None,
        3,
    )
    .await;
    seed_tx(&pool, 1, 100, 0, 1_790_000_000, &key(9)).await;
    for (seq, (epoch, num, den)) in [(0u32, 1u8, 1_000u128), (1, 1, 1_000_000), (2, 0, 1)]
        .into_iter()
        .enumerate()
    {
        let seq = u32::try_from(seq).expect("seq");
        seed_event(
            &pool,
            1,
            seq,
            100,
            0,
            "EpochRolled",
            &market,
            1_790_000_000,
            "{}",
        )
        .await;
        sqlx::query(
            "INSERT INTO share_epochs (market, epoch, acc_end, num, den, vest_flushed, full_wipe, \
             signature, ix_index, ev_source, ev_seq, slot, ts) \
             VALUES ($1, $2, 0, $3, $4::numeric, 0, $5, $6, 0, 0, $7, 100, 1790000000)",
        )
        .bind(&market)
        .bind(i64::from(epoch))
        .bind(i16::from(num))
        .bind(den.to_string())
        .bind(num == 0)
        .bind(sig(1))
        .bind(i32::try_from(seq).expect("seq"))
        .execute(&pool)
        .await
        .expect("share_epochs");
    }
    let records = [(1u8, 1_000u128), (1, 1_000_000), (0, 1)];
    for (from, to) in [(0i64, 0i64), (0, 1), (0, 2), (1, 2), (0, 3), (2, 3)] {
        let shares: u128 = 123_456_789_012_345_678_901;
        let got: String =
            sqlx::query_scalar("SELECT sw_shares_at_epoch($1, $2::numeric, $3, $4)::text")
                .bind(&market)
                .bind(shares.to_string())
                .bind(from)
                .bind(to)
                .fetch_one(&pool)
                .await
                .expect("sw_shares_at_epoch");
        let want = formulas::shares_at_epoch(
            shares,
            records[usize::try_from(from).expect("from")..usize::try_from(to).expect("to")]
                .iter()
                .copied(),
        );
        assert_eq!(got, want.to_string(), "{from}..{to}");
    }
    pool.close().await;
    pg.stop().await.expect("stop");
}

// ---- seeding -----------------------------------------------------------------------------------

async fn seed_tx(pool: &PgPool, n: u32, slot: i64, tx_index: i32, block_time: i64, payer: &str) {
    sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
         fee_payer, fee) VALUES ($1, $2, $3, $4, 'finalized', 'replay', $5, 5000::numeric) \
         ON CONFLICT DO NOTHING",
    )
    .bind(sig(n))
    .bind(slot)
    .bind(tx_index)
    .bind(block_time)
    .bind(payer)
    .execute(pool)
    .await
    .expect("ingest_txs");
}

#[allow(clippy::too_many_arguments)]
async fn seed_event(
    pool: &PgPool,
    n: u32,
    seq: u32,
    slot: i64,
    tx_index: i32,
    kind: &str,
    market: &str,
    ts: i64,
    data: &str,
) {
    sqlx::query(
        "INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, block_time, \
         program, invocation, kind, market, ev_slot, ev_ts, data) \
         VALUES ($1, 0, 0, $2, $3, $4, $5, $6, 'test', $7, $8, $3, $5, $9::jsonb)",
    )
    .bind(sig(n))
    .bind(i32::try_from(seq).expect("seq"))
    .bind(slot)
    .bind(tx_index)
    .bind(ts)
    .bind(shortwin::ID.to_string())
    .bind(kind)
    .bind(market)
    .bind(data)
    .execute(pool)
    .await
    .expect("events");
}

/// The block position `History::tx` gives transaction `n`.
fn tx_index(n: u32) -> i32 {
    i32::try_from(n).expect("n")
}

/// A market's after-state (`market_state_log`), with its event and transaction.
#[derive(Clone, Copy, Default)]
struct State {
    tokens_free: u64,
    borrowed_q: u64,
    shares: u128,
    open: u32,
    notional: u64,
    vault: u64,
    lender_sol: u64,
    protocol_fees: u64,
}

struct History<'a> {
    pool: &'a PgPool,
    market: String,
    n: u32,
}

impl History<'_> {
    /// A new transaction at `ts` (slot = ts − T0 so chain order follows time; the block
    /// position is the transaction's number, so two transactions of one second never clash).
    async fn tx(&mut self, ts: i64) -> (u32, i64) {
        self.n += 1;
        let slot = ts - 1_789_000_000;
        seed_tx(
            self.pool,
            self.n,
            slot,
            i32::try_from(self.n).expect("n"),
            ts,
            &key(9),
        )
        .await;
        (self.n, slot)
    }

    async fn log(&mut self, ts: i64, kind: &str, s: State) {
        let (n, slot) = self.tx(ts).await;
        seed_event(
            self.pool,
            n,
            0,
            slot,
            tx_index(n),
            kind,
            &self.market,
            ts,
            "{}",
        )
        .await;
        sqlx::query(
            "INSERT INTO market_state_log (signature, ix_index, ev_source, ev_seq, market, slot, \
             tx_index, ts, kind, tokens_free, borrowed_q, total_shares, open_positions, \
             total_notional_open, open_vault_lamports, lender_sol, protocol_fees) \
             VALUES ($1, 0, 0, 0, $2, $3, $14, $4, $5, $6::numeric, $7::numeric, $8::numeric, $9, \
                     $10::numeric, $11::numeric, $12::numeric, $13::numeric)",
        )
        .bind(sig(n))
        .bind(&self.market)
        .bind(slot)
        .bind(ts)
        .bind(kind)
        .bind(s.tokens_free.to_string())
        .bind(s.borrowed_q.to_string())
        .bind(s.shares.to_string())
        .bind(i64::from(s.open))
        .bind(s.notional.to_string())
        .bind(s.vault.to_string())
        .bind(s.lender_sol.to_string())
        .bind(s.protocol_fees.to_string())
        .bind(tx_index(n))
        .execute(self.pool)
        .await
        .expect("market_state_log");
    }

    async fn crank(&mut self, ts: i64, ema: u128) {
        let (n, slot) = self.tx(ts).await;
        seed_event(
            self.pool,
            n,
            0,
            slot,
            tx_index(n),
            "PriceCranked",
            &self.market,
            ts,
            "{}",
        )
        .await;
        sqlx::query(
            "INSERT INTO market_snapshots (signature, ix_index, ev_source, ev_seq, market, slot, \
             tx_index, ts, spot_wad, ema_price, ema_liq, liq_slow, ema_count, lb_lo, lb_hi, tier, \
             util_bps, rent_basis0, rent_basis1, rent_basis2, vest_bal, cranker) \
             VALUES ($1, 0, 0, 0, $2, $3, $7, $4, $5::numeric, $5::numeric, 0, 0, 1, 0, 0, 3, 0, \
                     0, 0, 0, 0, $6)",
        )
        .bind(sig(n))
        .bind(&self.market)
        .bind(slot)
        .bind(ts)
        .bind(ema.to_string())
        .bind(key(9))
        .bind(tx_index(n))
        .execute(self.pool)
        .await
        .expect("market_snapshots");
        self.observe(n, slot, ts, "crank_price", false).await;
    }

    async fn observe(&mut self, n: u32, slot: i64, ts: i64, name: &str, failed: bool) {
        sqlx::query(
            "INSERT INTO invocations (signature, ix_index, inner_index, stack_height, slot, \
             tx_index, block_time, program, name, market, failed) \
             VALUES ($1, 0, -1, 1, $2, $8, $3, $4, $5, $6, $7)",
        )
        .bind(sig(n))
        .bind(slot)
        .bind(ts)
        .bind(shortwin::ID.to_string())
        .bind(name)
        .bind(&self.market)
        .bind(failed)
        .bind(tx_index(n))
        .execute(self.pool)
        .await
        .expect("invocations");
    }

    async fn fee(
        &mut self,
        ts: i64,
        fee_type: &str,
        recipient: &str,
        keeper: Option<&str>,
        amount: u64,
    ) {
        let (n, slot) = self.tx(ts).await;
        seed_event(
            self.pool,
            n,
            0,
            slot,
            tx_index(n),
            "Fee",
            &self.market,
            ts,
            "{}",
        )
        .await;
        sqlx::query(
            "INSERT INTO fees (signature, ix_index, ev_source, ev_seq, fee_type, recipient, \
             recipient_key, market, amount, slot, tx_index, ts) \
             VALUES ($1, 0, 0, 0, $2, $3, $4, $5, $6::numeric, $7, $9, $8)",
        )
        .bind(sig(n))
        .bind(fee_type)
        .bind(recipient)
        .bind(keeper)
        .bind(&self.market)
        .bind(amount.to_string())
        .bind(slot)
        .bind(ts)
        .bind(tx_index(n))
        .execute(self.pool)
        .await
        .expect("fees");
    }

    async fn coverage(&mut self, ts: i64, kind: &str, gap_s: Option<i64>, paused: Option<bool>) {
        let (n, slot) = self.tx(ts).await;
        seed_event(
            self.pool,
            n,
            0,
            slot,
            tx_index(n),
            "Coverage",
            &self.market,
            ts,
            "{}",
        )
        .await;
        sqlx::query(
            "INSERT INTO coverage_events (signature, ix_index, ev_source, ev_seq, market, kind, \
             gap_s, paused, by_key, slot, tx_index, ts) \
             VALUES ($1, 0, 0, 0, $2, $3, $4, $5, $6, $7, $9, $8)",
        )
        .bind(sig(n))
        .bind(&self.market)
        .bind(kind)
        .bind(gap_s)
        .bind(paused)
        .bind(paused.map(|_| key(8)))
        .bind(slot)
        .bind(ts)
        .bind(tx_index(n))
        .execute(self.pool)
        .await
        .expect("coverage_events");
    }
}

#[allow(clippy::too_many_arguments)]
async fn seed_market(
    pool: &PgPool,
    market: &str,
    mint: &str,
    created_ts: i64,
    last: State,
    ema: Option<u128>,
    shares_epoch: u32,
) {
    sqlx::query(
        "INSERT INTO markets (market, mint, token_program, creator, created_slot, created_ts, \
         created_signature, creation_fee, venue_at_listing, tier_at_listing, venue, tier, \
         tokens_free, borrowed_q, total_shares, share_epoch, open_positions, total_notional_open, \
         open_vault_lamports, lender_sol, protocol_fees, ema_price, \
         last_slot, last_tx_index, last_ix_index, last_ev_source, last_ev_seq) \
         VALUES ($1, $2, $3, $4, 1, $5, $6, 0, 6, 3, 6, 3, $7::numeric, $8::numeric, $9::numeric, \
                 $10, $11, $12::numeric, $13::numeric, $14::numeric, $15::numeric, $16::numeric, \
                 1, 0, 0, 0, 0)",
    )
    .bind(market)
    .bind(mint)
    .bind(shortwin::constants::TOKEN_PROGRAM_ID.to_string())
    .bind(key(4))
    .bind(created_ts)
    .bind(sig(900_000))
    .bind(last.tokens_free.to_string())
    .bind(last.borrowed_q.to_string())
    .bind(last.shares.to_string())
    .bind(i64::from(shares_epoch))
    .bind(i64::from(last.open))
    .bind(last.notional.to_string())
    .bind(last.vault.to_string())
    .bind(last.lender_sol.to_string())
    .bind(last.protocol_fees.to_string())
    .bind(ema.map(|e| e.to_string()))
    .execute(pool)
    .await
    .expect("markets");
}

#[allow(clippy::too_many_arguments)]
async fn seed_position(
    pool: &PgPool,
    position: &str,
    market: &str,
    borrower: &str,
    q: u64,
    s: u64,
    spot: u128,
    ema: u128,
    opened_ts: i64,
    status: &str,
    exit: Option<(i64, Option<&str>, u64, u64)>,
) {
    let open_sig = sig(800_000);
    sqlx::query(
        "INSERT INTO positions (position, market, borrower, nonce, tier, venue, q, s, m, v_open, v, \
         n_open, fee_open, venue_rent, entry_price, spot_wad_open, ema_wad_open, b_spot_open, \
         b_ema_open, deadline, opened_slot, opened_tx_index, opened_ts, open_signature, status, \
         exit_ts, exit_keeper, buyback_cost, b_live_exit, liquidated_expired, settle_condition, \
         duration_s) \
         VALUES ($1, $2, $3, 0, 3, 6, $4::numeric, $5::numeric, 100, 200, 200, $5::numeric, 10, 0, \
                 0, $6::numeric, $7::numeric, 0, 0, $8 + 259200, $8 - 1789000000, 1, $8, $9, $10, \
                 $11, $12, $13::numeric, $14::numeric, $15, $16, $11 - $8)",
    )
    .bind(position)
    .bind(market)
    .bind(borrower)
    .bind(q.to_string())
    .bind(s.to_string())
    .bind(spot.to_string())
    .bind(ema.to_string())
    .bind(opened_ts)
    .bind(open_sig)
    .bind(status)
    .bind(exit.map(|e| e.0))
    .bind(exit.and_then(|e| e.1.map(str::to_string)))
    .bind(exit.map(|e| e.2.to_string()))
    .bind(exit.map(|e| e.3.to_string()))
    .bind((status == "liquidated").then_some(false))
    .bind((status == "settled").then_some(1i16))
    .execute(pool)
    .await
    .expect("positions");
}

// ---- views ---------------------------------------------------------------------------------------

/// exits, closes, CR liquidations, expiries, S1, close rate, settlement rate, average exit
/// slippage, average open impact.
type ExitsRow = (
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn views_on_a_seeded_history() {
    let (pg, pool) = start().await;
    let t0 = 1_790_000_000i64;
    let (m1, m2) = (key(2), key(3));
    let last1 = State {
        tokens_free: 1_000_000,
        borrowed_q: 3_000,
        shares: 2_000_000,
        open: 1,
        notional: 50_000,
        vault: 70_000,
        lender_sol: 12_345,
        protocol_fees: 99,
    };
    seed_market(&pool, &m1, &key(20), t0, last1, Some(2 * WAD), 2).await;
    seed_market(&pool, &m2, &key(21), t0, State::default(), None, 0).await;
    // Positions of m1: open, closed (+3 % slippage), liquidated by keeper K with no keeper fee
    // leg, expired by K (−1 %), settled by another keeper.
    let (k, k2) = (key(30), key(31));
    seed_position(
        &pool,
        &key(40),
        &m1,
        &key(50),
        3_000,
        5_500,
        2 * WAD,
        2 * WAD,
        t0,
        "open",
        None,
    )
    .await;
    seed_position(
        &pool,
        &key(41),
        &m1,
        &key(50),
        1_000,
        1_900,
        2 * WAD,
        2 * WAD,
        t0,
        "closed",
        Some((t0 + 100, None, 1_030, 1_000)),
    )
    .await;
    seed_position(
        &pool,
        &key(42),
        &m1,
        &key(51),
        1_000,
        2_000,
        2 * WAD,
        2 * WAD,
        t0,
        "liquidated",
        Some((t0 + 200, Some(&k), 2_500, 2_500)),
    )
    .await;
    seed_position(
        &pool,
        &key(43),
        &m1,
        &key(51),
        1_000,
        2_000,
        2 * WAD,
        2 * WAD,
        t0,
        "expired",
        Some((t0 + 300, Some(&k), 990, 1_000)),
    )
    .await;
    seed_position(
        &pool,
        &key(44),
        &m1,
        &key(52),
        1_000,
        2_000,
        2 * WAD,
        2 * WAD,
        t0,
        "settled",
        Some((t0 + 400, Some(&k2), 0, 0)),
    )
    .await;
    let mut h = History {
        pool: &pool,
        market: m1.clone(),
        n: 0,
    };
    h.fee(t0 + 300, "expiry_keeper_reward", "keeper", Some(&k), 77)
        .await;
    h.fee(t0 + 400, "settle_keeper_reward", "keeper", Some(&k2), 55)
        .await;
    // Coverage: set, set again (not a transition), clear, set (ongoing).
    h.coverage(t0 + 1_000, "pause_set", None, Some(true)).await;
    h.coverage(t0 + 1_500, "pause_set", None, Some(true)).await;
    h.coverage(t0 + 2_000, "pause_set", None, Some(false)).await;
    h.coverage(t0 + 2_500, "pause_set", None, Some(true)).await;
    sqlx::query("UPDATE ingest_state SET latest_block_time = $1")
        .bind(t0 + 3_000)
        .execute(&pool)
        .await
        .expect("chain clock");

    // Exits: counts, rates (bps of the 4 exits), slippage, keepers.
    let row: ExitsRow = sqlx::query_as(
        "SELECT exits, closes, liquidations_cr, expiries, settlements_s1, close_rate_bps, \
                    settlement_rate_bps, avg_exit_slippage_bps::text, avg_open_impact_bps::text \
             FROM v_market_exits WHERE market = $1",
    )
    .bind(&m1)
    .fetch_one(&pool)
    .await
    .expect("v_market_exits");
    assert_eq!((row.0, row.1, row.2, row.3, row.4), (4, 1, 1, 1, 1));
    assert_eq!(
        row.5,
        Some(i64::try_from(formulas::share_bps(1, 4).expect("rate")).expect("bps"))
    );
    assert_eq!(row.6, Some(2_500));
    // Exit slippage over closes, liquidations and expiries: (300 + 0 − 100) / 3.
    let slips = [
        formulas::exit_slippage_bps(1_030, 1_000).expect("q"),
        formulas::exit_slippage_bps(2_500, 2_500).expect("q"),
        formulas::exit_slippage_bps(990, 1_000).expect("q"),
    ];
    let avg: f64 = slips.iter().map(|&v| v as f64).sum::<f64>() / 3.0;
    let got: f64 = row.7.expect("slippage").parse().expect("number");
    assert!((got - avg).abs() < 1e-9, "{got} vs {avg}");
    // Open impact: every position sold 1,000 or 3,000 tokens at spot 2 lamports for less.
    let impacts = [
        formulas::open_impact_bps(3_000, 2 * WAD, 5_500),
        formulas::open_impact_bps(1_000, 2 * WAD, 1_900),
        formulas::open_impact_bps(1_000, 2 * WAD, 2_000),
        formulas::open_impact_bps(1_000, 2 * WAD, 2_000),
        formulas::open_impact_bps(1_000, 2 * WAD, 2_000),
    ];
    let avg: f64 = impacts.iter().map(|&v| v as f64).sum::<f64>() / 5.0;
    let got: f64 = row.8.expect("impact").parse().expect("number");
    assert!((got - avg).abs() < 1e-9, "{got} vs {avg}");

    // Keeper activity: K liquidated (no keeper fee leg written) and expired; K2 settled.
    let keepers: Vec<(String, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT keeper, liquidations, expiries, settlements, rewards_lamports::text \
         FROM v_keeper_activity ORDER BY keeper",
    )
    .fetch_all(&pool)
    .await
    .expect("v_keeper_activity");
    let mut want = vec![
        (k.clone(), 1, 1, 0, "77".to_string()),
        (k2.clone(), 0, 0, 1, "55".to_string()),
    ];
    want.sort();
    assert_eq!(keepers, want);

    // Coverage pauses: [t0+1000, t0+2000) and the ongoing one from t0+2500 to the chain clock.
    let pauses: Vec<(i64, i64, i64, bool)> = sqlx::query_as(
        "SELECT paused_at, resumed_at, paused_s, ongoing FROM v_coverage_pauses \
         WHERE market = $1 ORDER BY paused_at",
    )
    .bind(&m1)
    .fetch_all(&pool)
    .await
    .expect("v_coverage_pauses");
    assert_eq!(
        pauses,
        vec![
            (t0 + 1_000, t0 + 2_000, 1_000, false),
            (t0 + 2_500, t0 + 3_000, 500, true)
        ]
    );
    let (n_pauses, paused_s): (i64, String) = sqlx::query_as(
        "SELECT coverage_pauses, coverage_paused_s::text FROM v_market_coverage WHERE market = $1",
    )
    .bind(&m1)
    .fetch_one(&pool)
    .await
    .expect("v_market_coverage");
    assert_eq!((n_pauses, paused_s.as_str()), (2, "1500"));

    // The coin creator's share of S: its shares are in epoch 0, the market in epoch 2 after
    // two rebases by 1,000.
    let creator = key(60);
    sqlx::query("UPDATE markets SET coin_creator = $1 WHERE market = $2")
        .bind(&creator)
        .bind(&m1)
        .execute(&pool)
        .await
        .expect("coin_creator");
    let mut ep = History {
        pool: &pool,
        market: m1.clone(),
        n: 500,
    };
    for epoch in 0..2u32 {
        let (n, slot) = ep.tx(t0 + 10 + i64::from(epoch)).await;
        seed_event(&pool, n, 0, slot, 0, "EpochRolled", &m1, t0, "{}").await;
        sqlx::query(
            "INSERT INTO share_epochs (market, epoch, acc_end, num, den, vest_flushed, full_wipe, \
             signature, ix_index, ev_source, ev_seq, slot, ts) \
             VALUES ($1, $2, 0, 1, 1000, 0, false, $3, 0, 0, 0, $4, $5)",
        )
        .bind(&m1)
        .bind(i64::from(epoch))
        .bind(sig(n))
        .bind(slot)
        .bind(t0)
        .execute(&pool)
        .await
        .expect("share_epochs");
    }
    let creator_shares: u128 = 500_000_000_000;
    sqlx::query(
        "INSERT INTO lender_positions (market, owner, shares, epoch, deposited_tokens, deposits, \
         first_slot, first_ts, last_slot, last_ts) \
         VALUES ($1, $2, $3::numeric, 0, 777, 1, 1, $4, 1, $4)",
    )
    .bind(&m1)
    .bind(&creator)
    .bind(creator_shares.to_string())
    .bind(t0)
    .execute(&pool)
    .await
    .expect("lender_positions");
    let bps: Option<String> = sqlx::query_scalar(
        "SELECT coin_creator_share_bps::text FROM v_market_creator WHERE market = $1",
    )
    .bind(&m1)
    .fetch_one(&pool)
    .await
    .expect("v_market_creator");
    let converted = formulas::shares_at_epoch(creator_shares, [(1, 1_000), (1, 1_000)]);
    assert_eq!(
        bps,
        formulas::share_bps(converted, last1.shares).map(|v| v.to_string())
    );

    // Overview and global: TVL / OI / utilisation and the global utilisation at mark.
    let (tvl, oi, util, known): (String, String, i64, bool) = sqlx::query_as(
        "SELECT tvl_lamports::text, oi_lamports::text, util_bps, price_known \
         FROM v_market_overview WHERE market = $1",
    )
    .bind(&m1)
    .fetch_one(&pool)
    .await
    .expect("v_market_overview");
    assert_eq!(
        tvl,
        formulas::tvl_lamports(1_000_000, 2 * WAD, 12_345, 70_000).to_string()
    );
    assert_eq!(oi, formulas::oi_lamports(3_000, 2 * WAD).to_string());
    assert_eq!(
        util,
        i64::from(shortwin_indexer::numeric::util_bps(1_000_000, 3_000))
    );
    assert!(known);
    let known2: bool =
        sqlx::query_scalar("SELECT price_known FROM v_market_overview WHERE market = $1")
            .bind(&m2)
            .fetch_one(&pool)
            .await
            .expect("unpriced");
    assert!(!known2, "no P_ema: price unknown");
    let (g_util, g_inv): (Option<i64>, String) =
        sqlx::query_as("SELECT util_bps, inventory_value_lamports::text FROM v_global_stats")
            .fetch_one(&pool)
            .await
            .expect("v_global_stats");
    let inv = formulas::inventory_value_lamports(1_000_000, 3_000, 2 * WAD);
    assert_eq!(g_inv, inv.to_string());
    assert_eq!(
        g_util,
        formulas::global_util_bps(formulas::oi_lamports(3_000, 2 * WAD), inv)
            .map(|v| i64::try_from(v).expect("bps"))
    );
    pool.close().await;
    pg.stop().await.expect("stop");
}

// ---- rollups -----------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct Hour {
    bucket_start: i64,
    tokens_free: String,
    borrowed_q: String,
    open_positions: i64,
    ema_price: Option<String>,
    util_bps: i64,
    oi_lamports: String,
    tvl_lamports: String,
    fees_protocol: String,
    fees_lenders: String,
    opens: i64,
    closes: i64,
    coverage_gaps: i64,
    coverage_gap_s: i64,
    positions_open_s: i64,
    coverage_paused_s: i64,
    observations: i64,
    max_observation_gap_s: Option<i64>,
}

async fn hours(pool: &PgPool, market: &str) -> Vec<Hour> {
    sqlx::query_as(
        "SELECT bucket_start, tokens_free::text, borrowed_q::text, open_positions, \
                ema_price::text, util_bps, oi_lamports::text, tvl_lamports::text, \
                fees_protocol::text, fees_lenders::text, opens, closes, coverage_gaps, \
                coverage_gap_s, positions_open_s, coverage_paused_s, observations, \
                max_observation_gap_s \
         FROM market_stats_hourly WHERE market = $1 ORDER BY bucket_start",
    )
    .bind(market)
    .fetch_all(pool)
    .await
    .expect("market_stats_hourly")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
async fn rollups_carry_state_and_sum_flows_hour_by_hour() {
    let (pg, pool) = start().await;
    let h0 = rollups::bucket_of(1_790_000_000);
    let (h1, h2, h3) = (h0 + 3_600, h0 + 7_200, h0 + 10_800);
    let market = key(2);
    let listed = State {
        protocol_fees: 25_000_000,
        ..State::default()
    };
    let deposited = State {
        tokens_free: 1_000_000,
        shares: 1_000_000,
        ..listed
    };
    let opened = State {
        tokens_free: 999_000,
        borrowed_q: 1_000,
        open: 1,
        notional: 2_000,
        vault: 5_000,
        protocol_fees: 25_000_010,
        ..deposited
    };
    let closed = State {
        tokens_free: 1_000_000,
        borrowed_q: 0,
        open: 0,
        notional: 0,
        vault: 0,
        lender_sol: 10,
        protocol_fees: 25_000_012,
        ..opened
    };
    seed_market(&pool, &market, &key(20), h0 + 100, closed, Some(3 * WAD), 0).await;
    let mut h = History {
        pool: &pool,
        market: market.clone(),
        n: 0,
    };
    h.log(h0 + 100, "MarketCreated", listed).await;
    h.fee(h0 + 100, "creation_fee", "protocol", None, 25_000_000)
        .await;
    h.log(h0 + 200, "Deposit", deposited).await;
    h.crank(h0 + 300, 2 * WAD).await;
    h.log(h0 + 400, "Open", opened).await;
    let (n, slot) = h.tx(h0 + 400).await;
    h.observe(n, slot, h0 + 400, "open_short", false).await;
    h.fee(h0 + 400, "open_fee", "protocol", None, 10).await;
    seed_position(
        &pool,
        &key(40),
        &market,
        &key(50),
        1_000,
        1_900,
        2 * WAD,
        2 * WAD,
        h0 + 400,
        "closed",
        Some((h1 + 500, None, 1_100, 1_000)),
    )
    .await;
    h.coverage(h0 + 1_000, "pause_set", None, Some(true)).await;
    h.coverage(h1 + 50, "gap", Some(45), None).await;
    h.crank(h1 + 100, 3 * WAD).await;
    h.coverage(h1 + 200, "pause_set", None, Some(false)).await;
    h.coverage(h1 + 300, "pause_set", None, Some(true)).await;
    h.log(h1 + 500, "Close", closed).await;
    let (n, slot) = h.tx(h1 + 500).await;
    h.observe(n, slot, h1 + 500, "close_short", false).await;
    let (n, slot) = h.tx(h1 + 600).await;
    h.observe(n, slot, h1 + 600, "crank_price", true).await;
    h.fee(h1 + 500, "rent_lender", "lenders", None, 10).await;
    h.fee(h1 + 500, "rent_protocol", "protocol", None, 2).await;
    sqlx::query("UPDATE ingest_state SET latest_block_time = $1")
        .bind(h3 + 1_800)
        .execute(&pool)
        .await
        .expect("chain clock");

    // One pass up to the chain clock: three complete hours (h0, h1, h2).
    let p = rollups::pass(&pool, None).await.expect("pass");
    assert_eq!(
        (p.from, p.to, p.market_hours, p.global_hours),
        (0, h3, 3, 3)
    );
    let got = hours(&pool, &market).await;
    let w = |s: &State, ema: u128| {
        (
            s.tokens_free.to_string(),
            s.borrowed_q.to_string(),
            i64::from(s.open),
            formulas::oi_lamports(s.borrowed_q, ema).to_string(),
            formulas::tvl_lamports(s.tokens_free, ema, s.lender_sol, s.vault).to_string(),
            i64::from(shortwin_indexer::numeric::util_bps(
                s.tokens_free,
                s.borrowed_q,
            )),
        )
    };
    let state = |x: &Hour| {
        (
            x.tokens_free.clone(),
            x.borrowed_q.clone(),
            x.open_positions,
            x.oi_lamports.clone(),
            x.tvl_lamports.clone(),
            x.util_bps,
        )
    };
    assert_eq!(got.len(), 3);
    // h0: open from +400; paused from +1000; P_ema 2 (the crank, then the open at the same price).
    assert_eq!(state(&got[0]), w(&opened, 2 * WAD));
    assert_eq!(
        got[0].ema_price.as_deref(),
        Some((2 * WAD).to_string().as_str())
    );
    assert_eq!(
        (got[0].fees_protocol.as_str(), got[0].opens),
        ("25000010", 1)
    );
    assert_eq!(
        got[0].positions_open_s,
        formulas::overlap_seconds(&[(h0 + 400, h1 + 500)], h0, h1)
    );
    assert_eq!(
        got[0].coverage_paused_s,
        formulas::overlap_seconds(&[(h0 + 1_000, h1 + 200), (h1 + 300, h3)], h0, h1)
    );
    // Two observations (the crank at +300, the open at +400); the gap between them started
    // with no position open.
    assert_eq!(
        (got[0].observations, got[0].max_observation_gap_s),
        (2, None)
    );
    // h1: the close at +500 (rent), the gap, the crank at 3; the failed crank is no observation.
    assert_eq!(state(&got[1]), w(&closed, 3 * WAD));
    assert_eq!(
        (got[1].closes, got[1].coverage_gaps, got[1].coverage_gap_s),
        (1, 1, 45)
    );
    assert_eq!(
        (got[1].fees_protocol.as_str(), got[1].fees_lenders.as_str()),
        ("2", "10")
    );
    assert_eq!(got[1].positions_open_s, 500);
    assert_eq!(
        got[1].coverage_paused_s,
        formulas::overlap_seconds(&[(h0 + 1_000, h1 + 200), (h1 + 300, h3)], h1, h2)
    );
    // Observations at h1+100 (the crank, 3,300 s after the open: the largest gap) and h1+500.
    assert_eq!(
        (got[1].observations, got[1].max_observation_gap_s),
        (2, Some(3_300))
    );
    // h2: nothing happens — the state carries forward, the pause continues.
    assert_eq!(state(&got[2]), w(&closed, 3 * WAD));
    assert_eq!(
        (got[2].opens, got[2].closes, got[2].observations),
        (0, 0, 0)
    );
    assert_eq!(got[2].coverage_paused_s, 3_600);
    assert_eq!(got[2].positions_open_s, 0);

    // The global hours sum the markets'.
    let g: Vec<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT bucket_start, markets, tvl_lamports::text, fees_protocol::text \
         FROM global_stats_hourly ORDER BY bucket_start",
    )
    .fetch_all(&pool)
    .await
    .expect("global_stats_hourly");
    assert_eq!(g.len(), 3);
    for (i, x) in got.iter().enumerate() {
        assert_eq!((g[i].0, g[i].1), (x.bucket_start, 1));
        assert_eq!((&g[i].2, &g[i].3), (&x.tvl_lamports, &x.fees_protocol));
    }

    // Idempotent: a second pass computes nothing new and changes nothing.
    let again = rollups::pass(&pool, None).await.expect("pass");
    assert_eq!(
        (again.from, again.to, again.market_hours, again.late_reset),
        (h3, h3, 0, false)
    );
    assert_eq!(hours(&pool, &market).await, got);

    // Late data: an event committed now with a block time in h1 (a gap-fill) recomputes from h1.
    h.fee(h1 + 900, "open_fee", "protocol", None, 1_000).await;
    let late = rollups::pass(&pool, None).await.expect("pass");
    assert!(late.late_reset);
    assert_eq!((late.from, late.to, late.market_hours), (h1, h3, 2));
    let after = hours(&pool, &market).await;
    assert_eq!(after[1].fees_protocol, "1002");
    assert_eq!(after[0], got[0]);

    // An explicit reset (the rollback path) recomputes from that hour; the hours equal.
    rollups::reset_from(&pool, h0 + 10).await.expect("reset");
    let p = rollups::pass(&pool, Some(h3)).await.expect("pass");
    assert_eq!((p.from, p.to, p.market_hours), (h0, h3, 3));
    assert_eq!(hours(&pool, &market).await, after);

    // The daily view: flows summed, state of the day's last hour.
    let (opens, closes, tvl): (String, String, String) = sqlx::query_as(
        "SELECT opens::text, closes::text, tvl_lamports::text FROM v_market_stats_daily \
         WHERE market = $1",
    )
    .bind(&market)
    .fetch_one(&pool)
    .await
    .expect("v_market_stats_daily");
    assert_eq!((opens.as_str(), closes.as_str()), ("1", "1"));
    assert_eq!(tvl, after[2].tvl_lamports);
    pool.close().await;
    pg.stop().await.expect("stop");
}

// ---- retention ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_prunes_old_rolled_up_non_admin_invocations_only() {
    let (pg, pool) = start().await;
    let now = 1_800_000_000i64;
    let market = key(2);
    seed_market(
        &pool,
        &market,
        &key(20),
        now - 90 * 86_400,
        State::default(),
        None,
        0,
    )
    .await;
    let mut h = History {
        pool: &pool,
        market: market.clone(),
        n: 0,
    };
    // 40 days ago: a crank (pruned) and a set_pause (kept); 10 days ago: a crank (kept: recent).
    for (age_days, name) in [(40, "crank_price"), (40, "set_pause"), (10, "crank_price")] {
        let ts = now - age_days * 86_400;
        let (n, slot) = h.tx(ts).await;
        h.observe(n, slot, ts, name, false).await;
    }
    sqlx::query("UPDATE ingest_state SET latest_block_time = $1")
        .bind(now)
        .execute(&pool)
        .await
        .expect("chain clock");
    // Nothing is rolled up yet: nothing may be pruned.
    assert_eq!(
        analytics::prune_invocations(&pool, 30)
            .await
            .expect("prune"),
        0
    );
    rollups::run_once(&pool, None).await.expect("rollups");
    assert_eq!(
        analytics::prune_invocations(&pool, 30)
            .await
            .expect("prune"),
        1
    );
    let left: Vec<String> =
        sqlx::query_scalar("SELECT name FROM invocations ORDER BY block_time, name")
            .fetch_all(&pool)
            .await
            .expect("invocations");
    assert_eq!(
        left,
        vec!["set_pause".to_string(), "crank_price".to_string()]
    );
    pool.close().await;
    pg.stop().await.expect("stop");
}
