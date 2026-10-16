#!/usr/bin/env python3
"""
short.win -- liveness battery for a proposed parameter configuration (ECONOMICS §5.7, §15; R3B-6,
R4A-1, R4A-4, R4B-1).

CI gate for governance proposals.  Runs every row with the DEFAULT configuration and with the
proposal and FAILS if any row that is a liquidation / expiry / still open under the defaults
becomes an S1 (lender loss), or leaves the lender worse by more than 1 % of the value of Q.

The battery (v0.4.1) has four parts:
  core   the §5.7 lag-study pumps, gaps, fast pumps and the Raydium 50 % LP drains + pumps (65 rows);
  noise  the core rows with deterministic per-second price noise +-0.3 % and +-1 %, 3 seeds each
         (R4B-1: live and reference differ within a second, as on a real pool) (390 rows);
  grid   the reviewer's genuine-pump grid: 8 markets x k in {4,6,8,12,15} x d in {20..100 s} x
         size {1, 0.5} x keeper latency {1, 2} (R4A-1) (960 rows);
  aged   positions aged close to their max duration at 80 / 90 % utilisation (rent due >> K_s),
         then a ramp, with +-0.3 % noise, 3 seeds (240 rows).

    python3 config_check.py                                  # defaults vs defaults (sanity)
    python3 config_check.py kappa_liq_bps=2000 delta_liq_bps=300
    python3 config_check.py tau=6                            # EMA tau as if it were a parameter
    python3 config_check.py --scan                           # one-at-a-time compiled extremes (ECON §15)
    python3 config_check.py --corners                        # every corner of the core box + stress corner
    python3 config_check.py --targeted                       # the §5.7 bound re-derivation table
    python3 config_check.py --summary                        # outcomes of the default configuration per part
    python3 config_check.py --jobs 12 ...                    # parallel (default: all cores)
"""
import itertools
import multiprocessing as mp
import os
import sys
from dataclasses import replace

from shortwin_sim import (DEFAULT, MARKETS, TIERS, TE, Market, Scenario, SC, drain, lag_pumps, noisy_path,
                          ramp_path, run, summarize, tier_of, validate_params, COMPILED_BOUNDS, TIER_BOUNDS)

KEYS = ["A", "B", "C", "D@0%", "D@50%", "RA", "RB", "RC"]
AGE = {"A": 25 * 86400, "B": 12 * 86400, "C": 6 * 86400, "D@0%": 60 * 3600, "D@50%": 60 * 3600,
       "RA": 25 * 86400, "RB": 12 * 86400, "RC": 6 * 86400}
TIER_FIELDS = ("ema_dtmax_s", "ema_clamp_bps", "ema_tau_s", "cr_liq_bps", "cr_open_bps", "penalty_bps",
               "max_liq_slip_bps", "max_dur_s", "min_rent_s", "impact_pos_bps", "impact_mkt_bps", "rate_base_bps",
               "rate_s1_bps", "rate_s2_bps")


def core_rows():
    rows = []
    pumps = lag_pumps() + [SC["gap 2x"], Scenario("pump 3x/60s", ramp_path(TE, 3.0, 60)),
                           Scenario("pump 2x/20s", ramp_path(TE, 2.0, 20))]
    for key in KEYS:
        for sc in pumps:
            rows.append(dict(key=key, sc=sc))
    for key in ("RA", "RB", "RC"):
        for k, dur in ((2.0, 600), (2.0, 120), (3.0, 600)):
            rows.append(dict(key=key, sc=Scenario(f"drain 50%+{k}x/{dur}s", ramp_path(TE + 60, k, dur),
                                                  events=((TE, drain(5000)),))))
    return rows


def battery(parts=("core", "noise", "grid", "aged"), seeds=(1, 2, 3), jitters=(0.003, 0.01)):
    """Row specs: dict(key, sc, [j, seed], [size, lat], [util, skip_to])."""
    rows = []
    core = core_rows()
    if "core" in parts:
        rows += core
    if "noise" in parts:
        for j in jitters:
            for seed in seeds:
                rows += [dict(r, j=j, seed=seed) for r in core]
    if "grid" in parts:
        for lat in (1, 2):
            for size in (1.0, 0.5):
                for key in KEYS:
                    for k in (4.0, 6.0, 8.0, 12.0, 15.0):
                        for d in (20, 40, 50, 70, 75, 100):
                            rows.append(dict(key=key, sc=Scenario(f"pump {k:g}x/{d}s", ramp_path(TE, k, d)),
                                             size=size, lat=lat))
    if "aged" in parts:
        for key, te in AGE.items():
            for util in (8000, 9000):
                for k, d in ((3.0, 20), (3.0, 60), (6.0, 40), (10.0, 60), (10.0, 180)):
                    sc = Scenario(f"aged pump {k:g}x/{d}s", ramp_path(te, k, d), t_event=te, window=1800)
                    for seed in seeds:
                        rows.append(dict(key=key, sc=sc, util=util, skip_to=te - 30, j=0.003, seed=seed))
    return rows


