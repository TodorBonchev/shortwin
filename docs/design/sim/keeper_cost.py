#!/usr/bin/env python3
"""
short.win -- keeper cost model (ECONOMICS §5.8, R3B-5; O17).  Deterministic arithmetic on MEASURED
inputs (M7: ARCH §9 "keeper cost measurement ... feeding back into ECON §5.8"; U27 closed).

Inputs:
  * base fee 5,000 lamports per signature: mainnet getFeeForMessage, slot 450,897,283.
  * priority price scenarios (micro-lamports per CU): 0 / 1,000 / 10,000 / 100,000 (the O17 ceiling).
    Mainnet getRecentPrioritizationFees on pAMM, a PumpSwap pool and AMM v4 (slots 450,897,105-255):
    the per-slot MINIMUM landed fee was 0 in >= 75 % of slots, p90 0-1,943, max 1.31 M.
  * priority is paid on the REQUESTED limit, which the keeper sets to ceil(simulated x 1.1); a
    transaction costs 5,000 x signatures + ceil(requested CU x price / 10^6) (the keeper's fee model).
  * CU per instruction: the real keeper's own transactions, measured per venue by
    services/keeper-litesvm `m7_cost::m7_keeper_cost_inputs` (tests/bench/m7_keeper/results.json,
    `cu.*.requested_cu`, the costliest measured), in place of the v0.4 estimates 35 k / 90 k / 220 k.
    The attacker's LP-wrapped crank stays an estimate (~150 k CU, the M4G-2 reproducer's).
  * batching: k crank_price instructions per transaction (one signature).  Measured (T-SIZE, the
    keeper's builder): v1 (4,096 B, 64 inline addresses) holds 8 PumpSwap / 9 AMM v4 / 9 CPMM / 11
    curve cranks, and a batch of 6 fits every venue and a mix (the keeper's default `batch_v1` = 6);
    v0 through the protocol ALT holds 3 PumpSwap / 4 AMM v4 / 4 CPMM / 5 curve cranks (the fallback).
  * cadence: the keeper's own, measured (quiet: one crank per 30 s = 2,880 a day; near a threshold:
    one a second; an armed position 43 sol_settle calls; a foreign price deviation 116 cranks at the
    harness's 2 ticks a second, ~150 at 2.5 slots a second; an isolated LP-wrapped trigger 8 cranks
    above the control; a PumpSwap warm-up 8 cranks).
  * revenue: SPEC §6.1 rent at 80 % utilisation on the beta cap of 25 SOL open notional per market
    (ADOPTED Q21); protocol share 15 %.
  * O17 budget as built at M7 (superseded by the owner's decision A of 2026-10-01; kept for the
    comparison): `per_market_daily_lamports` 30,000,000 (0.03 SOL), charged with the market's share
    of its crank transactions (a batch's fee split equally among its markets).
  * The global budget (owner decision A, 2026-10-01; ARCH §7.1 duty 8, ECON §5.8; PROPOSED defaults):
    `global_daily_lamports` G = 1 SOL a UTC day; covered (tier-1) markets are those with open positions
    whose lender value at risk V = floor(borrowed_q x min(P_ema, P_ACC_MAX) / WAD) is at least
    `var_floor_lamports` (1 SOL), ranked by V; each gets an entitlement of (1 + `reserve_margin_bps`)
    x its quiet day at its own per-crank charge (2 x), funded in rank order; the rest of G is the free
    pool.  A charge draws on the market's own entitlement, then the free pool, then (exit-critical and
    baseline cranks only, never bursts) the entitlements of lower-ranked markets, lowest first;
    exit-critical cranks (and warm-ups of markets without positions) beyond that go to the
    overdraft (never throttled).  Below the floor a market
    gets the 30-s idle cadence only, from the free pool and within `tail_share_bps` (20 %) of G.
  * Attackers (assumptions stated with each table): a price round trip moves 0.99 % of the pool's
    reserve each way (a reverted >= 2 % excursion, 2 phi delta of fees + one base fee); a junk market
    costs >= 0.035 SOL to list (ARCH §3.2: account rent at (len + 128) x 5,080 lamports plus the
    0.005 fee, 0.035-0.038 SOL; the review's 0.047 took the older rent rate); a junk position is
    Tier D (cr_open 4.0, L 1.75, 72 h max duration, rate 100 % APR at U = 0 and 287.5 % at U = 50 %,
    the protocol's 15 % of it lost, the lender's 85 % returned to a self-lending attacker), a venue
    fee of 1 % one way, renewed every 72 h.

    python3 keeper_cost.py
"""
import json
import math
import os

