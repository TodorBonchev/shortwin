#!/usr/bin/env python3
"""
short.win -- economic / risk reference model and scenario simulation (v0.4.1).

Companion to docs/design/ECONOMICS.md v0.4.1 (normative for the math crate
together with pool_model.py v0.4.1; ARCHITECTURE §2.2, §8.1).  The tables in
ECONOMICS §4, §5.7, §8, §10 and §11 were produced by this script; the
manipulation tables of §12 come from ./attacks/ (which import this module).
Standard library only (Python >= 3.8).  Deterministic (fixed seeds).

    python3 shortwin_sim.py                 # everything, Monte Carlo 200 paths/cell
    python3 shortwin_sim.py --mc 400        # the run used for ECONOMICS.md
    python3 shortwin_sim.py --quick         # skip Monte Carlo and fuzz
    python3 shortwin_sim.py --only static   # static|scen|ema|lgg|mc|fuzz|selftest

What it models (integer math wherever the on-chain program uses integers):
  * Pump.fun bonding curve: virtual-reserve constant product, exact-out buy
    net = floor(Q*vs/(vt-Q)) + 1, exact-in sell floor(Q*vs/(vt+Q)), protocol and
    creator fees ceil'ed on the SOL leg; completion at real_token_reserves == 0;
    migration to a canonical PumpSwap pool seeded with 206.9M tokens and
    (real_sol - pool_migration_fee) lamports, of which 20.69 % is booked as
    virtual_quote_reserves and spent by pump on zero-fee buy-and-burn over ~6 min
    (proportional rule, 130 of 130 migrations checked; ECON §1.2).
  * PumpSwap: exact-out buy ceil(Y*b/(X-b)) with Y = real + virtual quote, fees
    (lp / protocol / creator) ceil'ed on top, LP fee retained; exact-in sell;
    sells capped by real quote reserves; market-cap fee tiers (hypothetical-state
    quotes use the LIVE tier, R3B-7b).
  * Raydium AMM v4 / CPMM (D2): constant product, fee on the INPUT amount,
    a configurable share of the fee leaving the reserves; LP units with a
    permanently locked part (lambda), so the locked depth D = lambda*sqrt(X*Y)
    is modelled and unlocked LP can be withdrawn only down to the locked part.
  * short.win v0.4 (ECONOMICS.md v0.4 changelog):
      - fixed exit bound delta over the reference (default 2 %), no Dutch clock
        (R3B simplification 1; removed: exec run, liq_exec_since, ring `lo`);
      - liquidation EMA leg on the newest liq_persist_n ring entries; no seconds
        conditions (liq_persist_s / settle_persist_s removed);
      - ring entries cache the position-independent reprojection (X_i, Y_i);
        reprice rounds so that Y/X >= P (X floor, Y ceil with a ceil sqrt, R3B-7c);
      - ONE settlement-robustness rule (R3A-1, R3A-6a, V2-9): every settlement
        decision (arm/keep, refute, kind, amount paid) must come out the same
        on the whole box spanned by the EMA state (P_ema, L_ema) and the ring
        median state (P_med, L_med); amounts within fv_dev_bps;
      - an armed position's clock keeps running on every non-refuting call
        (R3A-6b); "not liquidatable" disarms only if box-robust;
      - venue changes (graduation, Raydium fast-path switch) rewrite every ring
        entry's L and lobs to the reset liquidity (R3B-7a, R3A-11d);
      - effective ert = 0 on Raydium (R3A-3 i); caps on the conservative token
        reserve min(X_now, L_slow/sqrt(P_ema)[, D/(lambda_min*sqrt(P_ema))])
        (R3A-3 ii); one-way fee carried in ppm, no rounding (R3A-3 iii); risk
        quotes capped at venue_fee_hard_cap_bps, route-broken flag, S3 on an
        executable venue (R3A-3 iv); rent through three basis indices with
        per-position locked coefficients (the program's form, R3A-3);
      - liquidation keeper reward floored at the settlement reward K_s, paid
        from the borrower remainder (R3B-10);
      - removed owner-option knobs (not built): fv_liq, settle_s2_first,
        margin_ema_own_sale; and the v0.2 comparison knobs (arming, dutch_clock,
        ref_liq, obs_when_halted, straddle_exec, ref_lo_k / lb_hist).
  * short.win v0.4.1 (fourth review round, ECONOMICS.md changelog #74-#89):
      - a liquidation reserves nothing in max_in (SPEC §5.2): it buys back iff
        B_live <= min(B_ref*(1+delta), V - K_s - ert) -- the v0.4 penalty reserve and the
        v0.3 rent reserve both blocked honest liquidations in genuine fast pumps (R4A-1,
        R4B-1); an expiry keeps rent and the expiry fee senior (liq_reserve / exp_reserve);
      - the settlement box takes its medians over the ring entries' bucket extremes
        (lower median of the lows, upper median of the highs) instead of the clamped obs,
        so push-then-freeze cannot move it (R4B-2); ring entry (ts, p, l, x, y, lo, hi,
        llo, lhi); the liquidation reference is also capped at lo_x*(1 + kappa_eff);
      - FV's p' is decided on the box corner it is evaluated at (R4A-3);
      - the immediate part of an S2/S3 take is min(x, Q*P_ema + R_due); impact, fees and
        penalty vest (R4B-6; this also removes the B_FV reconstruction of R4A-5);
      - per-second price noise, the genuine-pump grid and aged positions in the
        config_check.py battery (R4A-1, R4B-1, R4A-4); compiled bounds re-derived;
      - analysis-only comparison switches: box_obs (v0.4 box), pprime_raw (v0.4 p'),
        ref_cap_lo=False (v0.4 reference), liq_reserve="rent" / "rent+penalty";
      - validate_params checks every ECON §15 bound of the parameters this model has
        (M1 doc sync): also the penalty split's protocol and keeper parts (<= 5,000 each)
        and the per-tier bounds (cr_open_bps <= 100,000, penalty, duration, impacts, S2
        slippage, rates, minimum rent), so the program's params::validate is this function;
        the bounds of parameters the model does not have (market sizing, boost, warm-up,
        force delay, lender timing, theta, oracle, Raydium selection) are
        shortwin_math::params::validate_econ.
  * Raydium pool selection as built after M4 (ECONOMICS.md changelog #91-#95; M4-SEC-1/-3/-5/-6,
    M4V-1/-2/-3): locked_floor_ok (listing / zero-position / list-only floor, /lambda_min),
    position_floor_ok (a collection window's floor: the locked SOL itself); pool_min_real_sol is the
    module constant POOL_MIN_REAL_SOL, not a Params field.
  * Post-change confirmation (M4R, ECONOMICS.md changelog #96; PROPOSED, owner item O18; replaces
    #92's adoption_liquidity, whose rule attacks/venue_switch.py keeps as the "M4 as built" comparison):
    Market.adopt_pool reads no sample of the adopted pool -- L_ema and the ring's L := the anchor
    floor(D*1e4/lambda_min), L_slow = min(L_slow, D) -- and marks the state unconfirmed (warm = 8,
    guard = 1); _integrate_warmup re-seeds P_ema = min(min of the post-change bucket lows, the largest v0.4.1
    step from the previous P_ema) (M4X-1: the seed rises no faster than the v0.4.1 EMA can; since M6-SEC-1, #105,
    it falls no faster either) and
    L_ema = min(max lhi, lomed(lhi)*(1 + C)) over the post-change entries; gated() (liquidate / expire /
    sol_settle wait while warm > 0 on an executable venue; Raydium opens refused); exit_reference's guard
    min(L_ema, L_mlo), and while the guard is set L_slow integrates min(lobs, L_mlo) (M4X-2); window_floor_ok
    (a proposal's window floor at min(P_ema, P_cand), M4R-2).  A market
    that never switches runs the v0.4.1 paths unchanged.  A Raydium price start (Observation::new_raydium, the
    M4V-5 first-open gate) is not modelled: every Market is created unmarked.  Market.fast_switch keeps the v0.4.1
    rewrite for the math crate's differential vectors only; FAST_SCENARIOS replays adopt_pool;
    attacks/switch_warmup.py compares the rule with the M4 rule as built.
  * Reference cap (M4F-1, ECONOMICS.md changelog #98; PROPOSED with #96): on a Raydium venue exit_reference
    caps the EMA terms of the reference liquidity at the pool's locked depth D since AIR-14,
    L_b = max(L_live, min(max(L_ema', L_slow), D)) = L_live (Market.ref_depth_cap; L_live is never capped and D <=
    L_live; the pump curve has no D and no cap).  It was floor(D*1e4/lambda_min), the depth sizing credits the
    pool with: a deposit held into a position's deadline and pulled there left the reference at up to
    lambda/lambda_min over the drained depth (2x at lambda ~ 1) and held a position's expiry up for tens of
    minutes, as M6E-1 found on PumpSwap; at D a genuine drain of a pool is priced on the drained pool, as on
    PumpSwap.  The guard and M4X-2's input guard read L_mlo, so
    they hold only while the keeper's observations fill the buckets (ECON §5.8 duty 9;
    attacks/switch_warmup.py part 4d).
  * Switch only if all fit (owner decision 2026-09-29, ECONOMICS.md changelog #99; ARCH §2.6 rule 3): an adoption
    that carries open positions -- a fast-path window's, or a list-only market's window with positions -- is
    allowed only if every open position fits the candidate's normal opening caps, evaluated as an open on it right
    after the adoption would be (carried_fit_ok: per-position impact and fee-scaled caps on carried_q_bound =
    min(max_q_hwm, sum_q), market OI and fee-scaled caps on the outstanding sum_q, on the candidate's conservative
    X0 = carried_x0 = min(X_now, min(L_slow, D)/sqrt(p), D/(lambda_min*sqrt(p))) <= D/sqrt(p), its locked token
    reserve), judged at p = min(P_ema, P_cand) and at p = P_ema, the stricter binding (Market.carried_fit).  The
    fee-scaled caps take phi = min(phi_inc, phi_cand, CARRIED_FEE_CAP_PPM) (FIT-2, changelog #100; the compiled
    cap of 2,500 ppm since FITS-1, #101): phi_inc = Market.min_open_fee_ppm, the lowest one-way venue fee paid by
    the opens since open_positions last returned to 0 -- the fee the open positions' caps and margin were computed
    at -- so a position carried onto a higher-fee pool stays in its own fee's size class, and every carried position
    is in the <= 0.75 %-of-X0 class the sims and the fork validated, whatever the two fees (a 1 % -> 1 % switch
    included); the impact caps are the candidate's.  A proposal reads the candidate's live X_now; the
    execution does not, so no LP round trip or buy around it can cancel a window (the guarantee is the execution's
    X0, FIT-1).  A proposal whose candidate does not fit opens no window (CarriedPositionsTooLarge); an execution
    whose candidate does not fit cancels.  Market.max_q_hwm is the high-water mark of q over the positions opened
    since open_positions last returned to 0 (Market.open; reset only when the last open position exits,
    Market.try_exit), sum_q the outstanding Q.  If no candidate fits, the positions stay on the halted pool and exit
    through S3 at fair value.  fast_path_events applies both checks; a position at the incumbent's size cap fits
    only a candidate whose locked depth covers the incumbent's L_slow: FAST_SCENARIOS' 50 % and 25 % candidates no
    longer carry it and its 120 % candidate does.  Zero-position adoptions and listings are unaffected.
  * The liquidation spot leg at the guarded EMA liquidity (FITS-2 (a), ECONOMICS.md changelog #101; PROPOSED, owner
    item O18): the CR-liquidatability spot leg is judged at L_spot = max(L_live, L_floor) (Market.spot_liquidity;
    L_floor = min(L_ema, L_mhi) while ema_guard = 1 -- AIR-17: the upper median of the bucket liquidity highs, not
    L_ema' = min(L_ema, L_mlo), whose lows withdraw-wrapped cranks set -- else L_ema; uncapped), so an LP withdrawal in the same
    transaction cannot make it true -- a withdraw-wrapped liquidation or expiry of a position healthy at the pool's
    depth needs L_ema itself dragged down first, which the keeper's per-second liquidity-anomaly coverage denies to
    attacker-only seconds (ECON §5.8 duty 2; attacks/switch_warmup.py part 5b) -- not to a genuine held withdrawal:
    the coverage holds the low it reached and a freeze of withdraw-wrapped cranks keeps it, forcing what the same
    LP's withdrawal for good forces (FITX-1, changelog #102, part 5b (vii)).  It is the one CR-liquidatability leg of liquidate, expire
    (UseLiquidate / UseExpire) and sol_settle (arming, "not liquidatable" gating, S3 due): Market.status.  The exit
    bound's L_b (exit_reference), the S1 / S2 legs and the refutation keep reading L_live as before.
  * The curve's cap reserves (M5-SEC-3, ECONOMICS.md changelog #103; ARCH §4.6; the program's
    engine::caps::curve_reserves): a curve open is sized on X0 = min(vT, x_ema) and the Tier D guard reads
    rT_guard = min(rT, max(0, x_ema - (vT - rT))), x_ema = floor(isqrt(vT*vQ)*1e9/isqrt_ceil(P_ema)), whenever
    P_ema > P_spot, the live (vT, rT) otherwise (Market.curve_cap_reserves): a sale wrapped around the open no
    longer lifts either.  Consecutive capped curve opens wait for the EMA to follow each open's own sale, as on
    PumpSwap (attacks/ema_atomic_multi.py's basket fill uses the same reserves on the curve).
  * M6 as built and the owner's decisions of 2026-09-30 (ECONOMICS.md changelog #104, #105; ARCH §2.5, §3.4, §5.4;
    SPEC §14 M6R-1, M6-SEC-1, M6-SEC-3, M6R-4):
      - the graduation (Market._graduate; venues::pump_amm::graduate): pump's migrate_v2 (migration_delay_s after
        the completion) creates the canonical pool and starts its boost, other traders trade it from then on
        (Market.exo_venue), and the market moves onto it in the prologue of its next instruction, after the roll of
        the curve's last bucket and with no sample of the pool: the ring and L_ema rewritten to the pool's
        lp_supply (its isqrt(X*(Y + VQR)) at creation), L_slow = min(L_slow, anchor), both route chains cleared,
        the observation state restarted (ema_warm = 8, ema_guard = 1, as a Raydium adoption), nothing recorded in
        that instruction; an open or keeper exit whose own prologue would transition is refused (VenueMigrating).
        Keeper exits and settlements wait for the 8 warm-up integrations (gated), PumpSwap opens too (OWNER-M6R-2,
        kept); the keeper's exit on a graduated position lands 8-12 s after the migration (OWNER-M6R-4, accepted).
        Market._switch_to_pool keeps v0.4.1's rule for the math crate's unit vectors only;
      - the straddle bundle (V1-9; Market._straddle_to_pool): tx1 the completion, tx2 migrate_v2, tx3 the
        transition and the pool's first bucket; the keeper's tx3 carries no exit (the warm-up gates it: the keeper
        exits on the pool at the gate), a borrower's own tx3 carries its close_short, which the warm-up does not
        gate (Market._buy_back);
      - the boost on the ELF (CPPool.boost_tick): a chunk of b lamports burns floor(X*(b - 1)/(Y + b - 1)) tokens;
      - M6-SEC-1: the warm-up price seed falls, like it rises, by at most the largest v0.4.1 step per integration
        (Market._integrate_warmup: x11/12 at dt = 1, x6/7 at dt = 2), so one low recorded in a warm-up second no
        longer sets P_ema at once;
      - M6R-1: canonical PumpSwap pools take third-party LP (R-VEN Update H11; CPPool.add_liquidity /
        remove_liquidity scale both vaults, VQR and lp_supply; the burned creation LP stays); their locked depth is
        the Raydium one on the burned LP, D = floor(lambda_bps*isqrt(X*(Y + VQR))/1e4), lambda_bps =
        floor((lp_supply - lp_mint.supply)*1e4/lp_supply) (CPPool.lock_bps / locked_depth; = sqrt_k() while no third
        party holds LP), and the Raydium rules that read D apply (Market.locked_depth_venue): the sizing term
        D/(lambda_min*sqrt(P_ema)) (conservative_x) and the reference cap floor(D*1e4/lambda_min) (ref_depth_cap) --
        lambda_min on PumpSwap 100 % since OWNER-M6D-1, #106: below; during a PumpSwap warm-up L_slow is capped at D
        (observe), which undoes a deposit that raised the graduation's lp_supply anchor.  The spot leg at
        max(L_live, L_ema') (FITS-2 (a)) and the guard already applied to every venue; the keeper's per-second
        liquidity-anomaly coverage (FITS-2 (b)) is keeper-side (attacks/pumpswap_lp.py).  The example pools A / B / C
        carry no third-party LP (lambda = 1, D = sqrt_k), so no rule reading D binds on them; a CPPool marked
        non-canonical (attacks/fvcap.py) keeps the old reading;
      - M6-SEC-3: PumpCurve.create_pool_halted (PumpSwap's disable bit 0) stalls migrate_v2 and refuses curve opens
        (GraduationHalted).
  * OWNER-M6D-1 (the owner's decision of 2026-09-30, ECONOMICS.md changelog #106; ARCH §2.3.3, §4.6, §4.7; SPEC §14
    M6D-1): PumpSwap credits its burned LP only.  Every term that divides a canonical PumpSwap pool's D by lambda_min
    takes PUMPSWAP_LOCK_MIN_BPS = 10,000 (lambda_min = 100 %) instead of pool_lock_min_bps (Market.lock_min_bps):
    X0 = min(X_now, L_slow/sqrt(P_ema), D/sqrt(P_ema)) (conservative_x), and with it the per-position, market OI,
    fee-scaled, wallet and utilisation caps (the margin stays priced on the pool as it is, attacks/pumpswap_lp.py
    3 (f)); the program's tier bound R_eff <= max(floor(D*isqrt(P_ema)/1e9) - VQR+, 0) (the sim's tiers are fixed;
    shortwin-math's tier vectors transcribe it); and the exit reference's cap floor(D*1e4/lambda_min) = D
    (ref_depth_cap; the program's price start anchor too): on a canonical pool D <= L_live, so L_b = L_live there.
    A position sized on a third-party deposit held for about an hour (up to lambda/lambda_min = 2x the burned
    depth's cap before) gapped into S1 when the LP left at a genuine pump (M6D-1; attacks/pumpswap_lp.py part 3 (e));
    a reference cap kept at 2D, as #106 was first written, let a deposit held into a position's deadline and pulled
    there hold its expiry up for up to an hour (M6E-1, part 3 (f) (iv): the buyback on the drained pool 1.2 % above a
    reference at twice its depth, over delta_exp = 1 %).  Raydium keeps pool_lock_min_bps for its sizing (its
    reference cap is D too since AIR-14).
  * pump's v3 (pump-final, owner decisions of 2026-10-13 D1 (a) - D4; §12's sim work, P7): the curve adapter trades
    buy_v3 / sell_v3, so a live curve's buyback of q > rT is ONE buy_v3 by pump's synthetic migration
    (PumpCurve.buy): rT from the curve at floor(rT*vQ/(vT - rT)) + 1, the rest q2 from the would-be pool
    (X_s, Y_s) = (curve_base_ata - rT, rQ + n1 - pool_migration_fee) at ceil(Y_s*q2/(X_s - q2)), both parts at the
    curve's fees; the curve completes with pcB = q2, pcQ = n2 (PumpCurve.base_ata / pcb / pcq; rQ excludes pcQ).
    Params.synthetic (default True):
      - quote_at prices a live curve's q > rT as B_v3 (both legs at the curve's fee, eps1 = 1; X_s the live
        curve_base_ata - rT for the live state, live=True from Market.quote_spot and the live sigma of
        Market.slip_at (S2's live leg, the keeper's buyback gate), else 206.9M); B_str stays the quote of a
        migrating market's states (pump-final G-2) -- on the post-completion pool;
      - the pool migrate_v2 creates (PumpCurve.migrate) is (curve_base_ata, rQ + pcQ - fee) with the boost
        floor(X1*Y1/1e15) (CPPool.graduate; P1 correction (b)), and migrate_preview prices the pause on it after a
        pool part (the live curve_base_ata, donations included) and on the standard 206.9M without one;
      - fv_parts on a migrating curve reprices at p' = ceil(ceil(p*Y1/Y0)*X0/X1) after a pool part (rho; §5.4,
        Market._migrating_price), p itself without one;
      - the straddle bundle (_straddle_to_pool) runs only with synthetic=False: the retired path of ECON §10.2's
        comparison row; a keeper's or a borrower's exit of q > rT is the synthetic buy itself (_buy_back).
    Without a pool part and without a token donation (donate_tokens; no scenario has one) every output is
    bit-identical to v0.4.1's except where a live curve's q > rT used to be quoted B_str.
  * Scenario library, EMA / exit-bound lag study, loss-given-gap curves,
    break-even gap frequencies, an ILLUSTRATIVE (uncalibrated) jump-diffusion
    Monte Carlo, and a randomised exit fuzz with attacker bundles, attacker-only
    seconds and sandwiches that asserts the invariants of ECONOMICS.md §13 and
    now produces S2 and S3 exits.

Every protocol constant lives in `Params` / `Tier`.  Venue constants were read
from Solana mainnet on 2026-09-27 (ECONOMICS.md §16).  The frozen v0.3 model is
in ./legacy/v0_3/ (comparison rows only; not normative).
"""
from __future__ import annotations

import argparse
import copy
import math
import random
import sys
from dataclasses import dataclass, replace
from typing import Callable, Optional

# ----------------------------------------------------------------------------
# Units & fixed point (ECON §2 numerics table)
# ----------------------------------------------------------------------------
LAMPORTS_PER_SOL = 1_000_000_000
WAD = 10**18            # prices: lamports per raw token unit * 1e18; indices
BPS = 10_000
PPM = 1_000_000         # fee rates in ppm (pump bps x 100 exactly)
YEAR_S = 31_536_000     # 365 days
SQRT_WAD = 10**9        # sqrt(WAD)
INF = 10**40
P_ACC_MAX = 10**29      # rent accrual clamps P_ema here: J indices stay < 2^128 for 100 years (ECON §2.2)
P_OPEN_MAX = 10**27     # opens refused above this EMA price (1 SOL per raw unit)


