-- short.win indexer — 0006 analytics (INDEXER.md §8). Owner: ANALYTICS.
--
-- Views over the projections (current values) and hourly rollup tables the analytics job fills
-- (time series, §8.6). Every formula is the one INDEXER.md §8 states, with a Rust twin in
-- `analytics::formulas` (the tests hold them equal); amounts are lamports (u64 / NUMERIC) unless
-- named `_tokens`; prices are WAD (lamports per raw token unit × 10^18). Windows ("24 h") use the
-- chain clock `sw_chain_now()`. Columns are only ever added to a view (the API reads them by
-- name).

-- ---- helpers ---------------------------------------------------------------------------------

-- Opening price impact in bps (§8.3 "execution slippage"; `formulas::open_impact_bps`): the
-- sale's shortfall from the pre-trade spot value ⌊(q·spot/WAD − s)·10⁴ / (q·spot/WAD)⌋, 0 when
-- the sale got at least the spot value (or the spot value is 0).
CREATE FUNCTION sw_open_impact_bps(q NUMERIC, spot NUMERIC, s NUMERIC) RETURNS NUMERIC
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE WHEN sw_mul_wad(q, spot) = 0 OR s >= sw_mul_wad(q, spot) THEN 0
                ELSE div((sw_mul_wad(q, spot) - s) * 10000, sw_mul_wad(q, spot)) END
$$;

-- Exit execution against its quote in bps (§8.3; `formulas::exit_slippage_bps`):
-- (buyback_cost − b_live)·10⁴ / b_live, truncated toward zero (negative: better than quoted);
-- NULL without a quote.
CREATE FUNCTION sw_exit_slippage_bps(buyback NUMERIC, b_live NUMERIC) RETURNS NUMERIC
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE WHEN buyback IS NULL OR b_live IS NULL OR b_live = 0 THEN NULL
                ELSE div((buyback - b_live) * 10000, b_live) END
$$;

-- A lender's shares converted from epoch `p_from` to epoch `p_to` through the market's
-- `share_epochs` records (§6.3; `formulas::convert_shares` applied per record):
-- shares := ⌊shares·num/den⌋ for every record e with p_from ≤ e < p_to.
CREATE FUNCTION sw_shares_at_epoch(p_market TEXT, p_shares NUMERIC, p_from BIGINT, p_to BIGINT)
    RETURNS NUMERIC LANGUAGE plpgsql STABLE AS $$
DECLARE
    s NUMERIC := p_shares;
    r RECORD;
BEGIN
    IF p_shares IS NULL OR p_from >= p_to THEN
        RETURN p_shares;
    END IF;
    FOR r IN SELECT num, den FROM share_epochs
             WHERE market = p_market AND epoch >= p_from AND epoch < p_to
             ORDER BY epoch
    LOOP
        s := CASE WHEN r.den = 0 THEN 0 ELSE div(s * r.num, r.den) END;
    END LOOP;
    RETURN s;
END
$$;

-- The instructions whose prologue records an observation (`model::OBSERVING_INSTRUCTIONS`).
CREATE FUNCTION sw_observing(name TEXT) RETURNS BOOLEAN
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$
    SELECT name IN ('crank_price', 'open_short', 'close_short', 'liquidate', 'expire',
                    'sol_settle', 'propose_pool', 'execute_pool_switch')
$$;

-- The SOL/USD price in force (OracleCache): USD = lamports · price · 10^expo / 10^9.
CREATE VIEW v_sol_usd AS
SELECT price, expo, conf, publish_time,
       price::NUMERIC * power(10::NUMERIC, expo) AS usd_per_sol
FROM oracle_prices
ORDER BY publish_time DESC
LIMIT 1;

-- ---- per market, current (§8.1) ---------------------------------------------------------------

