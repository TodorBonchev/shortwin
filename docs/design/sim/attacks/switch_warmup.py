"""M4R: post-change confirmation of the observation state after a Raydium venue change (ECON §5.2, §10.3,
§10.5, §12.12; ARCH §2.6 rule 4, §4.4; changelog #96; PROPOSED, owner item O18).

The problem.  After a fast-path adoption the M4 rule as built seeded the exits from single-instruction samples:
L_new = min(L_rec, L_live, floor(D*1e4/lambda_min)) and a P_ema frozen during the halt.  An unlocked LP of the
candidate withdraws around a sampling instruction (L_new = D), then holds L_ema there with one withdraw-wrapped
crank per second (directional aggregation freezes an EMA with one observation on its own side every second) and
liquidates positions within ~q/x of cr_liq that are healthy on the pool as it trades (M4R-1); the window floor
judged at the stale P_ema admits a pool whose real locked SOL is floor*sqrt(P_live/P_ema) after a dump (M4R-2).

The rule (the program's; ../shortwin_sim.py Market.adopt_pool / _integrate_warmup / gated / exit_reference /
window_floor_ok):
  * the adoption reads no sample: L_ema and every ring entry's L := the anchor cap = floor(D*1e4/lambda_min),
    L_slow = min(L_slow, D), P_ema and the ring prices carried; ema_warm := 8, ema_guard := 1;
  * each of the next 8 integrations re-seeds, over the post-change entries only, P_ema = min of the bucket
    price lows and of the largest v0.4.1 step up from the previous P_ema (M4X-1: the seed rises no faster than
    the v0.4.1 EMA can), floored at the largest v0.4.1 step down from it (M6-SEC-1, #105: nor falls faster), and
    L_ema = min(max lhi, floor(lomed(lhi)*(1e4 + C)/1e4)) -- no blend, L_slow untouched;
  * while ema_warm > 0 on an executable venue liquidate / expire fail (EmaWarmingUp), sol_settle evaluates
    nothing and Raydium opens are refused; a halted venue is never gated;
  * while ema_guard = 1 the exit reference reads L_b = max(L_live, min(L_ema, L_mlo), L_slow) and L_slow
    integrates min(lobs, L_mlo) (M4X-2); the guard clears at the first integration after the warm-up with
    L_ema <= 1.01 * L_mlo;
  * on Raydium the EMA terms of L_b are capped at the sized depth (M4F-1, Market.ref_depth_cap):
    L_b = max(L_live, min(max(L_ema', L_slow), floor(D*1e4/lambda_min)));
  * a proposal that opens or improves a window judges the window floor at min(P_ema, P_cand); the execution at
    P_ema, every failure cancels (as built; a genuine dump inside the window is adopted at P_ema, M4X-3);
  * #99 (owner decision 2026-09-29, "switch only if all fit"): a window whose adoption carries open positions
    opens, improves and executes only if every open position fits the candidate's opening caps on its
    locked-depth X0 (Market.carried_fit, at the same prices as the window floor); since FIT-2's fix (#100) the
    carried fee-scaled caps take min(phi_inc, phi_cand), phi_inc = Market.min_open_fee_ppm.  Parts 1-10 study
    the adoption's mechanics and FORCE the adoption: most of their carried positions exceed the candidate's caps
    and under #99 would stay on the halted pool (part 11a lists the verdicts).  The mechanisms stay in force for
    the positions that fit, and part 11 applies the rule (Spec.fit).  Since FITS-1 (#101) the carried fee-scaled
    caps take min(phi_inc, phi_cand, CARRIED_FEE_CAP_PPM = 2,500): every carried position is in the 0.75 %-of-X0
    class, a 1 % -> 1 % switch included;
  * FITS-2 (#101): the liquidation spot leg (liquidate, expire, sol_settle) reads the robust spot on
    L_spot = max(L_live, L_ema') (Market.spot_liquidity), so a withdrawal in the same transaction cannot make it
    true; the keeper covers a market with open positions every second while foreign LP-wrapped observations recur
    (Coverage "new").
Compared with "M4 as built" (venue_switch.BuiltM4).  Analysis rules: "M4R without the guard" (part 4), "M4R with
L_slow unguarded" (part 4c: the rule before M4X-2, which had no reference cap either), "M4R without the step bound"
(part 8d: before M4X-1), "M4R without the reference cap" (parts 4d, 4e: before M4F-1), "#99's fit with the carried
fee caps at phi_cand" (part 11d: before FIT-2's fix), "#100's fit" (part 11d (iii): the carried fee caps at
min(phi_inc, phi_cand) without the 2,500-ppm cap, before FITS-1), "#100" (part 5b: the spot leg at L_live with the #99
keeper, before FITS-2), "the withdrawn lows bound" (part 5b: #100's first fix for FIT-2, an EMA-term bound at
floor(L_mlo*1.01) withdrawn after FITV-2) and FITX-1's two candidate floors of the spot leg, at L_mhi and at
min(L_mhi, L_slow) (part 5b (vii): MhiFloor, MhiSlowFloor; NOT adopted, owner item O18).
The guard and M4X-2's input guard read L_mlo, the lower median of the ring's bucket liquidity lows, so they hold
only while the buckets carry honest observations (L_mlo moves once 5 of the 8 newest entries are attacker-only):
engine P's keeper cranks every second, except in parts 4d / 4e / 5b / 11, whose keeper follows its coverage duties
(Coverage: "old" = as built at M4R; "m4f1" = M4F-1's per-second duty while the guard is set and liquidity-aware
per-slot bursts, the bursts reverted by #99 (M4G-2); "n99" = the keeper after #99: per second while the guard is
set, bursts on price only; "new" = the keeper after #101 (FITS-2 (b)): "n99" plus per-second coverage for 10 s,
extended while deviations recur, after a foreign bucket liquidity deviation on a market with open positions).
Since #101 (FITS-2 (a)) the Market's liquidation spot leg reads max(L_live, L_ema') (Market.spot_liquidity); the
historical rules "M4 as built" (BuiltM4), "#100" (Spot100) and "the withdrawn lows bound" read it at L_live, as
they did.

Three run engines, ported from the M4R redesign's harnesses (the designers' fast-path loops, integer-exact;
with the rules replaced by the sim's):
  W  run_fast_path (designer "warmup-gate"): the M4R-1 reproducer -- a 100-SOL fully locked AMM v4 incumbent at
     RA's price (Tier C), a CPMM candidate at m x the pre-halt price; after the adoption, per second: the honest
     keeper's crank and liquidation attempt, and the attacker's actors (wrapped cranks, wrapped liquidations,
     atomic price bundles, held LP / price moves).
  P  switch_run (designer "invariant-seed"): incumbents R100 (100 SOL fully locked at RC's price, Tier C), RA,
     RC; attacker a1 = the LiteSVM reproducer's (execution withdraw-wrapped, then a withdraw-wrapped crank and
     liquidation every second after the keeper), a2 = every sample withdraw-wrapped and the wrapped liquidation
     first in each second.  A failed attacker liquidation keeps its prologue's observation (an upper bound on
     the attacker: on chain the attacker bundles a wrapped crank_price instead).  Spec.keeper_rule puts the
     keeper on its coverage duties after the adoption (part 4d) instead of a crank every second.
  M  run_plan (designer "multi-sample"): a 60-s pre-halt period (optionally at pre_mult x), the same incumbent
     at lambda 99.78 %, the LP-holder's wrapped crank and liquidation after the keeper every second.
Parts:
  1  M4R-1 grid (W): q 0.40 / 0.74 % of the incumbent's X, four LP-wrap variants of the two sampling
     instructions, the per-second withdraw-wrapped freeze and wrapped liquidation, 120 s.
  2  lambda < lambda_min candidates under the M4R-1 attacker (W; P on a fine grid).
  3  Honest depth and box after the warm-up (W): thinner, deeper and lambda < lambda_min candidates.
  4  Capital deposit freeze (P: RA -> 630 / 314 SOL; W: near-threshold and the big position); 4c the freeze held
     for 10-30 min before a genuine pump (M4X-2); 4d the same freeze with the keeper on its idle cadence after the
     gate, under the coverage duties as built at M4R, under M4F-1's and after #99, with and without the reference
     cap (M4F-1); 4e the steady-state controls (no switch).
  5  Withdraw contest while the guard is set (P); 5b the steady-state withdraw drag under the keeper's real
     cadence (FITV-2, FITS-2): #101 (the spot leg at max(L_live, L_ema'), the #101 keeper) against #100 and the
     withdrawn lows bound, short and hour-long drags, smaller positions, the keeper's missed seconds, a genuine
     spike and retreat (the spot leg's own case), and the liveness of a genuine withdrawal; (vii) FITX-1 (#102): a
     genuinely reached low (a held withdrawal) frozen by withdraw-wrapped cranks, against the same LP's withdrawal
     for good, and the candidate floors with their cost under a capital deposit freeze.
  6  M4R-2: a dump during the halt (P); a dump inside the collection window (M4X-3).
  7  Genuine moves (W, P, M).
  8  Degraded mode: no keeper during the warm-up (W, M); the steady-state variant for comparison (pre-existing);
     8d atomic price bundles in attacker-only seconds, the attacker's best pump sizes (M4X-1).
  9  Stale-high P_ema after a mild dip (M).
  10 Targeted random search (W): 1,200 runs, seed 23.
  11 #99, switch only if all fit: 11a the fit verdicts of parts 1-10's carried positions; 11b part 4d's candidates
     under the rule (no window; S3 at fair value at the deadline); 11c carried positions at the fit limit under the
     M4F-1 / M4G-1 / M4G-3 attacks (delay only); 11d FIT-2: carried positions on a 1 % candidate -- FIT-2's
     refused under the carried fee caps, those at the fit limit under the pushes, the 1 % -> 1 % switch (#100's
     residual 15, closed by FITS-1: refused, S3 at fair value; at the #101 limit liquidated), native controls;
     11e FIT-3: a closed large position keeps the mark -- the S3 timing.
  selftest: an honest switch reaches L_ema / sqrt(X*Y) = 1.000 at the gate, the box is narrow after it and the
  guard clears at the gate.
Deterministic; standard library only.

    python3 switch_warmup.py            # every part (redirect to ../out/attacks/switch_warmup.out)
    python3 switch_warmup.py 1 4 10     # some parts
"""
import copy
import math
import random
import sys
from dataclasses import replace

from common import *  # noqa: F401,F403
from venue_switch import BuiltM4

TIER_C = TIERS["C"]


class NoGuard(Market):
    """Analysis rule: M4R without the reference guard (the exit reference reads L_ema as in v0.4.1)."""

    def mark_restart(self):
        super().mark_restart()
        self.guard = False


class NoBound(Market):
    """Analysis rule: M4R before M4X-1 -- the warm-up price seed is the min of the post-change bucket lows alone,
    with no step bound (one attacker-only warm-up seeds any pumped price)."""

    def _integrate_warmup(self, t):
        if t > self.ema_t:
            T = self.tier
            k = EMA_WARMUP_BUCKETS - self.warm
            ents = self.ring[len(self.ring) - k:] if k else []
            p = min([e[5] for e in ents] + [self.b_lo])
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


class NoSlowGuard(Market):
    """Analysis rule: M4R before M4X-2 -- L_slow integrates lobs even while the guard is set, so a deposit-frozen
    L_ema climbs into L_slow, which the exit reference reads unguarded.  Before M4X-2 there was no reference cap
    either (M4F-1 came later), so this rule has none: part 4c's rows show the rule as it was."""

    def ref_depth_cap(self):
        return None

    def _integrate(self, t):
        if self.warm > 0 or not self.guard:
            return super()._integrate(t)
        n0 = self.ema_count
        self.guard = False
        super()._integrate(t)
        self.guard = True
        if self.ema_count != n0:
            self._guard_check()


class NoCap(Market):
    """Analysis rule: M4R before M4F-1 -- no cap on the exit reference liquidity, L_b = max(L_live, L_ema', L_slow)
    (the rule as built at M4R; the sim's Market caps the EMA terms at floor(D*1e4/lambda_min) on Raydium)."""

    def ref_depth_cap(self):
        return None


class PhiCand(Market):
    """Analysis rule: #99's fit as first built, before FIT-2's fix (#100) -- the carried fee-scaled caps at the
    candidate's fee alone, not min(phi_inc, phi_cand, CARRIED_FEE_CAP_PPM) (part 11d); the exits as adopted."""

    def carried_fee(self, pool):
        return pool.fee_oneway_ppm()


class Fit100(Market):
    """Analysis rule: #100's fit, before FITS-1 (#101) -- the carried fee-scaled caps at min(phi_inc, phi_cand) without
    the compiled CARRIED_FEE_CAP_PPM, so a 1 % -> 1 % switch carries up to the 2.47 % impact cap (part 11d (iii),
    #100's residual 15); the exits as adopted."""

    def carried_fee(self, pool):
        phi = pool.fee_oneway_ppm()
        return phi if self.min_open_fee_ppm == 0 else min(self.min_open_fee_ppm, phi)


class Spot100(Market):
    """Historical rule: #100's exits, before FITS-2 (a) (#101) -- the liquidation spot leg on the robust spot at L_live,
    which an LP withdrawal in the same transaction lowers (part 5b; with the #99 keeper, Coverage "n99" / "old")."""

    def spot_liquidity(self, L_live):
        return L_live


