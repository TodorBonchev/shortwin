#!/usr/bin/env python3
"""Dump the Raydium (M3), pump (M5) and PumpSwap (M6) mainnet fixtures of the program tests
(ARCHITECTURE §1.2, §8.2).

Writes, under ``tests/fixtures/``:

- ``programs/raydium_amm_v4.so``, ``programs/raydium_cpmm.so``: the deployed ELFs (the
  ProgramData account after its 45-byte header, cut at the ELF's own end; every byte after it
  is zero on mainnet and is re-created by the loader in ``tests/litesvm/src/fork.rs``);
- ``accounts/raydium/*.bin``: the raw data of every account the Raydium tests load — the two
  program accounts and their ProgramData headers (T-LATCH), the Fartcoin AMM v4 pool and its
  reversed-order sibling, five CPMM pools (USELESS; a creator-fee pool with the fee on WSOL as
  token 0; a Token-2022 pool; a creator-fee pool with WSOL as token 1; a pool charging the
  creator fee on both tokens), their vaults, mints, LP mints and observations, both vault
  authorities, the AMM v4 ``AmmConfig`` and every CPMM ``AmmConfig``;
- ``programs/pump.so``, ``programs/pump_fees.so``, ``programs/pump_amm.so`` and
  ``accounts/pump/*.bin`` (M5; groups ``pump_programs`` and ``pump_state``): the pump,
  pump-fees and PumpSwap ELFs with their program accounts and ProgramData headers; pump's
  ``Global``, both pump-fees ``FeeConfig`` accounts, PumpSwap's ``GlobalConfig``, both event
  authorities and volume accumulators, pump's fee, buyback and withdraw authorities, PumpSwap's
  protocol-fee and buyback WSOL ATAs; and one set of accounts per pump coin **role** (``PUMP_COINS``:
  a fresh curve, a curve near completion, a 115-byte Token-2022 curve, legacy SPL-Token curves of
  115 and 49 bytes, a graduated coin with its canonical PumpSwap pool and boost vault, a
  mayhem-mode and a USDC-quoted curve that short.win must refuse, a 150-byte legacy curve and
  Fartcoin's graduated-elsewhere curve). Accounts that may legitimately not exist (a creator
  vault, a ``sharing-config``, a closed boost vault, the event authorities) are recorded as
  ``"absent": true`` entries without a file;
- ``accounts/pamm/*.bin`` (M6; group ``pamm_state``): PumpSwap's statics (``GlobalConfig``, its
  ``FeeConfig``, the global volume accumulator, WSOL, Rent, the 16 recipient WSOL ATAs) and one
  set of accounts per canonical-pool **role** (``PAMM_POOLS``: a fresh graduate with its boost
  running, a mature 301-byte pool, a cashback coin, pools of 211 and 261 bytes); group
  ``pamm_synthetic``: pools **derived offline** from ``pamm_state`` bytes (``PAMM_SYNTHETIC``: the
  mature pool with a negative ``virtual_quote_reserves``), each marked ``"synthetic": true`` with
  its ``derived_from`` source and the ``patch`` applied;
- M10 program groups (no test loads them yet; ``M10_PROGRAM_GROUPS``):
  ``raydium_cpmm_2026_10`` (``programs/raydium_cpmm_2026_10.so`` and
  ``accounts/raydium_cpmm_2026_10/*.bin``: CPMM as redeployed at slot 452,376,460; the pool and
  config **state** stays ``raydium_state``'s, whose layouts the redeploy kept, and
  ``raydium_programs`` keeps the 445,763,504 deploy for T-LATCH); ``spl_programs``
  (``programs/token_2022.so`` with ``accounts/spl/*.bin``: the deployed upgradeable Token-2022;
  ``programs/spl_memo_v3.so``: spl-memo v3 under BPFLoader2, whose account data is the ELF);
  ``squads_programs`` (``programs/squads_v4.so``, ``accounts/squads/*.bin``: Squads v4 and its
  ``ProgramConfig``; ``idl/squads_multisig_program.json``: the IDL 2.1.0 vendored from the source
  repository at a pinned commit, recorded in the group's snapshot under ``vendored``);
  ``pump_programs_2026_10`` (``programs/{pump,pump_fees,pump_amm}_2026_10.so`` and
  ``accounts/pump_2026_10/*.bin``: the three pump programs as redeployed at slots 452,654,932 /
  452,655,002 / 452,654,882; ELFs only, ``pump_state`` / ``pamm_state`` stay the state, whose
  layouts the redeploy kept apart from appended fee buckets, and ``pump_programs`` keeps the
  2026-09 deploys for T-LATCH); ``pump_programs_2026_10_13``
  (``programs/{pump,pump_amm}_2026_10_08.so``, ``programs/pump_fees_2026_10_12.so`` and
  ``accounts/pump_2026_10_13/*.bin``: the pump and PumpSwap redeploys of 2026-10-08 at slots
  454,596,459 / 454,596,406 and pump-fees' of 2026-10-12 at 456,132,171, the 2026-10-13 re-pin; the
  2026-10-02 group stays T-LATCH's "before"); ``pump_programs_2026_10_13b``
  (``programs/pump_2026_10_13.so`` and ``accounts/pump_2026_10_13b/*.bin``: pump alone as
  redeployed on 2026-10-13 at slot 456,494,698, the re-pin moved to it; PumpSwap and pump-fees
  stay ``pump_programs_2026_10_13``'s, and that group keeps pump's 2026-10-08 build). Each of them
  is pinned (deploy slot, trimmed-ELF or account sha256) and a fetch that reads anything else
  stops before it writes a file;
- ``accounts/pump_2026_10_13/*.bin`` (group ``pump_state_2026_10_13``): the state the 2026-10-08/12
  builds introduced, in one ``getMultipleAccounts`` call: pump's 1,088-byte ``Global``, both
  ``FeeConfig`` accounts and PumpSwap's ``GlobalConfig`` (re-dumped so the group is self-contained),
  and one set of accounts per coin role (``PUMP_COINS_2026_10_13``: 166-byte curves near completion,
  minutes old and carrying fee buckets, dormant 141- and 151-byte curves, a complete unmigrated
  and a migrated synthetic completion, a cashback and a mayhem curve, a WSOL pool carrying fee
  buckets);
  ``fetch --group pump_state_2026_10_13 --coin fresh166=latest`` takes the newest ``create_v2``;
- ``manifest.json``: one entry per account (address, owner, lamports, executable, rent epoch,
  space, **slot**, sha256, purpose, decoded fields) and one snapshot record per fetch;
- ``SHA256SUMS``: every fixture file (CI: ``cd tests/fixtures && sha256sum -c SHA256SUMS``).

All venue *state* accounts are read in **one** ``getMultipleAccounts`` call, so pools, vaults
and fee counters are one consistent mainnet state at one slot (pinned in the manifest).
Programs are fetched separately (their deploy slot is in the ProgramData header).
A fetch stages every file it writes and puts them on disk, with ``manifest.json`` and
``SHA256SUMS``, only once every group it fetched has passed its checks: a fetch that stops
leaves the committed fixtures untouched.

Usage (Python 3.14, standard library only)::

    scripts/dump-fixtures.py            # = check: offline; every file against the manifest,
                                        #   the ProgramData reconstructions, the cross-references
    scripts/dump-fixtures.py fetch      # fetch what is missing (idempotent: a complete, valid
                                        #   fixture set is left byte-for-byte unchanged)
    scripts/dump-fixtures.py fetch --refresh   # re-dump everything at a new slot
    scripts/dump-fixtures.py fetch --rpc URL   # default: api.mainnet-beta, then publicnode
    scripts/dump-fixtures.py fetch --refresh --group pump_state \
        --coin fresh=MINT --coin graduated=MINT     # re-dump one group; replace a coin role
    scripts/dump-fixtures.py candidates        # mainnet: newest creates, latest migrations,
                                               #   (--near) live curves nearest completion
    scripts/dump-fixtures.py fetch --refresh --group pamm_state --pool fresh=latest
                                        # re-dump the PumpSwap pools (the fresh role must be
                                        #   minutes old; pamm_synthetic is re-derived with it)
    scripts/dump-fixtures.py check --live      # mainnet, read-only: has any pinned program been
                                               #   redeployed? (exit 1 naming each drifted one)

``check --live`` reads the six ``VenueGuard`` programs, Token-2022, spl-memo and Squads (plus its
``ProgramConfig``) with read-only RPC calls (one ``getMultipleAccounts`` for the program
accounts, one for the 45-byte ProgramData headers, then one ``getAccountInfo`` per pinned ELF,
paced). It compares each against its **newest** fixture (deploy slot, upgrade authority, trimmed
ELF sha256; older groups of the same program, kept for T-LATCH, are listed but not compared) and
each ``VenueGuard`` program's deploy slot against the newest ``VENUE_GUARD_SLOTS_YYYY_MM_DD`` in
``programs/shortwin/src/constants.rs`` (the test baseline; Jupiter has no fixture, so its slot is
compared with that constant only). Run it before every freeze and merge.

The pump coin roles are pinned mints (``PUMP_COINS``). A curve moves on: at a refresh each role's
expectation is re-checked (e.g. "near" must still be live with ``rT`` at most ``NEAR_RT_MAX``,
"graduated" must have its canonical pool) and a role that no longer holds stops the fetch with a
message; ``candidates`` lists replacements and ``--coin role=MINT`` pins one.

Entries of the manifest this script does not manage (e.g. the Pyth dump) are kept verbatim.
A refresh replaces the pinned bytes: the reader unit tests
(``crates/shortwin-venue-state/src/{raydium_v4,raydium_cpmm,pump,pump_amm}.rs``) assert the
decoded values of this dump and move with it (the manifest's ``decoded`` fields list the new
ones).
Exit status: 0 ok, 1 a check failed (``check --live``: a program drifted); an RPC or consistency
error while fetching exits with its message.
"""

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import re
import struct
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "tests", "fixtures"))
MANIFEST = os.path.join(FIXTURES, "manifest.json")
SUMS = os.path.join(FIXTURES, "SHA256SUMS")
ACCOUNT_DIR = "accounts/raydium"
PUMP_ACCOUNT_DIR = "accounts/pump"
PROGRAM_DIR = "programs"

RPCS = ["https://api.mainnet-beta.solana.com", "https://solana-rpc.publicnode.com"]

# ---- constants (R-RAY §1.1, §2) ------------------------------------------------------------

LOADER_UPGRADEABLE = "BPFLoaderUpgradeab1e11111111111111111111111"
TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
WSOL = "So11111111111111111111111111111111111111112"
SYSTEM = "11111111111111111111111111111111"
AMM_V4 = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
CPMM = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C"
AMM_V4_AUTHORITY = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"
AMM_V4_CONFIG = "9DCxsMizn3H1hprZ7xWe6LDzeUeZBksYFpBWBtSf1PQX"
CPMM_AUTHORITY = "GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL"
CPMM_POOL_DISC = bytes.fromhex("f7ede3f5d7c3de46")
CPMM_CONFIG_DISC = bytes.fromhex("daf42168cbcb2b6f")
CPMM_OBSERVATION_LEN = 4075

# (name, program, ProgramData, sha256 of the ELF with trailing zeros trimmed per R-RAY §1.3,
# expected deploy slot or None)
PROGRAMS = [
    (
        "raydium_amm_v4",
        AMM_V4,
        "A7ZG7ByDi8DpzT9Ab7CiXhvgYTJQmaDPJkMDoPitaCQV",
        "d4cee0cd4f78f69d05f5d645fc6d120dff830f231162d35cf2b2dae1df7b2c9f",
        None,
    ),
    (
        "raydium_cpmm",
        CPMM,
        "DMawCQzbgNTmbzaESc7o6pvL1KAeetY8zA7jNpzntHhU",
        "bdaff73e7ed9fc75f091d732987d36476d7378d32c47495fe9eef0a930dbf8cc",
        None,
    ),
]

# (prefix, pool address, purpose). R-RAY §2.1, §2.4, §6.4, §12.3; ARCH §8.2.
V4_POOLS = [
    (
        "ray_v4_fartcoin",
        "Bzc9NZfMqkXR6fz1DBph7BDf9BroyEf6pnzESP7v5iiw",
        "Fartcoin's main AMM v4 pool (R-RAY §2.1, §6.4; ARCH §2.5 example): WSOL is the coin, "
        "swap fee 25/10,000, status 6; the M3 fork E2E pool and T-RAY-MATH's AMM v4 venue",
    ),
    (
        "ray_v4_fartcoin_rev",
        "9GWMrDgHzWqT5wLZSwKbsFud63q93vEuA5TE6Qt9txVs",
        "Fartcoin's tiny reversed-order AMM v4 pool (R-RAY §2.1; ARCH §8.2): WSOL is the pc, "
        "and need_take_pnl is non-zero on both sides, so reserves = vault - need_take_pnl bite",
    ),
]
CPMM_POOLS = [
    (
        "cpmm_useless",
        "Q2sPHPdUWFMg7M7wwrQKLrn619cAucfRsmhVJffodSp",
        "USELESS CPMM pool (R-RAY §2.4, §6.4): WSOL token 0, config 0 (0.25 %), creator fee "
        "off; the M3 fork E2E pool",
    ),
    (
        "cpmm_creator_fee",
        "FQd8z3Dy6NRxZgx152SUmYL9AyyqAbYCPtSmy4spEgnH",
        "creator-fee CPMM pool (R-RAY §2.4, §3.5 E): WSOL token 0, config 0, creator fee 0.05 % "
        "on token 0 (creator_fee_on 1) -> on the input of buys, on the output of sells; large "
        "uncollected creator counter",
    ),
    (
        "cpmm_token2022",
        "7ZRtFcgfxKcUfdACiWf9abqBs9mVy569VeDLdLxb7Ztx",
        "largest Token-2022 CPMM pool (R-RAY §2.4 `7ZRtFcgf…`): WSOL token 0 (SPL), token 1 "
        "under Token-2022, config 0, creator fee off",
    ),
    (
        "cpmm_wsol_token1",
        "3Cv9uPpaSqQD55oiiegpC5SBm9664kqbJoB5K7YBQfGb",
        "deepest creator-fee CPMM pool with WSOL as token 1 (creator_fee_on 2 = WSOL; config 10: "
        "0.50 % + 0.40 %): the reversed side order and a second config",
    ),
    (
        "cpmm_creator_both",
        "FrZnJAFt8HtvsvERXMLmXTfJTEkRbkwitTKTwS7SzRXf",
        "deepest CPMM pool with WSOL token 0 and creator_fee_on 0 (BothToken: the creator fee is "
        "always on the input), config 0",
    ),
]

# ---- base58 ---------------------------------------------------------------------------------

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58encode(b):
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


def b58decode(s):
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    body = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    return b"\0" * (len(s) - len(s.lstrip("1"))) + body


def key_at(d, off):
    return b58encode(d[off : off + 32])


def u64_at(d, off):
    return struct.unpack_from("<Q", d, off)[0]


def u16_at(d, off):
    return struct.unpack_from("<H", d, off)[0]


def sha256(b):
    return hashlib.sha256(b).hexdigest()


# ---- RPC ------------------------------------------------------------------------------------


class Rpc:
    def __init__(self, urls):
        self.urls = urls

    def call(self, method, params):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        last = None
        for url in self.urls:
            for attempt in range(6):
                req = urllib.request.Request(
                    url, data=body, headers={"Content-Type": "application/json"}
                )
                try:
                    with urllib.request.urlopen(req, timeout=120) as r:
                        out = json.loads(r.read())
                except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
                    last = e
                    time.sleep(2 * (attempt + 1))
                    continue
                err = out.get("error")
                if err and err.get("code") in (429, -32429, -32005):
                    last = err
                    time.sleep(3 * (attempt + 1))
                    continue
                if err:
                    raise SystemExit(f"rpc {method} on {url}: {err}")
                self.used = url
                return out["result"]
        raise SystemExit(f"rpc {method} failed on every endpoint: {last}")

    def multiple(self, keys, data_slice=None):
        cfg = {"encoding": "base64"}
        if data_slice:
            cfg["dataSlice"] = {"offset": data_slice[0], "length": data_slice[1]}
        res = self.call("getMultipleAccounts", [keys, cfg])
        return res["context"]["slot"], res["value"]


def decode_value(v):
    return {
        "owner": v["owner"],
        "lamports": v["lamports"],
        "executable": v["executable"],
        "rent_epoch": v["rentEpoch"],
        "data": base64.b64decode(v["data"][0]),
    }


# ---- decoders (for the manifest's "decoded" fields and the cross-checks) ---------------------


def decode_token_account(d):
    return {
        "mint": key_at(d, 0),
        "owner": key_at(d, 32),
        "amount": u64_at(d, 64),
        "delegate": key_at(d, 76) if struct.unpack_from("<I", d, 72)[0] else None,
        "state": d[108],
        "is_native": u64_at(d, 113) if struct.unpack_from("<I", d, 109)[0] else None,
    }


def tlv_types(d):
    """Token-2022 extension types of a mint (R-MINT §3.2)."""
    out = []
    if len(d) <= 166:
        return out
    off = 166
    while off + 4 <= len(d):
        t, n = struct.unpack_from("<HH", d, off)
        if t == 0:
            break
        out.append(t)
        off += 4 + n
    return out


def decode_mint(d, owner):
    out = {
        "program": owner,
        "supply": u64_at(d, 36),
        "decimals": d[44],
        "mint_authority": key_at(d, 4) if struct.unpack_from("<I", d, 0)[0] else None,
        "freeze_authority": key_at(d, 50) if struct.unpack_from("<I", d, 46)[0] else None,
    }
    if owner == TOKEN_2022:
        out["extension_types"] = tlv_types(d)
    return out


def decode_amm_info(d):
    return {
        "status": u64_at(d, 0),
        "nonce": u64_at(d, 8),
        "coin_decimals": u64_at(d, 32),
        "pc_decimals": u64_at(d, 40),
        "pnl_numerator": u64_at(d, 160),
        "pnl_denominator": u64_at(d, 168),
        "swap_fee_numerator": u64_at(d, 176),
        "swap_fee_denominator": u64_at(d, 184),
        "need_take_pnl_coin": u64_at(d, 192),
        "need_take_pnl_pc": u64_at(d, 200),
        "pool_open_time": u64_at(d, 224),
        "coin_vault": key_at(d, 336),
        "pc_vault": key_at(d, 368),
        "coin_vault_mint": key_at(d, 400),
        "pc_vault_mint": key_at(d, 432),
        "lp_mint": key_at(d, 464),
        "amm_owner": key_at(d, 688),
        "lp_amount": u64_at(d, 720),
    }


def decode_pool_state(d):
    return {
        "amm_config": key_at(d, 8),
        "pool_creator": key_at(d, 40),
        "token_0_vault": key_at(d, 72),
        "token_1_vault": key_at(d, 104),
        "lp_mint": key_at(d, 136),
        "token_0_mint": key_at(d, 168),
        "token_1_mint": key_at(d, 200),
        "token_0_program": key_at(d, 232),
        "token_1_program": key_at(d, 264),
        "observation_key": key_at(d, 296),
        "auth_bump": d[328],
        "status": d[329],
        "mint_0_decimals": d[331],
        "mint_1_decimals": d[332],
        "lp_supply": u64_at(d, 333),
        "protocol_fees_token_0": u64_at(d, 341),
        "protocol_fees_token_1": u64_at(d, 349),
        "fund_fees_token_0": u64_at(d, 357),
        "fund_fees_token_1": u64_at(d, 365),
        "open_time": u64_at(d, 373),
        "recent_epoch": u64_at(d, 381),
        "creator_fee_on": d[389],
        "enable_creator_fee": d[390],
        "creator_fees_token_0": u64_at(d, 397),
        "creator_fees_token_1": u64_at(d, 405),
    }