CREATE VIEW v_market_overview AS
SELECT m.market,
       m.mint,
       tm.symbol,
       tm.name,
       tm.decimals,
       m.creator,
       m.coin_creator,
       m.venue,
       m.tier,
       m.created_ts,
       m.paused_opens,
       m.paused_coverage,
       m.mint_unsafe,
       m.route_broken,
       m.wipe_live,
       m.tokens_free,
       m.borrowed_q,
       (m.tokens_free + m.borrowed_q)                                   AS inventory_tokens,
       sw_util_bps(m.tokens_free, m.borrowed_q)                         AS util_bps,
       m.total_shares,
       m.open_positions,
       m.total_notional_open,
       m.open_vault_lamports,
       m.lender_sol,
       m.protocol_fees,
       m.lender_count,
       m.ema_price,
       m.spot_wad,
       m.price_ts,
       -- P_ema unknown (no crank yet, or a zero EMA): mark values use 0 (§8.1).
       (COALESCE(m.ema_price, 0) > 0)                                   AS price_known,
       -- OI at mark: ⌊ΣB_Q · P_ema / WAD⌋.
       sw_mul_wad(m.borrowed_q, COALESCE(m.ema_price, 0))               AS oi_lamports,
       -- TVL: unborrowed tokens at mark + SOL held for lenders + position vaults (§8.1).
       sw_mul_wad(m.tokens_free, COALESCE(m.ema_price, 0))
           + m.lender_sol + m.open_vault_lamports                       AS tvl_lamports,
       -- The lenders' claim at mark: (T + ΣB_Q)·P_ema/WAD + lender_sol.
       sw_mul_wad(m.tokens_free + m.borrowed_q, COALESCE(m.ema_price, 0))
           + m.lender_sol                                               AS lender_value_lamports,
       -- Lendable inventory at mark (the global utilisation's denominator, §8.7).
       sw_mul_wad(m.tokens_free + m.borrowed_q, COALESCE(m.ema_price, 0))
                                                                        AS inventory_value_lamports,
       m.share_epoch
FROM markets m
LEFT JOIN token_metadata tm ON tm.mint = m.mint;

-- Fees by type (§8.2): all time and the last 24 h of chain time.
CREATE VIEW v_market_fees AS
SELECT market,
       fee_type,
       recipient,
       count(*)                                                         AS n,
       sum(amount)                                                      AS total,
       COALESCE(sum(amount) FILTER (WHERE ts >= sw_chain_now() - 86400), 0) AS last_24h
FROM fees
GROUP BY market, fee_type, recipient;

-- Exits (§8.3): counts by outcome, S1/S2/S3, rates (bps of exits; SPEC §13 "close vs
-- liquidation vs SOL-settlement rate"), average duration, execution slippage, borrower P&L.
CREATE VIEW v_market_exits AS
SELECT market,
       count(*)                                                                    AS positions,
       count(*) FILTER (WHERE status = 'open')                                     AS open,
       count(*) FILTER (WHERE status = 'closed')                                   AS closes,
       count(*) FILTER (WHERE status = 'liquidated'
                          AND NOT COALESCE(liquidated_expired, FALSE))             AS liquidations_cr,
       count(*) FILTER (WHERE status = 'liquidated'
                          AND COALESCE(liquidated_expired, FALSE))                 AS liquidations_expired,
       count(*) FILTER (WHERE status = 'expired')                                  AS expiries,
       count(*) FILTER (WHERE status = 'settled' AND settle_condition = 1)         AS settlements_s1,
       count(*) FILTER (WHERE status = 'settled' AND settle_condition = 2)         AS settlements_s2,
       count(*) FILTER (WHERE status = 'settled' AND settle_condition = 3)         AS settlements_s3,
       count(*) FILTER (WHERE status <> 'open')                                    AS exits,
       avg(duration_s) FILTER (WHERE status <> 'open')                             AS avg_duration_s,
       COALESCE(sum(penalty) FILTER (WHERE status = 'liquidated'), 0)              AS liquidation_penalties,
       COALESCE(sum(q) FILTER (WHERE status = 'liquidated'), 0)                    AS liquidated_tokens,
       div(count(*) FILTER (WHERE status = 'closed') * 10000,
           NULLIF(count(*) FILTER (WHERE status <> 'open'), 0))::BIGINT            AS close_rate_bps,
       div(count(*) FILTER (WHERE status = 'liquidated') * 10000,
           NULLIF(count(*) FILTER (WHERE status <> 'open'), 0))::BIGINT            AS liquidation_rate_bps,
       div(count(*) FILTER (WHERE status = 'expired') * 10000,
           NULLIF(count(*) FILTER (WHERE status <> 'open'), 0))::BIGINT            AS expiry_rate_bps,
       div(count(*) FILTER (WHERE status = 'settled') * 10000,
           NULLIF(count(*) FILTER (WHERE status <> 'open'), 0))::BIGINT            AS settlement_rate_bps,
       avg(sw_open_impact_bps(q, spot_wad_open, s))                                AS avg_open_impact_bps,
       avg(sw_exit_slippage_bps(buyback_cost, b_live_exit))
           FILTER (WHERE status IN ('closed', 'liquidated', 'expired'))            AS avg_exit_slippage_bps,
       COALESCE(sum(borrower_pnl) FILTER (WHERE status <> 'open'), 0)              AS borrower_pnl
FROM positions
GROUP BY market;

-- Realised lender loss (§8.4; SPEC §13): SOL settlement value vs token value at settlement.
CREATE VIEW v_market_lender_loss AS
SELECT market,
       count(*)                                                AS settlements,
       count(*) FILTER (WHERE condition = 1)                   AS s1,
       count(*) FILTER (WHERE condition = 2)                   AS s2,
       count(*) FILTER (WHERE condition = 3)                   AS s3,
       sum(token_value)                                        AS token_value,
       sum(lender_amount)                                      AS lender_amount,
       sum(lender_pnl)                                         AS lender_pnl,
       sum(realised_lender_loss)                               AS realised_lender_loss,
       count(*) FILTER (WHERE realised_lender_loss > 0)        AS loss_events,
       CASE WHEN sum(token_value) > 0
            THEN div(sum(realised_lender_loss) * 10000, sum(token_value)) END AS loss_bps_of_value,
       CASE WHEN sum(n_open) > 0
            THEN div(sum(realised_lender_loss) * 10000, sum(n_open)) END      AS loss_bps_of_notional,
       COALESCE(sum(realised_lender_loss) FILTER (WHERE condition = 1), 0)    AS realised_lender_loss_s1,
       COALESCE(sum(realised_lender_loss) FILTER (WHERE condition = 2), 0)    AS realised_lender_loss_s2,
       COALESCE(sum(realised_lender_loss) FILTER (WHERE condition = 3), 0)    AS realised_lender_loss_s3,
       sum(n_open)                                             AS n_open
FROM settlements
GROUP BY market;

-- ---- counters the API reads instead of whole-table aggregates (M8-SEC-3) -------------------------
--
-- Kept exact by statement-level triggers on the rows they count, so every path that writes or
-- deletes those rows — the writer, a rollback's cascade, the retention prune, a rebuild's
-- re-attribution, an operator's re-ingest — moves them too. A request then reads O(1) rows per
-- market or keeper, however many observations, failed transactions or venue trades the tables
-- hold (organic keeper traffic is ≈ 86,400 invocations per covered market-day; failed
-- transactions cost an attacker ≈ 5,000 lamports each).

-- Venue trade totals per market (`v_market_creator`).
CREATE TABLE market_trade_totals (
    market                 pubkey  PRIMARY KEY,
    trades                 BIGINT  NOT NULL DEFAULT 0,
    shortwin_volume        NUMERIC NOT NULL DEFAULT 0,
    shortwin_creator_fees  NUMERIC NOT NULL DEFAULT 0,
    all_creator_fees       NUMERIC NOT NULL DEFAULT 0
);

CREATE FUNCTION sw_trade_totals_ins() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO market_trade_totals AS t (market, trades, shortwin_volume, shortwin_creator_fees,
                                          all_creator_fees)
    SELECT market, count(*),
           COALESCE(sum(sol_amount) FILTER (WHERE is_shortwin), 0),
           COALESCE(sum(fee_creator_lamports) FILTER (WHERE is_shortwin), 0),
           COALESCE(sum(fee_creator_lamports), 0)
    FROM new_rows WHERE market IS NOT NULL GROUP BY market
    ON CONFLICT (market) DO UPDATE SET
        trades = t.trades + EXCLUDED.trades,
        shortwin_volume = t.shortwin_volume + EXCLUDED.shortwin_volume,
        shortwin_creator_fees = t.shortwin_creator_fees + EXCLUDED.shortwin_creator_fees,
        all_creator_fees = t.all_creator_fees + EXCLUDED.all_creator_fees;
    RETURN NULL;
END
$$;

CREATE FUNCTION sw_trade_totals_del() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE market_trade_totals t SET
        trades = t.trades - d.trades,
        shortwin_volume = t.shortwin_volume - d.v,
        shortwin_creator_fees = t.shortwin_creator_fees - d.scf,
        all_creator_fees = t.all_creator_fees - d.acf
    FROM (SELECT market, count(*) AS trades,
                 COALESCE(sum(sol_amount) FILTER (WHERE is_shortwin), 0) AS v,
                 COALESCE(sum(fee_creator_lamports) FILTER (WHERE is_shortwin), 0) AS scf,
                 COALESCE(sum(fee_creator_lamports), 0) AS acf
          FROM old_rows WHERE market IS NOT NULL GROUP BY market) d
    WHERE t.market = d.market;
    DELETE FROM market_trade_totals WHERE trades <= 0;
    RETURN NULL;
END
$$;

-- `UPDATE … SET market` (the projection attributes a pool's trades, a rebuild clears them):
-- out of the old market, into the new one.
CREATE FUNCTION sw_trade_totals_upd() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE market_trade_totals t SET
        trades = t.trades - d.trades,
        shortwin_volume = t.shortwin_volume - d.v,
        shortwin_creator_fees = t.shortwin_creator_fees - d.scf,
        all_creator_fees = t.all_creator_fees - d.acf
    FROM (SELECT market, count(*) AS trades,
                 COALESCE(sum(sol_amount) FILTER (WHERE is_shortwin), 0) AS v,
                 COALESCE(sum(fee_creator_lamports) FILTER (WHERE is_shortwin), 0) AS scf,
                 COALESCE(sum(fee_creator_lamports), 0) AS acf
          FROM old_rows WHERE market IS NOT NULL GROUP BY market) d
    WHERE t.market = d.market;
    INSERT INTO market_trade_totals AS t (market, trades, shortwin_volume, shortwin_creator_fees,
                                          all_creator_fees)
    SELECT market, count(*),
           COALESCE(sum(sol_amount) FILTER (WHERE is_shortwin), 0),
           COALESCE(sum(fee_creator_lamports) FILTER (WHERE is_shortwin), 0),
           COALESCE(sum(fee_creator_lamports), 0)
    FROM new_rows WHERE market IS NOT NULL GROUP BY market
    ON CONFLICT (market) DO UPDATE SET
        trades = t.trades + EXCLUDED.trades,
        shortwin_volume = t.shortwin_volume + EXCLUDED.shortwin_volume,
        shortwin_creator_fees = t.shortwin_creator_fees + EXCLUDED.shortwin_creator_fees,
        all_creator_fees = t.all_creator_fees + EXCLUDED.all_creator_fees;
    DELETE FROM market_trade_totals WHERE trades <= 0;
    RETURN NULL;
END
$$;

CREATE TRIGGER venue_trades_totals_ins AFTER INSERT ON venue_trades
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION sw_trade_totals_ins();
CREATE TRIGGER venue_trades_totals_del AFTER DELETE ON venue_trades
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION sw_trade_totals_del();
CREATE TRIGGER venue_trades_totals_upd AFTER UPDATE ON venue_trades
    REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION sw_trade_totals_upd();

-- Observing invocations per market and hour (`v_market_coverage` / `sw_market_coverage`: the
-- last 24 h are the complete hours from these plus one partial hour read from the rows).
CREATE TABLE market_observations_hourly (
    market        pubkey NOT NULL,
    bucket_start  BIGINT NOT NULL,
    observations  BIGINT NOT NULL DEFAULT 0,
    failed        BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (market, bucket_start)
);

CREATE FUNCTION sw_obs_counts_ins() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO market_observations_hourly AS h (market, bucket_start, observations, failed)
    SELECT market, (block_time / 3600) * 3600,
           count(*) FILTER (WHERE NOT failed), count(*) FILTER (WHERE failed)
    FROM new_rows
    WHERE market IS NOT NULL AND block_time IS NOT NULL AND sw_observing(name)
    GROUP BY 1, 2
    ON CONFLICT (market, bucket_start) DO UPDATE SET
        observations = h.observations + EXCLUDED.observations,
        failed = h.failed + EXCLUDED.failed;
    RETURN NULL;
END
$$;

CREATE FUNCTION sw_obs_counts_del() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    WITH d AS (
        SELECT market, (block_time / 3600) * 3600 AS b,
               count(*) FILTER (WHERE NOT failed) AS obs, count(*) FILTER (WHERE failed) AS failed
        FROM old_rows
        WHERE market IS NOT NULL AND block_time IS NOT NULL AND sw_observing(name)
        GROUP BY 1, 2)
    UPDATE market_observations_hourly h SET
        observations = h.observations - d.obs, failed = h.failed - d.failed
    FROM d WHERE h.market = d.market AND h.bucket_start = d.b;
    DELETE FROM market_observations_hourly h
    USING (SELECT DISTINCT market, (block_time / 3600) * 3600 AS b FROM old_rows
           WHERE market IS NOT NULL AND block_time IS NOT NULL) d
    WHERE h.market = d.market AND h.bucket_start = d.b
      AND h.observations <= 0 AND h.failed <= 0;
    RETURN NULL;
END
$$;

CREATE TRIGGER invocations_obs_ins AFTER INSERT ON invocations
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION sw_obs_counts_ins();
CREATE TRIGGER invocations_obs_del AFTER DELETE ON invocations
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION sw_obs_counts_del();

-- The instructions a failed keeper attempt is counted for (§8.5, `v_keeper_failures`).
CREATE FUNCTION sw_keeper_attempt(name TEXT) RETURNS BOOLEAN
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$
    SELECT name IN ('crank_price', 'liquidate', 'expire', 'sol_settle', 'sweep_trader',
                    'propose_pool', 'execute_pool_switch', 'set_coverage_pause')
$$;

-- Failed keeper attempts by fee payer, instruction and error code: `v_keeper_failures`'s
-- `failures` (one per transaction and instruction), kept as counters. `err_code` -1: no custom
-- program error. `last_ts`: the newest failure counted (a rollback or the prune does not lower
-- it).
CREATE TABLE keeper_failure_counts (
    keeper       pubkey NOT NULL,
    instruction  TEXT   NOT NULL,
    err_code     BIGINT NOT NULL,
    failures     BIGINT NOT NULL,
    last_ts      BIGINT,
    PRIMARY KEY (keeper, instruction, err_code)
);

-- New failed invocations: a (transaction, instruction) pair counts once, when its first row
-- arrives (the writer inserts ingest_txs first, then the invocations). Procedural on purpose:
-- every read filters on the signature alone (the instruction in a FILTER), so only the primary
-- keys can serve it, and sequential scans are off, whatever stale statistics would suggest — a
-- plan over `invocations (name, …)` would read every observation per failed transaction.
CREATE FUNCTION sw_keeper_failures_ins() RETURNS trigger LANGUAGE plpgsql
    SET enable_seqscan = off AS $$
DECLARE
    r RECORD;
    t RECORD;
    n BIGINT;
BEGIN
    FOR r IN SELECT signature, name, count(*) AS rows_new FROM new_rows
             WHERE failed AND sw_keeper_attempt(name) GROUP BY 1, 2
    LOOP
        SELECT fee_payer, err_code, block_time INTO t
        FROM ingest_txs WHERE signature = r.signature AND err IS NOT NULL;
        CONTINUE WHEN NOT FOUND;
        SELECT count(*) FILTER (WHERE name = r.name) INTO n
        FROM invocations WHERE signature = r.signature;
        CONTINUE WHEN n <> r.rows_new;
        INSERT INTO keeper_failure_counts AS k (keeper, instruction, err_code, failures, last_ts)
        VALUES (t.fee_payer, r.name, COALESCE(t.err_code, -1), 1, t.block_time)
        ON CONFLICT (keeper, instruction, err_code) DO UPDATE SET
            failures = k.failures + 1,
            last_ts = GREATEST(k.last_ts, EXCLUDED.last_ts);
    END LOOP;
    RETURN NULL;
END
$$;

-- Deleted invocations whose transaction stays (the retention prune): a pair leaves the count
-- with its last row. A deleted transaction (a rollback) is handled before its rows cascade
-- (`sw_keeper_failures_tx_del`); its invocations then find no transaction here.
CREATE FUNCTION sw_keeper_failures_del() RETURNS trigger LANGUAGE plpgsql
    SET enable_seqscan = off AS $$
DECLARE
    r RECORD;
    t RECORD;
    n BIGINT;
BEGIN
    FOR r IN SELECT DISTINCT signature, name FROM old_rows
             WHERE failed AND sw_keeper_attempt(name)
    LOOP
        SELECT fee_payer, err_code INTO t
        FROM ingest_txs WHERE signature = r.signature AND err IS NOT NULL;
        CONTINUE WHEN NOT FOUND;
        SELECT count(*) FILTER (WHERE name = r.name) INTO n
        FROM invocations WHERE signature = r.signature;
        CONTINUE WHEN n > 0;
        UPDATE keeper_failure_counts SET failures = failures - 1
        WHERE keeper = t.fee_payer AND instruction = r.name
          AND err_code = COALESCE(t.err_code, -1);
        DELETE FROM keeper_failure_counts
        WHERE keeper = t.fee_payer AND instruction = r.name
          AND err_code = COALESCE(t.err_code, -1) AND failures <= 0;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION sw_keeper_failures_tx_del() RETURNS trigger LANGUAGE plpgsql
    SET enable_seqscan = off AS $$
DECLARE
    names TEXT[];
BEGIN
    IF OLD.err IS NOT NULL THEN
        SELECT array_agg(DISTINCT name) FILTER (WHERE failed AND sw_keeper_attempt(name))
        INTO names FROM invocations WHERE signature = OLD.signature;
        UPDATE keeper_failure_counts SET failures = failures - 1
        WHERE keeper = OLD.fee_payer AND instruction = ANY(names)
          AND err_code = COALESCE(OLD.err_code, -1);
        DELETE FROM keeper_failure_counts WHERE keeper = OLD.fee_payer AND failures <= 0;
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER invocations_keeper_failures_ins AFTER INSERT ON invocations
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION sw_keeper_failures_ins();
CREATE TRIGGER invocations_keeper_failures_del AFTER DELETE ON invocations
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION sw_keeper_failures_del();
CREATE TRIGGER ingest_txs_keeper_failures BEFORE DELETE ON ingest_txs
    FOR EACH ROW EXECUTE FUNCTION sw_keeper_failures_tx_del();

-- Time-bounded reads of `invocations` (the 24 h window's partial hour, the rollups' hours, the
-- retention prune) and of `venue_trades` (the rollups' hours).
CREATE INDEX invocations_market_time ON invocations (market, block_time) WHERE market IS NOT NULL;
CREATE INDEX invocations_time ON invocations (block_time);
CREATE INDEX venue_trades_ts ON venue_trades (ts) WHERE market IS NOT NULL;

-- Creator exposure (SPEC R1) and creator-fee income from short.win's own volume (C0-18; §8.3).
-- The coin creator's share of S converts its shares to the market's current epoch first (§6.3).
-- The venue volumes come from `market_trade_totals` (M8-SEC-3).
CREATE VIEW v_market_creator AS
SELECT m.market,
       m.coin_creator,
       COALESCE(lp.shares, 0)                                  AS coin_creator_shares,
       COALESCE(lp.deposited_tokens, 0)                        AS coin_creator_deposited_tokens,
       CASE WHEN m.total_shares > 0
            THEN div(COALESCE(sw_shares_at_epoch(m.market, lp.shares, lp.epoch, m.share_epoch), 0)
                     * 10000, m.total_shares) END              AS coin_creator_share_bps,
       COALESCE(cf.shortwin_creator_fees, 0)                   AS creator_fee_income_lamports,
       COALESCE(cf.shortwin_volume, 0)                         AS shortwin_volume_lamports,
       COALESCE(cf.all_creator_fees, 0)                        AS venue_creator_fees_lamports,
       m.creator                                               AS listed_by
FROM markets m
LEFT JOIN lender_positions lp
       ON lp.market = m.market AND lp.owner = m.coin_creator
LEFT JOIN market_trade_totals cf
       ON cf.market = m.market;

-- Coverage-pause intervals (O17): a `CoveragePauseSet` that sets the flag → the next one of the
-- market that clears it (ongoing until now). Repeated sets or clears are not transitions.
CREATE VIEW v_coverage_pauses AS
WITH ordered AS (
    SELECT market, ts, paused, by_key, slot, tx_index, ix_index, ev_source, ev_seq,
           lag(paused) OVER w AS prev_paused
    FROM coverage_events
    WHERE kind = 'pause_set'
    WINDOW w AS (PARTITION BY market ORDER BY slot, tx_index, ix_index, ev_source, ev_seq)
), transitions AS (
    SELECT market, ts, paused, by_key,
           lead(ts) OVER (PARTITION BY market
                          ORDER BY slot, tx_index, ix_index, ev_source, ev_seq) AS next_ts
    FROM ordered
    WHERE prev_paused IS DISTINCT FROM paused
)
SELECT market,
       ts                                                                  AS paused_at,
       COALESCE(next_ts, greatest(ts, sw_chain_now()))                     AS resumed_at,
       COALESCE(next_ts, greatest(ts, sw_chain_now())) - ts                AS paused_s,
       next_ts IS NULL                                                     AS ongoing,
       by_key
FROM transitions
WHERE paused;

-- Keeper coverage (§8.5) of one market: the program's own gap signal, pauses, and the
-- observations sent in the last 24 h of chain time. Every read is bound to the market (indexes
-- `coverage_events_market_ts`, `invocations_market_time`, `market_observations_hourly`), so
-- a market's page costs the same whatever the other markets' (or the attackers') rows
-- (M8-SEC-3): the 24 h are the complete hours from the counters plus the window's first, partial
-- hour from the rows.
CREATE FUNCTION sw_market_coverage(p_market TEXT)
    RETURNS TABLE (market pubkey, gaps BIGINT, gap_s_total NUMERIC, gap_s_max BIGINT,
                   gaps_24h BIGINT, gap_s_24h NUMERIC, coverage_pauses BIGINT,
                   coverage_paused_s NUMERIC, paused_coverage BOOLEAN, observations_24h BIGINT,
                   failed_observations_24h BIGINT)
    LANGUAGE SQL STABLE AS $$
    SELECT m.market,
           g.gaps, g.gap_s_total, g.gap_s_max, g.gaps_24h, g.gap_s_24h,
           p.pauses, p.paused_s, m.paused_coverage,
           (COALESCE(h.obs, 0) + e.obs)::BIGINT,
           (COALESCE(h.failed, 0) + e.failed)::BIGINT
    FROM markets m
    CROSS JOIN (SELECT sw_chain_now() - 86400 AS since) w
    CROSS JOIN LATERAL (SELECT ((w.since + 3599) / 3600) * 3600 AS full_from) b
    CROSS JOIN LATERAL (
        SELECT count(*)                                         AS gaps,
               COALESCE(sum(c.gap_s), 0)                        AS gap_s_total,
               max(c.gap_s)                                     AS gap_s_max,
               count(*) FILTER (WHERE c.ts >= w.since)          AS gaps_24h,
               COALESCE(sum(c.gap_s) FILTER (WHERE c.ts >= w.since), 0) AS gap_s_24h
        FROM coverage_events c WHERE c.market = m.market AND c.kind = 'gap') g
    CROSS JOIN LATERAL (
        SELECT count(*) AS pauses, COALESCE(sum(vp.paused_s), 0) AS paused_s
        FROM v_coverage_pauses vp WHERE vp.market = m.market) p
    CROSS JOIN LATERAL (
        SELECT sum(o.observations) AS obs, sum(o.failed) AS failed
        FROM market_observations_hourly o
        WHERE o.market = m.market AND o.bucket_start >= b.full_from) h
    CROSS JOIN LATERAL (
        SELECT count(*) FILTER (WHERE NOT i.failed) AS obs, count(*) FILTER (WHERE i.failed) AS failed
        FROM invocations i
        WHERE i.market = m.market AND i.block_time >= w.since AND i.block_time < b.full_from
          AND sw_observing(i.name)) e
    WHERE m.market = p_market
$$;

-- Every market's coverage (SQL readers; the API calls `sw_market_coverage` for one market).
CREATE VIEW v_market_coverage AS
SELECT c.*
FROM markets m
CROSS JOIN LATERAL sw_market_coverage(m.market) c;

-- Keeper activity by wallet and market (§8.5): exits from the positions the keeper closed (a
-- liquidation with a zero keeper penalty still counts), rewards from the keeper fee legs.
CREATE VIEW v_keeper_activity AS
WITH exits AS (
    SELECT exit_keeper AS keeper,
           market,
           count(*) FILTER (WHERE status = 'liquidated') AS liquidations,
           count(*) FILTER (WHERE status = 'expired')    AS expiries,
           count(*) FILTER (WHERE status = 'settled')    AS settlements
    FROM positions
    WHERE exit_keeper IS NOT NULL
    GROUP BY exit_keeper, market
), rewards AS (
    SELECT recipient_key AS keeper, market, sum(amount) AS rewards_lamports
    FROM fees
    WHERE recipient = 'keeper' AND recipient_key IS NOT NULL
    GROUP BY recipient_key, market
)
SELECT COALESCE(e.keeper, r.keeper)          AS keeper,
       COALESCE(e.market, r.market)          AS market,
       COALESCE(e.liquidations, 0)           AS liquidations,
       COALESCE(e.expiries, 0)               AS expiries,
       COALESCE(e.settlements, 0)            AS settlements,
       COALESCE(r.rewards_lamports, 0)       AS rewards_lamports
FROM exits e
FULL JOIN rewards r ON r.keeper = e.keeper AND r.market = e.market;

-- Failed keeper attempts by wallet, instruction and program error (§8.5: `UseSolSettle`,
-- `ExitBoundNotMet`, `EmaWarmingUp`, …), from the rows. The API reads the same counts from
-- `keeper_failure_counts` (M8-SEC-3; tests hold them equal).
CREATE VIEW v_keeper_failures AS
SELECT t.fee_payer                AS keeper,
       i.name                     AS instruction,
       t.err_code,
       count(DISTINCT t.signature) AS failures,
       max(t.block_time)          AS last_ts
FROM ingest_txs t
JOIN invocations i ON i.signature = t.signature
WHERE t.err IS NOT NULL
  AND sw_keeper_attempt(i.name)
GROUP BY t.fee_payer, i.name, t.err_code;

-- ---- global, current (§8.7) -------------------------------------------------------------------

CREATE VIEW v_global_stats AS
SELECT (SELECT count(*) FROM markets)                                     AS markets,
       (SELECT count(*) FROM markets WHERE open_positions > 0)            AS markets_with_positions,
       (SELECT COALESCE(sum(tvl_lamports), 0) FROM v_market_overview)     AS tvl_lamports,
       (SELECT COALESCE(sum(oi_lamports), 0) FROM v_market_overview)      AS oi_lamports,
       (SELECT COALESCE(sum(total_notional_open), 0) FROM markets)        AS notional_open_lamports,
       (SELECT COALESCE(sum(open_positions), 0) FROM markets)             AS open_positions,
       (SELECT COALESCE(sum(lender_sol), 0) FROM markets)                 AS lender_sol,
       (SELECT COALESCE(sum(protocol_fees), 0) FROM markets)              AS protocol_fees_unswept,
       (SELECT COALESCE(sum(amount), 0) FROM fees WHERE recipient = 'protocol') AS protocol_revenue,
       (SELECT COALESCE(sum(amount), 0) FROM fees WHERE recipient = 'lenders')  AS lender_fee_income,
       (SELECT COALESCE(sum(amount), 0) FROM fees WHERE recipient = 'keeper')   AS keeper_income,
       (SELECT count(*) FROM positions WHERE status = 'closed')           AS closes,
       (SELECT count(*) FROM positions WHERE status = 'liquidated')       AS liquidations,
       (SELECT count(*) FROM positions WHERE status = 'expired')          AS expiries,
       (SELECT count(*) FROM settlements WHERE condition = 1)             AS settlements_s1,
       (SELECT count(*) FROM settlements WHERE condition = 2)             AS settlements_s2,
       (SELECT count(*) FROM settlements WHERE condition = 3)             AS settlements_s3,
       (SELECT COALESCE(sum(realised_lender_loss), 0) FROM settlements)   AS realised_lender_loss,
       (SELECT COALESCE(sum(lender_pnl), 0) FROM settlements)             AS settlement_lender_pnl,
       (SELECT count(DISTINCT owner) FROM lender_positions)               AS unique_lenders,
       (SELECT count(DISTINCT borrower) FROM positions)                   AS unique_borrowers,
       (SELECT count(*) FROM (SELECT owner FROM lender_positions
                              GROUP BY owner HAVING sum(deposits) >= 2) r) AS repeat_lenders,
       (SELECT count(*) FROM (SELECT borrower FROM positions
                              GROUP BY borrower HAVING count(*) >= 2) r)   AS repeat_borrowers,
       (SELECT avg(duration_s) FROM positions WHERE status <> 'open')     AS avg_duration_s,
       (SELECT COALESCE(sum(gap_s), 0) FROM coverage_events WHERE kind = 'gap') AS coverage_gap_s,
       (SELECT COALESCE(sum(creator_fee_income_lamports), 0) FROM v_market_creator) AS creator_fee_income,
       (SELECT COALESCE(sum(inventory_value_lamports), 0) FROM v_market_overview) AS inventory_value_lamports,
       -- Global utilisation (§8.7): OI at mark / Σ inventory at mark, bps (rounded up as the
       -- program's per-market utilisation); NULL without a priced inventory.
       (SELECT CASE WHEN sum(inventory_value_lamports) > 0
                    THEN ceil(sum(oi_lamports) * 10000 / sum(inventory_value_lamports))::BIGINT END
        FROM v_market_overview)                                           AS util_bps,
       (SELECT COALESCE(sum(amount), 0) FROM fees
        WHERE fee_type IN ('rent_lender', 'rent_protocol'))               AS rental_fees,
       (SELECT avg(sw_open_impact_bps(q, spot_wad_open, s)) FROM positions) AS avg_open_impact_bps,
       (SELECT avg(sw_exit_slippage_bps(buyback_cost, b_live_exit)) FROM positions
        WHERE status IN ('closed', 'liquidated', 'expired'))              AS avg_exit_slippage_bps;

-- ---- time series (§8.6): filled by the analytics job ------------------------------------------

-- One row per market and hour (bucket_start = ⌊ts/3600⌋·3600, chain time). State columns are the
-- market's state after the last event at or before the bucket's end (carried forward when the
-- hour had no event); flow columns sum the hour's events.
CREATE TABLE market_stats_hourly (
    market                 pubkey      NOT NULL,
    bucket_start           BIGINT      NOT NULL CHECK (bucket_start % 3600 = 0),
    -- state at the end of the bucket
    tokens_free            u64         NOT NULL,
    borrowed_q             u64         NOT NULL,
    total_shares           u128        NOT NULL,
    open_positions         u32         NOT NULL,
    total_notional_open    u64         NOT NULL,
    open_vault_lamports    u64         NOT NULL,
    lender_sol             u64         NOT NULL,
    ema_price              u128,
    util_bps               u32         NOT NULL,
    oi_lamports            NUMERIC(40, 0) NOT NULL,
    tvl_lamports           NUMERIC(40, 0) NOT NULL,
    -- ⌊(T + ΣB_Q)·P_ema/WAD⌋: the global utilisation's denominator (§8.7).
    inventory_value_lamports NUMERIC(40, 0) NOT NULL DEFAULT 0,
    -- flows within the bucket
    fees_protocol          u64         NOT NULL DEFAULT 0,
    fees_lenders           u64         NOT NULL DEFAULT 0,
    fees_keeper            u64         NOT NULL DEFAULT 0,
    opens                  u32         NOT NULL DEFAULT 0,
    closes                 u32         NOT NULL DEFAULT 0,
    liquidations           u32         NOT NULL DEFAULT 0,
    expiries               u32         NOT NULL DEFAULT 0,
    settlements_s1         u32         NOT NULL DEFAULT 0,
    settlements_s2         u32         NOT NULL DEFAULT 0,
    settlements_s3         u32         NOT NULL DEFAULT 0,
    realised_lender_loss   u64         NOT NULL DEFAULT 0,
    settlement_lender_pnl  i128        NOT NULL DEFAULT 0,
    notional_opened        u64         NOT NULL DEFAULT 0,
    shortwin_volume        u64         NOT NULL DEFAULT 0,
    creator_fees           u64         NOT NULL DEFAULT 0,
    deposits               u32         NOT NULL DEFAULT 0,
    withdrawals            u32         NOT NULL DEFAULT 0,
    coverage_gaps          u32         NOT NULL DEFAULT 0,
    coverage_gap_s         BIGINT      NOT NULL DEFAULT 0,
    -- seconds of the bucket with open_positions > 0, and with the coverage pause set
    positions_open_s       BIGINT      NOT NULL DEFAULT 0,
    coverage_paused_s      BIGINT      NOT NULL DEFAULT 0,
    -- successful observing invocations naming the market (§8.5)
    observations           u32         NOT NULL DEFAULT 0,
    -- the largest interval between consecutive successful observations that ends in the bucket,
    -- positions open at its start (NULL: none)
    max_observation_gap_s  BIGINT,
    computed_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (market, bucket_start)
);
CREATE INDEX market_stats_hourly_bucket ON market_stats_hourly (bucket_start);

-- The protocol-wide hour: sums of market_stats_hourly (state columns summed across markets).
CREATE TABLE global_stats_hourly (
    bucket_start           BIGINT      PRIMARY KEY CHECK (bucket_start % 3600 = 0),
    markets                u32         NOT NULL,
    open_positions         BIGINT      NOT NULL,
    total_notional_open    NUMERIC(40, 0) NOT NULL,
    oi_lamports            NUMERIC(40, 0) NOT NULL,
    tvl_lamports           NUMERIC(40, 0) NOT NULL,
    lender_sol             NUMERIC(40, 0) NOT NULL,
    inventory_value_lamports NUMERIC(40, 0) NOT NULL DEFAULT 0,
    -- ⌈OI·10⁴ / inventory at mark⌉ (§8.7); NULL without a priced inventory.
    util_bps               u32,
    fees_protocol          NUMERIC(40, 0) NOT NULL,
    fees_lenders           NUMERIC(40, 0) NOT NULL,
    fees_keeper            NUMERIC(40, 0) NOT NULL,
    opens                  BIGINT      NOT NULL,
    closes                 BIGINT      NOT NULL,
    liquidations           BIGINT      NOT NULL,
    expiries               BIGINT      NOT NULL,
    settlements_s1         BIGINT      NOT NULL,
    settlements_s2         BIGINT      NOT NULL,
    settlements_s3         BIGINT      NOT NULL,
    realised_lender_loss   NUMERIC(40, 0) NOT NULL,
    settlement_lender_pnl  NUMERIC(40, 0) NOT NULL,
    notional_opened        NUMERIC(40, 0) NOT NULL,
    shortwin_volume        NUMERIC(40, 0) NOT NULL,
    creator_fees           NUMERIC(40, 0) NOT NULL,
    deposits               BIGINT      NOT NULL DEFAULT 0,
    withdrawals            BIGINT      NOT NULL DEFAULT 0,
    coverage_gaps          BIGINT      NOT NULL,
    coverage_gap_s         BIGINT      NOT NULL,
    positions_open_s       BIGINT      NOT NULL DEFAULT 0,
    coverage_paused_s      BIGINT      NOT NULL DEFAULT 0,
    observations           BIGINT      NOT NULL DEFAULT 0,
    computed_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The rollup job's progress (§8.6): every bucket before `complete_before` is final.
-- `last_event_id`: the newest `events.id` the last pass saw; `last_tx_seq`: the newest
-- `ingest_txs.seq` (0101). A transaction committed later with a block time before
-- `complete_before` (a gap-fill, a late confirmation) moves it back — whether or not it carries
-- an event: a no-op observation, a failed keeper attempt or a third-party trade changes its
-- hour too (M8-SEC-14).
CREATE TABLE rollup_state (
    name             TEXT        PRIMARY KEY,
    complete_before  BIGINT      NOT NULL DEFAULT 0,
    last_event_id    BIGINT      NOT NULL DEFAULT 0,
    last_tx_seq      BIGINT      NOT NULL DEFAULT 0,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO rollup_state (name) VALUES ('market_stats_hourly'), ('global_stats_hourly');

-- Fills `market_stats_hourly` for every market and every hour [h, h + 3600) with
-- p_from ≤ h < p_to (both multiples of 3600), from the market's listing hour on; returns the
-- market-hours written. Idempotent (ON CONFLICT DO UPDATE). INDEXER.md §8.6; Rust caller
-- `analytics::rollups::run_once`.
CREATE FUNCTION sw_rollup_market_hours(p_from BIGINT, p_to BIGINT) RETURNS BIGINT
    LANGUAGE plpgsql AS $$
DECLARE
    n BIGINT;
BEGIN
    IF p_to <= p_from THEN
        RETURN 0;
    END IF;
    WITH grid AS (
        SELECT m.market, b.bucket_start
        FROM markets m
        CROSS JOIN LATERAL generate_series(
            greatest(p_from, (m.created_ts / 3600) * 3600), p_to - 3600, 3600::BIGINT
        ) AS b(bucket_start)
    ),
    -- The market's state after the last accounting event before the bucket's end.
    state AS (
        SELECT g.market, g.bucket_start, s.tokens_free, s.borrowed_q, s.total_shares,
               s.open_positions, s.total_notional_open, s.open_vault_lamports, s.lender_sol
        FROM grid g
        LEFT JOIN LATERAL (
            SELECT l.tokens_free, l.borrowed_q, l.total_shares, l.open_positions,
                   l.total_notional_open, l.open_vault_lamports, l.lender_sol
            FROM market_state_log l
            WHERE l.market = g.market AND l.ts < g.bucket_start + 3600
            ORDER BY l.ts DESC, l.slot DESC, l.tx_index DESC, l.ix_index DESC,
                     l.ev_source DESC, l.ev_seq DESC
            LIMIT 1) s ON TRUE
    ),
    -- The P_ema in force at the bucket's end: the last PriceCranked or Open before it.
    price AS (
        SELECT g.market, g.bucket_start, p.ema
        FROM grid g
        LEFT JOIN LATERAL (
            SELECT c.ema FROM (
                (SELECT ms.ema_price AS ema, ms.ts, ms.slot, ms.tx_index, 0 AS prio
                 FROM market_snapshots ms
                 WHERE ms.market = g.market AND ms.ts < g.bucket_start + 3600 AND ms.ema_price > 0
                 ORDER BY ms.ts DESC, ms.slot DESC, ms.tx_index DESC, ms.ix_index DESC,
                          ms.ev_seq DESC
                 LIMIT 1)
                UNION ALL
                (SELECT po.ema_wad_open, po.opened_ts, po.opened_slot, po.opened_tx_index, 1
                 FROM positions po
                 WHERE po.market = g.market AND po.opened_ts < g.bucket_start + 3600
                   AND po.ema_wad_open > 0
                 ORDER BY po.opened_ts DESC, po.opened_slot DESC, po.opened_tx_index DESC
                 LIMIT 1)
            ) c
            ORDER BY c.ts DESC, c.slot DESC, c.tx_index DESC, c.prio DESC
            LIMIT 1) p ON TRUE
    ),
    fee_f AS (
        SELECT market, (ts / 3600) * 3600 AS bucket_start,
               sum(amount) FILTER (WHERE recipient = 'protocol') AS fees_protocol,
               sum(amount) FILTER (WHERE recipient = 'lenders')  AS fees_lenders,
               sum(amount) FILTER (WHERE recipient = 'keeper')   AS fees_keeper
        FROM fees WHERE ts >= p_from AND ts < p_to GROUP BY 1, 2
    ),
    open_f AS (
        SELECT market, (opened_ts / 3600) * 3600 AS bucket_start,
               count(*) AS opens, sum(n_open) AS notional_opened
        FROM positions WHERE opened_ts >= p_from AND opened_ts < p_to GROUP BY 1, 2
    ),
    exit_f AS (
        SELECT market, (exit_ts / 3600) * 3600 AS bucket_start,
               count(*) FILTER (WHERE status = 'closed')     AS closes,
               count(*) FILTER (WHERE status = 'liquidated') AS liquidations,
               count(*) FILTER (WHERE status = 'expired')    AS expiries
        FROM positions WHERE exit_ts >= p_from AND exit_ts < p_to GROUP BY 1, 2
    ),
    settle_f AS (
        SELECT market, (ts / 3600) * 3600 AS bucket_start,
               count(*) FILTER (WHERE condition = 1) AS s1,
               count(*) FILTER (WHERE condition = 2) AS s2,
               count(*) FILTER (WHERE condition = 3) AS s3,
               sum(realised_lender_loss)             AS loss,
               sum(lender_pnl)                       AS pnl
        FROM settlements WHERE ts >= p_from AND ts < p_to GROUP BY 1, 2
    ),
    trade_f AS (
        SELECT market, (ts / 3600) * 3600 AS bucket_start,
               sum(sol_amount) FILTER (WHERE is_shortwin)           AS volume,
               sum(fee_creator_lamports) FILTER (WHERE is_shortwin) AS creator_fees
        FROM venue_trades
        WHERE market IS NOT NULL AND ts >= p_from AND ts < p_to GROUP BY 1, 2
    ),
    lender_f AS (
        SELECT market, (ev_ts / 3600) * 3600 AS bucket_start,
               count(*) FILTER (WHERE kind = 'Deposit')  AS deposits,
               count(*) FILTER (WHERE kind = 'Withdraw') AS withdrawals
        FROM events
        WHERE kind IN ('Deposit', 'Withdraw') AND market IS NOT NULL
          AND ev_ts >= p_from AND ev_ts < p_to
        GROUP BY 1, 2
    ),
    gap_f AS (
        SELECT market, (ts / 3600) * 3600 AS bucket_start,
               count(*) AS gaps, sum(gap_s) AS gap_s
        FROM coverage_events
        WHERE kind = 'gap' AND ts >= p_from AND ts < p_to GROUP BY 1, 2
    ),
    obs_f AS (
        SELECT market, (block_time / 3600) * 3600 AS bucket_start, count(*) AS observations
        FROM invocations
        WHERE market IS NOT NULL AND NOT failed AND sw_observing(name)
          AND block_time >= p_from AND block_time < p_to
        GROUP BY 1, 2
    ),
    -- open_positions > 0 intervals [t0, t1) overlapping [p_from, p_to): the log rows in the
    -- range plus each market's last row before it.
    log_rows AS (
        SELECT l.market, l.ts, l.open_positions, l.slot, l.tx_index, l.ix_index, l.ev_source,
               l.ev_seq
        FROM market_state_log l
        WHERE l.ts >= p_from AND l.ts < p_to
        UNION ALL
        SELECT m.market, b.ts, b.open_positions, b.slot, b.tx_index, b.ix_index, b.ev_source,
               b.ev_seq
        FROM markets m
        CROSS JOIN LATERAL (
            SELECT l.ts, l.open_positions, l.slot, l.tx_index, l.ix_index, l.ev_source, l.ev_seq
            FROM market_state_log l
            WHERE l.market = m.market AND l.ts < p_from
            ORDER BY l.ts DESC, l.slot DESC, l.tx_index DESC, l.ix_index DESC,
                     l.ev_source DESC, l.ev_seq DESC
            LIMIT 1) b
    ),
    open_iv AS (
        SELECT market, t0, COALESCE(t1, p_to) AS t1
        FROM (SELECT market, ts AS t0, open_positions,
                     lead(ts) OVER (PARTITION BY market
                                    ORDER BY slot, tx_index, ix_index, ev_source, ev_seq) AS t1
              FROM log_rows) x
        WHERE open_positions > 0
    ),
    open_s AS (
        SELECT g.market, g.bucket_start,
               sum(greatest(0, least(iv.t1, g.bucket_start + 3600)
                               - greatest(iv.t0, g.bucket_start))) AS positions_open_s
        FROM grid g
        JOIN open_iv iv ON iv.market = g.market
                       AND iv.t0 < g.bucket_start + 3600 AND iv.t1 > g.bucket_start
        GROUP BY 1, 2
    ),
    -- coverage-paused intervals, the same way from the pause_set events.
    pause_rows AS (
        SELECT c.market, c.ts, c.paused, c.slot, c.tx_index, c.ix_index, c.ev_source, c.ev_seq
        FROM coverage_events c
        WHERE c.kind = 'pause_set' AND c.ts >= p_from AND c.ts < p_to
        UNION ALL
        SELECT m.market, b.ts, b.paused, b.slot, b.tx_index, b.ix_index, b.ev_source, b.ev_seq
        FROM markets m
        CROSS JOIN LATERAL (
            SELECT c.ts, c.paused, c.slot, c.tx_index, c.ix_index, c.ev_source, c.ev_seq
            FROM coverage_events c
            WHERE c.market = m.market AND c.kind = 'pause_set' AND c.ts < p_from
            ORDER BY c.ts DESC, c.slot DESC, c.tx_index DESC, c.ix_index DESC,
                     c.ev_source DESC, c.ev_seq DESC
            LIMIT 1) b
    ),
    pause_iv AS (
        SELECT market, t0, COALESCE(t1, p_to) AS t1
        FROM (SELECT market, ts AS t0, paused,
                     lead(ts) OVER (PARTITION BY market
                                    ORDER BY slot, tx_index, ix_index, ev_source, ev_seq) AS t1
              FROM pause_rows) x
        WHERE paused
    ),
    pause_s AS (
        SELECT g.market, g.bucket_start,
               sum(greatest(0, least(iv.t1, g.bucket_start + 3600)
                               - greatest(iv.t0, g.bucket_start))) AS coverage_paused_s
        FROM grid g
        JOIN pause_iv iv ON iv.market = g.market
                        AND iv.t0 < g.bucket_start + 3600 AND iv.t1 > g.bucket_start
        GROUP BY 1, 2
    ),
    -- Consecutive successful observations (a week of look-back for the first one's
    -- predecessor), each gap attributed to the hour of its later observation when positions
    -- were open at its earlier one.
    obs AS (
        SELECT market, block_time AS t,
               lag(block_time) OVER (PARTITION BY market
                                     ORDER BY slot, tx_index, ix_index, inner_index) AS prev
        FROM invocations
        WHERE market IS NOT NULL AND NOT failed AND sw_observing(name)
          AND block_time IS NOT NULL
          AND block_time < p_to AND block_time >= p_from - 7 * 86400
    ),
    obs_gap AS (
        SELECT o.market, (o.t / 3600) * 3600 AS bucket_start, max(o.t - o.prev) AS gap
        FROM obs o
        WHERE o.t >= p_from AND o.prev IS NOT NULL
          AND EXISTS (SELECT 1 FROM open_iv iv
                      WHERE iv.market = o.market AND iv.t0 <= o.prev AND iv.t1 > o.prev)
        GROUP BY 1, 2
    )
    INSERT INTO market_stats_hourly AS h (
        market, bucket_start, tokens_free, borrowed_q, total_shares, open_positions,
        total_notional_open, open_vault_lamports, lender_sol, ema_price, util_bps, oi_lamports,
        tvl_lamports, inventory_value_lamports, fees_protocol, fees_lenders, fees_keeper, opens,
        closes, liquidations, expiries, settlements_s1, settlements_s2, settlements_s3,
        realised_lender_loss, settlement_lender_pnl, notional_opened, shortwin_volume,
        creator_fees, deposits, withdrawals, coverage_gaps, coverage_gap_s, positions_open_s,
        coverage_paused_s, observations, max_observation_gap_s, computed_at)
    SELECT g.market, g.bucket_start,
           COALESCE(s.tokens_free, 0), COALESCE(s.borrowed_q, 0), COALESCE(s.total_shares, 0),
           COALESCE(s.open_positions, 0), COALESCE(s.total_notional_open, 0),
           COALESCE(s.open_vault_lamports, 0), COALESCE(s.lender_sol, 0),
           p.ema,
           sw_util_bps(COALESCE(s.tokens_free, 0), COALESCE(s.borrowed_q, 0)),
           sw_mul_wad(COALESCE(s.borrowed_q, 0), COALESCE(p.ema, 0)),
           sw_mul_wad(COALESCE(s.tokens_free, 0), COALESCE(p.ema, 0))
               + COALESCE(s.lender_sol, 0) + COALESCE(s.open_vault_lamports, 0),
           sw_mul_wad(COALESCE(s.tokens_free, 0) + COALESCE(s.borrowed_q, 0), COALESCE(p.ema, 0)),
           COALESCE(f.fees_protocol, 0), COALESCE(f.fees_lenders, 0), COALESCE(f.fees_keeper, 0),
           COALESCE(o.opens, 0), COALESCE(x.closes, 0), COALESCE(x.liquidations, 0),
           COALESCE(x.expiries, 0), COALESCE(st.s1, 0), COALESCE(st.s2, 0), COALESCE(st.s3, 0),
           COALESCE(st.loss, 0), COALESCE(st.pnl, 0), COALESCE(o.notional_opened, 0),
           COALESCE(t.volume, 0), COALESCE(t.creator_fees, 0), COALESCE(lf.deposits, 0),
           COALESCE(lf.withdrawals, 0), COALESCE(gp.gaps, 0), COALESCE(gp.gap_s, 0),
           COALESCE(os.positions_open_s, 0), COALESCE(ps.coverage_paused_s, 0),
           COALESCE(ob.observations, 0), og.gap, now()
    FROM grid g
    LEFT JOIN state s     ON s.market = g.market AND s.bucket_start = g.bucket_start
    LEFT JOIN price p     ON p.market = g.market AND p.bucket_start = g.bucket_start
    LEFT JOIN fee_f f     ON f.market = g.market AND f.bucket_start = g.bucket_start
    LEFT JOIN open_f o    ON o.market = g.market AND o.bucket_start = g.bucket_start
    LEFT JOIN exit_f x    ON x.market = g.market AND x.bucket_start = g.bucket_start
    LEFT JOIN settle_f st ON st.market = g.market AND st.bucket_start = g.bucket_start
    LEFT JOIN trade_f t   ON t.market = g.market AND t.bucket_start = g.bucket_start
    LEFT JOIN lender_f lf ON lf.market = g.market AND lf.bucket_start = g.bucket_start
    LEFT JOIN gap_f gp    ON gp.market = g.market AND gp.bucket_start = g.bucket_start
    LEFT JOIN obs_f ob    ON ob.market = g.market AND ob.bucket_start = g.bucket_start
    LEFT JOIN open_s os   ON os.market = g.market AND os.bucket_start = g.bucket_start
    LEFT JOIN pause_s ps  ON ps.market = g.market AND ps.bucket_start = g.bucket_start
    LEFT JOIN obs_gap og  ON og.market = g.market AND og.bucket_start = g.bucket_start
    ON CONFLICT (market, bucket_start) DO UPDATE SET
        tokens_free = EXCLUDED.tokens_free, borrowed_q = EXCLUDED.borrowed_q,
        total_shares = EXCLUDED.total_shares, open_positions = EXCLUDED.open_positions,
        total_notional_open = EXCLUDED.total_notional_open,
        open_vault_lamports = EXCLUDED.open_vault_lamports, lender_sol = EXCLUDED.lender_sol,
        ema_price = EXCLUDED.ema_price, util_bps = EXCLUDED.util_bps,
        oi_lamports = EXCLUDED.oi_lamports, tvl_lamports = EXCLUDED.tvl_lamports,
        inventory_value_lamports = EXCLUDED.inventory_value_lamports,
        fees_protocol = EXCLUDED.fees_protocol, fees_lenders = EXCLUDED.fees_lenders,
        fees_keeper = EXCLUDED.fees_keeper, opens = EXCLUDED.opens, closes = EXCLUDED.closes,
        liquidations = EXCLUDED.liquidations, expiries = EXCLUDED.expiries,
        settlements_s1 = EXCLUDED.settlements_s1, settlements_s2 = EXCLUDED.settlements_s2,
        settlements_s3 = EXCLUDED.settlements_s3,
        realised_lender_loss = EXCLUDED.realised_lender_loss,
        settlement_lender_pnl = EXCLUDED.settlement_lender_pnl,
        notional_opened = EXCLUDED.notional_opened, shortwin_volume = EXCLUDED.shortwin_volume,
        creator_fees = EXCLUDED.creator_fees, deposits = EXCLUDED.deposits,
        withdrawals = EXCLUDED.withdrawals, coverage_gaps = EXCLUDED.coverage_gaps,
        coverage_gap_s = EXCLUDED.coverage_gap_s, positions_open_s = EXCLUDED.positions_open_s,
        coverage_paused_s = EXCLUDED.coverage_paused_s, observations = EXCLUDED.observations,
        max_observation_gap_s = EXCLUDED.max_observation_gap_s,
        computed_at = EXCLUDED.computed_at;
    GET DIAGNOSTICS n = ROW_COUNT;
    -- A market rebuilt or rolled back to before its first hour leaves no stale rows behind.
    DELETE FROM market_stats_hourly h
    WHERE h.bucket_start >= p_from AND h.bucket_start < p_to
      AND NOT EXISTS (SELECT 1 FROM markets m
                      WHERE m.market = h.market AND (m.created_ts / 3600) * 3600 <= h.bucket_start);
    RETURN n;
END
$$;

-- Fills `global_stats_hourly` for the hours [p_from, p_to) from `market_stats_hourly`; returns
-- the hours written.
CREATE FUNCTION sw_rollup_global_hours(p_from BIGINT, p_to BIGINT) RETURNS BIGINT
    LANGUAGE plpgsql AS $$
DECLARE
    n BIGINT;
BEGIN
    IF p_to <= p_from THEN
        RETURN 0;
    END IF;
    INSERT INTO global_stats_hourly AS g (
        bucket_start, markets, open_positions, total_notional_open, oi_lamports, tvl_lamports,
        lender_sol, inventory_value_lamports, util_bps, fees_protocol, fees_lenders, fees_keeper,
        opens, closes, liquidations, expiries, settlements_s1, settlements_s2, settlements_s3,
        realised_lender_loss, settlement_lender_pnl, notional_opened, shortwin_volume,
        creator_fees, deposits, withdrawals, coverage_gaps, coverage_gap_s, positions_open_s,
        coverage_paused_s, observations, computed_at)
    SELECT bucket_start, count(*), sum(open_positions), sum(total_notional_open),
           sum(oi_lamports), sum(tvl_lamports), sum(lender_sol), sum(inventory_value_lamports),
           CASE WHEN sum(inventory_value_lamports) > 0
                THEN ceil(sum(oi_lamports) * 10000 / sum(inventory_value_lamports))::BIGINT END,
           sum(fees_protocol), sum(fees_lenders), sum(fees_keeper), sum(opens), sum(closes),
           sum(liquidations), sum(expiries), sum(settlements_s1), sum(settlements_s2),
           sum(settlements_s3), sum(realised_lender_loss), sum(settlement_lender_pnl),
           sum(notional_opened), sum(shortwin_volume), sum(creator_fees), sum(deposits),
           sum(withdrawals), sum(coverage_gaps), sum(coverage_gap_s), sum(positions_open_s),
           sum(coverage_paused_s), sum(observations), now()
    FROM market_stats_hourly
    WHERE bucket_start >= p_from AND bucket_start < p_to
    GROUP BY bucket_start
    ON CONFLICT (bucket_start) DO UPDATE SET
        markets = EXCLUDED.markets, open_positions = EXCLUDED.open_positions,
        total_notional_open = EXCLUDED.total_notional_open, oi_lamports = EXCLUDED.oi_lamports,
        tvl_lamports = EXCLUDED.tvl_lamports, lender_sol = EXCLUDED.lender_sol,
        inventory_value_lamports = EXCLUDED.inventory_value_lamports,
        util_bps = EXCLUDED.util_bps, fees_protocol = EXCLUDED.fees_protocol,
        fees_lenders = EXCLUDED.fees_lenders, fees_keeper = EXCLUDED.fees_keeper,
        opens = EXCLUDED.opens, closes = EXCLUDED.closes, liquidations = EXCLUDED.liquidations,
        expiries = EXCLUDED.expiries, settlements_s1 = EXCLUDED.settlements_s1,
        settlements_s2 = EXCLUDED.settlements_s2, settlements_s3 = EXCLUDED.settlements_s3,
        realised_lender_loss = EXCLUDED.realised_lender_loss,
        settlement_lender_pnl = EXCLUDED.settlement_lender_pnl,
        notional_opened = EXCLUDED.notional_opened, shortwin_volume = EXCLUDED.shortwin_volume,
        creator_fees = EXCLUDED.creator_fees, deposits = EXCLUDED.deposits,
        withdrawals = EXCLUDED.withdrawals, coverage_gaps = EXCLUDED.coverage_gaps,
        coverage_gap_s = EXCLUDED.coverage_gap_s, positions_open_s = EXCLUDED.positions_open_s,
        coverage_paused_s = EXCLUDED.coverage_paused_s, observations = EXCLUDED.observations,
        computed_at = EXCLUDED.computed_at;
    GET DIAGNOSTICS n = ROW_COUNT;
    DELETE FROM global_stats_hourly g
    WHERE g.bucket_start >= p_from AND g.bucket_start < p_to
      AND NOT EXISTS (SELECT 1 FROM market_stats_hourly h WHERE h.bucket_start = g.bucket_start);
    RETURN n;
END
$$;

-- Daily views of the hourly tables: flows summed, state taken at the day's last hour.
CREATE VIEW v_market_stats_daily AS
SELECT market,
       div(bucket_start, 86400) * 86400                          AS day_start,
       (array_agg(tvl_lamports ORDER BY bucket_start DESC))[1]   AS tvl_lamports,
       (array_agg(oi_lamports ORDER BY bucket_start DESC))[1]    AS oi_lamports,
       (array_agg(util_bps ORDER BY bucket_start DESC))[1]       AS util_bps,
       (array_agg(open_positions ORDER BY bucket_start DESC))[1] AS open_positions,
       sum(fees_protocol)                                        AS fees_protocol,
       sum(fees_lenders)                                         AS fees_lenders,
       sum(fees_keeper)                                          AS fees_keeper,
       sum(opens)                                                AS opens,
       sum(closes)                                               AS closes,
       sum(liquidations)                                         AS liquidations,
       sum(expiries)                                             AS expiries,
       sum(settlements_s1)                                       AS settlements_s1,
       sum(settlements_s2)                                       AS settlements_s2,
       sum(settlements_s3)                                       AS settlements_s3,
       sum(realised_lender_loss)                                 AS realised_lender_loss,
       sum(settlement_lender_pnl)                                AS settlement_lender_pnl,
       sum(notional_opened)                                      AS notional_opened,
       sum(shortwin_volume)                                      AS shortwin_volume,
       sum(creator_fees)                                         AS creator_fees,
       sum(deposits)                                             AS deposits,
       sum(withdrawals)                                          AS withdrawals,
       sum(coverage_gaps)                                        AS coverage_gaps,
       sum(coverage_gap_s)                                       AS coverage_gap_s,
       sum(positions_open_s)                                     AS positions_open_s,
       sum(coverage_paused_s)                                    AS coverage_paused_s,
       sum(observations)                                         AS observations,
       max(max_observation_gap_s)                                AS max_observation_gap_s,
       (array_agg(lender_sol ORDER BY bucket_start DESC))[1]     AS lender_sol,
       (array_agg(inventory_value_lamports ORDER BY bucket_start DESC))[1] AS inventory_value_lamports
FROM market_stats_hourly
GROUP BY market, div(bucket_start, 86400);

CREATE VIEW v_global_stats_daily AS
SELECT div(bucket_start, 86400) * 86400                          AS day_start,
       (array_agg(tvl_lamports ORDER BY bucket_start DESC))[1]   AS tvl_lamports,
       (array_agg(oi_lamports ORDER BY bucket_start DESC))[1]    AS oi_lamports,
       (array_agg(util_bps ORDER BY bucket_start DESC))[1]       AS util_bps,
       (array_agg(open_positions ORDER BY bucket_start DESC))[1] AS open_positions,
       sum(fees_protocol)                                        AS fees_protocol,
       sum(fees_lenders)                                         AS fees_lenders,
       sum(fees_keeper)                                          AS fees_keeper,
       sum(opens)                                                AS opens,
       sum(closes)                                               AS closes,
       sum(liquidations)                                         AS liquidations,
       sum(expiries)                                             AS expiries,
       sum(settlements_s1)                                       AS settlements_s1,
       sum(settlements_s2)                                       AS settlements_s2,
       sum(settlements_s3)                                       AS settlements_s3,
       sum(realised_lender_loss)                                 AS realised_lender_loss,
       sum(settlement_lender_pnl)                                AS settlement_lender_pnl,
       sum(notional_opened)                                      AS notional_opened,
       sum(shortwin_volume)                                      AS shortwin_volume,
       sum(creator_fees)                                         AS creator_fees,
       sum(deposits)                                             AS deposits,
       sum(withdrawals)                                          AS withdrawals,
       sum(coverage_gaps)                                        AS coverage_gaps,
       sum(coverage_gap_s)                                       AS coverage_gap_s,
       sum(positions_open_s)                                     AS positions_open_s,
       sum(coverage_paused_s)                                    AS coverage_paused_s,
       sum(observations)                                         AS observations,
       (array_agg(lender_sol ORDER BY bucket_start DESC))[1]     AS lender_sol,
       (array_agg(markets ORDER BY bucket_start DESC))[1]        AS markets,
       (array_agg(inventory_value_lamports ORDER BY bucket_start DESC))[1] AS inventory_value_lamports
FROM global_stats_hourly
GROUP BY div(bucket_start, 86400);
