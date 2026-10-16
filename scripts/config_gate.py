#!/usr/bin/env python3
"""short.win governance config gate (ARCHITECTURE §2.3.3, §3.1 `propose_config`, A73; T-CONFIG).

Run on a governance proposal before the multisig signs it. The proposal is a list of parameter
overrides against the ECON §15 defaults, named as the program and `shortwin_math::params::ParamSet`
name them (ECON §15 names):

    name=value        a global parameter, or a per-tier parameter set in every tier
    T:name=value      a per-tier parameter in tier T only (A, B, C or D)

The gate runs, in order:

1. `shortwin_sim.validate_params` on the would-be configuration (the compiled bounds and relational
   checks; `params::validate` in the program is the same function, T-CONFIG);
2. `config_check.py`'s liveness battery (1,655 runs: core, noise, genuine-pump grid, aged positions;
   sizing parameters compared at equal position size), the proposal against the defaults.

Parameters the sim does not model (`params::validate_econ`: min_notional, the oracle limits, ...) are
listed and not simulated; the program still refuses them out of bounds at `propose_config`.

Exit status (and the last line printed):

    0  ACCEPTED                     no row worse than the defaults, or only lender-income rows and
                                    --owner-signoff was given
    1  REJECTED (liveness)          a liquidation / expiry / open row became an S1, or got stuck
    2  REJECTED (validate_params)   outside the compiled bounds or a relational check
    3  NEEDS OWNER SIGN-OFF         only rows that leave lenders > 1 % of Q worse (A73: the owner
                                    signs these off explicitly; rerun with --owner-signoff REF)
    64 usage error                  unknown or non-governable name, malformed value

Unlike `config_check.py` (an analysis tool) the gate applies a per-tier override to exactly the tiers
it names and never clamps a value: `cr_liq_bps=…` is checked as proposed in every tier.
"""
import argparse
import multiprocessing as mp
import os
import sys
from dataclasses import replace

HERE = os.path.dirname(os.path.abspath(__file__))
SIM_DIR = os.path.normpath(os.path.join(HERE, "..", "docs", "design", "sim"))
sys.path.insert(0, SIM_DIR)

import config_check as cc  # noqa: E402  (after the path set-up)
from shortwin_sim import DEFAULT, TIERS, validate_params  # noqa: E402

EXIT_OK, EXIT_LIVENESS, EXIT_BOUNDS, EXIT_SIGNOFF, EXIT_USAGE = 0, 1, 2, 3, 64

# ECON §15 / program names -> shortwin_sim names, where they differ.
GLOBAL_ALIASES = {
    "settle_keeper_fixed": "settle_fixed",
    "settle_keeper_bps": "settle_bps",
    "settle_keeper_cap": "settle_cap",
    "penalty_lender_bps": "pen_lender_bps",
    "penalty_protocol_bps": "pen_protocol_bps",
    "penalty_keeper_bps": "pen_keeper_bps",
}
TIER_ALIASES = {
    "max_duration_s": "max_dur_s",
    "rate_slope1_bps": "rate_s1_bps",
    "rate_slope2_bps": "rate_s2_bps",
}
# Global parameters the program has and the sim models (sim names).
SIM_GLOBAL = {
    "open_fee_bps", "rent_protocol_bps", "pen_lender_bps", "pen_protocol_bps", "pen_keeper_bps",
    "expiry_fee_bps", "settle_fixed", "settle_bps", "settle_cap", "expiry_keeper_fixed", "expiry_keeper_bps",
    "expiry_keeper_cap", "util_kink_bps", "util_cap_bps", "wallet_cap_bps", "curve_oi_gamma_bps",
    "fee_size_cap_x100", "fee_oi_cap_x100", "s3_wait_s", "grad_grace_s", "max_migration_pause_s",
    "open_ema_dev_bps", "ref_max_age_s", "liq_persist_n", "settle_persist_n", "settle_arm_gap_s",
    "kappa_liq_bps", "kappa_base_bps", "kappa_growth_n", "delta_liq_bps", "delta_exp_bps", "exec_tol_bps",
    "exec_rent_tol", "fv_dev_bps", "venue_fee_cap_bps", "venue_fee_hard_cap_bps", "pool_lock_min_bps",
    "pool_fast_route_s", "pool_fast_window_s", "liq_slow_tau_s", "liq_slow_dtmax_s",
}
# Market-level EMA fields: one value for every market in the program (ARCH §2.3.3), a tier field in the
# sim; never per tier.
SIM_EMA = {"ema_dtmax_s", "ema_clamp_bps"}
# Per-tier parameters (sim names).
SIM_TIER = {"m_bps", "cr_open_bps", "cr_liq_bps", "penalty_bps", "max_dur_s", "impact_pos_bps",
            "impact_mkt_bps", "max_liq_slip_bps", "rate_base_bps", "rate_s1_bps", "rate_s2_bps", "min_rent_s"}
