#!/usr/bin/env python3
"""Records the decoder fixtures of ``test_decode.py`` (WP-9; GC-7): about 20 real mainnet
transactions per venue, each venue's from one pool, with the pool facts the checks need.

- ``pump.json.gz``: 20 consecutive trades of one pump curve (every successful transaction of the
  curve in that window, so each trade's virtual reserves follow from the previous one's);
- ``pamm.json.gz``: 20 transactions trading on one canonical PumpSwap pool (sampled seconds);
- ``cpmm.json.gz``: 20 consecutive swaps of one CPMM pool with its ``AmmConfig`` fee rates;
- ``ammv4.json.gz``: 20 consecutive swaps of a pump graduate's AMM v4 pool, and the swaps among
  the newest 60 successful transactions of SOL/USDC, each pool with its ``need_take_pnl`` read
  right after the window;
- ``discovery.json.gz``: one pump create and one migration transaction (``universe.py``'s
  parsers).

Sources: the M10 pilot's transaction cache (``--seed-cache``, read-only; recorded 2026-10-02 from
publicnode) for pump, PumpSwap, CPMM and the first AMM v4 pool; live RPC for SOL/USDC and the
discovery pair (at
``--rps``, default 0.1, so that it stays inside the crawler's budget while one runs).

    python3 scripts/gapcal/tests/record_fixtures.py --seed-cache ~/.cache/shortwin-work/m10s/gapcal/cache \
        --pilot-tapes ~/.cache/shortwin-work/m10s/gapcal/tapes [--cache DIR]
"""

import argparse
import base64
import csv
import gzip
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import decode as D  # noqa: E402
import universe as U  # noqa: E402
from rpc import Rpc  # noqa: E402

N = 20
PUMP_CURVE = ("WMS5K65NAsmQUQNGEr6tqqZAWFEFPkU42WYcoGmhaBD", "Gxb8BPs3aQ8XQRfsyUmcjk7Mt9XMvQL52L2crURhe83Z")
PAMM_POOL = "4JAnKFddd5fFTk5PxDj3PpTWJuTEfSWQEjZJcgKQ9KwW"
CPMM_POOL = "7Pu2ytSzMwg5qhMxXV4qb4ie7gCGhy3kYjbgwFdfmMPB"
AMMV4_POOL = "FVq2chsiAcPqwL9L3zafF8juQHb7mui6LWMuE8g5L4K4"
CREATE_SIG = "yULHtCdXxQmVsZimUQngrZMEasdYP9DhtGs7pykRCT8GsQf3ojJZky7E1tUmLwm7QCMVkDukJ7yJkAv92QrrbSz"
MIGRATION_SIG = "2fzhz2BNEL3wb2X6geT6FPfb1QVEEKkvFBwkjRkEwprQpRqK3f7rHtMqDz7LdvaaPtxTmsMuUJZubAJ5XMNCG3os"


def write(name, obj):
    path = os.path.join(HERE, "fixtures", name)
    with gzip.open(path, "wt") as f:
        json.dump(obj, f, separators=(",", ":"), sort_keys=True)
    print(f"{path}: {len(obj.get('txs', []))} transactions, {os.path.getsize(path)} bytes")


def chain_order(txs):
    return sorted(txs, key=lambda tx: (tx["slot"], tx.get("transactionIndex", 0)))


def pilot_txs(rpc, tapes, label):
    rows = list(csv.DictReader(open(os.path.join(tapes, f"{label}.sigs.csv"))))
    txs = [rpc.cached_transaction(r["signature"]) for r in rows if r["ok"] == "1"]
    return chain_order([t for t in txs if t is not None])


def account(rpc, key):
    _, vals = rpc.multiple([key])
    return base64.b64decode(vals[0]["data"][0])


