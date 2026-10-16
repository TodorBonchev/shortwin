"""ECON §12.5 `ema_atomic_multi.py` (b) on `D@50%`, the OI-cap basket and S1 forcing (G-C-curve).

Regenerates the constants of `tests/pump_curve_attacks.rs`: `D50_FILL` (`ema_atomic_multi.fill`,
with each step's `max_open_q`, the cap reserves and the OI cap — at t = 3 the sim sizes the
fourth open on `curve_cap_reserves()` read before the open's own observation, which is why the
program refuses its 1,811,466,694,782 `CapMarketOi` and takes 1,552,563,673,080), the S1
multiples (`cr_open × 1.1 / 1.3 / 1.6`), the held (b) row (`ema_atomic_multi.rolling`: best
net, `n`, push and round-trip totals), and the per-second trace at ×4.4 behind
`held_pushes_forcing_s1_on_the_oi_basket_lose` (the first candidate second 18, the fourth
position's S1 at 58).

    python3 tests/litesvm/sim/curve_s1_forcing_d50.py
"""
import _attacks  # noqa: F401
from common import *  # noqa: E402,F401,F403
import ema_atomic_multi as EM  # noqa: E402

print("=== fill, step by step")
m = Market(MARKETS["D@50%"](), tier_of("D@50%"), DEFAULT)
poss, t = [], 0
while True:
    q = m.max_open_q()
    X0, rt = m.curve_cap_reserves()
    tot = sum(p.Q for p in poss)
    cap = m.oi_cap_q(X0 + q)
    g = m.P.curve_oi_gamma_bps
    print("t", t, "max_open_q", q, "X0", X0, "rt", rt, "cap", cap, "tot", tot,
          "gamma", (g * (rt + q) // BPS) - tot, "ema", m.ema, "spot", m.venue.spot_wad())
    q = min(q, cap - tot, (g * (rt + q) // BPS) - tot)
    if q <= q // 1000 + 1 or len(poss) > 40:
        break
    if q * m.venue.spot_wad() // WAD < 100_000_000:
        break
    poss.append(m.open(q, t))
    t += 1
print([p.Q for p in poss])

print("=== EM.fill and the held (b) row")
m0 = Market(MARKETS["D@50%"](), tier_of("D@50%"), DEFAULT)
poss, t0 = EM.fill(m0)
print("fill", [p.Q for p in poss], "t", t0, "cr_open", m0.tier.cr_open_bps)
print("mults s1", tuple(m0.tier.cr_open_bps / BPS * f for f in (1.1, 1.3, 1.6)))
print(EM.rolling("D@50%", "held", "s1"))

print("=== held x4.4, second by second")
m = Market(MARKETS["D@50%"](), tier_of("D@50%"), DEFAULT)
poss, t0 = EM.fill(m)
t0 = warm(m, t0, t0 + 60)
p_true = m.venue.spot_wad()
print("t0", t0, "p_true", p_true, "V", [p.V for p in poss], "M", [p.M for p in poss])
mult = 4.4
alive = set(range(len(poss)))
acc = dict(push=0, rt=0, keeper=0, s1=0)
for n in range(1, 59):
    t = t0 + n
    sz = move_size(m.venue, int(p_true * mult))
    c = push_second(m, t, int(p_true * mult), "held")
    acc["push"] += c
    cand = [i for i in alive if would_s1(m, poss[i], t)]
    line = f"n {n} size {sz} cost {c} cand {cand}"
    if cand:
        dX = max(move_size(m.venue, int(p_true * mult)), 0)
        pnl, res = EM.exit_all(m, poss, alive, t, dX)
        acc["rt"] += pnl
        line += (f" dX {dX} rt {pnl} res "
                 f"{[(i, r['kind'], r.get('keeper'), r.get('lender_sol'), r.get('borrower')) for i, r in res]}")
        for i, r in res:
            acc["keeper"] += r["keeper"]
            acc["s1"] += r["lender_sol"] - poss[i].Q * p_true // WAD
    armed = [(i, p.s_kind, p.s_t) for i, p in enumerate(poss) if getattr(p, 's_kind', None)]
    line += f" armed {armed}"
    print(line)
print(acc)
