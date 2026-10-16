"""C0-5: forcing the liquidation of a FRESH position by EMA manipulation.

Adapted from the critic's scratchpad/audit/ema_atomic.py.  The attacker pushes
the pool to p_true * mult every second (mult optimised over a grid) until the
position is liquidatable under the v0.4 rules (robust spot leg + EMA leg on the
newest liq_persist_n = 3 ring entries), then is the keeper of a sandwiched
liquidation.  Model "atomic" = critic's bundles with the in-house keeper also
observing each second; model "held" = attacker controls every observation of
the second (no arbitrage).  Net = final bundle P&L (incl. keeper share) - push cost.
"""
from common import *

MULTS = (1.25, 1.5, 1.75, 2.0, 2.5, 3.0)


def attack(key, model, P=DEFAULT, max_s=60):
    m0 = new_market(key, P)
    pos0 = m0.open(m0.max_open_q(), 0)
    t0 = warm(m0, 0, 60)
    p_true = m0.venue.spot_wad()
    best = None
    for mult in MULTS:
        m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
        got = push_until(m, [pos], t0, p_true, mult, model, lambda mm, tt: would_liq(mm, pos, tt), max_s)
        if got is None:
            continue
        t, cost, n = got
        sw = sandwich_exit(m, pos, t + 1)        # final bundle in the next second
        if sw is None:
            continue
        net = sw[0] - cost
        if best is None or net > best[0]:
            best = (net, mult, n, cost, sw[0], pos.M - sw[1]["borrower"], sw[1])
    return pos0, best


if __name__ == "__main__":
    for model in ("atomic", "held"):
        print(f"\n=== fresh position (60 s after open), model={model} ===")
        print("| market | margin M (SOL) | best push (x true price, seconds) | push cost (SOL) | final sandwich + keeper (SOL) | attacker net (SOL) | borrower loss (SOL) |")
        print("|---|---|---|---|---|---|---|")
        for key in ("A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC"):
            pos, b = attack(key, model)
            if b is None:
                print(f"| {key} | {sol(pos.M):.3f} | not liquidatable within 60 s at any push level | | | | |"); continue
            net, mult, n, cost, fin, bl, r = b
            print(f"| {key} | {sol(pos.M):.3f} | x{mult:g}, {n} s | {sol(cost):.3f} | {sol(fin):+.3f} | {sol(net):+.3f} | {sol(bl):.3f} |")
