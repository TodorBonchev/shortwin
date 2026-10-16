"""G-E2 (V1-2): a borrower's paid veto of its own liquidation on ECON's `RA`, run, not priced.

Regenerates the constants of `tests/attacks_exit_it.rs`
`a_borrower_veto_of_its_own_liquidation_costs_the_sims_figures_ra`: the EMA leg's start (the
second `t_start` and the multiple `k`), the honest flat-price liquidation 2 s later, and for a veto
of w = 10 / 60 / 300 s (one low observation a second at 99.5 % of `block_spot.liq_price_target`)
its total cost, its first and last second's cost, the liquidation 2 s after it stops and that
liquidation's legs.

    python3 tests/litesvm/sim/ra_veto.py
"""
import _attacks  # noqa: F401
from common import *  # noqa: E402,F401,F403
from block_spot import liq_price_target  # noqa: E402

LEGS = ("B", "b_ref", "max_in", "pen", "keeper", "borrower", "rent", "lender_sol", "protocol")

P = DEFAULT
m0 = new_market("RA", P)
pos0 = m0.open(m0.max_open_q(), 0)
t = warm(m0, 0, 120)
k = 1.0
while True:
    k *= 1.001
    t += 1
    m0.exo_move(int(pos0.spot_post * k))
    m0.step_to(t)
    r = copy.deepcopy(m0).try_exit(copy.deepcopy(pos0), t)
    assert r is None or r["kind"] != "liquidation"
    if m0.status(pos0, t)["ema_run"] >= 1:
        break
t_start = t
print("EMA run starts at", t, "k", k, "spot", m0.venue.spot_wad())
# The control: the price flat from here, no veto.
mh, ph = copy.deepcopy(m0), copy.deepcopy(pos0)
th = t
while True:
    r = copy.deepcopy(mh).try_exit(copy.deepcopy(ph), th)
    if r is not None and r["kind"] == "liquidation":
        break
    th += 1
    mh.step_to(th)
print("honest flat: liquidated at", th, "(+", th - t_start, ")", {kk: r[kk] for kk in LEGS})
for w in (10, 60, 300):
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    tt = t_start
    cost = 0
    costs = []
    for i in range(w):
        if i > 0:
            tt += 1
            m.step_to(tt)
        tgt = liq_price_target(m, pos) * 995 // 1000
        d = move_size(m.venue, tgt)
        assert d < 0
        got = m.venue.sell(-d)
        m.observe(tt)
        c = m.venue.buy(-d) - got
        cost += c
        costs.append(c)
        r = copy.deepcopy(m).try_exit(copy.deepcopy(pos), tt)
        assert r is None or r["kind"] != "liquidation", (w, i, r and r["kind"])
    last = tt
    while True:
        tt += 1
        m.step_to(tt)
        r = copy.deepcopy(m).try_exit(copy.deepcopy(pos), tt)
        if r is not None and r["kind"] == "liquidation":
            break
        assert tt - last < 30
    print("w", w, "cost", cost, "first", costs[0], "last", costs[-1], "liq after", tt - last, "s",
          {kk: r[kk] for kk in LEGS})
