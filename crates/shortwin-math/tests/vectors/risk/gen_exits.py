#!/usr/bin/env python3
"""Vectors for shortwin_math::{risk, waterfall} captured from shortwin_sim.py v0.4.1 runs.

The sim's own Market methods are wrapped (never modified) to record their inputs and
outputs while it runs the scenario library (every market x SCENARIOS, GRAD_SCENARIOS,
FAST_SCENARIOS), ECON §13's exit fuzz (fuzz_exits(1000, 20260927), its statistics asserted
against ECON_13_EXIT_FUZZ), attack scripts of sim/attacks/ and a few branch-targeted runs.
Only the default switches are recorded (rows from markets with an analysis switch or a
Market subclass are skipped):

  * CR, spot leg, the EMA leg on the ring's cached (x, y)   <- Market.status
  * "normal buyback allowed" (try_exit's buyback_ok)       <- Market.status (transcribed)
  * sigma                                                  <- Market.slip_at
  * D4 margin (newest ring entry's cached (x, y)), open fee, R_min   <- Market.open
  * D4 margin with an undefined EMA-leg quote (the spot fallback)     <- Market.open
  * liquidation (keeper floor) / expiry waterfalls          <- Market.check_exit
  * close waterfall                                         <- Market.try_exit(borrower_close=True)
  * FV and its p' at every evaluated (P, L)                <- Market.fv_parts
  * settlement waterfall, its vesting lump (S1: AIR-21; tokens at 0 on a venue that cannot
    execute: AIR-09), K_s                                   <- Market._settle
  * K_s and K_e on random amounts and locked terms         <- Market.settle_reward, expiry_keeper

The settlement-box decisions themselves (legs, refutation, amount check, arming) belong to
`settle` and are not captured here. Branch-targeted runs (all through the sim's try_exit):
positions whose minimum rent is raised to a fraction of the vault before a liquidation
(rent and penalty truncation), small positions topped up as by add_margin and held to
expiry with a large rent (expiry keeper reward truncated), pools disabled once the EMA leg
holds (S3, FV cap binding and not), an expired healthy position settled in S3 (the
expiry-fee p'), small liquidations (the keeper floor, N22), the drained-pool model S2
cases of attacks/fvcap.py, the liquidation sandwiches of attacks/sandwich.py and
attacks/ptf_sandwich.py, and opens right after a crash the EMA still lags, larger than the
newest ring entry's cached token reserve (Market.open's `qe is None` branch: `B_margin =
max(qs[1], qe[1] if qe else qs[1])`, the spot quote alone; no other run reaches it, and
only these opens' margin rows are kept, in `margin_ema_undefined`). `exit_cascade` samples the buyback exits of attacks/cascade.py's
liquidation baskets (markets filled to the OI cap under gaps and pumps), each with its
fixed-delta bound and `max_in`. Writes exits.json.
"""
import json
import random
import sys
from collections import defaultdict
from dataclasses import replace
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
SIM_DIR = ROOT / "docs" / "design" / "sim"
sys.path.insert(0, str(SIM_DIR))
sys.path.insert(0, str(SIM_DIR / "attacks"))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)

U64 = 2**64 - 1
SEED = 20260927
BPS, WAD = sim.BPS, sim.WAD
M = sim.Market
# ECON §13 "Fuzz results" (v0.4.1): the exit fuzz whose rows this generator samples. AIR-22 (the
# sustained growth g as the lower median of the newest kappa_growth_n step rates, not their minimum)
# moves it from ECON's published row (exits 709, sandwich attempts 8,191, bundles 256,972,
# attacker-only seconds 817, blocker runs 196, noisy runs 519, freezer runs 246, frozen seconds
# 1,907, max loss over B_ref 103 bps, max settle box 95 bps; liquidation 213, expiry 286, S1 174,
# S2 8, S3 28): the fuzz draws from one random stream, so every run after the first changed exit
# draws differently.
ECON_13_EXIT_FUZZ = {
    "runs": 1000, "exits": 697, "sandwich_attempts": 8948, "bundles": 253491, "attacker_only_seconds": 772,
    "blocker_runs": 192, "noisy_runs": 490, "freezer_runs": 267, "frozen_seconds": 2905,
    "penalty_truncated_by_sandwich": 22, "max_loss_over_bref_bps": 102, "max_settle_box_bps": 94,
    "reserve_waits_liq": 0, "reserve_waits_exp": 0,
    "kinds": {"liquidation": 224, "expiry": 281, "S1": 147, "S2": 10, "S3": 35}}