def cdiv(a: int, b: int) -> int:
    """Ceiling division (b > 0)."""
    return -((-a) // b)


def fee_ceil(x: int, bps: int) -> int:
    return cdiv(x * bps, BPS)


def isqrt_ceil(n: int) -> int:
    r = math.isqrt(n)
    return r if r * r == n else r + 1


def sol(lamports: float) -> float:
    return lamports / LAMPORTS_PER_SOL


def reprice(L: int, p_wad: int):
    """Constant-product reserves (X, Y) with sqrt(X*Y) ~ L and Y/X >= p (WAD).
    R3B-7c: sp = ceil(sqrt(P)); X = floor(L*1e9/sp) <= true X; Y = ceil(L*sp/1e9)
    >= true Y, so the reprojected price is never below P and quotes on it are
    upper bounds (ECON §2 'quotes round up')."""
    if L <= 0 or p_wad <= 0:
        return None
    sp = isqrt_ceil(p_wad)
    X = L * SQRT_WAD // sp
    if X <= 0:
        return None
    return X, cdiv(L * sp, SQRT_WAD)


def lower_median(xs):
    s = sorted(xs)
    return s[(len(s) - 1) // 2]


def upper_median(xs):
    s = sorted(xs)
    return s[len(s) // 2]


# ----------------------------------------------------------------------------
# Venue constants (mainnet, 2026-09-27 -- ECONOMICS.md §1)
# ----------------------------------------------------------------------------
PUMP_VT0 = 1_073_000_000_000_000        # Global.initial_virtual_token_reserves
PUMP_VS0 = 30_000_000_000               # Global.initial_virtual_sol_reserves
PUMP_RT0 = 793_100_000_000_000          # Global.initial_real_token_reserves
PUMP_SUPPLY = 1_000_000_000_000_000     # Global.token_total_supply
PUMP_MIGRATION_FEE = 15_000_001         # Global.pool_migration_fee
PUMP_POOL_TOKENS = PUMP_SUPPLY - PUMP_RT0   # 206.9M tokens seeded into PumpSwap
# Pool.virtual_quote_reserves = floor(migrated_sol * 2069 / 10_000): exact on all 130
# non-mayhem migrations decoded 2026-09-25..27 (R-VEN §1.4); source closed (UNVERIFIED as a rule).
# Since pump-final P1 (correction (b), measured on pump's 2026-10-08 / 10-13 ELFs with and without
# donations) the rule is floor(X1*Y1/1e15) on the pool migrate_v2 creates (CPPool.graduate), which is
# this figure on the standard 206.9M tokens and more after a token donation to curve_base_ata.
BOOST_BPS = 2069
BOOST_CALLS = 40
BOOST_DURATION_S = 360


class FeeSchedule:
    """(lp_bps, protocol_bps, creator_bps) as a function of market cap (lamports)."""

    def __init__(self, rows):
        self.rows = sorted(rows)

    def fees(self, mcap_lamports: int):
        f = self.rows[0][1:]
        for th, lp, pr, cr in self.rows:
            if mcap_lamports >= th:
                f = (lp, pr, cr)
        return f


# pump FeeConfig 8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt: one tier, 0/95/30
PUMP_CURVE_FEES = FeeSchedule([(0, 0, 95, 30)])
# pump_amm FeeConfig 5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx fee_tiers
PUMPSWAP_CANONICAL_FEES = FeeSchedule([
    (0, 2, 93, 30), (420000000000, 20, 5, 95), (1470000000000, 20, 5, 90),
    (2460000000000, 20, 5, 85), (3440000000000, 20, 5, 80), (4420000000000, 20, 5, 75),
    (9820000000000, 20, 5, 70), (14740000000000, 20, 5, 65), (19650000000000, 20, 5, 60),
    (24560000000000, 20, 5, 55), (29470000000000, 20, 5, 50), (34380000000000, 20, 5, 45),
    (39300000000000, 20, 5, 40), (44210000000000, 20, 5, 35), (49120000000000, 20, 5, 30),
    (54030000000000, 20, 5, 28), (58940000000000, 20, 5, 25), (63860000000000, 20, 5, 23),
    (68770000000000, 20, 5, 20), (73681000000000, 20, 5, 18), (78590000000000, 20, 5, 15),
    (83500000000000, 20, 5, 13), (88400000000000, 20, 5, 10), (93330000000000, 20, 5, 8),
    (98240000000000, 20, 5, 5),
])


def flat_fees(lp: int, pr: int, cr: int) -> FeeSchedule:
    return FeeSchedule([(0, lp, pr, cr)])


# ----------------------------------------------------------------------------
# Venues
# ----------------------------------------------------------------------------
class Venue:
    kind = "abstract"
    plus_one = 0           # pump curve charges +1 lamport on exact-out buys
    lp_mutable = False     # can LPs add / withdraw liquidity (drain scenarios)?
    pump = False           # pump venue (venue-side rent tolerance ert applies)

    def eff(self):          # (token reserve X, SOL reserve Y) used for pricing
        raise NotImplementedError

    def fees(self):          # SOL-leg fee bps tuple (pump style)
        raise NotImplementedError

    def fee_oneway_ppm(self) -> int:
        """One-way total venue fee in ppm (pump bps x 100 exactly; Raydium rate).
        R3A-3 iii: carried in ppm so no rounding is needed anywhere."""
        return sum(self.fees()) * 100

    def fee_oneway_bps(self) -> int:
        return cdiv(self.fee_oneway_ppm(), 100)

    def has_route(self) -> bool:
        """venue_executable (ARCH §5.3)."""
        return True

    def has_price(self) -> bool:
        X, Y = self.eff()
        return X > 0 and Y > 0

    def spot_wad(self) -> int:
        X, Y = self.eff()
        return Y * WAD // X

    def sqrt_k(self) -> int:
        X, Y = self.eff()
        return math.isqrt(X * Y)

    def _risk_fees(self, cap_ppm):
        f = self.fees()
        if cap_ppm is not None and sum(f) * 100 > cap_ppm:
            return (cap_ppm // 100,)          # one combined fee at the hard cap (V2-1)
        return f

    def quote_buy_on(self, q: int, X: int, Y: int, cap_ppm: Optional[int] = None):
        """short.win's conservative exact-out quote on reserves (X, Y), fees on top
        of the SOL leg.  cap_ppm: risk quotes cap the total fee at the hard cap.
        Returns (net_ex_fees, total) lamports or None."""
        if q <= 0:
            return (0, 0)
        if q >= X:
            return None
        fees = self._risk_fees(cap_ppm)
        net = cdiv(Y * q, X - q) + self.plus_one
        return net, net + sum(fee_ceil(net, f) for f in fees)

    def quote_at(self, q: int, X: int, Y: int, two_leg: bool = True, cap_ppm: Optional[int] = None,
                 live: bool = False):
        """Executable quote on a hypothetical state (X, Y) of this venue (live: (X, Y) is the venue's own live
        state; only the pump curve reads it, PumpCurve.quote_at)."""
        return self.quote_buy_on(q, X, Y, cap_ppm)

    def move_to_price(self, p_wad: int) -> None:
        X, Y = self.eff()
        target_X = math.isqrt(X * Y * WAD // p_wad)
        if target_X < X:
            self.buy(X - target_X)
        elif target_X > X:
            self.sell(target_X - X)


class PumpCurve(Venue):
    kind = "curve"
    plus_one = 1
    pump = True
    # PumpSwap GlobalConfig.disable_flags bit 0 (create_pool; R-VEN Update H4): pump's migrate_v2 fails (6017), so a
    # completed curve stays migrating (the stuck-migration path, ECON §10.3), and since M6-SEC-3 (owner decision
    # 2026-09-30, #105) opens on a live curve are refused (GraduationHalted); read from the curve's ReadSet (C6)
    create_pool_halted = False
    # pump's v3 (pump-final §5.2, §5.3; the program's CurvePricing.synthetic): a live curve's buy of q > rT executes
    # as ONE buy_v3 by synthetic migration -- rT from the curve, which completes, and q - rT from the would-be pool
    # (curve_base_ata - rT, rQ + n_curve(rT) - pool_migration_fee), both parts at the curve's fees -- and is quoted
    # B_v3 (quote_at).  False (Params.synthetic, set by Market): the retired straddle bundle, quoted B_str, executed
    # by Market._straddle_to_pool (comparison rows of ECON §10.2 only).
    synthetic = True

    def __init__(self, vt: int, vs: int, rt: int, rs: int, sched: FeeSchedule = PUMP_CURVE_FEES,
                 base_ata: Optional[int] = None):
        self.vt, self.vs, self.rt, self.rs = vt, vs, rt, rs
        self.off_t, self.off_s = vt - rt, vs - rs      # constant per curve [ECON §1.1]
        self.sched = sched
        self.complete = rt == 0
        # curve_base_ata: the tokens the curve holds -- rT and the 206.9M migrate_v2 seeds, plus any token donation
        # (donate_tokens); a synthetic buy prices its pool part on curve_base_ata - rT and migrate_v2 seeds the pool
        # with what it holds then (pump-final V3, §5.4)
        self.base_ata = rt + PUMP_POOL_TOKENS if base_ata is None else base_ata
        # BondingCurve.post_complete_base_out / post_complete_quote_in (@150 / @158; pcB, pcQ): written by the buy
        # that completes the curve with a pool part; rQ excludes pcQ (pump-final V5)
        self.pcb = self.pcq = 0

    @classmethod
    def at_progress(cls, frac: float) -> "PumpCurve":
        """Curve after `frac` of the initial real token reserves were bought."""
        sold = int(PUMP_RT0 * frac)
        vt = PUMP_VT0 - sold
        vs = cdiv(PUMP_VT0 * PUMP_VS0, vt)
        return cls(vt, vs, PUMP_RT0 - sold, vs - PUMP_VS0)

    def eff(self):
        return self.vt, self.vs

    def fees(self):
        mcap = self.vs * PUMP_SUPPLY // self.vt
        return self.sched.fees(mcap)

    def has_route(self):
        return not self.complete

    def progress(self) -> float:
        return 1 - self.rt / PUMP_RT0

    # exact venue execution (mirrors pump SDK / program rounding)
    def buy(self, q: int) -> int:
        """pump's exact-out buy of q: floor(q*vQ/(vT - q)) + 1 on the curve, each fee ceil'ed on top.  q > rT is
        pump's v3 synthetic migration (pump-final §5.2, V3; T-PUMP-MATH-V3 (b)): the curve part rT at n1 =
        floor(rT*vQ/(vT - rT)) + 1 (v3's ceil, which it takes as floor + 1 on an exact division too), then the
        pool part q2 = q - rT on the would-be pool (X_s, Y_s) = (curve_base_ata - rT, rQ + n1 - pool_migration_fee)
        at n2 = ceil(Y_s*q2/(X_s - q2)) (NotEnoughTokensToBuy unless q2 < X_s), both parts at the curve's fees at its
        pre-trade market cap; the curve completes with pcB = q2, pcQ = n2, rQ = rQ0 + n1 (rQ excludes pcQ)."""
        assert not self.complete and q > 0, (q, self.rt, self.complete)
        if q > self.rt:
            return self._buy_synthetic(q)
        net = q * self.vs // (self.vt - q) + 1
        cost = net + sum(fee_ceil(net, f) for f in self.fees())
        self.vs += net
        self.vt -= q
        self.rt -= q
        self.rs += net
        self.base_ata -= q
        if self.rt == 0:
            self.complete = True
        return cost

    def _buy_synthetic(self, q: int) -> int:
        fees = self.fees()
        rt = self.rt
        n1 = rt * self.vs // (self.vt - rt) + 1 if rt > 0 else 0
        x_s = self.base_ata - rt
        q2 = q - rt
        if q2 >= x_s:
            raise ValueError("NotEnoughTokensToBuy")
        y_s = self.rs + n1 - PUMP_MIGRATION_FEE
        n2 = cdiv(y_s * q2, x_s - q2)
        cost = sum(n + sum(fee_ceil(n, f) for f in fees) for n in ((n1, n2) if rt > 0 else (n2,)))
        self.vs += n1
        self.vt -= rt
        self.rs += n1
        self.rt = 0
        self.base_ata -= q
        self.pcb, self.pcq = q2, n2
        self.complete = True
        return cost

    def sell(self, q: int) -> int:
        assert not self.complete and q > 0
        fees = self.fees()
        gross = q * self.vs // (self.vt + q)
        out = gross - sum(fee_ceil(gross, f) for f in fees)
        self.vs -= gross
        self.vt += q
        self.rt += q
        self.rs -= gross
        self.base_ata += q
        return out

    def donate_tokens(self, d: int) -> None:
        """A token transfer to curve_base_ata (anyone may send one): a live curve's synthetic buy prices its pool
        part on it, and migrate_v2 seeds it; FV prices it only after a pool part (pump-final §5.4, §6.8)."""
        self.base_ata += d

    def quote_at(self, q: int, X: int, Y: int, two_leg: bool = True, cap_ppm: Optional[int] = None,
                 live: bool = False):
        """Quote on a hypothetical curve state (vt = X, vs = Y).  If q exceeds that state's real token
        reserves rT = X - off_t:
          * a live curve (synthetic, pump-final §5.2): B_v3, what pump's buy_v3 executes -- rT on the curve,
            then q - rT on the would-be pool (X_s, Y_s) = (pool tokens, rQ + n_curve(rT) - pool_migration_fee),
            BOTH legs at the curve's fees (capped in the risk view) with eps1 = 1 (quote_buy_on); X_s is the live
            curve_base_ata - rT for the live state (live=True: Market.quote_spot, the live sigma of Market.slip_at)
            and the standard 206.9M on every hypothetical one (the EMA leg, the box, B_ref; a live curve carries no
            pool part, pump-final G-3);
          * a migrating curve's state (complete; the EMA leg's ring entries reproject to live-looking curves,
            pump-final G-2), or synthetic=False (the retired bundle, comparison rows only): the straddle quote B_str,
            ALL of q on the pool migrate_v2 will create from that state, at its PumpSwap tier (V1-9) -- the
            post-completion pool (§5.4: the live curve_base_ata after a pool part, + pcQ) on a migrating curve.
        The state's rQ = Y - off_s saturates at 0, as the program's curve_reprice (a price below the curve's
        start; B_str's q > rT is then above the pool's 206.9M anyway).  two_leg=False = 'no straddle quote'
        (analysis rows only)."""
        if q <= 0:
            return (0, 0)
        rt = max(X - self.off_t, 0)
        if q <= rt:
            return self.quote_buy_on(q, X, Y, cap_ppm)
        if not two_leg:
            return None
        rq = max(Y - self.off_s, 0)
        if self.synthetic and not self.complete:
            leg1 = self.quote_buy_on(rt, X, Y, cap_ppm) if rt > 0 else (0, 0)
            if leg1 is None:
                return None
            x_s = self.base_ata - self.rt if live else PUMP_POOL_TOKENS
            y_s = rq + leg1[0] - PUMP_MIGRATION_FEE
            if y_s <= 0:
                return None
            leg2 = self.quote_buy_on(q - rt, x_s, y_s, cap_ppm)
            if leg2 is None:
                return None
            return leg1[0] + leg2[0], leg1[1] + leg2[1]
        leg1 = self.quote_buy_on(rt, X, Y) if rt > 0 else (0, 0)
        tokens, pcq = (self.pool_tokens(), self.pcq) if self.complete else (PUMP_POOL_TOKENS, 0)
        migrated = rq + leg1[0] - PUMP_MIGRATION_FEE + pcq
        pool = CPPool.graduate(migrated, tokens)
        return pool.quote_buy_on(q, *pool.eff(), cap_ppm=cap_ppm)

    def pool_tokens(self) -> int:
        """The tokens §5.4 prices a migrating curve's pool on (pump-final §5.4, R-2): the live curve_base_ata after a
        pool part (what migrate_v2 seeds, donations included), else the deterministic 206.9M (a donation ignored,
        ARCH §4.3)."""
        return self.base_ata if (self.pcb or self.pcq) else PUMP_POOL_TOKENS

    def migrate_preview(self) -> "CPPool":
        """The pool migrate_v2 will create, as short.win prices the pause (pump-final §5.4): (pool_tokens(),
        rQ - pool_migration_fee + pcQ)."""
        return CPPool.graduate(self.rs - PUMP_MIGRATION_FEE + self.pcq, self.pool_tokens())

    def migrate(self) -> "CPPool":
        """pump's migrate_v2: the canonical pool seeded with the live curve_base_ata and rQ + pcQ - fee (pump-final
        V5, T-PUMP-MATH-V3 (d)); = migrate_preview() unless tokens were donated to a curve completed without a pool
        part."""
        assert self.complete
        return CPPool.graduate(self.rs - PUMP_MIGRATION_FEE + self.pcq, self.base_ata)

    def move_to_price(self, p_wad: int) -> None:
        """Exogenous traders move the curve toward p_wad (stops at completion)."""
        if self.complete:
            return
        k = self.vt * self.vs
        target_vt = math.isqrt(k * WAD // p_wad)
        if target_vt < self.vt:
            q = min(self.vt - target_vt, self.rt)
            if q > 0:
                self.buy(q)
        elif target_vt > self.vt:
            self.sell(target_vt - self.vt)


class CPPool(Venue):
    """PumpSwap pool: fees on the SOL leg, effective quote = real + virtual."""
    kind = "amm"
    pump = True

    def __init__(self, X: int, Y: int, sched: FeeSchedule, supply: int = PUMP_SUPPLY, vq: int = 0,
                 boost_vault: int = 0, canonical: bool = True):
        self.X, self.Y, self.sched, self.supply, self.vq = X, Y, sched, supply, vq
        self.disabled = False
        self.canonical = canonical          # created by migrate_v2: the only PumpSwap pools short.win lists
        # lp_mutable: the scenario library's drain model (a pool marked non-canonical: the "LP lock violated" model
        # case of attacks/fvcap.py); canonical pools take third-party LP (R-VEN Update H11) through add_liquidity /
        # remove_liquidity below, but their creation LP is burned, and the example pools A / B / C carry none
        self.lp_mutable = not canonical
        # LP accounting (M6R-1, ECONOMICS.md #105; R-VEN Update H11): lp_units = Pool.lp_supply (@203, the pool's own
        # counter: isqrt(X*(Y + VQR)) at creation, the graduation anchor), lp_locked = the burned LP
        # (Pool.lp_supply - lp_mint.supply): all of it on a canonical pool until a third party deposits
        self.lp_units = math.isqrt(X * (Y + vq))
        self.lp_locked = self.lp_units if canonical else 0
        self.boost_vault = boost_vault
        self.boost_next: Optional[int] = None
        self.boost_chunk = 0
        self.boost_interval = 1

    @classmethod
    def graduate(cls, migrated: int, tokens: int = PUMP_POOL_TOKENS) -> "CPPool":
        """Canonical pool right after migrate_v2: `tokens` (206.9M on a standard curve; the live curve_base_ata,
        pump-final §5.4) and `migrated` lamports of effective quote, of which the boost floor(tokens*migrated/1e15)
        is booked as virtual_quote_reserves (pump-final P1 correction (b), shortwin_math::quote::migration_boost;
        = floor(migrated*2069/1e4), BOOST_BPS, on the standard 206.9M)."""
        vq = tokens * migrated // PUMP_SUPPLY
        return cls(tokens, migrated - vq, PUMPSWAP_CANONICAL_FEES, PUMP_SUPPLY, vq=vq,
                   boost_vault=vq, canonical=True)

    def start_boost(self, t: int) -> None:
        if self.boost_vault > 0:
            self.boost_chunk = cdiv(self.boost_vault, BOOST_CALLS)
            self.boost_interval = max(1, BOOST_DURATION_S // BOOST_CALLS)
            self.boost_next = t + 1

    def boost_tick(self, t: int) -> None:
        """Pump's boost authority: zero-fee buy with boost-vault SOL, tokens burned.  The ELF's rounding (M6 as built,
        ECONOMICS.md #104 (3), #105; graduation_scenarios::the_elf_prices_a_boost_chunk_at_one_lamport_less): a chunk of
        b lamports moves b into the WSOL vault and burns floor(X*(b - 1)/(Y + b - 1)) tokens on the effective reserves
        (v0.4.1: floor(X*b/(Y + b))); VQR is unchanged."""
        while self.boost_next is not None and t >= self.boost_next and self.boost_vault > 0:
            b = min(self.boost_chunk, self.boost_vault)
            X, Y = self.eff()
            out = X * (b - 1) // (Y + b - 1)
            self.X -= out
            self.Y += b
            self.supply -= out
            self.boost_vault -= b
            self.boost_next += self.boost_interval
        if self.boost_vault == 0:
            self.boost_next = None

    def run_boost_to_end(self) -> None:
        self.start_boost(0)
        t = 0
        while self.boost_vault > 0:
            t += self.boost_interval
            self.boost_tick(t)

    def eff(self):
        return self.X, self.Y + self.vq

    def fees(self):
        X, Y = self.eff()
        return self.sched.fees(Y * self.supply // X)

    def has_route(self):
        return not self.disabled and self.X > 0 and self.Y > 0

    def lock_bps(self) -> int:
        """lambda_bps = floor((Pool.lp_supply - lp_mint.supply)*1e4/Pool.lp_supply) (M6R-1, #105): the Raydium formula
        of ARCH §2.6 on the canonical pool's own LP counter and its LP mint; 10,000 while no third party holds LP."""
        return self.lp_locked * BPS // self.lp_units if self.lp_units else 0

    def locked_depth(self) -> int:
        """D = floor(lambda_bps*isqrt(X*(Y + VQR))/1e4) (M6R-1, #105; ARCH §2.6's metric on the pricing reserves): the
        depth of the burned LP.  A third-party deposit or withdrawal scales X, Y, VQR and lp_supply together (R-VEN
        Update H11), so it leaves D unchanged up to the whole-bps floor of lambda; a swap moves it only by the LP fee
        the pool keeps, the boost not at all (up to rounding in the pool's favour).  = sqrt_k() while every LP unit is
        burned (the pre-#105 reading of a canonical pool)."""
        return self.lock_bps() * self.sqrt_k() // BPS

    def add_liquidity(self, frac_bps: int) -> None:
        """A third-party PumpSwap deposit of frac_bps of the pool (R-VEN Update H11: accepted on canonical pools, boost
        vault funded or not): both vaults, VQR (floor(VQR*base_in/base)) and lp_supply scale; the minted LP circulates
        (unlocked)."""
        self.X += self.X * frac_bps // BPS
        self.Y += self.Y * frac_bps // BPS
        self.vq += self.vq * frac_bps // BPS
        self.lp_units += self.lp_units * frac_bps // BPS

    def buy(self, b: int) -> int:
        X, Y = self.eff()
        assert 0 < b < X
        lp, pr, cr = self.fees()
        qin = cdiv(Y * b, X - b)
        lpf = fee_ceil(qin, lp)
        total = qin + lpf + fee_ceil(qin, pr) + fee_ceil(qin, cr)
        self.X -= b
        self.Y += qin + lpf
        return total

    def sell(self, b: int) -> int:
        X, Y = self.eff()
        lp, pr, cr = self.fees()
        qout = Y * b // (X + b)
        lpf = fee_ceil(qout, lp)
        if qout - lpf > self.Y:
            raise ValueError("InsufficientRealQuoteReserves")
        out = qout - lpf - fee_ceil(qout, pr) - fee_ceil(qout, cr)
        self.X += b
        self.Y -= qout - lpf
        return out

    def move_to_price(self, p_wad: int) -> None:
        """Exogenous traders; sells stop at the real-reserve floor VQR/X."""
        X, Y = self.eff()
        target_X = math.isqrt(X * Y * WAD // p_wad)
        if target_X < X:
            self.buy(X - target_X)
        elif target_X > X:
            b = target_X - X
            if self.vq > 0:
                b = min(b, X * self.Y // self.vq * 99 // 100)
            if b > 0:
                self.sell(b)

    def remove_liquidity(self, frac_bps: int, force: bool = False) -> None:
        """A withdrawal of frac_bps of the pool's LP.  A pool marked non-canonical (lp_mutable: the scenario library's
        drains, attacks/fvcap.py's "LP lock violated" model case) keeps the v0.4.1 model: both reserves fall by
        frac_bps, nothing else is tracked.  A canonical pool (R-VEN Update H11): only unlocked (third-party) LP can be
        withdrawn -- both vaults, VQR and lp_supply scale down; the burned creation LP stays."""
        if self.lp_mutable:
            self.X -= self.X * frac_bps // BPS
            self.Y -= self.Y * frac_bps // BPS
            return
        burn = self.lp_units * frac_bps // BPS
        if self.lp_units - burn < self.lp_locked:
            raise ValueError("canonical pool: the burned creation LP cannot be withdrawn")
        self.X -= self.X * frac_bps // BPS
        self.Y -= self.Y * frac_bps // BPS
        self.vq -= self.vq * frac_bps // BPS
        self.lp_units -= burn


class RaydiumPool(Venue):
    """Raydium AMM v4 / CPMM: constant product with the fee on the INPUT amount
    (D2; exact per-venue forms in ARCH §4.3).  LP units: lp_units ~ sqrt(k) at
    creation, lp_locked = the permanently locked (burned) part; lambda =
    lp_locked/lp_units; locked depth D = floor(lambda_bps*isqrt(X*Y)/1e4)
    (R3A-12).  Unlocked LP can be withdrawn only down to the locked part unless
    force=True (a model case outside what an adopted pool allows)."""
    kind = "ray"
    lp_mutable = True

    def __init__(self, X: int, Y: int, fee_ppm: int = 2500, out_share_ppm: int = 120_000,
                 creator_ppm: int = 0, supply: int = PUMP_SUPPLY, name: str = "AMMv4",
                 lock_bps: int = 5000):
        self.X, self.Y = X, Y
        self.fee_ppm, self.out_share_ppm, self.creator_ppm = fee_ppm, out_share_ppm, creator_ppm
        self.supply, self.name = supply, name
        self.disabled = False
        self.lp_units = math.isqrt(X * Y)
        self.lp_locked = self.lp_units * lock_bps // BPS
        self.lock_broken = False

    def eff(self):
        return self.X, self.Y

    def fees(self):
        return (cdiv(self.fee_ppm + self.creator_ppm, 100),)

    def fee_oneway_ppm(self) -> int:
        return self.fee_ppm + self.creator_ppm

    def has_route(self):
        return not self.disabled and self.X > 0 and self.Y > 0

    def lock_bps(self) -> int:
        return self.lp_locked * BPS // self.lp_units if self.lp_units else 0

    def locked_depth(self) -> int:
        return self.lock_bps() * self.sqrt_k() // BPS

    def _rate(self, cap_ppm):
        r = self.fee_ppm + self.creator_ppm
        return min(r, cap_ppm) if cap_ppm is not None else r

    def _gross(self, net: int, cap_ppm=None) -> int:
        return cdiv(net * PPM, PPM - self._rate(cap_ppm))

    def _keep(self, fee: int) -> int:
        """Part of a fee that stays in the pricing reserves (LP share)."""
        tot = self.fee_ppm + self.creator_ppm
        cre = fee * self.creator_ppm // tot if tot else 0
        trade = fee - cre
        return trade - trade * self.out_share_ppm // PPM

    def quote_buy_on(self, q: int, X: int, Y: int, cap_ppm: Optional[int] = None):
        if q <= 0:
            return (0, 0)
        if q >= X:
            return None
        net = cdiv(Y * q, X - q)
        return net, self._gross(net, cap_ppm)

    def buy(self, q: int) -> int:
        net, total = self.quote_buy_on(q, self.X, self.Y)
        self.X -= q
        self.Y += net + self._keep(total - net)
        return total

    def sell(self, q: int) -> int:
        fee_t = cdiv(q * (self.fee_ppm + self.creator_ppm), PPM)
        qn = q - fee_t
        out = self.Y * qn // (self.X + qn)
        self.X += qn + self._keep(fee_t)
        self.Y -= out
        return out

    def remove_liquidity(self, frac_bps: int, force: bool = False) -> None:
        burn = self.lp_units * frac_bps // BPS
        if self.lp_units - burn < self.lp_locked:
            if not force:
                raise ValueError("cannot withdraw locked LP")
            self.lock_broken = True                  # model case: lock violated / non-adopted pool
            self.lp_locked = self.lp_units - burn
        self.X -= self.X * frac_bps // BPS
        self.Y -= self.Y * frac_bps // BPS
        self.lp_units -= burn

    def add_liquidity(self, frac_bps: int) -> None:
        self.X += self.X * frac_bps // BPS
        self.Y += self.Y * frac_bps // BPS
        self.lp_units += self.lp_units * frac_bps // BPS


# ----------------------------------------------------------------------------
# Protocol parameters (ECON §15; compiled bounds there)
# ----------------------------------------------------------------------------
@dataclass(frozen=True)
class Tier:
    name: str
    m_bps: int                 # spec "initial margin (of notional)" (informational since D4)
    cr_open_bps: int           # spec "opening CR" (enforced, D4)
    cr_liq_bps: int            # spec "liquidation CR"
    penalty_bps: int           # spec "liq. penalty"
    max_dur_s: int             # spec "max duration"
    impact_pos_bps: int        # spec per-position impact cap
    impact_mkt_bps: int        # spec per-market impact cap
    max_liq_slip_bps: int      # spec S2 threshold
    rate_base_bps: int         # spec §6.1 base
    rate_s1_bps: int           # derived: (rate@80% - base)/0.8
    rate_s2_bps: int           # derived: (rate@100% - rate@80%)/0.2
    min_rent_s: int            # spec: 1h (D: 4h)
    ema_tau_s: int             # compile-time constant EMA_TAU_S = 5 in v0.4 (analysis rows vary it)
    ema_dtmax_s: int           # PROPOSED (market-level)
    ema_clamp_bps: int         # PROPOSED (market-level)


TIERS = {
    #          m      CRopen  Lliq   pen  max duration    ipos imkt  S2   base   slope1   slope2  minrent  tau dtmax clamp
    "A": Tier("A", 10_000, 20_000, 12_500, 500, 30 * 86400, 500, 2000, 2000, 1000, 5000, 100_000, 3600, 5, 2, 10_000),
    "B": Tier("B", 15_000, 25_000, 13_500, 700, 14 * 86400, 500, 2000, 2000, 2500, 9375, 150_000, 3600, 5, 2, 10_000),
    "C": Tier("C", 20_000, 30_000, 15_000, 800, 7 * 86400, 500, 2000, 2000, 5000, 18_750, 300_000, 3600, 5, 2, 10_000),
    "D": Tier("D", 30_000, 40_000, 17_500, 1000, 72 * 3600, 300, 1000, 3000, 10_000, 37_500, 550_000, 4 * 3600, 5, 2, 10_000),
}


@dataclass(frozen=True)
class Params:
    # ---- SPEC -------------------------------------------------------------------
    open_fee_bps: int = 25                 # 0.25 % of notional
    rent_protocol_bps: int = 1500          # 85/15 (snapshot per position, R3A-5/R3B-9)
    pen_lender_bps: int = 5000             # 50/20/30
    pen_protocol_bps: int = 2000
    pen_keeper_bps: int = 3000
    expiry_fee_bps: int = 200              # 2 % (base: quoted reference cost)
    settle_fixed: int = 10_000_000         # 0.01 SOL
    settle_bps: int = 100                  # 1 % of vault
    settle_cap: int = 250_000_000          # ADOPTED Q4: 0.25 SOL
    util_kink_bps: int = 8000
    util_cap_bps: int = 9000
    wallet_cap_bps: int = 2500             # ADOPTED Q10
    s3_wait_s: int = 600
    grad_grace_s: int = 600
    # ---- PROPOSED ------------------------------------------------------------------
    curve_oi_gamma_bps: int = 5000         # Tier D: sum(Q) <= 50 % of real_token_reserves
    max_migration_pause_s: int = 3600      # ADOPTED Q5
    migration_delay_s: int = 2             # observed ~6 slots
    open_ema_dev_bps: int = 1000
    fee_size_cap_x100: int = 300           # per position q/X <= 3.00 x one-way venue fee
    fee_oi_cap_x100: int = 750             # market sum(Q)/X <= 7.50 x one-way venue fee
    pool_lock_min_bps: int = 5000          # lambda_min: sizing and the reference cap on locked depth / lambda_min
                                           #   (Raydium; PumpSwap takes PUMPSWAP_LOCK_MIN_BPS in both, #106, M6E-1)
    venue_fee_cap_bps: int = 150           # opens refused above (not exercised by the sim's opens)
    venue_fee_hard_cap_bps: int = 500      # risk quotes and FV use at most this fee; above = S3 evidence
    # ---- D3 -------------------------------------------------------------------------
    fv_cap: bool = True                    # S2/S3 only; S1 never capped (SPEC)
    fv_dev_bps: int = 100                  # settlement box: amounts must agree within 1 % (R3A-1, V2-9)
    liq_slow_tau_s: int = 3600
    liq_slow_dtmax_s: int = 60
    # ---- observation, EMA ring, persistence ---------------------------------------
    ring_len: int = 8
    ref_max_age_s: int = 2                 # last completed bucket must be <= 2 s old
    liq_persist_n: int = 3                 # newest 3 ring entries below L (no seconds condition, v0.4)
    settle_persist_n: int = 40             # 40 EMA updates on one eligibility clock (>= 39 s, v0.4)
    settle_arm_gap_s: int = 10             # evaluations > 10 s apart re-arm
    # ---- exit bounds ------------------------------------------------------------------
    exit_ref: str = "fresh"                # liquidation reference "fresh" | "ema" (analysis row only)
    kappa_liq_bps: int = 2500              # fresh reference capped at P_ema*(1 + kappa_eff),
    kappa_base_bps: int = 0                #   kappa_eff = min(kappa, kappa_base + (tau+1)*g)
    kappa_growth_n: int = 3                #   g = lower-median per-second EMA growth (bps/s) over 3 steps (AIR-22)
    delta_liq_bps: int = 200               # FIXED liquidation bound over the reference (v0.4; was Dutch 1->5 %)
    delta_exp_bps: int = 100               # expiry bound over min(EMA, ring min, last bucket low)
    exec_tol_bps: int = 10                 # epsilon: venue rounding drift
    exec_rent_tol: int = 6_500_000         # ert on pump venues (0 on Raydium, R3A-3 i)
    expiry_keeper_fixed: int = 1_000_000   # K_e = min(0.001 SOL + 0.1 % B_ref, 0.05 SOL)
    expiry_keeper_bps: int = 10
    expiry_keeper_cap: int = 50_000_000
    liq_keeper_parity: bool = True         # R3B-10: liquidation keeper reward >= K_s (borrower pays top-up)
    # ---- S3 -------------------------------------------------------------------------
    s3_obs_gap_s: int = 60                 # no-route observations > 60 s apart restart the S3 timer
    pool_fast_route_s: int = 120           # Raydium fast path: pool_fail continuously for 120 s ...
    pool_fast_window_s: int = 60           # ... then a 60 s collection window
    # ---- analysis-only switches (not protocol parameters) ------------------------------
    synthetic: bool = True                 # pump's v3 (pump-final §5.2): a live curve's q > rT is one buy_v3 by
                                           #   synthetic migration, quoted B_v3 (PumpCurve.quote_at / buy);
                                           #   False: the retired straddle bundle, B_str (comparison rows, §10.2)
    two_leg: bool = True                   # False: 'no straddle quote' rows (§10.2)
    box_obs: bool = False                  # comparison rows only: the v0.4 box (medians of clamped obs)
    pprime_raw: bool = False               # comparison rows only: the v0.4 p' (raw CR-liquidatable flag)
    ref_cap_lo: bool = True                # v0.4.1 (R4B-2): liquidation P_ref also <= lo_x*(1+kappa_eff), so a
                                           #   pushed-and-frozen P_ema cannot lift the bound (False: v0.4 row)
    liq_reserve: str = "none"              # liquidation max_in reserve (v0.4.1, R4A-1 / R4B-1): "none" (SPEC §5.2:
                                           #   buy back iff B <= min(bound, V - K_s - ert)); comparison rows:
                                           #   "rent" (v0.3 rule), "rent+penalty" (v0.4 #50, withdrawn)
    exp_reserve: str = "rent+fee"          # expiry max_in reserve (nothing is urgent): rent and expiry fee senior
    margin_policy: str = "cr"              # "notional" = m x N_open (static table only)
    check_invariants: bool = True


DEFAULT = Params()

# Compiled hard bounds of the simulated parameters (ECON §15; EMA_TAU_S = 5 is a constant). The penalty
# split's lender part is bounded by the sum check (with both other parts in [0, 5,000] it lies in [0, 10,000]).
COMPILED_BOUNDS = {
    "open_fee_bps": (0, 100), "rent_protocol_bps": (0, 3000), "pen_protocol_bps": (0, 5000),
    "pen_keeper_bps": (0, 5000), "expiry_fee_bps": (0, 500),
    "settle_fixed": (0, 10_000_000), "settle_bps": (0, 100), "settle_cap": (0, 250_000_000),
    "util_kink_bps": (0, 9500), "util_cap_bps": (0, 9500), "wallet_cap_bps": (500, 10_000),
    "s3_wait_s": (600, 86_400), "grad_grace_s": (600, 86_400), "curve_oi_gamma_bps": (1000, 9000),
    "max_migration_pause_s": (600, 86_400), "open_ema_dev_bps": (100, 5000),
    "fee_size_cap_x100": (100, 500), "fee_oi_cap_x100": (300, 1000), "pool_lock_min_bps": (2000, 10_000),
    "venue_fee_cap_bps": (25, 300), "venue_fee_hard_cap_bps": (300, 2000), "fv_dev_bps": (50, 200),
    "liq_slow_tau_s": (600, 86_400), "liq_slow_dtmax_s": (1, 600), "ref_max_age_s": (1, 3),
    "liq_persist_n": (2, 3), "settle_persist_n": (40, 600), "settle_arm_gap_s": (2, 60),
    "kappa_liq_bps": (2500, 3000), "kappa_base_bps": (0, 1000), "kappa_growth_n": (3, 4),
    "delta_liq_bps": (200, 500), "delta_exp_bps": (50, 300), "exec_tol_bps": (0, 100),
    "exec_rent_tol": (6_000_000, 10_000_000), "expiry_keeper_fixed": (0, 10_000_000),
    "expiry_keeper_bps": (0, 100), "expiry_keeper_cap": (0, 250_000_000),
    "pool_fast_route_s": (30, 420), "pool_fast_window_s": (10, 120),
}
# Compiled hard bounds of the per-tier parameters (ECON §15), checked for every tier. The EMA fields are
# market-level in the program (ARCH §2.3.3: one value for all tiers); cr_open_bps's lower bounds and cr_liq_bps's
# bounds are the relational checks of validate_params (cr_open >= cr_liq + 2,000, cr_liq >= 10,500, SPEC_CR);
# m_bps is informational (unbounded).
TIER_BOUNDS = {"ema_dtmax_s": (1, 2), "ema_clamp_bps": (5000, 10_000), "ema_tau_s": (5, 5),
               "cr_open_bps": (0, 100_000), "penalty_bps": (0, 2000), "max_dur_s": (3600, 90 * 86400),
               "impact_pos_bps": (0, 1000), "impact_mkt_bps": (0, 5000), "max_liq_slip_bps": (500, 5000),
               "rate_base_bps": (0, 50_000), "rate_s1_bps": (0, 1_000_000), "rate_s2_bps": (0, 1_000_000),
               "min_rent_s": (0, 86_400)}
# v0.4.1 (R4A-4): the SPEC §4 opening and liquidation CRs are per-tier compiled MINIMA (governance may
# only raise them: every lower value loses liquidations in the §5.7 battery)
SPEC_CR = {"A": (20_000, 12_500), "B": (25_000, 13_500), "C": (30_000, 15_000), "D": (40_000, 17_500)}


def validate_params(P: Params = DEFAULT, tiers=None) -> list:
    """ECON §15 compiled bounds and relational checks of every parameter this model has (the program's
    shortwin_math::params::validate, ARCH §2.3.3); returns the list of violations."""
    bad = []
    for k, (lo, hi) in COMPILED_BOUNDS.items():
        v = getattr(P, k)
        if not lo <= v <= hi:
            bad.append(f"{k}={v} outside [{lo}, {hi}]")
    for T in (tiers or TIERS).values():
        for k, (lo, hi) in TIER_BOUNDS.items():
            v = getattr(T, k)
            if not lo <= v <= hi:
                bad.append(f"tier {T.name} {k}={v} outside [{lo}, {hi}]")
        if T.cr_open_bps < T.cr_liq_bps + 2000 or T.cr_liq_bps < 10_500:
            bad.append(f"tier {T.name}: cr_open >= cr_liq + 2,000 and cr_liq >= 10,500 required")
        if T.name in SPEC_CR and (T.cr_open_bps < SPEC_CR[T.name][0] or T.cr_liq_bps < SPEC_CR[T.name][1]):
            bad.append(f"tier {T.name}: cr_open >= {SPEC_CR[T.name][0]:,} and cr_liq >= {SPEC_CR[T.name][1]:,} (SPEC minima)")
    rel = [(P.util_kink_bps <= P.util_cap_bps, "util_kink_bps <= util_cap_bps"),
           (P.pen_lender_bps + P.pen_protocol_bps + P.pen_keeper_bps == BPS, "penalty split sums to 10,000"),
           (P.kappa_base_bps <= P.kappa_liq_bps, "kappa_base_bps <= kappa_liq_bps"),
           (P.delta_liq_bps >= P.delta_exp_bps, "delta_liq_bps >= delta_exp_bps"),
           (P.fee_size_cap_x100 <= P.fee_oi_cap_x100, "fee_size_cap_x100 <= fee_oi_cap_x100"),
           (P.pool_fast_route_s + P.pool_fast_window_s <= 540, "pool_fast_route_s + pool_fast_window_s <= 540"),
           (P.venue_fee_cap_bps < P.venue_fee_hard_cap_bps, "venue_fee_cap_bps < venue_fee_hard_cap_bps"),
           (P.settle_fixed <= P.settle_cap, "settle_fixed <= settle_cap"),
           (P.expiry_keeper_fixed <= P.expiry_keeper_cap, "expiry_keeper_fixed <= expiry_keeper_cap"),
           (P.ring_len >= max(P.liq_persist_n, P.kappa_growth_n + 1), "ring_len >= max(liq_persist_n, kappa_growth_n + 1)")]
    bad += [msg for ok, msg in rel if not ok]
    return bad


def rate_bps(tier: Tier, util_bps: int, kink: int = 8000) -> int:
    """Display only (the program integrates the kinked curve through basis indices)."""
    return (tier.rate_base_bps + tier.rate_s1_bps * min(util_bps, kink) // BPS
            + tier.rate_s2_bps * max(util_bps - kink, 0) // BPS)


def max_q_impact(X: int, impact_bps: int) -> int:
    """Largest Q whose buyback from (X+Q) moves marginal price by <= impact."""
    return math.isqrt(X * X * (BPS + impact_bps) // BPS) - X


def max_total_q_market(X_now: int, impact_bps: int) -> int:
    """Largest total outstanding Q whose buyback moves marginal price by <= impact."""
    return X_now - math.isqrt(X_now * X_now * BPS // (BPS + impact_bps))


# ----------------------------------------------------------------------------
# Raydium pool selection as built after M4 (ECON §5.2, §6.2, §10.1; ARCH §2.6; shortwin_math::pool_select)
# ----------------------------------------------------------------------------
# pool_min_real_sol (ECON §15) is a params::validate_econ parameter: it is deliberately NOT a Params field,
# so validate_params and the math crate's params vectors are unchanged.
POOL_MIN_REAL_SOL = 25_000_000_000


def locked_sol(D: int, p_wad: int) -> int:
    """floor(D*isqrt(P)/1e9): the SOL side of the locked depth at price P (ECON §2.1; tier::locked_sol)."""
    return D * math.isqrt(p_wad) // SQRT_WAD


def depth_at_lambda(D: int, lam_bps: int) -> int:
    """floor(D*1e4/lambda): the sqrt(X*Y) a pool of locked depth D has at locked share lambda; 0 for lambda = 0.
    With lambda_min it is the most depth sizing ever credits the pool with (the adoption cap)."""
    return D * BPS // lam_bps if lam_bps else 0


# OWNER-M6D-1 (the owner's decision of 2026-09-30, ECONOMICS.md changelog #106; compiled, u32 bps; ARCH §2.3.3
# PUMPSWAP_LOCK_MIN_BPS): the lambda_min of every term that divides a canonical PumpSwap pool's D -- X0's D term
# (Market.conservative_x), the program's tier bound and the exit reference's cap (Market.ref_depth_cap; since M6E-1)
# -- is 100 %, so nothing a third party deposited is credited to a size, a tier or an exit's reference; Raydium keeps
# pool_lock_min_bps (Market.lock_min_bps) for its sizing; its reference cap is D as well since AIR-14.
PUMPSWAP_LOCK_MIN_BPS = 10_000


def locked_floor_ok(D: int, p_ref: int, P: Params = DEFAULT, floor: int = POOL_MIN_REAL_SOL) -> bool:
    """Eligibility floor of a listing, a zero-position and a list-only adoption (M4-SEC-1, M4-SEC-3):
    floor(floor(D*isqrt(P_ref)/1e9)*1e4/lambda_min) >= pool_min_real_sol, i.e. floor*lambda_min <= locked*1e4.
    No lambda_now clause.  P_ref = the market's P_ema, or the candidate's spot on a market without a price."""
    lm = P.pool_lock_min_bps
    return lm > 0 and floor * lm <= locked_sol(D, p_ref) * BPS


def position_floor_ok(D: int, p_ref: int, floor: int = POOL_MIN_REAL_SOL) -> bool:
    """Floor of an adoption that carries open positions (a collection window's; M4V-1): the locked SOL
    itself, floor(D*isqrt(P_ref)/1e9) >= pool_min_real_sol (locked_floor_ok without the /lambda_min)."""
    return locked_sol(D, p_ref) >= floor


def window_floor_ok(D: int, p_ema: Optional[int], p_cand: int, floor: int = POOL_MIN_REAL_SOL) -> bool:
    """M4R-2 (ECON §10.3; ARCH §3.9 propose_pool): a proposal that opens or improves a collection window
    judges the window floor at p_win = min(P_ema, P_cand), P_cand the candidate's pre-trade floor(Y*WAD/X) in
    the proposing instruction (P_cand alone on a market without a price).  execute_pool_switch keeps the
    floor at P_ema (position_floor_ok), and every failure there still cancels."""
    p_win = p_cand if p_ema is None else min(p_ema, p_cand)
    return position_floor_ok(D, p_win, floor)


def carried_x0(D: int, l_slow: int, p: int, x_now: Optional[int] = None, P: Params = DEFAULT) -> int:
    """#99: the candidate's conservative X0 as an open on it right after the adoption computes it (conservative_x
    on the candidate at price p; caps::conservative_x0):
        X0 = min(X_now, floor(L_slow'*1e9/isqrt_ceil(p)), floor(D*1e9*1e4/(lambda_min*isqrt_ceil(p))))
    with L_slow' = min(L_slow, D) -- what adopt_pool leaves (an adoption never raises L_slow) -- so X0 <= D/sqrt(p),
    the candidate's locked token reserve.  X_now, its live token reserve, is read by a proposal only; the execution
    passes None, so no unlocked LP's withdrawal or buy wrapped around the execution can cancel a window."""
    sp = isqrt_ceil(p)
    x = min(min(l_slow, D) * SQRT_WAD // sp, D * SQRT_WAD * BPS // (P.pool_lock_min_bps * sp))
    return x if x_now is None else min(x, x_now)


def carried_q_bound(max_q_hwm: int, sum_q: int) -> int:
    """#99: the O(1) bound on every open position's q the fit checks: min(max_q_hwm, sum_q) -- the high-water mark
    of q since open_positions last returned to 0 (>= every open q) and the outstanding Q (>= each open q too); a
    zero mark with positions open (an account written before the field existed) bounds nothing, so sum_q alone."""
    return sum_q if max_q_hwm == 0 else min(max_q_hwm, sum_q)


# FITS-1 (#101; PROPOSED, compiled, u32 ppm; caps::CARRIED_FEE_CAP_PPM): the carried fit's fee-scaled caps never take
# a fee above 25 bps, so every carried position is in the <= 0.75 %-of-X0 class (fee_size_cap_x100 = 300 at 2,500
# ppm) that the switch sims and the fork validated under every push; larger positions stay on the halted pool and
# settle S3 at fair value (the accepted fallback, residual 12).
CARRIED_FEE_CAP_PPM = 2_500


def carried_phi(min_open_fee_ppm: int, phi_cand: int) -> int:
    """FIT-2 (#100), FITS-1 (#101): the fee the carried fit's fee-scaled caps take,
    min(phi_inc, phi_cand, CARRIED_FEE_CAP_PPM) (ppm).  phi_inc = Market.min_open_fee_ppm, the low-water mark of the
    one-way venue fee paid by the opens since open_positions last returned to 0 -- the fee at which the open
    positions' caps and margin were computed, i.e. the incumbent's fee unless it changed or a position was carried
    from an earlier pool; a zero mark (no open counted it: an account written before the field existed) bounds
    nothing, so min(phi_cand, CARRIED_FEE_CAP_PPM).  A position carried onto a higher-fee pool therefore stays in the
    size class of the fee it was margined at (0.75 % of X0 at 25 bps), not the candidate's (2.47 % at 1 %); and since
    FITS-1 a switch between two high-fee pools (1 % -> 1 %: min(phi_inc, phi_cand) = 1 %) carries no more than the
    25-bps class either: its margin was posted on the deeper incumbent's cost, and at the 2.47 % impact cap the
    pushes ended a 4x/30 s pump in S1 (#100's residual 15).  The impact caps do not read phi."""
    phi = phi_cand if min_open_fee_ppm == 0 else min(min_open_fee_ppm, phi_cand)
    return min(phi, CARRIED_FEE_CAP_PPM)


def carried_fit_ok(x0: int, phi_ppm: int, tier: Tier, q_max: int, sum_q: int, P: Params = DEFAULT) -> bool:
    """#99 'switch only if all fit' (owner decision 2026-09-29; ECON §6.1, §6.2; ARCH §2.6 rule 3, §4.6;
    shortwin_math caps::carried_fit): every open position fits the candidate's normal opening caps on its X0
    (carried_x0):
        q_max <= min(max_q_impact(X0, iota_pos), floor(fee_size_cap_x100*phi*X0/(100*1e6)))      per position
        sum_q <= min(max_total_q_market(X0, iota_mkt), floor(fee_oi_cap_x100*phi*X0/(100*1e6)))  per market
    q_max = carried_q_bound(max_q_hwm, sum_q) >= every open q.  The market caps take X1 = X0 (nothing is sold into the
    candidate: an open's sum B_Q + q <= OI_cap(X0 + q) at q = 0); phi = carried_phi(min_open_fee_ppm, the
    candidate's one-way fee) = min(phi_inc, phi_cand, CARRIED_FEE_CAP_PPM) (ppm; FIT-2, #100; FITS-1, #101), so
    only the fee-scaled caps change with it; the tier is the one the market would have on the candidate (the sim
    takes the market's: A, B and C share the impact caps at the defaults)."""
    cap = max_q_impact(x0, tier.impact_pos_bps)
    if P.fee_size_cap_x100:
        cap = min(cap, x0 * phi_ppm * P.fee_size_cap_x100 // (100 * PPM))
    oi = max_total_q_market(x0, tier.impact_mkt_bps)
    if P.fee_oi_cap_x100:
        oi = min(oi, x0 * phi_ppm * P.fee_oi_cap_x100 // (100 * PPM))
    return q_max <= cap and sum_q <= oi


# ----------------------------------------------------------------------------
# Post-change confirmation (M4R, ECON §5.2, changelog #96; PROPOSED, owner item O18)
# ----------------------------------------------------------------------------
# Every Raydium adoption writes the anchor cap = floor(D*1e4/lambda_min) into L_ema and every ring entry (no
# sample of the adopted pool is read) and marks the observation state unconfirmed: ema_warm = EMA_WARMUP_BUCKETS,
# ema_guard = 1.  The next EMA_WARMUP_BUCKETS integrations re-seed P_ema and L_ema from the post-change bucket
# extremes on the side that cannot force an exit (Market._integrate); while ema_warm > 0 on an executable venue
# the keeper exits wait (Market.gated); while ema_guard = 1 the exit reference reads min(L_ema, L_mlo)
# (Market.exit_reference).  A market that never switches pools keeps ema_warm = ema_guard = 0 and runs exactly
# the v0.4.1 code paths, except that on Raydium the exit reference's EMA terms are capped at the locked depth
# D (M4F-1, Market.ref_depth_cap; floor(D*1e4/lambda_min) until AIR-14).  The guard reads L_mlo, the lower
# median of the ring's bucket liquidity lows: it holds only while at most 4 of the 8 newest buckets are
# attacker-only, which the keeper's per-second coverage while the guard is set provides (ECON §5.8 duty 9).
# After the guard clears, a push only delays the exit of a carried position, because every carried position fits
# the new pool's opening caps (#99, carried_fit_ok; attacks/switch_warmup.py part 11).
EMA_WARMUP_BUCKETS = 8          # = RING_LEN (compiled; u8); >= liq_persist_n's compiled maximum (3)
REF_GUARD_TOL_BPS = 100         # compiled; = fv_dev_bps's default: the guard clears once L_ema <= L_mlo*(1 + 1 %)


# ----------------------------------------------------------------------------
# Position & market engine
# ----------------------------------------------------------------------------
class Position:
    __slots__ = ("Q", "S", "M", "V", "fee_open", "J_open", "coef", "p_rent", "ert", "R_min", "t_open",
                 "deadline", "B_open", "B_margin", "B_ema_open", "spot_pre", "spot_post", "L", "pen_bps",
                 "slip_max", "s_kind", "s_count", "s_t", "s_last", "live")

    def __init__(self, **kw):
        self.s_kind = None
        self.s_count = self.s_t = self.s_last = 0
        self.live = False                      # registered in its Market's aggregates (Market.open) until it exits
        for k, v in kw.items():
            setattr(self, k, v)

    def disarm(self):
        self.s_kind = None

    @property
    def armed(self):
        return self.s_kind is not None


class InvariantError(AssertionError):
    pass


class Market:
    # ring entry: (ts, P_ema, L_ema, X_i, Y_i, lo, hi, llo, lhi) -- (X_i, Y_i) = reprice(L_ema, P_ema)
    # cached at integration (R3B simplification 3); (lo, hi, llo, lhi) are the integrated bucket's
    # price and liquidity extremes (v0.4.1, R4B-2: the settlement box takes the lower median of the
    # lows and the upper median of the highs, so a frozen second -- which contains an honest
    # observation -- keeps its honest low and high).
    warm = 0            # M4R ema_warm / ema_guard defaults (also for states built without __init__, e.g. the
    guard = False       # math crate's vector generators): a market that never switches has both at 0
    open_positions = 0  # #99 aggregates (Market.open_positions, borrowed_q, max_q_hwm; same defaults): the open
    sum_q = 0           # positions registered by Market.open, their outstanding Q, and the high-water mark of q
    max_q_hwm = 0       # since open_positions last returned to 0 (carried_fit)
    min_open_fee_ppm = 0  # FIT-2 (#100): the lowest one-way fee (ppm) an open paid since open_positions last was 0
    migrated_pool = None  # M6 (#105): the canonical pool pump's migrate_v2 created, until the prologue's venue sync
    synced_at = None      # M6 (#105): the second of the last -> PumpSwap transition (that instruction records nothing)

    def __init__(self, venue: Venue, tier: Tier, P: Params = DEFAULT, util_bps: int = 8000, t0: int = 0):
        self.venue, self.tier, self.P = venue, tier, P
        if isinstance(venue, PumpCurve):
            venue.synthetic = P.synthetic          # B_v3 on the live curve, or the retired bundle (comparison rows)
        self.util_bps = util_bps
        self.t = t0
        self.ema = venue.spot_wad()
        self.liq = venue.sqrt_k()
        self.ema_t = t0
        self.liq_slow = self.liq
        self.slow_t = t0
        self.ema_count = 0
        self.ring = [self._entry(t0, self.ema, self.liq, self.ema, self.ema, self.liq, self.liq)]
        self.b_sec: Optional[int] = None          # current observation bucket (second)
        self.b_lo = self.b_hi = self.b_llo = self.b_lhi = 0
        self.lb_sec: Optional[int] = None         # last completed bucket (one stored bucket, V1-12)
        self.lb_lo = self.lb_hi = 0
        self.J = [0, 0, 0]                        # rent basis indices (ECON §7)
        self.index_t = t0
        self.complete_t: Optional[int] = None
        self.graduated_t: Optional[int] = None
        self.route_broken = False                 # Emergency flag (A40): opens + S3 evidence only
        self.no_route_since: Optional[int] = None  # S3 chain (s3_no_route)
        self.last_no_route_obs: Optional[int] = None
        self.pool_fail_since: Optional[int] = None  # fast-path chain (pool_fail, R3B-8 / R3A-4)
        self.last_pool_fail_obs: Optional[int] = None
        self.pending_target: Optional[int] = None
        self.warm = 0                             # Market.ema_warm: integrations left in the post-change warm-up
        self.guard = False                        # Market.ema_guard: the post-change reference guard (M4R)
        self.open_positions = 0                   # #99: Market.open_positions, borrowed_q (sum_q) and max_q_hwm
        self.sum_q = 0
        self.max_q_hwm = 0
        self.min_open_fee_ppm = 0                 # FIT-2 (#100): Market.min_open_fee_ppm (carried_phi)
        self.migrated_pool: Optional[CPPool] = None  # M6 (#105): migrate_v2's pool, until the prologue's venue sync
        self.synced_at: Optional[int] = None      # M6 (#105): the second of the last -> PumpSwap transition
        self._q_hint = 0                          # sim helper: size of the position under test

    @staticmethod
    def _entry(t, p, l, lo, hi, llo, lhi, obs=None, lobs=None):
        # (obs, lobs) at indices 9, 10 are sim-only: the v0.4 comparison rows (box_obs=True) use them
        st = reprice(l, p) or (0, 0)
        return (t, p, l, st[0], st[1], lo, hi, llo, lhi, p if obs is None else obs, l if lobs is None else lobs)

    # --- time evolution ---------------------------------------------------
    def rate(self) -> int:
        return rate_bps(self.tier, self.util_bps, self.P.util_kink_bps)

    def accrue(self, t: int) -> None:
        """Rent basis indices on P_ema BEFORE this instruction's EMA roll (ECON §7)."""
        dt = t - self.index_t
        if dt > 0:
            U, k, p = self.util_bps, self.P.util_kink_bps, min(self.ema, P_ACC_MAX)
            self.J[0] += p * dt
            self.J[1] += cdiv(p * min(U, k) * dt, BPS)
            self.J[2] += cdiv(p * max(U - k, 0) * dt, BPS)
            self.index_t = t

    def migrating(self) -> bool:
        return self.venue.kind == "curve" and self.venue.complete

    def advance(self, t: int) -> None:
        """Exogenous state up to t: graduation state machine, boost, rent accrual,
        continuation of exogenous price moves across migration.  M6 as built (ECONOMICS.md #104, #105; ARCH §2.5):
        pump's migrate_v2 (migration_delay_s after the completion; never while PumpSwap's create_pool is disabled)
        creates the canonical pool and starts its boost, other traders trade it from then on (exo_venue), and the
        market itself moves onto it in the prologue of its next instruction (observe: the roll, then the venue sync,
        _graduate)."""
        if self.migrating():
            if self.complete_t is None:
                self.complete_t = t
            if (self.migrated_pool is None and not self.venue.create_pool_halted
                    and t >= self.complete_t + self.P.migration_delay_s):
                self.migrated_pool = self.venue.migrate()
                self.migrated_pool.start_boost(t)
        v = self.exo_venue()
        if isinstance(v, CPPool):
            v.boost_tick(t)
        self.accrue(t)
        self.t = max(self.t, t)
        if self.pending_target is not None and self.exo_venue().has_route():
            tgt, self.pending_target = self.pending_target, None
            self.exo_move(tgt)

    def exo_venue(self) -> Venue:
        """The venue other traders trade: the pool migrate_v2 created, from then until the market's venue sync."""
        return self.migrated_pool if self.migrated_pool is not None else self.venue

    def s3_no_route(self) -> bool:
        """ARCH §5.3: !venue_executable v route_broken v fee above the hard cap."""
        v = self.venue
        if not v.has_route():
            return True
        return self.route_broken or v.fee_oneway_ppm() > self.P.venue_fee_hard_cap_bps * 100

    def pool_fail(self) -> bool:
        """R3B-8 / R3A-4: the fast-path predicate (venue-side evidence only; never the flag)."""
        v = self.venue
        return (not v.has_route()) or v.fee_oneway_ppm() > self.P.venue_fee_hard_cap_bps * 100

    @staticmethod
    def _chain(since, last, flag, t, gap):
        if not flag:
            return None, None
        if since is None or last is None or t - last > gap:
            since = t
        return since, t

    def observe(self, t: int) -> None:
        """Prologue of every instruction that carries the venue accounts: roll the
        completed bucket, update the S3 and fast-path chains, then add this
        instruction's pre-trade spot to the current bucket (only if the venue can
        execute, or is a migrating curve whose final state is still recorded).
        M6 as built (#104, #105): if migrate_v2 has created the canonical pool, the venue sync moves the market onto
        it after the roll (_graduate) and the instruction does nothing else here; on a canonical PumpSwap pool in
        its warm-up, L_slow is capped at the pool's locked depth D (M6R-1)."""
        P = self.P
        if self.b_sec is not None and t > self.b_sec:
            self._integrate(t)
        if self.migrated_pool is not None:
            # M6 as built (#104, #105; ARCH §2.5, prologue step 6): the venue sync moves the market onto the canonical
            # pool after the roll; the transitioning instruction runs no route chain and records no observation
            self._graduate(self.migrated_pool, t)
            return
        v = self.venue
        self._warmup_slow_cap()
        self.no_route_since, self.last_no_route_obs = self._chain(
            self.no_route_since, self.last_no_route_obs, self.s3_no_route(), t, P.s3_obs_gap_s)
        if v.kind == "ray":
            self.pool_fail_since, self.last_pool_fail_obs = self._chain(
                self.pool_fail_since, self.last_pool_fail_obs, self.pool_fail(), t, P.s3_obs_gap_s)
        if v.has_price() and (v.has_route() or self.migrating()):
            p, lq = v.spot_wad(), v.sqrt_k()
            if self.b_sec != t:
                self.b_sec, self.b_lo, self.b_hi, self.b_llo, self.b_lhi = t, p, p, lq, lq
            else:
                self.b_lo, self.b_hi = min(self.b_lo, p), max(self.b_hi, p)
                self.b_llo, self.b_lhi = min(self.b_llo, lq), max(self.b_lhi, lq)

    def _warmup_slow_cap(self) -> None:
        """M6R-1 (owner decision 2026-09-30, ECONOMICS.md #105; ARCH §2.5, prologue step 6): during a PumpSwap warm-up
        (after the graduation or a PumpSwap price start) every executable observation caps L_slow at the pool's locked
        depth D -- the Raydium adoption's L_slow = min(L_slow, D), taken at the first instruction that reads D (the
        transition itself reads no LP mint).  L_slow does not integrate during the warm-up and D moves only by the LP
        fee the pool keeps, so the first cap is the one that binds; on an honest graduation it never does (D >= the
        lp_supply anchor >= L_slow), and it undoes a deposit that raised the anchor between migrate_v2 and the
        transition."""
        v = self.venue
        if isinstance(v, CPPool) and v.canonical and self.warm > 0 and v.has_route():
            self.liq_slow = min(self.liq_slow, v.locked_depth())

    def _integrate(self, t: int) -> None:
        """Integrate the completed bucket (C0-5): the observation is the EMA clamped
        into [bucket low, bucket high], then to +-C, time-weighted with dt <= DT_MAX.
        While ema_warm > 0 (M4R post-change confirmation) the warm-up branch runs instead;
        after either branch an integration with ema_warm = 0 may clear the reference guard."""
        if self.warm > 0:
            self._integrate_warmup(t)
            return
        P, T = self.P, self.tier
        c = T.ema_clamp_bps
        obs = min(max(self.ema, self.b_lo), self.b_hi)
        obs = min(max(obs, self.ema * BPS // (BPS + c)), self.ema * (BPS + c) // BPS)
        lobs = min(max(self.liq, self.b_llo), self.b_lhi)
        lobs = min(max(lobs, self.liq * BPS // (BPS + c)), self.liq * (BPS + c) // BPS)
        dt = min(t - self.ema_t, T.ema_dtmax_s)
        if dt > 0:
            self.ema = (self.ema * T.ema_tau_s + obs * dt) // (T.ema_tau_s + dt)
            self.liq = (self.liq * T.ema_tau_s + lobs * dt) // (T.ema_tau_s + dt)
            self.ema_t = t
            self.ema_count += 1
            self.ring.append(self._entry(t, self.ema, self.liq, self.b_lo, self.b_hi, self.b_llo, self.b_lhi,
                                         obs, lobs))
            if len(self.ring) > P.ring_len:
                del self.ring[0]
            # M4X-2: while the post-change guard is set, L_slow integrates no more than the ring's honest lows
            # (L_mlo after the push): a deposit-frozen L_ema must not climb into L_slow, which the reference
            # L_b reads unguarded.
            slow_in = min(lobs, self.ring_medians()[2]) if self.guard else lobs
            dts = min(t - self.slow_t, P.liq_slow_dtmax_s)
            self.liq_slow = (self.liq_slow * P.liq_slow_tau_s + slow_in * dts) // (P.liq_slow_tau_s + dts)
            self.slow_t = t
            self._guard_check()
        self.lb_sec, self.lb_lo, self.lb_hi = self.b_sec, self.b_lo, self.b_hi
        self.b_sec = None

    def _integrate_warmup(self, t: int) -> None:
        """M4R warm-up integration (ECON §5.2; shortwin_math::observe::integrate, ema.warm > 0).  With
        k = EMA_WARMUP_BUCKETS - ema_warm post-change entries already in the ring and E = this bucket plus
        those k entries (n = k + 1 <= 8):
            P_ema = max(min(min over E of the bucket price lows,            (fresh, the safe side)
                            floor((P_ema*tau + floor(P_ema*(1e4 + C)/1e4)*dt)/(tau + dt))),
                                                                            the largest v0.4.1 step up (M4X-1)
                        floor((P_ema*tau + floor(P_ema*1e4/(1e4 + C))*dt)/(tau + dt)))
                                                                            the largest v0.4.1 step down (M6-SEC-1)
            L_ema = min(max H, floor(lomed(H)*(1e4 + C)/1e4))               H = the bucket liquidity highs of E,
                                                                            lomed = H[(n-1)//2] ascending
        with dt as in _integrate; no liquidity blend; the ring entry is pushed with the bucket's extremes and the
        reprojection of the seed; L_slow and slow_ts unchanged; ema_warm -= 1.  The price seed moves per integration
        by no more than the v0.4.1 EMA can, up (x(tau + dt*(1 + C))/(tau + dt): 9/7 at dt = 2, 7/6 at dt = 1;
        observe::price_up_bound) and, since M6-SEC-1 (owner decision 2026-09-30, ECONOMICS.md #105), down
        (x(tau + dt/(1 + C))/(tau + dt): 6/7 at dt = 2, 11/12 at dt = 1; observe::price_down_bound): attacker-only
        warm-up seconds reach no higher than the same seconds in steady state with a large enough pump (M4X-1), a
        genuine move is tracked at that rate either way (up 2.2x, down 0.50x by the gate), and one low recorded in a
        warm-up second -- a sell-wrapped crank next to the keeper's -- pulls the seed down by at most one v0.4.1
        step per remaining warm-up integration ((11/12)^k), no longer to the low at once (the low stays in E, so it
        keeps pulling until the gate: at most x0.4986 over the 8 integrations).  A seed that lags a fall forces no
        exit: the spot leg reads the live price (§5.5).  A withdraw-wrapped sample lowers only its bucket's llo,
        which the seed never reads; a deposit-wrapped one raises one bucket's lhi, bounded by the lower median
        unless >= 5 of 8 buckets are pushed.  The dt <= 0 path is as in _integrate: no step."""
        if t > self.ema_t:
            T = self.tier
            k = EMA_WARMUP_BUCKETS - self.warm
            ents = self.ring[len(self.ring) - k:] if k else []
            dt = min(t - self.ema_t, T.ema_dtmax_s)
            up = (self.ema * T.ema_tau_s + self.ema * (BPS + T.ema_clamp_bps) // BPS * dt) // (T.ema_tau_s + dt)
            down = (self.ema * T.ema_tau_s + self.ema * BPS // (BPS + T.ema_clamp_bps) * dt) // (T.ema_tau_s + dt)
            p = max(min([e[5] for e in ents] + [self.b_lo, up]), down)
            H = sorted([e[8] for e in ents] + [self.b_lhi])
            n = len(H)
            l = min(H[n - 1], H[(n - 1) // 2] * (BPS + self.tier.ema_clamp_bps) // BPS)
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

    def _guard_check(self) -> None:
        """M4R reference guard: cleared at the first integration with ema_warm = 0 after it and
        L_ema*1e4 <= L_mlo*(1e4 + REF_GUARD_TOL_BPS), L_mlo = lower median of the ring's bucket liquidity lows.
        An honest switch clears it at the gate (the 8th warm-up integration)."""
        if self.guard and self.warm == 0 and self.liq * BPS <= self.ring_medians()[2] * (BPS + REF_GUARD_TOL_BPS):
            self.guard = False

    def mark_restart(self) -> None:
        """observe::mark_restart: the observation state was (re)started by a Raydium venue change, a Raydium or
        PumpSwap price start or the -> PumpSwap graduation (M6, #104, #105); it is unconfirmed until
        EMA_WARMUP_BUCKETS integrations have re-seeded it."""
        self.warm = EMA_WARMUP_BUCKETS
        self.guard = True

    def gated(self) -> bool:
        """M4R gate (ARCH §3.6-§3.8): while ema_warm > 0 on an EXECUTABLE venue, liquidate / expire fail with
        EmaWarmingUp and sol_settle evaluates nothing (no arm change, no settlement).  A venue that cannot execute
        is never gated, so S3 is never deadlocked.  (Opens: refused while ema_warm > 0 on Raydium and -- M6, kept by
        the owner on 2026-09-30, OWNER-M6R-2 -- on PumpSwap, Market.open.)"""
        return self.warm > 0 and self.venue.has_route()

    def step_to(self, t: int) -> None:
        """Honest crank at time t (advance + observe)."""
        self.advance(t)
        self.observe(t)

    def reset_ema_to_spot(self, t: int) -> None:
        """Monte-Carlo shortcut for calm periods (keeper cranks, EMA converged)."""
        if self.venue.has_route():
            self.ema = self.venue.spot_wad()
            self.liq = self.venue.sqrt_k()
            self.ema_t = t
            self.ema_count += 1
            self.ring.append(self._entry(t, self.ema, self.liq, self.ema, self.ema, self.liq, self.liq))
            if len(self.ring) > self.P.ring_len:
                del self.ring[0]

    def _rewrite_ring_liq(self, L: int) -> None:
        """R3B-7a / R3A-11d: on any venue change every ring entry's L and its liquidity
        extremes are set to the reset liquidity (and the cached reprojection recomputed)."""
        self.ring = [self._entry(e[0], e[1], L, e[5], e[6], L, L, e[9], L) for e in self.ring]
        self.b_sec = None                          # the open bucket is cleared (ARCH §4.4)

    def _switch_to_pool(self, pool: CPPool, t: int) -> None:
        """Graduation, v0.4.1 rule: P_ema carries over; liquidity EMAs reset to the new pool (observe::VenueChange::
        Graduation).  Kept unchanged ONLY for the math crate's unit vectors (gen_oracle.py unit_rewrite); the
        program's rule is _graduate (M6 as built, ECONOMICS.md #104, #105), which every scenario runs."""
        self.venue = pool
        self.graduated_t = t
        pool.start_boost(t)
        self.liq = pool.sqrt_k()
        self.liq_slow = self.liq
        self._rewrite_ring_liq(self.liq)

    def _restart_on(self, pool: Venue, l_new: int, slow_cap: int) -> None:
        """observe::rewrite_liquidity(l_new, RaydiumLocked { slow_cap }) then mark_restart (engine::pool_select::adopt,
        venues::pump_amm::graduate): L_ema and every ring entry's l / llo / lhi := l_new ((x, y) recomputed), L_slow =
        min(L_slow, slow_cap), never raised; the open bucket and both route chains cleared; P_ema, the ring prices and
        the age kept; ema_warm = 8, ema_guard = 1."""
        self.venue = pool
        self.liq = l_new
        self.liq_slow = min(self.liq_slow, slow_cap)
        self._rewrite_ring_liq(l_new)
        self.pool_fail_since = self.last_pool_fail_obs = None
        self.no_route_since = self.last_no_route_obs = None
        self.mark_restart()

    def _graduate(self, pool: CPPool, t: int) -> None:
        """PumpCurve / PumpMigrating -> PumpSwap as built (M6, ECONOMICS.md #104; ARCH §2.5; venues::pump_amm::graduate;
        kept by the owner on 2026-09-30, #105: OWNER-M6R-2 the warm-up, OWNER-M6R-4 the exit delay): the prologue's
        venue sync, after the roll of the curve's last bucket, reads no sample of the pool -- the anchor is the pool's
        lp_supply (its isqrt(X*(Y + VQR)) at creation, which no trade moves; a deposit between migrate_v2 and the
        transition raises it, which the warm-up's L_slow cap at D undoes, observe) -- and restarts the observation
        state on it: _restart_on(pool, anchor, anchor) (L_slow = min(L_slow, anchor)), graduated_at = now.  The
        transitioning instruction records nothing; the 8 warm-up integrations re-seed P_ema and L_ema from the pool's
        own buckets, and keeper exits, settlements and PumpSwap opens wait for them (gated, Market.open): the keeper's
        exit on a graduated position lands 8-12 s after the migration."""
        anchor = pool.lp_units
        self.graduated_t = t
        self.migrated_pool = None
        self._restart_on(pool, anchor, anchor)
        self.synced_at = t

    def fast_switch(self, pool: Venue, t: int) -> None:
        """Raydium fast path (incumbent failure), v0.4.1 rule: L_ema and L_slow reset to
        min(new sqrt(k), current value); ring L rewritten; age kept (ARCH §2.6).  Kept unchanged
        ONLY for the math crate's differential vectors (VenueChange::Raydium); the program's rule is
        adopt_pool (M4R post-change confirmation), which FAST_SCENARIOS replays."""
        self.venue = pool
        self.liq = min(pool.sqrt_k(), self.liq)
        self.liq_slow = min(pool.sqrt_k(), self.liq_slow)
        self._rewrite_ring_liq(self.liq)
        self.pool_fail_since = self.last_pool_fail_obs = None
        self.no_route_since = self.last_no_route_obs = None

    def adopt_pool(self, pool: Venue, t: int) -> None:
        """Raydium adoption of a priced market (ECON §5.2; ARCH §2.6 rule 4; engine::pool_select::adopt: every
        Raydium adoption -- listing-window replacement, zero-position, fast path, a list-only market's window):
        NO sample of the adopted pool is read.  With D its locked depth and cap = floor(D*1e4/lambda_min),
        L_ema and every ring entry's l / llo / lhi := cap ((x, y) recomputed; VenueChange::RaydiumLocked
        { slow_cap: D }), L_slow = min(L_slow, D), never raised; the open bucket and both route chains cleared;
        P_ema, the ring prices and the age kept; then mark_restart (ema_warm = 8, ema_guard = 1).  The warm-up
        integrations re-seed P_ema and L_ema from the new pool's own post-change buckets (_integrate_warmup)."""
        D = pool.locked_depth()
        cap = depth_at_lambda(D, self.P.pool_lock_min_bps)
        self.venue = pool
        self.liq = cap
        self.liq_slow = min(self.liq_slow, D)
        self._rewrite_ring_liq(cap)
        self.pool_fail_since = self.last_pool_fail_obs = None
        self.no_route_since = self.last_no_route_obs = None
        self.mark_restart()

    def exo_move(self, p_wad: int) -> None:
        """Other market participants trade the venue toward price p_wad (the migrated pool from migrate_v2 on, before
        the market's own venue sync)."""
        v = self.exo_venue()
        if not v.has_route():
            self.pending_target = p_wad
            return
        v.move_to_price(p_wad)
        if v.kind == "curve" and v.complete:
            self.pending_target = p_wad   # continue on the AMM after migration

    # --- quotes ---------------------------------------------------------------
    def cap_ppm(self) -> int:
        return self.P.venue_fee_hard_cap_bps * 100

    def quote_spot(self, Q: int, risk: bool = False):
        """Execution quote (live fee) or risk quote (fee capped at the hard cap) on the live state (a live curve's
        B_v3 on its live would-be pool, curve_base_ata - rT: pump-final §5.2)."""
        v = self.venue
        if not v.has_route():
            return None
        return v.quote_at(Q, *v.eff(), two_leg=self.P.two_leg, cap_ppm=self.cap_ppm() if risk else None, live=True)

    def quote_xy(self, Q: int, X: int, Y: int):
        """Risk quote on a hypothetical state (cached ring entry or a reprojection)."""
        if X <= 0:
            return None
        return self.venue.quote_at(Q, X, Y, two_leg=self.P.two_leg, cap_ppm=self.cap_ppm())

    def quote_repriced(self, Q: int, p_wad: int, L: int):
        st = reprice(L, p_wad)
        return None if st is None else self.quote_xy(Q, *st)

    def quote_ema(self, Q: int):
        e = self.ring[-1]
        return self.quote_xy(Q, e[3], e[4])

    def slip_at(self, Q: int, X: int, Y: int, live: bool = False) -> int:
        """sigma: average execution premium over the marginal price of (X, Y), ex fees,
        in bps, floor (SPEC S2 slippage; ECON §2 numerics).  live=True: (X, Y) is the venue's live state, so a live
        curve's B_v3 prices its pool part on the live curve_base_ata - rT, as the program's sigma_live does
        (RiskQuotes::live -> Quoter::buy_live; pump-final §5.2): S2's live leg and the keeper's buyback gate."""
        q = self.venue.quote_at(Q, X, Y, two_leg=self.P.two_leg, live=live)
        if q is None:
            return INF
        p = Y * WAD // X
        return q[0] * WAD * BPS // (Q * p) - BPS

    def slip_repriced(self, Q: int, p_wad: int, L: int) -> int:
        st = reprice(L, p_wad)
        return INF if st is None else self.slip_at(Q, *st)

    # --- position lifecycle ----------------------------------------------------
    def rent_accrued(self, pos: Position) -> int:
        base, s1, s2 = pos.coef
        d = [self.J[i] - pos.J_open[i] for i in range(3)]
        return cdiv(pos.Q * (base * d[0] + s1 * d[1] + s2 * d[2]), BPS * YEAR_S * WAD)

    def rent_due(self, pos: Position) -> int:
        return max(self.rent_accrued(pos), pos.R_min)

    def curve_cap_reserves(self):
        """#103 (M5-SEC-3; ARCH §4.6; the program's engine::caps::curve_reserves): the curve's cap reserves
        (X0, rT_guard) -- the curve as it stands at the EMA price whenever that is the smaller one:
            P_ema <= P_spot:  (vT, rT)                                    the live values, unrounded
            else:             x_ema = floor(isqrt(vT*vQ)*1e9/isqrt_ceil(P_ema))
                              (min(vT, x_ema), min(rT, max(0, x_ema - (vT - rT))))
        The curve's own L = isqrt(vT*vQ) is not moved by a curve trade (§2.3), so a sale of delta wrapped
        around the open -- [sell(delta), open, buy(delta)], which lifts the live vT and rT by delta for the
        open only -- no longer lifts X0 or the Tier D guard's rT (v0.4.1 read the live vT and rT)."""
        v = self.venue
        if self.ema <= v.spot_wad():
            return v.vt, v.rt
        x_ema = v.sqrt_k() * SQRT_WAD // isqrt_ceil(self.ema)
        return min(v.vt, x_ema), min(v.rt, max(0, x_ema - (v.vt - v.rt)))

    def locked_depth_venue(self) -> bool:
        """Whether the venue has a locked depth D the caps and the reference read (conservative_x, ref_depth_cap):
        Raydium, and since M6R-1 (owner decision 2026-09-30, ECONOMICS.md #105) a canonical PumpSwap pool, D from its
        burned LP (CPPool.lock_bps / locked_depth: Pool.lp_supply and the LP mint's supply).  A CPPool marked
        non-canonical is the scenario library's model of a violated lock (lp_mutable, attacks/fvcap.py): no D, as
        before #105.  The curve has none (its L is virtual; no LP exists)."""
        v = self.venue
        return v.kind == "ray" or (isinstance(v, CPPool) and v.canonical)

    def lock_min_bps(self) -> int:
        """The lambda_min that divides D wherever it is credited to a size or a tier (conservative_x; the program's
        tier bound; the program's VenueKind::lock_min_bps; ref_depth_cap divides by 100 % on every venue since
        AIR-14): Raydium pool_lock_min_bps; a canonical PumpSwap pool
        PUMPSWAP_LOCK_MIN_BPS = 10,000 since OWNER-M6D-1 (owner decision 2026-09-30, ECONOMICS.md #106) -- a canonical
        pool's lock is its burned creation LP, so D itself is credited, never a third party's deposit (under #105 a
        deposit held ~1 h sized opens at up to lambda/lambda_min = 2x the burned depth's cap, and such a position gapped
        into S1 when the LP left at a genuine pump, M6D-1; a reference cap at 2D held an expiry up after a deposit
        pulled at the deadline, M6E-1)."""
        v = self.venue
        if isinstance(v, CPPool) and v.canonical:
            return PUMPSWAP_LOCK_MIN_BPS
        return self.P.pool_lock_min_bps

    def conservative_x(self) -> int:
        """R3A-3 ii: token reserve for caps: curve min(vT, x_ema) (#103, curve_cap_reserves; vT while
        P_ema <= P_spot); PumpSwap min(X_now, L_slow/sqrt(P_ema)); Raydium and (since M6R-1, #105) a canonical
        PumpSwap pool additionally <= D/(lambda_min*sqrt(P_ema)), lambda_min = lock_min_bps(): Raydium's
        pool_lock_min_bps, PumpSwap's 100 % since OWNER-M6D-1 (#106), i.e. D/sqrt(P_ema), the burned LP's token
        reserve at P_ema.  A deposit wrapped around the open lifts neither L_slow nor D (only X_now, which the minimum
        drops); a deposit held long enough to push L_slow is bounded at lambda/lambda_min of the pool's own depth on
        Raydium and adds nothing on PumpSwap."""
        v = self.venue
        X_now = v.eff()[0]
        if v.kind == "curve":
            return self.curve_cap_reserves()[0]
        sp = isqrt_ceil(self.ema)
        x = min(X_now, self.liq_slow * SQRT_WAD // sp)
        if self.locked_depth_venue():
            x = min(x, v.locked_depth() * SQRT_WAD * BPS // (self.lock_min_bps() * sp))
        return x

    def oi_cap_q(self, X_after: int) -> int:
        """Per-market cap on total outstanding Q on post-open reserves: SPEC impact cap,
        tightened on low-fee venues (fee_oi_cap, fee in ppm)."""
        cap = max_total_q_market(X_after, self.tier.impact_mkt_bps)
        if self.P.fee_oi_cap_x100:
            cap = min(cap, X_after * self.venue.fee_oneway_ppm() * self.P.fee_oi_cap_x100 // (100 * PPM))
        return cap

    def max_open_q(self) -> int:
        v = self.venue
        X0 = self.conservative_x()
        q = max_q_impact(X0, self.tier.impact_pos_bps)
        q = min(q, self.oi_cap_q(X0 + q))
        if v.kind == "curve":   # Tier D graduation guard: Q <= gamma*(rT_guard+Q) (#103: rT_guard, curve_cap_reserves)
            g = self.P.curve_oi_gamma_bps
            q = min(q, self.curve_cap_reserves()[1] * g // (BPS - g))
        if self.P.fee_size_cap_x100:
            q = min(q, X0 * v.fee_oneway_ppm() * self.P.fee_size_cap_x100 // (100 * PPM))
        return q

    def open(self, Q: int, t: int) -> Position:
        self.advance(t)
        if self.migrated_pool is not None:
            # M6 as built (ARCH §2.5): an open whose own prologue would move the market onto the pool is refused
            # (VenueMigrating) and the transition reverts with it; the next crank performs it
            raise ValueError("VenueMigrating")
        self.observe(t)
        v, T, P = self.venue, self.tier, self.P
        if self.ema > P_OPEN_MAX:
            raise ValueError("price above P_OPEN_MAX")
        if self.warm > 0 and v.kind in ("ray", "amm"):
            # M4R / M6 (OWNER-M6R-2 kept, #105): opens wait for ema_warm == 0 on Raydium and PumpSwap (EmaWarmingUp)
            raise ValueError("EmaWarmingUp")
        if v.kind == "curve" and v.create_pool_halted:
            # M6-SEC-3 (owner decision 2026-09-30, #105): no open on a curve whose graduation PumpSwap has halted
            raise ValueError("GraduationHalted")
        spot_pre = v.spot_wad()
        e = self.ring[-1]                       # EMA state after this instruction's roll (R3A-12)
        S = v.sell(Q)
        qs = self.quote_spot(Q)
        assert qs is not None
        # D4 literal: (M + S)/B >= CR_open at spot (post-open) and at the EMA reserves as they stand
        qe = v.quote_at(Q, e[3], e[4], two_leg=P.two_leg)
        B_margin = max(qs[1], qe[1] if qe else qs[1])
        if P.margin_policy == "cr":
            M = max(cdiv(T.cr_open_bps * B_margin, BPS) - S, 0)
        else:
            M = cdiv(T.m_bps * qs[1], BPS)
        fee_open = cdiv(P.open_fee_bps * qs[1], BPS)
        U, k = self.util_bps, P.util_kink_bps
        r_scaled = T.rate_base_bps * BPS + T.rate_s1_bps * min(U, k) + T.rate_s2_bps * max(U - k, 0)
        R_min = cdiv(Q * self.ema * r_scaled * T.min_rent_s, BPS * BPS * YEAR_S * WAD)
        # #99: open_short counts the position and raises the high-water mark of q (Market.max_q_hwm); FIT-2 (#100):
        # and lowers the low-water mark of the one-way fee its caps and margin were computed at (min_open_fee_ppm)
        self.open_positions += 1
        self.sum_q += Q
        self.max_q_hwm = max(self.max_q_hwm, Q)
        phi = v.fee_oneway_ppm()
        self.min_open_fee_ppm = phi if self.min_open_fee_ppm == 0 else min(self.min_open_fee_ppm, phi)
        return Position(Q=Q, S=S, M=M, V=S + M, fee_open=fee_open, J_open=tuple(self.J),
                        coef=(T.rate_base_bps, T.rate_s1_bps, T.rate_s2_bps), p_rent=P.rent_protocol_bps,
                        ert=P.exec_rent_tol if v.pump else 0, R_min=R_min, t_open=t,
                        deadline=t + T.max_dur_s, B_open=qs[1], B_margin=B_margin,
                        B_ema_open=qe[1] if qe else 0, spot_pre=spot_pre, spot_post=v.spot_wad(),
                        L=T.cr_liq_bps, pen_bps=T.penalty_bps, slip_max=T.max_liq_slip_bps, live=True)

    def _release(self, pos: Position) -> None:
        """#99: a registered position exited (close, liquidation, expiry, S1 / S2 / S3): the aggregates drop it, and
        the high-water mark of q resets when the last open position is gone (it is never lowered otherwise, so it
        stays >= every open position's q: conservative by design; a closed large position keeps it until the market
        empties, FIT-3, residual 14); so does the fee's low-water mark (FIT-2, #100; never raised otherwise, so it
        stays <= the fee every open position was margined at)."""
        if not getattr(pos, "live", False):
            return
        pos.live = False
        self.open_positions -= 1
        self.sum_q -= pos.Q
        if self.open_positions == 0:
            self.max_q_hwm = 0
            self.min_open_fee_ppm = 0

    def carried_fit(self, pool: Venue, execution: bool = False) -> bool:
        """#99 'switch only if all fit' for an adoption that would carry this market's open positions onto pool (a
        proposal that opens or improves a window; the execution with execution=True): carried_fit_ok on the market
        aggregates (carried_q_bound(max_q_hwm, sum_q), sum_q) against the candidate's X0 = carried_x0(D, L_slow, p,
        X_now) at p = min(P_ema, P_cand) -- the M4R-2 floor's reference, P_cand the candidate's spot in this
        instruction -- AND at p = P_ema, the first refusal winning: a lower price loosens X0's depth terms (a token
        reserve, ~ 1/sqrt(p)), so the second evaluation keeps a dump wrapped around the instruction from loosening
        them.  The proposal's X_now is not protected (FIT-1, #100): after a genuine pump of the candidate it binds, and
        a dump wrapped around the proposal lifts it inside the transaction; the execution drops X_now (carried_x0),
        which is the guarantee (up to lambda*sqrt(P_cand/P_ema) x a native open's cap).  True without open positions:
        zero-position adoptions and listings are unaffected (engine::pool_select::carried_fit).  The fee-scaled caps
        take phi = carried_fee(pool) (FIT-2, #100; FITS-1, #101)."""
        if self.open_positions == 0:
            return True
        D = pool.locked_depth()
        qb = carried_q_bound(self.max_q_hwm, self.sum_q)
        x_now = None if execution else pool.eff()[0]
        phi = self.carried_fee(pool)
        for p in (min(self.ema, pool.spot_wad()), self.ema):
            x0 = carried_x0(D, self.liq_slow, p, x_now, self.P)
            if not carried_fit_ok(x0, phi, self.tier, qb, self.sum_q, self.P):
                return False
        return True

    def carried_fee(self, pool: Venue) -> int:
        """The fee (ppm) the carried fit's fee-scaled caps take on candidate pool: carried_phi(min_open_fee_ppm, the
        candidate's one-way fee) = min(phi_inc, phi_cand, CARRIED_FEE_CAP_PPM) (FIT-2, #100; FITS-1, #101)."""
        return carried_phi(self.min_open_fee_ppm, pool.fee_oneway_ppm())

    def settle_reward(self, V: int) -> int:
        P = self.P
        return min(P.settle_fixed + V * P.settle_bps // BPS, P.settle_cap, V)

    def expiry_keeper(self, b_ref: int) -> int:
        P = self.P
        return min(P.expiry_keeper_fixed + b_ref * P.expiry_keeper_bps // BPS, P.expiry_keeper_cap)

    # --- settlement box (ONE rule for every settlement decision, v0.4) ---------------
    def ring_medians(self):
        """(P_mlo, P_mhi, L_mlo, L_mhi): lower median of the ring's bucket price lows, upper
        median of the price highs, lower median of the liquidity lows, upper median of the
        liquidity highs (v0.4.1, R4B-2).  Moving P_mlo up or P_mhi down needs 5 of 8 entries,
        and only an attacker-only second can raise a bucket low or lower a bucket high."""
        r = self.ring
        return (lower_median([e[5] for e in r]), upper_median([e[6] for e in r]),
                lower_median([e[7] for e in r]), upper_median([e[8] for e in r]))

    def settle_box(self, t: int):
        """(P_lo, P_hi, L_lo, L_hi) spanned by the EMA state and the ring's bucket extremes
        (ECON §5.6 v0.4.1).  If nothing has been integrated for more than ring_len seconds and
        the venue cannot execute now (a venue that records no observations: nothing in the box
        can have been pushed since), the box is the EMA point alone.  On an executable venue a
        stale integration proves nothing -- the program's lender instructions roll a pushed
        bucket without recording (ARCH A16), so the pushed EMA can age past ring_len with no
        honest observation after it -- and the ring medians always apply (M2-SEC-1).
        AIR-15: L_mhi is capped at ref_depth_cap() (Raydium, canonical PumpSwap; M4F-1's cap of the exit
        reference's EMA terms): a deposit wrapped around any record raises that second's liquidity high, which
        honest records in the same second cannot lower, so wraps in 4 of the 8 seconds would otherwise widen L_hi
        at will and defer S1 / S2 and the S2 / S3 amount check.  L_hi = max(L_ema, min(L_mhi, cap)): L_ema is never
        capped (the box keeps the EMA point), and the S1 / S2 corner never reads less depth than min(L_mhi, cap),
        which only attacker-only seconds lower (5 of 8) and no capital-only bundle moves (D is deposit- and
        withdraw-invariant)."""
        if t - self.ema_t > self.P.ring_len and not self.venue.has_route():
            return self.ema, self.ema, self.liq, self.liq
        if self.P.box_obs:                     # v0.4 comparison rows: lower medians of the clamped obs / lobs
            pm, lm = lower_median([e[9] for e in self.ring]), lower_median([e[10] for e in self.ring])
            return min(self.ema, pm), max(self.ema, pm), min(self.liq, lm), max(self.liq, lm)
        plo, phi, llo, lhi = self.ring_medians()
        cap = self.ref_depth_cap()
        if cap is not None:
            lhi = min(lhi, cap)
        return min(self.ema, plo), max(self.ema, phi), min(self.liq, llo), max(self.liq, lhi)

    def status(self, pos: Position, t: int) -> dict:
        P = self.P
        R = self.rent_due(pos)
        Veff = pos.V - R
        L = pos.L
        v = self.venue
        live = v.has_route()
        qs = self.quote_spot(pos.Q) if live else None                 # execution quote
        qs_r = self.quote_spot(pos.Q, risk=True) if live else None    # risk quote
        p_live = v.spot_wad() if v.has_price() else None
        lo_ok = self.lb_sec is not None and t - self.lb_sec <= P.ref_max_age_s
        p_rs = min(p_live, self.lb_lo) if (lo_ok and p_live is not None) else None
        L_live = v.sqrt_k() if v.has_price() else self.liq
        # FITS-2 (#101): the spot leg reads the robust spot on L_spot = max(L_live, L_ema'), never below the guarded
        # EMA liquidity, so an LP withdrawal wrapped around this instruction cannot make it true
        L_spot = self.spot_liquidity(L_live)
        qrs = self.quote_repriced(pos.Q, p_rs, L_spot) if p_rs is not None else None
        qe = self.quote_ema(pos.Q)

        def cr(q):
            return Veff * BPS // q[1] if (q and q[1] > 0) else 0

        below = lambda q: q is None or Veff * BPS // q[1] < L
        run = 0
        for e in reversed(self.ring):                # newest entries below L (cached reprojections)
            if not below(self.quote_xy(pos.Q, e[3], e[4])):
                break
            run += 1
        ema_leg = run >= P.liq_persist_n             # persist::liq_ema_leg (pure; the M4R gate is not part of it)
        if not live:
            spot_leg = True                          # SPEC: no route -> CR_spot := 0
        else:
            spot_leg = lo_ok and (qrs is None or cr(qrs) < L)
        # M4R gate (ARCH §3.6 precondition 1b): while ema_warm > 0 on an executable venue nothing is liquidatable
        # for a keeper exit; try_exit returns before any evaluation (no arm change)
        gated = self.gated()
        return dict(R=R, Veff=Veff, qs=qs, qs_r=qs_r, qrs=qrs, qe=qe, p_live=p_live, p_rs=p_rs, lo_ok=lo_ok,
                    L_live=L_live, L_spot=L_spot, cr_s=cr(qs), cr_rs=cr(qrs) if qrs else None, cr_e=cr(qe),
                    liq=spot_leg and ema_leg and not gated, ema_leg=ema_leg, spot_leg=spot_leg, ema_run=run,
                    expired=t >= pos.deadline, gated=gated)

    def ema_liquidity(self) -> int:
        """L_ema', the guarded EMA liquidity (M4R, #96; bounds::ref_liquidity's EMA term): min(L_ema, L_mlo) while
        the post-change reference guard is set (L_mlo the lower median of the ring's bucket liquidity lows), else
        L_ema."""
        return min(self.liq, self.ring_medians()[2]) if self.guard else self.liq

    def spot_liquidity(self, L_live: int) -> int:
        """FITS-2 (a) (#101; PROPOSED; ECON §5.5, §8.2; ARCH §3.6-§3.8): the liquidity the CR-liquidatability spot
        leg is judged at, L_spot = max(L_live, L_floor) -- the robust spot P_rs repriced on the deeper of the live pool
        and the EMA liquidity (uncapped: the reference cap binds L_b's EMA terms only).  L_floor = L_ema, except while
        the post-change guard is set: then min(L_ema, L_mhi), L_mhi the upper median of the ring's bucket liquidity
        highs (AIR-17; bounds::spot_liquidity).  It used to be L_ema' = min(L_ema, L_mlo), the reference's guard: a
        bucket low is the minimum of its second, so an unlocked LP's [withdraw, crank_price, deposit] in 4 of every 8
        seconds set L_mlo to the drained depth even with the keeper observing each second, and a liquidation wrapped
        in the same withdrawal was judged there (positions in the spike-and-retreat band forced); one honest
        observation keeps a bucket high at the pool's depth.  An LP withdrawal in the same transaction lowers L_live
        but not L_floor, so it cannot make the spot leg true; the leg then needs the EMA liquidity itself to have
        fallen -- genuinely (after a held withdrawal each integrated second closes 1/6 of the gap L_ema - L_live,
        tau = 5 s at dt = 1, the clamp at most halving L_ema: if the spot leg binds, the liquidation waits until the
        gap is small enough) or by attacker-only seconds, which the keeper's per-second liquidity-anomaly coverage
        denies after the first (ECON §5.8 duty 2).  The coverage holds L_ema but cannot lift it: a low reached by a
        genuine held withdrawal and then frozen by withdraw-wrapped cranks stays in the leg and forces what the same
        LP's withdrawal for good forces (FITX-1, #102; switch_warmup.py part 5b (vii); ECON §12.12 residual 10).  A
        deposit can raise L_floor (at most to L_ema) and so delay the leg by at most the position's own depth effect,
        ~ (1 - L_live/L_floor)*q/x in price.  The EMA leg (the ring's cached reprojections at L_ema) and L_b are
        unchanged."""
        floor = min(self.liq, self.ring_medians()[3]) if self.guard else self.liq
        return max(L_live, floor)

    def ema_growth_bps(self) -> int:
        """Sustained EMA growth g: the LOWER MEDIAN (index (n - 1) // 2 of the sorted values) of the
        per-second growth rates (bps/s, floor) over the last n = kappa_growth_n integrations, floored at 0
        (a single held-pump second cannot open the cap; a majority of flat or falling steps keeps it shut).
        AIR-22: the minimum used before let one flat step in every n -- a low at or below P_ema recorded in a
        pump's bucket ([sell, crank_price, buy back]), which the directional aggregation turns into obs = P_ema
        -- hold g, and with it kappa_eff, at 0 through a genuine pump (observe::sustained_growth)."""
        n = self.P.kappa_growth_n
        if n == 0 or len(self.ring) < n + 1:
            return 0
        rates = []
        for (t0, p0, *_r0), (t1, p1, *_r1) in zip(self.ring[-n - 1:-1], self.ring[-n:]):
            dt = t1 - t0
            if dt <= 0 or p0 <= 0:
                return 0
            rates.append((p1 - p0) * BPS // (p0 * dt))
        rates.sort()
        return max(0, rates[(n - 1) // 2])

    def kappa_eff(self) -> int:
        P = self.P
        if P.kappa_base_bps >= P.kappa_liq_bps:
            return P.kappa_liq_bps
        return min(P.kappa_liq_bps, P.kappa_base_bps + (self.tier.ema_tau_s + 1) * self.ema_growth_bps())

    def ref_depth_cap(self) -> Optional[int]:
        """M4F-1 (ECON §8.2; bounds::ref_liquidity): on a Raydium venue -- and since M6R-1 (owner decision 2026-09-30,
        ECONOMICS.md #105) on a canonical PumpSwap pool, D from its burned LP -- the EMA terms of the exit reference
        liquidity are capped at the pool's locked depth D now, floor(D*1e4/lambda) at lambda = 100 %.  L_b = max(L_live,
        min(max(L_ema', L_slow), cap)); L_live itself is never capped, and as D <= L_live, L_b = L_live: no
        third-party deposit is credited to an exit.  On a canonical PumpSwap pool lambda_min = 100 % (OWNER-M6D-1,
        #106; the cap too since M6E-1: at 2D a deposit held into a deadline and pulled there held a D-sized expiry up
        for up to an hour, attacks/pumpswap_lp.py 3 (f) (iv)); on Raydium the cap was floor(D*1e4/pool_lock_min_bps),
        the most depth sizing credits, until AIR-14 found the same hold there (engine::bounds::ref_depth_cap).  None
        (no cap) on the curve, which has no D, and on a non-canonical model pool (locked_depth_venue)."""
        v = self.venue
        if not self.locked_depth_venue():
            return None
        return depth_at_lambda(v.locked_depth(), BPS)

    def exit_reference(self, pos: Position, st: dict, t: int, is_liq: bool):
        """(risk quote at the reference, delta_bps, P_ref) bounding a keeper-initiated
        buyback, or None if no fresh bucket is available (crank first)."""
        P = self.P
        if not st["lo_ok"]:
            return None
        lo = self.lb_lo
        # deepest liquidity -> lowest reference cost; M4R: while the post-change guard is set, the EMA term is
        # min(L_ema, L_mlo) (bounds::ref_liquidity), so a pushed-up L_ema cannot lift the reference; M4F-1: on
        # Raydium and PumpSwap the EMA terms never credit the pool with more than its locked depth D (ref_depth_cap;
        # AIR-14), so L_b = L_live there
        l_e = min(self.liq, self.ring_medians()[2]) if self.guard else self.liq
        l_ref = max(l_e, self.liq_slow)
        cap = self.ref_depth_cap()
        if cap is not None:
            l_ref = min(l_ref, cap)
        L_b = max(st["L_live"], l_ref)
        if is_liq:
            if P.exit_ref == "ema":
                pref = self.ema
            else:
                g = self.ema_growth_bps()
                lo_x = lo * (BPS + g * max(0, t - self.lb_sec)) // BPS
                ke = self.kappa_eff()
                pref = min(max(self.ema, lo_x), self.ema * (BPS + ke) // BPS)
                if P.ref_cap_lo:                      # v0.4.1: also cap at the (honest) bucket low (R4B-2)
                    pref = min(pref, lo_x * (BPS + ke) // BPS)
            delta = P.delta_liq_bps                   # fixed (v0.4)
        else:
            pref = min(self.ema, min(e[1] for e in self.ring), lo)
            delta = P.delta_exp_bps
        q = self.quote_repriced(pos.Q, pref, L_b)
        if q is None:
            return None
        return q, delta, pref

    # --- settlement predicates on the box --------------------------------------------
    def _s1_at(self, pos, K, p, L):
        q = self.quote_repriced(pos.Q, p, L)
        return q is None or q[1] + pos.ert > pos.V - K

    def _s2_at(self, pos, p, L):
        return self.slip_repriced(pos.Q, p, L) > pos.slip_max

    def _legs(self, pos: Position, st: dict, box, K: int):
        """S1 / S2 legs: live (risk fee), robust spot, and the EMA leg on the whole box
        (the corner least favourable to the leg: lowest price, deepest liquidity)."""
        P_lo, P_hi, L_lo, L_hi = box
        v = self.venue
        qs_r = st["qs_r"]
        s1 = ((qs_r is None or qs_r[1] + pos.ert > pos.V - K)
              and self._s1_at(pos, K, st["p_rs"], st["L_live"])
              and self._s1_at(pos, K, P_lo, L_hi))
        s2 = (self.slip_at(pos.Q, *v.eff(), live=True) > pos.slip_max
              and self._s2_at(pos, st["p_rs"], st["L_live"])
              and self._s2_at(pos, P_lo, L_hi))
        return s1, s2

    def _refuted(self, pos: Position, st: dict, box, K: int) -> bool:
        """Refutation must hold on the whole box: at the corner least favourable to it
        (highest price; lowest liquidity, and for the S2 part also <= live liquidity,
        which an atomic LP deposit cannot raise).  One attacker-only second cannot
        refute, because the median corner does not move (R3A-6a)."""
        P_lo, P_hi, L_lo, L_hi = box
        s1_hi = self._s1_at(pos, K, P_hi, L_lo)
        L2 = min(st["L_live"], L_lo) if self.venue.has_price() else L_lo
        s2_hi = self._s2_at(pos, P_hi, L2)
        return not (s1_hi or s2_hi)

    def _not_liq_box(self, pos: Position, st: dict, box) -> bool:
        """'Not liquidatable' disarms only if box-robust: the EMA leg failed AND the
        corner most favourable to liquidation (highest price, lowest liquidity) is
        above L as well."""
        if st["ema_leg"]:
            return False
        P_lo, P_hi, L_lo, L_hi = box
        q = self.quote_repriced(pos.Q, P_hi, L_lo)
        return q is not None and st["Veff"] * BPS // q[1] >= pos.L

    def fv_parts(self, pos: Position, st: dict, p: int, L: int):
        """(FV, B_FV, p') on the state (p, L): FV = B_FV + ceil(p'*B_FV/1e4) + R_due (D3).
        p' (v0.4.1, R4A-3) is decided on the SAME state, never on the raw legs: the locked
        penalty unless the position is expired AND CR(V_eff, B_FV) >= L at (p, L), in which
        case the expiry fee (SPEC §4 reading, O15).  A contrary low observation (which
        fails the robust-spot leg) therefore cannot switch the reading."""
        if self.migrating():
            # pump-final §5.4: the pool migrate_v2 will create, (X1, Y1) = migrate_preview(), repriced at p' on its own
            # liquidity -- p' = p*rho rounded up after a pool part, rho = P(X1, Y1)/P(X0, Y0), (X0, Y0) the same
            # curve's standard pool (206.9M, rQ - fee) -- at its PumpSwap tier capped at the hard cap
            pool = self.venue.migrate_preview()
            p1 = self._migrating_price(pool, p)
            s = reprice(pool.sqrt_k(), p1) if p1 is not None else None
            q = pool.quote_buy_on(pos.Q, *s, cap_ppm=self.cap_ppm()) if s else None
        else:
            q = self.quote_repriced(pos.Q, p, L)
        if q is None:
            return None
        b = q[1]
        pen = pos.pen_bps
        if self.P.pprime_raw:                  # v0.4 comparison rows: p' from the raw 'CR-liquidatable' flag
            if not st["liq"]:
                pen = self.P.expiry_fee_bps
        elif st["expired"] and st["Veff"] * BPS // b >= pos.L:
            pen = self.P.expiry_fee_bps
        return b + cdiv(pen * b, BPS) + st["R"], b, pen

    def _migrating_price(self, pool: "CPPool", p: int) -> Optional[int]:
        """FV's price on a migrating curve (pump-final §5.4; shortwin_math::quote::migrating_fv_price): p itself
        without a pool part (pcQ = 0: bit-identical to the FV before pump's v3), else p' = ceil(ceil(p*Y1/Y0)*X0/X1)
        with (X1, Y1) = pool.eff() and (X0, Y0) = (206.9M, rQ - pool_migration_fee); None where the crate's is
        (Y0 = 0, X1 = 0, above u128)."""
        v = self.venue
        if not getattr(v, "pcq", 0):
            return p
        x1, y1 = pool.eff()
        x0, y0 = PUMP_POOL_TOKENS, v.rs - PUMP_MIGRATION_FEE
        if y0 <= 0 or x1 <= 0:
            return None
        scaled = cdiv(p * y1, y0)
        p1 = cdiv(scaled * x0, x1)
        return p1 if max(scaled, p1) < 2**128 else None

    def fv_at(self, pos: Position, st: dict, p: int, L: int):
        r = self.fv_parts(pos, st, p, L)
        return None if r is None else r[0]

    def _s3_due(self, pos: Position, st: dict, t: int) -> bool:
        P = self.P
        if not self.s3_no_route() or self.no_route_since is None:
            return False
        if not (st["liq"] or st["expired"]):
            return False
        if self.migrating():
            paused_for = t - (self.complete_t if self.complete_t is not None else t)
            if paused_for < P.max_migration_pause_s:
                return False
            mig_start = self.complete_t + P.max_migration_pause_s
        else:
            mig_start = -10**12
        start = max(self.no_route_since, (self.graduated_t if self.graduated_t is not None else -10**12)
                    + P.grad_grace_s, mig_start)
        return t >= start + P.s3_wait_s

    # --- exits -----------------------------------------------------------------------
    def try_exit(self, pos: Position, t: int, borrower_close: bool = False, borrower_limit: Optional[int] = None,
                 settle_only: bool = False):
        """Keeper liquidation / expiry / settlement, or borrower close, at time t.
        Returns a result dict, or None if nothing executes now.
        settle_only=True models a standalone sol_settle call (no buyback attempted).
        Every result is terminal: the position leaves the market aggregates (#99, _release)."""
        r = self._try_exit(pos, t, borrower_close, borrower_limit, settle_only)
        if r is not None:
            self._release(pos)
        return r

    def _try_exit(self, pos: Position, t: int, borrower_close: bool = False, borrower_limit: Optional[int] = None,
                  settle_only: bool = False):
        self.advance(t)
        if self.migrated_pool is not None and not settle_only:
            # M6 as built (ARCH §2.5): a close, liquidate or expire whose own prologue would move the market onto the
            # pool refuses VenueMigrating and the transition reverts with it (the second's crank performs it)
            return None
        synced = self.synced_at
        self.observe(t)
        if self.synced_at != synced:
            return None                             # a sol_settle that moved the market evaluates nothing (ARCH §2.5)
        P = self.P
        v = self.venue
        spot_before = v.spot_wad() if v.has_price() else self.ema
        st = self.status(pos, t)
        V, Q = pos.V, pos.Q
        ert = pos.ert
        res = dict(t=t, spot=spot_before, B=0, rent=0, pen=0, fee=0, keeper=0, protocol=0,
                   lender_sol=0, lender_lump=0, tokens=0, borrower=0, kind=None, cr_s=st["cr_s"],
                   cr_e=st["cr_e"], bound=None, base=None, fv=None)

        if borrower_close:
            qs = st["qs"]
            R = st["R"]
            if qs is None or qs[1] + R + ert > V:
                return None
            if borrower_limit is not None and qs[1] > borrower_limit:
                return None
            B = self._buy_back(Q, t)
            rp = R * pos.p_rent // BPS
            res.update(kind="close", B=B, rent=R, protocol=rp, lender_sol=R - rp, tokens=Q,
                       borrower=V - B - R)
            return res

        if self.gated():
            # M4R gate: liquidate / expire fail (EmaWarmingUp), sol_settle evaluates nothing -- no arm change
            return None
        K = self.settle_reward(V)
        box = self.settle_box(t)
        if not (st["liq"] or st["expired"]):
            # R3A-6b / V2-4: a non-liquidatable evaluation disarms only on box-robust
            # evidence; otherwise a live clock keeps running.  AIR-16 (ECON §5.6): a lapsed
            # clock (no evaluation for more than settle_arm_gap_s) is not revived here with its
            # old count -- _settlement re-arms it.
            if pos.armed:
                if self._not_liq_box(pos, st, box):
                    pos.disarm()
                elif t - pos.s_last <= P.settle_arm_gap_s:
                    pos.s_last = t
            return None

        if not P.synthetic and P.two_leg and not settle_only and v_is_straddle(v, Q):
            # The retired straddle bundle (synthetic=False, comparison rows only; M6 as built, ARCH §5.4 tx3,
            # OWNER-M6R-4): the keeper's tx1 its own completion of the curve, tx2 migrate_v2 and tx3 the transition
            # (a crank on the curve's ReadSet) plus a crank on the pool's (its first bucket).  tx3 carried no exit --
            # the transition starts the post-change warm-up -- and the keeper exited on the pool once the warm-up
            # had passed.  Since pump-final §6 (synthetic) the keeper's liquidate / expire below buys q > rT itself
            # in one instruction, pump's synthetic migration (_buy_back), priced B_v3
            self._straddle_to_pool(t)
            return None

        if v.has_route() and not settle_only:
            qs = st["qs"]
            buyback_ok = (qs is not None and qs[1] + ert <= V - K
                          and self.slip_at(Q, *v.eff(), live=True) <= pos.slip_max)
            if buyback_ok:
                r = self._buyback_exit(pos, st, t, res, K)
                if r is not None:
                    return r
        return self._settlement(pos, st, t, K, res, box)

    def _buyback_exit(self, pos, st, t, res, K):
        if self.gated():                             # M4R gate (try_exit returns before; direct callers)
            return None
        P = self.P
        V, Q = pos.V, pos.Q
        ert = pos.ert
        is_liq = st["liq"]
        ref = self.exit_reference(pos, st, t, is_liq)
        if ref is None:
            return None
        qref, delta, pref = ref
        b_ref = qref[1]
        R = st["R"]
        qs = st["qs"]
        # rent is senior to the buyback wherever the fair (reference) cost leaves room for it (C0-1 (3)).
        # v0.4.1 (R4A-1 / R4B-1): the v0.4 penalty reserve is withdrawn -- it blocked honest liquidations
        # in genuine fast pumps (live cost a few tenths of a percent above the lagging reference).
        rate = pos.pen_bps if is_liq else P.expiry_fee_bps
        bound = b_ref * (BPS + delta) // BPS
        mode = P.liq_reserve if is_liq else P.exp_reserve
        if mode == "none":
            senior = 0
            max_in = min(bound, V - K - ert)
        else:
            senior = R + (cdiv(rate * b_ref, BPS) if mode in ("rent+penalty", "rent+fee") else 0)
            max_in = min(bound, V - K - ert, max(V - senior - ert, b_ref * (BPS + P.exec_tol_bps) // BPS))
        if qs[1] > max_in:
            if qs[1] <= min(bound, V - K - ert):
                self.reserve_waits = getattr(self, "reserve_waits", 0) + 1   # ghost: waits caused by a reserve
            return None                         # wait: buyback above the reference bound
        b_quote = qs[1]
        v = self.venue
        tol = 5
        if v_is_straddle(v, Q) and P.synthetic:
            # pump-final §5.2's bound on B_v3 - charge: (1 + sum ceil(b_i/1e4)) + (D2 + sum ceil(D2*b_i/1e4)),
            # D2 = 1 + ceil(q2/(X_s - q2)) over the non-zero fee components (<= 7 for q2 <= X_s/2)
            q2, x_s = Q - v.rt, v.base_ata - v.rt
            d2 = 1 + cdiv(q2, x_s - q2)
            bs = [b for b in v.fees() if b > 0]
            tol = (1 + sum(cdiv(b, BPS) for b in bs)) + (d2 + sum(cdiv(d2 * b, BPS) for b in bs))
        B = self._buy_back(Q, t)
        if P.check_invariants and not (b_quote - tol <= B <= b_quote):
            raise InvariantError(("venue charge outside [quote-tol, quote]", B, b_quote, tol))
        base = min(st["qs_r"][1], b_ref)            # fee base on quoted (risk) costs
        W = V - B
        r_paid = min(R, W)
        W -= r_paid
        rp = r_paid * pos.p_rent // BPS
        res.update(B=B, rent=r_paid, tokens=Q, bound=bound, base=base, b_ref=b_ref, max_in=max_in, senior=senior,
                   delta=delta)
        if is_liq:
            pen = min(cdiv(pos.pen_bps * base, BPS), W)
            W -= pen
            kp = pen * P.pen_keeper_bps // BPS
            pp = pen * P.pen_protocol_bps // BPS
            top = 0
            if P.liq_keeper_parity:                 # R3B-10: keeper reward >= K_s, top-up from the borrower
                top = min(max(0, K - kp), W)
                W -= top
            res.update(kind="liquidation", pen=pen, keeper=kp + top, keeper_topup=top, protocol=rp + pp,
                       lender_sol=(r_paid - rp) + (pen - kp - pp), lender_lump=pen - kp - pp, borrower=W)
        else:
            fee = min(cdiv(P.expiry_fee_bps * base, BPS), W)
            W -= fee
            ke = min(self.expiry_keeper(b_ref), W)
            W -= ke
            res.update(kind="expiry", fee=fee, keeper=ke, protocol=rp, lender_sol=(r_paid - rp) + fee,
                       lender_lump=fee, borrower=W)
        if P.check_invariants:
            self.check_exit(pos, res, st, K)
        return res

    def _settlement(self, pos, st, t, K, res, box):
        """sol_settle: S1/S2 on one eligibility clock, S3 when due.  Every decision is
        taken on the settlement box (ECON §5.6)."""
        P = self.P
        v = self.venue
        if self.gated():                             # M4R gate: evaluate() is a no-op (S3 cannot be due: the
            return None                              # adoption cleared the no-route chain)
        if v.has_route():
            armed = pos.armed and t - pos.s_last <= P.settle_arm_gap_s
            if pos.armed and self._refuted(pos, st, box, K):
                pos.disarm()
                armed = False
            elif st["lo_ok"]:
                s1, s2 = self._legs(pos, st, box, K)
                if s1 or s2:
                    if not armed:
                        pos.s_kind, pos.s_count, pos.s_t = "armed", self.ema_count, t
                    pos.s_last = t
                    if self.ema_count - pos.s_count >= P.settle_persist_n:
                        r = self._settle(res, "S1" if s1 else "S2", pos, K, st, box)
                        if r is not None:
                            return r
                elif armed:
                    pos.s_last = t                  # not refuted: the clock keeps running
            elif armed:
                pos.s_last = t                      # stale bucket: no evidence either way
        if self._s3_due(pos, st, t):
            return self._settle(res, "S3", pos, K, st, box)
        return None

    def _settle(self, res: dict, kind: str, pos: Position, K: int, st: dict, box) -> Optional[dict]:
        """SOL settlement: token debt extinguished; lenders get V - K_s, capped at FV in
        S2/S3 (D3).  The amount must agree on the whole box within fv_dev_bps, else the
        settlement waits (no state change)."""
        take = pos.V - K
        borrower = 0
        fv = None
        lump = 0
        if kind in ("S2", "S3") and self.P.fv_cap:
            P_lo, P_hi, L_lo, L_hi = box
            f_lo = self.fv_at(pos, st, P_lo, L_hi)       # each corner embeds its own p' (R4A-3)
            f_hi = self.fv_at(pos, st, P_hi, L_lo)
            x_lo = take if f_lo is None else min(take, f_lo)
            x_hi = take if f_hi is None else min(take, f_hi)
            if (x_hi - x_lo) * BPS > self.P.fv_dev_bps * x_lo:
                return None                          # amount not box-consistent: wait
            fv = self.fv_at(pos, st, self.ema, self.liq)
            if fv is not None and take > fv:
                borrower, take = take - fv, fv
            res["dev_bps"] = (x_hi - x_lo) * BPS // max(x_lo, 1)
            if self.P.check_invariants and not (x_lo - 2 <= take <= x_hi + 2):   # +-2: reprice rounding
                raise InvariantError(("S2/S3 amount outside the box", x_lo, take, x_hi))  # I-E5
        if kind == "S1" or (kind in ("S2", "S3") and self.P.fv_cap):
            # v0.4.1 (R4B-6, R4A-5): only the zero-impact value of Q at P_ema plus the rent due is
            # immediate; the rest of the take (impact, fees, penalty) vests like a lump.  AIR-21: S1
            # too -- its legs need only B + ert > V - K_s, so V - K_s can exceed Q*P_ema + R_due by the
            # buyback's impact and fees, which a just-in-time deposit took at once; a genuine S1 loss
            # (V - K_s <= Q*P_ema + R_due) vests nothing.  AIR-09: a venue that cannot execute records
            # nothing, so P_ema is its pre-halt value and the tokens have no market value to pay at
            # once: they count 0, all but the rent vests.
            v_ema = pos.Q * self.ema // WAD if self.venue.has_route() else 0
            lump = take - min(take, v_ema + st["R"])
        res.update(kind=kind, keeper=K, lender_sol=take, lender_lump=lump, borrower=borrower, fv=fv)
        if self.P.check_invariants:
            if res["keeper"] + res["lender_sol"] + res["borrower"] != pos.V:
                raise InvariantError(("settle legs", res))
            if kind == "S1" and take != pos.V - K:
                raise InvariantError(("S1 capped", res))
            if kind == "S1" and not self._s1_at(pos, K, box[0], box[3]):
                raise InvariantError(("S1 without box evidence", res))
        return res

    def check_exit(self, pos: Position, res: dict, st: dict, K: int) -> None:
        """Economic invariants of a keeper-initiated buyback (ECON §13)."""
        V = pos.V
        legs = res["B"] + res["rent"] + res["pen"] + res["fee"] + res["borrower"] + res.get("keeper_topup", 0)
        if res["kind"] == "expiry":
            legs += res["keeper"]
        if legs != V or min(res["rent"], res["pen"], res["fee"], res["borrower"]) < 0:
            raise InvariantError(("waterfall", res, V))
        if res["B"] > res["bound"] or res["B"] > res["max_in"]:          # I-E1
            raise InvariantError(("exit above bound", res))
        if res["base"] > res["b_ref"]:                                   # I-E3
            raise InvariantError(("fee base above quoted reference", res))
        eps = res["b_ref"] * (BPS + self.P.exec_tol_bps) // BPS          # I-E2
        want = cdiv((pos.pen_bps if res["kind"] == "liquidation" else self.P.expiry_fee_bps) * res["base"], BPS)
        got = res["pen"] if res["kind"] == "liquidation" else res["fee"]
        mode = self.P.liq_reserve if res["kind"] == "liquidation" else self.P.exp_reserve
        if mode != "none" and V - res["senior"] - pos.ert >= eps and res["rent"] != st["R"]:
            raise InvariantError(("rent shortfall despite reserve", res))
        if mode in ("rent+penalty", "rent+fee") and V - res["senior"] - pos.ert >= eps and got != want:
            raise InvariantError(("penalty / fee truncated despite reserve", res, want))
        if got > want:
            raise InvariantError(("penalty/fee above rate x reference", res, want))
        if res["kind"] == "liquidation" and self.P.liq_keeper_parity and res["borrower"] > 0 \
                and res["keeper"] < K:                                    # I-E10 (R3B-10)
            raise InvariantError(("liquidation keeper below K_s with borrower equity left", res))

    def _straddle_to_pool(self, t: int) -> CPPool:
        """RETIRED (pump-final D2): reached only with synthetic=False (ECON §10.2's comparison rows); since pump's v3
        a live curve's q > rT is one buy_v3 (_buy_back, PumpCurve.buy).
        The straddle bundle's first three transactions (V1-9; M6 as built, ECONOMICS.md #104, #105; ARCH §5.4):
        tx1 the completer (the keeper, or the borrower's client) buys the curve's rT with its own SOL and keeps the
        tokens (keeper_straddle_cost), tx2 migrate_v2 creates the canonical pool and starts its boost, tx3's crank on
        the curve's ReadSet moves the market onto it (_graduate; this second's bucket was cleared by the rewrite) and
        the next instruction in tx3 -- the keeper's crank on the pool's ReadSet, or the borrower's close_short --
        records the pool's first bucket at t in its prologue."""
        v = self.venue
        self.keeper_straddle_cost = v.buy(v.rt) if v.rt else 0
        pool = v.migrate()
        pool.start_boost(t)
        self._graduate(pool, t)
        self.observe(t)
        return pool

    def _buy_back(self, Q: int, t: int) -> int:
        v = self.venue
        if v_is_straddle(v, Q) and not self.P.synthetic:
            # the retired borrower's own >= 3-transaction bundle (synthetic=False, comparison rows; a close, ECON
            # §10.2): tx3 = the pool's accounts, the transition crank and its close_short, which the warm-up does
            # not gate: the position bought all of Q on the new pool at once (B = B_str)
            return self._straddle_to_pool(t).buy(Q)
        # pump's v3 (pump-final §5.2): Q > rT is one buy_v3 by synthetic migration (PumpCurve.buy), the curve
        # completes in this instruction and the market is migrating (observe: the curve's final state is recorded)
        return v.buy(Q)


def v_is_straddle(v: Venue, Q: int) -> bool:
    """A buyback of Q on a live curve with Q > rT: pump's v3 executes it by synthetic migration (pump-final §5.2); before
    v3 pump refused it (6021) and only the retired straddle bundle executed it (synthetic=False)."""
    return v.kind == "curve" and not v.complete and Q > v.rt


# ----------------------------------------------------------------------------
# Scenario runner
# ----------------------------------------------------------------------------
@dataclass
class Scenario:
    name: str
    path: Callable[[int], Optional[float]]     # t -> price multiple of post-open spot (None = no push)
    t_event: int = 3600
    window: int = 3600
    events: tuple = ()                           # ((t, fn(market)), ...)
    close_at: Optional[int] = None
    horizon: Optional[int] = None               # default: event + window
    note: str = ""


def keeper_near(m: Market, pos: Position, t: int) -> bool:
    """In-house keeper duty (ECON §5.8): crank every second while a position is
    near a threshold, spot and EMA disagree, or expiry is close."""
    if t >= pos.deadline - 60:
        return True
    v = m.venue
    if not v.has_route():
        return True
    if abs(v.spot_wad() * BPS // max(m.ema, 1) - BPS) > 200:
        return True
    qs = m.quote_spot(pos.Q)
    R = m.rent_due(pos)
    return qs is None or (pos.V - R) * BPS // qs[1] < pos.L * 13 // 10


def run(market: Market, size_frac: float, sc: Scenario, keeper_latency: int = 1,
        calm_step: int = 30, honest: bool = True, skip_to: Optional[int] = None):
    """skip_to: fast-forward a calm period in one step (rent accrues on the constant EMA);
    used for aged positions (§5.7 battery, R4A-1)."""
    Q = max(1, int(market.max_open_q() * size_frac))
    market._q_hint = Q
    pos = market.open(Q, 0)
    P1 = pos.spot_post
    horizon = sc.horizon or (sc.t_event + sc.window)
    horizon = min(horizon, pos.deadline + 3 * 3600)
    events = sorted(sc.events, key=lambda e: e[0])
    ei = 0
    t = 0
    if skip_to:
        for tt in range(1, 12):
            market.step_to(tt)
        market.step_to(skip_to)
        t = skip_to
    act_at: Optional[int] = None
    result = None
    near = True
    while t < horizon:
        in_window = sc.t_event - 5 <= t <= sc.t_event + sc.window
        if in_window or act_at is not None or near:
            t += 1
        else:
            nxt = t + calm_step
            if t < sc.t_event - 5:
                nxt = min(nxt, sc.t_event - 5)
            if t < pos.deadline - 60:
                nxt = min(nxt, pos.deadline - 60)
            t = max(nxt, t + 1)
        # other traders act first in the second; then the keeper cranks / acts
        while ei < len(events) and events[ei][0] <= t:
            events[ei][1](market)
            ei += 1
        mult = sc.path(t)
        if mult is not None:
            market.exo_move(int(P1 * mult))
        if sc.close_at is not None and t >= sc.close_at:
            result = market.try_exit(pos, t, borrower_close=True)
            if result:
                break
        if honest:
            market.step_to(t)
        else:
            market.advance(t)
        st = market.status(pos, t)
        near = keeper_near(market, pos, t)
        if act_at is None and (st["liq"] or st["expired"]):
            act_at = t + keeper_latency
        if act_at is not None and t >= act_at:
            result = market.try_exit(pos, t)
            if result:
                break
            st = market.status(pos, t)
            if not (st["liq"] or st["expired"]):
                act_at = None
    if result is None:
        market.step_to(t)
        st = market.status(pos, t)
        result = dict(kind="open", t=t, spot=market.venue.spot_wad() if market.venue.has_route() else market.ema,
                      B=0, rent=0, pen=0, fee=0, keeper=0, protocol=0, lender_sol=0, tokens=Q, borrower=0,
                      cr_s=st["cr_s"], cr_e=st["cr_e"], mtm_close=st["qs"][1] + st["R"] if st["qs"] else None)
    t_end = sc.t_event + sc.window
    m_end = sc.path(t_end) if sc.t_event < 10**8 else None
    result["p_term"] = int(P1 * m_end) if m_end is not None else result["spot"]
    return pos, result, P1


def summarize(pos: Position, r: dict, P1: int):
    """Borrower P&L, lender outcome valued at the scenario's terminal price."""
    Q = pos.Q
    p_post = r.get("p_term", r["spot"])
    token_value = Q * p_post // WAD
    if r["kind"] == "open":
        mtm = r.get("mtm_close")
        borrower_pnl = (pos.V - mtm - pos.fee_open - pos.M) if mtm is not None else None
        lender_pnl = 0
    else:
        borrower_pnl = r["borrower"] - pos.M - pos.fee_open
        lender_pnl = r["lender_sol"] - (Q - r["tokens"]) * p_post // WAD
    return dict(
        kind=r["kind"], t=r["t"], mult=r["spot"] / P1 if P1 else 0, term=p_post / P1 if P1 else 0,
        cr_s=r["cr_s"] / BPS, cr_e=r["cr_e"] / BPS,
        V=pos.V, M=pos.M, B_open=pos.B_open, token_value=token_value,
        borrower_pnl=borrower_pnl, lender_sol=r["lender_sol"], tokens=r["tokens"],
        lender_pnl=lender_pnl, lender_pnl_pct=(100 * lender_pnl / token_value) if token_value else 0,
        keeper=r["keeper"], protocol=r["protocol"] + pos.fee_open,
    )


# ----------------------------------------------------------------------------
# Market factories (example venues)
# ----------------------------------------------------------------------------
def venue_tier_A():   # ~5,000 SOL liquidity (2Y); ~$606k at $121.2/SOL
    return CPPool(150_000_000_000_000, 2_500 * LAMPORTS_PER_SOL, PUMPSWAP_CANONICAL_FEES)


def venue_tier_B():   # ~2,000 SOL liquidity; ~$242k
    return CPPool(200_000_000_000_000, 1_000 * LAMPORTS_PER_SOL, PUMPSWAP_CANONICAL_FEES)


def venue_tier_C():
    """Freshly graduated pump token AFTER the boost was spent (opens are blocked
    while the boost vault is active): mainnet CreatePoolEvent numbers."""
    p = CPPool.graduate(84_990_359_345)
    p.run_boost_to_end()
    return p


def venue_tier_D(progress: float):
    return PumpCurve.at_progress(progress)


def venue_ray(X_tokens: int, Y_sol: float, fee_ppm: int, out_share_ppm: int, name: str, lock_bps: int = 5000):
    return RaydiumPool(X_tokens, int(Y_sol * LAMPORTS_PER_SOL), fee_ppm=fee_ppm, out_share_ppm=out_share_ppm,
                       name=name, lock_bps=lock_bps)


MARKETS = {
    "A": lambda: venue_tier_A(),
    "B": lambda: venue_tier_B(),
    "C": lambda: venue_tier_C(),
    "D@0%": lambda: venue_tier_D(0.0),
    "D@50%": lambda: venue_tier_D(0.5),
    "D@80%": lambda: venue_tier_D(0.8),
    "D@90%": lambda: venue_tier_D(0.9),
    "D@97%": lambda: venue_tier_D(0.97),
    # Raydium examples (D2).  Fee configs are EXAMPLES (AMM v4 25 bps with 12 % pnl share; CPMM
    # 0.25 % with 16 % protocol+fund share).  lambda = 50 % locked LP: the WORST adoptable pool at
    # pool_lock_min_bps (the D2 target coins' main pools have 92-99.8 %, R-RAY §6.4).
    "RA": lambda: venue_ray(150_000_000_000_000, 2_500, 2_500, 120_000, "AMMv4"),
    "RB": lambda: venue_ray(200_000_000_000_000, 1_000, 2_500, 160_000, "CPMM"),
    "RC": lambda: venue_ray(PUMP_POOL_TOKENS, 84.99, 2_500, 160_000, "CPMM"),
}


def tier_of(key: str) -> Tier:
    return TIERS[key[1]] if key.startswith("R") else TIERS[key[0]]


def new_market(key: str, P: Params = DEFAULT, util_bps: int = 8000, tier: Optional[Tier] = None) -> Market:
    return Market(MARKETS[key](), tier or tier_of(key), P, util_bps)


# ----------------------------------------------------------------------------
# Scenario library
# ----------------------------------------------------------------------------
def noisy_path(path, j, seed):
    """Deterministic per-second price noise +-j around a path (R4B-1): the keeper's observation
    and the exit see the noisy price, the reference the previous second's, so live and
    reference differ by up to ~2j, as on a real pool within a second."""
    rng = random.Random(seed)
    cache = {}

    def f(t):
        base = path(t)
        if base is None:
            return None
        if t not in cache:
            cache[t] = 1 + rng.uniform(-j, j)
        return base * cache[t]
    return f


def step_path(t_e, k):
    return lambda t: (k if t >= t_e else None)


def ramp_path(t_e, k, dur):
    g = math.log(k) / dur

    def f(t):
        if t < t_e:
            return None
        return math.exp(g * min(t - t_e, dur))
    return f


def spike_path(t_e, k, dur):
    return lambda t: (k if t_e <= t < t_e + dur else (1.0 if t >= t_e + dur else None))


def drain(frac_bps, force=False):
    def f(m: Market):
        if m.venue.lp_mutable:
            m.venue.remove_liquidity(frac_bps, force=force)
    return f


def disable_pool(m: Market):
    if isinstance(m.venue, (CPPool, RaydiumPool)):
        m.venue.disabled = True


def flag_route(m: Market):
    m.route_broken = True


def fast_path_events(t0: int, cand_frac_bps: int, horizon: int = 900):
    """Raydium incumbent halted at t0; the keeper's candidate is the pool at cand_frac_bps of the incumbent's reserves
    (lambda 90 %), trading at the market price.  Once pool_fail has held for pool_fast_route_s the keeper proposes it:
    the window opens if it passes the window floor at min(P_ema, P_cand) and every open position fits its opening
    caps (#99, Market.carried_fit at min(P_ema, P_cand)); otherwise the proposal is refused and re-tried every second.
    pool_fast_window_s after the window opened, execute_pool_switch adopts it (Market.adopt_pool: the M4R post-change
    confirmation; exits resume after the 8-bucket warm-up) if the chain that armed the window holds, the window floor
    passes at P_ema and the positions still fit at P_ema; otherwise the window is cancelled.  A candidate that never
    fits leaves the positions on the halted pool: healthy ones frozen until their deadline, then S3 at fair value.
    (Per-market state lives on the Market object, so the scenario can be reused.)"""
    def halt(m: Market):
        v = m.venue
        m._fp = RaydiumPool(v.X * cand_frac_bps // BPS, v.Y * cand_frac_bps // BPS, fee_ppm=v.fee_ppm,
                            out_share_ppm=v.out_share_ppm, name=v.name + "'", lock_bps=9000)
        m._fp_t0 = None
        v.disabled = True

    def maybe_switch(m: Market):
        P = m.P
        c = getattr(m, "_fp", None)
        if c is None or m.pool_fail_since is None:
            return
        if m._fp_t0 is None:
            if m.t >= m.pool_fail_since + P.pool_fast_route_s:
                c.move_to_price(m.ema)               # candidate trades at the market price
                p_cand = c.spot_wad()
                if window_floor_ok(c.locked_depth(), m.ema, p_cand) and m.carried_fit(c):
                    m._fp_t0 = m.t                   # the window opens (else refused: PoolTooThin /
            return                                   # CarriedPositionsTooLarge; the keeper re-proposes)
        if m.t >= m._fp_t0 + P.pool_fast_window_s:
            chain = m.pool_fail() and m.pool_fail_since + P.pool_fast_route_s <= m._fp_t0
            if chain and position_floor_ok(c.locked_depth(), m.ema) and m.carried_fit(c, execution=True):
                m.adopt_pool(c, m.t)                 # M4R: anchor cap, warm-up, guard (v0.4.1: fast_switch)
                m._fp = None
            else:
                m._fp_t0 = None                      # cancelled; the chain stays armed
    return ((t0, halt),) + tuple((t0 + i, maybe_switch) for i in range(1, horizon))


TE = 3600
SCENARIOS = [
    Scenario("gap 2x", step_path(TE, 2.0)),
    Scenario("gap 5x", step_path(TE, 5.0)),
    Scenario("gap 10x", step_path(TE, 10.0)),
    Scenario("pump 10x/60s", ramp_path(TE, 10.0, 60)),
    Scenario("pump 10x/10min", ramp_path(TE, 10.0, 600)),
    Scenario("pump 3x/1h", ramp_path(TE, 3.0, 3600), window=4000),
    Scenario("spike 10x/5s", spike_path(TE, 10.0, 5), window=600),
    Scenario("drain all unlocked LP (50%)", lambda t: None, events=((TE, drain(5000)),), window=600),
    Scenario("drain 50% + x1.5", step_path(TE + 1, 1.5), events=((TE, drain(5000)),), window=600),
    Scenario("drain 50% + pump 2x/120s", ramp_path(TE + 60, 2.0, 120), events=((TE, drain(5000)),), window=900),
    Scenario("pool disabled", lambda t: None, events=((TE, disable_pool),), window=3600),
    Scenario("route flagged (Emergency) + gap 2x", step_path(TE + 1, 2.0), events=((TE, flag_route),)),
    Scenario("dump -50%, close", ramp_path(TE, 0.5, 3600), close_at=2 * TE, window=3700),
    Scenario("flat to expiry", lambda t: None, t_event=10**9, window=0, horizon=10**9),
]
SC = {s.name: s for s in SCENARIOS}
# #99: a position at the incumbent's size cap (q 0.75 % of X on RA / RB / RC, lambda 50 %) fits a candidate only if
# the X0 an open would have on it right after the adoption -- min(X, min(L_slow, D)/sqrt(P_ema), D/(lambda_min*
# sqrt(P_ema))) -- reaches the incumbent's, i.e. the candidate's locked depth D covers the incumbent's L_slow (its
# whole depth): the 120 % pool at lambda 90 % (D = 1.08 L_slow) carries it; the 50 % (0.45) and 25 % (0.225) pools
# are refused and the position stays on the halted pool (frozen until its deadline, then S3 at fair value).  Index 1
# is the adopting replay the math crate's vectors trace (gen_settle, venue_switch.py (a)).
FAST_SCENARIOS = [
    Scenario("halt -> fast path to a 50% pool, then pump 2x/120s", ramp_path(TE + 300, 2.0, 120),
             events=fast_path_events(TE, 5000), window=1500),
    Scenario("halt -> fast path to a 120% pool, then pump 2x/120s", ramp_path(TE + 300, 2.0, 120),
             events=fast_path_events(TE, 12000), window=1500),
    Scenario("halt -> fast path to a 25% pool, then pump 2x/120s", ramp_path(TE + 300, 2.0, 120),
             events=fast_path_events(TE, 2500), window=1500),
]


def complete_curve(m: Market):
    """One exogenous buy takes all remaining real tokens -> curve completes."""
    v = m.venue
    if isinstance(v, PumpCurve) and not v.complete and v.rt > 0:
        v.buy(v.rt)


def leave_real_reserves(frac_of_q: float):
    """Exogenous buys until real_token_reserves = frac_of_q * (size-at-cap Q)."""
    def f(m: Market):
        v = m.venue
        if isinstance(v, PumpCurve) and not v.complete:
            target = int(m._q_hint * frac_of_q)
            if v.rt > target:
                v.buy(v.rt - target)
    return f


def amm_ramp_events(t0: int, k: float, dur: int):
    step = k ** (1 / dur)

    def mk():
        def f(m: Market):
            if isinstance(m.venue, CPPool) and m.venue.has_route():
                m.venue.move_to_price(int(m.venue.spot_wad() * step))
        return f
    return tuple((t0 + i, mk()) for i in range(dur))


GRAD_SCENARIOS = [
    Scenario("graduation gap (1 buy completes curve)", lambda t: None, events=((TE, complete_curve),),
             window=1800),
    Scenario("graduation, then +2x on AMM over 60s", lambda t: None,
             events=((TE, complete_curve),) + amm_ramp_events(TE + 30, 2.0, 60), window=1800),
    Scenario("graduation, then +3x on AMM over 60s", lambda t: None,
             events=((TE, complete_curve),) + amm_ramp_events(TE + 30, 3.0, 60), window=1800),
    Scenario("graduation, migration stuck", lambda t: None, events=((TE, complete_curve),),
             window=3 * 3600),
]


# ----------------------------------------------------------------------------
# Static analytics
# ----------------------------------------------------------------------------
def fmt_pct(x, d=1):
    return f"{100 * x:.{d}f}%"


def table(headers, rows):
    out = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    for r in rows:
        out.append("| " + " | ".join(str(c) for c in r) + " |")
    return "\n".join(out)


def opening_stats(key: str, size_frac: float, P: Params = DEFAULT):
    m = new_market(key, P)
    Q = max(1, int(m.max_open_q() * size_frac))
    X0 = m.venue.eff()[0]
    pos = m.open(Q, 0)
    return m, pos, Q / X0


def o2_margin(key: str, size_frac: float) -> int:
    """Analysis only (owner option O2, NOT a protocol parameter): margin with the EMA
    leg evaluated on the EMA reserves shifted by the open's own sale."""
    m = new_market(key)
    Q = max(1, int(m.max_open_q() * size_frac))
    m.advance(0)
    m.observe(0)
    v, T = m.venue, m.tier
    e = m.ring[-1]
    S = v.sell(Q)
    qs = m.quote_spot(Q)
    Xe, Ye = e[3] + Q, cdiv(e[3] * e[4], e[3] + Q)
    qe = v.quote_at(Q, Xe, Ye)
    return max(cdiv(T.cr_open_bps * max(qs[1], qe[1] if qe else qs[1]), BPS) - S, 0), qs[1]


STATIC_KEYS = ["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC"]


def static_tables():
    out = []
    rows = []
    for key in STATIC_KEYS:
        T = tier_of(key)
        for frac in (1.0, 0.1):
            m, pos, q = opening_stats(key, frac)
            M2, B2 = o2_margin(key, frac)
            S, B = pos.S, pos.B_open
            rho = S / B
            m_ = T.m_bps / BPS
            fee_ow = m.venue.fee_oneway_bps() if m.venue.has_route() else 0
            rows.append([key, f"{frac:.0%} of cap", f"{100*q:.2f}%", f"{fee_ow/100:.2f}%", f"{rho:.4f}",
                         f"{(S + m_ * B) / B:.3f}", f"{T.cr_open_bps/BPS:.2f}",
                         f"{100 * pos.M / B:.1f}%", f"{100 * M2 / B2:.1f}%"])
    out.append("### Opening CR and margin under D4\n")
    out.append(table(["market", "size", "q=Q/X0", "venue fee (one way)", "rho=S/B_open",
                      "CR if M = m x B_open (literal column)", "table CR (enforced)",
                      "D4 margin, literal: EMA leg on EMA reserves (% of B_open)",
                      "owner option O2 (not built): EMA leg shifted by the open's own sale (% of B_open)"], rows))

    rows = []
    for key in STATIC_KEYS + ["D@97%"]:
        m = new_market(key)
        v = m.venue
        X, Y = v.eff()
        supply = getattr(v, "supply", PUMP_SUPPLY)
        mcap = Y * supply // X
        oi_q = m.oi_cap_q(m.conservative_x())
        m2, pos, q = opening_stats(key, 1.0)
        kind = {"amm": "PumpSwap", "curve": "pump curve", "ray": getattr(v, "name", "Raydium")}[v.kind]
        K = m2.settle_reward(pos.V)
        rows.append([key, kind, f"{X/1e12:,.1f}M" if v.kind != "curve" else f"vt {X/1e12:,.1f}M / rt {v.rt/1e12:,.1f}M",
                     f"{sol(Y):,.2f}" + (" (virtual)" if v.kind == "curve" else ""), f"{sol(mcap):,.0f}",
                     f"{v.fee_oneway_ppm()/1e4:.2f}%", f"{pos.Q/1e12:,.3f}M", f"{sol(pos.B_open):,.3f}",
                     f"{sol(pos.M):,.3f}", f"{sol(pos.V):,.3f}", f"{sol(pos.fee_open):,.4f}",
                     f"{sol(oi_q * v.spot_wad() // WAD):,.2f}", f"{sol(K):,.4f}",
                     f"{sol(m2.expiry_keeper(pos.B_open)):,.4f}", f"{sol(pos.ert):.4f}"])
    out.append("\n### Example venues (6-decimal tokens) and a position at the per-position cap\n")
    out.append(table(["market", "venue", "token reserve X", "SOL reserve Y", "mcap (SOL)", "venue fee one way",
                      "max Q per position", "N_open = B_open (SOL)", "margin M (SOL)", "vault V (SOL)",
                      "open fee (SOL)", "OI cap notional (SOL)", "S1-S3 keeper reward K_s (SOL)",
                      "expiry keeper reward (SOL)", "ert (SOL)"], rows))

    rows = []
    for key in ["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RC"]:
        T = tier_of(key)
        spec_k = T.cr_open_bps / T.cr_liq_bps
        cells = [key, f"{spec_k:.2f}x"]
        for frac, rent_days in ((0.001, 0), (1.0, 0), (1.0, None)):
            k = price_rise_to_liq(key, frac, rent_days)
            cells.append("n/a" if k is None else f"{k:.2f}x")
        rows.append(cells)
    out.append("\n### Price rise to liquidation (spot multiple of post-open price, EMA assumed caught up)\n")
    out.append(table(["market", "spec (CR_open/L)", "actual, tiny size, no rent",
                      "actual, size at cap, no rent", "actual, size at cap, rent for max duration @80% util"],
                     rows))

    rows = []
    for T in TIERS.values():
        qp = math.sqrt(1 + T.impact_pos_bps / BPS) - 1
        qm = 1 - 1 / math.sqrt(1 + T.impact_mkt_bps / BPS)
        slip_q = T.max_liq_slip_bps / (BPS + T.max_liq_slip_bps)
        rows.append([T.name, f"{T.impact_pos_bps/100:.0f}%", f"{100*qp:.3f}% of X0",
                     f"{T.impact_mkt_bps/100:.0f}%", f"{100*qm:.3f}% of X",
                     f"{100*qm*0.25:.3f}% of X", f"{T.max_liq_slip_bps/100:.0f}%", f"{100*slip_q:.2f}% of X"])
    out.append("\n### Size caps as fractions of the venue token reserve X\n")
    out.append(table(["tier", "pos. impact", "max Q per position", "mkt impact", "max total Q (OI cap)",
                      "per-wallet (25% of OI cap)", "S2 slippage", "Q/X at which S2 triggers"], rows))

    rows = []
    for T in TIERS.values():
        r80 = rate_bps(T, 8000)
        r90 = rate_bps(T, 9000)
        r100 = rate_bps(T, 10000)
        maxdur_80 = r80 / BPS * T.max_dur_s / YEAR_S
        maxdur_90 = r90 / BPS * T.max_dur_s / YEAR_S
        min_rent = r80 / BPS * T.min_rent_s / YEAR_S
        rows.append([T.name, f"{T.rate_base_bps/100:.0f}%", f"{T.rate_s1_bps/100:.2f}%", f"{T.rate_s2_bps/100:.0f}%",
                     f"{r80/100:.0f}%", f"{r90/100:.0f}%", f"{r100/100:.0f}%",
                     f"{100*min_rent:.3f}%", f"{100*maxdur_80:.2f}%", f"{100*maxdur_90:.2f}%"])
    out.append("\n### Rent curve (APR) and rent as % of mark notional\n")
    out.append(table(["tier", "base", "slope1 (per 100% U)", "slope2 (per 100% U)", "rate@80%", "rate@90% (cap)",
                      "rate@100%", "min rent @80%", "max-duration rent @80%", "max-duration rent @90%"], rows))

    out.append("\n### Curve round trip (C1-6) and graduation boost (C1-13)\n")
    out.append(curve_round_trip_text())
    out.append(boost_text())
    out.append(s2_reach_text())
    return "\n".join(out)


def curve_round_trip_text() -> str:
    vt, vs, Q = 824_130_602_106_172, 39_059_343_535, 10_000_000_000_000
    c = PumpCurve(vt, vs, vt - 279_900_000_000_000, vs - 30_000_000_000)
    c0 = copy.deepcopy(c)
    proceeds = c.sell(Q)
    buyback = c.buy(Q)
    same_state = copy.deepcopy(c0).buy(Q)
    return (f"- sell {Q:,} raw then buy it back: proceeds {proceeds:,}, buyback {buyback:,} lamports -> "
            f"round-trip cost {100*(buyback-proceeds)/proceeds:.2f}% of proceeds (fees only; impact cancels). "
            f"Buying Q on the pre-sale state costs {same_state:,} -> {100*(same_state-proceeds)/proceeds:.2f}% "
            f"(the same-state bid/ask spread for size Q).\n")


def boost_text() -> str:
    p = CPPool.graduate(84_990_359_345)
    p0 = p.spot_wad()
    p.run_boost_to_end()
    return (f"- graduation boost: VQR = {84_990_359_345*BOOST_BPS//BPS:,} lamports "
            f"({BOOST_BPS/100:.2f}% of migrated SOL); after the zero-fee buy-and-burn the price is "
            f"{p.spot_wad()/p0:.4f}x the graduation price (constant-product: (1 + 0.2069)^2 = "
            f"{(1+BOOST_BPS/BPS)**2:.4f}), independent of the migrated amount.\n")


def s2_reach_text() -> str:
    """R3B: S2 is unreachable before S1 for capped positions at defaults.  For each example,
    the smallest price multiple at which sigma > sigma_max on the live state, after draining all
    unlocked LP (Raydium), versus the S1 multiple."""
    lines = ["- S2 reachability at the per-position cap (price multiple at which each first holds, live state):"]
    for key in ("A", "C", "D@0%", "RA", "RC"):
        m = new_market(key)
        pos = m.open(m.max_open_q(), 0)
        if m.venue.kind == "ray":
            m.venue.remove_liquidity(5000)
        K = m.settle_reward(pos.V)
        s1 = s2 = None
        for i in range(1, 800):
            k = 1.01 ** i
            v = copy.deepcopy(m.venue)
            v.move_to_price(int(pos.spot_post * k))
            if v.kind == "curve" and v.complete:
                break
            q = v.quote_at(pos.Q, *v.eff())
            if s1 is None and (q is None or q[1] + pos.ert > pos.V - K):
                s1 = k
            X, Y = v.eff()
            if s2 is None and (q is None or q[0] * WAD * BPS // (pos.Q * (Y * WAD // X)) - BPS > pos.slip_max):
                s2 = k
            if s1 and s2:
                break
        lines.append(f"  {key}{' (all unlocked LP drained)' if m.venue.kind == 'ray' else ''}: "
                     f"S1 at x{s1:.2f}, S2 at {'x%.1f' % s2 if s2 else 'never (curve completes first)'}")
    return "\n".join(lines) + "\n"


def price_rise_to_liq(key: str, size_frac: float, rent_days):
    """Bisection on the spot multiple k at which CR_spot hits the tier's liquidation
    CR, with EMA := spot (no lag).  rent_days=None -> max duration."""
    T = tier_of(key)

    def cr_at(k):
        m = new_market(key)
        Q = max(1, int(m.max_open_q() * size_frac))
        pos = m.open(Q, 0)
        t = 0
        if rent_days is None or rent_days > 0:
            t = T.max_dur_s - 1 if rent_days is None else int(rent_days * 86400)
            m.advance(t)
        m.exo_move(int(pos.spot_post * k))
        m.advance(t + 1)
        if m.migrating():
            m.advance(t + 1 + m.P.migration_delay_s)
            m.observe(t + 1 + m.P.migration_delay_s)      # M6 (#105): the next prologue moves the market onto the pool
            m.exo_move(int(pos.spot_post * k))
        qs = m.quote_spot(pos.Q)
        R = m.rent_due(pos)
        return (pos.V - R) * BPS // qs[1] / BPS if qs else 0

    lo, hi = 1.0, 100.0
    if cr_at(hi) >= T.cr_liq_bps / BPS:
        return None
    for _ in range(40):
        mid = math.sqrt(lo * hi)
        if cr_at(mid) < T.cr_liq_bps / BPS:
            hi = mid
        else:
            lo = mid
    return hi


# ----------------------------------------------------------------------------
# Scenario tables
# ----------------------------------------------------------------------------
def scenario_rows(keys, scenarios, sizes=(1.0,), P: Params = DEFAULT, latency=1, extra=None):
    rows = []
    for key in keys:
        for sc in scenarios:
            mk = MARKETS[key]()
            if sc.name.startswith(("drain",)) and not mk.lp_mutable:
                continue
            if key.startswith("D") and sc.name.startswith(("drain", "pool disabled", "route flagged", "halt")):
                continue
            if sc.name.startswith("halt") and mk.kind != "ray":
                continue
            for frac in sizes:
                Pp = extra(sc, P) if extra else P
                m = new_market(key, Pp)
                pos, r, P1 = run(m, frac, sc, keeper_latency=latency)
                s = summarize(pos, r, P1)
                t_exit = s["t"] - sc.t_event if sc.t_event < 10**8 else s["t"]
                bp = s["borrower_pnl"]
                rows.append([key, sc.name, s["kind"], f"{t_exit:+d}s", f"{s['mult']:.2f}x / {s['term']:.2f}x",
                             f"{s['cr_s']:.2f}/{s['cr_e']:.2f}", f"{sol(s['token_value']):.3f}",
                             "-" if bp is None else f"{sol(bp):+.4f} ({100*bp/s['M']:+.0f}% of M)",
                             f"{s['tokens']/pos.Q:.0%} + {sol(s['lender_sol']):.4f} SOL",
                             f"{sol(s['lender_pnl']):+.4f} ({s['lender_pnl_pct']:+.1f}%)",
                             f"{sol(s['keeper']):.4f}"])
    return rows


SCEN_HEADERS = ["market", "scenario", "outcome", "exit vs event", "price at exit / terminal",
                "CR spot/EMA at exit", "value of Q at terminal price (SOL)", "borrower P&L (SOL)",
                "lender gets", "lender P&L vs holding at terminal price (SOL, % of Q value)", "keeper SOL"]


def frozen_s3_rows():
    """#99: FAST_SCENARIOS rows whose candidate the position does not fit: after the scenario's horizon the keeper
    keeps the S3 chain alive (duty 5: a crank every 30 s on the halted pool) and settles at the deadline; the
    healthy position is never liquidatable at the frozen P_ema, so S3 comes with the expiry, at fair value (D3; the
    expiry fee as p', O15)."""
    rows = []
    for key in ("RA", "RB", "RC"):
        for sc in FAST_SCENARIOS:
            m = new_market(key)
            pos, r, P1 = run(m, 1.0, sc)
            if r["kind"] != "open" or m.venue.has_route():
                continue
            t = r["t"]
            res = None
            while res is None and t < pos.deadline + 3600:
                t += 30
                m.step_to(t)
                if t >= pos.deadline:
                    res = m.try_exit(pos, t)
            if res is None:
                rows.append([key, sc.name, "open", "-", "-", "-", "-", "-", "-", "-"])
                continue
            v_frozen = pos.Q * m.ema // WAD
            v_term = pos.Q * int(P1 * 2.0) // WAD
            rows.append([key, sc.name, res["kind"], f"+{(t - TE) / 86400:.2f} d (deadline +{(pos.deadline - TE) / 86400:.2f} d)",
                         f"{sol(res['lender_sol']):.4f}", f"{sol(v_frozen):.4f}",
                         f"{sol(res['lender_sol'] - v_frozen):+.4f} ({100 * (res['lender_sol'] - v_frozen) / v_frozen:+.1f}%)",
                         f"{sol(res['lender_sol'] - v_term):+.4f} ({100 * (res['lender_sol'] - v_term) / v_term:+.1f}%)",
                         f"{sol(res['borrower']):.4f}", f"{sol(res['keeper']):.4f}"])
    return rows


def grad_extra(sc: Scenario, P: Params) -> Params:
    if "stuck" in sc.name:
        return replace(P, migration_delay_s=10**9)
    return P


def scenario_tables():
    out = []
    out.append("### Deterministic scenarios, size at the per-position cap (event at t = 1 h after open)\n")
    rows = scenario_rows(["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC"], SCENARIOS)
    out.append(table(SCEN_HEADERS, rows))
    out.append("\n### Raydium incumbent halted, fast-path switch to a thinner pool (R3B-7a; ring L rewritten; #99: only "
               "onto a pool on which the position fits the opening caps -- 'open' at 1.00x = no window, frozen on the "
               "halted pool)\n")
    rows = scenario_rows(["RA", "RB", "RC"], FAST_SCENARIOS)
    out.append(table(SCEN_HEADERS, rows))
    out.append("\n#### #99: the positions no candidate fits, continued to their deadline (keeper crank every 30 s on the "
               "halted pool; S3 at fair value, the box at the EMA point)\n")
    out.append(table(["market", "scenario", "exit", "at (after the halt)", "lender gets (SOL)",
                      "value of Q at the frozen P_ema (SOL)", "lender P&L vs the frozen P_ema",
                      "lender P&L vs the terminal price (2.00x)", "borrower (SOL)", "keeper (SOL)"],
                     frozen_s3_rows()))
    out.append("\n### Graduation mid-position (Tier D; event at t = 1 h; migration 2 s after completion; "
               "boost 20.69 % spent over 6 min)\n")
    rows = scenario_rows(["D@50%", "D@80%", "D@90%", "D@97%"], GRAD_SCENARIOS, extra=grad_extra)
    out.append(table(SCEN_HEADERS, rows))
    out.append("\n### Buyback larger than remaining real_token_reserves (Tier D opened at 97% progress)\n")
    rows = []
    for label, synthetic, two_leg in (("synthetic v3 (one transaction)", True, True),
                                      ("straddle quote B_str (bundle path, retired)", False, True),
                                      ("no straddle quote", True, False)):
        P = replace(DEFAULT, synthetic=synthetic, two_leg=two_leg)
        for sc in (
            Scenario("rt -> Q/2, borrower closes 30s later", lambda t: None,
                     events=((TE, leave_real_reserves(0.5)),), close_at=TE + 30, window=600),
            Scenario("rt -> Q/2, then others complete curve at +10min, close after", lambda t: None,
                     events=((TE, leave_real_reserves(0.5)), (TE + 600, complete_curve)),
                     close_at=TE + 30, window=1200),
        ):
            m = new_market("D@97%", P)
            pos, r, P1 = run(m, 1.0, sc)
            s = summarize(pos, r, P1)
            rows.append([label, sc.name, s["kind"],
                         f"{s['t'] - TE:+d}s", f"{s['mult']:.3f}x", f"{sol(r['B']):.4f}", f"{sol(pos.B_open):.4f}",
                         "-" if s["borrower_pnl"] is None else f"{sol(s['borrower_pnl']):+.4f}"])
    out.append(table(["policy", "scenario", "outcome", "exit vs event", "price at exit", "buyback cost (SOL)",
                      "B_open (SOL)", "borrower P&L (SOL)"], rows))
    out.append("\n### SOL settlements: S1 uncapped (SPEC), S2/S3 fair-value cap (D3)\n")
    out.append(fv_table())
    out.append("\n### Keeper latency sensitivity (size at cap; 'pump 10x/60s' and 'gap 5x')\n")
    rows = []
    for lat in (1, 5, 30):
        for r in scenario_rows(["A", "C", "D@0%", "RA"], [SC["gap 5x"], SC["pump 10x/60s"]], latency=lat):
            rows.append([f"{lat}s"] + r)
    out.append(table(["keeper latency"] + SCEN_HEADERS, rows))
    return "\n".join(out)


def s3_flag_case(key, lender_settles: bool, reading_penalty: bool = False):
    """Emergency route-broken flag set; the position expires 20 min later (healthy CR).
    Honest keeper: expire() at the deadline.  Lender-keeper: sol_settle(S3) once due.
    reading_penalty=True prices FV with the tier penalty instead of the expiry fee
    (the rejected R3A-8a reading, analysis only)."""
    m = new_market(key)
    pos = m.open(m.max_open_q(), 0)
    pos.deadline = 7200
    t = 0
    while t < 7200 - 1800:
        t += 30
        m.step_to(t)
    m.route_broken = True
    if reading_penalty:
        m.P = replace(m.P, expiry_fee_bps=pos.pen_bps)
    while t < 7200 + 900:
        t += 1
        m.step_to(t)
        if t >= pos.deadline:
            r = m.try_exit(pos, t, settle_only=lender_settles)
            if r:
                return pos, r, m
    return pos, None, m


def fv_table():
    rows = []
    for key in ("A", "RA", "C", "D@0%"):
        for label, lender, penalty in (("honest keeper: expire()", False, False),
                                       ("lender-keeper: sol_settle(S3), FV with the expiry fee (default)", True, False),
                                       ("lender-keeper: sol_settle(S3), FV with the tier penalty (R3A-8a reading)",
                                        True, True)):
            got = s3_flag_case(key, lender, penalty)
            if got is None or got[1] is None:
                rows.append([key, label, "none"] + [""] * 6)
                continue
            pos, r, m = got
            K = m.settle_reward(pos.V)
            rows.append([key, label, r["kind"], f"{sol(pos.V - K):.4f}", "-" if r["fv"] is None else f"{sol(r['fv']):.4f}",
                         f"{sol(r['lender_sol']):.4f}", f"{r['tokens']/pos.Q:.0%}", f"{sol(r['borrower']):.4f}",
                         f"{sol(r['keeper']):.4f}"])
    t1 = table(["market", "who exits (Emergency flag set, position expires)", "outcome", "V - K_s (SOL)", "FV (SOL)",
                "lenders get (SOL)", "tokens returned", "borrower gets (SOL)", "keeper (SOL)"], rows)
    rows = []
    cases = [("D@80%", GRAD_SCENARIOS[3]), ("A", SC["gap 10x"]), ("D@0%", SC["gap 10x"]), ("RA", SC["pool disabled"])]
    for key, sc in cases:
        m = new_market(key, grad_extra(sc, DEFAULT))
        pos, r, P1 = run(m, 1.0, sc)
        s = summarize(pos, r, P1)
        fv = r.get("fv")
        rows.append([key, sc.name, s["kind"], f"{s['mult']:.2f}x", f"{sol(pos.V - m.settle_reward(pos.V)):.4f}",
                     "-" if fv is None else f"{sol(fv):.4f}", f"{sol(s['lender_sol']):.4f}",
                     f"{sol(s['token_value']):.4f}", f"{s['lender_pnl_pct']:+.1f}%",
                     "-" if s["borrower_pnl"] is None else
                     f"{sol(s['borrower_pnl']):+.4f} ({100*s['borrower_pnl']/s['M']:+.0f}% of M)"])
    t2 = table(["market", "scenario", "outcome", "price at exit", "V - K_s (SOL)", "FV (SOL)", "lender SOL",
                "spot value of Q (SOL)", "lender P&L vs holding", "borrower P&L"], rows)
    return t1 + "\n\n" + t2


# ----------------------------------------------------------------------------
# EMA / exit-bound lag study (genuine moves; manipulation is in ./attacks/)
# ----------------------------------------------------------------------------
LAG_VARIANTS = [
    ("v0.4 default (tau 5 s, fresh ref, kappa <= 25 %, fixed delta 2 %, persist 3)", {}),
    ("no liquidation persistence (1 update)", dict(liq_persist_n=1)),
    ("persistence 5 updates (above the compiled maximum)", dict(liq_persist_n=5)),
    ("fixed delta 1 % (below the compiled minimum)", dict(delta_liq_bps=100)),
    ("EMA-only reference, delta 16.7 % (critic's C0-1 suggestion)", dict(exit_ref="ema", delta_liq_bps=1667)),
    ("kappa 15 % (reviewer's suggested minimum)", dict(kappa_liq_bps=1500)),
    ("tau 8 s (reviewer's suggested maximum)", "tau8"),
    ("tau 10 s (v0.1 value)", "tau10"),
]
LAG_PUMPS = None


def lag_pumps():
    return [SC["pump 10x/60s"], Scenario("pump 10x/3min", ramp_path(TE, 10.0, 180)), SC["pump 10x/10min"],
            SC["gap 5x"]]


def lag_cell(key, kw, sc, P0=DEFAULT):
    if isinstance(kw, str) and kw.startswith("tau"):
        T = replace(tier_of(key), ema_tau_s=int(kw[3:]))
        m = Market(MARKETS[key](), T, P0)
    else:
        m = new_market(key, replace(P0, **kw))
    pos, r, P1 = run(m, 1.0, sc)
    s = summarize(pos, r, P1)
    return s


def ema_study():
    out = ["### Exit bounds and persistence vs genuine fast moves (size at cap, keeper latency 1 s)\n"]
    rows = []
    for key in ["A", "C", "D@0%", "RA"]:
        for label, kw in LAG_VARIANTS:
            cells = [key, label]
            for sc in lag_pumps():
                s = lag_cell(key, kw, sc)
                cells.append(f"{s['kind'][:4]} @{s['mult']:.2f}x +{s['t']-TE}s, lender {s['lender_pnl_pct']:+.0f}%")
            rows.append(cells)
    out.append(table(["market", "setting", "pump 10x/60s", "pump 10x/3min", "pump 10x/10min", "gap 5x"], rows))
    return "\n".join(out)


# ----------------------------------------------------------------------------
# Loss given gap & expected loss
# ----------------------------------------------------------------------------
GAPS = [1.5, 1.75, 2.0, 2.25, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0, 10.0, 15.0, 20.0, 50.0]
LGG_KEYS = ["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA"]


def loss_given_gap(key: str, k: float, size_frac: float = 1.0):
    m = new_market(key)
    pos, r, P1 = run(m, size_frac, Scenario("gap", step_path(TE, k), window=1800))
    s = summarize(pos, r, P1)
    return s["lender_pnl_pct"] / 100, s


def lgg_table():
    out = ["### Loss given an instantaneous gap (lender P&L vs holding, % of value of Q at post-gap price)\n"]
    rows = []
    for k in GAPS:
        row = [f"{k:g}x"]
        for key in LGG_KEYS:
            l, s = loss_given_gap(key, k)
            row.append(f"{100*l:+.1f}% ({s['kind'][:4]})")
        rows.append(row)
    out.append(table(["gap"] + LGG_KEYS, rows))
    return "\n".join(out)


_LGG_CACHE = {}


def lgg_points(key: str):
    if key not in _LGG_CACHE:
        pts = []
        for k in GAPS:
            l, s = loss_given_gap(key, k)
            pts.append((k, l * k))
        _LGG_CACHE[key] = pts
    return _LGG_CACHE[key]


def pareto_expectation(key: str, alpha: float, xmin: float, jmax: float = 50.0):
    pts = lgg_points(key)

    def interp(k):
        if k <= pts[0][0]:
            return pts[0][1] * (k - 1) / (pts[0][0] - 1)
        for (k0, v0), (k1, v1) in zip(pts, pts[1:]):
            if k0 <= k <= k1:
                return v0 + (v1 - v0) * (k - k0) / (k1 - k0)
        return pts[-1][1]
    ks = [xmin * (jmax / xmin) ** (i / 600) for i in range(601)]
    tot = 0.0
    for a, b in zip(ks, ks[1:]):
        tot += ((xmin / a) ** alpha - (xmin / b) ** alpha) * interp(math.sqrt(a * b))
    tot += (xmin / jmax) ** alpha * pts[-1][1]
    return tot


# ----------------------------------------------------------------------------
# Monte Carlo (jump-diffusion; ASSUMED parameters, not calibrated)
# ----------------------------------------------------------------------------
@dataclass(frozen=True)
class MCAssumptions:
    sigma_annual: float
    lam_up_per_day: float
    lam_dn_per_day: float
    alpha: float = 1.5
    xmin: float = 1.5
    jmax: float = 50.0


MC_ASSUMPTIONS = {
    "calm": {
        "A": MCAssumptions(1.5, 0.002, 0.002, alpha=2.0),
        "B": MCAssumptions(2.5, 0.005, 0.005, alpha=2.0),
        "C": MCAssumptions(4.0, 0.02, 0.02, alpha=2.0),
        "D": MCAssumptions(6.0, 0.05, 0.05, alpha=2.0),
    },
    "stressed": {
        "A": MCAssumptions(1.5, 0.02, 0.02, alpha=1.5),
        "B": MCAssumptions(2.5, 0.05, 0.05, alpha=1.5),
        "C": MCAssumptions(4.0, 0.20, 0.20, alpha=1.5),
        "D": MCAssumptions(6.0, 1.00, 1.00, alpha=1.5),
    },
}


def mc_path_run(key: str, A: MCAssumptions, rng: random.Random, size_frac: float, coarse: int):
    m = new_market(key)
    Q = max(1, int(m.max_open_q() * size_frac))
    pos = m.open(Q, 0)
    P1 = pos.spot_post
    T = m.tier
    horizon = T.max_dur_s + 600
    jumps = []
    t = 0.0
    lam = (A.lam_up_per_day + A.lam_dn_per_day) / 86400
    while True:
        t += rng.expovariate(lam)
        if t >= T.max_dur_s:
            break
        u = rng.random()
        J = min(A.xmin * (1 - rng.random()) ** (-1 / A.alpha), A.jmax)
        if u >= A.lam_up_per_day / (A.lam_up_per_day + A.lam_dn_per_day):
            J = 1 / J
        jumps.append((int(t), J))
    logp = 0.0
    t = 0
    fine_until = -1
    act = False
    ji = 0
    while t < horizon:
        near = t >= pos.deadline - 120
        step = 1 if (t < fine_until or act or near) else coarse
        nt = t + step
        if ji < len(jumps) and jumps[ji][0] <= nt:
            nt = max(t + 1, jumps[ji][0])
        dt = nt - t
        t = nt
        sig = A.sigma_annual * math.sqrt(dt / YEAR_S)
        logp += sig * rng.gauss(0, 1) - 0.5 * sig * sig
        while ji < len(jumps) and jumps[ji][0] <= t:
            logp += math.log(jumps[ji][1])
            ji += 1
            fine_until = t + 900
        m.exo_move(max(1, int(P1 * math.exp(logp))))
        m.step_to(t)
        if t >= fine_until and not act and not near:
            m.reset_ema_to_spot(t)
        st = m.status(pos, t)
        if st["liq"] or st["expired"]:
            act = True
            r = m.try_exit(pos, t)
            if r:
                return pos, r, P1
        else:
            act = False
    r = m.try_exit(pos, t)
    return pos, r or dict(kind="open", spot=m.ema, lender_sol=0, tokens=Q, borrower=0, keeper=0,
                          protocol=0, t=t, cr_s=0, cr_e=0), P1


def monte_carlo(n: int, seed: int):
    out = ["### Monte Carlo: lender and borrower outcome per position (ILLUSTRATIVE jump-diffusion)\n",
           "Position at the per-position cap, held to expiry unless liquidated/settled; 80% utilization;",
           "price = GBM(sigma) x Poisson up/down gaps with Pareto(alpha, 1.5x) multiples (<= 50x) that persist;",
           "1 s resolution for 15 min after each gap and near expiry, EMA/keeper modelled exactly there.\n"]
    rows = []
    for ri, regime in enumerate(("calm", "stressed")):
        for ci, (key, coarse) in enumerate((("A", 300), ("B", 300), ("C", 120), ("D@0%", 60), ("D@50%", 60),
                                            ("RA", 300))):
            A = MC_ASSUMPTIONS[regime][tier_of(key).name]
            n_kind = {}
            lp, lp2, bp, big = 0.0, 0.0, 0.0, 0
            for i in range(n):
                # one independent, deterministically seeded stream per path (v0.4): a path's random
                # numbers no longer depend on how long earlier paths ran
                rng = random.Random(seed * 1_000_003 + (ri * 16 + ci) * 100_003 + i)
                pos, r, P1 = mc_path_run(key, A, rng, 1.0, coarse)
                s = summarize(pos, r, P1)
                n_kind[s["kind"]] = n_kind.get(s["kind"], 0) + 1
                x = s["lender_pnl"] / pos.B_open if s["kind"] != "open" else 0.0
                lp += x
                lp2 += x * x
                if s["borrower_pnl"] is not None:
                    bp += s["borrower_pnl"] / pos.M
                if x < -0.10:
                    big += 1
            mean = lp / n
            se = math.sqrt(max(lp2 / n - mean * mean, 0) / n)
            kinds = ", ".join(f"{k} {v/n:.0%}" for k, v in sorted(n_kind.items()))
            rows.append([regime, key, f"sigma={A.sigma_annual:g}, up/dn gaps {A.lam_up_per_day:g}/d, alpha={A.alpha:g}",
                         n, kinds, f"{100*mean:+.2f}% +/- {100*se:.2f}%", f"{100*big/n:.1f}%", f"{100*bp/n:+.1f}%"])
    out.append(table(["regime", "market", "assumptions", "paths", "outcomes",
                      "E[lender P&L vs holding] (% of N_open)", "P(lender loss > 10% of N_open)",
                      "E[borrower P&L] (% of M)"], rows))
    return "\n".join(out)


def expected_loss_formula_table():
    out = ["### Break-even gap frequency: lender income vs. expected gap loss per position\n",
           "E[gap P&L per position] ~= lambda_up x T_max x E_J[P&L | one up-gap], J ~ Pareto(alpha, xmin=1.5x),",
           "truncated at 50x.  Lender income = 85% of rent at 80% utilization over T_max + 2% expiry fee.",
           "lambda* = income / (T_max x |E_J|) is the up-gap rate at which lenders break even.\n"]
    rows = []
    for key in ["A", "B", "C", "D@0%", "D@50%", "RA"]:
        T = tier_of(key)
        days = T.max_dur_s / 86400
        rent = rate_bps(T, 8000) / BPS * T.max_dur_s / YEAR_S * 0.85
        income = rent + DEFAULT.expiry_fee_bps / BPS
        cells = [key, f"{days:g} d", f"{100*income:.2f}%", f"{100*(DEFAULT.expiry_fee_bps/BPS)/income:.0f}%"]
        for alpha in (1.5, 2.0, 3.0):
            ej = pareto_expectation(key, alpha, 1.5)
            lam = income / (days * -ej) if ej < 0 else float("inf")
            cells.append(f"{100*ej:+.1f}% / 1 per {1/lam:,.0f} d" if lam != float("inf") else f"{100*ej:+.1f}% / never")
        rows.append(cells)
    out.append(table(["market", "T_max", "lender income per position (% of notional)", "expiry fee share of it",
                      "alpha=1.5: E_J / break-even rate", "alpha=2: E_J / break-even rate",
                      "alpha=3: E_J / break-even rate"], rows))
    return "\n".join(out)


# ----------------------------------------------------------------------------
# Randomised exit fuzz with attacker bundles, attacker-only seconds and sandwiches
# ----------------------------------------------------------------------------
def attacker_bundle(m: Market, t: int, target_wad: int) -> int:
    """[buy/sell to target, crank_price, trade back] in one atomic bundle; returns
    the attacker's net SOL cost (round-trip venue fees)."""
    v = m.venue
    if not v.has_route():
        m.observe(t)
        return 0
    X, Y = v.eff()
    tX = math.isqrt(X * Y * WAD // max(target_wad, 1))
    cost = 0
    if tX < X:
        d = X - tX
        if v.kind == "curve":
            d = min(d, v.rt - 1)
        if d > 0:
            cost += v.buy(d)
            m.observe(t)
            cost -= v.sell(d)
            return cost
    elif tX > X:
        d = tX - X
        got = v.sell(d)
        m.observe(t)
        if v.kind == "curve" and d > v.rt:
            d = v.rt
        cost += v.buy(d) - got
        return cost
    m.observe(t)
    return 0


def fuzz_exits(n_runs: int, seed: int):
    """Random paths x random attacker behaviour; every exit re-checks the invariants of
    ECON §13 (InvariantError).  v0.4: a third of the runs use a low sigma_max (S2
    reachable), some set the Emergency flag early (S3 on an executable venue), and a
    lender-attacker lands attacker-only seconds before settlements.  v0.4.1: half of the runs
    carry per-second price noise (+-0.3 % or +-1 %, R4B-1); a quarter of the runs add a FREEZER that lands an opposite observation in seconds that also hold the
    honest keeper's (push-then-freeze, R4B-2), up or down; blockers' low observations before
    an S3 exercise the p' veto (R4A-3); every S2/S3 amount must lie inside the box and the
    box within fv_dev_bps (I-E5); rent-reserve waits are counted (R4A-1 / R4B-1)."""
    rng = random.Random(seed)
    keys = ["A", "C", "D@0%", "D@50%", "RA", "RC"]
    stats = dict(runs=0, exits=0, sandwich_attempts=0, bundles=0, attacker_only_seconds=0, blocker_runs=0,
                 noisy_runs=0, freezer_runs=0, frozen_seconds=0, penalty_truncated_by_sandwich=0, max_loss_over_bref_bps=0,
                 max_settle_box_bps=0, reserve_waits_liq=0, reserve_waits_exp=0, kinds={})
    for i in range(n_runs):
        key = rng.choice(keys)
        variant = rng.random()
        P = DEFAULT
        tier = tier_of(key)
        if variant < 0.33:
            tier = replace(tier, max_liq_slip_bps=rng.choice([200, 400, 800]))   # S2 reachable (override)
        m = new_market(key, P, tier=tier)
        Q = max(1, int(m.max_open_q() * rng.choice([1.0, 0.5, 0.1])))
        pos = m.open(Q, 0)
        P1 = pos.spot_post
        t = 0
        mode = rng.choice(["gap", "ramp", "flat"])
        k = rng.choice([1.3, 1.6, 2.0, 3.0, 5.0])
        te = rng.randrange(60, 600)
        dur = rng.choice([30, 120, 600])
        horizon = te + 900 if mode != "flat" else pos.deadline + 120
        flag = variant > 0.8 and m.venue.kind != "curve"
        if flag:
            m.route_broken = True
        if mode == "flat":
            t = pos.deadline - 30
            m.step_to(t)
            if flag:
                m.no_route_since, m.last_no_route_obs = t - 700, t
        done = False
        blocker = rng.random() < 0.2
        pusher = rng.random() < 0.3                       # lender-attacker: attacker-only seconds
        freezer = rng.random() < 0.25                     # push-then-freeze partner (R4B-2)
        f_up = rng.random() < 0.5
        noise = rng.choice([0.0, 0.0, 0.003, 0.01])       # per-second price noise (R4B-1)
        nrng = random.Random(rng.random())
        stats["blocker_runs"] += blocker
        stats["freezer_runs"] += freezer
        stats["noisy_runs"] += noise > 0
        while t < horizon and not done:
            t += 1
            if noise:
                base = (k if (mode == "gap" and t >= te) else
                        math.exp(math.log(k) * min(max(t - te, 0), dur) / dur) if mode == "ramp" else 1.0)
                if m.venue.has_price():
                    m.exo_move(int(P1 * base * (1 + nrng.uniform(-noise, noise))))
            elif mode == "gap" and t == te:
                m.exo_move(int(P1 * k))
            elif mode == "ramp" and te <= t <= te + dur:
                m.exo_move(int(P1 * math.exp(math.log(k) * (t - te) / dur)))
            if pusher and m.venue.has_route() and pos.armed and rng.random() < 0.3:
                m.advance(t)                              # the attacker is alone in this second
                attacker_bundle(m, t, m.venue.spot_wad() * rng.choice([150, 300]) // 100)
                stats["attacker_only_seconds"] += 1
                r = m.try_exit(pos, t, settle_only=True)
                if r:
                    stats["exits"] += 1
                    stats["kinds"][r["kind"]] = stats["kinds"].get(r["kind"], 0) + 1
                    if r["kind"] in ("S2", "S3"):
                        stats["max_settle_box_bps"] = max(stats["max_settle_box_bps"], r.get("dev_bps", 0))
                    done = True
                continue
            if blocker and m.venue.has_route() and rng.random() < 0.9:
                attacker_bundle(m, t, m.venue.spot_wad() * rng.choice([85, 95, 99]) // 100)
                stats["bundles"] += 1
            if rng.random() < 0.3:
                tgt = int(m.venue.spot_wad() * rng.choice([0.5, 0.8, 1.2, 1.5, 2.0])) if m.venue.has_price() else 0
                if tgt:
                    attacker_bundle(m, t, tgt)
                    stats["bundles"] += 1
            if rng.random() < 0.8:
                m.step_to(t)
                if freezer and m.venue.has_route() and (pos.armed or t >= pos.deadline - 5) and rng.random() < 0.8:
                    # an opposite observation in a second that also holds the honest one: obs = P_ema (frozen)
                    attacker_bundle(m, t, m.ema * (BPS + 5 if f_up else BPS - 5) // BPS)
                    stats["frozen_seconds"] += 1
            else:
                m.advance(t)
            st = m.status(pos, t)
            if not (st["liq"] or st["expired"]):
                m.try_exit(pos, t, settle_only=True)      # keeper's sol_settle keeps/refutes the clock
                continue
            honest_m, honest_pos = copy.deepcopy(m), copy.deepcopy(pos)
            r_h = honest_m.try_exit(honest_pos, t)
            if rng.random() < 0.6 and m.venue.has_route():
                v = m.venue
                X = v.eff()[0]
                dX = int(X * rng.choice([0.005, 0.02, 0.05, 0.1, 0.2]))
                if v.kind == "curve":
                    dX = min(dX, v.rt - pos.Q - 1)
                if dX > 0:
                    v.buy(dX)
                    r = m.try_exit(pos, t)
                    if m.venue.has_route():
                        m.venue.sell(dX)
                    stats["sandwich_attempts"] += 1
                else:
                    r = m.try_exit(pos, t)
            else:
                r = m.try_exit(pos, t, settle_only=rng.random() < 0.3)
            if r:
                stats["exits"] += 1
                stats["kinds"][r["kind"]] = stats["kinds"].get(r["kind"], 0) + 1
                if r["kind"] in ("S2", "S3"):
                    stats["max_settle_box_bps"] = max(stats["max_settle_box_bps"], r.get("dev_bps", 0))
                if r_h and r["kind"] == r_h["kind"] and r["kind"] in ("liquidation", "expiry"):
                    senior = lambda x: x["rent"] + x["pen"] + x["fee"] + (x["keeper"] if x["kind"] == "expiry" else 0)
                    lost = senior(r_h) - senior(r)
                    if r["kind"] == "expiry" and r["rent"] < r_h["rent"]:      # expiry keeps rent senior
                        raise InvariantError(("sandwich reduced rent on an expiry", r_h, r))
                    if lost > (r["bound"] - r_h["B"]) + 2:
                        raise InvariantError(("sandwich took more than the bound slack", lost, r))
                    if lost > 1:
                        stats["penalty_truncated_by_sandwich"] += 1
                        stats["max_loss_over_bref_bps"] = max(stats["max_loss_over_bref_bps"],
                                                              lost * BPS // r["b_ref"])
                done = True
        stats["reserve_waits_exp" if st["expired"] and not st["liq"] else "reserve_waits_liq"] += getattr(m, "reserve_waits", 0)
        stats["runs"] += 1
    return stats


def selftest():
    """Quick checks used by CI of the design docs."""
    out = []
    m = new_market("A")
    for t in range(1, 30):
        m.step_to(t)
    e0 = m.ema
    attacker_bundle(m, 30, e0 * 3)
    m.step_to(31)
    up = m.ema / e0 - 1
    m2 = new_market("A")
    for t in range(1, 30):
        m2.step_to(t)
    e0b = m2.ema
    m2.step_to(30)
    attacker_bundle(m2, 30, e0b * 3)
    m2.step_to(31)
    up2 = m2.ema / e0b - 1
    tau = m.tier.ema_tau_s
    b1 = (tau + 2) / (tau + 1) - 1
    b2 = (tau + 4) / (tau + 2) - 1
    out.append(f"single integration after an atomic 3x bundle alone in its second: EMA {100*up:+.2f}% "
               f"(bound {100*b1:+.1f}% at dt=1, {100*b2:+.1f}% at dt=2, tau={tau}s); with an honest observation "
               f"in the same second: {100*up2:+.2f}%")
    # reprice rounding (R3B-7c): Y/X >= P on random states
    rng = random.Random(7)
    bad = 0
    for _ in range(20000):
        L = rng.randrange(10**6, 10**19)
        p = rng.randrange(10**6, 10**24)
        st = reprice(L, p)
        if st and st[1] * WAD < p * st[0]:
            bad += 1
    out.append(f"reprice: Y/X >= P in 20,000 random states: {'yes' if bad == 0 else f'NO ({bad})'}")
    for gap_rule in (10**9, DEFAULT.s3_obs_gap_s):
        P = replace(DEFAULT, s3_obs_gap_s=gap_rule)
        m = new_market("A", P)
        pos = m.open(m.max_open_q(), 0)
        for t in range(1, 61):
            m.step_to(t)
        m.exo_move(int(pos.spot_post * 1.8))
        for t in range(61, 181):
            m.step_to(t)
        first = None
        for t in range(181, 181 + 1500, 10):
            down = not (481 <= t < 601)
            m.venue.disabled = down
            if down:
                r = m.try_exit(pos, t)
                if r and r["kind"] == "S3":
                    first = t - 181
                    break
        out.append(f"S3 with observation-gap rule {'off' if gap_rule > 10**8 else str(gap_rule) + ' s'}: "
                   f"first S3 {first} s after the first outage (route was back, unobserved, from +300 s to +420 s)")
    return "\n".join(out)


# ----------------------------------------------------------------------------
def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--quick", action="store_true", help="skip Monte Carlo and fuzz")
    ap.add_argument("--mc", type=int, default=200, help="Monte Carlo paths per cell")
    ap.add_argument("--fuzz", type=int, default=300, help="fuzz runs")
    ap.add_argument("--seed", type=int, default=20260927)
    ap.add_argument("--only", choices=["static", "scen", "lgg", "ema", "mc", "fuzz", "selftest"], default=None)
    a = ap.parse_args(argv)
    sections = []
    if a.only in (None, "selftest"):
        sections.append(selftest())
    if a.only in (None, "static"):
        sections.append(static_tables())
    if a.only in (None, "scen"):
        sections.append(scenario_tables())
    if a.only in (None, "ema"):
        sections.append(ema_study())
    if a.only in (None, "lgg"):
        sections.append(lgg_table())
        sections.append(expected_loss_formula_table())
    if (a.only in (None, "mc")) and not a.quick:
        sections.append(monte_carlo(a.mc, a.seed))
    if (a.only in (None, "fuzz")) and not a.quick:
        s = fuzz_exits(a.fuzz, a.seed)
        sections.append(f"### Exit fuzz (attacker bundles, attacker-only seconds, sandwiches; invariants asserted)\n\n{s}")
    print("\n\n".join(sections))


if __name__ == "__main__":
    main()
