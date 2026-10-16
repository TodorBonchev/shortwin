"""M6R-1, M6-SEC-1 and OWNER-M6D-1 (the owner's decisions of 2026-09-30; ECONOMICS.md #105, #106, §1.2, §5.2, §6.1,
§6.2, §8.2, §10.1, §12.12 (12); ARCH §2.3.3, §2.5, §3.4, §4.6, §4.7): third-party liquidity on canonical PumpSwap
pools, the graduation's post-change warm-up price seed, and PumpSwap credited on the burned LP only.

The facts (R-VEN Update H11): the LP a canonical PumpSwap pool mints at creation is burned by migrate_v2, but anyone
can deposit into the pool and withdraw again -- boost vault funded or not -- and VQR scales with the deposit
(floor(VQR*base_in/base)); a deposit or withdrawal moves sqrt(X*(Y + VQR)) without moving the price and can be wrapped
around another instruction, exactly as on Raydium.  The graduation (M6 as built, ECONOMICS.md #104) reads the pool's
lp_supply as its anchor, and its post-change warm-up re-seeds P_ema from the pool's own bucket lows.

The rules compared (Market subclasses; "#106" is the sim's Market, the owner's decisions as the sims build them):
  "M6 as built" (Built104): no locked depth on PumpSwap -- the caps' X0 = min(X_now, L_slow/sqrt(P_ema)) without the
      D term, no cap on the exit reference liquidity, the graduation's L_slow capped at the lp_supply anchor only --
      and the warm-up price seed bounded upward only (M4X-1);
  "#105" (R105; the rule as decided before OWNER-M6D-1): M6R-1 -- D = floor(lambda_bps*isqrt(X*(Y + VQR))/1e4),
      lambda_bps = floor((lp_supply - lp_mint.supply)*1e4/lp_supply) (the burned LP's depth), the caps' X0 also
      <= D/(lambda_min*sqrt(P_ema)) with lambda_min = pool_lock_min_bps (50 %), the reference's EMA terms capped at
      floor(D*1e4/lambda_min), and L_slow capped at D during a PumpSwap warm-up (Market._warmup_slow_cap: the first
      PumpSwap observation after the transition) -- and M6-SEC-1 -- the seed falls by at most the largest v0.4.1 step
      down per integration (x11/12 at dt = 1);
  "#106" (the sim's Market): #105 with OWNER-M6D-1 -- PumpSwap credited on the burned LP only: every D term at
      lambda_min = PUMPSWAP_LOCK_MIN_BPS = 100 % (Market.lock_min_bps): the caps' X0 = min(X_now, L_slow/sqrt(P_ema),
      D/sqrt(P_ema)) and (since M6E-1) the reference cap D, so L_b = L_live on a canonical pool (D <= L_live);
  ablations: "M6-SEC-1 only" (NoD: #106 without the D rules) and "M6R-1 only" (NoDown: #106 with the seed unbounded
      downward); the historical spot leg at L_live (Spot100, before FITS-2 (a), #101); and Ref2D (#106 as first
      written, replaced by M6E-1: sizing on D, the reference cap kept at floor(D*1e4/pool_lock_min_bps) = 2D).
The spot leg at max(L_live, L_ema') (FITS-2 (a)) and the reference guard were venue-independent already; the keeper's
per-second liquidity-anomaly coverage (FITS-2 (b); switch_warmup.Coverage "new") is the keeper's, on any venue.
Parts:
  1  M6-SEC-1: one low recorded in a graduation's warm-up second (a [sell, crank_price, buy] next to the keeper's
     crank), on ECON §10.1's rows; the undisturbed exit, the as-built delay and #106's; a genuine dump right after the
     transition (nothing is forced when the seed lags a fall).
  2  M6R-1 at the graduation: a deposit between migrate_v2 and the transition crank raises the lp_supply anchor;
     L_slow after the transition and after the first PumpSwap observation, the carried position's exit, a new open's
     cap after the boost window.
  3  M6R-1 in steady state on a PumpSwap pool (market C: the fresh graduate after its boost, Tier C): (a) a deposit
     held k x the pool for H and pulled at a genuine pump (the reference cap); (b) an open while the deposit is held
     (sizing: the D term); (c) a deposit wrapped around the open (sizing and margin are not inflatable in one
     transaction, under any rule); (d) a pool with 50 % third-party LP: the withdraw-wrapped liquidation and the
     withdraw drag under the keeper's coverage, #100's spot leg at L_live against the rule as adopted (FITS-2);
     (e) M6D-1: (b)'s held deposit sizes the open (up to lambda/lambda_min = 2x the honest cap under #105) and
     leaves in the second a genuine pump starts -- L_slow (tau 1 h) keeps the reference at the cap, every
     liquidation fails its exit bound and the position gaps into S1 (lenders below Q's value, the borrower zeroed);
     #106 sizes on D and liquidates every row; (f) the reference cap under #106: D against 2D (Ref2D) on a grid of
     pulled deposits, the (d) drag, a withdraw-wrapped due liquidation, (M6E-1) an expiry after a deposit pulled at
     the deadline, and a governed pool_lock_min_bps below its default (Ref2D's cap 3D-5D).
Deterministic; standard library only.

    python3 pumpswap_lp.py            # every part (redirect to ../out/attacks/pumpswap_lp.out)
    python3 pumpswap_lp.py 1 3        # some parts
"""
import copy
import sys
from dataclasses import replace

from common import *
import switch_warmup as SW


