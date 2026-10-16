#!/usr/bin/env python3
"""Vectors for shortwin_math::{caps, tier} against `docs/design/sim/shortwin_sim.py`
v0.4.1, default switches (ARCHITECTURE §4.6, §4.7, §9 M0b; ECONOMICS §2.1, §6).

Caps (exact, no tolerance): `max_q_impact`, `max_total_q_market` (the sims' integer form
`X − isqrt(⌊X²·10⁴/(10⁴ + ι)⌋)`, normative since v0.4.1), `Market.oi_cap_q` and the
fee-scaled caps with the one-way fee in ppm (`⌊x100·φ_ppm·X/(100·10⁶)⌋`),
`Market.conservative_x` (`isqrt⁺(P_ema)` in both denominators, the locked-depth bound on
Raydium and -- since ECONOMICS changelog #105, M6R-1 -- on a canonical PumpSwap pool, whose `D`
is its burned LP's depth; the field `raydium_lock` keeps its name and carries `(D, λ_min)`
wherever `Market.locked_depth_venue()` holds, `λ_min` = `Market.lock_min_bps()`:
`pool_lock_min_bps` on Raydium, `PUMPSWAP_LOCK_MIN_BPS` = 10,000 on PumpSwap since #106
(OWNER-M6D-1: PumpSwap credited on its burned LP only); on the curve the cap reserves at the EMA
price, ECONOMICS changelog #103), and
`Market.max_open_q` on every example market of the sim, fresh and after an EMA-lagging price
path (so `L_slow ≠ L_live`, `P_ema ≠ P_live`). Utilisation is
`pool_model.Pool.borrow`'s check; the curve-gamma, wallet and notional predicates are
ECON §6.1 / ARCH §4.6 transcriptions (the sim has no function for them).

Tiers: the sim has no tier function; `assign` below transcribes ARCHITECTURE §4.7 /
ECONOMICS §6.2–§6.3 (v0.4.1): `R_slow = Y(reprice(L_slow, P_ema)) − VQR` with the sim's
own `reprice`, and on Raydium `R_eff ≤ ⌊⌊D·isqrt(P_ema)/10⁹⌋·10⁴/λ_min_bps⌋` (ECON §2.1:
locked SOL floored first); since #105 (M6R-1) on PumpSwap `R_eff ≤ ⌊max(⌊D·isqrt(P_ema)/10⁹⌋ −
VQR⁺, 0)·10⁴/λ_min_bps⌋` (`VQR⁺ = max(VQR, 0)`: the locked SOL less the virtual reserve, real SOL
only; the PumpSwap rows' `lock` is drawn from a stream of its own, so every other row keeps its
draws) with `λ_min_bps` = `PUMPSWAP_LOCK_MIN_BPS` = 10,000 since #106 (OWNER-M6D-1), i.e.
`R_eff ≤ max(⌊D·isqrt(P_ema)/10⁹⌋ − VQR⁺, 0)`; thresholds compared exactly against the cached
SOL/USD price.

Writes caps.json next to itself.
"""
import copy
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

BPS, WAD, PPM = sim.BPS, sim.WAD, sim.PPM
U64 = 2**64 - 1
SEED = 20260928
AGE_C_S, AGE_A_S = 86_400, 604_800
S = str


