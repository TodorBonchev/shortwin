# shortwin-math test vectors

Differential test vectors produced by the normative reference models (ARCHITECTURE
§8.1): `docs/design/sim/shortwin_sim.py` v0.4.1, its attack scripts
`docs/design/sim/attacks/*.py`, `docs/design/sim/pool_model.py` v0.4.2 (KI-65) and
`docs/design/sim/config_check.py` v0.4.1, always with their **default switches** (the
analysis switches only feed comparison rows; generators assert the defaults). The crate
must match them exactly: no tolerance, no exception. Mainnet-trade fixtures (curve,
PumpSwap and Raydium signatures of ARCH §8.1) follow the same layout.

## Layout

```
crates/shortwin-math/
├── tests/
│   ├── common/mod.rs                 # shared helpers: load_vectors, Dec<T>, proptest_config
│   ├── <track>_<topic>.rs            # Rust integration test(s) of a track
│   └── vectors/
│       └── <track>/                  # one directory per track: num, quotes, oracle, settle,
│                                     #   pool, risk, params
│           ├── gen_<topic>.py        # generator, committed
│           └── <topic>.json          # its output, committed next to it
```

`num/` is the worked example: `num/gen_num.py` writes `num/num.json`, and
`tests/num_vectors.rs` checks the `num` helpers against it.

A track may write several files from one generator when a single `<topic>.json` would
pass the size guidance below, and may use a denser encoding inside them if its
generator's docstring and the Rust parser document it:

