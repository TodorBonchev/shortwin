# short.win — Indexer and API (M8 contract)

*v1.2 • 2026-10-01 • the M8 contract, as built: three parallel tracks built against v1.0, then
integrated (§13.5) — the exit criteria are met (ARCH §9 M8; evidence in §13.5); v1.2 folds in the
M8 security review's fixes (M8-SEC-1 … -15; regressions §11.5a, open items §14); v1.3 (2026-10-03)
folds in M10: the venue trades' post-trade reserves are the venues' pricing reserves (GC-F1, §7.2,
§7.3, §11.5), checked on recorded mainnet transactions, and IX-16's decoder half is closed (§14);
2026-10-08 (M10, FE-30): `/v1/markets/{mint}/lenders` takes an additive `epoch` filter (§9.2);
2026-10-09: the owner deferred IX-1 and IX-11 to a 1.0.x batch after the audit tag (§14)*

This document turns ARCHITECTURE §7.2 ("Indexer and API") and the M8 row of ARCH §9 into a buildable
specification: the ingest pipeline, its correctness rules (commitment, reorgs, idempotency, ordering,
checkpoints), the mapping of **every** program event to the database, the venue-side ingest, the
analytics of SPEC §8 / §13 with exact formulas and SQL, the REST and WebSocket API, abuse limits,
deployment, the test strategy (embedded Postgres, LiteSVM-recorded sessions replayed into the
database and compared with the chain), and the file ownership of the three tracks.

The machine-readable halves of the contract live next to the code:

| Artifact | Path | What it fixes |
|---|---|---|
| Schema | `services/indexer/migrations/0001…0007_*.sql`, `0100_ingest.sql` | every table, view, domain and SQL helper (sqlx migrations) |
| HTTP API | `services/indexer/openapi.yaml` | every endpoint, parameter, schema, error and header (OpenAPI 3.1, validated) |
| Crate | `services/indexer/` (`shortwin-indexer`, lib + bin `shortwin-indexer`) | ingest, projections, analytics, API; frozen interfaces (§13.4), config shape |
| Replay crate | `services/indexer-litesvm/` (`shortwin-indexer-litesvm`, TEST-ONLY) | the recorder, the session set and the DB-vs-chain assertions (§11.3, §11.4) |
| Test fixture | `services/indexer/src/testing/pg.rs`, `tests/pg_fixture.rs` | embedded PostgreSQL 17.11 in user space, migrations applied |
| Deployment | `services/indexer/deploy/` | docker-compose, Dockerfile (+ `Dockerfile.dockerignore`), roles, config |

**Scope (SPEC §8 "In").** Indexer/API for markets, deposits, positions, closes, liquidations,
settlements and fees; analytics for TVL, utilisation, OI, fees, liquidations, SOL-settlement counts
and realised lender loss; plus ARCH §7.2's additions: Raydium/PumpSwap/curve trades, admin events,
latch flags, coverage, creator-wallet deposits (SPEC R1), creator-fee income from short.win volume
(C0-18) and keeper coverage. **Not in scope:** the keeper's own metrics (Prometheus, ARCH §7.1), the
frontend (M9), Jupiter (v1.1).

**Environment facts this contract is written around.** No Docker and no system Postgres on the
development machine: tests run PostgreSQL from the user cache (§11.1). Surfpool needs mainnet and
the network: the M8 exit criterion "replays an E2E session into correct DB state" is met with
sessions **recorded from the LiteSVM harness** (§11.3); a Surfpool session is an optional extra.

---

## 1. Architecture

```
            ┌──────────── sources (TxSource, §2) ─────────────┐
            │ RPC backfill   logsSubscribe live   Yellowstone │   recorded session (§11.3)
            │ (finalized)    (confirmed)          (optional)  │   (finalized, tests)
            └───────────────┬─────────────────────────────────┘
                            │ RawTx (§2.1), chain order per source
                            ▼
   decode (§5, §7): invocation tree → short.win events (emit_cpi! inner
          instructions, emit! logs through the invocation stack), short.win instruction args,
          venue trades and lifecycle events
                            │
                            ▼
   writer (§3): ONE Postgres transaction per Solana transaction, idempotent on
          (signature, ix_index, ev_source, ev_seq); projections (§6); NOTIFY
                            │                                  ▲ account refresher (§2.6)
                            ▼                                  │ getMultipleAccounts
   Postgres ──── views (§8) ─── rollups (§8.6) ──── API: axum REST + WebSocket (§9, §10)
```

**Processes** (one binary, `shortwin-indexer <command>`, §12): `live` (the single writer: live
source + gap-fill + finality tracker + refresher), `backfill` (history; never concurrently with
`live`), `rollups` (analytics job), `serve` (API; stateless, horizontally scalable), `migrate`,
`replay <session>`, `rebuild`.

**Crate** (`services/indexer`, member of the services workspace, its own lockfile per ARCH §1.2):
`config`, `db`, `numeric`, `model` (FOUNDATION); `ingest::{raw, source, session}` (FOUNDATION,
frozen); `ingest::{rpc, live, replay, decode::*, writer, project, finality, refresher}` (INGEST);
`analytics::{formulas, rollups}` (ANALYTICS); `api::{routes, ws, error, pagination, ratelimit}`
(API); `testing::pg` (FOUNDATION, feature `embedded-pg`). The program's own crates are linked for
exact layouts: `shortwin` (`events`, `instruction`, `state`), `shortwin-math`
(`mul_div`, `utilization_bps`), `shortwin-venue-state` (venue account readers).

**ARCH §7.2 → this document.** ARCH's table list maps as: `markets` → `markets`;
`market_snapshots` → `market_snapshots`; `coverage` → `coverage_events`; `pools` → `pools`,
`pool_events`; `positions` → `positions`; `position_events` / `lender_events` → `events` (indexed by
`position` / `actor`); `lender_positions`, `share_epochs`, `settlements`, `fees`, `venue_trades` →
same names; `venue_events` → `venue_events`, `venue_program_upgrades`, `venue_admin_states`,
`venue_guards`, `venue_extras`; `oracle` → `oracle_prices`. ARCH's API paths are under `/v1`
(§9.2). ARCH's "the indexer runs the same byte-level mint filter … before Create market" is listed
in §14 (not M8).

---

## 2. Ingest sources

### 2.1 The normalised transaction (`ingest::raw`, frozen)

Every source produces `RawTx { signature, slot, tx_index, block_time, commitment, fee_payer, fee,
compute_units, err, account_keys, instructions: [RawTopIx { ix, inner: [RawIx] }], logs }` with
`RawIx { program_id, accounts (resolved keys, lookup-table addresses included), data, stack_height }`.
The decoder sees nothing else, so every source decodes identically. A source that cannot report
`stackHeight` must not produce a `RawTx` (all supported ones can).

### 2.2 RPC backfill (`ingest::rpc`)

- **Streams** (each with a cursor, §3.5): `program:<short.win>` (the retired straddle helper's
  `program:<complete_curve>` is gone with its ingestion, pump-final §9, owner decision D2),
  `programdata:<venue>` for the six venue ProgramData accounts (`VENUE_PROGRAM_DATA`; the latch's
  upgrades, §7.3), and `pool:<pubkey>` for tracked pools (§7.1).
- **Paging:** `getSignaturesForAddress(address, {limit: page_limit (1–1,000, checked at start),
  before, until, commitment: "finalized"})` from the newest back to the stream's **newest finalized
  transaction** (`ingest_tx_streams`, §3.5; never rolled back, so the node keeps it) — or its
  cursor, the start of its history, or `backfill.until_signature`; pages are collected **until an
  empty page** (a provider that caps pages below `page_limit` does not end the history early,
  M8-SEC-11), then processed **oldest first**. A bound the node does not know (`-32020`: a
  rolled-back signature, a node with shorter retention) falls back to its slot — listing from the
  newest down to that slot — with an error log for the operator (M8-SEC-5). publicnode, for one,
  refuses an `until` older than its ≈ 28 h retention with `-32020` (measured 2026-10-02 by M10's
  gap-calibration crawler, GC-F3), so a cursor of a quiet stream on such a node takes this path.
- **Fetch:** `getTransaction(sig, {encoding: "base64", maxSupportedTransactionVersion: 1,
  commitment: "finalized"})` with `backfill.fetch_concurrency` in flight and at most
  `backfill.max_rps` requests per second. Version **1** is required: keeper batches are SIMD-0385 v1
  transactions (ARCH §6.4). Failed transactions are fetched too (recorded, not decoded; §3.6).
  **No listed signature is skipped (M8-SEC-2):** only a JSON `null` result means "not served"
  (every RPC error, whatever its text, is an error); a `null` is asked for again with backoff
  (6 attempts, ≈ 15 s), and then the whole chunk fails before any of it is written — no cursor
  moves past an unfetched transaction, no history is marked complete, the process exits non-zero
  and the next run lists it again.
- **Conversion** `rpc::to_raw_tx`: decode the base64 transaction (legacy, v0 with
  `meta.loadedAddresses`, v1), resolve keys (static, then loaded writable, then loaded read-only),
  group `meta.innerInstructions` (base58 data, `stackHeight`) under their top-level index, copy fee,
  `computeUnitsConsumed`, `logMessages`, `err`.
- **Block position (`tx_index`, §3.3):** the response's `transactionIndex` when the node returns it;
  else `getBlock(slot, {transactionDetails: "signatures", rewards: false, maxSupportedTransactionVersion: 1})`
  and the signature's index, cached per slot.
- **Merge:** several streams are merged by `(slot, tx_index)` before the writer; a transaction
  reached through two streams is written once (§3.1).

### 2.3 Live (`ingest::live`)

Default (`ingest.live_source = "logs_subscribe"`): JSON-RPC PubSub `logsSubscribe({mentions:
[<address>]}, {commitment: "confirmed"})`, one subscription per tracked address (the PubSub API takes
one address per subscription), plus `slotSubscribe` for watermarks. Each notified signature is fetched
with `getTransaction` at `confirmed` through `rpc::to_raw_tx`, with its block position as in §2.2;
transactions of one slot are released to the writer in block order once the slot is `confirmed`.
Reconnect with exponential backoff (1 s → 30 s), then gap-fill (§3.5). `logsSubscribe` notifications
for failed transactions are processed like successes (recorded, not decoded). A notified signature
the node does not serve (4 attempts) goes to `ingest_missing` and is asked for again at the
finality ticks (16 per tick, each backing off 2^attempts s up to 5 min) until it is ingested or its
fork is proven abandoned (the finalized block at its slot
does not carry it); `/v1/status` reports the count (M8-SEC-2). A failed gap-fill counts as a failed
connection, and the failure budget (20) resets only after a gap-fill completes: a gap-fill that can
never succeed ends the process instead of looping (M8-SEC-5). Endpoint URLs and RPC errors reach the
logs only through the endpoint's redactor (userinfo, path and query replaced; M8-SEC-15).

### 2.4 Yellowstone gRPC and `blockSubscribe` (behind the same trait)

`TxSource` (`ingest::source`, frozen) is the seam: a Yellowstone source subscribes
`SubscribeRequest { transactions: { shortwin: { account_include: [program ids, tracked pools],
vote: false, failed: true } }, slots, commitment: CONFIRMED }` and gets the transaction, meta,
`index` (the block position) and slot status in one stream; a `blockSubscribe` source (where a node
enables it) is the same with JSON-RPC. **M8 builds the PubSub source.** Yellowstone was to be built
only if `yellowstone-grpc-client` resolved on the services lockfile's Agave 4.2.2 line without
changing any existing entry; as checked (IX-4) version 14.0.1 resolves without changing a version but
adds 33 packages (tonic, prost, vendored protoc) and changes the dependency lists of 6 existing
entries, so it is **deferred to v1.1**. Selecting `live_source = "yellowstone"` is a start-up error.

### 2.5 Recorded sessions (`ingest::replay`)

A session file (§11.3) holds transactions exactly as `getTransaction` returns them; the replay
source feeds them through `rpc::to_raw_tx` at `finalized` with `source = 'replay'`, using the
session header's program ids. Its account checkpoints play the refresher's role (§2.6).

### 2.6 The account refresher (`ingest::refresher`)

Events do not carry every value the API shows; the refresher reads accounts at `confirmed`:

