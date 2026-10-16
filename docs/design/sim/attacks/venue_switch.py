"""R3B-7(a) / R3A-11(d): ring liquidity across a venue change; M4 / M4R: Raydium adoptions.

(a) At graduation the curve's sqrt(vT*vQ) (~5.7e12) is ~1.35x the new PumpSwap pool's (~4.2e12);
a Raydium fast-path switch may land on a pool with a quarter of the depth.  v0.3's ARCH text
carried every ring entry's L and lobs over; v0.4 (like the sims) rewrites them to the reset
liquidity.  Compared here: outcome of genuine moves after the change, and the number of seconds
after it during which the settlement box is wider than fv_dev_bps in L (which blocks any S2/S3
amount that depends on it).  The graduation rows run the program's rule since M6 (ECONOMICS.md #104, #105;
Market._graduate: the ring rewritten to the pool's lp_supply, then the 8-bucket warm-up, as a Raydium adoption;
the carried-over variant keeps the ring's L through the same restart); the Raydium rows replay
FAST_SCENARIOS[1], whose switch is the program's rule (Market.adopt_pool, M4R: the ring is rewritten to
the anchor floor(D*1e4/lambda_min) and the 8-bucket warm-up re-seeds it); the cell gives the wide
seconds and how many of them fall inside the warm-up gate, during which the box decides nothing.
Since #99 that replay is the 120 % candidate (lambda 90 %): a position at the incumbent's size cap fits only
a candidate whose locked depth covers the incumbent's L_slow (a quarter of the depth no longer carries it).

(b)-(d) The Raydium pool rules (ECON §5.2, §6.2, §10.1, §10.3; changelog #91-#96; M4-SEC-1/-3/-5/-6,
M4V-1/-2/-3, M4R-1/-2):
  * a collection window's candidate needs its locked SOL >= pool_min_real_sol (the window floor;
    listings and zero-position adoptions keep the /lambda_min floor); no lambda_now clause, so a pool
    with lambda < lambda_min is eligible when its locked SOL clears the floor.  M4R: a proposal judges
    the floor at min(P_ema, P_cand), the execution at P_ema (window_floor_ok / position_floor_ok);
  * M4R (the program's rule, Market.adopt_pool): the adoption reads no sample; L_ema and every ring
    entry := cap = floor(D*1e4/lambda_min), L_slow = min(L_slow, D), then the 8-bucket warm-up
    re-seeds P_ema / L_ema from the new pool's post-change buckets and gates the keeper exits;
  * #99 (owner decision 2026-09-29, the program's rule): a window whose adoption carries open positions
    opens and executes only if every open position fits the candidate's opening caps on its locked-depth
    X0 as an open right after the adoption computes it (Market.carried_fit: min(X_now, min(L_slow, D),
    D/lambda_min) over sqrt(P) at min(P_ema, P_cand) and at P_ema; the execution without X_now); the
    capped positions of these examples move only onto a candidate whose locked depth covers the
    incumbent's L_slow: no more onto the 25 %-depth or lambda 25 % candidates of (b) nor onto (d1)'s
    pool_fast_min_bps candidates ("no adoption (#99 ...)": frozen on the halted pool, S3 at fair
    value at the deadline); the comparison rules have no such check;
  * comparison rows: v0.4.1 (min(sqrt(X*Y)_live, current) from the adopting instruction's own sample,
    Market.fast_switch), the M4 review's record-only fix min(L_rec, cap), and M4 as built
    (L_new = min(L_rec, L_live, cap), L_rec the candidate's sqrt(X*Y) at the proposal that recorded it,
    L_live at the execution; BuiltM4 below, superseded by M4R).
(b) deeper and lambda < lambda_min candidates of an honest fast path; (c) LP moves around the two
sampling instructions of the M4 rules; (d) the L push on pools with lambda < lambda_min: held drains
to D (the lowest sample any instruction can read), consecutive attacker-only seconds, a contrary LP
observation every second (the S2/S3 amount spread it opens), and S2 reachability after the full drain.
The Market rows read the exit reference with M4F-1's cap (Market.ref_depth_cap): after a drain of a pool
with lambda < lambda_min the reference credits at most D/lambda_min (d1's two RC pump rows at lambda <= 25 %);
BuiltM4 keeps the uncapped L_b = max(L_live, L_ema, L_slow) of M4 as built, and its liquidation spot leg at L_live
(the Market rows read it at max(L_live, L_ema') since #101, FITS-2; no row here changes).
attacks/switch_warmup.py has the M4R attack tables (M4R-1, M4R-2, freezes, degraded mode).
Deterministic; standard library only.
"""
import copy
import sys
from common import *