# Governable parameters the sim does not model (`params::validate_econ`, `Rule::in_sim() == false`).
ECON_ONLY = {
    "min_notional", "market_max_notional", "market_creation_fee", "boost_dust_lamports", "boost_window_s",
    "market_warmup_s", "ema_force_delay_s", "lump_vest_s", "lock_grace_s", "theta_a_usd_e6", "theta_b_usd_e6",
    "oracle_max_age_s", "oracle_stale_s", "oracle_max_conf_bps", "oracle_max_step_bps", "pool_min_real_sol",
    "pool_switch_margin_bps", "pool_list_window_s", "pool_list_margin_bps", "pool_fast_min_bps",
}
TIER_NAMES = tuple(TIERS)  # ("A", "B", "C", "D")


class UsageError(Exception):
    pass


def parse(items):
    """`name=value` / `T:name=value` items -> (sim overrides, econ-only overrides). Every per-tier key is
    explicit (`T:name`), so config_check.make's analysis conveniences never apply."""
    sim, econ = {}, {}
    for raw in items:
        item = raw.strip()
        if not item or item.startswith("#"):
            continue
        if "=" not in item:
            raise UsageError(f"`{item}`: expected name=value or T:name=value")
        key, value = (s.strip() for s in item.split("=", 1))
        try:
            v = int(value.replace("_", ""), 10)
        except ValueError:
            raise UsageError(f"`{item}`: the value must be a non-negative integer") from None
        if v < 0:
            raise UsageError(f"`{item}`: the value must be a non-negative integer")
        tier = None
        if ":" in key:
            tier, key = (s.strip() for s in key.split(":", 1))
            if tier not in TIER_NAMES:
                raise UsageError(f"`{item}`: unknown tier `{tier}` (A, B, C or D)")
        name = TIER_ALIASES.get(key, GLOBAL_ALIASES.get(key, key))
        if name in SIM_TIER:
            for t in ((tier,) if tier else TIER_NAMES):
                put(sim, f"{t}:{name}", v, item)
        elif tier is not None:
            raise UsageError(f"`{item}`: `{key}` is not a per-tier parameter")
        elif name in SIM_EMA:
            for t in TIER_NAMES:
                put(sim, f"{t}:{name}", v, item)
        elif name in SIM_GLOBAL:
            put(sim, name, v, item)
        elif key in ECON_ONLY:
            put(econ, key, v, item)
        elif key in ("tau", "ema_tau_s", "ring_len"):
            raise UsageError(f"`{item}`: `{key}` is a compile-time constant (EMA_TAU_S, RING_LEN), not governable")
        else:
            raise UsageError(f"`{item}`: unknown parameter `{key}` (ECON §15 names)")
    return sim, econ


def put(d, key, v, item):
    if key in d and d[key] != v:
        raise UsageError(f"`{item}`: `{key}` is set twice with different values")
    d[key] = v


