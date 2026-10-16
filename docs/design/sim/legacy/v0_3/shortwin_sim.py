#!/usr/bin/env python3
"""
short.win -- economic / risk reference model and scenario simulation (v0.3).

Companion to docs/design/ECONOMICS.md v0.3.  The tables in its sections 4 and
11 were produced by this script; the manipulation tables of section 12 come
from the attack scripts in ./attacks/ (which import this module).
Standard library only (Python >= 3.8).

    python3 shortwin_sim.py                 # everything, Monte Carlo 200 paths/cell
    python3 shortwin_sim.py --mc 400        # the run used for ECONOMICS.md (~8 min)
    python3 shortwin_sim.py --quick         # skip Monte Carlo
    python3 shortwin_sim.py --only static   # static|scen|ema|lgg|mc|fuzz|selftest

What it models (integer math wherever the on-chain program would use integers):
  * Pump.fun bonding curve: virtual-reserve constant product, exact-out buy
    net = floor(Q*vs/(vt-Q)) + 1, exact-in sell floor(Q*vs/(vt+Q)), protocol and
    creator fees ceil'ed on the SOL leg; completion at real_token_reserves == 0;
    migration to a canonical PumpSwap pool seeded with (supply - initial real
    token reserves) tokens and (real_sol - pool_migration_fee) lamports, of which
    20.69 % is moved to the boost vault and booked as virtual_quote_reserves and
    then spent on zero-fee buy-and-burn over ~6 minutes (C1-13).
  * PumpSwap: exact-out buy ceil(Y*b/(X-b)) with Y = real + virtual quote, fees
    (lp / protocol / creator) ceil'ed on top, LP fee retained; exact-in sell;
    sells capped by real quote reserves; market-cap fee tiers.
  * Raydium AMM v4 / CPMM (owner decision D2): generic constant product with the
    fee charged on the INPUT amount (exact-out gross = ceil(net*1e6/(1e6-rate))),
    a configurable share of the fee leaving the reserves; LP can be withdrawn.
  * short.win protocol as PROPOSED in ECONOMICS.md v0.2: opening-CR margin rule
    at spot and EMA (D4), open fee, minimum rent, O(1) rent index on the EMA
    price, per-second observation buckets with directional aggregation and a
    one-second EMA lag, EMA history ring, persistence ("armed") rules for
    liquidation and S1/S2 (C0-5), exit buybacks bounded by a manipulation-
    resistant reference with a senior rent reserve and penalty / fee bases on
    the quoted reference cost (C0-1, C0-14), expiry keeper reward (C0-1),
    S1 uncapped and S2/S3 capped at FV = executable EMA cost + penalty + rent
    (D3, C0-7), continuous S3 timer (C0-16), fee-scaled size caps, graduation.
  * v0.3 (second review, 2026-09-27): SPEC-literal defaults for the D3/D4
    readings (margin EMA leg unshifted, FV on the fast liquidity EMA, S1 takes
    precedence whenever its legs hold; V1-3, V1-4); Dutch exit clock advanced
    only over seconds in which the position was executable (V1-1); settlement
    arming that only non-atomic evidence can disarm, one eligibility clock for
    S1 and S2, kind chosen at execution (V2-4); S2/S3 FV settles only while the
    EMA is within fv_dev_bps of the ring median (V2-9); exit-reference
    liquidity max(L_live, L_ema, L_slow) (V2-14); no observations recorded while
    the venue cannot execute (V2-5); straddle priced and executed as the
    >= 3-transaction bundle (keeper completes the curve, the position buys Q on
    the new pool; V1-9).
  * Scenario library, EMA / exit-bound lag study, loss-given-gap curves,
    break-even gap frequencies, an ILLUSTRATIVE (uncalibrated) jump-diffusion
    Monte Carlo, and a randomised exit fuzz with attacker bundles and
    sandwiches that asserts the economic invariants of ECONOMICS.md section 13.

Every protocol constant lives in `Params` / `Tier` so the tables can be re-run
with different choices.  Venue constants were read from Solana mainnet on
2026-09-27 (sources: ECONOMICS.md section 16).
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
# Units & fixed point
# ----------------------------------------------------------------------------
LAMPORTS_PER_SOL = 1_000_000_000
WAD = 10**18            # prices: lamports per raw token unit * 1e18; indices
BPS = 10_000
PPM = 1_000_000         # Raydium fee-rate denominator
YEAR_S = 31_536_000     # 365 days
SQRT_WAD = 10**9        # sqrt(WAD)
INF = 10**40


def cdiv(a: int, b: int) -> int:
    """Ceiling division for non-negative integers."""
    return -((-a) // b)


def fee_ceil(x: int, bps: int) -> int:
    return cdiv(x * bps, BPS)


def sol(lamports: float) -> float:
    return lamports / LAMPORTS_PER_SOL


def reprice(L: int, p_wad: int):
    """Constant-product reserves (X, Y) with sqrt(X*Y) = L and Y/X = p (WAD)."""
    sp = math.isqrt(p_wad)
    if sp <= 0 or L <= 0:
        return None
    return L * SQRT_WAD // sp, L * sp // SQRT_WAD


# ----------------------------------------------------------------------------
# Venue constants (mainnet, 2026-09-27 -- see ECONOMICS.md section 1)
# ----------------------------------------------------------------------------
PUMP_VT0 = 1_073_000_000_000_000        # Global.initial_virtual_token_reserves
PUMP_VS0 = 30_000_000_000               # Global.initial_virtual_sol_reserves
PUMP_RT0 = 793_100_000_000_000          # Global.initial_real_token_reserves
PUMP_SUPPLY = 1_000_000_000_000_000     # Global.token_total_supply
PUMP_MIGRATION_FEE = 15_000_001         # Global.pool_migration_fee
PUMP_POOL_TOKENS = PUMP_SUPPLY - PUMP_RT0   # 206.9M tokens seeded into PumpSwap
# C1-13: Pool.virtual_quote_reserves = floor(migrated_sol * 2069 / 10_000) on both
# pools checked (32UVHz1W..., 8DY32U41..., mainnet slot ~450,832,46x); the boost
# vault holds the same amount and is spent on zero-fee buy-and-burn (~40 calls,
# ~6 min).  Source is closed: proportional rule inferred from data (UNVERIFIED).
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

    def eff(self):          # (token reserve X, SOL reserve Y) used for pricing
        raise NotImplementedError

    def fees(self):          # SOL-leg fee bps tuple (pump style)
        raise NotImplementedError

    def fee_oneway_bps(self) -> int:
        return sum(self.fees())

    def has_route(self) -> bool:
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

    def quote_buy_on(self, q: int, X: int, Y: int, fees=None):
        """short.win's conservative exact-out quote on reserves (X, Y), fees on
        top of the SOL leg.  Returns (net_ex_fees, total) lamports or None."""
        if q <= 0:
            return (0, 0)
        if q >= X:
            return None
        fees = fees if fees is not None else self.fees()
        net = cdiv(Y * q, X - q) + self.plus_one
        return net, net + sum(fee_ceil(net, f) for f in fees)

    def quote_at(self, q: int, X: int, Y: int, two_leg: bool = True):
        """Executable quote on a hypothetical state (X, Y) of this venue."""
        return self.quote_buy_on(q, X, Y)

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
    straddle_exec = "bundle"      # set from Params by Market (V1-9)

    def __init__(self, vt: int, vs: int, rt: int, rs: int, sched: FeeSchedule = PUMP_CURVE_FEES):
        self.vt, self.vs, self.rt, self.rs = vt, vs, rt, rs
        self.off_t, self.off_s = vt - rt, vs - rs      # constant per curve [ECON §1.1]
        self.sched = sched
        self.complete = rt == 0

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
        assert not self.complete and 0 < q <= self.rt, (q, self.rt, self.complete)
        net = q * self.vs // (self.vt - q) + 1
        cost = net + sum(fee_ceil(net, f) for f in self.fees())
        self.vs += net
        self.vt -= q
        self.rt -= q
        self.rs += net
        if self.rt == 0:
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
        return out

    def quote_at(self, q: int, X: int, Y: int, two_leg: bool = True):
        """Quote on a hypothetical curve state (vt = X, vs = Y).  If q exceeds the
        real token reserves of that state the deterministic two-leg quote B2 is
        used (complete the curve -> migrate -> buy the rest on the new pool)."""
        if q <= 0:
            return (0, 0)
        rt = max(X - self.off_t, 0)
        if q <= rt:
            return self.quote_buy_on(q, X, Y)
        if not two_leg:
            return None
        leg1 = self.quote_buy_on(rt, X, Y) if rt > 0 else (0, 0)
        migrated = (Y - self.off_s) + leg1[0] - PUMP_MIGRATION_FEE
        pool = CPPool.graduate(migrated)
        if self.straddle_exec == "bundle":
            # V1-9: the MVP executes a straddle as a >= 3-transaction bundle: the keeper
            # buys rt with its OWN SOL (tx1), migrate_v2 (tx2), then the position buys all
            # of q on the new pool (tx3).  Price exactly that path.
            return pool.quote_buy_on(q, *pool.eff())
        leg2 = pool.quote_buy_on(q - rt, *pool.eff())
        if leg2 is None:
            return None
        return leg1[0] + leg2[0], leg1[1] + leg2[1]

    def migrate_preview(self) -> "CPPool":
        return CPPool.graduate(self.rs - PUMP_MIGRATION_FEE)

    def migrate(self) -> "CPPool":
        assert self.complete
        return self.migrate_preview()

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

    def __init__(self, X: int, Y: int, sched: FeeSchedule, supply: int = PUMP_SUPPLY, vq: int = 0,
                 boost_vault: int = 0, canonical: bool = True):
        self.X, self.Y, self.sched, self.supply, self.vq = X, Y, sched, supply, vq
        self.disabled = False
        self.canonical = canonical          # canonical pools: LP burned, boost pools reject deposit/withdraw
        self.lp_mutable = not canonical
        self.boost_vault = boost_vault
        self.boost_next: Optional[int] = None
        self.boost_chunk = 0
        self.boost_interval = 1

    @classmethod
    def graduate(cls, migrated: int) -> "CPPool":
        """Canonical pool right after migrate_v2 (C1-13 proportional boost)."""
        vq = migrated * BOOST_BPS // BPS
        return cls(PUMP_POOL_TOKENS, migrated - vq, PUMPSWAP_CANONICAL_FEES, PUMP_SUPPLY, vq=vq,
                   boost_vault=vq, canonical=True)

    def start_boost(self, t: int) -> None:
        if self.boost_vault > 0:
            self.boost_chunk = cdiv(self.boost_vault, BOOST_CALLS)
            self.boost_interval = max(1, BOOST_DURATION_S // BOOST_CALLS)
            self.boost_next = t + 1

    def boost_tick(self, t: int) -> None:
        """Pump's boost authority: zero-fee buy with boost-vault SOL, tokens burned."""
        while self.boost_next is not None and t >= self.boost_next and self.boost_vault > 0:
            b = min(self.boost_chunk, self.boost_vault)
            X, Y = self.eff()
            out = X * b // (Y + b)
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
        """Exogenous traders; sells stop at the real-reserve floor VQR/X (sell payouts
        are capped by real quote reserves, error InsufficientRealQuoteReserves)."""
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

    def remove_liquidity(self, frac_bps: int) -> None:
        if not self.lp_mutable:
            raise ValueError("canonical pool: LP burned / BoostPoolLiquidityUnsupported")
        self.X -= self.X * frac_bps // BPS
        self.Y -= self.Y * frac_bps // BPS


