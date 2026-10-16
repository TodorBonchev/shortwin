#!/usr/bin/env python3
"""Vectors for shortwin_math::settle and whole-market traces of observe / persist / bounds /
settle (ARCHITECTURE §2.2, §3.6-§3.8, §4.4, §4.5, §5.3; ECONOMICS §5, §8.2, §8.6, §10.3,
§12.9-§12.11, §13) from sim/shortwin_sim.py v0.4.1 with its default switches and the attack
scripts of sim/attacks/.

The sim files are never modified: Market is instrumented in THIS process (every wrapper only
reads state and calls pure sim functions) and the sim then runs its own code:

* units_settle.json   the sim's settle_box and _s3_due on synthetic states (settle_box on a
                      venue that cannot execute, `box`, and on an executable one, `box_exec`:
                      only the former collapses to the EMA point, M2-SEC-1; `box_cap`: with
                      a depth cap on L_mhi, AIR-15);
  units_evaluate_*.json  try_exit(settle_only=True) on synthetic states (stubbed venue quotes drawn around every decision
                      boundary: the S1 budget, sigma_max, the liquidation CR, fv_dev_bps, the
                      arm gap and settle_persist_n, a migrating venue's FV pool).
* trace_*.json        full-replay traces: every observation, roll, venue change, persistence
                      check, exit reference, max_in and settlement evaluation of one market
                      lineage, replayed operation by operation by tests/settle_vectors.rs:
                        attacks    arming.py, block_spot.py (a), refute_push.py (a)-(d),
                                   fv_push.py (M) up / flip / down / push-then-freeze /
                                   consecutive pushes, (V) p' veto, (E) Emergency-flag S3,
                                   fixed_delta.py (d), ptf_sandwich.py, venue_switch.py
                        scenarios  ECON §11.2 scenarios (gaps, pumps, drain, spike, disabled
                                   pool, Emergency flag, expiry, stuck migration), genuine
                                   reversals of an armed S1 (refutation, "not liquidatable")
                                   and S3 on a halted pool (the box at the EMA point)
                        random     the first runs of the exit fuzz, whole
                      A lineage forked from another (copy.deepcopy in the attack scripts) is
                      exported as a suffix with "base" naming the trace it continues.
* fuzz_*.json         ECON §13's exit fuzz, fuzz_exits(1000, 20260927) (its statistics are
                      asserted against ECON_13_EXIT_FUZZ): decision-only lines, each self-contained
                      (box, legs, quotes, arm state before and after, outcome): every
                      arming, re-arming, disarm, settlement and
                      amount wait, every evaluation of an armed position whose legs do not
                      show plain S1 (vetoes, stale buckets, S2), every 4th plain S1 tick and
                      every 8th evaluation of an unarmed position; every max_in.

Operations (whitespace-separated decimal tokens; "-" None, "1"/"0" booleans, "." not
recorded; tests/settle_vectors.rs documents each layout next to its parser):

  prm     dtmax clamp slow_tau slow_dtmax | kappa_liq kappa_base delta_liq delta_exp growth_n
          ref_max_age | liq_n eps expiry_fee | settle_n arm_gap fv_dev | s3_wait grace pause
  init    t p l
  obs     t gap [p l]                      (gap: pre-roll gap, "-" if nothing rolled)
  rewrite t g|r l_new                      (graduation | Raydium fast path, v0.4.1 rules: _switch_to_pool, fast_switch;
                                            no trace emits "g" since M6, #105)
  rewrite t R l_new slow_cap               (a restart: rewrite_liquidity(l_new, RaydiumLocked { slow_cap }), then
                                            observe::mark_restart (ema_warm = 8, ema_guard = 1); the next 8 rolls
                                            are warm-up integrations (their seed bounded both ways, M6-SEC-1); chk's
                                            leg is the pure EMA-leg predicate, the gate is try_exit's: no ref /
                                            maxin / settle line while ema_warm > 0 on an executable venue.  Emitted
                                            by the Raydium adoption (M4R: Market.adopt_pool, l_new =
                                            floor(D*1e4/lambda_min), slow_cap = D) and by the -> PumpSwap graduation
                                            as built (M6, ECON #104, #105: Market._graduate, l_new = slow_cap = the
                                            pool's lp_supply): inside a prologue it follows that second's "obs t gap"
                                            (the roll; the transitioning instruction records nothing); the straddle
                                            bundle's tx3 emits it alone, then the pool's first "obs".  M6R-1's cap
                                            of L_slow at D during a PumpSwap warm-up never binds in these traces
                                            (asserted: Market._warmup_slow_cap), so it has no op)
  pos     pid Q V K_s ert L penalty slip_max
  chk     t pid flags leg p_live p_rs lo_ok g P_mlo P_mhi L_mlo L_mhi
                                           (flags: EMA-leg predicate on each ring entry's
                                            cached (x, y), newest first)
  ref     t pid is_liq l_live L_b P_ref cap
                                           (cap: Market.ref_depth_cap, the pool's locked depth D on a Raydium
                                            venue (since AIR-14; floor(D*1e4/pool_lock_min_bps) before) and a
                                            canonical PumpSwap pool (M6R-1; lambda_min = PUMPSWAP_LOCK_MIN_BPS
                                            since OWNER-M6D-1 / M6E-1: D itself), "-" elsewhere; M4F-1: L_b =
                                            max(L_live, min(max(L_ema', L_slow), cap)))
  maxin   liq b_ref delta V K_s ert max_in | exp b_ref delta eps fee V K_s R ert pi max_in
  settle  t pid path deadline | ema_count P_ema L_ema ema_ts | P_lo P_hi L_lo L_hi
          | R cr_liq ema_leg exec p_rs L_live b_live sigma_live
          | no_route_now no_route_since graduated_at migrating migration_seen_at
          | cA sA fA cB fB sC cD sD fE    (A (P_lo,L_hi), B (P_hi,L_lo), C (P_hi,min(L_live,L_lo)),
                                           D (p_rs,L_live), E (P_ema,L_ema); c cost, s sigma, f B_FV)
          | nl refuted s1 s2              (the sim's intermediate decisions, "." if not evaluated)
          | armed count since last  | armed count since last       (before | after)
          | kind lenders borrower fv p' x_lo fv_lo p'_lo x_hi fv_hi p'_hi | wait_s2 wait_s3
          path N: the not-liquidatable branch of an armed position; S: _settlement.
  dcap    t cap                            (AIR-15: Market.ref_depth_cap, the cap of the box's L_mhi, for the
                                            settle line that follows at the same t; emitted in the full-replay
                                            traces only, and only when it binds: L_hi < max(L_ema, L_mhi))
  state   P_ema L_ema L_slow ema_ts slow_ts count | b_sec b_lo b_hi b_llo b_lhi
          | lb_sec lb_lo lb_hi | n | n x (ts p l x y lo hi llo lhi)

Run from anywhere: python3 crates/shortwin-math/tests/vectors/settle/gen_settle.py
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
SIM_DIR = ROOT / "docs" / "design" / "sim"
sys.path.insert(0, str(SIM_DIR))
sys.path.insert(0, str(SIM_DIR / "attacks"))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)

HERE = Path(__file__).resolve().parent
SEED = 20260927
U64 = 2**64 - 1
U128 = 2**128 - 1
BPS = sim.BPS
WAD = sim.WAD
M = sim.Market
MAX_FILE = 950_000
# ECON §13 "Fuzz results" (v0.4.1): the exit fuzz whose decisions fuzz_*.json holds, as AIR-22 (the
# sustained growth as the lower median of the newest kappa_growth_n step rates) moved it from ECON's
# published row (gen_exits.py's ECON_13_EXIT_FUZZ lists both).
ECON_13_EXIT_FUZZ = {'runs': 1000, 'exits': 697, 'sandwich_attempts': 8948, 'bundles': 253491,
                     'attacker_only_seconds': 772, 'blocker_runs': 192, 'noisy_runs': 490, 'freezer_runs': 267,
                     'frozen_seconds': 2905, 'penalty_truncated_by_sandwich': 22, 'max_loss_over_bref_bps': 102,
                     'max_settle_box_bps': 94, 'reserve_waits_liq': 0, 'reserve_waits_exp': 0,
                     'kinds': {'liquidation': 224, 'expiry': 281, 'S1': 147, 'S2': 10, 'S3': 35}}
ORIG = {name: getattr(M, name) for name in (
    "__init__", "observe", "_integrate", "_switch_to_pool", "_graduate", "_warmup_slow_cap", "fast_switch",
    "adopt_pool", "reset_ema_to_spot", "open", "status", "exit_reference", "_buyback_exit", "try_exit", "settle_box",
    "_settlement", "_not_liq_box", "_refuted", "_legs", "_settle", "quote_repriced")}
MODE = {"full": True, "sample": None, "copies": False}
CAPTURE = []
ROOTS = []


def tok(x):
    if x is None:
        return "-"
    if isinstance(x, bool):
        return "1" if x else "0"
    return str(x)


def line(*xs):
    return " ".join(tok(x) for x in xs)


def sig(x):
    """sigma for the vectors: INF (undefined) is "-", the rest clamped into [0, u64::MAX]
    (every comparison is with sigma_max < 2^32, so the clamp changes no decision)."""
    if x is None or x >= sim.INF:
        return None
    return min(max(x, 0), U64)


def cost(q):
    return None if q is None else q[1]


def ring_tokens(ring):
    out = [len(ring)]
    for e in ring:
        assert e[3] <= U128 and e[4] <= U128
        out += list(e[:9])
    return out


def market_state(m):
    b = [None] * 5 if m.b_sec is None else [m.b_sec, m.b_lo, m.b_hi, m.b_llo, m.b_lhi]
    lb = [None] * 3 if m.lb_sec is None else [m.lb_sec, m.lb_lo, m.lb_hi]
    return line(m.ema, m.liq, m.liq_slow, m.ema_t, m.slow_t, m.ema_count, *b, *lb, *ring_tokens(m.ring))


def arm(pos):
    return (pos.s_kind is not None, pos.s_count, pos.s_t, pos.s_last)


# ---------------------------------------------------------------------------------------
# Trace recording
# ---------------------------------------------------------------------------------------
class Trace:
    """Append-only op list whose deepcopy forks in O(1): the recorded prefix is frozen
    and shared, each copy continues on its own tail."""
    __slots__ = ("parent", "ops")

    def __init__(self, parent=None):
        self.parent = parent
        self.ops = []

    def __deepcopy__(self, memo):
        if self.ops:
            self.parent = (self.parent, tuple(self.ops))
            self.ops = []
        return Trace(self.parent)

    def flatten(self):
        chunks, node = [], self.parent
        while node is not None:
            chunks.append(node[1])
            node = node[0]
        out = []
        for c in reversed(chunks):
            out.extend(c)
        out.extend(self.ops)
        return out


def prm_line(m):
    P, T = m.P, m.tier
    # Only the normative v0.4.1 readings (ARCH §2.2: vectors use the default switches).
    assert (T.ema_tau_s == 5 and P.ring_len == 8 and not P.box_obs and not P.pprime_raw and P.ref_cap_lo
            and P.liq_reserve == "none" and P.exp_reserve == "rent+fee" and P.fv_cap and P.exit_ref == "fresh"
            and P.liq_keeper_parity and P.two_leg and P.synthetic), P
    return line("prm", T.ema_dtmax_s, T.ema_clamp_bps, P.liq_slow_tau_s, P.liq_slow_dtmax_s, "|",
                P.kappa_liq_bps, P.kappa_base_bps, P.delta_liq_bps, P.delta_exp_bps, P.kappa_growth_n,
                P.ref_max_age_s, "|", P.liq_persist_n, P.exec_tol_bps, P.expiry_fee_bps, "|", P.settle_persist_n,
                P.settle_arm_gap_s, P.fv_dev_bps, "|", P.s3_wait_s, P.grad_grace_s, P.max_migration_pause_s)


def emit(m, op):
    tr = getattr(m, "_trace", None)
    if tr is None:
        return
    p = prm_line(m)
    if p != getattr(m, "_prm", None):
        m._prm = p
        tr.ops.append(p)
    tr.ops.append(op)


def m_deepcopy(self, memo):
    """copy.deepcopy's default for a plain object, plus the capture list."""
    cls = self.__class__
    new = cls.__new__(cls)
    memo[id(self)] = new
    for k, v in self.__dict__.items():
        setattr(new, k, copy.deepcopy(v, memo))
    if MODE["copies"]:
        CAPTURE.append(new)
    return new


