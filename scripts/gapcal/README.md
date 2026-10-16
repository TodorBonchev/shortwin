# scripts/gapcal: gap-frequency calibration crawler (M10 WP-9)

This is a read-only crawler of public mainnet RPC. It records the price paths of a sample of
markets short.win could list, which are needed to calibrate the Monte Carlo's per-tier jump
parameters (`lam_up`, `lam_dn`, `alpha`, `sigma`; ECON §11.5–§11.7, G37, U21, O9). The method is in
[`docs/research/gap-calibration.md`](../../docs/research/gap-calibration.md).

It uses the standard library only (Python 3.14) and has no dependencies.

| file | role |
|---|---|
| `rpc.py` | JSON-RPC client. It sends `get*` methods only. Each `Rpc` instance has its own rate limiter (both endpoints together); the crawler runs two and enforces `fg + bg ≤ 3` requests/s. On 429 or 5xx it backs off. It routes each request to publicnode or api.mainnet-beta and caches transactions (gzip). |
| `decode.py` | Python port of the M8 decoders (pump `TradeEvent`, PumpSwap `BuyEvent`/`SellEvent`, CPMM `SwapEvent`, AMM v4 `ray_log`; the log-to-instruction attribution of `logs.rs`), with exact post-trade reserves. Also the account readers, the SPEC §3 mint filter and the ECON §6.2 tier function. |
| `universe.py` | Weekly census of eligible PumpSwap / CPMM / AMM v4 pools, their tiers and the seeded panel selection. Also the parsers for the creation and migration cohorts. |
| `fetch.py` | The crawler (`crawl`, `status`, `stop`, `replay`), which writes the poll, trade, activity and event tapes. |
| `tests/` | Offline unit tests. `fixtures/` holds about 20 recorded mainnet transactions per venue (`record_fixtures.py` re-records them). |

```sh
python3 -m unittest discover scripts/gapcal/tests     # offline, a few seconds
DATA=~/.cache/shortwin-work/gapcal-data               # data lives outside git
setsid nohup python3 scripts/gapcal/fetch.py crawl --data $DATA > $DATA/crawl.log 2>&1 &
python3 scripts/gapcal/fetch.py status --data $DATA
python3 scripts/gapcal/fetch.py stop --data $DATA     # resume with the same crawl command
python3 scripts/gapcal/fetch.py replay --data $DATA --day 2026-10-02   # 0 RPC calls
```

The crawl runs its own weekly census on its 1 req/s `bg` budget. A separate
`universe.py census` (1 req/s) refuses to run while the crawl is alive, since the two together
would exceed 3 requests/s; stop the crawl first, or let the crawl adopt the census.
