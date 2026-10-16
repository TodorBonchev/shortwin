#!/usr/bin/env python3
"""Vectors for shortwin_math::num (ARCHITECTURE §2.2; ECONOMICS §2, §3.1).

Expected values come from Python integers and the sim's own helpers
(`shortwin_sim.cdiv`, `shortwin_sim.fee_ceil`, `shortwin_sim.isqrt_ceil`, v0.4.1: the
`isqrt⁺` of `reprice`, ECON §2.1). Writes num.json next to itself.
"""
import json
import math
import random
import sys
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "docs" / "design" / "sim"))
import shortwin_sim as sim  # noqa: E402  (v0.4.1)

U64 = 2**64 - 1
U128 = 2**128 - 1
I128_MIN, I128_MAX = -(2**127), 2**127 - 1
SEED = 20260927


def edge_int(rng: random.Random, bits: int) -> int:
    pick = rng.random()
    if pick < 0.1:
        return rng.choice([0, 1, 2**bits - 1, 2 ** (bits - 1), sim.WAD, sim.BPS])
    return rng.getrandbits(rng.randint(1, bits))


def fits(x, lo, hi):
    return str(x) if lo <= x <= hi else None


def main() -> None:
    rng = random.Random(SEED)
    mul_div = []
    for _ in range(300):
        a, b = edge_int(rng, 128), edge_int(rng, 128)
        d = edge_int(rng, 128) or 1
        mul_div.append({
            "a": str(a), "b": str(b), "d": str(d),
            "floor": fits(a * b // d, 0, U128),
            "ceil": fits(sim.cdiv(a * b, d), 0, U128),
        })

    fee_ceil = []
    for _ in range(300):
        x, bps = edge_int(rng, 64), rng.randint(0, 20_000)
        fee_ceil.append({"x": str(x), "bps": str(bps), "fee": fits(sim.fee_ceil(x, bps), 0, U64)})

    mul_div_i128 = []
    for _ in range(300):
        a = edge_int(rng, 127) * rng.choice([-1, 1])
        b, d = edge_int(rng, 128), edge_int(rng, 128) or 1
        mul_div_i128.append({
            "a": str(a), "b": str(b), "d": str(d),
            "floor": fits(a * b // d, I128_MIN, I128_MAX),
            "ceil": fits(sim.cdiv(a * b, d), I128_MIN, I128_MAX),
        })

    isqrt = []
    for _ in range(300):
        x = edge_int(rng, 256)
        isqrt.append({"x": str(x), "floor": str(math.isqrt(x)), "ceil": fits(sim.isqrt_ceil(x), 0, U128)})

    out = {
        "generator": Path(__file__).name,
        "source": "docs/design/sim/shortwin_sim.py v0.4.1 (cdiv, fee_ceil, isqrt_ceil) and Python integers",
        "mul_div": mul_div,
        "fee_ceil": fee_ceil,
        "mul_div_i128": mul_div_i128,
        "isqrt": isqrt,
    }
    Path(__file__).with_name("num.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