def rand_x(rng):
    pick = rng.random()
    if pick < 0.1:
        return rng.choice([0, 1, 2, 3, 100, 10**6, U64 // 2, U64, U64 - 1])
    if pick < 0.2:
        return rng.randrange(U64 - 2**60, U64 + 1)      # radicand of the position cap > 2^128
    if pick < 0.4:
        return rng.randrange(0, 2**64)
    return rng.randrange(10**9, 10**16)


def gen_impact(rng):
    out = []
    for _ in range(500):
        x = rand_x(rng)
        ip = rng.choice([0, 300, 500, 1_000, rng.randrange(0, 1_001), rng.randrange(0, 2**32)])
        im = rng.choice([0, 1_000, 2_000, 5_000, rng.randrange(0, 5_001), rng.randrange(0, 2**32)])
        pos = sim.max_q_impact(x, ip)
        tot = sim.max_total_q_market(x, im)
        out.append(dict(x=S(x), impact_pos_bps=S(ip), impact_mkt_bps=S(im),
                        max_q_position=S(min(pos, U64)), max_total_q=S(tot)))
    return out


def gen_fee(rng):
    out = []
    for _ in range(300):
        x = rand_x(rng)
        # ppm rates: pump bps x 100, AMM v4 25/10,000, CPMM t + c (not a whole bp)
        phi = rng.choice([2_500, 9_000, 12_500, 15_000, 2_550, 50, rng.randrange(0, 200_001),
                          rng.randrange(0, 10**6)])
        k_pos = rng.choice([0, 100, 300, 500, rng.randrange(100, 501), rng.randrange(0, 2**32)])
        k_mkt = rng.choice([0, 300, 750, 1_000, rng.randrange(300, 1_001), rng.randrange(0, 2**32)])
        im = rng.choice([1_000, 2_000, rng.randrange(0, 5_001)])
        size = min(x * phi * k_pos // (100 * PPM), U64) if k_pos else None   # saturating: never binds
        oi = min(x * phi * k_mkt // (100 * PPM), U64) if k_mkt else None
        # Market.oi_cap_q on a venue with this one-way fee in ppm (RaydiumPool.fee_oneway_ppm)
        m = sim.Market(sim.RaydiumPool(max(x, 1), 10**12, fee_ppm=phi), sim.TIERS["A"],
                       replace(sim.DEFAULT, fee_oi_cap_x100=k_mkt))
        assert m.venue.fee_oneway_ppm() == phi
        m.tier = replace(m.tier, impact_mkt_bps=im)
        oi_sim = m.oi_cap_q(x)
        assert oi_sim == (min(sim.max_total_q_market(x, im), oi) if oi is not None
                          else sim.max_total_q_market(x, im))
        out.append(dict(x=S(x), phi_ppm=S(phi), fee_size_cap_x100=S(k_pos), fee_oi_cap_x100=S(k_mkt),
                        impact_mkt_bps=S(im), fee_size_cap=None if size is None else S(size),
                        fee_oi_cap=None if oi is None else S(oi), oi_cap=S(oi_sim)))
    return out


def gen_preds(rng):
    out = []
    for _ in range(400):
        big = rng.random() < 0.2
        t = rng.randrange(0, 2**64) if big else rng.randrange(0, 10**6)
        b = rng.randrange(0, 2**64) if big else rng.randrange(0, 10**6)
        q = rng.randrange(0, 2**64) if big else rng.randrange(0, 10**6)
        cap = rng.choice([9_000, 9_500, 0, 10_000, rng.randrange(0, 10_001)])
        # pool_model.Pool.borrow: refused iff (B + q)*BPS > util_cap*(T + B)
        util = not ((b + q) * BPS > cap * (t + b))
        sq, rt = rng.randrange(0, 10**6), rng.randrange(0, 10**6)
        g = rng.choice([5_000, 1_000, 9_000, rng.randrange(0, 10_000)])
        gamma = (sq + q) * BPS <= g * (rt + q)
        gmax = min(max(g * rt - BPS * sq, 0) // (BPS - g), U64)
        if sq == 0:
            assert gmax == rt * g // (BPS - g)          # Market.max_open_q's curve term
        qo, oi, w = rng.randrange(0, 10**6), rng.randrange(0, 10**7), rng.choice([2_500, 500, 10_000])
        wallet = (qo + q) * BPS <= w * oi
        n, tot = rng.randrange(0, 2 * 10**8), rng.randrange(0, 3 * 10**10)
        mn, mx = rng.choice([10**8, 10**7]), rng.choice([0, 25 * 10**9, 10**9])
        notional = n >= mn and (mx == 0 or tot + n <= mx)
        out.append(dict(t_free=S(t), borrowed=S(b), q=S(q), util_cap_bps=S(cap), util_ok=util,
                        sum_q=S(sq), rt=S(rt), gamma_bps=S(g), curve_gamma_ok=gamma,
                        curve_gamma_max_q=S(gmax), q_open=S(qo), oi_cap=S(oi), wallet_cap_bps=S(w),
                        wallet_ok=wallet, n_open=S(n), total_notional_open=S(tot), min_notional=S(mn),
                        market_max_notional=S(mx), notional_ok=notional))
    return out


def lock_of(m):
    """(D, lambda_min) wherever the sim's caps read the locked depth: Raydium and, since #105 (M6R-1), a canonical
    PumpSwap pool (Market.locked_depth_venue); None on the curve.  lambda_min is the market's (Market.lock_min_bps):
    pool_lock_min_bps on Raydium, PUMPSWAP_LOCK_MIN_BPS on PumpSwap since #106."""
    v = m.venue
    return [S(v.locked_depth()), S(m.lock_min_bps())] if m.locked_depth_venue() else None


def gen_conservative(rng):
    """Market.conservative_x on AMM markets whose EMA state was moved off the live pool."""
    out = []
    keys = ["A", "B", "C", "RA", "RB", "RC"]
    for i in range(300):
        key = rng.choice(keys)
        lam = rng.choice([5_000, 2_000, 10_000, rng.randrange(2_000, 10_001)])
        m = sim.new_market(key, replace(sim.DEFAULT, pool_lock_min_bps=lam))
        v = m.venue
        if v.kind == "ray" and rng.random() < 0.5:
            try:
                v.remove_liquidity(rng.randrange(1, 5_000))
            except ValueError:
                pass
        X, Y = v.eff()
        p0 = Y * WAD // X
        pick = rng.random()
        if pick < 0.1:
            m.ema = rng.choice([1, 2, 3, WAD, WAD + 1, 10**36])
        elif pick < 0.2:
            m.ema = rng.randrange(1, 10**30)
        else:
            m.ema = max(1, int(p0 * rng.uniform(0.2, 5)))
        m.liq_slow = rng.choice([m.liq, max(1, int(m.liq * rng.uniform(0.3, 1.5))), rng.randrange(1, 2**100)])
        x = m.conservative_x()
        out.append(dict(key=key, x_now=S(X), l_slow=S(m.liq_slow), p_ema=S(m.ema), raydium_lock=lock_of(m),
                        expected=S(x)))
    return out


def lagging(m, rng):
    """Move the pool for a few seconds so P_ema, L_ema and L_slow lag the live state."""
    t = 0
    p = m.venue.spot_wad()
    for _ in range(rng.randrange(3, 12)):
        t += 1
        p = int(p * rng.uniform(0.9, 1.25))
        m.advance(t)
        m.exo_move(p)
        m.observe(t)
    t += 1
    m.step_to(t)
    return m


def gen_markets(rng):
    """Every example market of the sim: the pieces of Market.max_open_q.  A curve market also records its
    virtual SOL reserve vQ, so the Rust side recomputes the curve's cap reserves (ECON #103, M5-SEC-3; ARCH
    §4.6; Market.curve_cap_reserves): X0 = min(vT, x_ema) and the Tier D guard's rT_guard whenever
    P_ema > P_spot, the live (vT, rT) otherwise."""
    out = []
    for key in sim.MARKETS:
        for lag in (False, True, True):
            m = sim.new_market(key)
            if lag:
                lagging(m, rng)
            v, T = m.venue, m.tier
            if v.kind == "curve" and v.complete:
                continue
            X0 = m.conservative_x()
            phi = v.fee_oneway_ppm()
            q_imp = sim.max_q_impact(X0, T.impact_pos_bps)
            out.append(dict(key=key, kind=v.kind, x_now=S(v.eff()[0]), l_slow=S(m.liq_slow), p_ema=S(m.ema),
                            raydium_lock=lock_of(m), x0=S(X0), phi_ppm=S(phi),
                            impact_pos_bps=S(T.impact_pos_bps), impact_mkt_bps=S(T.impact_mkt_bps),
                            max_q_position=S(q_imp), oi_cap_at=S(X0 + q_imp), oi_cap=S(m.oi_cap_q(X0 + q_imp)),
                            fee_size_cap=S(X0 * phi * m.P.fee_size_cap_x100 // (100 * PPM)),
                            fee_size_cap_x100=S(m.P.fee_size_cap_x100), fee_oi_cap_x100=S(m.P.fee_oi_cap_x100),
                            gamma_bps=S(m.P.curve_oi_gamma_bps),
                            max_open_q=S(m.max_open_q()), rt=S(v.rt) if v.kind == "curve" else None,
                            vq=S(v.vs) if v.kind == "curve" else None))
    assert len(out) >= 30
    return out


# ---- ARCHITECTURE §4.7 transcription (v0.4.1) ---------------------------------------

def below(lam, price, expo, theta_e6):
    shift = expo + 6
    if abs(shift) > 30:
        return None
    if shift >= 0:
        return lam * price * 10**shift < theta_e6 * 10**9
    return lam * price < theta_e6 * 10**9 * 10**(-shift)


def assign(c):
    if c["venue"] == "curve":
        return "D", None
    r_slow = max(c["y_slow"] - c["vqr"], 0)
    r_eff = min(c["r_now"], r_slow)
    if c["venue"] in ("raydium", "pumpswap"):
        d, lam_min = c["lock"]
        if lam_min == 0:
            return "C", None
        locked = d * math.isqrt(c["p_ema"]) // sim.SQRT_WAD          # ECON §2.1: floor first
        if c["venue"] == "pumpswap":
            locked = max(locked - max(c["vqr"], 0), 0)              # #105: less VQR+ (real SOL only, Q19)
        r_eff = min(r_eff, locked * BPS // lam_min)
    lam = 2 * r_eff
    stale = c["now"] - c["publish_time"] > c["oracle_stale_s"]
    if c["price"] <= 0 or stale or c["graduated_at"] <= 0:
        return "C", lam
    age = c["now"] - c["graduated_at"]
    bb, ba = below(lam, c["price"], c["expo"], c["theta_b"]), below(lam, c["price"], c["expo"], c["theta_a"])
    if bb is None:
        return "C", lam
    if age < AGE_C_S or bb:
        return "C", lam
    if not ba and age >= AGE_A_S:
        return "A", lam
    return "B", lam


def gen_tiers(rng):
    cases = []
    now = 1_790_000_000
    for i in range(600):
        venue = rng.choice(["pumpswap", "pumpswap", "raydium", "raydium", "curve"])
        price = rng.choice([12_120_000_000, 20_000_000_000, 5_000_000_000, 0, -1, rng.randrange(1, 10**12)])
        expo = rng.choice([-8, -8, -8, -6, -10, 0, 2, -40, 30])
        sol_usd = price * 10.0**expo if price > 0 else 1.0
        theta_a = rng.choice([500_000_000_000, rng.randrange(50_000_000_000, 50_000_000_000_000)])
        theta_b = rng.choice([100_000_000_000, rng.randrange(10_000_000_000, theta_a)])
        # Liquidity around the thresholds.
        th = rng.choice([theta_a, theta_b]) / 1e6 / max(sol_usd, 1e-9) * 1e9 / 2
        r_now = max(0, min(U64, int(th * rng.choice([0.5, 0.999, 1.0, 1.001, 2.0, 1.0]))))
        if i % 7 == 0:
            r_now = rng.randrange(0, 2**64)
        p_ema = rng.choice([WAD, rng.randrange(10**6, 10**24), rng.randrange(1, 10**30)])
        vqr = 0 if venue == "raydium" else rng.choice([0, 0, rng.randrange(0, 10**11)])
        # L_slow such that Y(reprice(L_slow, P_ema)) lands around R_now + VQR.
        target = r_now + vqr + rng.choice([0, 0, rng.randrange(0, 10**12)])
        l_slow = max(1, target * sim.SQRT_WAD // max(sim.isqrt_ceil(p_ema), 1) + rng.choice([0, 1, -1]))
        st = sim.reprice(l_slow, p_ema)
        if st is None:
            l_slow, st = 10**12, sim.reprice(10**12, p_ema)
        y_slow = st[1]
        age = rng.choice([0, AGE_C_S - 1, AGE_C_S, AGE_A_S - 1, AGE_A_S, 10 * AGE_A_S, rng.randrange(0, 10**7)])
        grad = now - age if rng.random() > 0.05 else rng.choice([0, -5])
        pub = now - rng.choice([0, 30, 3_600, 3_601, 100_000, -20])
        c = dict(venue=venue, r_now=r_now, l_slow=l_slow, y_slow=y_slow, vqr=vqr, p_ema=p_ema,
                 now=now, graduated_at=grad, price=price, expo=expo, publish_time=pub,
                 oracle_stale_s=rng.choice([3_600, 600, 86_400]), theta_a=theta_a, theta_b=theta_b,
                 lock=[rng.randrange(0, 10**16), rng.choice([5_000, 2_000, 10_000, 3_000, 0])]
                 if venue == "raydium" else None)
        if venue == "raydium" and rng.random() < 0.7:
            # Locked SOL over lambda_min around R_now (below, at, above it).
            lam_min = c["lock"][1] or 5_000
            num, den = rng.choice([(1, 2), (9, 10), (1, 1), (1, 1), (2, 1)])
            c["lock"][0] = r_now * lam_min * sim.SQRT_WAD * num // (den * BPS * max(math.isqrt(p_ema), 1)) \
                + rng.choice([0, 1, 7])
        if venue == "pumpswap":
            # #105 (M6R-1): a canonical pool's locked depth, from a stream of its own (the other rows keep their
            # draws); its locked SOL less VQR over lambda_min around R_now, or far above (the bound idle).  #106
            # (OWNER-M6D-1): PumpSwap's lambda_min is PUMPSWAP_LOCK_MIN_BPS (100 %), whatever pool_lock_min_bps is
            lr = random.Random(f"pumpswap-lock-{i}")
            lam_min = sim.PUMPSWAP_LOCK_MIN_BPS
            if lr.random() < 0.7:
                num, den = lr.choice([(1, 2), (9, 10), (1, 1), (1, 1), (2, 1)])
                locked = r_now * lam_min * num // (den * BPS) + vqr
                d = locked * sim.SQRT_WAD // max(math.isqrt(p_ema), 1) + lr.choice([0, 1, 7])
            else:
                d = lr.choice([0, lr.randrange(0, 10**16), 2**100])
            c["lock"] = [d, lam_min]
        cases.append(tier_row(c))
    assert {c["tier"] for c in cases} == {"A", "B", "C", "D"}
    cases += stale_boundary_rows(now)
    return cases


def tier_row(c):
    tier, lam = assign(c)
    row = {k: (S(v) if isinstance(v, int) and not isinstance(v, bool) else v) for k, v in c.items()}
    if c["lock"] is not None:
        row["lock"] = [S(x) for x in c["lock"]]
    row["tier"] = tier
    row["liquidity"] = None if lam is None else S(lam)
    return row


def stale_boundary_rows(now):
    """OWNER-M7-15, decided 2026-10-01 (SPEC §14): `oracle_stale_s` defaults to 600 s, its floor. Its
    boundary at that value -- a cache exactly 600 s old tiers as fresh, 601 s old is Tier C -- on an aged
    Raydium and PumpSwap market whose liquidity is well above theta_A (Tier A when fresh) or between theta_B
    and theta_A (Tier B). A grid of its own, appended after the random rows (whose draws it leaves alone)."""
    price, expo, theta_a, theta_b = 12_120_000_000, -8, 500_000_000_000, 100_000_000_000
    sol_usd = price * 10.0**expo
    rows = []
    for venue in ("raydium", "pumpswap"):
        for theta, mult in ((theta_a, 3), (theta_b, 3)):
            # R_now = 3 x theta's half-liquidity (Lambda = 2 R_eff): 1.5 x theta_A (A) or 1.5 x theta_B (B).
            r_now = int(theta / 1e6 / sol_usd * 1e9 / 2) * mult
            p_ema = WAD
            l_slow = r_now * sim.SQRT_WAD // sim.isqrt_ceil(p_ema)
            y_slow = sim.reprice(l_slow, p_ema)[1]
            lam_min = 5_000 if venue == "raydium" else sim.PUMPSWAP_LOCK_MIN_BPS
            depth = 10 * r_now * sim.SQRT_WAD // max(math.isqrt(p_ema), 1)     # locked SOL 10 x R_now: idle
            for age_of_print in (0, 30, 300, 599, 600, 601, 3_600):
                c = dict(venue=venue, r_now=r_now, l_slow=l_slow, y_slow=y_slow, vqr=0, p_ema=p_ema, now=now,
                         graduated_at=now - 10 * AGE_A_S, price=price, expo=expo, publish_time=now - age_of_print,
                         oracle_stale_s=600, theta_a=theta_a, theta_b=theta_b, lock=[depth, lam_min])
                rows.append(tier_row(c))
    fresh = {r["tier"] for r in rows if int(r["now"]) - int(r["publish_time"]) <= 600}
    stale = {r["tier"] for r in rows if int(r["now"]) - int(r["publish_time"]) > 600}
    assert fresh == {"A", "B"} and stale == {"C"}, (fresh, stale)
    return rows


def main() -> None:
    rng = random.Random(SEED)
    out = {
        "generator": Path(__file__).name,
        "source": "docs/design/sim/shortwin_sim.py v0.4.1 (max_q_impact, max_total_q_market, Market.oi_cap_q, "
                  "Market.max_open_q, Market.conservative_x, Market.curve_cap_reserves (#103), reprice), pool_model.py v0.4.1 (Pool.borrow "
                  "utilisation check), ECONOMICS.md v0.4.1 §2.1, §6 and ARCHITECTURE.md v0.4.1 §4.6-§4.7 "
                  "(transcribed where the sims have no function)",
        "impact": gen_impact(rng),
        "fee": gen_fee(rng),
        "predicates": gen_preds(rng),
        "conservative_x0": gen_conservative(rng),
        "markets": gen_markets(rng),
        "tiers": gen_tiers(rng),
    }
    Path(__file__).with_name("caps.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")
    print({k: len(v) for k, v in out.items() if isinstance(v, list)})


if __name__ == "__main__":
    main()
