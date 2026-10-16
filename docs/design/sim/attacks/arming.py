"""V2-4 (second review): resetting S1/S2 arming with evidence produced inside one
transaction.

A position sits in S1 about 5 % beyond the S1 boundary (a genuine gap; the
EMA has caught up).  The in-house keeper cranks and calls sol_settle every
second.  Every 20 s the BORROWER lands one atomic bundle
[dump below the S1 price -> sol_settle -> buy back]: inside it the live S1 leg
fails.  v0.2 disarmed on any false evaluation, so the 40-update / 30 s arming
never completed; v0.3 disarms only on non-atomic (EMA) evidence, keeps one
eligibility clock for S1 and S2 and chooses the kind at execution; v0.4
additionally takes every settlement decision on the settlement box (EMA state
and ring-median state).  No arbitrage; the borrower's cost is the round-trip
venue fees.
"""
from dataclasses import replace
from common import *


def s1_boundary(m, pos):
    """Spot multiple (of the current price) at which B_live + ert > V - K_s."""
    K = m.settle_reward(pos.V)
    lo, hi = 1.0, 20.0
    for _ in range(50):
        mid = (lo + hi) / 2
        v = copy.deepcopy(m.venue)
        v.move_to_price(int(v.spot_wad() * mid))
        q = v.quote_at(pos.Q, *v.eff())
        if q is None or q[1] + getattr(pos, "ert", m.P.exec_rent_tol) > pos.V - K:
            hi = mid
        else:
            lo = mid
    return hi


def run(key, P, every=20, horizon=600, attack=True, S=None):
    m = (S.new_market if S else new_market)(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    kb = s1_boundary(m, pos)
    p0 = m.venue.spot_wad()
    m.exo_move(int(p0 * kb * 1.05))                 # genuine gap to 5 % beyond the S1 boundary
    cost, calls = 0, 0
    t_gap = t
    for s in range(1, horizon + 1):
        t += 1
        m.step_to(t)
        r = m.try_exit(pos, t, settle_only=True)     # keeper
        if r:
            return r["kind"], t - t_gap, calls, cost
        if attack and s % every == 0:
            tgt = int(p0 * kb * 0.99)                # live S1 leg fails inside the bundle
            d = move_size(m.venue, tgt)
            if d < 0:
                got = m.venue.sell(-d)
                m.try_exit(pos, t, settle_only=True)     # the borrower's own sol_settle
                cost += m.venue.buy(-d) - got
                calls += 1
    return "none", horizon, calls, cost


if __name__ == "__main__":
    import sys as _sys
    V03 = legacy_sim()
    print("| market | arming rule | borrower bundles every 20 s | outcome | settled after (s) | borrower bundles | borrower cost (SOL) |")
    print("|---|---|---|---|---|---|---|")
    for key in ("A", "B", "C", "D@0%", "RA", "RB", "RC"):
        for label, S, P in (("v0.2 (any false check disarms)", V03, V03.replace(V03.DEFAULT, arming="v2")),
                            ("v0.3 (non-atomic evidence only)", V03, V03.DEFAULT),
                            ("v0.4.1 (settlement box)", None, DEFAULT)):
            for attack in (False, True):
                kind, secs, calls, cost = run(key, P, attack=attack, S=S)
                print(f"| {key} | {label} | {'yes' if attack else 'no'} | {kind} | "
                      f"{secs if kind != 'none' else '> 600'} | {calls} | {sol(cost):.3f} |")
