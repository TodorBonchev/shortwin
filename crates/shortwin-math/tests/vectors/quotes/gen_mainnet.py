#!/usr/bin/env python3
"""Mainnet trade vectors for the quote track (ARCHITECTURE §8.1 "differential tests
against mainnet"; R-PUMP §4.1, R-VEN §2.4, R-RAY §3.4).

Extracts every exact-out buy and exact-in sale from raw `getTransaction` dumps, with the
pre-trade state the venue priced on and the amounts the venue itself reported:

* pump curve  `TradeEvent` (`buy`/`buy_v2`, `sell`/`sell_v2`) of SOL-quoted, non-mayhem
  coins: pre-state = event post-state -/+ the trade; the completing buy +
  `CompletePumpAmmMigrationEvent` pairs give the migrated pool (`migrated_pool`);
* PumpSwap    `BuyEvent` (`buy`) / `SellEvent` of WSOL-quoted, non-mayhem pools: pool reserves, VQR,
  base supply, the fee bps charged (a cashback coin reports its creator-fee slot as
  cashback), and the pool's fee inputs (canonical or not, mayhem, creator override) for
  `fees::pump_amm`;
* AMM v4      `ray_log` `SwapBaseIn`/`SwapBaseOut` (pool_coin/pool_pc = the reserves the
  math used) + each pool's `swap_fee_numerator/denominator`;
* CPMM        `SwapEvent` (`*_vault_before` = reserves net of the fee counters) + the
  pool's `AmmConfig` rates and creator-fee flags. Its `input_amount`/`output_amount` are
  the curve's own amounts, so pools of Token-2022 mints with a transfer fee (which
  short.win does not list) still test the swap arithmetic.

Exact-in buys (pump `buy_exact_*_in`, PumpSwap `buy_exact_quote_in`, the boost) have no
counterpart in the quote API and are counted, not extracted.

Sources (not committed; every case carries its signature, so it can be re-fetched):
the research passes' dumps under `--research` (`pumpfun/txs`, `venues/txcache`,
`raydium/tx_*`) and any `--extra` directories of further dumps:

    python3 gen_mainnet.py --research <scratchpad> [--extra <dir> ...] [--fetch]

Without `--research` it only prints a note and exits 0, leaving `mainnet.json` as
committed (CI's `vectors` job runs every generator without arguments).

`--fetch` refreshes `venue_state.json` (pool and fee-config state read from mainnet
RPC; committed). `mainnet.json` holds only values observed on chain, plus that state.
"""
import argparse
import base64
import collections
import glob
import json
import struct
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _solana import (AMM_V4, ANCHOR_EVENT_IX, CPMM, PAMM, PUMP, WSOL, Idl, attributed_logs,  # noqa: E402
                     b58d, b58e, dump, find_pda, instructions, load_tx, rpc)

HERE = Path(__file__).resolve().parent
CP_SWAP_EVENT = bytes.fromhex("40c6cde8260871e2")
PUMP_GLOBAL = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf"
PUMP_FEE_CONFIG = "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt"
PAMM_GLOBAL = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw"
PAMM_FEE_CONFIG = "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx"
DEFAULT_KEY = "11111111111111111111111111111111"
S = str  # every integer is written as a decimal string (tests/vectors/README.md)


def u64(b, o):
    return struct.unpack_from("<Q", b, o)[0]


def load_all(research, extras, skipped):
    files = (sorted(glob.glob(str(research / "pumpfun/txs/*.json")))
             + sorted(glob.glob(str(research / "venues/txcache/*.json")))
             + sorted(glob.glob(str(research / "raydium/tx_*.json"))))
    for e in extras:
        files += sorted(glob.glob(str(e / "**/*.json"), recursive=True))
    txs, seen = [], set()
    for path in files:
        t = load_tx(path)
        if t is None:
            skipped["failed or missing tx"] += 1
            continue
        sig = t["transaction"]["signatures"][0]
        if sig not in seen:
            seen.add(sig)
            txs.append(t)
    return sorted(txs, key=lambda t: t["transaction"]["signatures"][0])


def events(t, program, idl):
    for n, (prog, data, _) in enumerate(instructions(t)):
        if prog == program and data[:8] == ANCHOR_EVENT_IX:
            name, ev = idl.event(data)
            if name:
                yield n, name, ev


