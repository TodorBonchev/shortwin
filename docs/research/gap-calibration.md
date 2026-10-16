# Gap-frequency calibration (WP-9): method

*Research note for short.win, M10 WP-9. Written 2026-10-02. Status: **method only**. The crawl started on 2026-10-02; results, fits and the re-run simulations follow once the tapes cover at least 30 days. Owner review of the finished report is the M10 exit criterion.*

---

## 0. What is being calibrated

**"Gap" means a price jump, not an observation gap.** In ECON §11.5–§11.7, G37, U21 and O9 a gap is a persistent price move of at least 1.5× that the keeper cannot act inside. The Monte Carlo of §11.6 draws such moves per tier from four parameters:

- `lam_up` and `lam_dn`: the rates of up and down gaps per market-day;
- `alpha`: the tail index of their size;
- `sigma`: the volatility between gaps.

These are the numbers this work measures. They are uncalibrated today: "calm" and "stressed" are two guesses.

**What this work does not calibrate.** Organic inter-trade gaps (seconds with no trade) do not set `COVERAGE_GAP_S` (40 s), `oracle_stale_s` (600 s) or the pause thresholds. Those protect against a missing keeper or oracle, not against a quiet pool. The crawler records trade activity anyway, and its statistics go in an appendix only.

**Why not the indexer.** ECON §11.6 said "from indexer data". That is not possible before launch, because the M8 indexer streams only short.win's own markets. The source is therefore a crawl of public mainnet RPC that reuses the M8 decoders (ported to Python). After M11 the indexer takes over on listed markets (GC-6).

## 1. Universe: markets chosen by on-chain eligibility

Markets are chosen by **on-chain eligibility, never by a volume ranking**. Choosing the most traded pools would select for the pumps and dumps being measured.

The selection code is `scripts/gapcal/universe.py`.

### 1.1 Census

A census runs weekly. It uses `getProgramAccounts` on api.mainnet-beta, because publicnode refuses indexed requests without a token.

| venue | eligible when (as the program judges it) | real SOL `R`, sizing reserve `R_eff` |
|---|---|---|
| PumpSwap | `index` 0; WSOL quote; the pool sits at the canonical address `["pool", 0, pool-authority(mint), mint, WSOL]`; not mayhem; the base mint passes SPEC §3's filter (`decode.mint_check`, a port of `shortwin_venue_state::mint::check`) | `R` = quote vault; `R_eff = min(R, R + VQR)` (Q19, #103) |
| Raydium CPMM | a WSOL side; the swap bit is clear; the token mint passes the filter; the locked-depth floor `⌊locked·10⁴/λ_min⌋ ≥ pool_min_real_sol` holds, with `locked = λ·R` and `λ = (lp_supply − LP mint supply)/lp_supply` | `R` = WSOL vault − protocol, fund and creator counters; `R_eff = min(R, locked/λ_min)` (ARCH §4.7) |
| Raydium AMM v4 | the same, with the status in {1, 6, 7} and `λ` from `lp_amount` | `R` = WSOL vault − `need_take_pnl` |

The defaults are `pool_min_real_sol` = 25 SOL and `λ_min` = 5,000 bps.

`locked = λ·R` equals ECON §6.2's `⌊D·isqrt(P)/10⁹⌋` up to rounding, since `D·√P = λ·√(XY)·√(Y/X) = λ·Y`. The census judges it at the live price; the program judges it at `P_ema`.

### 1.2 Tier of each census pool

The tier is ECON §6.2's function (`decode.tier`):

- `Λ = 2·R_eff`;
- θ_A = $500k and θ_B = $100k, converted at the census's SOL/USD;
- C if `age < 24 h` or `Λ < θ_B`;
- A if `Λ ≥ θ_A` and `age ≥ 7 d`;
- B otherwise.

SOL/USD comes from the AMM v4 SOL/USDC pool `58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2`, net of `need_take_pnl`. The program uses Pyth at `price − conf`, so the census can differ from it by about ±0.5 % at the threshold.

Age is needed only where it decides the tier, that is when `Λ ≥ θ_B`. It is measured as follows:

- CPMM: `now − open_time`.
- PumpSwap and AMM v4: the time since the oldest signature of the pool's LP mint. Only the pool's creation, deposits and withdrawals touch that mint, so this is usually one call.

