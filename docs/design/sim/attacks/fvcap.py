"""C0-7 / D3: the fair-value cap never binds in S1; in S2/S3 FV is the EXECUTABLE buyback
cost on EMA reserves + penalty (expiry fee for expired, not CR-liquidatable positions) + rent.

v0.4.  Every drain here is a MODEL CASE: an adopted pool keeps >= lambda_min = 50 % of its LP
locked (the examples use exactly 50 %), so these drains are applied with force=True ("LP lock
violated / non-adopted pool") -- they are the only way to reach S2 at capped sizes (S2 needs a
~48x rise on PumpSwap and ~126x on a fully drained adoptable Raydium pool, ECON §8.6).  PumpSwap
canonical pools cannot be drained at all (LP burned), so the PumpSwap rows use the same pools
marked non-canonical.  Part (a): the critic's cases (70-80 % drain, then a sustained x1.6-2.0
move); part (b): S2 cases where the cap binds.  The settlement box (ECON §5.6) applies.
"""
from common import *


def market(key, noncanonical=True, P=DEFAULT):
    m = new_market(key, P)
    if noncanonical and isinstance(m.venue, CPPool):
        m.venue.canonical = False
        m.venue.lp_mutable = True
    return m


def case(key, drain_bps, k, P=DEFAULT):
    m = market(key, P=P)
    pos = m.open(m.max_open_q(), 0)
    t = warm(m, 0, 60)
    m.venue.remove_liquidity(drain_bps, force=True)
    t = warm(m, t, t + 600)                 # liquidity EMA catches up (sustained drain)
    m.exo_move(int(m.venue.spot_wad() * k))
    t = warm(m, t, t + 120)                 # EMA catches up
    r = None
    for s in range(1, 200):                 # keeper retries every second (persistence)
        t += 1
        m.step_to(t)
        r = m.try_exit(pos, t)
        if r:
            break
    return m, pos, r


if __name__ == "__main__":
    print("=== (a) critic's cases (model: LP lock violated): outcome and lenders' take ===")
    print("| market | LP drained | move | outcome | V - K_s (SOL) | FV at EMA executable cost (SOL) | lenders get (SOL) | borrower gets (SOL) | S1 lenders' take == V - K_s |")
    print("|---|---|---|---|---|---|---|---|---|")
    for key, drain in (("A", 7000), ("A", 8000), ("B", 8000), ("C", 7000), ("RA", 8000), ("RB", 8000)):
        for k in (1.6, 1.7, 1.8, 1.9, 2.0):
            m, pos, r = case(key, drain, k)
            if r is None or r["kind"] not in ("S1", "S2"):
                continue
            K = m.settle_reward(pos.V)
            st = m.status(pos, r["t"])
            fv = m.fv_at(pos, dict(st, liq=True), m.ema, m.liq)
            ok = "yes" if r["kind"] != "S1" or r["lender_sol"] == pos.V - K else "NO"
            print(f"| {key} | {drain/100:.0f}% | x{k} | {r['kind']} | {sol(pos.V-K):.3f} | {sol(fv):.3f} | "
                  f"{sol(r['lender_sol']):.3f} | {sol(r['borrower']):.3f} | {ok if r['kind']=='S1' else 'n/a (S2)'} |")
    print("\n=== (b) S2 (thin liquidity, model drains) cases: FV cap binds, excess to the borrower ===")
    print("| market | LP drained | move | outcome | V - K_s (SOL) | FV (SOL) | lenders get (SOL) | borrower gets (SOL) | lenders' take / value of Q at spot |")
    print("|---|---|---|---|---|---|---|---|---|")
    for key, drain in (("RA", 9500), ("RB", 9500), ("RC", 9500), ("A", 9500), ("C", 9500)):
        for k in (1.0, 1.2, 1.4):
            m, pos, r = case(key, drain, k)
            if r is None or r["kind"] != "S2":
                continue
            K = m.settle_reward(pos.V)
            val = pos.Q * m.venue.spot_wad() // WAD
            print(f"| {key} | {drain/100:.0f}% | x{k} | {r['kind']} | {sol(pos.V-K):.3f} | {sol(r['fv']):.3f} | "
                  f"{sol(r['lender_sol']):.3f} | {sol(r['borrower']):.3f} | {r['lender_sol']/val:.3f} |")
