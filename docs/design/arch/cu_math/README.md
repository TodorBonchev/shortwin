# cu_math — per-primitive CU of `shortwin-math` on SBF (ARCHITECTURE v0.4 §6.1, V18)

A raw-entrypoint SBF program that runs one `shortwin-math` primitive `n` times (instruction
data: `[bench id u8][n u32 LE]`). `measure.py` simulates it on a local validator and reports
`(CU(n) − CU(0))/n` minus the loop overhead. Results of 2026-09-27: `results.txt`, `results.json`
(read by `../cu_budget.py`).

```sh
# 1. build (platform-tools v1.57, SBPF v3; offline: the crate has zero dependencies)
cargo-build-sbf --tools-version v1.57 --arch v3 --manifest-path docs/design/arch/cu_math/Cargo.toml \
  --sbf-out-dir /tmp/cubench-out -- --offline
# 2. local validator with the program preloaded (any ledger dir)
solana-test-validator --reset --ledger /tmp/cubench-ledger --rpc-port 18899 \
  --bpf-program Bench111111111111111111111111111111111111111 /tmp/cubench-out/cubench.so &
# 3. measure (retries until the program is deployed; writes the JSON given as argument)
python3 docs/design/arch/cu_math/measure.py docs/design/arch/cu_math/results.json
```

The 2026-09-27 run used a read-only copy of `crates/shortwin-math` as built for M0 (v0.3
semantics); the per-primitive costs do not depend on the v0.3/v0.4 differences except where
`results.txt` names both (the EMA-leg entry). M0b turns this into a Mollusk bench with budgets
(T-CU-MATH).

**Superseded (M0b).** `src/lib.rs` here calls the v0.3 API (`observe::obs_median`, `integrate`
returning `bool`, `rent::credited_for(&pos, &basis, 1_500)`, `PosRent` without
`rent_protocol_bps`) and does not build against the v0.4.1 crate; it stays as the V18 evidence
for `results.*` and `../cu_budget.py`. The maintained bench is its v0.4.1 port,
[`tests/bench/cu_math`](../../../../tests/bench/cu_math) (Mollusk, T-CU-MATH).