# ---------------------------------------------------------------------------- pump curve
def pump_cases(txs, idl, skipped):
    trades, migrations = [], []
    for t in txs:
        sig = t["transaction"]["signatures"][0]
        for n, name, ev in events(t, PUMP, idl):
            if name == "CompletePumpAmmMigrationEvent":
                migrations.append(dict(sig=sig, mint=ev["mint"], mint_amount=ev["mint_amount"],
                                       sol_amount=ev["sol_amount"], fee=ev["pool_migration_fee"]))
            if name != "TradeEvent":
                continue
            if ev["quote_mint"] not in (DEFAULT_KEY, WSOL):
                skipped["pump: non-SOL quote"] += 1
                continue
            if ev["mayhem_mode"]:
                # Rejected at market creation (ADOPTED Q6); the mayhem program sets these
                # curves' virtual reserves, and most of their trades do not price on the
                # reserves the event reports.
                skipped["pump: mayhem-mode curve (out of scope)"] += 1
                continue
            if ev["ix_name"] not in ("buy", "sell"):
                skipped[f"pump: exact-in {ev['ix_name']} (no API)"] += 1
                continue
            q, sol = ev["token_amount"], ev["sol_amount"]
            vt, vq = ev["virtual_token_reserves"], ev["virtual_sol_reserves"]
            rt, rq = ev["real_token_reserves"], ev["real_sol_reserves"]
            buy = ev["is_buy"]
            pre = (vt + q, vq - sol, rt + q, rq - sol) if buy else (vt - q, vq + sol, rt - q, rq + sol)
            # A cashback coin reports its creator-fee slot as cashback (same rate and cost).
            fee, cfee = ev["fee"], ev["creator_fee"] + (ev["cashback"] or 0)
            case = dict(
                sig=sig, event=n, ix=ev["ix_name"], mint=ev["mint"],
                creator_is_default=ev["creator"] == DEFAULT_KEY,
                side="buy" if buy else "sell", q=S(q),
                vt=S(pre[0]), vq=S(pre[1]), rt=S(pre[2]), rq=S(pre[3]),
                protocol_bps=S(ev["fee_basis_points"]),
                creator_bps=S(ev["creator_fee_basis_points"] + (ev["cashback_fee_basis_points"] or 0)),
                cashback=(ev["cashback"] or 0) > 0,
                sol_amount=S(sol), protocol_fee=S(fee), creator_fee=S(cfee),
                vt_after=S(vt), vq_after=S(vq),
            )
            if buy:
                case["cost"] = S(sol + fee + cfee)
            else:
                case["proceeds"] = S(sol - fee - cfee)
            trades.append(case)
    # The completing buy (rT -> 0) of each migrated mint: its pre-state gives the pool.
    completing = {c["mint"]: c for c in trades if c["side"] == "buy" and c["rt"] == c["q"]}
    migs = []
    for m in migrations:
        c = completing.get(m["mint"])
        if c is None:
            skipped["pump: migration without a (non-mayhem) completing buy"] += 1
            continue
        migs.append(dict(buy_sig=c["sig"], sig=m["sig"], mint=m["mint"], q=c["q"], vt=c["vt"],
                         vq=c["vq"], rt=c["rt"], rq=c["rq"], migration_fee=S(m["fee"]),
                         pool_tokens=S(m["mint_amount"]), migrated_sol=S(m["sol_amount"])))
    return trades, migs


# ------------------------------------------------------------------------------ PumpSwap
def decode_fee_config(b):
    """pump-fees `FeeConfig`: bump, admin, flat_fees, fee_tiers, stable_fee_tiers, ..."""
    o = 8 + 1 + 32
    flat = [S(v) for v in struct.unpack_from("<3Q", b, o)]
    o += 24
    n = struct.unpack_from("<I", b, o)[0]
    o += 4
    tiers = []
    for _ in range(n):
        th = int.from_bytes(b[o:o + 16], "little")
        lp, pr, cr = struct.unpack_from("<3Q", b, o + 16)
        tiers.append(dict(threshold=S(th), lp=S(lp), protocol=S(pr), creator=S(cr)))
        o += 40
    return dict(flat=flat, tiers=tiers)


