"""ECON §12.6 / `sandwich.py`'s held liquidation sandwich on `D@50%` (G-C-curve).

Regenerates the constants of `tests/pump_curve_attacks.rs`
`held_pushes_before_a_liquidation_sandwich_do_not_pay`: `sandwich.setup_liq("D@50%")`, the
honest liquidation, then for h = 1 / 2 / 3 held seconds at `P_ema × (1 + x)` (x = 2.5 % … 30 %)
the pushes' cost and sizes, the best sandwich's `dX` and P&L, net, and the liquidation's legs.
The test runs h = 1 at +2.5 % and h = 3 at +30 %.

    python3 tests/litesvm/sim/curve_held_sandwich_d50.py
"""
import _attacks  # noqa: F401
from common import *  # noqa: E402,F401,F403
import sandwich as SW  # noqa: E402

LEGS = ("B", "b_ref", "max_in", "pen", "keeper", "borrower", "rent", "lender_sol", "protocol")

m0, pos0, t, k = SW.setup_liq("D@50%")
print("t", t, "k", k, "V", pos0.V, "M", pos0.M)
honest = copy.deepcopy(m0).try_exit(copy.deepcopy(pos0), t)
print("honest", {kk: honest[kk] for kk in LEGS})
for h in (1, 2, 3):
    for x in (250, 500, 1000, 1500, 2000, 3000):
        m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
        tt = t
        pre = 0
        sizes = []
        for _ in range(h):
            tt += 1
            tgt = m.ema * (BPS + x) // BPS
            sizes.append(move_size(m.venue, tgt))
            pre += push_second(m, tt, tgt, "held")
        sw = sandwich_exit(m, pos, tt + 1)
        if sw is None:
            print(h, x, "none")
            continue
        r = sw[1]
        print(h, x, "pre", pre, "sizes", sizes, "dX", sw[2], "pnl", sw[0], "net", sw[0] - pre,
              {kk: r[kk] for kk in LEGS})
