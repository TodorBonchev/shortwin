-- short.win indexer — 0001 foundation (docs/design/INDEXER.md §4, §5). Owner: FOUNDATION (frozen:
-- a change here needs every track's agreement and an INDEXER.md update).
--
-- Conventions (INDEXER.md §4.1):
--   * Integer types of the program keep their exact range through domains: u64 / u128 / i128 are
--     NUMERIC with a range check (exact sums, no float). They cross the Rust boundary as decimal
--     text: `$n::numeric` when binding, `col::text` when reading (`crate::numeric`).
--   * Public keys and signatures are base58 TEXT (the API's representation; `pubkey`, `signature`).
--     `Pubkey::default()` (all zeros, "11111111111111111111111111111111") is stored as NULL.
--   * Times: `slot` BIGINT, chain-clock seconds BIGINT (`ts`, the event's own `ts` field, or the
--     block time). TIMESTAMPTZ is used only for wall-clock bookkeeping (`ingested_at`, …).
--   * Every chain-derived row carries its ordering key: (slot, tx_index, ix_index, ev_source,
--     ev_seq) — the canonical chain order (INDEXER.md §3.3).

CREATE DOMAIN u8 AS SMALLINT CHECK (VALUE BETWEEN 0 AND 255);
CREATE DOMAIN u16 AS INTEGER CHECK (VALUE BETWEEN 0 AND 65535);
CREATE DOMAIN u32 AS BIGINT CHECK (VALUE BETWEEN 0 AND 4294967295);
CREATE DOMAIN u64 AS NUMERIC(20, 0)
    CHECK (VALUE BETWEEN 0 AND 18446744073709551615);
CREATE DOMAIN u128 AS NUMERIC(39, 0)
    CHECK (VALUE BETWEEN 0 AND 340282366920938463463374607431768211455);
CREATE DOMAIN i128 AS NUMERIC(39, 0)
    CHECK (VALUE BETWEEN -170141183460469231731687303715884105728
                     AND 170141183460469231731687303715884105727);
CREATE DOMAIN pubkey AS TEXT CHECK (VALUE ~ '^[1-9A-HJ-NP-Za-km-z]{32,44}$');
CREATE DOMAIN signature AS TEXT CHECK (VALUE ~ '^[1-9A-HJ-NP-Za-km-z]{64,88}$');

-- 10^18, the program's WAD (prices are lamports per raw token unit × WAD).
CREATE FUNCTION sw_wad() RETURNS NUMERIC
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$ SELECT 1000000000000000000::NUMERIC $$;

-- ⌊a·b / WAD⌋ for non-negative a, b (exact; `div` truncates, which is the floor here).
CREATE FUNCTION sw_mul_wad(a NUMERIC, b NUMERIC) RETURNS NUMERIC
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$ SELECT div(a * b, sw_wad()) $$;

-- Utilisation in basis points exactly as `shortwin_math::rent::utilization_bps`:
-- ⌈borrowed·10⁴ / (free + borrowed)⌉, 0 for an empty pool.
CREATE FUNCTION sw_util_bps(t_free NUMERIC, borrowed NUMERIC) RETURNS BIGINT
    LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE WHEN t_free + borrowed = 0 THEN 0
                ELSE ceil(borrowed * 10000 / (t_free + borrowed))::BIGINT END
$$;

-- ---- ingest bookkeeping (INDEXER.md §2, §3) -------------------------------------------------

-- One row per ingested transaction (successful or failed) that mentions a tracked address.
-- Every chain-derived row references it with ON DELETE CASCADE: deleting a transaction (a
-- rollback, §3.4) removes everything decoded from it.
CREATE TABLE ingest_txs (
    signature      signature   PRIMARY KEY,
    slot           BIGINT      NOT NULL,
    -- The transaction's position in its block (INDEXER.md §3.3: always the block position).
    tx_index       INTEGER     NOT NULL CHECK (tx_index >= 0),
    block_time     BIGINT,
    commitment     TEXT        NOT NULL CHECK (commitment IN ('confirmed', 'finalized')),
    source         TEXT        NOT NULL
                   CHECK (source IN ('rpc_backfill', 'ws_live', 'yellowstone', 'replay')),
    fee_payer      pubkey      NOT NULL,
    fee            u64         NOT NULL,
    compute_units  BIGINT,
    -- NULL on success; the RPC `UiTransactionError` JSON otherwise (no events are decoded).
    err            JSONB,
    -- Custom program error of the failing instruction, when there is one (keeper analytics).
    err_ix_index   SMALLINT,
    err_code       BIGINT,
    -- The log stream was cut ("Log truncated"): `emit!` log events may be missing (§5.1).
    logs_truncated BOOLEAN     NOT NULL DEFAULT FALSE,
    shortwin_ixs   SMALLINT    NOT NULL DEFAULT 0,
    ingested_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (slot, tx_index)
);
CREATE INDEX ingest_txs_commitment ON ingest_txs (commitment, slot) WHERE commitment = 'confirmed';
CREATE INDEX ingest_txs_fee_payer ON ingest_txs (fee_payer, slot DESC);

-- The single-row ingest state: the confirmed and finalized watermarks (§3.2).
CREATE TABLE ingest_state (
    id              BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (id),
    -- Every transaction of a tracked address up to this slot has been ingested (confirmed).
    confirmed_slot  BIGINT      NOT NULL DEFAULT 0,
    -- Every ingested transaction at or below this slot is finalized (or was rolled back).
    finalized_slot  BIGINT      NOT NULL DEFAULT 0,
    -- The newest ingested block time: the analytics' "now" (`sw_chain_now()`), so windows such as
    -- "last 24 h" mean chain time, in replayed sessions too (§8).
    latest_block_time BIGINT,
    -- The program's first transaction, once the backfill reached it (NULL while unknown).
    genesis_slot    BIGINT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO ingest_state (id) VALUES (TRUE);

-- The analytics' clock: the newest ingested block time, else the wall clock.
CREATE FUNCTION sw_chain_now() RETURNS BIGINT
    LANGUAGE SQL STABLE PARALLEL SAFE AS $$
    SELECT COALESCE((SELECT latest_block_time FROM ingest_state WHERE id),
                    extract(epoch FROM now())::BIGINT)
$$;

-- One cursor per signature stream (`getSignaturesForAddress` address; §3.5).
CREATE TABLE ingest_cursors (
    stream            TEXT        PRIMARY KEY,   -- 'program:<id>' | 'pool:<pubkey>' | 'programdata:<pubkey>'
    address           pubkey      NOT NULL,
    -- Newest signature whose transaction (and every older one) is ingested.
    newest_signature  signature,
    newest_slot       BIGINT,
    -- Backfill frontier going back in time, and whether the stream's history is complete.
    oldest_signature  signature,
    oldest_slot       BIGINT,
    backfill_complete BOOLEAN     NOT NULL DEFAULT FALSE,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Anything the decoder could not decode is kept, never dropped (§5.1, §10.3).
CREATE TABLE ingest_quarantine (
    signature   signature   NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    ix_index    SMALLINT    NOT NULL,
    ev_source   SMALLINT    NOT NULL CHECK (ev_source IN (0, 1, 2)),
    ev_seq      INTEGER     NOT NULL,
    program     pubkey      NOT NULL,
    reason      TEXT        NOT NULL,
    raw         BYTEA,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq)
);

-- Audit of rolled-back transactions (§3.4).
CREATE TABLE ingest_rollbacks (
    signature      signature   NOT NULL,
    slot           BIGINT      NOT NULL,
    reason         TEXT        NOT NULL,
    reingested     BOOLEAN     NOT NULL DEFAULT FALSE,
    rolled_back_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (signature, rolled_back_at)
);
