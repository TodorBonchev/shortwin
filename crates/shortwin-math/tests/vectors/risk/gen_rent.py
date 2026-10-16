#!/usr/bin/env python3
"""Vectors for shortwin_math::rent from the v0.4.1 sims (ECONOMICS §2.1, §7; ARCHITECTURE §4.5).

Every expected value comes from a v0.4.1 reference model with its default switches, and
the crate must match it exactly (no tolerance):

  * `paths`: `shortwin_sim.Market.accrue` drives the three rent basis indices J0, J1, J2
    (P~ = min(P_ema, P_ACC_MAX) before the roll; prices above P_ACC_MAX included) over
    random steps (dt, P_ema, U). Each position is a `shortwin_sim.Position` with its own
    locked rate curve, `p_rent` (the per-position rent_protocol_bps snapshot, R3A-5) and
    R_min; `Market.rent_accrued` / `Market.rent_due` give its rent at its close. The
    continuous lender credit of each accrual and each position's true-up are ECON §2.1 /
    §7's forms (A_j = sum q*coef_j*(1e4 - p_pos), floor with the remainder carried into the
    next accrual since AIR-03, as pool_model.Pool.advance's credit_rem: `lender_credit_rem`
    after each step, left as it was by a credit beyond u64; credited_for, ceil), written
    here in exact integers because shortwin_sim.py does not credit lenders; `receivable`
    follows ARCH §4.5's clamp, and ECON I-P7 is asserted (receivable and A_j return to 0
    once every position has closed). Positions open at time `open_step` (before step
    `open_step`) and close at time `close_step`; at each time opens come before closes.
    A credit, true-up or receivable beyond u64 is refused by the program (`Overflow`):
    that value is null, and `receivable` is null from there on.
  * `pool_trace`: `pool_model.Pool` (v0.4.1, continuous mode) under random opens with
    governance changes of rent_protocol_bps (new positions only), accruals and closes
    (repay / settle): its `receivable` after every operation, each accrual's credit and
    remainder (`credit_rem`, in the three-index denominator) and each close's
    `_credited_for`. pool_model keeps one index; with every position on a base-only curve
    (coef = (c, 0, 0)) and accruals whose c*dJ0 is a multiple of 1e4*YEAR,
    `d_index = c*dJ0/(1e4*YEAR)` makes its formulas the three-index ones (asserted here, its
    remainder times 1e4*YEAR too), so the Rust replay (rent::accrue, Aggr,
    lender_credit_carried, credited_for) must reproduce pool_model's receivable exactly.
  * `min_rent`: `Market.open`'s R_min expression (the exact kinked rate).
  * `rate`: the sims' display-only `rate_bps` (each slope term floored).
  * `utilization`: ECON §2.1's U = ceil(B_Q*1e4/(T + B_Q)).

Writes rent.json next to itself.
"""
import json
import math
import random
import sys
from dataclasses import replace
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "docs" / "design" / "sim"))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)
import pool_model  # noqa: E402  (v0.4.1)

WAD, BPS, YEAR_S = sim.WAD, sim.BPS, sim.YEAR_S
U64 = 2**64 - 1
U128 = 2**128 - 1
RENT_DEN = BPS * YEAR_S * WAD
RENT_DEN_BPS = BPS * RENT_DEN
SEED = 20260927

assert sim.P_ACC_MAX == 10**29 and pool_model.WAD == WAD and pool_model.BPS == BPS


