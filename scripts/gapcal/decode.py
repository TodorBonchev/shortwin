"""Venue trade and account decoders of the gap-calibration crawler (WP-9): a Python port of the M8
indexer's decoders (``services/indexer/src/ingest/decode/{venue,logs}.rs``, INDEXER.md §7.2) and of
the byte-level readers the fixtures script and the keeper use, standard library only.

**Trades** (``decode_trades``) are read from a ``getTransaction(encoding=json)`` result, in chain
order, each with the pool's **post-trade reserves** (``sol_after`` / ``tok_after``) and, where the
event carries them, the pre-trade ones:

- pump ``TradeEvent`` (``emit_cpi!``, an inner instruction of pump): the curve's virtual (pricing)
  and real reserves after the trade;
- PumpSwap ``BuyEvent`` / ``SellEvent`` (``emit_cpi!`` of pAMM): the vault balances before the
  trade; after it, the LP fee stays and the protocol / creator fees leave, so ``quote_after =
  quote_before ± quote_amount_in_with_lp_fee`` (buy) / ``quote_amount_out_without_lp_fee`` (sell)
  equals the quote vault's balance after the trade exactly. The pricing quote reserve adds the
  pool's ``virtual_quote_reserves`` (an account field, not in the event: the crawler joins it);
- Raydium CPMM ``SwapEvent`` (``emit!``, only from the CPMM program): the pricing reserves before
  the trade (vaults minus the protocol, fund and creator fee counters). M8 adds the whole input;
  with the pool's ``AmmConfig`` rates the protocol and fund parts of the trade fee (and the
  creator fee) that go to the counters are taken out too (``exact``), so ``sol_after`` is the
  next swap's ``*_before`` to the lamport;
- Raydium AMM v4 ``ray_log`` (``SwapBaseIn`` 3 / ``SwapBaseOut`` 4): the reserves before the
  swap net of ``need_take_pnl``, attributed to the swap instruction it was logged under (its
  account 1 is the pool), so ``vault − need_take_pnl`` after the swap.

Orientation: ``sol_*`` is always the WSOL side (lamports), ``tok_*`` the token side (raw units);
``side`` is the trader's ("buy" = tokens out of the pool), as in M8.

**Accounts**: pump ``BondingCurve``, PumpSwap ``Pool``, CPMM ``PoolState`` / ``AmmConfig``, AMM v4
``AmmInfo``, SPL token accounts and mints (``mint_check``: SPEC §3's allowlist filter, the port of
``shortwin_venue_state::mint::check``). **Tiers**: ``tier`` is ECON §6.2 / ARCH §4.7's function on
the inputs the crawler can observe.
"""

import base64
import struct

from rpc import anchor_event_disc, b58decode, b58encode

PUMP = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PAMM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
CPMM = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C"
AMMV4 = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
ATA_PROGRAM = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
WSOL = "So11111111111111111111111111111111111111112"
WSOL_2022 = "9pan9bMn5HatX4EJdBwg9VgCa7Uz5HL8N1m5D3NdXejP"
SYSTEM = "11111111111111111111111111111111"
USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"

EVENT_IX_TAG = bytes.fromhex("e445a52e51cb9a1d")  # Anchor's emit_cpi! instruction tag
D_TRADE = anchor_event_disc("TradeEvent")
D_BUY = anchor_event_disc("BuyEvent")
D_SELL = anchor_event_disc("SellEvent")
D_SWAP = anchor_event_disc("SwapEvent")
assert D_SWAP.hex() == "40c6cde8260871e2"  # venue.rs CPMM_SWAP_EVENT
assert D_TRADE.hex() == "bddb7fd34ee661ee"  # dump-fixtures.py PUMP_TRADE_EVENT

CURVE_DISC = bytes.fromhex("17b7f83760d8ac60")
PAMM_POOL_DISC = bytes.fromhex("f19a6d0411b16dbc")
CPMM_POOL_DISC = bytes.fromhex("f7ede3f5d7c3de46")
CPMM_CONFIG_DISC = bytes.fromhex("daf42168cbcb2b6f")
AMMV4_SWAPS = (9, 11, 16, 17)  # SwapBaseIn, SwapBaseOut, SwapBaseInV2, SwapBaseOutV2
FEE_RATE_DENOMINATOR = 1_000_000  # CPMM rates are parts per million


def u64_at(d, off):
    return struct.unpack_from("<Q", d, off)[0]


def u16_at(d, off):
    return struct.unpack_from("<H", d, off)[0]


