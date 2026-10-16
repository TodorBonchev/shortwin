-- short.win indexer — 0100 ingest additions (INDEXER.md §3.3, §5.3, §6.4). Owner: INGEST.
-- Additive only: no column of 0001–0007 changes.

-- Projection inputs from the same transaction that no event field carries (INDEXER.md §5.3,
-- §6.4; IX-1 / IX-11), stored with the event so a rebuild reads nothing but `events`:
--   EpochRolled   {"record_donation": "<u64>"}: lamports pre-funded at the ShareEpoch record's
--                 address and swept into lender_sol by the rolling sol_settle (its System transfer);
--   PoolProposed / PoolSwitched from propose_pool {"candidate_kind": <u8>}: the candidate's venue;
--   ConfigProposed {"change_kind", "payload", "payload_hash"}: propose_config's decoded
--                 ConfigChange and sha256(kind ‖ payload) as the program computes it.
-- NULL for every other event. Never served as event data (`data` is the event's own fields).
ALTER TABLE events ADD COLUMN derived JSONB;

-- The global projection (§6.6) needs a rebuild: a global event arrived below one already applied
-- (a gap-fill) or a rollback removed one (§3.3, §3.4). The writer rebuilds it after its batch.
ALTER TABLE protocol_state ADD COLUMN needs_rebuild BOOLEAN NOT NULL DEFAULT FALSE;

-- The last P_ema an event of the market carried (PriceCranked.ema_price, Open.ema_wad) and its
-- time: a settlement's P_last (§8.4). Event-derived only — `ema_price` may hold a fresher
-- account read (the live refresher's `price_source = 'account'`), this never does.
ALTER TABLE markets ADD COLUMN ema_event u128;
ALTER TABLE markets ADD COLUMN ema_event_ts BIGINT;
