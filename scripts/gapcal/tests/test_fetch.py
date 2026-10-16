"""Offline tests of ``fetch.py``: day tapes and the manifest (late rows, crash-safe merges), a
finished day's replay costing 0 calls, the trade-tape rows, the poll's pricing reserves and market
lifecycle on a fake RPC (restarts, late cohort markets), and signature listings whose cursor the
endpoint no longer holds.

    python3 -m unittest discover scripts/gapcal/tests
"""

import base64
import csv
import gzip
import hashlib
import json
import os
import struct
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import decode as D  # noqa: E402
import fetch as F  # noqa: E402
import universe as U  # noqa: E402

DAY = "2026-10-01"
T_DAY = 1_790_856_000  # 2026-10-01T12:00:00Z


def fixture(name):
    with gzip.open(os.path.join(HERE, "fixtures", name), "rt") as f:
        return json.load(f)


def markets():
    pamm, cpmm, v4, pump = (fixture(f"{n}.json.gz") for n in ("pamm", "cpmm", "ammv4", "pump"))
    ms = {
        "1": {"mid": 1, "venue": "pump_swap", "pool": pamm["pool"], "mint": pamm["mint"], "tier": "B",
              "reason": "census-B", "added": T_DAY},
        "2": {"mid": 2, "venue": "raydium_cpmm", "pool": cpmm["pool"], "tier": "C", "reason": "census-C",
              "added": T_DAY, "rates": cpmm["rates"]},
        "3": {"mid": 3, "venue": "raydium_v4", "pool": v4["pools"][0]["pool"], "tier": "A",
              "reason": "census-A", "added": T_DAY, "sol_is_first": v4["pools"][0]["sol_is_coin"]},
        "4": {"mid": 4, "venue": "pump_curve", "pool": pump["curve"], "mint": pump["mint"], "tier": "D",
              "reason": "cohort-create", "added": T_DAY},
    }
    return ms, {"1": pamm, "2": cpmm, "3": v4["pools"][0], "4": pump}


class Tapes(unittest.TestCase):
    def test_closed_days_are_gzipped_and_hashed(self):
        with tempfile.TemporaryDirectory() as d:
            st = F.Store(d)
            st.write("poll", [[f"{T_DAY + i}.000", 100 + i, 1, 5, 6, 5, 6] for i in range(3)], T_DAY)
            st.write("events", [[f"{T_DAY}.000", 100, 1, "added", "x"]], T_DAY)
            st.finalize_past_days()
            st.close()
            gz = os.path.join(d, "tapes", DAY, "poll.csv.gz")
            self.assertTrue(os.path.exists(gz))
            self.assertFalse(os.path.exists(gz[:-3]))
            e = st.manifest["days"][DAY]["poll"]
            with open(gz, "rb") as f:
                self.assertEqual(e["sha256"], hashlib.sha256(f.read()).hexdigest())
            self.assertEqual((e["rows"], e["slot_min"], e["slot_max"]), (3, 100, 102))
            with gzip.open(gz, "rt") as f:
                self.assertEqual(next(csv.reader(f)), F.TAPES["poll"])
            # Rows of a closed day that arrive late are merged into its tape, not overwriting it.
            st.write("poll", [[f"{T_DAY + 9}.000", 109, 1, 7, 8, 7, 8]], T_DAY)
            st.close()
            st.finalize_past_days()
            with gzip.open(gz, "rt") as f:
                self.assertEqual([r[1] for r in csv.reader(f)], ["slot", "100", "101", "102", "109"])
            e = st.manifest["days"][DAY]["poll"]
            self.assertEqual((e["rows"], e["slot_max"]), (4, 109))
            # Idempotent, and a restarted store finalizes nothing twice.
            F.Store(d).finalize_past_days()
            self.assertEqual(F.Store._load(os.path.join(d, "manifest.json"), {})["days"][DAY]["poll"]["sha256"],
                             e["sha256"])

    def test_a_merge_cut_short_by_a_crash_is_not_repeated(self):
        with tempfile.TemporaryDirectory() as d:
            st = F.Store(d)
            st.write("poll", [[f"{T_DAY}.000", 100, 1, 5, 6, 5, 6]], T_DAY)
            st.finalize_past_days()
            st.write("poll", [[f"{T_DAY + 9}.000", 109, 1, 7, 8, 7, 8]], T_DAY)
            st.close()
            plain = os.path.join(d, "tapes", DAY, "poll.csv")
            # The crash: the merged tape replaced the .gz, the plain file was not removed yet.
            with gzip.open(plain + ".gz", "rb") as f:
                old = f.read()
            with open(plain, "rb") as f:
                late = f.read().split(b"\n", 1)[1]
            with gzip.open(plain + ".gz", "wb") as f:
                f.write(old + late)
            F.Store(d).finalize_past_days()
            with gzip.open(plain + ".gz", "rt") as f:
                self.assertEqual([r[1] for r in csv.reader(f)], ["slot", "100", "109"])
            self.assertFalse(os.path.exists(plain))