The age is the pool's age, not the age since a short.win listing (O13). For a market listed today, the tier would be C for 24 h and at most B for 7 d.

### 1.3 Selection

The census-selected markets, called the "panel":

- every Tier A pool (up to 150; the first census found 77);
- a seeded random sample of Tier B, up to 80;
- a seeded random sample of Tier C pools with `R_eff ≥ 25 SOL`, up to 30.

The 25-SOL floor that Raydium enforces is applied to PumpSwap too. Without it, the C sample would be mostly abandoned graduates of a few SOL, which nobody would list.

A panel market stays in the panel while it remains eligible. Later censuses only top the panel up to the caps.

### 1.4 Cohorts

Cohorts are followed from their birth, so there is no survivorship selection. A cohort market found more than 240 s (twice the discovery period) after its creation or migration is not followed: it would have entered only because it survived. Such a find is written to the events tape as a skip, and the lag of every cohort market is recorded in its `added` event.

- **Tier D (pump curves).** A deterministic 1 % sample (by signature hash) of pump creations, roughly 350 a day out of about 35,000. Creations are found through `getSignaturesForAddress` on pump's mint authority `TSLvdd1…`.
  - A sampled curve must be SOL-quoted, not mayhem, and pass the mint filter.
  - It is followed until it graduates (`complete`) or dies. Death means no reserve change for 6 h; the market's life is counted to its last trade.
  - A graduating curve hands over to its canonical PumpSwap pool, which joins the fresh C cohort below.
  - At most 200 curves are followed at once.
