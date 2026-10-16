"""R3B simplification 1: replace the Dutch exit clock by a fixed delta over the reference.

Adapted from the reviewer's scratchpad/r3/R3B/sim/{dutch.py, dutch2.py, dutch3.py,
sandwich_fixed.py}.  Variants: the v0.3 model with its Dutch clock (1 % -> 5 %, 1 %/s over
executable seconds), the v0.3 model with a fixed 1 % / 2 % bound, and the v0.4 model (fixed
2 %, plus every other v0.4 change).  Liveness: does any genuine move end differently (outcome,
exit price or lender P&L)?  Extraction: the atomic-model sandwich at the first executable second
and after the keeper waits 4 s.
"""
import sys
from common import *
from block_spot import setup_liq_S

V03 = legacy_sim()
V04 = sys.modules["shortwin_sim"]
TE = 3600


def variants():
    return [("v0.3 Dutch 1->5 %", V03, V03.DEFAULT),
            ("v0.3 fixed 1 %", V03, V03.replace(V03.DEFAULT, delta_liq_slope_bps=0, delta_liq_max_bps=100)),
            ("v0.3 fixed 2 %", V03, V03.replace(V03.DEFAULT, delta_liq_min_bps=200, delta_liq_slope_bps=0,
                                                   delta_liq_max_bps=200)),
            ("v0.4.1 (fixed 2 %)", V04, DEFAULT)]


def cell(S, P, key, sc, lat=1):
    m = S.new_market(key, P)
    pos, r, P1 = S.run(m, 1.0, sc, keeper_latency=lat)
    s = S.summarize(pos, r, P1)
    return (s["kind"], s["t"] - TE, round(s["mult"], 3), round(s["lender_pnl_pct"], 1))


def part_a():
    keys = ["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC"]
    out = []
    for lat in (1, 2):
        base = {}
        for label, S, P in variants():
            pumps = [S.SC["pump 10x/60s"], S.Scenario("pump 10x/3min", S.ramp_path(TE, 10.0, 180)),
                     S.SC["pump 10x/10min"], S.SC["gap 5x"], S.SC["gap 2x"],
                     S.Scenario("pump 3x/60s", S.ramp_path(TE, 3.0, 60)),
                     S.Scenario("pump 2x/20s", S.ramp_path(TE, 2.0, 20)),
                     S.Scenario("pump 2x/5s", S.ramp_path(TE, 2.0, 5))]
            diff = worse = n = 0
            for key in keys:
                for i, sc in enumerate(pumps):
                    c = cell(S, P, key, sc, lat)
                    n += 1
                    if label.startswith("v0.3 Dutch"):
                        base[(key, i)] = c
                    else:
                        b = base[(key, i)]
                        if (c[0], c[2], c[3]) != (b[0], b[2], b[3]):
                            diff += 1
                            if (b[0] != "S1" and c[0] == "S1") or c[3] < b[3] - 0.5:
                                worse += 1
            out.append((f"latency {lat} s", label, n, diff, worse))
    return out


def part_b():
    gaps = {"A": [1.7, 1.8, 1.9, 2.1], "C": [2.0, 2.2, 2.5, 2.8], "D@0%": [2.4, 2.6, 3.0, 3.5],
            "RA": [1.7, 1.8, 1.9, 2.1]}
    out = []
    for lat in (1, 3, 5):
        base = {}
        for label, S, P in variants():
            diff = worse = n = 0
            for key, gl in gaps.items():
                scs = [S.Scenario(f"gap {g}x", S.step_path(TE, g)) for g in gl] + \
                      [S.Scenario(f"pump {g}x/30s", S.ramp_path(TE, g * 1.3, 30)) for g in gl]
                for i, sc in enumerate(scs):
                    c = cell(S, P, key, sc, lat)
                    n += 1
                    if label.startswith("v0.3 Dutch"):
                        base[(key, i)] = c
                    else:
                        b = base[(key, i)]
                        if (c[0], c[2], c[3]) != (b[0], b[2], b[3]):
                            diff += 1
                            if (b[0] != "S1" and c[0] == "S1") or c[3] < b[3] - 0.5:
                                worse += 1
            out.append((f"latency {lat} s", label, n, diff, worse))
    return out


def part_c():
    rows = []
    for key in ["RA", "RB", "RC"]:
        for k, dur in ((2.0, 600), (2.0, 120), (3.0, 600)):
            cells = [key, f"drain 50 % + {k:g}x/{dur}s"]
            for label, S, P in variants():
                sc = S.Scenario(f"d", S.ramp_path(TE + 60, k, dur), events=((TE, S.drain(5000)),))
                c = cell(S, P, key, sc)
                cells.append(f"{c[0][:4]} @{c[2]:.2f}x, lender {c[3]:+.0f}%")
            rows.append(cells)
    return rows


def part_d():
    rows = []
    for label, S, P in variants():
        for wait in (0, 4):
            cells = [label, f"{wait} s"]
            for key in ("A", "B", "C", "D@0%", "RA", "RB", "RC"):
                m, pos, t, k = setup_liq_S(S, key, P)
                honest = copy.deepcopy(m).try_exit(copy.deepcopy(pos), t)
                mm, pp = copy.deepcopy(m), copy.deepcopy(pos)
                tt = t
                for _ in range(wait):
                    tt += 1
                    mm.step_to(tt)
                sw = sandwich_exit(mm, pp, tt)
                if sw is None or honest is None:
                    cells.append("n/a")
                    continue
                cells.append(f"{sol(sw[0] - honest['keeper']):+.3f} / {sol(honest['borrower'] - sw[1]['borrower']):.3f}")
            rows.append(cells)
    return rows


if __name__ == "__main__":
    print("=== (a) reviewer's dutch.py: 9 markets x 8 genuine pump/gap paths; cells that differ from the v0.3 Dutch clock ===")
    print("| keeper latency | variant | runs | cells different (outcome, exit price or lender P&L) | of which worse for lenders |")
    print("|---|---|---|---|---|")
    for r in part_a():
        if "Dutch" not in r[1]:
            print("| " + " | ".join(str(x) for x in r) + " |")
    print("\n=== (b) reviewer's dutch2.py: gaps near the liquidation band and 30 s ramps ===")
    print("| keeper latency | variant | runs | cells different | of which worse for lenders |")
    print("|---|---|---|---|---|")
    for r in part_b():
        if "Dutch" not in r[1]:
            print("| " + " | ".join(str(x) for x in r) + " |")
    print("\n=== (c) reviewer's dutch3.py: 50 % LP drain (all unlocked LP at lambda_min) then a pump ===")
    print("| market | path | " + " | ".join(v[0] for v in variants()) + " |")
    print("|---|---|" + "---|" * len(variants()))
    for r in part_c():
        print("| " + " | ".join(r) + " |")
    print("\n=== (d) reviewer's sandwich_fixed.py: atomic-model liquidation sandwich, extra over an honest keeper / borrower loss (SOL) ===")
    print("| variant | keeper waits | A | B | C | D@0% | RA | RB | RC |")
    print("|---|---|---|---|---|---|---|---|---|")
    for r in part_d():
        print("| " + " | ".join(r) + " |")