| Directory | Generator(s) → files | Encoding |
|---|---|---|
| `num/` | `gen_num.py` → `num.json` | this README |
| `quotes/` | `gen_sim.py` → `sim.json`; `gen_caps.py` → `caps.json` (caps, example markets, keeper rewards, tiers); `gen_port.py` → `port.json`; `gen_mainnet.py` → `mainnet.json` (+ `venue_state.json`); `gen_synthetic.py` → `synthetic.json` (pump's synthetic completions and their `migrate_v2`, pump-final §5) | this README |
| `oracle/` | `gen_oracle.py` → `units_{observe,ring,persist,bounds,ref}.json` (units only since v0.4.1; the full replays moved to `settle/`; `units_ref.json`: the exit reference liquidity with the guard and the Raydium depth cap, M4F-1, and the spot leg's liquidity floor, FITS-2 / AIR-17) | one line per operation of whitespace-separated decimal tokens, `-` for `None`, `1`/`0` for booleans (docstring of `gen_oracle.py`, parsers in `oracle_vectors.rs`) |
| `settle/` | `gen_settle.py` → `units_settle.json`, `units_evaluate_1.json`, `trace_{attacks_1..3,scenarios_1,random_1}.json`, `fuzz_{1..3}.json` | the oracle line encoding plus quote tables at the points the sim's rule reads; an attack variant continuing another trace is a suffix with `base` (docstring of `gen_settle.py`, parsers in `settle_vectors.rs`) |
| `pool/` | `gen_pool.py` → `pool_fuzz_continuous_{90,100}_{1..3}.json` (each option (a) ECON §9.6 run in parts that continue one CRC chain), `pool_lock_fuzz_1.json`, `pool_stranded_{1..5}.json`, `pool_ext_{lazy,runs,dust,extreme,topup,orphans}_1.json` (`orphans`: KI-65's lazy conversion orders, plus the `orphan_vest_demo` and `whale_order_demo` pools), `pool_scenarios_1.json` | one line per `pool_model` operation; the state after every line is checked through a chained CRC-32 of a canonical state string plus full checkpoints, and `liveness` blocks record each wind-down (I-P6) (docstring of `gen_pool.py`) |
| `risk/` | `gen_rent.py` → `rent.json`; `gen_exits.py` → `exits.json` | this README |
| `params/` | `gen_params.py` → `params.json` | `[overrides, violations]` string pairs (docstring of `gen_params.py`, parser in `params_vectors.rs`) |

`gen_mainnet.py` is an extractor, not a sim generator: `mainnet.json` holds trades
observed on mainnet, read from raw `getTransaction` dumps that are not committed (every
case carries its signature, so it can be re-fetched). Run without `--research` it
prints a note and leaves the file alone, which is what CI's `vectors` job does.
`gen_synthetic.py` is the same kind of extractor for pump's synthetic migration: its sample of
eleven completing buys and their `migrate_v2` is listed in the script, `--dumps <dir> --fetch`
re-reads them (read-only `getTransaction`, some are v1 transactions), and without `--dumps` it
leaves `synthetic.json` alone.

## Generators

- Python 3.14, **standard library only** (the sims are stdlib-only too).
- Import the sims from `docs/design/sim` by a path relative to the generator, never by
  copying them, and do not let Python write `__pycache__` into `docs/`:

  ```python
  import sys
  from pathlib import Path

  sys.dont_write_bytecode = True
  ROOT = Path(__file__).resolve().parents[5]          # repository root
  sys.path.insert(0, str(ROOT / "docs" / "design" / "sim"))
  import shortwin_sim as sim                           # noqa: E402  (v0.4.1)
  import pool_model                                    # noqa: E402  (v0.4.2)
  ```

- Deterministic: fixed seeds (`random.Random(<seed>)`), no timestamps, and
  `json.dumps(obj, indent=1, sort_keys=True) + "\n"`, so re-running a generator on an
  unchanged sim produces no diff.
- Write the JSON next to the generator (`Path(__file__).with_name("<topic>.json")`).
  Run from anywhere: `python3 crates/shortwin-math/tests/vectors/<track>/gen_<topic>.py`.
- Keep committed files small (under 1 MB each). Large fuzz traces are better replayed
  from a seed in the Rust test, or split across files (see the table above).
- Never edit `docs/`. The sims are normative (ARCH §2.2): if a doc disagrees with a sim,
  encode the sim's behaviour and record the doc issue in the crate README's design notes.

## JSON format

- Top level is an object with `generator` (file name), `source` (sim file, version and
  function or ECON section) and one or more arrays of cases.
- **Every integer is a decimal string** (`"-12"`, `"340282366920938463463374607431768211455"`):
  JSON numbers lose precision above 2^53 in many tools, and lamports·WAD products
  exceed 2^64. Booleans stay JSON booleans; enums are strings.
- An undefined result (a quote the API returns as `None`) is `null`. An expected error
  is the `MathError::as_str()` text, e.g. `{"error": "overflow"}` (the pool traces use
  short codes such as `!overflow` or `!above_allowance`, one per `MathError` variant;
  `pool_harness.rs` maps them).
- No comparison with a sim carries a tolerance: since v0.4.1 every value matches exactly
  (ARCH §8.1). The curve cases' `tolerance` in `quotes/sim.json` is not one: it bounds how
  far ECON's quote may sit above the venue's own charge (`gen_sim.py`).

## Rust side

```rust
mod common;

use common::Dec;
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    a: Dec<u128>,
    expected: Option<Dec<u128>>,
}

#[test]
fn matches_sim() {
    let cases: Vec<Case> = common::load_vectors("<track>", "<topic>.json");
    // ...
}
```

`Dec<T>` parses any `FromStr` type (`u64`, `u128`, `i128`, `U256`) from its decimal
string and works inside `Vec` and `Option`; `#[serde(deserialize_with = "common::dec")]`
is the field-level form.

## Property tests

Every `proptest!` block uses `#![proptest_config(common::proptest_config())]`: at least
2,000 cases (ARCHITECTURE §8.1), raised, never lowered, by `PROPTEST_CASES`. Proptest
keeps its default failure persistence: a failing case is written to
`tests/<file>.proptest-regressions` and replayed first on the next run. Commit such a
file together with the fix, so the case stays a regression test.
