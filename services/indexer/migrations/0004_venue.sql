-- short.win indexer — 0004 venue-side ingest (INDEXER.md §7). Owner: INGEST.

-- Pools and curves the indexer decodes trades of, with the orientation the decoders need
-- (AMM v4 `ray_log` carries no mints; §7.2). Filled at MarketCreated / PoolProposed / PoolSwitched /
-- VenueStateChanged by the refresher.
CREATE TABLE venue_pools (
    pool          pubkey      PRIMARY KEY,
    venue         TEXT        NOT NULL CHECK (venue IN ('pump_curve', 'pump_swap', 'raydium_v4', 'raydium_cpmm', 'mock')),
    program       pubkey      NOT NULL,
    mint          pubkey      NOT NULL,
    market        pubkey,
    -- AMM v4: SOL is the coin side; CPMM: SOL is token 0 (Market's SOL_IS_TOKEN0_OR_COIN flag).
    sol_is_first  BOOLEAN,
    lp_mint       pubkey,
    -- Third-party trades of this pool are ingested (§7.1 "full venue ingest").
    track_trades  BOOLEAN     NOT NULL DEFAULT FALSE,
    first_seen_slot BIGINT    NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX venue_pools_mint ON venue_pools (mint);

-- Every decoded venue trade (§7.2): short.win's own legs (nested under a short.win invocation)
-- and, for tracked pools, everyone else's.
CREATE TABLE venue_trades (
    signature            signature NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    ix_index             SMALLINT  NOT NULL,
    -- 0: an `emit_cpi!` event of the venue (pump TradeEvent, PumpSwap Buy/SellEvent);
    -- 1: a log line (CPMM SwapEvent "Program data:", AMM v4 "ray_log:").
    ev_source            SMALLINT  NOT NULL CHECK (ev_source IN (0, 1)),
    ev_seq               INTEGER   NOT NULL,
    slot                 BIGINT    NOT NULL,
    tx_index             INTEGER   NOT NULL,
    ts                   BIGINT    NOT NULL,
    venue                TEXT      NOT NULL CHECK (venue IN ('pump_curve', 'pump_swap', 'raydium_v4', 'raydium_cpmm', 'mock')),
    program              pubkey    NOT NULL,
    pool                 pubkey    NOT NULL,
    mint                 pubkey    NOT NULL,
    market               pubkey,
    -- From the trader's side: 'buy' = tokens out of the pool, 'sell' = tokens into it.
    side                 TEXT      NOT NULL CHECK (side IN ('buy', 'sell')),
    token_amount         u64       NOT NULL,
    -- Lamports paid (buy) or received (sell) by the trader, fees included as the venue reports.
    sol_amount           u64       NOT NULL,
    fee_lp               u64,
    fee_protocol         u64,
    fee_creator          u64,
    -- The creator fee in lamports (CPMM may charge it on the token side: valued at the trade's
    -- price, §7.2).
    fee_creator_lamports u64,
    creator              pubkey,
    user_key             pubkey    NOT NULL,
    -- The trade is nested under a short.win invocation (one of the protocol's legs).
    is_shortwin          BOOLEAN   NOT NULL,
    shortwin_invocation  TEXT,
    pool_token_reserve   u64,
    pool_sol_reserve     u64,
    -- ⌊sol_amount·WAD / token_amount⌋ (lamports per raw unit × WAD).
    price_wad            u128,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq)
);
CREATE INDEX venue_trades_market_ts ON venue_trades (market, ts) WHERE market IS NOT NULL;
CREATE INDEX venue_trades_mint_ts ON venue_trades (mint, ts);
CREATE INDEX venue_trades_shortwin ON venue_trades (market, ts) WHERE is_shortwin;

-- Venue lifecycle and admin events other than trades (pump CompleteEvent, PumpSwap
-- CreatePoolEvent / DisableEvent, …; §7.3).
CREATE TABLE venue_events (
    signature   signature NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    ix_index    SMALLINT  NOT NULL,
    ev_source   SMALLINT  NOT NULL CHECK (ev_source IN (0, 1)),
    ev_seq      INTEGER   NOT NULL,
    slot        BIGINT    NOT NULL,
    tx_index    INTEGER   NOT NULL,
    ts          BIGINT    NOT NULL,
    venue       TEXT      NOT NULL,
    program     pubkey    NOT NULL,
    kind        TEXT      NOT NULL,
    pool        pubkey,
    mint        pubkey,
    market      pubkey,
    data        JSONB     NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq)
);
CREATE INDEX venue_events_market ON venue_events (market, slot DESC) WHERE market IS NOT NULL;

-- BPF upgradeable loader `Upgrade` of one of the six venue programs (the latch's trigger, §7.3).
CREATE TABLE venue_program_upgrades (
    signature    signature NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    ix_index     SMALLINT  NOT NULL,
    inner_index  SMALLINT  NOT NULL,
    -- `VenueGuard` index (GUARD_PUMP … GUARD_JUPITER).
    venue        u8        NOT NULL,
    program      pubkey    NOT NULL,
    programdata  pubkey    NOT NULL,
    slot         BIGINT    NOT NULL,
    ts           BIGINT,
    PRIMARY KEY (signature, ix_index, inner_index)
);

-- Venue admin state the adapters read, observed by the refresher's account watch (§7.3): one row
-- per observed change of a watched field.
CREATE TABLE venue_admin_states (
    account        pubkey      NOT NULL,
    field          TEXT        NOT NULL,
    observed_slot  BIGINT      NOT NULL,
    venue          TEXT        NOT NULL,
    market         pubkey,
    old_value      TEXT,
    new_value      TEXT        NOT NULL,
    observed_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account, field, observed_slot)
);