class RaydiumPool(Venue):
    """Raydium AMM v4 / CPMM, generic constant product with the fee on the INPUT
    amount (D2).  Verified structure: CPMM swap_base_output grosses the input up
    with ceil(net*1e6/(1e6-rate)) and prices on vault minus accrued protocol/fund/
    creator fees (raydium-io/raydium-cp-swap@59fb845a curve/fees.rs,
    calculator.rs, states/pool.rs); AMM v4 swap fee 25/10000 of input, 12 % of it
    (pnl) leaves the reserves (raydium-io/raydium-amm@d26944bf state.rs).
    Exact rounding and per-pool fee configs are reconciled in the architecture
    step against docs/research/raydium-amm.md."""
    kind = "ray"
    lp_mutable = True

    def __init__(self, X: int, Y: int, fee_ppm: int = 2500, out_share_ppm: int = 120_000,
                 creator_ppm: int = 0, supply: int = PUMP_SUPPLY, name: str = "AMMv4"):
        self.X, self.Y = X, Y
        self.fee_ppm, self.out_share_ppm, self.creator_ppm = fee_ppm, out_share_ppm, creator_ppm
        self.supply, self.name = supply, name
        self.disabled = False

    def eff(self):
        return self.X, self.Y

    def fees(self):
        return ((self.fee_ppm + self.creator_ppm) // 100,)

    def has_route(self):
        return not self.disabled and self.X > 0 and self.Y > 0

    def _gross(self, net: int) -> int:
        return cdiv(net * PPM, PPM - self.fee_ppm - self.creator_ppm)

    def _keep(self, fee: int) -> int:
        """Part of a fee that stays in the pricing reserves (LP share)."""
        tot = self.fee_ppm + self.creator_ppm
        cre = fee * self.creator_ppm // tot if tot else 0
        trade = fee - cre
        return trade - trade * self.out_share_ppm // PPM

    def quote_buy_on(self, q: int, X: int, Y: int, fees=None):
        if q <= 0:
            return (0, 0)
        if q >= X:
            return None
        net = cdiv(Y * q, X - q)
        return net, self._gross(net)

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

    def remove_liquidity(self, frac_bps: int) -> None:
        self.X -= self.X * frac_bps // BPS
        self.Y -= self.Y * frac_bps // BPS

    def add_liquidity(self, frac_bps: int) -> None:
        self.X += self.X * frac_bps // BPS
        self.Y += self.Y * frac_bps // BPS


# ----------------------------------------------------------------------------
# Protocol parameters
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
    ema_tau_s: int             # PROPOSED
    ema_dtmax_s: int           # PROPOSED
    ema_clamp_bps: int         # PROPOSED


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
    rent_protocol_bps: int = 1500          # 85/15
    pen_lender_bps: int = 5000             # 50/20/30
    pen_protocol_bps: int = 2000
    pen_keeper_bps: int = 3000
    expiry_fee_bps: int = 200              # 2 % (base: quoted reference cost, C0-14)
    settle_fixed: int = 10_000_000         # 0.01 SOL
    settle_bps: int = 100                  # 1 % of vault
    settle_cap: int = 250_000_000          # Q4 default 0.25 SOL (PROPOSED, adopted)
    util_kink_bps: int = 8000
    util_cap_bps: int = 9000
    wallet_cap_bps: int = 2500             # Q10: 25 % of the market OI cap
    s3_wait_s: int = 600
    grad_grace_s: int = 600
    # ---- PROPOSED, first pass (kept) ---------------------------------------------------
    curve_oi_gamma_bps: int = 5000         # Tier D: sum(Q) <= 50 % of real_token_reserves
    max_migration_pause_s: int = 3600      # Q5
    migration_delay_s: int = 2             # observed ~6 slots
    two_leg: bool = True                   # B2 pricing when Q > real reserves
    liq_ema: bool = True                   # EMA leg also uses EMA of liquidity
    open_ema_dev_bps: int = 1000
    margin_policy: str = "cr"              # D4 "cr"; "notional" = m * N_open (static table only)
    fee_size_cap_x100: int = 300           # PROPOSED: per position q/X <= 3.00 x one-way venue fee
    fee_oi_cap_x100: int = 750             # PROPOSED: market sum(Q)/X <= 7.50 x one-way venue fee
    # ---- D4 ----------------------------------------------------------------------
    margin_ema_own_sale: bool = False      # SPEC-literal (V1-4); True = EMA leg shifted by the open's own sale (OWNER O2)
    # ---- D3 / C0-7 -----------------------------------------------------------------
    fv_cap: bool = True                    # S2/S3 only; S1 never capped
    fv_liq: str = "ema"                    # SPEC-literal (V1-3/V1-4): fast liquidity EMA; "max" = max(L_ema, L_slow) (OWNER O1)
    settle_precedence: str = "S1"          # V1-3: S1 whenever all S1 legs hold (D3); "S2" = v0.2 reading (OWNER O1)
    fv_dev_bps: int = 100                  # V2-9: S2/S3 settle only while |P_ema/median(ring obs) - 1| <= 1 % (and L likewise)
    liq_slow_tau_s: int = 3600
    liq_slow_dtmax_s: int = 60
    # ---- C0-5 observation buckets, EMA ring, persistence ---------------------------
    ring_len: int = 8                      # EMA history entries kept on the market
    ref_max_age_s: int = 2                 # last completed bucket must be <= 2 s old
    ref_lo_k: int = 1                      # fresh reference = low of the last completed bucket (constant 1 on-chain, V1-12)
    liq_persist_n: int = 3                 # CR_ema < L at >= 3 consecutive EMA updates ...
    liq_persist_s: int = 2                 # ... spanning >= 2 s
    settle_persist_n: int = 40             # S1/S2 held at >= 40 EMA updates ...
    settle_persist_s: int = 30             # ... spanning >= 30 s
    settle_arm_gap_s: int = 10             # re-checks <= 10 s apart, else re-arm
    # ---- C0-1 / C0-14 exit bounds ------------------------------------------------------
    exit_ref: str = "fresh"                # liquidation reference: "fresh" | "ema"
    kappa_liq_bps: int = 2500              # fresh reference capped at P_ema * (1 + kappa_eff) with
    kappa_base_bps: int = 0                #   kappa_eff = min(kappa, kappa_base + (tau+1) * g_ema),
    kappa_growth_n: int = 3                #   g_ema = min per-second EMA growth over the last 3 updates
    delta_liq_min_bps: int = 100           # liquidation bound: Dutch start at the first executable second ...
    delta_liq_slope_bps: int = 100         # ... + 1 %/s after that ...
    delta_liq_max_bps: int = 500           # ... up to 5 %
    delta_exp_bps: int = 100               # expiry bound over min(EMA, ring min, last bucket low)
    dutch_clock: str = "exec"              # V1-1: "exec" = seconds in which both legs held; "ema" = v0.2 (EMA run)
    ref_liq: str = "max3"                  # V2-14: exit-reference liquidity max(L_live, L_ema, L_slow); "max2" = v0.2
    arming: str = "v3"                     # V2-4: only non-atomic evidence disarms; "v2" = v0.2 (any false check disarms)
    obs_when_halted: bool = False          # V2-5: no observations while the venue cannot execute (v0.2: True)
    straddle_exec: str = "bundle"          # V1-9: keeper completes the curve, position buys Q on the new pool; "self2" = v0.2
    exec_tol_bps: int = 10                 # epsilon: venue rounding drift
    exec_rent_tol: int = 6_500_000         # venue-side rent budget (lamports; worst case 5,811,520, C1-9)
    expiry_keeper_fixed: int = 1_000_000   # expiry keeper reward: 0.001 SOL ...
    expiry_keeper_bps: int = 10            # ... + 0.1 % of the reference cost ...
    expiry_keeper_cap: int = 50_000_000    # ... capped at 0.05 SOL
    # ---- C0-16 ---------------------------------------------------------------------
    s3_obs_gap_s: int = 60                 # no-route observations > 60 s apart restart the S3 timer
    # ---- checks ----------------------------------------------------------------------
    check_invariants: bool = True


DEFAULT = Params()


def rate_bps(tier: Tier, util_bps: int, kink: int = 8000) -> int:
    return (tier.rate_base_bps + tier.rate_s1_bps * min(util_bps, kink) // BPS
            + tier.rate_s2_bps * max(util_bps - kink, 0) // BPS)


def max_q_impact(X: int, impact_bps: int) -> int:
    """Largest Q whose buyback from (X+Q) [post-sell reserves] moves marginal
    price by <= impact: Q <= X*(sqrt(1+i) - 1)."""
    return math.isqrt(X * X * (BPS + impact_bps) // BPS) - X


def max_total_q_market(X_now: int, impact_bps: int) -> int:
    """Largest total outstanding Q whose buyback from current reserves moves
    marginal price by <= impact: sum(Q) <= X*(1 - 1/sqrt(1+i))."""
    return X_now - math.isqrt(X_now * X_now * BPS // (BPS + impact_bps))


# ----------------------------------------------------------------------------
# Position & market engine
# ----------------------------------------------------------------------------
class Position:
    __slots__ = ("Q", "S", "M", "V", "fee_open", "I_open", "R_min", "t_open", "deadline", "B_open",
                 "B_margin", "B_ema_open", "spot_pre", "spot_post", "L", "pen_bps",
                 "s_kind", "s_count", "s_t", "s_last")
    # s_kind: v0.3 uses a single eligibility clock; s_kind is "armed" (any kind) or None

    def __init__(self, **kw):
        self.s_kind = None
        self.s_count = self.s_t = self.s_last = 0
        for k, v in kw.items():
            setattr(self, k, v)

    def disarm(self):
        self.s_kind = None


class InvariantError(AssertionError):
    pass


class Market:
    def __init__(self, venue: Venue, tier: Tier, P: Params = DEFAULT, util_bps: int = 8000, t0: int = 0):
        self.venue, self.tier, self.P = venue, tier, P
        if isinstance(venue, PumpCurve):
            venue.straddle_exec = P.straddle_exec
        self.util_bps = util_bps
        self.t = t0
        self.ema = venue.spot_wad()
        self.liq = venue.sqrt_k()
        self.ema_t = t0
        self.liq_slow = self.liq
        self.slow_t = t0
        self.ema_count = 0
        # ring entry: (ts, P_ema, L_ema, count, low of the bucket integrated, obs, lobs) -- obs/lobs are the
        # directional-aggregated observations that moved the EMA (V2-9 FV gate), low feeds the Dutch clock (V1-1)
        self.ring = [(t0, self.ema, self.liq, 0, None, self.ema, self.liq)]
        self.b_sec: Optional[int] = None          # current observation bucket (second)
        self.b_lo = self.b_hi = self.b_llo = self.b_lhi = 0
        self.b_n = 0
        self.lb_sec: Optional[int] = None         # last completed bucket
        self.lb_lo = self.lb_hi = 0
        self.lb_hist = []                         # [(sec, lo)] of recent completed buckets
        self.index = 0
        self.index_t = t0
        self.complete_t: Optional[int] = None
        self.graduated_t: Optional[int] = None
        self.no_route_since: Optional[int] = None
        self.last_no_route_obs: Optional[int] = None
        self.pending_target: Optional[int] = None
        self._q_hint = 0                          # sim helper: size of the position under test

    # --- time evolution ---------------------------------------------------
    def rate(self) -> int:
        return rate_bps(self.tier, self.util_bps, self.P.util_kink_bps)

    def accrue(self, t: int) -> None:
        dt = t - self.index_t
        if dt > 0:
            # index: lamports per raw token unit * WAD, rounded up (pool-favourable),
            # on the EMA as it stood before this instruction's EMA update (left endpoint)
            self.index += cdiv(self.ema * self.rate() * dt, BPS * YEAR_S)
            self.index_t = t

    def migrating(self) -> bool:
        return self.venue.kind == "curve" and self.venue.complete

    def advance(self, t: int) -> None:
        """Exogenous state up to t: graduation state machine, boost, rent accrual,
        continuation of exogenous price moves across migration."""
        if self.migrating():
            if self.complete_t is None:
                self.complete_t = t
            if t >= self.complete_t + self.P.migration_delay_s:
                self._switch_to_pool(self.venue.migrate(), t)
        v = self.venue
        if isinstance(v, CPPool):
            v.boost_tick(t)
        self.accrue(t)
        self.t = max(self.t, t)
        if self.pending_target is not None and self.venue.has_route():
            tgt, self.pending_target = self.pending_target, None
            self.exo_move(tgt)

    def observe(self, t: int) -> None:
        """Prologue of every short.win instruction touching the market (and of
        crank_price): roll the observation bucket, record the S3 route status,
        then add this instruction's pre-trade spot to the current bucket."""
        P = self.P
        if self.b_sec is not None and t > self.b_sec:
            self._integrate(t)
        v = self.venue
        if v.has_route():
            self.no_route_since = None
            self.last_no_route_obs = None
        else:
            if (self.no_route_since is None or self.last_no_route_obs is None
                    or t - self.last_no_route_obs > P.s3_obs_gap_s):
                self.no_route_since = t                  # C0-16: continuous chain only
            self.last_no_route_obs = t
        if v.has_price() and (v.has_route() or self.migrating() or P.obs_when_halted):
            # V2-5: a venue that cannot execute (halted pool, fee above the hard cap) is not
            # observed, so P_ema is not re-weighted towards a frozen pool; the completed
            # (migrating) curve is final and cannot be traded, so it is still recorded.
            p, lq = v.spot_wad(), v.sqrt_k()
            if self.b_sec != t:
                self.b_sec, self.b_lo, self.b_hi, self.b_llo, self.b_lhi, self.b_n = t, p, p, lq, lq, 1
            else:
                self.b_lo, self.b_hi = min(self.b_lo, p), max(self.b_hi, p)
                self.b_llo, self.b_lhi = min(self.b_llo, lq), max(self.b_lhi, lq)
                self.b_n += 1

    def _integrate(self, t: int) -> None:
        """Integrate the completed bucket into the EMA (C0-5): the observation is
        the EMA clamped into [bucket low, bucket high] -- it moves up only if every
        observation of that second was above the EMA (to the lowest of them) and
        down only if every one was below (to the highest) -- then clamped to
        +-C, time-weighted with dt <= DT_MAX."""
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
            dts = min(t - self.slow_t, P.liq_slow_dtmax_s)
            self.liq_slow = (self.liq_slow * P.liq_slow_tau_s + lobs * dts) // (P.liq_slow_tau_s + dts)
            self.slow_t = t
            self.ema_t = t
            self.ema_count += 1
            self.ring.append((t, self.ema, self.liq, self.ema_count, self.b_lo, obs, lobs))
            if len(self.ring) > P.ring_len:
                del self.ring[0]
        self.lb_sec, self.lb_lo, self.lb_hi = self.b_sec, self.b_lo, self.b_hi
        self.lb_hist.append((self.b_sec, self.b_lo))
        if len(self.lb_hist) > 8:
            del self.lb_hist[0]
        self.b_sec = None

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
            self.ring.append((t, self.ema, self.liq, self.ema_count, self.ema, self.ema, self.liq))
            if len(self.ring) > self.P.ring_len:
                del self.ring[0]

    def _switch_to_pool(self, pool: CPPool, t: int) -> None:
        self.venue = pool
        self.graduated_t = t
        pool.start_boost(t)
        self.liq = pool.sqrt_k()        # reset liquidity EMAs on venue switch
        self.liq_slow = self.liq
        self.ring = [(te, pe, self.liq, ce, lo, ob, self.liq) for (te, pe, _l, ce, lo, ob, _lo) in self.ring]
        if self.b_sec is not None:
            self.b_llo = self.b_lhi = self.liq

    def exo_move(self, p_wad: int) -> None:
        """Other market participants trade the venue toward price p_wad."""
        v = self.venue
        if not v.has_route():
            self.pending_target = p_wad
            return
        v.move_to_price(p_wad)
        if v.kind == "curve" and v.complete:
            self.pending_target = p_wad   # continue on the AMM after migration

    # --- quotes ---------------------------------------------------------------
    def quote_spot(self, Q: int):
        v = self.venue
        if not v.has_route():
            return None
        return v.quote_at(Q, *v.eff(), two_leg=self.P.two_leg)

    def quote_repriced(self, Q: int, p_wad: int, L: int):
        st = reprice(L, p_wad)
        if st is None:
            return None
        return self.venue.quote_at(Q, st[0], st[1], two_leg=self.P.two_leg)

    def ema_liq(self) -> int:
        return self.liq if self.P.liq_ema else self.venue.sqrt_k()

    def quote_ema(self, Q: int):
        return self.quote_repriced(Q, self.ema, self.ema_liq())

    def slip_at(self, Q: int, X: int, Y: int) -> int:
        """Average execution premium over the marginal price of state (X, Y), ex fees (SPEC S2 slippage)."""
        q = self.venue.quote_at(Q, X, Y, two_leg=self.P.two_leg)
        if q is None:
            return INF
        p = Y * WAD // X
        return q[0] * WAD * BPS // (Q * p) - BPS

    def slip_repriced(self, Q: int, p_wad: int, L: int) -> int:
        st = reprice(L, p_wad)
        return INF if st is None else self.slip_at(Q, *st)

    # --- position lifecycle ----------------------------------------------------
    def rent_due(self, pos: Position) -> int:
        return max(cdiv(pos.Q * (self.index - pos.I_open), WAD), pos.R_min)

    def oi_cap_q(self, X_after: int) -> int:
        """Per-market cap on total outstanding Q, evaluated on post-open reserves:
        SPEC impact cap, tightened on low-fee venues (fee_oi_cap)."""
        cap = max_total_q_market(X_after, self.tier.impact_mkt_bps)
        if self.P.fee_oi_cap_x100:
            cap = min(cap, X_after * self.venue.fee_oneway_bps() * self.P.fee_oi_cap_x100 // (100 * BPS))
        return cap

    def max_open_q(self) -> int:
        v = self.venue
        X0 = v.eff()[0]
        q = max_q_impact(X0, self.tier.impact_pos_bps)
        q = min(q, self.oi_cap_q(X0 + q))
        if v.kind == "curve":   # Tier D graduation guard: Q <= gamma*(rt+Q)
            g = self.P.curve_oi_gamma_bps
            q = min(q, v.rt * g // (BPS - g))
        if self.P.fee_size_cap_x100:
            q = min(q, X0 * v.fee_oneway_bps() * self.P.fee_size_cap_x100 // (100 * BPS))
        return q

    def open(self, Q: int, t: int) -> Position:
        self.advance(t)
        self.observe(t)
        v, T, P = self.venue, self.tier, self.P
        spot_pre = v.spot_wad()
        ema_pre, liq_pre = self.ema, self.ema_liq()
        S = v.sell(Q)
        qs = self.quote_spot(Q)
        assert qs is not None
        # D4: margin makes (M + S) / B >= CR_open at spot and at EMA.  The EMA leg is
        # evaluated on the EMA reserves shifted by this open's own sale (PROPOSED
        # reading; margin_ema_own_sale=False gives the unshifted variant).
        Xe, Ye = reprice(liq_pre, ema_pre)
        if P.margin_ema_own_sale:
            Xe, Ye = Xe + Q, cdiv(Xe * Ye, Xe + Q)
        qe = v.quote_at(Q, Xe, Ye, two_leg=P.two_leg)
        B_margin = max(qs[1], qe[1] if qe else qs[1])
        if P.margin_policy == "cr":
            M = max(cdiv(T.cr_open_bps * B_margin, BPS) - S, 0)
        else:
            M = cdiv(T.m_bps * qs[1], BPS)
        fee_open = cdiv(P.open_fee_bps * qs[1], BPS)
        R_min = cdiv(Q * self.ema * self.rate() * T.min_rent_s, WAD * BPS * YEAR_S)
        return Position(Q=Q, S=S, M=M, V=S + M, fee_open=fee_open, I_open=self.index, R_min=R_min,
                        t_open=t, deadline=t + T.max_dur_s, B_open=qs[1], B_margin=B_margin,
                        B_ema_open=qe[1] if qe else 0, spot_pre=spot_pre, spot_post=v.spot_wad(),
                        L=T.cr_liq_bps, pen_bps=T.penalty_bps)

    def settle_reward(self, V: int) -> int:
        P = self.P
        return min(P.settle_fixed + V * P.settle_bps // BPS, P.settle_cap, V)

    def expiry_keeper(self, b_ref: int) -> int:
        P = self.P
        return min(P.expiry_keeper_fixed + b_ref * P.expiry_keeper_bps // BPS, P.expiry_keeper_cap)

    def _ring_run(self, Q: int, pred):
        """Walk the EMA ring from the newest entry back while pred(quote) holds.
        Returns (count, oldest_t, newest_t)."""
        n, oldest, newest = 0, None, None
        for (te, pe, le, *_rest) in reversed(self.ring):
            q = self.quote_repriced(Q, pe, le)
            if not pred(q):
                break
            n += 1
            oldest = te
            if newest is None:
                newest = te
        return n, oldest, newest

    def status(self, pos: Position, t: int) -> dict:
        P = self.P
        R = self.rent_due(pos)
        Veff = pos.V - R
        L = pos.L
        v = self.venue
        live = v.has_route()
        qs = self.quote_spot(pos.Q) if live else None
        p_live = v.spot_wad() if v.has_price() else None
        lo_ok = self.lb_sec is not None and t - self.lb_sec <= P.ref_max_age_s
        p_rs = min(p_live, self.lb_lo) if (lo_ok and p_live is not None) else None
        L_live = v.sqrt_k() if v.has_price() else self.liq
        qrs = self.quote_repriced(pos.Q, p_rs, L_live) if p_rs is not None else None
        qe = self.quote_ema(pos.Q)

        def cr(q):
            return Veff * BPS // q[1] if (q and q[1] > 0) else 0

        # liquidation, EMA leg: CR_ema < L at the last >= N updates spanning >= M seconds
        below = lambda q: q is None or Veff * BPS // q[1] < L
        n, oldest, newest = self._ring_run(pos.Q, below)
        ema_leg = n >= P.liq_persist_n and newest - oldest >= P.liq_persist_s
        if not live:
            spot_leg = True                      # SPEC: no route -> CR_spot := 0
        else:
            spot_leg = lo_ok and (qrs is None or cr(qrs) < L)
        # V1-1: executable run = newest ring entries whose EMA state AND whose integrated
        # bucket low (the robust-spot leg of that second) were both below L.  One blocked
        # second (a low observation that fails the spot leg) breaks the run.
        ex_oldest = None
        if ema_leg:
            for (te, pe, le, _c, lo, *_r) in reversed(self.ring):
                if lo is None or not (below(self.quote_repriced(pos.Q, pe, le))
                                      and below(self.quote_repriced(pos.Q, lo, le))):
                    break
                ex_oldest = te
        return dict(R=R, Veff=Veff, qs=qs, qrs=qrs, qe=qe, p_live=p_live, p_rs=p_rs, lo_ok=lo_ok,
                    cr_s=cr(qs), cr_rs=cr(qrs) if qrs else None, cr_e=cr(qe),
                    liq=spot_leg and ema_leg, ema_leg=ema_leg, spot_leg=spot_leg,
                    liq_armed_at=oldest if n else None, liq_exec_since=ex_oldest,
                    ema_run=n, expired=t >= pos.deadline)

    def ema_growth_bps(self) -> int:
        """Sustained EMA growth: the MINIMUM per-second growth (bps) over the last
        kappa_growth_n integrations (a single held-pump second cannot open the cap)."""
        n = self.P.kappa_growth_n
        if len(self.ring) < n + 1:
            return 0
        g = None
        for (t0, p0, *_r0), (t1, p1, *_r1) in zip(self.ring[-n - 1:-1], self.ring[-n:]):
            dt = t1 - t0
            if dt <= 0 or p0 <= 0:
                return 0
            gi = (p1 - p0) * BPS // (p0 * dt)
            g = gi if g is None else min(g, gi)
        return max(0, g or 0)

    def kappa_eff(self) -> int:
        P = self.P
        if P.kappa_base_bps >= P.kappa_liq_bps:
            return P.kappa_liq_bps
        return min(P.kappa_liq_bps, P.kappa_base_bps + (self.tier.ema_tau_s + 1) * self.ema_growth_bps())

    def lo_ref(self, t: int) -> int:
        """Robust fresh spot: min low of the completed buckets of the last ref_lo_k seconds."""
        k = self.P.ref_lo_k
        lows = [lo for (sec, lo) in self.lb_hist if sec >= self.lb_sec - k + 1]
        return min(lows) if lows else self.lb_lo

    def exit_reference(self, pos: Position, st: dict, t: int, is_liq: bool):
        """(quote at the reference price, delta_bps) bounding a keeper-initiated
        buyback, or None if no fresh bucket is available (crank first)."""
        P = self.P
        if not st["lo_ok"]:
            return None
        lo = self.lo_ref(t)
        v = self.venue
        L_b = max(v.sqrt_k(), self.liq)          # deeper of live and EMA liquidity -> lower bound
        if P.ref_liq == "max3":
            L_b = max(L_b, self.liq_slow)        # V2-14: quiet-second LP pulls cannot lower the reference
        if is_liq:
            if P.exit_ref == "ema":
                pref = self.ema
            else:
                # bucket low extrapolated by the sustained EMA growth over its age (<= 2 s)
                g = self.ema_growth_bps()
                lo_x = lo * (BPS + g * max(0, t - self.lb_sec)) // BPS
                pref = min(max(self.ema, lo_x), self.ema * (BPS + self.kappa_eff()) // BPS)
            start = st["liq_exec_since"] if P.dutch_clock == "exec" else st["liq_armed_at"]
            since = (start + P.liq_persist_s) if start is not None else t
            delta = min(P.delta_liq_min_bps + P.delta_liq_slope_bps * max(0, t - since), P.delta_liq_max_bps)
        else:
            pref = min(self.ema, min(e[1] for e in self.ring), lo)   # (ring entries: ts, P, L, n, low)
            delta = P.delta_exp_bps
        q = self.quote_repriced(pos.Q, pref, L_b)
        if q is None:
            return None
        return q, delta, pref

    def try_exit(self, pos: Position, t: int, borrower_close: bool = False, borrower_limit: Optional[int] = None,
                 settle_only: bool = False):
        """Keeper liquidation / expiry / settlement or borrower close at time t.
        Returns a result dict, or None if nothing is executable now.
        settle_only=True models a standalone sol_settle call (no buyback attempted)."""
        self.advance(t)
        self.observe(t)
        P, T = self.P, self.tier
        v = self.venue
        spot_before = v.spot_wad() if v.has_price() else self.ema
        st = self.status(pos, t)
        V, Q = pos.V, pos.Q
        ert = P.exec_rent_tol
        res = dict(t=t, spot=spot_before, B=0, rent=0, pen=0, fee=0, keeper=0, protocol=0,
                   lender_sol=0, tokens=0, borrower=0, kind=None, cr_s=st["cr_s"], cr_e=st["cr_e"],
                   bound=None, base=None, fv=None)

        if borrower_close:
            qs = st["qs"]
            R = st["R"]
            if qs is None or qs[1] + R + ert > V:
                return None
            if borrower_limit is not None and qs[1] > borrower_limit:
                return None
            B = self._buy_back(Q, t)
            rp = R * P.rent_protocol_bps // BPS
            res.update(kind="close", B=B, rent=R, protocol=rp, lender_sol=R - rp, tokens=Q,
                       borrower=V - B - R)
            return res

        if not (st["liq"] or st["expired"]):
            # V2-4: "not liquidatable" is a no-op unless the EMA leg (non-atomic evidence)
            # fails; a failing spot leg can be produced inside one transaction.
            if P.arming == "v2" or not st["ema_leg"]:
                pos.disarm()
            return None
        K = self.settle_reward(V)

        # --- no route: migration pause / S3 -----------------------------------
        if not v.has_route():
            if self.migrating():
                paused_for = t - (self.complete_t if self.complete_t is not None else t)
                if paused_for < P.max_migration_pause_s:
                    return None                     # spec: liquidations pause during migration
                mig_start = self.complete_t + P.max_migration_pause_s
            else:
                mig_start = -10**12
            if self.no_route_since is None:
                return None
            start = max(self.no_route_since, (self.graduated_t if self.graduated_t is not None else -10**12)
                        + P.grad_grace_s, mig_start)
            if t >= start + P.s3_wait_s:
                return self._settle(res, "S3", pos, K, st)
            return None

        qs = st["qs"]
        s1_live = qs is None or qs[1] + ert > V - K
        s2_live = self.slip_at(Q, *v.eff()) > T.max_liq_slip_bps
        if not (s1_live or s2_live):
            if P.arming == "v2":
                pos.disarm()
            if settle_only:
                return None
            is_liq = st["liq"]
            ref = self.exit_reference(pos, st, t, is_liq)
            if ref is None:
                return None
            qref, delta, pref = ref
            b_ref = qref[1]
            R = st["R"]
            # C0-1(3): rent is senior to the buyback wherever the fair (reference) cost leaves
            # room for it; penalty / expiry fee stay junior to the buyback (SPEC §5.2 order)
            # but their base is the quoted reference cost, so a sandwich cannot enlarge them.
            senior = R
            bound = b_ref * (BPS + delta) // BPS
            max_in = min(bound, V - K - ert, max(V - senior - ert, b_ref * (BPS + P.exec_tol_bps) // BPS))
            if qs[1] > max_in:
                return None                         # wait: buyback above the reference bound
            b_quote = qs[1]
            B = self._buy_back(Q, t)
            if P.check_invariants and not (b_quote - 5 <= B <= b_quote):   # quote rounds each fee up
                raise InvariantError(("venue charge outside [quote-5, quote]", B, b_quote))
            base = min(b_quote, b_ref)             # C0-1(2), C0-14: fee base on quoted costs
            W = V - B
            r_paid = min(R, W)
            W -= r_paid
            rp = r_paid * P.rent_protocol_bps // BPS
            res.update(B=B, rent=r_paid, tokens=Q, bound=bound, base=base, b_ref=b_ref, max_in=max_in,
                       senior=senior)
            if is_liq:
                pen = min(cdiv(pos.pen_bps * base, BPS), W)
                W -= pen
                kp = pen * P.pen_keeper_bps // BPS
                pp = pen * P.pen_protocol_bps // BPS
                res.update(kind="liquidation", pen=pen, keeper=kp, protocol=rp + pp,
                           lender_sol=(r_paid - rp) + (pen - kp - pp), borrower=W)
            else:
                fee = min(cdiv(P.expiry_fee_bps * base, BPS), W)
                W -= fee
                ke = min(self.expiry_keeper(b_ref), W)
                W -= ke
                res.update(kind="expiry", fee=fee, keeper=ke, protocol=rp, lender_sol=(r_paid - rp) + fee,
                           borrower=W)
            if P.check_invariants:
                self.check_exit(pos, res, st, K)
            return res

        # --- S1 / S2 with persistence ("armed") --------------------------------------
        if P.arming == "v2":                    # v0.2: any false evaluation disarms; kind change re-arms
            if P.settle_precedence == "S2" and s2_live and self._settle_cond(pos, st, "S2", K):
                kind = "S2"
            elif s1_live and self._settle_cond(pos, st, "S1", K):
                kind = "S1"
            elif s2_live and self._settle_cond(pos, st, "S2", K):
                kind = "S2"
            else:
                pos.disarm()
                return None
            if pos.s_kind != kind or t - pos.s_last > P.settle_arm_gap_s:
                pos.s_kind, pos.s_count, pos.s_t = kind, self.ema_count, t
            pos.s_last = t
            if self.ema_count - pos.s_count >= P.settle_persist_n and t - pos.s_t >= P.settle_persist_s:
                return self._settle(res, kind, pos, K, st)
            return None
        # v0.3 (V2-4): one eligibility clock for S1 and S2; only evidence that no caller can
        # produce inside one transaction (EMA state) disarms; the kind is chosen at execution.
        if self._refuted(pos, st, K):
            pos.disarm()
            return None
        if not st["lo_ok"]:
            return None                          # stale bucket: no state change
        s1 = s1_live and self._settle_cond(pos, st, "S1", K)
        s2 = s2_live and self._settle_cond(pos, st, "S2", K)
        armed = pos.s_kind is not None and t - pos.s_last <= P.settle_arm_gap_s
        if not (s1 or s2):
            if armed:
                pos.s_last = t                   # not refuted: the clock keeps running
            return None
        if not armed:
            pos.s_kind, pos.s_count, pos.s_t = "armed", self.ema_count, t
        pos.s_last = t
        if self.ema_count - pos.s_count < P.settle_persist_n or t - pos.s_t < P.settle_persist_s:
            return None
        if P.settle_precedence == "S1":          # V1-3 / D3: S1 whenever all its legs hold
            kind = "S1" if s1 else "S2"
        else:
            kind = "S2" if s2 else "S1"
        if kind == "S2" and not self._fv_ok():
            return None                          # V2-9: EMA off its ring median -> wait
        return self._settle(res, kind, pos, K, st)

    def _refuted(self, pos: Position, st: dict, K: int) -> bool:
        """V2-4: non-atomic evidence that neither S1 nor S2 holds: the EMA says the vault
        covers the buyback, and slippage at the EMA price with the LOWER of live and EMA
        liquidity is within sigma_max (an atomic LP add cannot raise that liquidity)."""
        P, T = self.P, self.tier
        qe = st["qe"]
        s1_ema = qe is None or qe[1] + P.exec_rent_tol > pos.V - K
        L2 = min(self.venue.sqrt_k(), self.ema_liq()) if self.venue.has_price() else self.ema_liq()
        s2_ema = self.slip_repriced(pos.Q, self.ema, L2) > T.max_liq_slip_bps
        return not (s1_ema or s2_ema)

    def _fv_ok(self) -> bool:
        """V2-9: FV reads P_ema / L_ema (D3); settle only while both are within fv_dev_bps
        of the median of the ring's integrated observations.  One attacker-only second
        moves P_ema up to +16.7 % (and leaves a bump that decays over ~10 s) but moves only
        one of the 8 observations, so the median ignores it.  A venue that cannot execute
        records no observations (V2-5), so its EMA cannot be pushed at all."""
        P = self.P
        if not self.venue.has_route() or not P.fv_dev_bps:
            return True
        ps = sorted(e[5] for e in self.ring)
        ls = sorted(e[6] for e in self.ring)
        pm, lm = ps[(len(ps) - 1) // 2], ls[(len(ls) - 1) // 2]
        return (abs(self.ema - pm) * BPS <= P.fv_dev_bps * pm
                and abs(self.liq - lm) * BPS <= P.fv_dev_bps * lm)

    def _settle_cond(self, pos: Position, st: dict, kind: str, K: int) -> bool:
        """S1/S2 confirmed on the live quote, the robust spot (last bucket low)
        and the current EMA state (the persistence rule is applied by the caller)."""
        P, T = self.P, self.tier
        V, Q = pos.V, pos.Q
        ert = P.exec_rent_tol
        if not st["lo_ok"]:
            return False
        if kind == "S1":
            def over(q):
                return q is None or q[1] + ert > V - K
            return over(st["qs"]) and over(st["qrs"]) and over(st["qe"])
        smax = T.max_liq_slip_bps
        v = self.venue
        L_live = v.sqrt_k()
        return (self.slip_at(Q, *v.eff()) > smax
                and self.slip_repriced(Q, st["p_rs"], L_live) > smax
                and self.slip_repriced(Q, self.ema, self.ema_liq()) > smax)

    def fv_quote(self, Q: int):
        """Executable buyback cost at the EMA price on EMA reserves (D3)."""
        L = max(self.liq, self.liq_slow) if self.P.fv_liq == "max" else self.liq
        if self.migrating():
            pool = self.venue.migrate_preview()
            st = reprice(pool.sqrt_k(), self.ema)
            return pool.quote_buy_on(Q, *st) if st else None
        return self.quote_repriced(Q, self.ema, L)

    def _settle(self, res: dict, kind: str, pos: Position, K: int, st: dict) -> dict:
        """SOL settlement: token debt extinguished; lenders get vault minus keeper
        reward, capped at FV in S2/S3 only (D3); any excess to the borrower."""
        take = pos.V - K
        borrower = 0
        fv = None
        if kind in ("S2", "S3") and self.P.fv_cap:
            q = self.fv_quote(pos.Q)
            if q is not None:
                pen_bps = pos.pen_bps if st["liq"] else self.P.expiry_fee_bps
                fv = q[1] + cdiv(pen_bps * q[1], BPS) + st["R"]
                if take > fv:
                    borrower, take = take - fv, fv
        res.update(kind=kind, keeper=K, lender_sol=take, borrower=borrower, fv=fv)
        if self.P.check_invariants:
            if res["keeper"] + res["lender_sol"] + res["borrower"] != pos.V:
                raise InvariantError(("settle legs", res))
            if kind == "S1" and take != pos.V - K:
                raise InvariantError(("S1 capped", res))
        return res

    def check_exit(self, pos: Position, res: dict, st: dict, K: int) -> None:
        """Economic invariants of a keeper-initiated buyback (ECON §13)."""
        V = pos.V
        legs = res["B"] + res["rent"] + res["pen"] + res["fee"] + res["borrower"]
        if res["kind"] == "expiry":
            legs += res["keeper"]
        if legs != V or min(res["rent"], res["pen"], res["fee"], res["borrower"]) < 0:
            raise InvariantError(("waterfall", res, V))
        if res["B"] > res["bound"] or res["B"] > res["max_in"]:          # I-E1
            raise InvariantError(("exit above bound", res))
        if res["base"] > res["b_ref"]:                                   # I-E3
            raise InvariantError(("fee base above quoted reference", res))
        # I-E2: rent is paid in full whenever the fair buyback leaves room for it
        eps = res["b_ref"] * (BPS + self.P.exec_tol_bps) // BPS
        if V - res["senior"] - self.P.exec_rent_tol >= eps and res["rent"] != st["R"]:
            raise InvariantError(("rent shortfall despite reserve", res))
        # I-E3': penalty / fee never exceed the rate times the quoted reference cost
        want = cdiv((pos.pen_bps if res["kind"] == "liquidation" else self.P.expiry_fee_bps) * res["base"], BPS)
        got = res["pen"] if res["kind"] == "liquidation" else res["fee"]
        if got > want:
            raise InvariantError(("penalty/fee above rate x reference", res, want))

    def _buy_back(self, Q: int, t: int) -> int:
        v = self.venue
        if v.kind == "curve" and Q > v.rt:
            leg1_q = v.rt
            if self.P.straddle_exec == "bundle":
                # V1-9: tx1 the keeper completes the curve with its own SOL (it keeps the
                # tokens), tx2 migrate_v2, tx3 the position buys all of Q on the new pool.
                self.keeper_straddle_cost = v.buy(leg1_q) if leg1_q else 0
                pool = v.migrate()
                self._switch_to_pool(pool, t)
                return pool.buy(Q)
            cost = v.buy(leg1_q) if leg1_q else 0
            pool = v.migrate()
            self._switch_to_pool(pool, t)
            return cost + pool.buy(Q - leg1_q)
        return v.buy(Q)


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
    """In-house keeper duty (ECON §5.6): crank every second while a position is
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
        calm_step: int = 30, honest: bool = True):
    Q = max(1, int(market.max_open_q() * size_frac))
    market._q_hint = Q
    pos = market.open(Q, 0)
    P1 = pos.spot_post
    horizon = sc.horizon or (sc.t_event + sc.window)
    horizon = min(horizon, pos.deadline + 3 * 3600)
    events = sorted(sc.events, key=lambda e: e[0])
    ei = 0
    t = 0
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
    # value the lenders' outcome at the scenario's terminal price (v0.1 used the price at
    # the exit transaction, which understated S1 losses when a pump continued afterwards)
    t_end = sc.t_event + sc.window
    m_end = sc.path(t_end) if sc.t_event < 10**8 else None
    result["p_term"] = int(P1 * m_end) if m_end is not None else result["spot"]
    return pos, result, P1


def summarize(pos: Position, r: dict, P1: int):
    """Borrower P&L, lender outcome valued at the scenario's terminal price."""
    Q = pos.Q
    p_post = r.get("p_term", r["spot"])
    token_value = Q * p_post // WAD                 # lamports value of Q at the terminal price
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
    while the boost vault is active, Q15): mainnet CreatePoolEvent numbers."""
    p = CPPool.graduate(84_990_359_345)
    p.run_boost_to_end()
    return p


def venue_tier_D(progress: float):
    return PumpCurve.at_progress(progress)


def venue_ray(X_tokens: int, Y_sol: float, fee_ppm: int, out_share_ppm: int, name: str):
    return RaydiumPool(X_tokens, int(Y_sol * LAMPORTS_PER_SOL), fee_ppm=fee_ppm, out_share_ppm=out_share_ppm,
                       name=name)


MARKETS = {
    "A": lambda: venue_tier_A(),
    "B": lambda: venue_tier_B(),
    "C": lambda: venue_tier_C(),
    "D@0%": lambda: venue_tier_D(0.0),
    "D@50%": lambda: venue_tier_D(0.5),
    "D@80%": lambda: venue_tier_D(0.8),
    "D@90%": lambda: venue_tier_D(0.9),
    "D@97%": lambda: venue_tier_D(0.97),
    # Raydium examples (D2).  Fee configs are EXAMPLES (AMM v4 25 bps with 12 % pnl
    # share; CPMM 0.25 % with 16 % protocol+fund share) -- see raydium-amm.md.
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


def drain(frac_bps):
    def f(m: Market):
        if m.venue.lp_mutable:
            m.venue.remove_liquidity(frac_bps)
    return f


def disable_pool(m: Market):
    if isinstance(m.venue, (CPPool, RaydiumPool)):
        m.venue.disabled = True


TE = 3600
SCENARIOS = [
    Scenario("gap 2x", step_path(TE, 2.0)),
    Scenario("gap 5x", step_path(TE, 5.0)),
    Scenario("gap 10x", step_path(TE, 10.0)),
    Scenario("pump 10x/60s", ramp_path(TE, 10.0, 60)),
    Scenario("pump 10x/10min", ramp_path(TE, 10.0, 600)),
    Scenario("pump 3x/1h", ramp_path(TE, 3.0, 3600), window=4000),
    Scenario("spike 10x/5s", spike_path(TE, 10.0, 5), window=600),
    Scenario("drain 50%", lambda t: None, events=((TE, drain(5000)),), window=600),
    Scenario("drain 90%", lambda t: None, events=((TE, drain(9000)),), window=600),
    Scenario("drain 90%+1.5x", step_path(TE + 1, 1.5), events=((TE, drain(9000)),), window=600),
    Scenario("drain 97%+1.2x", step_path(TE + 1, 1.2), events=((TE, drain(9700)),), window=900),
    Scenario("drain 98%+1.2x", step_path(TE + 1, 1.2), events=((TE, drain(9800)),), window=900),
    Scenario("pool disabled", lambda t: None, events=((TE, disable_pool),), window=3600),
    Scenario("dump -50%, close", ramp_path(TE, 0.5, 3600), close_at=2 * TE, window=3700),
    Scenario("flat to expiry", lambda t: None, t_event=10**9, window=0, horizon=10**9),
]
SC = {s.name: s for s in SCENARIOS}


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


STATIC_KEYS = ["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC"]


def static_tables():
    out = []
    # --- opening CR under the D4 margin rule and the readings it replaced ---
    rows = []
    for key in STATIC_KEYS:
        T = tier_of(key)
        for frac in (1.0, 0.1):
            m, pos, q = opening_stats(key, frac)                  # default: SPEC-literal EMA leg (V1-4)
            m2, pos2, _ = opening_stats(key, frac, replace(DEFAULT, margin_ema_own_sale=True))   # OWNER O2 option
            S, B = pos.S, pos.B_open
            rho = S / B
            m_ = T.m_bps / BPS
            fee_ow = m.venue.fee_oneway_bps() if m.venue.has_route() else 0
            rows.append([key, f"{frac:.0%} of cap", f"{100*q:.2f}%", f"{fee_ow/100:.2f}%", f"{rho:.4f}",
                         f"{(S + m_ * B) / B:.3f}", f"{T.cr_open_bps/BPS:.2f}",
                         f"{100 * pos.M / B:.1f}%", f"{100 * pos2.M / pos2.B_open:.1f}%"])
    out.append("### Opening CR and margin under D4\n")
    out.append(table(["market", "size", "q=Q/X0", "venue fee (one way)", "rho=S/B_open",
                      "CR if M = m x B_open (literal column)", "table CR (enforced)",
                      "D4 margin, literal: EMA leg on EMA reserves (default; % of B_open)",
                      "D4 margin, EMA leg shifted by the open's own sale (option O2; % of B_open)"], rows))

    # --- example venues and position sizes at the caps ---
    rows = []
    for key in STATIC_KEYS + ["D@97%"]:
        m = new_market(key)
        v = m.venue
        X, Y = v.eff()
        supply = getattr(v, "supply", PUMP_SUPPLY)
        mcap = Y * supply // X
        oi_q = m.oi_cap_q(X)
        m2, pos, q = opening_stats(key, 1.0)
        kind = {"amm": "PumpSwap", "curve": "pump curve", "ray": getattr(v, "name", "Raydium")}[v.kind]
        rows.append([key, kind, f"{X/1e12:,.1f}M" if v.kind != "curve" else f"vt {X/1e12:,.1f}M / rt {v.rt/1e12:,.1f}M",
                     f"{sol(Y):,.2f}" + (" (virtual)" if v.kind == "curve" else ""), f"{sol(mcap):,.0f}",
                     f"{v.fee_oneway_bps()/100:.2f}%", f"{pos.Q/1e12:,.3f}M", f"{sol(pos.B_open):,.3f}",
                     f"{sol(pos.M):,.3f}", f"{sol(pos.V):,.3f}", f"{sol(pos.fee_open):,.4f}",
                     f"{sol(oi_q * v.spot_wad() // WAD):,.2f}", f"{sol(m2.settle_reward(pos.V)):,.4f}",
                     f"{sol(m2.expiry_keeper(pos.B_open)):,.4f}"])
    out.append("\n### Example venues (6-decimal tokens) and a position at the per-position cap\n")
    out.append(table(["market", "venue", "token reserve X", "SOL reserve Y", "mcap (SOL)", "venue fee one way",
                      "max Q per position", "N_open = B_open (SOL)", "margin M (SOL)", "vault V (SOL)",
                      "open fee (SOL)", "OI cap notional (SOL)", "S1-S3 keeper reward (SOL)",
                      "expiry keeper reward (SOL)"], rows))

    # --- price rise to liquidation ---
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

    # --- caps ---
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

    # --- rent ---
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

    # --- C1-6 curve round trip, C1-13 boost ---
    out.append("\n### Curve round trip (C1-6) and graduation boost (C1-13)\n")
    out.append(curve_round_trip_text())
    out.append(boost_text())
    return "\n".join(out)


def curve_round_trip_text() -> str:
    """C1-6: sell Q then immediately buy Q back on the same curve, vs the
    same-state bid/ask spread for size Q (state from R-PUMP §4.2)."""
    vt, vs, Q = 824_130_602_106_172, 39_059_343_535, 10_000_000_000_000
    c = PumpCurve(vt, vs, vt - 279_900_000_000_000, vs - 30_000_000_000)
    c0 = copy.deepcopy(c)
    proceeds = c.sell(Q)
    buyback = c.buy(Q)
    same_state = copy.deepcopy(c0).buy(Q)          # the program's charge (floor + 1); the quote rounds up by <= 1
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


def price_rise_to_liq(key: str, size_frac: float, rent_days):
    """Bisection on the spot multiple k at which CR_spot hits the tier's
    liquidation CR, with EMA := spot (no lag).  rent_days=None -> max duration."""
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
            if key.startswith("D") and sc.name.startswith(("drain", "pool disabled")):
                continue
            for frac in sizes:
                Pp = P
                if extra:
                    Pp = extra(sc, P)
                m = new_market(key, Pp)
                pos, r, P1 = run(m, frac, sc, keeper_latency=latency)
                s = summarize(pos, r, P1)
                t_exit = s["t"] - sc.t_event if sc.t_event < 10**8 else s["t"]
                bp = s["borrower_pnl"]
                rows.append([key, sc.name, f"{frac:.0%}", s["kind"], f"{t_exit:+d}s", f"{s['mult']:.2f}x / {s['term']:.2f}x",
                             f"{s['cr_s']:.2f}/{s['cr_e']:.2f}",
                             f"{sol(s['V']):.3f}", f"{sol(s['token_value']):.3f}",
                             "-" if bp is None else f"{sol(bp):+.4f} ({100*bp/s['M']:+.0f}% of M)",
                             f"{s['tokens']/pos.Q:.0%} + {sol(s['lender_sol']):.4f} SOL",
                             f"{sol(s['lender_pnl']):+.4f} ({s['lender_pnl_pct']:+.1f}%)",
                             f"{sol(s['keeper']):.4f}"])
    return rows


SCEN_HEADERS = ["market", "scenario", "size", "outcome", "exit vs event", "price at exit / terminal",
                "CR spot/EMA at exit", "vault SOL", "value of Q at terminal price (SOL)", "borrower P&L (SOL)",
                "lender gets", "lender P&L vs holding at terminal price (SOL, % of Q value)", "keeper SOL"]


def grad_extra(sc: Scenario, P: Params) -> Params:
    if "stuck" in sc.name:
        return replace(P, migration_delay_s=10**9)
    return P


def scenario_tables():
    out = []
    out.append("### Deterministic scenarios, size at the per-position cap (event at t = 1 h after open)\n")
    rows = scenario_rows(["A", "B", "C", "D@0%", "D@50%", "D@90%", "RA", "RB", "RC"], SCENARIOS, sizes=(1.0,))
    out.append(table(SCEN_HEADERS, rows))
    out.append("\n### Graduation mid-position (Tier D; event at t = 1 h; migration 2 s after completion; "
               "boost 20.69 % spent over 6 min)\n")
    rows = scenario_rows(["D@50%", "D@80%", "D@90%", "D@97%"], GRAD_SCENARIOS, sizes=(1.0,), extra=grad_extra)
    out.append(table(SCEN_HEADERS, rows))
    out.append("\n### Buyback larger than remaining real_token_reserves (Tier D opened at 97% progress)\n")
    rows = []
    for two_leg in (True, False):
        P = replace(DEFAULT, two_leg=two_leg)
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
            rows.append(["straddle quote B_str (bundle path)" if two_leg else "no straddle quote", sc.name, s["kind"], f"{s['t'] - TE:+d}s", f"{s['mult']:.3f}x",
                         f"{sol(r['B']):.4f}", f"{sol(pos.B_open):.4f}",
                         "-" if s["borrower_pnl"] is None else f"{sol(s['borrower_pnl']):+.4f}"])
    out.append(table(["policy", "scenario", "outcome", "exit vs event", "price at exit", "buyback cost (SOL)",
                      "B_open (SOL)", "borrower P&L (SOL)"], rows))
    out.append("\n### SOL settlements: S1 uncapped (SPEC), S2/S3 fair-value cap (D3) vs no cap\n")
    out.append(fv_table())
    out.append("\n### Keeper latency sensitivity (size at cap; 'pump 10x/60s' and 'gap 5x')\n")
    rows = []
    for lat in (1, 5, 30):
        for r in scenario_rows(["A", "C", "D@0%", "RA"], [SC["gap 5x"], SC["pump 10x/60s"]], latency=lat):
            rows.append([f"{lat}s"] + r)
    out.append(table(["keeper latency"] + SCEN_HEADERS, rows))
    return "\n".join(out)


def fv_table():
    rows = []
    cases = [("RA", SC["drain 97%+1.2x"]), ("RB", SC["drain 98%+1.2x"]), ("RC", SC["drain 98%+1.2x"]),
             ("D@80%", GRAD_SCENARIOS[3]), ("A", SC["gap 10x"]), ("D@0%", SC["gap 10x"])]
    for label, P in (("no cap", replace(DEFAULT, fv_cap=False)),
                     ("D3 literal: FV on the fast liquidity EMA, S1 first (default)", DEFAULT),
                     ("option O1: FV liquidity max(fast, slow EMA), S2 first",
                      replace(DEFAULT, fv_liq="max", settle_precedence="S2"))):
        for key, sc in cases:
            m = new_market(key, grad_extra(sc, P))
            pos, r, P1 = run(m, 1.0, sc)
            s = summarize(pos, r, P1)
            fv = r.get("fv")
            qx = m.fv_quote(pos.Q) if r["kind"] in ("S1", "S2", "S3") else None
            if qx is not None and m.P.fv_liq != "ema" and not m.migrating():
                qx = m.quote_repriced(pos.Q, m.ema, m.liq)          # benchmark: executable cost at (P_ema, L_ema)
            ex = "-" if qx is None else f"{sol(qx[1]):.4f}"
            rat = "-" if qx is None else f"{s['lender_sol'] / qx[1]:.3f}"
            rows.append([label, key, sc.name, s["kind"], f"{s['mult']:.2f}x",
                         f"{sol(pos.V - m.settle_reward(pos.V)):.4f}", "-" if fv is None else f"{sol(fv):.4f}",
                         f"{sol(s['lender_sol']):.4f}", ex, rat, f"{sol(s['token_value']):.4f}",
                         f"{s['lender_pnl_pct']:+.1f}%",
                         "-" if s["borrower_pnl"] is None else
                         f"{sol(s['borrower_pnl']):+.4f} ({100*s['borrower_pnl']/s['M']:+.0f}% of M)"])
    return table(["settlement rule", "market", "scenario", "outcome", "price at exit", "V - K_s (SOL)", "FV (SOL)",
                  "lender SOL", "executable cost of Q at EMA reserves (SOL)", "lender SOL / executable cost",
                  "spot value of Q (SOL)", "lender P&L vs holding (spot value)", "borrower P&L"], rows)


# ----------------------------------------------------------------------------
# EMA / exit-bound lag study (genuine moves; manipulation is in ./attacks/)
# ----------------------------------------------------------------------------
LAG_VARIANTS = [
    ("v0.3 default (tau 5 s, fresh ref, adaptive kappa <= 25 %, delta 1->5 %, persist 3/2 s)", {}),
    ("no liquidation persistence (1 update)", dict(liq_persist_n=1, liq_persist_s=0)),
    ("EMA-only reference, delta 16.7 % (critic's C0-1 suggestion)", dict(exit_ref="ema", delta_liq_min_bps=1667,
                                                                       delta_liq_slope_bps=0,
                                                                       delta_liq_max_bps=1667)),
    ("static kappa 25 % (no growth test)", dict(kappa_base_bps=2500)),
    ("tau 10 s (v0.1 value)", "tau10"),
]


def ema_study():
    out = ["### Exit bounds and persistence vs genuine fast moves (size at cap, keeper latency 1 s)\n"]
    rows = []
    pumps = [SC["pump 10x/60s"], Scenario("pump 10x/3min", ramp_path(TE, 10.0, 180)), SC["pump 10x/10min"],
             SC["gap 5x"]]
    for key in ["A", "C", "D@0%", "RA"]:
        for label, kw in LAG_VARIANTS:
            cells = [key, label]
            for sc in pumps:
                if kw == "tau10":
                    T = replace(tier_of(key), ema_tau_s=10)
                    m = Market(MARKETS[key](), T, DEFAULT)
                else:
                    m = new_market(key, replace(DEFAULT, **kw))
                pos, r, P1 = run(m, 1.0, sc)
                s = summarize(pos, r, P1)
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
    """Lender P&L vs holding, as a fraction of value of Q at the post-gap price,
    for an instantaneous gap of multiple k one hour after open (price holds)."""
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
            pts.append((k, l * k))          # convert to pre-gap notional units
        _LGG_CACHE[key] = pts
    return _LGG_CACHE[key]


def pareto_expectation(key: str, alpha: float, xmin: float, jmax: float = 50.0):
    """E_J[lender P&L] for one up-gap J ~ Pareto(alpha, xmin) truncated at jmax
    (mass above jmax placed at jmax), with the loss-given-gap curve linearly
    interpolated; result in units of the pre-gap value of Q."""
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
    sigma_annual: float        # diffusion vol
    lam_up_per_day: float      # up-jump intensity
    lam_dn_per_day: float      # down-jump intensity
    alpha: float = 1.5         # Pareto tail of jump multiple
    xmin: float = 1.5          # smallest "jump" (smaller moves live in the diffusion)
    jmax: float = 50.0


MC_ASSUMPTIONS = {
    # ILLUSTRATIVE ONLY -- not calibrated to market data (see ECONOMICS.md open questions)
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
    for regime in ("calm", "stressed"):
        rng = random.Random(seed)
        for key, coarse in (("A", 300), ("B", 300), ("C", 120), ("D@0%", 60), ("D@50%", 60), ("RA", 300)):
            A = MC_ASSUMPTIONS[regime][tier_of(key).name]
            n_kind = {}
            lp, lp2, bp, big = 0.0, 0.0, 0.0, 0
            for i in range(n):
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
        income = rate_bps(T, 8000) / BPS * T.max_dur_s / YEAR_S * 0.85 + DEFAULT.expiry_fee_bps / BPS
        cells = [key, f"{days:g} d", f"{100*income:.2f}%"]
        for alpha in (1.5, 2.0, 3.0):
            ej = pareto_expectation(key, alpha, 1.5)
            lam = income / (days * -ej) if ej < 0 else float("inf")
            cells.append(f"{100*ej:+.1f}% / 1 per {1/lam:,.0f} d" if lam != float("inf") else f"{100*ej:+.1f}% / never")
        rows.append(cells)
    out.append(table(["market", "T_max", "lender income per position (% of notional)",
                      "alpha=1.5: E_J / break-even rate", "alpha=2: E_J / break-even rate",
                      "alpha=3: E_J / break-even rate"], rows))
    return "\n".join(out)


# ----------------------------------------------------------------------------
# Randomised exit fuzz with attacker bundles and sandwiches (C0-5 d, C0-12)
# ----------------------------------------------------------------------------
def attacker_bundle(m: Market, t: int, target_wad: int) -> int:
    """[buy/sell to target, crank_price, trade back] in one atomic bundle; returns
    the attacker's net SOL cost (>= 0 up to rounding: round-trip venue fees)."""
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
        X2, Y2 = v.eff()
        if v.kind == "curve" and d > v.rt:
            d = v.rt
        cost += v.buy(d) - got
        return cost
    m.observe(t)
    return 0


def fuzz_exits(n_runs: int, seed: int):
    """Random paths x random attacker behaviour; every exit re-checks the
    invariants of ECON §13 (raises InvariantError).  Also compares lender /
    protocol / keeper takes against the same exit without the sandwich."""
    rng = random.Random(seed)
    keys = ["A", "C", "D@0%", "D@50%", "RA", "RC"]
    stats = dict(runs=0, exits=0, sandwich_attempts=0, bundles=0, blocker_runs=0,
                 penalty_truncated_by_sandwich=0, max_loss_over_bref_bps=0, kinds={})
    for i in range(n_runs):
        key = rng.choice(keys)
        m = new_market(key)
        Q = max(1, int(m.max_open_q() * rng.choice([1.0, 0.5, 0.1])))
        pos = m.open(Q, 0)
        P1 = pos.spot_post
        t = 0
        mode = rng.choice(["gap", "ramp", "flat"])
        k = rng.choice([1.3, 1.6, 2.0, 3.0, 5.0])
        te = rng.randrange(60, 600)
        dur = rng.choice([30, 120, 600])
        horizon = te + 900 if mode != "flat" else pos.deadline + 120
        if mode == "flat":
            t = pos.deadline - 30
            m.step_to(t)
        done = False
        blocker = rng.random() < 0.2              # V1-2 / V2-14: a contrary (low) observation every second
        stats["blocker_runs"] += blocker
        while t < horizon and not done:
            t += 1
            if mode == "gap" and t == te:
                m.exo_move(int(P1 * k))
            elif mode == "ramp" and te <= t <= te + dur:
                m.exo_move(int(P1 * math.exp(math.log(k) * (t - te) / dur)))
            if blocker and m.venue.has_route() and rng.random() < 0.9:
                attacker_bundle(m, t, m.venue.spot_wad() * rng.choice([85, 95, 99]) // 100)
                stats["bundles"] += 1
            if rng.random() < 0.3:               # attacker bundle this second
                tgt = int(m.venue.spot_wad() * rng.choice([0.5, 0.8, 1.2, 1.5, 2.0])) if m.venue.has_price() else 0
                if tgt:
                    attacker_bundle(m, t, tgt)
                    stats["bundles"] += 1
            if rng.random() < 0.8:               # honest crank this second
                m.step_to(t)
            else:
                m.advance(t)
            st = m.status(pos, t)
            if not (st["liq"] or st["expired"]):
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
                    c = v.buy(dX)
                    r = m.try_exit(pos, t)
                    back = m.venue.sell(dX) if m.venue.has_route() else 0
                    stats["sandwich_attempts"] += 1
                else:
                    r = m.try_exit(pos, t)
            else:
                r = m.try_exit(pos, t)
            if r:
                stats["exits"] += 1
                stats["kinds"][r["kind"]] = stats["kinds"].get(r["kind"], 0) + 1
                if r_h and r["kind"] == r_h["kind"] and r["kind"] in ("liquidation", "expiry"):
                    senior = lambda x: x["rent"] + x["pen"] + x["fee"] + (x["keeper"] if x["kind"] == "expiry" else 0)
                    lost = senior(r_h) - senior(r)
                    if r["rent"] < r_h["rent"]:
                        raise InvariantError(("sandwich reduced rent", r_h, r))
                    # I-E4: a sandwich can reduce the (truncated) penalty / fee by at most
                    # bound - honest buyback cost; never rent
                    if lost > (r["bound"] - r_h["B"]) + 2:
                        raise InvariantError(("sandwich took more than the bound slack", lost, r))
                    if lost > 1:
                        stats["penalty_truncated_by_sandwich"] += 1
                        stats["max_loss_over_bref_bps"] = max(stats["max_loss_over_bref_bps"],
                                                              lost * BPS // r["b_ref"])
                done = True
        stats["runs"] += 1
    return stats


def selftest():
    """Quick checks used by CI of the design docs."""
    out = []
    # EMA single-update bounds and bucket lag
    m = new_market("A")
    t = 0
    for t in range(1, 30):
        m.step_to(t)
    e0 = m.ema
    attacker_bundle(m, 30, e0 * 3)      # atomic pump to 3x, alone in its second
    m.step_to(31)
    up = m.ema / e0 - 1
    m2 = new_market("A")
    for t in range(1, 30):
        m2.step_to(t)
    e0b = m2.ema
    m2.step_to(30)                        # honest observation in the same second
    attacker_bundle(m2, 30, e0b * 3)
    m2.step_to(31)
    up2 = m2.ema / e0b - 1
    tau = m.tier.ema_tau_s
    b1 = (tau + 2) / (tau + 1) - 1
    b2 = (tau + 4) / (tau + 2) - 1
    out.append(f"single integration after an atomic 3x bundle alone in its second: EMA {100*up:+.2f}% "
               f"(bound {100*b1:+.1f}% at dt=1, {100*b2:+.1f}% at dt=2, tau={tau}s); with an honest observation "
               f"in the same second: {100*up2:+.2f}%")
    # C0-16: a lender-keeper cranks only while the route is down; the route is back
    # (unobserved) for 2 minutes in between.  S3 needs a continuous no-route chain.
    for gap_rule in (10**9, DEFAULT.s3_obs_gap_s):
        P = replace(DEFAULT, s3_obs_gap_s=gap_rule)
        m = new_market("A", P)
        pos = m.open(m.max_open_q(), 0)
        for t in range(1, 61):
            m.step_to(t)
        m.exo_move(int(pos.spot_post * 1.8))     # liquidatable on the EMA leg
        for t in range(61, 181):
            m.step_to(t)
        first = None
        for t in range(181, 181 + 1500, 10):
            down = not (481 <= t < 601)            # route back for 2 minutes, nobody cranks
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
        sections.append(f"### Exit fuzz (attacker bundles + sandwiches, invariants asserted)\n\n{s}")
    print("\n\n".join(sections))


if __name__ == "__main__":
    main()