CAP = defaultdict(list)
CASCADE = [False]   # while True, buyback exits go to CAP["exit_cascade"] with their bounds


def s(x):
    return None if x is None else str(x)


def qv(q):
    return None if q is None else str(q[1])


def qn(q):
    return None if q is None else str(q[0])


def sig(x):
    assert x >= 0, x
    return str(min(x, U64))


def default_model(m):
    """Rows are recorded only for the v0.4.1 model with its default switches."""
    P = m.P
    return (type(m) is M and not P.box_obs and not P.pprime_raw and P.ref_cap_lo and P.liq_reserve == "none"
            and P.exp_reserve == "rent+fee" and P.liq_keeper_parity and P.fv_cap and P.two_leg and P.synthetic
            and P.margin_policy == "cr" and P.exit_ref == "fresh")


def wrap(name):
    def deco(fn):
        fn.__wrapped__ = getattr(M, name)
        setattr(M, name, fn)
        return fn
    return deco


STATUS_CALLS = [0]


@wrap("status")
def status(self, pos, t):
    """Every status call is an evaluation point; a deterministic sample of them (rarer
    states more often) is recorded, with the buyback predicate on the same state."""
    st = status.__wrapped__(self, pos, t)
    if not default_model(self):
        return st
    STATUS_CALLS[0] += 1
    v = self.venue
    rare = not v.has_route() or not st["lo_ok"] or st["spot_leg"] or st["ema_leg"]
    if STATUS_CALLS[0] % (7 if rare else 97):
        return st
    Veff, L = st["Veff"], pos.L
    CAP["spot_leg"].append(dict(executable=v.has_route(), fresh=st["lo_ok"], v=s(pos.V), r_due=s(st["R"]),
                                b_rs=qv(st["qrs"]), l_bps=s(L), expected=st["spot_leg"]))
    for q, cr in ((st["qs"], st["cr_s"]), (st["qs_r"], None), (st["qe"], st["cr_e"]), (st["qrs"], st["cr_rs"])):
        if q is not None and q[1] > 0:
            want = Veff * BPS // q[1]
            assert cr is None or cr == want
            CAP["cr"].append(dict(v=s(pos.V), r_due=s(st["R"]), b_hat=qv(q), l_bps=s(L), cr=s(max(want, 0)),
                                  below=want < L))
        elif q is None:
            CAP["cr"].append(dict(v=s(pos.V), r_due=s(st["R"]), b_hat=None, l_bps=s(L), cr="0", below=True))
    # EMA leg (decision 2): the newest liq_persist_n entries' cached (x, y), newest first.
    entries = []
    for e in reversed(self.ring):
        q = self.quote_xy(pos.Q, e[3], e[4])
        entries.append([s(e[3]), s(e[4]), qv(q)])
    CAP["ema_leg"].append(dict(v=s(pos.V), r_due=s(st["R"]), l_bps=s(L), n=s(self.P.liq_persist_n),
                               entries=entries, run=s(st["ema_run"]), expected=st["ema_leg"]))
    if v.has_route():
        K = self.settle_reward(pos.V)
        qs = st["qs"]
        slip = self.slip_at(pos.Q, *v.eff(), live=True)
        ok = qs is not None and qs[1] + pos.ert <= pos.V - K and slip <= pos.slip_max   # try_exit's buyback_ok
        CAP["buyback"].append(dict(v=s(pos.V), k_s=s(K), ert=s(pos.ert), b_live=qv(qs), sigma_live=sig(slip),
                                   sigma_max=s(pos.slip_max), expected=ok))
    return st


@wrap("slip_at")
def slip_at(self, Q, X, Y, live=False):
    r = slip_at.__wrapped__(self, Q, X, Y, live=live)
    if default_model(self):
        q = self.venue.quote_at(Q, X, Y, two_leg=self.P.two_leg, live=live)
        CAP["sigma"].append(dict(n=qn(q), q=s(Q), x=s(X), y=s(Y), expected=sig(r)))
    return r


