#!/usr/bin/env python3
"""Differential vectors: quotes against `docs/design/sim/shortwin_sim.py` v0.4.1
(ARCHITECTURE §8.1, §9 M0b; ECONOMICS §2.1 `reprice`, §3.1), default switches only.

Every `sim*` / `buy*` / `risk*` field is the sim's own answer, which the Rust quotes must
equal exactly (no tolerance):

* live execution quotes (`Venue.quote_buy_on`, `PumpCurve.quote_at`, `RaydiumPool`), sales
  (`CPPool.sell`, `PumpCurve.sell`, `RaydiumPool.sell`);
* risk quotes at a fee hard cap (`quote_buy_on(..., cap_ppm)`): above the cap a pump
  venue charges ONE combined fee at the cap (`Venue._risk_fees`), Raydium grosses up at
  `min(rate, cap)`;
* quotes on hypothetical states through `Market.quote_repriced` / `quote_xy`: `reprice`
  (`sp = isqrt⁺(P)`, X floor, Y ceil) at the LIVE fee tier (R3B-7 b), capped at
  `venue_fee_hard_cap_bps`;
* `reprice`, `sqrt_k` (`L = isqrt(X·Y)`), `RaydiumPool.lock_bps` / `locked_depth`.

The v1.1 two-leg `B̂₂` is no longer in the sim; `sim_two_leg` transcribes ECON §3.1's
`B̂_curve(rT) + B̂_pool(q − rT)` on the sim's own primitives (`quote_buy_on`,
`CPPool.graduate`). On the pump curve each case also records `exact_*`: the same trade
executed by the sim's venue-execution methods (`PumpCurve.buy`, `CPPool.graduate`,
"mirrors pump SDK / program rounding"): the program charges `⌊q·vQ/(vT − q)⌋ + 1`
(ARCH §4.3; Rust `sol_leg::curve_charge_exact_out` equals `exact_*` on the curve),
which the quote exceeds by `0..=tolerance` lamports — a bound between quote and
execution, not a tolerance against the sim.

Since pump-final §12 (P7) the sim prices a live curve's `q > rT` as pump's v3 executes it
(`synthetic`, the default switch): `sim_v3` / `sim_risk` are `B̂_v3` (`PumpCurve.quote_at`;
Rust `CurvePricing` with `synthetic`, `quote::curve_synthetic`) on the case's would-be pool
`pool_tokens` (the live `curve_base_ata − rT`, a token donation included, on a live state;
206.9e12 on a reprojected one), and `exact_v3` is the sim's `PumpCurve.buy` (pump's
synthetic migration: `⌊⌋ + 1` on the curve part, `⌈⌉` on the pool part), at most §5.2's bound
`tolerance_v3` below it. `sim_str` / `sim_risk_str` / `exact_str` keep the straddle `B̂_str`
(a `PumpMigrating` market's states; the retired bundle). `migrating`: completed curves (an exact
or a synthetic completion, a donation before or after) — §5.4's FV quote `B̂_FV`
(`Market.fv_parts`' pool and `_migrating_price`; Rust `quote::migrating_fv`) and `B̂_str` on
reprojected ring states of the complete curve (Rust `CurvePricing` without `synthetic`).

Writes `sim.json` next to itself.
"""
import copy
import math
import random
import sys
from dataclasses import replace
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "docs" / "design" / "sim"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)
from _solana import dump  # noqa: E402

SEED = 20260927
S = str
U64 = 2**64 - 1
U128 = 2**128 - 1
CAPS = [500, 300, 2000, 125, 124, 120, 119, 100, 50, 25, 1]


def tiers_of(sched):
    return [dict(threshold=S(th), lp=S(lp), protocol=S(pr), creator=S(cr)) for th, lp, pr, cr in sched.rows]


