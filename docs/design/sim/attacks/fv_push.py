"""V2-9, R3A-1, R3A-6(a): moving a settlement decision with attacker-only seconds.

One second whose only observation is the attacker's moves P_ema by up to +16.7 % / -8.3 % and
leaves one pushed value among the ring's 8 aggregated observations.  Attacks (no arbitrage;
costs are round-trip venue fees):

  up      a lender-keeper pushes P_ema up in one attacker-only second, then calls sol_settle
          itself every second (raises an S2/S3 FV cap);
  flip n  the reviewer's r3_s1_bypass.py: n attacker-only pushes, then ONE bundle
          [pump live -> sol_settle -> dump] in the next second, trying to turn a capped S2/S3
          into an uncapped S1 (all S1 legs "hold": live pumped in-tx, robust spot = its own
          pushed bucket, EMA pushed);
  down    a borrower pushes P_ema down in one attacker-only second (lowers FV, or refutes);
  down*   the borrower repeats the push whenever the settlement would otherwise go through
          (deferral), for 600 s.

  ptf k   push-then-freeze (R4B-2): ONE attacker-only push, then k seconds in which the honest
          keeper observes AND the attacker lands an observation just beyond P_ema (the bucket
          straddles P_ema, so obs = P_ema: the pushed EMA is frozen), then the flip bundle;
  flip n  (R4A-9) n CONSECUTIVE attacker-only pushes (degraded mode or a censored keeper), and
          the same with one honest second between pushes;
  veto    (R4A-3) the borrower lands one contrary low observation in the second before the
          keeper's sol_settle(S3) (the V1-2 primitive: fails the robust-spot leg, not attacker-only).

v0.3 (legacy model): FV gate on S2 only, kind from instantaneous legs.  v0.4: ONE rule -- every
settlement decision must come out the same on the box spanned by (P_ema, L_ema) and the ring
medians of the aggregated observations; amounts within fv_dev_bps.  v0.4.1 (ECON §5.6): the box
medians are taken over the ring entries' BUCKET EXTREMES (lower median of the lows, upper median
of the highs), so a frozen second keeps its honest low and high; FV's p' is decided on the box
corner it is evaluated at, never on the raw legs.

Cases: (M) the drained RA pool of ECON §8.6 (97 % of LP pulled, x1.2 / x1.3; 95 %, x1.4) -- a
MODEL case beyond what an adopted pool allows; (E) realistic: the Emergency route-broken flag
is set and a healthy position expires -> S3 on an executable venue, FV cap binding hard.
"""
import sys
from dataclasses import replace
from common import *

V03 = legacy_sim()
V04 = sys.modules["shortwin_sim"]


def _drain(v, bps):
    try:
        v.remove_liquidity(bps, force=True)
    except TypeError:
        v.remove_liquidity(bps)


def ready_model(S, key, drain_bps, k, P):
    """(M) state one second before an honest keeper's sol_settle would execute."""
    m = S.new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 60)
    _drain(m.venue, drain_bps)
    t = warm(m, t, t + 600)
    m.exo_move(int(m.venue.spot_wad() * k))
    t = warm(m, t, t + 120)
    for _ in range(300):
        t += 1
        mc, pc = copy.deepcopy(m), copy.deepcopy(pos)
        mc.step_to(t)
        r = mc.try_exit(pc, t, settle_only=True)
        if r:
            return m, pos, t - 1, r
        m.step_to(t)
        m.try_exit(pos, t, settle_only=True)
    raise RuntimeError("no settlement")


def ready_flag(key, P=DEFAULT, cls=None):
    """(E) Emergency flag set 30 min before a healthy position's deadline; state one second
    before an honest keeper's sol_settle(S3) would execute at the deadline."""
    m = market_variant(cls, key, P) if cls else new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    pos.deadline = 7200
    t = 0
    while t < 5400:
        t += 30
        m.step_to(t)
    m.route_broken = True
    while t < pos.deadline - 1:
        t += 1
        m.step_to(t)
    mc, pc = copy.deepcopy(m), copy.deepcopy(pos)
    mc.step_to(t + 1)
    honest = mc.try_exit(pc, t + 1, settle_only=True)
    return m, pos, t, honest


