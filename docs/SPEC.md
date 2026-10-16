# short.win — Product & Implementation Specification

*v0.3 • Solana • Permissionless P2P token lending for shorting memecoins*
*Supersedes "ShortMeme" v0.1 and short.win draft v0.2. Domain: **short.win** • Brand: **ShortWin***

---

## 0. What changed

### v0.2 → v0.3 (2026-09-27)
All draft decisions marked "⚠ Owner confirm" in v0.2 were confirmed by the owner:

- **Sale proceeds stay locked in the position vault** (§2.1).
- **Duration:** open-ended with a per-tier max duration (§4, Q15).
- **Mint safety filter:** automatic rejection of freeze authority, permanent delegate, transfer hooks and transfer fees (§3).

Decided by the owner after the venue research and design review (docs/research/, docs/design/):

- **Graduated-token execution (amends #16):** trade the token's canonical constant-product pool directly — PumpSwap for pump.fun graduates, Raydium AMM v4 / CPMM otherwise. Jupiter moves to v1.1 as an optional better-price path, bounded by the on-chain pool quote (§2.1, §3, §9).
- **Raydium in the MVP:** tokens whose main SOL pool is Raydium AMM v4 or CPMM (pre-PumpSwap pump.fun graduates, LaunchLab, most older memecoins) can be shorted at launch. Tokens that trade only on concentrated-liquidity venues (Orca Whirlpools, Meteora DLMM, Raydium CLMM) can be listed and lent, but not shorted, until a later version (§3).
- **SOL settlement fair-value cap (amends §5.3):** S1 is unchanged. In S2 and S3 the lenders' take is capped at fair value and any excess returns to the borrower (§5.3).
- **Margin rule (clarifies §4):** initial margin is whatever makes the opening CR in the table hold after venue fees, checked at both spot and EMA price (§4).

Decided by the owner after the third and fourth design reviews (docs/design/ARCHITECTURE.md v0.4.1 §10):

- **Lender SOL income (O16, clarifies §2.4):** rent is credited to lenders continuously; one-off lender income vests over 6 hours (§2.4).
- **Emergency venue powers (O6, amends §9/§11):** a "route broken" flag and a bounded set of extra venue accounts, neither of which can move funds (§11).
- **Expired positions in S2/S3 (O15, clarifies §5.3):** the fair-value cap charges the 2% expiry fee, not the liquidation penalty (§5.3).
- **Liquidator floor (N22, clarifies §6.3):** a liquidating keeper earns at least the SOL-settlement keeper reward, topped up from the borrower's remainder (§6.3).

Decisions made during the build are recorded in §14. The round before the external audit (M10, 2026-10-02 and 2026-10-03):

- **Audit and release:** one full private audit plus a public contest on the fixed code, then a bug bounty at beta; the audited program is release **1.0.0** (tag `v1.0.0`), post-audit fixes 1.0.x; security contact security@short.win with a policy page at short.win/security (§9).
- **Multisigs (§11):** Governance 3-of-5 with a 48 h Squads time lock holds the upgrade authority and the fee account; Emergency 2-of-4 with no time lock is the pause authority; the program refuses a configuration in which two roles share a key.
- **Lenders:** rent paid above what had accrued (the minimum rent of an early exit) now vests like other one-off income (§2.4); a late depositor's share of a later shortfall is kept and shown at deposit (§2.4).
- **Accepted and disclosed:** the held-sandwich figures measured on the real venues (O19, §12).
- **Emergency extra accounts** may never name the position's borrower or the pool's own accounts (§11).

### v0.1 → v0.2

| Area | v0.1 | v0.2 |
|---|---|---|
| Name | ShortMeme | **short.win / ShortWin** |
| Token coverage | Curated, bonding curve later | **All Solana SPL tokens, including Pump.fun tokens still on the bonding curve, from day one** |
| Listing | Open question | **Fully permissionless** — anyone can create a market for any mint |
| Bad debt | Open question | **Lender bears it.** No insurance fund, no treasury backstop |
| Liquidation settlement | Open question | **Lender receives SOL (collateral + fees) instead of tokens** when buyback isn't possible |
| Short execution | Open question | **short.win executes the sell** — Pump.fun bonding curve if not graduated, Jupiter if graduated |
| Collateral, fees, rates | Open | Proposed values in §4–§6 |

---

## 1. Purpose

short.win lets anyone short any Solana token — including brand-new Pump.fun launches still on the bonding curve — by borrowing it from lenders who earn rent.

- **Lender** deposits tokens into a per-token pool and earns rental fees. Accepts that if a short can't be bought back, they are repaid in SOL instead of tokens.
- **Borrower** posts SOL margin; short.win borrows the tokens and sells them in the same transaction. Borrower profits if the price falls.
- **Protocol** creates markets permissionlessly, prices risk automatically, executes trades, liquidates unsafe positions, and collects fees.

**Core question:** can idle memecoin inventory be turned into borrowable short liquidity, with rules clear enough that lenders knowingly accept the risk?

---

## 2. Core flows

### 2.1 Open a short (atomic, one transaction)
1. Borrower picks a token and quantity `Q`, sets max slippage.
2. Borrower deposits SOL margin `M`.
3. Program borrows `Q` from the pool and sells it:
   - **Bonding curve** (not graduated): directly into the Pump.fun curve.
   - **Graduated**: directly into the token's canonical constant-product pool — PumpSwap for pump.fun graduates, Raydium AMM v4 / CPMM otherwise. ✅ Owner (v0.3). Jupiter is added in v1.1 as an optional better-price path, bounded by the on-chain pool quote.
4. Sale proceeds `S` **stay locked in the position vault** alongside `M`. ✅ Owner confirmed (v0.3)
5. Position records: `Q` owed, `S`, `M`, entry price, open time, tier, rate.

Keeping `S` in escrow is the single most important safety choice in this doc: collateral starts at `M + S` instead of just `M`, so the lender is protected by the sale proceeds plus the margin. The borrower never holds the tokens or the proceeds, so there's nothing to "run with."

### 2.2 Close a short
1. Borrower requests close with max slippage.
2. Program uses vault SOL to buy back `Q` tokens (curve or Jupiter, whichever is live).
3. `Q` tokens return to the pool; rent and fees are paid.
4. Remaining SOL goes to the borrower. Profit if buyback cost < `S`.

### 2.3 Liquidation — see §5.

### 2.4 Lend
1. Lender deposits tokens into the market's pool and receives pool shares.
2. Rent accrues to the pool continuously.
3. Lender withdraws pro-rata from **unborrowed** inventory. If the pool has received SOL from settlements (§5.3), withdrawals are a pro-rata mix of tokens and SOL.
4. One-off lender income — the lenders' share of liquidation penalties, expiry fees, settlement proceeds above the market value of the debt, and swept balances — **vests to lenders linearly over 6 hours** (configurable), so a deposit made just before such a payment cannot capture it. Rent is not vested; it is credited continuously as it accrues. ✅ Owner (v0.3, O16) **Rent paid above what had accrued** — the minimum rent of a position closed, liquidated or settled soon after it opened — counts as one-off income and vests too, so a deposit made around such an exit cannot capture it either. ✅ Owner (2026-10-02, M10-8C-1)
5. **A shortfall is shared by the lenders holding at the time.** If a position ends without enough SOL to pay the rent already credited to the pool, the unpaid part is taken from the lenders pro-rata to the shares they hold at that moment — including lenders who deposited after the rent was credited. A lender whose SOL balance turns negative must pay the difference in SOL before withdrawing tokens. The deposit screen shows the pool's unpaid rent and the deposit's share of it, and warns prominently when that share is at least 1 % of the deposit's value or 1 SOL. ✅ Owner (2026-10-03)

---

## 3. Markets & permissionless listing

- **Anyone can create a market** for any SPL mint by paying a small creation fee (covers rent for accounts; suggest ~0.02 SOL).
- No admin approval. Risk tier is **assigned automatically from on-chain state** (§4) and re-evaluated on every interaction.
- **Mint safety filter (automatic, not curation).** ✅ Owner confirmed (v0.3). Reject mints that could make the lender vault unsafe regardless of price:
  - active **freeze authority** (issuer could freeze the vault),
  - Token-2022 **permanent delegate** (issuer could pull tokens out of the vault),
  - Token-2022 **transfer hooks** or **transfer fees** (break accounting/CPI).
  Standard Pump.fun tokens pass this filter. This keeps listing permissionless while blocking tokens that are mechanically broken for lending.
- Supported venues (MVP): **Pump.fun bonding curve**, **PumpSwap** (canonical pool of pump.fun graduates), and **Raydium AMM v4 / CPMM** SOL pools. ✅ Owner (v0.3). Jupiter follows in v1.1.
- Tokens whose SOL liquidity is only on concentrated-liquidity venues (Orca Whirlpools, Meteora DLMM, Raydium CLMM) can be listed and lent at launch, but opening shorts on them waits for a later version that can price those pools on-chain.

---

## 4. Risk tiers & collateral

Tiers are computed on-chain from venue state and liquidity. Collateral ratio **CR = (vault SOL) ÷ (SOL cost to buy back Q now)**, where buyback cost is the *executable* cost including price impact, not spot × Q.

| Tier | Criteria | Initial margin (of notional) | Opening CR | Liquidation CR | Price rise to liquidation | Liq. penalty | Max duration |
|---|---|---|---|---|---|---|---|
| **A** | Graduated, pool liquidity ≥ $500k, age ≥ 7 days | 100% | 200% | 125% | ~1.6× | 5% | 30 days |
| **B** | Graduated, liquidity $100k–500k | 150% | 250% | 135% | ~1.85× | 7% | 14 days |
| **C** | Graduated, liquidity < $100k or graduated < 24h ago | 200% | 300% | 150% | ~2.0× | 8% | 7 days |
| **D** | Still on Pump.fun bonding curve | 300% | 400% | 175% | ~2.3× | 10% | 72 hours |

*"Price rise to liquidation" ≈ Opening CR ÷ Liquidation CR, ignoring rent accrual.*

**Pool age ✅ Owner (v0.3, O13).** A graduated pool's age (the "graduated < 24h ago" and "age ≥ 7 days" criteria) is measured from the moment short.win first observes it: the real graduation time for markets listed before their token graduated, otherwise the market's listing time. Raydium AMM v4 and PumpSwap pools (and CPMM pools created before mid-2024) store no provable creation time, so an old pool listed today starts in Tier C for 24 h and is at most Tier B until day 7. This errs on the conservative side; the UI explains the tier. A Raydium CPMM pool created after mid-2024 does record a provable creation time (`open_time`), and its age is counted from that. ✅ Owner (2026-09-28): accepted knowing a pool can be created early and listed later to start older, because a pool's tier liquidity is bounded by its locked depth.

**Margin rule ✅ Owner (v0.3).** The program enforces the **Opening CR** column: margin `M` is whatever makes (M + S) ÷ executable buyback cost ≥ Opening CR, checked at both the spot and the EMA price. The "Initial margin" column is the approximate result before venue fees; borrowers post about 2% more than it once fees (~1.25% each way) are included.

### Size caps (all tiers)
- **Per position:** buyback of `Q` must move price ≤ 5% (Tier D: ≤ 3%) at open time.
- **Per market (total open shorts):** total `Q` outstanding ≤ amount whose buyback would move price ≤ 20% (Tier D: ≤ 10%).
- **Utilization cap:** ≤ 90% of pool inventory borrowed.
- **Per wallet:** ≤ 25% of a market's open interest.

Caps are derived from live curve/pool reserves, so they grow automatically as a token gains liquidity.

### Duration ✅ Owner confirmed (v0.3, Q15)
Positions are **open-ended up to the tier's max duration**. After max duration, the position becomes liquidatable at no penalty to the lender side (borrower pays a 2% expiry fee instead of the full liquidation penalty). Reason: lenders need a guaranteed horizon, and bonding-curve tokens can go dead within days.

---

## 5. Liquidation & SOL settlement

### 5.1 Trigger
A position is liquidatable when **any** of these holds:
1. **CR < tier's liquidation CR**, confirmed on both the current executable price **and** a short-window EMA price (prevents one-block manipulation from liquidating someone).
2. **Max duration exceeded.**

Anyone can call liquidate (permissionless keepers). short.win runs its own keeper as a backstop.

### 5.2 Normal liquidation (token buyback)
1. Keeper triggers; program buys back `Q` using vault SOL, bounded by max slippage.
2. `Q` tokens + accrued rent return to the pool.
3. Penalty is taken from the remaining SOL (split per §6.3).
4. Anything left returns to the borrower.

### 5.3 SOL settlement (lender takes collateral instead of tokens)
When a normal buyback isn't possible, the **position settles in SOL** and the token debt is extinguished:

- **S1** (a real gap): the lender pool receives **all vault SOL minus the keeper reward**. The borrower receives nothing.
- **S2 / S3** (thin liquidity or no route): the lender pool receives **all vault SOL minus the keeper reward, capped at fair value** = executable cost to buy back `Q` at the EMA price + liquidation penalty + accrued rent. Any excess goes back to the borrower. ✅ Owner (v0.3). Reason: without the cap, lenders profit from settling healthy positions, which rewards them for draining liquidity or halting routes.
  - For a position settled in S2 or S3 only because it **expired** (it is not below its liquidation CR), fair value uses the **2% expiry fee** instead of the liquidation penalty, matching §4. ✅ Owner (v0.3, O15; the same reasoning applies to S2 as to S3)

SOL settlement applies **only** under these exact conditions:

| # | Condition | Typical cause |
|---|---|---|
| S1 | Executable cost to buy back `Q` **exceeds vault SOL** minus keeper reward | Price spiked faster than liquidation could run (gap) |
| S2 | Buying back `Q` would exceed **max liquidation slippage** (20%; Tier D 30%) | Liquidity too thin at that moment |
| S3 | **No executable route** for ≥ 10 minutes while the position is liquidatable | Venue halted, pool drained, migration stuck |

Rules:
- The program must **attempt the normal buyback first** (or prove via on-chain quote that it would fail) before S1/S2 settlement is allowed.
- **Graduation in progress:** while a Pump.fun token is migrating from curve to AMM, liquidations pause and the S3 timer doesn't start until 10 minutes after migration completes. *(Since 1.0.1: a buyback larger than the curve's remaining tokens completes the curve itself, buying the rest from the pool pump creates on the spot; a migration stuck after such a buy is valued on that pool, §14 fifteenth round.)*
- SOL received by a pool is distributed **pro-rata to all lenders in that pool** (pools are shared, so losses are shared).
- UI must disclose on the deposit screen, before signing: *"If a borrower can't be bought back, you will be repaid in SOL, which may be worth less than the tokens you lent."*