_make_all_tiers = cc.make


def make(overrides):
    """config_check.make plus explicit per-tier keys `T:name` (applied verbatim, never clamped)."""
    ov = dict(overrides)
    per_tier = {k: ov.pop(k) for k in list(ov) if ":" in k}
    P, tiers = _make_all_tiers(ov)
    for key, v in per_tier.items():
        t, name = key.split(":", 1)
        tiers[t] = replace(tiers[t], **{name: v})
    return P, tiers


def install():
    """Routes config_check's battery through `make` (worker processes are forked, so they see it) and
    makes its equal-size comparison recognise per-tier sizing keys."""
    cc.make = make
    cc.SIZING = set(cc.SIZING) | {f"{t}:{n}" for t in TIER_NAMES for n in cc.SIZING}
    try:
        mp.set_start_method("fork", force=True)
    except ValueError:                       # no fork on this platform: run serially
        os.environ["CC_JOBS"] = "1"


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="A73 governance gate: validate_params + config_check.py's liveness battery on a proposal.",
        epilog="Exit: 0 ACCEPTED, 1 REJECTED (liveness), 2 REJECTED (validate_params), "
               "3 NEEDS OWNER SIGN-OFF, 64 usage error.")
    ap.add_argument("overrides", nargs="*", help="name=value or T:name=value (ECON §15 names)")
    ap.add_argument("--file", "-f", action="append", default=[],
                    help="read overrides from a file (one per line, # comments)")
    ap.add_argument("--owner-signoff", metavar="REF",
                    help="the owner's explicit sign-off (e.g. a SPEC §14 entry) for lender-income-only rows")
    ap.add_argument("--jobs", type=int, default=0, help="worker processes (default: all cores)")
    args = ap.parse_args(argv)
    if args.jobs:
        os.environ["CC_JOBS"] = str(args.jobs)

    items = list(args.overrides)
    try:
        for path in args.file:
            with open(path, encoding="utf-8") as f:
                items += f.read().splitlines()
        sim, econ = parse(items)
    except (UsageError, OSError) as e:
        print(f"config-gate: {e}", file=sys.stderr)
        return EXIT_USAGE
    if args.owner_signoff is not None and not args.owner_signoff.strip():
        print("config-gate: --owner-signoff needs a non-empty reference", file=sys.stderr)
        return EXIT_USAGE

    install()
    shown = ", ".join(f"{k}={v}" for k, v in sim.items()) or "(none: the defaults)"
    print(f"config-gate: simulated overrides: {shown}")
    if econ:
        print("config-gate: not simulated (bounds checked on-chain by params::validate_econ at propose_config): "
              + ", ".join(f"{k}={v}" for k, v in econ.items()))

    P, tiers = make(sim)
    viol = validate_params(P, tiers)
    if viol:
        print("REJECTED by validate_params: " + "; ".join(viol))
        return EXIT_BOUNDS

    fails = cc.check(sim)
    nl = sum(f[3] == "liveness" for f in fails)
    battery = len(cc.rows_of(("core", "noise", "grid", "aged")))
    if nl:
        print(f"REJECTED (liveness): {nl} of {battery} rows lose a liquidation / expiry / open "
              f"(S1 or stuck), {len(fails) - nl} lender -1 % rows")
        return EXIT_LIVENESS
    if fails:
        if args.owner_signoff:
            print(f"ACCEPTED with the owner's sign-off `{args.owner_signoff.strip()}`: {len(fails)} of {battery} "
                  f"rows leave lenders more than 1 % of Q worse (no liveness row)")
            return EXIT_OK
        print(f"NEEDS OWNER SIGN-OFF: {len(fails)} of {battery} rows leave lenders more than 1 % of Q worse "
              f"(no liveness row); rerun with --owner-signoff REF once the owner has signed off")
        return EXIT_SIGNOFF
    print(f"ACCEPTED: no row of {battery} is worse than under the defaults")
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