def push(m, t, mult):
    m.advance(t)
    return attacker_bundle(m, t, int(m.venue.spot_wad() * mult))      # alone in its second


def attack_up(m0, pos0, t0, mult=3.0, horizon=60):
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    t = t0 + 1
    cost = push(m, t, mult)
    for _ in range(horizon):
        t += 1
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r, cost, t - t0 - 1
    return None, cost, None


def attack_flip(m0, pos0, t0, n, mult=3.0):
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    t = t0
    cost = 0
    for _ in range(n):
        t += 1
        cost += push(m, t, mult)
    t += 1
    m.advance(t)
    v = m.venue
    d = move_size(v, int(v.spot_wad() * mult))
    c = v.buy(d) if d > 0 else 0
    r = m.try_exit(pos, t, settle_only=True)
    back = v.sell(d) if d > 0 else 0
    cost += c - back
    return r, cost, n + 1


def attack_down(m0, pos0, t0, mult=0.33, repeat=False, horizon=600):
    """The borrower pushes down; the honest keeper calls sol_settle every other second.  With
    repeat=True the borrower pushes again whenever the keeper's next call would settle."""
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    t = t0 + 1
    cost = push(m, t, mult)
    pushes = 1
    for _ in range(horizon):
        t += 1
        if repeat:
            mc, pc = copy.deepcopy(m), copy.deepcopy(pos)
            mc.step_to(t)
            if mc.try_exit(pc, t, settle_only=True):
                cost += push(m, t, mult)                 # the borrower takes this second instead
                pushes += 1
                continue
        m.step_to(t)
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r, cost, t - t0 - 1, pushes
    return None, cost, None, pushes


