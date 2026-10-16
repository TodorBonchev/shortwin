"""Offline tests of ``universe.py``: addresses, deterministic sampling, the discovery parsers on
recorded transactions, and the census's eligibility and selection rules on synthetic RPC answers.

    python3 -m unittest discover scripts/gapcal/tests
"""

import base64
import gzip
import json
import os
import struct
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import decode as D  # noqa: E402
import universe as U  # noqa: E402
from rpc import b58encode  # noqa: E402


def fixture(name):
    with gzip.open(os.path.join(HERE, "fixtures", name), "rt") as f:
        return json.load(f)


class Addresses(unittest.TestCase):
    def test_canonical_pool_and_curve(self):
        pamm = fixture("pamm.json.gz")
        self.assertEqual(U.canonical_pool_of(pamm["mint"]), pamm["pool"])
        pump = fixture("pump.json.gz")
        self.assertEqual(U.curve_of(pump["mint"]), pump["curve"])

    def test_sampling_is_deterministic(self):
        keys = [f"sig{i}" for i in range(20_000)]
        hits = [k for k in keys if U.sample_hit(k, 0.01)]
        self.assertEqual(hits, [k for k in keys if U.sample_hit(k, 0.01)])
        self.assertTrue(150 <= len(hits) <= 250, len(hits))
        self.assertFalse(any(U.sample_hit(k, 0.0) for k in keys[:1000]))
        self.assertTrue(all(U.sample_hit(k, 1.0) for k in keys[:1000]))