def q_values(rng, X, n):
    """Trade sizes from dust to past the reserve."""
    out = [1, 2, X - 1, X, X + 1]
    for _ in range(n):
        r = rng.random()
        if r < 0.6:
            out.append(max(1, int(X * rng.uniform(0, 0.3))))
        elif r < 0.9:
            out.append(rng.randint(1, max(1, X // 10_000)))
        else:
            out.append(int(X * rng.uniform(0.3, 1.2)))
    return [q for q in out if 0 < q <= U64]


def pair(b):
    """A quote `(net, total)`; the Rust API is `None` when an amount does not fit `u64`."""
    return None if b is None or b[0] > U64 or b[1] > U64 else [S(b[0]), S(b[1])]


def cost(b):
    return None if b is None or b[1] > U64 else S(b[1])


def market(venue, cap_bps):
    """A sim market on `venue` whose risk quotes cap at `cap_bps` (Params.venue_fee_hard_cap_bps)."""
    return sim.Market(venue, sim.TIERS["A"], replace(sim.DEFAULT, venue_fee_hard_cap_bps=cap_bps))


def ema_points(rng, X, Y):
    """(L, P) pairs around the live state, far enough to cross fee tiers."""
    L = math.isqrt(X * Y)
    p0 = Y * sim.WAD // X
    out = []
    for k in (0.01, 0.2, 0.9, 1.0, 1.1, 5.0, 60.0):
        out.append((L, max(1, int(p0 * k))))
    out.append((max(1, L * rng.randint(1, 3) // 2), max(1, int(p0 * rng.uniform(0.5, 2)))))
    out.append((1, 1))
    return out


def pumpswap_case(rng, pool, q):
    X, Y = pool.eff()
    lp, pr, cr = pool.fees()
    cap = rng.choice(CAPS)
    buy = pool.quote_buy_on(q, X, Y) if q < X else None
    risk = pool.quote_buy_on(q, X, Y, cap_ppm=cap * 100) if q < X else None
    try:
        sell = copy.deepcopy(pool).sell(q)
    except ValueError:
        sell = None
    return dict(q=S(q), x=S(X), y=S(Y), y_real=S(pool.Y), supply=S(pool.supply),
                fees=[S(lp), S(pr), S(cr)], oneway_ppm=S(pool.fee_oneway_ppm()), cap_bps=S(cap),
                buy=pair(buy), buy_risk=pair(risk), sell=None if sell is None or sell < 0 else S(sell))


def curve_exact_str(curve, q):
    """What the retired bundle cost: the program's curve buy, or complete + migrate + buy all
    of q on the new pool (V1-9)."""
    c = copy.deepcopy(curve)
    if q <= c.rt:
        return c.buy(q)
    if c.rt > 0:
        c.buy(c.rt)
    pool = sim.CPPool.graduate(c.rs - sim.PUMP_MIGRATION_FEE)
    X, Y = pool.eff()
    if q >= X:
        return None
    return pool.quote_buy_on(q, X, Y)[1]


def curve_exact_v3(curve, q):
    """What pump's v3 charges (`PumpCurve.buy`): the curve, or past `rT` the synthetic
    migration on the would-be pool `curve_base_ata − rT` (`None`: NotEnoughTokensToBuy, or a
    reprojected state at or past completion, `rT = 0`, which pump does not trade)."""
    if curve.complete:
        return None
    try:
        return copy.deepcopy(curve).buy(q)
    except ValueError:
        return None


def v3_tolerance(state, q):
    """How far the quote may sit above pump's charge: 3 on the curve (ε₁ and two ceil'ed fees);
    past `rT` pump-final §5.2's bound `(1 + Σ⌈bᵢ/10⁴⌉) + (Δ₂ + Σ⌈Δ₂·bᵢ/10⁴⌉)`, `Δ₂ = 1 +
    ⌈q₂/(X_s − q₂)⌉` over the non-zero fee components."""
    if q <= state.rt:
        return 3
    q2, x_s = q - state.rt, state.base_ata - state.rt
    if q2 >= x_s:
        return 0
    d2 = 1 + sim.cdiv(q2, x_s - q2)
    bs = [b for b in state.fees() if b > 0]
    return (1 + sum(sim.cdiv(b, sim.BPS) for b in bs)) + (d2 + sum(sim.cdiv(d2 * b, sim.BPS) for b in bs))


def curve_two_leg_exact(curve, q):
    c = copy.deepcopy(curve)
    rt = c.rt
    if q <= rt:
        return c.buy(q)
    leg1 = c.buy(rt) if rt > 0 else 0
    pool = sim.CPPool.graduate(c.rs - sim.PUMP_MIGRATION_FEE)
    X, Y = pool.eff()
    if q - rt >= X:
        return None
    return leg1 + pool.quote_buy_on(q - rt, X, Y)[1]


def two_leg_quote(live, q, X, Y):
    """ECON §3.1 B̂₂ on the sim's primitives: B̂_curve(rT) + B̂_pool(q − rT) (v1.1 only)."""
    rt = max(X - live.off_t, 0)
    if q <= rt:
        return live.quote_buy_on(q, X, Y)
    leg1 = live.quote_buy_on(rt, X, Y) if rt > 0 else (0, 0)
    pool = sim.CPPool.graduate((Y - live.off_s) + leg1[0] - sim.PUMP_MIGRATION_FEE)
    PX, PY = pool.eff()
    if q - rt >= PX:
        return None
    leg2 = pool.quote_buy_on(q - rt, PX, PY)
    return leg1[0] + leg2[0], leg1[1] + leg2[1]


def curve_case(rng, curve, q, ema=None):
    """The live curve's quote on its own state (its would-be pool the live `curve_base_ata −
    rT`), or (ema = (L, P)) on `reprice(L, P)` through `Market.quote_repriced` (the live curve's
    fees and offsets, the standard 206.9e12): `B̂_v3` (the default `synthetic` switch) and the
    retired / migrating-state `B̂_str` (`synthetic` off)."""
    cap = rng.choice(CAPS)
    live = ema is None
    if live:
        X, Y = curve.vt, curve.vs
        state = curve
    else:
        X, Y = sim.reprice(*ema)
        rt = max(X - curve.off_t, 0)
        # rQ saturates at 0 below the curve's start, as the program's curve_reprice
        state = sim.PumpCurve(X, Y, rt, max(Y - curve.off_s, 0))
        state.off_t, state.off_s = curve.off_t, curve.off_s
    v3 = curve.quote_at(q, X, Y, live=live)
    risk = curve.quote_at(q, X, Y, cap_ppm=cap * 100, live=live)
    if not live:
        m = market(copy.deepcopy(curve), cap)
        assert m.quote_repriced(q, ema[1], ema[0]) == risk
    retired = copy.deepcopy(curve)
    retired.synthetic = False
    bstr = retired.quote_at(q, X, Y)
    risk_str = retired.quote_at(q, X, Y, cap_ppm=cap * 100)
    two = two_leg_quote(curve, q, X, Y)
    exact_v3 = curve_exact_v3(state, q)
    exact_str = curve_exact_str(state, q)
    two_exact = curve_two_leg_exact(state, q)
    sell = None
    if live:
        g = q * state.vs // (state.vt + q)
        if g <= state.rs:            # the curve pays g out of its real SOL
            sell = copy.deepcopy(state).sell(q)
    u64 = lambda x: None if x is None or x > U64 else S(x)  # noqa: E731
    return dict(
        q=S(q), vt=S(state.vt), vq=S(state.vs), rt=S(state.rt), rq=S(max(state.rs, 0)),
        off_t=S(curve.off_t), off_q=S(curve.off_s), ema=None if live else [S(ema[0]), S(ema[1])],
        fees=[S(f) for f in curve.fees()], cap_bps=S(cap),
        pool_tokens=S(curve.base_ata - curve.rt if live else sim.PUMP_POOL_TOKENS),
        sim_v3=cost(v3), sim_risk=cost(risk), sim_str=cost(bstr), sim_risk_str=cost(risk_str),
        sim_two_leg=cost(two),
        exact_v3=u64(exact_v3), exact_str=u64(exact_str), exact_two_leg=u64(two_exact),
        sell_checked=live,
        # Quote (ceil + 1) above the program's execution (floor + 1): n, then each ceil'ed fee.
        tolerance_v3=S(v3_tolerance(state, q)), tolerance=S(3 if q <= state.rt else 8),
        tolerance_two_leg=S(32),
        sell=None if sell is None or sell < 0 else S(sell),
    )


def migrating_cases(rng):
    """Completed curves (pump-final §5.4): the curve at 90 / 97 / 99.9 % completed by an exact
    buy of `rT` or by a synthetic buy past it (δ a fraction of `X_s`), a token donation to
    `curve_base_ata` before or after, then — on a `Market` over the complete curve — FV's quote
    `B̂_FV(q; p)` exactly as `Market.fv_parts` prices it (`migrate_preview`'s pool `(X₁, Y₁)`,
    `_migrating_price`'s `p'`, at the pool's tier capped), and `B̂_str` on reprojected ring
    states of the complete curve (`PumpCurve.quote_at`: all of `q` past the state's `rT` on that
    pool)."""
    fv, ring = [], []
    T = sim.PUMP_POOL_TOKENS
    for frac in (0.9, 0.97, 0.999):
        for delta_ppm, before, after in ((0, 0, 0), (0, 0, 10**13), (10_000, 0, 0), (50_000, 10**12, 0),
                                         (250_000, 0, 3 * 10**12), (500_000, T, 0), (999_000, 0, 0)):
            c = sim.PumpCurve.at_progress(frac)
            if before:
                c.donate_tokens(before)
            x_s = c.base_ata - c.rt
            c.buy(c.rt + x_s * delta_ppm // 1_000_000)
            if after:
                c.donate_tokens(after)
            assert c.complete
            pool = c.migrate_preview()
            x1, y1 = pool.eff()
            p_end = c.spot_wad()
            L = math.isqrt(c.vt * c.vs)
            for k in (0.5, 0.97, 1.0, 1.03, 2.0):
                cap = rng.choice(CAPS)
                m = market(c, cap)
                p = max(1, int(p_end * k))
                p1 = m._migrating_price(pool, p)
                st = sim.reprice(pool.sqrt_k(), p1) if p1 is not None else None
                edges = (1, x1 - 1, x1) if k == 1.0 else ()
                for q in (rng.randint(1, x1 // 100), x1 // 20) + edges:
                    b = pool.quote_buy_on(q, *st, cap_ppm=cap * 100) if st else None
                    fv.append(dict(vt=S(c.vt), vq=S(c.vs), rq=S(c.rs), pcb=S(c.pcb), pcq=S(c.pcq),
                                   pool_tokens=S(c.pool_tokens()), x1=S(x1), y1=S(y1), p=S(p),
                                   p1=None if p1 is None else S(p1), cap_bps=S(cap), q=S(q), fv=cost(b)))
                # Ring states of the complete curve: the curve at P_i reprojected (rT_i > 0 below its end).
                if k > 1.0:
                    continue
                X, Y = sim.reprice(L, p)
                rt_i = max(X - c.off_t, 0)
                for q in (max(1, rt_i // 2), max(rt_i, 1), rt_i + 10**12):
                    r = c.quote_at(q, X, Y, cap_ppm=cap * 100)
                    ring.append(dict(vt=S(c.vt), vq=S(c.vs), rq=S(c.rs), pcq=S(c.pcq),
                                     pool_tokens=S(c.pool_tokens()), off_t=S(c.off_t), off_q=S(c.off_s),
                                     l=S(L), p=S(p), fees=[S(f) for f in c.fees()], cap_bps=S(cap), q=S(q),
                                     sim_str=cost(r)))
    return fv, ring


def raydium_case(rng, pool, q):
    X, Y = pool.eff()
    cap = rng.choice(CAPS)
    buy = pool.quote_buy_on(q, X, Y) if q < X else None
    risk = pool.quote_buy_on(q, X, Y, cap_ppm=cap * 100) if q < X else None
    sell = copy.deepcopy(pool).sell(q)
    # Both Raydium programs reject a zero output (AMM v4 InvalidInput, CPMM require_gt!).
    return dict(q=S(q), x=S(X), y=S(Y), fee_ppm=S(pool.fee_ppm), creator_ppm=S(pool.creator_ppm),
                oneway_ppm=S(pool.fee_oneway_ppm()), cap_bps=S(cap),
                buy=pair(buy), buy_risk=pair(risk), sell=S(sell) if sell > 0 else None)


def repriced_cases(rng, venues):
    """Risk quotes on hypothetical states (Market.quote_repriced): live fee tier, capped."""
    out = []
    for kind, v in venues:
        X, Y = v.eff()
        for (L, P) in ema_points(rng, X, Y):
            cap = rng.choice(CAPS)
            m = market(copy.deepcopy(v), cap)
            st = sim.reprice(L, P)
            for q in (1, max(1, X // 1000), max(1, X // 20), rng.randint(1, X), X):
                r = m.quote_repriced(q, P, L)
                out.append(dict(kind=kind, x=S(X), y=S(Y), y_real=S(getattr(v, "Y", Y)),
                                supply=S(getattr(v, "supply", 0)),
                                fee_ppm=S(getattr(v, "fee_ppm", 0)), creator_ppm=S(getattr(v, "creator_ppm", 0)),
                                live_fees=[S(f) for f in v.fees()] if kind == "pumpswap" else None,
                                l=S(L), p=S(P), cap_bps=S(cap), q=S(q),
                                xy=None if st is None else [S(st[0]), S(st[1])],
                                risk=cost(r)))
    return out


def main():
    rng = random.Random(SEED)

    pumpswap = []
    pools = [sim.MARKETS["A"](), sim.MARKETS["B"](), sim.MARKETS["C"](),
             sim.CPPool.graduate(84_990_359_345)]
    for _ in range(12):
        X = rng.randint(10**12, 10**18)
        Y = rng.randint(10**9, 10**16)
        pools.append(sim.CPPool(X, Y, sim.PUMPSWAP_CANONICAL_FEES, supply=rng.choice([sim.PUMP_SUPPLY, X * 3]),
                                vq=rng.choice([0, Y // 5])))
    for p in pools:
        for q in q_values(rng, p.eff()[0], 12):
            pumpswap.append(pumpswap_case(rng, p, q))

    curve = []
    curves = []
    for frac in (0.0, 0.1, 0.5, 0.8, 0.9, 0.97, 0.999):
        c = sim.PumpCurve.at_progress(frac)
        curves.append(c)
        qs = q_values(rng, c.rt, 8) + [c.rt, c.rt + 1, c.rt * 2, sim.PUMP_POOL_TOKENS + c.rt]
        for q in qs:
            curve.append(curve_case(rng, c, q))
        L = math.isqrt(c.vt * c.vs)
        p0 = c.vs * sim.WAD // c.vt
        for k in (0.5, 0.9, 1.1, 2.0, 20.0):
            P = int(p0 * k)
            for q in (c.rt // 3 + 1, max(c.rt, 1), c.rt + 10**13):
                curve.append(curve_case(rng, c, q, ema=(L, P)))

    # pump-final §12 (P7): live curves whose curve_base_ata received a token donation (the live would-be
    # pool of B̂_v3 and of pump's synthetic buy), and completed curves (§5.4); their own random stream, so
    # every other section's cases are unchanged.
    rng2 = random.Random(SEED + 12)
    for frac in (0.5, 0.97):
        for d in (10**12, sim.PUMP_POOL_TOKENS):
            c = sim.PumpCurve.at_progress(frac)
            c.donate_tokens(d)
            x_s = c.base_ata - c.rt
            for q in (c.rt // 2, c.rt, c.rt + 1, c.rt + x_s // 3, c.rt + x_s - 1, c.rt + x_s):
                curve.append(curve_case(rng2, c, q))
    migrating, migrating_ring = migrating_cases(rng2)

    raydium = []
    rpools = [sim.MARKETS["RA"](), sim.MARKETS["RB"](), sim.MARKETS["RC"]()]
    for _ in range(10):
        rpools.append(sim.RaydiumPool(rng.randint(10**9, 10**18), rng.randint(10**9, 10**16),
                                      fee_ppm=rng.choice([50, 2_500, 2_550, 10_000, 40_000]),
                                      creator_ppm=rng.choice([0, 500, 14_950])))
    for p in rpools:
        for q in q_values(rng, p.eff()[0], 10):
            raydium.append(raydium_case(rng, p, q))

    venues = [("pumpswap", p) for p in pools[:8]] + [("raydium", p) for p in rpools[:8]]
    repriced = repriced_cases(rng, venues)
    assert any(c["risk"] is None for c in repriced) and sum(c["risk"] is not None for c in repriced) > 300

    reprice = []
    specials = [(1, 1), (1, 2), (1, 10**37), (1, 10**36), (10**9, 10**36 + 1), (2**64, 2**127), (3, U128),
                (U128, 1), (2**100, 2**120)]
    for i in range(400):
        if i < len(specials):
            L, P = specials[i]
        else:
            L = rng.choice([0, 1, rng.randint(1, 2**64), rng.randint(1, 2**100), rng.randint(1, 2**128 - 1)])
            P = rng.choice([0, 1, rng.randint(1, sim.WAD * 10**6), rng.randint(1, 2**120), rng.randint(1, U128)])
        r = sim.reprice(L, P)
        ok = r is not None and r[0] <= U128 and r[1] <= U128
        reprice.append(dict(l=S(L), p=S(P), xy=[S(r[0]), S(r[1])] if ok else None))
    assert sum(c["xy"] is None for c in reprice) > 20

    liquidity = []
    for i in range(200):
        X = rng.choice([0, 1, rng.randint(1, U64), rng.randint(1, 2**100)])
        Y = rng.choice([0, 1, rng.randint(1, 6 * 10**17), rng.randint(1, 2**64)])
        v = sim.CPPool(max(X, 1), max(Y, 1), sim.PUMPSWAP_CANONICAL_FEES)
        v.X, v.Y = X, Y
        k = v.sqrt_k() if X * Y <= U128 else None
        liquidity.append(dict(x=S(X), y=S(Y), l=None if k is None else S(k)))

    depth = []
    for i in range(160):
        X = rng.randint(10**9, 2**63)
        Y = rng.randint(10**9, 6 * 10**17)
        p = sim.RaydiumPool(X, Y, lock_bps=rng.choice([10_000, 9_978, 5_000, 2_000, rng.randint(0, 10_000)]))
        for _ in range(rng.randint(0, 3)):
            op = rng.random()
            if op < 0.4:
                p.add_liquidity(rng.randint(1, 20_000))
            elif op < 0.8:
                try:
                    p.remove_liquidity(rng.randint(1, 9_999))
                except ValueError:
                    pass
            else:
                p.buy(rng.randint(1, max(1, p.X // 10)))
        if p.X > U64 or p.Y > U64 or p.lp_units > U64:
            continue
        depth.append(dict(lp_counter=S(p.lp_units), lp_supply=S(p.lp_units - p.lp_locked), x=S(p.X), y=S(p.Y),
                          lambda_bps=S(p.lock_bps()), locked_depth=S(p.locked_depth())))
    assert len(depth) > 100

    assert sim.DEFAULT.synthetic and sim.DEFAULT.two_leg
    dump(dict(
        generator="gen_sim.py",
        source=("docs/design/sim/shortwin_sim.py v0.4.1 + pump-final P7 (default switches): CPPool, "
                "PumpCurve.quote_at/buy/sell/migrate_preview, Market.fv_parts (_migrating_price), "
                "RaydiumPool, Venue._risk_fees, Market.quote_repriced, reprice, sqrt_k, lock_bps, locked_depth"),
        pumpswap_tiers=tiers_of(sim.PUMPSWAP_CANONICAL_FEES),
        pump_migration_fee=S(sim.PUMP_MIGRATION_FEE), pump_pool_tokens=S(sim.PUMP_POOL_TOKENS),
        pump_supply=S(sim.PUMP_SUPPLY),
        pumpswap=pumpswap, curve=curve, migrating=migrating, migrating_ring=migrating_ring, raydium=raydium,
        repriced=repriced, reprice=reprice, liquidity=liquidity, locked_depth=depth,
        note=("sim_*, buy*, risk*, fv equal Rust exactly; curve exact_* is the program's (pump's) execution, "
              "0..=tolerance* lamports below the quote; sim_two_leg is ECON §3.1's B̂₂ on the sim's primitives"),
    ), Path(__file__).with_name("sim.json"))
    print(len(pumpswap), len(curve), len(migrating), len(migrating_ring), len(raydium), len(repriced), len(reprice),
          len(liquidity), len(depth))


if __name__ == "__main__":
    main()
