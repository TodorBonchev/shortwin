"""The calibration universe (WP-9 step 2): which mainnet markets the crawler follows, chosen from
**on-chain eligibility**, never from a volume ranking.

- **Census** (``census``; weekly, on the archive endpoint because publicnode refuses
  ``getProgramAccounts``): every canonical PumpSwap pool (``index`` 0, WSOL quote, at the canonical
  address, not mayhem, the base mint through SPEC §3's filter) and every Raydium AMM v4 / CPMM
  WSOL pool above the **locked-depth floor** ``⌊locked·10⁴/λ_min⌋ ≥ pool_min_real_sol`` (ECON
  §6.2: ``locked = λ·Y``, λ the burned LP share), AMM v4 executable statuses only. Each gets its
  real SOL, λ, an age and the tier ECON §6.2 would give it at the census's SOL/USD.
- **Selection** (``select``): every Tier A pool, a seeded random sample of Tier B and of the
  Tier C pools with at least ``C_MIN_REAL_SOL`` (the floor Raydium enforces, applied to PumpSwap
  too so that the C sample is not dominated by abandoned pools), up to the caps.
- **Cohorts** (discovered by the crawler, ``parse_create`` / ``parse_migration``): a seeded
  random sample of pump curve creations (Tier D), each followed until it graduates or dies, and
  of pump → PumpSwap migrations (fresh Tier C graduates), each followed for ``FRESH_DAYS``.
- **SOL/USD**: the AMM v4 SOL/USDC pool ``58oQChx4…`` (plan §1 WP-9).

Usage (standard library only)::

    python3 scripts/gapcal/universe.py census [--data DIR]   # a census now (refused while a crawl runs)
    python3 scripts/gapcal/universe.py show [--data DIR]     # the current universe, offline

The crawler (``fetch.py``) runs the census itself when the last one is older than a week.
"""

import argparse
import base64
import gzip
import hashlib
import json
import math
import os
import random
import struct
import sys
import time

import decode as D
from rpc import MAINNET_BETA, PUBLICNODE, Rpc, b58decode, b58encode

PUMP_MINT_AUTHORITY = "TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM"  # signs every pump create
PUMP_WITHDRAW_AUTHORITY = "39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg"  # every migration
CPMM_AUTHORITY = "GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL"
AMMV4_AUTHORITY = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"
SOL_USDC_V4 = "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2"
PAMM_CREATE_POOL = bytes.fromhex("e992d18ecf6840bc")
D_CREATE = bytes.fromhex("1b72a94ddeeb6376")  # pump CreateEvent

POOL_MIN_REAL_SOL = 25_000_000_000  # ECON §15 default (lamports of locked SOL / λ_min)
LAMBDA_MIN_BPS = 5_000  # pool_lock_min_bps default
C_MIN_REAL_SOL = POOL_MIN_REAL_SOL
AMMV4_EXECUTABLE = (1, 6, 7)  # STATUS_INITIALIZED, SWAP_ONLY, WAITING_TRADE (venue-state)
CPMM_SWAP_DISABLED = 4  # PoolStatusBitIndex::Swap
CAPS = {"A": 150, "B": 80, "C": 30}  # census-selected markets per tier (A: all of them)
FRESH_DAYS = 7  # a sampled graduate is followed for its first week (C -> B / A transitions)


# ---- addresses --------------------------------------------------------------------------------

_P = 2**255 - 19
_D = (-121665 * pow(121666, _P - 2, _P)) % _P


