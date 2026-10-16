#!/usr/bin/env python3
"""Raydium CPMM vectors from the research pass's own port of the program
(R-RAY §3.3–§3.4: `cp_base_input` / `cp_base_output`, written from
`raydium-cp-swap@59fb845a` `curve/calculator.rs` + `curve/fees.rs` and checked against 285
mainnet swaps), copied below with whitespace changes only.

It is an independent oracle for the branch no mainnet swap covers: an exact-out buy with
the creator fee on the output (token) side, where `q` is grossed up first (R-RAY §3.4
"Not covered by a mainnet sample"). Every branch is sampled, with the live configs'
rates (R-RAY §2.5) and random reserves. Writes `port.json` next to itself.
"""
import random
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _solana import dump  # noqa: E402

SEED = 20260927
S = str
U64 = 2**64 - 1


# --- research port (verify_cpmm.py), unchanged --------------------------------------
def ceil_div(a, b): return -(-a // b)


def cp_base_input(amount_in, x, y, tfr, cfr, cf_on_input):
    D = 1_000_000
    if cf_on_input:
        total = ceil_div(amount_in * (tfr + cfr), D); cf = total * cfr // (tfr + cfr); tf = total - cf; net = amount_in - total
        out = net * y // (x + net); return out, tf, cf
    tf = ceil_div(amount_in * tfr, D); net = amount_in - tf
    sw = net * y // (x + net); cf = ceil_div(sw * cfr, D); return sw - cf, tf, cf


def cp_base_output(amount_out, x, y, tfr, cfr, cf_on_input):
    D = 1_000_000
    pre = lambda a, r: a if r == 0 else (a * D + (D - r) - 1) // (D - r)  # noqa: E731
    if cf_on_input: actual = amount_out; cf = 0
    else: actual = pre(amount_out, cfr); cf = actual - amount_out
    sw = ceil_div(x * actual, y - actual)
    if cf_on_input:
        inp = pre(sw, tfr + cfr); total = inp - sw; cf = total * cfr // (tfr + cfr); tf = total - cf
    else:
        inp = pre(sw, tfr); tf = inp - sw
    return inp, tf, cf
# -------------------------------------------------------------------------------------


# (trade, creator) of the 21 live configs (R-RAY §2.5), in ppm.
CONFIGS = [(2500, 500), (10000, 500), (20000, 500), (40000, 500), (5000, 500), (3000, 500),
           (15000, 500), (25000, 500), (2500, 5500), (2500, 7500), (5000, 4000), (2500, 10000),
           (3000, 12000), (300, 9700), (7500, 5000), (5000, 7500), (50, 14950), (4000, 8500),
           (7000, 8000), (3500, 6500), (2500, 12500)]


def main():
    rng = random.Random(SEED)
    cases = []
    for i in range(600):
        t, c = rng.choice(CONFIGS)
        if rng.random() < 0.2:
            c = 0                                   # creator fee not enabled
        on_input = rng.random() < 0.5
        exact_out = rng.random() < 0.6
        x = rng.choice([rng.randint(10**6, 10**12), rng.randint(10**12, 10**17), rng.randint(1, 10**6)])
        y = rng.choice([rng.randint(10**6, 10**12), rng.randint(10**12, 10**17), rng.randint(1, 10**6)])
        if (t + c == 0 and on_input) or x == 0 or y == 0:
            continue
        if exact_out:
            amount = rng.choice([1, rng.randint(1, max(1, y // 100)), rng.randint(1, max(1, y // 2))])
            actual = amount if on_input else (amount * 10**6 + (10**6 - c) - 1) // (10**6 - c)
            if actual >= y:
                continue
            inp, tf, cf = cp_base_output(amount, x, y, t, c, on_input)
            if inp == 0 or inp > U64:
                continue                            # the venue rejects these; the quote is None
            cases.append(dict(kind="exact_out", x=S(x), y=S(y), amount=S(amount), trade=S(t), creator=S(c),
                              on_input=on_input, result=S(inp), trade_fee=S(tf), creator_fee=S(cf)))
        else:
            amount = rng.choice([1, rng.randint(1, 10**6), rng.randint(1, U64)])
            out, tf, cf = cp_base_input(amount, x, y, t, c, on_input)
            if out <= 0:
                continue
            cases.append(dict(kind="exact_in", x=S(x), y=S(y), amount=S(amount), trade=S(t), creator=S(c),
                              on_input=on_input, result=S(out), trade_fee=S(tf), creator_fee=S(cf)))
    dump(dict(generator="gen_port.py",
              source="research port of raydium-cp-swap@59fb845a calculator.rs/fees.rs (R-RAY §3.3)",
              protocol_rate="120000", fund_rate="40000", cpmm=cases), Path(__file__).with_name("port.json"))
    print(len(cases), sum(1 for c in cases if c["kind"] == "exact_out" and not c["on_input"]))


if __name__ == "__main__":
    main()