@wrap("open")
def open_(self, Q, t):
    pos = open_.__wrapped__(self, Q, t)
    if not default_model(self):
        return pos
    T = self.tier
    e = self.ring[-1]
    qe = self.venue.quote_at(Q, e[3], e[4], two_leg=self.P.two_leg)
    if qe is None:
        # Market.open: B_margin = max(qs[1], qe[1] if qe else qs[1]) -- the spot quote alone.
        assert pos.B_margin == pos.B_open and pos.B_ema_open == 0
        CAP["margin_ema_undefined"].append(dict(s=s(pos.S), b_spot=s(pos.B_open), b_ema=None,
                                                cr_open_bps=s(T.cr_open_bps), expected=s(pos.M)))
    else:
        assert qe[1] == pos.B_ema_open
        CAP["margin"].append(dict(s=s(pos.S), b_spot=s(pos.B_open), b_ema=s(pos.B_ema_open),
                                  cr_open_bps=s(T.cr_open_bps), expected=s(pos.M)))
    CAP["open_fee"].append(dict(n_open=s(pos.B_open), open_fee_bps=s(self.P.open_fee_bps), expected=s(pos.fee_open)))
    CAP["min_rent"].append(dict(q=s(Q), p_ema=s(self.ema), terms=[s(T.rate_base_bps), s(T.rate_s1_bps),
                                s(T.rate_s2_bps)], util_bps=s(self.util_bps), kink_bps=s(self.P.util_kink_bps),
                                min_rent_s=s(T.min_rent_s), expected=s(pos.R_min)))
    return pos


@wrap("check_exit")
def check_exit(self, pos, res, st, K):
    if default_model(self):
        P = self.P
        assert K == self.settle_reward(pos.V) and res["base"] == min(st["qs_r"][1], res["b_ref"])
        row = dict(
            kind=res["kind"], v=s(pos.V), b=s(res["B"]), r_due=s(st["R"]), b_live=s(st["qs_r"][1]),
            b_ref=s(res["b_ref"]), penalty_bps=s(pos.pen_bps), expiry_fee_bps=s(P.expiry_fee_bps),
            rent_protocol_bps=s(pos.p_rent), k_s=s(K),
            split=[s(P.pen_lender_bps), s(P.pen_protocol_bps), s(P.pen_keeper_bps)],
            expiry_keeper=[s(P.expiry_keeper_fixed), s(P.expiry_keeper_bps), s(P.expiry_keeper_cap)],
            rent=s(res["rent"]), penalty=s(res["pen"]), expiry_fee=s(res["fee"]), keeper=s(res["keeper"]),
            keeper_topup=s(res.get("keeper_topup", 0)), protocol=s(res["protocol"]), lenders=s(res["lender_sol"]),
            lump=s(res["lender_lump"]), borrower=s(res["borrower"]), k_e=s(self.expiry_keeper(res["b_ref"])))
        if CASCADE[0]:
            assert res["kind"] == "liquidation"
            row.update(delta_bps=s(res["delta"]), ert=s(pos.ert), bound=s(res["bound"]), max_in=s(res["max_in"]))
        CAP["exit_cascade" if CASCADE[0] else "exit"].append(row)
    return check_exit.__wrapped__(self, pos, res, st, K)


@wrap("try_exit")
def try_exit(self, pos, t, borrower_close=False, borrower_limit=None, settle_only=False):
    r = try_exit.__wrapped__(self, pos, t, borrower_close, borrower_limit, settle_only)
    if r and r["kind"] == "close" and default_model(self):
        CAP["close"].append(dict(v=s(pos.V), b=s(r["B"]), r_due=s(r["rent"]), rent_protocol_bps=s(pos.p_rent),
                                 protocol=s(r["protocol"]), lenders=s(r["lender_sol"]), borrower=s(r["borrower"])))
    return r


@wrap("fv_parts")
def fv_parts(self, pos, st, p, L):
    r = fv_parts.__wrapped__(self, pos, st, p, L)
    if default_model(self):
        CAP["fv"].append(dict(expired=st["expired"], v=s(pos.V), r_due=s(st["R"]), l_bps=s(pos.L),
                              penalty_bps=s(pos.pen_bps), expiry_fee_bps=s(self.P.expiry_fee_bps),
                              b_fv=None if r is None else s(r[1]), pprime=None if r is None else s(r[2]),
                              fv=None if r is None else s(r[0])))
    return r