DAY = 86_400
BASE = 5_000
LAMPORTS = 10**9
PRICES = (0, 1_000, 10_000, 100_000)
VENUES = ("pumpswap", "pump_curve", "raydium_cpmm", "raydium_amm_v4")
NAMES = {"pumpswap": "PumpSwap", "pump_curve": "curve", "raydium_cpmm": "CPMM", "raydium_amm_v4": "AMM v4"}
# Requested CU (ceil(consumed x 1.1)), the costliest measured per kind (tests/bench/m7_keeper/results.json).
# Re-baselined on short.win 1.0.1 (`72c82879…`, pump's v3 curve adapter; pump-final P8): the curve's
# exits trade one `buy_v3` with 10 TradeExtras (147,131 / 144,314, were 191,837 / 189,015).
CU = {
    "pumpswap": {"crank": 41_149, "sol_settle_keep": 66_136, "sol_settle_settle": 77_008,
                 "liquidate": 214_447, "expire": 211_324},
    "pump_curve": {"crank": 38_137, "sol_settle_keep": 63_941, "sol_settle_settle": 74_717,
                   "liquidate": 147_131, "expire": 144_314},
    "raydium_cpmm": {"crank": 32_560, "sol_settle_keep": 65_940, "sol_settle_settle": 78_494,
                     "liquidate": 124_787, "expire": 117_594},
    "raydium_amm_v4": {"crank": 32_497, "sol_settle_keep": 57_932, "sol_settle_settle": 68_833,
                       "liquidate": 104_764, "expire": 101_111},
}
LP_WRAP_CU = 150_000             # the attacker's [deposit, crank_price, withdraw] (estimate, M4G-2)
V1_K = 6                         # the keeper's `batch_v1`
V1_MAX = {"pumpswap": 8, "pump_curve": 11, "raydium_cpmm": 9, "raydium_amm_v4": 9}
V0_K = {"pumpswap": 3, "pump_curve": 5, "raydium_cpmm": 4, "raydium_amm_v4": 4}
QUIET_PER_DAY = DAY // 30        # 2,880
ARMED_SETTLES = 43
BURST_CRANKS = 116               # 150 at 2.5 slots a second
BURST_EC = 60                    # M7D-4: on a market near a threshold one crank a second is exit-critical
BURST_EXTRA = BURST_CRANKS - BURST_EC   # the rest of a burst: discretionary (56)
ROUND_TRIPS_PER_DAY = DAY // 60  # one round trip a minute keeps a market bursting all day (burst_s 60 s)
SELF_LP_LOST_BPS = 5             # PumpSwap's protocol fee: what a round trip on a self-LP pool loses
LIQ_TRIGGER_CRANKS = 8
LIQ_COVER_S = 10
BUDGET = 30_000_000              # per_market_daily_lamports as built at M7 (retired by decision A)
# The global budget (owner decision A of 2026-10-01; `[budget]`, PROPOSED defaults; ARCH §7.1 duty 8).
GLOBAL = 1_000_000_000           # global_daily_lamports: G, lamports per UTC day
VAR_FLOOR = 1_000_000_000        # var_floor_lamports: below it a market gets the idle cadence only
RESERVE_MARGIN_BPS = 10_000      # reserve_margin_bps: entitlement = quiet day x (1 + margin) = 2 x
TAIL_SHARE_BPS = 2_000           # tail_share_bps: below-floor idle cadence at most 20 % of G a day
TIER1 = 20                       # the burst tables' covered markets: the target and 19 quiet ones
JUNK_LISTING = 35_000_000        # ARCH §3.2: rent at (len + 128) x 5,080 + the 0.005 fee = 0.035-0.038 (the lower end)
JUNK_N = 100_000_000             # a min-notional junk position (ADOPTED Q11)
JUNK_PHI_BPS = 100               # its venue fee, one way
TIER_D = {"cr_open": 40_000, "cr_liq": 17_500, "dur_days": 3, "rate_u0_bps": 10_000, "rate_u50_bps": 28_750}
OPEN_FEE_BPS = 25
PROTOCOL_RENT_BPS = 1_500
SET_COVERAGE_PAUSE_CU = 8_921
CRANK_SOL_USD_CU = 7_212
CRANK_SOL_USD_PER_DAY = 1_560    # a feed publishing every 55 s
RATE80 = {"A": 5_000, "B": 10_000, "C": 20_000, "D": 40_000}   # APR bps at 80 % utilisation


def tx_cost(cu_total, price, signatures=1):
    """What the keeper pays for one transaction: base fees plus priority on the requested limit."""
    return signatures * BASE + math.ceil(cu_total * price / 1_000_000)