- **Late curves of 2026-10-02.** The first census held discovery back (it then ran on the census's budget), so 23 Tier D curves were added 415–8,372 s after their creation (mids 171–186 and 204–210, added 06:13:45–06:14:46Z). They are ended with reason `late`, flagged `late` in `universe.json`, and do not count toward the D target; the analysis excludes them.
- **Fresh Tier C (graduates).** A 0.4 % sample of the migration transactions, about 7 a day out of roughly 1,700. They are found through pump's withdraw authority `39azUYFW…`, which signs every `migrate`.
  - A sampled graduate is followed for its first 7 days, which covers its C → B / A transitions.
  - At most 50 are followed at once.

### 1.5 Sample targets

The targets are `6/λ*` market-days per tier, over at least 30 days:

| tier | target (market-days) | what the plan expects |
|---|---|---|
| D | ≥ 168 | covered by the cohorts in about 3–4 weeks |
| C | ≥ 504 | panel plus cohorts, about 80 live markets; covered in about a week |
| B | ≥ 1,230 | needs about 41 markets for 30 days; covered if the census finds them |
| A | ≥ 3,210 | needs about 107 markets for 30 days. Mainnet has fewer Tier A WSOL pools than that, so on public RPC A is an **upper bound**, as the plan predicts. Q5 (Dune or a paid RPC) can lift it. |

`fetch.py status` prints the market-days so far per tier and venue, by the tier at entry. The per-day tier is recomputed in the analysis from the tapes.

## 2. What is recorded

The crawler is `scripts/gapcal/fetch.py`. It runs as one detached process with two threads. Its data lives outside git, in `~/.cache/shortwin-work/gapcal-data/`.

### 2.1 Tapes

There is one CSV per tape per UTC day. When the day closes it is gzip'd, and `manifest.json` records its sha256, row count, and slot and time range.

| tape | rows | source |
|---|---|---|
| `poll` | `t, slot, mid, sol_px, tok_px, sol_raw, tok_raw`, one row only when the reserves changed | `getMultipleAccounts` of the vaults or the curve; see §2.2 |
| `trades` | `block_time, slot, tx_index, sig, ix, seq, mid, side, sol_amount, token_amount, sol_before, tok_before, sol_after, tok_after` | sampled transactions decoded by `decode.py`; see §2.3 |
| `activity` | `block_time, mid, ok, err` per active second | `getSignaturesForAddress` of each market every 30 min, with a cursor |
| `events` | `t, slot, mid, kind, value` | markets added (with a cohort's discovery lag) and ended (graduated, dead, ineligible, fresh window over, late; the end time in the value), VQR / counter / status changes, expired cursors and listing gaps, cohort skips, crawl start and stop |

Every tape is written by the day its rows arrive. A row's own time (`block_time`, or an end time in `value`) can fall on the previous day, so a closed day's tape and its hash never change after the day closes. A row that does reach a closed day's tape (a poll spanning midnight) is merged into the gzip'd tape, and the merge is safe against a crash.

Supporting files:

- `universe.json`: every market followed, with its selection reason and tier at entry;
- `census/`: every census;
- `tx/`: every fetched transaction, gzip'd;
- `state.json`: cursors, the transaction queue and the call counts per day.

### 2.2 Poll tapes: the keeper's view

The poll tapes record the pool's pricing reserves as the program would read them:

- **Tiers C, D and SOL/USD:** every 3 s.
- **Tiers A and B:** every 10 s.

How the pricing reserves are formed:

- CPMM: vault − counters.
- AMM v4: vault − `need_take_pnl`.
- PumpSwap: vault + VQR.
- Curves: the virtual reserves.

The counters, `need_take_pnl` and VQR are refreshed every 60 s. Between refreshes the CPMM counters lag by at most a minute of fees (16 % of 25 bps of that minute's volume).

`t` is the crawler's clock when the response arrived; `slot` is the response's context slot. Polls are live only: an outage of the crawler is a hole in the tape (§4).

### 2.3 Trade tapes

The trade tapes come from transactions decoded with the Python port of the M8 decoders:

- **Curves:** every successful signature, up to 1,000 per curve.
- **Pools:** the last successful transaction (up to three candidates) in each of a seeded random sample of active seconds, about 200 a day per market.

They serve three purposes:

- the 1-s and intra-second structure;
- U20 (reversion);
- decoder parity (GC-7, IX-16's decoder part).

The decoders are exact. `scripts/gapcal/tests/test_decode.py` checks them on 20 recorded transactions per venue:

| venue | check |
|---|---|
| PumpSwap | the event-derived post reserves equal the vault balances after the transaction, to the unit |
| CPMM | `SwapEvent`'s pre-trade reserves exclude the fee counters, and the M8 decoder added the whole input. With the pool's `AmmConfig` rates the decoder also removes the protocol and fund parts of the trade fee and the creator fee, so each swap's reserves after equal the next swap's before, and `vault − reserves` grows by exactly those fees. Without the rates, the chain breaks by exactly that amount (the M8 indexer's reserves overstated the input side by about 0.04 % of the trade; this finding, GC-F1, is fixed in the indexer, which now applies the watched `AmmConfig` rates and adds PumpSwap's VQR: INDEXER.md §7.2). |
| AMM v4 | `ray_log` reserves chain exactly, and `vault − reserves` equals `need_take_pnl` (0.55 SOL on SOL/USDC) |
| pump curve | virtual reserves chain; virtual − real = 30 SOL / 279.9 M tokens; the curve's token account = real + 206.9 M; its lamports move by exactly the trades' SOL |

### 2.4 Gentleness and reproducibility

**Request budget.** The two threads have their own rate limiters:

- `fg` (polls, state, listings, discovery): 1.9 requests/s;
- `bg` (transactions, census, ages): 1.0 requests/s.

`fg + bg ≤ 3` is enforced. Every retry counts against the budget. 429 and 5xx responses back off exponentially from 2 s up to 5 min with jitter, honouring `Retry-After`.

**Listing cursors.** Each market's listing continues from a cursor: the newest signature listed, with its slot and block time. publicnode cannot use a cursor older than its retention as `until` (-32020 "Transaction … not found", measured 2026-10-02), which a quiet market's cursor reaches. Such a cursor is found by paging back without `until` down to its slot. If publicnode's history ends first, the listing records a `listing_gap` only when the previous successful listing is older than 24 h as well; otherwise the market was just quiet. A failed listing still waits its turn, so one market cannot hold back the others. Discovery reads one page per period (about 40 min of creations), and a full page is recorded as a gap.

**Endpoints.** publicnode serves signature listings and transactions; it keeps about 28 h of them. api.mainnet-beta serves the rest:

- `getProgramAccounts`;
- `getMultipleAccounts` with more than 10 keys: publicnode answers 403 "Request blocked" from 11 keys up (measured 2026-10-02), and one 100-key request replaces ten;
- the backfill of transactions publicnode no longer has.

**Reproducibility.**
- Every transaction is cached. `fetch.py replay --day D` re-decodes a closed day's trade tape with a transport that refuses every request, and checks that it gets the same rows with 0 calls (tested).
- The manifest pins each tape's sha256 and slot range.
- `gapcal.py` will read only the pinned tapes.

## 3. Analysis (to run when the tapes suffice)

The analysis is WP-9 step 3: `docs/design/sim/gapcal.py`, writing `calibration/params.json` and `out/gapcal.txt`. Per tier, on the poll tapes (cross-checked against the trade tapes where both exist):

1. **σ** from 1-min log returns, excluding the windows of detected jumps.
2. **Declustered moves.** For W ∈ {1, 5, 15, 60, 300} s and J ∈ {1.25, 1.5, 1.75, 2, 3, 5, 10}: a move of at least J× within W that persists for 300 s, counted once per cluster. Up and down are counted separately.
3. **Poisson rates λ** per market-day, with exact (Garwood) confidence intervals. With zero events the bound is 3/N.
4. **Hill α** on the sizes of declustered moves of at least 1.5×.
5. **Duty-1 share f**: the share of seconds with `|P/P_ema − 1| > 2 %` under the program's EMA (τ 5 s, dt_max 2 s), and the keeper's cranks per day. These feed `keeper_cost.py`.
6. **Noise**: 1-s and intra-second noise from the trade tapes.
7. **U20**: reversion after jumps.
8. **Historical replay**: of the measured paths through `shortwin_sim.run`.

Then rerun `shortwin_sim.py --mc 400 --fuzz 1000` with a `calibrated` regime read from `params.json`, and `keeper_cost.py` with the measured f. As a self-check, `gapcal.py` must reproduce ECON §11.5's RA 5× row (S1, −59.3 %) on its own inputs.

## 4. Known limitations of the method

- **Poll resolution.** Moves inside 3 s (C, D) or 10 s (A, B) are seen only as their net change. Within-second structure comes from the sampled trade tapes. The W = 1 s window therefore rests on the trade tapes and on the curves' full trade history.
- **Holes.** Polls cannot be backfilled, so a crawler or RPC outage leaves a hole. A crawler outage lies between a `crawl_stop` (or the last row, after a crash) and the next `crawl_start` event; an RPC outage shows as a gap in `t`. The analysis must exclude holes from the exposure (market-days) rather than count them as quiet time; `fetch.py status` does not, and so overstates the exposure by the holes. A restart keeps each market's last polled reserves, so it neither repeats rows nor resets a quiet curve's death clock.
- **Outages longer than about a day.** publicnode keeps about 28 h of signatures, so the activity and trade tapes of such an outage are lost too (`listing_gap` events). Cohort creations from it are found late and skipped.
- **The first day's backlog.** The 23 Tier D curves found late after the first census (§1.4) are excluded, so the D sample starts at 06:14Z on 2026-10-02.
- **R_eff is approximated.** The census uses the live `R` and `λ·R`, while the program uses `R_slow` and `P_ema`. The tier at entry is therefore approximate near the thresholds, and the analysis re-tiers per day.
- **Floors.** The C floor of 25 SOL on PumpSwap is a choice of this study, not a program rule. Without it, C would be dominated by dead pools.
- **Death of a curve** is a crawler convention: 6 h without a reserve change.
- **Public-RPC bounds.** Tier A (and possibly B) are upper bounds, as the plan says, unless Q5 adds a data source.

## 5. Running the crawl

```sh
DATA=~/.cache/shortwin-work/gapcal-data
setsid nohup python3 scripts/gapcal/fetch.py crawl --data $DATA > $DATA/crawl.log 2>&1 &   # writes $DATA/crawl.pid
python3 scripts/gapcal/fetch.py status --data $DATA     # offline: running?, calls/s, coverage per tier, tape sizes
tail -f $DATA/crawl.log                                  # a stats line every 5 min; errors with tracebacks
python3 scripts/gapcal/fetch.py stop --data $DATA       # SIGTERM; saves state and exits
# resume: the same crawl command; it continues from state.json (cursors, queue) and universe.json
python3 -m unittest discover scripts/gapcal/tests        # offline tests (no network)
```
