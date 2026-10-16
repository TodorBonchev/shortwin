"""Offline tests of ``decode.py`` on recorded mainnet transactions (WP-9 exit evidence; GC-7):
the decoded post-trade reserves equal the pool's vault balances after the transaction — PumpSwap
exactly, CPMM minus its fee counters, AMM v4 minus ``need_take_pnl`` — and consecutive trades
chain (each trade's reserves before are the previous trade's after).

    python3 -m unittest discover scripts/gapcal/tests

Fixtures: ``fixtures/*.json.gz`` (``record_fixtures.py``).
"""

import gzip
import json
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import decode as D  # noqa: E402


def fixture(name):
    with gzip.open(os.path.join(HERE, "fixtures", name), "rt") as f:
        return json.load(f)


def trades(fx, keep, **kw):
    """``[(tx, [trades on the fixture's pool])]`` in chain order."""
    out = []
    for tx in fx["txs"]:
        tr, info = D.decode_trades(tx, **kw)
        assert not info["truncated"]
        out.append((tx, [t for t in tr if keep(t)]))
    return out


class PumpSwap(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fx = fixture("pamm.json.gz")

    def test_post_reserves_equal_the_vaults_exactly(self):
        pool = self.fx["pool"]
        got = trades(self.fx, lambda t: t["venue"] == "pump_swap" and t["pool"] == pool)
        self.assertGreaterEqual(len(got), 20)
        for tx, tr in got:
            self.assertTrue(tr, tx["transaction"]["signatures"][0])
            post = D.token_balances(tx)
            last = tr[-1]
            self.assertEqual(last["sol_after"], post[self.fx["quote_vault"]][1])
            self.assertEqual(last["tok_after"], post[self.fx["base_vault"]][1])
            self.assertEqual(last["mint"], self.fx["mint"])
            # Within a transaction, consecutive trades on the pool chain.
            for a, b in zip(tr, tr[1:]):
                self.assertEqual((a["sol_after"], a["tok_after"]), (b["sol_before"], b["tok_before"]))

    def test_pre_reserves_equal_the_vaults_before(self):
        pool = self.fx["pool"]
        for tx, tr in trades(self.fx, lambda t: t["venue"] == "pump_swap" and t["pool"] == pool):
            pre = D.token_balances(tx, "pre")
            self.assertEqual(tr[0]["sol_before"], pre[self.fx["quote_vault"]][1])
            self.assertEqual(tr[0]["tok_before"], pre[self.fx["base_vault"]][1])

    def test_trader_view(self):
        pool = self.fx["pool"]
        for _, tr in trades(self.fx, lambda t: t["venue"] == "pump_swap" and t["pool"] == pool):
            for t in tr:
                if t["side"] == "buy":  # tokens out of the pool, SOL in
                    self.assertLess(t["tok_after"], t["tok_before"])
                    self.assertGreater(t["sol_after"], t["sol_before"])
                else:
                    self.assertGreater(t["tok_after"], t["tok_before"])
                    self.assertLess(t["sol_after"], t["sol_before"])
                self.assertGreater(t["token_amount"], 0)


class Cpmm(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fx = fixture("cpmm.json.gz")

    def got(self, exact=True):
        pool = self.fx["pool"]
        kw = {"cpmm_rates": {pool: tuple(self.fx["rates"])}} if exact else {}
        return trades(self.fx, lambda t: t["venue"] == "raydium_cpmm" and t["pool"] == pool, **kw)

    def test_consecutive_swaps_chain_exactly(self):
        flat = [t for _, tr in self.got() for t in tr]
        self.assertEqual(len(flat), 20)
        for a, b in zip(flat, flat[1:]):
            self.assertEqual((a["sol_after"], a["tok_after"]), (b["sol_before"], b["tok_before"]))
            self.assertTrue(a["exact"])

    def test_vaults_minus_counters(self):
        """``vault − reserves`` is the fee counters: never negative, never decreasing, and growing
        by exactly the protocol and fund parts of each trade fee (plus the creator fee)."""
        prev = None
        rates = self.fx["rates"]
        for tx, tr in self.got():
            post = D.token_balances(tx)
            last = tr[-1]
            ctr = (post[self.fx["sol_vault"]][1] - last["sol_after"],
                   post[self.fx["tok_vault"]][1] - last["tok_after"])
            self.assertGreaterEqual(min(ctr), 0)
            if prev is not None:
                grow = [0, 0]
                for t in tr:
                    cut = t["trade_fee"] * rates[0] // 10**6 + t["trade_fee"] * rates[1] // 10**6
                    on_sol = (t["side"] == "buy") == t["creator_fee_on_input"]
                    grow[0 if t["side"] == "buy" else 1] += cut
                    grow[0 if on_sol else 1] += t["creator_fee"]
                self.assertEqual((ctr[0] - prev[0], ctr[1] - prev[1]), tuple(grow))
            prev = ctr

    def test_m8_reserves_overstate_by_the_counter_share(self):
        """Without the config rates (M8's formula) the input side keeps the protocol and fund
        fees, so the chain breaks by exactly that amount."""
        flat = [t for _, tr in self.got(exact=False) for t in tr]
        rates = self.fx["rates"]
        for a, b in zip(flat, flat[1:]):
            cut = a["trade_fee"] * rates[0] // 10**6 + a["trade_fee"] * rates[1] // 10**6
            sol_cut = cut + (a["creator_fee"] if a["creator_fee_on_input"] and a["side"] == "buy" else 0)
            tok_cut = (cut if a["side"] == "sell" else 0) + (
                a["creator_fee"] if (a["side"] == "sell") == a["creator_fee_on_input"] else 0)
            if a["side"] == "buy":
                self.assertEqual(a["sol_after"] - b["sol_before"], sol_cut)
            else:
                self.assertEqual(a["tok_after"] - b["tok_before"], tok_cut)


class AmmV4(unittest.TestCase):
    """Two pools: 20 consecutive swaps of a pump graduate's pool (``need_take_pnl`` 0) and the
    swaps among 60 consecutive transactions of SOL/USDC (``need_take_pnl`` non-zero)."""

    @classmethod
    def setUpClass(cls):
        cls.fx = fixture("ammv4.json.gz")

    def got(self, p):
        pool = p["pool"]
        return trades(p, lambda t: t["venue"] == "raydium_v4" and t["pool"] == pool,
                      v4_orientation={pool: p["sol_is_coin"]})

    def test_consecutive_swaps_chain_exactly(self):
        counts = []
        for p in self.fx["pools"]:
            flat = [t for _, tr in self.got(p) for t in tr]
            counts.append(len(flat))
            for a, b in zip(flat, flat[1:]):
                self.assertEqual((a["sol_after"], a["tok_after"]), (b["sol_before"], b["tok_before"]))
        self.assertEqual(counts[0], 20)
        self.assertGreaterEqual(counts[1], 3)

    def test_vaults_minus_need_take_pnl(self):
        pnls = []
        for p in self.fx["pools"]:
            pnl = (p["need_take_pnl_coin"], p["need_take_pnl_pc"])
            if not p["sol_is_coin"]:
                pnl = pnl[::-1]
            pnls.append(pnl)
            for tx, tr in self.got(p):
                self.assertTrue(tr)
                post = D.token_balances(tx)
                last = tr[-1]
                self.assertEqual(post[p["sol_vault"]][1] - last["sol_after"], pnl[0])
                self.assertEqual(post[p["tok_vault"]][1] - last["tok_after"], pnl[1])
        self.assertGreater(max(pnls[1]), 0)  # the check is not vacuous on SOL/USDC


class PumpCurve(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fx = fixture("pump.json.gz")

    def got(self):
        mint = self.fx["mint"]
        return trades(self.fx, lambda t: t["venue"] == "pump_curve" and t["mint"] == mint)

    def test_virtual_reserves_chain(self):
        flat = [t for _, tr in self.got() for t in tr]
        self.assertEqual(len(flat), 20)
        for a, b in zip(flat, flat[1:]):
            s = 1 if b["side"] == "buy" else -1
            self.assertEqual(b["sol_after"] - a["sol_after"], s * b["curve_sol_delta"])
            self.assertEqual(b["tok_after"] - a["tok_after"], -s * b["token_amount"])

    def test_real_reserves_match_the_curve_accounts(self):
        """Virtual − real is the curve's initial virtual offset (30 SOL; 279.9 M tokens); the
        curve's token account holds the real tokens plus the 206.9 M migration reserve; the
        curve's lamports move by exactly the trades' SOL (its rent and any stray lamports aside:
        the residue is not a constant over the fixture, a transfer between two trades moved it)."""
        curve = self.fx["curve"]
        for tx, tr in self.got():
            for t in tr:
                self.assertEqual(t["sol_after"] - t["real_sol_after"], 30_000_000_000)
                self.assertEqual(t["tok_after"] - t["real_tok_after"], 279_900_000_000_000)
            last = tr[-1]
            ata = [int(e["uiTokenAmount"]["amount"]) for e in tx["meta"]["postTokenBalances"]
                   if e["mint"] == self.fx["mint"] and e.get("owner") == curve]
            self.assertEqual(ata, [last["real_tok_after"] + 206_900_000_000_000])
            pre, post = D.lamport_balances(tx, "pre")[curve], D.lamport_balances(tx)[curve]
            moved = sum((1 if t["side"] == "buy" else -1) * t["curve_sol_delta"] for t in tr)
            if pre == 0:  # the create: the curve's rent arrives with the first buy
                self.assertEqual(post - last["real_sol_after"], post - moved)
            else:
                self.assertEqual(post - pre, moved)


class Parsers(unittest.TestCase):
    def test_log_attribution_survives_truncation(self):
        tx = fixture("cpmm.json.gz")["txs"][0]
        tx = json.loads(json.dumps(tx))
        tx["meta"]["logMessages"] = tx["meta"]["logMessages"][:2] + ["Log truncated"]
        tr, info = D.decode_trades(tx)
        self.assertTrue(info["truncated"])
        self.assertEqual([t for t in tr if t["venue"] == "raydium_cpmm"], [])

    def test_failed_transactions_have_no_trades(self):
        tx = json.loads(json.dumps(fixture("pamm.json.gz")["txs"][0]))
        tx["meta"]["err"] = {"InstructionError": [0, "Custom"]}
        self.assertEqual(D.decode_trades(tx)[0], [])

    def test_ammv4_needs_an_orientation(self):
        tx = fixture("ammv4.json.gz")["txs"][0]
        self.assertEqual([t for t in D.decode_trades(tx)[0] if t["venue"] == "raydium_v4"], [])

    def test_event_bodies_reject_short_input(self):
        self.assertIsNone(D.pump_trade(b"\0" * 100))
        self.assertIsNone(D.pamm_trade(True, b"\0" * 100, "x", D.WSOL))
        self.assertIsNone(D.pamm_trade(True, b"\0" * 400, "x", "y"))  # no SOL side
        self.assertIsNone(D.cpmm_swap(D.D_SWAP + b"\0" * 10))
        self.assertIsNone(D.ray_log(b"\x05" + b"\0" * 56))


class MintFilter(unittest.TestCase):
    """The byte-level filter's cases (``shortwin_venue_state::mint`` tests)."""

    @staticmethod
    def base(freeze=False):
        d = bytearray(82)
        d[0] = 1
        d[4:36] = bytes([9]) * 32
        d[44], d[45] = 6, 1
        if freeze:
            d[46] = 1
            d[50:82] = bytes([8]) * 32
        return bytes(d)

    def extended(self, entries):
        d = bytearray(512)
        d[:82] = self.base()
        d[165] = 1
        at = 166
        for t, n in entries:
            d[at:at + 4] = t.to_bytes(2, "little") + n.to_bytes(2, "little")
            at += 4 + n
        return bytes(d)

    def test_cases(self):
        key = "Mint1111111111111111111111111111111111111111"
        self.assertIsNone(D.mint_check(key, D.TOKEN, self.base()))
        self.assertEqual(D.mint_check(key, D.TOKEN, self.base(True)), "FreezeAuthority")
        self.assertEqual(D.mint_check(key, D.SYSTEM, self.base()), "UnsupportedProgram")
        self.assertEqual(D.mint_check(D.WSOL, D.TOKEN, self.base()), "Native")
        self.assertEqual(D.mint_check(key, D.TOKEN, self.base() + b"\0"), "Malformed")
        self.assertIsNone(D.mint_check(key, D.TOKEN_2022, self.extended([(18, 64), (19, 100)])))
        self.assertEqual(D.mint_check(key, D.TOKEN_2022, self.extended([(18, 64), (1, 108)])),
                         "DisallowedExtension(1)")
        self.assertEqual(D.mint_check(key, D.TOKEN_2022, self.extended([(18, 400)])),
                         "DisallowedExtension(None)")
        self.assertEqual(D.mint_check(key, D.TOKEN_2022, bytes(355)), "Malformed")


class Tiers(unittest.TestCase):
    def test_econ_6_2(self):
        usd = 125.0  # θ_B = 800 SOL of Λ, θ_A = 4,000 SOL
        sol = 10**9
        self.assertEqual(D.tier("pump_curve", 0, 0, usd), "D")
        self.assertEqual(D.tier("pump_swap", 10_000 * sol, 23 * 3600, usd), "C")  # young
        self.assertEqual(D.tier("pump_swap", 399 * sol, 30 * 86400, usd), "C")  # Λ < θ_B
        self.assertEqual(D.tier("raydium_v4", 400 * sol, 30 * 86400, usd), "B")
        self.assertEqual(D.tier("raydium_v4", 2_000 * sol, 6 * 86400, usd), "B")  # A size, < 7 d
        self.assertEqual(D.tier("raydium_cpmm", 2_000 * sol, 7 * 86400, usd), "A")


if __name__ == "__main__":
    unittest.main()