def key_at(d, off):
    return b58encode(d[off:off + 32])


# ---- transaction structure ------------------------------------------------------------------


def tx_keys(tx):
    """Static keys, then the lookup tables' writable and readonly keys (the json encoding's
    account index space)."""
    keys = list(tx["transaction"]["message"]["accountKeys"])
    la = tx["meta"].get("loadedAddresses") or {}
    return keys + la.get("writable", []) + la.get("readonly", [])


def invocations(tx):
    """Top-level instructions with their inner instructions (stack height ≥ 2), as dicts
    ``{program, accounts, data, stack_height}``; ``inner`` on the top-level ones."""
    keys = tx_keys(tx)
    msg = tx["transaction"]["message"]
    inner = {g["index"]: g["instructions"] for g in (tx["meta"].get("innerInstructions") or [])}

    def node(ix, h):
        return {"program": keys[ix["programIdIndex"]], "accounts": [keys[a] for a in ix["accounts"]],
                "data": b58decode(ix["data"]), "stack_height": h}

    out = []
    for k, ix in enumerate(msg["instructions"]):
        top = node(ix, 1)
        top["inner"] = [node(j, j.get("stackHeight") or 2) for j in inner.get(k, [])]
        out.append(top)
    return out


def parse_logs(tx, tops):
    """M8's log attribution (logs.rs): tracks ``invoke [n]`` / ``success`` / ``failed`` and
    matches each frame to its instruction node by program and stack height. Returns
    ``(entries, truncated)``; an entry is ``(k, j, program, line_no, kind, bytes)`` with ``kind``
    "data" ("Program data:") or "ray" ("Program log: ray_log:"), ``j`` the inner index or None
    for the top-level instruction."""
    entries, truncated = [], False
    top, cursor, stack = None, 0, []
    for line_no, line in enumerate(tx["meta"].get("logMessages") or []):
        if line == "Log truncated":
            truncated = True
            break
        parts = line.split(" ")
        if len(parts) == 4 and parts[0] == "Program" and parts[2] == "invoke":
            pid, depth = parts[1], int(parts[3].strip("[]"))
            if depth == 1:
                start = 0 if top is None else top + 1
                top = next((k for k in range(start, len(tops)) if tops[k]["program"] == pid), start)
                cursor, stack = 0, [(pid, None)]
            else:
                del stack[max(depth - 1, 0):]
                j = None
                if top is not None and top < len(tops):
                    inner = tops[top]["inner"]
                    j = next((i for i in range(cursor, len(inner))
                              if inner[i]["program"] == pid and inner[i]["stack_height"] == depth), None)
                    if j is not None:
                        cursor = j + 1
                stack.append((pid, j))
            continue
        if len(parts) >= 3 and parts[0] == "Program" and (parts[2] == "success" or parts[2].startswith("failed")):
            if stack and stack[-1][0] == parts[1]:
                stack.pop()
            continue
        if line.startswith("Program data: "):
            kind, payload = "data", line[len("Program data: "):]
        elif line.startswith("Program log: ray_log: "):
            kind, payload = "ray", line[len("Program log: ray_log: "):]
        else:
            continue
        if top is None or not stack:
            continue
        try:
            b = base64.b64decode(payload.strip(), validate=True)
        except (ValueError, base64.binascii.Error):
            continue
        entries.append((top, stack[-1][1], stack[-1][0], line_no, kind, b))
    return entries, truncated


# ---- event bodies (venue.rs) ----------------------------------------------------------------


def pump_trade(body):
    """pump ``TradeEvent`` body (after the discriminator), M8's ``pump_trade`` plus the real
    reserves. None if shorter than the known prefix."""
    if len(body) < 32 + 8 + 8 + 1 + 32 + 8 * 5 + 32 + 16 + 32 + 16:
        return None
    mint = key_at(body, 0)
    sol_amount, token_amount = u64_at(body, 32), u64_at(body, 40)
    is_buy = body[48] != 0
    user = key_at(body, 49)
    o = 81
    timestamp = struct.unpack_from("<q", body, o)[0]
    vsol, vtok, rsol, rtok = (u64_at(body, o + 8), u64_at(body, o + 16), u64_at(body, o + 24),
                              u64_at(body, o + 32))
    o += 40
    o += 32  # fee_recipient
    fee_bps, fee = u64_at(body, o), u64_at(body, o + 8)
    creator = key_at(body, o + 16)
    creator_fee = u64_at(body, o + 56)
    fees = fee + creator_fee
    trader_sol = sol_amount + fees if is_buy else max(sol_amount - fees, 0)
    return {"venue": "pump_curve", "mint": mint, "side": "buy" if is_buy else "sell",
            "token_amount": token_amount, "sol_amount": trader_sol, "user": user,
            "sol_after": vsol, "tok_after": vtok, "real_sol_after": rsol, "real_tok_after": rtok,
            "curve_sol_delta": sol_amount, "fee": fee, "fee_bps": fee_bps,
            "creator": creator, "creator_fee": creator_fee, "event_ts": timestamp}


