-- short.win indexer — 0101 ingest hardening (INDEXER.md §2.2, §3.2, §3.4–§3.6; the M8 security
-- review, M8-SEC-*). Owner: INGEST. Additive only: no column of 0001–0007 changes.

-- Which stream listed (and advanced its cursor with) each transaction (§3.5). The forward
-- listing's lower bound is the stream's newest *finalized* transaction (never rolled back, so the
-- node keeps it), and a rollback resets a cursor that named the rolled-back transaction to it
-- (M8-SEC-5).
CREATE TABLE ingest_tx_streams (
    signature  signature NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    stream     TEXT      NOT NULL,
    slot       BIGINT    NOT NULL,
    tx_index   INTEGER   NOT NULL,
    PRIMARY KEY (signature, stream)
);
CREATE INDEX ingest_tx_streams_newest ON ingest_tx_streams (stream, slot DESC, tx_index DESC);

-- Signatures a live notification named that the node did not serve (M8-SEC-2): kept and asked
-- for again until they are ingested or proven abandoned (the block at their slot, finalized,
-- does not carry them). `/v1/status` reports the count (an alert).
CREATE TABLE ingest_missing (
    signature      signature   PRIMARY KEY,
    slot           BIGINT      NOT NULL,
    attempts       INTEGER     NOT NULL DEFAULT 0,
    reason         TEXT        NOT NULL,
    first_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A monotonic ingestion sequence of transactions: the rollup job notices late rows that carry no
-- event (a no-op observation, a failed keeper transaction, a third-party trade) through it
-- (M8-SEC-14). The single writer commits in sequence order (§3.1).
ALTER TABLE ingest_txs ADD COLUMN seq BIGSERIAL;
CREATE UNIQUE INDEX ingest_txs_seq ON ingest_txs (seq);

-- A short.win event whose payload is longer than the indexer's layout (a program upgrade that
-- appended fields): the known prefix is decoded and applied, the payload is kept here with
-- `applied = TRUE` (M8-SEC-13). `applied = FALSE`: not applied at all (an alert).
ALTER TABLE ingest_quarantine ADD COLUMN applied BOOLEAN NOT NULL DEFAULT FALSE;
-- The market a quarantined short.win event names (every event's first field), when readable.
ALTER TABLE ingest_quarantine ADD COLUMN market pubkey;
CREATE INDEX ingest_quarantine_market ON ingest_quarantine (market, applied)
    WHERE market IS NOT NULL AND ev_source < 2;
-- `/v1/status` counts (bounded reads): event payloads (not instruction arguments, which anyone's
-- failed transaction can carry), and the last day's cut log streams.
CREATE INDEX ingest_quarantine_events ON ingest_quarantine (applied) WHERE ev_source < 2;
CREATE INDEX ingest_txs_logs_truncated ON ingest_txs (block_time) WHERE logs_truncated;

-- A pool of a market (M8-SEC-6): the listing's bonding curve, canonical pool or Raydium pool,
-- the graduated venue pool, or a pool the market proposed or adopted (`pools`). Anyone else's
-- venue trade (and a venue event naming a pool) belongs to a market only on one of these —
-- never by its mint alone, since any pool can name a listed mint.
CREATE FUNCTION sw_is_market_pool(p_market TEXT, p_pool TEXT) RETURNS BOOLEAN
    LANGUAGE SQL STABLE PARALLEL SAFE AS $$
    SELECT EXISTS (SELECT 1 FROM markets m WHERE m.market = p_market
                   AND p_pool IN (m.bonding_curve, m.canonical_pool, m.ray_pool, m.venue_pool))
        OR EXISTS (SELECT 1 FROM pools p WHERE p.market = p_market AND p.pool = p_pool)
$$;