def label(r):
    s = f"{r['key']} {r['sc'].name}"
    if r.get("j"):
        s += f" noise {r['j']*100:.1f}% s{r['seed']}"
    if r.get("size", 1.0) != 1.0 or r.get("lat", 1) != 1:
        s += f" size {r.get('size', 1.0)} lat {r.get('lat', 1)}"
    if r.get("util"):
        s += f" U{r['util']//100}"
    return s


def make(overrides):
    ov = dict(overrides)
    tier_ov = {k: ov.pop(k) for k in list(ov) if k in TIER_FIELDS}
    if "tau" in ov:
        tier_ov["ema_tau_s"] = ov.pop("tau")
    P = replace(DEFAULT, **ov)
    tiers = {}
    for n, T in TIERS.items():
        t_ov = dict(tier_ov)
        if "cr_liq_bps" in t_ov:                        # keep cr_open >= cr_liq + 2,000 per tier
            t_ov["cr_liq_bps"] = min(t_ov["cr_liq_bps"], T.cr_open_bps - 2000)
        if t_ov.get("cr_open_bps") == 0:                # 0 = each tier at its minimum cr_liq + 2,000
            t_ov["cr_open_bps"] = t_ov.get("cr_liq_bps", T.cr_liq_bps) + 2000
        tiers[n] = replace(T, **t_ov)
    return P, tiers


_ROWS = {}


def rows_of(parts):
    if parts not in _ROWS:
        _ROWS[parts] = battery(parts)
    return _ROWS[parts]


SIZING = {"fee_size_cap_x100", "fee_oi_cap_x100", "pool_lock_min_bps", "impact_pos_bps", "impact_mkt_bps",
          "curve_oi_gamma_bps"}


def outcome(args):
    parts, i, overrides, q_abs = args        # rows are rebuilt in each worker (closures do not pickle)
    r = rows_of(parts)[i]
    P, tiers = make(overrides)
    key = r["key"]
    T = tiers[tier_of(key).name]
    m = Market(MARKETS[key](), T, P, util_bps=r.get("util", 8000))
    sc = r["sc"]
    if r.get("j"):
        sc = Scenario(sc.name, noisy_path(sc.path, r["j"], r["seed"]), sc.t_event, sc.window, sc.events)
    size = r.get("size", 1.0)
    if q_abs:                                 # compare at the proposal's position size (sizing parameters)
        size = (q_abs + 0.5) / m.max_open_q()
    pos, res, P1 = run(m, size, sc, keeper_latency=r.get("lat", 1), skip_to=r.get("skip_to"))
    s = summarize(pos, res, P1)
    stuck = s["kind"] == "open" and res["cr_e"] < pos.L     # still open although liquidatable on the EMA
    return s["kind"], s["lender_pnl_pct"], s["mult"], pos.Q, stuck


_POOL = None
_BASE = {}


def pool():
    global _POOL
    if _POOL is None:
        n = int(os.environ.get("CC_JOBS", "0")) or os.cpu_count() or 1
        _POOL = mp.Pool(n) if n > 1 else None
    return _POOL


def evaluate(parts, overrides, q_abs=None):
    args = [(parts, i, overrides, q_abs[i] if q_abs else None) for i in range(len(rows_of(parts)))]
    p = pool()
    return p.map(outcome, args, chunksize=8) if p else [outcome(a) for a in args]


def base(parts):
    if parts not in _BASE:
        _BASE[parts] = evaluate(parts, {})
    return _BASE[parts]


def check(overrides: dict, verbose=True, parts=("core", "noise", "grid", "aged")):
    parts = tuple(parts)
    rows = rows_of(parts)
    new = evaluate(parts, overrides) if overrides else base(parts)
    if SIZING & set(overrides):
        # a sizing parameter changes Q; compare with the DEFAULT configuration at the same Q, so the
        # gate measures the parameter's liveness effect, not the known size effect of K_s (ECON §5.7)
        b = evaluate(parts, {}, q_abs=[y[3] for y in new])
    else:
        b = base(parts)
    fails = []
    for r, x, y in zip(rows, b, new):
        live = (x[0] in ("liquidation", "expiry", "open") and y[0] == "S1") or (x[0] != "open" and y[4])
        econ = y[1] < x[1] - 1.0
        if live or econ:
            fails.append((label(r), x, y, "liveness" if live else "lender -1 %"))
    if verbose:
        for f in fails:
            print(f"{'FAIL' if f[3] == 'liveness' else 'WARN'} {f[0]:48s} default {f[1][0]} @{f[1][2]:.2f}x lender "
                  f"{f[1][1]:+.1f}% -> proposed {f[2][0]} @{f[2][2]:.2f}x lender {f[2][1]:+.1f}%")
    return fails


