# m7_cascade — T-CASCADE (M7)

ARCHITECTURE §6.6 / §8.2 T-CASCADE (C0-17): many exits on one market in consecutive slots under
the per-writable-account block budget (`MAX_WRITABLE_ACCOUNT_UNITS` = 24,000,000 CU, ARCH V10),
charged with each transaction's **requested** limit against every account it write-locks — the
`Market`, its `MarketVault`, the pool vault, the venue's pool accounts, the keeper's fee payer.
Output only; the code is `services/keeper-litesvm/tests/scenarios/cascade.rs`.

- **Baskets:** `min_notional` positions (≈ 0.1 SOL each, one borrower each) filling the 25-SOL
  launch cap — on the mock venue (a 10,000-SOL pool, so only the launch cap binds; 246 positions)
  and on the PumpSwap `mature` fork (the heaviest measured `liquidate`, ≈ 212 k requested CU).
- **Paths:** `cascade.py`'s, scaled to the positions' Tier C terms (open at a live CR ≈ 2.9 against
  `L` = 1.5): a ×3 gap held (S1 settlements), pumps ×3 over 60 s, ×3 and ×10 over 10 minutes. The
  keeper ticks every slot.
- **The keeper's policy** (`[budget] market_cu_per_slot` = 12,000,000, `payer_cu_per_slot` =
  20,000,000): exits of a market stop for the slot at half the account's block budget, most
  under-collateralised first; the rest go out in the next slot. Each basket also runs without it
  (both caps 0; LiteSVM enforces no block limit, so that run is the optimistic bound).
- **Gate:** with the policy no slot asks more than 12 M of the `Market` or `MarketVault`, 20 M of
  the fee payer, 24 M of any account; each basket clears within 2 s of its first exit and at most
  1 s after the uncapped run; the policy is exercised (deferrals happen, and the uncapped keeper
  asks the `Market` for more than 12 M in a slot).

`results.md` has the per-path table and the per-slot, per-account tables; `results.json`
(`shortwin.m7.cascade/2`) the same per run.

```sh
M7_CASCADE_WRITE=1 cargo test --manifest-path services/Cargo.toml --release --locked \
  -p shortwin-keeper-litesvm --test scenarios -- --include-ignored --nocapture cascade::
```