def cdiv(a, b):
    return -((-a) // b)


def s(x):
    return str(x)


def opt64(x):
    return str(x) if 0 <= x <= U64 else None


TIER_TERMS = {k: [t.rate_base_bps, t.rate_s1_bps, t.rate_s2_bps] for k, t in sim.TIERS.items()}


def sim_tier(terms):
    return replace(sim.TIERS["A"], rate_base_bps=terms[0], rate_s1_bps=terms[1], rate_s2_bps=terms[2])


def rand_price(rng, extreme):
    pick = rng.random()
    if pick < 0.05:
        return rng.choice([1, 2, WAD, 10**6] + ([sim.P_ACC_MAX, sim.P_ACC_MAX + 1] if extreme else []))
    if pick < 0.2 and extreme:
        return rng.randrange(sim.P_ACC_MAX, 10**33)        # clamped to P_ACC_MAX
    if pick < 0.2:
        return rng.randrange(1, 10**12)
    return rng.randrange(10**12, 10**24)


def rand_util(rng):
    if rng.random() < 0.3:
        return rng.choice([0, 16, 4_000, 8_000, 8_016, 8_992, 9_000, 10_000])
    return rng.randrange(0, 10_001)


def rand_terms(rng):
    if rng.random() < 0.8:
        return list(TIER_TERMS[rng.choice("ABCD")])
    return [rng.randrange(0, 50_001), rng.randrange(0, 1_000_001), rng.randrange(0, 1_000_001)]


def rand_q(rng):
    if rng.random() < 0.1:
        return rng.choice([1, 2, 10**6])
    return rng.randrange(10**6, 10**15)


def rand_p_rent(rng):
    return rng.choice([1_500, 1_500, 0, 1_000, 3_000, rng.randrange(0, 3_001), 10_000])


def gen_paths(rng):
    paths = []
    for _ in range(48):
        extreme = rng.random() < 0.3                       # prices at and above P_ACC_MAX
        kink = rng.choice([8_000, 8_000, 8_000, 0, 5_000, 9_000, 9_500, rng.randrange(0, 9_501)])
        n = rng.randrange(1, 30)
        steps = [(rng.choice([0, 1, 1, 2, 5, 30, 60, 600, 3_600, 86_400, rng.randrange(0, 2_592_001)]),
                  rand_price(rng, extreme), rand_util(rng)) for _ in range(n)]
        positions = []
        for _ in range(rng.randrange(1, 6)):
            o = rng.randrange(0, n + 1)
            positions.append(dict(q=rand_q(rng), terms=rand_terms(rng), p_rent=rand_p_rent(rng), open=o,
                                  close=rng.randrange(o, n + 1),
                                  r_min=rng.choice([0, 0, 1_000, rng.randrange(0, 10**9)])))

        # The sim's market, driven step by step (Market.accrue clamps at P_ACC_MAX).
        m = sim.Market(sim.venue_tier_A(), sim.TIERS["A"], replace(sim.DEFAULT, util_kink_bps=kink), util_bps=0)
        st = dict(receivable=0, A=[0, 0, 0], open={}, rem=0)
        t = m.index_t

        def at_time(k):
            """Time k (before step k): opens, then closes (true-up, the position's rent)."""
            for i, ps in enumerate(positions):
                if ps["open"] == k:
                    st["open"][i] = sim.Position(Q=ps["q"], J_open=tuple(m.J), coef=tuple(ps["terms"]),
                                                 p_rent=ps["p_rent"], R_min=ps["r_min"])
                    for j in range(3):
                        st["A"][j] += ps["q"] * ps["terms"][j] * (BPS - ps["p_rent"])
            for i, ps in enumerate(positions):
                if ps["close"] == k:
                    pos = st["open"].pop(i)
                    dj = [m.J[j] - pos.J_open[j] for j in range(3)]
                    integ = ps["q"] * sum(c * x for c, x in zip(ps["terms"], dj))
                    accrued = m.rent_accrued(pos)
                    assert accrued == cdiv(integ, RENT_DEN)            # ECON §7 == the sim's form
                    due = m.rent_due(pos)
                    credited = cdiv(integ * (BPS - ps["p_rent"]), RENT_DEN_BPS)
                    if credited > U64:
                        st["receivable"] = None                 # credited_for: Overflow
                    elif st["receivable"] is not None:
                        st["receivable"] -= min(credited, st["receivable"])
                    for j in range(3):
                        st["A"][j] -= ps["q"] * ps["terms"][j] * (BPS - ps["p_rent"])
                    ps.update(j_open=[s(x) for x in pos.J_open], accrued=opt64(accrued),
                              due=opt64(due) if accrued <= U64 else None, credited_for=opt64(credited))

        at_time(0)
        out_steps = []
        for i, (dt, p, u) in enumerate(steps):
            m.ema, m.util_bps = p, u
            t += dt
            before = list(m.J)
            m.accrue(t)
            assert max(m.J) <= U128
            d = [a - b for a, b in zip(m.J, before)]
            aggr = list(st["A"])
            # AIR-03: the market's remainder carried in; a credit beyond u64 leaves it unchanged
            credit, rem = divmod(sum(a * x for a, x in zip(aggr, d)) + st["rem"], RENT_DEN_BPS)
            if credit <= U64:
                st["rem"] = rem
            if st["receivable"] is not None:
                st["receivable"] += credit
                if st["receivable"] > U64:                      # the u64 receivable: Overflow
                    st["receivable"] = None
            at_time(i + 1)
            rec = st["receivable"]
            out_steps.append(dict(dt=s(dt), p_ema=s(p), util_bps=s(u), delta=[s(x) for x in d],
                                  j=[s(x) for x in m.J], aggr=[s(x) for x in aggr], lender_credit=opt64(credit),
                                  lender_credit_rem=s(st["rem"]), receivable=None if rec is None else s(rec)))
        assert not st["open"] and st["A"] == [0, 0, 0]
        assert st["receivable"] in (0, None), "ECON I-P7"
        paths.append(dict(
            kink_bps=s(kink), steps=out_steps,
            positions=[dict(q=s(ps["q"]), terms=[s(x) for x in ps["terms"]], rent_protocol_bps=s(ps["p_rent"]),
                            open_step=s(ps["open"]), close_step=s(ps["close"]), r_min=s(ps["r_min"]),
                            j_open=ps["j_open"], accrued=ps["accrued"], due=ps["due"],
                            credited_for=ps["credited_for"])
                       for ps in positions]))
    return paths


def gen_pool_trace(rng):
    """pool_model's continuous crediting under governance changes of rent_protocol_bps."""
    traces = []
    for _ in range(12):
        c = rng.choice([1_000, 2_500, 5_000, 10_000, rng.randrange(1, 50_001)])
        kink = rng.choice([8_000, rng.randrange(0, 9_501)])
        p = pool_model.Pool(mode="continuous")
        p.deposit("L", 10**15)
        ops, open_ids, pid = [], [], 0
        rem = 0                       # the three-index remainder (AIR-03), = p.credit_rem * 1e4*YEAR

        def close(i):
            q, io, pr, _dl = p.positions[i]
            credited = p._credited_for(q, io, pr)
            rent = cdiv(q * (p.index - io), WAD)
            if rng.random() < 0.7:
                p.repay(i, rent, 0)
            else:
                p.settle(i, rent)
            ops.append(dict(op="close", id=s(i), credited_for=s(credited), receivable=s(p.receivable)))

        for _ in range(rng.randrange(40, 120)):
            r = rng.random()
            if r < 0.25 and len(open_ids) < 12:
                if rng.random() < 0.3:
                    p.rent_protocol_bps = rand_p_rent(rng)          # governance: new positions only
                q = rng.randrange(1, 10**13)
                p.borrow(pid, q)
                assert p.positions[pid][2] == p.rent_protocol_bps
                ops.append(dict(op="open", id=s(pid), q=s(q), rent_protocol_bps=s(p.rent_protocol_bps),
                                receivable=s(p.receivable)))
                open_ids.append(pid)
                pid += 1
            elif r < 0.75:
                dt = rng.choice([1, 2, 60, 3_600, 86_400, rng.randrange(1, 2_592_001)])
                # smallest price step with c*price*dt a multiple of 1e4*YEAR, times a random multiple
                g = (BPS * YEAR_S) // math.gcd(c * dt, BPS * YEAR_S)
                price = g * rng.randrange(1, max(2, 10**24 // g))
                assert price <= sim.P_ACC_MAX
                d0 = price * dt
                d_index = c * d0 // (BPS * YEAR_S)
                assert d_index * BPS * YEAR_S == c * d0
                before, a_pm = p.receivable, p.A
                p.advance(dt, d_index)
                credit = p.receivable - before
                want, rem = divmod((c * a_pm) * d0 + rem, RENT_DEN_BPS)  # A_0 = c*A_pm, dJ0 = price*dt
                assert credit == want and rem == p.credit_rem * BPS * YEAR_S
                ops.append(dict(op="accrue", dt=s(dt), p_ema=s(price), util_bps=s(rand_util(rng)),
                                credit=s(credit), credit_rem=s(rem), receivable=s(p.receivable)))
            elif open_ids:
                close(open_ids.pop(rng.randrange(len(open_ids))))
        while open_ids:
            close(open_ids.pop())
        assert p.receivable == 0 and p.A == 0, "ECON I-P7 (pool_model)"
        p.check()
        traces.append(dict(coef=s(c), kink_bps=s(kink), ops=ops))
    return traces


def sim_min_rent(q, p_ema, terms, u, k, min_rent_s):
    """shortwin_sim.Market.open's R_min, verbatim."""
    r_scaled = terms[0] * BPS + terms[1] * min(u, k) + terms[2] * max(u - k, 0)
    return cdiv(q * p_ema * r_scaled * min_rent_s, BPS * BPS * YEAR_S * WAD)


def gen_min_rent(rng):
    cases = []
    for _ in range(300):
        terms = rand_terms(rng)
        u, k = rand_util(rng), rng.choice([8_000, 8_000, rng.randrange(0, 9_501)])
        q = rand_q(rng)
        p = rng.randrange(1, 10**24) if rng.random() < 0.85 else rng.randrange(1, sim.P_OPEN_MAX + 1)
        mrs = rng.choice([3_600, 14_400, 0, 86_400, rng.randrange(0, 86_401)])
        cases.append(dict(q=s(q), p_ema=s(p), terms=[s(x) for x in terms], util_bps=s(u), kink_bps=s(k),
                          min_rent_s=s(mrs), expected=opt64(sim_min_rent(q, p, terms, u, k, mrs))))
    return cases


def gen_rate_util(rng):
    rates, utils = [], []
    for _ in range(300):
        terms = rand_terms(rng)
        u, k = rand_util(rng), rng.choice([8_000, rng.randrange(0, 10_001)])
        rates.append(dict(terms=[s(x) for x in terms], util_bps=s(u), kink_bps=s(k),
                          rate_bps=s(sim.rate_bps(sim_tier(terms), u, k))))
    for _ in range(200):
        t = rng.choice([0, 1, rng.randrange(0, 2**64), rng.randrange(0, 10**15)])
        b = rng.choice([0, 1, rng.randrange(0, 2**64), rng.randrange(0, 10**15)])
        utils.append(dict(t_free=s(t), borrowed=s(b), util_bps=s(cdiv(b * BPS, t + b) if t + b else 0)))
    return rates, utils


def main() -> None:
    rng = random.Random(SEED)
    paths = gen_paths(rng)
    pool_trace = gen_pool_trace(rng)
    min_rent = gen_min_rent(rng)
    rates, utils = gen_rate_util(rng)
    # Coverage: the clamp, overflowing rent and credits, several shares on one path, I-P7 checked.
    all_steps = [st for p in paths for st in p["steps"]]
    all_pos = [ps for p in paths for ps in p["positions"]]
    assert any(int(st["p_ema"]) > sim.P_ACC_MAX for st in all_steps)
    assert any(st["lender_credit"] is None for st in all_steps)
    assert sum(p["steps"][-1]["receivable"] == "0" for p in paths) >= 30
    assert any(ps["accrued"] is None for ps in all_pos) and sum(ps["accrued"] is not None for ps in all_pos) > 100
    assert any(len({ps["rent_protocol_bps"] for ps in p["positions"]}) > 2 for p in paths)
    # AIR-03: the carried remainder is exercised on both tracks.
    assert sum(st["lender_credit_rem"] != "0" for st in all_steps) > 100
    assert sum(op.get("credit_rem", "0") != "0" for tr in pool_trace for op in tr["ops"]) > 100
    out = {
        "generator": Path(__file__).name,
        "source": "docs/design/sim/shortwin_sim.py v0.4.1 (Market.accrue with P_ACC_MAX, Market.rent_accrued, "
                  "Market.rent_due, Market.open R_min, rate_bps); docs/design/sim/pool_model.py v0.4.1 "
                  "(Pool.advance with credit_rem, borrow, repay, settle, _credited_for, receivable); ECONOMICS.md "
                  "v0.4.1 §2.1 / §7 lender credit and true-up (A_j folds 1e4 - p_pos; the credit's remainder carried, "
                  "AIR-03)",
        "paths": paths,
        "pool_trace": pool_trace,
        "min_rent": min_rent,
        "rate": rates,
        "utilization": utils,
    }
    Path(__file__).with_name("rent.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