def summary(parts=("core", "noise", "grid", "aged")):
    parts = tuple(parts)
    rows = rows_of(parts)
    b = base(parts)
    by = {}
    for r, x in zip(rows, b):
        part = "aged" if r.get("util") else "grid" if "size" in r else "noise" if r.get("j") else "core"
        d = by.setdefault(part, dict(n=0, S1=0, liq=0, pnl=0.0))
        d["n"] += 1
        d["S1"] += x[0] == "S1"
        d["liq"] += x[0] == "liquidation"
        d["pnl"] += x[1]
    return by


# --- one-at-a-time compiled extremes (R4A-4): every Params / TierParams field with a liveness effect ---
SCAN_PARAMS = ["settle_fixed", "settle_bps", "settle_cap", "util_kink_bps", "util_cap_bps", "curve_oi_gamma_bps",
               "fee_size_cap_x100", "fee_oi_cap_x100", "pool_lock_min_bps", "venue_fee_hard_cap_bps", "fv_dev_bps",
               "liq_slow_tau_s", "liq_slow_dtmax_s", "ref_max_age_s", "liq_persist_n", "settle_persist_n",
               "settle_arm_gap_s", "kappa_liq_bps", "kappa_base_bps", "kappa_growth_n", "delta_liq_bps",
               "delta_exp_bps", "exec_tol_bps", "exec_rent_tol", "expiry_keeper_fixed", "expiry_keeper_bps",
               "expiry_keeper_cap", "expiry_fee_bps", "rent_protocol_bps", "s3_wait_s", "grad_grace_s",
               "max_migration_pause_s"]
# per-tier values, applied to every tier by make(); the compiled bounds come from shortwin_sim.TIER_BOUNDS (the one
# table validate_params checks). cr_liq_bps has no compiled bound of its own: 10,500 is the relational minimum (below
# every SPEC minimum, so validate_params rejects it) and 100,000 is capped by make() at each tier's cr_open - 2,000;
# cr_open_bps = 0 means each tier at cr_liq + 2,000 (make()).
TIER_SCAN = {k: TIER_BOUNDS[k] for k in ("ema_dtmax_s", "ema_clamp_bps")}
TIER_SCAN["cr_liq_bps"] = (10_500, 100_000)
TIER_SCAN.update({k: TIER_BOUNDS[k] for k in ("penalty_bps", "max_liq_slip_bps", "min_rent_s", "impact_pos_bps",
                                              "impact_mkt_bps", "rate_base_bps", "rate_s1_bps", "rate_s2_bps",
                                              "cr_open_bps")})
SCAN_PARTS = ("core", "noise", "aged")


def scan_values():
    out = []
    for k in SCAN_PARAMS:
        lo, hi = COMPILED_BOUNDS[k]
        for v in sorted({lo, hi}):
            if v != getattr(DEFAULT, k):
                out.append((k, v))
    for k, (lo, hi) in TIER_SCAN.items():
        for v in sorted({lo, hi}):
            if k in ("impact_pos_bps", "impact_mkt_bps") and v == 0:
                continue                                        # 0 = no position can open (not a liveness case)
            out.append((k, v))
    return out


def scan(extra=()):
    rows = battery(SCAN_PARTS)
    print(f"battery for the scan: {len(rows)} runs per value (core, noise +-0.3/1 %, aged)")
    print("| parameter | value | rows worse than the default (liquidation -> S1, or lender -1 % of Q) |")
    print("|---|---|---|")
    for name, v in list(scan_values()) + list(extra):
        ov = {name: v}
        P, tiers = make(ov)
        viol = validate_params(P, tiers)
        if viol:
            print(f"| `{name}` | {v} | (rejected by validate_params: {viol[0]}) |")
            continue
        fails = check(ov, verbose=False, parts=SCAN_PARTS)
        nl = sum(f[3] == "liveness" for f in fails)
        desc = "none" if not fails else f"{nl} liveness / {len(fails) - nl} lender -1 %: " + \
            "; ".join(f"{f[0]} -> {f[2][0]}" for f in fails[:3]) + (f" (+{len(fails) - 3} more)" if len(fails) > 3 else "")
        print(f"| `{name}` | {v} | {desc} |", flush=True)


# --- corners of the core (interacting) box, full battery ---
BOX = {"kappa_liq_bps": (2500, 3000), "liq_persist_n": (2, 3), "kappa_growth_n": (3, 4),
       "ema_dtmax_s": (1, 2), "ema_clamp_bps": (5000, 10000), "ref_max_age_s": (1, 3), "delta_liq_bps": (200, 500)}