def p_init(self, venue, tier, P=sim.DEFAULT, util_bps=8000, t0=0):
    ORIG["__init__"](self, venue, tier, P, util_bps, t0)
    self._trace = Trace()
    self._ctx = None
    self._capq = None
    self._gap = None
    self._nobs = 0
    self._nstatus = 0
    self._pids = set()
    CAPTURE.append(self)
    if MODE["full"]:
        emit(self, line("init", t0, self.ema, self.liq))


def p_integrate(self, t):
    self._gap = t - self.ema_t
    ORIG["_integrate"](self, t)


def p_observe(self, t):
    self._gap = None
    self._sync_op = None
    self._in_observe = True
    try:
        ORIG["observe"](self, t)
    finally:
        self._in_observe = False
    if not MODE["full"]:
        return
    if self._sync_op is not None:
        # M6 (#104, #105): the prologue rolled, then moved the market -> PumpSwap (Market._graduate); it records nothing
        emit(self, line("obs", t, self._gap))
        emit(self, self._sync_op)
        return
    v = self.venue
    rec = v.has_price() and (v.has_route() or self.migrating())
    obs = (v.spot_wad(), v.sqrt_k()) if rec else ()
    assert all(x > 0 for x in obs), "a zero observation (the crate refuses it)"
    emit(self, line("obs", t, self._gap, *obs))
    self._nobs += 1
    if self._nobs % 64 == 0:
        emit(self, "state " + market_state(self))


def p_switch(self, pool, t):
    ORIG["_switch_to_pool"](self, pool, t)
    if MODE["full"]:
        emit(self, line("rewrite", t, "g", pool.sqrt_k()))


def p_graduate(self, pool, t):
    ORIG["_graduate"](self, pool, t)
    if MODE["full"]:
        op = line("rewrite", t, "R", self.liq, self.liq)     # the anchor = lp_supply, slow_cap = the anchor
        if getattr(self, "_in_observe", False):
            self._sync_op = op                               # after the prologue's roll (p_observe)
        else:
            emit(self, op)                                   # the straddle bundle's tx3


