-- short.win indexer — 0005 admin, governance and the venue latch (INDEXER.md §6.6). Owner: INGEST.

-- `propose_config` / `apply_config` / `cancel_config`. The payload no event carries is decoded
-- from `propose_config`'s argument (`ConfigChange`) and checked against the event's hash
-- (`sha256(kind ‖ payload)`, §5.4).
CREATE TABLE config_changes (
    proposed_signature  signature   NOT NULL,
    proposed_ix_index   SMALLINT    NOT NULL,
    proposed_ev_source  SMALLINT    NOT NULL,
    proposed_ev_seq     INTEGER     NOT NULL,
    -- `ConfigChange` variant index + 1 (1 Params … 7 OracleFeed) and its name.
    kind                u8          NOT NULL,
    kind_name           TEXT        NOT NULL CHECK (kind_name IN (
                            'Params', 'EmaParams', 'Kink', 'Authorities', 'Timelock',
                            'EnableVenues', 'OracleFeed', 'Unknown')),
    hash                TEXT        NOT NULL CHECK (hash ~ '^[0-9a-f]{64}$'),
    payload             JSONB,
    payload_verified    BOOLEAN     NOT NULL DEFAULT FALSE,
    not_before          BIGINT      NOT NULL,
    proposed_slot       BIGINT      NOT NULL,
    proposed_ts         BIGINT      NOT NULL,
    status              TEXT        NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending', 'applied', 'cancelled', 'superseded')),
    applied_signature   signature,
    applied_slot        BIGINT,
    applied_ts          BIGINT,
    cancelled_signature signature,
    cancelled_slot      BIGINT,
    cancelled_ts        BIGINT,
    cancelled_by        pubkey,
    PRIMARY KEY (proposed_signature, proposed_ix_index, proposed_ev_source, proposed_ev_seq),
    FOREIGN KEY (proposed_signature, proposed_ix_index, proposed_ev_source, proposed_ev_seq)
        REFERENCES events (signature, ix_index, ev_source, ev_seq) ON DELETE CASCADE
);
CREATE INDEX config_changes_hash ON config_changes (hash, status);

-- The protocol configuration after every change, in chain order (§6.5): `initialize_config`'s
-- arguments, then each applied `ConfigChange` and each `disable_venue` (the Emergency clears a
-- `venue_enabled` bit at once; M8-SEC-7). "Parameters in force at time t" is the last row at or
-- before t's chain-order key.
CREATE TABLE protocol_params_history (
    signature      signature NOT NULL REFERENCES ingest_txs (signature) ON DELETE CASCADE,
    ix_index       SMALLINT  NOT NULL,
    slot           BIGINT    NOT NULL,
    tx_index       INTEGER   NOT NULL,
    ts             BIGINT    NOT NULL,
    source         TEXT      NOT NULL CHECK (source IN ('initialize_config', 'apply_config', 'disable_venue', 'account')),
    -- Decoded `Params`, `[TierParams; 4]`, `EmaParamsArgs` (§4.2 JSON encoding).
    params         JSONB     NOT NULL,
    tiers          JSONB     NOT NULL,
    ema            JSONB     NOT NULL,
    util_kink_bps  u32       NOT NULL,
    governance     pubkey    NOT NULL,
    pause_authority pubkey   NOT NULL,
    coverage_authority pubkey,
    fee_recipient  pubkey    NOT NULL,
    timelock_s     u32       NOT NULL,
    venue_enabled  u32       NOT NULL,
    pyth_sol_usd   pubkey    NOT NULL,
    pyth_feed_id   TEXT      NOT NULL,
    PRIMARY KEY (signature, ix_index)
);
CREATE INDEX protocol_params_history_order ON protocol_params_history (slot, tx_index, ix_index);

-- Current global state (single row): GlobalConfig flags the events change.
CREATE TABLE protocol_state (
    id                 BOOLEAN   PRIMARY KEY DEFAULT TRUE CHECK (id),
    -- PauseSet{scope: global}.
    paused_opens       BOOLEAN   NOT NULL DEFAULT FALSE,
    -- RouteBrokenSet{scope: global}: the `venue_enabled`-style bits currently broken.
    route_broken_bits  u32       NOT NULL DEFAULT 0,
    -- VenueDisabled.venue_enabled / initialize_config / EnableVenues.
    venue_enabled      u32,
    last_slot          BIGINT    NOT NULL DEFAULT 0,
    last_ts            BIGINT    NOT NULL DEFAULT 0
);
INSERT INTO protocol_state (id) VALUES (TRUE);

-- The venue-upgrade latch per `VenueGuard` index (VenueUpgradeFlagged / VenueRevalidated).
CREATE TABLE venue_guards (
    venue             u8        PRIMARY KEY,
    upgraded          BOOLEAN   NOT NULL,
    guard_slot        u64       NOT NULL,
    programdata_slot  u64       NOT NULL,
    flagged_at        BIGINT,
    expected_slot     u64,
    last_slot         BIGINT    NOT NULL,
    last_ts           BIGINT    NOT NULL
);

-- VenueExtrasSet (the Emergency's bounded extra venue accounts).
CREATE TABLE venue_extras (
    venue           u8        NOT NULL,
    ix              u8        NOT NULL,
    version         u32       NOT NULL,
    keys            TEXT[]    NOT NULL,
    stable_version  u32       NOT NULL,
    last_slot       BIGINT    NOT NULL,
    last_ts         BIGINT    NOT NULL,
    PRIMARY KEY (venue, ix)
);
