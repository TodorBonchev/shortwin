//! The embedded-Postgres fixture starts in user space and applies the whole schema
//! (INDEXER.md §11.1; the M8 contract's gate). FOUNDATION.

use shortwin_indexer::testing::pg::TestPg;

/// Every table, view and function the contract defines exists after the migrations, the domains
/// enforce their ranges, and the SQL helpers agree with the Rust formulas.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_starts_and_applies_the_migrations() {
    let pg = TestPg::start_in(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")))
        .await
        .expect("the embedded PostgreSQL starts");
    let pool = pg.fresh_db().await.expect("a migrated database");

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .expect("the migrations table");
    assert_eq!(
        usize::try_from(applied).expect("count"),
        shortwin_indexer::db::MIGRATOR.iter().count()
    );

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .expect("tables");
    for t in [
        "ingest_txs",
        "ingest_state",
        "ingest_cursors",
        "ingest_quarantine",
        "ingest_rollbacks",
        "events",
        "invocations",
        "markets",
        "market_state_log",
        "positions",
        "lender_positions",
        "share_epochs",
        "settlements",
        "fees",
        "protocol_sweeps",
        "market_snapshots",
        "coverage_events",
        "pool_events",
        "pools",
        "token_metadata",
        "account_snapshots",
        "oracle_prices",
        "venue_pools",
        "venue_trades",
        "venue_events",
        "venue_program_upgrades",
        "venue_admin_states",
        "config_changes",
        "protocol_params_history",
        "protocol_state",
        "venue_guards",
        "venue_extras",
        "market_stats_hourly",
        "global_stats_hourly",
        "rollup_state",
        "api_keys",
        // views
        "v_sol_usd",
        "v_market_overview",
        "v_market_fees",
        "v_market_exits",
        "v_market_lender_loss",
        "v_market_creator",
        "v_coverage_pauses",
        "v_market_coverage",
        "v_keeper_activity",
        "v_keeper_failures",
        "v_global_stats",
        "v_market_stats_daily",
        "v_global_stats_daily",
    ] {
        assert!(
            tables.iter().any(|x| x == t),
            "{t} missing after the migrations"
        );
    }

    // The views evaluate on an empty database.
    let markets: i64 = sqlx::query_scalar("SELECT markets FROM v_global_stats")
        .fetch_one(&pool)
        .await
        .expect("v_global_stats");
    assert_eq!(markets, 0);

    // Domains keep the program's integer ranges exactly.
    let max: String = sqlx::query_scalar("SELECT (18446744073709551615::u64)::text")
        .fetch_one(&pool)
        .await
        .expect("u64 max");
    assert_eq!(max, u64::MAX.to_string());
    assert!(
        sqlx::query("SELECT (18446744073709551616::u64)")
            .execute(&pool)
            .await
            .is_err(),
        "u64 must reject 2^64"
    );
    assert!(
        sqlx::query("SELECT ('not-a-key'::pubkey)")
            .execute(&pool)
            .await
            .is_err()
    );

    // The SQL helpers agree with the Rust formulas.
    let (util, oi): (i64, String) = sqlx::query_as(
        "SELECT sw_util_bps(2, 1), sw_mul_wad(18446744073709551615, 1000000000000000000000)::text",
    )
    .fetch_one(&pool)
    .await
    .expect("helpers");
    assert_eq!(util, i64::from(shortwin_indexer::numeric::util_bps(2, 1)));
    assert_eq!(
        oi,
        shortwin_indexer::analytics::formulas::oi_lamports(
            u64::MAX,
            1_000 * shortwin_indexer::numeric::WAD
        )
        .to_string()
    );

    // The single-row state tables are seeded.
    let finalized: i64 = sqlx::query_scalar("SELECT finalized_slot FROM ingest_state")
        .fetch_one(&pool)
        .await
        .expect("ingest_state");
    assert_eq!(finalized, 0);

    pool.close().await;
    pg.stop().await.expect("stop");
}

