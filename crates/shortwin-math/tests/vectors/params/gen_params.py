#!/usr/bin/env python3
"""T-CONFIG host vectors: shortwin_math::params::validate against shortwin_sim.validate_params v0.4.1
(ECONOMICS §15; ARCHITECTURE §2.3.3, §8.1 T-CONFIG, §9 M0b `params::{ParamSet, validate}`).

Every case is a parameter configuration and the exact set of violations the sim's
`validate_params(P, tiers)` reports for it (default switches; the analysis switches are not
parameters). Since the M1 doc sync the sim checks every ECON §15 bound of the parameters it
models (including the penalty split's parts and the per-tier bounds), so the crate's
`check_sim` must report the same set, `validate` must accept exactly the configurations with
an empty set, and `validate_econ` (the ECON §15 bounds of the parameters the sim does not
model, which no case moves) must accept every case. `validate_econ`'s rules are unit-tested
in `src/params.rs`.

Configurations:
  * the defaults (no violation), and `defaults`: every sim field's default value under the
    crate's name, so the test also pins `ParamSet::DEFAULT` to `shortwin_sim.DEFAULT`/`TIERS`;
  * every compiled bound of `COMPILED_BOUNDS` and `TIER_BOUNDS` one at a time at lo - 1, lo,
    hi, hi + 1 (where the crate's field type holds the value; the per-tier bounds for each
    tier), the per-tier CR checks and the SPEC CR minima at and around their edges, penalty
    splits;
  * every corner of `config_check.BOX` (the core box, incl. the relationally rejected ones),
    the `config_check --scan` values, `--targeted` rows and the stress corner, built with
    `config_check.make` exactly as the gate builds them (rows that set `tau` are skipped:
    tau is the compile-time constant EMA_TAU_S in the crate);
  * 3,000 seeded random configurations mixing in-bound, boundary and out-of-bound values
    (global, market-level and per-tier).

Encoding: each case is `[overrides, violations]`, two strings.
  overrides   space-separated `name=value` (global and market-level EMA parameters) and
              `T.name=value` (tier T in A..D), crate names (`ParamSet::set`), applied to
              `ParamSet::DEFAULT`;
  violations  space-separated, sorted `rule` (global) and `T:rule` (per tier), crate rule
              names (`params::Rule::name`); the sim's per-tier EMA bounds are market-level in
              the crate (all tiers carry the same value) and appear once, without a tier; the
              other per-tier bounds carry their tier.

Writes params.json next to itself.
"""
import itertools
import json
import random
import re
import sys
from dataclasses import fields, replace
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "docs" / "design" / "sim"))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)
import config_check as cc  # noqa: E402  (v0.4.1)

SEED = 20260927
N_RANDOM = 3000
TIER_NAMES = ("A", "B", "C", "D")

# sim Params field -> crate name (ParamSet::set / Rule::name); identity otherwise.
GLOBAL_NAME = {
    "settle_fixed": "settle_keeper_fixed", "settle_bps": "settle_keeper_bps", "settle_cap": "settle_keeper_cap",
    "pen_lender_bps": "penalty_lender_bps", "pen_protocol_bps": "penalty_protocol_bps",
    "pen_keeper_bps": "penalty_keeper_bps",
}
# sim Tier field -> crate TierParams name; the EMA fields are market-level in the crate.
TIER_NAME = {"max_dur_s": "max_duration_s", "rate_s1_bps": "rate_slope1_bps", "rate_s2_bps": "rate_slope2_bps"}
TIER_FIELDS = ("m_bps", "cr_open_bps", "cr_liq_bps", "penalty_bps", "max_dur_s", "impact_pos_bps", "impact_mkt_bps",
               "max_liq_slip_bps", "rate_base_bps", "rate_s1_bps", "rate_s2_bps", "min_rent_s")
MARKET_FIELDS = ("ema_dtmax_s", "ema_clamp_bps")
# The sim's per-tier compiled bounds that are per tier in the crate too (TierParams).
TIER_BOUND_FIELDS = tuple(k for k in sim.TIER_BOUNDS if k not in MARKET_FIELDS and k != "ema_tau_s")
PEN = ("pen_lender_bps", "pen_protocol_bps", "pen_keeper_bps")
# The sim's protocol parameters the crate carries (Params fields that are neither analysis
# switches nor sim-only modelling knobs).
SIM_ONLY = {"migration_delay_s", "fv_cap", "ring_len", "exit_ref", "liq_keeper_parity", "s3_obs_gap_s", "two_leg",
            "synthetic", "box_obs", "pprime_raw", "ref_cap_lo", "liq_reserve", "exp_reserve", "margin_policy",
            "check_invariants"}