def pamm_cases(txs, idl, state, skipped):
    out = []
    for t in txs:
        sig = t["transaction"]["signatures"][0]
        for n, name, ev in events(t, PAMM, idl):
            if name not in ("BuyEvent", "SellEvent"):
                continue
            if name == "BuyEvent" and ev["ix_name"] != "buy":
                skipped[f"pamm: exact-in {ev['ix_name']} (no API)"] += 1
                continue
            pool = state["pamm_pools"].get(ev["pool"])
            if pool is None or pool["quote_mint"] != WSOL:
                skipped["pamm: pool not WSOL-quoted (or closed)"] += 1
                continue
            if pool["mayhem"]:
                # Rejected by short.win (ADOPTED Q6); the mayhem program trades them fee-free.
                skipped["pamm: mayhem-mode pool (out of scope)"] += 1
                continue
            buy = name == "BuyEvent"
            case = dict(
                sig=sig, event=n, side="buy" if buy else "sell", pool=ev["pool"],
                canonical=pool["canonical"], mayhem=pool["mayhem"],
                pool_creator_fee_bps=S(pool["creator_fee_bps"]),
                q=S(ev["base_amount_out"] if buy else ev["base_amount_in"]),
                base=S(ev["pool_base_token_reserves"]), quote_real=S(ev["pool_quote_token_reserves"]),
                vqr=S(ev["virtual_quote_reserves"]), base_supply=S(ev["base_supply"]),
                coin_creator_is_default=ev["coin_creator"] == DEFAULT_KEY,
                lp_bps=S(ev["lp_fee_basis_points"]), protocol_bps=S(ev["protocol_fee_basis_points"]),
                creator_bps=S(ev["coin_creator_fee_basis_points"]),
                lp_fee=S(ev["lp_fee"]), protocol_fee=S(ev["protocol_fee"]),
                creator_fee=S(ev["coin_creator_fee"]),
                # Cashback coins pay the creator-fee slot to the trader's accumulator
                # instead (R-VEN §1.5): same rate, same cost, reported separately.
                cashback_bps=S(ev["cashback_fee_basis_points"]), cashback=S(ev["cashback"]),
            )
            if buy:
                case.update(net=S(ev["quote_amount_in"]), cost=S(ev["user_quote_amount_in"]))
            else:
                case.update(gross=S(ev["quote_amount_out"]), proceeds=S(ev["user_quote_amount_out"]))
            out.append(case)
    return out


# ------------------------------------------------------------------------------- Raydium
def v4_cases(txs, state, skipped):
    out = []
    for t in txs:
        sig = t["transaction"]["signatures"][0]
        pools = [acc[1] for prog, data, acc in instructions(t)
                 if prog == AMM_V4 and data and data[0] in (9, 11, 16, 17)]
        logs = []
        for prog, line in attributed_logs(t):
            if prog == AMM_V4 and line.startswith("Program log: ray_log: "):
                b = base64.b64decode(line.split("ray_log: ")[1])
                if b[0] in (3, 4):
                    logs.append(b)
        if len(pools) != len(logs):
            skipped["amm v4: swaps and ray_logs do not pair up"] += 1
            continue
        for n, (pool, b) in enumerate(zip(pools, logs)):
            kind, amount, limit, direction, _src, coin, pc, result = struct.unpack_from("<B7Q", b, 0)
            p = state["amm_v4"].get(pool)
            if p is None:
                skipped["amm v4: pool state unknown"] += 1
                continue
            # SwapDirection: 1 = PC2Coin (input pc), 2 = Coin2PC (input coin).
            x_in, y_out = (pc, coin) if direction == 1 else (coin, pc)
            in_mint = p["pc_mint"] if direction == 1 else p["coin_mint"]
            case = dict(sig=sig, swap=n, pool=pool, x_in=S(x_in), y_out=S(y_out),
                        num=S(p["num"]), den=S(p["den"]), sol_in=in_mint == WSOL)
            if kind == 3:
                case.update(kind="exact_in", amount_in=S(amount), amount_out=S(result))
            else:
                case.update(kind="exact_out", amount_out=S(limit), amount_in=S(result))
            out.append(case)
    return out