S = sys.modules["shortwin_sim"]


class CarryOver(Market):
    def _rewrite_ring_liq(self, L):
        self.b_sec = None                    # bucket cleared, ring L / lobs carried over (v0.3 ARCH text)


def adoption_liquidity_m4(l_live, l_rec, l_ema, D, P=DEFAULT):
    """M4 as built (changelog #92, superseded by M4R #96; comparison rows only): L_new written into L_ema and
    every ring entry at a Raydium adoption, cap = floor(D*1e4/lambda_min):
        a collection window recorded the candidate (l_rec > 0):  L_new = min(L_rec, L_live, cap)
        otherwise (no position to settle):                      L_new = min(L_live, L_ema, cap)"""
    cap = depth_at_lambda(D, P.pool_lock_min_bps)
    if l_rec > 0:
        return min(l_rec, l_live, cap)
    return min(l_live, l_ema, cap)


class BuiltM4(Market):
    """The Raydium adoption as built after M4 (comparison rows only): L_ema = adoption_liquidity_m4(...),
    L_slow = min(L_slow, D), ring L rewritten, P_ema carried; no warm-up, no gate, no guard, no cap on the
    exit reference liquidity (M4F-1): L_b = max(L_live, L_ema, L_slow), and the liquidation spot leg at L_live
    (before FITS-2 (a), #101)."""

    def ref_depth_cap(self):
        return None

    def spot_liquidity(self, L_live):
        return L_live

    def adopt_pool(self, pool, t, l_rec=0, l_live=None):
        D = pool.locked_depth()
        l_new = adoption_liquidity_m4(pool.sqrt_k() if l_live is None else l_live, l_rec, self.liq, D, self.P)
        self.venue = pool
        self.liq = l_new
        self.liq_slow = min(self.liq_slow, D)
        self._rewrite_ring_liq(self.liq)
        self.pool_fail_since = self.last_pool_fail_obs = None
        self.no_route_since = self.last_no_route_obs = None


def mk(key, carry):
    cls = CarryOver if carry else Market
    return cls(MARKETS[key](), tier_of(key), DEFAULT)


def box_wide_split(m, t0, t1, at_gate=None):
    """(seconds in [t0, t1) with the settlement box wider than fv_dev_bps in L, of which inside the M4R warm-up
    gate -- where the box decides nothing).  at_gate (a list), if given, receives L_ema / sqrt(X*Y) at the
    first second the gate is open (ema_warm = 0 after an M4R adoption)."""
    wide = gated = 0
    for t in range(t0, t1):
        m.step_to(t)
        if at_gate is not None and not at_gate and m.warm == 0:
            at_gate.append(m.liq / m.venue.sqrt_k())
        P_lo, P_hi, L_lo, L_hi = m.settle_box(t)
        if (L_hi - L_lo) * BPS > DEFAULT.fv_dev_bps * L_lo:
            wide += 1
            gated += m.gated()
    return wide, gated


def box_wide_seconds(m, t0, t1):
    return box_wide_split(m, t0, t1)[0]


