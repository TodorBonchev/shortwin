"""Measure per-primitive CU of shortwin-math on a local validator (ARCHITECTURE v0.4 §6.1, V18). See README.md."""
import base64, json, sys, urllib.request, struct, time
RPC = "http://127.0.0.1:18899"
ALPH = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58d(s):
    n = 0
    for c in s: n = n * 58 + ALPH.index(c)
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    pad = len(s) - len(s.lstrip("1"))
    return (b"\0" * pad + b).rjust(32, b"\0")[-32:] if len(b"\0"*pad+b) <= 32 else b
def rpc(m, p):
    r = urllib.request.Request(RPC, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(),
                               headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(r, timeout=30))
def cu(bench, n, payer):
    prog = b58d("Bench111111111111111111111111111111111111111")
    cb = b58d("ComputeBudget111111111111111111111111111111")
    keys = [payer, prog, cb]
    hdr = bytes([1, 0, 2])
    ix0 = bytes([2, 0]) + bytes([5]) + bytes([2]) + struct.pack("<I", 1_400_000)
    data = bytes([bench]) + struct.pack("<I", n)
    ix1 = bytes([1, 0]) + bytes([len(data)]) + data
    msg = hdr + bytes([len(keys)]) + b"".join(keys) + b"\0" * 32 + bytes([2]) + ix0 + ix1
    tx = bytes([1]) + b"\0" * 64 + msg
    r = rpc("simulateTransaction", [base64.b64encode(tx).decode(), {"encoding": "base64", "sigVerify": False,
                                                                   "replaceRecentBlockhash": True}])
    v = r["result"]["value"]
    if v.get("err"):
        return None, v
    return v["unitsConsumed"], None
for _ in range(60):
    try:
        if rpc("getHealth", [])["result"] == "ok": break
    except Exception: pass
    time.sleep(1)
ident = rpc("getIdentity", [])["result"]["identity"]
payer = b58d(ident)
names = {0: "loop baseline", 1: "mul_div u128 fast path (quote n)", 2: "mul_div U256 fallback (256/128 Knuth D)",
         3: "PumpSwap buy_exact_out (3 fee comps)", 4: "CPMM buy_exact_out", 5: "AMM v4 buy_exact_out",
         6: "reprice(L,P) (isqrt of P + 2 mul_div)", 7: "liquidity(X,Y) via U256 isqrt (v0.3 crate)",
         8: "L = isqrt u128 (v0.4, X*Y < 2^128)", 9: "rent::due (U256 position integral)",
         10: "rent::accrue (3 basis increments)", 11: "record + integrate (one EMA roll, ring push)",
         12: "obs_median (sort 8 obs + 8 lobs)", 13: "sigma_xy", 14: "v0.3 EMA-leg entry: reprice + quote + CR",
         15: "v0.4 EMA-leg entry: cached (x,y) quote + CR", 16: "U256 product + /u128 (kappa/g at extreme P)", 17: "mul_div_u64 (crate: via u128 division)", 18: "native u128 (a*b+d-1)/d, d const", 19: "native u64 a*b div_ceil d (sbpfv3 udiv64)", 20: "obs_median, full 8-entry ring", 21: "sustained_growth(3), full ring", 22: "rent::credited_for (U256)"}
N = 50
for _ in range(30):                      # a genesis-loaded program becomes invocable a few slots in
    base0, e = cu(0, 0, payer)
    if base0 is not None: break
    time.sleep(2)
if base0 is None: print("ERR", e); sys.exit(1)
loop = (cu(0, N, payer)[0] - base0) / N
print(f"baseline tx (CB + empty call): {base0} CU; loop overhead {loop:.1f} CU/iter")
res = {}
for b in range(1, 23):
    c0, e0 = cu(b, 0, payer); cN, eN = cu(b, N, payer)
    if cN is None: print(b, "ERR", eN); continue
    per = (cN - c0) / N - loop
    res[b] = per
    print(f"{b:2d} {names[b]:50s} {per:8.0f} CU")
json.dump({names[k]: round(v) for k, v in res.items()}, open(sys.argv[1], "w"), indent=1)
