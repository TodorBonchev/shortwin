"""G-E2 (V2-14, ECON §12.10 (d)): `refute_push.widen_s3`'s borrower side (`side = "P"`) — a
contrary `[dump pct, crank_price, buy back]` every second for `w` seconds from 11 s before the
deadline of a healthy position whose Emergency flag was set 30 min earlier — on ECON's A, C, RA
and RC (a max-size position), with the attack stopped after `w` seconds instead of run for the
whole horizon. Prints, per market, −1 % / −3 % and w = 0 / 20 / 60, the seconds after the deadline
the S3 lands and the attacker's cost.

`tests/attacks_settle_it.rs` `a_contrary_observation_every_second_defers_s3_only` runs the same
attack on the mock's `RC`-like pool with a 0.4 % position: at −3 % its S3 lands 14 / 54 s after
the deadline, as here; at −1 % it is not deferred there (here it is: the position is larger).

    python3 tests/litesvm/sim/s3_widen.py
"""
import _attacks  # noqa: F401
from common import *  # noqa: E402,F401,F403


def run(key, w, pct, lead=12):
    m = new_market(key)
    pos = m.open(m.max_open_q(), 0)
    pos.deadline = 7200
    t = 0
    while t < 5400:
        t += 30
        m.step_to(t)
    m.route_broken = True
    while t < pos.deadline - lead:
        t += 1
        m.step_to(t)
    cost = 0
    n = 0
    for _ in range(400):
        t += 1
        m.step_to(t)
        if n < w:
            cost += attacker_bundle(m, t, m.venue.spot_wad() * (BPS - pct) // BPS)
            n += 1
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r, t - pos.deadline, cost
    return None, None, cost


for key in ("A", "C", "RA", "RC"):
    for pct in (100, 300):
        for w in (0, 20, 60):
            r, secs, cost = run(key, w, pct)
            print(key, f"-{pct // 100} %", "w", w, "S3 after the deadline (s)", secs, "cost", cost,
                  r and {k: r.get(k) for k in ("kind", "lender_sol", "lender_lump", "borrower", "fv")})