def decode_amm_config(d):
    return {
        "bump": d[8],
        "disable_create_pool": d[9],
        "index": u16_at(d, 10),
        "trade_fee_rate": u64_at(d, 12),
        "protocol_fee_rate": u64_at(d, 20),
        "fund_fee_rate": u64_at(d, 28),
        "create_pool_fee": u64_at(d, 36),
        "creator_fee_rate": u64_at(d, 108),
    }


def elf_len(elf):
    """The ELF file's own length: the furthest byte any header, section or segment names."""
    if elf[:4] != b"\x7fELF" or elf[4] != 2 or elf[5] != 1:
        raise SystemExit("ProgramData does not hold a 64-bit little-endian ELF")
    (e_phoff, e_shoff) = struct.unpack_from("<QQ", elf, 0x20)
    (e_phentsize, e_phnum, e_shentsize, e_shnum) = struct.unpack_from("<HHHH", elf, 0x36)
    end = max(64, e_phoff + e_phnum * e_phentsize, e_shoff + e_shnum * e_shentsize)
    for i in range(e_phnum):
        o = e_phoff + i * e_phentsize
        p_offset, _, _, p_filesz = struct.unpack_from("<QQQQ", elf, o + 8)
        end = max(end, p_offset + p_filesz)
    for i in range(e_shnum):
        o = e_shoff + i * e_shentsize
        sh_type = struct.unpack_from("<I", elf, o + 4)[0]
        sh_offset, sh_size = struct.unpack_from("<QQ", elf, o + 24)
        if sh_type != 8:  # SHT_NOBITS occupies no file bytes
            end = max(end, sh_offset + sh_size)
    return end


# ---- pump (M5; R-PUMP §1–§3, §7; R-VEN §1–§2; ARCH §3.12, §4.1, §8.2) ------------------------

PUMP = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMP_AMM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
PUMP_FEES = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ"
ATA_PROGRAM = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
PUMP_GLOBAL = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf"
PUMP_FEE_CONFIG = "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt"
PUMP_EVENT_AUTHORITY = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1"
PUMP_GVA = "Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y"
PAMM_GLOBAL_CONFIG = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw"
PAMM_FEE_CONFIG = "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx"
PAMM_EVENT_AUTHORITY = "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR"
PAMM_GVA = "C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw"
RENT_SYSVAR = "SysvarRent111111111111111111111111111111111"
CURVE_DISC = bytes.fromhex("17b7f83760d8ac60")
PUMP_GLOBAL_DISC = bytes.fromhex("a7e8e8b1c86c727f")
FEE_CONFIG_DISC = bytes.fromhex("8f3492bbdb7b4c9b")
PAMM_POOL_DISC = bytes.fromhex("f19a6d0411b16dbc")
PAMM_GLOBAL_CONFIG_DISC = bytes.fromhex("95089ccaa0fcb0d9")
PUMP_MIGRATE_V2 = bytes.fromhex("bbcb121fceedfe29")
PUMP_EVENT_IX = bytes.fromhex("e445a52e51cb9a1d")
PUMP_CREATE_EVENT = bytes.fromhex("1b72a94ddeeb6376")
PUMP_TRADE_EVENT = bytes.fromhex("bddb7fd34ee661ee")

# The three pump programs; the deploy slots are ARCH §8.2's (= VENUE_GUARD_SLOTS_2026_09_27).
PUMP_PROGRAMS = [
    ("pump", PUMP, "B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu", None, 449_734_335),
    ("pump_fees", PUMP_FEES, "75Uu23mqWBb8LM8vDppqC1mQAnCcBuLXhVaDezVMQLRw", None, 446_465_969),
    ("pump_amm", PUMP_AMM, "6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc", None, 446_462_733),
]

# "Near completion": at most this many raw tokens left on the curve (100 M of the 793.1 M a
# standard curve sells), so a buy of a few % of the supply straddles the graduation.
NEAR_RT_MAX = 100_000_000_000_000

# (role, mint, expectation, purpose). The expectation is re-checked on every fetch (a curve
# moves on: replace the role with `--coin role=MINT`, see `candidates`).
PUMP_COINS = [
    (
        "fresh",
        "4mNDC2YAJRWsnbC1mS4KkiLbUorVwLJ7PBsJX7Vzpump",
        "live_sol",
        "a fresh curve (early): a Token-2022 create_v2 coin (125 bytes) minutes old; the curve "
        "adapter's default T-PUMP-MATH / fork venue",
    ),
    (
        "near",
        "E5Xyf3YypbrKX3UqSXN3kQkEaNTYD8GnL4tbye4xpump",
        "near",
        "a curve near completion (rT <= NEAR_RT_MAX): buys beyond rT (6021), the completing buy, "
        "B^_str and the straddle (ARCH §4.3, §5.4)",
    ),
    (
        "t22_115",
        "4bfGcxadHUJ9r1G3LHxGooKoMy29QR5nQhUJnnAYpump",
        "live_sol_115",
        "a live Token-2022 curve of 115 bytes (the length buy_v2/sell_v2 top a shorter curve up "
        "to; 1.8 M live curves have it)",
    ),
    (
        "legacy115",
        "8FxewF272LJ7Usov6PZk2gu1JKoR5Dq1v4sDqavJpump",
        "live_sol_legacy_115",
        "a live legacy SPL-Token (Tokenkeg) curve of 115 bytes without a creator (creator fee 0)",
    ),
    (
        "legacy49",
        "7XVFCatyjXN2g42Tiwq4PWTA2nWu7wvbBdnBw9Hipump",
        "live_sol_legacy_49",
        "a live legacy SPL-Token curve in the 2024 layout (49 bytes, created at slot "
        "300,000,011): every later field reads as zero; the first v2 trade grows it to 125",
    ),
    (
        "graduated",
        "7UfcTJD2RSRg9AJ4k2V2rs71Ew8Zy2avaU5roc8bpump",
        "graduated_pumpswap",
        "a completed, migrated curve with its canonical PumpSwap pool (vaults, LP mint, coin "
        "creator vault) and its boost vault, dumped minutes after migrate_v2",
    ),
    (
        "mayhem",
        "ABuH3Mh1Zds4b877ADY45M6NVc92C5qRvmNt5AEzpump",
        "mayhem",
        "a live mayhem-mode curve (2 B supply): short.win must refuse it (ADOPTED Q6, "
        "CurveMayhemUnsupported)",
    ),
    (
        "usdc",
        "C2c6VZ3VYeRA86x8gcDB1RMBpa9Ke5c65yFfkSbRSA4a",
        "usdc",
        "a live USDC-quoted curve (125 bytes): short.win must refuse it (ADOPTED Q6, "
        "CurveQuoteNotSol)",
    ),
    (
        "legacy150",
        "Coyj3LtKn1BNSgWc9HsGK5SKoGfEoDaymig4wrN6pump",
        "legacy_150",
        "a legacy SPL-Token coin whose curve was extended to 150 bytes (R-PUMP §2.1, §5)",
    ),
    (
        "fartcoin",
        "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump",
        "graduated_elsewhere",
        "Fartcoin: complete, every reserve 0, no canonical PumpSwap pool: graduated to Raydium "
        "before PumpSwap (ARCH §2.5 GraduatedElsewhere, [V3])",
    ),
]

# ---- ed25519 PDAs (the ATA and program-derived addresses the fixtures must match) ------------

_P = 2**255 - 19
_D = (-121665 * pow(121666, _P - 2, _P)) % _P


def _on_curve(b):
    """Whether 32 bytes decode to an ed25519 point (RFC 8032 §5.1.3)."""
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
    """`find_program_address(seeds, program)` -> base58 address."""
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
    raise SystemExit(f"no PDA for {seeds} under {program}")


def ata(owner, mint, token_program):
    return pda([owner, token_program, mint], ATA_PROGRAM)


def curve_of(mint):
    return pda([b"bonding-curve", mint], PUMP)


def canonical_pool_of(mint, quote=WSOL):
    authority = pda([b"pool-authority", mint], PUMP)
    return pda([b"pool", (0).to_bytes(2, "little"), authority, mint, quote], PUMP_AMM), authority


# ---- pump decoders ------------------------------------------------------------------------------


def _pad(d, n):
    return d + bytes(max(0, n - len(d)))


def decode_curve(d):
    """`BondingCurve` (R-PUMP §2.1; missing trailing fields read as zero, as pump reads them).
    A curve longer than 125 bytes also decodes the fields pump appended since (IDL
    `types.BondingCurve`): the fee buckets @125 / @133 (2026-10-02) and `depth` @141,
    `initial_virtual_quote_reserves` @142, `post_complete_base_out` @150 and
    `post_complete_quote_in` @158 (2026-10-08, 166 B)."""
    p = _pad(d, 166)
    vt, vq, rt, rq, supply = struct.unpack_from("<QQQQQ", p, 8)
    out = {
        "len": len(d),
        "virtual_token_reserves": vt,
        "virtual_quote_reserves": vq,
        "real_token_reserves": rt,
        "real_quote_reserves": rq,
        "token_total_supply": supply,
        "complete": p[48],
        "creator": key_at(p, 49),
        "is_mayhem_mode": p[81],
        "is_cashback_coin": p[82],
        "quote_mint": key_at(p, 83),
        "creator_fee_bps": u64_at(p, 115),
        "can_edit_creator_fee": p[123],
        "is_holder_reward": p[124],
    }
    if len(d) > 125:
        out.update({
            "creator_fee_bucket": u64_at(p, 125),
            "protocol_fees_bucket": u64_at(p, 133),
            "depth": p[141],
            "initial_virtual_quote_reserves": u64_at(p, 142),
            "post_complete_base_out": u64_at(p, 150),
            "post_complete_quote_in": u64_at(p, 158),
        })
    return out


def _keys(d, off, n):
    return [key_at(d, off + 32 * i) for i in range(n)]


def decode_pump_global(d):
    """pump `Global` (R-PUMP §2.2; IDL offsets; `max_curve_depth` @1087 since the 2026-10-08
    redeploy, 1,088 B)."""
    extra = {"max_curve_depth": d[1087]} if len(d) > 1087 else {}
    return {
        "authority": key_at(d, 9),
        "fee_recipients": [key_at(d, 41)] + _keys(d, 162, 7),
        "initial_virtual_token_reserves": u64_at(d, 73),
        "initial_virtual_sol_reserves": u64_at(d, 81),
        "initial_real_token_reserves": u64_at(d, 89),
        "token_total_supply": u64_at(d, 97),
        "fee_basis_points": u64_at(d, 105),
        "withdraw_authority": key_at(d, 113),
        "pool_migration_fee": u64_at(d, 146),
        "creator_fee_basis_points": u64_at(d, 154),
        "reserved_fee_recipients": [key_at(d, 483)] + _keys(d, 516, 7),
        "buyback_fee_recipients": _keys(d, 741, 8),
        "buyback_basis_points": u64_at(d, 997),
        "whitelisted_quote_mint": key_at(d, 1013),
        "creator_fee_configurable": d[1045],
        "max_configurable_creator_fee_bps": u64_at(d, 1046),
    } | extra


def _tiers(d, off):
    n = struct.unpack_from("<I", d, off)[0]
    out, off = [], off + 4
    for _ in range(n):
        lo, hi = struct.unpack_from("<QQ", d, off)
        lp, pr, cr = struct.unpack_from("<QQQ", d, off + 16)
        out.append({"threshold": lo | hi << 64, "lp": lp, "protocol": pr, "creator": cr})
        off += 40
    return out, off


def decode_fee_config(d):
    """pump-fees `FeeConfig` (R-PUMP §2.3; R-VEN §2.3)."""
    tiers, off = _tiers(d, 65)
    stable, off = _tiers(d, off)
    fees = lambda o: dict(zip(("lp", "protocol", "creator"), struct.unpack_from("<QQQ", d, o)))
    return {
        "bump": d[8],
        "admin": key_at(d, 9),
        "flat_fees": fees(41),
        "fee_tiers": tiers,
        "stable_fee_tiers": stable,
        "exotic_flat_fees": fees(off),
    }


def decode_pamm_pool(d):
    """PumpSwap `Pool` (R-VEN §1.2; the fee buckets @271 / @279 of the 2026-10-02 redeploy on a
    pool of at least 287 bytes)."""
    p = _pad(d, 301)
    extra = ({"protocol_fees_bucket": u64_at(p, 271), "creator_fees_bucket": u64_at(p, 279)}
             if len(d) >= 287 else {})
    return {
        "len": len(d),
        "pool_bump": p[8],
        "index": u16_at(p, 9),
        "creator": key_at(p, 11),
        "base_mint": key_at(p, 43),
        "quote_mint": key_at(p, 75),
        "lp_mint": key_at(p, 107),
        "pool_base_token_account": key_at(p, 139),
        "pool_quote_token_account": key_at(p, 171),
        "lp_supply": u64_at(p, 203),
        "coin_creator": key_at(p, 211),
        "is_mayhem_mode": p[243],
        "is_cashback_coin": p[244],
        "virtual_quote_reserves": int.from_bytes(p[245:261], "little", signed=True),
        "creator_fee_bps": u64_at(p, 261),
        "can_edit_creator_fee": p[269],
        "is_holder_reward": p[270],
    } | extra


def decode_global_config(d):
    """PumpSwap `GlobalConfig` (R-VEN §2.2–§2.3; IDL offsets)."""
    return {
        "admin": key_at(d, 8),
        "lp_fee_basis_points": u64_at(d, 40),
        "protocol_fee_basis_points": u64_at(d, 48),
        "disable_flags": d[56],
        "protocol_fee_recipients": _keys(d, 57, 8),
        "coin_creator_fee_basis_points": u64_at(d, 313),
        "reserved_fee_recipients": [key_at(d, 385)] + _keys(d, 418, 7),
        "buyback_fee_recipients": _keys(d, 643, 8),
        "buyback_basis_points": u64_at(d, 899),
        "boost_authority": key_at(d, 907),
        "boost_enabled": d[939],
        "creator_fee_configurable": d[940],
        "max_configurable_creator_fee_bps": u64_at(d, 941),
    }


def decode_pump_account(owner, d):
    """The manifest's `decoded` fields of a pump-group account, or None."""
    if owner == PUMP and d[:8] == CURVE_DISC:
        return decode_curve(d)
    if owner == PUMP and d[:8] == PUMP_GLOBAL_DISC:
        return decode_pump_global(d)
    if owner == PUMP_FEES and d[:8] == FEE_CONFIG_DISC:
        return decode_fee_config(d)
    if owner == PUMP_AMM and d[:8] == PAMM_POOL_DISC:
        return decode_pamm_pool(d)
    if owner == PUMP_AMM and d[:8] == PAMM_GLOBAL_CONFIG_DISC:
        return decode_global_config(d)
    if owner == "Sysvar1111111111111111111111111111111111111" and len(d) == 17:
        per_byte_year, threshold = struct.unpack_from("<Qd", d, 0)
        return {"lamports_per_byte_year": per_byte_year, "exemption_threshold": threshold,
                "burn_percent": d[16],
                "minimum_balance_0": int((128 * per_byte_year) * threshold)}
    if (owner in (TOKEN, TOKEN_2022) and len(d) == 82) or (
        owner == TOKEN_2022 and len(d) > 165 and d[165] == 1
    ):
        return decode_mint(d, owner)
    if owner in (TOKEN, TOKEN_2022) and len(d) >= 165:
        return decode_token_account(d)
    return None


# ---- pump roles -------------------------------------------------------------------------------


def role_problem(expect, curve, mint_owner, pool_valid):
    """Why a coin no longer fits its role (None if it does). `curve` is `decode_curve`'s dict."""
    live = not curve["complete"]
    sol = curve["quote_mint"] == SYSTEM
    normal = sol and not curve["is_mayhem_mode"]
    checks = {
        "live_sol": live and normal and mint_owner == TOKEN_2022 and curve["len"] >= 115,
        "near": live and normal and curve["len"] >= 115
        and curve["real_token_reserves"] <= NEAR_RT_MAX,
        "live_sol_115": live and normal and curve["len"] == 115 and mint_owner == TOKEN_2022,
        "live_sol_legacy_115": live and normal and curve["len"] == 115 and mint_owner == TOKEN,
        "live_sol_legacy_49": live and normal and curve["len"] == 49 and mint_owner == TOKEN,
        "graduated_pumpswap": curve["complete"] and curve["virtual_token_reserves"] == 0
        and normal and pool_valid,
        "mayhem": curve["is_mayhem_mode"] == 1 and live,
        "usdc": curve["quote_mint"] == USDC and live,
        "legacy_150": curve["len"] == 150 and mint_owner == TOKEN,
        "graduated_elsewhere": curve["complete"] and curve["virtual_token_reserves"] == 0
        and not pool_valid,
    }
    if expect not in checks:
        return f"unknown expectation {expect}"
    return None if checks[expect] else f"no longer `{expect}`: {curve}"


def pool_valid_for(mint, pool_addr, owner, d):
    """ARCH §2.5 `canonical_pool_valid` (the pool sits at the canonical address)."""
    if owner != PUMP_AMM or d is None or d[:8] != PAMM_POOL_DISC:
        return False
    p = decode_pamm_pool(d)
    authority = pda([b"pool-authority", mint], PUMP)
    return (p["index"] == 0 and p["creator"] == authority and p["base_mint"] == mint
            and p["quote_mint"] == WSOL and not p["is_mayhem_mode"])


