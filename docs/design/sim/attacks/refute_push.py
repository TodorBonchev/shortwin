"""R3A-6 (round 3): cheaper resets of the settlement clock.

(a) Refutation push (reviewer's r3_refute_push.py).  A genuine gap puts a position x % beyond
    the S1 boundary; the keeper cranks and calls sol_settle every second.  One second before the
    clock would mature, the borrower's atomic [dump to 0.7x -> crank -> buy back] is the only
    observation of that second.  v0.3 refuted on the instantaneous P_ema (-8.3 % in one step),
    so it disarmed and the 40-update clock restarted, repeatably.  v0.4 refutes only on the
    whole settlement box (EMA state AND ring-median state), which one second cannot move.
(b) Spot-leg veto (reviewer's r3_spotblock_arm.py).  One low observation per second (below the
    liquidation price) fails the robust-spot leg; v0.3 returned before refreshing s_last, so a
    block longer than settle_arm_gap_s (10 s) restarted the clock.  v0.4 keeps an armed clock
    running on every call that is not a box-robust refutation.
(c) Push-then-freeze refutation (R4B-2, reviewer's ptf_refute2.py).  One attacker-only dump
    second, then k seconds in which the honest keeper observes AND the borrower adds an
    observation just below P_ema (the bucket straddles P_ema, so the clamped obs = P_ema): v0.4's
    median of the clamped obs moved onto the dumped EMA after 3-4 frozen seconds and refuted,
    every cycle.  v0.4.1 takes the box medians over the bucket extremes: a frozen second keeps its
    honest high, so the refutation corner cannot move.
(d) Widening (v0.4.1 residual).  A contrary observation EVERY second (a paid round trip, or on
    Raydium an LP add / remove at no swap fee) widens the box on one side; it cannot flip a
    decision, but it defers S1 (like the V1-2 spot-leg veto) and an S3 amount on an executable
    venue for as long as it is paid.
No arbitrage; costs are round-trip venue fees.
"""
import sys
from common import *
from block_spot import liq_price_target

V04 = sys.modules["shortwin_sim"]
V03 = legacy_sim()


def s1_boundary(m, pos):
    K = m.settle_reward(pos.V)
    ert = getattr(pos, "ert", m.P.exec_rent_tol)
    lo, hi = 1.0, 20.0
    for _ in range(50):
        mid = (lo + hi) / 2
        v = copy.deepcopy(m.venue)
        v.move_to_price(int(v.spot_wad() * mid))
        q = v.quote_at(pos.Q, *v.eff())
        if q is None or q[1] + ert > pos.V - K:
            hi = mid
        else:
            lo = mid
    return hi


def mature_soon(S, m, pos, t, lead=1):
    P = m.P
    if pos.s_kind is None:
        return False
    need_s = getattr(P, "settle_persist_s", 0)
    return (t + lead - pos.s_t >= need_s) and (m.ema_count + lead - pos.s_count >= P.settle_persist_n)


