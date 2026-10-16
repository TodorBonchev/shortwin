#!/usr/bin/env python3
"""The gap-calibration crawler (WP-9 step 1): a rolling, resumable, gentle crawl of public mainnet
RPC that records, for every market of the calibration universe (``universe.py``), what short.win's
keeper would see.

Tapes (one file per UTC day under ``<data>/tapes/<day>/``, gzip'd and hashed into
``manifest.json`` when the day closes; data lives outside git):

- ``poll.csv``: ``t,slot,mid,sol_px,tok_px,sol_raw,tok_raw`` — the pool's pricing reserves read
  with ``getMultipleAccounts`` every ``poll_fast`` s (Tiers C, D and SOL/USD) or ``poll_slow`` s
  (Tiers A, B); a row only when the raw reserves changed. ``*_raw`` are the vault balances (the
  curve's real reserves); ``*_px`` subtract the CPMM fee counters / AMM v4 ``need_take_pnl`` and
  add PumpSwap's ``virtual_quote_reserves`` (both refreshed every ``state_period`` s), or are the
  curve's virtual reserves. ``t`` is the local clock at the response, ``slot`` its context slot.
- ``trades.csv``: ``block_time,slot,tx_index,sig,ix,seq,mid,side,sol_amount,token_amount,
  sol_before,tok_before,sol_after,tok_after`` — trades decoded from sampled transactions
  (``decode.py``): every successful signature of a curve (up to a cap), and for pools the last
  successful transaction of a seeded random sample of active seconds (``trade_quota`` a day).
- ``activity.csv``: ``block_time,mid,ok,err`` — per active second, the signatures touching the
  pool or curve (``getSignaturesForAddress``; the inter-trade gaps of the appendix).
- ``events.csv``: ``t,slot,mid,kind,value`` — market added (with the cohort's discovery lag) /
  ended (graduated, dead, ineligible, late; the end time in the value), VQR, counter and status
  changes, expired cursors and listing gaps, crawl start / stop (the poll holes). Every tape is
  written by arrival day; ``block_time`` / the value carry the earlier times.

Budget: two threads with their own rate limiters, ``fg`` (polls, state, listings, discovery) and
``bg`` (transactions, the weekly census, ages), ``fg_rps + bg_rps ≤ 3`` enforced; 429 / 5xx back
off (``rpc.py``). Every ``getTransaction`` is cached, so ``replay`` of a finished day costs 0 calls.

Usage (standard library only)::

    setsid nohup python3 scripts/gapcal/fetch.py crawl > DATA/crawl.log 2>&1 &   # detached
    python3 scripts/gapcal/fetch.py status          # offline: pid, rates, coverage per tier
    python3 scripts/gapcal/fetch.py stop            # SIGTERM; the crawl saves its state and exits
    python3 scripts/gapcal/fetch.py replay --day 2026-10-02   # re-decode a day: 0 calls

``crawl`` resumes from ``state.json`` (listing cursors, the transaction queue, discovery cursors)
and the universe; ``DATA`` defaults to ``~/.cache/shortwin-work/gapcal-data``.
"""

import argparse
import base64
import collections
import csv
import gzip
import hashlib
import io
import json
import os
import signal
import struct
import sys
import threading
import time
import traceback

import decode as D
import universe as U
from rpc import MAINNET_BETA, MAX_RPS, PUBLICNODE, Rpc, RpcError

CFG = {
    "fg_rps": 1.9,
    "bg_rps": 1.0,
    "poll_fast": 3.0,
    "poll_slow": 10.0,
    "state_period": 60.0,
    "list_period": 1800.0,
    "list_max_pages": 10,
    "discovery_period": 120.0,
    "census_every": 7 * 86400,
    "create_rate": 1 / 100,  # sampled share of pump creations (Tier D cohort; ~35,000 a day)
    "migration_rate": 1 / 250,  # sampled share of migrations (fresh Tier C cohort; ~1,700 a day)
    "dead_s": 6 * 3600,  # a curve without a reserve change this long has died
    "d_max": 200,  # live sampled curves at most
    "fresh_max": 50,  # live sampled fresh graduates at most
    "late_s": 240.0,  # a cohort market found later than this after its birth is not followed
    "trade_quota": 200,  # sampled transactions per pool market per day
    "curve_tx_cap": 1000,  # transactions per curve over its life (all of them below it)
    "queue_max": 100_000,
    "stats_period": 300.0,
}
SOL_USD_MID = 0
# publicnode keeps about 28 h of signatures and transactions: a ``getSignaturesForAddress`` whose
# ``until`` it no longer holds fails with -32020 "Transaction … not found" (measured 2026-10-02).
RETAIN_S = 24 * 3600
CURSOR_GONE = -32020
FAST_TIERS = ("C", "D", "SOL")
TAPES = {
    "poll": ["t", "slot", "mid", "sol_px", "tok_px", "sol_raw", "tok_raw"],
    "trades": ["block_time", "slot", "tx_index", "sig", "ix", "seq", "mid", "side", "sol_amount",
               "token_amount", "sol_before", "tok_before", "sol_after", "tok_after"],
    "activity": ["block_time", "mid", "ok", "err"],
    "events": ["t", "slot", "mid", "kind", "value"],
}


def utc_day(t):
    return time.strftime("%Y-%m-%d", time.gmtime(t))


def log(msg):
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {msg}", flush=True)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def atomic_json(path, obj):
    with open(path + ".tmp", "w") as f:
        json.dump(obj, f, separators=(",", ":"))
    os.replace(path + ".tmp", path)


# ---- storage --------------------------------------------------------------------------------