class Discovery(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fx = fixture("discovery.json.gz")

    def test_create(self):
        got = U.parse_create(self.fx["create"])
        self.assertEqual(len(got), 1)
        mint, curve = got[0]
        self.assertEqual(U.curve_of(mint), curve)

    def test_migration(self):
        got = U.parse_migration(self.fx["migration"])
        self.assertEqual(len(got), 1)
        pool, mint, quote = got[0]
        self.assertEqual(quote, D.WSOL)
        self.assertEqual(U.canonical_pool_of(mint), pool)

    def test_failed_or_unrelated(self):
        tx = json.loads(json.dumps(self.fx["create"]))
        tx["meta"]["err"] = {"InstructionError": [0, "Custom"]}
        self.assertEqual(U.parse_create(tx), [])
        self.assertEqual(U.parse_migration(self.fx["create"]), [])
        self.assertEqual(U.parse_create(self.fx["migration"]), [])


def key(i):
    return b58encode(bytes([i % 251 + 1]) * 31 + bytes([i // 251]))


def token_account(amount):
    return base64.b64encode(struct.pack("<Q", amount)).decode()


class FakeRpc:
    """Serves ``program_accounts`` / ``multiple`` from dicts; counts calls."""

    def __init__(self, program_accounts, accounts):
        self.pa, self.acc, self.calls = program_accounts, accounts, 0

    def program_accounts(self, program, filters, data_slice=None, timeout=0):
        self.calls += 1
        mem = tuple(f["memcmp"]["offset"] for f in filters if "memcmp" in f)
        return self.pa[(program, mem)]

    def multiple(self, keys, data_slice=None):
        self.calls += 1
        out = []
        for k in keys:
            v = self.acc.get(k)
            if v is None:
                out.append(None)
                continue
            owner, data = v
            if data_slice:
                data = data[data_slice[0]:data_slice[0] + data_slice[1]]
            out.append({"owner": owner, "data": [base64.b64encode(data).decode(), "base64"]})
        return 1, out

    def signatures(self, *a, **kw):
        self.calls += 1
        return [{"blockTime": 1_000, "signature": "s"}]


def mint_bytes(freeze=False):
    d = bytearray(82)
    d[45] = 1
    if freeze:
        d[46] = 1
    return bytes(d)


def lp_mint(supply):
    d = bytearray(mint_bytes())
    d[36:44] = struct.pack("<Q", supply)
    return bytes(d)


def cpmm_state(sol_first, sol_vault, tok_vault, lp, lp_counter, mint, status=0, open_time=1):
    d = bytearray(637)
    d[0:8] = D.CPMM_POOL_DISC
    v0, v1 = (sol_vault, tok_vault) if sol_first else (tok_vault, sol_vault)
    m0, m1 = (D.WSOL, mint) if sol_first else (mint, D.WSOL)
    for off, k in ((72, v0), (104, v1), (136, lp), (168, m0), (200, m1)):
        d[off:off + 32] = U.b58decode(k)
    d[329] = status
    d[333:341] = struct.pack("<Q", lp_counter)
    d[373:381] = struct.pack("<Q", open_time)
    return bytes(d)


class RaydiumCensus(unittest.TestCase):
    SOL = 10**9

    def run_census(self, pools):
        """``pools``: (vault SOL, λ in bps, status, mint freeze)."""
        vaults, by_side, acc = [], {0: [], 1: []}, {}
        for i, (sol, lam, status, freeze) in enumerate(pools):
            pool, sv, tv, lp, mint = key(5 * i), key(5 * i + 1), key(5 * i + 2), key(5 * i + 3), key(5 * i + 4)
            vaults.append({"pubkey": sv, "account": {"data": [token_account(sol), "base64"]}})
            first = i % 2 == 0
            by_side[0 if first else 1].append({"pubkey": pool, "account": {"data": [
                base64.b64encode(U.b58decode(sv)).decode(), "base64"]}})
            acc[pool] = (D.CPMM, cpmm_state(first, sv, tv, lp, 10_000, mint, status))
            acc[lp] = (D.TOKEN, lp_mint(10_000 - lam))
            acc[mint] = (D.TOKEN, mint_bytes(freeze))
        rpc = FakeRpc({(D.TOKEN, (0, 32)): vaults, (D.CPMM, (168,)): by_side[0],
                       (D.CPMM, (200,)): by_side[1]}, acc)
        return U.census_raydium(rpc, rpc, "raydium_cpmm", log=lambda m: None), rpc

    def test_locked_depth_floor(self):
        got, _ = self.run_census([
            (100 * self.SOL, 10_000, 0, False),  # fully locked 100 SOL: in
            (24 * self.SOL, 10_000, 0, False),  # locked 24 SOL / λ_min = 48 ≥ 25: in
            (100 * self.SOL, 1_200, 0, False),  # locked 12 SOL / λ_min = 24 < 25: out
            (100 * self.SOL, 1_300, 0, False),  # locked 13 SOL: in, R_eff = 26 SOL
            (12 * self.SOL, 10_000, 0, False),  # vault below 12.5 SOL: never fetched
            (500 * self.SOL, 10_000, 4, False),  # swap disabled: out
            (500 * self.SOL, 10_000, 0, True),  # freeze authority: out
        ])
        self.assertEqual(len(got), 3)
        effs = sorted(p["r_eff"] for p in got)
        self.assertEqual(effs, [24 * self.SOL, 26 * self.SOL, 100 * self.SOL])
        for p in got:
            self.assertEqual(p["venue"], "raydium_cpmm")
            self.assertIsNone(p["mint_reject"])


class Selection(unittest.TestCase):
    def test_caps_seeded_and_panel_kept(self):
        pools = [{"pool": f"p{i}", "tier": "ABC"[i % 3], "r_eff": 30 * 10**9} for i in range(300)]
        pools.append({"pool": "thin", "tier": "C", "r_eff": 10**9})
        rec = {"pools": pools}
        caps = {"A": 5, "B": 7, "C": 9}
        a = U.select(rec, {}, seed=1, caps=caps)
        self.assertEqual(a, U.select(rec, {}, seed=1, caps=caps))
        tiers = [p["tier"] for p, _ in a]
        self.assertEqual((tiers.count("A"), tiers.count("B"), tiers.count("C")), (5, 7, 9))
        self.assertNotIn("thin", [p["pool"] for p, _ in a])
        current = {p["pool"]: p["tier"] for p, _ in a}
        b = U.select(rec, current, seed=2, caps=caps)
        self.assertEqual({p["pool"] for p, _ in b}, set(current))
        self.assertTrue(all(r == "census-kept" for _, r in b))


class CrawlPid(unittest.TestCase):
    def test_census_sees_a_live_crawl(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertIsNone(U.crawl_pid(d))
            with open(os.path.join(d, "crawl.pid"), "w") as f:
                f.write(str(os.getpid()))
            self.assertEqual(U.crawl_pid(d), os.getpid())
            with open(os.path.join(d, "crawl.pid"), "w") as f:
                f.write("999999999")  # beyond pid_max: never alive
            self.assertIsNone(U.crawl_pid(d))


if __name__ == "__main__":
    unittest.main()