def cp_events(t):
    for prog, line in attributed_logs(t):
        if line.startswith("Program data: "):
            b = base64.b64decode(line.split("Program data: ")[1].split(" ")[0])
            if b[:8] == CP_SWAP_EVENT:
                yield prog, b


def cpmm_cases(txs, state, skipped):
    out = []
    for t in txs:
        sig = t["transaction"]["signatures"][0]
        n = 0
        for prog, b in cp_events(t):
            if prog != CPMM:
                skipped["cpmm: SwapEvent of another program"] += 1
                continue
            pool = b58e(b[8:40])
            ivb, ovb, ia, oa, itf, otf = struct.unpack_from("<6Q", b, 40)
            base_input = b[88] == 1
            in_mint, out_mint = b58e(b[89:121]), b58e(b[121:153])
            tf, cf = struct.unpack_from("<2Q", b, 153)
            cfi = b[169] == 1
            p = state["cpmm"].get(pool)
            if p is None:
                skipped["cpmm: pool state unknown"] += 1
                continue
            cfg = state["cpmm_configs"][p["amm_config"]]
            out.append(dict(
                sig=sig, swap=n, pool=pool, kind="exact_in" if base_input else "exact_out",
                x_in=S(ivb), y_out=S(ovb), amount_in=S(ia), amount_out=S(oa),
                trade_rate=S(cfg["trade"]), protocol_rate=S(cfg["protocol"]), fund_rate=S(cfg["fund"]),
                creator_rate=S(cfg["creator"]), enable_creator_fee=p["enable_creator_fee"],
                creator_fee_on=p["creator_fee_on"], sol_pool=WSOL in (in_mint, out_mint),
                sol_is_token0=p["token0_mint"] == WSOL, input_is_token0=p["token0_mint"] == in_mint,
                sol_in=in_mint == WSOL, creator_on_input=cfi, trade_fee=S(tf), creator_fee=S(cf),
                # Token-2022 transfer fees sit outside the curve: the event's amounts are
                # the curve's (input net of, output before, the transfer fee).
                input_transfer_fee=S(itf), output_transfer_fee=S(otf)))
            n += 1
    return out


