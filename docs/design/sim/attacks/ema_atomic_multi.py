"""C0-5 at the per-market OI cap: (a) a keeper forces and sandwiches the
liquidation of every position at once; (b) a lender owning 100 % of the pool
forces S1 on every position (the largest payoff: lenders receive all vault SOL
minus K_s for debt worth Q x true price).

Adapted from the critic's scratchpad/audit/ema_atomic_multi.py.  Positions are
opened at the per-position cap until the market OI cap binds.  The attacker
pushes the pool to p_true * mult every second (mult optimised).  For S1 the
attacker must keep every S1 leg true at every check for >= 40 EMA updates
spanning >= 30 s (settlement persistence), calling sol_settle each second.
"""
from common import *


def fill(m):
    poss, t = [], 0
    while True:
        q = m.max_open_q()
        X0 = m.venue.eff()[0]
        rt = m.venue.rt if m.venue.kind == "curve" else 0
        if m.venue.kind == "curve":     # #103 (M5-SEC-3): the curve's caps read its reserves at the EMA price
            X0, rt = m.curve_cap_reserves()
        tot = sum(p.Q for p in poss)
        cap = m.oi_cap_q(X0 + q)
        q = min(q, cap - tot)
        if m.venue.kind == "curve":
            g = m.P.curve_oi_gamma_bps
            q = min(q, (g * (rt + q) // BPS) - tot)
        if q <= q // 1000 + 1 or len(poss) > 40:
            break
        if q * m.venue.spot_wad() // WAD < 100_000_000:     # ADOPTED Q11: min notional 0.1 SOL
            break
        poss.append(m.open(q, t))
        t += 1
    return poss, t


def exit_all(m, poss, alive, t, dX):
    """Attacker transaction [buy dX, exit every open position it can, sell dX] on m."""
    v = m.venue
    if dX > 0 and v.kind == "curve":
        dX = min(dX, v.rt - 1)
    c = v.buy(dX) if dX > 0 else 0
    res = []
    for i in sorted(alive):
        r = m.try_exit(poss[i], t)
        if r is not None:
            res.append((i, r))
    back = m.venue.sell(dX) if (dX > 0 and m.venue.has_route()) else 0
    for i, _ in res:
        alive.discard(i)
    return back - c, res


def value(res, poss, p_true, mode):
    g = 0
    for i, r in res:
        g += r["keeper"]
        if mode == "s1" and r["kind"] in ("S1", "S2"):
            g += r["lender_sol"] - poss[i].Q * p_true // WAD
    return g


def best_tx(m, poss, alive, t, p_true, mode, fixed_target=None):
    """Pick the attacker's in-tx pump: grid of dX (liq) or the S1-level target (s1)."""
    X = m.venue.eff()[0]
    grid = [0] + [X * i // 1000 for i in (2, 5, 10, 20, 35, 50, 75, 100, 130, 160, 200, 250, 300, 350)]
    if fixed_target is not None:
        grid = [max(move_size(m.venue, fixed_target), 0)]
    best = None
    for dX in grid:
        mm, pp, aa = copy.deepcopy(m), copy.deepcopy(poss), set(alive)
        pnl, res = exit_all(mm, pp, aa, t, dX)
        tot = pnl + value(res, pp, p_true, mode)
        if res and (best is None or tot > best[0]):
            best = (tot, dX)
    return best


def rolling(key, model, mode, P=DEFAULT, T=None, mults=None, max_s=120):
    """mode 'liq': keeper forces + sandwiches liquidations; mode 's1': 100 % lender forces S1.
    The attacker stops at the best cumulative net (optimal stopping)."""
    m0 = Market(MARKETS[key](), T or tier_of(key), P)
    poss0, t0 = fill(m0)
    t0 = warm(m0, t0, t0 + 60)
    p_true = m0.venue.spot_wad()
    Tn = m0.tier
    if mults is None:
        mults = (1.5, 1.75, 2.0, 2.5, 3.0) if mode == "liq" else tuple(Tn.cr_open_bps / BPS * f for f in (1.1, 1.3, 1.6))
    best = None
    for mult in mults:
        m, poss = copy.deepcopy(m0), copy.deepcopy(poss0)
        alive = set(range(len(poss)))
        acc = dict(push=0, rt=0, keeper=0, s1=0, liq_n=0, s1_n=0, bl=0)
        for n in range(1, max_s + 1):
            t = t0 + n
            acc["push"] += push_second(m, t, int(p_true * mult), model)
            cand = [i for i in alive if (would_liq(m, poss[i], t) if mode == "liq" else would_s1(m, poss[i], t))]
            if cand:
                if mode == "liq":
                    bt = best_tx(m, poss, alive, t, p_true, mode)
                    dX = bt[1] if bt else 0
                else:
                    dX = max(move_size(m.venue, int(p_true * mult)), 0)
                pnl, res = exit_all(m, poss, alive, t, dX)
                acc["rt"] += pnl
                for i, r in res:
                    p = poss[i]
                    acc["keeper"] += r["keeper"]
                    acc["bl"] += p.M - r["borrower"]
                    if r["kind"] == "liquidation":
                        acc["liq_n"] += 1
                    else:
                        acc["s1_n"] += 1
                        acc["s1"] += r["lender_sol"] - p.Q * p_true // WAD
            net = acc["rt"] + acc["keeper"] + (acc["s1"] if mode == "s1" else 0) - acc["push"]
            if (acc["liq_n"] + acc["s1_n"]) and (best is None or net > best["net"]):
                best = dict(net=net, mult=mult, n=n, npos=len(poss), M=sum(p.M for p in poss), **acc)
            if not alive:
                break
    return best


def ratio_table():
    """ECON §6.1: held-model basket liquidation by fee_oi_cap_x100 (0 = off)."""
    from dataclasses import replace
    print("| fee_oi_cap_x100 | market | OI cap notional (SOL) | positions / margin (SOL) | held basket net (SOL) | borrower margin lost (SOL) |")
    print("|---|---|---|---|---|---|")
    for ratio, label in ((0, "off"), (750, "750 (default)"), (1000, "1000"), (500, "500")):
        P = replace(DEFAULT, fee_oi_cap_x100=ratio)
        for key in ("A", "B", "RA", "RB", "RC"):
            m = Market(MARKETS[key](), tier_of(key), P)
            X = m.venue.eff()[0]
            cap_n = m.oi_cap_q(X) * m.venue.spot_wad() // WAD
            b = rolling(key, "held", "liq", P)
            if b is None:
                print(f"| {label} | {key} | {sol(cap_n):.2f} | no exit forced | | |"); continue
            print(f"| {label} | {key} | {sol(cap_n):.2f} | {b['npos']} / {sol(b['M']):.2f} | {sol(b['net']):+.2f} | {sol(b['bl']):.2f} |")


if __name__ == "__main__" and "--ratios" in sys.argv:
    ratio_table()
    sys.exit(0)

if __name__ == "__main__":
    KEYS = ("A", "B", "C", "D@0%", "D@50%", "RA", "RB", "RC")
    for mode, title in (("liq", "(a) keeper forces and sandwiches liquidations of the OI-cap basket"),
                        ("s1", "(b) lender owning 100 % of the pool forces SOL settlement of the OI-cap basket")):
        for model in ("atomic", "held"):
            print(f"\n=== {title}, model={model} ===")
            print("| market | positions / total margin (SOL) | best push | seconds | push cost (SOL) | in-tx round trips incl. sandwiches (SOL) | keeper shares (SOL) | S1/S2 windfall to lenders (SOL) | liquidated / SOL-settled | attacker net (SOL) | borrower margin lost (SOL) |")
            print("|---|---|---|---|---|---|---|---|---|---|---|")
            for key in KEYS:
                b = rolling(key, model, mode)
                if b is None:
                    print(f"| {key} | | no exit forced within 120 s at any push level | | | | | | 0 / 0 | | |"); continue
                print(f"| {key} | {b['npos']} / {sol(b['M']):.2f} | x{b['mult']:.2f} | {b['n']} | {sol(b['push']):.2f} | {sol(b['rt']):+.2f} | {sol(b['keeper']):.2f} | {sol(b['s1']):+.2f} | {b['liq_n']} / {b['s1_n']} | {sol(b['net']):+.2f} | {sol(b['bl']):.2f} |")