/// The analytics views compute the §8 formulas on a seeded market (and show the tracks the
/// binding convention: u64/u128 as decimal text cast to NUMERIC, keys base58).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn views_match_the_formulas_on_a_seeded_market() {
    use shortwin_indexer::analytics::formulas;
    use shortwin_indexer::numeric::WAD;
    use solana_pubkey::Pubkey;
    use solana_signature::Signature;

    let pg = TestPg::start_in(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")))
        .await
        .expect("the embedded PostgreSQL starts");
    let pool = pg.fresh_db().await.expect("a migrated database");
    let key = |b: u8| Pubkey::new_from_array([b; 32]).to_string();
    let sig = Signature::from([9u8; 64]).to_string();
    let (market, mint, creator, position, borrower, keeper) =
        (key(2), key(3), key(4), key(5), key(6), key(7));

    sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
         fee_payer, fee) VALUES ($1, 100, 0, 1790000000, 'finalized', 'replay', $2, 5000::numeric)",
    )
    .bind(&sig)
    .bind(&keeper)
    .execute(&pool)
    .await
    .expect("ingest_txs");
    for (seq, kind) in [(0, "Open"), (1, "SolSettle")] {
        sqlx::query(
            "INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, \
             block_time, program, invocation, kind, market, ev_slot, ev_ts, data) \
             VALUES ($1, 0, 0, $2, 100, 0, 1790000000, $3, 'test', $4, $5, 100, 1790000000, '{}')",
        )
        .bind(&sig)
        .bind(seq)
        .bind(shortwin::ID.to_string())
        .bind(kind)
        .bind(&market)
        .execute(&pool)
        .await
        .expect("events");
    }
    // T = 1,000, ΣB_Q = 10, P_ema = 0.5 lamport per unit, lender_sol 7, open vaults 100.
    sqlx::query(
        "INSERT INTO markets (market, mint, token_program, creator, created_slot, created_ts, \
         created_signature, creation_fee, venue_at_listing, tier_at_listing, venue, tier, \
         tokens_free, borrowed_q, open_positions, open_vault_lamports, lender_sol, ema_price, \
         last_slot, last_tx_index, last_ix_index, last_ev_source, last_ev_seq) \
         VALUES ($1, $2, $3, $4, 100, 1790000000, $5, 0::numeric, 6, 3, 6, 3, \
                 $6::numeric, $7::numeric, 1, $8::numeric, $9::numeric, $10::numeric, \
                 100, 0, 0, 0, 1)",
    )
    .bind(&market)
    .bind(&mint)
    .bind(shortwin::constants::TOKEN_PROGRAM_ID.to_string())
    .bind(&creator)
    .bind(&sig)
    .bind("1000")
    .bind("10")
    .bind("100")
    .bind("7")
    .bind((WAD / 2).to_string())
    .execute(&pool)
    .await
    .expect("markets");
    // A gap settlement: q = 100 at P_ref = 2 lamports, lenders got 150.
    let p_ref = 2 * WAD;
    let outcome = formulas::settlement_outcome(150, 100, p_ref);
    sqlx::query(
        "INSERT INTO settlements (position, market, keeper, condition, b_live, b_rs, b_ema, p_lo, \
         p_hi, l_lo, l_hi, x_lo, x_hi, pprime_lo, pprime_hi, fv, k_s, lender_amount, lump, \
         borrower_amount, ema_age_s, q, n_open, tier, expired, p_ref, p_ref_source, token_value, \
         lender_pnl, realised_lender_loss, signature, ix_index, ev_source, ev_seq, slot, \
         tx_index, ts) \
         VALUES ($1, $2, $3, 1, 0, 0, 0, $4::numeric, $4::numeric, 0, 0, 0, 0, 0, 0, 0, 0, \
                 150, 0, 0, 0, 100, 400, 3, false, $4::numeric, 'box_point', $5::numeric, \
                 $6::numeric, $7::numeric, $8, 0, 0, 1, 100, 0, 1790000000)",
    )
    .bind(&position)
    .bind(&market)
    .bind(&keeper)
    .bind(p_ref.to_string())
    .bind(outcome.token_value.to_string())
    .bind(outcome.lender_pnl.to_string())
    .bind(outcome.realised_lender_loss.to_string())
    .bind(&sig)
    .execute(&pool)
    .await
    .expect("settlements");
    sqlx::query(
        "INSERT INTO fees (signature, ix_index, ev_source, ev_seq, fee_type, recipient, market, \
         position, amount, slot, tx_index, ts) \
         VALUES ($1, 0, 0, 0, 'open_fee', 'protocol', $2, $3, 25::numeric, 100, 0, 1790000000)",
    )
    .bind(&sig)
    .bind(&market)
    .bind(&position)
    .execute(&pool)
    .await
    .expect("fees");
    sqlx::query(
        "INSERT INTO positions (position, market, borrower, nonce, tier, venue, q, s, m, v_open, \
         v, n_open, fee_open, venue_rent, entry_price, spot_wad_open, ema_wad_open, b_spot_open, \
         b_ema_open, deadline, opened_slot, opened_tx_index, opened_ts, open_signature, status) \
         VALUES ($1, $2, $3, 0, 3, 6, 10, 5, 95, 100, 100, 5, 25, 0, 0, 0, 0, 0, 0, \
                 1790259200, 100, 0, 1790000000, $4, 'open')",
    )
    .bind(&position)
    .bind(&market)
    .bind(&borrower)
    .bind(&sig)
    .execute(&pool)
    .await
    .expect("positions");

    let (tvl, oi, util): (String, String, i64) = sqlx::query_as(
        "SELECT tvl_lamports::text, oi_lamports::text, util_bps FROM v_market_overview \
         WHERE market = $1",
    )
    .bind(&market)
    .fetch_one(&pool)
    .await
    .expect("v_market_overview");
    assert_eq!(
        tvl,
        formulas::tvl_lamports(1_000, WAD / 2, 7, 100).to_string()
    );
    assert_eq!(oi, formulas::oi_lamports(10, WAD / 2).to_string());
    assert_eq!(
        util,
        i64::from(shortwin_indexer::numeric::util_bps(1_000, 10))
    );

    let (loss, bps): (String, Option<String>) = sqlx::query_as(
        "SELECT realised_lender_loss::text, loss_bps_of_value::text FROM v_market_lender_loss \
         WHERE market = $1",
    )
    .bind(&market)
    .fetch_one(&pool)
    .await
    .expect("v_market_lender_loss");
    assert_eq!(loss, "50");
    assert_eq!(bps.as_deref(), Some("2500"));

    let (revenue, s1, borrowers): (String, i64, i64) = sqlx::query_as(
        "SELECT protocol_revenue::text, settlements_s1, unique_borrowers FROM v_global_stats",
    )
    .fetch_one(&pool)
    .await
    .expect("v_global_stats");
    assert_eq!((revenue.as_str(), s1, borrowers), ("25", 1, 1));

    // Deleting the transaction removes everything decoded from it (the rollback path, §3.4).
    sqlx::query("DELETE FROM ingest_txs WHERE signature = $1")
        .bind(&sig)
        .execute(&pool)
        .await
        .expect("rollback");
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM events) + (SELECT count(*) FROM settlements) \
         + (SELECT count(*) FROM fees)",
    )
    .fetch_one(&pool)
    .await
    .expect("counts");
    assert_eq!(left, 0);

    pool.close().await;
    pg.stop().await.expect("stop");
}