def attack_ptf(m0, pos0, t0, k, mult=3.0):
    """R4B-2 push-then-freeze flip: 1 attacker-only push, k frozen seconds, then the flip bundle."""
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    t = t0 + 1
    cost = push(m, t, mult)
    for _ in range(k):
        t += 1
        m.step_to(t)                                            # honest keeper observation
        cost += attacker_bundle(m, t, m.ema * (BPS + 5) // BPS)  # freeze: obs = P_ema
    t += 1
    m.advance(t)
    v = m.venue
    d = move_size(v, int(v.spot_wad() * mult))
    c = v.buy(d) if d > 0 else 0
    r = m.try_exit(pos, t, settle_only=True)
    back = v.sell(d) if d > 0 else 0
    return r, cost + c - back, k + 2


def attack_flip_gap(m0, pos0, t0, n, gap, mult=3.0):
    """R4A-9: n attacker-only pushes with `gap` honest seconds (keeper crank + sol_settle) between them."""
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    t, cost = t0, 0
    for i in range(n):
        t += 1
        cost += push(m, t, mult)
        if i < n - 1:
            for _ in range(gap):
                t += 1
                m.step_to(t)
                r = m.try_exit(pos, t, settle_only=True)
                if r:
                    return r, cost, "honest"
    t += 1
    m.advance(t)
    v = m.venue
    d = move_size(v, int(v.spot_wad() * mult))
    c = v.buy(d) if d > 0 else 0
    r = m.try_exit(pos, t, settle_only=True)
    back = v.sell(d) if d > 0 else 0
    return r, cost + c - back, None


def ready_veto(key, mult, P=DEFAULT):
    """R4A-3: the Emergency flag is set, a genuine move of x`mult` makes the position expired AND
    CR-liquidatable at its deadline; state one second before the keeper's sol_settle(S3)."""
    m = new_market(key, P)
    pos = m.open(m.max_open_q(), 0)
    t = 0
    while t < 5400:
        t += 30
        m.step_to(t)
    m.route_broken = True
    pos.deadline = 7200
    m.exo_move(int(m.venue.spot_wad() * mult))
    while t < pos.deadline - 1:
        t += 1
        m.step_to(t)
    return m, pos, t


def veto_run(m0, pos0, t, veto):
    """Honest: keeper cranks at t+1 and t+2, sol_settle at t+2.  Veto: in t+1 the borrower lands
    [sell -> crank -> buy back] below the liquidation price (the honest crank is in t+1 too)."""
    from block_spot import liq_price_target
    m, pos = copy.deepcopy(m0), copy.deepcopy(pos0)
    m.step_to(t + 1)
    cost = 0
    if veto:
        tgt = liq_price_target(m, pos) * 99 // 100
        d = move_size(m.venue, tgt)
        if d < 0:
            got = m.venue.sell(-d)
            m.observe(t + 1)
            cost = m.venue.buy(-d) - got
    m.step_to(t + 2)
    st = m.status(pos, t + 2)
    return m.try_exit(pos, t + 2, settle_only=True), cost, st


def row(label, name, r, cost, secs, honest):
    if r is None:
        return f"| {label} | {name} | none within the horizon | | | | | {sol(cost):.3f} |"
    fv = f"{sol(r['fv']):.3f}" if r.get("fv") else "-"
    return (f"| {label} | {name} | {r['kind']} | {secs} | {fv} | {sol(r['lender_sol']):.3f} | "
            f"{sol(r['lender_sol'] - honest['lender_sol']):+.3f} | {sol(cost):.3f} |")


HDR = ("| rule | attack | outcome | settles at (s after the first push) | FV (SOL) | lenders get (SOL) | "
       "change for lenders vs honest (SOL) | attacker cost (SOL) |\n|---|---|---|---|---|---|---|---|")

if __name__ == "__main__":
    print("=== (M) model case, RA 97 % drain x1.2 (ECON §8.6), and the reviewer's S2 -> S1 flip cases ===")
    for key, drain, k in (("RA", 9700, 1.2), ("RA", 9700, 1.3), ("RA", 9500, 1.4)):
        print(f"\n#### {key} {drain/100:.0f}% x{k}\n")
        print(HDR)
        for label, S, P in (("v0.3 (FV gate on S2 only, kind from instantaneous legs)", V03, V03.DEFAULT),
                            ("v0.4.1 (settlement box, fv_dev 1 %)", sys.modules["shortwin_sim"], DEFAULT)):
            m, pos, t0, honest = ready_model(S, key, drain, k, P)
            print(f"| {label} | honest | {honest['kind']} | 0 | {sol(honest['fv']):.3f} | "
                  f"{sol(honest['lender_sol']):.3f} | 0 | 0 |")
            r, cost, secs = attack_up(m, pos, t0)
            print(row(label, "lender pushes up (1 s)", r, cost, secs, honest))
            for n in (1, 2):
                r, cost, secs = attack_flip(m, pos, t0, n)
                print(row(label, f"S2 -> S1 flip, {n} pushed s + [pump, sol_settle, dump]", r, cost, secs, honest))
            r, cost, secs, pushes = attack_down(m, pos, t0)
            print(row(label, "borrower pushes down (1 s)", r, cost, secs, honest))
            r, cost, secs, pushes = attack_down(m, pos, t0, repeat=True)
            print(row(label, f"borrower pushes down whenever settlement is due ({pushes} pushes in 600 s)",
                      r, cost, secs, honest))
    print("\n=== (M) push-then-freeze (R4B-2): 1 attacker-only push + k frozen seconds + [pump, sol_settle, dump] ===")
    print("| case | rule | honest | k frozen s | outcome | lenders (SOL) | vs honest (SOL) | cost (SOL) |")
    print("|---|---|---|---|---|---|---|---|")
    for key, drain, k in (("RA", 9700, 1.2), ("RA", 9700, 1.3), ("RA", 9500, 1.4)):
        for label, S, P in (("v0.4 (box on clamped obs)", V04, V04.replace(V04.DEFAULT, box_obs=True)),
                            ("v0.4.1 (box on bucket extremes)", V04, DEFAULT)):
            m, pos, t0, honest = ready_model(S, key, drain, k, P)
            for kf in (0, 2, 3, 4, 5, 6, 8, 10):
                r, cost, secs = attack_ptf(m, pos, t0, kf)
                out = "none" if r is None else r["kind"]
                ls = "" if r is None else f"{sol(r['lender_sol']):.3f}"
                dv = "" if r is None else f"{sol(r['lender_sol'] - honest['lender_sol']):+.3f}"
                print(f"| {key} {drain/100:.0f}% x{k} | {label} | {honest['kind']} {sol(honest['lender_sol']):.3f} | "
                      f"{kf} | {out} | {ls} | {dv} | {sol(cost):.3f} |")
    print("\n=== (M) consecutive attacker-only pushes (R4A-9): n pushes, then [pump, sol_settle, dump] ===")
    print("| case | honest | n | honest s between pushes | outcome | lenders (SOL) | vs honest (SOL) | cost (SOL) |")
    print("|---|---|---|---|---|---|---|---|")
    for key, drain, k in (("RA", 9700, 1.2), ("RA", 9700, 1.3), ("RA", 9500, 1.4)):
        m, pos, t0, honest = ready_model(V04, key, drain, k, DEFAULT)
        for n in (1, 2, 3, 4, 5, 6):
            for gap in ((0,) if n == 1 else (0, 1)):
                r, cost, note = attack_flip_gap(m, pos, t0, n, gap)
                out = "none" if r is None else r["kind"] + (" (honest keeper settled first)" if note else "")
                ls = "" if r is None else f"{sol(r['lender_sol']):.3f}"
                dv = "" if r is None else f"{sol(r['lender_sol'] - honest['lender_sol']):+.3f}"
                print(f"| {key} {drain/100:.0f}% x{k} | {honest['kind']} {sol(honest['lender_sol']):.3f} | {n} | {gap} | "
                      f"{out} | {ls} | {dv} | {sol(cost):.3f} |")
    print("\n=== (V) p' veto (R4A-3): Emergency flag, expired AND CR-liquidatable at the deadline; one contrary low "
          "observation in the second before the keeper's sol_settle(S3) ===")
    print("| market | move | rule | legs at the call (spot / EMA) | honest: lenders / lump / borrower | veto: lenders / lump / borrower | veto cost (SOL) |")
    print("|---|---|---|---|---|---|---|")
    for key, mult in (("A", 1.70), ("RA", 1.62), ("C", 2.1), ("D@0%", 2.4)):
        for label, P in (("v0.4 (p' from the raw legs)", V04.replace(V04.DEFAULT, pprime_raw=True)),
                         ("v0.4.1 (p' on the box corner)", DEFAULT)):
            m, pos, t = ready_veto(key, mult, P)
            rh, _, sth = veto_run(m, pos, t, False)
            rv, cost, stv = veto_run(m, pos, t, True)
            f = lambda r: "none" if r is None else (f"{r['kind']} {sol(r['lender_sol']):.3f} / "
                                                     f"{sol(r['lender_lump']):.3f} / {sol(r['borrower']):.3f}")
            print(f"| {key} | x{mult} | {label} | {stv['spot_leg']} / {stv['ema_leg']} | {f(rh)} | {f(rv)} | {sol(cost):.3f} |")
    print("\n=== (E) realistic: Emergency route-broken flag, healthy position expires, S3 capped at FV ===")
    for key in ("A", "RA", "C"):
        print(f"\n#### {key}\n")
        print(HDR)
        for label, P, cls in (("v0.4.1 (settlement box, fv_dev 1 %)", DEFAULT, None),
                              ("v0.4.1 without the box (fv_dev 100 %)", replace(DEFAULT, fv_dev_bps=10_000), None),
                              ("rejected reading: lender-adverse corner for every decision", DEFAULT,
                               LenderAdverseCorner)):
            m, pos, t0, honest = ready_flag(key, P, cls)
            print(f"| {label} | honest | {honest['kind']} | 0 | {sol(honest['fv']):.3f} | "
                  f"{sol(honest['lender_sol']):.3f} | 0 | 0 |")
            r, cost, secs = attack_up(m, pos, t0)
            print(row(label, "lender pushes up (1 s)", r, cost, secs, honest))
            for n in (1, 2):
                r, cost, secs = attack_flip(m, pos, t0, n)
                print(row(label, f"S3 -> S1 flip, {n} pushed s + [pump, sol_settle, dump]", r, cost, secs, honest))
            r, cost, secs, pushes = attack_down(m, pos, t0)
            print(row(label, "borrower pushes down (1 s)", r, cost, secs, honest))
            r, cost, secs, pushes = attack_down(m, pos, t0, repeat=True)
            print(row(label, f"borrower pushes down whenever settlement is due ({pushes} pushes in 600 s)",
                      r, cost, secs, honest))
