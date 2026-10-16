"""C0-5: pushing a position that is NEAR its liquidation threshold over it.

Adapted from the critic's scratchpad/audit/ema_near.py: after an honest price
rise the EMA has fully caught up (CR_ema = c x L).  The attacker then pushes
the pool to p_true * mult every second (mult optimised) until the position is
liquidatable under the v0.4 rules, and is the keeper of a sandwiched liquidation in
the next second.  Honest counterfactual: the position stays open.  Critic's
v0.1 result: A x1.30 net +14.65 SOL, C x1.56 +0.95, D x1.82 +0.05 (and +20.2 /
+1.63 / +0.44 SOL with no push needed at CR_ema ~1.03-1.10 L).
"""
from common import *

MULTS = (1.02, 1.05, 1.1, 1.15, 1.2, 1.3, 1.5, 1.75, 2.0)


def setup(key, k, P=DEFAULT):
    m = new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 60)
    m.exo_move(int(m.venue.spot_wad() * k))
    t = warm(m, t, t + 300)                      # EMA fully caught up to the honest price
    return m, pos, t


def attack(key, k, model, P=DEFAULT):
    m0, pos0, t0 = setup(key, k, P)
    st0 = m0.status(pos0, t0)
    if st0["liq"]:
        return st0, None
    p_true = m0.venue.spot_wad()
    best = None
    for mult in MULTS:
        m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
        got = push_until(m, [pos], t0, p_true, mult, model, lambda mm, tt: would_liq(mm, pos, tt), 30)
        if got is None:
            continue
        t, cost, n = got
        sw = sandwich_exit(m, pos, t + 1)
        if sw is None:
            continue
        net = sw[0] - cost
        if best is None or net > best[0]:
            best = (net, mult, n, cost, sw[0], sw[1]["borrower"])
    return st0, best


if __name__ == "__main__":
    for model in ("atomic", "held"):
        print(f"\n=== near-threshold position, model={model} ===")
        print("| market | honest price rise (EMA caught up) | CR_ema before (x L) | best push (x price, seconds) | push cost (SOL) | final sandwich + keeper (SOL) | attacker net (SOL) | borrower left (SOL) |")
        print("|---|---|---|---|---|---|---|---|")
        for key in ("A", "C", "D@0%", "RA", "RC"):
            for k0 in (1.30, 1.40, 1.50, 1.55):
                kk = k0 * (1.4 if key.startswith("D") else 1.2 if key in ("C", "RC") else 1.0)
                st0, b = attack(key, kk, model)
                L = tier_of(key).cr_liq_bps
                if b is None:
                    note = "already liquidatable (honest)" if st0["liq"] else "not reached within 30 s"
                    print(f"| {key} | x{kk:.2f} | {st0['cr_e']/L:.3f} | {note} | | | | |"); continue
                net, mult, n, cost, fin, left = b
                print(f"| {key} | x{kk:.2f} | {st0['cr_e']/L:.3f} | x{mult:g}, {n} s | {sol(cost):.3f} | {sol(fin):+.3f} | {sol(net):+.3f} | {sol(left):.3f} |")