class Store:
    """The data directory: day tapes, manifest, universe and crawl state (one lock for all)."""

    def __init__(self, data):
        self.data = data
        self.lock = threading.RLock()
        os.makedirs(os.path.join(data, "tapes"), exist_ok=True)
        self.manifest_path = os.path.join(data, "manifest.json")
        self.manifest = self._load(self.manifest_path, {"days": {}})
        self.universe_path = os.path.join(data, "universe.json")
        self.universe = self._load(self.universe_path, {"markets": {}, "next_mid": 1})
        self.state_path = os.path.join(data, "state.json")
        self.state = self._load(self.state_path, {})
        for k, v in (("cursors", {}), ("last_list", {}), ("disc", {}), ("queue", []), ("hi", []),
                     ("last_change", {}), ("last_key", {}), ("quota", {}), ("calls", {})):
            self.state.setdefault(k, v)
        self.files = {}  # tape -> (day, file object, csv writer, stats)
        self.finalize_past_days()

    @staticmethod
    def _load(path, default):
        if os.path.exists(path):
            with open(path) as f:
                return json.load(f)
        return default

    def save(self):
        with self.lock:
            atomic_json(self.universe_path, self.universe)
            atomic_json(self.state_path, self.state)
            atomic_json(self.manifest_path, self.manifest)

    def close(self):
        with self.lock:
            for _, f, _ in self.files.values():
                f.close()
            self.files.clear()

    def tape_path(self, day, tape):
        return os.path.join(self.data, "tapes", day, f"{tape}.csv")

    def write(self, tape, rows, t=None):
        """Append rows to today's ``tape`` (rotating at the UTC day boundary)."""
        if not rows:
            return
        day = utc_day(t or time.time())
        with self.lock:
            cur = self.files.get(tape)
            if cur is None or cur[0] != day:
                if cur is not None:
                    cur[1].close()
                    self.finalize_past_days()
                path = self.tape_path(day, tape)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                new = not os.path.exists(path)
                f = open(path, "a", newline="")
                w = csv.writer(f)
                if new:
                    w.writerow(TAPES[tape])
                cur = self.files[tape] = (day, f, w)
            cur[2].writerows(rows)
            cur[1].flush()

    def finalize_past_days(self):
        """gzip and hash every tape of a closed day (idempotent; also after a crash)."""
        root = os.path.join(self.data, "tapes")
        today = utc_day(time.time())
        with self.lock:
            for day in sorted(os.listdir(root)):
                if day >= today:
                    continue
                for tape in TAPES:
                    plain = self.tape_path(day, tape)
                    if not os.path.exists(plain):
                        continue
                    if tape in self.files and self.files[tape][0] == day:
                        self.files.pop(tape)[1].close()
                    gz = plain + ".gz"
                    with open(plain, "rb") as f:
                        body = f.read()
                    if os.path.exists(gz):  # rows written after the day closed: merge, never overwrite
                        with gzip.open(gz, "rb") as f:
                            old = f.read()
                        late = body.split(b"\n", 1)[1]
                        # A crash after the replace below left the plain file behind: merged already.
                        body = old if late and old.endswith(late) else old + late
                    with gzip.open(gz + ".tmp", "wb") as dst:
                        dst.write(body)
                    os.replace(gz + ".tmp", gz)
                    os.remove(plain)
                    rows, smin, smax, tmin, tmax = summarize(gz, tape)
                    self.manifest["days"].setdefault(day, {})[tape] = {
                        "file": os.path.relpath(gz, self.data), "sha256": sha256_file(gz),
                        "rows": rows, "slot_min": smin, "slot_max": smax, "t_min": tmin,
                        "t_max": tmax}
                if day in self.manifest["days"]:
                    self.manifest["days"][day]["rpc_calls"] = self.state["calls"].get(day)
            atomic_json(self.manifest_path, self.manifest)

    # ---- universe ----

    def markets(self, live=True):
        with self.lock:
            return [dict(m) for m in self.universe["markets"].values() if not (live and m.get("ended"))]

    def add_market(self, rec, now, slot=None):
        """Adds a market (keyed by venue and pool) unless already followed; returns its mid."""
        with self.lock:
            for m in self.universe["markets"].values():
                if m["venue"] == rec["venue"] and m["pool"] == rec["pool"] and not m.get("ended"):
                    return None
            mid = self.universe["next_mid"]
            self.universe["next_mid"] = mid + 1
            rec = dict(rec, mid=mid, added=now)
            self.universe["markets"][str(mid)] = rec
        born = rec.get("created_at") or rec.get("graduated_at")
        lag = f"|lag={now - born:.0f}" if born else ""
        value = f"{rec['venue']}|{rec['pool']}|{rec.get('mint')}|{rec['tier']}|{rec['reason']}{lag}"
        self.write("events", [[f"{now:.3f}", slot or "", mid, "added", value]], now)
        return mid

    def update_market(self, mid, **kw):
        with self.lock:
            self.universe["markets"][str(mid)].update(kw)

    def end_market(self, mid, why, ended, slot=None):
        """Ends a market at ``ended``, which may be hours back (a dead curve ends at its last
        change): the event row is written when it is known, the end time in its value, so that
        a closed day's tape never changes after its hash."""
        with self.lock:
            m = self.universe["markets"][str(mid)]
            if m.get("ended"):
                return
            m["ended"], m["end_reason"] = ended, why
        t = time.time()
        self.write("events", [[f"{t:.3f}", slot or "", mid, "ended", f"{why}|{ended:.3f}"]], t)


def summarize(path, tape):
    """(rows, slot min, slot max, t min, t max) of a day tape (plain or gzip'd)."""
    rows, slots, ts = 0, [], []
    with (gzip.open if path.endswith(".gz") else open)(path, "rt", newline="") as f:
        r = csv.DictReader(f)
        for row in r:
            rows += 1
            s = row.get("slot")
            if s:
                slots.append(int(s))
            t = row.get("t") or row.get("block_time")
            if t:
                ts.append(float(t))
    return (rows, min(slots) if slots else None, max(slots) if slots else None,
            min(ts) if ts else None, max(ts) if ts else None)