def fetch_pump_state(rpc, coins):
    """Every pump venue-state account in one getMultipleAccounts call (one slot)."""
    # 1. What the derived keys depend on: Global / GlobalConfig (recipient lists), the mints
    #    (token program), the curves (creator), the canonical pools (vaults, coin creator).
    roots = [PUMP_GLOBAL, PAMM_GLOBAL_CONFIG]
    for _, mint, _, _ in coins:
        roots += [mint, curve_of(mint), canonical_pool_of(mint)[0]]
    _, vals = rpc.multiple(roots)
    if vals[0] is None or vals[1] is None:
        raise SystemExit("pump Global or PumpSwap GlobalConfig missing")
    g = decode_pump_global(decode_value(vals[0])["data"])
    gc = decode_global_config(decode_value(vals[1])["data"])
    derived = {}
    for i, (role, mint, expect, _) in enumerate(coins):
        vm, vc, vp = vals[2 + 3 * i: 5 + 3 * i]
        if vm is None or vc is None:
            raise SystemExit(f"{role}: mint {mint} or its curve does not exist")
        c = decode_curve(decode_value(vc)["data"])
        pool_d = decode_value(vp)["data"] if vp else None
        valid = pool_valid_for(mint, canonical_pool_of(mint)[0], vp and vp["owner"], pool_d)
        why = role_problem(expect, c, vm["owner"], valid)
        if why:
            raise SystemExit(f"role {role} ({mint}): {why}; pick another (`candidates`) and pass "
                             f"--coin {role}=MINT")
        derived[role] = {"token_program": vm["owner"], "creator": c["creator"],
                         "pool": decode_pamm_pool(pool_d) if valid else None}

    # 2. The account list, with names and purposes (first name wins for shared accounts).
    plan, seen = [], {}

    def add(name, addr, purpose, role=None, may_be_absent=False):
        if addr in seen:
            return
        seen[addr] = name
        plan.append((name, addr, purpose, role, may_be_absent))

    add("pump_global", PUMP_GLOBAL, "pump Global (R-PUMP §2.2): fee / buyback / reserved recipient "
        "lists, pool_migration_fee, creator_fee_configurable")
    add("pump_fee_config", PUMP_FEE_CONFIG, "pump-fees FeeConfig of pump (R-PUMP §2.3): the curve's "
        "market-cap fee tiers, read by pump through get_fees_with_quote_mint on every trade")
    add("pump_event_authority", PUMP_EVENT_AUTHORITY, "pump's emit_cpi authority "
        "PDA(pump, [\"__event_authority\"])", may_be_absent=True)
    add("pump_global_volume_accumulator", PUMP_GVA, "pump GlobalVolumeAccumulator (buy_v2 account 19)")
    add("pump_withdraw_authority", g["withdraw_authority"], "Global.withdraw_authority "
        "(migrate_v2 account 1, writable, not a signer)", may_be_absent=True)
    for i, k in enumerate(g["fee_recipients"]):
        add(f"pump_fee_recipient_{i}", k, f"pump normal fee recipient #{i} (Global list; a trade's "
            "fee_recipient must be one, R-PUMP §3.1)", may_be_absent=True)
    for i, k in enumerate(g["buyback_fee_recipients"]):
        add(f"pump_buyback_fee_recipient_{i}", k, f"pump buyback fee recipient #{i} (Global list, "
            "error 6057 otherwise)", may_be_absent=True)
    add("pump_mint_wsol", WSOL, "WSOL: every SOL coin's `quote_mint` account in the v2 "
        "instructions, the PumpSwap pools' quote")
    add("pump_mint_usdc", USDC, "USDC: the quote of the USDC coin role")
    add("sysvar_rent", RENT_SYSVAR, "the Rent sysvar at the snapshot: pump tops a creator vault "
        "up to Rent.minimum_balance(0) and creates UVAs at Rent.minimum_balance(137); fork.rs "
        "installs it so venue-side rent is mainnet's (T-RENT)")
    add("pamm_global_config", PAMM_GLOBAL_CONFIG, "PumpSwap GlobalConfig (R-VEN §2.2–§2.3): "
        "disable_flags, protocol / buyback recipient lists, creator_fee_configurable")
    add("pamm_fee_config", PAMM_FEE_CONFIG, "pump-fees FeeConfig of PumpSwap (R-VEN §2.3): the "
        "canonical pools' market-cap tiers")
    add("pamm_event_authority", PAMM_EVENT_AUTHORITY, "PumpSwap's emit_cpi authority",
        may_be_absent=True)
    add("pamm_global_volume_accumulator", PAMM_GVA, "PumpSwap GlobalVolumeAccumulator (buy)")
    for i, k in enumerate(gc["protocol_fee_recipients"]):
        add(f"pamm_protocol_fee_recipient_{i}_wsol_ata", ata(k, WSOL, TOKEN),
            f"WSOL ATA of PumpSwap protocol fee recipient #{i} {k} (must exist, ARCH §3.12 T10)",
            may_be_absent=True)
    for i, k in enumerate(gc["buyback_fee_recipients"]):
        add(f"pamm_buyback_fee_recipient_{i}_wsol_ata", ata(k, WSOL, TOKEN),
            f"WSOL ATA of PumpSwap buyback fee recipient #{i} {k} (must exist, ARCH §3.12 T17)",
            may_be_absent=True)
    for role, mint, expect, purpose in coins:
        dv = derived[role]
        tp, curve = dv["token_program"], curve_of(mint)
        pre = f"pump_{role}"
        add(f"{pre}_mint", mint, f"mint of the `{role}` coin: {purpose}", role)
        add(f"{pre}_curve", curve, f"BondingCurve of the `{role}` coin: {purpose}", role)
        add(f"{pre}_curve_base_ata", ata(curve, mint, tp), f"the `{role}` curve's token ATA "
            "(associated_bonding_curve)", role, may_be_absent=expect == "graduated_elsewhere")
        if expect not in ("graduated_elsewhere", "legacy_150"):
            add(f"{pre}_creator_vault", pda([b"creator-vault", dv["creator"]], PUMP),
                f"pump creator vault of the `{role}` curve's creator", role, may_be_absent=True)
            sharing = pda([b"sharing-config", mint], PUMP_FEES)
            if dv["creator"] == sharing:
                # Only a coin whose creator fee is shared names its sharing config as the
                # creator (R-PUMP §2.4); otherwise pump only checks it is empty.
                add(f"{pre}_sharing_config", sharing, f"pump-fees sharing-config of the `{role}` "
                    "mint (the curve's creator)", role, may_be_absent=True)
        if expect == "usdc":
            add(f"{pre}_curve_quote_ata", ata(curve, USDC, TOKEN), "the USDC curve's quote ATA",
                role)
        if dv["pool"]:
            pool, authority = canonical_pool_of(mint)
            p = dv["pool"]
            boost = pda([b"boost_vault", pool], PUMP_AMM)
            ccva = pda([b"creator_vault", p["coin_creator"]], PUMP_AMM)
            add(f"{pre}_pool", pool, f"the `{role}` coin's canonical PumpSwap Pool", role)
            add(f"{pre}_pool_base_vault", p["pool_base_token_account"], "the pool's base vault", role)
            add(f"{pre}_pool_quote_vault", p["pool_quote_token_account"], "the pool's WSOL vault",
                role)
            add(f"{pre}_lp_mint", p["lp_mint"], "the pool's LP mint", role)
            add(f"{pre}_pool_authority", authority, "pump PDA [\"pool-authority\", mint] (the "
                "pool's creator)", role, may_be_absent=True)
            add(f"{pre}_boost_vault", boost, "PumpSwap PDA [\"boost_vault\", pool]", role,
                may_be_absent=True)
            add(f"{pre}_boost_vault_ata", ata(boost, WSOL, TOKEN), "the boost vault's WSOL ATA "
                "(ARCH §3.12 P5; the boost gate, A13): absent once the boost has run", role,
                may_be_absent=True)
            add(f"{pre}_coin_creator_vault_authority", ccva, "PumpSwap PDA [\"creator_vault\", "
                "pool.coin_creator]", role, may_be_absent=True)
            add(f"{pre}_coin_creator_vault_ata", ata(ccva, WSOL, TOKEN), "its WSOL ATA (T11)", role,
                may_be_absent=True)
    if len(plan) > 100:
        raise SystemExit(f"{len(plan)} accounts do not fit one getMultipleAccounts call")

    # 3. One call, one slot.
    slot, vals = rpc.multiple([a for _, a, _, _, _ in plan])
    block_time = None
    try:
        block_time = rpc.call("getBlockTime", [slot])
    except SystemExit as e:
        print(f"WARNING: getBlockTime({slot}): {e}", file=sys.stderr)
    entries, files = [], []
    by_addr = {}
    for (name, addr, purpose, role, may_be_absent), v in zip(plan, vals):
        e = {"name": name, "group": "pump_state"}
        if role:
            e["role"] = role
        e.update({"purpose": purpose, "address": addr, "slot": slot})
        if v is None:
            if not may_be_absent:
                raise SystemExit(f"{name} {addr} does not exist at slot {slot}")
            e["absent"] = True
            entries.append(e)
            continue
        a = decode_value(v)
        d = a["data"]
        rel = f"{PUMP_ACCOUNT_DIR}/{name}.bin"
        write_file(rel, d)
        files.append(rel)
        e.update({
            "owner": a["owner"],
            "lamports": a["lamports"],
            "executable": a["executable"],
            "rent_epoch": a["rent_epoch"],
            "space": len(d),
            "sha256": sha256(d),
            "file": rel,
        })
        decoded = decode_pump_account(a["owner"], d)
        if decoded is not None:
            e["decoded"] = decoded
        entries.append(e)
        by_addr[addr] = (a["owner"], d)
    # 4. The dumped state must still fit every role and name what step 1 derived.
    for role, mint, expect, _ in coins:
        owner, cd = by_addr[curve_of(mint)]
        c = decode_curve(cd)
        pool, _ = canonical_pool_of(mint)
        po, pd_ = by_addr.get(pool, (None, None))
        why = role_problem(expect, c, by_addr[mint][0], pool_valid_for(mint, pool, po, pd_))
        if why:
            raise SystemExit(f"role {role} changed between the two reads: {why}")
        if c["creator"] != derived[role]["creator"]:
            raise SystemExit(f"role {role}: the curve's creator changed between the two reads")
    snapshot = {
        "group": "pump_state",
        "slot": slot,
        "block_time": block_time,
        "block_time_utc": dt.datetime.fromtimestamp(block_time, dt.UTC).strftime(
            "%Y-%m-%dT%H:%M:%SZ") if block_time else None,
        "fetched_at": now_utc(),
        "rpc": rpc.used,
        "method": "one getMultipleAccounts call (base64) over every account below (absent ones "
        "recorded as such), so all of them are the mainnet state at `slot`; a first call read "
        "Global, GlobalConfig, the mints, curves and canonical pools to derive the rest",
        "coins": {role: mint for role, mint, _, _ in coins},
    }
    return entries, files, snapshot


def check_pump(m):
    """Offline cross-checks of the pump groups; returns a list of problems."""
    problems = []
    entries = [e for e in m["accounts"] if e.get("group") in PUMP_GROUPS]
    state = [e for e in entries if e["group"] == "pump_state"]
    by_addr = {e["address"]: e for e in state}
    by_name = {e["name"]: e for e in entries}
    slots = {e["slot"] for e in state}
    if len(slots) > 1:
        problems.append(f"pump_state spans several slots: {sorted(slots)}")
    for name, program, _, _, slot in PUMP_PROGRAMS:
        pd_ = by_name.get(f"{name}_program_data")
        if pd_ is None:
            problems.append(f"{name}: no ProgramData fixture")
        elif pd_["decoded"]["deploy_slot"] != slot:
            problems.append(f"{name}: deploy slot {pd_['decoded']['deploy_slot']} != {slot}")
    for k in (PUMP_GLOBAL, PUMP_FEE_CONFIG, PAMM_GLOBAL_CONFIG, PAMM_FEE_CONFIG, PUMP_GVA, PAMM_GVA,
              RENT_SYSVAR):
        if k not in by_addr or by_addr[k].get("absent"):
            problems.append(f"static pump account {k} is not a fixture")
    snap = next((sn for sn in m.get("snapshots", []) if sn["group"] == "pump_state"), None)
    coins = snap["coins"] if snap else {}
    expect_of = {r: x for r, _, x, _ in PUMP_COINS}
    pinned = {r: mint for r, mint, _, _ in PUMP_COINS}
    if coins != pinned:
        problems.append(f"pump_state was dumped with coins {coins}, but PUMP_COINS pins {pinned}: "
                        "pin the dumped mints (a `--coin` override is for the fetch only)")
    for role, mint in coins.items():
        me, ce = by_addr.get(mint), by_addr.get(curve_of(mint))
        if not me or not ce or me.get("absent") or ce.get("absent"):
            problems.append(f"{role}: mint or curve missing")
            continue
        c = decode_curve(read_file(ce["file"]))
        pool, _ = canonical_pool_of(mint)
        pe = by_addr.get(pool)
        pvalid = bool(pe and not pe.get("absent")
                      and pool_valid_for(mint, pool, pe["owner"], read_file(pe["file"])))
        why = role_problem(expect_of.get(role, "?"), c, me["owner"], pvalid)
        if why:
            problems.append(f"{role}: {why}")
        base = by_addr.get(ata(curve_of(mint), mint, me["owner"]))
        if base and not base.get("absent"):
            dec = base.get("decoded", {})
            if dec.get("mint") != mint or dec.get("owner") != curve_of(mint):
                problems.append(f"{role}: curve base ATA mint/owner mismatch")
        elif expect_of.get(role) != "graduated_elsewhere":
            problems.append(f"{role}: no curve base ATA fixture")
        if pvalid:
            p = decode_pamm_pool(read_file(pe["file"]))
            for k, mk in (("pool_base_token_account", "base_mint"),
                          ("pool_quote_token_account", "quote_mint")):
                v = by_addr.get(p[k], {}).get("decoded", {})
                if v.get("mint") != p[mk] or v.get("owner") != pool:
                    problems.append(f"{role}: pool vault {k} mint/owner mismatch")
    return problems


# ---- the state of the 2026-10-08/12 redeploys (group pump_state_2026_10_13) ---------------------
#
# One getMultipleAccounts call over the statics the new builds changed or every new role reads —
# pump's `Global` as the 2026-10-08 redeploy extended it (1,088 B, `max_curve_depth` @1087), pump's
# `FeeConfig`, PumpSwap's `GlobalConfig` and `FeeConfig` (re-dumped so the group is
# self-contained; the snapshot records how each compares with `pump_state`'s bytes) — and one set
# of accounts per coin **role** (`PUMP_COINS_2026_10_13`): 166-byte curves (near completion with
# fee buckets, minutes old, carrying large buckets), dormant 141- and 151-byte curves the next
# trade grows to 166, a complete unmigrated synthetic completion, a migrated one with its pool, a
# cashback and a mayhem curve, and a WSOL pool carrying fee buckets. `pump_state` stays the state
# of the older suites and T-LATCH; the harness resolves an address both groups hold to this one.

PUMP_STATE_2026_10_13 = "pump_state_2026_10_13"
# `vQ − rQ` (`initial_virtual_quote_reserves`) of a SOL `create_v2` curve.
IVQR_SOL = 30_000_000_000

# The group's statics: (manifest name, address, purpose).
PUMP_STATICS_2026_10_13 = [
    ("pump_global_1088", PUMP_GLOBAL, "pump Global as the 2026-10-08 redeploy extended it "
     "(1,088 B): bytes 0..1087 in `pump_state`'s layout, @1087 `max_curve_depth` (u8); the reader "
     "and every curve test (role `global_1088`)"),
    ("pump_fee_config_2026_10_13", PUMP_FEE_CONFIG, "pump-fees FeeConfig of pump (the curve's "
     "market-cap tiers), which v3 trades read directly (no pump-fees CPI)"),
    ("pump_pamm_global_config_2026_10_13", PAMM_GLOBAL_CONFIG, "PumpSwap GlobalConfig "
     "(disable_flags, recipient lists)"),
    ("pump_pamm_fee_config_2026_10_13", PAMM_FEE_CONFIG, "pump-fees FeeConfig of PumpSwap (the "
     "canonical pools' market-cap tiers)"),
]

# (role, mint, expectation, purpose); re-checked on every fetch (`role_problem_2026_10_13`).
PUMP_COINS_2026_10_13 = [
    (
        "near166",
        "CZN9Ysi89cAvnkuR2cNCM5t32BNHNdnXTJ5HPd4Npump",
        "near166",
        "a live SOL, non-cashback, non-mayhem 166-byte curve near completion (rT <= NEAR_RT_MAX) "
        "carrying both fee buckets: v3 synthetic exits past rT, the differential (T-PUMP-MATH-V3 "
        "(b))",
    ),
    (
        "fresh166",
        "8aUT1att91Rjurcvm4z8xu1yjUuEazRVux4eSq2gpump",
        "fresh166",
        "a create_v2 curve minutes old (166 bytes, Token-2022, ivqr 30e9 = vQ - rQ): the default "
        "venue on the 2026-10-08 build",
    ),
    (
        "buckets166",
        "FfEVFW3nZspBefEDKk1Un5UJyBiB9oCpoRJ5ffA9pump",
        "buckets166",
        "a live 166-byte SOL curve carrying large fee buckets (creator_fee @125, protocol_fees "
        "@133): bucket handling",
    ),
    (
        "grow141",
        "E414Tuyx8D6UnwgU16XeKDQyHF6UfL4o6NFuGocNpump",
        "grow141",
        "a dormant live SOL curve of 141 bytes (last traded on the 2026-10-02 build): the next "
        "trade grows it to 166",
    ),
    (
        "grow151",
        "3VBb7Ej86VxKVJEBSpRKsNe7gVTo9tRZza4yqpYVpump",
        "grow151",
        "a dormant live SOL curve of 151 bytes: the next trade grows it to 166",
    ),
    (
        "synthetic_stuck",
        "9kTYjcB2KhTNF3MjWjp4vgeCEjNiEojDEc8et1zRqigJ",
        "synthetic_stuck",
        "curve E1bYRCNq...: complete and unmigrated after a synthetic completion, depth 1 "
        "(quoted in a pump coin), post_complete_base_out / quote_in set: a reader vector; "
        "short.win refuses it (CurveQuoteNotSol)",
    ),
    (
        "graduated_synthetic",
        "98u8RJzuhezFwkCFVdSe1bPoQ6e2DdRG79kQ19vkpump",
        "graduated_synthetic",
        "98u8RJ's curve 2ncbKy3K... (completed by a synthetic buy, tx 2vVRr9eg...) and its "
        "canonical pool D4PACvyr... migrate_v2 created at (X1, Y1): the pc* fields kept after "
        "migration",
    ),
    (
        "cashback166",
        "4eegvKYhyJ2Uq6moore2f21JrDponqYdqBfrfG9Rpump",
        "cashback166",
        "curve 8oe1sfhK...: a live 166-byte SOL cashback coin, which v3 trades refuse (6094): "
        "T-LISTING (owner D1)",
    ),
    (
        "mayhem166",
        "8GoErkdxCRuscAYHbpGhLFVZQkG4tekiAwUW5F21pump",
        "mayhem166",
        "curve 5jQXNPKb...: a live 166-byte mayhem-mode curve whose vQ - rQ differs from its ivqr: "
        "the guard and the listing refusal",
    ),
    (
        "wsol_pool_buckets",
        "GbMfpnmsavwc5uHCqhbuFrSLdtggJiAxub7UKQ2hpump",
        "wsol_pool_buckets",
        "pool ERtQ5Uhn...: a canonical WSOL PumpSwap pool carrying both fee buckets (@271, @279): "
        "PUMP-2026-10-1 live",
    ),
]


def quote_offset_consistent(c):
    """pump's `vQ − rQ == initial_virtual_quote_reserves` (vacuous when `ivqr` or `vT` is 0)."""
    ivqr = c.get("initial_virtual_quote_reserves", 0)
    return (ivqr == 0 or c["virtual_token_reserves"] == 0
            or c["virtual_quote_reserves"] - c["real_quote_reserves"] == ivqr)


def role_problem_2026_10_13(expect, c, mint_owner, pool):
    """Why a coin no longer fits its `pump_state_2026_10_13` role (None if it does). `c` is
    `decode_curve`'s dict, `pool` the decoded canonical pool when it is valid (else None)."""
    live = not c["complete"]
    sol = c["quote_mint"] == SYSTEM
    normal = sol and not c["is_mayhem_mode"] and not c["is_cashback_coin"]
    n = c["len"]
    g = lambda k: c.get(k, 0)  # noqa: E731
    buckets = g("creator_fee_bucket") > 0 and g("protocol_fees_bucket") > 0
    pc = g("post_complete_base_out") > 0 and g("post_complete_quote_in") > 0
    migrated = c["complete"] and c["virtual_token_reserves"] == 0
    checks = {
        "near166": live and normal and n == 166 and g("depth") == 0
        and c["real_token_reserves"] <= NEAR_RT_MAX and buckets,
        "fresh166": live and normal and n == 166 and g("depth") == 0 and mint_owner == TOKEN_2022
        and g("initial_virtual_quote_reserves") == IVQR_SOL and quote_offset_consistent(c),
        "buckets166": live and normal and n == 166 and g("depth") == 0 and buckets,
        "grow141": live and normal and n == 141,
        "grow151": live and normal and n == 151,
        "synthetic_stuck": bool(c["complete"]) and c["virtual_token_reserves"] > 0
        and g("depth") == 1 and not sol and pc,
        "graduated_synthetic": migrated and normal and pc and pool is not None,
        "cashback166": live and sol and c["is_cashback_coin"] == 1 and not c["is_mayhem_mode"]
        and n == 166,
        "mayhem166": live and c["is_mayhem_mode"] == 1 and n == 166
        and not quote_offset_consistent(c),
        "wsol_pool_buckets": migrated and normal and pool is not None and pool["len"] >= 287
        and pool.get("protocol_fees_bucket", 0) > 0 and pool.get("creator_fees_bucket", 0) > 0,
    }
    if expect not in checks:
        return f"unknown expectation {expect}"
    return None if checks[expect] else f"no longer `{expect}`: {c}"


