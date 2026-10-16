-- short.win indexer — 0003 protocol projections (INDEXER.md §6). Owner: INGEST.
--
-- Two kinds of table:
--   * per-event typed rows (`fees`, `settlements`, `market_snapshots`, `coverage_events`,
--     `pool_events`, `market_state_log`, `share_epochs`, `protocol_sweeps`): FK to `events`,
--     ON DELETE CASCADE, written once;
--   * aggregate projections (`markets`, `positions`, `lender_positions`, `pools`): a pure function
--     of the market's events in chain order (§6.1), updated incrementally and rebuildable
--     (`needs_rebuild`, §3.4). Their columns marked [E] are event-derived and asserted equal to
--     the chain account by the replay harness (§11.4); [A] columns come from the account refresher.

-- ---- markets --------------------------------------------------------------------------------

CREATE TABLE markets (
    market               pubkey      PRIMARY KEY,
    mint                 pubkey      NOT NULL UNIQUE,
    token_program        pubkey      NOT NULL,
    -- The wallet that listed the market (create_market's signer; confers nothing).
    creator              pubkey      NOT NULL,
    -- [A] The token's creator wallet on its venue (SPEC R1, ARCH C0-18): the pump curve's
    -- `creator` / the PumpSwap pool's `coin_creator` (Market.cached_creator / its pool cache), or a
    -- venue trade's creator field; NULL off the pump venues. A fee-sharing coin reports its
    -- sharing-config PDA here, not a wallet (§8.3).
    coin_creator         pubkey,
    coin_creator_source  TEXT        CHECK (coin_creator_source IN ('account', 'venue_trade')),
    created_slot         BIGINT      NOT NULL,
    created_ts           BIGINT      NOT NULL,
    created_signature    signature   NOT NULL,
    creation_fee         u64         NOT NULL,
    venue_at_listing     u8          NOT NULL,
    tier_at_listing      u8          NOT NULL,
    bonding_curve        pubkey,
    canonical_pool       pubkey,
    -- [E] venue kind (`VenueKind` as u8; §6.2 names): MarketCreated, VenueStateChanged.to, and a
    -- PoolSwitched adoption (the candidate's kind from `propose_pool`'s `candidate_kind`).
    venue                u8          NOT NULL,
    -- [E] the adopted Raydium pool (MarketCreated.ray_pool, then PoolSwitched.candidate).
    ray_pool             pubkey,
    -- VenueStateChanged.pool of the last transition (the canonical pool on → PumpSwap).
    venue_pool           pubkey,
    graduated_at         BIGINT,
    -- [E] latest tier seen (MarketCreated, Open, PriceCranked), by chain order.
    tier                 u8          NOT NULL,
    -- [E] flags (Market.flags).
    paused_opens         BOOLEAN     NOT NULL DEFAULT FALSE,
    paused_coverage      BOOLEAN     NOT NULL DEFAULT FALSE,
    coverage_cleared_at  BIGINT,
    mint_unsafe          BOOLEAN     NOT NULL DEFAULT FALSE,
    route_broken         BOOLEAN     NOT NULL DEFAULT FALSE,
    wipe_live            BOOLEAN     NOT NULL DEFAULT FALSE,
    -- [E] pool accounting (Market.tokens_free, borrowed_q, borrowed_q_by_tier, total_shares,
    -- share_epoch, open_positions, total_notional_open, max_q_hwm).
    tokens_free          u64         NOT NULL DEFAULT 0,
    borrowed_q           u64         NOT NULL DEFAULT 0,
    borrowed_q_tier0     u64         NOT NULL DEFAULT 0,
    borrowed_q_tier1     u64         NOT NULL DEFAULT 0,
    borrowed_q_tier2     u64         NOT NULL DEFAULT 0,
    borrowed_q_tier3     u64         NOT NULL DEFAULT 0,
    total_shares         u128        NOT NULL DEFAULT 0,
    share_epoch          u32         NOT NULL DEFAULT 0,
    open_positions       u32         NOT NULL DEFAULT 0,
    total_notional_open  u64         NOT NULL DEFAULT 0,
    max_q_hwm            u64         NOT NULL DEFAULT 0,
    -- [E] Σ v of the open positions (their vault SOL, margin top-ups included).
    open_vault_lamports  u64         NOT NULL DEFAULT 0,
    -- [E] the SOL ledgers of §6.4 (MarketVault.lender_sol, MarketVault.protocol_fees).
    lender_sol           u64         NOT NULL DEFAULT 0,
    protocol_fees        u64         NOT NULL DEFAULT 0,
    lender_count         u32         NOT NULL DEFAULT 0,
    -- Latest price state: PriceCranked (all of it), Open (spot_wad, ema_wad), or [A] a fresher
    -- account snapshot (`price_source`).
    spot_wad             u128,
    ema_price            u128,
    ema_liq              u128,
    liq_slow             u128,
    ema_count            u64,
    lb_lo                u128,
    lb_hi                u128,
    util_bps             u32,
    vest_bal             u64,
    rent_basis0          u128,
    rent_basis1          u128,
    rent_basis2          u128,
    price_slot           BIGINT,
    price_ts             BIGINT,
    price_source         TEXT        CHECK (price_source IN ('event', 'account')),
    -- [E] route chains (RouteChainChanged; PoolSwitched clears them).
    no_route_since       BIGINT      NOT NULL DEFAULT 0,
    last_no_route_obs    BIGINT      NOT NULL DEFAULT 0,
    pool_fail_since      BIGINT      NOT NULL DEFAULT 0,
    last_pool_fail_obs   BIGINT      NOT NULL DEFAULT 0,
    -- [E] the fast-path collection window (PoolProposed; cleared by PoolSwitched / Cancelled).
    fp_candidate         pubkey,
    fp_venue             u8,
    fp_d                 u128,
    fp_window_ends       BIGINT,
    -- [E] the incumbent's locked depth (PoolSwitched.d_cand, PoolProposed.d_inc).
    pool_d               u128,
    -- Projection watermark: the chain-order key of the last event applied (§3.3, §6.1).
    last_slot            BIGINT      NOT NULL,
    last_tx_index        INTEGER     NOT NULL,
    last_ix_index        SMALLINT    NOT NULL,
    last_ev_source       SMALLINT    NOT NULL,
    last_ev_seq          INTEGER     NOT NULL,
    needs_rebuild        BOOLEAN     NOT NULL DEFAULT FALSE,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX markets_creator ON markets (creator);
CREATE INDEX markets_venue_tier ON markets (venue, tier);
CREATE INDEX markets_needs_rebuild ON markets (market) WHERE needs_rebuild;

-- After-state of every event that changes a market's accounting (§6.1): the time series the
-- rollups (§8.6) and the replay checkpoints (§11.4) read.
CREATE TABLE market_state_log (
    signature            signature   NOT NULL,
    ix_index             SMALLINT    NOT NULL,
    ev_source            SMALLINT    NOT NULL,
    ev_seq               INTEGER     NOT NULL,
    market               pubkey      NOT NULL REFERENCES markets (market) ON DELETE CASCADE,
    slot                 BIGINT      NOT NULL,
    tx_index             INTEGER     NOT NULL,
    ts                   BIGINT      NOT NULL,
    kind                 TEXT        NOT NULL,
    tokens_free          u64         NOT NULL,
    borrowed_q           u64         NOT NULL,
    total_shares         u128        NOT NULL,
    open_positions       u32         NOT NULL,
    total_notional_open  u64         NOT NULL,
    open_vault_lamports  u64         NOT NULL,
    lender_sol           u64         NOT NULL,
    protocol_fees        u64         NOT NULL,
    ema_price            u128,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX market_state_log_order ON market_state_log (market, slot, tx_index, ix_index, ev_source, ev_seq);
CREATE INDEX market_state_log_ts ON market_state_log (market, ts);

-- ---- positions ------------------------------------------------------------------------------

CREATE TABLE positions (
    position              pubkey      PRIMARY KEY,
    market                pubkey      NOT NULL REFERENCES markets (market) ON DELETE CASCADE,
    borrower              pubkey      NOT NULL,
    nonce                 u64         NOT NULL,
    tier                  u8          NOT NULL,
    venue                 u8          NOT NULL,
    q                     u64         NOT NULL,
    s                     u64         NOT NULL,
    m                     u64         NOT NULL,
    v_open                u64         NOT NULL,
    margin_added          u64         NOT NULL DEFAULT 0,
    -- [E] ShortPosition.v: v_open + margin_added.
    v                     u64         NOT NULL,
    n_open                u64         NOT NULL,
    fee_open              u64         NOT NULL,
    venue_rent            u64         NOT NULL,
    entry_price           u128        NOT NULL,
    spot_wad_open         u128        NOT NULL,
    ema_wad_open          u128        NOT NULL,
    b_spot_open           u64         NOT NULL,
    b_ema_open            u64         NOT NULL,
    deadline              BIGINT      NOT NULL,
    opened_slot           BIGINT      NOT NULL,
    opened_tx_index       INTEGER     NOT NULL,
    opened_ts             BIGINT      NOT NULL,
    open_signature        signature   NOT NULL,
    -- Locked terms the analytics need (§6.5): the position account at open, else the protocol
    -- parameters in force at the open (`protocol_params_history`).
    rent_protocol_bps     u16,
    penalty_lender_bps    u16,
    penalty_protocol_bps  u16,
    penalty_keeper_bps    u16,
    expiry_fee_bps        u16,
    terms_source          TEXT        CHECK (terms_source IN ('account', 'config_history')),
    -- [E] settlement arming (SettleArmed / SettleDisarmed; ShortPosition.s_armed, s_count, s_since).
    armed                 BOOLEAN     NOT NULL DEFAULT FALSE,
    s_count               u64,
    s_since               BIGINT,
    arm_count             u32         NOT NULL DEFAULT 0,
    disarm_count          u32         NOT NULL DEFAULT 0,
    -- Exit.
    status                TEXT        NOT NULL DEFAULT 'open'
                          CHECK (status IN ('open', 'closed', 'liquidated', 'expired', 'settled')),
    exit_slot             BIGINT,
    exit_ts               BIGINT,
    exit_signature        signature,
    exit_keeper           pubkey,
    buyback_cost          u64,
    b_live_exit           u64,
    rent_paid             u64,
    rent_protocol         u64,
    penalty               u64,
    expiry_fee            u64,
    keeper_paid           u64,
    borrower_payout       u64,
    token_surplus         u64,
    liquidated_expired    BOOLEAN,
    settle_condition      u8          CHECK (settle_condition IN (1, 2, 3)),
    -- borrower_payout − (m + margin_added) − fee_open (§8.1 "borrower P&L").
    borrower_pnl          i128,
    duration_s            BIGINT
);
CREATE INDEX positions_market_status ON positions (market, status, opened_slot DESC);
CREATE INDEX positions_borrower ON positions (borrower, opened_slot DESC);
CREATE INDEX positions_open_deadline ON positions (deadline) WHERE status = 'open';

-- ---- lenders --------------------------------------------------------------------------------

CREATE TABLE lender_positions (
    market            pubkey   NOT NULL REFERENCES markets (market) ON DELETE CASCADE,
    owner             pubkey   NOT NULL,
    -- [E] LenderPosition.shares, epoch, lock, lock_until (§6.3 share conversion across epochs).
    shares            u128     NOT NULL DEFAULT 0,
    epoch             u32      NOT NULL DEFAULT 0,
    lock              u128     NOT NULL DEFAULT 0,
    lock_until        BIGINT   NOT NULL DEFAULT 0,
    deposited_tokens  u64      NOT NULL DEFAULT 0,
    withdrawn_tokens  u64      NOT NULL DEFAULT 0,
    sol_paid          u64      NOT NULL DEFAULT 0,
    pot_draw          u64      NOT NULL DEFAULT 0,
    deficit_covered   u64      NOT NULL DEFAULT 0,
    deposits          u32      NOT NULL DEFAULT 0,
    withdrawals       u32      NOT NULL DEFAULT 0,
    claims            u32      NOT NULL DEFAULT 0,
    first_slot        BIGINT   NOT NULL,
    first_ts          BIGINT   NOT NULL,
    last_slot         BIGINT   NOT NULL,
    last_ts           BIGINT   NOT NULL,
    PRIMARY KEY (market, owner)
);
CREATE INDEX lender_positions_owner ON lender_positions (owner);

-- EpochRolled (§6.3): the conversion every lender behind the epoch applies at its next touch.
CREATE TABLE share_epochs (
    market        pubkey    NOT NULL REFERENCES markets (market) ON DELETE CASCADE,
    epoch         u32       NOT NULL,
    acc_end       i128      NOT NULL,
    num           u8        NOT NULL,
    den           u128      NOT NULL,
    vest_flushed  u64       NOT NULL,
    full_wipe     BOOLEAN   NOT NULL,
    signature     signature NOT NULL,
    ix_index      SMALLINT  NOT NULL,
    ev_source     SMALLINT  NOT NULL,
    ev_seq        INTEGER   NOT NULL,
    slot          BIGINT    NOT NULL,
    ts            BIGINT    NOT NULL,
    PRIMARY KEY (market, epoch),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);

-- ---- settlements and fees -------------------------------------------------------------------

-- SolSettle with the realised-lender-loss inputs (§8.4).
CREATE TABLE settlements (
    position              pubkey    PRIMARY KEY,
    market                pubkey    NOT NULL,
    keeper                pubkey    NOT NULL,
    condition             u8        NOT NULL CHECK (condition IN (1, 2, 3)),
    b_live                u64       NOT NULL,
    b_rs                  u64       NOT NULL,
    b_ema                 u64       NOT NULL,
    p_lo                  u128      NOT NULL,
    p_hi                  u128      NOT NULL,
    l_lo                  u128      NOT NULL,
    l_hi                  u128      NOT NULL,
    x_lo                  u64       NOT NULL,
    x_hi                  u64       NOT NULL,
    pprime_lo             u16       NOT NULL,
    pprime_hi             u16       NOT NULL,
    fv                    u64       NOT NULL,
    k_s                   u64       NOT NULL,
    lender_amount         u64       NOT NULL,
    lump                  u64       NOT NULL,
    borrower_amount       u64       NOT NULL,
    ema_age_s             BIGINT    NOT NULL,
    -- From the position (immutable since its Open).
    q                     u64       NOT NULL,
    n_open                u64       NOT NULL,
    tier                  u8        NOT NULL,
    -- ts ≥ the position's deadline.
    expired               BOOLEAN   NOT NULL,
    -- §8.4: P_ref, how it was obtained, the token value of q at it, and the lenders' P&L.
    p_ref                 u128      NOT NULL,
    p_ref_source          TEXT      NOT NULL CHECK (p_ref_source IN ('crank_clamped', 'box_point', 'box_mid')),
    p_ref_age_s           BIGINT,
    token_value           u64       NOT NULL,
    lender_pnl            i128      NOT NULL,
    realised_lender_loss  u64       NOT NULL,
    signature             signature NOT NULL,
    ix_index              SMALLINT  NOT NULL,
    ev_source             SMALLINT  NOT NULL,
    ev_seq                INTEGER   NOT NULL,
    slot                  BIGINT    NOT NULL,
    tx_index              INTEGER   NOT NULL,
    ts                    BIGINT    NOT NULL,
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX settlements_market ON settlements (market, slot DESC);
CREATE INDEX settlements_condition ON settlements (condition, slot DESC);

-- The fee ledger (§8.2): one row per fee leg of an event.
CREATE TABLE fees (
    signature      signature NOT NULL,
    ix_index       SMALLINT  NOT NULL,
    ev_source      SMALLINT  NOT NULL,
    ev_seq         INTEGER   NOT NULL,
    fee_type       TEXT      NOT NULL CHECK (fee_type IN (
                       'creation_fee', 'open_fee', 'venue_rent',
                       'rent_lender', 'rent_protocol',
                       'liq_penalty_lender', 'liq_penalty_protocol', 'liq_penalty_keeper',
                       'liq_keeper_topup',
                       'expiry_fee', 'expiry_keeper_reward',
                       'settle_keeper_reward',
                       'trader_sweep_lenders', 'trader_sweep_wipe', 'trader_sweep_protocol',
                       'venue_rewards_lenders', 'venue_rewards_wipe', 'venue_rewards_protocol')),
    recipient      TEXT      NOT NULL CHECK (recipient IN ('protocol', 'lenders', 'keeper', 'venue')),
    -- The keeper's wallet for keeper legs.
    recipient_key  pubkey,
    market         pubkey    NOT NULL,
    position       pubkey,
    amount         u64       NOT NULL,
    -- TRUE when the amount is computed from locked terms rather than read from the event (§8.2).
    derived        BOOLEAN   NOT NULL DEFAULT FALSE,
    slot           BIGINT    NOT NULL,
    tx_index       INTEGER   NOT NULL,
    ts             BIGINT    NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq, fee_type),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX fees_market_ts ON fees (market, ts);
CREATE INDEX fees_type_ts ON fees (fee_type, ts);
CREATE INDEX fees_keeper ON fees (recipient_key, ts) WHERE recipient = 'keeper';

-- ProtocolFeesSwept (the outflow of `protocol_fees`; not a fee).
CREATE TABLE protocol_sweeps (
    signature   signature NOT NULL,
    ix_index    SMALLINT  NOT NULL,
    ev_source   SMALLINT  NOT NULL,
    ev_seq      INTEGER   NOT NULL,
    market      pubkey    NOT NULL,
    amount      u64       NOT NULL,
    recipient   pubkey    NOT NULL,
    slot        BIGINT    NOT NULL,
    ts          BIGINT    NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);

-- ---- prices, coverage, pools ----------------------------------------------------------------

-- PriceCranked.
CREATE TABLE market_snapshots (
    signature   signature NOT NULL,
    ix_index    SMALLINT  NOT NULL,
    ev_source   SMALLINT  NOT NULL,
    ev_seq      INTEGER   NOT NULL,
    market      pubkey    NOT NULL,
    slot        BIGINT    NOT NULL,
    tx_index    INTEGER   NOT NULL,
    ts          BIGINT    NOT NULL,
    spot_wad    u128      NOT NULL,
    ema_price   u128      NOT NULL,
    ema_liq     u128      NOT NULL,
    liq_slow    u128      NOT NULL,
    ema_count   u64       NOT NULL,
    lb_lo       u128      NOT NULL,
    lb_hi       u128      NOT NULL,
    tier        u8        NOT NULL,
    util_bps    u32       NOT NULL,
    rent_basis0 u128      NOT NULL,
    rent_basis1 u128      NOT NULL,
    rent_basis2 u128      NOT NULL,
    vest_bal    u64       NOT NULL,
    -- The transaction's fee payer (keeper attribution, §8.5).
    cranker     pubkey    NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX market_snapshots_market_ts ON market_snapshots (market, ts);

-- CoverageGap (log event) and CoveragePauseSet.
CREATE TABLE coverage_events (
    signature       signature NOT NULL,
    ix_index        SMALLINT  NOT NULL,
    ev_source       SMALLINT  NOT NULL,
    ev_seq          INTEGER   NOT NULL,
    market          pubkey    NOT NULL,
    kind            TEXT      NOT NULL CHECK (kind IN ('gap', 'pause_set')),
    gap_s           BIGINT,
    open_positions  u32,
    paused          BOOLEAN,
    reason          u8,
    by_key          pubkey,
    slot            BIGINT    NOT NULL,
    tx_index        INTEGER   NOT NULL,
    ts              BIGINT    NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX coverage_events_market_ts ON coverage_events (market, ts);

-- PoolProposed / PoolSwitched / PoolSwitchCancelled.
CREATE TABLE pool_events (
    signature    signature NOT NULL,
    ix_index     SMALLINT  NOT NULL,
    ev_source    SMALLINT  NOT NULL,
    ev_seq       INTEGER   NOT NULL,
    market       pubkey    NOT NULL,
    kind         TEXT      NOT NULL CHECK (kind IN ('proposed', 'switched', 'cancelled')),
    candidate    pubkey    NOT NULL,
    -- `POOL_KIND_*`: 0 listing, 1 listing window, 2 zero-position, 3 fast.
    pool_kind    u8        NOT NULL,
    -- The candidate's venue kind (4 AMM v4, 5 CPMM) when known (§6.2).
    candidate_venue u8,
    d_cand       u128      NOT NULL,
    d_inc        u128      NOT NULL,
    window_ends  BIGINT    NOT NULL,
    invocation   TEXT      NOT NULL,
    slot         BIGINT    NOT NULL,
    tx_index     INTEGER   NOT NULL,
    ts           BIGINT    NOT NULL,
    PRIMARY KEY (signature, ix_index, ev_source, ev_seq),
    FOREIGN KEY (signature, ix_index, ev_source, ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX pool_events_market ON pool_events (market, slot DESC);

-- Every pool a market proposed or adopted.
CREATE TABLE pools (
    market           pubkey   NOT NULL REFERENCES markets (market) ON DELETE CASCADE,
    pool             pubkey   NOT NULL,
    venue            u8,
    first_seen_slot  BIGINT   NOT NULL,
    first_seen_ts    BIGINT   NOT NULL,
    last_d           u128     NOT NULL,
    proposals        u32      NOT NULL DEFAULT 0,
    adopted_slot     BIGINT,
    adopted_ts       BIGINT,
    adoption_kind    u8,
    retired_slot     BIGINT,
    retired_ts       BIGINT,
    is_current       BOOLEAN  NOT NULL DEFAULT FALSE,
    PRIMARY KEY (market, pool)
);

-- ---- refresher-sourced data (§2.6) -----------------------------------------------------------

CREATE TABLE token_metadata (
    mint          pubkey      PRIMARY KEY,
    decimals      u8          NOT NULL,
    symbol        TEXT,
    name          TEXT,
    uri           TEXT,
    source        TEXT        NOT NULL CHECK (source IN ('metaplex', 'token2022', 'none')),
    fetched_slot  BIGINT      NOT NULL,
    fetched_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX token_metadata_symbol ON token_metadata (lower(symbol));

-- Latest decoded snapshot of each short.win account the refresher reads.
CREATE TABLE account_snapshots (
    pubkey        pubkey      PRIMARY KEY,
    account_type  TEXT        NOT NULL CHECK (account_type IN (
                      'GlobalConfig', 'VenueGuard', 'VenueExtras', 'OracleCache', 'Market',
                      'MarketVault', 'ShortPosition', 'LenderPosition', 'ShareEpoch',
                      'WalletExposure', 'Closed')),
    market        pubkey,
    lamports      u64         NOT NULL,
    slot          BIGINT      NOT NULL,
    data          JSONB       NOT NULL,
    fetched_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX account_snapshots_market ON account_snapshots (market, account_type);

-- OracleCache history (SOL/USD; `crank_sol_usd` emits no event).
CREATE TABLE oracle_prices (
    publish_time  BIGINT      PRIMARY KEY,
    price         BIGINT      NOT NULL,
    conf          u64         NOT NULL,
    expo          INTEGER     NOT NULL,
    updated_slot  BIGINT      NOT NULL,
    observed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
