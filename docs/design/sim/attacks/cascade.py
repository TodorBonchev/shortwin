"""Honest liquidation cascade on a market filled to the OI cap (reviewer's R3B/sim/cascade.py,
adapted to v0.4).  Each liquidation's own buyback moves the live price; the next position's
exit bound is computed from P_ema / the last bucket low, so it may have to wait.  Compared with
the same positions liquidated in isolation (the others never exit).  Fixed exit bound (v0.4)."""
import math
from common import *
from ema_atomic_multi import fill


def setup(key):
    m = Market(MARKETS[key](), tier_of(key), DEFAULT)
    poss, t = fill(m)
    for _ in range(60):
        t += 1
        m.step_to(t)
    return m, poss, t


def run_basket(key, path_fn, horizon=1800):
    m, poss, t = setup(key)
    p0, t0 = m.venue.spot_wad(), t
    alive, out = set(range(len(poss))), {}
    while t < t0 + horizon and alive:
        t += 1
        m.advance(t)
        mult = path_fn(t - t0)
        if mult is not None:
            m.exo_move(int(p0 * mult))
        m.observe(t)
        for i in sorted(alive, key=lambda i: m.status(poss[i], t)["cr_s"]):
            r = m.try_exit(poss[i], t)
            if r is not None:
                out[i] = (r["kind"], t - t0, r["spot"] / p0)
                alive.discard(i)
    return poss, out


def run_single(key, idx, path_fn, horizon=1800):
    m, poss, t = setup(key)
    p0, t0 = m.venue.spot_wad(), t
    pos = poss[idx]
    while t < t0 + horizon:
        t += 1
        m.advance(t)
        mult = path_fn(t - t0)
        if mult is not None:
            m.exo_move(int(p0 * mult))
        m.observe(t)
        r = m.try_exit(pos, t)
        if r is not None:
            return (r["kind"], t - t0, r["spot"] / p0)
    return ("open", horizon, None)


PATHS = {
    "gap 1.8x": lambda s: 1.8 if s >= 1 else None,
    "pump 2x/60s": lambda s: math.exp(math.log(2) * min(s, 60) / 60) if s >= 1 else None,
    "pump 3x/10min": lambda s: math.exp(math.log(3) * min(s, 600) / 600) if s >= 1 else None,
    "pump 10x/10min": lambda s: math.exp(math.log(10) * min(s, 600) / 600) if s >= 1 else None,
}

if __name__ == "__main__":
    print("| market | path | positions | basket outcomes | isolated outcomes | largest delay of a basket exit vs isolation (s) |")
    print("|---|---|---|---|---|---|")
    for key in ("A", "RA", "C"):
        for pname, pf in PATHS.items():
            poss, out = run_basket(key, pf)
            iso = [run_single(key, i, pf) for i in range(len(poss))]
            bk, ik = {}, {}
            for v in out.values():
                bk[v[0]] = bk.get(v[0], 0) + 1
            for v in iso:
                ik[v[0]] = ik.get(v[0], 0) + 1
            delay = max((out[i][1] - iso[i][1] for i in out if iso[i][0] != "open"), default=0)
            print(f"| {key} | {pname} | {len(poss)} | {bk} | {ik} | {delay} |")
