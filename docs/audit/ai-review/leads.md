# short.win — triage of the eight "leads worth a look"

The rust-auditor reports listed 225 leads. The consolidated summary picked eight as the most
consequential. Each was then checked against the code at `main` 9aade3e (on-chain code
byte-identical to `v1.0.0` / `e4895c2`), the design docs and the known-issues register. Lead 1 and
lead 2 were also checked against the Agave v4.1.2 / v4.2.2 source and read-only mainnet queries.
Leads 4–6 were measured with the normative sims (`shortwin_sim.py`, `pool_model.py`).

| Lead | Title | Verdict | Severity | ID |
|---|---|---|---|---|
| L-1 | A borrower whose address refuses lamports blocks its own liquidation | **Refuted** | None | — |
| L-2 | `ExtendProgram` in every slot blocks venue CPIs for that slot | Confirmed | Low | AIR-18 |
| L-3 | A live fee between a position's locked cap and the global hard cap leaves it with no forced exit | Confirmed | Low | AIR-19 |
| L-4 | An LP withdrawal inside `sol_settle` removes the live veto on S1 / S2 | Confirmed mechanism, harm not shown at defaults | Informational | AIR-20 |
| L-5 | S1 credits a take above market value at once, with no vest | Confirmed | Low | AIR-21 |
| L-6 | One flat price step every few seconds keeps `sustained_growth` at 0 and blocks liquidation in a fast pump | Confirmed | Low | AIR-22 |
| L-7 | Exits read `pool_lock_min_bps` live, not the value locked at open | Confirmed | Low | AIR-23 |
| L-8 | A pending `EnableVenues` change undoes `disable_venue` | Confirmed (operational gap) | Low | AIR-24 |

None of the confirmed leads is in `docs/audit/known-issues.md`.

## L-1 · Refuted

After `open_short` the program keeps only the borrower's key (`open_short.rs:588`), and the
keypair can later turn that address into an executable account. But crediting it still works:

- SIMD-0162 is active on mainnet (feature account read live; LiteSVM 0.16's mainnet feature list
  agrees). Agave PR #6847 removed the `ExecutableLamportChange` check. In v4.1.2 / v4.2.2
  `set_lamports` refuses only a debit by a non-owner and a change to a read-only account.
- The account loader does not reject an executable account loaded writable. Write locks are
  demoted only for reserved keys and for programs invoked at the top level of the same
  transaction.
- Every executing exit credits the real borrower (`liquidate.rs:99-101, 122`, `expire.rs:62-64,
  85`, `sol_settle.rs:119-121, 470-471`); `has_one = borrower` and the exposure seeds stop a
  keeper substituting another account. The credit (at least the position's rent) also keeps a
  0-lamport account rent-exempt.

## L-2 · AIR-18 · `ExtendProgram` blocks a program for the rest of its slot

- `ExtendProgram` is permissionless, limited to one extension per program per slot, at least
  10 KiB (SIMD-0431) up to the 10 MiB cap, and it redeploys the program at the current slot
  (agave `programs/bpf_loader/src/lib.rs` v4.2.2 :793-795, :867, :877-898, :912-915, :977-996).
- The new cache entry becomes visible only in the next slot; for the rest of the slot every
  invoke or CPI fails with `UnsupportedProgramId` ("Program is not deployed").
- Exits that CPI a venue (`liquidate`, `expire`, `close_short` via `venues::buy`) fail for that
  slot. pump-fees, Raydium AMM v4, CPMM and Token-2022 can be extended; pump and PumpSwap are at
  the 10 MiB cap and cannot. `sol_settle` and `crank_price` make no venue CPI.
- Cost: about 0.052 SOL of rent per blocked slot (mainnet 5,080 lamports/byte), about 14.5 SOL to
  block exits for 60 s. Each venue's budget is ~890–950 slots (~200 s, ~44–49 SOL) and is spent for
  good once the program hits 10 MiB.
- **short.win itself:** once deployed below 10 MiB, anyone can extend `shortwin` and
  `complete_curve` and block every short.win instruction for the rest of that slot (~867 slots,
  ~45 SOL budget).
- KI-63 calls `ExtendProgram` "harmless", which is wrong.
- **Fix direction (operational, no program change can stop third-party extensions):** extend
  short.win's ProgramData to the 10 MiB cap at deployment (about 45 SOL of rent deposit) or accept
  the bounded griefing; correct KI-63 and add a known-issues row with these bounds; have the keeper
  retry in the next slot.

**Erratum (2026-10-13).** The time and 60-s figures above assumed about 215 ms a slot. Recomputed
from the fixtures' ProgramData sizes and balances and the agave 4.2.2 loader (:877-898 the 10 KiB
minimum, :913 one extension per slot, :944-951 the payment, the shortfall below rent exemption):
- About 0.052 SOL of rent per blocked slot, plus the transaction fee: about 7.8 SOL to block a
  venue's exits for 60 s (150 slots at 400 ms; 171 slots, ≈ 8.9 SOL, at the 350-ms slots of
  SIMD-0525, active since slot 440,208,000 per LiteSVM 0.16's mainnet feature list).