# ---- signature listings ---------------------------------------------------------------------------


def cursor_gone(e):
    """Whether ``RpcError`` ``e`` is the endpoint no longer holding a listing's signature."""
    return isinstance(e.error, dict) and e.error.get("code") == CURSOR_GONE


def _at_or_before(s, c):
    """Whether listed signature ``s`` is cursor ``c`` or older than it."""
    if s["signature"] == c["sig"]:
        return True
    if c.get("slot") is not None and s.get("slot") is not None:
        return s["slot"] < c["slot"]
    return c.get("bt") is not None and s.get("blockTime") is not None and s["blockTime"] < c["bt"]


def list_since(rpc, address, cursor, max_pages, now, limit=1000):
    """The signatures of ``address`` newer than ``cursor``, newest first, at most ``max_pages``
    pages; returns ``(signatures, new cursor, notes)``, ``notes`` being ``(kind, value)`` rows
    for the events tape.

    A cursor is ``{"sig", "slot", "bt", "listed_at", "gone"}``: the newest signature listed so far
    and the time of the last successful listing (a bare signature, the state before 2026-10-02,
    is accepted). ``until`` fails with -32020 once the endpoint no longer holds the cursor's
    transaction (``RETAIN_S``), which a quiet market's cursor reaches. Such a cursor (older than
    ``RETAIN_S``, or rejected once: a ``cursor_expired`` note, then ``gone``) is found by paging back with
    ``before`` alone down to its slot or block time. History ending before it is a
    ``listing_gap`` only if the last successful listing is older than ``RETAIN_S`` as well;
    otherwise the market was quiet and nothing is lost. Without a cursor, one page is listed."""
    if isinstance(cursor, str):
        cursor = {"sig": cursor}
    notes, sigs, before, pages = [], [], None, 0
    by_hand = bool(cursor) and (cursor.get("gone") or cursor.get("bt") is not None
                                and now - cursor["bt"] > RETAIN_S)
    reached, found = cursor is None, None
    while pages < max_pages:
        until = cursor["sig"] if cursor and not by_hand else None
        pages += 1
        try:
            page = rpc.signatures(address, before=before, until=until, limit=limit)
        except RpcError as e:
            if not cursor_gone(e):
                raise
            if until:  # the cursor expired: retry this page without it
                notes.append(("cursor_expired", f"{cursor['sig']}|bt={cursor.get('bt')}"))
                by_hand, pages = True, pages - 1
                continue
            page = []  # the `before` signature is gone too: history ends here
        if by_hand:
            cut = next((i for i, s in enumerate(page) if _at_or_before(s, cursor)), None)
            if cut is not None:
                sigs += page[:cut]
                reached = True
                if page[cut]["signature"] == cursor["sig"]:
                    found = page[cut]
                break
        sigs += page
        if cursor is None or len(page) < limit:
            reached = reached or not by_hand  # with `until`, a short page ends at the cursor
            break
        before = page[-1]["signature"]
    else:  # every page full: the cursor is further back than max_pages
        notes.append(("listing_gap", f"more than {pages} pages since {cursor['sig']}|bt={cursor.get('bt')}"
                                     f"|oldest listed bt={sigs[-1].get('blockTime')}"))
        reached = True
    if not reached:  # paged by hand to the end of the endpoint's history
        listed = cursor.get("listed_at")
        if listed is None or now - listed > RETAIN_S:
            oldest = sigs[-1].get("blockTime") if sigs else None
            notes.append(("listing_gap", f"history before bt={oldest} gone; cursor {cursor['sig']}"
                                         f"|bt={cursor.get('bt')}|listed_at={listed}"))
    if sigs:
        new = {"sig": sigs[0]["signature"], "slot": sigs[0].get("slot"), "bt": sigs[0].get("blockTime"),
               "listed_at": now}
    elif cursor:
        new = dict(cursor, listed_at=now, gone=by_hand and not found)  # gone: no `until` next time
        if found:  # a bare cursor learns its slot and block time
            new.update(slot=found.get("slot"), bt=found.get("blockTime"))
    else:
        new = None
    return sigs, new, notes


# ---- the crawler ------------------------------------------------------------------------------