def _on_curve(b):
    """Whether 32 bytes decode to an ed25519 point (RFC 8032 §5.1.3; dump-fixtures.py)."""
    y = int.from_bytes(b, "little") & ((1 << 255) - 1)
    if y >= _P:
        return False
    y2 = y * y % _P
    u, v = (y2 - 1) % _P, (_D * y2 + 1) % _P
    x2 = u * pow(v, _P - 2, _P) % _P
    if x2 == 0:
        return True
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P:
        x = x * pow(2, (_P - 1) // 4, _P) % _P
    return (x * x - x2) % _P == 0


def pda(seeds, program):
    """``find_program_address(seeds, program)`` -> base58 address."""
    for bump in range(255, -1, -1):
        h = hashlib.sha256()
        for sd in seeds:
            h.update(sd if isinstance(sd, bytes) else b58decode(sd))
        h.update(bytes([bump]))
        h.update(b58decode(program))
        h.update(b"ProgramDerivedAddress")
        d = h.digest()
        if not _on_curve(d):
            return b58encode(d)
    raise ValueError(f"no PDA for {seeds}")


def curve_of(mint):
    return pda([b"bonding-curve", mint], D.PUMP)


def canonical_pool_of(mint):
    authority = pda([b"pool-authority", mint], D.PUMP)
    return pda([b"pool", (0).to_bytes(2, "little"), authority, mint, D.WSOL], D.PAMM)


def sample_hit(key, rate):
    """A deterministic Bernoulli(``rate``) draw keyed by ``key`` (a signature or an address):
    the same transaction is sampled the same way on every run."""
    h = int.from_bytes(hashlib.sha256(key.encode()).digest()[:8], "big")
    return h < rate * 2**64


# ---- discovery parsers ------------------------------------------------------------------------


def parse_create(tx):
    """``[(mint, bonding_curve)]`` of the pump ``CreateEvent``s of a successful transaction."""
    out = []
    if tx is None or tx["meta"].get("err") is not None:
        return out
    for top in D.invocations(tx):
        for ix in top["inner"]:
            d = ix["data"]
            if ix["program"] != D.PUMP or d[:8] != D.EVENT_IX_TAG or d[8:16] != D_CREATE:
                continue
            body, o = d[16:], 0
            try:
                for _ in range(3):  # name, symbol, uri
                    o += 4 + struct.unpack_from("<I", body, o)[0]
                out.append((D.key_at(body, o), D.key_at(body, o + 32)))
            except struct.error:
                continue
    return out


def parse_migration(tx):
    """``[(pool, mint, quote_mint)]`` of the PumpSwap ``create_pool`` instructions of a
    successful migration transaction (accounts 0, 3, 4)."""
    out = []
    if tx is None or tx["meta"].get("err") is not None:
        return out
    for top in D.invocations(tx):
        for ix in [top] + top["inner"]:
            if ix["program"] == D.PAMM and ix["data"][:8] == PAMM_CREATE_POOL and len(ix["accounts"]) > 4:
                a = ix["accounts"]
                out.append((a[0], a[3], a[4]))
    return out


# ---- census -----------------------------------------------------------------------------------


def _b64(acc):
    return base64.b64decode(acc["account"]["data"][0]) if "account" in acc else base64.b64decode(acc["data"][0])


def _multiple(rpc, keys, data_slice=None):
    """``{key: bytes or None}`` for any number of keys (100 per call)."""
    out = {}
    keys = list(dict.fromkeys(keys))
    for i in range(0, len(keys), 100):
        chunk = keys[i:i + 100]
        _, vals = rpc.multiple(chunk, data_slice=data_slice)
        for k, v in zip(chunk, vals):
            out[k] = None if v is None else (v["owner"], base64.b64decode(v["data"][0]))
    return out


def sol_usd(rpc):
    """SOL/USD from the AMM v4 SOL/USDC pool's vaults net of ``need_take_pnl``."""
    st = _multiple(rpc, [SOL_USDC_V4])[SOL_USDC_V4][1]
    a = D.decode_amm_info(st)
    v = _multiple(rpc, [a["coin_vault"], a["pc_vault"]], data_slice=(64, 8))
    coin = struct.unpack("<Q", v[a["coin_vault"]][1])[0] - a["need_take_pnl_coin"]
    pc = struct.unpack("<Q", v[a["pc_vault"]][1])[0] - a["need_take_pnl_pc"]
    assert a["coin_mint"] == D.WSOL and a["pc_mint"] == D.USDC, a
    return (pc / 1e6) / (coin / 1e9), a


def _mint_ok(mints):
    return {k: "Missing" if v is None else D.mint_check(k, v[0], v[1]) for k, v in mints.items()}


def lp_age(rpc, lp_mint, now, max_pages=5):
    """Seconds since the oldest signature of ``lp_mint`` (created with its pool; touched only by
    deposits and withdrawals, so usually one page), or None if older than ``max_pages``."""
    before = None
    for _ in range(max_pages):
        page = rpc.signatures(lp_mint, before=before, endpoint="archive")
        if not page:
            return None
        if len(page) < 1000:
            bt = page[-1].get("blockTime")
            return None if bt is None else now - bt
        before = page[-1]["signature"]
    return None


def census_pamm(rpc, rpc_archive, log):
    """Canonical WSOL PumpSwap pools with their real SOL."""
    disc = b58encode(D.PAMM_POOL_DISC)
    res = rpc_archive.program_accounts(D.PAMM, [
        {"memcmp": {"offset": 0, "bytes": disc}}, {"memcmp": {"offset": 9, "bytes": "11"}},
        {"memcmp": {"offset": 75, "bytes": D.WSOL}}], data_slice=(43, 32 * 5))
    log(f"census pamm: {len(res)} index-0 WSOL pools")
    rows = []
    for acc in res:
        d = _b64(acc)
        rows.append({"pool": acc["pubkey"], "mint": D.key_at(d, 0), "tok_vault": D.key_at(d, 96),
                     "sol_vault": D.key_at(d, 128)})
    bal = _multiple(rpc, [r["sol_vault"] for r in rows], data_slice=(64, 8))
    out = []
    for r in rows:
        v = bal.get(r["sol_vault"])
        r["real_sol"] = struct.unpack("<Q", v[1])[0] if v and len(v[1]) == 8 else 0
        if r["real_sol"] >= C_MIN_REAL_SOL:
            out.append(r)
    log(f"census pamm: {len(out)} with >= {C_MIN_REAL_SOL / 1e9:g} SOL")
    full = _multiple(rpc, [r["pool"] for r in out])
    mints = _mint_ok(_multiple(rpc, [r["mint"] for r in out]))
    keep = []
    for r in out:
        st = full.get(r["pool"])
        if not st or st[0] != D.PAMM:
            continue
        p = D.decode_pamm_pool(st[1])
        r.update(venue="pump_swap", vqr=p["virtual_quote_reserves"], mayhem=p["is_mayhem_mode"],
                 lp_mint=p["lp_mint"], lp_supply=p["lp_supply"], mint_reject=mints.get(r["mint"]))
        if p["is_mayhem_mode"] or r["mint_reject"] or canonical_pool_of(r["mint"]) != r["pool"]:
            continue
        # Q19 / #103: real SOL only, the smaller of real and effective under a negative VQR.
        r["r_eff"] = min(r["real_sol"], r["real_sol"] + min(r["vqr"], 0))
        keep.append(r)
    log(f"census pamm: {len(keep)} canonical, non-mayhem, mint filter passed")
    return keep


def census_raydium(rpc, rpc_archive, venue, log):
    """AMM v4 or CPMM WSOL pools above the locked-depth floor."""
    if venue == "raydium_cpmm":
        program, authority, size = D.CPMM, CPMM_AUTHORITY, 637
        sides = [(168, 72, True), (200, 104, False)]  # (WSOL mint offset, its vault, sol first)
    else:
        program, authority, size = D.AMMV4, AMMV4_AUTHORITY, 752
        sides = [(400, 336, True), (432, 368, False)]
    vaults = rpc_archive.program_accounts(D.TOKEN, [
        {"dataSize": 165}, {"memcmp": {"offset": 0, "bytes": D.WSOL}},
        {"memcmp": {"offset": 32, "bytes": authority}}], data_slice=(64, 8))
    big = {}
    for acc in vaults:
        amt = struct.unpack("<Q", _b64(acc))[0]
        if amt >= POOL_MIN_REAL_SOL * LAMBDA_MIN_BPS // 10_000:  # locked <= real <= vault
            big[acc["pubkey"]] = amt
    log(f"census {venue}: {len(vaults)} WSOL vaults, {len(big)} could clear the floor")
    del vaults
    cands = []
    for mint_off, vault_off, sol_first in sides:
        res = rpc_archive.program_accounts(program, [
            {"dataSize": size}, {"memcmp": {"offset": mint_off, "bytes": D.WSOL}}],
            data_slice=(vault_off, 32))
        for acc in res:
            sol_vault = D.key_at(_b64(acc), 0)
            if sol_vault in big:
                cands.append({"pool": acc["pubkey"], "sol_vault": sol_vault, "sol_is_first": sol_first})
        del res
    log(f"census {venue}: {len(cands)} candidate pools")
    states = _multiple(rpc, [c["pool"] for c in cands])
    keep, need = [], []
    for c in cands:
        st = states.get(c["pool"])
        if not st or st[0] != program:
            continue
        if venue == "raydium_cpmm":
            p = D.decode_cpmm_pool(st[1])
            if p["status"] & CPMM_SWAP_DISABLED:
                continue
            ctr = D.cpmm_counters(p)
            sol_ctr, tok_ctr = (ctr[0], ctr[1]) if c["sol_is_first"] else (ctr[1], ctr[0])
            c["tok_vault"] = p["token_1_vault"] if c["sol_is_first"] else p["token_0_vault"]
            c.update(lp_counter=p["lp_supply"], lp_mint=p["lp_mint"], open_time=p["open_time"],
                     amm_config=p["amm_config"], sol_counter=sol_ctr, tok_counter=tok_ctr,
                     mint=p["token_1_mint"] if c["sol_is_first"] else p["token_0_mint"])
        else:
            a = D.decode_amm_info(st[1])
            if a["status"] not in AMMV4_EXECUTABLE:
                continue
            sol_ctr, tok_ctr = ((a["need_take_pnl_coin"], a["need_take_pnl_pc"]) if c["sol_is_first"]
                                else (a["need_take_pnl_pc"], a["need_take_pnl_coin"]))
            c["tok_vault"] = a["pc_vault"] if c["sol_is_first"] else a["coin_vault"]
            c.update(lp_counter=a["lp_amount"], lp_mint=a["lp_mint"], open_time=None,
                     sol_counter=sol_ctr, tok_counter=tok_ctr,
                     mint=a["pc_mint"] if c["sol_is_first"] else a["coin_mint"])
        c.update(venue=venue, real_sol=big[c["sol_vault"]] - c["sol_counter"])
        need.append(c)
    lps = _multiple(rpc, [c["lp_mint"] for c in need])
    mints = _mint_ok(_multiple(rpc, [c["mint"] for c in need]))
    for c in need:
        lp = lps.get(c["lp_mint"])
        supply = struct.unpack_from("<Q", lp[1], 36)[0] if lp and len(lp[1]) >= 44 else c["lp_counter"]
        lam = (c["lp_counter"] - supply) * 10_000 // c["lp_counter"] if c["lp_counter"] > supply else 0
        locked = lam * c["real_sol"] // 10_000
        c.update(lambda_bps=lam, locked_sol=locked, mint_reject=mints.get(c["mint"]))
        if c["mint_reject"] or locked * 10_000 // LAMBDA_MIN_BPS < POOL_MIN_REAL_SOL:
            continue
        # ARCH §4.7: R_eff = min(R, locked / λ_min) on Raydium.
        c["r_eff"] = min(c["real_sol"], locked * 10_000 // LAMBDA_MIN_BPS)
        keep.append(c)
    log(f"census {venue}: {len(keep)} above the locked-depth floor, mint filter passed")
    return keep


def census(rpc, rpc_archive, log=print, now=None, previous=None):
    """Run a full census; returns the census record (also the selection's input). Ages known
    from ``previous`` (an earlier census record) are carried forward instead of looked up."""
    now = int(now or time.time())
    known = {}
    for p in (previous or {}).get("pools", []):
        if p.get("age_s") is not None:
            known[p["pool"]] = p["age_s"] + now - previous["time"]
    usd, _ = sol_usd(rpc)
    log(f"census: SOL/USD {usd:.2f} (AMM v4 SOL/USDC)")
    pools = census_pamm(rpc, rpc_archive, log)
    for venue in ("raydium_cpmm", "raydium_v4"):
        pools += census_raydium(rpc, rpc_archive, venue, log)
    theta_b = D.THETA_B_USD / usd * 1e9
    for p in pools:
        if p["venue"] == "raydium_cpmm" and p.get("open_time"):
            p["age_s"] = max(now - p["open_time"], 0)
        elif p["pool"] in known:
            p["age_s"] = known[p["pool"]]
        elif 2 * p["r_eff"] >= theta_b:  # the age decides A / B (and C under 24 h) only here
            p["age_s"] = lp_age(rpc_archive, p["lp_mint"], now)
        else:
            p["age_s"] = None
        age = p["age_s"] if p["age_s"] is not None else D.AGE_A_S
        p["tier"] = D.tier(p["venue"], p["r_eff"], age, usd)
    counts = {}
    for p in pools:
        counts[(p["venue"], p["tier"])] = counts.get((p["venue"], p["tier"]), 0) + 1
    log("census tiers: " + ", ".join(f"{v}/{t} {n}" for (v, t), n in sorted(counts.items())))
    return {"time": now, "sol_usd": usd, "pools": pools}


def select(census_rec, current, seed, caps=CAPS):
    """Markets to follow from a census: every pool already followed that is still eligible, then
    every Tier A pool and a seeded random sample of Tier B and C (``r_eff ≥ C_MIN_REAL_SOL``) up
    to ``caps``. ``current``: ``{pool: tier}`` of the census-selected markets followed now.
    Returns ``[(pool record, reason)]``."""
    rng = random.Random(seed)
    by_tier = {"A": [], "B": [], "C": []}
    for p in census_rec["pools"]:
        if p["tier"] in by_tier and p["r_eff"] >= C_MIN_REAL_SOL:
            by_tier[p["tier"]].append(p)
    out = []
    for t, pools in by_tier.items():
        pools.sort(key=lambda p: p["pool"])
        kept = [p for p in pools if p["pool"] in current]
        rest = [p for p in pools if p["pool"] not in current]
        rng.shuffle(rest)
        room = max(caps[t] - len(kept), 0)
        out += [(p, "census-kept") for p in kept] + [(p, f"census-{t}") for p in rest[:room]]
    return out


# ---- CLI ----------------------------------------------------------------------------------------


def default_data():
    return os.path.expanduser("~/.cache/shortwin-work/gapcal-data")


def save_census(data, rec):
    day = time.strftime("%Y-%m-%d", time.gmtime(rec["time"]))
    path = os.path.join(data, "census", f"census-{day}-{rec['time']}.json.gz")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with gzip.open(path + ".tmp", "wt") as f:
        json.dump(rec, f, separators=(",", ":"))
    os.replace(path + ".tmp", path)
    return path


def latest_census(data, with_path=False):
    """The newest saved census (and its path), or None."""
    d = os.path.join(data, "census")
    files = sorted(f for f in os.listdir(d) if f.endswith(".json.gz")) if os.path.isdir(d) else []
    if not files:
        return (None, None) if with_path else None
    path = os.path.join(d, files[-1])
    with gzip.open(path, "rt") as f:
        rec = json.load(f)
    return (rec, path) if with_path else rec


def crawl_pid(data):
    """The pid of a live crawl in ``data`` (``crawl.pid``), or None."""
    try:
        with open(os.path.join(data, "crawl.pid")) as f:
            pid = int(f.read().strip() or 0)
        if pid <= 0:
            return None
        os.kill(pid, 0)
        return pid
    except PermissionError:  # alive, another user's
        return pid
    except (OSError, ValueError):
        return None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["census", "show"])
    ap.add_argument("--data", default=default_data())
    ap.add_argument("--rps", type=float, default=1.0)
    args = ap.parse_args()
    if args.cmd == "census":
        pid = crawl_pid(args.data)
        if pid:  # the crawl's own fg + bg already use the 3 req/s budget
            raise SystemExit(f"a crawl is running (pid {pid}): its budget is the whole 3 req/s; "
                             "stop it first (fetch.py stop), or let it run its own census")
        rpc = Rpc(PUBLICNODE, MAINNET_BETA, rps=args.rps, log=print)
        rec = census(rpc, rpc)
        print(save_census(args.data, rec), rpc.stats)
        return 0
    path = os.path.join(args.data, "universe.json")
    if not os.path.exists(path):
        print("no universe yet")
        return 1
    u = json.load(open(path))
    live = [m for m in u["markets"].values() if not m.get("ended")]
    by = {}
    for m in live:
        by[(m["venue"], m["tier"], m["reason"].split("-")[0])] = by.get((m["venue"], m["tier"], m["reason"].split("-")[0]), 0) + 1
    for k, n in sorted(by.items()):
        print(*k, n)
    print(f"{len(live)} live of {len(u['markets'])} markets")
    return 0


if __name__ == "__main__":
    sys.exit(main())