### 5.4 Bad debt
**Lenders absorb it**, pro-rata within the affected market. No insurance fund, no treasury backstop. Markets are fully isolated — a loss in one token's pool cannot touch any other pool.

---

## 6. Fees & rates

### 6.1 Rental rate — dynamic, utilization-based (kinked)
```
if U ≤ 80%: rate = base + slope1 × U
if U > 80%: rate = base + slope1 × 80% + slope2 × (U − 80%)
```

| Tier | Base (APR) | Rate at 80% util | Rate at 100% util |
|---|---|---|---|
| A | 10% | 50% | 250% |
| B | 25% | 100% | 400% |
| C | 50% | 200% | 800% |
| D | 100% | 400% | 1,500% |

- **Minimum rent:** 1 hour of rent (Tier D: 4 hours), so flash open/close can't borrow for free.
- Rent is denominated in SOL, accrued per second on current notional, paid from the vault on close/liquidation.

### 6.2 Protocol fees
- **Open fee:** 0.25% of notional (protocol).
- **Close fee:** none.
- **Market creation:** ~0.02 SOL.

### 6.3 Splits

| Fee | Lenders | Protocol | Keeper/liquidator |
|---|---|---|---|
| Rent | 85% | 15% | — |
| Liquidation penalty | 50% | 20% | 30% |
| SOL settlement | all remaining SOL | — | fixed reward (0.01 SOL + 1% of vault, capped) |
| Expiry fee (2%) | 100% | — | — |