class Crawler:
    def __init__(self, data, cfg, transport=None):
        if cfg["fg_rps"] + cfg["bg_rps"] > MAX_RPS + 1e-9:
            raise SystemExit(f"fg_rps + bg_rps must be <= {MAX_RPS}")
        self.cfg, self.store = cfg, Store(data)
        kw = {"cache_dir": os.path.join(data, "tx"), "log": log}
        if transport:
            kw["transport"] = transport
        self.fg = Rpc(PUBLICNODE, MAINNET_BETA, rps=cfg["fg_rps"], **kw)
        self.bg = Rpc(PUBLICNODE, MAINNET_BETA, rps=cfg["bg_rps"], **kw)
        self.stop = threading.Event()
        # mid -> the last polled (sol_raw, tok_raw, sol_px, tok_px), kept in the state so that a
        # restart neither repeats every row nor resets the death clock of quiet curves.
        self.last_raw = {int(k): tuple(v) for k, v in self.store.state["last_key"].items()}
        self.errors = 0
        self.counted = 0
        self.started = time.time()
        self.ensure_sol_usd()
        self.upgrade_cursors()
        self.retire_late(self.started)

    # ---- bookkeeping ----

    def count_calls(self):
        """Adds the calls since the last count to today's total (``state.calls``)."""
        day = utc_day(time.time())
        calls = self.fg.stats["calls"] + self.bg.stats["calls"]
        with self.store.lock:
            self.store.state["calls"][day] = self.store.state["calls"].get(day, 0) + calls - self.counted
            self.counted = calls

    def ensure_sol_usd(self):
        with self.store.lock:
            if str(SOL_USD_MID) in self.store.universe["markets"]:
                return
            self.store.universe["markets"][str(SOL_USD_MID)] = {
                "mid": SOL_USD_MID, "venue": "raydium_v4", "pool": U.SOL_USDC_V4, "mint": D.USDC,
                "tier": "SOL", "reason": "sol-usd", "added": time.time(), "sol_is_first": True}

    def upgrade_cursors(self):
        """Gives each bare-signature listing cursor (state before 2026-10-02) its block time (the
        newest ``activity`` second of the market) and last listing time, so that ``list_since``
        can find it once it expires."""
        st = self.store.state
        bare = [mid for mid, c in st["cursors"].items() if isinstance(c, str)]
        if not bare:
            return
        newest = {}
        root = os.path.join(self.store.data, "tapes")
        for day in sorted(os.listdir(root)):
            for path in (self.store.tape_path(day, "activity"), self.store.tape_path(day, "activity") + ".gz"):
                if not os.path.exists(path):
                    continue
                with (gzip.open if path.endswith(".gz") else open)(path, "rt", newline="") as f:
                    for row in csv.DictReader(f):
                        newest[row["mid"]] = max(newest.get(row["mid"], 0), int(row["block_time"]))
        with self.store.lock:
            for mid in bare:
                st["cursors"][mid] = {"sig": st["cursors"][mid], "slot": None, "bt": newest.get(mid),
                                      "listed_at": st["last_list"].get(mid)}
        log(f"upgraded {len(bare)} listing cursors")

    def retire_late(self, now):
        """Ends the cohort markets found more than ``late_s`` after their birth (creation or
        migration) and flags them ``late``: the cohorts must be followed from birth, and these
        only entered because they had survived (``coverage`` leaves them out)."""
        for m in self.store.markets():
            born = m.get("created_at") if m["reason"] == "cohort-create" else \
                m.get("graduated_at") if m["reason"] == "fresh-migration" else None
            if born and m["added"] - born > self.cfg["late_s"]:
                self.store.update_market(m["mid"], late=round(m["added"] - born))
                self.store.end_market(m["mid"], "late", now)

    # ---- fg: polls ----

    def poll(self, tiers):
        ms = [m for m in self.store.markets() if (m["tier"] in FAST_TIERS) == (tiers == "fast")]
        curves = [m for m in ms if m["venue"] == "pump_curve"]
        pools = [m for m in ms if m["venue"] != "pump_curve" and m.get("sol_vault") and m.get("tok_vault")]
        now = time.time()
        rows = []
        for i in range(0, len(curves), 100):
            chunk = curves[i:i + 100]
            slot, vals = self.fg.multiple([m["pool"] for m in chunk], data_slice=(8, 41))
            t = time.time()
            for m, v in zip(chunk, vals):
                if v is None:
                    continue
                d = base64.b64decode(v["data"][0])
                if len(d) < 41:
                    continue
                vt, vq, rt, rq = struct.unpack_from("<QQQQ", d, 0)
                rows += self._row(m, t, slot, vq, vt, rq, rt)
                if d[40] == 1:
                    self.graduated(m, t, slot)
        for i in range(0, len(pools), 50):
            chunk = pools[i:i + 50]
            keys = [k for m in chunk for k in (m["sol_vault"], m["tok_vault"])]
            slot, vals = self.fg.multiple(keys, data_slice=(64, 8))
            t = time.time()
            for j, m in enumerate(chunk):
                a, b = vals[2 * j], vals[2 * j + 1]
                if a is None or b is None:
                    continue
                sol = struct.unpack("<Q", base64.b64decode(a["data"][0]))[0]
                tok = struct.unpack("<Q", base64.b64decode(b["data"][0]))[0]
                sol_px = sol - m.get("sol_counter", 0) + m.get("vqr", 0)  # PumpSwap: Y = vault + VQR
                tok_px = tok - m.get("tok_counter", 0)
                rows += self._row(m, t, slot, sol_px, tok_px, sol, tok)
        self.store.write("poll", rows, now)
        if tiers == "fast":
            self.check_ends(now)

    def _row(self, m, t, slot, sol_px, tok_px, sol_raw, tok_raw):
        key = (sol_raw, tok_raw, sol_px, tok_px)
        mid = m["mid"]
        if self.last_raw.get(mid) == key:
            return []
        self.last_raw[mid] = key
        with self.store.lock:
            self.store.state["last_change"][str(mid)] = t
            self.store.state["last_key"][str(mid)] = list(key)
        return [[f"{t:.3f}", slot, mid, sol_px, tok_px, sol_raw, tok_raw]]

    def check_ends(self, now):
        for m in self.store.markets():
            if m["mid"] == SOL_USD_MID:
                continue
            last = self.store.state["last_change"].get(str(m["mid"]), m["added"])
            if m["venue"] == "pump_curve" and now - last > self.cfg["dead_s"]:
                self.store.end_market(m["mid"], "dead", last)  # the market ended at its last trade
            elif m["reason"].startswith(("fresh", "graduate")) and now - m["added"] > U.FRESH_DAYS * 86400:
                self.store.end_market(m["mid"], "fresh window over", now)

    def graduated(self, m, t, slot):
        self.store.end_market(m["mid"], "graduated", t, slot)
        pool = U.canonical_pool_of(m["mint"])
        self.store.add_market({"venue": "pump_swap", "pool": pool, "mint": m["mint"], "tier": "C",
                               "reason": "graduate-of-D", "graduated_at": t}, t, slot)

    # ---- fg: venue state ----

    def refresh_states(self):
        ms = [m for m in self.store.markets() if m["venue"] != "pump_curve"]
        for i in range(0, len(ms), 100):
            chunk = ms[i:i + 100]
            slot, vals = self.fg.multiple([m["pool"] for m in chunk])
            t = time.time()
            for m, v in zip(chunk, vals):
                if v is None:
                    continue
                d = base64.b64decode(v["data"][0])
                try:
                    self.apply_state(m, d, v["owner"], t, slot)
                except (struct.error, IndexError, ValueError) as e:
                    self.store.write("events", [[f"{t:.3f}", slot, m["mid"], "state_error", repr(e)]], t)

    def apply_state(self, m, d, owner, t, slot):
        upd, ev = {}, []
        if m["venue"] == "pump_swap" and owner == D.PAMM:
            p = D.decode_pamm_pool(d)
            upd = {"vqr": p["virtual_quote_reserves"], "sol_vault": p["quote_vault"],
                   "tok_vault": p["base_vault"], "lp_mint": p["lp_mint"]}
            if p["quote_mint"] != D.WSOL or p["is_mayhem_mode"]:
                self.store.end_market(m["mid"], "ineligible (quote / mayhem)", t, slot)
        elif m["venue"] == "raydium_cpmm" and owner == D.CPMM:
            p = D.decode_cpmm_pool(d)
            c0, c1 = D.cpmm_counters(p)
            first = m.get("sol_is_first", p["token_0_mint"] == D.WSOL)
            upd = {"sol_counter": c0 if first else c1, "tok_counter": c1 if first else c0,
                   "status": p["status"]}
        elif m["venue"] == "raydium_v4" and owner == D.AMMV4:
            a = D.decode_amm_info(d)
            first = a["coin_mint"] == D.WSOL
            upd = {"sol_counter": a["need_take_pnl_coin"] if first else a["need_take_pnl_pc"],
                   "tok_counter": a["need_take_pnl_pc"] if first else a["need_take_pnl_coin"],
                   "status": a["status"], "sol_vault": a["coin_vault"] if first else a["pc_vault"],
                   "tok_vault": a["pc_vault"] if first else a["coin_vault"], "sol_is_first": first}
        for k in ("vqr", "sol_counter", "status"):
            if k in upd and upd[k] != m.get(k):
                ev.append([f"{t:.3f}", slot, m["mid"], k, upd[k]])
        if upd:
            self.store.update_market(m["mid"], **upd)
        self.store.write("events", ev, t)

    # ---- fg: listings ----

    def list_due(self, now):
        """List the most overdue market's new signatures (one market per call). An ended market
        is listed once more after its end, so that its last trades are not lost."""
        last = self.store.state["last_list"]
        ms = [m for m in self.store.markets(live=False) if m["mid"] != SOL_USD_MID
              and (not m.get("ended") or last.get(str(m["mid"]), 0) <= m["ended"])]
        if not ms:
            return False

        def due(m):  # an ended market is due at once
            return 0 if m.get("ended") else last.get(str(m["mid"]), 0)

        m = min(ms, key=due)
        if now - due(m) < self.cfg["list_period"]:
            return False
        try:
            self.list_market(m)
        finally:  # a failed listing waits its turn too, so one market cannot starve the rest
            with self.store.lock:
                last[str(m["mid"])] = max(now, m.get("ended") or 0) + (1 if m.get("ended") else 0)
        return True

    def note(self, mid, notes):
        t = time.time()
        self.store.write("events", [[f"{t:.3f}", "", mid, kind, value] for kind, value in notes], t)

    def list_market(self, m):
        mid = str(m["mid"])
        sigs, cursor, notes = list_since(self.fg, m["pool"], self.store.state["cursors"].get(mid),
                                         self.cfg["list_max_pages"], time.time())
        self.note(m["mid"], notes)
        with self.store.lock:
            if cursor:
                self.store.state["cursors"][mid] = cursor
        if not sigs:
            return
        per_sec = collections.OrderedDict()
        for s in sigs:  # newest first
            bt = s.get("blockTime")
            if bt is None:
                continue
            per_sec.setdefault(bt, []).append(s)
        rows = [[bt, m["mid"], sum(1 for s in ss if s["err"] is None),
                 sum(1 for s in ss if s["err"] is not None)] for bt, ss in reversed(per_sec.items())]
        self.store.write("activity", rows)
        self.sample(m, per_sec)

    def sample(self, m, per_sec):
        """Queue transactions to decode: a curve's every successful signature (up to its cap);
        a pool's last successful signatures of a seeded random sample of active seconds."""
        mid = str(m["mid"])
        active = [(bt, [s for s in ss if s["err"] is None]) for bt, ss in per_sec.items()]
        active = [(bt, ok) for bt, ok in active if ok]
        items = []
        if m["venue"] == "pump_curve":
            used = self.store.state["quota"].get(mid, 0)
            for bt, ok in active:
                for s in ok:
                    if used >= self.cfg["curve_tx_cap"]:
                        break
                    items.append([m["mid"], bt, [s["signature"]]])
                    used += 1
            with self.store.lock:
                self.store.state["quota"][mid] = used
        else:
            # Expected `trade_quota` a day: this listing covers `list_period` seconds.
            want = self.cfg["trade_quota"] * self.cfg["list_period"] / 86400
            rate = min(1.0, want / max(len(active), 1))
            for bt, ok in active:
                if U.sample_hit(f"{m['mid']}:{bt}", rate):
                    items.append([m["mid"], bt, [s["signature"] for s in ok[:3]]])
        with self.store.lock:
            q = self.store.state["queue"]
            room = self.cfg["queue_max"] - len(q)
            if len(items) > room:
                self.store.write("events", [[f"{time.time():.3f}", "", m["mid"], "queue_full",
                                             len(items) - max(room, 0)]])
            q.extend(items[:max(room, 0)])

    # ---- fg: discovery ----

    def discover(self):
        for kind, addr, rate in (("create", U.PUMP_MINT_AUTHORITY, self.cfg["create_rate"]),
                                 ("migration", U.PUMP_WITHDRAW_AUTHORITY, self.cfg["migration_rate"])):
            cursor = self.store.state["disc"].get(kind)
            # One page (1,000 signatures, about 40 min of creations): a longer outage is a gap.
            page, new, notes = list_since(self.fg, addr, cursor, 1, time.time())
            self.note("", [(k, f"{kind}|{v}") for k, v in notes])
            with self.store.lock:
                if new:
                    self.store.state["disc"][kind] = new
                if cursor is None:  # first run: start from now
                    continue
                for s in page:
                    if s["err"] is None and U.sample_hit(s["signature"], rate):
                        self.store.state["hi"].append([kind, s["signature"], s.get("slot")])
        # The sampled transactions are few (about one a minute): fetched here, on the fg budget,
        # so that a running census (bg) does not hold the cohorts back.
        while not self.stop.is_set():
            with self.store.lock:
                if not self.store.state["hi"]:
                    break
                kind, sig, _ = self.store.state["hi"][0]
            self.guarded("discovery tx", self.handle_discovery, kind, sig)
            with self.store.lock:
                self.store.state["hi"].pop(0)

    # ---- bg: discovery transactions, trades, census ----

    def handle_discovery(self, kind, sig):
        tx = self.fg.transaction(sig)
        now = time.time()
        if tx is None:
            return
        lag = now - (tx.get("blockTime") or now)
        if lag > self.cfg["late_s"]:  # found too late to follow from birth (after an outage)
            self.store.write("events", [[f"{now:.3f}", tx.get("slot"), "", f"skip_{kind}",
                                         f"{sig}|late lag={lag:.0f}"]], now)
            return
        if kind == "create":
            for mint, curve in U.parse_create(tx):
                info = U._multiple(self.fg, [curve, mint])
                cv, mv = info.get(curve), info.get(mint)
                if not cv or not mv:
                    continue
                c = D.decode_curve(cv[1])
                why = D.mint_check(mint, mv[0], mv[1])
                if c["is_mayhem_mode"] or c["quote_mint"] != D.SYSTEM or why or self.live_count("cohort-create") >= self.cfg["d_max"]:
                    self.store.write("events", [[f"{now:.3f}", tx.get("slot"), "", "skip_create",
                                                 f"{mint}|mayhem={c['is_mayhem_mode']}|quote={c['quote_mint']}|{why}"]], now)
                    continue
                self.store.add_market({"venue": "pump_curve", "pool": curve, "mint": mint, "tier": "D",
                                       "reason": "cohort-create", "created_at": tx.get("blockTime")},
                                      now, tx.get("slot"))
        else:
            for pool, mint, quote in U.parse_migration(tx):
                if quote != D.WSOL or U.canonical_pool_of(mint) != pool:
                    continue
                mv = U._multiple(self.fg, [mint]).get(mint)
                why = "Missing" if not mv else D.mint_check(mint, mv[0], mv[1])
                if why or self.live_count("fresh") >= self.cfg["fresh_max"]:
                    self.store.write("events", [[f"{now:.3f}", tx.get("slot"), "", "skip_migration", f"{mint}|{why or 'cap'}"]], now)
                    continue
                self.store.add_market({"venue": "pump_swap", "pool": pool, "mint": mint, "tier": "C",
                                       "reason": "fresh-migration", "graduated_at": tx.get("blockTime")},
                                      now, tx.get("slot"))

    def live_count(self, reason_prefix):
        return sum(1 for m in self.store.markets() if m["reason"].startswith(reason_prefix))

    def handle_trade(self, item):
        mid, bt, sigs = item
        m = self.store.universe["markets"].get(str(mid))
        if m is None:
            return
        for sig in sigs:
            tx = self.bg.transaction(sig)
            rows = trade_rows(m, sig, tx)
            if rows:
                self.store.write("trades", rows)  # by arrival day; the rows carry block_time
                return

    def census_due(self, now):
        return now - self.store.universe.get("census_time", 0) > self.cfg["census_every"]

    def run_census(self):
        """Runs a census (or adopts one younger than ``census_every`` that ``universe.py census``
        saved) and merges its selection into the universe."""
        rec, path = U.latest_census(self.store.data, with_path=True)
        if rec is None or time.time() - rec["time"] > self.cfg["census_every"]:
            rec = U.census(self.bg, self.bg, log=log, previous=rec)
            path = U.save_census(self.store.data, rec)
        now = time.time()
        current = {m["pool"]: m["tier"] for m in self.store.markets() if m["reason"].startswith("census")}
        chosen = U.select(rec, current, seed=int(rec["time"]))
        eligible = {p["pool"] for p in rec["pools"]}
        for m in self.store.markets():
            if m["reason"].startswith("census") and m["pool"] not in eligible:
                self.store.end_market(m["mid"], "ineligible at census", now)
        rates = {}
        cfgs = [p["amm_config"] for p, _ in chosen if p.get("amm_config")]
        for k, v in U._multiple(self.bg, cfgs).items():
            if v:
                c = D.decode_cpmm_config(v[1])
                rates[k] = [c["protocol_fee_rate"], c["fund_fee_rate"]]
        for p, reason in chosen:
            if reason == "census-kept":
                continue
            rec_m = {k: p.get(k) for k in ("venue", "pool", "mint", "sol_vault", "tok_vault", "tier",
                                          "lp_mint", "sol_is_first", "amm_config", "age_s", "r_eff",
                                          "lambda_bps")}
            rec_m.update(reason=reason, vqr=p.get("vqr", 0), sol_counter=p.get("sol_counter", 0),
                         tok_counter=p.get("tok_counter", 0), rates=rates.get(p.get("amm_config")))
            self.store.add_market(rec_m, now)
        with self.store.lock:
            self.store.universe["census_time"] = rec["time"]
            self.store.universe["sol_usd"] = rec["sol_usd"]
            self.store.universe["census_file"] = os.path.relpath(path, self.store.data)
        self.store.save()
        log(f"census done: {len(rec['pools'])} eligible pools, {len(chosen)} selected")

    # ---- loops ----

    def guarded(self, name, fn, *a):
        try:
            return fn(*a)
        except RpcError as e:
            self.errors += 1
            log(f"{name}: rpc error {e}")
        except Exception:  # keep crawling; the traceback is in the log
            self.errors += 1
            log(f"{name}: unexpected error\n{traceback.format_exc()}")
            time.sleep(5)
        return None

    def fg_loop(self):
        """Earliest-deadline-first over the periodic jobs, so that a late poll cannot starve the
        others; listings fill the idle time."""
        c = self.cfg
        now = time.time()
        jobs = {  # name -> [next due, period, function]
            "state": [now, c["state_period"], self.refresh_states],
            "fast": [now, c["poll_fast"], lambda: self.poll("fast")],
            "slow": [now, c["poll_slow"], lambda: self.poll("slow")],
            "disc": [now, c["discovery_period"], self.discover],
            "save": [now + 60, 60.0, self.checkpoint],
            "stats": [now + c["stats_period"], c["stats_period"], self.log_stats],
        }
        while not self.stop.is_set():
            now = time.time()
            name, job = min(jobs.items(), key=lambda kv: kv[1][0])
            if job[0] <= now:
                job[0] = max(job[0] + job[1], now)
                self.guarded(name, job[2])
            elif not self.guarded("listing", self.list_due, now):
                self.stop.wait(max(0.05, min(j[0] for j in jobs.values()) - time.time()))

    def checkpoint(self):
        self.count_calls()
        self.store.save()
        self.store.finalize_past_days()

    def bg_loop(self):
        # A restart re-applies the selection of the current census (cheap, idempotent: kept
        # markets stay), so that changed caps take effect without a new census.
        if self.store.universe.get("census_time") and not self.census_due(time.time()) \
                and not self.cfg.get("no_census"):
            self.guarded("reselect", self.run_census)
        while not self.stop.is_set():
            if self.census_due(time.time()) and not self.cfg.get("no_census"):
                self.guarded("census", self.run_census)
                if self.census_due(time.time()):  # failed: retry in an hour
                    with self.store.lock:
                        self.store.universe["census_time"] = time.time() - self.cfg["census_every"] + 3600
                continue
            with self.store.lock:
                item = self.store.state["queue"].pop(0) if self.store.state["queue"] else None
            if item:
                self.guarded("trade tx", self.handle_trade, item)
            else:
                self.stop.wait(1.0)

    def log_stats(self):
        el = max(time.time() - self.started, 1)
        calls = self.fg.stats["calls"] + self.bg.stats["calls"]
        live = self.store.markets()
        tiers = collections.Counter(m["tier"] for m in live)
        log(f"stats: {calls} calls in {el:.0f}s = {calls / el:.2f}/s (fg {self.fg.stats['calls']}, "
            f"bg {self.bg.stats['calls']}, retries {self.fg.stats['retries'] + self.bg.stats['retries']}, "
            f"cache hits {self.bg.stats['cache_hits']}), errors {self.errors}, markets {dict(tiers)}, "
            f"queue {len(self.store.state['queue'])}, discovery {len(self.store.state['hi'])}; "
            f"by method {json.dumps({**self.fg.stats['by'], **{'bg:' + k: v for k, v in self.bg.stats['by'].items()}})}")
        with self.store.lock:
            self.store.state["last_stats"] = {"t": time.time(), "calls": calls, "elapsed": el,
                                              "errors": self.errors, "retries": self.fg.stats["retries"] + self.bg.stats["retries"]}

    def run(self):
        log(f"crawl start: data {self.store.data}, cfg {json.dumps(self.cfg)}")
        self.note("", [("crawl_start", os.getpid())])  # polls resume: the hole since crawl_stop
        threads = [threading.Thread(target=self.fg_loop, name="fg", daemon=True),
                   threading.Thread(target=self.bg_loop, name="bg", daemon=True)]
        for th in threads:
            th.start()
        try:
            while not self.stop.is_set():
                self.stop.wait(1.0)
        finally:
            for th in threads:
                th.join(timeout=120)
            self.count_calls()
            self.log_stats()
            self.note("", [("crawl_stop", os.getpid())])
            self.store.save()
            self.store.close()
            log("crawl stopped; state saved")