def p_warmup_slow_cap(self):
    before = self.liq_slow
    ORIG["_warmup_slow_cap"](self)
    # M6R-1's warm-up cap of L_slow at D binds only after a deposit around the transition, which no trace has;
    # a binding cap would need an op of its own
    assert self.liq_slow == before, ("the PumpSwap warm-up's L_slow cap bound in a trace", before, self.liq_slow)


def p_fast(self, pool, t):
    l_new = pool.sqrt_k()
    ORIG["fast_switch"](self, pool, t)
    if MODE["full"]:
        emit(self, line("rewrite", t, "r", l_new))


def p_adopt(self, pool, t):
    ORIG["adopt_pool"](self, pool, t)
    if MODE["full"]:
        emit(self, line("rewrite", t, "R", self.liq, pool.locked_depth()))


def p_reset(self, t):
    raise RuntimeError("reset_ema_to_spot (Monte Carlo shortcut) is not part of the traced model")


def p_open(self, Q, t):
    pos = ORIG["open"](self, Q, t)
    pid = pos.t_open
    assert pid not in self._pids, "two positions opened in one second"
    self._pids.add(pid)
    emit(self, line("pos", pid, pos.Q, pos.V, self.settle_reward(pos.V), pos.ert, pos.L, pos.pen_bps,
                    pos.slip_max))
    return pos


def p_quote_repriced(self, Q, p_wad, L):
    cap = getattr(self, "_capq", None)
    if cap is not None:
        cap.append((p_wad, L))
    return ORIG["quote_repriced"](self, Q, p_wad, L)