def pamm_trade(buy, body, base_mint, quote_mint):
    """PumpSwap ``BuyEvent`` / ``SellEvent`` body, M8's ``pump_amm_trade``: ``base_mint`` /
    ``quote_mint`` from the emitting instruction's accounts 3 and 4; None unless one is WSOL."""
    if base_mint != WSOL and quote_mint != WSOL:
        return None
    if len(body) < 14 * 8 + 32 * 7 + 16:
        return None
    u = [u64_at(body, 8 * i) for i in range(14)]
    timestamp = struct.unpack_from("<q", body, 0)[0]
    base_amount, pool_base, pool_quote = u[1], u[5], u[6]
    lp_fee, protocol_fee, quote_net_lp, user_quote = u[9], u[11], u[12], u[13]
    pool = key_at(body, 112)
    user = key_at(body, 144)
    coin_creator = key_at(body, 112 + 32 * 6)
    coin_creator_fee = u64_at(body, 112 + 32 * 7 + 8)
    if buy:
        base_after, quote_after = max(pool_base - base_amount, 0), pool_quote + quote_net_lp
    else:
        base_after, quote_after = pool_base + base_amount, max(pool_quote - quote_net_lp, 0)
    base_is_sol = base_mint == WSOL
    if base_is_sol:  # a reversed pool: the trader's view swaps
        side = "sell" if buy else "buy"
        tok_amt, sol_amt, mint = user_quote, base_amount, quote_mint
        tok_b, sol_b, tok_a, sol_a = pool_quote, pool_base, quote_after, base_after
    else:
        side = "buy" if buy else "sell"
        tok_amt, sol_amt, mint = base_amount, user_quote, base_mint
        tok_b, sol_b, tok_a, sol_a = pool_base, pool_quote, base_after, quote_after
    return {"venue": "pump_swap", "pool": pool, "mint": mint, "side": side,
            "token_amount": tok_amt, "sol_amount": sol_amt, "user": user,
            "sol_before": sol_b, "tok_before": tok_b, "sol_after": sol_a, "tok_after": tok_a,
            "fee_lp": lp_fee, "fee_protocol": protocol_fee, "fee_creator": coin_creator_fee,
            "creator": coin_creator, "event_ts": timestamp}


def cpmm_swap(payload, rates=None):
    """Raydium CPMM ``SwapEvent`` (the whole "Program data:" payload). ``rates`` = the pool's
    ``AmmConfig`` ``(protocol_fee_rate, fund_fee_rate)``: with them ``sol_after`` / ``tok_after``
    are the exact pricing reserves after the swap (``exact`` True); without, M8's (the protocol
    and fund parts of the trade fee and an input-side creator fee still counted)."""
    if payload[:8] != D_SWAP or len(payload) < 8 + 32 + 8 * 6 + 1 + 64 + 16 + 1:
        return None
    b = payload[8:]
    pool = key_at(b, 0)
    in_before, out_before, in_amt, out_amt = u64_at(b, 32), u64_at(b, 40), u64_at(b, 48), u64_at(b, 56)
    o = 32 + 8 * 6 + 1
    in_mint, out_mint = key_at(b, o), key_at(b, o + 32)
    trade_fee, creator_fee = u64_at(b, o + 64), u64_at(b, o + 72)
    creator_on_input = b[o + 80] != 0
    buy = in_mint == WSOL
    if not buy and out_mint != WSOL:
        return None  # not a SOL pool
    in_after, out_after = in_before + in_amt, max(out_before - out_amt, 0)
    if rates is not None:
        protocol_fee = trade_fee * rates[0] // FEE_RATE_DENOMINATOR
        fund_fee = trade_fee * rates[1] // FEE_RATE_DENOMINATOR
        in_after -= protocol_fee + fund_fee
        if creator_on_input:
            in_after -= creator_fee
        else:
            out_after = max(out_after - creator_fee, 0)
    if buy:
        side, mint, tok_amt, sol_amt = "buy", out_mint, out_amt, in_amt
        sol_b, tok_b, sol_a, tok_a = in_before, out_before, in_after, out_after
    else:
        side, mint, tok_amt, sol_amt = "sell", in_mint, in_amt, out_amt
        sol_b, tok_b, sol_a, tok_a = out_before, in_before, out_after, in_after
    return {"venue": "raydium_cpmm", "pool": pool, "mint": mint, "side": side,
            "token_amount": tok_amt, "sol_amount": sol_amt, "sol_before": sol_b,
            "tok_before": tok_b, "sol_after": sol_a, "tok_after": tok_a, "trade_fee": trade_fee,
            "creator_fee": creator_fee, "creator_fee_on_input": creator_on_input,
            "exact": rates is not None}


