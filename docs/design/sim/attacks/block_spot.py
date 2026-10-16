"""V1-1 / V1-2 (second review): blocking the robust-spot leg with ONE low
observation per second.

(a) Offensive (V1-1).  While a genuine pump makes the position liquidatable,
    the attacker lands one atomic [sell -> crank_price -> buy back] per second
    that leaves a price below the liquidation price in that second's bucket, so
    P_rs = min(P_live, lb_lo) fails the spot leg for every honest keeper; the
    EMA leg keeps holding (directional aggregation freezes P_ema).  After w
    blocked seconds it leaves one clean second and sandwiches the liquidation.
    v0.2 started the Dutch delta clock at the start of the EMA-leg run, so delta
    had ramped to 4-5 % by then (reviewer: Tier A +0.923 SOL over an honest
    keeper, borrower -3.24 SOL).  v0.3 advanced the clock only over seconds in
    which both legs held on a completed bucket.  v0.4 has no clock: the bound is
    a fixed delta (2 %) over the reference, so blocking cannot widen it -- the
    whole attack class is gone; the table shows what remains.
(b) Defensive (V1-2).  The same primitive lets a BORROWER veto its own
    liquidation for as long as it pays.  Per-second cost of one low observation
    (round-trip venue fees of the dump) at the first executable second and
    further into the pump, against the equity the borrower would lose to a
    liquidation at that moment.
Honest keeper cranks every second (ECON §5.8); no arbitrage (costs = fees only).
Setup adapted from the reviewer's scratchpad/verify/probe_block_delta2.py.
"""
from dataclasses import replace
from common import *
import sandwich as SW


def liq_price_target(m, pos):
    """Largest spot price at which the spot leg (live liquidity) still fails."""
    st = m.status(pos, m.t)
    Veff, L = st["Veff"], pos.L
    Lk = m.venue.sqrt_k()
    lo, hi = m.venue.spot_wad() // 4, m.venue.spot_wad()
    for _ in range(80):
        mid = (lo + hi) // 2
        q = m.quote_repriced(pos.Q, mid, Lk)
        if q is not None and Veff * BPS // q[1] >= L:
            lo = mid
        else:
            hi = mid
    return lo


def dump_cost(v, target):
    """Round-trip fees of [sell to target -> buy back] on a copy of venue v."""
    vv = copy.deepcopy(v)
    d = move_size(vv, target)
    if d >= 0:
        return 0
    got = vv.sell(-d)
    return vv.buy(-d) - got


def setup_liq_S(S, key, P, g=0.001):
    m = S.new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    k = 1.0
    while k < 5:
        k *= 1 + g
        t += 1
        m.exo_move(int(pos.spot_post * k))
        m.step_to(t)
        if m.status(pos, t)["liq"]:
            return m, pos, t, k
    raise RuntimeError("never liquidatable")


def offensive(S, key, w, P, g=0.001):
    mh, ph, th, _ = setup_liq_S(S, key, P, g=g)
    honest = copy.deepcopy(mh).try_exit(copy.deepcopy(ph), th)
    m = S.new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    k, cost, blocked = 1.0, 0, 0
    while k < 5:
        k *= 1 + g
        t += 1
        m.exo_move(int(pos.spot_post * k))
        m.step_to(t)
        r = copy.deepcopy(m).try_exit(copy.deepcopy(pos), t)
        if r is not None and r["kind"] == "liquidation":
            return None
        if m.status(pos, t)["ema_run"] >= 1 or blocked:
            if blocked >= w:
                break
            tgt = liq_price_target(m, pos) * 995 // 1000
            d = move_size(m.venue, tgt)
            if d < 0:
                got = m.venue.sell(-d)
                m.observe(t)
                cost += m.venue.buy(-d) - got
            blocked += 1
    sw, wait = None, 0
    for wait in range(0, 10):
        tf = t + 1
        k *= 1 + g
        m.exo_move(int(pos.spot_post * k))
        sw = sandwich_exit(m, pos, tf)
        if sw is not None:
            break
        m.step_to(tf)
        t = tf
    if sw is None:
        return None
    pnl, r = sw[0], sw[1]
    d_bps = r["bound"] * BPS // r["b_ref"] - BPS
    return dict(cost=cost, pnl=pnl, extra=pnl - cost - honest["keeper"], hb=honest["borrower"],
                ab=r["borrower"], delta=d_bps, hl=honest["lender_sol"] + honest["protocol"],
                al=r["lender_sol"] + r["protocol"], wait=wait)


def defensive(key, P):
    m, pos, t, k = SW.setup_liq(key, P)
    honest = copy.deepcopy(m).try_exit(copy.deepcopy(pos), t)
    equity = honest["borrower"] if honest else 0
    tgt = liq_price_target(m, pos)
    cells = []
    for mult in (1.00, 1.05, 1.10, 1.20):
        v = copy.deepcopy(m.venue)
        if mult > 1:
            v.move_to_price(int(v.spot_wad() * mult))
        c_spot = dump_cost(v, tgt * 995 // 1000)
        c_ema = dump_cost(v, m.ema * 999 // 1000)
        cells.append((mult, c_spot, c_ema))
    return equity, cells


if __name__ == "__main__":
    import sys as _sys
    V03 = legacy_sim()
    V04 = _sys.modules["shortwin_sim"]
    KEYS = ("A", "B", "C", "D@0%", "RA", "RB", "RC")
    print("=== (a) block the spot leg for w seconds, then sandwich at the first clean second ===")
    print("| market | w (s) | exit bound | block cost (SOL) | first executable second after the block | sandwich + keeper share (SOL) "
          "| extra over an honest keeper (SOL) | borrower remainder honest -> attacked (SOL) | delta at the exit "
          "| lenders + protocol honest -> attacked (SOL) |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for key in KEYS:
        for w in (1, 4, 8):
            for label, S, P in (("v0.2 Dutch clock (EMA-leg run)", V03, V03.replace(V03.DEFAULT, dutch_clock="ema")),
                                ("v0.3 Dutch clock (executable seconds)", V03, V03.DEFAULT),
                                ("v0.4.1 fixed delta 2 %", V04, DEFAULT)):
                r = offensive(S, key, w, P)
                if r is None:
                    print(f"| {key} | {w} | {label} | not executable |"); continue
                print(f"| {key} | {w} | {label} | {sol(r['cost']):.3f} | +{r['wait'] + 1} s | {sol(r['pnl']):+.3f} | {sol(r['extra']):+.3f} | "
                      f"{sol(r['hb']):.3f} -> {sol(r['ab']):.3f} | {r['delta']/100:.2f}% | {sol(r['hl']):.4f} -> {sol(r['al']):.4f} |")
    print("\n=== (b) defensive veto: cost of one blocking observation per second vs the borrower's equity ===")
    print("| market | borrower remainder if liquidated at the first executable second (SOL) | spot / liquidation price "
          "| cost to fail the spot leg (SOL per s) | cost to freeze P_ema (SOL per s) | seconds of spot-leg veto the equity pays for |")
    print("|---|---|---|---|---|---|")
    for key in KEYS:
        eq, cells = defensive(key, DEFAULT)
        for mult, cs, ce in cells:
            secs = f"{eq / cs:,.0f}" if cs > 0 else "n/a"
            print(f"| {key} | {sol(eq):.3f} | x{mult:.2f} | {sol(cs):.4f} | {sol(ce):.4f} | {secs} |")