@wrap("_settle")
def settle(self, res, kind, pos, K, st, box):
    fvp = self.fv_parts(pos, st, self.ema, self.liq) if kind in ("S2", "S3") else None
    out = settle.__wrapped__(self, res, kind, pos, K, st, box)
    if out is not None and default_model(self):
        assert out["fv"] == (None if fvp is None else fvp[0]) and K == self.settle_reward(pos.V)
        CAP["settle"].append(dict(
            # executable: the program prices the immediate part at P_ema only on a venue that
            # executes, at 0 otherwise (sol_settle; AIR-09)
            kind=kind, v=s(pos.V), k_s=s(K), q=s(pos.Q), p_ema=s(self.ema), r_due=s(st["R"]),
            executable=self.venue.has_route(),
            expired=st["expired"], l_bps=s(pos.L), penalty_bps=s(pos.pen_bps),
            expiry_fee_bps=s(self.P.expiry_fee_bps), b_fv=None if fvp is None else s(fvp[1]),
            pprime=None if fvp is None else s(fvp[2]), fv=s(out["fv"]), keeper=s(out["keeper"]),
            lenders=s(out["lender_sol"]), lump=s(out["lender_lump"]), borrower=s(out["borrower"])))
    return out


# The attack scripts import the (now instrumented) sim through `common`.
import fvcap as FC  # noqa: E402
import ptf_sandwich as PTF  # noqa: E402
import sandwich as SW  # noqa: E402
import cascade as CS  # noqa: E402


# ------------------------------------------------------------------------------------
# Branch-targeted runs (every exit still goes through the sim's own try_exit).

def underwater(key, frac_bps, size=1.0):
    """Liquidation after the minimum rent was raised to frac of the vault: V_eff drops,
    the position becomes liquidatable at the current price and the rent (then the
    penalty, then on small positions the keeper floor's top-up) is truncated by what the
    buyback leaves."""
    m = sim.new_market(key)
    pos = m.open(max(1, int(m.max_open_q() * size)), 0)
    for t in range(1, 30):
        m.step_to(t)
    pos.R_min = pos.V * frac_bps // BPS
    for t in range(30, 400):
        m.step_to(t)
        if m.status(pos, t)["liq"] and m.try_exit(pos, t):
            return