def ray_log(b):
    """AMM v4 ``ray_log`` swap record (57 bytes: log type 3 / 4 + 7 u64), else None."""
    if len(b) != 57 or b[0] not in (3, 4):
        return None
    u = [u64_at(b, 1 + 8 * i) for i in range(7)]
    return {"log_type": b[0], "amount": u[0], "bound": u[1], "direction": u[2],
            "pool_coin": u[4], "pool_pc": u[5], "result": u[6]}


def ray_v4_trade(log, pool, sol_is_coin):
    """M8's ``ray_v4_trade``: reserves after the swap (net of ``need_take_pnl``)."""
    a_in, a_out = (log["amount"], log["result"]) if log["log_type"] == 3 else (log["result"], log["bound"])
    coin_in = log["direction"] == 2
    sol_in = coin_in == sol_is_coin
    side, tok_amt, sol_amt = ("buy", a_out, a_in) if sol_in else ("sell", a_in, a_out)
    if coin_in:
        coin_after, pc_after = log["pool_coin"] + a_in, max(log["pool_pc"] - a_out, 0)
    else:
        coin_after, pc_after = max(log["pool_coin"] - a_out, 0), log["pool_pc"] + a_in
    if sol_is_coin:
        sol_b, tok_b, sol_a, tok_a = log["pool_coin"], log["pool_pc"], coin_after, pc_after
    else:
        sol_b, tok_b, sol_a, tok_a = log["pool_pc"], log["pool_coin"], pc_after, coin_after
    return {"venue": "raydium_v4", "pool": pool, "mint": None, "side": side,
            "token_amount": tok_amt, "sol_amount": sol_amt, "sol_before": sol_b,
            "tok_before": tok_b, "sol_after": sol_a, "tok_after": tok_a}


# ---- whole transactions ---------------------------------------------------------------------


def decode_trades(tx, v4_orientation=None, cpmm_rates=None):
    """Every venue trade of a successful transaction, in chain order (by instruction; CPI events
    before log events, as M8 sorts them). ``v4_orientation``: ``{pool: sol_is_coin}`` for the AMM
    v4 pools to decode (others are skipped: a ``ray_log`` does not say which side is SOL);
    ``cpmm_rates``: ``{pool: (protocol_fee_rate, fund_fee_rate)}`` for exact CPMM reserves.
    Each trade gets ``ix`` (top-level index), ``seq`` and ``source``. Returns ``(trades, info)``,
    ``info`` = ``{"truncated": bool, "unattributed_ray_logs": n}``."""
    info = {"truncated": False, "unattributed_ray_logs": 0}
    if tx is None or tx["meta"].get("err") is not None:
        return [], info
    tops = invocations(tx)
    out = []
    # 1. CPI events (pump, pAMM): an inner instruction of the emitting program carrying the tag.
    for k, top in enumerate(tops):
        nodes = [top] + top["inner"]
        for n, ix in enumerate(nodes):
            d = ix["data"]
            if n == 0 or d[:8] != EVENT_IX_TAG or ix["program"] not in (PUMP, PAMM):
                continue
            disc, body = d[8:16], d[16:]
            t = None
            if ix["program"] == PUMP and disc == D_TRADE:
                t = pump_trade(body)
            elif ix["program"] == PAMM and disc in (D_BUY, D_SELL):
                parent = _parent(nodes, n)
                accs = parent["accounts"] if parent else []
                base_mint = accs[3] if len(accs) > 4 else None
                quote_mint = accs[4] if len(accs) > 4 else None
                t = pamm_trade(disc == D_BUY, body, base_mint, quote_mint)
            if t is not None:
                t.update(ix=k, seq=n, source="cpi")
                out.append(t)
    # 2. Log events (CPMM SwapEvent, AMM v4 ray_log).
    entries, info["truncated"] = parse_logs(tx, tops)
    for k, j, program, line_no, kind, b in entries:
        t = None
        if kind == "data" and program == CPMM and b[:8] == D_SWAP:
            pool = key_at(b, 8) if len(b) >= 40 else None
            t = cpmm_swap(b, (cpmm_rates or {}).get(pool))
        elif kind == "ray" and program == AMMV4:
            log = ray_log(b)
            if log is None:
                continue
            node = tops[k] if j is None else tops[k]["inner"][j] if j < len(tops[k]["inner"]) else None
            if node is None or len(node["accounts"]) < 2 or not node["data"] or node["data"][0] not in AMMV4_SWAPS:
                info["unattributed_ray_logs"] += 1
                continue
            pool = node["accounts"][1]
            if v4_orientation is None or pool not in v4_orientation:
                continue
            t = ray_v4_trade(log, pool, v4_orientation[pool])
        if t is not None:
            t.update(ix=k, seq=10_000 + line_no, source="log")
            out.append(t)
    out.sort(key=lambda t: (t["ix"], t["source"] == "log", t["seq"]))
    return out, info