def per_market_crank(venue, price, k=1):
    """One market's share of a batch of k cranks of `venue` (lamports)."""
    return tx_cost(k * CU[venue]["crank"], price) / k


def targeted_day(venue, price, k, f):
    """SOL per market-day: per-second cranks for a share f of the day, one per 30 s otherwise."""
    c = per_market_crank(venue, price, k)
    return DAY * (f * c + (1 - f) * c / 30) / LAMPORTS


def quiet_day(venue, price, k):
    """Lamports of one market's quiet cadence for a day (one crank per 30 s)."""
    return QUIET_PER_DAY * per_market_crank(venue, price, k)


def entitlement(venue, price, k):
    """A covered market's daily entitlement: its quiet day x (1 + reserve_margin_bps)."""
    return quiet_day(venue, price, k) * (10_000 + RESERVE_MARGIN_BPS) / 10_000


def round_trip(depth_sol, phi_bps):
    """The attacker's price round trip on a pool of `depth_sol` SOL: 2 phi delta (delta 0.99 %) + a base fee."""
    return 2 * phi_bps / 10_000 * 0.0099 * depth_sol * LAMPORTS + BASE


def burst_cost(price):
    """The keeper's cost of one price burst: 116 unbatched PumpSwap cranks (one market due per slot)."""
    return BURST_CRANKS * tx_cost(CU["pumpswap"]["crank"], price)


def global_bursts(price, per_burst, tier1=TIER1, k=V1_K):
    """Under G: (bursts that degrade the first market, the keeper's spend on them, bursts that degrade
    each further market once the free pool is spent).  The storm starts with the UTC day, every covered
    market quiet (k-batched); a burst draws on the target's entitlement, then the free pool."""
    ent = entitlement("pumpswap", price, k)
    free = max(GLOBAL - tier1 * ent, 0)
    first = math.ceil((ent + free) / per_burst)
    return first, ent + free, math.ceil(ent / per_burst)


def junk_upkeep_day():
    """Lamports a day to keep one min-notional Tier D position open on a self-lent junk market (renewed
    every 72 h: the open fee and the venue fee on the sale and the buyback; the protocol's 15 % of the
    rent at U = 0)."""
    d = TIER_D
    fees = (OPEN_FEE_BPS + 2 * JUNK_PHI_BPS) / 10_000 * JUNK_N / d["dur_days"]
    rent = PROTOCOL_RENT_BPS / 10_000 * d["rate_u0_bps"] / 10_000 * JUNK_N / 365
    return fees + rent


def hover_junk(v_r):
    """One junk market that outranks a real market of value at risk v_r (lamports) and holds a Tier D
    position near its threshold (live CR < 1.3 L) all day: (notional at open, capital, attacker's
    lamports a day).  The price is h = cr_open / (1.3 L) above the open, so N = V / h; capital = the
    margin (cr_open - 1) N plus the lender deposit 2 N (U = 50 %); a day = the open fee and 2 phi on N
    every 72 h plus the protocol's 15 % of 287.5 % APR on V."""
    d = TIER_D
    h = d["cr_open"] / (1.3 * d["cr_liq"])
    n = v_r / h
    capital = (d["cr_open"] / 10_000 - 1) * n + 2 * n
    per_day = (OPEN_FEE_BPS + 2 * JUNK_PHI_BPS) / 10_000 * n / d["dur_days"] \
        + PROTOCOL_RENT_BPS / 10_000 * d["rate_u50_bps"] / 10_000 * v_r / 365
    return n, capital, per_day


def junk_round_trip(v, lost_bps):
    """Lamports of the attacker's round trip on a junk market's own pool holding value at risk v: the caps
    need a conservative depth >= V / (7.5 phi), so 2 phi delta x depth = 2 delta V / 7.5 whatever phi;
    lost_bps / phi of it is lost (the full fee, or on a self-LP pool only the venue protocol's share),
    plus the base fee."""
    return 2 * 0.0099 * v / 7.5 * lost_bps / JUNK_PHI_BPS + BASE