| Trigger | Reads | Writes |
|---|---|---|
| a confirmed slot with events of market M (at most every `market_min_interval_ms` per market) | `Market`, `MarketVault` | `account_snapshots`; `markets.coin_creator` (`cached_creator`, or the PumpSwap pool cache's `CACHED_COIN_CREATOR`); the price columns when the snapshot's slot is newer than `markets.price_slot` (`price_source = 'account'`) |
| `Open` | the new `ShortPosition` | `positions` locked terms, `terms_source = 'account'` (§6.5) |
| every `oracle_interval_s` | `OracleCache` (PDA `sol_usd`) | `oracle_prices` (`crank_sol_usd` emits no event) |
| every `venue_watch_interval_s` | pump `Global`, `fee_config`; PumpSwap `GlobalConfig`, `fee_config`; each Raydium market's `AmmInfo` / `PoolState` / `AmmConfig` | `venue_admin_states` on a change of a watched field (§7.3) |
| `MarketCreated`, `PoolProposed`, `PoolSwitched`, `VenueStateChanged` | Metaplex metadata PDA, Token-2022 metadata extension, the mint; the pool account | `token_metadata`; `venue_pools` (orientation, LP mint) |

Refresher values are **[A]** (account-sourced) in §6; they are never inputs to the event-derived
[E] columns, which the replay harness checks against the chain independently (§11.4).

**As built, the account price override is live-only:** only the live refresher writes the price
columns from a `Market` snapshot (`price_source = 'account'`). A replayed checkpoint and a rebuild
keep them event-derived (so they stay a function of the events and incremental application equals
a rebuild); the refresher re-applies a newer account price on its next tick. Checkpoints still write
`account_snapshots`, `coin_creator`, `oracle_prices` and the locked terms a position lacks. A
settlement's `P_last` (§8.4) is the event-derived `markets.ema_event` (`0100_ingest.sql`), never an
account read.

---

## 3. Correctness: idempotency, commitment, ordering, rollback, checkpoints

### 3.1 Idempotency and the single writer

- **Unit of work:** one Postgres transaction per Solana transaction (or per run of consecutive ones;
  never splitting one). It starts with `INSERT INTO ingest_txs … ON CONFLICT (signature) DO
  NOTHING`; if the row existed, nothing else is written (`IngestReport.txs_duplicate`).
- **Row keys:** every decoded row is keyed by **(signature, ix_index, ev_source, ev_seq)**:
  `ix_index` = top-level instruction; `ev_source` 0 = an `emit_cpi!` inner instruction (`ev_seq` =
  its index in that top-level instruction's inner list), 1 = a log line (`ev_seq` = the line's
  index in the transaction's logs); `fees` add `fee_type`; `invocations` use `(signature, ix_index,
  inner_index)` with −1 for the top-level instruction. Re-ingesting anything is a no-op.
- **Single writer:** the writer takes `pg_try_advisory_lock(0x5357_494e_4458)` ("SWINDX") for its
  lifetime and exits if another process holds it. One writer means `events.id` commits in increasing
  order (the WebSocket's resume key, §9.3) and projections apply in one order. `backfill` and `live`
  are both writers: run `backfill` before `live`, or let `live` gap-fill (§3.5).

### 3.2 Commitment levels and finality

- Backfill ingests at **finalized**; live at **confirmed**; the live gap-fill lists at `confirmed`
  but fetches and writes every signature the node reports `finalized` at **finalized** (only the
  head stays `confirmed`, M8-SEC-1). `ingest_txs.commitment` records which.
- **Finality tracker** (`ingest::finality`, every `finality.poll_ms`): `getSlot({commitment:
  "finalized"})` = F; the `confirmed` transactions with `slot ≤ F` are checked with
  `getSignatureStatuses` (batches of ≤ 256, **`searchTransactionHistory: true`** — without it a node
  answers from its ~300-slot status cache only, and every older transaction would look abandoned,
  M8-SEC-1):

  | Status | Then |
  |---|---|
  | finalized, in the stored slot | `commitment = 'finalized'` |
  | finalized, in another slot (re-included after a fork switch) | rolled back and re-ingested from `getTransaction` at `finalized` — its true slot, block position and time (M8-SEC-12); kept if that is not served |
  | confirmed / processed | left for the next pass |
  | none | `getTransaction` at `finalized`: served in its slot → promoted; in another slot → moved as above; not served → the finalized block at the stored slot: carries it → promoted; does not (or the finalized chain skipped the slot) → the fork was abandoned → rollback (§3.4); the node cannot tell (pruned, not stored) → kept `confirmed` (`unresolved`, logged) |

  History is never deleted on a guess: an unresolved transaction holds `ingest_state.finalized_slot`
  below it (the finality-backlog alert, §12.3). At most 64 status-less transactions are looked into
  per pass (the rest wait for the next one), so a node without history cannot turn the tracker into
  an RPC flood. `finalized_slot` advances to F once every
  transaction at or below F is resolved.
- **API semantics:** projections are at `confirmed`; every row derived from a transaction carries
  `finalized = (slot ≤ finalized_slot)`; responses carry `X-Indexer-Slot` and
  `X-Indexer-Finalized-Slot` (§9.1).

### 3.3 Ordering

- **Chain order** is `(slot, tx_index, ix_index, ev_source, ev_seq)`; `tx_index` is **always the
  block position** (RPC `transactionIndex`, `getBlock` signatures, Yellowstone `index`, the
  recorder's per-slot counter). Within one top-level instruction, CPI events keep their emission
  order (the order the program emits them, e.g. `sol_settle`: `VenueStateChanged` →
  `SettleArmed|SettleDisarmed` → `SolSettle` → `EpochRolled`); log events sort after them and affect
  no accounting projection (§5.1).
- **Projections apply in chain order per market.** Each `markets` row keeps its watermark
  `(last_slot, last_tx_index, last_ix_index, last_ev_source, last_ev_seq)`. An event at or below the
  watermark (a gap-fill writing history after newer live events) is inserted with its raw rows only,
  and the market is marked `needs_rebuild`; the writer rebuilds marked markets at the end of the
  batch (§3.4). Global projections (`protocol_state`, `venue_guards`, `venue_extras`,
  `config_changes`, `protocol_params_history`) are rebuilt the same way from their events.

### 3.4 Rollback and rebuild

- **Rollback** of a transaction (`finality::rollback_signature`, one Postgres transaction): `DELETE
  FROM ingest_txs WHERE signature = $1` — every decoded row references it with `ON DELETE CASCADE`
  (events, invocations, typed rows, venue rows, history rows); its markets are marked
  `needs_rebuild` (and `protocol_state.needs_rebuild` when it held a global event or
  `initialize_config`); the `account_snapshots` at or after its slot are dropped (they may show the
  abandoned fork; the refresher reads the accounts again, and the [A] columns they fed follow);
  `ingest_rollbacks` records it; `rollup_state.complete_before` falls back to the hour of its block
  time (`analytics::rollups::reset_from`, inside the same transaction); every stream cursor that
  named it as its newest goes back to the stream's newest finalized transaction (its oldest to the
  oldest remaining one), so no cursor ever names a signature the chain does not have (M8-SEC-5;
  the same-position replacement of §3.1 goes through this path too); `pg_notify(
  'shortwin_rollback', <slot>)`; the writer then rebuilds the marked projections (a transaction the
  tracker found in another slot is written there, §3.2). A source's `SourceItem::Rollback { slot }`
  rolls back every non-finalized transaction at or above `slot`, newest first. Finalized
  transactions are never rolled back.
- **Rebuild** (`project::rebuild`): delete the market's `markets` row (cascading to `positions`,
  `lender_positions`, `pools`, `share_epochs`, `market_state_log`), then re-apply every event of the
  market from `events` in chain order with the same code path as incremental application.
  Incremental application and a rebuild must produce identical rows (asserted, §11.4).
- Rollbacks at `confirmed` are rare (optimistic confirmation); the design favours simple,
  deterministic recovery over speed.

### 3.5 Checkpointing and restart

- `ingest_cursors(stream)`: `newest_signature/newest_slot` (everything of the stream up to it is
  ingested), `oldest_signature/oldest_slot` and `backfill_complete` (the backward frontier). Updated
  in the same Postgres transaction as the rows they cover, together with `ingest_tx_streams` (which
  streams listed each transaction): a stream's newest **finalized** transaction is the next
  listing's lower bound (§2.2) and a rollback's cursor reset (§3.4).
- `ingest_state`: `confirmed_slot`, `finalized_slot`, `latest_block_time` (the analytics clock,
  `sw_chain_now()`), `genesis_slot`.
- **Live start-up / reconnect:** (1) subscribe and buffer; (2) gap-fill each stream from its newest
  finalized transaction to now with the backfill (listed at `confirmed`, finalized ones written at
  `finalized`); (3) drain the buffer (duplicates are no-ops); (4) stream. A crash at any point loses
  nothing: cursors only advance with committed rows, and a notified signature not yet served waits
  in `ingest_missing` (a table, so a restart keeps it).

### 3.6 Failures

- **Failed transactions** (`meta.err`): `ingest_txs` row with `err`, `err_ix_index`, `err_code`
  (the custom error of the failing instruction), and their `invocations` (`failed = true`); no events
  (a failed transaction commits nothing; its inner instructions up to the failure must not be read
  as events). Keeper analytics use them (§8.5).
- **Undecodable payloads** (an unknown event discriminator from short.win, a Borsh error, an
  instruction whose arguments do not parse): `ingest_quarantine` with the raw bytes, the reason and
  (for a short.win event, its first field) the market; the transaction's other rows are written. A
  quarantined short.win event is an alert (it means the program and the indexer disagree on a
  layout): `/v1/status` counts them (`quarantined`) and `/v1/markets/{mint}` says how many name the
  market (`quarantined_events`: its projection may be incomplete; M8-SEC-13).
- **Longer layouts:** a short.win event with bytes after the indexer's layout (a program upgrade
  that appended fields, as M2 did to `Close` / `Liquidate` / `Expire`) is decoded on its known
  prefix and **applied**, and recorded in `ingest_quarantine` with `applied = TRUE`
  (`/v1/status.layout_warnings`: upgrade the indexer). A shorter payload (an older layout replayed
  on a newer indexer) still quarantines: versioned decoders are an open item (§14 IX-18).
  *AIR-01 (`caa5852`):* instructions decode against the embedded IDL too, and
  `revalidate_venue` gained `expected_elf_hash`, so the two-argument call of a build before
  AIR-01 no longer parses and a backfill of such a build's history quarantines it (the
  committed recordings were migrated, §11.3). No legacy decoder: the owner accepted this on
  2026-10-13; it matters only if such a build was deployed and its history must be indexed.
- **Absent logs:** a `getTransaction` response without `logMessages` is read as a cut log stream
  (`ingest_txs.logs_truncated`; `/v1/status.logs_truncated` counts the last day's).
- **RPC errors:** retried with backoff inside the source (a permanent JSON-RPC code — a bound or a
  slot the node does not have, an invalid request — is returned at once); a source gives up only
  after its retry budget and the process exits non-zero (the supervisor restarts it; §3.5 makes
  that safe). Their text is redacted before it is logged (M8-SEC-15).

---

## 4. Storage conventions

### 4.1 Types (`0001_foundation.sql`, `crate::numeric`)

| Program type | Postgres | Rust boundary |
|---|---|---|
| `u8`, `u16`, `u32` | domains `u8` (SMALLINT), `u16` (INTEGER), `u32` (BIGINT) with range checks | `i16` / `i32` / `i64` |
| `u64` | domain `u64` = NUMERIC(20,0), 0 … 2^64−1 | decimal text: bind `$n::numeric`, read `col::text` |
| `u128` / `i128` | domains `u128` / `i128` = NUMERIC(39,0) with exact ranges | decimal text |
| `i64` (times, `gap_s`) | BIGINT | `i64` |
| `Pubkey` | domain `pubkey` (base58 TEXT); `Pubkey::default()` → NULL | `numeric::key_or_null` |
| signature | domain `signature` (base58 TEXT) | |
| `[u8; 32]` | TEXT, lowercase hex | `numeric::hex` |

Sums and products stay exact in NUMERIC; `sw_mul_wad(a, p) = div(a·p, 10^18)` and
`sw_util_bps(t, b)` are the SQL twins of `numeric::mul_wad_floor` and the program's
`utilization_bps` (the fixture test checks they agree). Queries use sqlx's runtime API
(`query`/`query_as` + `FromRow`), never the compile-time macros (no database at build time); sqlx
0.9 takes `&'static str` SQL — dynamic SQL is wrapped in `sqlx::AssertSqlSafe` and may splice
identifiers only from fixed allowlists (§10.2).

### 4.2 JSON encoding (`events.data`, `invocations.args`, the API)

Fields by their Rust names; `u8/u16/u32/bool` as JSON numbers/booleans; **`u64`, `i64`, `u128`,
`i128` as decimal strings** (JavaScript loses precision above 2^53); keys base58; byte arrays as
lowercase hex; arrays of keys as arrays of base58 strings; `Pubkey::default()` as `null`.
Exception: the API serves slots and unix seconds as JSON numbers (they fit).

### 4.3 Table families

| Family | Tables | Written | Deleted by |
|---|---|---|---|
| bookkeeping | `ingest_txs`, `ingest_state`, `ingest_cursors`, `ingest_quarantine`, `ingest_rollbacks` | writer, finality | rollback (`ingest_txs` row) |
| raw decoded | `events`, `invocations`, `venue_trades`, `venue_events`, `venue_program_upgrades` | writer, once | cascade from `ingest_txs` |
| per-event typed | `fees`, `settlements`, `market_snapshots`, `coverage_events`, `pool_events`, `share_epochs`, `protocol_sweeps`, `market_state_log`, `config_changes`, `protocol_params_history` | projections, once per event | cascade from `events` / `ingest_txs` |
| aggregate projections | `markets`, `positions`, `lender_positions`, `pools`, `protocol_state`, `venue_guards`, `venue_extras` | projections, updated | rebuild (§3.4) |
| refresher | `account_snapshots`, `token_metadata`, `oracle_prices`, `venue_pools`, `venue_admin_states` | refresher | never (latest wins) |
| analytics | `market_stats_hourly`, `global_stats_hourly`, `rollup_state` + views | rollup job | recomputed |
| API | `api_keys` | operators | operators |

### 4.4 Retention

Everything chain-derived is kept, except `invocations` of non-admin instructions older than
`analytics.invocation_retention_days` (30), pruned by the rollup job after their hours are rolled up
(keeper per-second calls make it the largest table). As built, `analytics::RETAINED_INSTRUCTIONS`
keeps the admin, listing and transparency instructions forever — `initialize_config` above all:
the global rebuild reads its arguments (IX-17). `venue_trades` of third-party trades may be
pruned by an operator policy (they are re-fetchable); short.win's own legs never are.

---

## 5. Decoding

### 5.1 Event sources and trust

- **`emit_cpi!` events** (every short.win event but `CoverageGap`; `MintSafetyTripped` only from
  `flag_unsafe_mint`):
  an inner instruction whose **program is short.win** and whose data is
  `EVENT_IX_TAG_LE (e445a52e51cb9a1d) ‖ discriminator ‖ Borsh`. Anchor's event dispatcher requires
  the `__event_authority` PDA to sign, which only the program can do, so such an inner instruction in
  a **successful** transaction was emitted by short.win. Complete (inner instructions are never
  truncated).
- **`emit!` log events** (`CoverageGap`; `MintSafetyTripped` on exit paths; until pump-final
  P6 also the retired helper complete_curve's `CurveCompleted` / `CurveAlreadyComplete`, no
  longer decoded): `Program data: <base64(discriminator ‖ Borsh)>` lines,
  accepted **only while the emitting program is on top of the invocation stack** (§5.2) — any program
  can print "Program data:", so a line printed by another program is never taken for a short.win
  event. Log events are **advisory**: the runtime may truncate logs ("Log truncated" →
  `ingest_txs.logs_truncated`); no accounting projection depends on them.
- **Venue events** follow the same two rules for their own programs (§7.2).

### 5.2 Attribution: the invocation tree and the log stack

1. **Invocation tree.** For each top-level instruction, its inner instructions with `stack_height`.
   Every short.win invocation (any height: a Squads multisig executes `propose_config` by CPI) is
   decoded by its 8-byte Anchor discriminator into `invocations(name, args, market, failed)`;
   `market` is the account the IDL names `market` for that instruction, where it has one.
2. **CPI event → emitter.** An event inner instruction at height h+1 belongs to the nearest
   preceding short.win invocation at height h in the same top-level instruction (h = 1: the
   top-level instruction itself). `events.invocation` is that instruction's name.
3. **Log stack.** Parse `Program <id> invoke [n]` (push; n = 1 begins top-level instruction k+1),
   `Program <id> success` / `Program <id> failed: …` (pop). Each `Program data:` /
   `Program log: ray_log:` line is attributed to the program on top, with its top-level index, its
   line index (`ev_seq`) and the nearest short.win frame below it (a venue trade's
   `shortwin_invocation`). `Log truncated` stops the parse.
4. **Actors.** `events.position` = the event's `position` field; `events.actor` = `owner` (lender
   events), `borrower` (`Open`, `Close`), `keeper` (`Liquidate`, `Expire`, `SolSettle`), `by`
   (`CoveragePauseSet`, `PauseSet`, `ConfigCancelled`), else the transaction's fee payer for
   `PriceCranked` and NULL otherwise.

### 5.3 The event catalog → tables

`model::SHORTWIN_EVENTS` pins the 36 names and discriminators to the IDL (unit test
`catalog_matches_the_idl`). Every event is written to `events` (kind = the Rust name, `market` NULL
for the global ones whose field is `Pubkey::default()`); the table below is what else it does. "Emitted
by" is the program as built (grep of `emit_cpi!` / `emit!`). [L] = log event.