GLOBAL_FIELDS = [f.name for f in fields(sim.Params) if f.name not in SIM_ONLY]

# Bit width of the crate's field (default u32).
WIDTH = {"ref_max_age_s": 8, "liq_persist_n": 8, "kappa_growth_n": 8, "settle_persist_n": 16, "settle_arm_gap_s": 16,
         "kappa_liq_bps": 16, "kappa_base_bps": 16, "delta_liq_bps": 16, "delta_exp_bps": 16, "exec_tol_bps": 16,
         "ema_dtmax_s": 16, "settle_fixed": 64, "settle_cap": 64, "expiry_keeper_fixed": 64, "expiry_keeper_cap": 64,
         "exec_rent_tol": 64}

REL = {
    "util_kink_bps <= util_cap_bps": "util_kink_le_cap",
    "penalty split sums to 10,000": "penalty_split_sum",
    "kappa_base_bps <= kappa_liq_bps": "kappa_base_le_liq",
    "delta_liq_bps >= delta_exp_bps": "delta_liq_ge_exp",
    "fee_size_cap_x100 <= fee_oi_cap_x100": "fee_size_le_oi",
    "pool_fast_route_s + pool_fast_window_s <= 540": "pool_fast_within_s3",
    "venue_fee_cap_bps < venue_fee_hard_cap_bps": "venue_fee_cap_lt_hard",
    "settle_fixed <= settle_cap": "settle_keeper_fixed_le_cap",
    "expiry_keeper_fixed <= expiry_keeper_cap": "expiry_keeper_fixed_le_cap",
    "ring_len >= max(liq_persist_n, kappa_growth_n + 1)": "ring_len_covers_persistence",
}

assert sim.TIER_BOUNDS["ema_tau_s"] == (5, 5) and sim.DEFAULT.ring_len == 8 and sim.DEFAULT.s3_obs_gap_s == 60
assert not sim.validate_params(sim.DEFAULT, sim.TIERS)
assert all(k in GLOBAL_FIELDS for k in sim.COMPILED_BOUNDS)
assert all(k in TIER_FIELDS for k in TIER_BOUND_FIELDS)


def fits(name, v):
    return 0 <= v < 2 ** WIDTH.get(name, 32)


def parse(msg):
    """One sim violation message -> (crate rule name, tier or None)."""
    m = re.fullmatch(r"(\w+)=(-?\d+) outside \[(-?\d+), (-?\d+)\]", msg)
    if m:
        return GLOBAL_NAME.get(m[1], m[1]), None
    m = re.fullmatch(r"tier ([A-D]) (\w+)=(-?\d+) outside \[(-?\d+), (-?\d+)\]", msg)
    if m:
        if m[2] in MARKET_FIELDS:
            return m[2], None
        assert m[2] in TIER_BOUND_FIELDS, msg
        return TIER_NAME.get(m[2], m[2]), m[1]
    m = re.fullmatch(r"tier ([A-D]): cr_open >= cr_liq \+ 2,000 and cr_liq >= 10,500 required", msg)
    if m:
        return "tier_cr_gap", m[1]
    m = re.fullmatch(r"tier ([A-D]): cr_open >= [\d,]+ and cr_liq >= [\d,]+ \(SPEC minima\)", msg)
    if m:
        return "tier_spec_cr", m[1]
    return REL[msg], None


def encode(P, tiers):
    """(overrides, violations) of a sim configuration, or None if the crate cannot hold it."""
    ov = []
    for k in GLOBAL_FIELDS:
        v = getattr(P, k)
        if v != getattr(sim.DEFAULT, k):
            if not fits(k, v):
                return None
            ov.append(f"{GLOBAL_NAME.get(k, k)}={v}")
    for k in MARKET_FIELDS:
        vals = {getattr(tiers[n], k) for n in TIER_NAMES}
        if len(vals) != 1:
            return None                       # per-tier EMA values: market-level in the crate
        (v,) = vals
        if v != getattr(sim.TIERS["A"], k):
            if not fits(k, v):
                return None
            ov.append(f"{k}={v}")
    for n in TIER_NAMES:
        if tiers[n].ema_tau_s != sim.TIER_BOUNDS["ema_tau_s"][0]:
            return None                       # tau is the compile-time constant EMA_TAU_S in the crate
        for k in TIER_FIELDS:
            v = getattr(tiers[n], k)
            if v != getattr(sim.TIERS[n], k):
                if not fits(k, v):
                    return None
                ov.append(f"{n}.{TIER_NAME.get(k, k)}={v}")
    viol = sorted({parse(m) for m in sim.validate_params(P, tiers)}, key=lambda rt: (rt[1] or "", rt[0]))
    return [" ".join(ov), " ".join(r if t is None else f"{t}:{r}" for r, t in viol)]


