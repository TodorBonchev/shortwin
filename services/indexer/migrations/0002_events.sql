-- short.win indexer — 0002 decoded events and invocations (INDEXER.md §5). Owner: INGEST.
--
-- `events` holds every short.win and complete_curve event, decoded with the program's own types,
-- in one append-only table: the WebSocket feed, `/v1/.../events` and the projections' rebuild
-- (§3.4) read it. The typed tables of 0003–0005 are projections of it (FK, ON DELETE CASCADE).

CREATE TABLE events (
    -- Ingestion sequence: the WebSocket resume key (§9.3). Not the chain order.
    id          BIGSERIAL   NOT NULL UNIQUE,
    signature   signature   NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    -- Top-level instruction the event belongs to.
    ix_index    SMALLINT    NOT NULL,
    -- 0: an `emit_cpi!` self-CPI (ev_seq = its index in the top-level instruction's inner list);
    -- 1: an `emit!` "Program data:" log line (ev_seq = the line's index in the transaction's logs).
    ev_source   SMALLINT    NOT NULL CHECK (ev_source IN (0, 1)),
    ev_seq      INTEGER     NOT NULL,
    slot        BIGINT      NOT NULL,
    tx_index    INTEGER     NOT NULL,
    block_time  BIGINT,
    -- The emitting program (short.win or complete_curve).
    program     pubkey      NOT NULL,
    -- The instruction that emitted it (`open_short`, `sol_settle`, …; §5.2 attribution).
    invocation  TEXT        NOT NULL,
    -- The event's type name exactly as in `programs/shortwin/src/events.rs` (`SolSettle`, …).
    kind        TEXT        NOT NULL,
    -- `market` field (NULL for the global events, whose field is `Pubkey::default()`, and for
    -- complete_curve's events).
    market      pubkey,
    -- `position` field where the event has one; the acting wallet otherwise (owner, borrower,
    -- keeper, `by`): the indexes of `/v1/positions/{pubkey}` and `/v1/lenders/{owner}`.
    position    pubkey,
    actor       pubkey,
    -- The event's own `slot` and `ts` fields (the program clock).
    ev_slot     BIGINT      NOT NULL,
    ev_ts       BIGINT      NOT NULL,
    -- Every field, by its Rust name (§4.2 JSON encoding: integers wider than 32 bits as decimal
    -- strings, pubkeys base58, byte arrays hex).
    data        JSONB       NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq)
);
CREATE INDEX events_chain_order ON events (slot, tx_index, ix_index, ev_source, ev_seq);
CREATE INDEX events_market ON events (market, slot DESC, tx_index DESC) WHERE market IS NOT NULL;
CREATE INDEX events_kind ON events (kind, slot DESC);
CREATE INDEX events_position ON events (position, slot DESC) WHERE position IS NOT NULL;
CREATE INDEX events_actor ON events (actor, slot DESC) WHERE actor IS NOT NULL;

-- Every short.win invocation (top-level or CPI) of every ingested transaction, failed ones
-- included: instruction name, decoded arguments, the `market` account when the instruction names
-- one. Keeper analytics count observations that emit no event (a no-op `sol_settle`), and the
-- admin projections read arguments no event carries (`propose_config`'s payload, §5.4). Pruned
-- after `analytics.invocation_retention_days` (§8.6) except for admin instructions.
CREATE TABLE invocations (
    signature    signature   NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    ix_index     SMALLINT    NOT NULL,
    -- -1 for the top-level instruction itself; else its index in the inner list.
    inner_index  SMALLINT    NOT NULL,
    stack_height SMALLINT    NOT NULL,
    slot         BIGINT      NOT NULL,
    tx_index     INTEGER     NOT NULL,
    block_time   BIGINT,
    program      pubkey      NOT NULL,
    -- snake_case instruction name from the IDL; 'unknown' if the discriminator is not known.
    name         TEXT        NOT NULL,
    market       pubkey,
    -- Decoded arguments (§4.2 JSON encoding); NULL when the instruction has none or failed to
    -- decode (then quarantined with ev_source 2).
    args         JSONB,
    failed       BOOLEAN     NOT NULL,
    PRIMARY KEY (signature, ix_index, inner_index)
);
CREATE INDEX invocations_market ON invocations (market, name, slot DESC) WHERE market IS NOT NULL;
CREATE INDEX invocations_name ON invocations (name, slot DESC);
