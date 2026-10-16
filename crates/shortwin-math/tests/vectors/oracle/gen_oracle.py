#!/usr/bin/env python3
"""Unit vectors for shortwin_math::{observe, persist, bounds} (ARCHITECTURE §2.2, §3.6,
§3.7, §4.4; ECONOMICS §2.1, §5.1-§5.5, §8.2) from sim/shortwin_sim.py v0.4.1 with its
default switches.

Every expected value is computed by the sim's own methods on synthetic states that
exercise the corners (bucket and clamp combinations, dt caps and dt <= 0, 128-bit
extremes, adversarial ring shapes, stale buckets, the reference caps):

* units_observe.json, units_ring.json
    integrate  Market._integrate: EMA step, slow EMA, the ring entry with its cached
               (x, y) = reprice(l, p) and the bucket extremes, the last bucket, the
               pre-roll gap.
    rewrite    Market._switch_to_pool (graduation) and Market.fast_switch (Raydium) on
               synthetic states: L_ema / L_slow, every ring entry's l, llo, lhi and (x, y),
               the cleared bucket.
    growth     (units_ring.json) Market.ema_growth_bps and Market.ring_medians on arbitrary
               rings.
* units_persist.json
    persist    the ring loop of Market.status (the liquidation EMA leg on the newest
               liq_persist_n entries' cached (x, y)) with a coin-flip predicate per entry.
* units_bounds.json
    reference  Market.exit_reference (liquidation, ref_cap_lo = True; expiry): P_ref and
               L_b, captured from its quote call.
    max_in     the max_in lines of Market._buyback_exit (liquidation: liq_reserve =
               "none"; expiry: exp_reserve = "rent+fee"), transcribed; the settle track's
               traces assert the transcription against every exit the sim executes.
* units_ref.json
    ref_liquidity  Market.exit_reference's L_b alone (M4F-1): the guard on or off and, on a
               Raydium venue, Market.ref_depth_cap = D (floor(D*1e4/lambda) at lambda = 100 %
               since AIR-14) binding or not (none on another venue); drawn after units_bounds,
               so the other files keep their random stream.
    spot_liquidity Market.spot_liquidity (FITS-2 (a); AIR-17): max(L_live, L_floor), L_floor
               = min(L_ema, L_mhi) while the guard is set, else L_ema; its own random stream
               (SEED + 2), so ref_liquidity keeps its cases.

Ranges. (x, y) is compared exactly, so every reprojection in these vectors lies where the
u128 forms of ECON §2.1 are exact for any correct implementation: L*10^9 <= u128::MAX and
L*isqrt_ceil(P) <= u128::MAX (checked; ECON §2.2's bounds are far inside).

Encoding: single-line strings of whitespace-separated decimal tokens ("-" for None, "1"/"0"
for booleans); tests/oracle_vectors.rs documents each layout next to its parser.

  state  P_ema L_ema L_slow ema_ts slow_ts count | b_sec b_lo b_hi b_llo b_lhi | lb_sec lb_lo lb_hi
         | n | n x (ts p l x y lo hi llo lhi)          (ring oldest first)

Run from anywhere: python3 crates/shortwin-math/tests/vectors/oracle/gen_oracle.py
"""
import json
import math
import random
import sys
from dataclasses import replace
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "docs" / "design" / "sim"))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)

HERE = Path(__file__).resolve().parent
SEED = 20260927
U64 = 2**64 - 1
U128 = 2**128 - 1
BPS = sim.BPS
WAD = sim.WAD
M = sim.Market
assert sim.DEFAULT.ring_len == 8 and not sim.DEFAULT.box_obs and sim.DEFAULT.ref_cap_lo
assert sim.DEFAULT.liq_reserve == "none" and sim.DEFAULT.exp_reserve == "rent+fee"
assert all(t.ema_tau_s == 5 for t in sim.TIERS.values())


def tok(x):
    if x is None:
        return "-"
    if isinstance(x, bool):
        return "1" if x else "0"
    return str(x)


def line(*xs):
    return " ".join(tok(x) for x in xs)


def exact_reprice(L, p):
    """The region where reprice's u128 products cannot overflow (module docstring)."""
    return L * 10**9 <= U128 and L * sim.isqrt_ceil(p) <= U128


