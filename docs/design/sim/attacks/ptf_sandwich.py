"""R4B-2 (reviewer's ptf_sandwich.py): push-then-freeze lifts the LIQUIDATION REFERENCE.

v0.4: P_ref = min(max(P_ema, lo_x), P_ema*(1 + kappa_eff)).  The honest keeper observes every second
except ONE (the attacker-only push second).  After the push the attacker freezes P_ema with one
atomic bundle per second (the honest keeper's observation is in the bucket too, so obs = P_ema), then,
as keeper, sandwiches the liquidation of a position that is genuinely liquidatable at spot.  The exit
bound is 2 % over a reference that sits at the pushed P_ema.  v0.4.1 also caps the reference at
lo_x*(1 + kappa_eff), the (honest) bucket low extrapolated by the sustained growth -- a frozen EMA has
no sustained growth, so the reference stays at the honest price.
"""
from common import *
from sandwich import setup_liq


def run(key, mult, freeze_s, lead, P):
    m0, pos0, t_liq, k = setup_liq(key, P)
    honest = copy.deepcopy(m0).try_exit(copy.deepcopy(pos0), t_liq)
    m = new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    kk = 1.0
    cost = 0
    while t < t_liq - lead:
        kk *= 1.001
        t += 1
        m.exo_move(int(pos.spot_post * kk))
        m.step_to(t)
    kk *= 1.001
    t += 1
    m.exo_move(int(pos.spot_post * kk))
    m.advance(t)
    cost += attacker_bundle(m, t, int(m.venue.spot_wad() * mult))            # attacker-only push second
    fz = 0
    while t < t_liq - 1:
        kk *= 1.001
        t += 1
        m.exo_move(int(pos.spot_post * kk))
        m.step_to(t)                                                          # honest keeper observation
        if fz < freeze_s:
            cost += attacker_bundle(m, t, m.ema * (BPS + 5) // BPS)             # freeze bundle
            fz += 1
    kk *= 1.001
    t += 1
    m.exo_move(int(pos.spot_post * kk))
    m.advance(t)
    best = sandwich_exit(m, pos, t, want=("liquidation",))
    return honest, best, cost, fz


if __name__ == "__main__":
    print("| market | rule | push | freeze s | honest keeper: B / borrower (SOL) | attacked: B / borrower (SOL) | "
          "B_ref / honest B_ref | attacker net after push and freeze cost (SOL) | borrower loss (SOL) | lenders + protocol change (SOL) |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for key in ("A", "B", "C", "RA", "RB"):
        for label, P in (("v0.4 (reference from the pushed P_ema)", replace(DEFAULT, ref_cap_lo=False, box_obs=True)),
                         ("v0.4.1 (also <= lo_x*(1 + kappa_eff))", DEFAULT)):
            for mult, fr, lead in ((1.5, 4, 6), (2.0, 4, 6), (2.0, 8, 10)):
                honest, best, cost, fz = run(key, mult, fr, lead, P)
                if best is None:
                    print(f"| {key} | {label} | x{mult} | {fz} | {sol(honest['B']):.3f} / {sol(honest['borrower']):.3f} | "
                          f"no liquidation | | | | |")
                    continue
                pnl, r, dX, mm, pp = best
                lp_h = honest['rent'] + honest['pen'] - honest['keeper'] + honest.get('keeper_topup', 0)
                lp_a = r['rent'] + r['pen'] - r['keeper'] + r.get('keeper_topup', 0)
                print(f"| {key} | {label} | x{mult} | {fz} | {sol(honest['B']):.3f} / {sol(honest['borrower']):.3f} | "
                      f"{sol(r['B']):.3f} / {sol(r['borrower']):.3f} | {r['b_ref']/honest['b_ref']:.3f} | "
                      f"{sol(pnl - cost):+.3f} | {sol(honest['borrower'] - r['borrower']):.3f} | {sol(lp_a - lp_h):+.4f} |",
                      flush=True)