| Event | Emitted by | Typed rows | Projection effects (§6) |
|---|---|---|---|
| `MarketCreated` | `create_market` | `fees`: `creation_fee` (protocol) | insert `markets` (mint, token_program, creator, venue, tier, bonding_curve, canonical_pool, ray_pool); `protocol_fees += creation_fee`; refresher: `token_metadata`, `venue_pools` |
| `Deposit` | `deposit` | — | `lender_positions` (+shares, deposited_tokens, deposits); market `T := t_after`, `S := s_after`, `wipe_live := wipe_live ∧ s_after = 0`; `market_state_log` |
| `Withdraw` | `withdraw` | — | `lender_positions` (−shares, withdrawn_tokens, sol_paid, pot_draw); `T`, `S`; `lender_sol −= sol_paid` |
| `ClaimSol` | `claim_sol` | — | `lender_positions` (sol_paid, pot_draw, claims); `lender_sol −= sol_paid` |
| `DeficitCovered` | `cover_deficit` | — | `lender_positions.deficit_covered += amount`; `lender_sol += amount` |
| `LenderSynced` | `sync_lender` | — | `lender_positions` (epoch conversion, lock); no market change |
| `Open` | `open_short` | `fees`: `open_fee` (protocol), `venue_rent` (venue; PumpSwap `r`, when > 0) | insert `positions`; `T −= q`, `ΣB_Q += q` (and by tier), `open_positions += 1`, `total_notional_open += n_open`, `max_q_hwm = max(·, q)`, `open_vault += v`, `protocol_fees += fee_open`; price columns from `spot_wad`, `ema_wad`; `tier` |
| `Close` | `close_short` | `fees`: `rent_lender` = rent − rent_protocol, `rent_protocol` | position → `closed`; `T += q`, `ΣB_Q −= q`, `open_positions −= 1`, `total_notional_open −= n_open`, `open_vault −= v`, `max_q_hwm := 0` if no position is left; `lender_sol += rent − rent_protocol`; `protocol_fees += rent_protocol` |
| `Liquidate` | `liquidate` | `fees`: `rent_lender`, `rent_protocol` (both `derived`, §6.5), `liq_penalty_lender/_protocol/_keeper`, `liq_keeper_topup` | position → `liquidated` (`liquidated_expired = expired`); as `Close` with `q` from the position; `lender_sol += rent − ⌊rent·p⌋ + penalty_lender`; `protocol_fees += ⌊rent·p⌋ + penalty_protocol` |
| `Expire` | `expire` | `fees`: `rent_lender`, `rent_protocol` (`derived`), `expiry_fee` (lenders), `expiry_keeper_reward` | position → `expired`; as `Close`; `lender_sol += rent − ⌊rent·p⌋ + expiry_fee`; `protocol_fees += ⌊rent·p⌋` |
| `SolSettle` | `sol_settle` | `settlements` (§8.4); `fees`: `settle_keeper_reward` (k_s) | position → `settled` (`settle_condition`); `ΣB_Q −= q` (T unchanged: the tokens are extinguished), `open_positions −= 1`, `total_notional_open −= n_open`, `open_vault −= v`; `lender_sol += lender_amount`; `vest_bal := 0` when `vest_flushed > 0` (AIR-07: the settlement flushed the vest, with or without an epoch roll) |
| `SettleArmed` | `sol_settle` | — | position `armed = true`, `s_count`, `s_since`, `arm_count += 1` |
| `SettleDisarmed` | `sol_settle` | — | position `armed = false`, `disarm_count += 1` |
| `EpochRolled` | `sol_settle` (a settlement that rolls) | `share_epochs` (`full_wipe = num = 0`) | `S := ⌊S·num/den⌋`, `share_epoch := epoch + 1`, `wipe_live := (num = 0)`, `vest_bal := 0` on a full wipe, `vest_bal := record_donation` on a rebase with `vest_flushed > 0` (the record's donation vests anew after the flush); lender counts recomputed; `lender_sol += record_donation` (§6.4; `events.derived`) |
| `ProtocolFeesSwept` | `sweep_protocol_fees` | `protocol_sweeps` | `protocol_fees −= amount` |
| `MarginAdded` | `add_margin` | — | position `margin_added += amount`, `v += amount`; `open_vault += amount` |
| `PriceCranked` | `crank_price` | `market_snapshots` (cranker = fee payer) | price columns (`spot_wad`, `ema_price`, `ema_liq`, `liq_slow`, `ema_count`, `lb_lo/hi`, `util_bps`, `rent_basis`, `vest_bal`), `tier` |
| `VenueStateChanged` | `crank_price`, `sol_settle`, a completing `close_short` / `liquidate` / `expire` | — | `venue := to`, `venue_pool := pool`, `graduated_at` (non-zero); refresher: `venue_pools`. Since pump v3 a close, liquidation or expiry past the curve's `rT` completes the curve in the exit (`→ pump_migrating`, pump-final §9); the retired straddle bundle took the market from the curve straight to PumpSwap |
| `RouteChainChanged` | `crank_price` | — | route-chain columns |
| `PoolProposed` | `propose_pool` | `pool_events` (`candidate_venue` = the invocation's `candidate_kind`, kept in `events.derived`) | `fp_candidate`, `fp_venue`, `fp_d`, `fp_window_ends`; `pool_d := d_inc`; `pools` upsert (`proposals += 1`, `last_d`) |
| `PoolSwitched` | `propose_pool`, `execute_pool_switch` | `pool_events` | `ray_pool := candidate`; `venue :=` the candidate's kind (`propose_pool`'s `candidate_kind`; for `execute_pool_switch` the `fp_venue` the window recorded); `pool_d := d_cand`; route chains and `fp_*` cleared; `pools`: candidate adopted (`is_current`), the previous one retired |
| `PoolSwitchCancelled` | `execute_pool_switch` | `pool_events` | `fp_*` cleared |
| `TraderSwept` | `sweep_trader`, `open_short`, `close_short`, `liquidate`, `expire` | `fees`: `trader_sweep_lenders` / `_wipe` / `_protocol` by `to` | `to ∈ {VEST, WIPE_RECORD}`: `lender_sol += lamports`; `to = PROTOCOL`: `protocol_fees += lamports` |
| `VenueRewardsClaimed` | (`claim_venue_rewards`: not built, never emitted) | `fees`: `venue_rewards_*` | as `TraderSwept` (mapping kept for completeness) |
| `MintSafetyTripped` | `flag_unsafe_mint` (CPI); [L] `withdraw`, `close_short`, `liquidate`, `expire`, `sol_settle` | — | CPI only: `mint_unsafe := true` (one-way). [L]: advisory, no projection |
| `VenueUpgradeFlagged` | `flag_venue_upgraded` | — | `venue_guards[venue]`: `upgraded = true`, `guard_slot`, `programdata_slot`, `flagged_at` |
| `VenueRevalidated` | `revalidate_venue` | — | `venue_guards[venue]`: `upgraded = false`, `guard_slot := programdata_slot`, `expected_slot` |
| `CoverageGap` [L] | any instruction whose prologue integrates a bucket (`engine::observe`) | `coverage_events` (`gap`) | none (advisory) |
| `CoveragePauseSet` | `set_coverage_pause` | `coverage_events` (`pause_set`) | `paused_coverage := paused`; on a clear `coverage_cleared_at := ts` (opens resume at `+COVERAGE_CLEAR_DELAY_S` = 5 s) |
| `ConfigProposed` | `propose_config` | `config_changes` (payload from the invocation's `ConfigChange`, §5.4; decoded at ingest into `events.derived`) | any pending change → `superseded` (the program has one pending slot: a proposal replaces whatever is pending, whatever its kind) |
| `ConfigApplied` | `apply_config` | — | `config_changes.status = 'applied'`; new `protocol_params_history` row (previous snapshot with the change applied); `EnableVenues` → `protocol_state.venue_enabled` |
| `ConfigCancelled` | `cancel_config` | — | `config_changes.status = 'cancelled'`, `cancelled_by` |
| `PauseSet` | `set_pause` | — | scope MARKET: `markets.paused_opens`; GLOBAL: `protocol_state.paused_opens` |
| `VenueDisabled` | `disable_venue` | — | `protocol_state.venue_enabled := venue_enabled` |
| `RouteBrokenSet` | `set_route_broken` | — | scope MARKET: `markets.route_broken`; GLOBAL: bit `venue_bit` of `protocol_state.route_broken_bits` |
| `VenueExtrasSet` | `set_venue_extras` | — | `venue_extras[(venue, ix)]` := version, keys, stable_version |

**`vest_bal` and the lender events** (AIR-07; `project::vest_after`): a `Deposit`, `Withdraw`,
`ClaimSol`, `DeficitCovered` or `LenderSynced` with `s_after = 0` sets `vest_bal := 0` (R3B-2: the
last shares are paid the vest first; the program holds no vest while `S = 0`).

**Instructions without an event** that still matter: `initialize_config` (its `InitConfigArgs` →
the first `protocol_params_history` row and `protocol_state.venue_enabled`); `crank_sol_usd` (the
refresher reads `OracleCache`); no-op `sol_settle` / `crank_price` calls (only `invocations`;
keeper coverage, §8.5).

### 5.4 Instruction arguments

Decoded with `shortwin::instruction::<Name>` (Anchor discriminator ‖ Borsh) for every short.win
invocation and stored in `invocations.args` (§4.2). Consumers: `propose_config` (`ConfigChange` →
`config_changes.payload`; the indexer recomputes `sha256(kind ‖ payload)` and sets
`payload_verified` when it equals `ConfigProposed.hash`), `initialize_config` (`InitConfigArgs`),
`propose_pool` (`candidate_kind` → `pool_events.candidate_venue`, `markets.fp_venue`),
`open_short` (`nonce`, `q`, bounds — for the API's transaction view), `set_pause` /
`set_route_broken` (scope), `create_market` (`venue_hint`). Arguments that fail to decode are
quarantined with `ev_source = 2`.

**`events.derived` (`0100_ingest.sql`).** The projection inputs from the same transaction that no
event field carries are stored with the event at decode time, so a rebuild reads nothing but
`events` (and `initialize_config`'s invocation): `EpochRolled` of a rolling `sol_settle` →
`{"record_donation": "<u64>"}` (§6.4); `PoolProposed` / `PoolSwitched` of `propose_pool` →
`{"candidate_kind": <u8>}`; `ConfigProposed` → `{"change_kind", "payload", "payload_hash"}` (the
decoded `ConfigChange` and `sha256(kind ‖ payload)` as the program computes it). NULL elsewhere;
never served (the API serves `events.data`, the event's own fields). Every event is also decoded
twice — the program's own Borsh type and an IDL walker over the embedded
`clients/js/idl/shortwin.json` that renders the §4.2 JSON — and a disagreement quarantines it.

---

## 6. Projections

### 6.1 Rules

- A projection is a **pure function of its events in chain order** (plus `share_epochs` for lender
  conversions and `protocol_params_history` for locked terms, both themselves projections of earlier
  events). Incremental application = rebuild (§3.4).
- **[E]** columns are event-derived and expected to equal the chain account exactly; the replay
  harness asserts each one (§11.4). **[E\*]** columns are exact *as of the last event that carries
  them* (the program also updates them silently in other instructions' prologues): price state,
  `tier`, route chains — the refresher corrects them live, the harness compares them only where the
  last observing invocation of the market emitted them. **[A]** columns come from the refresher.
- After every event that changes a market's accounting, a `market_state_log` row records the
  after-state (`tokens_free, borrowed_q, total_shares, open_positions, total_notional_open,
  open_vault_lamports, lender_sol, protocol_fees, ema_price`): the rollups' state source (§8.6).

### 6.2 `markets`

| Columns | Class | Rule |
|---|---|---|
| `venue` | [E] | `MarketCreated.venue`; `VenueStateChanged.to`; `PoolSwitched` → the candidate's kind (§5.3) |
| `ray_pool`, `fp_*` | [E] | pool events (§5.3) |
| `pool_d` | [E\*] | `PoolProposed.d_inc` / `PoolSwitched.d_cand`; every observation's prologue also refreshes it with the locked depth (`engine/prologue.rs`: `market.pool_d.set(st.locked_depth)`), so it is exact only right after those events (IX-12) |
| `paused_opens`, `paused_coverage`, `coverage_cleared_at`, `mint_unsafe`, `route_broken`, `wipe_live` | [E] | `Market.flags` bits 0, 6, 1, 2, 5 and `coverage_cleared_at` (§5.3) |
| `tokens_free` (T), `borrowed_q` (ΣB_Q), `borrowed_q_tier0..3`, `total_shares` (S), `share_epoch`, `open_positions`, `total_notional_open`, `max_q_hwm` | [E] | §5.3 rules; `max_q_hwm := 0` when `open_positions` reaches 0 |
| `open_vault_lamports` | [E] | Σ `v` of open positions (= Σ open `ShortPosition.v`) |
| `lender_sol`, `protocol_fees` | [E] | the ledgers of §6.4 (= `MarketVault.lender_sol`, `.protocol_fees`) |
| `lender_count` | [E] | lenders whose shares, converted to the current epoch (§6.3), are > 0 |
| price columns, `tier`, `util_bps`, `rent_basis*` | [E\*] / [A] | last `PriceCranked` / `Open` in chain order, or a newer account snapshot (`price_source`) |
| `vest_bal` | [E\*] / [A] | the last `PriceCranked` (or a newer account snapshot), and 0 after every reported flush: `SolSettle.vest_flushed > 0`; `EpochRolled` (full wipe → 0; flushing rebase → `record_donation`); a lender event with `s_after = 0` (AIR-07; `project::vest_after`). Lumps vested since the last crank are not tracked. `markets.vest_bal` is not served by the API; `PriceSnapshot.vest_bal` is the crank's `market_snapshots` row |
| route chains | [E\*] | last `RouteChainChanged`; cleared by `PoolSwitched` |
| `coin_creator` | [A] | the refresher (`cached_creator` / pool cache), else a venue trade's creator field |

`venue` names (`model::venue_name`): 0 none, 1 pump_curve, 2 pump_migrating, 3 pump_swap,
4 raydium_v4, 5 raydium_cpmm, 6 mock (test-venue builds only).

### 6.3 Lenders and share epochs

- `share_epochs(market, epoch)` from `EpochRolled` (`num` 0 = full wipe, 1 = rebase; `den =
  1,000^k`).
- A lender event carries the lender's `epoch` **after** the instruction, `shares` = shares minted
  (deposit) or burned (withdraw; 0 otherwise), `lock`, `lock_until`. The projection mirrors the
  program's lazy conversion (`touch`): for each epoch e from the row's `epoch` up to the event's
  `epoch` − 1, `shares := ⌊shares·num_e/den_e⌋` (`formulas::convert_shares`; a live full-wipe record
  keeps the lender on it, so `epoch` does not advance past it — the event's `epoch` says so); then
  `± ev.shares`; `lock`, `lock_until`, `epoch` := the event's. These are the on-chain
  `LenderPosition` values [E]; the API additionally converts to the market's current epoch for
  `share_bps`.
- Closed `LenderPosition` accounts (shares, pending and lock all 0) keep their row (history).

### 6.4 The SOL ledgers (`lender_sol`, `protocol_fees`)

```
lender_sol    += Close.rent − Close.rent_protocol
               + Liquidate.rent − ⌊Liquidate.rent·p/10⁴⌋ + Liquidate.penalty_lender
               + Expire.rent − ⌊Expire.rent·p/10⁴⌋ + Expire.expiry_fee
               + SolSettle.lender_amount
               + TraderSwept.lamports / VenueRewardsClaimed.lamports   (to ∈ {VEST, WIPE_RECORD})
               + DeficitCovered.amount
               + record_donation   (a sol_settle that rolls: the System transfer, inner to that
                                    invocation, from its `share_epoch_rec` account to `market_vault`
                                    — lamports someone pre-funded the record's address with, swept by
                                    `arming::create_pda_account` and vested; no event field carries it)
               − (Withdraw | ClaimSol | Deposit | DeficitCovered | LenderSynced).sol_paid
protocol_fees += MarketCreated.creation_fee + Open.fee_open
               + Close.rent_protocol + ⌊Liquidate.rent·p/10⁴⌋ + Liquidate.penalty_protocol
               + ⌊Expire.rent·p/10⁴⌋
               + TraderSwept.lamports / VenueRewardsClaimed.lamports   (to = PROTOCOL)
               − ProtocolFeesSwept.amount
```

`p` = the position's locked `rent_protocol_bps` (§6.5). These are ARCH §5.2's waterfalls and
§3.3's pool calls (`shortwin_math::pool`: `repay`, `settle`, `sweep`, `cover`, `claim`); the replay
harness asserts them against `MarketVault` (§11.4). Known flows without an event of their own: a
`credit` while `S = 0` and no wipe record is live moves SOL from `lender_sol` to `protocol_fees`
(`pool::credit`; ARCH §3.3, R3A-10) — for a trader sweep the event's `to = PROTOCOL` already says so;
a continuous-rent `accrue` credit while `S = 0` (only with a dust position left after the last
lender, ARCH §9 M0b note 4) does it silently. The harness exposes any path that reaches it (IX-11).

As built, `record_donation` is decoded at ingest into the rolling `sol_settle`'s `EpochRolled`
`events.derived` (§5.4), so both ledgers are identities inside the database (§11.4 item 6 sums
`events.derived->>'record_donation'` and checks it against the session's System transfers). Both
ledgers are exact on every recorded session, the 7,000,000-lamport record donation of
`tests/data/ingest/epoch_roll_prefunded_record.jsonl` and the trader sweeps included.

### 6.5 Positions and locked terms

`positions` rows come from `Open` (q, s, m, v, n_open, fee_open, venue_rent, entry_price,
spot/ema at open, b_spot/b_ema, deadline, tier, venue) and the exit event (§5.3). `q` is immutable (no
partial closes in the MVP), so `Liquidate`, `Expire` and `SolSettle` take it from the row.

Locked terms the analytics need (`rent_protocol_bps`, the penalty split, `expiry_fee_bps`) are not
in `Open`. Sources: the **parameters in force at the open** (`protocol_params_history`: the last row
at or before the open's chain-order key; `Params.rent_protocol_bps`, `penalty_lender_bps`,
`penalty_protocol_bps`, `penalty_keeper_bps`, `expiry_fee_bps`; `terms_source = 'config_history'`)
— exact when the indexer has the program's history from its first transaction, which the backfill
guarantees; as built this source comes **first**, because it is a function of the events alone (so
incremental application equals a rebuild); the **`ShortPosition` account** read by the refresher (or
a replayed checkpoint) fills a position the history cannot (`terms_source = 'account'`). The replay
harness checks that both sources agree for every position of every session. Fee rows computed from
them have `derived = true`.

`positions.m` is the margin **at open** (`Open.m`); the chain's `ShortPosition.m` grows with every
`add_margin` (`instructions/add_margin.rs` adds to both `m` and `v`), so chain `m = positions.m +
positions.margin_added` (§11.4 item 3).
`borrower_pnl = borrower_payout − (m + margin_added) − fee_open` (`formulas::borrower_pnl`; the
simulator's definition; `venue_rent` is reported separately); `duration_s = exit_ts − opened_ts`.

### 6.6 Admin, governance, the latch

`config_changes` (one row per `ConfigProposed`, then applied / cancelled / superseded),
`protocol_params_history` (full configuration after `initialize_config`, each `ConfigApplied` and
each `VenueDisabled` (`source = 'disable_venue'`): params, tiers, ema, kink, authorities,
timelock, venue bits, oracle feed), `protocol_state` (global pause, global route-broken bits,
venue_enabled — as `GlobalConfig::apply` moves it: `disable_venue` clears a bit at once, only an
applied `EnableVenues` sets bits (`|=`, into the live value), and no other change touches it; a
later `ConfigApplied` never rewrites it from a snapshot, M8-SEC-7), `venue_guards` (the upgrade latch per
`VenueGuard` index: 0 pump, 1 pump_amm, 2 pump_fees, 3 raydium_v4, 4 raydium_cpmm, 5 jupiter),
`venue_extras`. The `/v1/admin/*` and `/v1/venues` endpoints and the `admin` WebSocket channel read
them; the transparency they give is the point (SPEC §11's powers are visible as they are used).

---

## 7. Venue-side ingest

### 7.1 Scope

- **short.win's own legs** — always: every venue trade nested under a short.win invocation (opens
  sell, closes / liquidations / expiries buy; an exit past a pump curve's `rT` is two rows, below);
  they are in short.win's transactions already. `is_shortwin = true`, `shortwin_invocation` = the
  short.win frame below the venue frame (§5.2).
- **Everyone's trades of listed pools** — when `venue_trades.third_party = true`: one signature
  stream per tracked pool (§2.2), at most `max_pools`, chosen by OI then TVL. For charts (candles,
  §9.2) and, once markets are listed, the **gap-frequency calibration** of ECON §11.6 / G37. *M10:*
  before launch this indexer sees only short.win's markets, so the calibration runs from a
  public-RPC crawl that ports these decoders to Python (`scripts/gapcal`,
  `docs/research/gap-calibration.md`); a "gap" there is a persistent price jump of at least 1.5×,
  not an observation gap.
- `venue_pools` gives each pool its venue, mint, market, orientation and LP mint (refresher, §2.6).
- **Which market a trade belongs to (M8-SEC-6).** A short.win leg: the market of its short.win
  instruction. Anyone else's trade (and a venue event that names a pool): only when it trades one of
  **the market's pools** (`sw_is_market_pool`: the listing's bonding curve, canonical pool or
  Raydium pool, the graduated venue pool, or a pool the market proposed or adopted, `pools`) —
  never by its mint alone, since anyone can create a pool that names a listed mint (a PumpSwap pool
  pairing it with a worthless token, a dust CPMM pool) and list short.win in the transaction. Rows
  on other pools are stored with `market` NULL and never reach the market's `/trades`, `/candles`,
  `trades:<mint>` or creator totals. Rows ingested before their pool joined the market (a gap-fill)
  are attributed when it joins (`MarketCreated`, `VenueStateChanged`, `PoolProposed`,
  `PoolSwitched`); a rebuild does the same, so both end equal. PumpSwap `Buy/SellEvent`s are decoded
  only when one side of the pool is WSOL (CPMM already required it).

### 7.2 Decoders (`ingest::decode::venue`)

| Venue | Event | Found as | Fields used | Orientation |
|---|---|---|---|---|
| pump curve | `TradeEvent` | `emit_cpi!` of the pump program (`6EF8…`), Anchor event tag | mint, sol_amount, token_amount, is_buy, user, post-trade virtual/real reserves, fee, creator, creator_fee (later fields when present); `quote_mint`, read past the variable-length `shareholders` vector | token = mint; side = is_buy ? buy : sell; only on a SOL-quoted curve (`quote_mint` zero or WSOL, or an event that ends before it): a curve quoted in another token (since pump's 2026-10-08 build, e.g. a `multi_hop_swap`'s curve hop) gives no row, its amounts not being lamports (M8-SEC-6) |
| pump curve | `CompleteEvent` | `emit_cpi!` of pump | mint, bonding_curve, user | → `venue_events` |
| pump curve | `PostCompleteBuyEvent` (pump's 2026-10-08 redeploy) | `emit_cpi!` of pump, after the `TradeEvent` and the `CompleteEvent` of a buy past `rT` (pump v3's synthetic migration) | user, mint, bonding_curve, quote_mint, base_out, quote_in, fee, creator_fee, the would-be pool's reserves after | a **second trade row of the same instruction** (`ev_seq` keeps the two apart): buy `base_out` for `quote_in + fee + creator_fee` lamports, reserves = the would-be pool's after; only on a SOL-quoted curve (`quote_mint` zero or WSOL) |
| PumpSwap | `BuyEvent` / `SellEvent` | `emit_cpi!` of pAMM (`pAMM…`) | base/quote amounts, lp_fee, protocol_fee, pool, user, coin_creator, coin_creator_fee, pool reserves; *M10:* `virtual_quote_reserves` (the 2026-09 event tail, after the cashback / buyback fields, an i128) | base = token, quote = WSOL (canonical pools) |
| PumpSwap | `CreatePoolEvent`, `DisableEvent`, … | `emit_cpi!` of pAMM | as declared | → `venue_events` |
| pump curve, PumpSwap | ★`SweepBondingCurveFeeEvent` (pump), `SweepPoolFeeEvent` (pAMM): the only events the 2026-10-02 redeploys add | `emit_cpi!` | the curve or pool, mint, recipient, `amount`, `bucket` (`pump-rust-client` 0.2.0's IDLs; no recorded sweep yet, a unit test on the IDL layout and on each discriminator being in the dumped redeploy only) | → `venue_events` (a sweep moves the vault and the VQR oppositely: no reserve row) |
| Raydium CPMM | `SwapEvent` (`40c6cde8260871e2`) | `emit!` "Program data:" **only while CPMM (`CPMMoo8…`) is on top** — CLMM and others emit an event with the same name and discriminator | pool_id, input/output vault before, input/output amounts, input/output mint, trade_fee, creator_fee, creator_fee_on_input | from the mints |
| Raydium AMM v4 | `ray_log` | "Program log: ray_log: <base64 bincode>" while AMM v4 (`675k…`) is on top | `SwapBaseInLog` (log_type 3: amount_in, minimum_out, direction, user_source, pool_coin, pool_pc, out_amount) / `SwapBaseOutLog` (4: max_in, amount_out, direction, …, deduct_in) | `venue_pools.sol_is_first` (SOL is the coin side or not) |
| mock | — | the TEST-ONLY venue emits nothing | — | — |

**A buy past the curve's `rT` (pump-final §9).** pump v3 buys `rT` on the curve (`TradeEvent`,
which completes it: `CompleteEvent`) and `q − rT` from the pool `migrate_v2` will create
(`PostCompleteBuyEvent`) in one instruction: two `venue_trades` rows on the curve and one
`venue_events` row. For short.win's own exit past `rT` (a close, liquidation or expiry) both rows
are `is_shortwin`, and their `sol_amount`s sum to the exit's `buyback_cost`
(`tests/ingest_sessions.rs` on `tests/data/ingest/pump_synthetic_exit.jsonl`). The `TradeEvent`'s
`ix_name` is a label only: v3 trades emit `buy` / `sell` / `buy_exact_quote_in`, a multi-hop trade
`multi_hop_swap`; a PumpSwap event of a `multi_hop_swap` takes its pool's mints from the event
pool's hop group of the instruction's accounts.

**An open that grows the curve.** pump v3 grows a curve shorter than 166 bytes on its first v3
trade and takes the rent top-up from the trade's user after crediting the sale: a System
transfer from the trader to the bonding curve inside the same instruction. short.win's curve leg
cannot separate it (`Open.venue_rent` is PumpSwap's only, 0 on the curve), so such an open's `s`
is the `TradeEvent`'s `sol_amount` less that transfer; every other leg matches its event exactly
(`tests/ingest_sessions.rs`, on every recorded session).

Normalised row (`venue_trades`): `side` from the trader's view (buy = tokens out of the pool),
`token_amount`, `sol_amount` (lamports paid or received), fees, `fee_creator_lamports` (a CPMM
creator fee charged on the token side is valued at the trade's price `⌊fee·sol_amount/token_amount⌋`),
`creator`, `user_key`, post-trade reserves, `price_wad = ⌊sol_amount·WAD/token_amount⌋`.

**Post-trade reserves are the venue's pricing reserves (M10 GC-F1).** `pool_token_reserve` and
`pool_sol_reserve` are what the next trade of the pool prices on, so consecutive trades of a pool
chain exactly:
- pump: the curve's virtual reserves.
- PumpSwap: base = the base vault; quote = the quote vault + the event's `virtual_quote_reserves`,
  the effective quote reserves every buy and sell prices on. VQR is read from the event's tail and
  is 0 for an event that predates the field. No quote reserve is stored when the sum leaves
  [0, u64::MAX]. *M10 pump revalidation:* since the 2026-10-02 redeploy v2 `BuyEvent` /
  `SellEvent` keep the v1 layout (`ix_name` "buy_v2" …) and carry the **pre-trade** VQR; their
  fees stay in the vault as buckets and come off the VQR by the same amount, so `vault_before ±
  quote_amount_*_lp_fee + VQR` is still the post-trade effective reserve exactly
  (`pamm_it::buckets::v2_trades_keep_their_fees_in_the_pool_buckets`). The stored quote reserve is
  the pricing reserve, not what a sale can be paid (that is `vault − buckets`, short.win's `R`
  since PUMP-2026-10-1).
- CPMM: `vault − (protocol + fund + creator counters)`. On the input side that is
  `*_vault_before + input_amount − ⌊trade_fee·protocol_fee_rate/10⁶⌋ − ⌊trade_fee·fund_fee_rate/10⁶⌋
  − creator_fee` (the creator fee only when charged on the input); on the output side
  `*_vault_before − output_amount − creator_fee` (only when charged on the output). The rates are
  those of the swap's `amm_config` (its account 2): the venue admin watch's observation at or before
  the transaction's slot (§7.3), else the earliest one, else 120,000 / 40,000 ppm, the rates of every
  mainnet config (`CpmmFeeRates::MAINNET`).
- AMM v4: `ray_log`'s `pool_coin` / `pool_pc` ± the swap, i.e. vault − `need_take_pnl`.

Checked on recorded mainnet transactions (`tests/venue_mainnet.rs`, §11.5). **M8 builds stored
other values:** CPMM reserves with the protocol and fund shares of the trade fee still counted (and
the creator counters ignored), so consecutive swaps did not chain, and PumpSwap quote reserves
without VQR (understated by 17.6 SOL on the recorded canonical pool). Rows written by an M8 build
(before GC-F1) keep those values: there is no migration or backfill, and `rebuild` re-applies
events without re-decoding `venue_trades`, so only a re-ingest (a fresh database indexed from the
chain) gives them the pricing reserves. No schema change.

**Layouts are pinned against the venue programs the repository runs** (`tests/fixtures/programs/*.so`,
sha-checked): a decoder test replays a recorded fork session (§11.3: Raydium v4, CPMM, pump curve,
graduation onto PumpSwap) and checks every decoded trade of a short.win leg against the leg's own
short.win event (an open's sale `s` = the trade's net SOL; a close's `buyback_cost` = the buy's SOL
in). Field lists beyond those checks (pump's later `TradeEvent` fields) are decoded tolerantly:
known prefix, trailing bytes ignored.

### 7.3 Venue admin actions, latch flags, coverage

- **Latch flags:** short.win's own `VenueUpgradeFlagged` / `VenueRevalidated` (`venue_guards`), and
  the raw cause: BPF upgradeable loader `Upgrade` instructions targeting a venue program, from the
  `programdata:<venue>` streams → `venue_program_upgrades` (the time between an upgrade and its
  flag is a keeper-coverage metric).
- **Emergency and admin actions:** `PauseSet`, `VenueDisabled`, `RouteBrokenSet`, `VenueExtrasSet`,
  `Config*` (§6.6).
- **Venue admin state** (no events on Raydium; pump / PumpSwap have some): the refresher's watch of
  the fields the adapters read — PumpSwap `GlobalConfig.disable_flags` (bit 0 halts curve opens,
  `GraduationHalted`), fee configs and recipients, AMM v4 `AmmInfo.status` and fee fields, CPMM
  `PoolState.status` and `AmmConfig` rates — with `shortwin-venue-state`'s readers;
  `venue_admin_states` keeps every change (`old_value → new_value`, observed slot). *As built
  (M10):* the watch reads each Raydium market pool's `status` and, since GC-F1, its CPMM
  `AmmConfig`'s `trade_fee_rate`, `protocol_fee_rate`, `fund_fee_rate` and `creator_fee_rate`
  (`account` = the config); the writer reads `protocol_fee_rate` / `fund_fee_rate` back for the
  CPMM swaps' pricing reserves (§7.2). It watches no AMM v4 fee field and no pump / PumpSwap config
  field yet (IX-16). A field's first observation after a restart is compared with its latest stored
  row (`refresher::observe_admin_value`), so a restart records only real changes, with their old
  value.
- **Coverage:** `CoverageGap` and `CoveragePauseSet` (`coverage_events`), plus `invocations` for
  observation counts (§8.5).

---

## 8. Analytics

All amounts are lamports, all prices WAD; "mark" means `P_ema` (the market's EMA price, the same
reference the program sizes and settles on). Every formula has a Rust reference in
`analytics::formulas` and a SQL implementation in `0006_analytics.sql`; ANALYTICS owns keeping them
equal (tests in both). Windows use the chain clock `sw_chain_now()` (newest ingested block time), so a
replayed session's "24 h" is the session's.

### 8.1 Market state (per market, current — `v_market_overview`)

| Metric | Formula |
|---|---|
| Lendable inventory | `I = T + ΣB_Q` (raw token units) |
| Utilisation | `U_bps = ⌈ΣB_Q·10⁴ / (T + ΣB_Q)⌉`, 0 for an empty pool — exactly `rent::utilization_bps` (`sw_util_bps`) |
| Open interest | tokens `ΣB_Q`; at mark `OI = ⌊ΣB_Q·P_ema / WAD⌋`; notional at open `total_notional_open` (Σ `n_open`) |
| TVL | `⌊T·P_ema / WAD⌋ + lender_sol + Σ_open v` — assets custodied: unborrowed tokens at mark, SOL held for lenders, position vaults (margin + proceeds). Borrowed tokens are not counted (sold; their proceeds are in the vaults). `protocol_fees` (unswept) is reported separately |
| Lenders' claim | `⌊(T + ΣB_Q)·P_ema / WAD⌋ + lender_sol` |
| USD | `lamports · price · 10^expo / 10^9` at the latest `oracle_prices` row (`v_sol_usd`); null without one |

`P_ema` unknown (no crank yet): mark values use 0 and `price_known = false`.

### 8.2 Fees by type (`fees`, `v_market_fees`)

| `fee_type` | Recipient | Amount |
|---|---|---|
| `creation_fee` | protocol | `MarketCreated.creation_fee` |
| `open_fee` | protocol | `Open.fee_open` |
| `venue_rent` | venue | `Open.venue_rent` (PumpSwap `r`; not protocol revenue) |
| `rent_lender` / `rent_protocol` | lenders / protocol | `Close`: `rent − rent_protocol` / `rent_protocol`; `Liquidate`, `Expire`: `rent − ⌊rent·p/10⁴⌋` / `⌊rent·p/10⁴⌋` (`derived`) |
| `liq_penalty_lender/_protocol/_keeper` | lenders / protocol / keeper | `Liquidate.penalty_*` |
| `liq_keeper_topup` | keeper | `Liquidate.keeper_topup` (the N22 floor, paid from the borrower's remainder) |
| `expiry_fee` | lenders | `Expire.expiry_fee` |
| `expiry_keeper_reward` | keeper | `Expire.keeper_reward` |
| `settle_keeper_reward` | keeper | `SolSettle.k_s` |
| `trader_sweep_lenders/_wipe/_protocol` | lenders / lenders / protocol | `TraderSwept.lamports` by `to` |
| `venue_rewards_*` | as above | `VenueRewardsClaimed` (never emitted today) |

Protocol revenue = Σ recipient `protocol`; lender fee income = Σ recipient `lenders`; keeper income =
Σ recipient `keeper`. A SOL settlement's lender take is not a fee (it is the settlement value, §8.4);
its rent part is inside `lender_amount` and not separated. Zero amounts are not written.

### 8.3 Exits, liquidations, S1/S2/S3, creator exposure, slippage (`v_market_exits`, `v_market_creator`)

- **Counts by outcome:** closes; liquidations split CR (`expired = false`) / expired; expiries; SOL
  settlements by condition **S1 / S2 / S3** (`SolSettle.condition` 1/2/3); open. **Rates** (SPEC §13
  "close vs liquidation vs SOL-settlement rate") = count / exits.
- **Average duration** = mean `exit_ts − opened_ts` over exited positions.
- **Execution slippage** (SPEC §13): open impact `⌊(q·spot/WAD − s)·10⁴ / (q·spot/WAD)⌋` bps (0 when
  `s ≥` the spot value; `formulas::open_impact_bps`), and exit execution vs quote
  `buyback_cost / b_live − 1` for closes, liquidations and expiries.
- **Creator-wallet deposits (SPEC R1):** the lender row of `markets.coin_creator` (the token's
  creator wallet: pump `curve.creator` / PumpSwap `coin_creator`): shares, deposited tokens, share
  of `S` in bps. `markets.creator` (who listed the market) is shown too but confers nothing. A
  fee-sharing coin reports its sharing-config PDA as creator, not a wallet (disclosed).
- **Creator-fee income from short.win volume (C0-18):** `Σ venue_trades.fee_creator_lamports WHERE
  is_shortwin` per market, next to short.win's volume `Σ sol_amount WHERE is_shortwin` and, with
  third-party ingest, the coin's total creator fees.

### 8.4 Realised lender loss (SPEC §13) — `settlements`, `v_market_lender_loss`

SPEC §13: "SOL settlement value vs. token value at settlement". Per `SolSettle` of a position with
`q` tokens:

```
P_ref        = p_lo                          if p_lo = p_hi                        ('box_point')
             = clamp(P_last, p_lo, p_hi)     if a P_ema was seen before it          ('crank_clamped')
             = ⌊(p_lo + p_hi)/2⌋             otherwise                              ('box_mid')
token_value  = ⌊q · P_ref / WAD⌋
lender_pnl   = lender_amount − token_value          (signed)
realised_lender_loss = max(0, −lender_pnl)
```

- `P_last` = the market's last `P_ema` in chain order before the settlement (`PriceCranked.ema_price`
  or `Open.ema_wad`), `p_ref_age_s` its age. The settlement's own `P_ema` is not in the event, but
  the box always contains it (`P_lo = min(P_ema, P_mlo)`, `P_hi = max(P_ema, P_mhi)`, ARCH §3.8), and
  a degenerate box *is* `P_ema` — so clamping bounds the error by the box width and is exact in the
  degenerate case.
- `lender_amount` includes the settlement's rent and penalty/fee parts, exactly as the simulator's
  `lender_pnl = lender_sol − Q·p/WAD` (`docs/design/sim/shortwin_sim.py` `summarize`) and ECON §11.6's
  "lender P&L vs holding"; aggregates are therefore comparable with ECON's tables.
- Aggregates: Σ `realised_lender_loss`, Σ `lender_pnl`, count of settlements with a loss, loss in bps
  of Σ `token_value` and of Σ `n_open` (ECON reports % of `N_open`), by market, by condition, global,
  hourly (§8.6). Normal liquidations, closes and expiries return the tokens: no lender loss by
  definition.

### 8.5 Keeper coverage (`v_market_coverage`, `v_coverage_pauses`, `v_keeper_activity`)

- **The program's gap signal:** `CoverageGap` (a bucket integration found the EMA older than
  `COVERAGE_GAP_S` = 40 s with open positions; A69, M7-12): count, Σ `gap_s`, max, last 24 h. It is
  a log event: undercounted if logs truncate (`ingest_txs.logs_truncated` tells).
- **Coverage pauses (O17):** intervals from `CoveragePauseSet` paused → the next `CoveragePauseSet`
  of the market (ongoing until now); count and paused seconds.
- **Observations sent:** `invocations` of the observing instructions (`model::OBSERVING_INSTRUCTIONS`:
  crank_price, open_short, close_short, liquidate, expire, sol_settle, propose_pool,
  execute_pool_switch) naming the market — successful and failed — per window; this sees the keeper's
  no-op `sol_settle` calls that emit nothing.
- **Coverage ratio** (hourly, §8.6): `1 − gap_s / positions_open_s`, where `positions_open_s` = the
  seconds of the window with `open_positions > 0` (from `market_state_log`), and the largest interval
  between consecutive successful observations while positions were open (`max_observation_gap_s`).
- **Keepers:** per keeper wallet, liquidations / expiries / settlements from the positions it exited
  (`positions.exit_keeper`, so an exit with a zero keeper fee leg still counts) and rewards (`fees`
  with recipient `keeper`); failed attempts by instruction and error code from
  `ingest_txs.err_code` (`v_keeper_failures`; e.g. `UseSolSettle`, `ExitBoundNotMet`,
  `EmaWarmingUp`), served as `KeeperActivity.failures`.
- **Counters, not whole-table aggregates (M8-SEC-3).** The API never aggregates `invocations`,
  failed transactions or venue trades per request (the keeper alone writes ≈ 86,400 invocations per
  covered market-day, and anyone can add failed transactions at ≈ 5,000 lamports each or trades on
  their own pools): `market_observations_hourly` (observing invocations per market and hour),
  `keeper_failure_counts` (= `v_keeper_failures`'s counts) and `market_trade_totals` (= the venue
  sums of `v_market_creator`) are kept exact by statement-level triggers on the rows — every path
  that writes or deletes them (the writer, a rollback's cascade, the retention prune, a rebuild's
  re-attribution) moves them too (tested equal to the rows' aggregates). `sw_market_coverage(market)`
  reads one market only: its gaps and pauses through their indexes, its last 24 h as the complete
  hours from the counters plus the window's first, partial hour from `invocations (market,
  block_time)`; `v_market_coverage` is that function over every market.

### 8.6 Time series (`market_stats_hourly`, `global_stats_hourly`; `analytics::rollups`)

Hourly buckets `[h, h + 3600)` of chain time. **State columns** = the market's state after the last
`market_state_log` row at or before the bucket's end (carried forward through empty hours), priced at
the `P_ema` in force then (last `market_snapshots` / `Open` price ≤ the bucket's end):
`tokens_free, borrowed_q, total_shares, open_positions, total_notional_open, open_vault_lamports,
lender_sol, ema_price, util_bps, oi_lamports, tvl_lamports`. **Flow columns** sum the bucket: fees by
recipient, opens, closes, liquidations, expiries, S1/S2/S3, realised lender loss, settlement lender
P&L, notional opened, short.win volume, creator fees, deposits, withdrawals, coverage gaps and gap
seconds, `positions_open_s`, `coverage_paused_s`, observations, `max_observation_gap_s`.
`global_stats_hourly` sums the markets. The job runs every `rollup_interval_s`, recomputes buckets
idempotently (`ON CONFLICT DO UPDATE`) from `rollup_state.complete_before` to the last complete hour,
and resets `complete_before` on a rollback (§3.4) and on late data: any transaction written since the
last pass (`ingest_txs.seq` > `rollup_state.last_tx_seq`) whose block time falls before
`complete_before` — with or without an event (a no-op observation, a failed keeper attempt, a
third-party trade change their hour too, M8-SEC-14). Daily views aggregate hours (flows summed, state
from the day's last hour).

### 8.7 Global (`v_global_stats`) and SPEC §13

| SPEC §13 metric | Source |
|---|---|
| Lendable inventory | Σ `inventory_tokens` per market (tokens are per mint; the global figure is at mark: Σ lenders' claim) |
| Utilisation | per market `util_bps`; global = OI at mark / Σ inventory at mark |
| Unique lenders / borrowers | `count(DISTINCT owner)` / `count(DISTINCT borrower)` |
| OI | Σ `oi_lamports`, Σ `total_notional_open` |
| Rental fees | Σ `rent_lender` + `rent_protocol` |
| Protocol revenue | Σ fees with recipient `protocol` |
| Avg duration | mean `duration_s` |
| Close vs liquidation vs SOL-settlement rate | §8.3 |
| Realised lender loss | §8.4 |
| Avg execution slippage | §8.3 |
| Repeat lenders / borrowers | owners with ≥ 2 deposits / borrowers with ≥ 2 positions |

### 8.8 Views index

`v_sol_usd`, `v_market_overview`, `v_market_fees`, `v_market_exits`, `v_market_lender_loss`,
`v_market_creator`, `v_coverage_pauses`, `v_market_coverage` (over `sw_market_coverage`),
`v_keeper_activity`, `v_keeper_failures`, `v_global_stats`, `v_market_stats_daily`,
`v_global_stats_daily`; the counters `market_observations_hourly`, `keeper_failure_counts`,
`market_trade_totals` (§8.5). They are the
API's read model for current values; `/v1/*/stats` and `/v1/stats/timeseries` read the hourly
tables. SQL helpers with Rust twins (tests in `tests/analytics_views.rs`): `sw_mul_wad`,
`sw_util_bps`, `sw_open_impact_bps`, `sw_exit_slippage_bps`, `sw_shares_at_epoch`, `sw_observing`,
`sw_chain_now`; the API's `sw_api_venue/tier/guard/condition` name functions (`0007_api.sql`, pinned
to `model::*` by a test) and its chain-order indexes `venue_trades_chain_order`,
`settlements_chain_order` (additive, on INGEST's tables).

**Served (as integrated).** Besides the v1.0 fields, the API serves ANALYTICS' additive columns:
`ExitCounts` close / liquidation / expiry / settlement rates (bps of exits), mean open impact and
exit slippage, `borrower_pnl`; `LenderLoss` realised loss by condition (S1, S2, S3);
`MarketDetail.inventory_value_lamports`; `GlobalStats` `inventory_value_lamports`, global `util_bps`,
`rental_fees`, mean open impact and exit slippage; `KeeperActivity.failures`; and the time-series
metrics `markets`, `lender_sol`, `inventory_value`, `notional_opened`, `settlement_lender_pnl`,
`deposits`, `withdrawals`, `positions_open_s`, `coverage_paused_s`, `observations`. Not served (read
in SQL only): `v_market_creator.venue_creator_fees_lamports` and `listed_by` (= `MarketDetail.creator`),
`sw_shares_at_epoch` (the API converts `share_bps` in Rust).

---

## 9. API

### 9.1 Conventions (normative detail in `openapi.yaml`)

- **Versioned paths** `/v1/...`; JSON; `application/problem+json` errors with `code ∈ {bad_request,
  invalid_pubkey, invalid_signature, invalid_cursor, limit_out_of_range, range_too_large, not_found,
  rate_limited, timeout, unavailable, internal}`.
- **Numbers:** §4.2 (u64/u128/i128 as strings; slots and seconds as numbers).
- **Pagination:** keyset only. `limit` 1–500 (default 100; `[api] default_page_limit` /
  `max_page_limit`, configurable within 1–500 and 200–500; a request above `max_page_limit` is
  `limit_out_of_range`). The web asks for up to 200 a page (a borrower's positions; 100 for a
  market's events, 50 for a claim's sync candidates, FRONTEND §3.6) and does not retry with a
  smaller one, so `max_page_limit` must be at least 200: the config check refuses to start below
  it (`WEB_MIN_PAGE_LIMIT`; the owner's decision of 2026-10-09, FE-30). `cursor` =
  URL-safe base64 of the last row's sort key (e.g. `(slot, tx_index, ix_index, ev_source, ev_seq)` for events, `(sort value,
  market)` for markets), validated on decode (`invalid_cursor`); body `{data: [...], next_cursor}`.
- **Headers:** `X-Indexer-Slot`, `X-Indexer-Finalized-Slot`, `RateLimit-Limit/-Remaining/-Reset`;
  `Retry-After` on 429/503; `Cache-Control: public, max-age=2` on lists, `max-age=3600` on a
  finalized `/v1/tx/{signature}`.
- **Markets are keyed by mint** (R-MINT §10.11); search resolves symbols and names to mints.
- **Status codes (as built):** every database-backed operation documents `503` (`unavailable`: the
  database is unreachable or the query semaphore is saturated for > 1 s; `timeout`: the statement
  or the request timed out) with `Retry-After`; `/v1/health` never touches the database. The API is
  GET-only: any other method answers `405` (problem+json, code `bad_request`). An optional
  `X-Api-Key` (the `ApiKey` security scheme) lifts the anonymous limit; an unknown or disabled key
  is served as anonymous, never 401.

### 9.2 Endpoints → read model

| Endpoint | Reads |
|---|---|
| `GET /v1/health`, `/v1/status` | process; `ingest_state`, `ingest_cursors`, `rollup_state`; the data-quality counts `quarantined`, `layout_warnings`, `missing`, `logs_truncated` (last 24 h) — partial indexes, bounded reads |
| `GET /v1/markets` (q, venue, tier, has_positions, sort, order) | `v_market_overview` + `v_sol_usd` |
| `GET /v1/markets/{mint}` | `v_market_overview`, `v_market_fees`, `v_market_exits`, `v_market_lender_loss`, `v_market_creator` (`market_trade_totals`), `sw_market_coverage(market)`, `protocol_params_history` (tier terms), `ingest_quarantine` (`quarantined_events`) |
| `GET /v1/markets/{mint}/positions` | `positions` |
| `GET /v1/markets/{mint}/lenders` (active; *M10, FE-30:* `epoch` = `behind` / `current`: `lender_positions.epoch` before / at `markets.share_epoch`, same order and cursor; the web's claim reads the behind lenders first, FRONTEND §3.6) | `lender_positions` ⋈ `markets` (+ `share_epochs` for `share_bps`) |
| `GET /v1/markets/{mint}/pools` | `pools`, `pool_events`, `markets.fp_*` |
| `GET /v1/markets/{mint}/coverage` | `sw_market_coverage(market)`, `coverage_events`, `v_coverage_pauses` |
| `GET /v1/markets/{mint}/liquidations`, `GET /v1/liquidations` | `positions` (liquidated / expired / settled) + `settlements` |
| `GET /v1/markets/{mint}/events` | `events` |
| `GET /v1/markets/{mint}/snapshots` | `market_snapshots` |
| `GET /v1/markets/{mint}/trades`, `/candles` | `venue_trades` of the market's pools (`market` set, §7.1; candles aggregated on read; ≤ 1,000 buckets) |
| `GET /v1/markets/{mint}/stats`, `/fees` | `market_stats_hourly` / `v_market_stats_daily`; `v_market_fees` |
| `GET /v1/positions/{pubkey}` | `positions`, `events` (by position), `settlements` |
| `GET /v1/borrowers/{owner}/positions` | `positions` |
| `GET /v1/lenders/{owner}`, `/events` | `lender_positions`; `events` (by actor, lender kinds) |
| `GET /v1/stats`, `/v1/stats/timeseries` | `v_global_stats` (served from memory for 2 s, its `max-age`); `global_stats_hourly` / `v_global_stats_daily` |
| `GET /v1/settlements`, `GET /v1/keepers` | `settlements`; `v_keeper_activity` + `keeper_failure_counts` (per listed keeper) |
| `GET /v1/admin/config`, `/changes`, `/events` | `protocol_params_history` + `protocol_state`; `config_changes`; `events` (admin kinds) |
| `GET /v1/venues` | `protocol_state`, `venue_guards`, `venue_extras`, `venue_admin_states` |
| `GET /v1/search` | `markets` ⋈ `token_metadata` (exact mint, then symbol prefix, then name prefix; ≤ 20) |
| `GET /v1/tx/{signature}` | `ingest_txs`, `events`, `invocations`, `venue_trades` |
| `GET /v1/ws` | §9.3 |

### 9.3 WebSocket (`/v1/ws`)

- **Protocol:** JSON text frames. Client: `{"op":"subscribe","channels":[…],"since":<event id>?}`,
  `{"op":"unsubscribe","channels":[…]}`, `{"op":"ping"}`. Server: `subscribed` / `unsubscribed`,
  `{"type":"event","channel","id","data":Event}`, `{"type":"market"|"trade"|"stats","channel","data"}`,
  `{"type":"rollback","slot"}`, `pong`, `heartbeat` (every 20 s), `error`.
- **Channels:** `markets` (a `MarketSummary` whenever a market's projection changes, coalesced to ≤ 1
  per market per second), `market:<mint>` (every event of the market), `position:<pubkey>`,
  `lender:<owner>`, `liquidations` (`Liquidate` / `Expire` / `SolSettle`, all markets), `admin`
  (§6.6 kinds), `coverage` (`CoverageGap` / `CoveragePauseSet`), `trades:<mint>` (venue trades),
  `stats` (`GlobalStats` every 10 s).
- **Fan-out:** the writer's `pg_notify('shortwin_events', <max events.id>)` (delivered at commit,
  §3.1); each API process holds one `LISTEN` connection, reads `events WHERE id > last` in id order,
  routes by channel through a broadcast channel. **Resume:** `since` replays up to 1,000 events with
  `id > since` from the database before live ones (`id` is the ingestion sequence: increasing in
  commit order because of the single writer; a retried transaction can leave gaps, which resumption
  tolerates). **Rollback:** `pg_notify('shortwin_rollback', <slot>)` → a
  `rollback` message on every channel; clients refetch.
- **Back-pressure:** per connection an outbound buffer of `ws_outbound_buffer` (1,000) messages; when
  full the connection is closed with code **4008** (and an `error{code: "lagged"}` frame first).
- **As built:** one hub per process, started by the first connection, with a 1 s poll fallback next
  to its `LISTEN`; `since` replays only the **newly subscribed event channels** (`markets`,
  `trades:` and `stats` ignore it), at most 1,000 events, and suppresses live events at or below the
  replayed ids — past the cap the client pages through REST (API-4); `trades:<mint>` follows
  `venue_trades` in chain order from the newest trade at its first subscriber, so trades gap-filled
  below that watermark are not pushed, and a rollback lowers the watermark to the rolled-back slot
  (API-5); the `rollback` message (no `channel` field) goes once to every connection with a
  subscription; an inbound-rate violation is a policy close (1008) after the queued replies, a lagged
  close (4008) drops the backlog. `ws_heartbeat_s` (20) and `ws_stats_interval_s` (10) are `[api]`
  keys.
- **The replay is a database read (M8-SEC-4):** it costs the client 5 requests of its GCRA bucket
  (`error{rate_limited}` when they do not fit), waits at most 1 s for a query permit
  (`max_concurrent_queries`; `error{unavailable}` otherwise), and reads each newly subscribed channel
  through an index — `events (market, id)`, `(position, id)`, `(actor, kind, id)`, `(kind, id)` — at
  most 1,000 ids per channel, then the lowest 1,000: never a scan of `events`, whatever `since` is.
  Neither error ends the subscription. A process holds at most `ws_max_conns_total` (10,000)
  connections (503 at the upgrade, M8-SEC-8).

---

## 10. Rate limiting and abuse limits

### 10.1 Limits (defaults in `[api]`)

| Limit | Anonymous (per client address) | `partner` key (`api_keys`) | `internal` key |
|---|---|---|---|
| Requests | GCRA 20 req/s, burst 40 | 100 req/s, burst 200 | unlimited (still query-bounded) |
| Route cost | 1 token; `/candles`, `/stats/timeseries`, `/search`, `/keepers`, `/markets?q=` (the decoded name: `%71=` too, M8-SEC-10) and a WebSocket `since` replay cost 5 | same | — |
| WebSocket connections | 5 | 50 (`ws_conns`) | 500 |
| WebSocket connections per process | `ws_max_conns_total` (10,000), all clients together | same | same |
| Subscriptions per connection | 50 | 50 | 500 |
| Inbound WebSocket messages | 10/s, then `error{rate_limited}`, then close | same | same |

Client address = the TCP peer, or the last `X-Forwarded-For` hop not in `trusted_proxies`; IPv6
clients are bucketed by `ipv6_bucket_prefix` (/64 by default; /56 or /48 make one allocation one
client, §14 API-9). Behind a load balancer `trusted_proxies` must list it — otherwise every client
is the balancer's address, one bucket and one WebSocket allowance for everyone: `serve` warns at
start when the list is empty, and once per process on an `X-Forwarded-For` from an untrusted peer;
the reference deployment ships the balancer's address (§12.1, M8-SEC-8). API keys travel in
`X-Api-Key`; only their SHA-256 is stored (looked up with a 60 s cache; a cache miss first costs
the address one anonymous request and takes a query permit, so random keys buy no free database
lookups; known and unknown keys are cached apart, and a flood of random keys evicts only other
unknown keys, least recently used first). 429 responses carry `Retry-After`. **`/v1/health` and
`/v1/status` are not rate-limited** (probes; the document lists no 429 for them; API-1): they read
cached keys only — an uncached key is never looked up on a probe (M8-SEC-9) — and `/v1/status`
takes a query permit. Unknown routes are rate-limited.

### 10.2 Query safety

- Every list is keyset-paginated (no OFFSET); `limit ≤ 500`; time ranges ≤ 90 days for hourly series,
  ≤ 5 years daily, ≤ 1,000 buckets per response (`range_too_large` otherwise).
- `statement_timeout` = 2 s for the API role (set per role, §12.1, and per connection);
  `max_concurrent_queries` (32) per process behind a semaphore (`503 unavailable` when saturated for
  > 1 s) — REST requests, key lookups and WebSocket replays alike; request timeout 5 s.
- **No request aggregates an attacker-inflatable or organically unbounded table (M8-SEC-3):** a
  market's page and its coverage read that market's rows through indexes and the trigger-kept
  counters (§8.5); `/v1/keepers` reads the failure counters of the keepers it lists; `/v1/stats` is
  served from memory for 2 s. `cost_one_pages_stay_fast_at_thirty_days_of_keeper_observations`
  (`tests/security_api.rs`, CI) holds the market, coverage, keepers, stats, trades and list pages
  under 750 ms at 30 days × 86,400 observations of one market plus 100,000 failed transactions and
  100,000 trades on a pool that is no market's (the review measured 2 s timeouts at 14 days).
- Inputs are parsed before SQL: keys and signatures by base58 decoding; enums by allowlist; `q` ≤ 64
  chars, matched with prefix `LIKE` on `lower()` columns, `%`/`_` escaped. Sort columns and channel
  names come from fixed allowlists; no request text is ever spliced into SQL.
- The API role is read-only (`default_transaction_read_only = on`); `api_keys` holds only SHA-256
  hashes of the keys.

### 10.3 Abuse of the data plane

- **Junk markets** (permissionless listing; ≈ 0.035–0.047 SOL each): lists default to `sort=tvl`;
  `/v1/markets` filters out markets with neither positions nor lenders unless `q` matches them;
  token metadata strings are length-capped (symbol 16, name 64, uri 200) and served as text, never
  HTML.
- **Forged logs:** §5.1 (attribution by the invocation stack).
- **Forged trades:** a venue trade belongs to a market only on the market's pools (§7.1, M8-SEC-6).
- **Undecodable payloads:** quarantined (§3.6), never dropped, alerting.
- **WebSocket fan-out amplification:** channel caps (§10.1), coalesced `markets` updates, the
  outbound buffer (§9.3).

---

## 11. Testing

### 11.1 The embedded-Postgres harness (`testing::pg`, feature `embedded-pg`)

- `TestPg::start_in(root)` runs **PostgreSQL 17.11** as the current user through
  `postgresql_embedded`, from the **zonky** builds (Maven Central `io.zonky.test.postgres`, ~15 MB,
  downloaded once into `~/.cache/shortwin/postgresql-zonky`, overridable with
  `SHORTWIN_PG_INSTALL_DIR`). The zonky builds bundle libxml2, OpenSSL, ICU and zlib and need only
  glibc; the theseus builds (the crate's default) link the system `libxml2.so.2`, which current
  distributions (libxml2 ≥ 2.14, soname 16) no longer ship. No Docker, no root, no system Postgres.
- Data lives under `root` (integration tests pass `env!("CARGO_TARGET_TMPDIR")`, i.e.
  `services/target/tmp`; `SHORTWIN_PG_TMP` overrides), `fsync`/`synchronous_commit`/`full_page_writes`
  off, a random port on 127.0.0.1, removed on drop (`pg_ctl stop -m fast`, then the directory).
- `fresh_db()` creates a uniquely named database and applies `db::MIGRATOR`; tests take their own
  database, so they run in parallel against one server or several.
- The gate test `tests/pg_fixture.rs` starts a server, migrates, checks every table and view of the
  contract exists, the domains enforce their ranges, the SQL helpers agree with the Rust formulas
  (~3 s once the binaries are cached).
- The crate's own tests get the feature through a self dev-dependency (`shortwin-indexer = { path =
  ".", features = ["embedded-pg"] }`); the production binary never links `postgresql_embedded`.

### 11.2 API contract tests (`services/indexer/tests/api_*.rs`; API track)

- The router (`api::router`) is driven as a tower service (`tower::ServiceExt::oneshot`, bodies read
  with `http-body-util`); WebSocket tests bind 127.0.0.1:0 and connect with `tokio-tungstenite`.
- `openapi.yaml` is parsed with `serde-saphyr`; for every `operationId` the tests call the endpoint on
  a seeded database (API's own SQL seed `tests/data/api_seed.sql` until replayed sessions exist,
  then the replayed sessions of §11.3) and validate status, `Content-Type` and the body against the
  response schema with `jsonschema` (2020-12, `#/components/...` refs resolved against the document,
  no network). Every documented error (400 per invalid parameter class, 404, 429) is exercised; the
  rate limiter is tested with a tightened config.
- A test asserts the router serves exactly the paths of `openapi.yaml` (no undocumented route, no
  missing one).
- **As built** (`api_contract`, `api_limits`, `api_ws`, helpers in `tests/api_support/`):
  `every_operation_conforms_to_openapi` produces every documented (operation, status) pair but the
  WebSocket's 101 (`api_ws`'s) — 503 `unavailable` on every database-backed operation with the pool
  closed — and fails if one is never produced; the document must read the same under YAML 1.2 and
  YAML 1.1 (no bare `y`/`n`/`on`/`off` scalars); ~150 assertions on the seeded values; every list
  pages without gaps or repeats at limits 1–3 and cursors are endpoint-bound.
- **On replayed sessions (join 2; `tests/api_replayed.rs`):** every committed session (§11.3) is
  ingested through the real pipeline and rolled up, then every REST operation is called for every
  market, position, borrower, lender and keeper the session has, every enumerated filter and series
  interval, and validated against the document (≈ 1,800 responses); every list pages identically at
  limit 7 and 500; `/v1/markets/{mint}/events` serves every event of the market; each `Market`,
  `MarketVault`, live `ShortPosition` and `LenderPosition` of the session's final checkpoint equals
  what the API serves (48 accounts), a closed position is served exited, and each market's open
  positions list has `Market.open_positions` rows.

### 11.3 Recorded sessions and the replay source

- **Format** (`ingest::session`, frozen): JSON Lines — a header (`version`, program ids, recorder,
  scenario, build), transactions as `getTransaction` returns them (flat RPC shape: `slot`, `blockTime`,
  `transactionIndex`, `transaction: [<base64>, "base64"]`, `meta` with `innerInstructions` (base58
  data, `stackHeight`), `logMessages`, `err`, `fee`, `computeUnitsConsumed`, `loadedAddresses`,
  `version`), and account checkpoints (`after` = the signature it follows, every tracked account as
  base64, closed keys).
- **Recorder hook** (REPLAY track; `tests/litesvm/src/record.rs`, declared in the harness's
  `lib.rs`): `Env::start_recording()` / `start_recording_with(CheckpointPolicy::{Manual, EveryTx,
  AfterEvents})`; `Env::take_recording() -> Recording { txs: Vec<RecordedTx>, checkpoints:
  Vec<(usize, AccountCheckpoint)> }`, `RecordedTx { tx: VersionedTransaction, loaded_writable,
  loaded_readonly, slot, unix_timestamp, tx_index, pre_balances, post_balances, result }`;
  `Env::account_checkpoint()` (every short.win-owned account plus the `record_track` keys, and the
  tracked keys that no longer exist); `Env::execute_recorded(tx)` — the one place a chain
  transaction executes: `Env::send` (both paths), `Env::send_v0` (`raydium.rs`), the keeper
  backend's `send_with_hooks` and bundle path (`services/keeper-litesvm`); `Env::recording_mark()` /
  `recording_rollback(mark)` for a failed bundle (it commits nothing). Only failures a cluster
  commits are recorded (instruction errors, rent, a restricted program); prefix replays on VM
  clones and simulations never are. `tx_index` is a per-slot counter (it panics if the clock goes
  back). `AfterEvents` (the default) snapshots after every transaction with a short.win `emit_cpi!`
  event other than `PriceCranked`. `shortwin-indexer-litesvm::recorder` renders a recording as
  `getTransaction` JSON (wincode wire bytes — v1 as the message, then the header's signatures, as
  `to_raw_tx` reads it —, compiled inner instructions with `stackHeight`, logs, `err`, fee,
  balances, `loadedAddresses`), keeping only the transactions that mention short.win or
  (until pump-final P6) complete_curve (what `getSignaturesForAddress(program)` returns; block
  positions keep their gaps).
- **Sessions as built.** REPLAY's nine, recorded at test time from the current builds (so they never
  go stale; `services/indexer-litesvm/src/scenarios.rs`), and INGEST's thirteen committed fixtures:

| Session | From | Covers |
|---|---|---|
| `mock_lifecycle` (committed, thinned to its first and last checkpoints) | a scripted `Env` scenario on the mock venue, 966 transactions, 74 checkpoints (*AIR-07:* 836 and 62 before M5–M7, `590e724`; the committed copy re-recorded on the AI review's build in `5c4488f`, with `mock_admin`) | S1, S2, S3, CR liquidation, expiry, a wipe with a donation and a trader sweep, a flushing rebase whose pre-funded record's donation vests anew (M5), a vest flush without an epoch roll (M6), the last shares leaving a running vest (M7), every lender instruction, the Emergency's powers, the latch, the unsafe-mint flag |
| `mock_admin` (committed, thinned) | `Env` on the mock | every `ConfigChange` kind through the timelock, a cancel, locked terms across a `Params` change, the coverage pause and the Emergency's clear, extras, an arm then a disarm |
| `keeper_liquidation`, `keeper_coverage_pause` | `services/keeper-litesvm` (`Harness`) | the keeper's v1 / v0 batches; the O17 drill with the coverage key installed through the real timelock |
| `raydium_lifecycle`, `pool_switch` | the Raydium fork | AMM v4 and CPMM, a close, keeper liquidations on both, S3 on an admin-halted pool; `PoolProposed`, `PoolSwitchCancelled`, `PoolSwitched` |
| `pump_synthetic_exit` | the pump fork | the borrower's own close past `rT` (pump v3's synthetic migration: two trade rows, `CompleteEvent`, `VenueStateChanged → pump_migrating`); its thinned copy is INGEST's `pump_synthetic_exit.jsonl` (`SHORTWIN_RECORD_SESSIONS=<dir>` writes it to `<dir>/ingest/`) |
| `pump_graduation`, `pump_graduation_raced` | the pump fork + the keeper (since pump-final P4 without the straddle bundle) | curve open and close, the keeper's synthetic expiry past `rT` (`VenueStateChanged`), `migrate_v2` and the transition, an exit on PumpSwap; raced, a third party's synthetic completion fails the keeper's expiry |
| `tests/data/ingest/*.jsonl` (13, INGEST's) | LiteSVM scenarios of `tests/litesvm` and `keeper-litesvm`; since pump-final P6 `pump_synthetic_exit` is `indexer-litesvm`'s (it replaces `pump_straddle_graduation`, and `curve_completed_and_already_complete` left with the helper's ingestion) | governance and pauses, the borrower's synthetic exit past `rT`, the prefunded `ShareEpoch` record (7,000,000 lamports), expiries / flags / latch, the keeper's degraded mode (v1) and straddle graduation (v1 + v0), lender sync and the unsafe mint, margin top-ups, Raydium's fast path and venue review, a pool switch then a liquidation, settle arming, trader sweeps, venue extras |

  Across the set every event kind of §5.3 appears except `VenueRewardsClaimed`, which the program
  never emits. Regenerate REPLAY's with `SHORTWIN_RECORD_SESSIONS=<dir> cargo test -p
  shortwin-indexer-litesvm`, then copy `<dir>/committed/*.jsonl` (not byte-reproducible: random
  keypairs). One of INGEST's fixtures, `expiries_flags_and_latch.jsonl`, holds a resent duplicate
  transaction (a test artifact; ingested once, idempotently; until pump-final P6 the deleted
  `pump_straddle_graduation.jsonl` held two). INGEST's fixtures other than `pump_synthetic_exit`
  have no recording procedure (sessions recorded before pump-final P6 also carry a
  `complete_curve_id` header field, which is ignored); a layout change migrates them in place: `SolSettle.vest_flushed` (`cb756ca`), and AIR-01's `revalidate_venue`
  `expected_elf_hash` (`caa5852`: an all-zero hash, the old exact-slot check, appended to the one
  call in `raydium_fast_path_venue_review.jsonl`, whose recorded signature therefore no longer
  signs its message; nothing verifies it).

Surfpool sessions (a `recorder = "surfpool"` header, transactions fetched by signature from the local
validator) are optional and use the same format.

### 11.4 DB-vs-chain assertions (`shortwin-indexer-litesvm::assert`)

For each session: ingest into a fresh database **up to each checkpoint** and assert:

1. **Markets:** every [E] column of §6.2 equals the checkpoint's `Market` (`tokens_free`,
   `borrowed_q`, `borrowed_q_by_tier`, `total_shares`, `share_epoch`, `open_positions`,
   `total_notional_open`, `max_q_hwm`, `venue`, flags, `coverage_cleared_at`, the Raydium pool cache's
   pool and `pool_d`, `fp_*`); [E\*] columns where the market's last observing invocation emitted them.
   `vest_bal` ([E\*]): 0 wherever the checkpoint's vest is flushed (`vest_bal = 0` with
   `vest_end > vest_last`), and equal to the harness model's at every checkpoint
   (`assert::vest_vs_model`; AIR-07).
2. **Vaults:** `lender_sol`, `protocol_fees` equal `MarketVault`'s (the §6.4 ledgers).
3. **Positions:** every open `ShortPosition` ↔ a `positions` row with status `open` and equal `q, s,
   v, n_open, fee_open, deadline, tier, s_armed/s_count/s_since`, `m` = `positions.m +
   margin_added` (§6.5) and locked terms; every closed tracked position ↔ a row with the right exit
   status; `open_vault_lamports` = Σ open `v`.
4. **Lenders:** every `LenderPosition` ↔ `lender_positions` (`shares, epoch, lock, lock_until`); closed
   ones ↔ shares and lock 0. **Share epochs:** every `ShareEpoch` record ↔ `share_epochs`.
5. **Admin:** `GlobalConfig` ↔ the latest `protocol_params_history` + `protocol_state`; `VenueGuard` ↔
   `venue_guards`; `VenueExtras` ↔ `venue_extras`.
6. **Ledger identities:** per market `protocol_fees = Σ fees(protocol) − Σ protocol_sweeps`;
   `lender_sol = Σ lender inflows (§6.4, the record donation from `events.derived`) − Σ sol_paid`,
   the donation also checked against the session's System transfers; Σ fees by type over the
   session equal the events' fields.
7. **Analytics:** `v_market_overview.tvl_lamports`, `oi_lamports`, `util_bps` equal
   `formulas::*` evaluated on the checkpoint's accounts (with the projection's `P_ema`); every
   settlement's `token_value`/`lender_pnl`/`realised_lender_loss` equal `formulas::settlement_outcome`
   recomputed; the rollups (`rollups::run_once`) over the session reproduce the views' end-state.
8. **Idempotence:** re-ingesting the whole session writes nothing (`txs_written = 0`, identical
   table dumps).
9. **Rebuild:** `ingest::rebuild(None)` leaves every projection table identical.
10. **Out of order:** ingesting the second half before the first (a gap-fill) then rebuilding gives
    the same database as in-order ingest.
11. **Rollback:** ingest at `confirmed` and roll up; the finality tracker (`finality::run_once`
    with a stub cluster that finalizes the first n − k and knows nothing of the last k) promotes
    and rolls back through the §3.4 path; the refresher's role re-reads the accounts (the
    checkpoints up to the cut); roll up again — the database **and the hourly rollups** equal one
    that ingested only the first n − k. (Without `rollups::reset_from` in the rollback the rollups
    differ: checked by mutation.)
12. **Coverage of the catalog:** across the session set, every event kind of §5.3 that the program
    emits appears at least once, and nothing is quarantined.
13. **The API's read model** (`assert::read_model`, the API track's assumptions): exit events carry
    `actor` = their keeper and `position`, and have a `positions` row; position events carry
    `position`; lender events `actor` = owner; cancelled changes name `cancelled_by`; venue trades
    with both amounts have `price_wad`.

This is the M8 exit criterion "replays an E2E session into correct DB state" (§13.5). As built,
`assert::replay_session` runs items 1–13 on each of the eight sessions through
`ingest::ingest_session` / `ingest::rebuild` / the finality tracker (no skip path: the pipeline is
built); the assertions are also run on the harness's own reference writer (`seed.rs`), where a
tampered row is caught by each assertion family (a stale `vest_bal` on a flushed market
included). The reference model (`chain::check_session`, `Model::check_against`) compares its
`vest_bal` with the chain
in two cases: right after the event that set it (the market's last event, in the last
transaction applied: a crank's snapshot, a reported flush, or a flushing rebase's record
donation), and wherever the chain's vest is flushed, where it must be 0.

### 11.5 Decoder unit tests (INGEST)

Synthetic transactions built in Rust (Borsh-serialised `shortwin::events::*` behind
`EVENT_IX_TAG_LE`, crafted log streams) for: every event of the catalog; attribution at stack heights
1–4; a forged "Program data:" line from another program (rejected); a CPMM-discriminator event from
CLMM (rejected); `Log truncated`; failed transactions; v0 lookup-table and v1 key resolution; the
`ConfigChange` hash check. Then the committed sessions of §11.3 end-to-end through `to_raw_tx`.
`the_committed_sessions_decode_with_the_current_layouts` (no database): every committed session
decodes with no short.win payload refused, so a layout change (an appended event field, as
`SolSettle.vest_flushed`) must migrate or re-record the fixtures.

**On recorded mainnet transactions (M10 GC-F1).** `tests/venue_mainnet.rs` runs the gap-calibration
crawler's recorded mainnet transactions (`scripts/gapcal/tests/fixtures`, about 20 per venue, each
venue's from one pool) through `to_raw_tx`, the decoders and, for CPMM, the writer:
- CPMM: 20 consecutive swaps of one pool chain exactly in `venue_trades`, with creator fees on both
  sides; `vault − reserves` grows by exactly the protocol and fund shares plus the creator fee; the
  watched rate at or before the slot is selected; with the rates zeroed (M8's formula) every link
  breaks by exactly the counters' share.
- PumpSwap: the effective reserves (vault + VQR) price every sale exactly, and VQR has one value
  (17,600,247,874 on `4JAnKF…`) after every buy and sell of 20 transactions (slots 452,463,723 to
  452,464,091).
- AMM v4: the `ray_log` reserves are vault − `need_take_pnl`, and chain.
- pump: the curve's virtual reserves chain by each trade's SOL and token amounts.

### 11.5a The security review's regressions (M8-SEC-1 … -15)

The M8 security review's reproducers, turned around (each fails on the code the review found and
passes on the fix): `tests/security_ingest.rs` (a mock JSON-RPC node in axum serving the committed
`mock_lifecycle` session — an unserved or erroring `getTransaction` stops the backfill before it,
short pages and unknown bounds, the live `ingest_missing` retries; stub clusters for the finality
tracker's decision table and the cursor reset; the disabled venue across `Kink` / `EnableVenues` and a rebuild; trades on pools that
are not the market's; longer and broken event layouts with `/v1/status`; late eventless rows and
the rollups), `tests/security_api.rs` (random keys on the probes, the percent-encoded search, the
reference deployment's proxy and the WebSocket process cap, the replay's index reads / charge /
semaphore on 300,000 events, the trigger-kept counters through rollbacks, prunes and
re-attribution, and the **load test**: 30 days × 86,400 observations of one market plus attacker
rows, every former whole-table page under 750 ms), the redaction unit tests (`ingest::rpc`), the
deployment-config test (`config`), and two validator tests in `ingest_rpc_validator` (history older
than the status cache survives the tracker; a rolled-back newest signature does not halt `live`).
`mock_admin` (§11.3) disables a venue before an unrelated `Kink` change, so assertion 5 checks the
bit stays cleared.

### 11.6 Gates (CI's `services` job runs them all)

`cargo build/clippy --workspace --all-targets --locked -D warnings`, `cargo fmt` (the new crates are
in the CI line), `cargo deny`, `cargo audit` (CI `services`); the tests run in CI's own **`indexer`**
job: the fixture first (it downloads the zonky binaries on a cold cache; cached by
`actions/cache`), `cargo test -p shortwin-indexer` (unit and decoder tests, ingest, analytics, the
API contract on the seed and on replayed sessions, limits, WebSocket), `cargo test -p
shortwin-indexer-litesvm` (the replay harness, with the program artifacts of `anchor-build`), the
RPC paths against `solana-test-validator` (`ingest_rpc_validator --ignored`), a check that the
production indexer's dependencies hold no `postgresql_embedded`, and `docker compose config` on the
reference deployment.

---

## 12. Deployment and operations

### 12.1 Reference deployment (`services/indexer/deploy/`)

- `docker-compose.yml`: `postgres:17.11` (volume, health check), `migrate` (one-shot, ingest role),
  `ingest` (`live`; **exactly one**), `rollups`, `api` (`serve`, 2 replicas, port 8080). Secrets are
  files under `deploy/secrets/` (git-ignored); `SHORTWIN_RPC_URL` / `SHORTWIN_WS_URL` from the
  environment. First start: `docker compose run --rm ingest backfill`, then `docker compose up -d`.
- `Dockerfile`: builds `shortwin-indexer` from the repository root (path dependencies), runs as an
  unprivileged user on `debian:bookworm-slim`. `Dockerfile.dockerignore` (BuildKit's per-Dockerfile
  ignore file) keeps `target/`, `node_modules/`, `.git`, `keys/` and every `*-keypair.json`, the
  secrets and `.env` files out of the build context. The `api` service's two replicas publish host
  ports 8080–8081 **on 127.0.0.1 only** (a load balancer on the host in front), and the compose
  network has a fixed subnet (`172.30.0.0/24`) so the address the API sees the balancer from — the
  gateway `172.30.0.1` — is the `[api] trusted_proxies` entry `deploy/indexer.toml` ships (a unit
  test holds the two files together; M8-SEC-8). A balancer on another machine: bind the ports to the
  private interface it uses and list its address instead.
- `initdb/01-roles.sh`: `shortwin_ingest` (schema owner: migrations, writer, rollups) and
  `shortwin_api` (read-only, `statement_timeout = 2s`, default privileges grant SELECT on everything
  the ingest role creates).
- **Not run here** (no Docker on the development machine); CI validates the compose file (`docker
  compose config`) and the roles script's syntax; the tested database path is §11.1. A managed
  Postgres works the same (TLS through sqlx's rustls). Both the writer (its session-level advisory
  lock, §3.1) and each API process's `LISTEN` connection (§9.3) need session-mode connections; a
  PgBouncer in transaction mode may front only the API's ordinary queries.
- Sizing (first estimate): the keeper's per-second calls dominate `invocations` (≈ 86,400 rows per
  covered market-day) — hence the 30-day retention (§4.4); everything else is event-rate bound.

### 12.2 Configuration

`indexer.example.toml` lists every key with its default (`[database]`, `[ingest]` and its
sub-tables, `[api]`, `[analytics]`); `DATABASE_URL`, `SHORTWIN_RPC_URL`, `SHORTWIN_WS_URL` override.
Each track owns its section struct (§13.2).

### 12.3 Operations

- `shortwin-indexer rebuild [--market <pubkey>]` after a decoder fix (projections only; raw rows are
  kept). A decoder fix that changes raw rows: re-ingest the affected range (delete the
  `ingest_txs` rows by slot range and re-run the backfill from the cursor reset).
- Alerts: quarantined short.win events (`/v1/status.quarantined` > 0) and longer layouts
  (`layout_warnings` > 0: upgrade the indexer); notified signatures not served
  (`missing` > 0); `/v1/status` lag > 60 s; finality backlog (`finalized_slot` far behind
  `confirmed_slot`: transactions the node cannot resolve, §3.2); WebSocket lagged closes; rollback
  count (with history-searching statuses a rollback now means a proven abandoned fork); error logs
  of a listing bound the node does not know (§2.2).
- The RPC endpoint must serve `getTransaction` with `maxSupportedTransactionVersion: 1` (v1
  transactions; ARCH §12 U25 tracks v1 support on the cluster).

### 12.4 Dependencies (frozen by this contract; `services/Cargo.toml`)

`sqlx 0.9` (runtime-tokio, postgres, migrate, macros, json, tls-rustls-ring-webpki), `axum 0.8`
(ws), `tower 0.5`, `tower-http 0.6`, `tokio-tungstenite 0.29` (the version axum's `ws` uses),
`sha2 0.10`, `thiserror 2`; TEST-ONLY `postgresql_embedded 0.21` (zonky), and for the contract tests
`serde-saphyr 1.3`, `jsonschema 0.58` (no default features), `http-body-util 0.1`. sqlx 0.9 and
postgresql_embedded 0.21 declare Rust 1.94, so the two indexer crates set `rust-version = "1.94"`
(the toolchain is 1.98.1; the other services crates keep 1.89). Adding them changed no existing
lockfile entry. `services/deny.toml` allows the `PostgreSQL` licence (the fixture crates' "(Apache-2.0
OR MIT) AND PostgreSQL"). **The services `Cargo.lock` has one writer:** a track that needs another
crate asks the integrator.

---

## 13. Tracks and file ownership

### 13.1 Tracks

| Track | Builds |
|---|---|
| **INGEST** | `rpc::to_raw_tx`, the RPC backfill, the live PubSub source (+ Yellowstone if it resolves), the session replay source, the decoders (short.win, instruction args, venues, the log stack), the writer, the projections and rebuild, finality and rollback, the refresher and venue watch, the CLI dispatcher; decoder unit tests (§11.5) |
| **API** | axum REST per `openapi.yaml`, the WebSocket (§9.3), errors, pagination, rate and abuse limits (§10), the contract tests (§11.2) with its SQL seed |
| **ANALYTICS + REPLAY** | the views (verification and fixes), the rollup job and retention (§8.6, §4.4), `analytics::formulas`, the recorder hook and recorder, the session set, the DB-vs-chain assertions (§11.3, §11.4) |

### 13.2 File ownership

| Path | Owner |
|---|---|
| `docs/design/INDEXER.md` | FOUNDATION (changes by agreement; each track may append to §14) |
| `services/Cargo.toml`, `services/Cargo.lock`, `services/deny.toml`, `.github/workflows/ci.yml` | FOUNDATION / integrator (single writer) |
| `services/indexer/Cargo.toml`, `services/indexer-litesvm/Cargo.toml` | FOUNDATION / integrator (target sections: the crate's owner may add `[[test]]` / `[[bin]]` entries) |
| `services/indexer/src/{lib,config,db,numeric,model}.rs` | FOUNDATION (frozen; additive vocabulary in `model.rs` by agreement) |
| `services/indexer/src/ingest/{raw,source,session}.rs` | FOUNDATION (frozen) |
| `services/indexer/src/testing/**`, `services/indexer/tests/pg_fixture.rs` | FOUNDATION |
| `services/indexer/src/main.rs`, `src/ingest/**` (except the three above; as built also `store.rs`, `decode/idl.rs`, `decode/tests.rs`), `tests/ingest_*.rs`, `tests/ingest_common/**`, `tests/data/ingest/**` (its 14 fixture sessions), `tests/data/sessions/**` (consumes; REPLAY produces), `indexer.example.toml` `[ingest]` section | **INGEST** |
| `services/indexer/migrations/0002_events.sql` … `0005_admin.sql`, `0100–0199_*.sql` | **INGEST** |
| `services/indexer/src/api/**` (as built also `params.rs`, `sql.rs`), `tests/api_*.rs` but `api_replayed.rs`, `tests/api_support/**`, `tests/data/api_seed.sql`, `openapi.yaml` (fixes by agreement), `indexer.example.toml` `[api]` section | **API** |
| `services/indexer/tests/api_replayed.rs` (join 2: the contract on replayed sessions) | integrator (uses `api_support`) |
| `services/indexer/migrations/0007_api.sql`, `0300–0399_*.sql` | **API** |
| `services/indexer/src/analytics/**`, `tests/analytics_*.rs`, `indexer.example.toml` `[analytics]` section | **ANALYTICS + REPLAY** |
| `services/indexer/migrations/0006_analytics.sql`, `0200–0299_*.sql` | **ANALYTICS + REPLAY** |
| `services/indexer-litesvm/**` (except `Cargo.toml`; as built also `src/scenarios.rs`, `src/seed.rs`), `tests/litesvm/src/env.rs` recording hook (additive), `tests/litesvm/src/raydium.rs` `send_v0`'s recording call, `services/keeper-litesvm/src/lib.rs` recording calls (additive), `tests/litesvm/src/record.rs` (new; declared in the harness's `lib.rs`) | **ANALYTICS + REPLAY** |
| `services/indexer/deploy/**` | FOUNDATION (INGEST and API may update their services' commands) |
| `services/README.md` indexer section | FOUNDATION (each track appends its own subsection) |

### 13.3 Migrations

`0001_foundation.sql` is frozen. Each track owns its files above and may **edit them until the first
deployment** (the embedded test databases are always fresh, so sqlx's checksums never bite before
then), or add files in its range (INGEST 0100–0199, ANALYTICS 0200–0299, API 0300–0399; the unit
test `migrations_are_numbered_in_track_ranges` enforces the ranges). A change to a column another
track reads (§9.2's read model, §6's projections) needs that track's agreement and an update here.

### 13.4 Frozen interfaces (signatures fixed by the skeleton, kept as built)

- `ingest::raw::{RawTx, RawTopIx, RawIx, Commitment}`, `ingest::source::{TxSource, SourceItem}`,
  `ingest::session::{Session, SessionHeader, SessionTx, AccountsCheckpoint, SessionAccount,
  SessionLine, SessionWriter, read, SESSION_VERSION}`.
- Entry points: `ingest::{run_backfill, run_live, replay_file, ingest_session, rebuild}`,
  `ingest::rpc::to_raw_tx`, `ingest::{ProgramIds, IngestReport}`; `api::{router, serve, AppState,
  ApiConfig}`; `analytics::{run_jobs, AnalyticsConfig}`, `analytics::rollups::run_once`,
  `analytics::formulas::*`; `db::{MIGRATOR, connect, migrate, NOTIFY_EVENTS, NOTIFY_ROLLBACK}`;
  `testing::pg::TestPg`.
- The schema columns named in §6, §8 and §9.2; the OpenAPI document.

### 13.5 Integration order and the M8 exit

1. **In parallel from day 1:** INGEST (decoders on synthetic transactions, writer, projections on
   the fixture); API (routes and contract tests on its SQL seed); REPLAY (recorder hook, recorder,
   `mock_lifecycle` session committed first — INGEST's first real input).
2. **Join 1:** INGEST ingests the committed sessions; REPLAY's assertions 1–6 go green.
3. **Join 2:** API's contract tests also run on replayed sessions; ANALYTICS' assertions 7–12 and the
   rollups go green.
4. **M8 exit (ARCH §9):** "Replays an E2E session into correct DB state" = §11.4 passes on every
   session of §11.3 (LiteSVM-recorded; Surfpool optional); "API contract tests" = §11.2 passes for
   every operation. Plus the gates of §11.6.

**As integrated (2026-10-01).** Joins 1 and 2 are done and the exit is met:

- *Wiring:* `assert::replay_session` drives the real pipeline only (the v1.0 skip-while-not-built
  path is gone); item 11 goes through the finality tracker, whose rollback calls
  `analytics::rollups::reset_from` inside its transaction; the record donation is read from
  `events.derived`; the API's read-model assumptions are assertion 13; `tests/api_replayed.rs`
  runs the contract on replayed sessions; `shortwin-indexer run` stops at its first failing role
  (`try_join!`, so a dead ingest cannot leave a stale API serving).
- *"Replays an E2E session into correct DB state":* `cargo test -p shortwin-indexer-litesvm` —
  `every_session_replays_into_the_chain_state` passes §11.4 items 1–13 on all nine sessions
  (2,371 transactions; the projections equal the chain at all 178 checkpoints; re-measured
  locally on `pump-v3`, 2026-10-16, AUDIT §10 "The pump v3 gate"; 1,369 / 159 on eight sessions
  at M8, before AIR-07's `590e724` gave `mock_lifecycle` M5–M7 and pump-final P6 added
  `pump_synthetic_exit`; idempotent re-ingest, rebuild, out-of-order delivery, rollback with the
  rollups), plus `the_contract_rules_equal_the_chain_at_every_checkpoint`,
  `the_assertions_hold_on_a_faithful_database_and_catch_tampering`, the analytics reproductions
  and catalog coverage (8 tests; CI's PostgreSQL run has not yet run on these totals);
  `cargo test -p shortwin-indexer --test ingest_sessions` — INGEST's thirteen fixtures and
  REPLAY's committed two, each matching the chain, with ordering, rebuild, rollback and
  single-writer tests (6; 8 since pump-final P6: the synthetic exit's rows and a retired helper
  transaction).
- *"API contract tests":* `api_contract` (6), `api_limits` (6), `api_ws` (6) on the seed and
  `api_replayed` (1, ≈ 1,800 responses on sixteen replayed sessions, 48 accounts compared with the
  chain through the API).
- *Gates:* the services workspace's tests, clippy `-D warnings` and fmt; CI's `indexer` job (§11.6).

## 14. Open items and known limits

| # | Item | Status |
|---|---|---|
| IX-1 | `Liquidate` and `Expire` carry `rent` but not its protocol share; `SolSettle` carries neither `q` nor `P_ema`; `Liquidate`/`Expire` lack `q`. The indexer derives them (§6.5, §8.4: exact rent split from locked terms; `q` from `Open`; `P_ref` bounded by the box). Adding `rent_protocol`, `q` and `ema_price` to those events would remove the derivations — a program change (IDL, CU, trace) for the owner to weigh after the 24-hour fuzz run | deferred (owner, 2026-10-09): to a 1.0.x batch after the audit tag; until then the indexer derives them |
| IX-2 | Price state, `tier` and route chains change silently in other instructions' prologues ([E\*]); exact current values need the refresher (live) | by design |
| IX-3 | `CoverageGap` and exit-path `MintSafetyTripped` are log events: lost if logs truncate (flagged per transaction) | by design (ARCH A69) |
| IX-4 | Yellowstone gRPC source: built only if `yellowstone-grpc-client` resolves on the Agave 4.2.2 line without changing existing lockfile entries. Checked: 14.0.1 changes no version but adds 33 packages (tonic, prost, vendored protoc) and changes the dependency lists of 6 existing entries (hashbrown ×3, rustls, tokio-stream, tower); `live_source = "yellowstone"` is a start-up error | deferred to v1.1 |
| IX-5 | Venue event layouts (pump `TradeEvent`'s later fields, PumpSwap `Buy/SellEvent`) are pinned against the fixture programs, not against every historical layout; a venue upgrade can add fields (tolerated) or change them (quarantine → alert) | by design |
| IX-6 | USD values exist only while the refresher runs (`crank_sol_usd` emits no event; historical Pyth prices are not in transactions) | by design |
| IX-7 | ARCH §7.2's "the indexer runs the same byte-level mint filter and pool-eligibility checks to show reasons before Create market" is a live check (RPC reads), not indexed data; deferred to M9 with the frontend (an API endpoint `GET /v1/mints/{mint}/check` then) | deferred |
| IX-8 | `yoke-derive 0.8.3` in `services/Cargo.lock` was yanked upstream (2026-09-30; pre-existing, not from M8): `cargo deny` / `cargo audit` failed on it | fixed at integration: `cargo update -p yoke-derive --precise 0.8.4` (that one entry) |
| IX-9 | The docker-compose deployment is not run on the development machine (no Docker); CI validates the compose file (`docker compose config`) and the roles script's syntax, and the image's build context excludes `target/` and the keys (`Dockerfile.dockerignore`); nothing builds the image or runs the stack yet | open (M11 deployment) |
| IX-10 | Fee-sharing pump coins report their sharing-config PDA as `coin_creator` (SPEC R1 attribution then needs the sharing config's recipients) | open |
| IX-11 | Two `lender_sol` flows have no event field: the `ShareEpoch` record-address donation a rolling `sol_settle` sweeps (decoded from its System transfer into `events.derived`, §5.4, §6.4) and an `accrue` credit while `S = 0` (dust edge case). An `EpochRolled.record_donation` field would make the first explicit | deferred (owner, 2026-10-09), with IX-1: to a 1.0.x batch after the audit tag; until then the indexer decodes the donation |
| IX-12 | `markets.pool_d` is [E\*] (every observation's prologue refreshes it); not asserted, the refresher's value is the current one | by design |
| IX-13 | No event carries the record donation (§6.4). REPLAY proposed a `share_epochs.record_donation` column; INGEST's `events.derived.record_donation` (on the rolling `EpochRolled`) does the same job, and assertion 6 reads it — no column added | closed (integration) |
| IX-14 | Raydium S2 on the fork is not in the session set: it needs `raydium_v4_support.rs`'s `s2_corner` init arguments, `hold_unlocked_lp` and `remove_liquidity`, which live in a test-only module of `tests/litesvm/tests`. S2 is covered on the mock venue (`mock_lifecycle`) | open (move those helpers into `tests/litesvm/src`) |
| IX-15 | IX-11's silent `accrue` credit while `S = 0` is reached by no session; the harness would report it as a `lender_sol` mismatch | open |
| IX-16 | Venue layouts not exercised against mainnet. *M10 (GC-7 / GC-F1): the decoder half is closed* — PumpSwap `Buy/SellEvent` field order (the prefix and the VQR tail), CPMM `SwapEvent` and AMM v4 `ray_log` decode recorded mainnet transactions exactly (`tests/venue_mainnet.rs`, §11.5; and the Python port, `scripts/gapcal/tests/test_decode.py`). Still open: pump / pAMM lifecycle events decode only `CompleteEvent` and `CreatePoolEvent` fields (others stored as raw hex); the refresher's token metadata reads Metaplex only (no Token-2022 metadata extension), and the venue admin watch reads Raydium pool `status` and CPMM `AmmConfig` rates only (no AMM v4 fee fields, no pump / PumpSwap config fields) — both tested at the SQL level only | partly closed (M10: decoders); the refresher and the admin watch: M11 |
| IX-17 | The invocation retention prune keeps `initialize_config` (and every admin, listing and transparency instruction, `analytics::RETAINED_INSTRUCTIONS`) forever: the global rebuild reads `initialize_config`'s arguments | by design |
| API-1 | `/v1/health` and `/v1/status` are not rate-limited (probes; no 429 documented) | by design (owner may change) |
| API-2 | 503 is documented on every database-backed operation (was only `/v1/status`); the contract test produces it on each | fixed at integration |
| API-3 | `FeeLine.required`'s `n` read as `false` under YAML 1.1 (serde-saphyr's default, many generators); quoted, and the contract test now requires YAML 1.1 and 1.2 to read the same document | fixed at integration |
| API-4 | WebSocket `since` replays only newly subscribed event channels, at most 1,000 events; past the cap the client pages through REST | by design |
| API-5 | `trades:<mint>` pushes follow chain order from the newest trade at its first subscriber; gap-filled history below that is not pushed | by design |
| API-6 | `v_market_stats_daily` aggregates a market's whole hourly history per request (its `day_start` filter is not pushed below the `GROUP BY`); fine at M8 volumes | open (performance; an hourly-range query in the API when it matters) |
| API-7 | The WebSocket behind a reverse proxy (`X-Forwarded-For` for the per-client connection cap) and the docker-compose API service were not run here (no Docker) | open (M11 deployment) |
| IX-18 | Event decoders follow the current build: a **longer** payload (appended fields) is applied on its known prefix and flagged (`layout_warnings`); a **shorter** one (an older layout re-backfilled on a newer indexer) is quarantined and the market flagged (`quarantined_events`). Decoders versioned by program deploy slot would apply both; needed only once the program is upgraded on mainnet with a layout change (M8-SEC-13) | open (before the first program upgrade that changes an event) |
| IX-19 | The finality tracker keeps a `confirmed` transaction whose fate the node cannot tell (no status with history, not served at finalized, its block pruned): it holds `finalized_slot` back until resolved (finality-backlog alert). A node with full history (or Bigtable) resolves everything; a pruned node can stall the watermark after long downtime — the operator points `rpc_url` at an archival node (M8-SEC-1) | by design (operations) |
| API-8 | IPv6 rate-limit buckets: `ipv6_bucket_prefix` = 64 by default (one subscriber; a /48 holder has 65,536 buckets). /56 cuts that to 256 per /48 but puts up to 256 mobile subscribers of a carrier that hands out /64s behind one bucket (20 req/s for all of them); /48 is one bucket per allocation. Configurable; the default is the owner's call (M8-SEC-8) | open (owner) |
| API-9 | `/v1/stats` is served from memory for 2 s (its `max-age`): a new market or position shows there within 2 s, not at once (M8-SEC-3) | by design |