def expiry_small(key, b_target, rent_gap_bps):
    """Small position topped up as by add_margin, minimum rent set so that CR sits just
    above L at expiry: the expiry keeper reward is truncated by the borrower's rest."""
    m = sim.new_market(key)
    X, Y = m.venue.eff()
    pos = m.open(max(1, b_target * X // Y), 0)
    extra = 30_000_000
    pos.V += extra
    pos.M += extra
    b = m.status(pos, 0)["qs"][1]
    pos.R_min = pos.V - b * (pos.L + rent_gap_bps) // BPS
    t = pos.deadline - 30
    m.step_to(t)
    while t < pos.deadline + 120:
        t += 1
        m.step_to(t)
        st = m.status(pos, t)
        if (st["liq"] or st["expired"]) and m.try_exit(pos, t):
            return


def s3_after_ema_leg(key, mult, hold_s):
    """Price gap, the pool is disabled `hold_s` seconds after the EMA leg first holds,
    and S3 follows after the continuous no-route chain (FV at the frozen EMA)."""
    m = sim.new_market(key)
    pos = m.open(m.max_open_q(), 0)
    for t in range(1, 60):
        m.step_to(t)
    m.exo_move(int(pos.spot_post * mult))
    seen = None
    for t in range(60, 2_000):
        if seen is not None and t == seen + hold_s:
            sim.disable_pool(m)
        m.step_to(t)
        st = m.status(pos, t)
        if seen is None and st["ema_leg"]:
            seen = t
        if seen is not None and t > seen + hold_s and m.try_exit(pos, t):
            return


def expired_s3(key, frac):
    """S3 of an expired position that is not CR-liquidatable: FV uses the expiry fee (O15)."""
    m = sim.new_market(key)
    pos = m.open(max(1, int(m.max_open_q() * frac)), 0)
    m.step_to(pos.deadline - 10)
    sim.disable_pool(m)
    for t in range(pos.deadline - 9, pos.deadline + 1_300):
        m.step_to(t)
        if m.try_exit(pos, t):
            return


def open_ema_undefined(key, crash, size):
    """An open whose EMA-leg quote is undefined (Market.open's `qe is None` branch): the
    price falls `crash`-fold two seconds before the open, so the EMA (tau = 5 s) still stands
    far above spot and the newest ring entry's cached token reserve x is at most Q (`size`
    halves of x, in the same second as the last roll). The program's open guards (EMA
    deviation, caps), which the sim's open does not model, would refuse such an open; the
    margin rule is the sim's either way."""
    m = sim.new_market(key)
    for t in range(1, 30):
        m.step_to(t)
    m.exo_move(m.venue.spot_wad() // crash)
    m.step_to(30)
    m.step_to(31)
    x = m.ring[-1][3]
    try:
        m.open(max(x, x * size // 2), 31)
    except ValueError:        # the live pool cannot pay the sale out
        pass


def run_all():
    for key in sim.MARKETS:
        for sc in sim.SCENARIOS:
            sim.run(sim.new_market(key), 1.0, sc)
    for key in ("A", "D@50%", "RA", "RB"):
        for sc in sim.SCENARIOS[:7]:
            sim.run(sim.new_market(key), 0.3, sc)
    # Small positions: the liquidation keeper floor tops the keeper up (N22, ECON §8.5).
    for key in ("A", "B", "C", "D@0%", "D@50%", "RA", "RC"):
        for frac in (0.1, 0.03, 0.01):
            for name in ("gap 2x", "pump 10x/10min", "pump 10x/60s"):
                sim.run(sim.new_market(key), frac, sim.SC[name])
    # Borrower closes at several sizes, moves and ages (rent split at the position's share).
    for key in ("A", "B", "C", "D@0%", "D@50%", "RA", "RB", "RC"):
        for size in (1.0, 0.3, 0.05):
            for k in (0.5, 0.8, 1.2, 1.4):
                sim.run(sim.new_market(key), size, sim.Scenario(f"close {k}", sim.ramp_path(sim.TE, k, 600),
                                                                  close_at=sim.TE + 400, window=900))
    for key in ("A", "C", "RB"):
        for util in (8_000, 9_000):
            sim.run(sim.new_market(key, util_bps=util), 0.5,
                    sim.Scenario("close after 3 days", lambda t: None, t_event=10**9, window=0,
                                 close_at=3 * 86_400, horizon=3 * 86_400 + 60))
    for key in ("D@80%", "D@90%", "D@97%"):
        for sc in sim.GRAD_SCENARIOS:
            sim.run(sim.new_market(key), 1.0, sc)
    for key in ("RA", "RB", "RC"):
        for sc in sim.FAST_SCENARIOS:
            sim.run(sim.new_market(key), 1.0, sc)
    # rent-mode variants: higher utilisation and a different kink
    for key in ("A", "C", "D@50%", "RB"):
        for util in (9_000, 8_123):
            sim.run(sim.new_market(key, util_bps=util), 1.0, sim.SC["gap 2x"])
    # ECON §13's exit fuzz (ARCH §8.1): 1,000 runs, seed 20260927; the rows are sampled below.
    stats = sim.fuzz_exits(1_000, SEED)
    assert stats == ECON_13_EXIT_FUZZ, stats
    for key in ("A", "C", "D@0%", "RA"):
        for frac in (3_500, 4_200, 4_600, 4_800, 5_000, 5_300, 6_000):
            underwater(key, frac)
    for key in ("A", "C", "RA"):                         # the top-up truncated by the borrower's rest
        for frac in (4_500, 4_600, 4_650, 4_700, 4_725, 4_750):
            underwater(key, frac, 0.05)
    for key in ("A", "B", "RA"):
        for b_target, gap in ((3_000_000, 150), (2_000_000, 50), (8_000_000, 300), (50_000_000, 200)):
            expiry_small(key, b_target, gap)
    for key in ("A", "B", "C", "RA"):
        for mult, hold in ((2.0, 1), (2.5, 2), (3.0, 3), (1.8, 5), (4.0, 1)):
            s3_after_ema_leg(key, mult, hold)
    for key in ("A", "C", "D@0%", "RA"):
        for frac in (1.0, 0.33):
            expired_s3(key, frac)
    # attacks/fvcap.py: drained-pool model cases (S1 with the cap unused, S2 with the cap binding).
    for key, drain in (("A", 8000), ("B", 8000), ("RA", 8000), ("RA", 9500), ("A", 9500), ("C", 9500)):
        for k in (1.0, 1.4, 2.0):
            FC.case(key, drain, k)
    # attacks/sandwich.py and ptf_sandwich.py: sandwiched liquidations (truncated rent / penalty).
    for key in ("A", "C", "D@0%", "RA"):
        m, pos, t, _k = SW.setup_liq(key)
        SW.attack(m, pos, t, ("liquidation",), "atomic")
    for key in ("A", "RA"):
        PTF.run(key, 2.0, 4, 6, sim.DEFAULT)
    # attacks/cascade.py (ARCH §8.1): honest liquidation baskets on markets filled to the OI
    # cap, each buyback moving the price the next position's fixed-delta bound is checked
    # against. Its buyback exits (with their bounds) form their own section; every other
    # capture of these runs is dropped, so the samples above do not depend on them.
    before = {k: len(v) for k, v in CAP.items()}
    CASCADE[0] = True
    for key in ("A", "RA", "C"):
        for path in CS.PATHS.values():
            CS.run_basket(key, path)
    CASCADE[0] = False
    for k in list(CAP):
        if k != "exit_cascade":
            del CAP[k][before.get(k, 0):]
    # D4 with an undefined EMA-leg quote: no run above reaches it; only the margin rows of
    # these targeted opens are kept (their open-fee and R_min rows are dropped, so the
    # samples above do not depend on them).
    assert not CAP["margin_ema_undefined"]
    before = {k: len(v) for k, v in CAP.items()}
    for key in ("A", "B", "C", "RA", "RB", "RC"):
        for crash in (1_000, 2_000, 5_000):
            for size in (2, 3):
                open_ema_undefined(key, crash, size)
    for k in list(CAP):
        if k != "margin_ema_undefined":
            del CAP[k][before.get(k, 0):]
    return stats


def gen_keepers(rng):
    """Market.settle_reward / Market.expiry_keeper on random amounts, at the defaults and at
    random terms inside the compiled bounds (ECON §15)."""
    out = []
    for i in range(300):
        P = sim.DEFAULT
        if i % 2:
            P = replace(sim.DEFAULT, settle_fixed=rng.randrange(0, 10_000_001), settle_bps=rng.randrange(0, 101),
                        settle_cap=rng.randrange(0, 250_000_001), expiry_keeper_fixed=rng.randrange(0, 10_000_001),
                        expiry_keeper_bps=rng.randrange(0, 101), expiry_keeper_cap=rng.randrange(0, 250_000_001))
        m = sim.new_market("A", P)
        v = rng.choice([0, 1, 10_000_000, rng.randrange(0, 2**64), rng.randrange(0, 10**11)])
        b = rng.choice([0, 1, rng.randrange(0, 2**64), rng.randrange(0, 10**11)])
        out.append(dict(settle_terms=[s(P.settle_fixed), s(P.settle_bps), s(P.settle_cap)],
                        expiry_terms=[s(P.expiry_keeper_fixed), s(P.expiry_keeper_bps), s(P.expiry_keeper_cap)],
                        v=s(v), k_s=s(m.settle_reward(v)), b_ref=s(b), k_e=s(m.expiry_keeper(b))))
    return out


def pick(rng, rows, n, key=None):
    """Deterministic, de-duplicated sample of at most n rows, stratified by `key`: the
    smallest groups are taken whole and their unused quota goes to the larger ones."""
    uniq = sorted({json.dumps(r, sort_keys=True) for r in rows})
    groups = defaultdict(list)
    for u in uniq:
        r = json.loads(u)
        groups[key(r) if key else 0].append(r)
    out = []
    left, todo = n, sorted(groups, key=lambda g: (len(groups[g]), str(g)))
    for i, g in enumerate(todo):
        per = max(1, left // (len(todo) - i))
        rows_g = groups[g]
        take = rows_g if len(rows_g) <= per else rng.sample(rows_g, per)
        out.extend(take)
        left -= len(take)
    return out


def main() -> None:
    stats = run_all()
    rng = random.Random(SEED)

    def exit_class(r):
        if r["kind"] == "liquidation":
            full = -((-int(r["penalty_bps"]) * min(int(r["b_live"]), int(r["b_ref"]))) // BPS)
            kp = int(r["penalty"]) * int(r["split"][2]) // BPS
            topup = int(r["keeper_topup"])
            return ("liq", r["rent"] != r["r_due"], r["penalty"] != str(full), topup > 0,
                    0 < topup < int(r["k_s"]) - kp)
        return ("exp", r["rent"] != r["r_due"], r["keeper"] != r["k_e"])

    def settle_class(r):
        capped = r["fv"] is not None and int(r["fv"]) < int(r["v"]) - int(r["k_s"])
        return (r["kind"], capped, r["lump"] != "0", r["pprime"] == r["expiry_fee_bps"])

    def fv_class(r):
        return (r["expired"], r["b_fv"] is None, r["pprime"] == r["expiry_fee_bps"])

    out = {
        "generator": Path(__file__).name,
        "source": "docs/design/sim/shortwin_sim.py v0.4.1 (default switches): Market.status, slip_at, open, "
                  "check_exit, try_exit, fv_parts, _settle, settle_reward, expiry_keeper, captured while running "
                  "SCENARIOS, GRAD_SCENARIOS, FAST_SCENARIOS, fuzz_exits(1000, 20260927), attacks/fvcap.py, "
                  "attacks/sandwich.py, attacks/ptf_sandwich.py, attacks/cascade.py and branch-targeted runs",
        "spot_leg": pick(rng, CAP["spot_leg"], 400, lambda r: (r["executable"], r["fresh"], r["expected"])),
        "cr": pick(rng, CAP["cr"], 400, lambda r: (r["b_hat"] is None, r["below"])),
        "ema_leg": pick(rng, CAP["ema_leg"], 150, lambda r: (r["expected"], r["run"])),
        "buyback": pick(rng, CAP["buyback"], 300, lambda r: (r["expected"], r["b_live"] is None)),
        "sigma": pick(rng, CAP["sigma"], 400, lambda r: r["n"] is None),
        "margin": pick(rng, CAP["margin"], 300),
        "open_fee": pick(rng, CAP["open_fee"], 200),
        "min_rent": pick(rng, CAP["min_rent"], 200),
        "exit": pick(rng, CAP["exit"], 500, exit_class),
        "close": pick(rng, CAP["close"], 100),
        "fv": pick(rng, CAP["fv"], 400, fv_class),
        "settle": pick(rng, CAP["settle"], 300, settle_class),
        "exit_cascade": pick(rng, CAP["exit_cascade"], 40, exit_class),
        "keepers": gen_keepers(random.Random(SEED + 1)),
        "exit_fuzz": {k: str(stats[k]) for k in ("runs", "exits", "penalty_truncated_by_sandwich")},
        # Picked last, so the samples above keep their draws.
        "margin_ema_undefined": pick(rng, CAP["margin_ema_undefined"], 40),
    }
    # Every branch must be present.
    kinds = {exit_class(r) for r in out["exit"]}
    for need in (("liq", False, False, False, False), ("liq", False, True), ("liq", True, True),
                 ("liq", False, False, True, False), ("liq", False, False, True, True),
                 ("exp", False, False), ("exp", False, True)):
        assert any(k[:len(need)] == need for k in kinds), (need, sorted(kinds))
    skinds = {settle_class(r) for r in out["settle"]}
    # (a capped take always vests something: FV = B_FV(1 + p') + R_due > Q*P_ema + R_due; an S1
    # vests its take above Q*P_ema + R_due, AIR-21, and a genuine S1 loss nothing)
    for need in (("S1", False, False), ("S1", False, True), ("S2", True, True), ("S3", True, True),
                 ("S3", False, False), ("S3", False, True)):
        assert any(k[:len(need)] == need for k in skinds), (need, sorted(skinds))
    assert any(k[3] for k in skinds if k[0] == "S3"), "an S3 at the expiry-fee p' (O15)"
    # S3 on an executable venue (flag or fee cap: P_ema immediate) and on one that cannot execute
    # (all but R_due vests, AIR-09)
    assert {r["executable"] for r in out["settle"] if r["kind"] == "S3"} == {True, False}
    fkinds = {fv_class(r) for r in out["fv"]}
    assert (True, False, True) in fkinds and (True, False, False) in fkinds and (False, False, False) in fkinds
    for cat in ("spot_leg", "ema_leg", "buyback"):
        assert {r["expected"] for r in out[cat]} == {True, False}, cat
    assert any(not r["executable"] for r in out["spot_leg"])
    assert out["close"]
    assert len(out["margin_ema_undefined"]) >= 20
    assert all(r["b_ema"] is None for r in out["margin_ema_undefined"])
    Path(__file__).with_name("exits.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