class LowsBound(Spot100):
    """Analysis rule: the lows bound of #100's first fix for FIT-2, WITHDRAWN (FITV-2): on Raydium the EMA terms of
    L_b, L_slow included, also at most floor(L_mlo*(1e4 + REF_GUARD_TOL_BPS)/1e4), L_mlo the lower median of the
    ring's bucket liquidity lows -- L_b = max(L_live, min(max(L_ema', L_slow), floor(D*1e4/lambda_min),
    floor(L_mlo*1.01))); the spot leg at L_live as it was then.  Part 5b shows the forced liquidation it let an
    unlocked LP make."""

    def ref_depth_cap(self):
        cap = super().ref_depth_cap()
        if cap is None or not self.ring:
            return cap
        return min(cap, self.ring_medians()[2] * (BPS + REF_GUARD_TOL_BPS) // BPS)


class MhiFloor(Market):
    """Analysis rule (FITX-1; NOT adopted -- a candidate for the owner, O18): the spot leg also floored at L_mhi, the
    upper median of the ring's bucket liquidity highs (the settlement box's L_hi term, §5.6):
    L_spot = max(L_live, L_ema', L_mhi).  Every keeper-covered second carries the honest depth as its bucket high, so
    lowering L_mhi takes 5 of the 8 buckets attacker-only (which the #101 keeper denies): a genuinely reached low
    L_ema held by withdraw-wrapped cranks no longer lowers the leg.  A deposit-wrapped crank raises a bucket high
    (5 of 8 of them raise L_mhi), which delays the leg while such a freeze lasts."""

    def spot_liquidity(self, L_live):
        return max(super().spot_liquidity(L_live), self.ring_medians()[3])


class MhiSlowFloor(Market):
    """Analysis rule (FITX-1; NOT adopted -- a candidate for the owner, O18): as MhiFloor, the floor bounded by L_slow
    (whose input is min(lobs, L_mlo), which deposits do not raise): L_spot = max(L_live, L_ema', min(L_mhi, L_slow))."""

    def spot_liquidity(self, L_live):
        return max(super().spot_liquidity(L_live), min(self.ring_medians()[3], self.liq_slow))


RULE_CLS = {"built": BuiltM4, "m4r": Market, "noguard": NoGuard, "nobound": NoBound, "nocap": NoCap,
            "phicand": PhiCand, "fit100": Fit100, "s100": Spot100, "lows": LowsBound, "mhi": MhiFloor,
            "mhislow": MhiSlowFloor}
RULE_NAME = {"built": "M4 as built", "m4r": "M4R", "noguard": "M4R without the guard (analysis)",
             "nobound": "M4R without the step bound (analysis; before M4X-1)",
             "noslow": "M4R with L_slow unguarded (analysis; before M4X-2)",
             "nocap": "M4R without the reference cap (before M4F-1)",
             "phicand": "#99's fit with the carried fee caps at phi_cand (before FIT-2's fix)",
             "fit100": "#100's fit: min(phi_inc, phi_cand), no carried fee cap (before FITS-1)",
             "s100": "#100: the spot leg at L_live (before FITS-2)",
             "lows": "the withdrawn lows bound (FITV-2; analysis)",
             "mhi": "the spot leg floored at L_mhi (FITX-1 candidate; analysis, not adopted)",
             "mhislow": "the spot leg floored at min(L_mhi, L_slow) (FITX-1 candidate; analysis, not adopted)"}
RULES2 = ("built", "m4r")


def hdr(cols):
    print("| " + " | ".join(cols) + " |")
    print("|" + "---|" * len(cols))


def row(cells):
    print("| " + " | ".join(str(c) for c in cells) + " |")


# ======================================================================================================
# Engine W (warmup-gate's run_fast_path)
# ======================================================================================================
def lp_withdraw_all(v):
    """Withdraw every unlocked LP unit; returns the LP units to re-deposit."""
    units = v.lp_units - v.lp_locked
    if units <= 0:
        return 0
    v.X -= v.X * units // v.lp_units
    v.Y -= v.Y * units // v.lp_units
    v.lp_units -= units
    return units


def lp_deposit(v, units):
    """Deposit `units` LP units at the pool's current ratio (reserves scale proportionally)."""
    if units <= 0:
        return
    v.X += v.X * units // v.lp_units
    v.Y += v.Y * units // v.lp_units
    v.lp_units += units


def lp_withdraw_raw(v, units):
    v.X -= v.X * units // v.lp_units
    v.Y -= v.Y * units // v.lp_units
    v.lp_units -= units


class Wrap:
    """Context: an LP move around one instruction ('wd' = withdraw every unlocked LP, 'dep' = deposit as much
    again as the LP counter (L doubles), None = nothing)."""

    def __init__(self, v, move):
        self.v, self.move, self.u = v, move, 0

    def __enter__(self):
        if self.move == "wd":
            self.u = lp_withdraw_all(self.v)
        elif self.move == "dep":
            self.u = self.v.lp_units
            lp_deposit(self.v, self.u)
        return self

    def __exit__(self, *exc):
        if self.move == "wd":
            lp_deposit(self.v, self.u)
        elif self.move == "dep":
            lp_withdraw_raw(self.v, self.u)
        return False


P_BASE_X, P_BASE_Y = 150_000_000_000_000, 2_500 * LAMPORTS_PER_SOL      # RA's price


def pool_at(sol_, lock_bps, fee_ppm=2500, out_share_ppm=120_000, name="AMMv4"):
    """A Raydium pool with `sol_` SOL real at the base price and locked share lock_bps."""
    Y = int(sol_ * LAMPORTS_PER_SOL)
    X = P_BASE_X * Y // P_BASE_Y
    return RaydiumPool(X, Y, fee_ppm=fee_ppm, out_share_ppm=out_share_ppm, name=name, lock_bps=lock_bps)


TH = 600          # the incumbent halts at TH (position opened at 0, EMA converged)


class Cfg:
    def __init__(self, **kw):
        self.inc_sol = 100.0
        self.inc_lock = 10_000
        self.inc_fee = 2500
        self.cand_sol = 52.0
        self.cand_lock = 5000
        self.cand_fee = 2500
        self.cand_mult = 1.97          # candidate price at adoption / pre-halt price
        self.q_bps = 40                # position q, bps of the incumbent's X (reproducer: 40)
        self.rec_move = None           # LP move around the recording proposal
        self.exec_move = None          # LP move around the execution
        self.keeper = lambda s: True   # s = seconds after the adoption: honest keeper cranks + tries exits
        self.att_crank = None          # the attacker's per-second wrapped crank: None | "wd" | "dep"
        self.att_liq = None            # the attacker's per-second wrapped exit attempt: None | "wd" | "dep"
        self.att_price = None          # per-second atomic price bundle around a crank: None | ("pump"|"dump", k)
        self.held = None               # (s0, s1, move): a genuine LP move held from s0 to s1 after the adoption
        self.held_price = None         # (s0, s1, k): price held at k x the path from s0 to s1 (attacker-only)
        self.path = None               # s -> price multiple of the pre-halt price after the adoption (None: flat)
        self.secs = 120
        self.new_halt_at = None        # the adopted pool halts s seconds after the adoption
        self.observe_from = 1          # nobody observes before this many seconds after the adoption
        for k, v in kw.items():
            setattr(self, k, v)


def proposal_floor(rule, m, c):
    """The window floor of a proposal: M4 as built at P_ema; M4R at min(P_ema, P_cand) (M4R-2)."""
    if rule == "built":
        return position_floor_ok(c.locked_depth(), m.ema)
    return window_floor_ok(c.locked_depth(), m.ema, c.spot_wad())


def box_wide(m):
    P_lo, P_hi, L_lo, L_hi = m.settle_box(m.t)
    return (L_hi - L_lo) * BPS > DEFAULT.fv_dev_bps * L_lo


def run_fast_path(rule, cfg):
    """Returns a dict: adoption second (relative to the halt), exit (kind, seconds after adoption, who, spot
    multiple), L_ema/sqrt(XY) right after the adoption and at the end of the warm-up, borrower/lender P&L."""
    inc = pool_at(cfg.inc_sol, cfg.inc_lock, fee_ppm=cfg.inc_fee)
    m = RULE_CLS[rule](inc, TIER_C, DEFAULT)
    X0 = inc.X
    Q = X0 * cfg.q_bps // BPS
    pos = m.open(Q, 0)
    p0 = inc.spot_wad()
    for t in range(1, 61):
        m.step_to(t)
    for t in range(61, TH - 10, 30):
        m.step_to(t)
    for t in range(TH - 10, TH):
        m.step_to(t)
    inc.disabled = True
    c = pool_at(cfg.cand_sol, cfg.cand_lock, fee_ppm=cfg.cand_fee, out_share_ppm=160_000, name="CPMM")
    c.move_to_price(int(p0 * cfg.cand_mult))
    out = dict(adopt=None, exit=None, who=None, t_exit=None, mult=None, l_adopt=None, l_warm=None,
               p_warm=None, t_warm=None, cancelled=0, pos=pos, m=m, cand=c, Q=Q,
               wide_warm=0, wide_after=0, q_x=Q / c.X, t_clear=None)
    P = m.P
    t = TH
    l_rec = None
    fp_t0 = None
    t_adopt = None
    while t < TH + 1200 and t_adopt is None:
        t += 1
        m.step_to(t)                                   # the keeper's crank of the halted incumbent (chain)
        if m.pool_fail_since is None:
            continue
        armed = t >= m.pool_fail_since + P.pool_fast_route_s
        if fp_t0 is None and armed:
            snap = (c.X, c.Y)
            with Wrap(c, cfg.rec_move):
                ok = proposal_floor(rule, m, c)
                if ok:
                    l_rec = c.sqrt_k()
            c.X, c.Y = snap
            if ok:
                fp_t0 = t
        if fp_t0 is not None and t >= fp_t0 + P.pool_fast_window_s:
            snap = (c.X, c.Y)
            with Wrap(c, cfg.exec_move):
                live = c.sqrt_k()
                ok = position_floor_ok(c.locked_depth(), m.ema)
            c.X, c.Y = snap
            if ok:
                if rule == "built":
                    m.adopt_pool(c, t, l_rec=l_rec, l_live=live)
                else:
                    m.adopt_pool(c, t)
                t_adopt = t
            else:
                fp_t0 = None                           # cancelled
                out["cancelled"] += 1
    if t_adopt is None:
        return finish(m, pos, out, t, None)
    out["adopt"] = t_adopt - TH
    out["l_adopt"] = m.liq / c.sqrt_k()
    held_units = 0
    for s in range(1, cfg.secs + 1):
        t = t_adopt + s
        mult = cfg.path(s) if cfg.path else None
        if mult is not None and c.has_route():
            c.move_to_price(int(p0 * mult))
        if cfg.new_halt_at is not None and s == cfg.new_halt_at:
            c.disabled = True
        if cfg.held and s == cfg.held[0]:
            held_units = lp_withdraw_all(c) if cfg.held[2] == "wd" else 0
        if cfg.held and s == cfg.held[1] and held_units:
            lp_deposit(c, held_units)
            held_units = 0
        r = None
        who = None
        hp = cfg.held_price and cfg.held_price[0] <= s < cfg.held_price[1] and c.has_route()
        if hp:                                         # the attacker holds the price for the whole second
            base = c.spot_wad()
            c.move_to_price(int(base * cfg.held_price[2]))
        observed = False
        if s >= cfg.observe_from:
            if cfg.keeper(s):
                m.step_to(t)
                r = m.try_exit(pos, t)
                who = "keeper"
                observed = True
            if r is None and cfg.att_crank and c.has_route():
                with Wrap(c, cfg.att_crank):
                    m.advance(t)
                    m.observe(t)
                observed = True
            if r is None and cfg.att_price and c.has_route():
                kind, k = cfg.att_price
                b0 = c.spot_wad()
                snap = (c.X, c.Y)
                c.move_to_price(int(b0 * (k if kind == "pump" else 1 / k)))
                m.advance(t)
                m.observe(t)
                c.X, c.Y = snap
                observed = True
            if r is None and cfg.att_liq and c.has_route():
                m2, pos2 = copy.deepcopy((m, pos))     # a reverted liquidate records nothing
                c2 = m2.venue
                wr = Wrap(c2, cfg.att_liq)
                wr.__enter__()
                r2 = m2.try_exit(pos2, t)
                wr.__exit__()
                if r2 is not None:
                    m, pos, c, r = m2, pos2, c2, r2
                    who = "attacker"
        if not observed:
            m.advance(t)
        if hp:
            c.move_to_price(base)
        if c.has_route() and c.has_price():
            wide = box_wide(m)
            if m.warm > 0:
                out["wide_warm"] += wide
            elif s <= 60 + (EMA_WARMUP_BUCKETS if rule != "built" else 0):
                out["wide_after"] += wide
        if rule != "built" and m.warm == 0 and out["l_warm"] is None and c.has_price():
            out["l_warm"] = m.liq / c.sqrt_k()
            out["p_warm"] = m.ema / c.spot_wad()
            out["t_warm"] = s
        if rule != "built" and out["t_clear"] is None and not m.guard:
            out["t_clear"] = s
        if r is not None:
            out["who"] = who
            return finish(m, pos, out, t, r, t_adopt, p0)
    return finish(m, pos, out, t, None, t_adopt, p0)


def finish(m, pos, out, t, r, t_adopt=None, p0=None):
    if r is None:
        out["exit"] = "open"
        v = m.venue
        if v.has_route():
            qs = v.quote_at(pos.Q, *v.eff())
            R = m.rent_due(pos)
            out["borrower_eq"] = (pos.V - qs[1] - R) / LAMPORTS_PER_SOL if qs else None
        else:
            out["borrower_eq"] = None
        out["lender_sol"] = 0.0
    else:
        out["exit"] = r["kind"]
        out["t_exit"] = t - (t_adopt or 0)
        out["mult"] = r["spot"] / p0 if p0 else None
        out["borrower_eq"] = r["borrower"] / LAMPORTS_PER_SOL
        out["lender_sol"] = r["lender_sol"] / LAMPORTS_PER_SOL
    out["m"] = m
    return out


def wcell(o, money=False):
    """Engine W cell: outcome, k = keeper / a = attacker, seconds after the adoption, (borrower SOL at the exit
    or equity if open[ / lender SOL])."""
    if o["adopt"] is None:
        return "no adoption" + (" (window floor)" if not o["cancelled"] else f" ({o['cancelled']} cancelled)")
    if o["exit"] == "open":
        be = o.get("borrower_eq")
        return f"open ({be:.3f})" if be is not None else "open"
    if money:
        return (f"{o['exit']} {o['who'][0]} +{o['t_exit']}s @{o['mult']:.2f}x ({o['borrower_eq']:.3f} / "
                f"{o['lender_sol']:.3f})")
    return f"{o['exit']} {o['who'][0]} +{o['t_exit']}s ({o['borrower_eq']:.3f})"


def ramp(s0, k, dur, base=1.0):
    g = math.log(k) / dur
    return lambda s: base * math.exp(g * min(max(s - s0, 0), dur)) if s >= s0 else base


def step(s0, k, base=1.0):
    return lambda s: base * k if s >= s0 else base


# ======================================================================================================
# Engine P (invariant-seed's switch_run)
# ======================================================================================================
FAST_MIN_BPS = 2500


def unlocked_bps(v):
    return (v.lp_units - v.lp_locked) * BPS // v.lp_units


def wrapped(v, move):
    """The pool one instruction sees inside an LP round trip: 'wd' = every unlocked LP withdrawn
    (sqrt(X*Y) -> D), 'dep' = as much again as the LP counter deposited (sqrt(X*Y) x2, lambda /2).  Price
    unchanged."""
    if move is None:
        return v
    w = copy.deepcopy(v)
    if move == "wd":
        u = unlocked_bps(w)
        if u > 0:
            w.remove_liquidity(u)
    elif move.startswith("dep"):
        k = int(move[3:] or "1")
        w.add_liquidity(BPS * k)
    else:
        raise ValueError(move)
    return w


def restore(v, w):
    """After an instruction ran on the wrapped copy w of v (possibly trading), the round trip closes: the LP
    moved out (in) goes back (out) at the new reserve ratio."""
    if w is v:
        return
    v.X = w.X * v.lp_units // w.lp_units
    v.Y = w.Y * v.lp_units // w.lp_units


class _PWindow:
    """The collection window's hooks of engine P."""

    def floor_at_proposal(self, cand_sample):
        return window_floor_ok(cand_sample.locked_depth(), self.ema, cand_sample.spot_wad())

    def floor_at_execution(self, cand_sample):
        return position_floor_ok(cand_sample.locked_depth(), self.ema)

    def win_record(self, t, cand_sample):
        pass

    def win_observe(self, t, cand_sample):
        pass                                     # a re-proposal of the window's own candidate records nothing

    def win_adopt(self, pool, t, exec_sample):
        self.adopt_pool(pool, t)


class PBuilt(_PWindow, BuiltM4):
    def floor_at_proposal(self, cand_sample):
        return position_floor_ok(cand_sample.locked_depth(), self.ema)

    def win_record(self, t, cand_sample):
        self.fp_lrec = cand_sample.sqrt_k()

    def win_adopt(self, pool, t, exec_sample):
        self.adopt_pool(pool, t, l_rec=self.fp_lrec, l_live=exec_sample.sqrt_k())


class PM4R(_PWindow, Market):
    pass


class PNoGuard(_PWindow, NoGuard):
    pass


class PNoSlow(_PWindow, NoSlowGuard):
    pass


class PNoCap(_PWindow, NoCap):
    pass


class PPhiCand(_PWindow, PhiCand):
    pass


class PFit100(_PWindow, Fit100):
    pass


P_RULES = {"built": PBuilt, "m4r": PM4R, "noguard": PNoGuard, "noslow": PNoSlow, "nocap": PNoCap,
           "phicand": PPhiCand, "fit100": PFit100}


# The keeper's coverage after the adoption (engine P's Spec.keeper_rule; ECON §5.8 duties 1, 2, 9; services/keeper
# decide::market_needs and coverage::MarketTrack).  Every other engine-P row has the keeper crank every second.
IDLE_S = 30            # duty 1: every idle_interval_s on a market with open positions
NEAR_CR_BPS = 13_000   # duty 1: per second while the live CR (risk fee) is below 1.3 L (near_cr_bps)
EMA_DEV_BPS = 200      # duty 1: per second while |P_spot/P_ema - 1| > 2 % (ema_dev_bps)
BURST_DEV_BPS = 200    # duty 2: a foreign observation beyond 2 % of the keeper's own view (burst_dev_bps) ...
BURST_S = 60           # ... starts burst_s seconds of per-slot coverage (a price deviation)
LIQ_COVER_S = 10       # duty 2 (#101, FITS-2 (b)): a foreign bucket LIQUIDITY deviation on a market with open positions
                       # starts per-SECOND coverage of the next liq_cover_s seconds, extended while deviations recur


class Coverage:
    """When the keeper observes after the adoption (engine P, Spec.keeper_rule; the steady-state runs of parts 4e, 5b).
      "old"   the keeper as built at M4R: per second while ema_warm > 0 (duty 9), near a threshold (live CR < 1.3 L)
              or |P_spot/P_ema - 1| > 2 % (duty 1), and in a duty-2 burst, which only a foreign observation whose
              bucket PRICE deviates starts; otherwise every 30 s (duty 1's idle cadence).
      "m4f1"  M4F-1 (#98; its liquidity bursts REVERTED by #99, M4G-2): duty 9 also while ema_guard = 1 (a market
              with positions after a switch, until the guard clears), and duty 2 compares the bucket's liquidity
              (llo, lhi) with the keeper's own sqrt(X*Y) as well, so every LP-wrapped crank starts (and, repeated,
              extends) a 60-s per-slot burst, on any market.
      "n99"   the keeper after #99: duty 9 also while ema_guard = 1, bursts on PRICE deviations only (as "old").
              After the guard clears it is the old keeper.
      "new"   the keeper after #101 (FITS-2 (b); ECON §5.8 duty 2): "n99", and on a market WITH open positions a
              foreign bucket whose liquidity (llo / lhi) deviates from the keeper's own sqrt(X*Y) by more than
              burst_dev_bps starts per-SECOND coverage of the next LIQ_COVER_S seconds, extended while deviations
              recur -- one keeper crank per second for every second the attacker keeps wrapping (1:1), so a
              withdraw- or deposit-wrapped crank gets one attacker-only second (the one that started the coverage),
              then shares every bucket with an honest observation; a market without positions never gets it; price
              bursts are unchanged.
    A burst starts in the second after the foreign observation (the attacker's second stays attacker-only: the
    keeper sees it only once it has landed) and covers every second up to BURST_S after it.  In the sim a burst's
    per-slot coverage is one crank per second, which is all the bucket extremes need; the per-slot / per-second
    difference is the keeper's cost (keeper_cost.py)."""

    def __init__(self, rule):
        assert rule in ("old", "m4f1", "n99", "new")
        self.rule, self.last, self.burst_until, self.liq_until = rule, None, None, None

    def due(self, m, pos, t):
        v = m.venue
        if not v.has_route():
            return self.last is None or t - self.last >= IDLE_S
        if self.burst_until is not None and t <= self.burst_until:
            return True
        if self.liq_until is not None and t <= self.liq_until:
            return True
        if m.warm > 0 or (self.rule != "old" and m.guard):
            return True
        spot = v.spot_wad()
        if abs(spot - m.ema) * BPS > m.ema * EMA_DEV_BPS:
            return True
        qr = m.quote_spot(pos.Q, risk=True)
        cr = (pos.V - m.rent_due(pos)) * BPS // qr[1] if (qr and qr[1] > 0) else 0
        if cr * BPS < pos.L * NEAR_CR_BPS:
            return True
        return self.last is None or t - self.last >= IDLE_S

    def observed(self, t):
        self.last = t

    def check_foreign(self, m, t, view):
        """After second t: the bucket of t against the keeper's own view (spot, sqrt(X*Y)) of the pool in t."""
        if m.b_sec != t or view is None:
            return
        p, lq = view
        lo, hi = BPS - BURST_DEV_BPS, BPS + BURST_DEV_BPS
        dev = m.b_lo * BPS < p * lo or m.b_hi * BPS > p * hi
        ldev = m.b_llo * BPS < lq * lo or m.b_lhi * BPS > lq * hi
        if self.rule == "m4f1":
            dev = dev or ldev
        if dev:
            self.burst_until = max(self.burst_until or t, t + BURST_S - 1)
        elif self.rule == "new" and ldev and m.open_positions > 0:
            self.liq_until = max(self.liq_until or t, t + LIQ_COVER_S)


def ray_pool(sol_real, lock_bps, fee_ppm=2500, out_share_ppm=120_000, name="AMMv4"):
    """A Raydium pool with sol_real SOL at the RC token price (PUMP_POOL_TOKENS per 84.99 SOL)."""
    xps = PUMP_POOL_TOKENS / 84.99
    return RaydiumPool(int(xps * sol_real), int(sol_real * LAMPORTS_PER_SOL), fee_ppm=fee_ppm,
                       out_share_ppm=out_share_ppm, name=name, lock_bps=lock_bps)


def _fee(v, fee_ppm):
    v.fee_ppm = fee_ppm
    return v


INCUMBENTS = {
    "R100": (lambda: ray_pool(100, 10_000), "C"),          # the M4R-1 reproducer's incumbent: 100 SOL, fully locked
    "RA": (lambda: MARKETS["RA"](), "A"),
    "RC": (lambda: MARKETS["RC"](), "C"),
    "RAC": (lambda: MARKETS["RA"](), "C"),                  # RA's pool at Tier C: the fork's M4X-2 / FIT-2 market
    "RAC1": (lambda: _fee(MARKETS["RA"](), 10_000), "C"),   # the same pool at a 1 % fee (FIT-2's 1 % -> 1 % rows)
}


class Spec:
    def __init__(self, **kw):
        self.inc = "R100"
        self.size = 1.0                  # position = size x max_open_q
        self.cand_sol = 52               # candidate's real SOL at the pre-halt price
        self.cand_lock = 5000            # candidate lambda (bps)
        self.m = 1.0                     # candidate price at the halt, x pre-halt spot (a move during the halt)
        self.post = None                 # t_rel (s after adoption) -> multiple of the adoption price, or None
        self.rec_move = None             # attacker makes the recording proposal inside an LP move
        self.exec_move = None            # attacker executes inside an LP move
        self.rec_pump = None             # attacker makes the recording proposal inside a price push (x spot)
        self.win_move = None             # (f, s): the candidate genuinely moves to f x at window second s (M4X-3)
        self.win_lp = None               # attacker re-proposes an LP-wrapped candidate every window second
        self.freeze = None               # attacker's per-second wrapped crank after adoption: 'wd' / 'dep'
        self.freeze_from = 1             # ... from this second after adoption
        self.att_liq = None              # attacker's per-second wrapped liquidation after adoption: 'wd'
        self.att_first = False           # the attacker's transactions land before the keeper's in each second
        self.no_exit_until = 0           # the keeper does not try exits before this second after adoption
        self.keeper_off = ()             # absolute-second ranges (a, b) without the keeper
        self.keeper_rule = None          # after the adoption: None = the keeper cranks every second; "old" /
                                         # "m4f1" / "n99" / "new" = its coverage duties (Coverage; parts 4d, 11)
        self.fit = False                 # #99: the proposal (at min(P_ema, P_cand)) and the execution (at P_ema) also
                                         # need every open position to fit the candidate (Market.carried_fit); the
                                         # parts before 11 force the adoption (False)
        self.to_s3 = False               # without an adoption: the keeper keeps the S3 chain (a crank every 30 s)
                                         # and settles at the position's deadline
        self.held_dep = None             # (s0, s1, k): an LP deposits k x the pool's LP counter at s0 after the
                                         # adoption and withdraws it at s1 (a real, held deposit; M4G-3)
        self.cand_fee = None             # the candidate's trade fee (ppm; None = the incumbent's; FIT-2's 1 % rows)
        self.horizon = 120               # seconds after adoption
        self.pre_warm = TE               # honest period before the halt
        for k, v in kw.items():
            setattr(self, k, v)


def as_tuple(x):
    if x is None:
        return ()
    return x if isinstance(x, tuple) else (x,)


def keeper_on(spec, t):
    return not any(a <= t < b for a, b in spec.keeper_off)


def try_liq(m, pos, t, move=None):
    v = m.venue
    w = wrapped(v, move)
    m.venue = w
    r = m.try_exit(pos, t)
    m.venue = v
    restore(v, w)
    return r


def obs_wrapped(m, t, move):
    v = m.venue
    w = wrapped(v, move)
    m.advance(t)
    m.venue = w
    m.observe(t)
    m.venue = v
    restore(v, w)


def pump_cost(v, k):
    """Round-trip cost (lamports) of pushing v's spot to k x and back inside one transaction (no arbitrage)."""
    w = copy.deepcopy(v)
    d = move_size(w, int(w.spot_wad() * k))
    if d > 0:
        c = w.buy(d)
        return c - w.sell(d)
    if d < 0:
        got = w.sell(-d)
        return w.buy(-d) - got if -d < w.X else 0
    return 0


def switch_run(spec, rule):
    """Returns a dict: adopted, t_adopt, l_ratio (L_ema/sqrt(X*Y) of the adopted pool at the switch), exit (result
    dict or None), t_exit_rel, who, box_wide (seconds of the first 60 after adoption with the box > fv_dev wide in
    L), t_clear (seconds after the adoption at which the guard cleared), cost (attacker round-trip fees); with
    spec.keeper_rule also k_secs (seconds after the adoption the keeper observed) and att_only (seconds after the
    warm-up with the attacker's observation and none of the keeper's)."""
    f, tier = INCUMBENTS[spec.inc]
    m = P_RULES[rule](f(), TIERS[tier], DEFAULT)
    Q = max(1, int(m.max_open_q() * spec.size))
    pos = m.open(Q, 0)
    t = 0
    while t < spec.pre_warm - 60:
        t += 10
        m.step_to(t)
    while t < spec.pre_warm:
        t += 1
        m.step_to(t)
    inc = m.venue
    p_pre = inc.spot_wad()
    cand = RaydiumPool(inc.X * spec.cand_sol * LAMPORTS_PER_SOL // inc.Y,
                       spec.cand_sol * LAMPORTS_PER_SOL,
                       fee_ppm=inc.fee_ppm if spec.cand_fee is None else spec.cand_fee,
                       out_share_ppm=inc.out_share_ppm, name="cand", lock_bps=spec.cand_lock)
    cand.move_to_price(int(p_pre * spec.m))
    pool_d = inc.locked_depth()
    inc.disabled = True
    out = dict(pool_d=pool_d, adopted=False, t_adopt=None, l_ratio=None, exit=None, t_exit_rel=None, who=None,
               box_wide=None, cost=0, pos=pos, p_pre=p_pre, cancelled=0, floor_refused=0, t_clear=None,
               k_secs=0, att_only=0, fit_refused=0, fit_cancelled=0, s3_t=None)
    held_units = 0
    cov = Coverage(spec.keeper_rule) if spec.keeper_rule else None
    fp_t0 = None
    t_adopt = None
    wide = 0
    t_end = spec.pre_warm + 400 + spec.horizon
    while t < t_end:
        t += 1
        kon = keeper_on(spec, t)
        if t_adopt is not None and spec.post is not None:
            g = spec.post(t - t_adopt)
            if g is not None:
                m.venue.move_to_price(int(out["p_adopt"] * g))
        if t_adopt is not None and spec.held_dep is not None:
            s0, s1, k = spec.held_dep
            if t - t_adopt == s0:
                held_units = m.venue.lp_units * k
                lp_deposit(m.venue, held_units)
            elif t - t_adopt == s1 and held_units:
                lp_withdraw_raw(m.venue, held_units)
                held_units = 0
        if t_adopt is None:
            # --- halted incumbent; the window ------------------------------------------------------
            armed = m.pool_fail_since is not None and t >= m.pool_fail_since + m.P.pool_fast_route_s
            if fp_t0 is None:
                if kon:
                    m.step_to(t)                       # keeper crank: the pool_fail chain
                if armed and (kon or spec.rec_move or spec.rec_pump):
                    # the recording proposal: the attacker's (wrapped) if it plays, else the keeper's
                    m.advance(t)
                    m.observe(t)                       # its prologue on the halted incumbent
                    s = wrapped(cand, spec.rec_move)
                    if spec.rec_pump:
                        s = copy.deepcopy(cand)
                        s.move_to_price(int(s.spot_wad() * spec.rec_pump))
                    fast_ok = s.locked_depth() * BPS >= FAST_MIN_BPS * out["pool_d"]
                    floor_ok = fast_ok and m.floor_at_proposal(s)
                    if floor_ok and (not spec.fit or m.carried_fit(s)):
                        m.win_record(t, s)
                        fp_t0 = t
                        if spec.rec_pump:
                            out["cost"] += pump_cost(cand, spec.rec_pump)
                    elif floor_ok:
                        out["fit_refused"] += 1            # #99: CarriedPositionsTooLarge, no window
                    else:
                        out["floor_refused"] += 1
                continue
            if t < fp_t0 + m.P.pool_fast_window_s:
                if spec.win_move and t == fp_t0 + spec.win_move[1]:
                    cand.move_to_price(int(cand.spot_wad() * spec.win_move[0]))
                if kon:
                    m.advance(t)
                    m.observe(t)                       # prologue on the incumbent
                    m.win_observe(t, cand)
                for mv in as_tuple(spec.win_lp):
                    m.advance(t)
                    m.observe(t)
                    m.win_observe(t, wrapped(cand, mv))
                continue
            # --- the execution --------------------------------------------------------------------
            if not (kon or spec.exec_move):
                continue
            m.advance(t)
            m.observe(t)
            s = wrapped(cand, spec.exec_move)
            chain_ok = (m.pool_fail_since is not None and m.pool_fail_since + m.P.pool_fast_route_s <= fp_t0
                        and m.pool_fail())
            fit_ok = not spec.fit or m.carried_fit(s, execution=True)
            if not fit_ok:
                out["fit_cancelled"] += 1
            if chain_ok and m.floor_at_execution(s) and fit_ok:
                m.win_adopt(cand, t, s)
                t_adopt = t
                out.update(adopted=True, t_adopt=t, l_ratio=m.liq / cand.sqrt_k(), p_adopt=cand.spot_wad(), x_adopt=cand.X,
                           ema_adopt=m.ema, p_ratio=m.ema / cand.spot_wad(), t_clear=None if m.guard else 0)
            else:
                out["cancelled"] += 1
                fp_t0 = None
            continue
        # --- after the adoption: keeper crank + liquidation, attacker's freeze and wrapped liquidation
        tr = t - t_adopt
        view = None
        if cov is not None:                            # part 4d: the keeper's coverage duties decide
            v = m.venue
            view = (v.spot_wad(), v.sqrt_k()) if (v.has_route() and v.has_price()) else None
            kon = cov.due(m, pos, t)
        done = False
        k_obs = a_obs = False
        for actor in (("attacker", "keeper") if spec.att_first else ("keeper", "attacker")):
            if actor == "keeper" and kon:
                m.step_to(t)
                k_obs = True
                if cov is not None:
                    cov.observed(t)
                if tr < spec.no_exit_until:
                    continue
                r = m.try_exit(pos, t)
                if r is not None:
                    out.update(exit=r, t_exit_rel=tr, who="keeper")
                    done = True
                    break
            if actor == "attacker":
                if tr >= spec.freeze_from:
                    for mv in as_tuple(spec.freeze):
                        obs_wrapped(m, t, mv)
                        a_obs = True
                if spec.att_liq:
                    r = try_liq(m, pos, t, spec.att_liq)
                    if r is not None:
                        out.update(exit=r, t_exit_rel=tr, who="attacker")
                        done = True
                        break
        if cov is not None:
            cov.check_foreign(m, t, view)
            out["k_secs"] += k_obs
            out["att_only"] += a_obs and not k_obs and m.warm == 0
        if done:
            break
        if out["t_clear"] is None and not m.guard:
            out["t_clear"] = tr
        if "p_gate" not in out and m.warm == 0 and m.venue.has_price():
            out["p_gate"] = m.ema / m.venue.spot_wad()
        if tr <= 60:
            P_lo, P_hi, L_lo, L_hi = m.settle_box(t)
            if (L_hi - L_lo) * BPS > DEFAULT.fv_dev_bps * L_lo:
                wide += 1
        if tr >= spec.horizon:
            break
    if spec.to_s3 and t_adopt is None and out["exit"] is None:
        # #99: no candidate fits -- the position stays on the halted incumbent, never liquidatable at the frozen P_ema,
        # and exits through S3 at its deadline (duty 5: a crank every 30 s keeps the chain)
        while t < pos.deadline + 3600:
            t += 30
            m.step_to(t)
            if t >= pos.deadline:
                r = m.try_exit(pos, t)
                if r is not None:
                    out.update(exit=r, who="keeper", s3_t=t - spec.pre_warm)
                    break
    out["box_wide"] = wide
    out["m"] = m
    out["t_end"] = t
    return out


def lender_pnl(o):
    r = o["exit"]
    if r is None:
        return 0
    pos = o["pos"]
    return r["lender_sol"] - (pos.Q - r["tokens"]) * r["spot"] // WAD


def pcell(o, with_money=True):
    if not o["adopted"]:
        return "no adoption" + (f" (refused {o['floor_refused']}, cancelled {o['cancelled']})"
                                if (o["floor_refused"] or o["cancelled"]) else "")
    r = o["exit"]
    if r is None:
        return f"open ({o['t_end'] - o['t_adopt']} s)"
    who = "" if o["who"] == "keeper" else " by attacker"
    s = f"{r['kind']} +{o['t_exit_rel']} s{who} @{r['spot'] / o['p_pre']:.3f}x"
    if with_money:
        s += f", lender {sol(lender_pnl(o)):+.3f}, borrower {sol(r['borrower']):.3f}"
    return s


def lratio(o):
    return "-" if not o["adopted"] else f"{o['l_ratio']:.3f}"


# ======================================================================================================
# Engine M (multi-sample's run_plan)
# ======================================================================================================
P0X = 150_000_000_000_000
TH_M = 60


def ms_withdraw(v, lp):
    dx, dy = v.X * lp // v.lp_units, v.Y * lp // v.lp_units
    v.X -= dx
    v.Y -= dy
    v.lp_units -= lp


def ms_deposit(v, lp):
    dx, dy = cdiv(v.X * lp, v.lp_units), cdiv(v.Y * lp, v.lp_units)
    v.X += dx
    v.Y += dy
    v.lp_units += lp


def with_move(v, move, fn):
    """Runs fn() with pool v wrapped in `move`; the round trip restores the pool (bar rounding).
    move: None | 'wd' (every unlocked LP withdrawn) | 'dep' (as much again as the LP counter deposited) |
    ('pump', f) / ('dump', f) (spot moved by a factor f with a swap, reversed after, reserves restored)."""
    if move is None:
        return fn()
    if move == "wd":
        lp = v.lp_units - v.lp_locked
        if lp <= 0:
            return fn()
        ms_withdraw(v, lp)
        try:
            return fn()
        finally:
            ms_deposit(v, lp)
    if move == "dep":
        lp = v.lp_units
        ms_deposit(v, lp)
        try:
            return fn()
        finally:
            ms_withdraw(v, lp)
    kind, f = move
    X = v.X
    snap = (v.X, v.Y, v.lp_units)
    if kind == "pump":
        d = X - math.isqrt(X * X * BPS // int(f * BPS))       # X' = X/sqrt(f)
        paid = v.buy(d)
        try:
            return fn()
        finally:
            got = v.sell(d)
            v.wrap_cost = getattr(v, "wrap_cost", 0) + paid - got
            v.X, v.Y, v.lp_units = snap
    if kind == "dump":
        d = math.isqrt(X * X * int(f * BPS) // BPS) - X       # X' = X*sqrt(f)
        got = v.sell(d)
        try:
            return fn()
        finally:
            paid = v.buy(d)
            v.wrap_cost = getattr(v, "wrap_cost", 0) + paid - got
            v.X, v.Y, v.lp_units = snap
    raise ValueError(move)


def sample_of(v, move):
    return with_move(v, move, lambda: (v.spot_wad(), v.sqrt_k()))


def ms_market(rule):
    v = RaydiumPool(P0X, 100 * LAMPORTS_PER_SOL, fee_ppm=2500, out_share_ppm=120_000, name="AMMv4", lock_bps=9978)
    return RULE_CLS[rule](v, TIER_C, DEFAULT)


def ms_cand(sol_at_p0=52.0, lam=5000):
    y = int(sol_at_p0 * LAMPORTS_PER_SOL)
    x = P0X * y // (100 * LAMPORTS_PER_SOL)
    return RaydiumPool(x, y, fee_ppm=2500, out_share_ppm=160_000, name="CPMM", lock_bps=lam)


def run_plan(rule, mult=1.0, q_bps=40, rec_move=None, exec_move=None, post_crank="wd", post_liq="wd",
             secs=90, pre_mult=1.0):
    m = ms_market(rule)
    P = m.P
    Q = m.venue.X * q_bps // BPS
    pos = m.open(Q, 0)
    P0 = pos.spot_post
    t = 0
    for t in range(1, TH_M + 1):
        if t == 1 and pre_mult != 1.0:
            m.exo_move(int(P0 * pre_mult))
        m.step_to(t)
        if pre_mult != 1.0 and m.try_exit(pos, t) is not None:
            return dict(adopted=False, result="liquidated before the halt", pos=pos, m=m), m
    m.venue.disabled = True
    c = ms_cand()
    c.move_to_price(int(P0 * mult))
    out = dict(P0=P0, Q=Q, pos=pos, exit=None, exit_by=None, exit_s=None, adopted=False, c=c, m=m)
    while True:
        t += 1
        m.step_to(t)
        if m.pool_fail_since is not None and t >= m.pool_fail_since + P.pool_fast_route_s:
            break
    D = c.locked_depth()
    p_rec, l_rec = sample_of(c, rec_move)
    ok = position_floor_ok(D, m.ema) if rule == "built" else window_floor_ok(D, m.ema, p_rec)
    if not ok:
        out.update(result="no window (window floor)")
        return out, m
    due = t + P.pool_fast_window_s
    while t < due - 1:
        t += 1
        m.step_to(t)
    t = due
    m.step_to(t)
    if not position_floor_ok(D, m.ema):
        out.update(result="cancelled at execution (window floor)")
        return out, m
    if rule == "built":
        _, l_live = sample_of(c, exec_move)
        m.adopt_pool(c, t, l_rec=l_rec, l_live=l_live)
    else:
        m.adopt_pool(c, t)
    out.update(adopted=True, t_adopt=t, l_ratio=m.liq / c.sqrt_k(), p_ratio=m.ema / c.spot_wad())
    for s in range(1, secs + 1):
        t += 1
        r = None
        m.step_to(t)
        r = m.try_exit(pos, t)
        if r is not None:
            out.update(exit=r, exit_by="keeper", exit_s=s)
        if r is None and post_crank is not None:
            with_move(c, post_crank, lambda: m.observe(t))
        if r is None and post_liq is not None:
            r = with_move(c, post_liq, lambda: m.try_exit(pos, t))
            if r is not None:
                out.update(exit=r, exit_by="attacker", exit_s=s)
        if r is not None:
            break
    out["m"] = m
    out["t_end"] = t
    return out, m


def honest_health(mult, q_bps=40):
    """CR of the position on the candidate's honest state (live quote at the true depth / at D)."""
    m = ms_market("m4r")
    Q = m.venue.X * q_bps // BPS
    pos = m.open(Q, 0)
    c = ms_cand()
    c.move_to_price(int(pos.spot_post * mult))
    R = m.rent_due(pos)
    q = c.quote_at(Q, *c.eff(), cap_ppm=m.cap_ppm())
    qd = with_move(c, "wd", lambda: c.quote_at(Q, *c.eff(), cap_ppm=m.cap_ppm()))
    return (pos.V - R) * BPS // q[1], (pos.V - R) * BPS // qd[1]


def mcell(o):
    if not o["adopted"]:
        return o["result"]
    r = o["exit"]
    if r is None:
        return "open"
    return f"{r['kind']} by {o['exit_by']} +{o['exit_s']} s"


def mpnl(o):
    """borrower P&L / lender P&L vs holding the tokens / attacker's keeper reward (SOL) at the candidate's final
    price; an open position is marked at its live buyback cost + rent."""
    pos, m, c = o["pos"], o["m"], o["c"]
    p_term = c.spot_wad()
    r = o["exit"]
    if r is None:
        st = m.status(pos, o["t_end"])
        mtm = st["qs"][1] + st["R"]
        return f"{(pos.V - mtm - pos.fee_open - pos.M) / 1e9:+.4f} / +0.0000 / 0.0000"
    b = r["borrower"] - pos.M - pos.fee_open
    lnd = r["lender_sol"] - (pos.Q - r["tokens"]) * p_term // WAD
    att = r["keeper"] if o["exit_by"] == "attacker" else 0
    return f"{b / 1e9:+.4f} / {lnd / 1e9:+.4f} / {att / 1e9:.4f}"


# ======================================================================================================
# Parts
# ======================================================================================================
WRAPS = (("none", None, None), ("withdraw around execute", None, "wd"),
         ("withdraw around proposal", "wd", None), ("withdraw around both", "wd", "wd"))


def part1():
    print("## 1. M4R-1: a lambda 50 % candidate (52 SOL at the pre-halt price) whose every unlocked LP unit the")
    print("attacker holds; the position carried from a 100-SOL fully locked AMM v4 (Tier C, cr_liq 1.5).  The")
    print("candidate trades at m x the pre-halt price, flat after the adoption.  Every second after the adoption:")
    print("the keeper's honest crank and plain liquidation attempt; the attacker's [withdraw all, crank, deposit]")
    print("and [withdraw all, liquidate, deposit].  Cells: outcome, k = keeper / a = attacker, seconds after the")
    print("adoption, (borrower SOL at the exit, or equity if open).  120 s.  The honest threshold is m 2.01 (the")
    print("'none' rows): every earlier exit in a wrapped row is forced.")
    print()
    for q in (40, 74):
        print(f"### q = {q / 100:.2f} % of the incumbent's X")
        print()
        hdr(["m", "LP wraps of the two samples"] + [RULE_NAME[r] for r in RULES2])
        for mult in (1.96, 1.97, 1.98, 1.99, 2.00, 2.01, 2.02, 2.05):
            for name, rm, em in WRAPS:
                cells = [wcell(run_fast_path(r, Cfg(cand_mult=mult, q_bps=q, rec_move=rm, exec_move=em,
                                                   att_crank="wd", att_liq="wd", secs=120))) for r in RULES2]
                row([f"{mult:.2f}", name] + cells)
        print()
    print("Control (q 0.40 %, withdraw around execute, NO per-second wrapped crank; the attacker still tries a")
    print("withdraw-wrapped liquidation every second):")
    print()
    hdr(["m"] + [RULE_NAME[r] for r in RULES2])
    for mult in (1.98, 1.99, 2.00):
        row([f"{mult:.2f}"] + [wcell(run_fast_path(r, Cfg(cand_mult=mult, exec_move="wd", att_liq="wd", secs=120)))
                               for r in RULES2])
    print()


def part2():
    print("## 2. lambda < lambda_min candidates under the M4R-1 attacker")
    print()
    print("### 2a. 240 SOL at 25 % / 400 SOL at 10 % (engine W; withdraw-wrapped execute, per-second withdraw-")
    print("wrapped crank and liquidation), q 0.74 %, flat at m.  Honest: liquidatable at 2.05, not at 2.03.")
    print()
    hdr(["candidate", "m", "honest (M4R)"] + [RULE_NAME[r] for r in RULES2])
    for sol_, lock in ((240, 2500), (400, 1000)):
        for mult in (1.99, 2.00, 2.01, 2.02, 2.03, 2.05):
            h = wcell(run_fast_path("m4r", Cfg(cand_sol=sol_, cand_lock=lock, cand_mult=mult, secs=120, q_bps=74)))
            cells = [wcell(run_fast_path(r, Cfg(cand_sol=sol_, cand_lock=lock, cand_mult=mult, exec_move="wd",
                                                att_crank="wd", att_liq="wd", secs=120, q_bps=74)))
                     for r in RULES2]
            row([f"{sol_} SOL, {lock / 100:.0f} %", f"{mult:.2f}", h] + cells)
    print()
    print("### 2b. The fine grid of a 104-SOL lambda 25 % candidate (engine P, R100, position at the size cap):")
    print("honest vs attacker a2 (every sample withdraw-wrapped, its wrapped liquidation first in each second).")
    print()
    hdr(["rule", "m", "honest only", "a2 attacked", "L_ema / √(X·Y) at the switch (a2)"])
    for rule in RULES2:
        for mm in (2.00, 2.01, 2.02, 2.03, 2.04):
            h = switch_run(Spec(cand_sol=104, cand_lock=2500, m=mm, horizon=120), rule)
            a = switch_run(Spec(cand_sol=104, cand_lock=2500, m=mm, rec_move="wd", exec_move="wd", win_lp="wd",
                                freeze="wd", att_liq="wd", att_first=True, horizon=120), rule)
            row([RULE_NAME[rule], f"{mm:.2f}", pcell(h, False), pcell(a, False), lratio(a)])
    print()


def diag(o):
    la = "-" if o["l_adopt"] is None else f"{o['l_adopt']:.3f}"
    lw = "-" if o["l_warm"] is None else f"{o['l_warm']:.3f}"
    return la, lw, o["wide_warm"], o["wide_after"]


def part3():
    print("## 3. Honest fast path onto thinner / deeper (M4-SEC-6) / lambda < lambda_min (M4V-3) candidates;")
    print("incumbent 100 SOL fully locked; honest keeper; then a 2.3x/120 s pump from +30 s (q 0.40 %).  Columns:")
    print("L_ema / √(X·Y) right after the adoption and at the gate (M4R), seconds the box is wider than 1 % in L")
    print("during the warm-up / in the 60 s after it (M4 as built: the first 60 s), the exit in the pump, and the")
    print("second the guard cleared (M4R).")
    print()
    hdr(["candidate (SOL at the pre-halt price, lambda)", "rule", "L at adoption", "L at the gate",
         "box wide: warm-up / after", "exit in the pump", "guard cleared"])
    for sol_, lock in ((30, 9978), (52, 5000), (150, 9000), (150, 9978), (120, 2500), (240, 2500), (400, 1000)):
        for rule in RULES2:
            o = run_fast_path(rule, Cfg(cand_sol=sol_, cand_lock=lock, cand_mult=1.0, path=ramp(30, 2.3, 120),
                                        secs=400))
            la, lw, ww, wa = diag(o)
            tc = "-" if rule == "built" else f"+{o['t_clear']} s"
            row([f"{sol_} SOL, {lock / 100:.2f} %", RULE_NAME[rule], la, lw, f"{ww} / {wa}", wcell(o), tc])
    print()


UP = [("none", {}),
      ("deposit around the recording proposal (M4V-2)", dict(rec_move="dep")),
      ("deposit around the execution", dict(exec_move="dep")),
      ("deposit around both", dict(rec_move="dep", exec_move="dep")),
      ("deposit freeze after the adoption only", dict(freeze="dep")),
      ("both + deposit freeze after", dict(rec_move="dep", exec_move="dep", freeze="dep"))]


def part4():
    print("## 4. Capital deposit freeze: the attacker deposits as much again as the pool's LP counter around")
    print("sampling instructions (capital only, no swap fee) and around one crank every second from the adoption")
    print("(up-freeze).  'M4R without the guard' is an analysis row: the exit reference would read the frozen L_ema.")
    for inc, cs, title in (("RA", 630, "RA -> 630 SOL, lambda 99.78 % (D = 50 % of pool_d; q = 3.0 % of the "
                                       "candidate's X)"),
                           ("RA", 314, "RA -> 314 SOL, lambda 99.78 % (D = 25 % of pool_d, the fast-path minimum; "
                                       "q = 6.0 %)")):
        print()
        print(f"### 4a. {title}; genuine pump 3x/120 s from the adoption (engine P).  Cells: L_ema / √(X·Y) at the")
        print("switch; seconds with the box > 1 % wide in L (first 60 s); outcome (time after the adoption, price vs")
        print("pre-halt, lender P&L at the exit price, borrower remainder).")
        print()
        rules = ("built", "m4r", "noguard")
        hdr(["LP move"] + [RULE_NAME[r] for r in rules])
        for name, kw in UP:
            cells = [name]
            for rule in rules:
                o = switch_run(Spec(inc=inc, cand_sol=cs, cand_lock=9978, post=ramp_path(1, 3.0, 120), horizon=400,
                                    **kw), rule)
                cells.append(f"{lratio(o)}; {o['box_wide']} s; {pcell(o)}")
            row(cells)
    print()
    print("### 4b. Engine W (honest keeper every second): near-threshold rows and the big carried position (1 %-fee")
    print("incumbent of 200 SOL at lambda 100 %, q at the size cap, onto a 50-SOL lambda 99.78 % 1 %-fee candidate,")
    print("D = 25 % of pool_d).  Cells: outcome, seconds after the adoption (borrower / lender SOL, or equity if")
    print("open).  Honest threshold of the q 0.40 % position: m 2.01.")
    print()
    hdr(["attack", "m"] + [RULE_NAME[r] for r in RULES2])
    big = dict(inc_sol=200, inc_fee=10000, cand_fee=10000, cand_sol=50, cand_lock=9978, q_bps=250)
    rows = [
        ("deposit-wrapped crank every second (up-freeze)", 2.01, dict(att_crank="dep")),
        ("deposit-wrapped crank every second (up-freeze)", 2.02, dict(att_crank="dep")),
        ("deposit-wrapped crank every second (up-freeze)", 2.03, dict(att_crank="dep")),
        ("deposit-wrapped crank every second (up-freeze)", 2.05, dict(att_crank="dep")),
        ("deposit-wrapped crank every second + deposit-wrapped execute", 2.05, dict(att_crank="dep", exec_move="dep")),
        ("deposit-wrapped crank + withdraw-wrapped liquidation every second", 1.99,
         dict(att_crank="dep", att_liq="wd")),
        ("big carried position, no attacker", 2.0, dict(big)),
        ("big carried position, deposit-wrapped crank every second (capital = the pool)", 2.0,
         dict(big, att_crank="dep")),
        ("big carried position, deposit-wrapped crank every second (capital = the pool)", 2.5,
         dict(big, att_crank="dep")),
        ("big carried position, deposit-wrapped crank every second (capital = the pool)", 3.0,
         dict(big, att_crank="dep")),
    ]
    for name, mult, kw in rows:
        row([name, f"{mult:.2f}"] + [wcell(run_fast_path(r, Cfg(cand_mult=mult, secs=300, **kw)), True)
                                     for r in RULES2])
    print()
    part4c()
    part4d()
    part4e()


def flat_then_ramp(w):
    """Flat at the adoption price for w seconds, then the part-4 genuine pump 3x over 120 s."""
    return lambda s: 1.0 if s <= w else 1.0 + 2.0 * min(s - w, 120) / 120


def part4c():
    print("### 4c. The freeze held (M4X-2): the deposit freeze runs from the adoption for W seconds at a flat price,")
    print("then the part-4 genuine pump 3x/120 s (engine P; the freeze goes on).  dep = capital of the pool's size,")
    print("dep3 = three times.  Before M4X-2, L_slow integrated the frozen L_ema (lobs) while the guard was set --")
    print("+1/tau_slow per second -- and L_b = max(L_live, min(L_ema, L_mlo), L_slow) took it unguarded: the")
    print("reference blocked again after 10-30 min and the pump ended in S1.  Cells: outcome; L_slow / sqrt(X*Y) at")
    print("the end.")
    print()
    rules = ("m4r", "noslow")
    hdr(["candidate", "freeze", "W (s)"] + [RULE_NAME[r] for r in rules])
    for cs, fr, w in ((314, "dep", 600), (314, "dep", 1800), (314, "dep3", 600), (630, "dep3", 600),
                      (630, "dep3", 1800)):
        cells = [f"RA -> {cs} SOL, lambda 99.78 %", fr, w]
        for rule in rules:
            o = switch_run(Spec(inc="RA", cand_sol=cs, cand_lock=9978, post=flat_then_ramp(w), horizon=w + 400,
                                freeze=fr), rule)
            m = o["m"]
            cells.append(f"{pcell(o)}; {m.liq_slow / m.venue.sqrt_k():.3f}")
        row(cells)
    print()


# (candidate SOL, freeze, W, first second of the freeze, ramp top): part 4d's rows
CADENCE_ROWS = ((314, None, 120, 1, 3.0), (314, "dep", 120, 1, 3.0), (314, "dep3", 120, 1, 3.0),
                (314, "dep7", 120, 1, 3.0), (314, "dep", 600, 1, 3.0), (314, "dep", 120, 40, 3.0),
                (314, "dep7", 120, 40, 3.0), (630, "dep3", 120, 1, 3.0), (630, "dep7", 120, 1, 3.0),
                (630, "dep3", 600, 1, 3.0), (320, "dep7", 120, 1, 4.0))
CADENCE_COLS = (("old", "built"), ("old", "nocap"), ("old", "m4r"), ("m4f1", "nocap"), ("m4f1", "m4r"),
                ("n99", "m4r"), ("new", "m4r"))
KEEPER_NAME = {"old": "old keeper", "m4f1": "M4F-1 keeper (liquidity bursts, reverted by #99)",
               "n99": "#99 keeper", "new": "#101 keeper"}


def flat_then_ramp_to(w, top):
    """Flat at the adoption price for w seconds, then a genuine linear pump to top x over 120 s."""
    return lambda s: 1.0 if s <= w else 1.0 + (top - 1.0) * min(s - w, 120) / 120


def part4d():
    print("### 4d. The keeper's idle cadence after the gate (M4F-1): part 4c's freeze with the keeper on its coverage")
    print("duties instead of a crank every second (Spec.keeper_rule, class Coverage).  'old keeper' = as built at M4R")
    print("(ECON §5.8): per second while ema_warm > 0, near a threshold (CR < 1.3 L) or |P_spot/P_ema - 1| > 2 %, and in")
    print("a duty-2 burst, which only a PRICE deviation starts; otherwise every 30 s.  A deposit-wrapped crank does not")
    print("move the price, so 29 of every 30 seconds after the gate are the attacker's: its buckets lift L_mlo, the")
    print("guard clears (+15 s), L_ema = L_mlo = the deposit's depth, and the reference blocks the genuine pump.  'M4F-1")
    print("keeper' (#98): per second also while ema_guard = 1, and a burst on a foreign bucket whose llo / lhi")
    print("deviates from the keeper's own sqrt(X*Y) by > 2 % (every LP-wrapped crank); its liquidity bursts are")
    print("REVERTED by #99 (a cheap keeper-cost DoS, M4G-2; keeper misses ratchet the freeze, M4G-1).  '#99 keeper' =")
    print("per second while ema_guard = 1, bursts on price only.  '#101 keeper' (FITS-2 (b)) = the #99 keeper plus")
    print("per-SECOND coverage for 10 s after a foreign bucket liquidity deviation on a market with positions, extended")
    print("while deviations recur (one crank per attacker second).  These carried positions exceed the candidate's")
    print("opening caps, so under #99 no window opens (part 11); the rows force the adoption.  'M4R' caps the EMA terms of")
    print("L_b at floor(D*1e4/lambda_min) (2D here); 'before M4F-1' has no cap.  RA -> cand SOL at lambda 99.78 %, flat W")
    print("s after the adoption, then a genuine pump to the top over 120 s; the attacker deposit-wraps one crank every")
    print("second from 'from' s (depK = K x the pool's LP counter deposited; from 40 = after an honest guard clear at")
    print("+9 s).  q is 5.9 % of the 314-SOL candidate's X, 2.9 % of the 630-SOL one's; the 320-SOL 4x row takes the")
    print("LiteSVM reproducer's candidate size and ramp (m4r_it::m4f1_*, where q is 3.1 % of X).  Cells: outcome;")
    print("attacker-only seconds after the gate; seconds the keeper observed.")
    print()
    hdr(["candidate", "freeze", "W (s)", "from", "top"] +
        [f"{KEEPER_NAME[k]}, {RULE_NAME[r]}" for k, r in CADENCE_COLS])
    for cs, fr, w, frm, top in CADENCE_ROWS:
        cells = [f"RA -> {cs} SOL", fr or "none", w, f"+{frm} s", f"{top:.0f}x"]
        for kr, rule in CADENCE_COLS:
            o = switch_run(Spec(inc="RA", cand_sol=cs, cand_lock=9978, post=flat_then_ramp_to(w, top),
                                horizon=w + 400, freeze=fr, freeze_from=frm, keeper_rule=kr), rule)
            cells.append(f"{pcell(o)}; {o['att_only']} a-only s; keeper {o['k_secs']} s")
        row(cells)
    print()


def cadence_steady_run(rule, keeper_rule, sol_real, lam, fr, w, top=3.0, dur=120, tier="A", drain=False, att_from=1,
               horizon=400, fee_ppm=2500, held=None):
    """No venue switch (steady state): a Raydium market with a position at the size cap, the EMA converged; after it,
    optionally every unlocked LP unit genuinely withdrawn and held (drain); the keeper on its coverage duties
    (Coverage: 'old' / 'm4f1' / 'new'; the guard is never set here, so 'm4f1' differs only by the liquidity bursts
    and 'new' not at all); the
    attacker deposit-wraps one crank every second from att_from; flat for w s, then a genuine pump to top x over dur
    s.  held = (s0, s1, k): an LP deposits k x the pool's LP counter at second s0 and withdraws it at w + s1 (a real,
    held deposit, M4G-3; part 11d's native control).  Returns (outcome cell, q/x, attacker-only seconds)."""
    pool = ray_pool(sol_real, lam, fee_ppm=fee_ppm)
    m = RULE_CLS[rule](pool, TIERS[tier], DEFAULT)
    Q = max(1, int(m.max_open_q()))
    pos = m.open(Q, 0)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    if drain:
        lp_withdraw_all(pool)
    qx = Q / pool.X
    p0 = pool.spot_wad()
    cov = Coverage(keeper_rule)
    t0 = t
    a_only = 0
    held_units = 0
    for s in range(1, w + horizon + 1):
        t = t0 + s
        if held is not None and s == held[0]:
            held_units = pool.lp_units * held[2]
            lp_deposit(pool, held_units)
        elif held is not None and s == w + held[1] and held_units:
            lp_withdraw_raw(pool, held_units)
            held_units = 0
        if s > w:
            pool.move_to_price(int(p0 * (1 + (top - 1) * min(s - w, dur) / dur)))
        view = (pool.spot_wad(), pool.sqrt_k())
        k = cov.due(m, pos, t)
        if k:
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                lp = r["lender_sol"] - (pos.Q - r["tokens"]) * r["spot"] // WAD
                return (f"{r['kind']} +{s - w} s @{r['spot'] / p0:.3f}x, lender {sol(lp):+.3f}, borrower "
                        f"{sol(r['borrower']):.3f}", qx, a_only)
        a = fr is not None and s >= att_from
        if a:
            obs_wrapped(m, t, fr)
        elif not k:
            m.advance(t)
        cov.check_foreign(m, t, view)
        a_only += a and not k
    return "open", qx, a_only


STEADY_COLS = (("old", "nocap"), ("old", "m4r"), ("m4f1", "m4r"), ("new", "m4r"))


def part4e():
    print("### 4e. Steady-state controls (no switch; M4F-1): a position at the size cap on a Raydium pool, the EMA")
    print("converged; the keeper on its coverage duties (Coverage, as in 4d; the guard is never set without a switch,")
    print("so the #99 keeper is the old keeper here and 'old keeper, M4R' is the rule after #99; '#101 keeper, M4R' is")
    print("the rule as adopted (#101): a deposit-wrapped crank deviates the bucket's lhi, so the keeper then covers every")
    print("second); the attacker deposit-wraps one crank every second (after the drain: from +60 s); flat W s, then a genuine")
    print("pump.  At the open caps (q/x <= 1.5 %) the push delays the liquidation by 0-2 s under the old keeper even")
    print("without the cap: the exposure of 4d is a switch's (a carried position that is large on the new pool).")
    print("Cells: outcome (seconds after the pump starts, price, lender P&L at the exit price, borrower remainder);")
    print("attacker-only seconds.")
    print()
    hdr(["pool", "q / x", "ramp", "freeze"] + [f"{KEEPER_NAME[k]}, {RULE_NAME[r]}" for k, r in STEADY_COLS])
    for sol_real, lam, w in ((314, 9978, 120), (314, 9978, 600), (314, 5000, 600), (1250, 9978, 600)):
        for fr in (None, "dep", "dep3"):
            res = [cadence_steady_run(r, k, sol_real, lam, fr, w) for k, r in STEADY_COLS]
            row([f"{sol_real} SOL, lambda {lam / 100:.2f} %, Tier A, flat {w} s", f"{res[0][1] * 100:.2f} %",
                 "3x/120 s", fr or "none"] + [f"{c}; {a} a-only s" for c, _q, a in res])
    for sol_real, tier in ((314, "A"), (150, "B"), (60, "C")):
        for top, dur in ((3.0, 120), (2.5, 60)):
            for fr in (None, "dep3", "dep7"):
                res = [cadence_steady_run(r, k, sol_real, 5000, fr, 300, top, dur, tier, drain=True, att_from=60)
                       for k, r in STEADY_COLS]
                row([f"{sol_real} SOL, lambda 50 % drained, Tier {tier}, flat 300 s", f"{res[0][1] * 100:.2f} %",
                     f"{top:g}x/{dur} s", fr or "none"] + [f"{c}; {a} a-only s" for c, _q, a in res])
    print()


def part5():
    print("## 5. Withdraw contest while the guard is set: RA -> 625 SOL, lambda 50 % (D = 25 % of pool_d; q = 3 % of")
    print("the candidate's X); candidate at 1.60x the pre-halt price (the position is liquidatable on it).  The")
    print("attacker holds the candidate's unlocked LP and capital (engine P).  Cells: outcome and the buyback B paid;")
    print("the overpay is the borrower's, against the honest liquidation of the same rule.")
    print()
    hdr(["attacker", "rule", "outcome", "B (SOL)", "borrower overpay vs honest"])
    atts = [("none (honest)", {}),
            ("withdraw-wrapped liquidation only, lands first", dict(att_liq="wd", att_first=True)),
            ("deposit freeze + both samples deposit-wrapped (blocker)",
             dict(rec_move="dep", exec_move="dep", win_lp="dep", freeze="dep")),
            ("combined: every sample both ways, withdraw-wrapped liquidation first",
             dict(rec_move="wd", exec_move="dep", win_lp=("wd", "dep"), freeze=("wd", "dep"), att_liq="wd",
                  att_first=True))]
    for rule in RULES2:
        base = switch_run(Spec(inc="RA", cand_sol=625, cand_lock=5000, m=1.60, horizon=120), rule)
        for name, kw in atts:
            o = switch_run(Spec(inc="RA", cand_sol=625, cand_lock=5000, m=1.60, horizon=120, **kw), rule)
            B = o["exit"]["B"] if o["exit"] else None
            Bh = base["exit"]["B"] if base["exit"] else None
            over = f"{(B - Bh) * 100 / Bh:+.2f} %" if (B and Bh) else "-"
            row([name, RULE_NAME[rule], pcell(o), f"{sol(B):.4f}" if B else "-", over])
    print()
    part5b()


# Part 5b's pools (FITV-2's fork: a 160-SOL lambda 99.78 % CPMM at a 1 % fee, the attacker's own deposit of 1x / 3x /
# 3.95x the LP counter held unlocked): (real SOL, lambda bps, fee ppm, lambda_min bps, hold levels)
DRAG_POOLS = ((320, 4989, 10_000, 5000, 2.05), (640, 2494, 10_000, 5000, 2.00), (792, 2016, 10_000, 2000, 1.90),
              (320, 4989, 2500, 5000, 2.02), (160, 9978, 10_000, 5000, 2.05))
# Part 5b's rule sets: (the Market rule, the keeper's Coverage rule, the attacker's first second).  "a101" = the rules
# as adopted (#101: the spot leg at max(L_live, L_ema'), the #101 keeper; the attacker starts in the second after the
# keeper's first observation, so its first dragged second is attacker-only); "a100" = #100's (the spot leg at L_live,
# the #99 keeper -- the "old" keeper without a switch); "lows" = the withdrawn lows bound (#100's first fix); the
# ablations "spot" (#101's spot leg, the #99 keeper) and "kpr" (the #101 keeper, the spot leg at L_live).
DRAG_VARIANTS = {"a101": ("m4r", "new", 2), "a100": ("s100", "old", 1), "lows": ("lows", "old", 1),
                 "spot": ("m4r", "old", 1), "kpr": ("s100", "new", 2), "mhi": ("mhi", "new", 2),
                 "mhislow": ("mhislow", "new", 2)}
DRAG_NAME = {"a101": "#101 (as adopted: spot leg at max(L_live, L_ema'), #101 keeper)",
             "a100": "#100 (spot leg at L_live, #99 keeper)", "lows": "the withdrawn lows bound",
             "spot": "#101's spot leg alone (#99 keeper)", "kpr": "the #101 keeper alone (spot leg at L_live)",
             "mhi": "#101 with the spot leg floored at L_mhi (FITX-1 candidate; analysis, not adopted)",
             "mhislow": "#101 with the spot leg floored at min(L_mhi, L_slow) (FITX-1 candidate; analysis, not adopted)"}


def drag_run(var, sol_real, lam, fee_ppm, hold, attack, lam_min=5000, w=30, tier="C", dur=60, horizon=300, size=1.0,
             misses=(), every=1, held=0, gone=False):
    """FITV-2 / FITS-2 in steady state (no switch): a Raydium market at Tier C whose unlocked LP (1 - lambda of the pool)
    the attacker holds, lambda_min = lam_min; a position at size x the open cap, the EMA converged (the keeper cranked
    every second); then the keeper on its duties (Coverage: every 30 s in quiet periods, per second near a threshold or
    at |P_spot/P_ema - 1| > 2 %; the #101 keeper also per second for 10 s after a foreign bucket liquidity deviation,
    extended while they recur).  From the variant's first second, in every second before the keeper's tick, the
    attacker (attack=True) sends [withdraw all, crank_price, deposit] -- the withdraw drag: an attacker-only second's
    bucket low and high are the drained depth D, so such seconds pull L_ema toward D (1/6 of the gap a second at
    dt = 1), and a keeper observation in the same second cannot lift it (the clamped observation is L_ema itself) --
    and [withdraw all, liquidate, deposit], which lands only if the program accepts it.  `misses`: seconds (relative)
    in which the keeper's observation does not land (never two in a row: no degraded mode); `every`: the attacker
    wraps only every `every`-th second (a pause-and-resume drag).  `held` (FITX-1, the held drain; with attack=True):
    the attacker's unlocked LP is GENUINELY out of the pool for the first `held` seconds (withdrawn at second 1, the
    attacker cranking every second at the drained depth, which the keeper reads too -- nothing is foreign), and
    re-deposited at held + 1, from which second the wraps run.  `gone` (with attack=False): the unlocked LP leaves at
    second 1 and stays out, nobody else acts -- the genuine withdrawal the held drain stands in for.  Flat for w s (the
    drag before the pump), then a genuine pump to hold x over dur s, held for the horizon.  Returns (the exit result
    with who / s / p0, or None; q/x; (L_ema, L_slow, L_mlo) / sqrt(X*Y) at the pump's first second -- the pool's
    sqrt(X*Y) with every LP unit in; (keeper observations, attacker transactions) over the run)."""
    rule, keeper, att_from = DRAG_VARIANTS[var]
    P = replace(DEFAULT, pool_lock_min_bps=lam_min)
    pool = ray_pool(sol_real, lam, fee_ppm=fee_ppm)
    m = RULE_CLS[rule](pool, TIERS[tier], P)
    Q = max(1, m.max_open_q() * int(round(size * 10_000)) // 10_000)
    pos = m.open(Q, 0)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    qx = Q / pool.X
    p0, H = pool.spot_wad(), pool.sqrt_k()
    cov = Coverage(keeper)
    t0 = t
    at_pump = None
    n_k = n_a = 0
    held = held if attack else 0
    if held:
        att_from = held + 1
    units = 0
    for s in range(1, w + dur + horizon + 1):
        t = t0 + s
        if (held or (gone and not attack)) and s == 1:
            units = lp_withdraw_all(pool)
        if held and s == held + 1:
            lp_deposit(pool, units)
        if s > w:
            pool.move_to_price(int(p0 * (1 + (hold - 1) * min(s - w, dur) / dur)))
        if s == w + 1:
            at_pump = (m.liq / H, m.liq_slow / H, m.ring_medians()[2] / H)
        view = (pool.spot_wad(), pool.sqrt_k())
        if held and s <= held:
            # the held drain: an honest crank at the genuinely drained depth (and a liquidation attempt there)
            n_a += 1
            obs_wrapped(m, t, None)
            r = try_liq(m, pos, t)
            if r is not None:
                return dict(r, who="attacker", s=s - w, p0=p0), qx, at_pump, (n_k, n_a)
        a = attack and s >= att_from and (s - att_from) % every == 0
        if a:
            n_a += 1
            obs_wrapped(m, t, "wd")
            r = try_liq(m, pos, t, "wd")
            if r is not None:
                return dict(r, who="attacker", s=s - w, p0=p0), qx, at_pump, (n_k, n_a)
        if s not in misses and cov.due(m, pos, t):
            n_k += 1
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                return dict(r, who="keeper", s=s - w, p0=p0), qx, at_pump, (n_k, n_a)
        elif not a and not (held and s <= held):
            m.advance(t)
        cov.check_foreign(m, t, view)
    return None, qx, at_pump, (n_k, n_a)


def drag_cell(r):
    if r is None:
        return "open"
    return f"{r['kind']} +{r['s']} s by the {r['who']} @{r['spot'] / r['p0']:.3f}x"


def drag_threshold(pool, lo=1.3, hi=3.5, tol=0.005, size=1.0):
    """The lowest hold level (x, to tol) at which the honest keeper liquidates (drag_run without an attacker)."""
    sol_real, lam, fee, lam_min, _ = pool
    while hi - lo > tol:
        mid = (lo + hi) / 2
        r = drag_run("a101", sol_real, lam, fee, mid, False, lam_min, 30, size=size)[0]
        if r is not None and r["kind"] == "liquidation":
            hi = mid
        else:
            lo = mid
    return hi


def drag_hits(var, pool, w, top, size=1.0, misses=(), start=1.80, every=1, held=0, with_s=False):
    """The hold levels start, start + 0.01, ... below `top` (the honest threshold) at which the attacker's wrapped
    liquidation lands after a w-s drag (`held`: after a held drain of that many seconds, drag_run); with_s: pairs
    (level, the landing second after the pump starts)."""
    sol_real, lam, fee, lam_min, _ = pool
    hits = []
    for i in range(0, 200):
        h = round(start + 0.01 * i, 2)
        if h >= top:
            break
        r = drag_run(var, sol_real, lam, fee, h, True, lam_min, w, size=size, misses=misses, every=every,
                     held=held)[0]
        if r is not None and r["who"] == "attacker":
            hits.append((h, r["s"]) if with_s else h)
    return hits


def hits_cell(hits):
    """A hold-level list as a range string ("none" if empty; a gap is listed), with the count in brackets."""
    if not hits:
        return "none"
    runs, a = [], hits[0]
    for x, y in zip(hits, hits[1:] + [None]):
        if y is None or round(y - x, 2) > 0.01:
            runs.append(f"{a:.2f}x" if a == x else f"{a:.2f}-{x:.2f}x")
            a = y
    return ", ".join(runs) + f" ({len(hits)})"


def drag_forced(var, pool, w, top, size=1.0, misses=(), start=1.80, every=1, held=0):
    """drag_hits as a range string."""
    return hits_cell(drag_hits(var, pool, w, top, size, misses, start, every, held))


def drag_min_w(var, pool, hold, lo=30, hi=8 * 3600, tol=60, size=1.0):
    """The shortest drag (s before the pump, to tol) after which the attacker's wrapped liquidation lands under `var`."""
    sol_real, lam, fee, lam_min, _ = pool
    if drag_run(var, sol_real, lam, fee, hold, True, lam_min, hi, size=size)[0] is None:
        return None
    while hi - lo > tol:
        mid = (lo + hi) // 2
        if drag_run(var, sol_real, lam, fee, hold, True, lam_min, mid, size=size)[0] is not None:
            hi = mid
        else:
            lo = mid
    return hi


def min_w_cell(mw):
    return "not within 8 h" if mw is None else f"{mw} s ({mw / 3600:.2f} h)"


def pool_name(pool):
    sol_real, lam, fee, lam_min, _ = pool
    return f"{sol_real} SOL, lambda {lam / 100:.2f} %, {fee / 10_000:g} %, lambda_min {lam_min / 100:g} %"


def spread_misses(w, k):
    """k keeper misses spread evenly over the first w seconds of the drag (after its attacker-only first second)."""
    if k == 0:
        return ()
    return tuple(3 + (w - 6) * i // max(k - 1, 1) for i in range(k)) if k > 1 else (w // 2,)


def part5b():
    print("## 5b. The steady-state withdraw drag under the keeper's real cadence (FITV-2, FITS-2; V2-14).  No switch: a")
    print("Tier C Raydium market whose unlocked LP the attacker holds (FITV-2's fork: a 160-SOL lambda 99.78 % 1 % CPMM")
    print("plus the attacker's own deposit of 1x / 3x / 3.95x the LP counter -> lambda 49.9 / 24.9 / 20.2 %), a position")
    print("at the open cap (in (iii) a share of it), the EMA converged.  The keeper follows its duties (every 30 s in")
    print("quiet periods; per second near a threshold or at |P_spot/P_ema - 1| > 2 %).  Every second, before the keeper,")
    print("the attacker sends [withdraw all, crank_price, deposit] -- an attacker-only second's bucket is the drained")
    print("depth D, so such seconds drag L_ema toward D (1/6 of the gap each), and the keeper's own observation in a")
    print("dragged second cannot lift it (the clamped observation is L_ema itself) -- and [withdraw all, liquidate,")
    print("deposit].  Flat for W s (the drag), then a genuine pump to the hold level over 60 s, held 300 s.  The rule sets:")
    print("'#100' = the spot leg at the drained L_live and the #99 keeper (29 of every 30 quiet seconds are the")
    print("attacker's, so L_ema reaches D within about 8 s); L_b = max(L_live, min(max(L_ema', L_slow), cap)), whose")
    print("L_slow term (V2-14) credits the depth that was there until it has followed the dragged L_ema (tau_slow = 1 h).")
    print("'#101' (as adopted, FITS-2) = the spot leg at max(L_live, L_ema') (a withdrawal in the same transaction")
    print("cannot make it true) and the #101 keeper, which covers every second for 10 s after a foreign bucket")
    print("liquidity deviation on a market with positions, extended while they recur: the attacker starts in the")
    print("second after a keeper observation, so its first dragged second is attacker-only (one step, L_ema x ~11/12);")
    print("every later second shares its bucket with the keeper's honest observation, which holds L_ema where it is.")
    print("'the withdrawn lows bound' = #100's first fix for FIT-2 (#100's exits), which bounded the EMA terms by")
    print("floor(L_mlo*1.01) = D (FITV-2).  Cells: outcome (seconds after the pump starts, by whom, price); for #101 the")
    print("keeper observations / attacker transactions over the run; L_ema / L_slow / L_mlo as a share of sqrt(X*Y)")
    print("at the pump.")
    print()
    print("(i) The drag held W s before a pump to the first table's hold level.")
    print()
    hdr(["pool", "q / x", "hold", "W (drag, s)", "honest (no attacker)", "attacker, #101 (as adopted)",
         "attacker, #100", "attacker, the withdrawn lows bound", "at the pump, #101 (attacker)",
         "at the pump, #100 (attacker)"])
    for pool in DRAG_POOLS[:3]:
        sol_real, lam, fee, lam_min, hold = pool
        for w in (30, 600, 3600, 3 * 3600):
            h, qx, _, _ = drag_run("a101", sol_real, lam, fee, hold, False, lam_min, w)
            a1, _, ap1, (nk, na) = drag_run("a101", sol_real, lam, fee, hold, True, lam_min, w)
            a0, _, ap0, _ = drag_run("a100", sol_real, lam, fee, hold, True, lam_min, w)
            lw = drag_run("lows", sol_real, lam, fee, hold, True, lam_min, w)[0]
            row([pool_name(pool), f"{qx * 100:.2f} %", f"{hold:.2f}x", w, drag_cell(h),
                 f"{drag_cell(a1)}; keeper {nk} / attacker {na}", drag_cell(a0), drag_cell(lw),
                 " / ".join(f"{x:.3f}" for x in ap1), " / ".join(f"{x:.3f}" for x in ap0)])
    print()
    print("(ii) Forcing windows: the hold levels (1.80x, 1.81x, ...) below the honest keeper's threshold -- the lowest")
    print("hold level (to 0.005x) it liquidates at without an attacker -- at which the attacker's wrapped liquidation")
    print("lands after a W-s drag (their count in brackets).  Above a band the drained buyback no longer fits the vault")
    print("(V - K_s - ert) and nothing is forced.  The last columns are the shortest drag (to 60 s) after which the")
    print("attacker forces the first table's hold level; the attacker pays one transaction a second for it and must")
    print("hold the pool's unlocked LP (under #101 the keeper pays one crank a second as well).")
    print()
    hdr(["pool", "q / x", "honest threshold", "forced, #101, W 30 s", "forced, #101, W 1 h", "forced, #101, W 3 h",
         "forced, #100, W 30 s", "forced, #100, W 1 h", "forced, #100, W 3 h", "forced, the withdrawn lows bound, W 30 s",
         "shortest #101 drag that forces the hold level", "shortest #100 drag that forces the hold level"])
    for pool in DRAG_POOLS:
        sol_real, lam, fee, lam_min, hold = pool
        qx = drag_run("a101", sol_real, lam, fee, 1.0, False, lam_min, 1, horizon=1)[1]
        hon = drag_threshold(pool)
        row([pool_name(pool), f"{qx * 100:.2f} %", f"{hon:.3f}x"] +
            [drag_forced("a101", pool, w, hon) for w in (30, 3600, 3 * 3600)] +
            [drag_forced("a100", pool, w, hon) for w in (30, 3600, 3 * 3600)] + [drag_forced("lows", pool, 30, hon)] +
            [f"{hold:.2f}x: " + min_w_cell(drag_min_w(v, pool, hold)) for v in ("a101", "a100")])
    print()
    print("(iii) FITS-2 (a): smaller positions and the fork's drag lengths.  The 320-SOL lambda 49.89 % 1 % pool; the")
    print("position at a share of the open cap.  Below about half the cap delta (2 %) covers the drained cost")
    print("cost(D)/cost(H) - 1, so under #100 the wrapped liquidation fits the bound with L_slow still at the pool's")
    print("depth: a 30-s drag forces a band below the honest threshold (the fork: FITV_QPM = 500, forced at 2.025x); a")
    print("longer drag widens it for the larger positions (the fork's cap-sized regression position, 1.9 % of X,")
    print("after 25 min).  Columns: forced hold levels (from 1.80x) after a W-s drag; the ablations take one of #101's")
    print("two changes at a time (W 1 h).")
    print()
    base = DRAG_POOLS[0]
    hdr(["position (share of the open cap)", "q / x", "honest threshold", "forced, #100, W 30 s",
         "forced, #100, W 25 min", "forced, #100, W 1 h", "forced, #101, W 30 s", "forced, #101, W 25 min",
         "forced, #101, W 1 h", "forced, #101's spot leg alone, W 1 h", "forced, the #101 keeper alone, W 1 h"])
    for size in (1.0, 0.79, 0.5, 0.3, 0.15):
        qx = drag_run("a101", *base[:3], 1.0, False, base[3], 1, horizon=1, size=size)[1]
        hon = drag_threshold(base, size=size)
        row([f"{size:g}", f"{qx * 100:.2f} %", f"{hon:.3f}x"] +
            [drag_forced("a100", base, w, hon, size) for w in (30, 1500, 3600)] +
            [drag_forced("a101", base, w, hon, size) for w in (30, 1500, 3600)] +
            [drag_forced(v, base, 3600, hon, size) for v in ("spot", "kpr")])
    print()
    print("(iv) What is left under #101: the keeper's missed seconds (the M4G-1 class, downward), and pause-and-")
    print("resume drags.  While the attacker wraps every second, each second the keeper's observation does not land is")
    print("another attacker-only step (L_ema - D shrinks by 1/6), and a covered second's bucket holds L_ema, so the")
    print("steps add up; they need not be consecutive (no degraded mode).  An attacker that pauses until the coverage")
    print("window lapses (10 s) gets one attacker-only second per resumption, but the covered seconds of the pause")
    print("carry honest observations only and give back 1/6 of the gap each, so the drag does not accumulate.  The")
    print("320-SOL lambda 49.89 % 1 % pool, the position at the open cap and at half of it, the drag held 1 h (k")
    print("isolated misses spread over it, or the attacker wrapping every n-th second).  Cells: forced hold levels")
    print("(from 1.80x); L_ema / L_slow / L_mlo at the pump (the hold level of (i)); keeper observations / attacker")
    print("transactions over that run.")
    print()
    hdr(["position", "q / x", "honest threshold", "attacker", "forced, #101", "at the pump", "keeper / attacker"])
    for size in (1.0, 0.5):
        qx = drag_run("a101", *base[:3], 1.0, False, base[3], 1, horizon=1, size=size)[1]
        hon = drag_threshold(base, size=size)
        for k, n in ((0, 1), (1, 1), (3, 1), (5, 1), (10, 1), (20, 1), (0, 2), (0, 11)):
            ms = spread_misses(3600, k)
            _, _, ap, (nk, na) = drag_run("a101", *base[:3], base[4], True, base[3], 3600, size=size, misses=ms,
                                          every=n)
            who = (f"every second, {k} keeper miss{'es' if k != 1 else ''}" if n == 1 else
                   f"one wrap every {n} s, no keeper miss")
            row([f"{size:g} x the cap", f"{qx * 100:.2f} %", f"{hon:.3f}x", who,
                 drag_forced("a101", base, 3600, hon, size, ms, every=n), " / ".join(f"{x:.3f}" for x in ap),
                 f"{nk} / {na}"])
    print()
    part5b_spike()
    part5b_liveness()
    part5b_held()


def spike_run(var, pool, path, horizon, attack, tier="C", size=1.0):
    """The spot leg's own case (FITS-2 (a)): drag_run's market with the keeper on its duties; the price follows
    path(s) (a multiple of the starting price); with attack=True the unlocked-LP holder sends only
    [withdraw all, liquidate, deposit] every second, before the keeper (no drag).  Returns the exit result with who /
    s / p0, or None."""
    rule, keeper, _ = DRAG_VARIANTS[var]
    sol_real, lam, fee, lam_min, _ = pool
    P = replace(DEFAULT, pool_lock_min_bps=lam_min)
    v = ray_pool(sol_real, lam, fee_ppm=fee)
    m = RULE_CLS[rule](v, TIERS[tier], P)
    Q = max(1, m.max_open_q() * int(round(size * 10_000)) // 10_000)
    pos = m.open(Q, 0)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    p0 = v.spot_wad()
    cov = Coverage(keeper)
    t0 = t
    for s in range(1, horizon + 1):
        t = t0 + s
        v.move_to_price(int(p0 * path(s)))
        view = (v.spot_wad(), v.sqrt_k())
        if attack:
            r = try_liq(m, pos, t, "wd")
            if r is not None:
                return dict(r, who="attacker", s=s, p0=p0)
        if cov.due(m, pos, t):
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                return dict(r, who="keeper", s=s, p0=p0)
        elif not attack:
            m.advance(t)
        cov.check_foreign(m, t, view)
    return None


def spike_path(k, r, up=10, hold=5, down=2):
    """A genuine spike: a ramp to k x over `up` s, held `hold` s, then back to r x over `down` s, flat after."""
    return lambda s: (1 + (k - 1) * min(s, up) / up) if s <= up + hold else (k + (r - k) * min(s - up - hold, down) / down)


SPIKES = ((2.2, 20, 5), (2.3, 10, 5), (2.4, 10, 5), (2.6, 5, 3))   # part 5b (v): (k, ramp s, hold s)
RETREATS = (1.98, 2.00, 2.01, 2.02, 2.03, 2.04, 2.05, 2.06, 2.07)


def levels_cell(levels):
    if not levels:
        return "none"
    return ", ".join(f"{x:.2f}x" for x in levels) + f" ({len(levels)})"


def part5b_spike():
    print("(v) The spot leg's own case (FITS-2 (a)): no drag, a genuine spike and retreat.  The price ramps to k x over")
    print("the ramp, holds, and falls back over 2 s to r x below the honest threshold: P_ema lags the spike, so for a")
    print("few seconds after the retreat the EMA leg is true while the robust spot at the pool's depth is not -- the")
    print("spot leg is the veto.  The unlocked-LP holder sends only [withdraw all, liquidate, deposit] every second,")
    print("before the keeper.  Under #100 the spot leg reads the drained L_live, the withdrawal makes it true, and the")
    print("liquidation of a position healthy at the pool's depth lands; under #101 it reads max(L_live, L_ema') and the")
    print("veto holds.  The #101 keeper alone (the spot leg at L_live) does not help: no attacker-only second is")
    print("involved.  Cells: the retreat levels r (of 1.98x, 2.00x, 2.01x, ..., 2.07x below the honest threshold) at")
    print("which the attacker's wrapped liquidation lands while the honest keeper leaves the position open.")
    print()
    hdr(["pool", "honest threshold", "spike (k, ramp, hold)", "retreats the honest keeper leaves open",
         "forced, #100", "forced, the #101 keeper alone", "forced, #101 (as adopted)"])
    for pool in (DRAG_POOLS[0], DRAG_POOLS[1], DRAG_POOLS[3]):
        hon = drag_threshold(pool)
        for k, up, hold in SPIKES:
            open_, forced = [], {"a100": [], "kpr": [], "a101": []}
            for r in RETREATS:
                if r >= hon:
                    continue
                path = spike_path(k, r, up, hold)
                if spike_run("a101", pool, path, 200, False) is not None:
                    continue
                open_.append(r)
                for v in forced:
                    x = spike_run(v, pool, path, 200, True)
                    if x is not None and x["who"] == "attacker":
                        forced[v].append(r)
            row([pool_name(pool), f"{hon:.3f}x", f"{k:g}x, {up} s, {hold} s", levels_cell(open_)] +
                [levels_cell(forced[v]) for v in ("a100", "kpr", "a101")])
    print()


def wd_run(var, pool, path, wd_at, horizon, tier="C", size=1.0, noise=0.0, seed=0):
    """A GENUINE withdrawal (liveness, FITS-2 (c)): the market of drag_run with the keeper on its duties and no
    attacker; at second wd_at every unlocked LP unit is withdrawn and stays out; the price follows path(s) (a multiple
    of the starting price), with per-second noise +-noise.  Returns (the exit result with s, or None; q/x)."""
    rule, keeper, _ = DRAG_VARIANTS[var]
    sol_real, lam, fee, lam_min, _ = pool
    P = replace(DEFAULT, pool_lock_min_bps=lam_min)
    v = ray_pool(sol_real, lam, fee_ppm=fee)
    m = RULE_CLS[rule](v, TIERS[tier], P)
    Q = max(1, m.max_open_q() * int(round(size * 10_000)) // 10_000)
    pos = m.open(Q, 0)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    qx = Q / v.X
    p0 = v.spot_wad()
    rng = random.Random(seed)
    cov = Coverage(keeper)
    t0 = t
    for s in range(1, horizon + 1):
        t = t0 + s
        if s == wd_at:
            lp_withdraw_all(v)
        f = path(s) * (1 + rng.uniform(-noise, noise)) if noise else path(s)
        v.move_to_price(int(p0 * f))
        view = (v.spot_wad(), v.sqrt_k())
        if cov.due(m, pos, t):
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                return dict(r, s=s, p0=p0), qx
        else:
            m.advance(t)
        cov.check_foreign(m, t, view)
    return None, qx


def wd_threshold(pool, drained, size=1.0, lo=1.3, hi=3.5, tol=0.002):
    """The lowest flat hold level (x, to tol; reached by a 60-s ramp, held 300 s) at which the honest keeper liquidates,
    on the pool as it is (drained=False) or with every unlocked LP withdrawn before the ramp (drained=True); #100's
    rules (without a withdrawal both rule sets are the same)."""
    while hi - lo > tol:
        mid = (lo + hi) / 2
        r = wd_run("a100", pool, lambda s, h=mid: 1 + (h - 1) * min(s, 60) / 60, 1 if drained else 10 ** 9, 360,
                   size=size)[0]
        if r is not None and r["kind"] == "liquidation":
            hi = mid
        else:
            lo = mid
    return hi


def wd_cell(r, wd_at):
    if r is None:
        return "open"
    return f"{r['kind']} +{r['s'] - wd_at} s @{r['spot'] / r['p0']:.3f}x"


def ramp_hold(h, up=60):
    """A 60-s ramp to h x, then flat."""
    return lambda s: 1 + (h - 1) * min(s, up) / up


def ramp_dip(h, dip, at):
    """A 60-s ramp to h*(1 + dip), flat, and at second `at` (the withdrawal's) back to h x, flat after."""
    top = h * (1 + dip)
    return lambda s: (1 + (top - 1) * min(s, 60) / 60) if s < at else h


# Part 5b (vi): (pool, share of the open cap) with a band between the drained and the full-depth thresholds
LIVE_CASES = ((DRAG_POOLS[3], 1.0), (DRAG_POOLS[0], 0.3), (DRAG_POOLS[1], 0.3))


def part5b_liveness():
    print("(vi) Liveness: a GENUINE withdrawal (FITS-2 (c)).  No attacker; every unlocked LP unit leaves the pool at")
    print("second 90 and stays out, so a position that is healthy at the pool's full depth becomes liquidatable at")
    print("its new depth.  Under #101 the spot leg reads max(L_live, L_ema'), and L_ema follows the drained depth at")
    print("1/6 of the gap per integrated second (tau = 5 s; the keeper observes every second near a threshold or")
    print("while |P_spot/P_ema - 1| > 2 %, else every 30 s, dt <= 2 s).  The EMA leg (the ring's reprojections at")
    print("L_ema) already waited for that decay under #100: in a flat or rising market it binds, and the new spot leg")
    print("costs nothing; it delays an exit only while the robust spot is below P_ema (a dip, noise), until L_ema has")
    print("decayed.  Positions where a band exists -- the drained buyback within delta of the reference, which L_b's")
    print("L_slow term keeps at the pre-withdrawal depth for about tau_slow: the 25-bps pool at the open cap and the")
    print("1 % pools at 0.3 of it.  Paths (x the starting price; h the middle of the band): 'flat h' = a 60-s ramp to")
    print("h, flat; 'dip d' = a 60-s ramp to h*(1 + d), flat, and the price back to h with the withdrawal (the")
    print("robust spot below P_ema); 'noise' = 'flat h' with +-1 % per-second noise (seeds 1-3); 'pump' = 1.2 h over")
    print("60 s, the withdrawal at second 30.  Cells: outcome, seconds after the withdrawal, price; the delay of #101")
    print("against #100.")
    print()
    hdr(["pool", "position", "q / x", "threshold at full depth / drained", "path", "#100 (spot leg at L_live)",
         "#101 (as adopted)", "delay"])
    for pool, size in LIVE_CASES:
        full = wd_threshold(pool, False, size)
        dr = wd_threshold(pool, True, size)
        h = round((full + dr) / 2, 4)
        qx = wd_run("a101", pool, lambda s: 1.0, 10 ** 9, 1, size=size)[1]
        cases = [(f"flat {h:.4f}x", ramp_hold(h), 90, 0.0, 0)]
        cases += [(f"dip {d * 100:g} %", ramp_dip(h, d, 90), 90, 0.0, 0) for d in (0.0025, 0.005)]
        cases += [(f"flat {h:.4f}x, noise +-1 %, seed {sd}", ramp_hold(h), 90, 0.01, sd) for sd in (1, 2, 3)]
        cases += [(f"pump to {1.2 * h:.3f}x over 60 s", ramp_hold(1.2 * h), 30, 0.0, 0)]
        for name, path, wd_at, noise, sd in cases:
            r0 = wd_run("a100", pool, path, wd_at, 600, size=size, noise=noise, seed=sd)[0]
            r1 = wd_run("a101", pool, path, wd_at, 600, size=size, noise=noise, seed=sd)[0]
            d = "-" if (r0 is None or r1 is None) else f"{r1['s'] - r0['s']:+d} s"
            row([pool_name(pool), f"{size:g} x the cap", f"{qx * 100:.2f} %", f"{full:.4f}x / {dr:.4f}x", name,
                 wd_cell(r0, wd_at), wd_cell(r1, wd_at), d])
    print()
    print("At the open cap on the 1 % pools there is no such band: the drained buyback exceeds B_ref*(1 + delta) while")
    print("L_slow still credits the pre-withdrawal depth (V2-14), so after a genuine withdrawal the position waits for")
    print("L_slow under both rule sets -- in a pump that follows at once it can gap into S1 (pre-existing, unchanged")
    print("by #101):")
    print()
    hdr(["pool", "position", "path", "#100 (spot leg at L_live)", "#101 (as adopted)", "no withdrawal (#101)"])
    for pool in (DRAG_POOLS[0], DRAG_POOLS[1]):
        for top, dur in ((3.0, 120), (2.5, 300)):
            path = lambda s, top=top, dur=dur: 1 + (top - 1) * min(s, dur) / dur
            r0 = wd_run("a100", pool, path, 1, 900)[0]
            r1 = wd_run("a101", pool, path, 1, 900)[0]
            rh = wd_run("a101", pool, path, 10 ** 9, 900)[0]
            row([pool_name(pool), "the open cap", f"withdrawal at +1 s, pump {top:g}x/{dur} s", wd_cell(r0, 1),
                 wd_cell(r1, 1), wd_cell(rh, 1)])
    print()


# Part 5b (vii): (pool, share of the open cap) -- the fork's three FITX-1 cases (half the cap and 1.9 % of X on the
# 1 % pool, the 25-bps pool at the open cap) among the drag pools' positions (lambda 99.78 % has nothing to withdraw)
HELD_CASES = ((DRAG_POOLS[0], 1.0), (DRAG_POOLS[0], 0.79), (DRAG_POOLS[0], 0.5), (DRAG_POOLS[0], 0.3),
              (DRAG_POOLS[1], 1.0), (DRAG_POOLS[1], 0.5), (DRAG_POOLS[2], 1.0), (DRAG_POOLS[3], 1.0))
HELD_S = (3, 5, 10, 20)
HELD_WS = (60, 1500, 3600)


def gone_check(pool, w, hits, size=1.0):
    """For each forced (level, second) of the held drain: whether the honest keeper also liquidates at that level when
    the same unlocked LP leaves at second 1 and stays out (drag_run gone=True), no later.  Returns (ok, n, worst
    second difference forced - gone)."""
    sol_real, lam, fee, lam_min, _ = pool
    ok, worst = 0, None
    for h, s in hits:
        g = drag_run("a101", sol_real, lam, fee, h, False, lam_min, w, size=size, gone=True)[0]
        if g is not None and g["kind"] == "liquidation" and g["s"] <= s:
            ok += 1
        if g is not None:
            worst = s - g["s"] if worst is None else min(worst, s - g["s"])
    return ok, len(hits), worst


def dep_freeze_run(var, pool, hold, fr, w=60, size=1.0, dur=60, horizon=300):
    """The candidates' cost (part 5b (vii)): drag_run's market (steady state, Tier C, the keeper on its duties) under a
    capital DEPOSIT freeze instead -- an LP with capital `fr` ('dep' = as much again as the LP counter, 'depk' = k x)
    deposit-wraps one crank every second from second 2 (M4F-1's push; it pays no fee), flat w s, then a genuine pump to
    hold x over dur s, held for the horizon; nobody else acts.  Returns the outcome cell (seconds after the pump
    starts)."""
    rule, keeper, _ = DRAG_VARIANTS[var]
    sol_real, lam, fee, lam_min, _ = pool
    P = replace(DEFAULT, pool_lock_min_bps=lam_min)
    v = ray_pool(sol_real, lam, fee_ppm=fee)
    m = RULE_CLS[rule](v, TIERS["C"], P)
    Q = max(1, m.max_open_q() * int(round(size * 10_000)) // 10_000)
    pos = m.open(Q, 0)
    t = 0
    while t < TE:
        t += 1
        m.step_to(t)
    p0 = v.spot_wad()
    cov = Coverage(keeper)
    t0 = t
    for s in range(1, w + dur + horizon + 1):
        t = t0 + s
        if s > w:
            v.move_to_price(int(p0 * (1 + (hold - 1) * min(s - w, dur) / dur)))
        view = (v.spot_wad(), v.sqrt_k())
        a = fr is not None and s >= 2
        if a:
            obs_wrapped(m, t, fr)
        if cov.due(m, pos, t):
            m.step_to(t)
            cov.observed(t)
            r = m.try_exit(pos, t)
            if r is not None:
                return f"{r['kind']} +{s - w} s"
        elif not a:
            m.advance(t)
        cov.check_foreign(m, t, view)
    return f"open ({horizon} s)"


def part5b_held():
    print("(vii) What is left under #101: a GENUINELY reached low, frozen (FITX-1, the verification of #101).  The")
    print("attacker withdraws its unlocked LP for real at second 1 and keeps it out for `held` s, cranking every second")
    print("at the drained depth -- the keeper reads the same depth, so nothing is foreign and no coverage starts, and")
    print("L_ema genuinely falls toward D (at lambda 49.9 %: 0.741 / 0.667 / 0.566 / 0.510 of the full depth after")
    print("3 / 5 / 10 / 20 s).  It then re-deposits and withdraw-wraps a crank every second: its first wrap starts the")
    print("keeper's per-second coverage, and every later bucket holds the drained and the keeper's honest observation,")
    print("[D, H], which leaves L_ema where it is (the clamped observation is L_ema itself) -- the freeze, at one keeper")
    print("crank per attacker transaction.  L_slow follows the drained lows (its input is min(lobs, L_mlo), and every")
    print("bucket's low is D; tau_slow = 1 h), as after a withdrawal for good.")
    print("After W s flat, the genuine pump to the hold level, with [withdraw all, liquidate, deposit] every second: the")
    print("spot leg reads max(L_live, L_ema') = the frozen low, the EMA leg the same low, and where delta covers the")
    print("drained cost (or L_slow has followed) the exit bound passes.  Cells: forced hold levels (from 1.80x, below the")
    print("honest threshold with the LP in the pool).  'The LP out for good': the same LP withdraws at second 1 and")
    print("stays out, nobody else acts -- the genuine withdrawal the protocol liquidates on (FITS-2 (c)); for every")
    print("forced level of the row, whether the honest keeper liquidates it then too, no later (count, and the smallest")
    print("lag of the forced liquidation behind it, in seconds).")
    print()
    hdr(["pool", "position", "q / x", "honest threshold (LP in)", "forced, #101, W 60 s, held " +
         " / ".join(str(k) for k in HELD_S) + " s", "forced, #101, held 20 s, W 25 min", "forced, #101, held 20 s, W 1 h",
         "L_ema / L_slow at the pump (held 20 s; W 60 s / 25 min / 1 h)",
         "each forced level: the LP out for good liquidates it too, no later"])
    hons = {}
    for pool, size in HELD_CASES:
        sol_real, lam, fee, lam_min, _ = pool
        qx = drag_run("a101", sol_real, lam, fee, 1.0, False, lam_min, 1, horizon=1, size=size)[1]
        hon = hons[(pool, size)] = drag_threshold(pool, size=size)
        cells, ok, n, worst = [], 0, 0, None
        for held in HELD_S:
            hits = drag_hits("a101", pool, 60, hon, size, held=held, with_s=True)
            cells.append(hits_cell([h for h, _ in hits]))
            o, k, wv = gone_check(pool, 60, hits, size)
            ok, n = ok + o, n + k
            worst = wv if worst is None else (worst if wv is None else min(worst, wv))
        long_cells, aps = [], []
        for w in HELD_WS[1:]:
            hits = drag_hits("a101", pool, w, hon, size, held=20, with_s=True)
            long_cells.append(hits_cell([h for h, _ in hits]))
            o, k, wv = gone_check(pool, w, hits, size)
            ok, n = ok + o, n + k
            worst = wv if worst is None else (worst if wv is None else min(worst, wv))
        for w in HELD_WS:
            ap = drag_run("a101", sol_real, lam, fee, pool[4], True, lam_min, w, size=size, held=20)[2]
            aps.append(f"{ap[0]:.3f} / {ap[1]:.3f}")
        row([pool_name(pool), f"{size:g} x the cap", f"{qx * 100:.2f} %", f"{hon:.3f}x", "; ".join(cells)] +
            long_cells + ["; ".join(aps), f"{ok} of {n}" + (f" (lag >= {worst:+d} s)" if worst is not None else "")])
    print()
    print("The held drain needs a few seconds of the attacker's capital genuinely out of the pool (3 s force a level at half")
    print("the cap); after it, the freeze costs one transaction a second, as the drag did.  Everything it forces is what the")
    print("same LP forces by withdrawing for good (the rows' last column): the frozen L_ema is at or above the depth a")
    print("withdrawal for good reaches, L_slow follows the same drained lows, the wrapped liquidation buys at the same")
    print("drained depth, and its bound is no looser (a deeper L_b only lowers max_in).  What the attacker saves is keeping")
    print("its LP in the pool (its fees) after the held seconds.  The two candidate floors (analysis rules, NOT adopted;")
    print("owner item O18) judge the spot leg also at L_mhi -- the upper median of the ring's bucket liquidity highs, the")
    print("box's L_hi term: every covered second's high is the keeper's honest depth, so lowering it needs 5 of 8")
    print("attacker-only buckets -- or at min(L_mhi, L_slow), which deposit-wrapped cranks cannot raise.  Forced hold levels")
    print("after a 20-s held drain; (iv)'s missed keeper seconds (1 h, the open cap and half of it, 3 / 20 misses); their")
    print("cost under a capital deposit freeze; and the delay they add to (vi)'s genuine withdrawals, against #101:")
    print()
    hdr(["pool", "position", "honest threshold", "floor", "forced, held 20 s, W 60 s", "forced, W 25 min",
         "forced, W 1 h"])
    for pool, size in HELD_CASES:
        hon = hons[(pool, size)]
        for var in ("a101", "mhi", "mhislow"):
            row([pool_name(pool), f"{size:g} x the cap", f"{hon:.3f}x",
                 {"a101": "none (#101)", "mhi": "L_mhi", "mhislow": "min(L_mhi, L_slow)"}[var]] +
                [drag_forced(var, pool, w, hon, size, held=20) for w in HELD_WS])
    print()
    print("(At 640 SOL, half the cap, the 2.02x level is one the honest keeper liquidates too -- at +90 s, the")
    print("threshold's bisection is to 0.005x -- so there the wrapped liquidation only comes first, under every rule.)")
    print()
    base = DRAG_POOLS[0]
    hdr(["position", "honest threshold", "keeper misses in a 1-h drag", "forced, #101", "forced, floor L_mhi",
         "forced, floor min(L_mhi, L_slow)"])
    for size in (1.0, 0.5):
        hon = hons[(base, size)]
        for k in (3, 20):
            ms = spread_misses(3600, k)
            row([f"{size:g} x the cap", f"{hon:.3f}x", k] +
                [drag_forced(v, base, 3600, hon, size, ms) for v in ("a101", "mhi", "mhislow")])
    print()
    print("Their cost: a capital DEPOSIT freeze (M4F-1's push, residual 2) -- an LP deposit-wraps a crank every second")
    print("(as much again as the LP counter, or 7x; no fee) -- raises every covered bucket's high, so 5 of 8 of them")
    print("raise L_mhi, while #101's covered buckets hold L_ema at the honest depth.  Flat 60 s, then a 60-s pump to a")
    print("hold level just above the honest threshold (+0.5 % ... +5 % of price), held 300 s; cells: the liquidation,")
    print("seconds after the pump starts, or open:")
    print()
    hdr(["pool", "position", "hold level", "deposit freeze", "#101", "floor L_mhi", "floor min(L_mhi, L_slow)"])
    for pool, size in ((DRAG_POOLS[0], 1.0), (DRAG_POOLS[0], 0.5), (DRAG_POOLS[3], 1.0), (DRAG_POOLS[4], 1.0)):
        hon = hons[(pool, size)] if (pool, size) in hons else drag_threshold(pool, size=size)
        for dh in (0.005, 0.01, 0.02, 0.05):
            h = round(hon + hon * dh, 3)
            for fr in (None, "dep", "dep7"):
                row([pool_name(pool), f"{size:g} x the cap", f"{h:.3f}x (+{dh * 100:g} %)", fr or "none (control)"] +
                    [dep_freeze_run(v, pool, h, fr, size=size) for v in ("a101", "mhi", "mhislow")])
    print()
    delays = {"mhi": [], "mhislow": []}
    n = 0
    for pool, size in LIVE_CASES:
        full = wd_threshold(pool, False, size)
        dr = wd_threshold(pool, True, size)
        h = round((full + dr) / 2, 4)
        cases = [(ramp_hold(h), 90, 0.0, 0)] + [(ramp_dip(h, d, 90), 90, 0.0, 0) for d in (0.0025, 0.005)]
        cases += [(ramp_hold(h), 90, 0.01, sd) for sd in (1, 2, 3)] + [(ramp_hold(1.2 * h), 30, 0.0, 0)]
        for path, wd_at, noise, sd in cases:
            n += 1
            r1 = wd_run("a101", pool, path, wd_at, 600, size=size, noise=noise, seed=sd)[0]
            for v in delays:
                rv = wd_run(v, pool, path, wd_at, 600, size=size, noise=noise, seed=sd)[0]
                delays[v].append(None if (r1 is None or rv is None) else rv["s"] - r1["s"])
    for v, ds in delays.items():
        got = [d for d in ds if d is not None]
        print(f"(vi)'s {n} genuine withdrawals under the floor {RULE_NAME[v].split(' (')[0][len('the spot leg floored at '):]}: "
              f"{len(got)} liquidated (#101: {n}), the delay against #101 {min(got):+d} to {max(got):+d} s")
    print()


def part6():
    print("## 6. M4R-2: the token dumps f x during the halt; the only candidate has 30 SOL of locked SOL at the")
    print("pre-halt price (lambda 99.78 %, D = 30 % of pool_d), i.e. 30/sqrt(f) SOL at the live price (window floor")
    print("25 SOL).  Attacker: none / the thin pool's backer pump-wraps the recording proposal back to the frozen")
    print("price; or no dump during the halt but a genuine one 5 s into the collection window.  Cells: adopted?; P_ema")
    print("vs the candidate's price at the switch (M4R: -> at the gate); real locked SOL at the live price; the")
    print("attacker's round-trip cost (engine P).")
    print()
    hdr(["f", "attacker"] + [RULE_NAME[r] for r in RULES2])
    cases = []
    for f in (4, 16):
        cases += [(f, "none", dict(m=1.0 / f)),
                  (f, "pump-wrapped recording proposal", dict(m=1.0 / f, rec_pump=f * 1.0)),
                  (f, "none; the dump comes 5 s INTO the window (M4X-3)", dict(win_move=(1.0 / f, 5)))]
    for f, name, kw in cases:
        cells = [f"{f}x", name]
        for rule in RULES2:
            o = switch_run(Spec(cand_sol=30, cand_lock=9978, horizon=30, **kw), rule)
            if o["adopted"]:
                c = o["m"].venue
                ls = locked_sol(c.locked_depth(), c.spot_wad())
                pg = f" -> {o['p_gate']:.2f}" if (rule != "built" and "p_gate" in o) else ""
                cells.append(f"adopted; P_ema/P {o['p_ratio']:.2f}{pg}; {sol(ls):.1f} SOL locked; cost "
                             f"{sol(o['cost']):.2f} SOL")
            else:
                cells.append(f"not adopted (refused {o['floor_refused']}, cancelled {o['cancelled']}); cost "
                             f"{sol(o['cost']):.2f} SOL")
        row(cells)
    print()
    print("A dump inside the 60-s window (after the proposal that opened it) is judged only by the execution, at")
    print("P_ema (M4X-3): adopted under both rules.  The warm-up then lowers P_ema towards the live price -- since")
    print("M6-SEC-1 (#105) by at most one v0.4.1 step per integration, so at the gate P_ema is still 1.86x / 7.44x")
    print("the pool after a 4x / 16x dump (the M4R column; 1.00x before #105), and the steady-state EMA follows")
    print("after it.  That is the safe side: no exit is forced (the spot leg reads the live price), and opens wait")
    print("out the adopted pool's listing window (PoolListWindow), by which time P_ema is on the pool, and are then")
    print("refused PoolTooThin (tiers and R_eff see the pool as it is).  A re-check at the execution against P_cand")
    print("would either revert (and hold every other proposal, M4V-4) or cancel (and let a dump-wrapped execution")
    print("cancel windows until S3).")
    print()


def part7():
    print("## 7. Genuine moves (no attacker; honest keeper every second): exits must still happen.")
    print()
    print("### 7a. Engine W, q 0.40 %.  Cells: outcome, seconds after the adoption, price vs pre-halt at the exit,")
    print("(borrower / lender SOL).  The dump row: the M4R proposal judges the 52-SOL lambda 50 % candidate at its")
    print("live price (13 SOL locked < 25 SOL, M4R-2) and opens no window.")
    print()
    hdr(["scenario"] + [RULE_NAME[r] for r in RULES2])
    cases = [("pump during the halt: candidate at 2.2x, flat", dict(cand_mult=2.2)),
             ("pump during the halt: 3x, flat", dict(cand_mult=3.0)),
             ("pump during the halt: 5x, flat", dict(cand_mult=5.0)),
             ("pump during the halt: 16x, flat", dict(cand_mult=16.0)),
             ("step 2.2x at +1 s", dict(cand_mult=1.0, path=step(1, 2.2))),
             ("step 5x at +1 s", dict(cand_mult=1.0, path=step(1, 5.0))),
             ("ramp 2.2x/60 s from +1 s", dict(cand_mult=1.0, path=ramp(1, 2.2, 60))),
             ("ramp 5x/60 s from +1 s", dict(cand_mult=1.0, path=ramp(1, 5.0, 60))),
             ("ramp 2.2x/120 s from +300 s", dict(cand_mult=1.0, path=ramp(300, 2.2, 120), secs=700)),
             ("dump 4x during the halt, then 10x/60 s from +1 s", dict(cand_mult=0.25, path=ramp(1, 10.0, 60, 0.25))),
             ("lambda 99.78 % 30-SOL candidate: pump during the halt 2.0x, flat",
              dict(cand_sol=30, cand_lock=9978, cand_mult=2.0)),
             ("lambda 99.78 % 30-SOL candidate: pump during the halt 3x, flat",
              dict(cand_sol=30, cand_lock=9978, cand_mult=3.0)),
             ("lambda 99.78 % 30-SOL candidate: step 2x at +1 s", dict(cand_sol=30, cand_lock=9978, path=step(1, 2.0))),
             ]
    for name, kw in cases:
        kw.setdefault("secs", 300)
        row([name] + [wcell(run_fast_path(r, Cfg(**kw)), True) for r in RULES2])
    print()
    print("### 7b. The big carried position (part 4b's markets), pump during the halt to m, flat (engine W).")
    print()
    hdr(["m", "q / x_cand"] + [RULE_NAME[r] for r in RULES2])
    for mult in (1.6, 1.8, 2.0, 2.5):
        os_ = [run_fast_path(r, Cfg(inc_sol=200, inc_fee=10000, cand_fee=10000, cand_sol=50, cand_lock=9978,
                                    q_bps=250, cand_mult=mult, secs=300)) for r in RULES2]
        row([f"{mult:.1f}", f"{os_[0]['q_x'] * 100:.2f} %"] + [wcell(o, True) for o in os_])
    print()
    print("### 7c. Engine P: candidate at 50 % of the incumbent's reserves (R100: 50 SOL), lambda 90 %; 'during the")
    print("halt' = the candidate trades at m x the pre-halt price when the window opens; 'after' = the adopted pool")
    print("moves from the adoption on.  Cells: outcome (time after the adoption, price vs pre-halt, lender P&L at")
    print("the exit price, borrower remainder).")
    print()
    hdr(["market", "move"] + [RULE_NAME[r] for r in RULES2])
    moves = [("during the halt: x2", dict(m=2.0)), ("during the halt: x5", dict(m=5.0)),
             ("during the halt: x0.5, then flat", dict(m=0.5)),
             ("after: step x2 at +1 s", dict(post=step_path(1, 2.0))),
             ("after: step x5 at +1 s", dict(post=step_path(1, 5.0))),
             ("after: ramp x2 over 30 s", dict(post=ramp_path(1, 2.0, 30))),
             ("after: ramp x5 over 60 s", dict(post=ramp_path(1, 5.0, 60)))]
    for inc, cs in (("R100", 50), ("RA", 1250), ("RC", 43)):
        for name, kw in moves:
            row([inc, name] + [pcell(switch_run(Spec(inc=inc, cand_sol=cs, cand_lock=9000, horizon=300, **kw), r))
                               for r in RULES2])
    print()
    print("### 7d. Engine M: a genuine pump during the halt, flat after (honest keeper only).")
    print()
    hdr(["candidate price"] + [RULE_NAME[r] for r in RULES2])
    for mult in (2.1, 2.5, 3.0, 5.0):
        row([f"{mult}x"] + [mcell(run_plan(r, mult=mult, post_crank=None, post_liq=None, secs=300)[0])
                            for r in RULES2])
    print()


def part8():
    print("## 8. Degraded mode: no honest observer during the warm-up.")
    print()
    print("### 8a. Engine W, m 1.99 (healthy on the pool as it trades; honest threshold 2.01).  (a) The keeper is")
    print("absent for the first k seconds after the adoption; the M4R-1 attacker is present every second")
    print("(withdraw-wrapped execute, cranks and liquidation attempts).  (b) Nobody observes for 600 s after the")
    print("adoption, then the keeper; genuinely liquidatable at 2.2x.  (c) The adopted pool halts at +3 s (warm-up")
    print("incomplete), 2.2x; keeper cranks every second.  (d) A genuine held withdrawal of every unlocked LP unit")
    print("for s in [1, k), re-deposited at k, then the per-second withdraw-wrapped freeze and liquidation.  (e), (f)")
    print("A held price push through the warm-up (the §12.6 held model).")
    print()
    hdr(["case"] + [RULE_NAME[r] for r in RULES2])
    for k in (2, 4, 8, 9, 13, 31):
        row([f"(a) keeper absent s < {k}"] + [wcell(run_fast_path(r, Cfg(
            cand_mult=1.99, exec_move="wd", att_crank="wd", att_liq="wd", keeper=lambda s, k=k: s >= k, secs=150)))
            for r in RULES2])
    row(["(b) no observation for 600 s, then the keeper (2.2x)"] +
        [wcell(run_fast_path(r, Cfg(cand_mult=2.2, observe_from=600, secs=900))) for r in RULES2])
    row(["(c) the new pool halts at +3 s (2.2x), 1,500 s"] +
        [wcell(run_fast_path(r, Cfg(cand_mult=2.2, new_halt_at=3, secs=1500))) for r in RULES2])
    for k in (4, 9, 21, 41):
        row([f"(d) held withdrawal s in [1, {k}), then withdraw-wrapped freeze"] + [wcell(run_fast_path(r, Cfg(
            cand_mult=1.99, held=(1, k, "wd"), att_crank="wd", att_liq="wd", secs=200))) for r in RULES2])
    row(["(e) the attacker holds the pool at 1.2x for s in [1, 9) (every observation sees it), withdraw-wrapped "
         "liquidation every second"] + [wcell(run_fast_path(r, Cfg(cand_mult=1.99, held_price=(1, 9, 1.2),
                                                                   att_liq="wd", secs=300)), True) for r in RULES2])
    row(["(f) as (e), the keeper absent for s in [1, 9)"] + [wcell(run_fast_path(r, Cfg(
        cand_mult=1.99, held_price=(1, 9, 1.2), att_liq="wd", keeper=lambda s: s >= 9, secs=300)), True)
        for r in RULES2])
    print()
    print("### 8b. Engine M: no keeper in the window nor after; the recording proposal is pump-wrapped (3x) and the")
    print("attacker holds the adopted pool at mult x the true price every second (held model: every observation")
    print("of the second is pushed) until the position would be liquidatable, then sandwiches the liquidation.")
    print("Cells: P_ema / P at the switch; the best mult, the seconds held, the attacker's net (SOL).")
    print()
    hdr(["rule", "result"])
    for rule in RULES2:
        o, m = run_plan(rule, mult=1.0, rec_move=("pump", 3.0), post_crank=None, post_liq=None, secs=0)
        pos, t0 = o["pos"], o["t_adopt"]
        p_true = m.venue.spot_wad()
        best = None
        for mu in (1.1, 1.2, 1.3, 1.5, 2.0, 3.0):
            mm, pp = copy.deepcopy(m), copy.deepcopy(pos)
            got = push_until(mm, [pp], t0, p_true, mu, "held", lambda a, b: would_liq(a, pp, b), 60)
            if got is None:
                continue
            t, cost, n = got
            sw = sandwich_exit(mm, pp, t + 1)
            if sw is None:
                continue
            net = sw[0] - cost
            if best is None or net > best[0]:
                best = (net, mu, n)
        res = ("not liquidatable in 60 s" if best is None else
               f"x{best[1]} for {best[2]} s, net {best[0] / 1e9:+.4f} SOL")
        row([RULE_NAME[rule], f"P/P {o['p_ratio']:.3f}; " + res])
    print()
    print("### 8c. Steady state for comparison (no switch; the same under both rules; pre-existing).  The position is")
    print("opened on a lambda 50 % 52-SOL CPMM pool; the price walks to m x over 60 s (EMA converged).  At T0 the")
    print("attacker withdraws every unlocked LP unit for k seconds (a GENUINE withdrawal: the keeper's cranks see D),")
    print("re-deposits, then holds L_ema with a withdraw-wrapped crank every second and tries a withdraw-wrapped")
    print("liquidation every second; or k attacker-only seconds (keeper absent, the attacker's cranks withdraw-")
    print("wrapped).  Cells: outcome, seconds after T0, L_ema / √(X·Y) at the end.")
    print()
    ks = (0, 3, 5, 8, 12, 20, 40)
    hdr(["m", "variant"] + [f"k = {k}" for k in ks])
    for mult in (1.99, 2.00, 2.01):
        for label, kw in (("held withdrawal k s, then freeze", lambda k: dict(k_held=k)),
                          ("k attacker-only seconds, then freeze", lambda k: dict(k_held=0, keeper_gap=(0, k)))):
            cells = []
            for k in ks:
                who, s_, r, l = steady_run(mult, **kw(k))
                cells.append(f"{who} +{s_}s, {l:.2f}" if r else f"open, {l:.2f}")
            row([f"{mult:.2f}", label] + cells)
    ref = []
    for mult in (2.00, 2.01, 2.02):
        who, s_, r, l = steady_run(mult, 0, freeze=False, secs=60)
        ref.append(f"m {mult:.2f} {'liquidated' if r else 'healthy'}")
    print()
    print("Reference (no attack, 60 s): " + "; ".join(ref) + ".")
    print()
    part8d()


def bundle_market(rule, extra_bps):
    """Engine W's reproducer market with the position's vault raised by extra_bps of V (add_margin), adopted onto
    the 52-SOL lambda 50 % candidate trading flat at the pre-halt price: (market, position, candidate, t_adopt)."""
    inc = pool_at(100.0, 10_000)
    m = RULE_CLS[rule](inc, TIER_C, DEFAULT)
    pos = m.open(inc.X * 40 // BPS, 0)
    pos.V += pos.V * extra_bps // BPS
    p0 = inc.spot_wad()
    for t in list(range(1, 61)) + list(range(61, TH - 10, 30)) + list(range(TH - 10, TH)):
        m.step_to(t)
    inc.disabled = True
    c = pool_at(52.0, 5000, out_share_ppm=160_000, name="CPMM")
    c.move_to_price(p0)
    t, fp_t0 = TH, None
    while True:
        t += 1
        m.step_to(t)
        if m.pool_fail_since is None:
            continue
        if fp_t0 is None and t >= m.pool_fail_since + m.P.pool_fast_route_s and proposal_floor(rule, m, c):
            fp_t0 = t
            l_rec = c.sqrt_k()
        if fp_t0 is not None and t >= fp_t0 + m.P.pool_fast_window_s:
            if rule == "built":
                m.adopt_pool(c, t, l_rec=l_rec, l_live=c.sqrt_k())
            else:
                m.adopt_pool(c, t)
            return m, pos, c, t


def bundle_run(base, kc, kl, absent, secs=30):
    """The keeper is absent for s <= absent after the adoption, then cranks and tries to liquidate every second;
    every second the attacker sends [buy to kc x, crank_price, sell] and [buy to kl x, liquidate, sell] (atomic
    bundles; a failed liquidation records nothing).  Returns the second of a forced liquidation (the attacker's)
    or None."""
    m, pos, c, t_adopt = copy.deepcopy(base)
    for s in range(1, secs + 1):
        t = t_adopt + s
        if s > absent:
            m.step_to(t)
            if m.try_exit(pos, t) is not None:
                return None
        snap = (c.X, c.Y)
        c.move_to_price(int(c.spot_wad() * kc))
        m.advance(t)
        m.observe(t)
        c.X, c.Y = snap
        m2, pos2 = copy.deepcopy((m, pos))
        m2.venue.move_to_price(int(m2.venue.spot_wad() * kl))
        if m2.try_exit(pos2, t) is not None:
            return s
    return None


def honest_threshold(extra_bps):
    """The candidate's price (x pre-halt, step 0.05) from which the honest keeper liquidates within 30 s (M4R)."""
    for i in range(150, 900, 5):
        mult = i / 100
        m, pos, c, t_adopt = bundle_market("m4r", extra_bps)
        c.move_to_price(int(c.spot_wad() * mult))
        for s in range(1, 31):
            m.step_to(t_adopt + s)
            if m.try_exit(pos, t_adopt + s) is not None:
                return mult
    return None


def part8d():
    print("### 8d. Atomic price bundles in attacker-only seconds (M4X-1).  Engine W's market; the position's vault")
    print("raised by add_margin (+0 / +25 / +50 / +100 % of V) so its honest threshold is about 2-4x the pre-halt")
    print("price; the candidate trades FLAT at 1.00x after the adoption.  The keeper is absent for s <= k after the")
    print("adoption (degraded mode); every second the attacker sends [buy kc, crank_price, sell] and [buy kl, liquidate,")
    print("sell], with its best kc in {1.5 ... 12} and kl in {1.0, 1.1, 1.25, 1.5} x the threshold (the pumped buyback")
    print("must still fit V - K_s).  Cells: the earliest forced liquidation, seconds after the adoption (kc, kl), or")
    print("open (30 s).  Before M4X-1 the warm-up seed read the pumped low directly: 8 attacker-only seconds forced ANY")
    print("threshold at +9 s.  With the step bound the reach is exactly as built's (whose P_ema moves at most")
    print("x(tau + dt*(1 + C))/(tau + dt) per integration, the same bound), and never earlier (the gate).")
    print()
    rules = ("built", "nobound", "m4r")
    ks = (7, 8, 9, 10, 12)
    hdr(["margin", "threshold", "rule"] + [f"keeper absent s <= {k}" for k in ks])
    for extra in (0, 2500, 5000, 10_000):
        th = honest_threshold(extra)
        for rule in rules:
            base = bundle_market(rule, extra)
            cells = []
            for absent in ks:
                best = None
                for kc in (1.5, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0, 12.0):
                    for f in (1.0, 1.1, 1.25, 1.5):
                        s_ = bundle_run(base, kc, th * f, absent)
                        if s_ is not None and (best is None or s_ < best[0]):
                            best = (s_, kc, th * f)
                cells.append("open" if best is None else f"+{best[0]} s ({best[1]:g}, {best[2]:.2f})")
            row([f"+{extra // 100} %", f"{th:.2f}x", RULE_NAME[rule]] + cells)
    print()


def part9():
    print("## 9. Stale-high P_ema after a mild dip (engine M): the incumbent's price rises to pre x the open spot")
    print("(EMA converged there), the incumbent halts, the candidate trades at m x (a mild dip); after the adoption")
    print("the LP holder sends a withdraw-wrapped crank and a withdraw-wrapped liquidation every second after the")
    print("keeper.  CR = the position's CR on the candidate at its true depth / at D (cr_liq 15,000).  Cells: P_ema /")
    print("P at the switch; outcome in 90 s [borrower / lender / attacker's keeper reward, SOL].")
    print()
    hdr(["pre -> m", "CR", "M4 as built", "M4R"])
    for pre, mult in ((2.0, 1.98), (2.005, 1.98), (2.01, 1.98), (2.01, 1.985), (2.01, 1.99), (2.01, 1.95)):
        h, d = honest_health(mult)
        cells = []
        for rule in RULES2:
            o, m = run_plan(rule, mult=mult, pre_mult=pre)
            cells.append(f"P/P {o.get('p_ratio', 0):.3f} {mcell(o)} [{mpnl(o)}]")
        row([f"{pre}->{mult}", f"{h}/{d}"] + cells)
    print()


def steady_run(mult, k_held, q_bps=40, lam=5000, freeze=True, secs=200, keeper_gap=None):
    """No switch (steady state, the same under both rules): the position is opened on a lambda `lam` 52-SOL CPMM
    pool; the price walks to mult x over 60 s (the EMA follows honestly); at T0 the attacker withdraws every
    unlocked LP unit for k_held seconds (genuine: every observation sees D), re-deposits, and from then on sends a
    withdraw-wrapped crank and a withdraw-wrapped liquidation every second.  keeper_gap: (s0, s1) keeper absent
    (attacker-only seconds, his cranks withdraw-wrapped)."""
    c = pool_at(52, lam, 2500, 160_000, "CPMM")
    m = Market(c, TIER_C, DEFAULT)
    pos = m.open(c.X * q_bps // BPS, 0)
    p0 = c.spot_wad()
    for t in range(1, 61):
        m.step_to(t)
    for t in range(61, 121):
        c.move_to_price(int(p0 * (1 + (mult - 1) * (t - 60) / 60)))
        m.step_to(t)
    for t in range(121, 200):
        m.step_to(t)
    T0 = 200
    units = lp_withdraw_all(c) if k_held else 0
    for s in range(0, secs):
        t = T0 + s
        if k_held and s == k_held:
            lp_deposit(c, units)
        keeper = keeper_gap is None or not (keeper_gap[0] <= s < keeper_gap[1])
        if keeper:
            m.step_to(t)
            r = m.try_exit(pos, t)
            if r:
                return ("keeper", s, r, m.liq / c.sqrt_k())
        if freeze and (s >= k_held or not keeper):
            with Wrap(c, "wd"):
                m.advance(t)
                m.observe(t)
            m2, pos2 = copy.deepcopy((m, pos))
            c2 = m2.venue
            wr = Wrap(c2, "wd")
            wr.__enter__()
            r2 = m2.try_exit(pos2, t)
            wr.__exit__()
            if r2:
                return ("attacker", s, r2, m.liq / c.sqrt_k())
        if not keeper and not freeze:
            m.advance(t)
    return ("open", secs, None, m.liq / c.sqrt_k())


FUZZ_ATT = [dict(rec_move="wd", exec_move="wd", att_crank="wd", att_liq="wd"),
            dict(exec_move="wd", att_crank="wd", att_liq="wd", att_price=("pump", 1.5)),
            dict(rec_move="dep", exec_move="wd", att_crank="wd", att_liq="wd"),
            dict(rec_move="wd", exec_move="dep", att_crank="dep", att_liq="wd"),
            dict(att_crank="wd", att_liq="wd")]


def fuzz_forced(n=1200, seed=23):
    """Random candidate (lambda 10-99.78 %, 52-300 SOL), price multiple drawn around the position's own threshold,
    random size; attacker = one of the strongest per-second LP schedules.  FORCED = the attacked run exits while
    the honest-only run of the same rule stays open for 90 s."""
    rng = random.Random(seed)
    cases = []
    for _ in range(n):
        lam = rng.choice((1000, 2500, 5000, 7500, 9000, 9978))
        sol_ = rng.choice((52, 80, 150, 300))
        mult = round(rng.uniform(1.93, 2.06), 3)
        q = rng.choice((20, 40, 74))
        cases.append((lam, sol_, mult, q, rng.choice(FUZZ_ATT)))
    res = {}
    for rule in RULES2:
        forced = honest_open = n_ad = 0
        ex = []
        for lam, sol_, mult, q, att in cases:
            base = dict(cand_sol=sol_, cand_lock=lam, cand_mult=mult, q_bps=q, secs=90)
            h = run_fast_path(rule, Cfg(**base))
            if h["adopt"] is None:
                continue
            n_ad += 1
            if h["exit"] != "open":
                continue
            honest_open += 1
            a = run_fast_path(rule, Cfg(**base, **att))
            if a["exit"] != "open":
                forced += 1
                ex.append((lam, sol_, mult, q, att, a["exit"], a["t_exit"], a["who"]))
        res[rule] = (n_ad, honest_open, forced, ex)
    return cases, res


def part10():
    print("## 10. Targeted random search (engine W): 1,200 runs, seed 23.  Candidate lambda 10-99.78 %, 52-300 SOL,")
    print("m drawn in [1.93, 2.06] around the position's threshold, q 0.20 / 0.40 / 0.74 %; attacker = one of five")
    print("per-second LP schedules (withdraw or deposit around the samples, withdraw-wrapped cranks and liquidations,")
    print("atomic 1.5x price bundles).  FORCED = the attacked run exits while the honest-only run of the same rule")
    print("stays open for 90 s.")
    print()
    cases, res = fuzz_forced()
    hdr(["rule", "adopted", "honest-open", "FORCED",
         "first forced cases (lambda, SOL, m, q bps, attacker, exit, s, by)"])
    for rule in RULES2:
        n_ad, ho, fo, ex = res[rule]
        row([RULE_NAME[rule], f"{n_ad}/{len(cases)}", ho, fo, "; ".join(str(e) for e in ex[:3]) or "-"])
    print()


# ======================================================================================================
# Part 11: switch only if all fit (#99)
# ======================================================================================================
def fit_numbers(m, c):
    """(q / X0, the per-position cap / X0, sum Q / X0, the OI cap / X0, fits) of market m's open positions on
    candidate c (#99): X0 = carried_x0 at P_ema with the candidate's live X (the proposal's, the strictest of the
    evaluations; the fee-scaled caps at the rule's carried fee, Market.carried_fee: min(phi_inc, phi_cand,
    CARRIED_FEE_CAP_PPM) as adopted, FIT-2 and FITS-1); fits = the proposal and the execution both pass
    (Market.carried_fit)."""
    x0 = carried_x0(c.locked_depth(), m.liq_slow, m.ema, c.X, m.P)
    phi = m.carried_fee(c)
    cap = min(max_q_impact(x0, m.tier.impact_pos_bps), x0 * phi * m.P.fee_size_cap_x100 // (100 * PPM))
    oi = min(max_total_q_market(x0, m.tier.impact_mkt_bps), x0 * phi * m.P.fee_oi_cap_x100 // (100 * PPM))
    qb = carried_q_bound(m.max_q_hwm, m.sum_q)
    fits = m.carried_fit(c) and m.carried_fit(c, execution=True)
    return qb / x0, cap / x0, m.sum_q / x0, oi / x0, fits


def w_setup(cand_sol, cand_lock, q_bps, inc_sol=100.0, inc_lock=10_000, inc_fee=2500, cand_fee=2500):
    """Engine W's market (position open, EMA converged) and candidate at the pre-halt price."""
    inc = pool_at(inc_sol, inc_lock, fee_ppm=inc_fee)
    m = Market(inc, TIER_C, DEFAULT)
    m.open(inc.X * q_bps // BPS, 0)
    for t in range(1, 61):
        m.step_to(t)
    return m, pool_at(cand_sol, cand_lock, fee_ppm=cand_fee, out_share_ppm=160_000, name="CPMM")


def p_setup(inc, cand_sol, cand_lock, size=1.0, cand_fee=None, rule="m4r"):
    """Engine P's market (position at size x the cap, EMA converged) and candidate at the pre-halt price (its fee the
    incumbent's, or cand_fee ppm)."""
    f, tier = INCUMBENTS[inc]
    m = P_RULES[rule](f(), TIERS[tier], DEFAULT)
    m.open(max(1, int(m.max_open_q() * size)), 0)
    for t in range(1, 61):
        m.step_to(t)
    v = m.venue
    return m, RaydiumPool(v.X * cand_sol * LAMPORTS_PER_SOL // v.Y, cand_sol * LAMPORTS_PER_SOL,
                          fee_ppm=v.fee_ppm if cand_fee is None else cand_fee, out_share_ppm=v.out_share_ppm,
                          name="cand", lock_bps=cand_lock)


def fit_limit_size(inc, cand_sol, cand_lock, cand_fee=None, share=1.0, rule="m4r"):
    """The size (x the incumbent's max_open_q, bisected to 1e-4) at which the carried position is at the candidate's
    per-position cap under `rule` (fit_numbers), times `share`."""
    lo, hi = 0.0001, 4.0
    while hi - lo > 1e-4:
        mid = (lo + hi) / 2
        m, c = p_setup(inc, cand_sol, cand_lock, mid, cand_fee, rule)
        if fit_numbers(m, c)[4]:
            lo = mid
        else:
            hi = mid
    return math.floor(lo * share * 10_000) / 10_000


def m_setup(q_bps=40):
    """Engine M's market and candidate."""
    m = ms_market("m4r")
    m.open(m.venue.X * q_bps // BPS, 0)
    for t in range(1, 61):
        m.step_to(t)
    return m, ms_cand()


def fit_row(label, cand, m, c):
    qx, cap, sq, oi, ok = fit_numbers(m, c)
    row([label, cand, f"{qx * 100:.3f} %", f"{cap * 100:.3f} %", f"{sq * 100:.3f} % / {oi * 100:.3f} %",
         "adopted" if ok else "refused (CarriedPositionsTooLarge)"])


def f99cell(o, ctrl=None):
    """Part 11 cell: without an adoption, every proposal refused and the S3 at the deadline; otherwise the exit, its
    delay against the control run of the same column, and the lender P&L at the exit price."""
    if not o["adopted"]:
        r = o["exit"]
        if r is None:
            return f"no window (#99: {o['fit_refused']} proposals refused); open"
        pos = o["pos"]
        v = pos.Q * o["m"].ema // WAD
        return (f"no window (#99: every proposal refused); {r['kind']} at the deadline (+{o['s3_t'] / 86400:.2f} d): "
                f"lender {sol(r['lender_sol']):.3f} for q worth {sol(v):.3f} at the frozen P_ema "
                f"({(r['lender_sol'] - v) * 100 / v:+.1f} %), borrower {sol(r['borrower']):.3f}")
    r = o["exit"]
    if r is None:
        return f"open ({o['t_end'] - o['t_adopt']} s)"
    s = f"{r['kind']} +{o['t_exit_rel']} s @{r['spot'] / o['p_pre']:.3f}x"
    if ctrl is not None and ctrl["exit"] is not None and ctrl["adopted"]:
        d = o["t_exit_rel"] - ctrl["t_exit_rel"]
        dp = (r["spot"] / ctrl["exit"]["spot"] - 1) * 100
        s += f" ({d:+d} s, {dp:+.1f} % in price)"
    return s + f", lender {sol(lender_pnl(o)):+.3f}"


# (candidate SOL, lambda, size x RA's cap, ramp top): carried positions at the #99 fit limit
FIT_ROWS = ((2510, 9978, 1.0, 3.0), (5000, 5000, 1.0, 3.0), (1260, 9978, 0.5, 3.0), (630, 9978, 0.25, 3.0),
            (314, 9978, 0.125, 3.0), (320, 9978, 0.125, 4.0))
# (label, freeze, freeze_from, held deposit (s0, s1 relative to W, k), W)
FIT_ATTACKS = (("none (control)", None, 1, None, 120),
               ("deposit freeze dep from +1 s (M4F-1, while the guard is set)", "dep", 1, None, 120),
               ("deposit freeze dep7 from +1 s", "dep7", 1, None, 120),
               ("deposit freeze dep from +40 s (M4F-1 after the clear; M4G-1's limit: every second after the clear "
                "but the keeper's attacker-only)", "dep", 40, None, 120),
               ("deposit freeze dep7 from +40 s", "dep7", 40, None, 120),
               ("held deposit 1x from +20 s, pulled at the pump's first second (M4G-3)", None, 1, (20, 1, 1), 120),
               ("held deposit 7x from +20 s, pulled at the pump's first second (M4G-3)", None, 1, (20, 1, 7), 120),
               ("held deposit 7x from +20 s, pulled 40 s into the pump (just before the control's exit)", None, 1,
                (20, 40, 7), 120),
               ("none (control)", None, 1, None, 600),
               ("held deposit 7x from +20 s for 580 s, pulled at the pump's first second (M4G-3)", None, 1,
                (20, 1, 7), 600))
FIT_COLS = (("new", "m4r"), ("old", "m4r"), ("new", "nocap"))
# Part 11d (FIT-2): (label, freeze, freeze_from, held deposit (s0, s1 relative to W, k), W); the #101 keeper
FIT2_ATTACKS = (("none (control)", None, 1, None, 120),
                ("deposit freeze dep7 from +40 s (M4F-1 after the clear; M4G-1's limit)", "dep7", 40, None, 120),
                ("none (control, W 600)", None, 1, None, 600),
                ("held deposit 7x from +20 s, pulled at the pump's first second (M4G-3)", None, 1, (20, 1, 7), 600),
                ("held deposit 7x from +20 s, pulled at the pump, then a deposit freeze dep7", "dep7", 601,
                 (20, 1, 7), 600))
FIT2_RAMPS = (30, 60, 120)


def part11():
    print("## 11. Switch only if all fit (#99, owner decision 2026-09-29; ECON §6.1, §6.2, §12.12 (11); ARCH §2.6")
    print("rule 3).  A proposal that opens or improves a window whose adoption carries open positions, and the")
    print("execution, also need every open position to fit the candidate's normal opening caps as an open on it right")
    print("after the adoption: X0 = min(X_cand, min(L_slow, D)/sqrt(p), D/(lambda_min*sqrt(p))) <= D/sqrt(p), the")
    print("candidate's locked token reserve (the execution without X_cand), at p = min(P_ema, P_cand) and at P_ema;")
    print("min(max_q_hwm, sum Q) <= min(impact cap, 3.00*phi*X0) and sum Q <= min(OI cap, 7.50*phi*X0), phi =")
    print("min(phi_inc, phi_cand) since FIT-2's fix (#100; phi_inc = Market.min_open_fee_ppm, the lowest fee an open paid")
    print("since the market last had no position) and at most 2,500 ppm since FITS-1 (#101; CARRIED_FEE_CAP_PPM)")
    print("(Market.carried_fit; CarriedPositionsTooLarge / cancelled).  Parts 1-10 force the adoption to study its")
    print("mechanics; this part applies the rule.")
    print()
    print("### 11a. The carried positions of parts 1-10 on their candidates (X0 at P_ema with the candidate's live X;")
    print("one position, so sum Q = q).  Cells: q / X0; the per-position cap / X0 (the fee-scaled 0.75 % at 25 bps and,")
    print("since FITS-1 (#101), at any fee: a 1 % -> 1 % switch's was the 2.470 % impact cap); sum Q / X0 and the OI cap")
    print("/ X0; the #99 verdict.")
    print()
    hdr(["parts", "candidate", "q / X0", "position cap / X0", "sum Q / X0 vs OI cap / X0", "#99"])
    for q in (40, 74):
        m, c = w_setup(52, 5000, q)
        fit_row(f"1, 3, 4b, 7a, 8a, 10 (engine W, q {q / 100:.2f} % of the 100-SOL incumbent's X)",
                "52 SOL, lambda 50 %", m, c)
    for sol_, lock in ((240, 2500), (400, 1000)):
        m, c = w_setup(sol_, lock, 74)
        fit_row("2a (q 0.74 %)", f"{sol_} SOL, lambda {lock / 100:.0f} %", m, c)
    for sol_, lock in ((30, 9978), (150, 9000), (150, 9978), (120, 2500), (240, 2500), (400, 1000)):
        m, c = w_setup(sol_, lock, 40)
        fit_row("3, 7a (q 0.40 %)", f"{sol_} SOL, lambda {lock / 100:.2f} %", m, c)
    m, c = w_setup(50, 9978, 250, inc_sol=200, inc_fee=10000, cand_fee=10000)
    fit_row("4b, 7b (big carried position, 1 % fees, q 2.5 %)", "50 SOL, lambda 99.78 %", m, c)
    for cs in (630, 314, 320):
        m, c = p_setup("RA", cs, 9978)
        fit_row("4a, 4c, 4d (engine P, RA at the size cap)", f"{cs} SOL, lambda 99.78 %", m, c)
    m, c = p_setup("RA", 625, 5000)
    fit_row("5 (RA at the size cap)", "625 SOL, lambda 50 %", m, c)
    for cs, lock, part in ((104, 2500, "2b"), (30, 9978, "6"), (50, 9000, "7c")):
        m, c = p_setup("R100", cs, lock)
        fit_row(f"{part} (engine P, R100 at the size cap)", f"{cs} SOL, lambda {lock / 100:.2f} %", m, c)
    m, c = m_setup()
    fit_row("7d, 8b, 9 (engine M, q 0.40 %)", "52 SOL, lambda 50 %", m, c)
    cases, _ = fuzz_cases()
    n_floor = n_fit = 0
    for lam, sol_, mult, q, att in cases:
        m, c = w_setup(sol_, lam, q)
        c.move_to_price(int(m.ema * mult))
        D = c.locked_depth()
        if window_floor_ok(D, m.ema, c.spot_wad()) and position_floor_ok(D, m.ema):
            n_floor += 1
            n_fit += m.carried_fit(c) and m.carried_fit(c, execution=True)
    row(["10 (random search)", "lambda 10-99.78 %, 52-300 SOL, q 0.20-0.74 %", "-", "-", "-",
         f"{n_fit} of the {n_floor} the window floor admits (of {len(cases)})"])
    print()
    print("### 11b. Part 4d's candidates under #99 (engine P, the #101 keeper, the deposit freeze from +1 s): no window")
    print("opens, the position stays on the halted incumbent at its pre-halt P_ema (never liquidatable there) and exits")
    print("through S3 at fair value at its deadline (the keeper keeps the chain with a crank every 30 s).  The same for")
    print("every attack of 4d, M4G-1 and M4G-3: nothing after an adoption happens.")
    print()
    hdr(["candidate", "q / X0 (cap 0.75 %)", "#99 outcome"])
    for cs, fr in ((314, "dep"), (630, "dep3"), (320, "dep7")):
        o = switch_run(Spec(inc="RA", cand_sol=cs, cand_lock=9978, post=flat_then_ramp_to(120, 3.0), horizon=520,
                            freeze=fr, keeper_rule="new", fit=True, to_s3=True), "m4r")
        m, c = p_setup("RA", cs, 9978)
        row([f"RA -> {cs} SOL, lambda 99.78 %", f"{fit_numbers(m, c)[0] * 100:.2f} %", f99cell(o)])
    print()
    print("### 11c. Carried positions at the #99 fit limit (RA's position at size x its cap, carried onto a candidate")
    print("whose opening caps it just fits) under the M4F-1 / M4G attacks: the rule on (engine P, fit enforced);")
    print("flat W s after the adoption, then a genuine pump to the top over 120 s.  Columns: the #101 keeper (per second")
    print("while the guard is set, price bursts, per-second coverage while foreign liquidity deviations recur) and the")
    print("keeper as built at M4R, with the reference cap; and the #101 keeper without the cap.  Cells: outcome (time")
    print("after the adoption, price vs pre-halt; delay against the control of the same column), lender P&L at the")
    print("exit price.")
    for cs, lock, size, top in FIT_ROWS:
        m, c = p_setup("RA", cs, lock, size)
        qx0, cap, _, _, ok = fit_numbers(m, c)
        print()
        print(f"#### RA -> {cs} SOL, lambda {lock / 100:.2f} %; q = {size:g} x RA's cap = {qx0 * 100:.3f} % of X0 "
              f"(cap {cap * 100:.3f} %), {m.max_q_hwm * 100 / c.X:.3f} % of the candidate's X; pump to {top:g}x")
        print()
        hdr(["attack", "W (s)"] + [f"{KEEPER_NAME[k]}, {RULE_NAME[r]}" for k, r in FIT_COLS])
        ctrl = {}
        for name, fr, frm, held, w in FIT_ATTACKS:
            cells = [name, w]
            for k, r in FIT_COLS:
                hd = None if held is None else (held[0], w + held[1], held[2])
                o = switch_run(Spec(inc="RA", cand_sol=cs, cand_lock=lock, size=size, post=flat_then_ramp_to(w, top),
                                    horizon=w + 400, freeze=fr, freeze_from=frm, held_dep=hd, keeper_rule=k,
                                    fit=True), r)
                if fr is None and held is None:
                    ctrl[(k, r, w)] = o
                cells.append(f99cell(o, ctrl.get((k, r, w))))
            row(cells)
    print()
    part11d()
    part11e()


def ramp_to(w, top, dur):
    """Flat at the adoption price for w seconds, then a genuine linear pump to top x over dur seconds."""
    return lambda s: 1.0 if s <= w else 1.0 + (top - 1.0) * min(s - w, dur) / dur


def fit2_table(inc, cs, size, rule, ramps=FIT2_RAMPS):
    """Part 11d's matrix: every FIT2_ATTACKS attack (rows) at each 4x ramp (columns), the position carried onto the
    cs-SOL lambda 99.78 % 1 % candidate under `rule` (fit enforced, the #101 keeper); cells with the delay against the
    control of the same W and ramp."""
    cells = {}
    for dur in ramps:
        ctrl = {}
        for name, fr, frm, held, w in FIT2_ATTACKS:
            hd = None if held is None else (held[0], w + held[1], held[2])
            o = switch_run(Spec(inc=inc, cand_sol=cs, cand_lock=9978, size=size, cand_fee=10_000,
                                post=ramp_to(w, 4.0, dur), horizon=w + 400, freeze=fr, freeze_from=frm,
                                held_dep=hd, keeper_rule="new", fit=True), rule)
            if fr is None and held is None:
                ctrl[w] = o
            cells[(name, dur)] = f99cell(o, ctrl.get(w))
    hdr(["attack"] + [f"4x/{d} s" for d in ramps])
    for name, *_ in FIT2_ATTACKS:
        row([name] + [cells[(name, d)] for d in ramps])


DAY = 86_400


def fit3_run(griefer, opens_d=(0, 10, 20), share=0.25, cand_bps=4000, halt_d=21, horizon_d=40):
    """FIT-3 (residual 14): RA's market (Tier A, 30-day terms) with three honest positions of `share` x the
    incumbent's size cap opened on days opens_d; with `griefer`, one borrower opens at the incumbent's cap on day
    20 and closes a second later, so max_q_hwm stays at its q while the honest positions are open.  On day halt_d
    the incumbent halts; the only candidate has cand_bps of its reserves at lambda 99.78 % (its per-position cap
    about 0.4 x the incumbent's), which every honest position and their OI fit.  The keeper cranks every 30 s
    (duty 5), proposes the candidate whenever it fits (Market.carried_fit) and executes the window 60 s later;
    each position exits at its deadline (S3 on the halted pool, an expiry on an adopted one).  Returns (events,
    proposals refused, the incumbent's cap, the griefer's cost in lamports)."""
    v = MARKETS["RA"]()
    m = Market(v, TIERS["A"], DEFAULT)
    cap = m.max_open_q()
    t = 0
    pos, ev = [], []
    cost = None

    def adv(to, dt):
        nonlocal t
        while t < to:
            t = min(t + dt, to)
            m.step_to(t)

    for d in opens_d:
        adv(d * DAY, 60)
        pos.append(m.open(max(1, int(cap * share)), t))
    if griefer:
        adv(t + 3600, 60)
        g = m.open(int(m.max_open_q()), t)
        adv(t + 1, 1)
        r = m.try_exit(g, t, borrower_close=True)
        cost = g.M + g.fee_open - r["borrower"]
    adv(halt_d * DAY, 60)
    th = t
    c = RaydiumPool(v.X * cand_bps // BPS, v.Y * cand_bps // BPS, fee_ppm=v.fee_ppm, out_share_ppm=v.out_share_ppm,
                    name="cand", lock_bps=9978)
    v.disabled = True
    fp_t0 = adopted = None
    refused = 0
    while t < th + horizon_d * DAY and m.open_positions:
        t += 30 if (fp_t0 is None and adopted is None) else 1
        m.step_to(t)
        for k, p in enumerate(pos):
            if p.live and t >= p.deadline:
                r = m.try_exit(p, t)
                if r is not None:
                    # the lender's P&L against holding q: S3 pays SOL for all of q (at the frozen P_ema); a buyback
                    # exit returns the tokens bought back and pays SOL on top (at the exit price)
                    px = m.ema if r["kind"] == "S3" else r["spot"]
                    pnl = r["lender_sol"] - (p.Q - r["tokens"]) * px // WAD
                    ref = p.Q * px // WAD
                    ev.append((f"position {k + 1} (opened day {opens_d[k]}) exits", t - th, r["kind"],
                               m.sum_q / cap, f"{sol(pnl):+.3f} SOL ({pnl * 100 / ref:+.1f} % of q at "
                               f"{'the frozen P_ema' if r['kind'] == 'S3' else 'the exit price'})"))
        if adopted is None and m.open_positions:
            armed = m.pool_fail_since is not None and t >= m.pool_fail_since + m.P.pool_fast_route_s
            if fp_t0 is None and armed:
                if window_floor_ok(c.locked_depth(), m.ema, c.spot_wad()) and m.carried_fit(c):
                    fp_t0 = t
                else:
                    refused += 1
            elif fp_t0 is not None and t >= fp_t0 + m.P.pool_fast_window_s:
                if m.pool_fail() and position_floor_ok(c.locked_depth(), m.ema) and m.carried_fit(c, execution=True):
                    m.adopt_pool(c, t)
                    adopted = t
                    n = m.open_positions
                    ev.append(("the candidate adopted", t - th, f"{n} position{'s' if n != 1 else ''} carried",
                               m.sum_q / cap, "-"))
                else:
                    fp_t0 = None
    return ev, refused, cap, cost


def part11e():
    print("### 11e. FIT-3: a closed large position keeps the mark (residual 14; max_q_hwm resets only when")
    print("open_positions reaches 0).  RA's market (Tier A, 30-day terms): three honest positions of 0.25 x the")
    print("incumbent's size cap, opened on days 0 / 10 / 20; with the griefer, one borrower opens at the incumbent's")
    print("cap on day 20 and closes a second later.  The incumbent halts on day 21; the only candidate has 40 % of its")
    print("reserves at lambda 99.78 %, and every honest position and their OI fit it.  The fit bounds every open q by")
    print("min(max_q_hwm, sum Q): the griefer's closed position keeps the first term at the cap, so the candidate is")
    print("refused until the S3 exits bring sum Q down to its per-position cap.  Each healthy position on the halted")
    print("pool is frozen at the pre-halt P_ema and settles S3 at its own deadline (fair value, the expiry fee as p');")
    print("one carried by the adoption expires on the new pool.  Cells: time after the halt; sum Q / the incumbent's")
    print("cap after the event; the lender's P&L against holding q (at the frozen P_ema for S3, else the exit price).")
    for griefer in (False, True):
        ev, refused, cap, cost = fit3_run(griefer)
        print()
        head = ("with the griefer (cost {:.6f} SOL: open fee, minimum rent and the round trip's impact); {} proposals "
                "refused (CarriedPositionsTooLarge)".format(sol(cost), refused) if griefer else
                "control (no griefer)")
        print(f"#### {head}")
        print()
        hdr(["event", "after the halt", "outcome", "sum Q / cap after", "lender P&L"])
        for name, dt, kind, sq, lend in ev:
            row([name, f"+{dt} s" if dt < DAY else f"+{dt / DAY:.2f} d", kind, f"{sq:.3f}", lend])
    print()


def part11d():
    print("### 11d. FIT-2: a carried position on a higher-fee candidate, and fast pumps (engine P, fit enforced, the #101")
    print("keeper).  On a 1 % pool the per-position impact cap binds (2.47 % of X0) where 25 bps' fee-scaled cap is")
    print("0.75 %, and a carried position's margin was posted on the incumbent's cost: V/B on the new pool is lower than")
    print("for a position of the same size opened there.  RAC = RA's 2,500-SOL lambda 50 % 0.25 % AMM v4 pool at Tier C")
    print("(the fork's M4X-2 / FIT-2 market), RA the same pool at Tier A, RAC1 the same pool at a 1 % fee; the candidate")
    print("a lambda 99.78 % pool at a 1 % fee; flat W s after the adoption, then a genuine pump to 4x over R s.  '#99's")
    print("fit at phi_cand' is the rule FIT-2 broke (the carried fee-scaled caps at the candidate's fee); '#100's fit' the")
    print("carried fee-scaled caps at min(phi_inc, phi_cand) (phi_inc = Market.min_open_fee_ppm, the lowest fee an open")
    print("paid since the market last had no position), which FITS-1 broke at a 1 % -> 1 % switch; 'M4R' is the rule as")
    print("adopted (#101): the carried fee-scaled caps at min(phi_inc, phi_cand, 2,500 ppm), the impact caps unchanged,")
    print("the exit reference of #99, the spot leg at max(L_live, L_ema').  Both analysis fits run with the exits and the")
    print("keeper as adopted.  Cells: outcome (time after the adoption, price vs pre-halt; delay against the control of")
    print("the same W and ramp), lender P&L at the exit price.")
    print()
    print("(i) FIT-2's positions: RAC -> 320 SOL at 1 %, the position at 100 % / 76 % of #99's fit limit, under #99's fit")
    print("at phi_cand (the adoption allowed) and under M4R (#101).")
    for share in (1.0, 0.76):
        size = fit_limit_size("RAC", 320, 9978, 10_000, share, "phicand")
        m, c = p_setup("RAC", 320, 9978, size, 10_000)
        q0 = fit_numbers(m, c)
        s3 = switch_run(Spec(inc="RAC", cand_sol=320, cand_lock=9978, size=size, cand_fee=10_000,
                             post=ramp_to(120, 4.0, 30), horizon=520, keeper_rule="new", fit=True, to_s3=True), "m4r")
        print()
        print(f"#### {share * 100:.0f} % of #99's limit: q {q0[0] * 100:.3f} % of X0 (#101's cap {q0[1] * 100:.3f} %), "
              f"{m.max_q_hwm * 100 / c.X:.3f} % of the candidate's X.  M4R (#101), any ramp and attack: {f99cell(s3)}.")
        print(f"{RULE_NAME['phicand']}:")
        print()
        fit2_table("RAC", 320, size, "phicand")
    print()
    print("(ii) Positions at the #101 fit limit on 1 % candidates, under the pushes (M4R).")
    for inc, cs in (("RAC", 320), ("RAC", 2510), ("RA", 320)):
        size = fit_limit_size(inc, cs, 9978, 10_000)
        m, c = p_setup(inc, cs, 9978, size, 10_000)
        qx0, cap, _, _, ok = fit_numbers(m, c)
        assert ok
        print()
        print(f"#### {inc} (Tier {m.tier.name}, fee {m.venue.fee_ppm / 10_000:g} %) -> {cs} SOL, lambda 99.78 %, fee 1 %; "
              f"q at the fit limit = {qx0 * 100:.3f} % of X0 (cap {cap * 100:.3f} %), "
              f"{m.max_q_hwm * 100 / c.X:.3f} % of the candidate's X")
        print()
        fit2_table(inc, cs, size, "m4r")
    print()
    print("(iii) A 1 % -> 1 % switch (FITS-1): RAC1 (the 2,500-SOL pool at a 1 % fee, Tier C) -> 320 SOL at 1 %.")
    print("min(phi_inc, phi_cand) is the candidate's own fee, so under #100's fit the limit was the impact cap (2.47 % of")
    print("X0), on a margin posted on the deeper incumbent's cost (#100's residual 15); since #101 the carried fee caps")
    print("take at most 2,500 ppm, the limit is 0.75 % of X0, and those positions settle S3 at fair value.")
    full = fit_limit_size("RAC1", 320, 9978, 10_000, rule="fit100")
    for share in (1.0, 0.76, 0.7, 0.65, 0.6, 0.5):
        size = math.floor(full * share * 10_000) / 10_000
        m, c = p_setup("RAC1", 320, 9978, size, 10_000, rule="fit100")
        qx0, cap, _, _, ok = fit_numbers(m, c)
        assert ok
        m1, c1 = p_setup("RAC1", 320, 9978, size, 10_000)
        cap1 = fit_numbers(m1, c1)[1]
        s3 = switch_run(Spec(inc="RAC1", cand_sol=320, cand_lock=9978, size=size, cand_fee=10_000,
                             post=ramp_to(120, 4.0, 30), horizon=520, keeper_rule="new", fit=True, to_s3=True), "m4r")
        print()
        print(f"#### RAC1 -> 320 SOL: q = {share:g} x #100's fit limit = {qx0 * 100:.3f} % of X0 (#100's cap "
              f"{cap * 100:.3f} %, #101's {cap1 * 100:.3f} %), {m.max_q_hwm * 100 / c.X:.3f} % of the candidate's X.  "
              f"M4R (#101), any ramp and attack: {f99cell(s3)}.")
        print(f"{RULE_NAME['fit100']}:")
        print()
        fit2_table("RAC1", 320, size, "fit100", (30, 45, 60, 120))
    size = fit_limit_size("RAC1", 320, 9978, 10_000)
    m, c = p_setup("RAC1", 320, 9978, size, 10_000)
    qx0, cap, _, _, ok = fit_numbers(m, c)
    assert ok
    print()
    print(f"#### RAC1 -> 320 SOL at the #101 fit limit: q = {qx0 * 100:.3f} % of X0 (cap {cap * 100:.3f} %), "
          f"{m.max_q_hwm * 100 / c.X:.3f} % of the candidate's X (M4R)")
    print()
    fit2_table("RAC1", 320, size, "m4r", (30, 45, 60, 120))
    print()
    print("Native controls: the 320-SOL lambda 99.78 % 1 % pool listed directly (no switch), a position at its own open")
    print("cap (Tier C; Tier A for comparison with RA's row), the EMA converged; the old keeper (no switch, no guard:")
    print("the #99 keeper) and the #101 keeper; the same attacks from the listing's +20 / +40 s, W 600 / 120.  Cells:")
    print("outcome (seconds after the pump starts, price, lender P&L at the exit price, borrower remainder);")
    print("attacker-only seconds.")
    print()
    hdr(["pool", "q / x", "ramp", "attack", "old keeper, M4R", "#101 keeper, M4R"])
    for tier in ("C", "A"):
        for dur in FIT2_RAMPS:
            for name, fr, w, held in (("none", None, 120, None), ("deposit freeze dep7 from +40 s", "dep7", 120, None),
                                      ("held deposit 7x from +20 s, pulled at the pump", None, 600, (20, 1, 7))):
                res = [cadence_steady_run("m4r", k, 320, 9978, fr, w, 4.0, dur, tier, att_from=40, fee_ppm=10_000,
                                          held=held) for k in ("old", "new")]
                row([f"320 SOL, lambda 99.78 %, 1 %, Tier {tier}", f"{res[0][1] * 100:.2f} %", f"4x/{dur} s", name] +
                    [f"{c}; {a} a-only s" for c, _q, a in res])
    print()


def fuzz_cases(n=1200, seed=23):
    """Part 10's cases, drawn as fuzz_forced draws them."""
    rng = random.Random(seed)
    cases = []
    for _ in range(n):
        lam = rng.choice((1000, 2500, 5000, 7500, 9000, 9978))
        sol_ = rng.choice((52, 80, 150, 300))
        mult = round(rng.uniform(1.93, 2.06), 3)
        q = rng.choice((20, 40, 74))
        cases.append((lam, sol_, mult, q, rng.choice(FUZZ_ATT)))
    return cases, None


def selftest():
    """Honest switch (52-SOL lambda 50 % and 150-SOL lambda 99.78 % candidates, flat): L_ema / sqrt(X*Y) at the gate,
    box width after it, the second the guard cleared; and the gate itself."""
    out = []
    for sol_, lock in ((52, 5000), (150, 9978), (240, 2500)):
        o = run_fast_path("m4r", Cfg(cand_sol=sol_, cand_lock=lock, cand_mult=1.0, secs=60))
        out.append(f"honest switch onto {sol_} SOL at lambda {lock / 100:.2f} %: L_ema/sqrt(XY) {o['l_adopt']:.3f} at "
                   f"the adoption, {o['l_warm']:.3f} at the gate (+{o['t_warm']} s); P_ema/P {o['p_warm']:.3f}; box "
                   f"wide {o['wide_warm']} s in the warm-up, {o['wide_after']} s after; guard cleared at "
                   f"+{o['t_clear']} s")
    o = run_fast_path("m4r", Cfg(cand_mult=2.2, secs=30))
    out.append(f"gate: a genuinely liquidatable position (2.2x) is liquidated at +{o['t_exit']} s "
               f"(the 8th warm-up integration lands at +9 s)")
    return "\n".join(out)


PARTS = {"1": part1, "2": part2, "3": part3, "4": part4, "5": part5, "6": part6, "7": part7, "8": part8,
         "9": part9, "10": part10, "11": part11}

if __name__ == "__main__":
    want = sys.argv[1:] or ["selftest"] + list(PARTS)
    for k in want:
        if k == "selftest":
            print("## selftest")
            print()
            print(selftest())
            print()
        else:
            PARTS[k]()