def relational_ok(ov):
    P, tiers = make(ov)
    return not validate_params(P, tiers)


def corners(parts=("core", "noise", "grid", "aged"), stress=None):
    names = list(BOX)
    bad = n = 0
    for combo in itertools.product(*(BOX[k] for k in names)):
        ov = dict(zip(names, combo))
        if not relational_ok(ov):
            continue
        n += 1
        f = check(dict(ov), verbose=False, parts=parts)
        if f:
            bad += 1
            print("FAIL", ov, [(x[0], x[2][0]) for x in f][:3], flush=True)
    print(f"core-box corners that pass the relational checks (tau = 5 s): {n}; failing on the full battery "
          f"({len(battery(parts))} runs each): {bad}")
    if stress:
        f = check(dict(stress), verbose=False, parts=parts)
        print(f"stress corner (every scanned parameter at its liveness-adverse compiled extreme together): "
              f"{len(f)} rows worse" + ("" if not f else ": " + "; ".join(f"{x[0]} -> {x[2][0]}" for x in f[:5])))


TARGETED = [("tau", 6), ("tau", 7), ("kappa_liq_bps", 2250), ("kappa_liq_bps", 2000), ("kappa_growth_n", 5),
            ("liq_persist_n", 4), ("liq_persist_n", 5), ("delta_liq_bps", 150), ("delta_liq_bps", 100),
            ("settle_fixed", 20_000_000), ("settle_fixed", 50_000_000), ("settle_bps", 200), ("settle_bps", 500),
            ("settle_cap", 1_000_000_000), ("cr_liq_bps", 12_500), ("cr_open_bps", 0), ("rate_base_bps", 0),
            ("rate_s1_bps", 0), ("rate_s2_bps", 0), ("exec_tol_bps", 0), ("penalty_bps", 2000), ("pool_lock_min_bps", 10000)]


def targeted():
    """ECON §5.7 bound re-derivation (v0.4.1): values just outside (or at) the compiled bounds on the FULL
    battery.  cr_liq_bps = 12,500 applies it to every tier; cr_open_bps = 0 means each tier at cr_liq + 2,000."""
    print("| parameter | value | rows worse on the 1,655-run battery (liveness / lender -1 %) | first liveness rows |")
    print("|---|---|---|---|")
    for k, v in TARGETED:
        f = check({k: v}, verbose=False)
        nl = sum(x[3] == "liveness" for x in f)
        first = "; ".join(f"{x[0]} -> {x[2][0]}" for x in [y for y in f if y[3] == "liveness"][:2])
        print(f"| `{k}` | {v:,} | {nl} / {len(f) - nl} | {first} |", flush=True)


# the liveness-adverse compiled extreme of every scanned parameter (from --scan), checked together
STRESS = dict(kappa_liq_bps=2500, liq_persist_n=3, kappa_growth_n=4, ema_dtmax_s=1, ema_clamp_bps=5000,
              ref_max_age_s=1, delta_liq_bps=200, settle_fixed=10_000_000, settle_bps=100, settle_cap=250_000_000,
              liq_slow_tau_s=86_400, fv_dev_bps=50, exec_tol_bps=0, venue_fee_hard_cap_bps=300,
              max_liq_slip_bps=500, rate_base_bps=50_000, rate_s1_bps=1_000_000)


if __name__ == "__main__":
    argv = [a for a in sys.argv[1:]]
    if "--jobs" in argv:
        i = argv.index("--jobs")
        os.environ["CC_JOBS"] = argv[i + 1]
        del argv[i:i + 2]
    if "--corners" in argv:
        corners(stress=STRESS)
        sys.exit(0)
    if "--scan" in argv:
        scan()
        sys.exit(0)
    if "--targeted" in argv:
        targeted()
        sys.exit(0)
    if "--summary" in argv:
        for part, d in summary().items():
            print(f"{part:6s}: {d['n']} runs, liquidations {d['liq']}, S1 {d['S1']}, mean lender P&L "
                  f"{d['pnl']/d['n']:+.2f} % of Q")
        sys.exit(0)
    ov = {}
    for a in argv:
        k, v = a.split("=")
        ov[k] = int(v)
    P, tiers = make(ov)
    viol = validate_params(P, tiers)
    if viol and "tau" not in ov:
        print("REJECTED by validate_params():", "; ".join(viol))
        sys.exit(2)
    f = check(ov)
    nl = sum(x[3] == "liveness" for x in f)
    if not f:
        print("OK")
        sys.exit(0)
    if nl:
        print(f"{nl} liveness rows (liquidation/expiry/open -> S1, or stuck) and {len(f) - nl} lender -1 % rows: REJECTED")
        sys.exit(1)
    print(f"OK for liveness; {len(f)} rows leave lenders more than 1 % of Q worse: needs the owner's explicit sign-off")
    sys.exit(3)