def trade_rows(m, sig, tx):
    """Trade-tape rows of market ``m`` in transaction ``tx``."""
    if tx is None:
        return []
    v4 = {m["pool"]: m.get("sol_is_first", True)} if m["venue"] == "raydium_v4" else None
    rates = {m["pool"]: tuple(m["rates"])} if m.get("rates") else None
    trades, _ = D.decode_trades(tx, v4_orientation=v4, cpmm_rates=rates)
    rows = []
    for t in trades:
        if t["venue"] != m["venue"]:
            continue
        if m["venue"] == "pump_curve" and t["mint"] != m["mint"]:
            continue
        if m["venue"] != "pump_curve" and t.get("pool") != m["pool"]:
            continue
        rows.append([tx.get("blockTime"), tx.get("slot"), tx.get("transactionIndex", ""), sig,
                     t["ix"], t["seq"], m["mid"], t["side"], t["sol_amount"], t["token_amount"],
                     t.get("sol_before", ""), t.get("tok_before", ""), t["sol_after"], t["tok_after"]])
    return rows


# ---- commands -----------------------------------------------------------------------------------


def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except (OSError, TypeError):
        return False


def cmd_crawl(args):
    cfg = dict(CFG)
    for k in ("fg_rps", "bg_rps"):
        if getattr(args, k) is not None:
            cfg[k] = getattr(args, k)
    cfg["no_census"] = args.no_census
    os.makedirs(args.data, exist_ok=True)
    pidf = os.path.join(args.data, "crawl.pid")
    if os.path.exists(pidf):
        old = int(open(pidf).read().strip() or 0)
        if old != os.getpid() and pid_alive(old):
            raise SystemExit(f"a crawl is already running (pid {old})")
    with open(pidf, "w") as f:
        f.write(str(os.getpid()))
    cr = Crawler(args.data, cfg)
    signal.signal(signal.SIGTERM, lambda *_: cr.stop.set())
    signal.signal(signal.SIGINT, lambda *_: cr.stop.set())
    cr.run()
    return 0