def entry(t, p, l, lo, hi, llo, lhi):
    """The sim's 11-field ring tuple (the two trailing comparison fields are not exported).
    Synthetic entries only carry state, so a reprojection beyond u128 is stored as (0, 0)."""
    e = M._entry(t, p, l, lo, hi, llo, lhi)
    if e[3] > U128 or e[4] > U128:
        e = e[:3] + (0, 0) + e[5:]
    return e


def ring_tokens(ring):
    out = [len(ring)]
    for e in ring:
        out += list(e[:9])
    return out


def market_state(m):
    b = [None] * 5 if m.b_sec is None else [m.b_sec, m.b_lo, m.b_hi, m.b_llo, m.b_lhi]
    lb = [None] * 3 if m.lb_sec is None else [m.lb_sec, m.lb_lo, m.lb_hi]
    return line(m.ema, m.liq, m.liq_slow, m.ema_t, m.slow_t, m.ema_count, *b, *lb, *ring_tokens(m.ring))


def bare(tier=None, P=sim.DEFAULT):
    m = object.__new__(M)
    m.P, m.tier = P, tier or sim.TIERS["A"]
    m.b_sec, m.lb_sec, m.lb_lo, m.lb_hi = None, None, 0, 0
    return m


def rnd_value(rng, top=128):
    pick = rng.random()
    if pick < 0.05:
        return rng.choice([1, 2, 3, U128, U128 - 1, 2**127, WAD])
    if pick < 0.15:
        return rng.randrange(1, 2**top)
    return rng.randrange(10**rng.randint(3, 20), 10**rng.randint(21, 38))


def rnd_liq(rng):
    """Liquidities, mostly realistic (ECON §2.2: L <= 3.3e18), sometimes extreme."""
    pick = rng.random()
    if pick < 0.1:
        return rng.choice([1, 2, 10**19, 2**64, 10**25, 10**29])
    return rng.randrange(10**6, 10**rng.randint(8, 20))


