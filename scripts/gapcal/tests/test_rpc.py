"""Offline tests of ``rpc.py``: read-only, rate limit, backoff, endpoint routing and the
transaction cache (a fake transport; nothing reaches the network).

    python3 -m unittest discover scripts/gapcal/tests
"""

import json
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import rpc as R  # noqa: E402


class Clock:
    def __init__(self):
        self.t = 1000.0
        self.slept = []

    def now(self):
        return self.t

    def sleep(self, dt):
        self.slept.append(dt)
        self.t += dt


class Transport:
    """Answers from a script of ``(status, payload, headers)``; records every request."""

    def __init__(self, clock, script):
        self.clock, self.script, self.requests = clock, list(script), []

    def __call__(self, url, body, timeout):
        req = json.loads(body)
        self.requests.append((self.clock.t, url, req["method"], req["params"]))
        status, payload, headers = self.script.pop(0)
        if isinstance(payload, Exception):
            raise payload
        if callable(payload):
            payload = payload(req)
        return status, headers or {}, json.dumps(payload).encode()


def ok(result):
    return 200, lambda req: {"jsonrpc": "2.0", "id": req["id"], "result": result}, None


def make(script, **kw):
    clock = Clock()
    tr = Transport(clock, script)
    return R.Rpc("https://primary", "https://archive", transport=tr, sleep=clock.sleep,
                 clock=clock.now, **kw), tr, clock


class ReadOnly(unittest.TestCase):
    def test_only_read_methods(self):
        rpc, tr, _ = make([])
        for m in ("sendTransaction", "requestAirdrop", "simulateTransaction"):
            with self.assertRaises(ValueError):
                rpc.call(m, [])
        self.assertEqual(tr.requests, [])

    def test_rps_cap(self):
        with self.assertRaises(ValueError):
            R.RateLimiter(3.5)
        with self.assertRaises(ValueError):
            R.RateLimiter(0)


class Rate(unittest.TestCase):
    def test_requests_are_spaced(self):
        rpc, tr, _ = make([ok(1)] * 10, rps=2.5)
        for _ in range(10):
            rpc.call("getSlot", [])
        times = [t for t, *_ in tr.requests]
        gaps = [b - a for a, b in zip(times, times[1:])]
        self.assertTrue(all(g >= 0.4 - 1e-9 for g in gaps), gaps)

    def test_retries_count_against_the_limit(self):
        rpc, tr, clock = make([(429, {}, None), (503, {}, None), ok(7)], rps=3.0)
        self.assertEqual(rpc.call("getSlot", []), 7)
        times = [t for t, *_ in tr.requests]
        self.assertTrue(all(b - a >= 1 / 3 - 1e-9 for a, b in zip(times, times[1:])))
        self.assertEqual(rpc.stats["retries"], 2)

    def test_backoff_is_exponential_and_honours_retry_after(self):
        rpc, tr, clock = make([(429, {}, {"Retry-After": "17"}), (500, {}, None), (502, {}, None),
                               ok(1)], rps=3.0)
        rpc.call("getSlot", [])
        backoffs = [s for s in clock.slept if s > 0.5]
        self.assertEqual(backoffs[0], 17.0)
        self.assertTrue(3.0 <= backoffs[1] <= 5.0 and 6.0 <= backoffs[2] <= 10.0, backoffs)

    def test_rpc_rate_errors_retry_others_raise(self):
        err = lambda code: (200, lambda req: {"jsonrpc": "2.0", "id": req["id"],  # noqa: E731
                                              "error": {"code": code, "message": "x"}}, None)
        rpc, _, _ = make([err(-32005), ok(3)])
        self.assertEqual(rpc.call("getSlot", []), 3)
        rpc, _, _ = make([err(-32602)])
        with self.assertRaises(R.RpcError):
            rpc.call("getSlot", [])
        rpc, _, _ = make([(403, {"error": {"code": -32602, "message": "Request blocked"}}, None)])
        with self.assertRaises(R.RpcError):
            rpc.call("getSlot", [])

    def test_network_errors_retry_then_give_up(self):
        rpc, tr, _ = make([(0, OSError("reset"), None)] * 3, max_attempts=3)
        with self.assertRaises(R.RpcError):
            rpc.call("getSlot", [])
        self.assertEqual(len(tr.requests), 3)
        self.assertEqual(rpc.stats["errors"], 1)


class Routing(unittest.TestCase):
    def test_large_batches_and_program_accounts_go_to_the_archive(self):
        val = {"context": {"slot": 5}, "value": [None] * 11}
        rpc, tr, _ = make([ok({"context": {"slot": 5}, "value": [None] * 10}), ok(val), ok([])])
        rpc.multiple([f"k{i}" for i in range(10)])
        rpc.multiple([f"k{i}" for i in range(11)])
        rpc.program_accounts("P", [])
        self.assertEqual([u for _, u, *_ in tr.requests],
                         ["https://primary", "https://archive", "https://archive"])

    def test_transactions_are_cached_and_backfilled(self):
        with tempfile.TemporaryDirectory() as d:
            tx = {"slot": 1, "blockTime": 2, "meta": {}, "transaction": {}}
            rpc, tr, _ = make([ok(None), ok(tx)], cache_dir=d)
            self.assertEqual(rpc.transaction("SigA"), tx)
            self.assertEqual([(u, m) for _, u, m, _ in tr.requests],
                             [("https://primary", "getTransaction"), ("https://archive", "getTransaction")])
            # A second read, and a fresh client on the same cache, cost nothing.
            self.assertEqual(rpc.transaction("SigA"), tx)
            rpc2, tr2, _ = make([], cache_dir=d)
            self.assertEqual(rpc2.transaction("SigA"), tx)
            self.assertEqual(tr2.requests, [])
            self.assertEqual(rpc2.stats["calls"], 0)

    def test_missing_transactions_are_not_cached(self):
        with tempfile.TemporaryDirectory() as d:
            rpc, tr, _ = make([ok(None), ok(None)], cache_dir=d)
            self.assertIsNone(rpc.transaction("SigB"))
            self.assertEqual(os.listdir(d), [])


class Base58(unittest.TestCase):
    def test_round_trip(self):
        for k in (R.b58encode(bytes(32)), "So11111111111111111111111111111111111111112",
                  "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"):
            self.assertEqual(R.b58encode(R.b58decode(k)), k)
        self.assertEqual(R.anchor_event_disc("SwapEvent").hex(), "40c6cde8260871e2")


if __name__ == "__main__":
    unittest.main()