- AMM v4's and Token-2022's ProgramData hold ≈ 1.76 / ≈ 2.60 SOL above rent exemption, which pays
  their first ≈ 34 / ≈ 50 extensions; pump-fees' and CPMM's hold exactly the minimum.
- Each venue's budget is 887–947 slots (≈ 5.9–6.3 min at 400 ms, ≈ 5.2–5.5 min at 350 ms;
  ≈ 44–49 SOL) and is spent for good once the program hits 10 MiB. short.win: ≈ 867 slots,
  ≈ 45 SOL; complete_curve: ≈ 1,013 slots, ≈ 53 SOL; each if its ProgramData is allocated at its
  v1.0.0 ELF size (no `--max-len`).
- These bounds rest on SIMD-0431's 10 KiB minimum (feature
  `loader_v3_minimum_extend_program_size`, active since slot 432,864,000 per the same list); with
  1-byte extensions they would be ≈ 8.9–10.4 M slots per program.

The slot and SOL figures above stand, rounded. Known-issues KI-73 carries the corrected bounds.

## L-3 · AIR-19 · A fee inside the cap band strands forced exits

- Keeper exits judge "buyback allowed" on the live-fee quote (`liquidate.rs:337, 371-377`;
  `risk.rs:263-270`), while `B̂_ref` is priced at the position's locked cap
  (`engine/bounds.rs:93-96`) and compared with the live quote (`liquidate.rs:460-461`). They fail
  `ExitBoundNotMet` whenever the live fee is more than about δ_liq (2 %) above the locked cap
  (about δ_exp, 1 %, for expiry).
- S1 prices every leg at `min(φ_live, position cap)` (`sol_settle.rs:201-207`,
  `arming.rs:196-202`, `venues/mod.rs:897-905`); S2 is out of reach; S3's `s3_no_route` compares
  with the **global** cap (`route.rs:32-39`, `settle.rs:684-686`), so a fee inside the band counts
  as routed and clears the S3 chain (`settle.rs:706-708`).
- Result: a liquidatable position has no forced exit until the S1 legs hold at the capped fee
  (the borrower is then zeroed instead of liquidated); an expired healthy position has none of
  the four exits, with no end date. ARCH:1450 and `settle.rs:121-122` ("no band exists") are false
  in this band.
- Preconditions: Governance raises `venue_fee_hard_cap_bps` (range 300–2,000, `params.rs:975`)
  above positions' locked 500 bps, and a venue admin raises the fee into the band (KI-44).
  Mitigations exist: voluntary close, Emergency `set_route_broken` (no timelock), lowering the cap.
- **Fix direction:** treat a fee above the position's own locked cap as no-route for that position
  (`arming.rs:164-172`), and drive the market's S3 chain with the lowest cap any open position
  locked (a Market field reset when `open_positions == 0`).

## L-4 · AIR-20 · Informational