def p_status(self, pos, t):
    st = self._stub_st if hasattr(self, "_stub_st") else ORIG["status"](self, pos, t)
    ctx = self._ctx
    if ctx is not None and ctx["st"] is None:
        ctx["st"] = st
    if not MODE["full"]:
        return st
    self._nstatus += 1
    if ctx is not None or self._nstatus % 16 == 0:
        Veff, L = st["Veff"], pos.L
        flags = []
        for e in reversed(self.ring):
            q = self.quote_xy(pos.Q, e[3], e[4])
            flags.append("1" if (q is None or Veff * BPS // q[1] < L) else "0")
        emit(self, line("chk", t, pos.t_open, "".join(flags), st["ema_leg"], st["p_live"], st["p_rs"], st["lo_ok"],
                        self.ema_growth_bps(), *self.ring_medians()))
    return st


def p_exit_ref(self, pos, st, t, is_liq):
    cap = []
    self._capq = cap
    try:
        r = ORIG["exit_reference"](self, pos, st, t, is_liq)
    finally:
        self._capq = None
    assert len(cap) <= 1
    pref, lb = cap[0] if cap else (None, None)
    if r is not None:
        assert r[2] == pref
    self._last_ref = r
    if MODE["full"]:
        emit(self, line("ref", t, pos.t_open, is_liq, st["L_live"], lb, pref, self.ref_depth_cap()))
    return r


def max_in_liq(b_ref, delta, V, K, ert):
    """Transcription of Market._buyback_exit, liq_reserve = "none"."""
    return min(b_ref * (BPS + delta) // BPS, V - K - ert)


def max_in_exp(b_ref, delta, eps, fee, V, K, R, ert):
    """Transcription of Market._buyback_exit, exp_reserve = "rent+fee"."""
    return min(b_ref * (BPS + delta) // BPS, V - K - ert,
               max(V - R - sim.cdiv(fee * b_ref, BPS) - ert, b_ref * (BPS + eps) // BPS))


def p_buyback(self, pos, st, t, res, K):
    self._last_ref = None
    r = ORIG["_buyback_exit"](self, pos, st, t, res, K)
    ref = self._last_ref
    if ref is not None:
        P = self.P
        b_ref, delta = ref[0][1], ref[1]
        if st["liq"]:
            mi = max_in_liq(b_ref, delta, pos.V, K, pos.ert)
            op = line("maxin", "liq", b_ref, delta, pos.V, K, pos.ert, mi if mi >= 0 else None)
        else:
            mi = max_in_exp(b_ref, delta, P.exec_tol_bps, P.expiry_fee_bps, pos.V, K, st["R"], pos.ert)
            pi = sim.cdiv(P.expiry_fee_bps * b_ref, BPS)
            op = line("maxin", "exp", b_ref, delta, P.exec_tol_bps, P.expiry_fee_bps, pos.V, K, st["R"], pos.ert,
                      pi, mi if mi >= 0 else None)
        if r is not None:
            assert r["max_in"] == mi, (r["max_in"], mi)
        else:
            assert st["qs"][1] > mi, "a buyback that waited below its max_in"
        emit(self, op)
    return r


def binding_cap(m, t, b):
    """AIR-15: Market.ref_depth_cap() if it binds on the box b that settle_box(t) returned (b's L_hi below the
    uncapped max(L_ema, L_mhi)), else None (no cap, a cap above L_mhi or L_ema, or the EMA point)."""
    if hasattr(m, "_stub_box") or (t - m.ema_t > m.P.ring_len and not m.venue.has_route()):
        return None
    cap = m.ref_depth_cap()
    if cap is None or b[3] == max(m.liq, m.ring_medians()[3]):
        return None
    assert b[3] == max(m.liq, cap) < max(m.liq, m.ring_medians()[3]), (b, cap)
    return cap


def p_settle_box(self, t):
    b = self._stub_box if hasattr(self, "_stub_box") else ORIG["settle_box"](self, t)
    if self._ctx is not None and self._ctx["box"] is None:
        self._ctx["box"] = b
        self._ctx["dcap"] = binding_cap(self, t, b)
    return b


def p_settlement(self, pos, st, t, K, res, box):
    if self._ctx is not None:
        self._ctx["path"] = "S"
    return ORIG["_settlement"](self, pos, st, t, K, res, box)


def p_not_liq(self, pos, st, box):
    r = ORIG["_not_liq_box"](self, pos, st, box)
    if self._ctx is not None:
        self._ctx["nl"] = r
    return r


def p_refuted(self, pos, st, box, K):
    r = ORIG["_refuted"](self, pos, st, box, K)
    if self._ctx is not None:
        self._ctx["refuted"] = r
    return r


def p_legs(self, pos, st, box, K):
    r = ORIG["_legs"](self, pos, st, box, K)
    if self._ctx is not None:
        self._ctx["legs"] = r
    return r


def p_settle(self, res, kind, pos, K, st, box):
    take = pos.V - K
    info = {"kind": kind}
    if kind in ("S2", "S3"):
        parts = [self.fv_parts(pos, st, box[0], box[3]), self.fv_parts(pos, st, box[1], box[2]),
                 self.fv_parts(pos, st, self.ema, self.liq)]
        info["corners"] = [(take if f is None else min(take, f[0]), f and f[0], f and f[2]) for f in parts]
    r = ORIG["_settle"](self, res, kind, pos, K, st, box)
    info["r"] = None if r is None else dict(r)
    if self._ctx is not None:
        self._ctx["settles"].append(info)
    return r


def quote_table(m, pos, st, bx, path, executable):
    """The quotes the crate may ask for at this evaluation (module docstring), from the
    sim's own quote_repriced / slip_repriced / fv_parts ("." where it cannot ask)."""
    P_lo, P_hi, L_lo, L_hi = bx
    Q = pos.Q
    dot = "."

    def c(p, L):
        return cost(m.quote_repriced(Q, p, L))

    def s(p, L):
        return sig(m.slip_repriced(Q, p, L))

    def f(p, L):
        r = m.fv_parts(pos, st, p, L)
        return None if r is None else r[1]
    if path == "N":
        return [dot, dot, dot, c(P_hi, L_lo), dot, dot, dot, dot, dot]
    if not executable:
        return [dot, dot, f(P_lo, L_hi), dot, f(P_hi, L_lo), dot, dot, dot, f(m.ema, m.liq)]
    L2 = min(st["L_live"], L_lo)
    p_rs = st["p_rs"]
    d = [c(p_rs, st["L_live"]), s(p_rs, st["L_live"])] if p_rs is not None else [dot, dot]
    return [c(P_lo, L_hi), s(P_lo, L_hi), f(P_lo, L_hi), c(P_hi, L_lo), f(P_hi, L_lo), s(P_hi, L2), *d,
            f(m.ema, m.liq)]


def settle_line(m, pos, t, path, st, bx, ctx, before, after):
    v = m.venue
    executable = v.has_route()
    qs_r = st["qs_r"]
    sig_live = sig(m.slip_at(pos.Q, *v.eff(), live=True)) if executable else "."
    legs = ctx["legs"]
    b01 = lambda x: "." if x is None else x
    settled = [i for i in ctx["settles"] if i["r"] is not None]
    assert len(settled) <= 1
    out = [None] * 11
    if settled:
        i = settled[0]
        r = i["r"]
        out[:3] = [i["kind"], r["lender_sol"], r["borrower"]]
        assert r["lender_sol"] + r["borrower"] + r["keeper"] == pos.V
        if i["kind"] != "S1":
            (xl, fl, pl), (xh, fh, ph), (xe, fe, pe) = i["corners"]
            assert r["fv"] == fe and r["lender_sol"] == xe
            out[3:] = [fe, pe, xl, fl, pl, xh, fh, ph]
    w2 = any(i["kind"] == "S2" and i["r"] is None for i in ctx["settles"])
    w3 = any(i["kind"] == "S3" and i["r"] is None for i in ctx["settles"])
    return line("settle", t, pos.t_open, path, pos.deadline, "|", m.ema_count, m.ema, m.liq, m.ema_t, "|", *bx, "|",
                st["R"], st["liq"], st["ema_leg"], executable, st["p_rs"], st["L_live"], cost(qs_r), sig_live, "|",
                m.s3_no_route(), m.no_route_since, m.graduated_t, m.migrating(), m.complete_t, "|",
                *quote_table(m, pos, st, bx, path, executable), "|",
                b01(ctx["nl"]), b01(ctx["refuted"]), b01(legs and legs[0]), b01(legs and legs[1]), "|",
                *before, "|", *after, "|", *out, "|", w2, w3)


SAMPLE = {"idle": 0, "tick": 0}


def keep_line(before, after, ctx):
    """Decision-only mode (the exit fuzz): every arming, re-arming, disarm, settlement and
    amount wait, every evaluation of an armed position whose legs do not show plain S1
    (vetoes, stale buckets, S2), 1 in 4 plain S1 ticks and 1 in MODE["sample"] evaluations
    of an unarmed position that stays unarmed. MODE["sample"] None keeps everything."""
    if MODE["sample"] is None or ctx["settles"] or before[0] != after[0] \
            or (after[0] and before[1:3] != after[1:3]):
        return True
    legs = ctx["legs"]
    if before[0]:
        if not (legs and legs[0] and not legs[1]):
            return True
        SAMPLE["tick"] += 1
        return SAMPLE["tick"] % 4 == 0
    SAMPLE["idle"] += 1
    return SAMPLE["idle"] % MODE["sample"] == 0


def p_try_exit(self, pos, t, borrower_close=False, borrower_limit=None, settle_only=False):
    if borrower_close:
        return ORIG["try_exit"](self, pos, t, borrower_close, borrower_limit, settle_only)
    before = arm(pos)
    ctx = {"st": None, "box": None, "dcap": None, "path": None, "nl": None, "refuted": None, "legs": None,
           "settles": []}
    prev, self._ctx = self._ctx, ctx
    try:
        r = ORIG["try_exit"](self, pos, t, borrower_close, borrower_limit, settle_only)
    finally:
        self._ctx = prev
    st, after = ctx["st"], arm(pos)
    path = ctx["path"]
    if path is None and not (st["liq"] or st["expired"]) and before[0]:
        path = "N"
    if path is None:
        return r
    if not MODE["full"] and path == "S" and not keep_line(before, after, ctx):
        return r
    if MODE["full"] and ctx["dcap"] is not None:
        emit(self, line("dcap", t, ctx["dcap"]))
    emit(self, settle_line(self, pos, t, path, st, ctx["box"], ctx, before, after))
    return r


PATCHES = {"__init__": p_init, "observe": p_observe, "_integrate": p_integrate, "_switch_to_pool": p_switch,
           "_graduate": p_graduate, "_warmup_slow_cap": p_warmup_slow_cap, "fast_switch": p_fast,
           "adopt_pool": p_adopt, "reset_ema_to_spot": p_reset, "open": p_open, "status": p_status,
           "exit_reference": p_exit_ref, "_buyback_exit": p_buyback, "try_exit": p_try_exit,
           "settle_box": p_settle_box, "_settlement": p_settlement, "_not_liq_box": p_not_liq,
           "_refuted": p_refuted, "_legs": p_legs, "_settle": p_settle, "quote_repriced": p_quote_repriced,
           "__deepcopy__": m_deepcopy}
for _name, _fn in PATCHES.items():
    setattr(M, _name, _fn)

# The attack scripts import the (now instrumented) sim through `common`.
import common  # noqa: E402
import arming as ARM  # noqa: E402
import block_spot as BS  # noqa: E402
import fixed_delta as FD  # noqa: E402
import fv_push as FP  # noqa: E402
import ptf_sandwich as PTF  # noqa: E402
import refute_push as RP  # noqa: E402

assert RP.V04 is sim and FP.V04 is sim


def export(name, m, base=None):
    ops = m._trace.flatten()
    if base is not None:
        bops = base[1]
        assert ops[:len(bops)] == bops, f"{name} does not continue {base[0]}"
        ops = ops[len(bops):]
    d = {"name": name, "ops": ops + ["state " + market_state(m)]}
    if base is not None:
        d["base"] = base[0]
    return d


def base_of(name, m):
    return (name, m._trace.flatten())


def captured(fn, *a, **kw):
    """Runs fn and returns (result, the Markets it created or deep-copied, in order)."""
    CAPTURE.clear()
    MODE["copies"] = True
    try:
        r = fn(*a, **kw)
    finally:
        MODE["copies"] = False
    got = list(CAPTURE)
    CAPTURE.clear()
    return r, got


def wrap_return(module, name, sink):
    """module.name's non-None results land in sink (the attack returns only numbers)."""
    orig = getattr(module, name)

    def w(*a, **kw):
        r = orig(*a, **kw)
        if r is not None:
            sink.append(r)
        return r
    setattr(module, name, w)
    return orig


# ---------------------------------------------------------------------------------------
# Attack traces (ECON §12), v0.4.1 defaults only (the scripts' legacy / analysis rows are
# not normative)
# ---------------------------------------------------------------------------------------
def attack_traces():
    D = sim.DEFAULT
    out = []
    # §12.10 arming.py: the borrower's atomic [dump -> sol_settle -> buy back] every 20 s.
    for key in ("A", "RA", "D@0%"):
        for attack in (True, False):
            (kind, *_r), ms = captured(ARM.run, key, D, horizon=200, attack=attack)
            out.append(export(f"arming/{key}/{'attack' if attack else 'honest'}/{kind}", ms[0]))
    # §12.9 (a) block_spot.py: block the spot leg w seconds, then the best sandwich.
    for key, w in (("A", 4), ("RA", 8), ("D@0%", 1)):
        sink = []
        orig = wrap_return(BS, "sandwich_exit", sink)
        try:
            captured(BS.offensive, sim, key, w, D)
        finally:
            BS.sandwich_exit = orig
        assert sink, "no sandwich"
        out.append(export(f"block_spot/{key}/w{w}/{sink[-1][1]['kind']}", sink[-1][3]))
    # §12.10 refute_push.py (a) refutation push, (b) spot-leg veto, (c) push-then-freeze,
    # (d) widening.
    for key in ("A", "RA"):
        for attack in (True, False):
            (kind, secs, *_r), ms = captured(RP.refute_run, sim, key, 1.05, attack, horizon=300)
            out.append(export(f"refute_push/a/{key}/x1.05/{'attack' if attack else 'honest'}/{kind}+{secs}", ms[0]))
    (kind, secs, _c), ms = captured(RP.spotblock_run, sim, "A", 12)
    out.append(export(f"refute_push/b/A/veto12/{kind}+{secs}", ms[0]))
    for key in ("A", "RA"):
        (kind, secs, *_r), ms = captured(RP.ptf_refute_run, sim, key, 1.03, True, D, horizon=300)
        out.append(export(f"refute_push/c/{key}/x1.03/ptf/{kind}+{secs}", ms[0]))
    (kind, secs, _c), ms = captured(RP.widen_s1, "A", 1.02, "P", horizon=120, pct=100)
    out.append(export(f"refute_push/d/A/S1/widen-1%/{kind}", ms[0]))
    (kind, secs, _c), ms = captured(RP.widen_s1, "RA", 1.02, "L", horizon=120)
    out.append(export(f"refute_push/d/RA/S1/widen-LP/{kind}", ms[0]))
    (r, secs, _c, _h), ms = captured(RP.widen_s3, "RA", "P", pct=100, horizon=60)
    out.append(export(f"refute_push/d/RA/S3/widen-1%/{r['kind'] if r else 'none'}", ms[0]))
    # §12.11 fv_push.py (M): the drained-RA model case one second before the honest settlement.
    for drain, k in ((9700, 1.2), (9500, 1.4)):
        (m0, pos0, t0, honest), ms = captured(FP.ready_model, sim, "RA", drain, k, D)
        base = f"fv_push/M/RA{drain // 100}x{k}"
        out.append(export(base, m0))
        b = base_of(base, m0)
        runs = [("up", FP.attack_up, (m0, pos0, t0)),
                ("flip1", FP.attack_flip, (m0, pos0, t0, 1)),
                ("down", FP.attack_down, (m0, pos0, t0)),
                ("down-repeat", FP.attack_down, (m0, pos0, t0, 0.33, True, 200)),
                ("ptf4", FP.attack_ptf, (m0, pos0, t0, 4)),
                ("ptf10", FP.attack_ptf, (m0, pos0, t0, 10)),
                ("flip5-consecutive", FP.attack_flip_gap, (m0, pos0, t0, 5, 0)),
                ("flip6-gap1", FP.attack_flip_gap, (m0, pos0, t0, 6, 1))]
        for name, fn, args in runs:
            res, ms = captured(fn, *args)
            r = res[0]
            out.append(export(f"{base}/{name}/{r['kind'] if r else 'none'}", ms[0], b))
    # (V) the p' veto: Emergency flag, expired AND CR-liquidatable at the deadline.
    for key, mult in (("A", 1.70), ("RA", 1.62), ("D@0%", 2.4)):
        (m0, pos0, t0), _ms = captured(FP.ready_veto, key, mult, D)
        base = f"fv_push/V/{key}x{mult}"
        out.append(export(base, m0))
        b = base_of(base, m0)
        for veto in (False, True):
            (r, _c, _st), ms = captured(FP.veto_run, m0, pos0, t0, veto)
            out.append(export(f"{base}/{'veto' if veto else 'honest'}/{r['kind'] if r else 'none'}", ms[0], b))
    # (E) Emergency flag, healthy position expires: S3 on an executable venue, FV binding.
    for key in ("A", "RA"):
        (m0, pos0, t0, honest), ms = captured(FP.ready_flag, key, D)
        base = f"fv_push/E/{key}"
        out.append(export(base, m0))
        b = base_of(base, m0)
        for name, fn, args in (("up", FP.attack_up, (m0, pos0, t0)), ("flip1", FP.attack_flip, (m0, pos0, t0, 1)),
                               ("down-repeat", FP.attack_down, (m0, pos0, t0, 0.33, True, 120))):
            res, ms = captured(fn, *args)
            r = res[0]
            out.append(export(f"{base}/{name}/{r['kind'] if r else 'none'}", ms[0], b))
    # §12.2 fixed_delta.py (d): the atomic liquidation sandwich at once and after 4 s.
    for key in ("A", "RA"):
        (m, pos, t, _k), ms = captured(FD.setup_liq_S, sim, key, D)
        base = f"fixed_delta/d/{key}"
        out.append(export(base, m))
        b = base_of(base, m)
        for wait in (0, 4):
            mm, pp = copy.deepcopy(m), copy.deepcopy(pos)
            tt = t
            for _ in range(wait):
                tt += 1
                mm.step_to(tt)
            sw = common.sandwich_exit(mm, pp, tt)
            out.append(export(f"{base}/wait{wait}/{sw[1]['kind'] if sw else 'none'}", sw[3] if sw else mm, b))
    # R4B-2 ptf_sandwich.py: push-then-freeze before the liquidation sandwich.
    for key, mult, fr, lead in (("A", 2.0, 4, 6), ("RA", 2.0, 8, 10)):
        sink = []
        orig = wrap_return(PTF, "sandwich_exit", sink)
        try:
            captured(PTF.run, key, mult, fr, lead, D)
        finally:
            PTF.sandwich_exit = orig
        assert sink, "no sandwich"
        out.append(export(f"ptf_sandwich/{key}/x{mult}/freeze{fr}/{sink[-1][1]['kind']}", sink[-1][3]))
    # venue_switch.py: graduation and a Raydium fast path rewrite the ring's liquidity.
    for key, sc in (("D@90%", sim.GRAD_SCENARIOS[1]), ("RA", sim.FAST_SCENARIOS[1])):
        m = sim.new_market(key)
        pos, r, _p1 = sim.run(m, 1.0, replace(sc, window=700))
        out.append(export(f"venue_switch/{key}/{sc.name}/{r['kind']}", m))
    return out


def scenario_traces():
    out = []
    D = sim.DEFAULT
    cases = [("A", "gap 5x", 120, D), ("C", "pump 10x/60s", 150, D), ("RA", "drain 50% + pump 2x/120s", 400, D),
             ("D@0%", "spike 10x/5s", 90, D), ("RC", "pool disabled", 1300, D),
             ("B", "route flagged (Emergency) + gap 2x", 1300, D)]
    for key, name, window, P in cases:
        m = sim.new_market(key, P)
        pos, r, _p1 = sim.run(m, 1.0, replace(sim.SC[name], window=window))
        out.append(export(f"scenario/{key}/{name}/{r['kind']}", m))
    out += reversal_traces() + halted_s3_traces()
    for key in ("B", "RB"):                          # expiry with a short tier duration
        m = sim.Market(sim.MARKETS[key](), replace(sim.tier_of(key), max_dur_s=900), D)
        pos, r, _p1 = sim.run(m, 1.0, sim.SC["flat to expiry"])
        out.append(export(f"scenario/{key}/flat to expiry (15 min)/{r['kind']}", m))
    # the stuck migration (ECON §8.6's D@80% case): the completed curve is still observed,
    # S3 is priced on the post-migration pool (max_migration_pause_s at its compiled minimum;
    # the keeper calls sol_settle every 10 s)
    P = replace(sim.grad_extra(sim.GRAD_SCENARIOS[3], D), max_migration_pause_s=600)
    m = sim.new_market("D@80%", P)
    pos = m.open(m.max_open_q(), 0)
    t = common.warm(m, 0, 120)
    sim.complete_curve(m)
    r = None
    while r is None and t < 2_000:
        t += 1
        m.step_to(t)
        if t % 10 == 0:
            r = m.try_exit(pos, t, settle_only=True)
    out.append(export(f"scenario/D@80%/graduation, migration stuck (pause 600 s)/{r['kind'] if r else 'none'}", m))
    return out


def reversal_traces():
    """A genuine gap arms S1 (the keeper calls sol_settle every second), then the price
    genuinely reverses before the clock matures: partly (still liquidatable: the box
    refutes) or fully (the EMA leg fails first: box-robust "not liquidatable" disarms)."""
    out = []
    for key in ("A", "RA", "D@0%"):
        for back, label in ((0.9, "partial"), (None, "full")):
            m = sim.new_market(key)
            pos = m.open(m.max_open_q(), 0)
            t = common.warm(m, 0, 120)
            kb = RP.s1_boundary(m, pos)
            p0 = m.venue.spot_wad()
            m.exo_move(int(p0 * kb * 1.05))
            for _ in range(25):
                t += 1
                m.step_to(t)
                m.try_exit(pos, t, settle_only=True)
            assert pos.armed, "not armed by the gap"
            m.exo_move(int(p0 * kb * back) if back else p0)
            for _ in range(30):
                t += 1
                m.step_to(t)
                m.try_exit(pos, t, settle_only=True)
            out.append(export(f"reversal/{key}/{label}/{'armed' if pos.armed else 'disarmed'}", m))
    return out


def halted_s3_traces():
    """A liquidatable (not S1) position whose pool the admin then halts: no observations, the
    EMA freezes, the box collapses to the EMA point after RING_LEN seconds, and S3 settles
    after s3_wait_s at FV on the EMA point (the keeper cranks every 10 s, inside the 60 s
    chain gap)."""
    out = []
    for key, mult in (("RA", 1.9), ("A", 1.9)):
        m = sim.new_market(key)
        pos = m.open(m.max_open_q(), 0)
        t = common.warm(m, 0, 120)
        m.exo_move(int(m.venue.spot_wad() * mult))
        for _ in range(20):
            t += 1
            m.step_to(t)
            m.try_exit(pos, t, settle_only=True)
        assert m.status(pos, t)["liq"], "not liquidatable before the halt"
        m.venue.disabled = True
        r = None
        while r is None and t < 2_000:
            t += 10
            m.step_to(t)
            r = m.try_exit(pos, t, settle_only=True)
        out.append(export(f"halted/{key}/x{mult}/{r['kind'] if r else 'none'}", m))
    return out


def fuzz_roots(n):
    """Runs ECON §13's exit fuzz for n runs; returns (stats, the n markets it created)."""
    orig_new = sim.new_market

    def fuzz_new(*a, **kw):
        m = orig_new(*a, **kw)
        ROOTS.append(m)
        return m
    ROOTS.clear()
    sim.new_market = fuzz_new
    try:
        stats = sim.fuzz_exits(n, SEED)
    finally:
        sim.new_market = orig_new
    roots = list(ROOTS)
    ROOTS.clear()
    assert len(roots) == n
    return stats, roots


# ---------------------------------------------------------------------------------------
# Units: the sim's settle_box, _s3_due and try_exit(settle_only=True) on synthetic states
# ---------------------------------------------------------------------------------------
class FakePool:
    def __init__(self, L, table):
        self.L, self.table = L, table

    def sqrt_k(self):
        return self.L

    def quote_buy_on(self, q, X, Y, cap_ppm=None):
        v = self.table[(X, Y)]
        return None if v is None else (v, v)


class FakeVenue:
    def __init__(self, route, curve, l_live, pool):
        self.route, self.kind, self.complete = route, ("curve" if curve else "fake"), curve and not route
        self.l, self.pool = l_live, pool

    def has_route(self):
        return self.route

    def has_price(self):
        return True

    def spot_wad(self):
        return 10**15

    def sqrt_k(self):
        return self.l

    def eff(self):
        return (1, 1)

    def migrate_preview(self):
        return self.pool


def bare():
    m = object.__new__(M)
    m.P, m.tier = sim.DEFAULT, sim.TIERS["A"]
    m._trace, m._ctx, m._capq, m._pids = Trace(), None, None, set()
    return m


def unit_box(rng, n, executable=False):
    """settle_box on synthetic rings, on a venue that can execute now or not (M2-SEC-1: only a
    venue that cannot execute collapses the box to the EMA point after ring_len seconds)."""
    cases = []
    for _ in range(n):
        m = bare()
        m.venue = FakeVenue(executable, False, 1, None)
        size = rng.randrange(1, 9)
        t = rng.randrange(0, 10**6)
        ring = []
        base = rng.randrange(10**9, 10**24)
        for j in range(size):
            p = base * rng.randrange(8_000, 12_500) // 10_000
            L = rng.randrange(10**9, 10**13)
            lo = p * rng.randrange(7_000, 10_001) // 10_000 if rng.random() < 0.7 else p
            hi = p * rng.randrange(10_000, 14_000) // 10_000 if rng.random() < 0.7 else p
            llo = L * rng.randrange(9_000, 10_001) // 10_000
            lhi = L * rng.randrange(10_000, 11_000) // 10_000
            ring.append(M._entry(t + j, p, L, lo, hi, llo, lhi))
        m.ring = ring
        m.ema = base * rng.randrange(7_000, 14_000) // 10_000
        m.liq = rng.randrange(10**9, 10**13)
        m.ema_t = t + size - 1 - rng.choice([0, 0, 0, 3])
        now = m.ema_t + rng.choice([0, 1, 2, 7, 8, 9, 10, 100])
        cases.append(line(now, m.ema, m.liq, m.ema_t, *M.settle_box(m, now), *ring_tokens(ring)))
    return cases


def unit_box_cap(rng, n):
    """AIR-15: settle_box with a depth cap (Market.ref_depth_cap stubbed on the bare market) on synthetic rings whose
    liquidity highs are often deposit-wrapped (up to 3x), on a venue that can execute now or not: caps below L_ema,
    at L_mhi and around it."""
    cases = []
    for _ in range(n):
        m = bare()
        executable = rng.random() < 0.7
        m.venue = FakeVenue(executable, False, 1, None)
        size = rng.randrange(1, 9)
        t = rng.randrange(0, 10**6)
        ring = []
        base = rng.randrange(10**9, 10**24)
        for j in range(size):
            p = base * rng.randrange(8_000, 12_500) // 10_000
            L = rng.randrange(10**9, 10**13)
            lo = p * rng.randrange(7_000, 10_001) // 10_000 if rng.random() < 0.7 else p
            hi = p * rng.randrange(10_000, 14_000) // 10_000 if rng.random() < 0.7 else p
            llo = L * rng.randrange(9_000, 10_001) // 10_000
            lhi = L * rng.randrange(10_000, 30_000) // 10_000 if rng.random() < 0.5 else L
            ring.append(M._entry(t + j, p, L, lo, hi, llo, lhi))
        m.ring = ring
        m.ema = base * rng.randrange(7_000, 14_000) // 10_000
        m.liq = rng.randrange(10**9, 10**13)
        m.ema_t = t + size - 1 - rng.choice([0, 0, 0, 3])
        now = m.ema_t + rng.choice([0, 1, 2, 7, 8, 9, 10, 100])
        l_mhi = sim.upper_median([e[8] for e in ring])
        k = rng.random()
        if k < 0.25:
            cap = m.liq * rng.randrange(1_000, 10_000) // 10_000
        elif k < 0.4:
            cap = l_mhi + rng.choice([-1, 0, 1])
        else:
            cap = l_mhi * rng.randrange(5_000, 15_000) // 10_000
        m.ref_depth_cap = lambda c=cap: c
        cases.append(line(now, executable, cap, m.ema, m.liq, m.ema_t, *M.settle_box(m, now), *ring_tokens(ring)))
    return cases


def unit_s3(rng, n):
    cases = []
    for _ in range(n):
        m = bare()
        m.P = replace(sim.DEFAULT, s3_wait_s=rng.choice([600, 700, 86_400]), grad_grace_s=rng.choice([600, 900]),
                      max_migration_pause_s=rng.choice([600, 3600]))
        t = rng.randrange(10**5, 10**6)
        nr_now = rng.random() < 0.8
        m.s3_no_route = lambda f=nr_now: f
        m.no_route_since = None if rng.random() < 0.15 else t - rng.randrange(0, 2 * m.P.s3_wait_s)
        m.graduated_t = None if rng.random() < 0.5 else t - rng.randrange(0, 3 * m.P.grad_grace_s)
        mig = rng.random() < 0.3
        m.venue = FakeVenue(not mig, mig, 1, None)
        m.complete_t = (None if rng.random() < 0.2 else t - rng.randrange(0, 2 * (m.P.max_migration_pause_s
                                                                                 + m.P.s3_wait_s)))
        liq, expired = rng.random() < 0.6, rng.random() < 0.3
        st = {"liq": liq, "expired": expired}
        r = M._s3_due(m, sim.Position(), st, t)
        cases.append(line(t, nr_now, m.no_route_since, m.graduated_t, mig, m.complete_t, liq or expired,
                          m.P.s3_wait_s, m.P.grad_grace_s, m.P.max_migration_pause_s, r))
    return cases


def unit_evaluate(rng, n):
    """try_exit(settle_only=True) with the observation prologue stubbed out (the state is
    the post-roll state), a synthetic status and box, and quotes drawn per (P, L) point."""
    traces = []
    for i in range(n):
        m = bare()
        P = replace(sim.DEFAULT, settle_persist_n=rng.choice([40, 40, 41, 600]), settle_arm_gap_s=rng.choice([2, 10, 60]),
                    fv_dev_bps=rng.choice([50, 100, 200]), expiry_fee_bps=rng.choice([0, 200, 500]),
                    s3_wait_s=rng.choice([600, 1000]), grad_grace_s=600, max_migration_pause_s=rng.choice([600, 3600]))
        m.P = P = replace(P, check_invariants=False)     # random quotes are not monotone in (P, L)
        V = rng.randrange(10**7, 10**12)
        K = m.settle_reward(V)
        ert = rng.choice([0, 6_500_000])
        L_cr = rng.choice([12_500, 13_500, 15_000, 17_500])
        pen = rng.choice([500, 700, 800, 1000])
        slip_max = rng.choice([200, 2000, 3000])
        t = rng.randrange(10**4, 10**6)
        deadline = t + rng.choice([-5, 0, 1, 1000])
        ema_count = rng.randrange(1_000, 10**5)
        pos = sim.Position(Q=rng.randrange(10**6, 10**14), V=V, L=L_cr, pen_bps=pen, slip_max=slip_max, deadline=deadline,
                           ert=ert, t_open=0, R_min=0)
        if rng.random() < 0.6:
            pos.s_kind = "armed"
            pos.s_count = ema_count - rng.choice([0, 1, 39, 40, 41, 599, 600, 1000])
            pos.s_t = t - rng.randrange(0, 200)
            pos.s_last = t - rng.choice([0, 1, 2, 10, 11, 60, 61])
        else:
            pos.s_count, pos.s_t, pos.s_last = rng.randrange(0, ema_count), t - 500, t - 400
        # box and point
        p_ema = rng.randrange(10**12, 10**16)
        l_ema = rng.randrange(10**11, 10**14)
        shape = rng.random()
        if shape < 0.25:
            bx = (p_ema, p_ema, l_ema, l_ema)
        else:
            bx = (p_ema * rng.randrange(9_000, 10_001) // 10_000, p_ema * rng.randrange(10_000, 11_000) // 10_000,
                  l_ema * rng.randrange(9_500, 10_001) // 10_000, l_ema * rng.randrange(10_000, 10_500) // 10_000)
        route = rng.random() < 0.8
        curve = not route and rng.random() < 0.5
        L_live = l_ema * rng.randrange(9_000, 11_000) // 10_000
        lo_ok = rng.random() < 0.85
        p_rs = p_ema * rng.randrange(9_000, 11_000) // 10_000 if lo_ok else None
        # quotes: around the decision boundaries
        room = V - K - ert
        table_c, table_s, table_f = {}, {}, {}
        pool_L = rng.randrange(10**11, 10**14)
        pool_table = {}

        def draw_cost():
            k = rng.random()
            if k < 0.1:
                return None
            if k < 0.2:
                return max(1, room + rng.choice([0, 1]))          # the S1 budget boundary
            if k < 0.3:
                return max(1, V * BPS // L_cr + rng.choice([-1, 0, 1]))  # the CR boundary (R = 0)
            return max(1, room * rng.randrange(5_000, 15_000) // 10_000)

        def draw_sig():
            k = rng.random()
            if k < 0.1:
                return None
            if k < 0.3:
                return slip_max + rng.choice([0, 1])
            return rng.randrange(0, 3 * slip_max + 1)
        points = [(bx[0], bx[3]), (bx[1], bx[2]), (bx[1], min(L_live, bx[2])), (p_ema, l_ema)]
        if p_rs is not None:
            points.append((p_rs, L_live))
        for pt in points:
            if pt not in table_c:
                table_c[pt] = draw_cost()
                table_s[pt] = draw_sig()
        # FV amounts near the fv_dev boundary: tie the high corner to the low one
        if table_c[points[0]] is not None and table_c[points[1]] is not None and rng.random() < 0.5 \
                and points[0] != points[1]:
            lo = table_c[points[0]]
            table_c[points[1]] = lo + lo * P.fv_dev_bps // BPS // rng.choice([1, 2, 3]) + rng.choice([0, 1])
        if (not curve and points[0] != points[1] and deadline > t and rng.random() < 0.15
                and (V - K) // 3 > 10**5):
            # an amount exactly on the fv_dev boundary (or one lamport past it)
            R_fix = rng.choice([0, rng.randrange(0, 10**6)])

            def fv_of(b):
                return b + sim.cdiv(pen * b, BPS) + R_fix
            b_lo = (V - K) // 3
            for _ in range(10**5):
                F = fv_of(b_lo)
                if F * P.fv_dev_bps % BPS == 0:
                    target = F + F * P.fv_dev_bps // BPS + rng.choice([0, 0, 1])
                    lo_b, hi_b = b_lo, 2 * b_lo + 10
                    while lo_b < hi_b:
                        mid = (lo_b + hi_b) // 2
                        if fv_of(mid) < target:
                            lo_b = mid + 1
                        else:
                            hi_b = mid
                    if fv_of(lo_b) == target:
                        table_c[points[0]], table_c[points[1]] = b_lo, lo_b
                        break
                b_lo += 1
            boundary_R = R_fix
        else:
            boundary_R = None
        for pt in points:
            fvv = table_c[pt] if not curve else draw_cost()
            table_f[pt] = fvv
            if curve:
                s = sim.reprice(pool_L, pt[0])
                pool_table[s] = fvv
        m.venue = FakeVenue(route, curve, L_live, FakePool(pool_L, pool_table))
        m.ema, m.liq, m.ema_count, m.ema_t = p_ema, l_ema, ema_count, t - rng.choice([0, 1, 5])
        m.ring = [M._entry(t, p_ema, l_ema, p_ema, p_ema, l_ema, l_ema)]
        m.s3_no_route = lambda f=(not route or rng.random() < 0.4): f
        m.no_route_since = None if rng.random() < 0.3 else t - rng.randrange(0, 1500)
        m.graduated_t = None if rng.random() < 0.7 else t - rng.randrange(0, 1500)
        m.complete_t = None if not curve else t - rng.randrange(0, 5000)
        m.advance = lambda tt: None
        m.observe = lambda tt: None
        R = rng.choice([0, 0, rng.randrange(0, V // 10 + 1)]) if boundary_R is None else boundary_R
        liq = rng.random() < 0.7
        ema_leg = liq or rng.random() < 0.3
        qs_r = None if rng.random() < 0.2 else (0, max(1, room + rng.choice([-1, 0, 1, 10**6])))
        st = dict(R=R, Veff=V - R, qs=qs_r, qs_r=qs_r, qrs=None, qe=None, p_live=10**15, p_rs=p_rs, lo_ok=lo_ok,
                  L_live=L_live, cr_s=0, cr_rs=None, cr_e=0, liq=liq, ema_leg=ema_leg, spot_leg=liq,
                  ema_run=0, expired=t >= deadline)
        m._stub_st, m._stub_box = st, bx
        m.quote_repriced = lambda Q, p, L: (None if table_c[(p, L)] is None else (0, table_c[(p, L)]))
        m.slip_repriced = lambda Q, p, L: sim.INF if table_s[(p, L)] is None else table_s[(p, L)]
        sig_live = draw_sig()
        m.slip_at = lambda Q, X, Y, live=False, s=sig_live: sim.INF if s is None else s
        m.tier = replace(sim.TIERS["A"], max_liq_slip_bps=slip_max, penalty_bps=pen, cr_liq_bps=L_cr)
        emit(m, line("pos", 0, pos.Q, V, K, ert, L_cr, pen, slip_max))
        M.try_exit(m, pos, t, settle_only=True)
        ops = m._trace.flatten()
        if any(o.startswith("settle") for o in ops):
            traces.append({"name": f"unit/{i}", "ops": ops})
    return traces


# ---------------------------------------------------------------------------------------
def dump(name, obj):
    obj = {"generator": Path(__file__).name,
           "source": "docs/design/sim/shortwin_sim.py v0.4.1 (default switches) + attacks/", **obj}
    text = json.dumps(obj, indent=1, sort_keys=True) + "\n"
    HERE.joinpath(name).write_text(text)
    return len(text)


def dump_split(prefix, traces):
    """Traces in files of at most MAX_FILE bytes, in order (a base precedes its forks)."""
    files, cur, size = [], [], 0
    for t in traces:
        s = len(json.dumps(t))
        if cur and size + s > MAX_FILE and "base" not in t:
            files.append(cur)
            cur, size = [], 0
        cur.append(t)
        size += s
    if cur:
        files.append(cur)
    out = {}
    for i, f in enumerate(files, 1):
        name = f"{prefix}_{i}.json"
        out[name] = dump(name, {"traces": f})
    return out


def main():
    rng = random.Random(SEED)
    sizes = {}
    MODE["full"], MODE["sample"] = False, None      # units: every evaluation
    units = {"box": unit_box(rng, 300), "s3": unit_s3(rng, 400),
             # its own stream, so the other files keep theirs (M2-SEC-1)
             "box_exec": unit_box(random.Random(SEED + 1), 300, executable=True),
             # likewise (AIR-15)
             "box_cap": unit_box_cap(random.Random(SEED + 2), 300)}
    sizes["units_settle.json"] = dump("units_settle.json", units)
    sizes.update(dump_split("units_evaluate", unit_evaluate(rng, 1500)))
    MODE["full"] = True
    sizes.update(dump_split("trace_attacks", attack_traces()))
    sizes.update(dump_split("trace_scenarios", scenario_traces()))
    # the first runs of the exit fuzz, whole
    _stats, roots = fuzz_roots(6)
    sizes.update(dump_split("trace_random", [export(f"fuzz/{i}", m) for i, m in enumerate(roots)]))
    # ECON §13's exit fuzz, decision-only
    MODE["full"], MODE["sample"] = False, 8
    stats, roots = fuzz_roots(1000)
    assert stats == ECON_13_EXIT_FUZZ, stats
    fuzz = []
    for i, m in enumerate(roots):
        ops = m._trace.flatten()
        if any(o.startswith(("settle", "maxin")) for o in ops):
            fuzz.append({"name": f"fuzz/{i}", "ops": ops})
    sizes.update(dump_split("fuzz", fuzz))
    for k, v in sizes.items():
        print(f"{k}: {v:,} bytes")


if __name__ == "__main__":
    main()
