-- short.win indexer — 0007 API support (INDEXER.md §9, §10). Owner: API.

-- Optional API keys for higher rate limits (§10.1). Only a SHA-256 of the key is stored; the
-- key travels in the `X-Api-Key` header. Anonymous access needs no row.
CREATE TABLE api_keys (
    key_sha256   TEXT        PRIMARY KEY CHECK (key_sha256 ~ '^[0-9a-f]{64}$'),
    label        TEXT        NOT NULL,
    tier         TEXT        NOT NULL CHECK (tier IN ('partner', 'internal')),
    -- Overrides of the tier's defaults (requests per second, burst, WebSocket connections).
    rps          INTEGER     CHECK (rps > 0),
    burst        INTEGER     CHECK (burst > 0),
    ws_conns     INTEGER     CHECK (ws_conns > 0),
    disabled     BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Mint search (§9.2 `/v1/search`): symbol and name prefix lookups, always resolving to a mint.
CREATE INDEX token_metadata_name ON token_metadata (lower(name) text_pattern_ops);
CREATE INDEX token_metadata_symbol_prefix ON token_metadata (lower(symbol) text_pattern_ops);

-- The API's names for the program's small enums, exactly `crate::model::{venue_name, tier_name,
-- guard_name}` and `SolSettle.condition` (the API test `sql_names_match_the_model` pins them).
-- STRICT: NULL in, NULL out (a pool whose venue is unknown).
CREATE FUNCTION sw_api_venue(v SMALLINT) RETURNS TEXT
    LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT CASE v WHEN 0 THEN 'none' WHEN 1 THEN 'pump_curve' WHEN 2 THEN 'pump_migrating'
                  WHEN 3 THEN 'pump_swap' WHEN 4 THEN 'raydium_v4' WHEN 5 THEN 'raydium_cpmm'
                  WHEN 6 THEN 'mock' ELSE 'unknown' END
$$;

CREATE FUNCTION sw_api_tier(t SMALLINT) RETURNS TEXT
    LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT CASE t WHEN 0 THEN 'A' WHEN 1 THEN 'B' WHEN 2 THEN 'C' WHEN 3 THEN 'D'
                  ELSE 'unknown' END
$$;

CREATE FUNCTION sw_api_guard(g SMALLINT) RETURNS TEXT
    LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT CASE g WHEN 0 THEN 'pump' WHEN 1 THEN 'pump_amm' WHEN 2 THEN 'pump_fees'
                  WHEN 3 THEN 'raydium_v4' WHEN 4 THEN 'raydium_cpmm' WHEN 5 THEN 'jupiter'
                  ELSE 'unknown' END
$$;

CREATE FUNCTION sw_api_condition(c SMALLINT) RETURNS TEXT
    LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT CASE c WHEN 1 THEN 'S1' WHEN 2 THEN 'S2' WHEN 3 THEN 'S3' END
$$;

-- Read paths of the API that the ingest tables' own indexes do not cover (additive; §10.2):
--   * the WebSocket `trades:<mint>` feed follows `venue_trades` in chain order (§9.3);
--   * `/v1/settlements` without a filter pages settlements newest first.
CREATE INDEX venue_trades_chain_order ON venue_trades (slot, tx_index, ix_index, ev_source, ev_seq);
CREATE INDEX settlements_chain_order ON settlements (slot, tx_index, ix_index, ev_source, ev_seq);

-- The WebSocket `since` replay reads each subscribed channel through an index, at most 1,000 ids
-- per channel, whatever `since` is (M8-SEC-4): never a scan of `events`.
CREATE INDEX events_market_id ON events (market, id) WHERE market IS NOT NULL;
CREATE INDEX events_position_id ON events (position, id) WHERE position IS NOT NULL;
CREATE INDEX events_actor_kind_id ON events (actor, kind, id) WHERE actor IS NOT NULL;
CREATE INDEX events_kind_id ON events (kind, id);

-- `/v1/markets/{mint}/trades` pages the market's trades (its pools', M8-SEC-6) in chain order.
CREATE INDEX venue_trades_market_chain ON venue_trades (market, slot, tx_index, ix_index, ev_source, ev_seq)
    WHERE market IS NOT NULL;