class Replay(unittest.TestCase):
    def test_a_finished_day_replays_with_zero_calls(self):
        with tempfile.TemporaryDirectory() as d:
            ms, fx = markets()
            st = F.Store(d)
            st.universe["markets"].update(ms)
            st.save()
            n = 0
            for mid, f in fx.items():
                for tx in f["txs"]:
                    sig = tx["transaction"]["signatures"][0]
                    path = os.path.join(d, "tx", sig[:2], sig + ".json.gz")
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    with gzip.open(path, "wt") as out:
                        json.dump(tx, out)
                    rows = F.trade_rows(ms[mid], sig, tx)
                    self.assertTrue(rows, (mid, sig))
                    n += len(rows)
                    st.write("trades", rows, T_DAY)
            st.finalize_past_days()
            st.close()
            same, count, calls = F.replay_day(d, DAY)
            self.assertTrue(same)
            self.assertEqual((count, calls), (n, 0))
            self.assertGreaterEqual(n, 80)

    def test_trade_rows_keep_only_the_market(self):
        ms, fx = markets()
        tx = fx["2"]["txs"][0]
        sig = tx["transaction"]["signatures"][0]
        rows = F.trade_rows(ms["2"], sig, tx)
        self.assertTrue(all(r[6] == 2 for r in rows))
        other = dict(ms["2"], pool="11111111111111111111111111111111")
        self.assertEqual(F.trade_rows(other, sig, tx), [])
        self.assertEqual(F.trade_rows(ms["2"], sig, None), [])


class FakeChain:
    """A transport serving getMultipleAccounts / getSignaturesForAddress from dicts;
    ``broken`` addresses answer every listing with a non-retryable RPC error."""

    def __init__(self):
        self.accounts, self.requests, self.listed, self.broken = {}, [], [], set()

    def __call__(self, url, body, timeout):
        req = json.loads(body)
        self.requests.append(req["method"])
        if req["method"] == "getSignaturesForAddress":
            self.listed.append(req["params"][0])
            if req["params"][0] in self.broken:
                err = {"code": -32602, "message": "broken"}
                return 200, {}, json.dumps({"jsonrpc": "2.0", "id": req["id"], "error": err}).encode()
        if req["method"] == "getMultipleAccounts":
            keys, opts = req["params"]
            out = []
            for k in keys:
                v = self.accounts.get(k)
                if v is None:
                    out.append(None)
                    continue
                data = v[1]
                if "dataSlice" in opts:
                    data = data[opts["dataSlice"]["offset"]:opts["dataSlice"]["offset"] + opts["dataSlice"]["length"]]
                out.append({"owner": v[0], "data": [base64.b64encode(data).decode(), "base64"]})
            res = {"context": {"slot": 7}, "value": out}
        else:
            res = []
        return 200, {}, json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": res}).encode()


def token_acc(amount):
    d = bytearray(165)
    d[64:72] = struct.pack("<Q", amount)
    return (D.TOKEN, bytes(d))