# ----------------------------------------------------------------------- venue state (RPC)
def fetch_state(txs, pump_idl, pamm_idl):
    amm, cp, pamm, mints = set(), set(), set(), set()
    for t in txs:
        for prog, data, acc in instructions(t):
            if prog == AMM_V4 and data and data[0] in (9, 11, 16, 17):
                amm.add(acc[1])
        for prog, b in cp_events(t):
            if prog == CPMM:
                cp.add(b58e(b[8:40]))
        for _, name, ev in events(t, PAMM, pamm_idl):
            if name in ("BuyEvent", "SellEvent"):
                pamm.add(ev["pool"])
        for _, name, ev in events(t, PUMP, pump_idl):
            if name == "TradeEvent":
                mints.add(ev["mint"])

    slot = 0

    def accounts(keys):
        nonlocal slot
        res, keys = {}, sorted(keys)
        for i in range(0, len(keys), 100):
            r = rpc("getMultipleAccounts", [keys[i:i + 100], {"encoding": "base64"}])
            slot = max(slot, r["context"]["slot"])
            for k, v in zip(keys[i:i + 100], r["value"]):
                res[k] = base64.b64decode(v["data"][0]) if v else None
        return res

    state = {"amm_v4": {}, "cpmm": {}, "cpmm_configs": {}, "pamm_pools": {}, "pump_curves": {}}
    for k, b in accounts(amm).items():
        state["amm_v4"][k] = dict(num=u64(b, 176), den=u64(b, 184), coin_mint=b58e(b[400:432]),
                                  pc_mint=b58e(b[432:464]))
    for k, b in accounts(cp).items():
        state["cpmm"][k] = dict(amm_config=b58e(b[8:40]), token0_mint=b58e(b[168:200]),
                                creator_fee_on=b[389], enable_creator_fee=b[390] == 1)
    for k, b in accounts({p["amm_config"] for p in state["cpmm"].values()}).items():
        state["cpmm_configs"][k] = dict(trade=u64(b, 12), protocol=u64(b, 20), fund=u64(b, 28),
                                        creator=u64(b, 108))
    for k, b in accounts(pamm).items():
        if b is None:
            continue
        b = b + bytes(max(0, 301 - len(b)))  # older pools are shorter: missing fields read as 0
        index, creator, base_mint = struct.unpack_from("<H", b, 9)[0], b58e(b[11:43]), b58e(b[43:75])
        authority = find_pda([b"pool-authority", b58d(base_mint)], PUMP)
        state["pamm_pools"][k] = dict(
            base_mint=base_mint, quote_mint=b58e(b[75:107]), mayhem=b[243] == 1,
            canonical=index == 0 and creator == authority, creator_fee_bps=u64(b, 261))
    curves = {m: find_pda([b"bonding-curve", b58d(m)], PUMP) for m in mints}
    acc = accounts(set(curves.values()))
    for m, c in curves.items():
        b = acc.get(c)
        if b is not None:
            b = b + bytes(max(0, 123 - len(b)))  # shorter accounts read trailing fields as 0
            state["pump_curves"][m] = dict(creator_fee_bps=u64(b, 115))
    g = accounts({PUMP_GLOBAL, PUMP_FEE_CONFIG, PAMM_GLOBAL, PAMM_FEE_CONFIG})
    state["pump_fee_config"] = decode_fee_config(g[PUMP_FEE_CONFIG])
    state["pump_creator_fee_configurable"] = g[PUMP_GLOBAL][1045] == 1
    state["pamm_fee_config"] = decode_fee_config(g[PAMM_FEE_CONFIG])
    state["pamm_creator_fee_configurable"] = g[PAMM_GLOBAL][940] == 1
    state["slot"] = slot
    state["note"] = ("Pool and fee-config state read from mainnet at `slot`, after the sampled trades; "
                     "every case must still reproduce, which confirms it did not change in between.")
    return state


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--research", type=Path)
    ap.add_argument("--extra", type=Path, action="append", default=[])
    ap.add_argument("--fetch", action="store_true")
    a = ap.parse_args()
    if a.research is None:
        # CI's `vectors` job runs every gen_*.py without arguments. mainnet.json holds
        # observed trades, not sim output: without the raw dumps there is nothing to
        # regenerate, and the committed file stays as it is.
        print("gen_mainnet.py: no --research dumps given; mainnet.json left unchanged "
              "(extract with --research <dir> [--extra <dir> ...] [--fetch])")
        return
    skipped = collections.Counter()
    pump_idl = Idl(a.research / "pumpfun/repo/idl/pump.json")
    pamm_idl = Idl(a.research / "venues/ppd/idl/pump_amm.json")
    txs = load_all(a.research, a.extra, skipped)
    state_path = HERE / "venue_state.json"
    if a.fetch or not state_path.exists():
        dump(fetch_state(txs, pump_idl, pamm_idl), state_path)
    state = json.load(open(state_path))
    # The research's own FeeConfig snapshot (2026-09-26) must equal the live one.
    snap = json.load(open(a.research / "venues/feeconfig.json"))
    assert decode_fee_config(base64.b64decode(snap["data"])) == state["pamm_fee_config"]
    pump, migs = pump_cases(txs, pump_idl, skipped)
    for c in pump:
        c["curve_creator_fee_bps"] = S(state["pump_curves"].get(c["mint"], {}).get("creator_fee_bps", 0))
    out = dict(
        generator="gen_mainnet.py",
        source="mainnet getTransaction dumps (R-PUMP §4.1, R-VEN §2.4, R-RAY §3.4) and venue_state.json",
        pump_curve=pump, pump_migrations=migs,
        pump_fee_config=state["pump_fee_config"],
        pump_creator_fee_configurable=state["pump_creator_fee_configurable"],
        pumpswap=pamm_cases(txs, pamm_idl, state, skipped),
        pumpswap_fee_config=state["pamm_fee_config"],
        pumpswap_creator_fee_configurable=state["pamm_creator_fee_configurable"],
        ray_v4=v4_cases(txs, state, skipped), cpmm=cpmm_cases(txs, state, skipped),
        skipped={k: S(v) for k, v in sorted(skipped.items())},
    )
    dump(out, HERE / "mainnet.json")
    for k in ("pump_curve", "pump_migrations", "pumpswap", "ray_v4", "cpmm"):
        print(k, len(out[k]))
    for k, v in sorted(skipped.items()):
        print("skipped:", k, v)


if __name__ == "__main__":
    main()