def coverage(markets, now):
    """Market-days followed per tier (at entry) and per venue; cohort markets found late (not
    from birth: ``retire_late``) do not count."""
    out = collections.defaultdict(float)
    for m in markets:
        if m["mid"] == SOL_USD_MID or m.get("late"):
            continue
        end = m.get("ended") or now
        out[(m["tier"], m["venue"])] += max(end - m["added"], 0) / 86400
    return out


def cmd_status(args):
    d = args.data
    pidf = os.path.join(d, "crawl.pid")
    pid = int(open(pidf).read().strip()) if os.path.exists(pidf) else None
    print(f"crawl pid {pid}: {'running' if pid and pid_alive(pid) else 'not running'}")
    st = Store._load(os.path.join(d, "state.json"), {})
    uni = Store._load(os.path.join(d, "universe.json"), {"markets": {}})
    ls = st.get("last_stats")
    if ls:
        print(f"last stats {time.strftime('%Y-%m-%d %H:%M:%SZ', time.gmtime(ls['t']))}: {ls['calls']} calls "
              f"in {ls['elapsed']:.0f}s = {ls['calls'] / ls['elapsed']:.2f}/s, errors {ls['errors']}, retries {ls['retries']}")
    print("calls per UTC day:", st.get("calls"))
    now = time.time()
    ms = list(uni["markets"].values())
    live = [m for m in ms if not m.get("ended")]
    print(f"markets: {len(live)} live, {len(ms) - len(live)} ended ({sum(1 for m in ms if m.get('late'))} "
          f"cohort markets found late, not counted); census "
          f"{time.strftime('%Y-%m-%d %H:%M', time.gmtime(uni['census_time'])) if uni.get('census_time') else 'not yet'}"
          f", SOL/USD {uni.get('sol_usd')}")
    print(f"queue {len(st.get('queue', []))} transactions, {len(st.get('hi', []))} discovery")
    cov = coverage(ms, now)
    targets = {"A": 3210, "B": 1230, "C": 504, "D": 168}
    for t in "ABCD":
        days = sum(v for (tt, _), v in cov.items() if tt == t)
        nlive = sum(1 for m in live if m["tier"] == t)
        print(f"  tier {t}: {nlive:3d} live, {days:8.2f} market-days of {targets[t]} target "
              + " ".join(f"{v}={x:.2f}" for (tt, v), x in sorted(cov.items()) if tt == t))
    days = sorted(os.listdir(os.path.join(d, "tapes"))) if os.path.isdir(os.path.join(d, "tapes")) else []
    for day in days[-3:]:
        sizes = {}
        for tape in TAPES:
            for ext in (".csv", ".csv.gz"):
                p = os.path.join(d, "tapes", day, tape + ext)
                if os.path.exists(p):
                    sizes[tape + ext] = os.path.getsize(p)
        print(f"  {day}: " + ", ".join(f"{k} {v / 1e6:.1f} MB" for k, v in sizes.items()))
    return 0