def _parent(nodes, n):
    """The nearest preceding node one stack level up (M8's ``parent_of``)."""
    h = nodes[n]["stack_height"]
    for i in range(n - 1, -1, -1):
        if nodes[i]["stack_height"] + 1 == h:
            return nodes[i]
    return None


def token_balances(tx, which="post"):
    """``{token account: (mint, raw amount)}`` from ``pre`` / ``postTokenBalances``."""
    keys = tx_keys(tx)
    out = {}
    for e in tx["meta"].get(f"{which}TokenBalances") or []:
        out[keys[e["accountIndex"]]] = (e["mint"], int(e["uiTokenAmount"]["amount"]))
    return out


def lamport_balances(tx, which="post"):
    keys = tx_keys(tx)
    return dict(zip(keys, tx["meta"][f"{which}Balances"]))


# ---- accounts -------------------------------------------------------------------------------


def _pad(d, n):
    return d + bytes(max(0, n - len(d)))


def decode_curve(d):
    """pump ``BondingCurve`` (R-PUMP §2.1; missing trailing fields read as zero)."""
    p = _pad(d, 151)
    vt, vq, rt, rq, supply = struct.unpack_from("<QQQQQ", p, 8)
    return {"len": len(d), "virtual_token_reserves": vt, "virtual_quote_reserves": vq,
            "real_token_reserves": rt, "real_quote_reserves": rq, "token_total_supply": supply,
            "complete": p[48], "creator": key_at(p, 49), "is_mayhem_mode": p[81],
            "quote_mint": key_at(p, 83)}


def decode_pamm_pool(d):
    """PumpSwap ``Pool`` (R-VEN §1.2; dump-fixtures.py ``decode_pamm_pool``)."""
    p = _pad(d, 301)
    return {"len": len(d), "index": u16_at(p, 9), "creator": key_at(p, 11),
            "base_mint": key_at(p, 43), "quote_mint": key_at(p, 75), "lp_mint": key_at(p, 107),
            "base_vault": key_at(p, 139), "quote_vault": key_at(p, 171),
            "lp_supply": u64_at(p, 203), "coin_creator": key_at(p, 211),
            "is_mayhem_mode": p[243],
            "virtual_quote_reserves": int.from_bytes(p[245:261], "little", signed=True)}


def decode_cpmm_pool(d):
    """Raydium CPMM ``PoolState`` (R-RAY §2.4)."""
    return {"amm_config": key_at(d, 8), "token_0_vault": key_at(d, 72),
            "token_1_vault": key_at(d, 104), "lp_mint": key_at(d, 136),
            "token_0_mint": key_at(d, 168), "token_1_mint": key_at(d, 200),
            "token_0_program": key_at(d, 232), "token_1_program": key_at(d, 264),
            "status": d[329], "lp_supply": u64_at(d, 333),
            "protocol_fees_token_0": u64_at(d, 341), "protocol_fees_token_1": u64_at(d, 349),
            "fund_fees_token_0": u64_at(d, 357), "fund_fees_token_1": u64_at(d, 365),
            "open_time": u64_at(d, 373), "creator_fee_on": d[389], "enable_creator_fee": d[390],
            "creator_fees_token_0": u64_at(d, 397), "creator_fees_token_1": u64_at(d, 405)}