def wide_cell(w, unit=""):
    """'n' or, when some wide seconds fall inside the warm-up gate, 'n (g in the gate)'."""
    wide, gated = w
    return f"{wide}{unit} ({gated} in the gate)" if gated else f"{wide}{unit}"


def change_run(m2, sc):
    """Replays sc's events on m2 (a fresh position, no exit) up to the venue change; returns its second."""
    t, ev = 0, sorted(sc.events, key=lambda e: e[0])
    ei, changed = 0, None
    venue0 = id(m2.venue)
    while t < sc.t_event + 400 and changed is None:
        t += 1
        while ei < len(ev) and ev[ei][0] <= t:
            ev[ei][1](m2)
            ei += 1
        m2.step_to(t)
        if id(m2.venue) != venue0:
            changed = t
    return changed


# --- (b)-(d): the Raydium adoption rules -----------------------------------------------------------------
RULES = {"v041": "v0.4.1: min(√(X·Y)_live, L_ema)", "record": "M4 review fix: min(L_rec, cap)",
         "built": "M4 as built: min(L_rec, L_live, cap)", "m4r": "M4R: cap + 8-bucket warm-up (proposed)"}


def unlocked_bps(v):
    return (v.lp_units - v.lp_locked) * BPS // v.lp_units


def lp_sample(c, move):
    """The sqrt(X*Y) of candidate c that one instruction reads when it is wrapped in an LP move of c:
    None = none; "dep" = a deposit as large as the pool's LP counter (lambda halves, D unchanged);
    "wd" = every unlocked LP withdrawn (sqrt(X*Y) falls to ~D).  The round trip leaves c as it was."""
    if move is None:
        return c.sqrt_k()
    cc = copy.deepcopy(c)
    if move == "dep":
        cc.add_liquidity(BPS)
    else:
        cc.remove_liquidity(unlocked_bps(cc))
    return cc.sqrt_k()


def cand_pool(v, cand_bps, lock_bps):
    return RaydiumPool(v.X * cand_bps // BPS, v.Y * cand_bps // BPS, fee_ppm=v.fee_ppm,
                       out_share_ppm=v.out_share_ppm, name=v.name + "'", lock_bps=lock_bps)


def m4_fast_path_events(t0, cand_bps, lock_bps, rule, rec_move=None, exec_move=None, horizon=900):
    """The incumbent halts at t0; once pool_fail has held for pool_fast_route_s a proposal opens the
    collection window on the candidate (cand_bps of the incumbent's reserves at locked share lock_bps) and
    records its sqrt(X*Y); pool_fast_window_s later the execution adopts it under `rule`.  Under the M4 and
    M4R rules a candidate below the window floor opens no window (PoolTooThin) and the market stays halted
    (M4R: the proposal judges it at min(P_ema, P_cand)).  The M4R adoption reads no sample, so the LP moves
    around the proposal and the execution change nothing there."""
    def halt(m):
        v = m.venue
        m._fp = cand_pool(v, cand_bps, lock_bps)
        m._fp_rec = None
        m._pool_d = v.locked_depth()
        v.disabled = True

    def tick(m):
        P = m.P
        c = getattr(m, "_fp", None)
        if c is None or m.pool_fail_since is None:
            return
        if m._fp_rec is None and m.t >= m.pool_fail_since + P.pool_fast_route_s:
            if rule == "m4r":                        # #99: and every open position fits the candidate's caps
                ok = window_floor_ok(c.locked_depth(), m.ema, c.spot_wad())
                if ok and not m.carried_fit(c):
                    ok, m._refused = False, "#99: the position does not fit"
            else:
                ok = rule == "v041" or position_floor_ok(c.locked_depth(), m.ema)
            if not ok:
                m._fp = None
                return
            m._fp_rec = lp_sample(c, rec_move)
        if m.t >= m.pool_fail_since + P.pool_fast_route_s + P.pool_fast_window_s:
            c.move_to_price(m.ema)                   # candidate trades at the market price
            live = lp_sample(c, exec_move)
            if rule == "v041":                       # v0.4.1 reads the executing instruction's own sample
                cw = copy.deepcopy(c)
                if exec_move == "dep":
                    cw.add_liquidity(BPS)
                elif exec_move == "wd":
                    cw.remove_liquidity(unlocked_bps(cw))
                m.fast_switch(cw, m.t)
                m.venue = c                          # the round trip restores the pool
            elif rule == "record":                   # the M4 review's fix (superseded by M4V-2)
                m.adopt_pool(c, m.t, l_rec=m._fp_rec, l_live=INF)
            elif rule == "built":                    # M4 as built (superseded by M4R)
                m.adopt_pool(c, m.t, l_rec=m._fp_rec, l_live=live)
            else:                                    # M4R: no sample; the floor and the fit (#99) at P_ema
                if position_floor_ok(c.locked_depth(), m.ema) and m.carried_fit(c, execution=True):
                    m.adopt_pool(c, m.t)
            m._fp = None
    return ((t0, halt),) + tuple((t0 + i, tick) for i in range(1, horizon))