def build(ov=None, tier_ov=None, market=None):
    """A sim configuration from Params overrides, per-tier overrides and market-level EMA values."""
    P = replace(sim.DEFAULT, **(ov or {}))
    tiers = {}
    for n, T in sim.TIERS.items():
        t = dict((tier_ov or {}).get(n, {}))
        t.update(market or {})
        tiers[n] = replace(T, **t)
    return P, tiers


def edges(lo, hi):
    return [lo - 1, lo, hi, hi + 1]


def main():
    cases, seen = [], set()

    def add(P, tiers):
        c = encode(P, tiers)
        if c is not None and c[0] not in seen:
            seen.add(c[0])
            cases.append(c)
        return c is not None

    assert add(*build())
    # compiled bounds one at a time
    for k, (lo, hi) in sim.COMPILED_BOUNDS.items():
        for v in edges(lo, hi):
            if fits(k, v):
                add(*build({k: v}))
    for k in MARKET_FIELDS:
        lo, hi = sim.TIER_BOUNDS[k]
        for v in edges(lo, hi):
            if fits(k, v):
                add(*build(market={k: v}))
    for n in TIER_NAMES:
        for k in TIER_BOUND_FIELDS:
            lo, hi = sim.TIER_BOUNDS[k]
            for v in edges(lo, hi):
                if fits(k, v):
                    add(*build(tier_ov={n: {k: v}}))
    # per-tier CR checks and SPEC minima
    for n in TIER_NAMES:
        T = sim.TIERS[n]
        spec_open, spec_liq = sim.SPEC_CR[n]
        for co in (spec_open - 1, spec_open, T.cr_liq_bps + 1999, T.cr_liq_bps + 2000, 100_000, 100_001):
            add(*build(tier_ov={n: {"cr_open_bps": co}}))
        for cl in (10_499, 10_500, spec_liq - 1, spec_liq, T.cr_open_bps - 2000, T.cr_open_bps - 1999):
            add(*build(tier_ov={n: {"cr_liq_bps": cl}}))
        for co, cl in ((spec_open, spec_open - 2000), (12_499, 10_499), (0, 0), (100_000, 98_000)):
            add(*build(tier_ov={n: {"cr_open_bps": co, "cr_liq_bps": cl}}))
    # penalty splits: the sum and the protocol and keeper parts' bounds (the lender part follows from them)
    for split in ((5000, 2000, 3000), (5000, 2000, 3001), (4000, 1000, 5000), (3000, 1000, 6000), (0, 0, 0),
                  (10_000, 0, 0), (0, 5001, 4999), (0, 5000, 5000), (3198, 166, 6636), (0, 5001, 5001),
                  (3000, 5001, 1999)):
        add(*build(dict(zip(PEN, split))))
    # relational checks at equality and one unit either side (out-of-bound values included)
    for ov in ({"kappa_liq_bps": 2500, "kappa_base_bps": 2500}, {"kappa_liq_bps": 2500, "kappa_base_bps": 2501},
               {"kappa_liq_bps": 999, "kappa_base_bps": 1000}, {"delta_liq_bps": 200, "delta_exp_bps": 200},
               {"delta_liq_bps": 200, "delta_exp_bps": 201}, {"fee_size_cap_x100": 500, "fee_oi_cap_x100": 499},
               {"fee_size_cap_x100": 500, "fee_oi_cap_x100": 500}, {"pool_fast_route_s": 420, "pool_fast_window_s": 120},
               {"pool_fast_route_s": 421, "pool_fast_window_s": 119}, {"pool_fast_route_s": 420, "pool_fast_window_s": 121},
               {"venue_fee_cap_bps": 300, "venue_fee_hard_cap_bps": 300}, {"venue_fee_cap_bps": 299, "venue_fee_hard_cap_bps": 300},
               {"settle_fixed": 5_000_000, "settle_cap": 5_000_000}, {"settle_fixed": 5_000_001, "settle_cap": 5_000_000},
               {"expiry_keeper_fixed": 7, "expiry_keeper_cap": 7}, {"expiry_keeper_fixed": 8, "expiry_keeper_cap": 7},
               {"util_kink_bps": 9000, "util_cap_bps": 9000}, {"util_kink_bps": 9001, "util_cap_bps": 9000},
               {"liq_persist_n": 8, "kappa_growth_n": 7}, {"liq_persist_n": 9, "kappa_growth_n": 3},
               {"liq_persist_n": 2, "kappa_growth_n": 8}):
        add(*build(ov))
    # config_check: every corner of the core box, the scan, the targeted rows, the stress corner
    names = list(cc.BOX)
    for combo in itertools.product(*(cc.BOX[k] for k in names)):
        assert add(*cc.make(dict(zip(names, combo))))
    gate = list(cc.scan_values()) + [(k, v) for k, v in cc.TARGETED if k != "tau"]
    for k, v in gate:
        add(*cc.make({k: v}))
    assert add(*cc.make(dict(cc.STRESS)))
    # seeded random configurations
    rng = random.Random(SEED)
    pool = [k for k in sim.COMPILED_BOUNDS if k not in PEN]      # the split is drawn as a whole below

    def value(lo, hi, p_in):
        r = rng.random()
        if r < p_in:
            return rng.randint(lo, hi)
        if r < (1 + p_in) / 2:
            return rng.choice(edges(lo, hi))
        return rng.randint(0, 2 * hi + 10)

    made = 0
    while made < N_RANDOM:
        p_in = 0.95 if made % 2 else 0.45     # half the configurations mostly in bounds
        ov = {}
        for k in rng.sample(pool, rng.randint(1, 6)):
            v = value(*sim.COMPILED_BOUNDS[k], p_in)
            if fits(k, v):
                ov[k] = v
        if rng.random() < 0.25:
            if rng.random() < p_in:                               # a split within the parts' bounds
                b, c = rng.randint(0, 5000), rng.randint(0, 5000)
                a = 10_000 - b - c if rng.random() < 0.9 else rng.randint(0, 10_000)
            else:
                a, b = rng.randint(0, 10_000), rng.randint(0, 10_000)
                c = 10_000 - a - b if rng.random() < 0.7 else rng.randint(0, 10_000)
            if min(a, b, c) >= 0:
                ov.update(zip(PEN, (a, b, c)))
        market = {}
        if rng.random() < 0.2:
            k = rng.choice(MARKET_FIELDS)
            v = value(*sim.TIER_BOUNDS[k], p_in)
            if fits(k, v):
                market[k] = v
        tier_ov = {}
        if rng.random() < 0.3:
            for n in rng.sample(TIER_NAMES, rng.randint(1, 4)):
                spec_open, spec_liq = sim.SPEC_CR[n]
                t = {}
                if rng.random() < 0.6:
                    t["cr_open_bps"] = rng.choice([spec_open - 1, spec_open, rng.randint(spec_open, 100_000),
                                                   rng.randint(9_000, 110_000)])
                if rng.random() < 0.6:
                    t["cr_liq_bps"] = rng.choice([10_499, 10_500, spec_liq - 1, spec_liq,
                                                  rng.randint(spec_liq, spec_open - 2_000), rng.randint(9_000, 60_000)])
                if rng.random() < 0.5:
                    for k in rng.sample(TIER_BOUND_FIELDS, rng.randint(1, 3)):
                        if k != "cr_open_bps":
                            v = value(*sim.TIER_BOUNDS[k], p_in)
                            if fits(k, v):
                                t[k] = v
                tier_ov[n] = t
        if add(*build(ov, tier_ov, market)):
            made += 1

    defaults = {GLOBAL_NAME.get(k, k): str(getattr(sim.DEFAULT, k)) for k in GLOBAL_FIELDS}
    defaults.update({k: str(getattr(sim.TIERS["A"], k)) for k in MARKET_FIELDS})
    for n in TIER_NAMES:
        defaults.update({f"{n}.{TIER_NAME.get(k, k)}": str(getattr(sim.TIERS[n], k)) for k in TIER_FIELDS})
    out = {
        "generator": "gen_params.py",
        "source": "shortwin_sim.py v0.4.1 validate_params, COMPILED_BOUNDS, TIER_BOUNDS, SPEC_CR; "
                  "config_check.py v0.4.1 BOX, scan_values, TARGETED, STRESS",
        "defaults": defaults,
        "cases": cases,
    }
    path = Path(__file__).with_name("params.json")
    path.write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")
    nviol = sum(1 for c in cases if c[1])
    print(f"{path.name}: {len(cases)} configurations, {nviol} rejected by validate_params")


if __name__ == "__main__":
    main()
