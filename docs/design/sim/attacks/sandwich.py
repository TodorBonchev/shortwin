"""C0-1: sandwiching the program's own exit buybacks (expire / liquidate).

Adapted from the critic's scratchpad/audit/sandwich.py; re-run on ECONOMICS.md v0.4.
The attacker is the keeper: [buy dX on the venue] -> [short.win expire/liquidate]
-> [sell dX], dX optimised on a grid.  Columns compare with an honest keeper
exiting at the first possible second.
  model "atomic": the attacker only sandwiches (the in-house keeper's
                  observations of earlier seconds are honest).
  model "held":   before the exit the attacker may also hold the pool at
                  EMA*(1+x) for h = 1..3 whole seconds (every observation sees
                  it, no arbitrage; x and h optimised, cost charged) to raise the
                  reference price the bound is computed from.
Liquidations at the first executable second and after the attacker waits 4 s
with no competing keeper; v0.4's bound is a fixed delta (2 %) over the reference,
so waiting no longer widens it (v0.3's Dutch clock went 1 % -> 5 %).  v0.4.1 (R4A-1, R4B-1):
a liquidation reserves nothing in max_in (SPEC §5.2: buy back iff B <= min(bound, V - K_s - ert));
an expiry keeps rent and the expiry fee senior (ECON §8.2).  The last section compares the three
liquidation reserve rules under the held model, on fresh positions and on positions aged at
90 % utilisation (rent due >> K_s).
Critic's v0.1 result: expiry A +43.74 SOL (borrower -57.16), liquidation A
+17.48 SOL (borrower -18.29, lenders -2.32).
"""
from common import *


def setup_expiry(key, P=DEFAULT):
    m = new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = 0
    while t < pos.deadline - 120:
        t = min(t + 30, pos.deadline - 120)
        m.step_to(t)
    while t < pos.deadline:
        t += 1
        m.step_to(t)
    return m, pos, t


def setup_liq(key, P=DEFAULT, g=0.001, util=8000, age=0):
    """Honest, sustained rise of +0.1 %/s (EMA tracks it) until the first second
    at which the position is liquidatable on both legs (an honest keeper would
    liquidate now).  age > 0: the calm period is fast-forwarded (rent accrues)."""
    m = Market(MARKETS[key](), tier_of(key), P, util_bps=util)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    if age:
        m.step_to(age)
        t = warm(m, age, age + 30)
    k = 1.0
    while k < 5:
        k *= 1 + g
        t += 1
        m.exo_move(int(pos.spot_post * k))
        m.step_to(t)
        if m.status(pos, t)["liq"]:
            return m, pos, t, k
    raise RuntimeError("never liquidatable")


def attack(m0, pos0, t_exit, want, model, wait=0):
    """Returns (honest_result, (attacker_net, attacked_result, (held_s, x_bps)))."""
    honest = copy.deepcopy(m0).try_exit(copy.deepcopy(pos0), t_exit)     # honest keeper, first moment
    best = None
    plans = [(0, 0)] if model == "atomic" else [(0, 0)] + [(h, x) for h in (1, 2, 3)
                                                             for x in (250, 500, 1000, 1500, 2000, 3000)]
    for h, x in plans:
        m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
        t = t_exit
        for _ in range(wait):                  # nobody exits while the attacker waits
            t += 1
            m.step_to(t)
        pre = 0
        for _ in range(h):                     # held pump: every observation of the second sees it
            t += 1
            pre += push_second(m, t, m.ema * (BPS + x) // BPS, "held")
        tf = t + 1 if h else t
        sw = sandwich_exit(m, pos, tf, want=want)
        if sw is None:
            continue
        pnl = sw[0] - pre
        if best is None or pnl > best[0]:
            best = (pnl, sw[1], (h, x))
    return honest, best


def row(key, V, h, a, f):
    lender = lambda r: r["lender_sol"] + r["protocol"]
    hs, x = a[2]
    plan = "none" if hs == 0 else f"{hs} s at +{x/100:.1f}%"
    return (f"| {key} | {sol(V):.3f} | {sol(h['B']):.3f} / {sol(h['rent']):.4f} / {sol(h[f]):.4f} / "
            f"{sol(h['borrower']):.3f} | {sol(a[1]['B']):.3f} / {sol(a[1]['rent']):.4f} / {sol(a[1][f]):.4f} / "
            f"{sol(a[1]['borrower']):.3f} | {plan} | {sol(a[0]):+.3f} | {sol(a[0] - h['keeper']):+.3f} | "
            f"{sol(h['borrower'] - a[1]['borrower']):.3f} | {sol(lender(h) - lender(a[1])):+.4f} |")


HDR = ("| market | vault V (SOL) | honest: B / rent / {f} / borrower | attacked: B / rent / {f} / borrower | "
       "held pump before exit | attacker net incl. keeper reward (SOL) | extra over an honest keeper (SOL) | "
       "borrower loss (SOL) | lender+protocol loss (SOL) |\n|---|---|---|---|---|---|---|---|---|")

KEYS = ("A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC")

if __name__ == "__main__":
    for model in ("atomic", "held"):
        print(f"\n=== expiry sandwich, model={model} (position at per-position cap; 80% util) ===")
        print(HDR.format(f="fee"))
        for key in KEYS:
            m, pos, t = setup_expiry(key)
            h, a = attack(m, pos, t, ("expiry",), model)
            if a is None:
                print(f"| {key} | {sol(pos.V):.3f} | exit not executable |"); continue
            print(row(key, pos.V, h, a, "fee"))
        for wait in (0, 4):
            print(f"\n=== liquidation sandwich, model={model}, keeper waits {wait} s after arming ===")
            print(HDR.format(f="penalty"))
            for key in KEYS:
                m, pos, t, k = setup_liq(key)
                h, a = attack(m, pos, t, ("liquidation",), model, wait=wait)
                if a is None or h is None:
                    print(f"| {key} | {sol(pos.V):.3f} | exit not executable at this time |"); continue
                print(row(key, pos.V, h, a, "pen"))
    print("\n=== held model, liquidation: the three reserve rules, fresh and aged positions (ECON §8.2, §12.6) ===")
    print("| market | position | rule | rent due / K_s (SOL) | lender+protocol loss vs honest (SOL) | attacker extra over an honest keeper (SOL) | borrower loss (SOL) |")
    print("|---|---|---|---|---|---|---|")
    lender = lambda r: r["lender_sol"] + r["protocol"]
    for key, age in (("A", 25 * 86400), ("B", 12 * 86400), ("RA", 25 * 86400)):
        for util, ag, lab in ((8000, 0, "fresh (1 h), 80 %"), (9000, age, f"aged {age // 86400} d, 90 %")):
            for rule in ("none", "rent", "rent+penalty"):
                P = replace(DEFAULT, liq_reserve=rule)
                m, pos, t, k = setup_liq(key, P, util=util, age=ag)
                R = m.rent_due(pos)
                h, a = attack(m, pos, t, ("liquidation",), "held")
                if a is None:
                    continue
                print(f"| {key} | {lab} | {rule}{' (v0.4.1)' if rule == 'none' else ''} | {sol(R):.3f} / "
                      f"{sol(m.settle_reward(pos.V)):.3f} | {sol(lender(h) - lender(a[1])):+.3f} | "
                      f"{sol(a[0] - h['keeper']):+.3f} | {sol(h['borrower'] - a[1]['borrower']):.3f} |", flush=True)