def m4_case(key, cand_bps, lock_bps, rule, rec_move=None, exec_move=None):
    """(summary of the pumped run, L_ema/sqrt(X*Y) at the switch or None, seconds box wide, market at the switch)."""
    sc = Scenario("halt -> fast path, then pump 2x/120s", ramp_path(TE + 300, 2.0, 120),
                  events=m4_fast_path_events(TE, cand_bps, lock_bps, rule, rec_move, exec_move), window=1500)
    cls = Market if rule in ("m4r", "v041") else BuiltM4
    m = cls(MARKETS[key](), tier_of(key), DEFAULT)
    pos, r, P1 = run(m, 1.0, sc)
    s = summarize(pos, r, P1)
    m2 = cls(MARKETS[key](), tier_of(key), DEFAULT)
    m2.open(m2.max_open_q(), 0)
    changed = change_run(m2, sc)
    if not changed:
        return s, None, "n/a", m2
    ratio = m2.liq / m2.venue.sqrt_k()
    gate = []
    wide = box_wide_split(m2, changed + 1, changed + 61, gate if rule == "m4r" else None)
    return s, (f"{ratio:.3f} → {gate[0]:.3f}" if gate else f"{ratio:.3f}"), wide, m2


def cand_facts(key, cand_bps, lock_bps):
    """(sqrt(X*Y) vs incumbent, lambda, D vs pool_d, real SOL, locked SOL at P_ema, v0.4.1 eligible, as built)."""
    m = new_market(key)
    v = m.venue
    c = cand_pool(v, cand_bps, lock_bps)
    D = c.locked_depth()
    ls = locked_sol(D, m.ema)
    v041 = c.Y >= POOL_MIN_REAL_SOL and c.lock_bps() >= DEFAULT.pool_lock_min_bps
    return (c.sqrt_k() / v.sqrt_k(), c.lock_bps(), D / v.locked_depth(), c.Y, ls, v041,
            position_floor_ok(D, m.ema))


def fmt_exit(s, TE0=TE):
    return f"{s['kind']} | {s['t'] - TE0:+d}s | {s['mult']:.2f}x | {s['lender_pnl_pct']:+.1f}%"