No reserve/insurance allocation, consistent with lenders bearing bad debt.

**Liquidator floor ✅ Owner (v0.3, N22).** A keeper that liquidates a position normally (§5.2) is paid at least what it would have earned from a SOL settlement of the same position (0.01 SOL + 1% of vault, capped). Any top-up above its 30% penalty share comes from the borrower's remainder. Without it, on small positions a keeper earns more by letting the position gap into a lender loss than by liquidating it.

---

## 7. Lender proposition

- **Earn rent** on tokens that would otherwise sit idle.
- **Isolated risk:** only exposed to the one token they lend.
- **Explicit, signed-off downside:** if the token pumps faster than liquidation can buy back, they receive SOL (collateral + penalty + rent) rather than tokens.
- **Creators:** identical treatment to any lender. No badges or special status at launch. (The UI may still show the fact that a deposit came from the mint's creator wallet, since it's public on-chain data — see risk R1.)

Lending is not staking and not risk-free.

---

## 8. MVP scope

**In:**
- Wallet connect, token search by mint/ticker, permissionless market creation
- Lender deposit / withdraw (token + SOL mix)
- Atomic open-short via the Pump.fun curve, PumpSwap, or Raydium AMM v4 / CPMM
- Close, liquidation, SOL settlement, expiry
- Position dashboard: `Q` owed, vault SOL, CR, liquidation price, P&L, rent accrued, time left
- Market page: tier, inventory, utilization, rate, open interest, caps, recent liquidations
- Keeper bot (in-house) + open liquidation instruction
- Indexer/API: markets, deposits, positions, closes, liquidations, settlements, fees
- Analytics: TVL, utilization, OI, fees, liquidations, SOL-settlement count and lender loss

**Deferred:** cross-token collateral, leverage beyond margin, creator status, governance, social/copy trading, limit orders, partial closes (consider for v1.1).

---

## 9. Technical architecture

- **On-chain program (Anchor):** markets, pools, positions, vaults, open/close/liquidate/settle, fee accounting, tier computation.
- **Venue adapters (CPI):** Pump.fun bonding curve (buy/sell, curve math on-chain); PumpSwap and Raydium AMM v4 / CPMM for graduated tokens, with reserves read from the same pool for on-chain pricing; Jupiter in v1.1.
- **Pricing:** on-chain executable-cost computation from curve/pool reserves + an EMA price updated on every interaction and by keeper crank.
- **Indexer/backend:** event indexing, REST/WebSocket API, market analytics.
- **Keeper:** monitors all positions, liquidates, handles expiry and SOL settlement.
- **Frontend (short.win):** fast trade UI, transaction simulation before signing, clear risk disclosures.
- **MEV:** submit open/close/liquidate with tight slippage and via bundles where available to limit sandwiching.
- **Security:** isolated pools, strict account constraints, checked math, admin keys limited to emergency pause and fee parameters (no ability to move user funds), fuzzing and invariant tests, external audit before mainnet. **Audit plan ✅ Owner (2026-10-02):** one full private audit, then a public contest (Sherlock or Cantina) on the fixed code, then a bug bounty at the beta. The audited program is release **1.0.0** (tagged `v1.0.0` with the audit tag); fixes after the audit are 1.0.x. The audit covers the on-chain programs and the core of the keeper (its exit, coverage, budget and transaction-building logic, about 4.7k lines). Security reports go to **security@short.win**; the policy page is short.win/security, and the programs carry the same contact on-chain. The repository goes public (or gets a public mirror) before mainnet.

---

## 10. On-chain state

- **GlobalConfig:** fee recipient, pause authority, fee parameters, tier parameter table, venue program IDs.
- **Market:** mint, venue state (curve / graduated + pool), tier (cached), pool vault (tokens), pool SOL vault, total shares, total borrowed `Q`, open-interest cap, EMA price, rate index, paused flag.
- **LenderPosition:** owner, market, shares.
- **ShortPosition:** borrower, market, `Q` owed, vault SOL (margin + proceeds), entry price, open timestamp, max-duration deadline, rate index at open, status.
- **Events:** MarketCreated, Deposit, Withdraw, Open, Close, Liquidate, SolSettle, Expire, FeeCollected.

### Key invariants
- Tokens in pool vault + `Q` outstanding across positions = total lender token claims (until SOL settlement converts some claims to SOL).
- A position's vault SOL can only leave via close, liquidation, or SOL settlement.
- No instruction lets an admin withdraw pool or position funds.
- Utilization never exceeds cap after an open.

---

## 11. Emergency controls