# ------------------------------------------------------------------------------------------------------------
# Rules
# ------------------------------------------------------------------------------------------------------------
def _warmup_unbounded_down(self, t):
    """Market._integrate_warmup before M6-SEC-1 (M4R with M4X-1): P_ema = min(post-change lows, the step up)."""
    if t > self.ema_t:
        T = self.tier
        k = EMA_WARMUP_BUCKETS - self.warm
        ents = self.ring[len(self.ring) - k:] if k else []
        dt = min(t - self.ema_t, T.ema_dtmax_s)
        up = (self.ema * T.ema_tau_s + self.ema * (BPS + T.ema_clamp_bps) // BPS * dt) // (T.ema_tau_s + dt)
        p = min([e[5] for e in ents] + [self.b_lo, up])
        H = sorted([e[8] for e in ents] + [self.b_lhi])
        n = len(H)
        l = min(H[n - 1], H[(n - 1) // 2] * (BPS + T.ema_clamp_bps) // BPS)
        self.ema, self.liq = p, l
        self.ema_t = t
        self.ema_count += 1
        self.warm -= 1
        self.ring.append(self._entry(t, p, l, self.b_lo, self.b_hi, self.b_llo, self.b_lhi))
        if len(self.ring) > self.P.ring_len:
            del self.ring[0]
        self._guard_check()
    self.lb_sec, self.lb_lo, self.lb_hi = self.b_sec, self.b_lo, self.b_hi
    self.b_sec = None


class NoD(Market):
    """#106 without M6R-1's locked-depth rules on PumpSwap (the D term of the caps, the reference cap, the warm-up's
    L_slow cap): M6 as built plus M6-SEC-1."""

    def locked_depth_venue(self):
        return self.venue.kind == "ray"

    def _warmup_slow_cap(self):
        pass


class NoDown(Market):
    """#106 without M6-SEC-1: the warm-up seed falls to the lowest post-change bucket low at once (M6 as built plus
    M6R-1 and OWNER-M6D-1)."""
    _integrate_warmup = _warmup_unbounded_down


class Built104(NoD):
    """M6 as built (ECONOMICS.md #104): neither M6R-1 nor M6-SEC-1."""
    _integrate_warmup = _warmup_unbounded_down


class Spot100(SW.Spot100):
    """#100's liquidation spot leg at L_live (before FITS-2 (a), #101) with #106's other rules."""


class R105(Market):
    """The rule as the owner decided it in #105, before OWNER-M6D-1 (#106): a canonical PumpSwap pool's D divided by
    pool_lock_min_bps like Raydium's, in sizing, X0 <= D/(lambda_min*sqrt(P_ema)) -- a deposit held ~1 h sizes opens
    at up to lambda/lambda_min = 2x the burned depth's cap (M6D-1, part 3 (e)) -- and in the reference cap."""

    def lock_min_bps(self):
        return self.P.pool_lock_min_bps


class Ref2D(Market):
    """Ablation (part 3 (f)): #106 as first written, before M6E-1 -- sizing on D itself, the exit reference's cap on a
    canonical PumpSwap pool kept at floor(D*1e4/pool_lock_min_bps) (2D at the default), so the EMA terms of L_b may
    credit a pulled deposit up to twice the drained depth."""

    def ref_depth_cap(self):
        if not self.locked_depth_venue():
            return None
        return depth_at_lambda(self.venue.locked_depth(), self.P.pool_lock_min_bps)


RULES = {"built": Built104, "nod": NoD, "nodown": NoDown, "r105": R105, "r106": Market, "s100": Spot100,
         "ref2d": Ref2D}
RULE_NAME = {"built": "M6 as built (#104)", "nod": "M6-SEC-1 only", "nodown": "M6R-1 only", "r105": "#105",
             "r106": "#106", "s100": "#100's spot leg at L_live",
             "ref2d": "#106 with the reference cap at 2D (as first kept)"}


def hdr(cols):
    print("| " + " | ".join(cols) + " |")
    print("|" + "---|" * len(cols))


def row(cells):
    print("| " + " | ".join(str(c) for c in cells) + " |")


# ------------------------------------------------------------------------------------------------------------
# PumpSwap LP moves (R-VEN Update H11: both vaults, VQR and lp_supply scale; the burned creation LP stays)
# ------------------------------------------------------------------------------------------------------------
def unlocked_bps(v):
    return (v.lp_units - v.lp_locked) * BPS // v.lp_units if v.lp_units else 0


def withdraw_unlocked(v):
    """Every third-party LP unit out (floor): sqrt(X*(Y + VQR)) falls to about D."""
    u = unlocked_bps(v)
    if u > 0:
        v.remove_liquidity(u)


def wrapped(v, move):
    """The pool one instruction sees inside an LP round trip: 'wd' = every third-party LP unit withdrawn, 'depK' = K x
    the pool deposited (price unchanged)."""
    if move is None:
        return v
    w = copy.deepcopy(v)
    if move == "wd":
        withdraw_unlocked(w)
    elif move.startswith("dep"):
        w.add_liquidity(BPS * int(move[3:] or "1"))
    else:
        raise ValueError(move)
    return w


def restore(v, w):
    """After an instruction ran on the wrapped copy w of v (possibly trading), the round trip closes: the LP moved out
    (in) goes back (out) at the new reserve ratio (VQR with it)."""
    if w is v:
        return
    v.X = w.X * v.lp_units // w.lp_units
    v.Y = w.Y * v.lp_units // w.lp_units
    v.vq = w.vq * v.lp_units // w.lp_units


def obs_wrapped(m, t, move):
    v = m.venue
    w = wrapped(v, move)
    m.advance(t)
    m.venue = w
    m.observe(t)
    m.venue = v
    restore(v, w)


def try_liq(m, pos, t, move=None):
    v = m.venue
    w = wrapped(v, move)
    m.venue = w
    r = m.try_exit(pos, t)
    m.venue = v
    restore(v, w)
    return r


def sell_wrapped_obs(m, t, f):
    """[sell to f x the spot, crank_price, buy the same tokens back] in second t (the attacker's low); returns the
    round-trip cost (lamports)."""
    v = m.venue
    m.advance(t)
    d = -move_size(v, int(v.spot_wad() * f))
    if v.vq > 0:
        d = min(d, v.X * v.Y // v.vq * 99 // 100)      # a payout stays within the real quote reserve
    if d <= 0:
        m.observe(t)
        return 0
    got = v.sell(d)
    m.observe(t)
    return v.buy(d) - got


def exit_cell(r, t0, p1):
    if r is None or r["kind"] == "open":
        return "open"
    return f"{r['kind']} +{r['t'] - t0}s @{r['spot'] / p1:.2f}x"


def lender_cell(pos, r):
    if r is None or r["kind"] == "open":
        return "-"
    return f"{r['tokens'] / pos.Q:.0%} + {sol(r['lender_sol']):.4f}"


# ------------------------------------------------------------------------------------------------------------
# Part 1: M6-SEC-1
# ------------------------------------------------------------------------------------------------------------
T_GRAD = TE + DEFAULT.migration_delay_s        # the transition second of GRAD_SCENARIOS (the keeper's crank)


def grad_run(rule, key, sc, events=(), size=1.0):
    m = RULES[rule](MARKETS[key](), tier_of(key), DEFAULT)
    pos, r, p1 = run(m, size, replace(sc, events=tuple(sc.events) + tuple(events)))
    return m, pos, r, p1


def low_event(t, f, cost):
    def ev(m):
        if isinstance(m.venue, CPPool):
            cost.append(sell_wrapped_obs(m, t, f))
    return (t, ev)


def seed_at_gate(rule, key, sc, events):
    """P_ema / the pool's spot at the 8th warm-up integration (the gate), no position exit (size tiny)."""
    m = RULES[rule](MARKETS[key](), tier_of(key), DEFAULT)
    pos = m.open(max(1, m.max_open_q() // 1000), 0)
    evs = sorted(tuple(sc.events) + tuple(events), key=lambda e: e[0])
    ei = 0
    for t in range(1, T_GRAD + 12):
        while ei < len(evs) and evs[ei][0] <= t:
            evs[ei][1](m)
            ei += 1
        m.step_to(t)
        if t > T_GRAD and m.warm == 0:
            return m.ema / m.venue.spot_wad(), t - T_GRAD
    return None, None


def part1():
    print("## 1. M6-SEC-1: one low in a graduation's warm-up second")
    print()
    print("ECON §10.1's graduation rows (migration 2 s after the completion, the keeper cranking every second): in the")
    print(f"k-th second after the transition (k = 1: the first post-change bucket) the attacker sends [sell to f x the")
    print("pool's spot, crank_price, buy back] next to the keeper's crank, so that bucket's low is f x the price.  As built")
    print("the warm-up seed is the lowest post-change bucket low (bounded upward only), so one such low sets P_ema for the")
    print("rest of the warm-up and the EMA leg then climbs back at most one v0.4.1 step (x7/6) a second; with M6-SEC-1")
    print("the seed also falls by at most one step (x11/12) per integration.  Cells: the exit (seconds after the")
    print("completion, price), lenders' tokens back + SOL; the attacker's round trip (SOL, fees at the pool's tier).")
    print()
    hdr(["market", "scenario", "attack", "M6 as built (#104)", "#106", "attacker round trip (SOL)"])
    cases = [("D@80%", GRAD_SCENARIOS[0]), ("D@90%", GRAD_SCENARIOS[2]), ("D@97%", GRAD_SCENARIOS[1])]
    for key, sc in cases:
        base = {}
        for rule in ("built", "r106"):
            m, pos, r, p1 = grad_run(rule, key, sc)
            base[rule] = (pos, r, p1)
        row([key, sc.name, "none (honest)"] +
            [f"{exit_cell(base[x][1], TE, base[x][2])}; {lender_cell(base[x][0], base[x][1])}" for x in ("built", "r106")]
            + ["-"])
        for f in (0.5, 0.25):
            for k in (1, 4, 8):
                cells, costs = [], []
                for rule in ("built", "r106"):
                    cost = []
                    m, pos, r, p1 = grad_run(rule, key, sc, (low_event(T_GRAD + k, f, cost),))
                    cells.append(f"{exit_cell(r, TE, p1)}; {lender_cell(pos, r)}")
                    costs.append(cost[0] if cost else 0)
                row([key, sc.name, f"low at {f:g}x in second {k}"] + cells + [f"{sol(costs[-1]):.3f}"])
    print()
    print("The seed at the gate (P_ema / the pool's spot at the 8th warm-up integration; no position exit), a low of f x")
    print("in the first post-change second, and a genuine dump of the pool to f x (held) right after the transition:")
    print()
    hdr(["market", "move", "M6 as built (#104)", "#106"])
    for key, sc in (("D@80%", GRAD_SCENARIOS[0]), ("D@97%", GRAD_SCENARIOS[0])):
        for label, mk in (("low at 0.5x in second 1", lambda: (low_event(T_GRAD + 1, 0.5, []),)),
                          ("low at 0.25x in second 1", lambda: (low_event(T_GRAD + 1, 0.25, []),)),
                          ("low at 0.1x in second 1", lambda: (low_event(T_GRAD + 1, 0.1, []),)),
                          ("genuine dump to 0.5x from second 1", lambda: dump_events(0.5)),
                          ("genuine dump to 0.25x from second 1", lambda: dump_events(0.25))):
            cells = []
            for rule in ("built", "r106"):
                g, s = seed_at_gate(rule, key, sc, mk())
                cells.append("-" if g is None else f"{g:.3f} (gate +{s} s)")
            row([key, label] + cells)
    print()
    print("The genuine dump, positions at the cap held on (nothing may be forced: the spot leg reads the live price):")
    print()
    hdr(["market", "move", "M6 as built (#104)", "#106"])
    for key in ("D@80%", "D@97%"):
        for f in (0.5, 0.25):
            cells = []
            for rule in ("built", "r106"):
                m, pos, r, p1 = grad_run(rule, key, replace(GRAD_SCENARIOS[0], window=600), dump_events(f))
                cells.append(f"{exit_cell(r, TE, p1)}; P_ema/P at the end {m.ema / m.venue.spot_wad():.3f}")
            row([key, f"completion, then a genuine dump to {f:g}x from the first post-change second"] + cells)
    print()


def dump_events(f):
    """Other traders dump the pool to f x its price in the first post-change second and keep it there 600 s."""
    state = {}

    def first(m):
        if isinstance(m.venue, CPPool):
            state["p"] = m.venue.spot_wad()
            m.venue.move_to_price(int(state["p"] * f))

    def keep(m):
        if isinstance(m.venue, CPPool) and "p" in state:
            m.venue.move_to_price(int(state["p"] * f))
    return ((T_GRAD + 1, first),) + tuple((T_GRAD + 1 + i, keep) for i in range(1, 600))


# ------------------------------------------------------------------------------------------------------------
# Part 2: M6R-1 at the graduation
# ------------------------------------------------------------------------------------------------------------
def dep_at_transition(k, withdraw_at=0):
    """[migrate_v2, deposit k x the pool, crank_price (the transition)] in the migration second; the deposit is withdrawn
    `withdraw_at` s later (0: in the next transaction of the same second)."""
    state = {}

    def at_t(m):
        m.advance(T_GRAD)
        p = m.migrated_pool
        if p is None:
            return
        p.add_liquidity(BPS * k)
        state["pool"] = p
        m.observe(T_GRAD)                  # the attacker's crank: the transition reads lp_supply
        if withdraw_at == 0:
            withdraw_unlocked(p)

    def later(m):
        if "pool" in state:
            withdraw_unlocked(state["pool"])
    evs = [(T_GRAD, at_t)]
    if withdraw_at:
        evs.append((T_GRAD + withdraw_at, later))
    return tuple(evs)


def slow_trace(rule, key, events, probe=(0, 1, 9, 900, 3600)):
    """L_slow / sqrt(X*(Y + VQR)) of the pool at T_GRAD + s for s in probe (no position exit: size tiny), and the cap
    q / X of an open right after the boost window (T_GRAD + 900 s), relative to the honest run's."""
    m = RULES[rule](MARKETS[key](), tier_of(key), DEFAULT)
    pos = m.open(max(1, m.max_open_q() // 1000), 0)
    evs = sorted(tuple(GRAD_SCENARIOS[0].events) + tuple(events), key=lambda e: e[0])
    ei, out, q900 = 0, {}, None
    for t in range(1, T_GRAD + max(probe) + 1):
        while ei < len(evs) and evs[ei][0] <= t:
            evs[ei][1](m)
            ei += 1
        s = t - T_GRAD
        if TE - 60 <= t <= T_GRAD + 60 or t % 30 == 0 or s in probe or s == 900:
            m.step_to(t)                   # the keeper: every second around the graduation, else every 30 s
        else:
            m.advance(t)
            continue
        if s in probe and isinstance(m.venue, CPPool):
            out[s] = m.liq_slow / m.venue.sqrt_k()
        if s == 900 and isinstance(m.venue, CPPool):
            q900 = m.max_open_q() / m.venue.X
    return out, q900


def part2():
    print("## 2. M6R-1 at the graduation: a deposit between migrate_v2 and the transition")
    print()
    print("The attacker sends [migrate_v2, deposit k x the new pool, crank_price] (the transition reads the inflated")
    print("lp_supply as its anchor: L_slow = min(the curve's L_slow, anchor)), then withdraws.  As built L_slow keeps the")
    print("curve's value, ~1.35x the pool's depth, and decays at tau_slow = 1 h; with M6R-1 the first PumpSwap observation")
    print("caps it at the pool's locked depth D (the burned creation LP).  Cells: L_slow / sqrt(X*(Y + VQR)) at the")
    print("transition second, 1 s, 9 s (the gate), 900 s and 3600 s after it; the cap q/X of an open 900 s after the")
    print("transition (the boost window; the boost is spent by then).")
    print()
    hdr(["market", "move", "rule", "L_slow / depth at +0 / +1 / +9 / +900 / +3600 s", "open cap q/X at +900 s"])
    for key in ("D@80%",):
        for label, ev in (("none (honest)", ()), ("deposit 1x, withdrawn in the same second", dep_at_transition(1)),
                          ("deposit 3x, withdrawn in the same second", dep_at_transition(3)),
                          ("deposit 3x, held 60 s", dep_at_transition(3, 60))):
            for rule in ("built", "r106"):
                tr, q = slow_trace(rule, key, ev)
                row([key, label, RULE_NAME[rule], " / ".join(f"{tr.get(s, 0):.3f}" for s in (0, 1, 9, 900, 3600)),
                     "-" if q is None else f"{100 * q:.3f} %"])
    print()
    print("The carried positions' exits under the same deposit (ECON §10.1 rows; the keeper cranking every second):")
    print()
    hdr(["market", "scenario", "move", "M6 as built (#104)", "#106"])
    for key, sc in (("D@80%", GRAD_SCENARIOS[0]), ("D@90%", GRAD_SCENARIOS[1]), ("D@97%", GRAD_SCENARIOS[1])):
        for label, ev in (("none (honest)", ()), ("deposit 3x, withdrawn in the same second", dep_at_transition(3))):
            cells = []
            for rule in ("built", "r106"):
                m, pos, r, p1 = grad_run(rule, key, sc, ev)
                cells.append(f"{exit_cell(r, TE, p1)}; {lender_cell(pos, r)}")
            row([key, sc.name, label] + cells)
    print()


# ------------------------------------------------------------------------------------------------------------
# Part 3: M6R-1 in steady state
# ------------------------------------------------------------------------------------------------------------
def pool_c(tp_bps=0):
    """Market C's pool (the fresh graduate after its boost, lambda = 1), with tp_bps x the pool of third-party LP."""
    p = MARKETS["C"]()
    if tp_bps:
        p.add_liquidity(tp_bps)
    return p


def steady_market(rule, tp_bps=0, tier="C", size=1.0):
    pool = pool_c(tp_bps)
    m = RULES[rule](pool, TIERS[tier], DEFAULT)
    pos = m.open(max(1, int(m.max_open_q() * size)), 0)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    return m, pos, t


def held_run(rule, k, hold_s, top=3.0, dur=120, keeper="new", horizon=600):
    """A position at the cap on C (Tier C), the EMA converged; an LP deposits k x the pool at +1 s and holds it
    hold_s seconds (L_ema, L_slow follow the deeper pool), then withdraws in the second a genuine pump to top x over
    dur s starts; the keeper on its coverage duties (switch_warmup.Coverage).  Returns the exit cell and the reference
    liquidity L_b / sqrt(X*(Y + VQR)) at the pump's start."""
    m, pos, t0 = steady_market(rule)
    pool = m.venue
    p0 = pool.spot_wad()
    cov = SW.Coverage(keeper)
    lb = None
    for s in range(1, hold_s + horizon + 1):
        t = t0 + s
        if s == 1 and k:
            pool.add_liquidity(BPS * k)
        if s == hold_s + 1 and k:
            withdraw_unlocked(pool)
        if s > hold_s:
            pool.move_to_price(int(p0 * (1 + (top - 1) * min(s - hold_s, dur) / dur)))
        view = (pool.spot_wad(), pool.sqrt_k())
        if s == hold_s + 1:
            l_e = min(m.liq, m.ring_medians()[2]) if m.guard else m.liq
            l_ref = max(l_e, m.liq_slow)
            cap = m.ref_depth_cap()
            lb = max(pool.sqrt_k(), l_ref if cap is None else min(l_ref, cap)) / pool.sqrt_k()
        if cov.due(m, pos, t):
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                return f"{r['kind']} +{s - hold_s} s @{r['spot'] / p0:.2f}x", lb
        else:
            m.advance(t)
        cov.check_foreign(m, t, view)
    return "open", lb


def sizing_run(rule, k, hold_s, wrap=None):
    """C's market; an LP deposits k x the pool and holds it hold_s seconds (the keeper cranking every 30 s); then an open
    at the cap -- with the deposit still in (wrap None) or wrapped in [deposit, open, withdraw] (wrap 'dep3') --
    after which every third-party LP unit leaves.  Returns (q / X of the pool after the withdrawal, the cap on the
    honest pool)."""
    m = RULES[rule](pool_c(), TIERS["C"], DEFAULT)
    honest_x = m.venue.X
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    pool = m.venue
    if k:
        pool.add_liquidity(BPS * k)
    for s in range(1, hold_s + 1):
        if s % 30 == 0:
            m.step_to(t + s)
        else:
            m.advance(t + s)
    t += hold_s + 1
    m.step_to(t)
    w = wrapped(pool, wrap)
    m.venue = w
    q = m.max_open_q()
    m.venue = pool
    restore(pool, w)
    withdraw_unlocked(pool)
    return q / pool.X, honest_x


def sized_held_run(rule, k, size=0.95, top=5.0, dur=120, hold_s=3600, pull=True, horizon=600, P=DEFAULT):
    """M6D-1: C's market (Tier C), the EMA converged; an LP deposits k x the pool and holds it hold_s seconds (the
    keeper cranking every 30 s: L_ema and L_slow follow the deeper pool); then an open of `size` x the cap on the
    deepened pool -- sized on the held deposit, the (b) bound -- and in the next second a genuine pump to top x over
    dur s starts, the deposit pulled (every third-party LP unit) in its first second (pull) or kept; the #101 keeper on
    its coverage duties (per second near the threshold); P the protocol parameters (part 3 (f) (v): a governed
    pool_lock_min_bps).  Returns (the exit cell, q / the honest cap, the lenders' P&L / Q's value at the exit's spot,
    the borrower's share of V)."""
    m = RULES[rule](pool_c(), TIERS["C"], P)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    pool = m.venue
    honest = m.max_open_q()
    if k:
        pool.add_liquidity(int(BPS * k))
    for s in range(1, hold_s + 1):
        if s % 30 == 0:
            m.step_to(t + s)
        else:
            m.advance(t + s)
    t += hold_s + 1
    m.step_to(t)
    pos = m.open(max(1, int(m.max_open_q() * size)), t)
    p0 = pool.spot_wad()
    cov = SW.Coverage("new")
    for s in range(1, horizon + 1):
        tt = t + s
        if s == 1 and k and pull:
            withdraw_unlocked(pool)
        pool.move_to_price(int(p0 * (1 + (top - 1) * min(s, dur) / dur)))
        view = (pool.spot_wad(), pool.sqrt_k())
        if cov.due(m, pos, tt):
            m.step_to(tt)
            cov.observed(tt)
            r = m.try_exit(pos, tt)
            if r is not None:
                qv = pos.Q * r["spot"] // WAD
                lp = r["lender_sol"] - (pos.Q - r["tokens"]) * r["spot"] // WAD
                return (f"{r['kind']} +{s} s @{r['spot'] / p0:.2f}x", pos.Q / honest, lp / qv, r["borrower"] / pos.V)
        else:
            m.advance(tt)
        cov.check_foreign(m, tt, view)
    return "open", pos.Q / honest, None, None


def drag_run(rule, keeper, hold, attack, tp_bps=BPS, w=30, dur=60, horizon=300, att_from=None, size=1.0):
    """FITS-2 on PumpSwap (steady state): C's pool with tp_bps x the pool of third-party LP, which the attacker holds
    (lambda = 1/(1 + tp)); a position at the cap, the EMA converged; the keeper on its coverage duties; in every second
    from att_from, before the keeper's tick, the attacker sends [withdraw all, crank_price, deposit] (the drag) and
    [withdraw all, liquidate, deposit]; flat w s, then a genuine pump to hold x over dur s, held."""
    m, pos, t0 = steady_market(rule, tp_bps, size=size)
    pool = m.venue
    p0 = pool.spot_wad()
    cov = SW.Coverage(keeper)
    if att_from is None:
        att_from = 2 if keeper == "new" else 1
    for s in range(1, w + dur + horizon + 1):
        t = t0 + s
        if s > w:
            pool.move_to_price(int(p0 * (1 + (hold - 1) * min(s - w, dur) / dur)))
        view = (pool.spot_wad(), pool.sqrt_k())
        a = attack and s >= att_from
        if a:
            obs_wrapped(m, t, "wd")
            r = try_liq(m, pos, t, "wd")
            if r is not None:
                return f"{r['kind']} +{s - w} s by the attacker @{r['spot'] / p0:.3f}x"
        if cov.due(m, pos, t):
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                return f"{r['kind']} +{s - w} s by the keeper @{r['spot'] / p0:.3f}x"
        elif not a:
            m.advance(t)
        cov.check_foreign(m, t, view)
    return "open"


def drag_threshold(rule, keeper, tp_bps=BPS, lo=1.2, hi=3.5, tol=0.005, size=1.0):
    """The lowest hold level at which the honest keeper liquidates (no attacker)."""
    while hi - lo > tol:
        mid = (lo + hi) / 2
        if drag_run(rule, keeper, mid, False, tp_bps, size=size).startswith("liquidation"):
            hi = mid
        else:
            lo = mid
    return hi


def wrapped_due_run(rule, tp_bps, size, top, dur=120, w=30, horizon=400, attack=True):
    """Part 3 (f): a DUE position.  C's pool with tp_bps x the pool of third-party LP, which the attacker holds; a
    position of `size` x the cap, the EMA converged; flat w s, then a genuine pump to top x over dur s, held; the #101
    keeper on its coverage duties.  With attack, the attacker sends [withdraw all, liquidate, deposit] in every second
    before the keeper (no drag crank: the liquidation's own withdrawal only), so its liquidation buys on the drained
    pool (depth D) against the reference.  Returns (who, seconds after the pump starts, the exit, its spot / the
    pre-pump spot, the borrower's share of V, the lenders' P&L against Q's value at the exit's spot) or None if the
    position stays open."""
    m, pos, t0 = steady_market(rule, tp_bps, size=size)
    pool = m.venue
    p0 = pool.spot_wad()
    cov = SW.Coverage("new")
    for s in range(1, w + dur + horizon + 1):
        t = t0 + s
        if s > w:
            pool.move_to_price(int(p0 * (1 + (top - 1) * min(s - w, dur) / dur)))
        view = (pool.spot_wad(), pool.sqrt_k())
        r, who = None, None
        if attack:
            m.advance(t)
            r, who = try_liq(m, pos, t, "wd"), "the attacker"
        if r is None and cov.due(m, pos, t):
            m.step_to(t)
            cov.observed(t)
            r, who = m.try_exit(pos, t), "the keeper"
        elif r is None and not attack:
            m.advance(t)
        if r is not None:
            qv = pos.Q * r["spot"] // WAD
            lp = r["lender_sol"] - (pos.Q - r["tokens"]) * r["spot"] // WAD
            return who, s - w, r, r["spot"] / p0, r["borrower"] / pos.V, lp / qv
        cov.check_foreign(m, t, view)
    return None


def part3():
    print("## 3. M6R-1 in steady state on a PumpSwap pool (market C: the fresh graduate after its boost, Tier C)")
    print()
    print("(a) A deposit of k x the pool held for H and pulled in the second a genuine 3x/120 s pump starts (the keeper on")
    print("its coverage duties, #101 keeper).  L_ema and L_slow follow the deeper pool while it is held; the exit reference")
    print("L_b = max(L_live, min(max(L_ema', L_slow), cap)) then credits the pulled depth unless the cap floor(D*1e4/")
    print("lambda_min) binds -- under #106 lambda_min = 100 % on PumpSwap, so the cap is D and L_b the drained pool's own")
    print("depth.  Cells: the exit (seconds after the pump starts, price); L_b / the pool's depth at the pump's first")
    print("second.")
    print()
    hdr(["deposit", "held", "M6 as built (#104)", "#106"])
    for k, hold_s in ((0, 600), (1, 600), (1, 3600), (3, 600), (3, 3600), (7, 3600)):
        cells = []
        for rule in ("built", "r106"):
            c, lb = held_run(rule, k, hold_s)
            cells.append(f"{c}; L_b/depth {lb:.2f}" if lb is not None else c)
        row([f"{k}x the pool" if k else "none", f"{hold_s} s"] + cells)
    print()
    print("(b), (c) An open at the cap after the deposit was held for H (still in during the open), or wrapped around the")
    print("open alone; every third-party LP unit then leaves.  Cells: the position's q / X of the pool after the")
    print("withdrawal (the honest open: 1x); a deposit wrapped around the open inflates nothing (the minimum drops X_now,")
    print("and L_slow, D and the ring's EMA state do not move within the transaction), a held one pushes L_slow, which")
    print("#105's D term bounds at lambda/lambda_min of the burned LP's depth and #106's (sizing on D) at the depth itself:")
    print()
    rules = ("built", "r105", "r106")
    hdr(["deposit", "held / wrapped"] + [RULE_NAME[r] for r in rules])
    base = sizing_run("r106", 0, 0)[0]
    for k, hold_s, wrap in ((0, 0, "dep3"), (1, 3600, None), (3, 3600, None), (7, 3600, None), (3, 14400, None),
                            (7, 14400, None)):
        cells = []
        for rule in rules:
            q, _hx = sizing_run(rule, k, hold_s, wrap)
            cells.append(f"{100 * q:.3f} % ({q / base:.2f}x)")
        row([f"{k or 3}x the pool", "wrapped around the open" if wrap else f"held {hold_s} s"] + cells)
    print()
    print("(d) FITS-2 on PumpSwap: C's pool with as much third-party LP again (lambda 50 %), which the attacker holds; a")
    print("position at the cap or half of it (#106 sizes on D: half and a quarter of #105's cap, the same positions);")
    print("flat 30 s, then a genuine pump to a hold level just below the honest threshold (healthy at the pool's depth).")
    print("From its first second the attacker sends [withdraw all, crank_price, deposit] (the withdraw drag: its")
    print("attacker-only seconds pull L_ema to D) and [withdraw all, liquidate, deposit] every second.  The liquidation")
    print("spot leg at L_live (#100) or at max(L_live, L_ema') (FITS-2 (a), #101), each with the #99 keeper (per second")
    print("only near a threshold, else every 30 s) or the #101 keeper (FITS-2 (b): per second for 10 s after a foreign")
    print("bucket liquidity, extended while it recurs); every column with #106's D rules.  Cells: the attacked outcome")
    print("at each hold level (the honest keeper leaves every one of them open):")
    print()
    hdr(["size (of the cap)", "honest threshold", "hold"] + [DRAG_NAMES[c] for c in DRAG_COMBOS])
    for size, th, hold, cells in drag_table("r106"):
        row([f"{size:g}", f"{th:.3f}x", f"{hold:.3f}x"] + cells)
    print()
    part3e()
    part3f()


DRAG_COMBOS = (("s100", "old"), ("r106", "old"), ("s100", "new"), ("r106", "new"))
DRAG_NAMES = {("s100", "old"): "spot leg at L_live, #99 keeper (#100)",
              ("r106", "old"): "spot leg at max(L_live, L_ema'), #99 keeper",
              ("s100", "new"): "spot leg at L_live, #101 keeper",
              ("r106", "new"): "spot leg at max(L_live, L_ema'), #101 keeper (as adopted)"}


class S100Ref2D(SW.Spot100, Ref2D):
    """Part 3 (f): Spot100 with Ref2D's reference cap."""


RULES["s100ref2d"] = S100Ref2D
RULE_NAME["s100ref2d"] = "#100's spot leg at L_live, the reference cap at 2D"
REF2D_OF = {"r106": "ref2d", "s100": "s100ref2d"}


def drag_table(rule):
    """Part 3 (d)'s cells: [(size, the honest threshold, hold, [outcome per DRAG_COMBOS column])]; rule 'ref2d' runs
    every column with the reference cap at 2D (Ref2D, part 3 (f)), the thresholds and holds staying #106's."""
    out = []
    for size in (1.0, 0.5):
        th = drag_threshold("r106", "new", size=size)
        for dh in (0.005, 0.01, 0.02, 0.04):
            hold = round(th - dh, 3)
            cells = [drag_run(REF2D_OF[r] if rule == "ref2d" else r, k, hold, True, size=size) for r, k in DRAG_COMBOS]
            out.append((size, th, hold, cells))
    return out


def part3e():
    print("(e) M6D-1: (b)'s held deposit sizes the open, then leaves at a genuine pump.  An LP deposits k x the pool")
    print("and holds it 1 h (a crank every 30 s); a position of `size` x the cap is opened on the deepened pool (under")
    print("#105 sized on the held deposit: X0's D term allows lambda/lambda_min of the burned LP's depth, 2x at")
    print("lambda_min 50 %; under #106 on the burned depth D itself); in the next second a genuine pump to top x over")
    print("120 s starts and the LP withdraws every third-party unit in its first second (or keeps it: the control).")
    print("L_slow (tau 1 h) still credits the deposit, so under #105 the reference L_b stays at the cap floor(D*1e4/")
    print("lambda_min) = 2D while the pool is back at D (#106: the cap is D, L_b the drained pool); the #101 keeper tries")
    print("every covered second.  Cells: the exit (seconds after the pump starts, price); q / the honest cap; the")
    print("lenders' P&L against Q's value at the exit's spot; the borrower's share of V.")
    print()
    rules = ("built", "r105", "r106", "ref2d")
    hdr(["deposit (third-party share)", "held, then", "pump", "size (of the cap)"] + [RULE_NAME[r] for r in rules])
    cases = [(0, True, 5.0, 0.95), (3, False, 5.0, 0.95), (3, True, 3.0, 0.95), (3, True, 5.0, 0.95),
             (3, True, 10.0, 0.95), (3, True, 5.0, 0.75), (3, True, 5.0, 0.5), (7, True, 5.0, 0.95),
             (1, True, 5.0, 0.95), (0.5, True, 5.0, 0.95), (0.25, True, 5.0, 0.95), (0.075, True, 5.0, 0.95)]
    for k, pull, top, size in cases:
        cells = []
        for rule in rules:
            c, qr, lp, bw = sized_held_run(rule, k, size=size, top=top, pull=pull)
            cells.append(c if lp is None else f"{c}; q {qr:.2f}x; lenders {100 * lp:+.1f} % of Q; borrower "
                                              f"{100 * bw:.0f} % of V")
        dep = "none" if not k else f"{k:g}x the pool ({100 * k / (1 + k):.0f} %)"
        row([dep, "-" if not k else ("pulled at the pump" if pull else "kept (control)"), f"{top:g}x / 120 s",
             f"{size:g}"] + cells)
    print()


def short_exit(c):
    """'liquidation +38 s @2.27x; ...' -> 'liquidation +38 s @2.27x'."""
    return c.split(";")[0]


def expiry_run(rule, k, size=0.95, pull_before=0, hold_s=3600, horizon=4000):
    """Part 3 (f) (iv), M6E-1: C's market (Tier C), the EMA converged; a position of `size` x the cap opened on the
    honest pool (sized on D, as every #106 open), the keeper cranking every 600 s to hold_s before its deadline; there
    an LP deposits k x the pool and holds it (a crank every 30 s: L_ema and L_slow follow the deeper pool), withdrawing
    every third-party unit pull_before s before the deadline; from the deadline the keeper cranks and tries expire
    every second.  Returns (seconds after the deadline, the exit kind, L_b / the pool's depth at the first try) or
    (None, None, L_b ratio) after horizon s."""
    m = RULES[rule](pool_c(), TIERS["C"], DEFAULT)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    pool = m.venue
    pos = m.open(max(1, int(m.max_open_q() * size)), t)
    dl = pos.deadline
    while t + 600 <= dl - hold_s:
        t += 600
        m.step_to(t)
    if k:
        pool.add_liquidity(int(BPS * k))
    for tt in range(t + 1, dl + 1):
        if k and tt == dl - pull_before:
            withdraw_unlocked(pool)
        if tt % 30 == 0 or tt == dl:
            m.step_to(tt)
        else:
            m.advance(tt)
    lb = None
    for s in range(1, horizon + 1):
        m.step_to(dl + s)
        if lb is None:
            l_e = min(m.liq, m.ring_medians()[2]) if m.guard else m.liq
            l_ref = max(l_e, m.liq_slow)
            cap = m.ref_depth_cap()
            lb = max(pool.sqrt_k(), l_ref if cap is None else min(l_ref, cap)) / pool.sqrt_k()
        r = m.try_exit(pos, dl + s)
        if r is not None:
            return s, r["kind"], lb
    return None, None, lb


def part3f():
    print("(f) The reference cap under #106.  #106 credits a canonical PumpSwap pool with its burned depth D alone: it")
    print("sizes positions on D and (M6E-1) caps the exit reference's EMA terms at D, so L_b = max(L_live, min(max(L_ema',")
    print("L_slow), D)) is L_live itself (D <= L_live on a canonical pool).  Ref2D is #106 as first written: sizing on D,")
    print("the reference cap kept at floor(D*1e4/pool_lock_min_bps) = 2D, so the EMA terms (V2-14: quiet-second")
    print("withdrawals cannot lower the reference) could still credit a pulled deposit up to twice the drained depth.")
    print()
    print("(i) (e)'s run over a grid -- a deposit of k x the pool held 1 h, then a position at the cap, the deposit pulled")
    print("in the first second of a genuine pump to top x over dur s (k = 0: no deposit, the control).  Cells: #106's exit")
    print("(seconds after the pump starts, price) at the cap and the lenders' P&L against Q's value; the last column")
    print("compares every cell of the row, at the cap and at half of it, with Ref2D:")
    print()
    hdr(["pump", "no deposit (control)", "1x the pool, pulled", "3x the pool, pulled",
         "Ref2D: the same exit second and price"])
    n_cells = n_same = 0
    dl = db = 0.0
    flips = []
    gap = []                                 # the lenders' extra loss where the control is S1 too (points of Q)
    for top in (2.0, 3.0, 5.0, 10.0, 20.0):
        for dur in (1, 30, 120, 300):
            cells, same, tot = [], 0, 0
            for size in (1.0, 0.5):
                ctl = None
                for k in (0, 1, 3):
                    a = sized_held_run("r106", k, size=size, top=top, dur=dur)
                    b = sized_held_run("ref2d", k, size=size, top=top, dur=dur)
                    kind = a[0].split(" ")[0]
                    if k == 0:
                        ctl, ctl_lp = kind, a[2]
                    elif kind == ctl == "S1":
                        gap.append(ctl_lp - a[2])
                    elif kind != ctl:
                        flips.append((ctl, kind, f"{top:g}x / {dur} s", k, size))
                    if size == 1.0:
                        cells.append(short_exit(a[0]) + ("" if a[2] is None else f"; {100 * a[2]:+.1f} %"))
                    tot += 1
                    same += short_exit(a[0]) == short_exit(b[0])
                    if a[2] is not None and b[2] is not None:
                        dl = max(dl, abs(a[2] - b[2]))
                        db = max(db, abs(a[3] - b[3]))
            n_cells += tot
            n_same += same
            row([f"{top:g}x / {dur} s"] + cells + [f"{same} of {tot}"])
    print()
    print(f"Ref2D against #106 over the grid: {n_same} of {n_cells} exits at the same second and price; the lenders' P&L")
    print(f"within {100 * dl:.2f} points of Q's value, the borrower's share within {100 * db:.2f} points of V (#106's")
    print("reference cost is the live pool's, so the penalty base min(B_live, B_ref) and the lenders' penalty on it are")
    print("a little higher where a pulled deposit left L_slow above the pool).")
    groups = {}
    for ctl, kind, pump, k, size in flips:
        g = groups.setdefault((ctl, kind), ({}, {}, {}))
        g[0][pump] = g[1][f"{k}x"] = g[2][f"{size:g}"] = True
    desc = "; ".join(f"{ctl} -> {kind} at {', '.join(g[0])} ({' / '.join(sorted(g[1]))} the pool, sizes "
                     f"{' / '.join(g[2])})" for (ctl, kind), g in groups.items())
    print(f"A pulled deposit changes the control's outcome in {len(flips)} of the {n_cells // 3 * 2} deposit cells: {desc}.")
    print("Sizing is on D under #106, but the margin is priced at the open on the pool as it is (max(B_spot, B_ema) at")
    print("the deposited depth), so a position opened while the deposit is held posts a little less margin than the same")
    print("q on D.  Tier C's opening CR of 300 % puts the liquidation threshold at about 2x and the V/B edge at about 3x;")
    print("the thinner margin moves both a little lower: a hold at 2.00x liquidates (the control stays open) and a 3x gap")
    print("that liquidates the control with the borrower zeroed settles in S1, the lenders whole in both.  Past the edge")
    print("every row, the control's too, is the genuine gap class (S1), and the pulled deposit adds "
          f"{100 * min(gap):.1f}-{100 * max(gap):.1f} points of")
    print("Q to the lenders' loss.")
    print()
    a, b = drag_table("r106"), drag_table("ref2d")
    same = sum(x == y for ra, rb in zip(a, b) for x, y in zip(ra[3], rb[3]))
    print(f"(ii) (d)'s drag table with Ref2D's cap in every column: {same} of {sum(len(r[3]) for r in a)} cells the same")
    print("(no drag of attacker-only seconds forced anything under the #101 keeper with either cap).")
    print()
    print("(iii) A due liquidation wrapped in the attacker's own withdrawal: C's pool with k x the pool of third-party LP")
    print("(the attacker's), a position at `size` x the cap, flat 30 s, then a genuine pump to top x over 120 s, held;")
    print("the #101 keeper on its duties; the attacker sends [withdraw all, liquidate, deposit] in every second before")
    print("the keeper, so its liquidation buys on the drained pool (depth D) against the reference.  Cells: who exits,")
    print("when (seconds after the pump starts), at what price; the borrower's share of V; the lenders' P&L against Q's")
    print("value:")
    print()
    hdr(["third-party LP", "size (of the cap)", "pump", "no attacker (the keeper)", "attacker, #106 (cap D)",
         "attacker, Ref2D (cap 2D)"])
    for tp in (BPS, 3 * BPS):
        for size in (1.0, 0.5):
            for top in (3.0, 5.0):
                cells = []
                for rule, att in (("r106", False), ("r106", True), ("ref2d", True)):
                    o = wrapped_due_run(rule, tp, size, top, attack=att)
                    if o is None:
                        cells.append("open")
                        continue
                    who, s, r, px, bw, lp = o
                    cells.append(f"{r['kind']} +{s} s by {who} @{px:.2f}x; borrower "
                                 f"{100 * bw:.1f} % of V; lenders {100 * lp:+.2f} %")
                row([f"{tp // BPS}x the pool ({100 * tp // (BPS + tp)} %)", f"{size:g}", f"{top:g}x / 120 s"] + cells)
    print()
    print("(iv) M6E-1: an expiry after a deposit pulled at the deadline.  A position of `size` x the cap (sized on D) held")
    print("to its deadline (Tier C, 7 d; the keeper cranking every 600 s); an LP deposits k x the pool an hour before the")
    print("deadline and holds it (a crank every 30 s: L_slow, tau 1 h, follows the deeper pool), withdrawing every")
    print("third-party unit at the deadline (or 600 / 1,800 s before); from the deadline the keeper cranks and tries")
    print("expire every second.  The expiry bound is B_ref*(1 + delta_exp), delta_exp = 1 %: a buyback on the drained")
    print("pool costs (1 - c/2)/(1 - c) - 1 more than a reference at twice its depth, 1.2 % at c = q/X = 2.35 %, so a")
    print("reference that still credits the pulled deposit refuses it until L_slow has decayed.  Cells: the expiry,")
    print("seconds after the deadline; L_b / the pool's depth at the first try:")
    print()
    hdr(["deposit (third-party share)", "pulled", "size (of the cap)", "#106 (cap D)", "Ref2D (cap 2D)"])
    for k, pull, size in ((0, 0, 0.95), (1, 0, 0.95), (2, 0, 0.95), (3, 0, 0.95), (3, 600, 0.95), (3, 1800, 0.95),
                          (3, 0, 0.88), (3, 0, 0.8), (3, 0, 0.5)):
        cells = []
        for rule in ("r106", "ref2d"):
            sec, kind, lb = expiry_run(rule, k, size=size, pull_before=pull)
            cells.append(f"{'open' if sec is None else f'{kind} +{sec} s'}; L_b/depth {lb:.2f}")
        dep = "none" if not k else f"{k:g}x the pool ({100 * k / (1 + k):.0f} %)"
        row([dep, "-" if not k else ("at the deadline" if pull == 0 else f"{pull} s before"), f"{size:g}"] + cells)
    print()
    print("(v) The governed lambda_min.  pool_lock_min_bps is a governed parameter in [2,000, 10,000]; under #106 it")
    print("reaches no PumpSwap term, while Ref2D's cap floor(D*1e4/pool_lock_min_bps) follows it (3D at 3,333, 5D at")
    print("2,000).  (e)'s run -- a deposit of k x the pool held 1 h, the open at 95 % of the cap (sized on D under both),")
    print("the deposit pulled in the first second of a genuine pump over 120 s.  Cells: the exit; the lenders' P&L")
    print("against Q's value; the borrower's share of V:")
    print()
    hdr(["pool_lock_min_bps", "deposit", "pump", "#106 (cap D)", "Ref2D (cap D*1e4/pool_lock_min_bps)"])
    for lam in (5000, 3333, 2000):
        P = replace(DEFAULT, pool_lock_min_bps=lam)
        for k, top in ((1, 5.0), (3, 5.0), (3, 10.0)):
            cells = []
            for rule in ("r106", "ref2d"):
                c, _qr, lp, bw = sized_held_run(rule, k, size=0.95, top=top, P=P)
                cells.append(c if lp is None else f"{c}; lenders {100 * lp:+.1f} % of Q; borrower {100 * bw:.0f} % of V")
            row([f"{lam:,}", f"{k}x the pool", f"{top:g}x / 120 s"] + cells)
    print()
    print("Why the cap is D (M6E-1).  With the positions sized on D and pool_lock_min_bps at its default, a cap at 2D")
    print("changes no liquidation in (i)-(iii): a buyback on the drained pool costs (1 - c/2)/(1 - c) - 1 more than a")
    print("reference at twice its depth, 1.27 % at the impact cap's c = 2.47 %, inside delta_liq = 2 %.  It is outside")
    print("delta_exp = 1 % above c = 1.96 % (about 80 % of the cap): (iv) at 2D a deposit held into a deadline and pulled")
    print("there holds such an expiry up for as long as L_slow stays above twice the drained depth -- up to an hour, a")
    print("free extension for the borrower with the lenders' tokens out, no attacker-only second needed (a genuine LP")
    print("leaving at the deadline does the same); and (v) at a governed pool_lock_min_bps below 5,000 the cap")
    print("floor(D*1e4/lambda_min) is 3D-5D and the pulled deposit gaps a position sized on D into S1 again, M6D-1's")
    print("loss.  At D the reference is the live pool (D <= L_live): no third-party deposit is credited to an exit, as to")
    print("a size or a tier, and pool_lock_min_bps reaches no PumpSwap term.  What the EMA terms gave on PumpSwap -- a")
    print("buyback wrapped in the LP's own withdrawal priced against the deeper pool -- is (iii)'s difference: the same")
    print("second, the borrower's share of V at most 0.1 points lower, the lenders whole.")
    print()


PARTS = {"1": part1, "2": part2, "3": part3}

if __name__ == "__main__":
    want = sys.argv[1:] or list(PARTS)
    for p in want:
        PARTS[p]()