def table_b():
    print("(b) Honest fast path (incumbent halted at t0; window at +120 s, adoption at +180 s; then pump 2x/120s "
          "from +300 s) onto thinner, deeper and lambda < lambda_min candidates.  Window floor: locked SOL at "
          "P_ema >= 25 SOL (M4R: at min(P_ema, P_cand) at the proposal, the same here; v0.4.1: real SOL >= 25 and "
          "lambda >= 50 %).  M4R: L_ema / √(X·Y) at the switch -> at the gate (8 integrations later); the wide "
          "seconds inside the warm-up gate decide nothing.  #99: M4R also needs the capped position to fit the "
          "candidate's opening caps as an open right after the adoption (X0 <= min(L_slow, D)/sqrt(P)): a "
          "candidate whose locked depth D is below the incumbent's L_slow does not carry it: no window, the "
          "position frozen on the halted pool.")
    print()
    print("| market | candidate: √(X·Y) / λ / D vs `pool_d` | real / locked SOL | eligible v0.4.1 / M4, M4R | "
          "rewrite | `L_ema` / √(X·Y) at the switch | outcome | exit vs event | price at exit | lender P&L | "
          "seconds with the box wider than 1 % in L (first 60 s) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for key in ("RA", "RB", "RC"):
        for cb, lb in ((2500, 9000), (15000, 9000), (10000, 2500), (20000, 2500)):
            k, lam, dr, y, ls, e41, eb = cand_facts(key, cb, lb)
            for rule in ("v041", "built", "m4r"):
                s, ratio, wide, m2 = m4_case(key, cb, lb, rule)
                rt = f"no adoption ({getattr(m2, '_refused', 'window floor')})" if ratio is None else ratio
                print(f"| {key} | {k * 100:.0f}% / {lam / 100:.0f}% / {dr * 100:.0f}% | {y / 1e9:.1f} / {ls / 1e9:.1f} | "
                      f"{'yes' if e41 else 'no'} / {'yes' if eb else 'no'} | {RULES[rule]} | {rt} | "
                      f"{fmt_exit(s)} | {wide if ratio is None else wide_cell(wide)} |")


def table_c():
    print("(c) LP moves around the two sampling instructions of the M4 rules (RA; the round trip costs no swap "
          "fee): 'deposit' = as much again as the pool's LP counter around the instruction (lambda halves, D "
          "stays), 'withdraw' = every unlocked LP withdrawn around it (√(X·Y) falls to D).  Cells: L_ema / √(X·Y) "
          "at the switch (M4R: -> at the gate), seconds with the box wider than 1 % in L (first 60 s; M4R: of "
          "which inside the warm-up gate).  M4R reads no sample, so its column is the same in every row.")
    print()
    print("| candidate: √(X·Y) / λ | LP move | " + " | ".join(RULES[r] for r in ("v041", "record", "built", "m4r"))
          + " | exit (every rule) |")
    print("|---|---|---|---|---|---|---|")
    moves = [("none", None, None), ("deposit around the recording proposal (M4V-2)", "dep", None),
             ("deposit around the execution (M4-SEC-5)", None, "dep"),
             ("withdraw around the recording proposal", "wd", None),
             ("withdraw around the execution (M4-SEC-5)", None, "wd")]
    for cb, lb in ((15000, 9978), (20000, 2500)):
        for name, rm, em in moves:
            cells, exits = [], set()
            for rule in ("v041", "record", "built", "m4r"):
                s, ratio, wide, m2 = m4_case("RA", cb, lb, rule, rm, em)
                if ratio is None:                    # #99: the capped position does not fit this candidate
                    cells.append(f"no adoption ({getattr(m2, '_refused', 'window floor')})")
                else:
                    cells.append(f"{ratio}, {wide_cell(wide, ' s')}")
                exits.add(fmt_exit(s).replace(" | ", ", "))
            ex = exits.pop() if len(exits) == 1 else " / ".join(sorted(exits))
            print(f"| {cb / 100:.0f}% / {lb / 100:.2f}% | {name} | " + " | ".join(cells) + f" | {ex} |")


def same_d_pool(key, lock_bps):
    """The example pool's locked depth D at locked share lock_bps (reserves scaled by 5,000/lock_bps):
    sizing (<= D/lambda_min) and a full drain (to D) are the example's; only the depth above D differs."""
    v = MARKETS[key]()
    return RaydiumPool(v.X * 5000 // lock_bps, v.Y * 5000 // lock_bps, fee_ppm=v.fee_ppm,
                       out_share_ppm=v.out_share_ppm, name=v.name, lock_bps=lock_bps)


def drain_all(m):
    m.venue.remove_liquidity(unlocked_bps(m.venue))


def drain_adopted(m):
    """drain_all on the carried position's new pool; nothing if no candidate was adopted (#99: the incumbent stays
    halted and the position frozen)."""
    if m.venue.has_route():
        drain_all(m)


def table_d1():
    print("(d1) Held drain of every unlocked LP (the lowest L any instruction or observation can read is D) on "
          "pools of the same locked depth D at lambda 50 / 25 / 10 %; capped position opened on the pool, and "
          "RA's capped position carried by the fast path onto a candidate whose unlocked LP is then drained.  "
          "#99: a candidate with D = 25 % of pool_d (pool_fast_min_bps) no longer carries it (an open on it right "
          "after the adoption is sized on min(L_slow, D) = D, an eighth of the incumbent's depth): refused, the "
          "position stays frozen on the halted pool (1.00x; S3 at fair value at its deadline); the thinnest "
          "candidate that carries it has D = the incumbent's whole depth (twice pool_d at lambda 50 %).")
    print()
    print("| market | pool | λ | Q (raw) | scenario | outcome | exit vs event | price at exit | CR spot/EMA at exit | "
          "lender P&L |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    scs = [("drain all unlocked LP", lambda t: None, 600),
           ("drain all unlocked LP + x1.5", step_path(TE + 1, 1.5), 600),
           ("drain all unlocked LP + pump 2x/120s", ramp_path(TE + 60, 2.0, 120), 900)]
    for key in ("RA", "RC"):
        for lb in (5000, 2500, 1000):
            for name, path, win in scs:
                v = same_d_pool(key, lb)
                lam = v.lock_bps()
                m = Market(v, tier_of(key), DEFAULT)
                pos, r, P1 = run(m, 1.0, Scenario(name, path, events=((TE, drain_all),), window=win))
                s = summarize(pos, r, P1)
                print(f"| {key} | opened on it | {lam / 100:.0f}% | {pos.Q:,} | {name} | {s['kind']} | "
                      f"{s['t'] - TE:+d}s | {s['mult']:.2f}x | {s['cr_s']:.2f}/{s['cr_e']:.2f} | "
                      f"{s['lender_pnl_pct']:+.1f}% |")
    for cb, lb in ((2500, 5000), (5000, 2500), (20000, 5000), (40000, 2500)):
        for name, path in (("drain all unlocked LP", lambda t: None),
                           ("drain all unlocked LP + x1.5", step_path(TE + 401, 1.5)),
                           ("drain all unlocked LP + pump 2x/120s", ramp_path(TE + 460, 2.0, 120))):
            ev = m4_fast_path_events(TE, cb, lb, "m4r") + ((TE + 400, drain_adopted),)
            m = new_market("RA")
            pool_d = m.venue.locked_depth()
            pos, r, P1 = run(m, 1.0, Scenario(name, path, events=ev, window=1500))
            s = summarize(pos, r, P1)
            d_share = cand_pool(new_market("RA").venue, cb, lb).locked_depth() * 100 / pool_d
            if m.venue.has_route():
                where = f"carried: fast path at +180 s onto {cb / 100:.0f}% √(X·Y) (D {d_share:.0f}% of pool_d), drained at +400 s"
            else:
                where = f"#99: {cb / 100:.0f}% √(X·Y) (D {d_share:.0f}% of pool_d) refused, frozen on the halted pool"
            print(f"| RA | {where} | {lb / 100:.0f}% | {pos.Q:,} | {name} | {s['kind']} | {s['t'] - TE:+d}s | "
                  f"{s['mult']:.2f}x | {s['cr_s']:.2f}/{s['cr_e']:.2f} | {s['lender_pnl_pct']:+.1f}% |")


def drained(v):
    c = copy.deepcopy(v)
    c.remove_liquidity(unlocked_bps(c))
    return c


def table_d2():
    print("(d2) k consecutive attacker-only seconds in which every observation is withdraw-wrapped (L = D), and "
          "the evaluating instruction wrapped too (live legs at D); RA's capped position at a flat price, EMA "
          "converged.  L values relative to the pool's √(X·Y).")
    print()
    print("| λ | k | `L_ema` | `L_lo` | `L_hi` | CR spot at D / EMA | liquidatable | S1 legs | S2 legs | refuted |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for lb in (5000, 2500, 1000):
        for k in (1, 3, 4, 5, 8, 40):
            v = same_d_pool("RA", lb)
            m = Market(v, tier_of("RA"), DEFAULT)
            sk = v.sqrt_k()
            pos = m.open(m.max_open_q(), 0)
            for t in range(1, 601):
                m.step_to(t)
            t = 600
            dv = drained(v)
            for _ in range(k):
                t += 1
                m.advance(t)
                m.venue = dv
                m.observe(t)
                m.venue = v
            t += 1
            m.advance(t)
            m.venue = dv
            m.observe(t)
            st = m.status(pos, t)
            box = m.settle_box(t)
            K = m.settle_reward(pos.V)
            s1, s2 = m._legs(pos, st, box, K)
            ref = m._refuted(pos, st, box, K)
            m.venue = v
            yn = lambda b: "yes" if b else "no"
            print(f"| {lb / 100:.0f}% | {k} | {m.liq / sk:.3f} | {box[2] / sk:.3f} | {box[3] / sk:.3f} | "
                  f"{st['cr_s'] / BPS:.2f} / {st['cr_e'] / BPS:.2f} | {yn(st['liq'])} | {yn(s1)} | {yn(s2)} | "
                  f"{yn(ref)} |")


def amount_spread(key, lb, fee_ppm, mult, expired):
    """bps by which the S2/S3 amount differs across the box when every second of the ring holds one
    withdraw-wrapped observation next to the honest one (L_lo = D, L_hi = √(X·Y)): > fv_dev_bps defers."""
    v = same_d_pool(key, lb)
    v.fee_ppm = fee_ppm
    m = Market(v, tier_of(key), DEFAULT)
    pos = m.open(m.max_open_q(), 0)
    for t in range(1, 601):
        m.step_to(t)
    t = 600
    if mult != 1.0:
        for i in range(1, 121):
            t += 1
            m.exo_move(int(pos.spot_post * mult ** (i / 120)))
            m.step_to(t)
        for _ in range(20):
            t += 1
            m.step_to(t)
    dv = drained(v)
    for _ in range(8):
        t += 1
        m.step_to(t)
        m.venue = dv
        m.observe(t)
        m.venue = v
    t += 1
    m.step_to(t)
    st = m.status(pos, t)
    if expired:
        st = dict(st, expired=True)
    P_lo, P_hi, L_lo, L_hi = m.settle_box(t)
    take = pos.V - m.settle_reward(pos.V)
    x_lo = min(take, m.fv_at(pos, st, P_lo, L_hi))
    x_hi = min(take, m.fv_at(pos, st, P_hi, L_lo))
    return (x_hi - x_lo) * BPS // x_lo, pos.Q * BPS / v.X


def table_d3():
    print("(d3) A contrary LP observation every second (an unlocked LP holder's [withdraw, crank, re-deposit] next "
          "to the honest crank: no swap fee): S2/S3 amount spread across the box, bps (settles only if <= "
          "fv_dev_bps = 100); capped position, expired at a flat price or liquidatable after a pump.")
    print()
    lams = (5000, 3333, 2500, 1000)
    print("| market | fee | state | " + " | ".join(f"λ {lb / 100:.0f}%" for lb in lams) + " |")
    print("|---|---|---|" + "---|" * len(lams))
    for key, mult in (("RA", 1.6), ("RC", 2.0)):
        for fee in (2500, 10000):
            for mu, exp, label in ((1.0, True, "expired, flat"), (mult, False, f"liquidatable (x{mult})")):
                cells = []
                for lb in lams:
                    d, _ = amount_spread(key, lb, fee, mu, exp)
                    cells.append(f"{d}")
                print(f"| {key} | {fee / 1e4:.2f}% | {label} | " + " | ".join(cells) + " |")


def table_d4():
    print("(d4) S2 reachability at the per-position cap after the full drain (price multiple at which each first "
          "holds on the live state; ECON §8.6's check on pools of the same D at lambda 50 / 25 / 10 %).")
    print()
    print("| market | λ | S1 at | S2 at |")
    print("|---|---|---|---|")
    for key in ("RA", "RC"):
        for lb in (5000, 2500, 1000):
            m = Market(same_d_pool(key, lb), tier_of(key), DEFAULT)
            lam = m.venue.lock_bps()
            pos = m.open(m.max_open_q(), 0)
            drain_all(m)
            K = m.settle_reward(pos.V)
            s1 = s2 = None
            for i in range(1, 800):
                k = 1.01 ** i
                v = copy.deepcopy(m.venue)
                v.move_to_price(int(pos.spot_post * k))
                q = v.quote_at(pos.Q, *v.eff())
                if s1 is None and (q is None or q[1] + pos.ert > pos.V - K):
                    s1 = k
                X, Y = v.eff()
                if s2 is None and (q is None or q[0] * WAD * BPS // (pos.Q * (Y * WAD // X)) - BPS > pos.slip_max):
                    s2 = k
                if s1 and s2:
                    break
            print(f"| {key} | {lam / 100:.0f}% | x{s1:.2f} | x{s2:.1f} |")


if __name__ == "__main__":
    print("| market | scenario | ring at the venue change | outcome | exit vs event | price at exit | lender P&L | "
          "seconds with the settlement box wider than 1 % in L (first 60 s after the change) |")
    print("|---|---|---|---|---|---|---|---|")
    cases = [("D@90%", S.GRAD_SCENARIOS[1]), ("D@90%", S.GRAD_SCENARIOS[2]), ("D@97%", S.GRAD_SCENARIOS[1]),
             ("RA", S.FAST_SCENARIOS[1]), ("RB", S.FAST_SCENARIOS[1]), ("RC", S.FAST_SCENARIOS[1])]
    for key, sc in cases:
        for carry in (True, False):
            m = mk(key, carry)
            pos, r, P1 = run(m, 1.0, sc)
            s = summarize(pos, r, P1)
            # box width right after the change, on a fresh run without the position's exit
            m2 = mk(key, carry)
            pos2 = m2.open(m2.max_open_q(), 0)
            t, ev = 0, sorted(sc.events, key=lambda e: e[0])
            ei, changed = 0, None
            venue0 = id(m2.venue)
            while t < sc.t_event + 400 and changed is None:
                t += 1
                while ei < len(ev) and ev[ei][0] <= t:
                    ev[ei][1](m2)
                    ei += 1
                m2.step_to(t)
                if id(m2.venue) != venue0:
                    changed = t
            wide = wide_cell(box_wide_split(m2, changed + 1, changed + 61)) if changed else "n/a"
            ring = ("carried over (v0.3 ARCH)" if carry else
                    "rewritten to the anchor (M4R)" if key.startswith("R") else "rewritten to lp_supply (M6)")
            print(f"| {key} | {sc.name} | {ring} | {s['kind']} | "
                  f"{s['t'] - sc.t_event:+d}s | {s['mult']:.2f}x | {s['lender_pnl_pct']:+.1f}% | {wide} |")
    for part in (table_b, table_c, table_d1, table_d2, table_d3, table_d4):
        print()
        part()