def refute_run(S, key, beyond, attack, horizon=600, cls=None):
    m = market_variant(cls, key) if cls else S.new_market(key)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    kb = s1_boundary(m, pos)
    p0 = m.venue.spot_wad()
    m.exo_move(int(p0 * kb * beyond))
    t_gap = t
    cost, pushes, last_push = 0, 0, None
    for _ in range(horizon):
        t += 1
        if attack and mature_soon(S, m, pos, t) and last_push != pos.s_t:
            last_push = pos.s_t
            m.advance(t)
            cost += attacker_bundle(m, t, int(m.venue.spot_wad() * 70 // 100))    # alone in its second
            pushes += 1
            continue
        m.step_to(t)
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r["kind"], t - t_gap, pushes, cost
    return "none", horizon, pushes, cost


def spotblock_run(S, key, block_len, beyond=1.05, start_after=20):
    m = S.new_market(key)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    kb = s1_boundary(m, pos)
    p0 = m.venue.spot_wad()
    m.exo_move(int(p0 * kb * beyond))
    t_gap = t
    cost = blocked = 0
    armed_at = None
    for _ in range(400):
        t += 1
        if pos.s_kind is not None and armed_at is None:
            armed_at = t
        if armed_at is not None and t - armed_at >= start_after and blocked < block_len:
            m.step_to(t)
            tgt = liq_price_target(m, pos) * 995 // 1000
            d = move_size(m.venue, tgt)
            if d < 0:
                got = m.venue.sell(-d)
                m.observe(t)
                cost += m.venue.buy(-d) - got
            m.try_exit(pos, t, settle_only=True)
            blocked += 1
            continue
        m.step_to(t)
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r["kind"], t - t_gap, cost
    return "none", None, cost


def ptf_refute_run(S, key, beyond, attack, P=None, horizon=600, lead=3, n_freeze=4, dump=0.7):
    m = S.new_market(key, P or S.DEFAULT)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    kb = s1_boundary(m, pos)
    m.exo_move(int(m.venue.spot_wad() * kb * beyond))
    t_gap = t
    cost = pushes = frz = 0
    freeze_left = 0
    last = None
    for _ in range(horizon):
        t += 1
        if attack and freeze_left == 0 and mature_soon(S, m, pos, t, lead=lead) and last != pos.s_t:
            last = pos.s_t
            m.advance(t)
            cost += attacker_bundle(m, t, int(m.venue.spot_wad() * dump))     # attacker-only second
            pushes += 1
            freeze_left = n_freeze
            continue
        m.step_to(t)                                                         # honest keeper observation
        if attack and freeze_left > 0:
            cost += attacker_bundle(m, t, m.ema * (BPS - 5) // BPS)            # freeze bundle
            frz += 1
            freeze_left -= 1
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r["kind"], t - t_gap, pushes, frz, cost
    return "none", horizon, pushes, frz, cost


def lp_widen(m, t):
    v = m.venue
    v.add_liquidity(10_000)                          # double L for one observation, no swap fee
    m.observe(t)
    v.remove_liquidity(5_000)


def widen_s1(key, beyond, side, horizon=300, pct=100):
    m = new_market(key)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 120)
    kb = s1_boundary(m, pos)
    m.exo_move(int(m.venue.spot_wad() * kb * beyond))
    t_gap, cost = t, 0
    for _ in range(horizon):
        t += 1
        m.step_to(t)
        if side == "L":
            lp_widen(m, t)
        elif side == "P":
            cost += attacker_bundle(m, t, m.venue.spot_wad() * (BPS - pct) // BPS)
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r["kind"], t - t_gap, cost
    return "none", None, cost


def widen_s3(key, side, pct=100, horizon=300, lead=12):
    """Emergency flag set 30 min before a healthy position's deadline; the widening starts `lead`
    seconds before the deadline (5 of the ring's 8 entries must be widened)."""
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
    honest = None
    cost = 0
    for _ in range(horizon):
        t += 1
        m.step_to(t)
        if side == "L":
            lp_widen(m, t)
        elif side == "P":
            cost += attacker_bundle(m, t, m.venue.spot_wad() * (BPS - pct) // BPS)
        r = m.try_exit(pos, t, settle_only=True)
        if r:
            return r, t - pos.deadline, cost, honest
    return None, None, cost, honest


if __name__ == "__main__":
    print("=== (a) borrower's attacker-only dump 1 s before the settlement clock matures, every cycle ===")
    print("| market | beyond S1 boundary | rule | attack | outcome | settled after (s) | attacker-only seconds | cost (SOL) |")
    print("|---|---|---|---|---|---|---|---|")
    for key in ("A", "B", "C", "D@0%", "RA", "RB", "RC"):
        for beyond in (1.03, 1.05, 1.08):
            for label, S, cls in (("v0.3", V03, None), ("v0.4.1", V04, None),
                                  ("rejected reading: lender-adverse corner", V04, LenderAdverseCorner)):
                for attack in (False, True):
                    if not attack and label != "v0.4.1":
                        continue
                    kind, secs, n, cost = refute_run(S, key, beyond, attack, cls=cls)
                    print(f"| {key} | x{beyond:.2f} | {label} | {'yes' if attack else 'no'} | {kind} | "
                          f"{secs if kind != 'none' else '> 600'} | {n} | {sol(cost):.3f} |")
    print("\n=== (c) push-then-freeze refutation (R4B-2): 1 dump second (x0.7) + 4 frozen seconds, 3 s before maturity, every cycle ===")
    print("| market | beyond S1 boundary | rule | attack | outcome | settled after (s) | attacker-only s | frozen s | cost (SOL) |")
    print("|---|---|---|---|---|---|---|---|---|")
    for key in ("A", "C", "D@0%", "RA", "RC"):
        for beyond in (1.03, 1.05):
            for label, P in (("v0.4 (box on clamped obs)", replace(DEFAULT, box_obs=True)),
                             ("v0.4.1 (box on bucket extremes)", DEFAULT)):
                for attack in (False, True):
                    if not attack and label.startswith("v0.4 "):
                        continue
                    k, secs, n, f, c = ptf_refute_run(V04, key, beyond, attack, P)
                    print(f"| {key} | x{beyond:.2f} | {label} | {'PTF' if attack else 'no'} | {k} | "
                          f"{secs if k != 'none' else '> 600'} | {n} | {f} | {sol(c):.3f} |")
    print("\n=== (d) widening: a contrary observation every second (v0.4.1 residual; deferral only) ===")
    print("| market | case | widening | outcome | at (s) | cost per second (SOL) |")
    print("|---|---|---|---|---|---|")
    for key in ("A", "C", "RA", "RC"):
        for side, pct, lab in (("none", 0, "none"), ("P", 100, "round trip -1 %"), ("P", 300, "round trip -3 %"),
                               ("L", 0, "LP add/remove x2 L (Raydium)")):
            if side == "L" and key in ("A", "C"):
                continue
            k, secs, c = widen_s1(key, 1.02, side, pct=pct)
            print(f"| {key} | S1, 2 % beyond the boundary | {lab} | {k} | {secs} | {sol(c)/max(secs or 300, 1):.4f} |")
            r, secs, c, h = widen_s3(key, side, pct=pct)
            print(f"| {key} | S3 on an executable venue (Emergency flag), due at the deadline | {lab} | "
                  f"{r['kind'] if r else 'none in 300 s'} | {'' if r is None else secs} | {sol(c)/(12 + (secs if r else 300)):.4f} |")
    print("\n=== (b) spot-leg veto for w seconds, starting 20 s after arming ===")
    print("| market | w (s) | rule | outcome | settled after (s) | cost (SOL) |")
    print("|---|---|---|---|---|---|")
    for key in ("A", "C", "D@0%", "RA"):
        for bl in (0, 9, 12, 20):
            for label, S in (("v0.3", V03), ("v0.4.1", V04)):
                k, secs, c = spotblock_run(S, key, bl)
                print(f"| {key} | {bl} | {label} | {k} | {secs} | {sol(c):.3f} |")