def hover_burst(price):
    """Junk above the floor hovering near a threshold AND bursting (one round trip a minute on each junk
    market's own pool) against TIER1 real covered markets ranked below it, k = 6: (junk markets, E, C, D,
    the keeper's day).  Since M7D-4 the exit-critical class is one crank a second (C); a burst's other
    cranks are discretionary, D a day, drawn on the junk market's own entitlement and the free pool only
    (and, since M7D-3, never on what its join took from lower-ranked markets).  So the bursts can drain the
    free pool F0 and the junk's own entitlements (n (D - E) >= F0), and only the exit-critical hovering
    takes the real markets' entitlements (n C >= TIER1 E)."""
    e = entitlement("pumpswap", price, V1_K)
    c = per_market_crank("pumpswap", price, V1_K) * DAY
    d = ROUND_TRIPS_PER_DAY * BURST_EXTRA * tx_cost(CU["pumpswap"]["crank"], price)
    f0 = max(GLOBAL - TIER1 * e, 0)
    n = max(math.ceil(TIER1 * e / c), math.ceil(f0 / (d - e)))
    keeper = n * c + min(n * d, f0 + n * e)
    return n, e, c, d, keeper


def table(headers, rows):
    out = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def cross_check():
    """The constants above equal tests/bench/m7_keeper/results.json (when this runs in the repository)."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "tests", "bench",
                        "m7_keeper", "results.json")
    if not os.path.exists(path):
        return "results.json not found: constants not cross-checked"
    d = json.load(open(path))
    kinds = {"crank": "crank", "sol_settle_keep": "sol_settle_keep", "sol_settle_settle": "sol_settle_settle",
             "liquidate": "liquidate", "expire": "expire"}
    for v in VENUES:
        for mine, theirs in kinds.items():
            got = d["cu"][v][theirs]["requested_cu"][1]
            assert CU[v][mine] == got, (v, mine, CU[v][mine], got)
        assert V1_MAX[v] == d["crank_batches"][v]["v1_max"], v
        assert V0_K[v] == d["crank_batches"][v]["v0_protocol_alt_max"], v
        for i, price in enumerate(PRICES):
            for f_i, f in enumerate((0, 0.01, 0.05, 0.2, 1)):
                for key, k in (("unbatched", 1), ("v1_k6", V1_K), ("v0_alt", V0_K[v])):
                    want = d["cost_per_market_day_sol"][v][key][str(price)][f_i]
                    assert abs(targeted_day(v, price, k, f) - want) < 6e-6, (v, key, price, f)
    cad = d["cadence"]
    assert (cad["quiet_cranks_per_day"], cad["armed_sol_settle_calls_per_episode"], cad["price_burst_cranks_per_trigger"],
            cad["liquidity_trigger_extra_cranks"], cad["pumpswap_warmup_cranks"]) == \
        (QUIET_PER_DAY, ARMED_SETTLES, BURST_CRANKS, LIQ_TRIGGER_CRANKS, 8)
    g = d["global_lines"]
    assert g["set_coverage_pause"]["requested_cu"][1] == SET_COVERAGE_PAUSE_CU
    assert g["crank_sol_usd"]["requested_cu"][1] == CRANK_SOL_USD_CU
    assert g["crank_sol_usd_per_day"] == CRANK_SOL_USD_PER_DAY
    return "constants cross-checked against tests/bench/m7_keeper/results.json (" + d["builds"]["shortwin_so_sha256"][:8] + ")"


def global_tables():
    """ECON §5.8 "The global budget" (owner decision A of 2026-10-01): capacity, bursts, liquidity
    triggers and junk markets against the PROPOSED defaults."""
    sol = lambda x: x / LAMPORTS
    print(f"\n### The global budget (owner decision A, 2026-10-01): entitlements and capacity. A covered market's "
          f"entitlement is (1 + reserve_margin_bps = {RESERVE_MARGIN_BPS:,}) x its quiet day at its own per-crank "
          "charge; G funds them in rank order and the rest is the free pool (PumpSwap cranks; SOL)\n")
    rows = []
    for price in PRICES:
        rows.append([f"{price:,}", f"{sol(quiet_day('pumpswap', price, V1_K)):.4f} / {sol(quiet_day('pumpswap', price, 1)):.4f}",
                     f"{sol(entitlement('pumpswap', price, V1_K)):.4f} / {sol(entitlement('pumpswap', price, 1)):.4f}"])
    print(table(["priority price (uL/CU)", "quiet day (k = 6 / unbatched)", "entitlement (k = 6 / unbatched)"], rows))
    print("\nCovered markets whose entitlement G funds in full (k = 6 / unbatched); below the floor the idle cadence "
          f"draws at most tail_share_bps = {TAIL_SHARE_BPS:,} of G, i.e. that many below-floor markets' quiet days "
          "(k = 6 / unbatched) in brackets:\n")
    rows = []
    for g in (500_000_000, 1_000_000_000, 2_000_000_000, 5_000_000_000):
        cells = [f"{sol(g):g}"]
        for price in PRICES:
            tail = g * TAIL_SHARE_BPS / 10_000
            cells.append(f"{int(g // entitlement('pumpswap', price, V1_K)):,} / {int(g // entitlement('pumpswap', price, 1)):,} "
                         f"({int(tail // quiet_day('pumpswap', price, V1_K)):,} / {int(tail // quiet_day('pumpswap', price, 1)):,})")
        rows.append(cells)
    print(table(["G (SOL a day)", "0 uL/CU", "1,000 uL/CU", "10,000 uL/CU", "100,000 uL/CU"], rows))

    print(f"\n### Price bursts on a real market under the global budget: G = {sol(GLOBAL):g} SOL, {TIER1} covered "
          f"markets (the target and {TIER1 - 1} quiet ones, k = 6), the storm from the start of the UTC day; a burst = "
          f"{BURST_CRANKS} unbatched cranks drawn on the target's entitlement, then the free pool, never another "
          "market's entitlement. As built: the per-market 0.03 SOL less a quiet day at k = 6. Bursts that degrade "
          "(opens paused) / the attacker's round trips (SOL)\n")
    rows = []
    for depth in (100, 1_000):
        for phi_bps in (25, 100):
            att = round_trip(depth, phi_bps)
            for price in (0, 100_000):
                c = burst_cost(price)
                old = math.ceil((BUDGET - quiet_day("pumpswap", price, V1_K)) / c)
                first, spend, nxt = global_bursts(price, c)
                rows.append([f"{depth:,} SOL", f"{phi_bps} bps", f"{price:,}",
                             f"{old:,} / {sol(old * att):.3f}",
                             f"{first:,} / {sol(first * att):.3f} (keeper {sol(spend):.3f})",
                             f"{nxt:,} / {sol(nxt * att):.3f}",
                             f"{sol((first + (TIER1 - 1) * nxt) * att):.2f} / {sol(TIER1 * old * att):.2f}"])
    print(table(["pool SOL reserve", "fee one way", "priority (uL/CU)", "as built: one market",
                 "global: the first market", "global: each further market once the free pool is spent",
                 f"all {TIER1} markets: global / as built (attacker SOL)"], rows))

    print(f"\n### LP-wrapped liquidity-anomaly triggers under the global budget (duty 2, OWNER-M7-2): "
          f"{LIQ_TRIGGER_CRANKS} keeper cranks per isolated trigger (one per 11 s and market at most), drawn on the "
          f"target's entitlement, then the free pool; G = {sol(GLOBAL):g} SOL, {TIER1} covered markets, PumpSwap. "
          "Triggers that degrade / the attacker's wrapped cranks (SOL) / market-hours at one trigger per 11 s\n")
    rows = []
    for price in PRICES:
        att = tx_cost(LP_WRAP_CU, price)
        cells = [f"{price:,}"]
        for k in (1, V1_K):
            per = LIQ_TRIGGER_CRANKS * per_market_crank("pumpswap", price, k)
            first, _, nxt = global_bursts(price, per)
            cells.append(f"{first:,} / {sol(first * att):.4f} / {first * 11 / 3600:.0f} h")
            cells.append(f"{nxt:,} / {sol(nxt * att):.5f}")
        rows.append(cells)
    print(table(["priority price (uL/CU)", "unbatched keeper: the first market", "unbatched: each further market",
                 "batched keeper (k = 6): the first market", "batched: each further market"], rows))

    up = junk_upkeep_day()
    print(f"\n### Junk markets below the floor (var_floor_lamports = {sol(VAR_FLOOR):g} SOL): the 30-s idle cadence "
          f"only, from the free pool and within tail_share_bps = {TAIL_SHARE_BPS:,} ({sol(GLOBAL * TAIL_SHARE_BPS / 10_000):g} "
          f"SOL at G = {sol(GLOBAL):g}); never a covered market's entitlement. The attacker lists each (>= {sol(JUNK_LISTING):.3f} "
          f"SOL) and keeps a min-notional Tier D position open on it ({sol(up):.5f} SOL a day). PumpSwap cranks\n")
    rows = []
    tail = GLOBAL * TAIL_SHARE_BPS / 10_000
    for price in PRICES:
        cells = [f"{price:,}"]
        for k in (V1_K, 1):
            q = quiet_day("pumpswap", price, k)
            n = math.ceil(tail / q)
            cells.append(f"{sol(q):.4f}: {n:,} markets, attacker {sol(n * JUNK_LISTING):.2f} SOL once + "
                         f"{sol(n * up):.3f} SOL a day")
        rows.append(cells)
    print(table(["priority price (uL/CU)", "keeper per junk market-day, k = 6: junk markets that fill the tail",
                 "unbatched"], rows))
    print(f"\nThe keeper's spend on junk below the floor is at most {sol(tail):g} SOL a day however many are listed "
          "(as built: 0.0024-0.0263 SOL per junk market-day, unbounded in number); beyond the tail the junk markets' "
          "idle cranks are refused and they degrade (their own opens paused).")

    print(f"\n### Junk markets above the floor that outrank a real market (exit-critical hovering): each holds a "
          f"Tier D position of value at risk V_J = V_R near its threshold all day, so its per-second cranks (never "
          f"throttled) draw on its entitlement, the free pool and the entitlements of every market ranked below it, "
          f"then the overdraft; n junk markets with n x C >= G degrade every market ranked below V_J (opens paused; "
          f"their exits continue on the overdraft). C = continuous coverage at k = 6 (PumpSwap)\n")
    rows = []
    for v_r in (1, 5, 25):
        n_open, cap, per = hover_junk(v_r * LAMPORTS)
        for price in (0, 100_000):
            c_day = per_market_crank("pumpswap", price, V1_K) * DAY
            n = math.ceil(GLOBAL / c_day)
            rows.append([f"{v_r} SOL", f"{price:,}", f"{sol(c_day):.4f}", f"{n}",
                         f"{sol(n * n_open):.1f}", f"{sol(n * cap):.1f}", f"{sol(n * per):.3f}",
                         f"{sol(n * c_day):.3f}", f"{c_day / per:.1f}x"])
    print(table(["V_R", "priority (uL/CU)", "C, keeper per junk market-day", "junk markets", "their notional at open",
                 "attacker capital", "attacker per day", "keeper per day", "keeper / attacker per junk market-day"],
                rows))

    print(f"\n### The same junk hovering AND bursting (M7D-4): one round trip a minute on each junk market's own pool "
          f"({ROUND_TRIPS_PER_DAY:,} a day). Exit-critical is one crank a second (C); a burst's other {BURST_EXTRA} "
          f"cranks are discretionary (D a junk market-day, unbatched), drawn on the junk's own entitlement and the "
          f"free pool only, so the bursts drain the free pool and the junk's own entitlements, and the hovering must "
          f"still take the {TIER1} real markets' entitlements (E each): n = max(ceil({TIER1} E / C), "
          f"ceil(F0 / (D - E))), F0 = G - {TIER1} E. The attacker's round trip on its own pool: 2 delta V / 7.5 of fees "
          f"whatever the fee (the depth the caps need for V), all lost at the full fee, only the protocol's "
          f"{SELF_LP_LOST_BPS} bps of {JUNK_PHI_BPS} on a self-LP pool. As reviewed (before M7D-4) every slot of a burst "
          f"on a hovering market was exit-critical, ~2.5 cranks a second never throttled: ~1.08 SOL a junk "
          f"market-day, one junk market spending G\n")
    rows = []
    for v_r in (1, 5, 25):
        n_open, cap, per = hover_junk(v_r * LAMPORTS)
        for price in (0, 100_000):
            n0 = math.ceil(GLOBAL / (per_market_crank("pumpswap", price, V1_K) * DAY))
            n, e, c, d, keeper = hover_burst(price)
            full = ROUND_TRIPS_PER_DAY * junk_round_trip(v_r * LAMPORTS, JUNK_PHI_BPS)
            lp = ROUND_TRIPS_PER_DAY * junk_round_trip(v_r * LAMPORTS, SELF_LP_LOST_BPS)
            rows.append([f"{v_r} SOL", f"{price:,}", f"{sol(c):.4f} / {sol(d):.4f}", f"{n0} -> {n}",
                         f"{sol(n * cap):.1f}", f"{sol(n * (per + full)):.3f} / {sol(n * (per + lp)):.3f}",
                         f"{sol(keeper):.3f}"])
    print(table(["V_R", "priority (uL/CU)", "C / D, keeper per junk market-day", "junk markets (hovering -> and bursting)",
                 "attacker capital", "attacker per day (full fee / self-LP pool)", "keeper per day"], rows))


def main():
    note = cross_check()
    print(f"<!-- {note} -->\n")
    print("### Measured inputs: requested CU per instruction (ceil(simulated x 1.1), what the priority fee is paid on) "
          "and crank batches\n")
    rows = []
    for v in VENUES:
        c = CU[v]
        rows.append([NAMES[v], f"{c['crank']:,}", f"{c['sol_settle_keep']:,} / {c['sol_settle_settle']:,}",
                     f"{c['liquidate']:,} / {c['expire']:,}", V1_MAX[v], V0_K[v]])
    print(table(["venue", "crank_price", "sol_settle (keep / settles)", "liquidate / expire", "v1 max cranks",
                 "v0 + protocol ALT cranks"], rows))

    print("\n### Cost of continuous coverage: one crank per market per second (SOL per market per day)\n")
    rows = []
    for v in VENUES:
        for price in PRICES:
            cells = [NAMES[v], f"{price:,}"]
            for k in (1, V0_K[v], V1_K, V1_MAX[v]):
                cells.append(f"{per_market_crank(v, price, k) * DAY / LAMPORTS:.4f}")
            rows.append(cells)
    print(table(["venue", "priority price (uL/CU)", "unbatched (k = 1)", "v0 + ALT (venue's k)", "v1 k = 6",
                 "v1 max k"], rows))

    print("\n### Targeted coverage (duties 1-2): per-second cranks for a share f of the day, else one per 30 s; "
          "v1 k = 6 (unbatched in brackets); SOL per market per day\n")
    rows = []
    for v in VENUES:
        for f in (0.0, 0.01, 0.05, 0.20, 1.0):
            cells = [NAMES[v], f"{100 * f:g} %"]
            for price in PRICES:
                cells.append(f"{targeted_day(v, price, V1_K, f):.4f} ({targeted_day(v, price, 1, f):.4f})")
            rows.append(cells)
    print(table(["venue", "f", "0 uL/CU", "1,000 uL/CU", "10,000 uL/CU", "100,000 uL/CU"], rows))

    print("\n### Episode costs (unbatched, the costliest venue: PumpSwap; SOL per episode)\n")
    rows = []
    v = "pumpswap"
    for price in PRICES:
        armed = ARMED_SETTLES * tx_cost(CU[v]["sol_settle_keep"], price)
        burst = BURST_CRANKS * tx_cost(CU[v]["crank"], price)
        liq = tx_cost(CU[v]["liquidate"], price) + 1_000_000        # + a Jito tip of 0.001 SOL (assumption)
        warm = 8 * tx_cost(CU[v]["crank"], price)
        rows.append([f"{price:,}", f"{armed / LAMPORTS:.5f}", f"{burst / LAMPORTS:.5f}", f"{liq / LAMPORTS:.5f}",
                     f"{warm / LAMPORTS:.5f}"])
    print(table(["priority price (uL/CU)", f"one armed position ({ARMED_SETTLES} x sol_settle)",
                 f"one price deviation ({BURST_CRANKS} x crank)", "one liquidation (+0.001 SOL tip)",
                 "a PumpSwap warm-up (8 cranks)"], rows))

    print("\n### Global lines (the protocol, not a market's budget; SOL)\n")
    rows = []
    for price in PRICES:
        sol_usd = CRANK_SOL_USD_PER_DAY * tx_cost(CRANK_SOL_USD_CU, price)
        pause = tx_cost(SET_COVERAGE_PAUSE_CU, price, signatures=2)
        rows.append([f"{price:,}", f"{sol_usd / LAMPORTS:.4f}", f"{pause / LAMPORTS:.8f}"])
    print(table(["priority price (uL/CU)", f"crank_sol_usd, a day ({CRANK_SOL_USD_PER_DAY:,} calls)",
                 "set_coverage_pause, one call (two signatures)"], rows))

    print("\n### Liquidity-anomaly coverage (duty 2, #101): the keeper's cost against the attacker's (SOL; a "
          f"market with open positions; per-second coverage for liq_cover_s = {LIQ_COVER_S} s, measured "
          f"{LIQ_TRIGGER_CRANKS} cranks above the control per isolated trigger; PumpSwap cranks)\n")
    rows = []
    for price in PRICES:
        att = tx_cost(LP_WRAP_CU, price)
        k1 = tx_cost(CU[v]["crank"], price)
        k6 = per_market_crank(v, price, V1_K)
        rows.append([f"{price:,}", f"{att / LAMPORTS:.7f}", f"{LIQ_TRIGGER_CRANKS * k1 / LAMPORTS:.6f}",
                     f"{LIQ_TRIGGER_CRANKS * k1 / att:.1f}x / {LIQ_TRIGGER_CRANKS * k6 / att:.1f}x",
                     f"{k1 / att:.2f}x / {k6 / att:.2f}x"])
    print(table(["priority price (uL/CU)", "attacker: one LP-wrapped crank",
                 f"keeper: one isolated trigger ({LIQ_TRIGGER_CRANKS} cranks)",
                 "isolated trigger, keeper / attacker (unbatched / k = 6)",
                 "sustained, per second, keeper / attacker (unbatched / k = 6)"], rows))

    print("\n### As built at M7, the per-market O17 budget (0.03 SOL per market-day; superseded by the global budget, "
          "owner decision A of 2026-10-01): what exhausts it (OWNER-M7-2). Triggers every 11 s on top "
          "of the quiet cadence of a market with a healthy position; the day's budget then puts the market in "
          "degraded mode (opens paused until the UTC day rolls over + 600 s; exits continue). LP-wrapped "
          "triggers need a pool with LP accounting: PumpSwap and Raydium only (the pump curve has none; M7-SEC-2)\n")
    rows = []
    for vv in VENUES:
        if vv == "pump_curve":
            continue
        for price in PRICES:
            cells = [NAMES[vv], f"{price:,}"]
            for k in (1, V1_K):
                crank = per_market_crank(vv, price, k)
                quiet = QUIET_PER_DAY * crank
                left = BUDGET - quiet
                if left <= 0:
                    cells.append("quiet cadence alone exceeds the budget")
                    continue
                n = math.ceil(left / (LIQ_TRIGGER_CRANKS * crank))   # the trigger that spends the last lamport
                att = n * tx_cost(LP_WRAP_CU, price)
                cells.append(f"{n:,} triggers, {n * 11 / 3600:.1f} h, attacker {att / LAMPORTS:.4f} SOL")
            cells.append(f"{QUIET_PER_DAY * per_market_crank(vv, price, V1_K) / BUDGET * 100:.0f} % / "
                         f"{QUIET_PER_DAY * per_market_crank(vv, price, 1) / BUDGET * 100:.0f} %")
            rows.append(cells)
    print(table(["venue", "priority price (uL/CU)", "unbatched keeper", "batched keeper (k = 6)",
                 "quiet day's share of the budget (k = 6 / unbatched)"], rows))

    print("\n### Price bursts (duty 2) after M7-SEC-2: a burst needs a reverted >= 2 % excursion, a price round "
          f"trip of ~1 % of the token reserve (2 phi delta of fees), against the keeper's {BURST_CRANKS} cranks "
          "(PumpSwap cranks; SOL; the budget columns as built at M7, the per-market 0.03 SOL)\n")
    rows = []
    for depth in (100, 1_000):
        for phi_bps in (25, 100):
            att = 2 * phi_bps / 10_000 * 0.0099 * depth * LAMPORTS + BASE
            cells = [f"{depth:,} SOL", f"{phi_bps} bps", f"{att / LAMPORTS:.5f}"]
            for price in (0, 100_000):
                keeper = BURST_CRANKS * tx_cost(CU["pumpswap"]["crank"], price)
                n = math.ceil((BUDGET - QUIET_PER_DAY * tx_cost(CU["pumpswap"]["crank"], price)) / keeper)
                cells.append(f"{keeper / LAMPORTS:.5f} ({att / keeper:.1f}x; {n} bursts = {n * att / LAMPORTS:.3f} SOL"
                             " to spend the day's budget)")
            rows.append(cells)
    print(table(["pool SOL reserve", "fee one way", "attacker: one round trip",
                 "keeper unbatched at 0 uL/CU (attacker / keeper; budget)",
                 "keeper unbatched at 100,000 uL/CU (attacker / keeper; budget)"], rows))
    print("\n(As built at M7, before M7-SEC-2: one plain crank_price after anyone's honest >= 2 % move, "
          "5,000 lamports, started the same burst.)")

    global_tables()

    print("\n### Revenue of a market at the beta cap (25 SOL open notional, 80 % utilisation; SOL per day)\n")
    rows = []
    for tier, apr in RATE80.items():
        rent = 25 * apr / 10_000 / 365
        rows.append([tier, f"{apr / 100:.0f} %", f"{rent:.4f}", f"{0.15 * rent:.4f}", f"{0.85 * rent:.4f}"])
    print(table(["tier", "rent APR at 80 %", "rent (all)", "protocol share (15 %)", "lender share (85 %)"], rows))

    print("\n### Break-even: open notional per market at which the protocol's rent share pays for continuous "
          "per-second coverage (v1 k = 6, PumpSwap cranks)\n")
    rows = []
    for price in (1_000, 10_000, 100_000):
        cost = per_market_crank("pumpswap", price, V1_K) * DAY / LAMPORTS
        cells = [f"{price:,}", f"{cost:.4f}"]
        for tier, apr in RATE80.items():
            cells.append(f"{cost / (0.15 * apr / 10_000 / 365):,.0f} SOL")
        rows.append(cells)
    print(table(["priority price (uL/CU)", "coverage cost (SOL/day)", "A", "B", "C", "D"], rows))


if __name__ == "__main__":
    main()
