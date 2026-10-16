"""ECON §12.3 `ema_near.py` on `D@50%` (G-C-curve).

Regenerates the constants of `tests/pump_curve_attacks.rs` `NEAR` (the honest rises
`k₀ × 1.4` and the sim's `CR_ema` in bps after them), the atomic and held tables
(`ema_near.attack`), and the held run at ×2.17 at the best push ×1.15 that
`held_pushes_near_the_threshold_do_not_pay` replays: the held seconds, their sizes and costs,
the final sandwich's `dX`, P&L and legs.

    python3 tests/litesvm/sim/curve_ema_near_d50.py
"""
import _attacks  # noqa: F401
from common import *  # noqa: E402,F401,F403
import ema_near as EN  # noqa: E402

L = tier_of("D@50%").cr_liq_bps
for k0 in (1.30, 1.40, 1.50, 1.55):
    kk = k0 * 1.4
    m0, pos0, t0 = EN.setup("D@50%", kk)
    st0 = m0.status(pos0, t0)
    print(f"x{kk!r}", "t0", t0, "cr_e", st0["cr_e"], "spot", m0.venue.spot_wad(), "ema", m0.ema,
          "liq", st0["liq"])
for model in ("atomic", "held"):
    for k0 in (1.30, 1.40, 1.50, 1.55):
        kk = k0 * 1.4
        st0, b = EN.attack("D@50%", kk, model)
        print(model, f"x{kk:.2f}", f"{st0['cr_e'] / L:.4f}", "liq" if st0["liq"] else "",
              b and (sol(b[0]), b[1], b[2], sol(b[3]), sol(b[4]), sol(b[5])))
kk = 1.55 * 1.4
m0, pos0, t0 = EN.setup("D@50%", kk)
p_true = m0.venue.spot_wad()
m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
target = int(p_true * 1.15)
t = t0
cost = 0
costs = []
sizes = []
for n in range(1, 31):
    t += 1
    sizes.append(move_size(m.venue, target))
    c = push_second(m, t, target, "held")
    cost += c
    costs.append(c)
    if would_liq(m, pos, t):
        break
print("held x2.17 at x1.15: n", n, "cost", cost, "sizes", sizes[:2], "costs", costs)
sw = sandwich_exit(m, pos, t + 1)
r = sw[1]
print("dX", sw[2], "pnl", sw[0],
      {kk: r[kk] for kk in ("B", "b_ref", "max_in", "pen", "keeper", "keeper_topup", "borrower", "rent")})