def cmd_stop(args):
    pidf = os.path.join(args.data, "crawl.pid")
    if not os.path.exists(pidf):
        print("no crawl.pid")
        return 1
    pid = int(open(pidf).read().strip())
    if not pid_alive(pid):
        print(f"pid {pid} not running")
        return 1
    os.kill(pid, signal.SIGTERM)
    for _ in range(300):
        if not pid_alive(pid):
            print(f"stopped {pid}")
            return 0
        time.sleep(1)
    print(f"pid {pid} still running after 300 s")
    return 1


def replay_day(data, day, transport=None):
    """Re-decodes ``day``'s trade tape from the transaction cache with a transport that refuses
    every request; returns (rows equal?, rows, calls)."""
    def refuse(url, body, timeout):
        raise AssertionError(f"replay made an RPC call: {body[:80]!r}")
    rpc = Rpc(PUBLICNODE, MAINNET_BETA, rps=MAX_RPS, cache_dir=os.path.join(data, "tx"),
              transport=transport or refuse)
    uni = Store._load(os.path.join(data, "universe.json"), {"markets": {}})
    path = os.path.join(data, "tapes", day, "trades.csv")
    opener = open
    if not os.path.exists(path):
        path, opener = path + ".gz", gzip.open
    with opener(path, "rt", newline="") as f:
        stored = list(csv.reader(f))[1:]
    out, seen = [], set()
    for row in stored:
        key = (row[3], row[6])
        if key in seen:
            continue
        seen.add(key)
        m = uni["markets"][row[6]]
        out += [["" if x is None else str(x) for x in r]
                for r in trade_rows(m, row[3], rpc.transaction(row[3]))]
    return out == stored, len(stored), rpc.stats["calls"]


def cmd_replay(args):
    same, n, calls = replay_day(args.data, args.day)
    print(f"{args.day}: {n} trade rows re-decoded from the cache, identical: {same}, RPC calls: {calls}")
    return 0 if same and calls == 0 else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["crawl", "status", "stop", "replay"])
    ap.add_argument("--data", default=U.default_data())
    ap.add_argument("--fg-rps", dest="fg_rps", type=float)
    ap.add_argument("--bg-rps", dest="bg_rps", type=float)
    ap.add_argument("--no-census", action="store_true", help="never run the census (tests)")
    ap.add_argument("--day", help="replay: the UTC day (YYYY-MM-DD)")
    args = ap.parse_args()
    return {"crawl": cmd_crawl, "status": cmd_status, "stop": cmd_stop, "replay": cmd_replay}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