def latest_create_166(rpc, blocks=40):
    """The mint of the newest `create_v2` coin whose curve fits `fresh166` (for `--coin
    fresh166=latest`): finalized blocks, newest first."""
    slot = rpc.call("getSlot", [{"commitment": "finalized"}])
    for s in range(slot, slot - blocks, -1):
        try:
            events = _events_in_block(rpc, s)
        except SystemExit as e:
            print(f"skip block {s}: {e}", file=sys.stderr)
            continue
        for mint in [m for kind, m, _ in events if kind == "create"]:
            _, (vm, vc) = rpc.multiple([mint, curve_of(mint)])
            if vm is None or vc is None:
                continue
            c = decode_curve(decode_value(vc)["data"])
            if role_problem_2026_10_13("fresh166", c, vm["owner"], None) is None:
                return mint
    raise SystemExit(f"no fresh166 create in the last {blocks} finalized blocks; pass "
                     "--coin fresh166=MINT")


def pump_coins_2026_10_13(overrides, rpc=None):
    """`PUMP_COINS_2026_10_13` with `role=MINT` overrides (`fresh166=latest`: the newest
    `create_v2` that fits the role); roles of `PUMP_COINS` are left to `pump_coins`."""
    over = dict(o.split("=", 1) for o in overrides or [])
    roles = {r for r, _, _, _ in PUMP_COINS_2026_10_13}
    unknown = set(over) - roles - {r for r, _, _, _ in PUMP_COINS}
    if unknown:
        raise SystemExit(f"unknown coin roles: {sorted(unknown)}")
    if over.get("fresh166") == "latest":
        over["fresh166"] = latest_create_166(rpc)
        print(f"fresh166 = {over['fresh166']} (the newest create_v2 that fits the role)")
    return [(r, over.get(r, mint), x, p) for r, mint, x, p in PUMP_COINS_2026_10_13]


def fetch_pump_state_2026_10_13(rpc, coins, m):
    """Group `pump_state_2026_10_13`: its statics and every role's accounts in one
    getMultipleAccounts call (one slot)."""
    # 1. What the derived keys depend on: GlobalConfig (none of its keys is derived here), the
    #    mints (token program), the curves (creator, quote), the canonical pools; then the quote
    #    mints of the curves not quoted in SOL (their token program).
    roots = [PUMP_GLOBAL, PAMM_GLOBAL_CONFIG]
    for _, mint, _, _ in coins:
        roots += [mint, curve_of(mint), canonical_pool_of(mint)[0]]
    _, vals = rpc.multiple(roots)
    if vals[0] is None or vals[1] is None:
        raise SystemExit("pump Global or PumpSwap GlobalConfig missing")
    derived = {}
    for i, (role, mint, expect, _) in enumerate(coins):
        vm, vc, vp = vals[2 + 3 * i: 5 + 3 * i]
        if vm is None or vc is None:
            raise SystemExit(f"{role}: mint {mint} or its curve does not exist")
        c = decode_curve(decode_value(vc)["data"])
        pool_d = decode_value(vp)["data"] if vp else None
        valid = pool_valid_for(mint, canonical_pool_of(mint)[0], vp and vp["owner"], pool_d)
        pool = decode_pamm_pool(pool_d) if valid else None
        why = role_problem_2026_10_13(expect, c, vm["owner"], pool)
        if why:
            raise SystemExit(f"role {role} ({mint}): {why}; pick another and pass "
                             f"--coin {role}=MINT")
        derived[role] = {"token_program": vm["owner"], "creator": c["creator"], "pool": pool,
                         "quote_mint": None if c["quote_mint"] == SYSTEM else c["quote_mint"]}
    quotes = sorted({d["quote_mint"] for d in derived.values() if d["quote_mint"]})
    quote_program = {}
    if quotes:
        _, qv = rpc.multiple(quotes)
        for q, v in zip(quotes, qv):
            if v is None:
                raise SystemExit(f"quote mint {q} does not exist")
            quote_program[q] = v["owner"]

    # 2. The account list (first name wins for shared accounts).
    plan, seen = [], {}

    def add(name, addr, purpose, role=None, may_be_absent=False):
        if addr in seen:
            return
        seen[addr] = name
        plan.append((name, addr, purpose, role, may_be_absent))

    for name, addr, purpose in PUMP_STATICS_2026_10_13:
        add(name, addr, purpose)
    for role, mint, expect, purpose in coins:
        dv = derived[role]
        tp, curve = dv["token_program"], curve_of(mint)
        pre = f"pump_{role}"
        add(f"{pre}_mint", mint, f"mint of the `{role}` coin: {purpose}", role)
        add(f"{pre}_curve", curve, f"BondingCurve of the `{role}` coin: {purpose}", role)
        add(f"{pre}_curve_base_ata", ata(curve, mint, tp), f"the `{role}` curve's token ATA "
            "(associated_bonding_curve)", role)
        add(f"{pre}_creator_vault", pda([b"creator-vault", dv["creator"]], PUMP),
            f"pump creator vault of the `{role}` curve's creator", role, may_be_absent=True)
        sharing = pda([b"sharing-config", mint], PUMP_FEES)
        if dv["creator"] == sharing:
            add(f"{pre}_sharing_config", sharing, f"pump-fees sharing-config of the `{role}` mint "
                "(the curve's creator)", role, may_be_absent=True)
        if dv["quote_mint"]:
            q = dv["quote_mint"]
            add(f"{pre}_quote_mint", q, f"the `{role}` curve's quote mint (a curve of depth > 0 "
                "is quoted in another pump coin)", role)
            add(f"{pre}_curve_quote_ata", ata(curve, q, quote_program[q]), f"the `{role}` "
                "curve's quote ATA (associated_quote_bonding_curve)", role)
        if dv["pool"]:
            pool, authority = canonical_pool_of(mint)
            p = dv["pool"]
            boost = pda([b"boost_vault", pool], PUMP_AMM)
            ccva = pda([b"creator_vault", p["coin_creator"]], PUMP_AMM)
            add(f"{pre}_pool", pool, f"the `{role}` coin's canonical PumpSwap Pool", role)
            add(f"{pre}_pool_base_vault", p["pool_base_token_account"], "the pool's base vault", role)
            add(f"{pre}_pool_quote_vault", p["pool_quote_token_account"], "the pool's WSOL vault",
                role)
            add(f"{pre}_lp_mint", p["lp_mint"], "the pool's LP mint", role)
            add(f"{pre}_pool_authority", authority, "pump PDA [\"pool-authority\", mint] (the "
                "pool's creator)", role, may_be_absent=True)
            add(f"{pre}_boost_vault", boost, "PumpSwap PDA [\"boost_vault\", pool]", role,
                may_be_absent=True)
            add(f"{pre}_boost_vault_ata", ata(boost, WSOL, TOKEN), "the boost vault's WSOL ATA "
                "(absent once the boost has run)", role, may_be_absent=True)
            add(f"{pre}_coin_creator_vault_authority", ccva, "PumpSwap PDA [\"creator_vault\", "
                "pool.coin_creator]", role, may_be_absent=True)
            add(f"{pre}_coin_creator_vault_ata", ata(ccva, WSOL, TOKEN), "its WSOL ATA (T11)", role,
                may_be_absent=True)
    if len(plan) > 100:
        raise SystemExit(f"{len(plan)} accounts do not fit one getMultipleAccounts call")

    # 3. One call, one slot.
    slot, vals = rpc.multiple([a for _, a, _, _, _ in plan])
    block_time = None
    try:
        block_time = rpc.call("getBlockTime", [slot])
    except SystemExit as e:
        print(f"WARNING: getBlockTime({slot}): {e}", file=sys.stderr)
    entries, files, by_addr = [], [], {}
    for (name, addr, purpose, role, may_be_absent), v in zip(plan, vals):
        e = {"name": name, "group": PUMP_STATE_2026_10_13}
        if role:
            e["role"] = role
        e.update({"purpose": purpose, "address": addr, "slot": slot})
        if v is None:
            if not may_be_absent:
                raise SystemExit(f"{name} {addr} does not exist at slot {slot}")
            e["absent"] = True
            entries.append(e)
            continue
        a = decode_value(v)
        d = a["data"]
        rel = f"{PUMP_2026_10_13_ACCOUNT_DIR}/{name}.bin"
        write_file(rel, d)
        files.append(rel)
        e.update({"owner": a["owner"], "lamports": a["lamports"], "executable": a["executable"],
                  "rent_epoch": a["rent_epoch"], "space": len(d), "sha256": sha256(d),
                  "file": rel})
        decoded = decode_pump_account(a["owner"], d)
        if decoded is not None:
            e["decoded"] = decoded
        entries.append(e)
        by_addr[addr] = (a["owner"], d)
    # 4. The dumped state must still fit every role and name what step 1 derived.
    for role, mint, expect, _ in coins:
        _, cd = by_addr[curve_of(mint)]
        c = decode_curve(cd)
        pool, _ = canonical_pool_of(mint)
        po, pd_ = by_addr.get(pool, (None, None))
        valid = pool_valid_for(mint, pool, po, pd_)
        why = role_problem_2026_10_13(expect, c, by_addr[mint][0],
                                      decode_pamm_pool(pd_) if valid else None)
        if why:
            raise SystemExit(f"role {role} changed between the two reads: {why}")
        if c["creator"] != derived[role]["creator"]:
            raise SystemExit(f"role {role}: the curve's creator changed between the two reads")
    # 5. How the statics compare with `pump_state`'s bytes (a record, not a pin).
    old = {e["address"]: e for e in m["accounts"]
           if e.get("group") == "pump_state" and not e.get("absent")}
    versus = {}
    for name, addr, _ in PUMP_STATICS_2026_10_13:
        d = by_addr[addr][1]
        o = old.get(addr)
        if o is None:
            versus[name] = "no pump_state fixture"
            continue
        od = read_file(o["file"])
        if d == od:
            versus[name] = f"equal to {o['name']} (pump_state, slot {o['slot']:,})"
        elif d[:len(od)] == od:
            versus[name] = (f"{o['name']}'s {len(od):,} bytes (pump_state, slot {o['slot']:,}) "
                            f"followed by {d[len(od):].hex()}")
        else:
            diff = [i for i in range(min(len(d), len(od))) if d[i] != od[i]]
            versus[name] = (f"differs from {o['name']} (pump_state, slot {o['slot']:,}): "
                            f"{len(od):,} -> {len(d):,} bytes, {len(diff)} differing bytes from "
                            f"offset {diff[0] if diff else len(od)}")
    snapshot = {
        "group": PUMP_STATE_2026_10_13,
        "slot": slot,
        "block_time": block_time,
        "block_time_utc": dt.datetime.fromtimestamp(block_time, dt.UTC).strftime(
            "%Y-%m-%dT%H:%M:%SZ") if block_time else None,
        "fetched_at": now_utc(),
        "rpc": rpc.used,
        "method": "one getMultipleAccounts call (base64) over every account below (absent ones "
        "recorded as such), so all of them are the mainnet state at `slot`; a first call read "
        "Global, GlobalConfig, the mints, curves and canonical pools (a second the quote mints of "
        "curves not quoted in SOL) to derive the rest",
        "coins": {role: mint for role, mint, _, _ in coins},
        "versus_pump_state": versus,
    }
    return entries, files, snapshot


def check_pump_2026_10_13(m):
    """Offline cross-checks of group `pump_state_2026_10_13`; returns a list of problems."""
    problems = []
    state = [e for e in m["accounts"] if e.get("group") == PUMP_STATE_2026_10_13]
    by_addr = {e["address"]: e for e in state}
    by_name = {e["name"]: e for e in state}
    slots = {e["slot"] for e in state}
    if len(slots) > 1:
        problems.append(f"{PUMP_STATE_2026_10_13} spans several slots: {sorted(slots)}")
    for name, addr, _ in PUMP_STATICS_2026_10_13:
        e = by_name.get(name)
        if e is None or e.get("absent") or e["address"] != addr:
            problems.append(f"static {name} {addr} is not a fixture")
    g = by_name.get("pump_global_1088")
    if g and not g.get("absent") and g["space"] != 1088:
        problems.append(f"pump_global_1088: {g['space']} bytes, not 1,088")
    snap = next((s for s in m.get("snapshots", []) if s["group"] == PUMP_STATE_2026_10_13), None)
    coins = snap["coins"] if snap else {}
    expect_of = {r: x for r, _, x, _ in PUMP_COINS_2026_10_13}
    pinned = {r: mint for r, mint, _, _ in PUMP_COINS_2026_10_13}
    if coins != pinned:
        problems.append(f"{PUMP_STATE_2026_10_13} was dumped with coins {coins}, but "
                        f"PUMP_COINS_2026_10_13 pins {pinned}: pin the dumped mints")
    for role, mint in coins.items():
        me, ce = by_addr.get(mint), by_addr.get(curve_of(mint))
        if not me or not ce or me.get("absent") or ce.get("absent"):
            problems.append(f"{role}: mint or curve missing")
            continue
        c = decode_curve(read_file(ce["file"]))
        pool, _ = canonical_pool_of(mint)
        pe = by_addr.get(pool)
        pd_ = read_file(pe["file"]) if pe and not pe.get("absent") else None
        valid = bool(pd_) and pool_valid_for(mint, pool, pe["owner"], pd_)
        why = role_problem_2026_10_13(expect_of.get(role, "?"), c, me["owner"],
                                      decode_pamm_pool(pd_) if valid else None)
        if why:
            problems.append(f"{role}: {why}")
        base = by_addr.get(ata(curve_of(mint), mint, me["owner"]))
        if not base or base.get("absent"):
            problems.append(f"{role}: no curve base ATA fixture")
        else:
            dec = base.get("decoded", {})
            if dec.get("mint") != mint or dec.get("owner") != curve_of(mint):
                problems.append(f"{role}: curve base ATA mint/owner mismatch")
        if valid:
            p = decode_pamm_pool(pd_)
            for k, mk in (("pool_base_token_account", "base_mint"),
                          ("pool_quote_token_account", "quote_mint")):
                v = by_addr.get(p[k], {}).get("decoded", {})
                if v.get("mint") != p[mk] or v.get("owner") != pool:
                    problems.append(f"{role}: pool vault {k} mint/owner mismatch")
    return problems


# ---- PumpSwap pools (M6; R-VEN Update 2026-09-30; ARCH §3.12 PumpSwap, §8.2) --------------------
#
# Group `pamm_state`: one getMultipleAccounts call over PumpSwap's statics (GlobalConfig, its
# FeeConfig, the global volume accumulator, WSOL, Rent, the 16 recipient WSOL ATAs) and one set of
# accounts per canonical-pool **role** (`PAMM_POOLS`), so M6's PumpSwap tests trade pools that
# exist on mainnet in every shape the adapter meets: a fresh graduate with its boost
# running, a mature 301-byte pool, a cashback coin, and two older lengths the program grows to 271
# bytes on their next trade. Group `pamm_synthetic`: pools **derived** from `pamm_state` bytes
# (no RPC): the mature pool with a negative `virtual_quote_reserves` (PPD `cb188ce0`; none existed
# on mainnet when this was written, R-VEN Update 2026-09-30 H3). A `negative_vqr` role for a real
# pool can be added with `--pool negative_vqr=MINT` once one exists.

PAMM_ACCOUNT_DIR = "accounts/pamm"
PUMP_WITHDRAW_AUTHORITY = "39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg"
PUMP_CREATE_POOL = bytes.fromhex("e992d18ecf6840bc")

# (role, mint, expectation, purpose). The pool is the mint's canonical PumpSwap pool (R-VEN §1.3).
PAMM_POOLS = [
    (
        "fresh",
        "6Rs3yJGQapP2FCgBv2Z6eiH99VJSGSGBvqmPCm13pump",
        "pamm_fresh",
        "a canonical pool minutes after migrate_v2 (271 bytes at creation; clients usually "
        "extend it to 301 within seconds), VQR = floor(0.2069 x migrated quote), the boost vault "
        "still holding SOL (the A13 boost gate, straddle tx3, T-VENUE-SWITCH on a real pool)",
    ),
    (
        "mature",
        "CbcyNo7m1amFWqEQm2m4PLv1UNvpcL3C1Ujm6AkzpKoU",
        "pamm_mature",
        "e/acc's canonical pool (R-VEN §1.4 `4JAnKF…`): 301 bytes, boost long over (vault "
        "closed), deep, with third-party LP (LP mint supply 7 % of lp_supply: deposits scale VQR, "
        "which is why it sits above its InitBoostEvent value, R-VEN Update 2026-09-30 H3, H11); "
        "the source of the synthetic negative-VQR pool",
    ),
    (
        "cashback",
        "34Bkw9UMYRQNFC4BB7E2fPR9U7GTx6oFZZFgP5wYTMAh",
        "pamm_cashback",
        "a canonical cashback-coin pool (is_cashback_coin): the ELF requires the cashback "
        "remaining accounts on buys and sells (6061 / 6060 without them) and accrues WSOL in "
        "ATA(pAMM UVA, WSOL); T-CASHBACK (pAMM), ARCH §3.12 T18",
    ),
    (
        "legacy211",
        "G8ZmGTbaM1xRioCR2g3EfSxiLiaGFMYuEWqQbTBPpump",
        "pamm_len_211",
        "an early (2025) canonical pool of 211 bytes (no coin_creator: creator fee 0): the next "
        "trade grows it to 271 bytes, the user topping its rent up to minimum_balance(271)",
    ),
    (
        "legacy261",
        "Ch89LLQzvdxuMtGJ4iJES8CwPumhAkDazB88auTgpump",
        "pamm_len_261",
        "a boosted canonical pool never extended: 261 bytes (VQR present, creator_fee_bps and the "
        "bools past the end read as 0); the next trade grows it to 271 bytes",
    ),
]

# (name, source role, rule, purpose). VQR := rule(quote vault amount); PPD `cb188ce0` only
# guarantees 0 <= vault + VQR <= u64::MAX, so any such value is a valid state.
PAMM_SYNTHETIC = [
    (
        "pamm_mature_pool_negative_vqr",
        "mature",
        "-(quote_vault_amount // 2)",
        "SYNTHETIC (not a mainnet account): the `mature` pool with virtual_quote_reserves set to "
        "-(quote vault // 2), so the effective quote reserve is half the real vault. The deployed "
        "PumpSwap ELF prices it exactly as effective = vault + VQR (buy, sell, "
        "buy_exact_quote_in, fee tier on the effective market cap) and fails 6023 Overflow once "
        "the sum leaves [0, u64::MAX] (R-VEN Update 2026-09-30 H3). Every other byte is the "
        "`mature` fixture's",
    ),
]