def rnd_ring(rng, n, t0=None, distinct=False):
    """Arbitrary ring tuples (entries need not come from integrations)."""
    t = rng.randrange(-1000, 10**6) if t0 is None else t0
    ring, used = [], set()
    for j in range(n):
        t += rng.choice([1, 1, 1, 2, 3, 0, 5]) if rng.random() < 0.9 else rng.choice([-1, 0, 100])
        if rng.random() < 0.5 and ring:
            p = min(U128, max(1, ring[-1][1] + ring[-1][1] * rng.randrange(-500, 3000) // 10_000))
        else:
            p = rnd_value(rng)
        L = rnd_liq(rng)
        lo, hi = sorted((rnd_value(rng), rnd_value(rng)))
        llo, lhi = sorted((rnd_liq(rng), rnd_liq(rng)))
        if rng.random() < 0.2:                       # a creation-style entry
            lo = hi = p
            llo = lhi = L
        e = entry(t, p, L, lo, hi, llo, lhi)
        if distinct:
            while (e[3], e[4]) in used and (e[3], e[4]) != (0, 0):
                L += 1
                e = entry(t, p, L, lo, hi, llo, lhi)
            used.add((e[3], e[4]))
        ring.append(e)
    return ring


# ---------------------------------------------------------------------------------------
# observe
# ---------------------------------------------------------------------------------------
def unit_integrate(rng, n):
    cases = []
    while len(cases) < n:
        tier = replace(sim.TIERS["A"], ema_dtmax_s=rng.choice([2, 2, 2, 1, 1, 3, 10]),
                       ema_clamp_bps=rng.choice([10_000, 10_000, 5_000, 7_777, 1_000]))
        P = replace(sim.DEFAULT, liq_slow_tau_s=rng.choice([3600, 600, 86_400, 17]),
                    liq_slow_dtmax_s=rng.choice([60, 1, 600, 7]))
        m = bare(tier, P)
        extreme = rng.random() < 0.3
        m.ema = rnd_value(rng) if extreme else rng.randrange(10**6, 10**30)
        m.liq = rnd_liq(rng)
        m.liq_slow = min(U128, max(1, m.liq + rng.randrange(-m.liq // 2, m.liq + 1)))
        t = rng.randrange(-10**6, 10**9)
        m.ema_t = t - rng.choice([1, 1, 2, 3, 5, 11, 100, 0, -3])
        m.slow_t = m.ema_t - rng.choice([0, 0, 0, 1, 30, 1000])
        m.ema_count = rng.randrange(0, 10**6)
        m.ring = rnd_ring(rng, rng.randrange(1, 9), t0=t - 100)
        m.b_sec = t - rng.choice([1, 1, 2, 7])

        def around(x, lim=U128):
            k = rng.random()
            if k < 0.3:
                return min(lim, max(1, x * rng.randrange(1, 40) // 10))
            if k < 0.45:
                return max(1, x * 10 // rng.randrange(11, 100))
            if k < 0.6:
                return x
            return rnd_value(rng) if lim == U128 else rnd_liq(rng)
        a, b = around(m.ema), around(m.ema)
        m.b_lo, m.b_hi = min(a, b), max(a, b)
        a, b = around(m.liq, 10**29), around(m.liq, 10**29)
        m.b_llo, m.b_lhi = min(a, b), max(a, b)
        if rng.random() < 0.3:                       # a lone observation
            m.b_hi, m.b_lhi = m.b_lo, m.b_llo
        if rng.random() < 0.3:                       # previous last bucket set
            m.lb_sec, m.lb_lo, m.lb_hi = m.b_sec - 1, m.b_lo, m.b_hi
        before = market_state(m)
        gap = t - m.ema_t
        stepped = m.ema_t < t                        # dt > 0: the ring gets a new entry
        M._integrate(m, t)
        if stepped and not exact_reprice(m.liq, m.ema):
            continue
        cases.append({"prm": line(tier.ema_dtmax_s, tier.ema_clamp_bps, P.liq_slow_tau_s, P.liq_slow_dtmax_s),
                      "t": str(t), "gap": str(gap), "before": before, "after": market_state(m)})
    return cases


def unit_growth(rng, n):
    cases = []
    for _ in range(n):
        ring = rnd_ring(rng, rng.randrange(1, 9))
        if rng.random() < 0.5:                       # a genuine trend: rising prices, 1-3 s apart
            t, p = rng.randrange(0, 10**6), rng.randrange(10**9, 10**30)
            ring = []
            for j in range(rng.randrange(1, 9)):
                L = rnd_liq(rng)
                ring.append(entry(t, p, L, p * 99 // 100, p, L, L))
                t += rng.choice([1, 1, 2, 3])
                p = min(U128, p + p * rng.randrange(0, 3000) // 10_000 + rng.randrange(0, 3))
        gn = rng.randrange(0, 9)
        m = bare(P=replace(sim.DEFAULT, kappa_growth_n=gn))
        m.ring = ring
        cases.append(line(gn, M.ema_growth_bps(m), *M.ring_medians(m), *ring_tokens(ring)))
    return cases


class FakePool:
    """What _switch_to_pool / fast_switch read from the new venue."""

    def __init__(self, L):
        self.L = L

    def sqrt_k(self):
        return self.L

    def start_boost(self, t):
        pass


def unit_rewrite(rng, n):
    cases = []
    while len(cases) < n:
        m = bare()
        t = rng.randrange(0, 10**6)
        m.ema = rng.randrange(10**6, 10**30) if rng.random() < 0.8 else rnd_value(rng)
        m.liq = rnd_liq(rng)
        m.liq_slow = rnd_liq(rng) if rng.random() < 0.5 else m.liq
        m.ema_t = m.slow_t = t
        m.ema_count = rng.randrange(0, 10**5)
        m.ring = rnd_ring(rng, rng.randrange(1, 9), t0=t - 20)
        if rng.random() < 0.6:
            m.b_sec, m.b_lo, m.b_hi, m.b_llo, m.b_lhi = t, 5, 9, 7, 8
        else:
            m.b_sec = None
        if rng.random() < 0.5:
            m.lb_sec, m.lb_lo, m.lb_hi = t - 1, 3, 4
        m.route_broken = False
        l_new = rnd_liq(rng) if rng.random() < 0.7 else rng.choice([1, m.liq, m.liq_slow, m.liq + 1])
        kind = rng.choice(["g", "r"])
        before = market_state(m)
        if kind == "g":
            M._switch_to_pool(m, FakePool(l_new), t)
        else:
            M.fast_switch(m, FakePool(l_new), t)
        if not all(exact_reprice(e[2], e[1]) for e in m.ring):
            continue
        cases.append(line(kind, l_new) + " | " + before + " | " + market_state(m))
    return cases


# ---------------------------------------------------------------------------------------
# persist
# ---------------------------------------------------------------------------------------
class FakeVenue:
    kind = "fake"

    def __init__(self, p, l):
        self.p, self.l = p, l

    def has_route(self):
        return True

    def has_price(self):
        return True

    def spot_wad(self):
        return self.p

    def sqrt_k(self):
        return self.l


def unit_persist(rng, n):
    """Market.status on synthetic rings: every cached (x, y) gets a coin-flip `below`."""
    cases = []
    V = L = 10_000
    for _ in range(n):
        ring = rnd_ring(rng, rng.randrange(1, 9), distinct=True)
        if rng.random() < 0.2:                       # an entry without reprojection
            e = ring[rng.randrange(len(ring))]
            ring[ring.index(e)] = e[:3] + (0, 0) + e[5:]
        bias = rng.choice([0.5, 0.8, 0.95])
        flags = {(e[3], e[4]): rng.random() < bias for e in ring}
        flags[(0, 0)] = True                         # quote_xy(X <= 0) is None: below
        pn = rng.randint(0, 5)
        m = bare(P=replace(sim.DEFAULT, liq_persist_n=pn))
        m.ring = ring
        m.ema, m.liq, m.liq_slow = ring[-1][1], ring[-1][2], ring[-1][2]
        m.venue = FakeVenue(10**18, 10**9)
        m.lb_sec, m.lb_lo = ring[-1][0], 10**18
        m.rent_due = lambda pos: 0
        m.quote_spot = lambda Q, risk=False: (0, 5_000)
        m.quote_repriced = lambda Q, p, l: (0, 5_000)
        m.quote_xy = lambda Q, X, Y, f=flags: None if X <= 0 else (0, 20_000 if f[(X, Y)] else 5_000)
        pos = sim.Position(Q=1, V=V, L=L, deadline=10**15, R_min=0)
        st = M.status(m, pos, ring[-1][0])
        fl = "".join("1" if flags[(e[3], e[4])] else "0" for e in reversed(ring))
        cases.append(line(pn, fl, st["ema_leg"], *ring_tokens(ring)))
    return cases


# ---------------------------------------------------------------------------------------
# bounds
# ---------------------------------------------------------------------------------------
def unit_reference(rng, n):
    """Market.exit_reference on synthetic states (the reference quote replaced by a capture)."""
    cases = []
    for _ in range(n):
        size = rng.randrange(1, 9)
        p = rnd_value(rng) if rng.random() < 0.2 else rng.randrange(10**9, 10**30)
        t = rng.randrange(0, 10**6)
        trend = rng.random() < 0.6
        ring = []
        for j in range(size):
            ring.append(entry(t, p, 10**9, p, p, 10**9, 10**9))
            t += rng.choice([1, 1, 2, 3])
            if trend:
                p = min(U128, p + p * rng.randrange(0, 1500) // 10_000)
            else:
                p = min(U128, max(1, p + p * rng.randrange(-900, 900) // 10_000))
        now = ring[-1][0] + rng.choice([0, 0, 1, 2, 3])
        ema = ring[-1][1] if rng.random() < 0.7 else (
            rnd_value(rng) if rng.random() < 0.5 else ring[-1][1] * rng.randrange(8_000, 13_000) // 10_000)
        ema = max(1, min(U128, ema))
        liq, slow, ll = rnd_liq(rng), rnd_liq(rng), rnd_liq(rng)
        P = replace(sim.DEFAULT, kappa_liq_bps=rng.choice([2500, 3000, 0, 5000, 100]),
                    kappa_base_bps=rng.choice([0, 0, 50, 1000, 6000]), kappa_growth_n=rng.randint(2, 7),
                    delta_liq_bps=rng.randint(200, 500), delta_exp_bps=rng.randint(50, 300),
                    ref_max_age_s=rng.randint(1, 5))
        m = bare(sim.TIERS["A"], P)
        m.ring, m.ema, m.liq, m.liq_slow = ring, ema, liq, slow
        m.venue = FakeVenue(10**18, ll)
        m.lb_sec = now - rng.choice([0, 1, 1, 2, 3, 6, -1])
        m.lb_lo = rnd_value(rng) if rng.random() < 0.3 else min(U128, max(1, ema * rng.randrange(5_000, 15_000) // 10_000))
        if rng.random() < 0.04:                      # every term of P_ref beyond u128
            m.ema, m.lb_lo = U128 - rng.randrange(0, 10), U128
            m.P = P = replace(P, kappa_liq_bps=2500, kappa_base_bps=50)
        cap = []
        m.quote_repriced = lambda Q, pp, L, c=cap: (c.append((pp, L)) or (0, 1))
        is_liq = rng.random() < 0.7
        lo_ok = now - m.lb_sec <= P.ref_max_age_s
        st = {"lo_ok": lo_ok, "L_live": ll}
        r = M.exit_reference(m, sim.Position(Q=1), st, now, is_liq)
        if r is None:
            res = ["none", None, None]
        else:
            pref, lb = cap[0]
            assert r[2] == pref and r[1] == (P.delta_liq_bps if is_liq else P.delta_exp_bps)
            res = ["overflow", None, None] if pref > U128 else ["ok", pref, lb]
        cases.append(line(now, is_liq, P.kappa_liq_bps, P.kappa_base_bps, P.delta_liq_bps, P.delta_exp_bps,
                          P.kappa_growth_n, P.ref_max_age_s, m.ema, liq, slow, ll, m.lb_sec, m.lb_lo, *res,
                          *ring_tokens(ring)))
    return cases


def max_in_liq(b_ref, delta, V, K, ert):
    """Transcription of Market._buyback_exit, liq_reserve = "none"."""
    return min(b_ref * (BPS + delta) // BPS, V - K - ert)


class FakeRay(FakeVenue):
    """A Raydium venue as Market.ref_depth_cap reads it (M4F-1): its locked depth D."""
    kind = "ray"

    def __init__(self, p, l, d):
        super().__init__(p, l)
        self.d = d

    def locked_depth(self):
        return self.d


def unit_ref_liquidity(rng, n):
    """L_b of Market.exit_reference (expiry; the reference quote replaced by a capture) with the
    post-change guard on or off and, on a Raydium venue, the M4F-1 cap: the pool's locked depth D
    itself since AIR-14 (floor(D*1e4/lambda) at lambda = 100 %, the lambda recorded), drawn around
    the EMA terms (so it binds about half the time); pool_lock_min_bps over its compiled range,
    which the cap no longer reads (AIR-14).

      ref_liquidity  l_live L_ema L_slow guard lambda D L_b | ring      (lambda, D "-" off Raydium)"""
    cases = []
    for _ in range(n):
        ring = rnd_ring(rng, rng.randrange(1, 9))
        lam = rng.choice([5_000, 5_000, 2_000, 10_000, rng.randrange(2_000, 10_001)])
        m = bare(P=replace(sim.DEFAULT, pool_lock_min_bps=lam))
        m.ring = ring
        m.ema = ring[-1][1]
        m.liq, m.liq_slow = rnd_liq(rng), rnd_liq(rng)
        if rng.random() < 0.3:                       # the M4F-1 shape: pushed EMA terms over a thin pool
            m.liq = m.liq * rng.randrange(2, 17)
        m.guard = rng.random() < 0.4
        l_e = min(m.liq, M.ring_medians(m)[2]) if m.guard else m.liq
        top = max(l_e, m.liq_slow)
        ll = rnd_liq(rng) if rng.random() < 0.6 else max(1, top * rng.randrange(2_000, 12_000) // 10_000)
        ray = rng.random() < 0.75
        if ray:
            pick = rng.random()
            if pick < 0.6:                           # cap = D around the EMA terms
                d = max(0, top * rng.randrange(3_000, 16_000) // 10_000)
            elif pick < 0.8:
                d = rnd_liq(rng)
            else:
                d = rng.choice([0, 1, top, U128 // BPS + 1, U128])
            m.venue = FakeRay(10**18, ll, d)
        else:
            d = None
            m.venue = FakeVenue(10**18, ll)
        now = ring[-1][0]
        m.lb_sec, m.lb_lo = now, ring[-1][1]
        cap = []
        m.quote_repriced = lambda Q, pp, L, c=cap: (c.append((pp, L)) or (0, 1))
        r = M.exit_reference(m, sim.Position(Q=1), {"lo_ok": True, "L_live": ll}, now, False)
        assert r is not None and len(cap) == 1
        cases.append(line(ll, m.liq, m.liq_slow, m.guard, BPS if ray else None, d, cap[0][1], "|",
                          *ring_tokens(ring)))
    return cases


def unit_spot_liquidity(rng, n):
    """Market.spot_liquidity (FITS-2 (a); AIR-17): the guard on or off, L_live drawn around the
    floor (a withdraw-drained pool below it, a deposit above it), L_ema around the ring's highs.

      spot_liquidity  l_live L_ema guard L_spot | ring"""
    cases = []
    for _ in range(n):
        ring = rnd_ring(rng, rng.randrange(1, 9))
        m = bare()
        m.ring = ring
        highs = sorted(e[8] for e in ring)
        m.liq = rnd_liq(rng) if rng.random() < 0.5 else max(1, rng.choice(highs) * rng.randrange(5_000, 15_000) // 10_000)
        m.guard = rng.random() < 0.6
        ll = rnd_liq(rng) if rng.random() < 0.4 else max(1, m.liq * rng.randrange(1_000, 13_000) // 10_000)
        cases.append(line(ll, m.liq, m.guard, M.spot_liquidity(m, ll), "|", *ring_tokens(ring)))
    return cases


def max_in_exp(b_ref, delta, eps, fee_bps, V, K, R, ert):
    """Transcription of Market._buyback_exit, exp_reserve = "rent+fee"."""
    senior = R + sim.cdiv(fee_bps * b_ref, BPS)
    return min(b_ref * (BPS + delta) // BPS, V - K - ert, max(V - senior - ert, b_ref * (BPS + eps) // BPS))


def unit_max_in(rng, n):
    cases = []
    for _ in range(n):
        big = rng.random() < 0.1
        b_ref = rng.randrange(0, 2**64) if big else rng.randrange(0, 10**12)
        V = rng.randrange(0, 2**64) if big else rng.randrange(0, 2 * 10**12)
        K = min(rng.randrange(0, V + 2) if rng.random() < 0.9 else rng.randrange(0, 2**64), U64)
        R = min(rng.randrange(0, V + 10**9), U64)
        ert = rng.choice([0, 6_500_000, rng.randrange(0, 10**7)])
        d, e, f = rng.randrange(0, 2**16), rng.randrange(0, 2**16), rng.randrange(0, 2**16)
        if rng.random() < 0.7:
            d, e, f = rng.randrange(0, 501), rng.randrange(0, 101), rng.randrange(0, 501)
        pi = sim.cdiv(f * b_ref, BPS)
        if pi > U64:
            continue
        ml = max_in_liq(b_ref, d, V, K, ert)
        me = max_in_exp(b_ref, d, e, f, V, K, R, ert)
        cases.append(line("liq", b_ref, d, V, K, ert, ml if ml >= 0 else None))
        cases.append(line("exp", b_ref, d, e, f, V, K, R, ert, pi, me if me >= 0 else None))
    return cases


def dump(name, obj):
    obj = {"generator": Path(__file__).name,
           "source": "docs/design/sim/shortwin_sim.py v0.4.1 (default switches)", **obj}
    text = json.dumps(obj, indent=1, sort_keys=True) + "\n"
    HERE.joinpath(name).write_text(text)
    return len(text)


def main():
    rng = random.Random(SEED)
    sizes = {
        "units_observe.json": dump("units_observe.json", {
            "integrate": unit_integrate(rng, 200), "rewrite": unit_rewrite(rng, 120)}),
        "units_ring.json": dump("units_ring.json", {"growth": unit_growth(rng, 300)}),
        "units_persist.json": dump("units_persist.json", {"persist": unit_persist(rng, 600)}),
        "units_bounds.json": dump("units_bounds.json", {
            "reference": unit_reference(rng, 600), "max_in": unit_max_in(rng, 500)}),
        "units_ref.json": dump("units_ref.json", {
            "ref_liquidity": unit_ref_liquidity(rng, 500),
            "spot_liquidity": unit_spot_liquidity(random.Random(SEED + 2), 400)}),
    }
    for k, v in sizes.items():
        print(f"{k}: {v:,} bytes")


if __name__ == "__main__":
    main()