class Poll(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.chain = FakeChain()
        self.cr = F.Crawler(self.dir.name, dict(F.CFG, no_census=True), transport=self.chain)

    def tearDown(self):
        self.cr.store.close()
        self.dir.cleanup()

    def rows(self):
        st = self.cr.store
        with open(st.tape_path(F.utc_day(time.time()), "poll")) as f:
            return list(csv.DictReader(f))

    def test_pricing_reserves(self):
        now = time.time()
        st = self.cr.store
        st.add_market({"venue": "pump_swap", "pool": "P1", "sol_vault": "S1", "tok_vault": "T1",
                       "tier": "C", "reason": "fresh-migration", "vqr": -5}, now)
        st.add_market({"venue": "raydium_cpmm", "pool": "P2", "sol_vault": "S2", "tok_vault": "T2",
                       "tier": "C", "reason": "census-C", "sol_counter": 7, "tok_counter": 3}, now)
        self.chain.accounts.update({"S1": token_acc(1000), "T1": token_acc(2000),
                                    "S2": token_acc(500), "T2": token_acc(600)})
        self.cr.poll("fast")
        self.cr.poll("fast")  # unchanged: no new rows
        got = {r["mid"]: (int(r["sol_px"]), int(r["tok_px"]), int(r["sol_raw"])) for r in self.rows()}
        self.assertEqual(got, {"1": (995, 2000, 1000), "2": (493, 597, 500)})
        self.assertEqual(len(self.rows()), 2)

    def test_curve_graduates_into_its_canonical_pool(self):
        now = time.time()
        mint = fixture("pamm.json.gz")["mint"]
        mid = self.cr.store.add_market({"venue": "pump_curve", "pool": "C1", "mint": mint, "tier": "D",
                                        "reason": "cohort-create"}, now)
        d = bytearray(151)
        d[8:40] = struct.pack("<QQQQ", 10, 20, 1, 2)
        self.chain.accounts["C1"] = (D.PUMP, bytes(d))
        self.cr.poll("fast")
        self.assertEqual(self.rows()[-1]["sol_px"], "20")
        d[48] = 1
        self.chain.accounts["C1"] = (D.PUMP, bytes(d))
        self.cr.poll("fast")
        ms = {m["mid"]: m for m in self.cr.store.markets(live=False)}
        self.assertEqual(ms[mid]["end_reason"], "graduated")
        grad = [m for m in ms.values() if m["reason"] == "graduate-of-D"]
        self.assertEqual([(m["pool"], m["tier"]) for m in grad], [(U.canonical_pool_of(mint), "C")])

    def test_a_quiet_curve_dies_at_its_last_change(self):
        now = time.time()
        mid = self.cr.store.add_market({"venue": "pump_curve", "pool": "C2", "mint": "M", "tier": "D",
                                        "reason": "cohort-create"}, now)
        last = now - 30 * 3600  # yesterday: the day's tapes are closed and hashed
        self.cr.store.state["last_change"][str(mid)] = last
        self.cr.check_ends(now)
        m = [m for m in self.cr.store.markets(live=False) if m["mid"] == mid][0]
        self.assertEqual((m["end_reason"], m["ended"]), ("dead", last))
        # The event goes to today's tape, the end time in its value; no closed day reopens.
        self.cr.store.close()
        self.assertEqual(os.listdir(os.path.join(self.dir.name, "tapes")), [F.utc_day(time.time())])
        with open(self.cr.store.tape_path(F.utc_day(time.time()), "events")) as f:
            ended = [r for r in csv.DictReader(f) if r["kind"] == "ended"]
        self.assertEqual([r["value"] for r in ended], [f"dead|{last:.3f}"])

    def test_a_restart_keeps_the_last_poll_and_the_death_clock(self):
        now = time.time()
        mid = self.cr.store.add_market({"venue": "pump_curve", "pool": "C4", "mint": "M", "tier": "D",
                                        "reason": "cohort-create"}, now)
        d = bytearray(151)
        d[8:40] = struct.pack("<QQQQ", 10, 20, 1, 2)
        self.chain.accounts["C4"] = (D.PUMP, bytes(d))
        self.cr.poll("fast")
        changed = self.cr.store.state["last_change"][str(mid)]
        self.cr.store.save()
        self.cr.store.close()
        self.cr = F.Crawler(self.dir.name, dict(F.CFG, no_census=True), transport=self.chain)
        self.cr.poll("fast")
        self.assertEqual([r["mid"] for r in self.rows()], [str(mid)])  # no repeated row
        self.assertEqual(self.cr.store.state["last_change"][str(mid)], changed)

    def test_cohort_markets_found_late_are_retired(self):
        now = time.time()
        st = self.cr.store
        late = st.add_market({"venue": "pump_curve", "pool": "C5", "mint": "M5", "tier": "D",
                              "reason": "cohort-create", "created_at": now - 3600}, now)
        on_time = st.add_market({"venue": "pump_curve", "pool": "C6", "mint": "M6", "tier": "D",
                                 "reason": "cohort-create", "created_at": now - 100}, now)
        st.save()
        st.close()
        self.cr = F.Crawler(self.dir.name, dict(F.CFG, no_census=True), transport=self.chain)
        ms = {m["mid"]: m for m in self.cr.store.markets(live=False)}
        self.assertEqual((ms[late]["end_reason"], ms[late]["late"]), ("late", 3600))
        self.assertNotIn("ended", ms[on_time])
        cov = F.coverage(list(ms.values()), now + 86_400)
        self.assertAlmostEqual(cov[("D", "pump_curve")], 1.0, places=3)  # the on-time curve only
        with open(st.tape_path(F.utc_day(now), "events")) as f:
            added = [r["value"] for r in csv.DictReader(f) if r["kind"] == "added"]
        self.assertTrue(added[-2].endswith("|lag=3600") and added[-1].endswith("|lag=100"), added)

    def test_a_creation_found_late_is_skipped(self):
        tx = fixture("discovery.json.gz")["create"]
        sig = tx["transaction"]["signatures"][0]
        path = os.path.join(self.dir.name, "tx", sig[:2], sig + ".json.gz")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with gzip.open(path, "wt") as f:
            json.dump(tx, f)
        self.assertGreater(time.time() - tx["blockTime"], F.CFG["late_s"])
        self.cr.handle_discovery("create", sig)
        self.assertEqual([m for m in self.cr.store.markets() if m["tier"] == "D"], [])
        self.assertEqual(self.chain.requests, [])  # decided from the cached transaction alone
        self.cr.store.close()
        with open(self.cr.store.tape_path(F.utc_day(time.time()), "events")) as f:
            self.assertEqual([r["kind"] for r in csv.DictReader(f)], ["skip_create"])

    def test_a_failing_listing_does_not_starve_the_rest(self):
        now = time.time()
        st = self.cr.store
        for pool in ("BAD", "OK1", "OK2"):
            st.add_market({"venue": "pump_curve", "pool": pool, "mint": pool, "tier": "D",
                           "reason": "cohort-create"}, now)
        self.chain.broken.add("BAD")
        for i in range(6):
            self.cr.guarded("listing", self.cr.list_due, now + i)
        self.assertEqual(self.chain.listed, ["BAD", "OK1", "OK2"])
        self.assertEqual(self.cr.errors, 1)

    def test_ended_markets_are_listed_once_more(self):
        now = time.time()
        st = self.cr.store
        mid = st.add_market({"venue": "pump_curve", "pool": "C3", "mint": "M", "tier": "D",
                             "reason": "cohort-create"}, now)
        self.assertTrue(self.cr.list_due(now))  # new: listed at once
        self.assertFalse(self.cr.list_due(now + 1))  # not due again for list_period
        st.end_market(mid, "graduated", now + 2)
        self.assertTrue(self.cr.list_due(now + 3))  # the final listing
        self.assertFalse(self.cr.list_due(now + 10_000))
        self.assertEqual(self.chain.requests.count("getSignaturesForAddress"), 2)

    def test_budget_is_enforced(self):
        with self.assertRaises(SystemExit):
            F.Crawler(self.dir.name, dict(F.CFG, fg_rps=2.5, bg_rps=1.0))


class History:
    """``getSignaturesForAddress`` over a synthetic history, newest first, as publicnode serves
    it: signatures older than ``kept`` are gone, and an ``until`` or ``before`` that is gone
    fails with -32020."""

    def __init__(self, n, kept=0):
        self.sigs = [{"signature": f"s{i}", "slot": 1000 + i, "blockTime": 10_000 + i, "err": None}
                     for i in reversed(range(n))]
        self.kept, self.calls = kept, []

    def signatures(self, address, before=None, until=None, limit=1000, endpoint="primary"):
        self.calls.append((before, until))
        idx = {s["signature"]: i for i, s in enumerate(self.sigs)}
        for s in (before, until):
            if s and (s not in idx or self.sigs[idx[s]]["blockTime"] < self.kept):
                raise F.RpcError("getSignaturesForAddress", "x",
                                 {"code": -32020, "message": f"Transaction {s} not found"})
        out = self.sigs[idx[before] + 1 if before else 0:]
        if until:
            out = out[:idx[until] - (idx[before] + 1 if before else 0)]
        return [s for s in out if s["blockTime"] >= self.kept][:limit]


class Listing(unittest.TestCase):
    NOW = 10_000 + 3000 + 30 * 3600

    def test_a_live_cursor_lists_with_until(self):
        h = History(3000)
        sigs, cur, notes = F.list_since(h, "A", {"sig": "s499", "slot": 1499, "bt": 10_499}, 10, 10_600)
        self.assertEqual([s["signature"] for s in sigs], [f"s{i}" for i in reversed(range(500, 3000))])
        self.assertEqual((cur["sig"], cur["slot"], cur["listed_at"], notes), ("s2999", 3999, 10_600, []))
        self.assertTrue(all(u == "s499" for _, u in h.calls))

    def test_a_rejected_cursor_is_found_by_paging_back(self):
        h = History(3000)
        h.signatures = self._reject_until(h.signatures)
        sigs, cur, notes = F.list_since(h, "A", {"sig": "s499", "slot": 1499, "bt": 10_499,
                                                 "listed_at": 10_550}, 10, 10_600)
        self.assertEqual([s["signature"] for s in sigs], [f"s{i}" for i in reversed(range(500, 3000))])
        self.assertEqual([k for k, _ in notes], ["cursor_expired"])  # no gap
        self.assertEqual(cur["sig"], "s2999")

    @staticmethod
    def _reject_until(fn):
        def f(address, before=None, until=None, limit=1000, endpoint="primary"):
            if until:
                raise F.RpcError("getSignaturesForAddress", "x", {"code": -32020, "message": "not found"})
            return fn(address, before=before, limit=limit)
        return f

    def test_a_cursor_past_retention_skips_until(self):
        h = History(3000, kept=10_000 + 2500)  # the cursor and 2,000 newer signatures are gone
        cursor = {"sig": "s499", "slot": 1499, "bt": 10_499, "listed_at": self.NOW - 3600}
        sigs, cur, notes = F.list_since(h, "A", cursor, 10, self.NOW)
        self.assertEqual(len(sigs), 500)
        self.assertTrue(all(u is None for _, u in h.calls))  # no -32020 round trip
        self.assertEqual(notes, [])  # listed an hour ago: the market was quiet since, nothing lost
        cursor["listed_at"] = self.NOW - 30 * 3600  # an outage: the gone history is a gap
        _, _, notes = F.list_since(h, "A", cursor, 10, self.NOW)
        self.assertEqual([k for k, _ in notes], ["listing_gap"])

    def test_a_quiet_market_keeps_listing(self):
        h = History(500, kept=10_000 + 600)  # everything gone, as for 92NvJRn… on 2026-10-02
        sigs, cur, notes = F.list_since(h, "A", "s499", 10, self.NOW)  # a bare cursor: until fails
        self.assertEqual((sigs, [k for k, _ in notes]), ([], ["cursor_expired", "listing_gap"]))
        self.assertEqual((cur["sig"], cur["listed_at"]), ("s499", self.NOW))
        n = len(h.calls)
        sigs, _, notes = F.list_since(h, "A", cur, 10, self.NOW + 1800)
        self.assertEqual((sigs, notes, h.calls[n:]), ([], [], [(None, None)]))  # one call, the gap once

    def test_a_bare_cursor_learns_its_slot(self):
        h = History(800)
        h.signatures = self._reject_until(h.signatures)
        sigs, cur, notes = F.list_since(h, "A", "s700", 10, 11_000)
        self.assertEqual([s["signature"] for s in sigs], [f"s{i}" for i in reversed(range(701, 800))])
        self.assertEqual(cur["sig"], "s799")
        sigs, cur, _ = F.list_since(h, "A", {"sig": "s799"}, 10, 11_000)
        self.assertEqual((sigs, cur["slot"], cur["bt"]), ([], 1799, 10_799))

    def test_more_pages_than_allowed_is_a_gap(self):
        h = History(3500)
        sigs, cur, notes = F.list_since(h, "A", {"sig": "s100", "slot": 1100, "bt": 10_100}, 1, 10_600)
        self.assertEqual((len(sigs), cur["sig"], [k for k, _ in notes]), (1000, "s3499", ["listing_gap"]))
        sigs, cur, notes = F.list_since(h, "A", None, 1, 10_600)  # first listing: one page, no gap
        self.assertEqual((len(sigs), notes), (1000, []))


class Coverage(unittest.TestCase):
    def test_market_days_per_tier(self):
        ms = [{"mid": 0, "tier": "SOL", "venue": "raydium_v4", "added": 0},
              {"mid": 1, "tier": "D", "venue": "pump_curve", "added": 0, "ended": 43_200},
              {"mid": 2, "tier": "C", "venue": "pump_swap", "added": 0}]
        cov = F.coverage(ms, 86_400)
        self.assertEqual(dict(cov), {("D", "pump_curve"): 0.5, ("C", "pump_swap"): 1.0})


if __name__ == "__main__":
    unittest.main()