PAMM_NEGATIVE_VQR_RULES = {"-(quote_vault_amount // 2)": lambda vault: -(vault // 2)}


def pamm_pool_problem(expect, mint, pool_addr, pool_owner, d, boost_amount):
    """Why a canonical pool no longer fits its role (None if it does)."""
    if not pool_valid_for(mint, pool_addr, pool_owner, d):
        return "the canonical pool is missing or not valid (ARCH §2.5 canonical_pool_valid)"
    p = decode_pamm_pool(d)
    checks = {
        "pamm_fresh": p["len"] in (271, 301) and not p["is_cashback_coin"]
        and p["virtual_quote_reserves"] > 0 and (boost_amount or 0) > 0,
        "pamm_mature": p["len"] == 301 and not p["is_cashback_coin"] and boost_amount is None
        and p["virtual_quote_reserves"] >= 0,
        "pamm_cashback": p["is_cashback_coin"] == 1,
        "pamm_len_211": p["len"] == 211,
        "pamm_len_261": p["len"] == 261,
        "pamm_negative_vqr": p["virtual_quote_reserves"] < 0,
    }
    if expect not in checks:
        return f"unknown expectation {expect}"
    return None if checks[expect] else f"no longer `{expect}`: len {p['len']}, VQR " \
        f"{p['virtual_quote_reserves']}, cashback {p['is_cashback_coin']}, boost {boost_amount}"


def latest_fresh_mint(rpc):
    """The mint of the newest non-mayhem, SOL-quoted, boosted `migrate_v2` (for `--pool
    fresh=latest`)."""
    sigs = rpc.call("getSignaturesForAddress", [PUMP_WITHDRAW_AUTHORITY, {"limit": 40}])
    for sg in sigs:
        if sg["err"]:
            continue
        tx = rpc.call("getTransaction", [sg["signature"], {"encoding": "json",
                                                           "maxSupportedTransactionVersion": 1}])
        if not tx:
            continue
        msg = tx["transaction"]["message"]
        la = tx["meta"].get("loadedAddresses") or {}
        keys = msg["accountKeys"] + la.get("writable", []) + la.get("readonly", [])
        for grp in tx["meta"].get("innerInstructions") or []:
            for ix in grp["instructions"]:
                if keys[ix["programIdIndex"]] != PUMP_AMM:
                    continue
                d = b58decode(ix["data"])
                if d[:8] == PUMP_CREATE_POOL and len(d) > 58 and d[58] == 0:
                    acc = [keys[i] for i in ix["accounts"]]
                    if acc[4] == WSOL:
                        return acc[3]
    raise SystemExit("no recent boosted SOL migration found; pass --pool fresh=MINT")


def pamm_pools(overrides, rpc=None):
    """`PAMM_POOLS` with `role=MINT` overrides (`fresh=latest`: the newest boosted migration);
    `negative_vqr=MINT` adds that role."""
    over = dict(o.split("=", 1) for o in overrides or [])
    known = {r for r, _, _, _ in PAMM_POOLS} | {"negative_vqr"}
    unknown = set(over) - known
    if unknown:
        raise SystemExit(f"unknown PumpSwap pool roles: {sorted(unknown)}")
    if over.get("fresh") == "latest":
        over["fresh"] = latest_fresh_mint(rpc)
        print(f"fresh = {over['fresh']} (the newest boosted migration)")
    out = [(r, over.get(r, mint), x, p) for r, mint, x, p in PAMM_POOLS]
    if "negative_vqr" in over:
        out.append(("negative_vqr", over["negative_vqr"], "pamm_negative_vqr",
                    "a real canonical pool with a negative virtual_quote_reserves (PPD cb188ce0)"))
    return out


def fetch_pamm_state(rpc, pools):
    """PumpSwap statics and every role's accounts in one getMultipleAccounts call (one slot)."""
    roots = [PAMM_GLOBAL_CONFIG]
    for _, mint, _, _ in pools:
        roots += [mint, canonical_pool_of(mint)[0]]
    _, vals = rpc.multiple(roots)
    if vals[0] is None:
        raise SystemExit("PumpSwap GlobalConfig missing")
    gc = decode_global_config(decode_value(vals[0])["data"])
    derived = {}
    for i, (role, mint, _, _) in enumerate(pools):
        vm, vp = vals[1 + 2 * i: 3 + 2 * i]
        if vm is None or vp is None:
            raise SystemExit(f"{role}: mint {mint} or its canonical pool does not exist")
        derived[role] = {"token_program": vm["owner"],
                         "pool": decode_pamm_pool(decode_value(vp)["data"])}

    plan, seen = [], {}

    def add(name, addr, purpose, role=None, may_be_absent=False):
        if addr in seen:
            return
        seen[addr] = name
        plan.append((name, addr, purpose, role, may_be_absent))

    add("pamm_state_global_config", PAMM_GLOBAL_CONFIG, "PumpSwap GlobalConfig at this group's "
        "slot (disable_flags @56: bit 0 create_pool, 1 deposit, 2 withdraw, 3 buy, 4 sell — "
        "T-DISABLE on the ELF, R-VEN Update 2026-09-30 H4; recipient lists; "
        "creator_fee_configurable)")
    add("pamm_state_fee_config", PAMM_FEE_CONFIG, "pump-fees FeeConfig of PumpSwap (the 25 "
        "market-cap tiers of canonical SOL pools, flat fees of the others)")
    add("pamm_state_global_volume_accumulator", PAMM_GVA, "PumpSwap GlobalVolumeAccumulator (buy)")
    add("pamm_state_mint_wsol", WSOL, "WSOL: the pools' quote mint")
    add("pamm_state_sysvar_rent", RENT_SYSVAR, "the Rent sysvar at this slot (5,080 lamports "
        "per byte): the pAMM UVA (137 B), fee ATAs (165 B) and a short pool's growth to 271 B "
        "are charged at it (T-RENT, pAMM)")
    for i, k in enumerate(gc["protocol_fee_recipients"]):
        add(f"pamm_state_protocol_fee_recipient_{i}_wsol_ata", ata(k, WSOL, TOKEN),
            f"WSOL ATA of PumpSwap protocol fee recipient #{i} {k} (T10: must exist)",
            may_be_absent=True)
    for i, k in enumerate(gc["buyback_fee_recipients"]):
        add(f"pamm_state_buyback_fee_recipient_{i}_wsol_ata", ata(k, WSOL, TOKEN),
            f"WSOL ATA of PumpSwap buyback fee recipient #{i} {k} (T17: must exist)",
            may_be_absent=True)
    for role, mint, _, purpose in pools:
        p = derived[role]["pool"]
        pool, authority = canonical_pool_of(mint)
        boost = pda([b"boost_vault", pool], PUMP_AMM)
        ccva = pda([b"creator_vault", p["coin_creator"]], PUMP_AMM)
        pre = f"pamm_{role}"
        add(f"{pre}_pool", pool, f"the `{role}` canonical PumpSwap Pool: {purpose}", role)
        add(f"{pre}_mint", mint, f"base mint of the `{role}` pool", role)
        add(f"{pre}_pool_base_vault", p["pool_base_token_account"], "the pool's base vault", role)
        add(f"{pre}_pool_quote_vault", p["pool_quote_token_account"], "the pool's WSOL vault",
            role)
        add(f"{pre}_lp_mint", p["lp_mint"], "the pool's LP mint", role)
        add(f"{pre}_coin_creator_vault_authority", ccva, "PumpSwap PDA [\"creator_vault\", "
            "pool.coin_creator] (T12)", role, may_be_absent=True)
        add(f"{pre}_coin_creator_vault_ata", ata(ccva, WSOL, TOKEN), "its WSOL ATA (T11: must "
            "exist)", role, may_be_absent=True)
        add(f"{pre}_boost_vault", boost, "PumpSwap PDA [\"boost_vault\", pool]", role,
            may_be_absent=True)
        add(f"{pre}_boost_vault_ata", ata(boost, WSOL, TOKEN), "the boost vault's WSOL ATA (P5; "
            "boost_buy_and_burn closes it when it spends the last lamport)", role,
            may_be_absent=True)
        add(f"{pre}_pool_authority", authority, "pump PDA [\"pool-authority\", mint] (the "
            "pool's creator)", role, may_be_absent=True)
        add(f"{pre}_curve", curve_of(mint), "the coin's pump BondingCurve (migrated: complete, "
            "reserves 0): classify_pump's detection key", role, may_be_absent=True)
    if len(plan) > 100:
        raise SystemExit(f"{len(plan)} accounts do not fit one getMultipleAccounts call")

    slot, vals = rpc.multiple([a for _, a, _, _, _ in plan])
    block_time = None
    try:
        block_time = rpc.call("getBlockTime", [slot])
    except SystemExit as e:
        print(f"WARNING: getBlockTime({slot}): {e}", file=sys.stderr)
    entries, files, by_addr = [], [], {}
    for (name, addr, purpose, role, may_be_absent), v in zip(plan, vals):
        e = {"name": name, "group": "pamm_state"}
        if role:
            e["role"] = role
        e.update({"purpose": purpose, "address": addr, "slot": slot})
        if v is None:
            if not may_be_absent:
                raise SystemExit(f"{name} {addr} does not exist at slot {slot}")
            e["absent"] = True
            entries.append(e)
            continue
        a = decode_value(v)
        d = a["data"]
        rel = f"{PAMM_ACCOUNT_DIR}/{name}.bin"
        write_file(rel, d)
        files.append(rel)
        e.update({"owner": a["owner"], "lamports": a["lamports"], "executable": a["executable"],
                  "rent_epoch": a["rent_epoch"], "space": len(d), "sha256": sha256(d),
                  "file": rel})
        decoded = decode_pump_account(a["owner"], d)
        if decoded is not None:
            e["decoded"] = decoded
        entries.append(e)
        by_addr[addr] = (a["owner"], d)
    for role, mint, expect, _ in pools:
        pool, _ = canonical_pool_of(mint)
        owner, d = by_addr.get(pool, (None, None))
        why = pamm_pool_problem(expect, mint, pool, owner, d, _boost_amount(by_addr, pool))
        if why:
            raise SystemExit(f"PumpSwap role {role} ({mint}): {why}; pick another and pass "
                             f"--pool {role}=MINT")
    snapshot = {
        "group": "pamm_state",
        "slot": slot,
        "block_time": block_time,
        "block_time_utc": dt.datetime.fromtimestamp(block_time, dt.UTC).strftime(
            "%Y-%m-%dT%H:%M:%SZ") if block_time else None,
        "fetched_at": now_utc(),
        "rpc": rpc.used,
        "method": "one getMultipleAccounts call (base64) over every account below (absent ones "
        "recorded as such), so all of them are the mainnet state at `slot`; a first call read "
        "GlobalConfig, the mints and the canonical pools to derive the rest",
        "pools": {role: mint for role, mint, _, _ in pools},
    }
    return entries, files, snapshot


def _boost_amount(by_addr, pool):
    """The boost vault ATA's WSOL amount, or None if the account does not exist."""
    boost_ata = ata(pda([b"boost_vault", pool], PUMP_AMM), WSOL, TOKEN)
    if boost_ata not in by_addr:
        return None
    return decode_token_account(by_addr[boost_ata][1])["amount"]


def synthesize_pamm(m):
    """The `pamm_synthetic` entries, rebuilt from the `pamm_state` fixture bytes (no RPC)."""
    state = {e["name"]: e for e in m["accounts"] if e.get("group") == "pamm_state"}
    entries, files = [], []
    for name, role, rule, purpose in PAMM_SYNTHETIC:
        src = state.get(f"pamm_{role}_pool")
        vault = state.get(f"pamm_{role}_pool_quote_vault")
        if not src or not vault or src.get("absent") or vault.get("absent"):
            raise SystemExit(f"{name}: pamm_state has no `{role}` pool (fetch pamm_state first)")
        d = bytearray(read_file(src["file"]))
        if len(d) < 261:
            raise SystemExit(f"{name}: the source pool is shorter than 261 bytes")
        amount = decode_token_account(read_file(vault["file"]))["amount"]
        vqr = PAMM_NEGATIVE_VQR_RULES[rule](amount)
        d[245:261] = vqr.to_bytes(16, "little", signed=True)
        d = bytes(d)
        rel = f"{PAMM_ACCOUNT_DIR}/{name}.bin"
        write_file(rel, d)
        files.append(rel)
        e = {k: src[k] for k in ("address", "owner", "lamports", "executable", "rent_epoch",
                                 "slot")}
        e.update({
            "name": name, "group": "pamm_synthetic", "role": role, "synthetic": True,
            "purpose": purpose, "derived_from": src["name"],
            "patch": {"offset": 245, "length": 16, "field": "virtual_quote_reserves (i128 LE)",
                      "rule": rule, "quote_vault_amount": amount, "value": vqr,
                      "effective_quote": amount + vqr},
            "space": len(d), "sha256": sha256(d), "file": rel,
            "decoded": decode_pamm_pool(d),
        })
        entries.append({k: e[k] for k in ("name", "group", "role", "synthetic", "purpose",
                                          "derived_from", "patch", "address", "owner",
                                          "lamports", "executable", "rent_epoch", "space",
                                          "slot", "sha256", "file", "decoded")})
    snapshot = {
        "group": "pamm_synthetic",
        "slot": state[f"pamm_{PAMM_SYNTHETIC[0][1]}_pool"]["slot"],
        "fetched_at": now_utc(),
        "rpc": None,
        "method": "derived offline from the pamm_state fixture bytes by scripts/dump-fixtures.py "
        "(synthesize_pamm): only `patch` differs from the source account",
    }
    return entries, files, snapshot


def check_pamm(m, only=None):
    """Offline cross-checks of the PumpSwap groups; returns a list of problems."""
    problems = []
    state = [e for e in m["accounts"] if e.get("group") == "pamm_state"]
    synth = [e for e in m["accounts"] if e.get("group") == "pamm_synthetic"]
    if only in (None, "pamm_state") and state:
        by_addr = {e["address"]: e for e in state if not e.get("absent")}
        slots = {e["slot"] for e in state}
        if len(slots) > 1:
            problems.append(f"pamm_state spans several slots: {sorted(slots)}")
        snap = next((s for s in m.get("snapshots", []) if s["group"] == "pamm_state"), None)
        expect = {r: x for r, _, x, _ in PAMM_POOLS}
        expect["negative_vqr"] = "pamm_negative_vqr"
        pinned = {r: mint for r, mint, _, _ in PAMM_POOLS}
        dumped = snap["pools"] if snap else {}
        if {r: v for r, v in dumped.items() if r in pinned} != pinned:
            problems.append(f"pamm_state was dumped with pools {dumped}, but PAMM_POOLS pins "
                            f"{pinned}: pin the dumped mints")
        files = {a: (e["owner"], read_file(e["file"])) for a, e in by_addr.items()}
        for role, mint in dumped.items():
            pool, _ = canonical_pool_of(mint)
            owner, d = files.get(pool, (None, None))
            why = pamm_pool_problem(expect.get(role, "?"), mint, pool, owner, d,
                                    _boost_amount(files, pool))
            if why:
                problems.append(f"pamm_state {role}: {why}")
                continue
            p = decode_pamm_pool(d)
            for k, mk in (("pool_base_token_account", "base_mint"),
                          ("pool_quote_token_account", "quote_mint")):
                v = by_addr.get(p[k], {}).get("decoded", {})
                if v.get("mint") != p[mk] or v.get("owner") != pool:
                    problems.append(f"pamm_state {role}: pool vault {k} mint/owner mismatch")
    if only in (None, "pamm_synthetic") and synth:
        state_by_name = {e["name"]: e for e in state}
        for e in synth:
            src = state_by_name.get(e.get("derived_from"))
            if not src or src.get("absent"):
                problems.append(f"{e['name']}: source {e.get('derived_from')} is not a fixture")
                continue
            want = bytearray(read_file(src["file"]))
            want[245:261] = e["patch"]["value"].to_bytes(16, "little", signed=True)
            got = read_file(e["file"])
            if bytes(want) != got:
                problems.append(f"{e['name']}: bytes are not the source with its patch")
            vault = state_by_name.get(f"pamm_{e['role']}_pool_quote_vault")
            amount = decode_token_account(read_file(vault["file"]))["amount"] if vault else None
            if amount != e["patch"]["quote_vault_amount"]:
                problems.append(f"{e['name']}: quote vault amount differs from the patch record")
            eff = (amount or 0) + e["patch"]["value"]
            if not (e["patch"]["value"] < 0 and 0 <= eff <= 2**64 - 1):
                problems.append(f"{e['name']}: VQR is not negative with vault + VQR in [0, u64]")
            if e["address"] != src["address"] or e["slot"] != src["slot"]:
                problems.append(f"{e['name']}: address or slot differs from its source")
    return problems


# ---- candidates (mainnet: replacements for the coin roles) --------------------------------------


def _events_in_block(rpc, slot):
    b = rpc.call("getBlock", [slot, {"encoding": "json", "maxSupportedTransactionVersion": 1,
                                     "transactionDetails": "full", "rewards": False}])
    out = []
    for tx in b["transactions"]:
        meta = tx["meta"]
        if meta is None or meta.get("err") is not None:
            continue
        la = meta.get("loadedAddresses") or {}
        keys = tx["transaction"]["message"]["accountKeys"] + la.get("writable", []) + la.get(
            "readonly", [])
        if PUMP not in keys:
            continue
        for inner in meta.get("innerInstructions") or []:
            for ix in inner["instructions"]:
                if keys[ix["programIdIndex"]] != PUMP:
                    continue
                d = b58decode(ix["data"])
                if d[:8] != PUMP_EVENT_IX:
                    continue
                body = d[16:]
                if d[8:16] == PUMP_CREATE_EVENT:
                    o = 0
                    for _ in range(3):
                        o += 4 + struct.unpack_from("<I", body, o)[0]
                    out.append(("create", key_at(body, o), body))
                elif d[8:16] == PUMP_TRADE_EVENT:
                    out.append(("trade", key_at(body, 0), body))
    return out


def cmd_candidates(args):
    rpc = Rpc([args.rpc] if args.rpc else RPCS)
    slot = rpc.call("getSlot", [{"commitment": "finalized"}])
    creates = []
    for s in range(slot, slot - args.blocks, -1):
        try:
            for kind, mint, _ in _events_in_block(rpc, s):
                if kind == "create":
                    creates.append((s, mint))
        except SystemExit as e:
            print(f"skip block {s}: {e}", file=sys.stderr)
    print("newest creates (fresh):")
    for s, mint in creates[:10]:
        print(f"  slot {s}  {mint}")
    print("latest migrations (graduated):")
    sigs = rpc.call("getSignaturesForAddress",
                    ["39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg", {"limit": 10}])
    for sg in sigs:
        if sg["err"]:
            continue
        tx = rpc.call("getTransaction", [sg["signature"], {"encoding": "json",
                                                           "maxSupportedTransactionVersion": 1}])
        msg = tx["transaction"]["message"]
        la = tx["meta"].get("loadedAddresses") or {}
        keys = msg["accountKeys"] + la.get("writable", []) + la.get("readonly", [])
        for ix in msg["instructions"]:
            if keys[ix["programIdIndex"]] == PUMP and b58decode(ix["data"])[:8] == PUMP_MIGRATE_V2:
                acc = [keys[i] for i in ix["accounts"]]
                print(f"  slot {sg['slot']}  {acc[2]}  quote {acc[3]}  pool {acc[10]}")
                break
    if args.near:
        print("live SOL non-mayhem curves nearest completion (getProgramAccounts, slow):")
        rows = []
        for size in (115, 125, 150, 151):
            res = rpc.call("getProgramAccounts", [PUMP, {
                "encoding": "base64", "dataSlice": {"offset": 24, "length": 8},
                "filters": [{"dataSize": size}, {"memcmp": {"offset": 48, "bytes": "1"}},
                            {"memcmp": {"offset": 83, "bytes": SYSTEM}},
                            {"memcmp": {"offset": 81, "bytes": "1"}}]}])
            rows += [(u64_at(base64.b64decode(a["account"]["data"][0]), 0), size, a["pubkey"])
                     for a in res]
        for rt, size, curve in sorted(rows)[:10]:
            print(f"  rT {rt:>20,}  {size} B  curve {curve} (mint: getTokenAccountsByOwner)")
    return 0


# ---- M10 program groups (CPMM redeploy, mainnet SPL programs, Squads v4) ---------------------
#
# The programs the M10 suites run on, pinned as mainnet deployed them (M10 plan WP-1). WP-2 moved
# the CPMM tests to `raydium_cpmm_2026_10`, WP-3 the SPL programs to `spl_programs`, WP-7 the
# governance tests to `squads_programs`; the pump revalidation (M10, 2026-10-03) the curve and
# PumpSwap tests to `pump_programs_2026_10`.

CPMM_2026_10_ACCOUNT_DIR = "accounts/raydium_cpmm_2026_10"
PUMP_2026_10_ACCOUNT_DIR = "accounts/pump_2026_10"
SPL_ACCOUNT_DIR = "accounts/spl"
SQUADS_ACCOUNT_DIR = "accounts/squads"
IDL_DIR = "idl"
LOADER_V2 = "BPFLoader2111111111111111111111111111111111"
MEMO = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr"
SQUADS = "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf"
SQUADS_PROGRAM_CONFIG = "BSTq9w3kZwNwpBXJEvTZz2G9ZTNyKBvoSeXMvwb4cNZr"
SQUADS_PROGRAM_CONFIG_DISC = hashlib.sha256(b"account:ProgramConfig").digest()[:8]
SQUADS_PROGRAM_CONFIG_LEN = 144  # 8 + authority 32 + multisig_creation_fee 8 + treasury 32 + 64

# Upgradeable programs: (name, program, ProgramData, trimmed-ELF sha256, deploy slot); both pins
# are hard (`fetch_programs`). The CPMM hash is verify.osec.io's `on_chain_hash` (2026-10-02).
CPMM_PROGRAMS_2026_10 = [
    (
        "raydium_cpmm_2026_10",
        CPMM,
        "DMawCQzbgNTmbzaESc7o6pvL1KAeetY8zA7jNpzntHhU",
        "93cad2458ac72435c94b8ac89faa686987f4e29d2f0e862d546fbc5aa814a0d2",
        452_376_460,
    ),
]
# The pump, pump-fees and PumpSwap redeploys of 2026-10-02 (15:47 UTC); the trimmed hashes are
# the ELFs read at slot 452,963,354. Not a public source: the ELFs carry the discriminators,
# error sites and `.rodata` of `pump-rust-client` 0.2.0's unreleased artifacts with a few
# instructions more (tests/fixtures/README.md).
PUMP_PROGRAMS_2026_10 = [
    (
        "pump_2026_10",
        PUMP,
        "B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu",
        "b0b777c64ccf811ab21493d47d618b8db272f9bfd06b4fba4ba66ef565cb0c75",
        452_654_932,
    ),
    (
        "pump_fees_2026_10",
        PUMP_FEES,
        "75Uu23mqWBb8LM8vDppqC1mQAnCcBuLXhVaDezVMQLRw",
        "f166c2967fbfa6a9d80146360e5b79a2b263e821c999f1e74f266162788462fd",
        452_655_002,
    ),
    (
        "pump_amm_2026_10",
        PUMP_AMM,
        "6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc",
        "015570358c672ae42e5706daa87eb67d4c45e8fd8afec80c66bd85b9f9333cc3",
        452_654_882,
    ),
]
# The redeploys the 2026-10-13 re-pin pins: pump and PumpSwap of 2026-10-08 (the
# synthetic migration, multi-hop, the 166-byte curve) and pump-fees of 2026-10-12 (an unexamined
# 10-08 build at 454,596,501 preceded it and can no longer be fetched). The trimmed hashes are the
# ELFs read at slots 456,311,559–456,311,578. Never refresh `pump_programs_2026_10`: T-LATCH starts
# from it.
PUMP_2026_10_13_ACCOUNT_DIR = "accounts/pump_2026_10_13"
PUMP_PROGRAMS_2026_10_13 = [
    (
        "pump_2026_10_08",
        PUMP,
        "B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu",
        "6956c2f83297b6f7f06bbe8033241589c39da9650597dc5c59fb5a431906bf6f",
        454_596_459,
    ),
    (
        "pump_amm_2026_10_08",
        PUMP_AMM,
        "6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc",
        "3ffd5e3b5a31ccc86b434405e8a1876926da1e8685f891dc964b7f919c172ee1",
        454_596_406,
    ),
    (
        "pump_fees_2026_10_12",
        PUMP_FEES,
        "75Uu23mqWBb8LM8vDppqC1mQAnCcBuLXhVaDezVMQLRw",
        "8ad2ca93c7ae97a7fb0de8a99de4d8825ccfdf89f73138cd49bda948e124b8b8",
        456_132_171,
    ),
]
# pump's redeploy of 2026-10-13 15:40:41 UTC (the nested quote chain: `create_v2` requires a
# bonding curve account for every coin down to the depth-0 root, `QuoteChainAccountsRequired`,
# and caps the depth at 5); the re-pin moved to it. The trimmed hash is the ELF read at slot
# 456,519,827. PumpSwap and pump-fees were not redeployed: they stay `pump_programs_2026_10_13`'s,
# which keeps pump's 2026-10-08 build (the differential `pump_math_it` runs against it).
PUMP_2026_10_13B_ACCOUNT_DIR = "accounts/pump_2026_10_13b"
PUMP_PROGRAMS_2026_10_13B = [
    (
        "pump_2026_10_13",
        PUMP,
        "B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu",
        "e202096978d8f20935153a4271938fd9f0a421d64c1749986b4c5c20ca2a52be",
        456_494_698,
    ),
]
SPL_PROGRAMS = [
    (
        "token_2022",
        TOKEN_2022,
        "DoU57AYuPFu2QU514RktNPG22QhApEjnKxnBcu4BHDTY",
        "7ea94a027005b39196fa08e6d0ddcd55ec32eb43266179921a834a1ba2eb1947",
        427_147_035,
    ),
]
SQUADS_PROGRAMS = [
    (
        "squads_v4",
        SQUADS,
        "Fy3YMJCvwbAXUgUM5b91ucUVA3jYzwWLHL3MwBqKsh8n",
        "d48660833989ecea3145ff726164fe640bd90696f03ce00dfd0cda258cbf2fac",
        302_582_236,
    ),
]
# BPFLoader2 programs (immutable; the account data is the ELF): (name, program, sha256 of the
# account data).
LOADER2_PROGRAMS = [
    ("spl_memo_v3", MEMO, "f520eaf096361abbb9639ea4dc3e5388a87b9330e121f476607b87c46ef67954"),
]
# Files vendored into a group from a pinned source (recorded in the group's snapshot).
SQUADS_IDL = {
    "file": f"{IDL_DIR}/squads_multisig_program.json",
    "url": "https://raw.githubusercontent.com/Squads-Protocol/v4/"
    "64af7330413d5c85cbbccfd8c27a05d45b6e666f/sdk/multisig/idl/squads_multisig_program.json",
    "sha256": "cb9a0a29040ec3853a5105c547ffc608bb0e3175d78dacf30d463aff939f9b9b",
    "version": "2.1.0",
    "purpose": "the Squads v4 Anchor IDL 2.1.0 (`sdk/multisig/idl`, set by Squads commit "
    "39c92c1 `bump(idl): to 2.1.0` and unchanged at 64af733): the instruction discriminators and "
    "account layouts the squads-v4-ix encoder (WP-7) is pinned against",
}

# Where each pinned build comes from, as far as it is known (`source` in the ProgramData entry).
PROGRAM_SOURCES = {
    "raydium_cpmm_2026_10": {
        "repository": "https://github.com/raydium-io/raydium-cp-swap",
        "commit": "bdeee23fc06467602e65305eb5bf3ce46887f943",
        "status": "UNVERIFIED: probably the feat/creator-fee-share branch head (the ELF names "
        "`CreatorFeeShare` / `create_creator_fee_share`, absent from the 445,763,504 build); not "
        "rebuilt. verify.osec.io (2026-10-02): not verified, registered commit cfdb70a8…",
    },
    "token_2022": {
        "repository": "https://github.com/solana-program/token-2022",
        "commit": "9bc02757f600ffe754746708a8a072bcd49d1260",
        "status": "verified build per verify.osec.io (on_chain_hash = executable_hash = "
        "elf_trimmed_sha256, last verified 2026-09-16; read 2026-10-02)",
    },
    **{
        name: {
            "repository": "https://crates.io/crates/pump-rust-client (no public program source)",
            "commit": None,
            "status": "UNVERIFIED: no published source or verified build. The ELF has the same "
            "Anchor discriminators, `.rodata` and error sites as the unreleased `artifacts/"
            f"{name.removesuffix('_2026_10')}.so` in pump-rust-client 0.2.0 (crates.io "
            "2026-09-29, crate sha256 4c940fb1…), with a slightly longer `.text` (a later build "
            "of that release); the on-chain Anchor IDLs were not updated",
        }
        for name in ("pump_2026_10", "pump_fees_2026_10", "pump_amm_2026_10")
    },
    **{
        name: {
            "repository": "https://crates.io/crates/pump-rust-client (no public program source)",
            "commit": None,
            "status": "UNVERIFIED: no published source or verified build. The interface is "
            "pump-public-docs 2293f9a6's IDL / pump-rust-client 0.4.0's (the live discriminators "
            "and errors; tests/fixtures/README.md). "
            + ("0.4.0's artifacts/pump_fees.so (853,528 B) is not this build; its `withdraw` "
               "(b712469c946da122, PDA seed `referral-vault`) is in no IDL, and the 2026-10-08 "
               "pump-fees build (slot 454,596,501) it replaced can no longer be fetched"
               if name.startswith("pump_fees") else
               "Not compared byte for byte with 0.4.0's artifacts"),
        }
        for name in ("pump_2026_10_08", "pump_amm_2026_10_08", "pump_fees_2026_10_12")
    },
    "pump_2026_10_13": {
        "repository": "https://crates.io/crates/pump-rust-client (no public program source)",
        "commit": None,
        "status": "UNVERIFIED: no published source or verified build. The interface is the "
        "2026-10-08 build's (the same instructions, account, event and instruction "
        "discriminators; the on-chain IDL unchanged); the error enum appends "
        "QuoteChainAccountsRequired (6109). Against the 2026-10-08 ELF only create_v2 and its quote "
        "derivation changed; the other instructions differ in the error tables and source line "
        "numbers alone (tests/fixtures/README.md)",
    },
    "squads_v4": {
        "repository": "https://github.com/Squads-Protocol/v4",
        "commit": "64af7330413d5c85cbbccfd8c27a05d45b6e666f",
        "status": "UNVERIFIED: the newest commit of the default branch (2024-11-19, program "
        "2.1.0) before the deploy (block time 2024-11-20T18:36:48Z); the commits up to the "
        "2024-11-22 release merge 8ba270d5 touch no program file; not rebuilt. verify.osec.io "
        "(2026-10-02): not verified, registered commit 3742e552… (2024-01-19, older)",
    },
}

# What the M10 groups are for: (program-account purpose tail, ProgramData purpose head).
M10_GROUP_USE = {
    "raydium_cpmm_2026_10": (
        "The CPMM redeploy of 452,376,460 (M10 WP-1); the CPMM suites move to it in WP-2",
        "the CPMM ELF as redeployed at 452,376,460 (UNVERIFIED source, see `source`). The ELF "
        "only: the pools and configs stay `raydium_state`'s (AmmConfig 236 B and PoolState 637 B "
        "unchanged; the 21 live configs equal the fixture bytes at slot 452,488,420), and "
        "`raydium_programs` keeps the 445,763,504 deploy for T-LATCH",
    ),
    "pump_programs_2026_10": (
        "The pump redeploy of 2026-10-02 (M10 pump revalidation); every curve and PumpSwap test "
        "runs it",
        "the ELF as redeployed on 2026-10-02 (UNVERIFIED source, see `source`). The ELF only: the "
        "state stays `pump_state`'s and `pamm_state`'s (the redeploy appends fee buckets to "
        "`BondingCurve` @125 and `Pool` @271 and keeps every other layout; the configs equal the "
        "fixture bytes at slot 452,964,363), and `pump_programs` keeps the 2026-09 deploys for "
        "T-LATCH",
    ),
    "pump_programs_2026_10_13": (
        "The 2026-10-08/12 redeploys (the 2026-10-13 re-pin): synthetic migration, multi-hop, "
        "the 166-B curve; every curve and PumpSwap suite runs it",
        "the ELF as redeployed on 2026-10-08 (pump, PumpSwap) or 2026-10-12 (pump-fees) "
        "(UNVERIFIED source, see `source`). The ELF only: the state the new builds introduced "
        "(166-byte curves, the 1,088-byte Global, synthetic completions) is group "
        "`pump_state_2026_10_13`'s; `pump_programs_2026_10` keeps the 2026-10-02 deploys for "
        "T-LATCH",
    ),
    "pump_programs_2026_10_13b": (
        "pump's redeploy of 2026-10-13 15:40 UTC (the nested quote chain; the re-pin moved to it); "
        "every curve suite runs it, with PumpSwap and pump-fees of `pump_programs_2026_10_13`",
        "the ELF as redeployed on 2026-10-13 at 456,494,698 (UNVERIFIED source, see `source`). The "
        "ELF only: the state stays `pump_state_2026_10_13`'s (no layout changed), and "
        "`pump_programs_2026_10_13` keeps the 2026-10-08 build",
    ),
    "spl_programs": (
        "The deployed mainnet program, so T-MEMO and the suites run the mainnet build instead of "
        "LiteSVM's bundled one (WP-3)",
        "the mainnet Token-2022 ELF (verified build, see `source`) and its live upgrade authority",
    ),
    "squads_programs": (
        "Squads v4, the governance and emergency multisigs' program (WP-7)",
        "the immutable Squads v4 ELF (no upgrade authority since 302,582,236; source see "
        "`source`)",
    ),
}


def decode_squads_program_config(d):
    """Squads v4 `ProgramConfig` (IDL 2.1.0: authority, multisig_creation_fee, treasury)."""
    return {
        "authority": key_at(d, 8),
        "multisig_creation_fee": u64_at(d, 40),
        "treasury": key_at(d, 48),
    }


def fetch_loader2_programs(rpc, programs=None):
    """BPFLoader2 programs: the account data is the ELF (``programs/<name>.so``, whole)."""
    entries, files = [], []
    for name, program, sha_expected in programs or LOADER2_PROGRAMS:
        res = rpc.call("getAccountInfo", [program, {"encoding": "base64"}])
        if res["value"] is None:
            raise SystemExit(f"{program} does not exist")
        a = decode_value(res["value"])
        d = a["data"]
        if a["owner"] != LOADER_V2 or not a["executable"]:
            raise SystemExit(f"{program}: not an executable BPFLoader2 program")
        n = elf_len(d)
        if any(d[n:]):
            raise SystemExit(f"{program}: non-zero bytes after the ELF's end ({n})")
        if sha256(d) != sha_expected:
            raise SystemExit(f"{name}: account sha256 {sha256(d)} != the pinned {sha_expected} "
                             "(BPFLoader2 programs are immutable: was it replaced by a feature?)")
        rel = f"{PROGRAM_DIR}/{name}.so"
        write_file(rel, d)
        files.append(rel)
        trimmed = d.rstrip(b"\0")
        entries.append(
            {
                "name": f"{name}_program",
                "group": "spl_programs",
                "purpose": f"{name}: the program account under BPFLoader2 (immutable), whose data "
                "is the ELF itself; the file is the whole account. The mainnet build T-MEMO runs "
                "(WP-3: `add_program_with_loader(.., bpf_loader)`)",
                "address": program,
                "owner": a["owner"],
                "lamports": a["lamports"],
                "executable": True,
                "rent_epoch": a["rent_epoch"],
                "space": len(d),
                "slot": res["context"]["slot"],
                "sha256": sha256(d),
                "sha256_expected": sha_expected,
                "file": rel,
                "elf_len": n,
                "elf_trimmed_len": len(trimmed),
                "elf_trimmed_sha256": sha256(trimmed),
                "decoded": {"e_flags": struct.unpack_from("<I", d, 0x30)[0]},
            }
        )
    return entries, files


def fetch_spl(rpc):
    """Group `spl_programs`: Token-2022 (upgradeable) and spl-memo v3 (BPFLoader2)."""
    entries, files, snap = fetch_programs(rpc, SPL_PROGRAMS, "spl_programs", SPL_ACCOUNT_DIR)
    e2, f2 = fetch_loader2_programs(rpc)
    snap["method"] += ("; spl-memo: getAccountInfo (base64) of the BPFLoader2 program account, "
                       "whose data is the ELF")
    return entries + e2, files + f2, snap


def fetch_vendored(spec):
    """Downloads a pinned file (sha256-checked) into the fixtures; returns its snapshot record."""
    req = urllib.request.Request(spec["url"], headers={"User-Agent": "short.win dump-fixtures"})
    with urllib.request.urlopen(req, timeout=120) as r:
        data = r.read()
    if sha256(data) != spec["sha256"]:
        raise SystemExit(f"{spec['url']}: sha256 {sha256(data)} != the pinned {spec['sha256']}")
    write_file(spec["file"], data)
    return {"file": spec["file"], "bytes": len(data), "sha256": spec["sha256"],
            "source": spec["url"], "version": spec["version"], "purpose": spec["purpose"]}


def fetch_squads(rpc):
    """Group `squads_programs`: Squads v4, its ProgramConfig and the vendored IDL 2.1.0."""
    entries, files, snap = fetch_programs(rpc, SQUADS_PROGRAMS, "squads_programs",
                                          SQUADS_ACCOUNT_DIR)
    if pda([b"multisig", b"program_config"], SQUADS) != SQUADS_PROGRAM_CONFIG:
        raise SystemExit("Squads ProgramConfig is not PDA(SQDS4, [multisig, program_config])")
    res = rpc.call("getAccountInfo", [SQUADS_PROGRAM_CONFIG, {"encoding": "base64"}])
    if res["value"] is None:
        raise SystemExit(f"Squads ProgramConfig {SQUADS_PROGRAM_CONFIG} does not exist")
    a = decode_value(res["value"])
    d = a["data"]
    if (a["owner"] != SQUADS or d[:8] != SQUADS_PROGRAM_CONFIG_DISC
            or len(d) != SQUADS_PROGRAM_CONFIG_LEN):
        raise SystemExit(f"{SQUADS_PROGRAM_CONFIG}: not a Squads v4 ProgramConfig")
    rel = f"{SQUADS_ACCOUNT_DIR}/squads_program_config.bin"
    write_file(rel, d)
    files.append(rel)
    entries.append(
        {
            "name": "squads_program_config",
            "group": "squads_programs",
            "purpose": "Squads v4 ProgramConfig PDA [\"multisig\", \"program_config\"]: "
            "multisig_create_v2 charges `multisig_creation_fee` to `treasury` (both accounts of "
            "that instruction; WP-7)",
            "address": SQUADS_PROGRAM_CONFIG,
            "owner": a["owner"],
            "lamports": a["lamports"],
            "executable": a["executable"],
            "rent_epoch": a["rent_epoch"],
            "space": len(d),
            "slot": res["context"]["slot"],
            "sha256": sha256(d),
            "file": rel,
            "decoded": decode_squads_program_config(d),
        }
    )
    snap["vendored"] = [fetch_vendored(SQUADS_IDL)]
    snap["method"] += ("; ProgramConfig: getAccountInfo (base64); the IDL from the source "
                       "repository at the pinned commit (raw.githubusercontent.com, sha256-pinned)")
    return entries, files + [SQUADS_IDL["file"]], snap


def check_m10(m, only=None):
    """Offline checks of the M10 program groups (or of group `only`) against the script's pins;
    returns problems."""
    problems = []

    def in_group(group):
        return {e["name"]: e for e in m["accounts"] if e.get("group") == group}

    for group, programs in (("raydium_cpmm_2026_10", CPMM_PROGRAMS_2026_10),
                            ("spl_programs", SPL_PROGRAMS), ("squads_programs", SQUADS_PROGRAMS),
                            ("pump_programs_2026_10", PUMP_PROGRAMS_2026_10),
                            ("pump_programs_2026_10_13", PUMP_PROGRAMS_2026_10_13),
                            ("pump_programs_2026_10_13b", PUMP_PROGRAMS_2026_10_13B)):
        if only not in (None, group):
            continue
        by_name = in_group(group)
        for name, program, program_data, trimmed, slot in programs:
            p, pd_ = by_name.get(f"{name}_program"), by_name.get(f"{name}_program_data")
            if p is None or pd_ is None:
                problems.append(f"{name}: no program / ProgramData fixture in {group}")
                continue
            if p["address"] != program or pd_["address"] != program_data:
                problems.append(f"{name}: addresses differ from the pins")
            if p["decoded"]["program_data"] != program_data:
                problems.append(f"{name}: the program account does not name its ProgramData")
            if pd_["elf_trimmed_sha256"] != trimmed or pd_["decoded"]["deploy_slot"] != slot:
                problems.append(f"{name}: trimmed ELF / deploy slot differ from the pins")
    for name, program, sha_expected in LOADER2_PROGRAMS if only in (None, "spl_programs") else []:
        e = in_group("spl_programs").get(f"{name}_program")
        if e is None:
            problems.append(f"{name}: no fixture in spl_programs")
        elif (e["address"] != program or e["sha256"] != sha_expected or e["owner"] != LOADER_V2
              or not e["executable"]):
            problems.append(f"{name}: differs from the pin (BPFLoader2, sha256 {sha_expected})")
    if only not in (None, "squads_programs"):
        return problems
    cfg = in_group("squads_programs").get("squads_program_config")
    if cfg is None or cfg["address"] != SQUADS_PROGRAM_CONFIG or cfg["owner"] != SQUADS:
        problems.append("squads_program_config: missing or not the Squads ProgramConfig")
    else:
        try:
            d = read_file(cfg["file"])
            if (d[:8] != SQUADS_PROGRAM_CONFIG_DISC or len(d) != SQUADS_PROGRAM_CONFIG_LEN
                    or decode_squads_program_config(d) != cfg["decoded"]):
                problems.append("squads_program_config: the decoded fields differ from the bytes")
        except FileNotFoundError:
            pass  # `check` reports the missing file
    snap = next((s for s in m.get("snapshots", []) if s["group"] == "squads_programs"), {})
    if [v["sha256"] for v in snap.get("vendored", [])] != [SQUADS_IDL["sha256"]]:
        problems.append("squads_programs: the vendored IDL is not the pinned 2.1.0")
    return problems


# ---- manifest -------------------------------------------------------------------------------


def load_manifest():
    with open(MANIFEST) as f:
        return json.load(f)


RAYDIUM_GROUPS = ("raydium_programs", "raydium_state")
PUMP_GROUPS = ("pump_programs", "pump_state")
# `pamm_synthetic` is derived from `pamm_state` and must follow it.
PAMM_GROUPS = ("pamm_state", "pamm_synthetic")
M10_PROGRAM_GROUPS = ("raydium_cpmm_2026_10", "spl_programs", "squads_programs",
                      "pump_programs_2026_10", "pump_programs_2026_10_13",
                      "pump_programs_2026_10_13b")
# The state the 2026-10-08/12 redeploys introduced (the 2026-10-13 re-pin), after its programs.
PUMP_STATE_2026_10_13_GROUPS = ("pump_state_2026_10_13",)
GROUPS = (RAYDIUM_GROUPS + PUMP_GROUPS + PAMM_GROUPS + M10_PROGRAM_GROUPS
          + PUMP_STATE_2026_10_13_GROUPS)
# Every directory whose files are all managed (SHA256SUMS lines outside them are kept verbatim).
MANAGED_DIRS = (ACCOUNT_DIR, PUMP_ACCOUNT_DIR, PAMM_ACCOUNT_DIR, CPMM_2026_10_ACCOUNT_DIR,
                PUMP_2026_10_ACCOUNT_DIR, PUMP_2026_10_13_ACCOUNT_DIR,
                PUMP_2026_10_13B_ACCOUNT_DIR, SPL_ACCOUNT_DIR, SQUADS_ACCOUNT_DIR, PROGRAM_DIR,
                IDL_DIR)


def managed(entry):
    return entry.get("group") in GROUPS


def entry_files(e):
    """The fixture files of a manifest entry (none for an absent account)."""
    if e.get("absent"):
        return []
    return [e["file"]] if "file" in e else list(e["files"].values())


def vendored_files(m):
    """The files the managed snapshots vendored (e.g. the Squads IDL)."""
    return [v["file"] for s in m.get("snapshots", []) if s["group"] in GROUPS
            for v in s.get("vendored", [])]


def write_manifest(m):
    text = json.dumps(m, indent=2, ensure_ascii=False) + "\n"
    with open(MANIFEST, "w") as f:
        f.write(text)


def write_sums(paths):
    """SHA256SUMS: the unmanaged lines as they are, then every managed file, sorted."""
    keep = []
    if os.path.exists(SUMS):
        with open(SUMS) as f:
            for line in f:
                p = line.rstrip("\n").split("  ", 1)[-1]
                if not p.startswith(tuple(d + "/" for d in MANAGED_DIRS)):
                    keep.append(line.rstrip("\n"))
    lines = keep + [f"{sha256(read_file(p))}  {p}" for p in sorted(paths)]
    with open(SUMS, "w") as f:
        f.write("\n".join(lines) + "\n")


# The files a fetch writes, staged until every group it fetches has passed its pins
# (`commit_files`): a fetch that stops (SystemExit) leaves the committed fixtures as they are.
STAGED = {}


def read_file(rel):
    if rel in STAGED:
        return STAGED[rel]
    with open(os.path.join(FIXTURES, rel), "rb") as f:
        return f.read()


def write_file(rel, data):
    """Stages ``data`` for ``rel`` (`read_file` sees it); `commit_files` writes it to disk."""
    STAGED[rel] = bytes(data)


def commit_files():
    for rel, data in sorted(STAGED.items()):
        path = os.path.join(FIXTURES, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(data)
    STAGED.clear()


def now_utc():
    return dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---- fetch ----------------------------------------------------------------------------------


def fetch_programs(rpc, programs=None, group="raydium_programs", account_dir=ACCOUNT_DIR):
    """Program accounts, ProgramData headers and ELFs; returns (entries, files, snapshot).

    ``programs``: ``(name, program, ProgramData, trimmed-ELF sha256 or None, expected deploy
    slot or None)`` tuples (default: the Raydium programs). In the M10 groups both pins are
    hard: a differing trimmed ELF stops the fetch (elsewhere it is a warning)."""
    entries, files = [], []
    slot_before = rpc.call("getSlot", [])
    use = M10_GROUP_USE.get(group)
    for name, program, program_data, trimmed_expected, slot_expected in programs or PROGRAMS:
        res = rpc.call("getAccountInfo", [program, {"encoding": "base64"}])
        prog = decode_value(res["value"])
        pd_res = rpc.call("getAccountInfo", [program_data, {"encoding": "base64"}])
        pd = decode_value(pd_res["value"])
        d = pd["data"]
        if prog["owner"] != LOADER_UPGRADEABLE or not prog["executable"]:
            raise SystemExit(f"{program}: not an upgradeable program")
        if prog["data"][:4] != b"\x02\0\0\0" or b58encode(prog["data"][4:36]) != program_data:
            raise SystemExit(f"{program}: program account does not name {program_data}")
        if pd["owner"] != LOADER_UPGRADEABLE or d[:4] != b"\x03\0\0\0":
            raise SystemExit(f"{program_data}: not a ProgramData account")
        elf = d[45:]
        n = elf_len(elf)
        if any(elf[n:]):
            raise SystemExit(f"{program_data}: non-zero bytes after the ELF's end ({n})")
        trimmed = elf.rstrip(b"\0")
        deploy_slot = u64_at(d, 4)
        authority = key_at(d, 13) if d[12] == 1 else None
        # The pins first: nothing is written (staged) for a program that fails them.
        if slot_expected and deploy_slot != slot_expected:
            raise SystemExit(
                f"{name}: deploy slot {deploy_slot} != {slot_expected} (the pinned slot; for a "
                "venue, ARCH §8.2 / VENUE_GUARD_SLOTS_*): the program was upgraded; dump it as a "
                "new group and move the pins deliberately (`check --live` names what drifted)"
            )
        if use and sha256(trimmed) != trimmed_expected:
            raise SystemExit(f"{name}: trimmed ELF sha256 {sha256(trimmed)} != the pinned "
                             f"{trimmed_expected}")
        if trimmed_expected and sha256(trimmed) != trimmed_expected:
            print(
                f"WARNING: {name}: trimmed ELF sha256 {sha256(trimmed)} differs from R-RAY §1.3 "
                f"{trimmed_expected} (upgraded since the research?)",
                file=sys.stderr,
            )
        prog_file = f"{account_dir}/{name}_program.bin"
        header_file = f"{account_dir}/{name}_program_data_header.bin"
        elf_file = f"{PROGRAM_DIR}/{name}.so"
        write_file(prog_file, prog["data"])
        write_file(header_file, d[:45])
        write_file(elf_file, elf[:n])
        files += [prog_file, header_file, elf_file]
        entries.append(
            {
                "name": f"{name}_program",
                "group": group,
                "purpose": f"the {name} program account (upgradeable loader; names its ProgramData). "
                + (use[0] if use else "tests/litesvm/src/fork.rs deploys the ELF at the mainnet "
                   "address with it"),
                "address": program,
                "owner": prog["owner"],
                "lamports": prog["lamports"],
                "executable": True,
                "rent_epoch": prog["rent_epoch"],
                "space": len(prog["data"]),
                "slot": res["context"]["slot"],
                "sha256": sha256(prog["data"]),
                "file": prog_file,
                "decoded": {"program_data": program_data},
            }
        )
        tests = "T-RAY-*" if group == "raydium_programs" else "T-PUMP-MATH, M5 / M6"
        stored = ("Stored as header ‖ ELF; the account is header ‖ ELF ‖ zeros up to `space`, "
                  "whose sha256 is `sha256` (fork.rs rebuilds and checks it)")
        entries.append(
            {
                "name": f"{name}_program_data",
                "group": group,
                "purpose": f"{name} ProgramData: {use[1]}. {stored}" if use else
                f"{name} ProgramData: the deployed ELF ({tests}) and the header the "
                "venue-upgrade latch reads (T-LATCH on the dumped ProgramData, ARCH §8.2, §9). "
                + stored,
                "address": program_data,
                "owner": pd["owner"],
                "lamports": pd["lamports"],
                "executable": False,
                "rent_epoch": pd["rent_epoch"],
                "space": len(d),
                "slot": pd_res["context"]["slot"],
                "sha256": sha256(d),
                "files": {"header": header_file, "elf": elf_file},
                "elf_len": n,
                "elf_sha256": sha256(elf[:n]),
                "elf_trimmed_len": len(trimmed),
                "elf_trimmed_sha256": sha256(trimmed),
                **({"elf_trimmed_sha256_expected": trimmed_expected} if trimmed_expected else {}),
                **({"deploy_slot_expected": slot_expected} if slot_expected else {}),
                **({"source": PROGRAM_SOURCES[name]} if name in PROGRAM_SOURCES else {}),
                "decoded": {
                    "deploy_slot": deploy_slot,
                    "upgrade_authority": authority,
                    "e_flags": struct.unpack_from("<I", elf, 0x30)[0],
                },
            }
        )
    snapshot = {
        "group": group,
        "slot": slot_before,
        "fetched_at": now_utc(),
        "rpc": rpc.used,
        "method": "getAccountInfo (base64) of each program and ProgramData account; "
        "`solana program dump -um <program> out.so` gives the same ELF bytes followed by the "
        "zero padding",
    }
    return entries, files, snapshot


def fetch_state(rpc):
    """Every venue-state account in one getMultipleAccounts call (one slot)."""
    # 1. The pools, to learn their derived accounts.
    roots = [a for _, a, _ in V4_POOLS] + [a for _, a, _ in CPMM_POOLS]
    _, vals = rpc.multiple(roots)
    derived = {}
    for (prefix, addr, _), v in zip(V4_POOLS + CPMM_POOLS, vals):
        if v is None:
            raise SystemExit(f"pool {addr} does not exist")
        d = decode_value(v)["data"]
        derived[addr] = decode_amm_info(d) if v["owner"] == AMM_V4 else decode_pool_state(d)
    # 2. Every CPMM AmmConfig (the 21 live configs, R-RAY §2.5).
    cfgs = rpc.call(
        "getProgramAccounts",
        [
            CPMM,
            {
                "encoding": "base64",
                "dataSlice": {"offset": 10, "length": 2},
                "filters": [
                    {"dataSize": 236},
                    {"memcmp": {"offset": 0, "bytes": b58encode(CPMM_CONFIG_DISC)}},
                ],
            },
        ],
    )
    configs = sorted(
        (struct.unpack("<H", base64.b64decode(c["account"]["data"][0]))[0], c["pubkey"])
        for c in cfgs
    )

    # 3. The account list, with names and purposes (first name wins for shared accounts).
    plan = []  # (name, address, purpose)
    seen = {}

    def add(name, addr, purpose):
        if addr in seen:
            return
        seen[addr] = name
        plan.append((name, addr, purpose))

    add("mint_wsol", WSOL, "SPL Token native mint (WSOL): the SOL side of every Raydium pool")
    add("ray_v4_authority", AMM_V4_AUTHORITY,
        "AMM v4 vault / LP authority `create_program_address([\"amm authority\", 254])` "
        "(a system account; swaps pass it read-only)")
    add("ray_v4_amm_config", AMM_V4_CONFIG,
        "AMM v4 AmmConfig (pnl_owner, create_pool_fee; R-RAY §1.1): not read by swaps or by "
        "the route predicates, kept for admin-action tests")
    add("cpmm_authority", CPMM_AUTHORITY,
        "CPMM vault / LP authority PDA [\"vault_and_lp_mint_auth_seed\"] (a system account)")
    for prefix, addr, purpose in V4_POOLS:
        f = derived[addr]
        sol_coin = f["coin_vault_mint"] == WSOL
        tok = f["pc_vault_mint"] if sol_coin else f["coin_vault_mint"]
        add(f"{prefix}_pool", addr, f"AmmInfo. {purpose}")
        add(f"{prefix}_coin_vault", f["coin_vault"],
            f"coin vault of {prefix} ({'WSOL' if sol_coin else 'token'}); reserves = amount - need_take_pnl_coin")
        add(f"{prefix}_pc_vault", f["pc_vault"],
            f"pc vault of {prefix} ({'token' if sol_coin else 'WSOL'}); reserves = amount - need_take_pnl_pc")
        add("mint_fartcoin" if prefix.startswith("ray_v4_fartcoin") else f"{prefix}_token_mint", tok,
            "the token mint of the AMM v4 Fartcoin pools (SPL Token)")
        add(f"{prefix}_lp_mint", f["lp_mint"], f"LP mint of {prefix} (supply for the locked share λ)")
    for prefix, addr, purpose in CPMM_POOLS:
        f = derived[addr]
        add(f"{prefix}_pool", addr, f"PoolState. {purpose}")
        add(f"{prefix}_token_0_vault", f["token_0_vault"],
            f"token 0 vault of {prefix}; reserves = amount - (protocol + fund + creator)_0")
        add(f"{prefix}_token_1_vault", f["token_1_vault"],
            f"token 1 vault of {prefix}; reserves = amount - (protocol + fund + creator)_1")
        for side in ("token_0_mint", "token_1_mint"):
            if f[side] != WSOL:
                add(f"{prefix}_token_mint", f[side], f"the token mint of {prefix}")
        add(f"{prefix}_lp_mint", f["lp_mint"], f"LP mint of {prefix} (supply for the locked share λ)")
        add(f"{prefix}_observation", f["observation_key"],
            f"ObservationState of {prefix} (written by every swap; mandatory swap account)")
    for index, addr in configs:
        add(f"cpmm_config_{index:02}", addr,
            f"CPMM AmmConfig index {index} (R-RAY §2.5): fee variants for T-RAY-MATH / T-RAY-FEE")
    if len(plan) > 100:
        raise SystemExit(f"{len(plan)} accounts do not fit one getMultipleAccounts call")

    # 4. One call, one slot.
    slot, vals = rpc.multiple([a for _, a, _ in plan])
    block_time = None
    try:
        block_time = rpc.call("getBlockTime", [slot])
    except SystemExit as e:
        print(f"WARNING: getBlockTime({slot}): {e}", file=sys.stderr)
    entries, files = [], []
    for (name, addr, purpose), v in zip(plan, vals):
        if v is None:
            raise SystemExit(f"{name} {addr} does not exist at slot {slot}")
        a = decode_value(v)
        d = a["data"]
        decoded = None
        if a["owner"] == AMM_V4 and len(d) == 752:
            decoded = decode_amm_info(d)
        elif a["owner"] == CPMM and d[:8] == CPMM_POOL_DISC:
            decoded = decode_pool_state(d)
        elif a["owner"] == CPMM and d[:8] == CPMM_CONFIG_DISC:
            decoded = decode_amm_config(d)
        elif (a["owner"] in (TOKEN, TOKEN_2022) and len(d) == 82) or (
            a["owner"] == TOKEN_2022 and len(d) > 165 and d[165] == 1
        ):
            decoded = decode_mint(d, a["owner"])
        elif a["owner"] in (TOKEN, TOKEN_2022) and len(d) >= 165:
            decoded = decode_token_account(d)
        elif a["owner"] == CPMM and len(d) == CPMM_OBSERVATION_LEN:
            decoded = {"pool_id": key_at(d, 11)}
        # The pools must still name what step 1 derived (all these keys are immutable).
        if addr in derived:
            for k, want in derived[addr].items():
                if k.endswith(("vault", "mint", "_key", "config", "program")) and decoded[k] != want:
                    raise SystemExit(f"{name}: {k} changed between the two reads")
        rel = f"{ACCOUNT_DIR}/{name}.bin"
        write_file(rel, d)
        files.append(rel)
        e = {
            "name": name,
            "group": "raydium_state",
            "purpose": purpose,
            "address": addr,
            "owner": a["owner"],
            "lamports": a["lamports"],
            "executable": a["executable"],
            "rent_epoch": a["rent_epoch"],
            "space": len(d),
            "slot": slot,
            "sha256": sha256(d),
            "file": rel,
        }
        if decoded is not None:
            e["decoded"] = decoded
        entries.append(e)
    snapshot = {
        "group": "raydium_state",
        "slot": slot,
        "block_time": block_time,
        "block_time_utc": dt.datetime.fromtimestamp(block_time, dt.UTC).strftime(
            "%Y-%m-%dT%H:%M:%SZ") if block_time else None,
        "fetched_at": now_utc(),
        "rpc": rpc.used,
        "method": "one getMultipleAccounts call (base64) over every account below, so all of "
        "them are the mainnet state at `slot`; the CPMM configs were listed first with "
        "getProgramAccounts(CPMM, dataSize 236)",
    }
    return entries, files, snapshot


def pump_coins(overrides):
    """`PUMP_COINS` with `role=MINT` overrides applied."""
    over = dict(o.split("=", 1) for o in overrides or [])
    unknown = (set(over) - {r for r, _, _, _ in PUMP_COINS}
               - {r for r, _, _, _ in PUMP_COINS_2026_10_13})
    if unknown:
        raise SystemExit(f"unknown coin roles: {sorted(unknown)}")
    return [(r, over.get(r, mint), x, p) for r, mint, x, p in PUMP_COINS]


def cmd_fetch(args):
    m = load_manifest()
    have = {s["group"] for s in m.get("snapshots", [])}
    groups = [g for g in GROUPS if not args.group or g in args.group]
    todo = [g for g in groups if args.refresh or g not in have or check(m, only=g)]
    if "pamm_state" in todo and "pamm_synthetic" not in todo:
        todo.append("pamm_synthetic")  # derived from pamm_state's bytes
    if not todo:
        print("fixtures complete and valid; nothing to fetch (use --refresh to re-dump)")
        return 0
    rpc = Rpc([args.rpc] if args.rpc else RPCS)
    for g in todo:
        if g == "raydium_programs":
            entries, files, snap = fetch_programs(rpc)
        elif g == "raydium_state":
            entries, files, snap = fetch_state(rpc)
        elif g == "pump_programs":
            entries, files, snap = fetch_programs(rpc, PUMP_PROGRAMS, g, PUMP_ACCOUNT_DIR)
        elif g == "pump_state":
            entries, files, snap = fetch_pump_state(rpc, pump_coins(args.coin))
        elif g == "pamm_state":
            entries, files, snap = fetch_pamm_state(rpc, pamm_pools(args.pool, rpc))
        elif g == "raydium_cpmm_2026_10":
            entries, files, snap = fetch_programs(rpc, CPMM_PROGRAMS_2026_10, g,
                                                  CPMM_2026_10_ACCOUNT_DIR)
        elif g == "pump_programs_2026_10":
            entries, files, snap = fetch_programs(rpc, PUMP_PROGRAMS_2026_10, g,
                                                  PUMP_2026_10_ACCOUNT_DIR)
        elif g == "pump_programs_2026_10_13":
            entries, files, snap = fetch_programs(rpc, PUMP_PROGRAMS_2026_10_13, g,
                                                  PUMP_2026_10_13_ACCOUNT_DIR)
        elif g == "pump_programs_2026_10_13b":
            entries, files, snap = fetch_programs(rpc, PUMP_PROGRAMS_2026_10_13B, g,
                                                  PUMP_2026_10_13B_ACCOUNT_DIR)
        elif g == PUMP_STATE_2026_10_13:
            entries, files, snap = fetch_pump_state_2026_10_13(
                rpc, pump_coins_2026_10_13(args.coin, rpc), m)
        elif g == "spl_programs":
            entries, files, snap = fetch_spl(rpc)
        elif g == "squads_programs":
            entries, files, snap = fetch_squads(rpc)
        else:
            entries, files, snap = synthesize_pamm(m)
        m["accounts"] = [e for e in m["accounts"] if e.get("group") != g] + entries
        m["snapshots"] = [s for s in m.get("snapshots", []) if s["group"] != g] + [snap]
        print(f"{g}: {len(entries)} accounts at slot {snap['slot']}")
    m["description"] = (
        "Mainnet accounts, programs and transactions dumped for the program tests (ARCHITECTURE "
        "§1.2, §8.2). Each entry records how it was fetched; SHA256SUMS pins the bytes (CI: cd "
        "tests/fixtures && sha256sum -c SHA256SUMS). The Raydium entries (groups raydium_programs "
        "and raydium_state), the pump entries (groups pump_programs and pump_state) and the "
        "PumpSwap entries (group pamm_state, and pamm_synthetic: pools derived offline from "
        "pamm_state bytes, marked `synthetic`) and the M10 program groups (raydium_cpmm_2026_10: "
        "the CPMM redeploy; spl_programs: the mainnet Token-2022 and spl-memo; squads_programs: "
        "Squads v4, its ProgramConfig and the vendored IDL; pump_programs_2026_10: the pump, "
        "pump-fees and PumpSwap redeploys of 2026-10-02; pump_programs_2026_10_13: those of "
        "2026-10-08/12; pump_programs_2026_10_13b: pump's of 2026-10-13) and the state those "
        "introduced (group pump_state_2026_10_13) are written "
        "by scripts/dump-fixtures.py; `snapshots` records the slot and RPC of each group "
        "(`vendored`: files taken from a pinned source)."
    )
    commit_files()
    write_manifest(m)
    managed_files = vendored_files(m)
    for e in m["accounts"]:
        if managed(e):
            managed_files += entry_files(e)
    write_sums(managed_files)
    # A role whose account was present at an earlier dump and is absent now leaves a stale file.
    for rel_dir in (PUMP_ACCOUNT_DIR, PAMM_ACCOUNT_DIR, PUMP_2026_10_13_ACCOUNT_DIR):
        full = os.path.join(FIXTURES, rel_dir)
        if os.path.isdir(full):
            for f in os.listdir(full):
                if f"{rel_dir}/{f}" not in managed_files:
                    os.remove(os.path.join(full, f))
    # `--group` checks what it fetched (a group not fetched yet is no failure of this fetch).
    m = load_manifest()
    problems = [p for g in todo for p in check(m, only=g)] if args.group else check(m)
    for p in problems:
        print("FAIL:", p, file=sys.stderr)
    return 1 if problems else 0


# ---- check ----------------------------------------------------------------------------------


def check(m, only=None):
    """Offline checks; returns a list of problems (empty = ok)."""
    problems = []
    entries = [e for e in m["accounts"] if managed(e) and (only is None or e["group"] == only)]
    if not entries:
        return [f"no entries for {only or 'the managed groups'}"]
    if only is None:
        have = {e["group"] for e in entries}
        problems += [f"no entries for {g}" for g in GROUPS if g not in have]
    by_addr = {e["address"]: e for e in m["accounts"] if managed(e)}
    sums = {}
    if os.path.exists(SUMS):
        with open(SUMS) as f:
            for line in f:
                h, p = line.rstrip("\n").split("  ", 1)
                sums[p] = h
    for e in entries:
        if e.get("absent"):
            if "file" in e or "sha256" in e:
                problems.append(f"{e['name']}: an absent account with a file")
            continue
        try:
            if "file" in e:
                d = read_file(e["file"])
                if sha256(d) != e["sha256"] or len(d) != e["space"]:
                    problems.append(f"{e['file']}: bytes differ from the manifest")
                if sums.get(e["file"]) != sha256(d):
                    problems.append(f"{e['file']}: missing or wrong in SHA256SUMS")
                names = (e.get("decoded") or {}).get("program_data")
                if (e["owner"] == LOADER_UPGRADEABLE and names
                        and (d[:4] != b"\x02\0\0\0" or key_at(d, 4) != names)):
                    problems.append(f"{e['name']}: the program account does not name its decoded "
                                    "ProgramData")
            else:
                header = read_file(e["files"]["header"])
                elf = read_file(e["files"]["elf"])
                full = header + elf + bytes(e["space"] - len(header) - len(elf))
                if sha256(full) != e["sha256"]:
                    problems.append(f"{e['name']}: header ‖ ELF ‖ zeros != the mainnet account")
                if sha256(elf) != e["elf_sha256"] or len(elf) != e["elf_len"]:
                    problems.append(f"{e['files']['elf']}: ELF differs from the manifest")
                if ("elf_trimmed_sha256_expected" in e
                        and sha256(elf.rstrip(b"\0")) != e["elf_trimmed_sha256_expected"]):
                    problems.append(f"{e['name']}: trimmed ELF hash != its pin (raydium_programs: "
                                    "R-RAY §1.3)")
                if ("deploy_slot_expected" in e
                        and u64_at(header, 4) != e["deploy_slot_expected"]):
                    problems.append(f"{e['name']}: deploy slot != its pin (venues: ARCH §8.2)")
                if u64_at(header, 4) != e["decoded"]["deploy_slot"]:
                    problems.append(f"{e['name']}: deploy slot differs from the header")
                authority = key_at(header, 13) if header[12] == 1 else None
                if (header[:4] != b"\x03\0\0\0" or header[12] not in (0, 1)
                        or authority != e["decoded"]["upgrade_authority"]):
                    problems.append(f"{e['name']}: upgrade authority differs from the header "
                                    "(`check --live` compares mainnet with the decoded one)")
                for p in e["files"].values():
                    if sums.get(p) != sha256(read_file(p)):
                        problems.append(f"{p}: missing or wrong in SHA256SUMS")
        except FileNotFoundError as ex:
            problems.append(f"{e['name']}: {ex}")
    if only in (None, "raydium_state"):
        # Cross-references: every account a pool names is in the fixture set with the
        # owner the swap requires.
        for e in entries:
            dec = e.get("decoded") or {}
            if e.get("owner") == AMM_V4 and "coin_vault" in dec:
                for k in ("coin_vault", "pc_vault", "coin_vault_mint", "pc_vault_mint", "lp_mint"):
                    if dec[k] not in by_addr:
                        problems.append(f"{e['name']}: {k} {dec[k]} is not a fixture")
                for k, mk in (("coin_vault", "coin_vault_mint"), ("pc_vault", "pc_vault_mint")):
                    v = by_addr.get(dec[k], {}).get("decoded", {})
                    if v.get("mint") != dec[mk] or v.get("owner") != AMM_V4_AUTHORITY:
                        problems.append(f"{e['name']}: {k} mint/authority mismatch")
            if e.get("owner") == CPMM and "token_0_vault" in dec:
                for k in ("amm_config", "token_0_vault", "token_1_vault", "token_0_mint",
                          "token_1_mint", "lp_mint", "observation_key"):
                    if dec[k] not in by_addr:
                        problems.append(f"{e['name']}: {k} {dec[k]} is not a fixture")
                for i in "01":
                    v = by_addr.get(dec[f"token_{i}_vault"], {})
                    if (v.get("decoded", {}).get("mint") != dec[f"token_{i}_mint"]
                            or v.get("decoded", {}).get("owner") != CPMM_AUTHORITY
                            or v.get("owner") != dec[f"token_{i}_program"]):
                        problems.append(f"{e['name']}: token_{i} vault mint/authority/program mismatch")
        slots = {e["slot"] for e in entries if e["group"] == "raydium_state"}
        if len(slots) > 1:
            problems.append(f"raydium_state spans several slots: {sorted(slots)}")
    if only in (None, "pump_state") and any(e["group"] == "pump_state" for e in entries):
        problems += check_pump(m)
    if only in (None, PUMP_STATE_2026_10_13) and any(
            e["group"] == PUMP_STATE_2026_10_13 for e in entries):
        problems += check_pump_2026_10_13(m)
    if only in (None, "pamm_state", "pamm_synthetic") and any(
            e["group"] in PAMM_GROUPS for e in entries):
        problems += check_pamm(m, only)
    for sn in m.get("snapshots", []):
        if sn["group"] not in GROUPS or only not in (None, sn["group"]):
            continue
        for v in sn.get("vendored", []):
            try:
                d = read_file(v["file"])
            except FileNotFoundError as ex:
                problems.append(f"{sn['group']}: {ex}")
                continue
            if sha256(d) != v["sha256"] or len(d) != v["bytes"]:
                problems.append(f"{v['file']}: bytes differ from the manifest")
            if sums.get(v["file"]) != sha256(d):
                problems.append(f"{v['file']}: missing or wrong in SHA256SUMS")
    if only is None or only in M10_PROGRAM_GROUPS:
        problems += check_m10(m, only)
    return problems


# ---- check --live (mainnet drift; read-only) ----------------------------------------------------

CONSTANTS_RS = os.path.normpath(
    os.path.join(HERE, "..", "programs", "shortwin", "src", "constants.rs"))
LIVE_PACE_S = 1.0  # between the large reads, to stay gentle on the public endpoints


def venue_guard_baseline():
    """The `VenueGuard` programs as ``constants.rs`` pins them: (name of the newest
    ``VENUE_GUARD_SLOTS_YYYY_MM_DD``, [(label, program, ProgramData, baseline slot)])."""
    with open(CONSTANTS_RS) as f:
        src = f.read()
    consts = re.findall(r"pub const (VENUE_GUARD_SLOTS_(\d{4}_\d{2}_\d{2})): \[u64; \w+\] = "
                        r"\[([^\]]*)\];", src)
    if not consts:
        raise SystemExit(f"{CONSTANTS_RS}: no VENUE_GUARD_SLOTS_YYYY_MM_DD constant")
    name, _, body = max(consts, key=lambda c: c[1])
    slots = [int(x.replace("_", "")) for x in re.findall(r"\d[\d_]*", body)]

    def array(const):
        m = re.search(rf"pub const {const}: \[Pubkey; \w+\] = \[(.*?)\];", src, re.S)
        if not m:
            raise SystemExit(f"{CONSTANTS_RS}: no {const}")
        return m.group(1)

    ids = re.findall(r"\b([A-Z0-9_]+_PROGRAM_ID)\b", array("VENUE_PROGRAM_IDS"))
    pds = re.findall(r'pubkey!\("(\w+)"\)', array("VENUE_PROGRAM_DATA"))
    programs = []
    for const in ids:
        m = re.search(rf'pub const {const}: Pubkey = pubkey!\("(\w+)"\);', src)
        if not m:
            raise SystemExit(f"{CONSTANTS_RS}: no {const}")
        programs.append((const.removesuffix("_PROGRAM_ID").lower(), m.group(1)))
    if not len(programs) == len(pds) == len(slots):
        raise SystemExit(f"{CONSTANTS_RS}: VenueGuard arrays of different lengths")
    return name, [(lb, p, pd_, sl) for (lb, p), pd_, sl in zip(programs, pds, slots)]


def _fmt_slot(slot):
    return f"{slot:,}" if slot is not None else "-"


def cmd_check_live(args):
    """Mainnet against the pins: exit 1 naming every drifted program (read-only RPC)."""
    m = load_manifest()
    rpc = Rpc([args.rpc] if args.rpc else RPCS)
    baseline_name, venues = venue_guard_baseline()
    # Every ProgramData fixture by address; the newest deploy is the one a program is held to.
    fixtures = {}
    for e in m["accounts"]:
        if managed(e) and "files" in e and "elf_sha256" in e:
            fixtures.setdefault(e["address"], []).append(e)
    for v in fixtures.values():
        v.sort(key=lambda e: e["decoded"]["deploy_slot"], reverse=True)
    targets = [(lb, p, pd_, sl) for lb, p, pd_, sl in venues]
    seen = {p for _, p, _, _ in targets}
    for name, program, program_data, _, _ in (CPMM_PROGRAMS_2026_10 + SPL_PROGRAMS + SQUADS_PROGRAMS
                                              + PUMP_PROGRAMS_2026_10 + PUMP_PROGRAMS_2026_10_13
                                              + PUMP_PROGRAMS_2026_10_13B):
        if program not in seen:
            targets.append((name, program, program_data, None))
            seen.add(program)
    by_name = {e["name"]: e for e in m["accounts"] if managed(e)}
    # (label, manifest name, address) of the accounts compared byte for byte.
    plain = [(n, f"{n}_program", p) for n, p, _ in LOADER2_PROGRAMS]
    plain.append(("squads_program_config", "squads_program_config", SQUADS_PROGRAM_CONFIG))

    # 1. The program accounts and the plain accounts (one call); 2. the ProgramData headers.
    slot, accs = rpc.multiple([p for _, p, _, _ in targets] + [a for _, _, a in plain])
    time.sleep(LIVE_PACE_S)
    _, heads = rpc.multiple([pd_ for _, _, pd_, _ in targets], data_slice=(0, 45))
    print(f"check --live at slot {slot:,} ({rpc.used}); VenueGuard baseline {baseline_name} "
          f"({os.path.relpath(CONSTANTS_RS)})")
    drifted = []
    for (label, program, program_data, base), acc, head in zip(targets, accs, heads):
        why, notes = [], []
        if acc is None or head is None:
            drifted.append(label)
            print(f"  {label:<24} DRIFT: program or ProgramData account missing")
            continue
        a = decode_value(acc)
        if (a["owner"] != LOADER_UPGRADEABLE or a["data"][:4] != b"\x02\0\0\0"
                or b58encode(a["data"][4:36]) != program_data):
            why.append(f"the program account no longer names {program_data}")
        h = decode_value(head)["data"]
        live_slot = u64_at(h, 4)
        live_auth = key_at(h, 13) if h[12] == 1 else None
        notes.append(f"deploy {_fmt_slot(live_slot)}")
        if base is not None:
            notes.append(f"baseline {_fmt_slot(base)}")
            if live_slot != base:
                why.append(f"deploy slot {_fmt_slot(live_slot)} != {baseline_name} "
                           f"{_fmt_slot(base)}")
        fx = fixtures.get(program_data, [])
        if not fx:
            notes.append("no fixture")
        else:
            cur = fx[0]
            notes.append(f"fixture {cur['group']} {_fmt_slot(cur['decoded']['deploy_slot'])}")
            if len(fx) > 1:
                notes.append("kept: " + ", ".join(
                    f"{e['group']} {_fmt_slot(e['decoded']['deploy_slot'])}" for e in fx[1:]))
            if live_slot != cur["decoded"]["deploy_slot"]:
                why.append(f"deploy slot {_fmt_slot(live_slot)} != fixture {cur['group']} "
                           f"{_fmt_slot(cur['decoded']['deploy_slot'])}")
            else:
                # Same deploy: confirm the bytes (one read per program, paced).
                time.sleep(LIVE_PACE_S)
                full = rpc.call("getAccountInfo", [program_data, {"encoding": "base64"}])
                elf = decode_value(full["value"])["data"][45:]
                live_trim = sha256(elf.rstrip(b"\0"))
                if live_trim != cur["elf_trimmed_sha256"]:
                    why.append(f"trimmed ELF {live_trim[:8]}… != fixture "
                               f"{cur['elf_trimmed_sha256'][:8]}…")
                else:
                    notes.append(f"ELF = fixture ({live_trim[:8]}…)")
            if live_auth != cur["decoded"]["upgrade_authority"]:
                why.append(f"upgrade authority {live_auth} != fixture "
                           f"{cur['decoded']['upgrade_authority']}")
        verdict = "DRIFT: " + "; ".join(why) if why else "unchanged"
        if why:
            drifted.append(label)
        print(f"  {label:<24} {verdict}\n  {'':<24} ({', '.join(notes)})")
    for (label, name, _), acc in zip(plain, accs[len(targets):]):
        e = by_name.get(name)
        if e is None:
            drifted.append(label)
            print(f"  {label:<24} DRIFT: no fixture `{name}` (run fetch)")
            continue
        if acc is None:
            drifted.append(label)
            print(f"  {label:<24} DRIFT: the account no longer exists")
            continue
        a = decode_value(acc)
        if sha256(a["data"]) != e["sha256"] or a["owner"] != e["owner"]:
            drifted.append(label)
            print(f"  {label:<24} DRIFT: account bytes {sha256(a['data'])[:8]}… / owner "
                  f"{a['owner']} != fixture {e['sha256'][:8]}… / {e['owner']}")
        else:
            print(f"  {label:<24} unchanged\n  {'':<24} (account sha256 = fixture "
                  f"{e['sha256'][:8]}…, owner {a['owner']})")
    sys.stdout.flush()
    if drifted:
        print(f"DRIFT: {', '.join(drifted)}", file=sys.stderr)
        return 1
    print("ok: every pinned program and account matches mainnet")
    return 0


def cmd_check(_args):
    problems = check(load_manifest())
    for p in problems:
        print("FAIL:", p, file=sys.stderr)
    if not problems:
        n = sum(1 for e in load_manifest()["accounts"] if managed(e))
        print(f"ok: {n} Raydium, pump, PumpSwap and M10 program fixture entries match their "
              "files, SHA256SUMS, their pins and each other")
    return 1 if problems else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd")
    ck = sub.add_parser("check", help="offline verification (default)")
    ck.add_argument("--live", action="store_true",
                    help="instead: compare the pinned programs with mainnet (read-only RPC)")
    ck.add_argument("--rpc", help="JSON-RPC endpoint for --live (default: mainnet-beta, then "
                    "publicnode)")
    f = sub.add_parser("fetch", help="dump from mainnet what is missing or invalid")
    f.add_argument("--refresh", action="store_true", help="re-dump every group at a new slot")
    f.add_argument("--rpc", help="JSON-RPC endpoint (default: mainnet-beta, then publicnode)")
    f.add_argument("--group", action="append", choices=GROUPS,
                   help="only this group (repeatable; default: all)")
    f.add_argument("--pool", action="append", metavar="ROLE=MINT",
                   help="replace a PumpSwap pool role's mint (pamm_state; `fresh=latest` takes "
                   "the newest boosted migration; `negative_vqr=MINT` adds that role)")
    f.add_argument("--coin", action="append", metavar="ROLE=MINT",
                   help="replace a pump coin role's mint (pump_state; repeatable)")
    c = sub.add_parser("candidates", help="mainnet: replacements for the pump coin roles")
    c.add_argument("--blocks", type=int, default=10, help="recent blocks to scan for creates")
    c.add_argument("--near", action="store_true",
                   help="also list live curves nearest completion (getProgramAccounts; slow)")
    c.add_argument("--rpc", help="JSON-RPC endpoint (default: mainnet-beta, then publicnode)")
    args = ap.parse_args()
    if args.cmd == "fetch":
        return cmd_fetch(args)
    if args.cmd == "candidates":
        return cmd_candidates(args)
    if getattr(args, "live", False):
        return cmd_check_live(args)
    return cmd_check(args)


if __name__ == "__main__":
    sys.exit(main())