- **Pause new opens** globally or per market (keeps closes and liquidations running).
- **Keeper coverage pause ✅ Owner (v0.3, O17).** A separate keeper key, the `coverage_authority`, may pause and resume new opens on one market when the in-house keeper cannot keep that market covered (degraded mode: an outage, or a market the global keeper budget can no longer cover — M7-13/M7-14, §14), with an alert. It moves no funds and never pauses closes, liquidations, settlements, withdrawals or claims; it is inert until governance installs the key with a timelocked authorities change, and only that key can set its flag. **The emergency multisig may also clear — never set — a market's coverage pause, so a lost or hijacked coverage key can be neutralised at once without moving any funds (✅ Owner, 2026-10-01, M7-3).** Any clear lets opens through only a few seconds later (5 s), so no key can slip an open past the pause within one transaction (M7-SEC-3).
- **Never pause closes or liquidations** except during a detected venue migration (§5.3).
- Pause authority is a multisig; fee/tier parameter changes are timelocked (suggest 48h). **Multisig layout ✅ Owner (2026-10-02):** a **Governance** multisig (Squads v4, 3-of-5, a 48 h Squads time lock) holds the program upgrade authority and governance (its first vault), and receives protocol fees (its second vault); every change it makes also waits for the program's own 48 h timelock, so a change takes about 96 h from approval. An **Emergency** multisig (Squads v4, 2-of-4, no time lock) is the pause authority and holds the venue-breakage powers below. The keeper's coverage key is a third, separate key. **The program refuses any configuration in which two of these roles — governance, pause authority, fee recipient, coverage key — share a key** (checked when the configuration is created and again whenever the authorities change). Members approve and sign in the Squads app; short.win's governance tool only prepares and verifies transactions (no hardware-wallet support in the tool). The multisigs are created on mainnet in the launch milestone (M11), with the members' keys.
- **Venue-breakage powers ✅ Owner (v0.3, O6; amends §9's "emergency pause and fee parameters" list).** The emergency multisig may also, without timelock:
  - set a **"route broken"** flag on a venue or market, which refuses new opens and starts the §5.3 S3 timer — it never pauses closes or liquidations and moves no funds;
  - set a **bounded list of extra accounts** passed to a venue's instructions after that venue changes its interface, so exits keep working without a program upgrade. Callers can always choose the previous or empty set, so a bad setting cannot block exits, and no setting can move funds. **An extra account can never be short.win's own accounts, the position's borrower or the token's pool and the accounts holding its reserves** (refused when used, also when generated from a template), so a setting cannot hand the venue a second copy of its reserves; the keeper and the app then fall back to the previous, stable or empty set. ✅ Owner (2026-10-03)
- **Emergency cancel of governance changes ✅ Owner (v0.3, M1-SEC-1).** The emergency multisig may cancel a pending *parameter* change during its timelock (a fast check on a compromised governance multisig), but not a change of the authorities or of the timelock itself, so governance can always rotate a compromised emergency key once the timelock has run.

---

## 12. Key risks to design against

| # | Risk | Mitigation |
|---|---|---|
| R1 | **Creator self-dealing:** creator lends supply, shorts it from another wallet, sells into the curve — a disguised dump. If the token later pumps, they may get SOL settlement on their own lending. | Show creator-wallet deposits on the market page; per-market OI and notional caps tied to liquidity (the bound the risk analysis relies on). Per-wallet caps are a UX limit only: they are per key, so one actor with four wallets fills a market's OI cap (M7-SEC-9). Can't be fully prevented in a permissionless design — disclose it. |
| R2 | **Liquidation manipulation:** attacker pumps a thin token for one block to force liquidations. | EMA + executable-price dual check; liquidation slippage bounds. **Residual accepted and disclosed (O19, ✅ Owner 2026-10-03):** an attacker who holds a pool's price up for whole seconds before liquidating a position that is already due can take part of that borrower's remaining SOL — measured on the real pools at about +61 SOL (Raydium AMM v4), +37 SOL (Raydium CPMM; the owner's decision cites its atomic sandwich extra, +5.34 SOL) and +10 SOL (PumpSwap) for a full-size position held 3 s at +15 %; the lenders' share is not reduced. More competing keepers are the lever (launch milestone). These were measured before the AI review's fix AIR-22 (§14, fourteenth round), with which the simulated attacker needs 2 held seconds instead of 3 and nets more (+2.84 instead of +1.98 SOL on the economics document's Raydium example); the real pools have not been re-measured. |
| R3 | **Gap risk:** 5–10× moves in minutes on bonding-curve tokens. | Tier D margins, tiny caps, SOL settlement; lenders pre-accept. |
| R4 | **Sandwiching opens/closes.** | Tight slippage, bundles. |
| R5 | **Venue changes** (Pump.fun curve/migration program upgrades). | Adapters versioned; pause opens for a market on unknown program state. Pump's 2026-10-08 … 10-13 redeploys led to 1.0.1 (§14, fifteenth round): every curve leg now depends on pump's v3 trade instructions, which pump can upgrade without a timelock; a breaking change halts curve exits until an adapter upgrade or SOL settlement. |
| R6 | **Regulatory.** A public shorting/lending product may be treated as a regulated financial product in some jurisdictions. | Get legal advice before launch; consider geo-restrictions on the frontend. |

---

## 13. Success metrics

Lendable inventory; utilization; unique lenders/borrowers; OI; rental fees; protocol revenue; avg. duration; close vs. liquidation vs. SOL-settlement rate; **realized lender loss** (SOL settlement value vs. token value at settlement); avg. execution slippage; repeat lenders/borrowers.

---

## 14. Decision log

| # | Question | Decision | Status |
|---|---|---|---|
| 9 | Bonding-curve tokens at launch? | Yes — all tokens, including un-graduated Pump.fun | ✅ Owner |
| 10 | Collateral & liquidation by tier | §4 table. Adopted as the v1.0.0 design; the values stay tunable by governance within the compiled bounds (ECONOMICS §15), behind the §11 timelock | ✅ Delegated; adopted by the owner (2026-10-09, eleventh round, O18) |
| 11 | SOL settlement for lenders? | Yes, under conditions S1–S3 (§5.3) | ✅ Owner; conditions adopted by the owner (2026-10-09, eleventh round, O18; ECONOMICS §14 G13, G18, G19, N15) |
| 12 | Who absorbs bad debt? | Lenders, pro-rata per market | ✅ Owner |
| 13 | Fixed or dynamic rent? | Dynamic, kinked utilization curve (§6.1). Adopted as the v1.0.0 design; the values stay tunable by governance within the compiled bounds (ECONOMICS §15), behind the §11 timelock | ✅ Delegated; adopted by the owner (2026-10-09, eleventh round, O18) |
| 14 | Creator special status? | No, identical treatment | ✅ Owner |
| 15 | Fixed vs open-ended duration? | Open-ended with per-tier max (72h–30d) | ✅ Owner (v0.3, 2026-09-27) |
| 16 | Venues | Pump.fun curve + PumpSwap + Raydium AMM v4 / CPMM, traded directly; Jupiter in v1.1 (was: curve + Jupiter) | ✅ Owner (amended v0.3, 2026-09-27) |
| — | Tokens only on concentrated-liquidity venues (Orca, Meteora DLMM, Raydium CLMM) | Listable and lendable at launch; shorting later | ✅ Owner (v0.3, 2026-09-27) |
| — | SOL settlement fair-value cap | S1 unchanged; S2/S3 lenders' take capped at fair value, excess to borrower (§5.3) | ✅ Owner (v0.3, 2026-09-27) |
| — | Margin rule | Enforce the table's Opening CR after fees, at spot and EMA (§4) | ✅ Owner (v0.3, 2026-09-27) |
| O16 | Lender SOL income | Rent credited continuously; one-off lender income vests over 6 h (§2.4) | ✅ Owner (v0.3, 2026-09-27) |
| O6 | Emergency venue-breakage powers | Route-broken flag + bounded venue extra accounts; no fund movement (§11) | ✅ Owner (v0.3, 2026-09-27) |
| O15 | Fair value for expired positions in S2/S3 | 2% expiry fee, not the liquidation penalty (§5.3) | ✅ Owner (v0.3, 2026-09-27) |
| N22 | Liquidation keeper pay on small positions | Floor at the SOL-settlement keeper reward, topped up from the borrower remainder (§6.3) | ✅ Owner (v0.3, 2026-09-27) |
| M1-SEC-1 | Emergency veto over pending governance changes | Parameter changes only; authorities and timelock changes can be cancelled only by governance (§11) | ✅ Owner (v0.3, 2026-09-27) |
| O13 | Pool age for tier criteria when the pool's creation time is not provable | Counted from short.win's first observation (graduation or listing) (§4) | ✅ Owner (v0.3, 2026-09-28) |
| O13b | Age of CPMM pools with a provable `open_time` (after mid-2024) | Counted from `open_time`, not first observation (§4; M3-SEC-4) | ✅ Owner (v0.3, 2026-09-28) |
| M4R-1 | Exits right after a Raydium pool switch | Redesigned before M5: the switch reads no single-transaction sample; exits wait for 8 s of the new pool's own observations ("post-change confirmation") | ✅ Owner (v0.3, 2026-09-28) |
| #99 | Pool switches that carry open positions | Only onto a pool whose normal opening caps every open position fits; otherwise positions exit through S3 at fair value | ✅ Owner (v0.3, 2026-09-29) |
| FITS-2 | Liquidity pushes by unlocked-LP holders on Raydium | Liquidation's spot check reads max(live, smoothed) liquidity; keeper observes every second while liquidity anomalies recur on markets with positions | ✅ Owner (v0.3, 2026-09-29) |
| FITX-1 | A genuine short withdrawal whose low reading is then frozen | Accepted as a bounded residual: it forces only the liquidation the same LP gets by withdrawing for good (same second, same price, borrower no worse off) | ✅ Owner (v0.3, 2026-09-29); AIR-14's widening ✅ Owner (2026-10-13, fourteenth round) |
| M4F-1 | Pool switch that would carry open positions (Raydium fast path; a list-only market with positions) | Switch only if every open position fits the new pool's normal opening size caps (§4); if no pool fits, positions stay on the failed pool and exit through S3 at fair value (§5.3) | ✅ Owner (v0.3, 2026-09-29) |
| M6R-1 | Third-party liquidity on canonical PumpSwap pools (anyone can deposit and withdraw) | The Raydium liquidity-push protections apply to PumpSwap: liquidation's spot check at max(live, smoothed) liquidity; the keeper's per-second coverage of liquidity anomalies on markets with positions; the pool's locked depth taken from its burned LP bounds sizing, tier and the exit reference, as on Raydium | ✅ Owner (v0.3, 2026-09-30) |
| M6-SEC-1 | The price seed after a venue change or graduation reads the lowest recorded price | The seed may fall by at most one normal smoothing step per second, as it may rise; no exit is forced by a seed that lags a fall | ✅ Owner (v0.3, 2026-09-30) |
| M6-SEC-3 | New curve positions while PumpSwap has disabled pool creation (graduation would stall) | Refuse new opens on the curve while that switch is set; existing positions unaffected | ✅ Owner (v0.3, 2026-09-30) |
| M6R-4 | Exit delay right after graduation | Accepted: keeper exits on a graduated position come 8–12 s after the migration (the new pool's prices are confirmed first) | ✅ Owner (v0.3, 2026-09-30) |
| M6D-1 | Sizing on PumpSwap while third-party liquidity sits in the pool (a position sized on a deposit that is withdrawn as a pump starts can end in lender loss) | PumpSwap is sized on its burned (permanently locked) liquidity only: position size, market caps and the tier count none of a third party's deposit; Raydium keeps sizing on its locked liquidity over the 50 % minimum. Every other place that credits the locked liquidity over the minimum takes 100 % on PumpSwap as well: the exit reference's depth limit is the burned liquidity itself, so a deposit withdrawn at a position's deadline cannot hold its expiry up (the #106 verification's M6E-1; ECON #106, §12.12 (12) (f)) | ✅ Owner (v0.3, 2026-09-30) |
| O17 | In-house keeper operating budget | Targeted coverage of every market with open positions (per-second only when needed: near thresholds, price/liquidity anomalies, warm-ups, guards; otherwise ~30 s); per-market budget whose exhaustion puts that market in degraded mode (opens paused automatically via the pause-opens-only coverage_authority, alert; exits continue); priority-fee ceiling 100,000 µlamports/CU | ✅ Owner (v0.3, 2026-09-30) |
| M7-13 / M7-14 | Keeper budget after the M7 review: one budget per market, or one for the whole protocol (price-burst griefing; junk markets listed to make the keeper spend) | **One global daily keeper budget** replaces the per-market budgets as the primary control. It funds the covered markets in order of their lenders' value at risk (the borrowed tokens valued at the smoothed price); price bursts and liquidity-anomaly coverage draw on it; markets below a configurable value-at-risk floor (junk or tiny markets) get the 30-second idle cadence only; a market the budget can no longer cover enters degraded mode (opens paused through the coverage role, alert; exits continue) — per-market degraded mode still applies | ✅ Owner (v0.3, 2026-10-01) |
| M7-12 | Threshold of the on-chain coverage-gap signal | 40 s (was 30 s), so the keeper's 30-second idle cadence with normal landing delays no longer raises it on healthy markets; a program constant | ✅ Owner (v0.3, 2026-10-01) |
| M7-15 | How old the cached SOL/USD price may be before tiers fall back to C | 600 s (was 3,600 s; the lowest allowed value); the keeper keeps pushing the price at least every 30 s and alerts when the cache is 300 s old | ✅ Owner (v0.3, 2026-10-01) |
| M7-3 | Who can clear a market's keeper coverage pause | The coverage key, and also the emergency multisig — clear only, never set — so a lost or hijacked coverage key can be neutralised at once; moves no funds; opens still resume only 5 s after any clear (§11) | ✅ Owner (v0.3, 2026-10-01) |
| M7-11 | A sale in an open's own transaction lowers the notional the open records (launch cap, minimum position, open fee) | Accepted and disclosed: a recorded notional is at least 90 % of the honest quote, so a market's launch cap can be exceeded by at most about 11 % of honestly quoted notional; the margin is unaffected | ✅ Owner (v0.3, 2026-10-01) |
| Q1 (M10) | Where CI runs | Merge the working branch into main; CI and the nightly fuzz run on main (the nightly fuzz is opt-in since the CI-minutes decision below); main is fast-forwarded after each verified step | ✅ Owner (2026-10-02) |
| Q4 (M10) | Multisig layout | Governance: Squads v4 3-of-5 with a 48 h Squads time lock, holds the upgrade authority (vault 0) and receives fees (vault 1); Emergency: Squads v4 2-of-4, no time lock, is the pause authority; a change through governance therefore takes about 96 h (48 h Squads + 48 h program timelock). The program checks that governance, pause authority, fee recipient and the coverage key are all different keys (§11) | ✅ Owner (2026-10-02) |
| Q3 (M10) | Security contact | security@short.win and a policy page (short.win/security); the same values in the programs' on-chain security.txt; the repository goes public (or gets a public mirror) before mainnet | ✅ Owner (2026-10-02) |
| Q2 (M10) | Audit shape | One full private audit plus a public contest (Sherlock or Cantina) on the fixed code, then a bug bounty at the beta; the owner requests quotes from 3–4 Solana audit firms (§9) | ✅ Owner (2026-10-02) |
| Q5, Q6 (M10) | Calibration data source; mainnet spending before the audit | Defaults: gap-frequency calibration runs on public RPC; no mainnet spending, so the live-network checks (v1 transactions landing, live bundle submission) stay unverified until the launch milestone; the multisigs are created on mainnet in M11, with the members' keys | ✅ Owner (2026-10-02, defaults) |
| M10-8C-1 | Rent paid above what had accrued (the minimum rent of an early exit) was credited to lenders at once, so a deposit made around the exit captured most of it | Fixed: it vests over 6 h like other one-off lender income, on every kind of exit (§2.4); a deposit wrapped around such an exit now gets nothing | ✅ Owner (2026-10-02) |
| Release (M10) | Program version at the audit | 1.0.0 for both programs, tagged `v1.0.0` together with the audit tag; post-audit fixes are 1.0.x (§9) | ✅ Owner (2026-10-02) |
| Audit scope (M10) | How much of the keeper the audit covers | The keeper's core: coverage budget, liquidation and settlement duties, transaction building (about 4.7k lines), with the on-chain programs (§9) | ✅ Owner (2026-10-02) |
| Calibration (M10) | Data for calibrating gap risk per tier | Public RPC only for now; Tier A (and possibly B) results are upper bounds; revisit after 30 days of data | ✅ Owner (2026-10-02) |
| CI minutes (M10) | The nightly fuzz run on main uses more CI time than the private repository has | The nightly fuzz run is opt-in (a repository switch, off by default) and long fuzz runs are done locally; verified work is pushed to main in batches, each push running the full CI | ✅ Owner (2026-10-02) |
| O19 | Held sandwich on the real venues (an attacker holds a pool's price up for seconds, then liquidates) | Accepted; no program change. The measured figures are disclosed per venue (§12 R2; the economics document, the audit's known issues and the borrower disclosure): for a 3-s hold at +15 % on a full-size position, Raydium AMM v4 +61.27 SOL, Raydium CPMM +37.33 SOL (the owner's decision cites CPMM's atomic sandwich extra over an honest keeper, +5.34 SOL), PumpSwap +10.20 SOL to the attacker, all of it from the liquidated borrower; the lenders' share is not reduced. Keeper competition is the lever (M11). Measured before AIR-22 (fourteenth round below): the simulated plan now needs a 2-s hold, and the real venues have not been re-measured | ✅ Owner (2026-10-03) |
| Late depositor (M10) | A lender who deposits while the pool carries a large unpaid rent balance shares any later shortfall pro-rata, and can owe far more SOL than it ever received | Kept as designed; the deposit screen shows the pool's unpaid rent and the deposit's share of it, and warns when that share is at least 1 % of the deposit's value or 1 SOL (§2.4). Web only, no program change | ✅ Owner (2026-10-03) |
| Extras denylist (M10) | Emergency extra venue accounts that name the pool's own reserve accounts or the borrower | Refused by the program on every venue: the adopted pool and the accounts holding its reserves, and the position's borrower, can never be extra accounts (§11); built before the code freeze | ✅ Owner (2026-10-03) |
| Squads signing (M10) | How multisig members sign | In the Squads app; short.win's governance tool only prepares and verifies transactions, with no hardware-wallet support; the runbook's worst-case Emergency extra-accounts change goes out as three single-signer transactions | ✅ Owner (2026-10-03) |
| B2-2 (M10) | A settlement's rent top-up (the rent above what was credited while the position was open), a consequence of the M10-8C-1 fix | Kept as built: on every settlement it vests over 6 h to the lenders who stay, like other one-off lender income, so S1 is immediate but for that top-up (§2.4). *(Superseded for S1 by the AI review's AIR-21, fourteenth round below: the part of an S1 above the value of the returned tokens at the smoothed price plus the rent now vests too.)* | ✅ Owner (2026-10-03); AIR-21's change to S1 ✅ Owner (2026-10-13, fourteenth round) |
| Extras denylist residuals (M10) | What the extras denylist still allows: a borrower whose wallet is a venue-wide extra account cannot be keeper-exited through that set; a venue change that needs the pool's own account as an extra needs a program upgrade; venue accounts outside the list can still be named; a refused set costs one more simulation | Accepted as disclosed (§11; the audit's known issues) | ✅ Owner (2026-10-03) |
| Deposit warning (M10) | When the deposit screen warns about the pool's unpaid rent | At 1 % of the deposit's value or 1 SOL, as built | ✅ Owner (2026-10-03) |
| Security policy (M10) | The defaults of the security policy (SECURITY.md and the /security page) | Kept: best-effort response times, no published encryption key yet, no bug bounty before the beta, the standard safe harbour, and the keeper, indexer and API short.win operates in scope; revisited in M11 | ✅ Owner (2026-10-03) |
| PUMP-2026-10-1 (M10) | Since pump's redeploy of 2026-10-02, PumpSwap keeps the fees of its new trade instructions in the pool's SOL vault until anyone sweeps them out, and pays no sale out of them; short.win counted them as SOL a sale can be paid from (in the open's sale limit and the pool's tier liquidity) | Fixed before the code freeze: short.win reads the pool's SOL as the vault less those fees, and its tier liquidity subtracts the full virtual reserve (§4); the app, the keeper and the test oracles follow | ✅ Owner (2026-10-03) |
| PUMP-2026-10-2 (M10) | Since the same redeploy, the first trade on an older, smaller PumpSwap pool or bonding curve grows its account and charges that trade the extra rent (81,280–386,080 lamports measured), which comes out of an open's sale; when the open's slippage tolerance cannot cover it the open is refused (on a 211-byte pool at its minimum rent, no open within the default wallet cap qualifies) | Disclosed, no program change; the app explains such a refusal: the pool (or bonding curve) needs a first trade to grow its account, so try again after another trade on it, or with a larger size (the app's wording: FRONTEND §5.9) | ✅ Owner (2026-10-03) |
| KI-66 (M10) | Since the sixth round of 2026-10-07 only a lender's own signature converts its position through a pool rebase (KI65-REV-SEC1). A lender holding a surplus (realised SOL beyond its entitlement, which belongs in the lenders' shared pot) that sits behind a rebase and never returns could then no longer be synced by anyone else, so the lenders the pot owes SOL waited for its owner | Pot-only touch: anyone's sync of such a lender, without the owner's signature, moves its surplus as of the rebase to the pot, and converts nothing (no shares, no rounding remainder, no epoch), so it cannot reorder conversions; the owner's later conversion counts nothing twice. What the absent position earns after the rebase still waits for its owner (disclosed, the audit's known issues KI-66). Program change; the full gate and a fresh 24 h fuzz | ✅ Owner (2026-10-07, seventh round) |
| Sub-unit conversion warning (M10) | After a rebase, a lender's position worth less than one new share converts to none at its own next claim, deposit or withdrawal, and whoever converts after it can receive that share — while income vests, possibly with all of the vesting income (KI-65's disclosed residual) | Web: the review of such a transaction shows a disclosure with the vest's end and advises waiting until then; the lender residuals disclosure gains the matching sentence (approved) | ✅ Owner (2026-10-07, seventh round) |
| KI-67 (M10) | `claim_sol` left a lender's position open once it had paid it in full after the lender's exit (every lender of a wiped market hit it), locking about 0.0026 SOL of rent | Fix: `claim_sol` closes the position and returns its rent when the claim leaves it current with no shares, nothing owed and no lock (`withdraw`'s conditions; a lock term without a lock is never read again, so it does not keep it open — confirmed by the owner, 2026-10-09, eleventh round); a later deposit re-creates it from scratch; a second claim of the closed position fails instead of paying nothing. Program change | ✅ Owner (2026-10-08, eighth round; the lock-term detail confirmed 2026-10-09, eleventh round) |
| KI-66 full (M10) | What an absent lender's unconverted shares earn after a pool rebase still reached the shared pot only when its owner returned (the seventh round's residual; 8 of 2,000 simulated pools left a present lender unpaid, worst 2.13 SOL) | Full fix: anyone's sync of such a lender also walks a "forward chain" — the share count no conversion order can lower — through the later rebases to the current epoch and moves that surplus to the pot too, still converting nothing; the owner's later conversion takes it back exactly. Residual: the rounding units an owner's own conversion may add (worst measured 727,760 lamports). Program change; the full gate and a fresh 24 h fuzz | ✅ Owner (2026-10-08, eighth round) |
| Claim-time auto-touch (M10) | A claimant short of its entitlement depended on someone syncing the absent lenders whose surplus it needs (the web only synced current-epoch lenders) | Web + WASM: the claim transaction prepends the syncs it needs (at most 4, within the transaction size and compute limits, simulated before signing); a chained owner's own settlement is capped at 7 records per transaction (compute) | ✅ Owner (2026-10-08, eighth round) |
| KI-68 (M10) | A sync of a current lender (or one parked on a pool's full-wipe record) by someone else rounds that lender's income down at each sync, as since M1: at most 1 lamport of its entitlement and 1 of its realised SOL per sync, while each sync costs its sender a 5,000-lamport transaction fee | Accepted and disclosed (the audit's known issues KI-68); no program change | ✅ Owner (2026-10-08, ninth round) |
| Claim-time sync candidates (M10, FE-30) | The claim picked the lenders to sync from the indexer's first 24, ordered by raw share counts, which are not comparable across pool rebases, so a large current lender could push an absent lender holding a surplus out of the list | Web: lenders behind the pool's share epoch (which includes, with one exception, every lender with a forward chain) first, then the rest, reading further pages when the first does not cover the claimant's shortfall, ranked by what each sync moves to the pot as today. A closed position's address that someone funded fails a stale review the way a closed position does, and the review's Retry rebuilds it too. As built and confirmed by the owner (2026-10-09, tenth round): the indexer gains a filter by that epoch, at most 4 pages of 50 lenders are read (for the review's waiting time), and the indexer refuses to start with a page limit below the web's largest request, 200 | ✅ Owner (2026-10-08, ninth round); the filter, the bound and the page-limit floor ✅ Owner (2026-10-09, tenth round) |
| New lender texts (M10) | The review texts for other lenders' syncs, syncs left for the next claim, a claim that closes the position, and a conversion to no shares during a vest | Approved as written (FRONTEND §4.2) | ✅ Owner (2026-10-08, ninth round) |
| pump v3 (after the audit tag) | pump.fun redeployed its programs on 2026-10-08, 10-12 and 10-13: bonding curves gained new trade instructions (v3) that buy past the curve's last tokens by creating the would-be pool's part on the spot ("synthetic migration"), and a pool created after such a buy opens at a higher price than short.win priced | Change the program: trade every curve leg with pump's v3 instructions, and price a stuck migration on the pool pump will really create; re-pin to pump's 2026-10-13 build; release as **1.0.1** (fifteenth round below) | ✅ Owner (2026-10-13) |
| D1 (pump v3) | pump's v3 trades refuse "cashback" coins, which can then not be shorted on the curve | (a) Refuse live cashback curves at listing (`CurveCashbackCoin`, 6109); such a coin can be shorted once it graduates to PumpSwap. About 3.3 % of recent curves; pump no longer creates new ones | ✅ Owner (2026-10-13) |
| D2 (pump v3) | The three-transaction bundle that closed a position larger than the curve's remaining tokens, and its helper program `complete_curve`, have no use once one v3 buy does it | Retired: the bundle, the keeper's straddle plan and the helper (never deployed) leave the code, the release and the audit scope; **the auditors must be told** of the scope change with the audit addendum | ✅ Owner (2026-10-13) |
| D3 (pump v3) | The risk and exit price of a position larger than the curve's remaining tokens | What the single v3 buy costs (`B̂_v3`: the curve's remaining tokens plus the rest from the would-be pool, at the curve's fee), not the old bundle's cost on the new pool: 0.8–96 % lower for such positions, so their liquidations come later; above the slippage limit such a position settles in SOL (S2) at once | ✅ Owner (2026-10-13) |
| D4 (pump v3) | Two program messages | As drafted: 6109 "pump.fun's current trades do not support cashback coins on the curve. This coin can be shorted once it graduates to PumpSwap."; 6031 "Buyback exceeds what the curve and its post-migration pool can deliver" (the app shows the same; FRONTEND §4.3) | ✅ Owner (2026-10-13) |
| 17 | Who executes the short? | short.win, atomically at open | ✅ Owner |
| — | Sale proceeds kept in position vault | Yes | ✅ Owner (v0.3, 2026-09-27) |
| 18 | Fee split | §6.3. Adopted as the v1.0.0 design; the values stay tunable by governance within the compiled bounds (ECONOMICS §15), behind the §11 timelock | ✅ Delegated; adopted by the owner (2026-10-09, eleventh round, O18) |
| 19 | Listing | Permissionless | ✅ Owner |
| — | Mint safety filter (freeze/permanent delegate/hooks/fees) | Reject automatically | ✅ Owner (v0.3, 2026-09-27) |
| 20 | Launch | Fully permissionless | ✅ Owner |

**Eleventh round (2026-10-09, the owner's sign-off before the code freeze):**

- **O18 accepted as the v1.0.0 design.** The delegated proposals of rows 10 (collateral and liquidation by tier), 13 (dynamic rent) and 18 (fee split), and every rule the economics document's decision register (ECONOMICS §14) listed as proposed — 37 rows, which now read "ADOPTED (owner, 2026-10-09, O18)" — are adopted as built. The settlement conditions S1–S3 of row 11 are among those rules (ECONOMICS §14 G13, G18, G19, N15). Parameter values stay tunable by governance within the compiled bounds (ECONOMICS §15), behind the §11 timelock.
- **O18's disclosed residuals accepted as disclosed:** the pool-switch rules' known limits — a standing minimum position pinning a Raydium market to a thinner pool, the M4 rules' two residuals, and the economics document's §12.12 residuals 1–4 and 13 (an attacker-only warm-up, the capital deposit freeze, the withdraw contest while the guard is set, a proposal wrapped in a pump, and where a carried position's fit is judged) — are accepted with the rules (the audit's known issues KI-16 … KI-21 and KI-25).
- **KI-67's lock-term detail kept as built:** closing a fully paid lender position ignores a leftover lock term when the position holds no lock (a lock of 0; the term is never read again) (row KI-67 above; the audit's known issues KI-67).
- **A position opened while a third party's deposit sits in a PumpSwap pool** keeps its margin priced on the pool as it is (accepted as built; the economics document's §12.12 residual 16, the audit's known issues KI-26). Pricing it on the permanently locked liquidity only, a program change, is not taken.
- **Extra event fields for the indexer** (IX-1: the protocol's rent share, the position size and the smoothed price on the liquidation, expiry and SOL-settlement events; IX-11: the record donation on the epoch-roll event) are deferred to a 1.0.x batch after the audit tag; until then the indexer derives them (INDEXER §14).

**Twelfth and thirteenth rounds (2026-10-09 … 2026-10-11, the web only; no program change):**

- **The web on phones** (twelfth round, 2026-10-09): the site works and looks good on phones and tablets as well as on a computer, keeping the dark trading-terminal style. Approved as built on 2026-10-10 (thirteenth round).
- **Newcomer help and four smaller changes** (thirteenth round, 2026-10-10): an introduction above the markets list, a glossary of the jargon and a "How it works" page; the lending form in the order of the short form; the keeper-coverage notice shown once; a Claim button that says what it pays; no text under 12 px. Connecting a phone wallet app through the Mobile Wallet Adapter waits until after the audit, and will be off by default. *(Lifted by the owner on 2026-10-13 (O-6): the Mobile Wallet Adapter is built and on by default, packages 0.7.0 / 3.0.0, the owner's real-device test before the release; the audit package's §7 O-6 row, the frontend document's §5.4 (6).)*
- **The new texts** (2026-10-11): all 143 approved as written (the frontend document's §4.3). One sentence of the approved disclosure "How this short can end" is amended, because a position past its deadline that can be liquidated is liquidated (§5.3): "Expiry: after {maxDuration} the position can be closed by anyone. Unless it can be liquidated at that point, you pay a {expiryFee} expiry fee instead of the penalty."
- **Claims** (2026-10-11): a shortfall under 10,000 lamports counts as none, and the Lending page's Claim works as the market page's does, syncing other lenders when that pays.

**Fourteenth round (2026-10-12 and 2026-10-13, the AI review's fixes, AIR-01 … AIR-24; `docs/audit/ai-review/`):**

- **AIR-22 kept** (the lower-median growth): a price held flat for one second in three no longer blocks liquidation in a fast pump. The cost is accepted: the simulated held-sandwich attacker needs 2 held seconds instead of 3 and nets +2.84 SOL instead of +1.98 SOL on the economics document's Raydium example; the borrower loses the same remainder and lenders plus protocol lose less (§12 R2, O19).
- **AIR-07's drained-pool gate kept:** while a pool's lent-out tokens leave at most 1/1,000 of its tokens unborrowed, deposits are refused (`PoolDrained`) until a repayment or settlement frees more, so no deposit can keep the vest from the lenders a settlement wipes. Some honest deposits wait (in the simulations 26 of 6,653, 102 of 5,910 and 378 of 26,749), and a drained pool takes no newcomer's tokens.
- **Residuals accepted and disclosed (the audit's known issues):** a holder's burn earlier in the same transaction still raises a PumpSwap position's exit risk fee (AIR-10); a third party's `ExtendProgram` can block short.win, or a venue, for the rest of a slot at about 0.052 SOL a slot until the program reaches 10 MiB, and the keeper retries in the next slot (AIR-18; short.win's program account is not pre-extended); the S3 chain's fee cap is the lowest any open position locked since the market last emptied, conservative because a closed position may have set it (AIR-19).
- **No behaviour change, wording and known issues only:** `apply_config` installs new authority keys without their signatures (AIR-04); a later lock in a lock era decays only once borrowing falls below the era's first-lock level, otherwise at its `lock_until` (AIR-11); inside an LP's own withdrawal only the settlement box corner vetoes S1/S2 (AIR-20; its optional code fix is not taken).
- **Confirmed by the owner (2026-10-13) — S1's excess now vests (AIR-21), and a halted venue's tokens count at 0 (AIR-09).** Both are program fixes on the review branch, drafted from the code and the sims rather than decided on 2026-10-12; the owner confirmed both on 2026-10-13, and AIR-21 changes row B2-2 above. The part of an S1 take above the returned tokens' value at the smoothed price plus the rent due (`⌊Q·P_ema/WAD⌋ + R_due`) now vests over 6 h, as an S2/S3 lump does, so S1 is no longer "immediate but for that top-up"; a genuine S1 loss (a take at or below that value) still vests only its rent top-up (B2-2). On a venue that cannot execute, the tokens count at 0, so of an S3 there only the rent due stays immediate, but for its top-up, and the rest vests (§2.4; the economics document's §8.6).
- **Accepted by the owner (2026-10-13) — AIR-14 moves a residual accepted under O18.** AIR-14 caps a Raydium exit's reference at the pool's locked depth (a deposit held into a deadline and pulled there could hold an expiry up for tens of minutes). It also removes the slow-liquidity credit that kept a liquidation wrapped in a liquidity provider's own withdrawal out of its bound. In the simulations a position at a 1 % pool's open cap is now liquidated as soon as a genuine withdrawal makes the drained pool call for it, instead of waiting and possibly gapping into S1. But the steady-state withdraw drag accepted under O18 (the economics document's §12.12 residual 10) widens. A drag of attacker-only seconds forces one level, 2.06× against 2.065×; a single missed keeper second forces a cap-sized position at 2.07× against 2.078×; and a low held for 3–20 s, then frozen, forces more levels on more pools. Every level a held drain forces is still one the provider's withdrawal for good would force too. The owner accepted the widened residual on 2026-10-13 (the audit's known issues KI-23).
- **Also decided on 2026-10-13:** the lender residuals disclosure quotes the held model's Tier A figures under AIR-22 (about 3.34 SOL on a fresh position and 4.54 SOL on an aged one, instead of 2.06 and 8.02; the frontend document's FE-31). The ≥ 24 h fuzz run is waived for the AI review's fix batch; CI and an external fix review are still owed (the audit package's §12). The indexer gets no decoder for the two-argument `revalidate_venue` of a build before AIR-01: a backfill of such a build's history would quarantine it.
- The other fixes correct the program to the rules as designed, and change no rate, fee split or parameter default (AIR-06 raises `min_notional`'s compiled floor to 0.053 SOL, below its 0.1 SOL default). They do move some measured outcomes; the design documents carry them (ECONOMICS changelog #117, ARCHITECTURE changelog 78; the confirmations of 2026-10-13 in #118 and 79).

**Fifteenth round (2026-10-13, pump v3; the design `pump-final`; built as 1.0.1, ECONOMICS changelog #119, ARCHITECTURE changelog 81, the audit addendum §13):**

- **The program changes** (rows "pump v3" and D1 … D4 above): every bonding-curve trade uses pump's v3 instructions; a close, liquidation or expiry of more tokens than the curve has left completes the curve in one transaction; a migration stuck after such a buy is valued on the pool pump will really create; live cashback curves are refused; the straddle bundle and its helper are retired.
- **Every residual the design lists is accepted** (its §16 and §17; the audit's known issues PUMP-2026-10-13-1 … -6), among them: whoever completes a curve past its last tokens sets how much higher the new pool opens, which matters only if pool creation stays halted long enough for SOL settlement (about 70 min) and is capped at the frozen positions' take; the price smoothing can lag a pool that opened much higher; every curve leg now depends on pump's v3 instructions.
- **The fee of such a buy follows the coin's live supply** (pump charges both of its parts at the fee tier of the curve's market cap on the live supply), so a holder's burn earlier in the same transaction can raise it under a multi-tier fee schedule, and the fee checks of opens and routes keep the fixed 1-billion-token supply. Accepted and disclosed beside AIR-10's burn residual, under the same decision of 2026-10-12 (the audit's known issues KI-77); no effect while pump's curve fee is one flat tier, as it is today. *(Decided after this round: the live-supply fee appeared only when the build's phase P1 introduced it on 2026-10-14; the owner confirmed it on 2026-10-16.)*
- **Release order:** the keeper first (it handles the old and the new account lists of curve exits, choosing by the deployed program), then the program upgrade through governance, then the web app; the next release drops the keeper's old list.
- **Version 1.0.1** (the 1.0.x rule above); the program pinned to pump's 2026-10-13 build.

---

## 15. Agent implementation brief

Treat this document as the baseline. Don't invent economics; any new parameter must be configurable and listed in the decision log.

- **Protocol agent:** accounts, instructions (create_market, deposit, withdraw, open_short, close_short, liquidate, sol_settle, expire, crank_price), state transitions, invariants in §10.
- **Economic/risk agent:** formalize CR, tier assignment, caps, rate curve, S1–S3; simulate 2×/5×/10× gaps, liquidity drains, and graduation mid-position; report expected lender loss per tier.
- **Application agent:** lender, borrower, market, and position UX; risk disclosures; indexer API.
- **Security agent:** threat model (§12), invariant/property tests, fuzzing of curve math and liquidation paths, adversarial scenarios (manipulation, self-dealing, MEV).