`engine/arming.rs:191, 239` passes the raw `L_live` into `settle::s1_legs` / `s2_legs`; only the
CR spot leg gets the FITS-2 floor (`arming.rs:206`). A withdrawal in the same transaction drains
it. This is documented as deliberate (`arming.rs:28-39`: the box corner is post-roll), and the
box corner still vetoes in most states. Model runs: in the band where it bites, the borrower gets 0
under both the honest liquidation and the forced S1 (the 5–10 % penalty already takes the
remainder), so the borrower harm does not show at defaults; the protocol loses its rent and penalty
share (0.05–0.37 SOL) and the keeper takes K_s. Only when the pool lock fell to 20 % (attacker owns
80 % of the LP, 1 % fee) did the borrower lose up to 0.59 SOL (~1.2 % of B̂).
**Optional fix:** judge the S1/S2 live and robust-spot legs at `bounds::spot_liquidity`
(max(L_live, L_ema')), keeping `min(L_live, L_lo)` in `refuted`.

## L-5 · AIR-21 · S1 credits its excess at once

`sol_settle.rs:257` → S1 arm `(take, 0)` at `waterfall.rs:348` → `pool::settle`
(`pool.rs:2285-2290`) with lump 0; `true_up` (`pool.rs:2196-2205`) credits `V − K_s − R_due` at
once to the shares that exist, including a same-transaction deposit. The S1 legs only require
`B̂ + ert > V − K_s`, so the take can exceed `⌊Q·P_ema⌋ + R_due` by up to impact plus fee: max
0.27–5.35 SOL (132–568 bps of Q·P_ema) across the sim's markets. A whale at 9× the pool captures
90 % of the excess atomically (+4.5 SOL on a 5 SOL excess); with R4B-6's split applied to S1 it
takes 0. Separate root cause from I-09 (S1 runs on an executable venue, so the price is not
stale; S1 simply has no cap). **Fix direction:** give S1 the same split at `waterfall.rs:347-353`
(`lump = take − min(take, immediate_cap(q, p_ema, r_due))`), mirrored in the sim's `_settle`
(`shortwin_sim.py:2098-2101`).

## L-6 · AIR-22 · A flat step zeroes the growth term

A bucket that straddles P_ema gives `obs = P_ema`, an exactly flat step (`observe.rs:764-772`); the
bucket low is a minimum (`:464`), so one low record per second suffices. `sustained_growth`
returns 0 if any of the newest 3 steps is flat (`observe.rs:821`), κ_eff falls to κ_base (default
0, `bounds.rs:172-179`, `params.rs:205`), P_ref becomes `min(P_ema, lb_lo)`
(`bounds.rs:198-232`), and `liquidate.rs:461` refuses (`ExitBoundNotMet`). An atomic [sell to ≤
P_ema → `crank_price` → buy back] every 3 s does it. Threshold about 20 bps/s (not 34); in
sustained 30–200 bps/s pumps to 3× liquidation is refused every second until the pump ends, then
S1 (lenders 5–49 SOL below Q·P on the sim's markets) at a flat 0.07–0.85 SOL/s veto cost, which
breaks KI-05's bound that the veto cost grows with the move. **Fix direction:** take the lower
median of the 3 step rates instead of the minimum (`observe.rs:819-836`; sim `ema_growth_bps`).

## L-7 · AIR-23 · `pool_lock_min_bps` read live at exit

`ref_depth_cap` reads `config.params.pool_lock_min_bps` on every liquidation and expiry
(`engine/bounds.rs:48-51`, called at `liquidate.rs:572-573`; Raydium only, PumpSwap uses a fixed
10,000). `ShortPosition` does not lock it (`position.rs:55-100, 121-159`). Lowering it (bounds
2,000–10,000, default 5,000) credits more depth to open positions: `B̂_ref` and max_in fall, exits
are refused while the live pool is thinner than the EMA / `L_slow` terms, and can end in S1 (ECON
§12.12 (12)(f)(v) measured −31.1 % / −52.3 % of Q for 3D / 5D caps on PumpSwap). Breaks A26
(ARCH:571-577), which limits `pool_*` parameters to pool selection. **Fix direction:** lock λ_min at
open (space in `ShortPosition._reserved`, set in `lock_terms`) and use it in `ref_depth_cap`, or use
`max(locked, live)` so the cap can only tighten.

## L-8 · AIR-24 · A pending `EnableVenues` re-enables a disabled venue

`disable_venue` only clears the bit (`disable_venue.rs:32-36`); `apply_config` is permissionless
once `now ≥ not_before` (`apply_config.rs:57-80`) and ORs the bits back in (`config.rs:199`). The
Emergency can cancel the change (`cancel_config.rs:16-17, 40-44`), but if it disables without
cancelling, anyone can bundle `[apply_config, open_short…]` at `not_before`. Opens only, bounded by
the open caps; the latch and `set_pause` still block opens. **Fix direction:** have
`disable_venue` strip the disabled bit from a pending `EnableVenues` change (or clear it).
