"""ARCH v0.4.1 §6.1: math CU per instruction from the measured shortwin-math primitives (SBF v3,
platform-tools v1.57; scratchpad/cubench) and counted primitive calls per the v0.4.1 procedures.
v0.4.1: sol_settle sorts two median pairs (bucket lows and highs, R4B-2); p' per corner is one CR
comparison (2 x MD64); liquidate drops pi_ref and adds the lo_x cap (one MD); withdraw adds cal_advance."""
import json, os
m = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "cu_math", "results.json")))
P = {
 "MD": m["mul_div u128 fast path (quote n)"], "MD256": m["mul_div U256 fallback (256/128 Knuth D)"],
 "MD64": m["mul_div_u64 (crate: via u128 division)"], "Qps": m["PumpSwap buy_exact_out (3 fee comps)"],
 "Qcp": m["CPMM buy_exact_out"], "Qv4": m["AMM v4 buy_exact_out"], "RP": m["reprice(L,P) (isqrt of P + 2 mul_div)"],
 "SQ": m["L = isqrt u128 (v0.4, X*Y < 2^128)"], "RD": m["rent::due (U256 position integral)"],
 "ACC": m["rent::accrue (3 basis increments)"], "ROLL": m["record + integrate (one EMA roll, ring push)"],
 "MED": m["obs_median, full 8-entry ring"], "SIG": m["sigma_xy"], "CF": m["rent::credited_for (U256)"],
 "LEG3": m["v0.3 EMA-leg entry: reprice + quote + CR"], "LEG4": m["v0.4 EMA-leg entry: cached (x,y) quote + CR"],
}
Q = P["Qps"]   # PumpSwap: the most expensive quote family on the hot pump path (CPMM +420)
# composite blocks
prologue = P["ACC"] + 3*P["MD"] + P["ROLL"] + P["RP"] + P["MD"] + P["SQ"] + 2*P["MD"]   # accrue+vest+credit, roll(+cache), spot P, L, fee tier
pool_exit = P["CF"] + 3*P["MD"] + P["MD"]           # credited_for, credit/credit_r/vest, resolve
blocks = {
 "open_short": prologue + 3*P["MD"] + 7*P["MD"] + P["SQ"] + Q + Q + Q + P["MD64"]*3 + P["RD"],
 "close_short": prologue + P["RD"] + Q + pool_exit,
 "liquidate (v0.4.1)": prologue + P["RD"] + 2*Q + (P["RP"] + Q) + 3*P["LEG4"] + P["SIG"] + (3*P["MD"] + 2*P["MD"] + P["RP"] + Q) + 7*P["MD64"] + pool_exit,
 "liquidate (v0.3 worst: EMA run 8 + exec run 8, no cache)": prologue - P["RP"] + P["RD"] + 2*Q + (P["RP"] + Q) + 16*P["LEG3"] + P["SIG"] + (3*P["MD"] + 2*P["MD"] + P["RP"] + Q) + 7*P["MD64"] + pool_exit,
 "expire": prologue + P["RD"] + 2*Q + (P["RP"] + Q) + 3*P["LEG4"] + P["SIG"] + (P["RP"] + Q) + 7*P["MD64"] + pool_exit,
 "sol_settle (armed, S2/S3 amount path)": prologue + P["RD"] + (P["RP"] + Q) + 3*P["LEG4"] + 2*P["MED"] + (P["RP"] + Q + P["SIG"]) + (P["RP"] + Q) + (P["RP"] + Q + P["SIG"]) + Q + 2*P["SIG"] + Q + 6*P["MD64"] + 2*P["MD64"] + pool_exit + 2*P["MD"],
 "crank_price": prologue + P["RP"] + 4*P["MD"],
 "deposit / withdraw (touch, allowance, locks, shares)": P["ACC"] + 3*P["MD"] + 2*P["MD256"] + 3*P["MD"] + 4*P["MD256"] + 2*P["MD256"],
 "withdraw extra: cal_advance (one day elapsed)": 2*P["MD64"],
 "claim_sol (touch + pot + claim)": 2*P["MD256"] + 2*P["MD"],
}
if __name__ == "__main__":
    print("primitive CU:", {k: v for k, v in P.items()})
    for k, v in blocks.items():
        print(f"{k:58s} {v/1000:6.1f} k CU")
