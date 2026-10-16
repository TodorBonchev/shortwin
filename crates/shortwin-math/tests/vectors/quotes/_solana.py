"""Shared helpers for the quote-track generators: base58, a minimal Borsh reader driven
by an Anchor IDL, instruction iteration and log attribution. Standard library only."""
import base64
import hashlib
import json
import struct
import urllib.request

ALPH = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

PUMP = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PAMM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
AMM_V4 = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
CPMM = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C"
WSOL = "So11111111111111111111111111111111111111112"
ANCHOR_EVENT_IX = bytes.fromhex("e445a52e51cb9a1d")


def b58d(s):
    n = 0
    for c in s:
        n = n * 58 + ALPH.index(c)
    b = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    return b"\0" * (len(s) - len(s.lstrip("1"))) + b


_P = 2**255 - 19
_D = (-121665 * pow(121666, _P - 2, _P)) % _P


def on_curve(b):
    """Whether 32 bytes decompress to an ed25519 point (PDAs must not)."""
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


def find_pda(seeds, program):
    prog = b58d(program)
    for bump in range(255, -1, -1):
        h = hashlib.sha256(b"".join(seeds) + bytes([bump]) + prog + b"ProgramDerivedAddress").digest()
        if not on_curve(h):
            return b58e(h)
    raise ValueError("no PDA")


def b58e(b):
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = ALPH[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


class Idl:
    """Borsh decoding of the instructions, accounts and events of an Anchor IDL."""

    FIXED = {"bool": ("<?", 1), "u8": ("<B", 1), "u16": ("<H", 2), "u32": ("<I", 4),
             "u64": ("<Q", 8), "i64": ("<q", 8)}

    def __init__(self, path):
        idl = json.load(open(path))
        self.types = {t["name"]: t for t in idl.get("types", [])}
        self.events = {bytes(e["discriminator"]): e["name"] for e in idl.get("events", [])}
        self.ixs = {bytes(i["discriminator"]): i for i in idl.get("instructions", [])}

    def dec(self, t, b, o):
        if isinstance(t, str):
            if t in self.FIXED:
                f, n = self.FIXED[t]
                if o + n > len(b):
                    raise EOFError
                return struct.unpack_from(f, b, o)[0], o + n
            if t in ("u128", "i128"):
                if o + 16 > len(b):
                    raise EOFError
                return int.from_bytes(b[o:o + 16], "little", signed=t == "i128"), o + 16
            if t == "pubkey":
                if o + 32 > len(b):
                    raise EOFError
                return b58e(b[o:o + 32]), o + 32
            if t == "string":
                n = struct.unpack_from("<I", b, o)[0]
                return b[o + 4:o + 4 + n].decode(), o + 4 + n
            raise ValueError(t)
        if "array" in t:
            et, n = t["array"]
            out = []
            for _ in range(n):
                v, o = self.dec(et, b, o)
                out.append(v)
            return out, o
        if "vec" in t:
            n = struct.unpack_from("<I", b, o)[0]
            o += 4
            out = []
            for _ in range(n):
                v, o = self.dec(t["vec"], b, o)
                out.append(v)
            return out, o
        if "option" in t:
            if b[o] == 0:
                return None, o + 1
            return self.dec(t["option"], b, o + 1)
        if "defined" in t:
            td = self.types[t["defined"]["name"]]["type"]
            if td["kind"] == "struct":
                return self.struct(td, b, o)
            if td["kind"] == "enum":
                return td["variants"][b[o]]["name"], o + 1
        raise ValueError(str(t))

    def struct(self, td, b, o):
        """Decodes a struct; trailing fields missing from `b` decode as None."""
        fields = td["fields"]
        named = bool(fields) and isinstance(fields[0], dict)
        out = {} if named else []
        for fd in fields:
            try:
                v, o = self.dec(fd["type"] if named else fd, b, o)
            except EOFError:
                v = None
            if named:
                out[fd["name"]] = v
            else:
                out.append(v)
        return out, o

    def event(self, data):
        """An `emit_cpi!` event (instruction data after the 8-byte event-ix tag)."""
        name = self.events.get(data[8:16])
        if not name:
            return None, None
        return name, self.struct(self.types[name]["type"], data, 16)[0]

    def ix(self, data):
        i = self.ixs.get(data[:8])
        if not i:
            return None, None
        out, o = {}, 8
        for a in i["args"]:
            try:
                out[a["name"]], o = self.dec(a["type"], data, o)
            except (EOFError, struct.error, IndexError):
                out[a["name"]] = None
        return i["name"], out


def load_tx(path):
    """A `getTransaction` result (raw RPC envelope or bare), or None for an error/null."""
    raw = json.load(open(path))
    t = raw.get("result") if isinstance(raw, dict) and "jsonrpc" in raw else raw
    if not t or t["meta"]["err"] is not None:
        return None
    return t


def keys_of(t):
    m = t["transaction"]["message"]
    la = t["meta"].get("loadedAddresses") or {}
    return list(m["accountKeys"]) + la.get("writable", []) + la.get("readonly", [])


def instructions(t):
    """All instructions in execution order: (program, data bytes, account keys)."""
    keys = keys_of(t)
    inner = {g["index"]: g["instructions"] for g in t["meta"].get("innerInstructions") or []}
    for i, ix in enumerate(t["transaction"]["message"]["instructions"]):
        yield keys[ix["programIdIndex"]], b58d(ix["data"]), [keys[a] for a in ix["accounts"]]
        for c in inner.get(i, []):
            yield keys[c["programIdIndex"]], b58d(c["data"]), [keys[a] for a in c["accounts"]]


def attributed_logs(t):
    """(program currently executing, log line) for every log line, from the
    `invoke` / `success` / `failed` markers."""
    stack = []
    for line in t["meta"].get("logMessages") or []:
        parts = line.split()
        if len(parts) >= 3 and parts[0] == "Program" and parts[2] == "invoke":
            stack.append(parts[1])
            continue
        if len(parts) >= 3 and parts[0] == "Program" and parts[2] in ("success", "failed:", "failed"):
            if stack and stack[-1] == parts[1]:
                stack.pop()
            continue
        yield (stack[-1] if stack else None), line


def rpc(method, params, url="https://api.mainnet-beta.solana.com"):
    req = urllib.request.Request(
        url, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"})
    r = json.load(urllib.request.urlopen(req, timeout=60))
    if "error" in r:
        raise RuntimeError(r["error"])
    return r["result"]


def dump(obj, path):
    with open(path, "w") as f:
        f.write(json.dumps(obj, indent=1, sort_keys=True) + "\n")
