"""Shared attacker model for the short.win attack scripts (ECONOMICS.md v0.4.1 §12).

Two attacker models, both with ZERO arbitrage (nobody trades against a pumped
pool) -- attack costs are therefore lower bounds:

  "atomic"  the critic's model (C0-5): once per second the attacker lands an
            atomic bundle [pump -> crank_price -> dump].  The in-house keeper also
            cranks every second (ECON §5.6 duty), so the second's observation
            bucket contains the true price as well.
  "held"    stronger: the attacker holds the pool at the target price for the
            whole second, so EVERY observation of that second -- including the
            honest keeper's -- sees it.  Equivalent to "no honest observer".
            Cost is still only the round-trip venue fees (no arbitrage).

Both models let the attacker be the keeper of the final exit and sandwich it:
[buy dX -> short.win liquidate/expire -> sell dX] with dX optimised on a grid.
"""
import copy
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))
from shortwin_sim import *            # noqa: E402,F401
import importlib.util as _ilu          # noqa: E402


def legacy_sim():
    """The frozen v0.3 model (../legacy/v0_3/shortwin_sim.py), for comparison rows only."""
    path = os.path.join(HERE, "..", "legacy", "v0_3", "shortwin_sim.py")
    spec = _ilu.spec_from_file_location("shortwin_sim_v03", path)
    mod = _ilu.module_from_spec(spec)
    sys.modules["shortwin_sim_v03"] = mod
    spec.loader.exec_module(mod)
    return mod


def move_size(v, target_wad):
    """Tokens to buy (>0) or sell (<0) to bring venue v to target price."""
    X, Y = v.eff()
    tX = math.isqrt(X * Y * WAD // max(target_wad, 1))
    d = X - tX
    if v.kind == "curve" and d > 0:
        d = min(d, v.rt - 1)
    return d


def push_second(m, t, target_wad, model):
    """One second of EMA pressure toward target.  Returns attacker cost (lamports)."""
    v = m.venue
    if not v.has_route():
        m.step_to(t)
        return 0
    d = move_size(v, target_wad)
    if model == "atomic":
        m.step_to(t)                       # honest keeper crank (true price)
        if d > 0:
            c = v.buy(d)
            m.observe(t)
            return c - v.sell(d)
        m.observe(t)
        return 0
    # held: every observation of this second sees the target price
    m.advance(t)
    c = v.buy(d) if d > 0 else 0
    m.observe(t)                           # attacker crank
    m.observe(t)                           # honest keeper crank sees the same pumped pool
    back = v.sell(d) if d > 0 else 0
    return c - back


def honest_second(m, t):
    m.step_to(t)
    return 0


def sandwich_exit(m0, pos0, t, want=("liquidation",), grid=None, cost_keeper_share=True):
    """Best [buy dX, exit, sell dX] for the attacker-keeper at time t.
    Returns (profit_incl_keeper_reward, result, dX, market_after) or None."""
    X = m0.venue.eff()[0]
    grid = grid or [0] + [X * i // 2000 for i in range(1, 200)] + [X * i // 2000 for i in range(200, 900, 4)]
    best = None
    for dX in grid:
        m = copy.deepcopy(m0)
        pos = copy.deepcopy(pos0)
        v = m.venue
        if dX > 0:
            if v.kind == "curve" and dX > v.rt - pos.Q - 1:
                break
            try:
                c = v.buy(dX)
            except AssertionError:
                break
        else:
            c = 0
        r = m.try_exit(pos, t)
        if r is None or r["kind"] not in want:
            continue
        back = m.venue.sell(dX) if (dX > 0 and m.venue.has_route()) else 0
        pnl = back - c + (r["keeper"] if cost_keeper_share else 0)
        if best is None or pnl > best[0]:
            best = (pnl, r, dX, m, pos)
    return best


def warm(m, t0, t1, step=1):
    t = t0
    while t < t1:
        t += step
        m.step_to(t)
    return t


def fmt(x):
    return f"{sol(x):+.3f}"


def push_until(m, poss, t, p_true, mult, model, cond, max_s=60):
    """Push the pool to p_true*mult every second until cond(m, t) holds.
    Returns (t, cost, seconds) or None if not reached within max_s seconds."""
    cost = 0
    for n in range(1, max_s + 1):
        t += 1
        cost += push_second(m, t, int(p_true * mult), model)
        if cond(m, t):
            return t, cost, n
    return None


def basket_sandwich(m0, poss0, t, want=("liquidation",)):
    """One attacker transaction [buy dX, exit p1, exit p2, ..., sell dX]."""
    X = m0.venue.eff()[0]
    best = None
    for i in [0] + list(range(1, 200, 3)) + list(range(200, 900, 8)):
        dX = X * i // 2000
        m = copy.deepcopy(m0)
        poss = copy.deepcopy(poss0)
        v = m.venue
        if dX > 0:
            if v.kind == "curve" and dX > v.rt - sum(p.Q for p in poss) - 1:
                break
            c = v.buy(dX)
        else:
            c = 0
        gain, done, res = 0, 0, []
        for p in poss:
            r = m.try_exit(p, t)
            if r is None or r["kind"] not in want:
                continue
            gain += r["keeper"]
            done += 1
            res.append((p, r))
        back = m.venue.sell(dX) if (dX > 0 and m.venue.has_route()) else 0
        pnl = back - c + gain
        if done and (best is None or pnl > best[0]):
            best = (pnl, done, res, dX)
    return best


def would_liq(m, pos, t):
    """Liquidatable if the attacker pumps live spot >= last bucket low inside its own final
    transaction: EMA leg held on the newest liq_persist_n ring entries (cached reprojections)
    and the bucket-low leg below L."""
    st = m.status(pos, t)
    if not st["ema_leg"] or not st["lo_ok"] or st.get("gated"):     # M4R: no keeper exit during the warm-up
        return False
    q = m.quote_repriced(pos.Q, m.lb_lo, m.venue.sqrt_k())
    return q is None or st["Veff"] * BPS // q[1] < pos.L


def would_s1(m, pos, t):
    """S1 legs (EMA box and bucket low) satisfied, assuming live spot pumped in-tx."""
    st = m.status(pos, t)
    K = m.settle_reward(pos.V)
    if not st["lo_ok"]:
        return False
    P_lo, P_hi, L_lo, L_hi = m.settle_box(t)
    return m._s1_at(pos, K, m.lb_lo, m.venue.sqrt_k()) and m._s1_at(pos, K, P_lo, L_hi)


class LenderAdverseCorner(Market):
    """Analysis only: the literal 'evaluate every decision on the lender-adverse side of
    (P_ema, P_med) / (L_ema, L_med)' reading -- refutation and the S2/S3 amount at the corner
    (P_lo, L_hi), no amount-consistency check.  Rejected in ECON §5.6; used for comparison rows."""

    def _refuted(self, pos, st, box, K):
        P_lo, P_hi, L_lo, L_hi = box
        return not (self._s1_at(pos, K, P_lo, L_hi) or self._s2_at(pos, P_lo, L_hi))

    def _settle(self, res, kind, pos, K, st, box):
        take = pos.V - K
        borrower = 0
        fv = None
        if kind in ("S2", "S3") and self.P.fv_cap:
            fv = self.fv_at(pos, st, box[0], box[3])
            if fv is not None and take > fv:
                borrower, take = take - fv, fv
        res.update(kind=kind, keeper=K, lender_sol=take, lender_lump=0, borrower=borrower, fv=fv)
        return res


def market_variant(cls, key, P=None):
    return cls(MARKETS[key](), tier_of(key), P or DEFAULT)