def first_window(txs, keep, consecutive):
    """The first ``N`` transactions with a trade ``keep`` accepts; with ``consecutive``, a run of
    ``N`` trade transactions whose reserves chain (no liquidity change in between)."""
    run = []
    for tx in txs:
        tr = [t for t in D.decode_trades(tx, **keep[1])[0] if keep[0](t)]
        if not tr:
            continue
        if consecutive and run and not consecutive(run[-1][1][-1], tr[0]):
            run = []
        run.append((tx, tr))
        if len(run) == N:
            return [tx for tx, _ in run]
    raise SystemExit(f"no window of {N}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed-cache", required=True)
    ap.add_argument("--pilot-tapes", required=True)
    ap.add_argument("--cache", default=None, help="transaction cache for the live fetches")
    ap.add_argument("--rps", type=float, default=0.1)
    args = ap.parse_args()
    rpc = Rpc(rps=args.rps, cache_dir=args.cache, seed_cache_dirs=[args.seed_cache], log=print)

    # pump: consecutive trades of one curve.
    curve, mint = PUMP_CURVE
    txs = first_window(pilot_txs(rpc, args.pilot_tapes, "pump_Gxb8"),
                       (lambda t: t["venue"] == "pump_curve" and t["mint"] == mint, {}),
                       lambda a, b: (b["sol_after"] - a["sol_after"]) == (1 if b["side"] == "buy" else -1) * b["curve_sol_delta"])
    write("pump.json.gz", {"venue": "pump_curve", "curve": curve, "mint": mint, "txs": txs})

    # PumpSwap: transactions trading on one canonical pool.
    p = D.decode_pamm_pool(account(rpc, PAMM_POOL))
    txs = first_window(pilot_txs(rpc, args.pilot_tapes, "pamm_eacc"),
                       (lambda t: t["venue"] == "pump_swap" and t["pool"] == PAMM_POOL, {}), None)
    write("pamm.json.gz", {"venue": "pump_swap", "pool": PAMM_POOL, "mint": p["base_mint"],
                           "base_vault": p["base_vault"], "quote_vault": p["quote_vault"], "txs": txs})

    # CPMM: consecutive swaps with the config's rates.
    st = D.decode_cpmm_pool(account(rpc, CPMM_POOL))
    cfg = D.decode_cpmm_config(account(rpc, st["amm_config"]))
    rates = (cfg["protocol_fee_rate"], cfg["fund_fee_rate"])
    kw = {"cpmm_rates": {CPMM_POOL: rates}}
    txs = first_window(pilot_txs(rpc, args.pilot_tapes, "cpmm_CURVE"),
                       (lambda t: t["venue"] == "raydium_cpmm" and t["pool"] == CPMM_POOL, kw),
                       lambda a, b: (a["sol_after"], a["tok_after"]) == (b["sol_before"], b["tok_before"]))
    sol_first = st["token_0_mint"] == D.WSOL
    write("cpmm.json.gz", {"venue": "raydium_cpmm", "pool": CPMM_POOL, "amm_config": st["amm_config"],
                           "rates": list(rates), "config": cfg,
                           "sol_vault": st["token_0_vault"] if sol_first else st["token_1_vault"],
                           "tok_vault": st["token_1_vault"] if sol_first else st["token_0_vault"],
                           "txs": txs})

    # AMM v4: 20 consecutive swaps of a pump graduate's pool (pilot; need_take_pnl 0), and every
    # swap among the newest 60 successful transactions of SOL/USDC (need_take_pnl non-zero; most
    # of that pool's transactions are bots that touch it without swapping).
    pools = []
    for pool, source in ((AMMV4_POOL, "pilot"), (U.SOL_USDC_V4, "live")):
        a = D.decode_amm_info(account(rpc, pool))
        first = a["coin_mint"] == D.WSOL
        keep = (lambda t, pool=pool: t["venue"] == "raydium_v4" and t["pool"] == pool,
                {"v4_orientation": {pool: first}})
        chained = lambda x, y: (x["sol_after"], x["tok_after"]) == (y["sol_before"], y["tok_before"])  # noqa: E731
        if source == "pilot":
            txs = first_window(pilot_txs(rpc, args.pilot_tapes, "v4_CRIME"), keep, chained)
        else:
            sigs = [s for s in rpc.signatures(pool, limit=100) if s["err"] is None]
            txs = chain_order([rpc.transaction(s["signature"]) for s in sigs[:60]])
            txs = [tx for tx in txs if any(keep[0](t) for t in D.decode_trades(tx, **keep[1])[0])]
        pools.append({"pool": pool, "sol_is_coin": first,
                      "sol_vault": a["coin_vault"] if first else a["pc_vault"],
                      "tok_vault": a["pc_vault"] if first else a["coin_vault"],
                      "need_take_pnl_coin": a["need_take_pnl_coin"],
                      "need_take_pnl_pc": a["need_take_pnl_pc"], "txs": txs})
    write("ammv4.json.gz", {"venue": "raydium_v4", "pools": pools,
                            "txs": [tx for p in pools for tx in p["txs"]]})

    write("discovery.json.gz", {"create": rpc.transaction(CREATE_SIG),
                                "migration": rpc.transaction(MIGRATION_SIG)})
    print(rpc.stats)
    return 0


if __name__ == "__main__":
    sys.exit(main())