def cpmm_counters(p):
    """``(token 0, token 1)`` fee counters the CPMM pricing reserves exclude."""
    return (p["protocol_fees_token_0"] + p["fund_fees_token_0"] + p["creator_fees_token_0"],
            p["protocol_fees_token_1"] + p["fund_fees_token_1"] + p["creator_fees_token_1"])


def decode_cpmm_config(d):
    return {"index": u16_at(d, 10), "trade_fee_rate": u64_at(d, 12),
            "protocol_fee_rate": u64_at(d, 20), "fund_fee_rate": u64_at(d, 28),
            "creator_fee_rate": u64_at(d, 108)}


def decode_amm_info(d):
    """Raydium AMM v4 ``AmmInfo`` (R-RAY §2.1)."""
    return {"status": u64_at(d, 0), "swap_fee_numerator": u64_at(d, 176),
            "swap_fee_denominator": u64_at(d, 184), "need_take_pnl_coin": u64_at(d, 192),
            "need_take_pnl_pc": u64_at(d, 200), "pool_open_time": u64_at(d, 224),
            "coin_vault": key_at(d, 336), "pc_vault": key_at(d, 368),
            "coin_mint": key_at(d, 400), "pc_mint": key_at(d, 432), "lp_mint": key_at(d, 464),
            "lp_amount": u64_at(d, 720)}


def token_amount(d):
    """An SPL token account's ``amount`` (offset 64)."""
    return u64_at(d, 64)


MINT_LEN, MULTISIG_LEN, ACCOUNT_TYPE_OFFSET, TLV_OFFSET = 82, 355, 165, 166
ALLOWED_EXTENSIONS = frozenset({18, 19, 20, 21, 22, 23})  # metadata / group pointers and data


def mint_check(key, owner, d):
    """SPEC §3's allowlist filter on raw bytes (``shortwin_venue_state::mint::check``): None if
    the mint passes, else the reason (the program's error name without ``Mint``)."""
    legacy = owner == TOKEN
    if not legacy and owner != TOKEN_2022:
        return "UnsupportedProgram"
    if key in (WSOL, WSOL_2022):
        return "Native"
    if (legacy and len(d) != MINT_LEN) or len(d) < MINT_LEN or len(d) == MULTISIG_LEN:
        return "Malformed"
    tags = struct.unpack_from("<I", d, 0)[0], struct.unpack_from("<I", d, 46)[0]
    if any(t not in (0, 1) for t in tags) or d[45] != 1:
        return "Malformed"
    tlv = b""
    if len(d) > MINT_LEN:
        if any(d[MINT_LEN:ACCOUNT_TYPE_OFFSET]) or len(d) <= ACCOUNT_TYPE_OFFSET or d[ACCOUNT_TYPE_OFFSET] != 1:
            return "Malformed"
        tlv = d[TLV_OFFSET:]
    if tags[1] == 1:
        return "FreezeAuthority"
    at = 0
    while at < len(tlv):
        if at + 2 > len(tlv):
            return None
        t = u16_at(tlv, at)
        if t == 0:
            return None
        if t not in ALLOWED_EXTENSIONS:
            return f"DisallowedExtension({t})"
        if at + 4 > len(tlv):
            return "DisallowedExtension(None)"
        end = at + 4 + u16_at(tlv, at + 2)
        if end > len(tlv):
            return "DisallowedExtension(None)"
        at = end
    return None


# ---- tiers (ECON §6.2, ARCH §4.7) --------------------------------------------------------------

THETA_A_USD, THETA_B_USD = 500_000, 100_000  # ADOPTED Q8
AGE_C_S, AGE_A_S = 24 * 3600, 7 * 86400


def tier(venue, real_sol, age_s, sol_usd):
    """The tier of a market on ``venue`` with real SOL reserve ``real_sol`` (lamports; the
    crawler's stand-in for ``R_eff``, see the method notes), ``age_s`` since graduation or pool
    creation and SOL at ``sol_usd``. Pump curves are D."""
    if venue == "pump_curve":
        return "D"
    lam = 2 * real_sol
    theta_a = THETA_A_USD / sol_usd * 1e9
    theta_b = THETA_B_USD / sol_usd * 1e9
    if age_s < AGE_C_S or lam < theta_b:
        return "C"
    if lam >= theta_a and age_s >= AGE_A_S:
        return "A"
    return "B"
