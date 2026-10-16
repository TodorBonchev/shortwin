// Smoke test of the wasm-bindgen facade through Node: BigInt u64/u128 arguments and
// results, `undefined` for undefined quotes, thrown MathError texts, and the numbers of
// the research worked examples (R-PUMP §4.2, R-VEN §2.4, R-RAY §3.5).
//
// cargo build -p shortwin-math-wasm --target wasm32-unknown-unknown --release
// wasm-bindgen --target nodejs --out-dir target/wasm-pkg target/wasm32-unknown-unknown/release/shortwin_math_wasm.wasm
// node crates/shortwin-math-wasm/js/smoke.cjs target/wasm-pkg
const assert = require("node:assert/strict");
const path = require("node:path");
const m = require(path.resolve(process.argv[2] ?? "target/wasm-pkg", "shortwin_math_wasm.js"));
const WAD = 10n ** 18n;

assert.equal(typeof m.version(), "string");

// R-PUMP §4.2 curve worked example.
const pamm = new m.PammSchedule(10n ** 15n, true, 0, false);
pamm.addTier(0n, 2, 93, 30);
pamm.addTier(420_000_000_000n, 20, 5, 95);
const curve = m.Venue.pumpCurve(new m.Fees(0, 95, 30), 279_900_000_000_000n, 30_000_000_000n, 15_000_001n, 206_900_000_000_000n, 0n, pamm);
const s = new m.CurveState(824_130_602_106_172n, 39_059_343_535n, 544_230_602_106_172n, 9_059_343_535n, false);
const q = 10_000_000_000_000n;
const b = curve.buyCurve(q, s);
assert.equal(b.cost, 485_764_635n); // ECON §3.2: the quote rounds up (the program charges 485,764,634)
assert.equal(b.net, 479_767_540n);
assert.equal(curve.sellCurve(q, s).proceeds, 462_410_820n);
assert.equal(curve.onewayBps(), 125);
assert.equal(curve.onewayPpm(), 12_500);
assert.equal(m.exceedsCap(curve.onewayPpm(), 124), true); // fee gates compare ppm with cap·100
assert.equal(m.exceedsCap(curve.onewayPpm(), 125), false);
// A curve venue has no (x, y) quote: the thrown Error carries the MathError text.
assert.throws(() => curve.buy(q, 1n, 1n), /invalid input/);

// pump-final §5 (P1): pump's synthetic migration on G0's near166 curve (pump charged
// 22,944,563,744 for rT + 10e12; B̂_v3 is 2 above), the migrated pool with pcQ (98u8RJ),
// ρ and the boost.
const sched = new m.CurveSchedule(true, 0, false);
sched.addTier(0n, 0, 95, 30);
const near = new m.CurveState(333_021_296_825_365n, 96_660_487_854n, 53_121_296_825_365n, 66_660_487_854n, false);
const fLive = sched.resolveAtSupply(near.vt, near.vq, 999_818_465_159_445n);
assert.equal(fLive.protocol, 95);
const nearVenue = m.Venue.pumpCurve(sched.resolve(near.vt, near.vq), 279_900_000_000_000n, 30_000_000_000n, 15_000_001n, 206_900_000_000_000n, 0n, pamm);
const synthetic = nearVenue.withSynthetic(fLive);
const qSyn = 53_121_296_825_365n + 10_000_000_000_000n;
assert.equal(synthetic.buyCurve(qSyn, near).cost, 22_944_563_746n);
assert.ok(nearVenue.buyCurve(qSyn, near).cost > 22_944_563_746n); // B̂_str without withSynthetic
assert.equal(synthetic.buyCurve(53_121_296_825_365n + 206_900_000_000_000n, near), undefined); // q − rT ≥ X_s
assert.throws(() => m.Venue.rayV4(25n, 10_000n).withSynthetic(fLive), /invalid input/);
// P1 correction (a) under G0's tier tripwire: on near166's burned supply the 10¹⁵ market cap
// (290,253,172,320) sits in the 70/22 tier, the live supply's (290,200,481,257) in the 80/25 one,
// which pump charged for rT + 50e12 (45,906,126,201): B̂_v3 covers it only at resolveAtSupply's fee.
const tiers = new m.CurveSchedule(true, 0, false);
tiers.addTier(0n, 0, 90, 28);
tiers.addTier(290_174_135_726n, 0, 80, 25);
tiers.addTier(290_226_826_789n, 0, 70, 22);
const fFixed = tiers.resolve(near.vt, near.vq);
const fTier = tiers.resolveAtSupply(near.vt, near.vq, 999_818_465_159_445n);
assert.deepEqual([fFixed.protocol, fFixed.creator], [70, 22]);
assert.deepEqual([fTier.protocol, fTier.creator], [80, 25]);
const tierVenue = m.Venue.pumpCurve(fFixed, 279_900_000_000_000n, 30_000_000_000n, 15_000_001n, 206_900_000_000_000n, 0n, pamm);
const q50 = 53_121_296_825_365n + 50_000_000_000_000n;
assert.equal(tierVenue.withSynthetic(fTier).buyCurve(q50, near).cost, 45_906_126_205n);
assert.equal(tierVenue.withSynthetic(fFixed).buyCurve(q50, near).cost, 45_906_126_201n - 59_057_853n);
const done = new m.CurveState(279_900_000_000_000n, 115_005_359_190n, 0n, 85_005_359_190n, true);
const pool1 = done.migratedPool(15_000_001n, 200_661_149_081_575n, 2_642_475_552n);
assert.deepEqual([pool1.x, pool1.y], [200_661_149_081_575n, 87_632_834_741n]);
const pool0 = done.migratedPool(15_000_001n, 206_900_000_000_000n, 0n);
assert.equal(m.migratingFvPrice(WAD, pool0.x, pool0.y, pool1.x, pool1.y) / 10n ** 12n, 1_063_149n); // ρ
assert.equal(m.migratingFvPrice(1n, 1n, 1n, 0n, 1n), undefined);
assert.equal(m.migrationBoost(pool1.x, pool1.y), 17_584_505_316n);
// P5 (pump-final §5.4, §8): migratingFv, the program's FV on a stuck migration. At the frozen
// curve's pool price, ρ reprices it onto the pool migrate_v2 created: within a basis point of that
// pool's own buy; without the pool part it is the plain pool's FV at p itself.
const doneSyn = m.Venue.pumpCurve(new m.Fees(0, 95, 30), 279_900_000_000_000n, 30_000_000_000n, 15_000_001n, 200_661_149_081_575n, 2_642_475_552n, pamm);
const donePlain = m.Venue.pumpCurve(new m.Fees(0, 95, 30), 279_900_000_000_000n, 30_000_000_000n, 15_000_001n, 206_900_000_000_000n, 0n, pamm);
const p0 = m.priceWad(pool0.x, pool0.y);
const fv = doneSyn.migratingFv(q, p0, done).cost;
const onPool1 = m.Venue.pumpSwap(pamm.resolve(pool1.y, pool1.x)).buy(q, pool1.x, pool1.y).cost;
assert.ok((fv > onPool1 ? fv - onPool1 : onPool1 - fv) * 10_000n < onPool1);
const plainFv = donePlain.migratingFv(q, p0, done).cost;
assert.equal(plainFv, m.Venue.pumpSwap(pamm.resolve(pool0.y, pool0.x)).buyRepriced(q, m.liquidity(pool0.x, pool0.y), p0).cost);
assert.ok(plainFv < fv);
assert.throws(() => m.Venue.rayV4(25n, 10_000n).migratingFv(q, p0, done), /invalid input/);

// Raydium AMM v4 (R-RAY §3.5 (A)); u128 reserves round-trip through BigInt.
const v4 = m.Venue.rayV4(25n, 10_000n);
const bq = v4.buy(235_932_179_000n, 37_639_277_914_829_371n, 17_595_123_332_536n);
assert.equal(bq.cost, 110_567_639n);
assert.equal(bq.tokenAfter, 37_639_277_914_829_371n - 235_932_179_000n);
assert.throws(() => m.Venue.rayV4(10n, 10n), /invalid input/);
const big = (1n << 127n) + 12345n;
const r = m.reprice(big, WAD);
assert.equal(r.x, big);
assert.equal(m.reprice(0n, WAD), undefined);
assert.equal(m.liquidity(1n << 60n, 1n << 60n), 1n << 60n);
assert.throws(() => m.liquidity(1n << 64n, 1n << 64n), /overflow/); // X·Y ≥ 2¹²⁸ (ECON §2.2)

// PumpSwap (R-VEN §2.4 TX-BUY) and the liquidation price.
const ps = m.Venue.pumpSwap(pamm.resolve(18_917_931_618n + 17_584_505_333n, 482_553_257_944_361n));
const y = 18_917_931_618n + 17_584_505_333n;
const bb = ps.buy(9_155_477_586_733n, 482_553_257_944_361n, y);
assert.equal(bb.cost, 714_778_826n);
const L = m.liquidity(482_553_257_944_361n, y);
const P = m.priceWad(482_553_257_944_361n, y);
const risk = ps.capped(100);
const bHat = risk.buyRepriced(q, L, P).cost;
const vEff = 2n * bHat;
const pLiq = risk.liquidationPrice(q, vEff, 12_500, L, P);
assert.ok(pLiq > P && pLiq < 2n * P);
assert.equal(m.crBelow(vEff, risk.buyRepriced(q, L, pLiq).cost, 12_500), true);
assert.equal(m.crBelow(vEff, risk.buyRepriced(q, L, pLiq - 1n).cost, 12_500), false);
assert.equal(m.crBps(vEff, undefined), 0n);
assert.equal(m.marginD4(100n, 100n, 120n, 20_000), 140n);
// An undefined EMA quote prices the spot quote alone (`marginD4`'s and `risk::margin_d4`'s
// documented `Option` semantics since the scaffold, as the sim and the program's open):
// ⌈20,000·100/10⁴⌉ − 100. The old `assert.throws` here contradicted that and stopped the
// script before the checks below (M6E-3).
assert.equal(m.marginD4(100n, 100n, undefined, 20_000), 100n);
assert.equal(m.marginD4(100n, 100n, 100n, 20_000), 100n);
assert.equal(m.marginD4(100n, 90n, undefined, 20_000), 80n);

// Rent due and waterfall previews.
const open = new m.RentBasis(0n, 0n, 0n);
const later = open.accrue(WAD, 0, 8_000, 31_536_000n);
const pos = new m.PositionRent(1_000_000_000n, open, 1_000, 5_000, 100_000, 1_500, 7n);
assert.equal(pos.due(later), 100_000_000n);
// The lenders' credit of that year from the aggregate `A_j = q·coef_j·(10⁴ − 1,500)`: 85 % of it.
const share = 10_000n - 1_500n;
const [a0, a1, a2] = [1_000n, 5_000n, 100_000n].map((c) => 1_000_000_000n * c * share);
assert.equal(open.lenderCredit(a0, a1, a2, WAD, 0, 8_000, 31_536_000n, 0n), 85_000_000n);
// AIR-03: the market's carried remainder (`Market.lender_credit_rem`) joins the floor. A day is
// 232,876 + 260/365 lamports, so 105/365 of the denominator completes the lamport; a remainder
// at the denominator throws, and so does a call without one.
const DEN = 10_000n * 10_000n * 31_536_000n * WAD;
const edge = (105n * DEN) / 365n;
assert.equal(open.lenderCredit(a0, a1, a2, WAD, 0, 8_000, 86_400n, edge - 1n), 232_876n);
assert.equal(open.lenderCredit(a0, a1, a2, WAD, 0, 8_000, 86_400n, edge), 232_877n);
assert.throws(() => open.lenderCredit(a0, a1, a2, WAD, 0, 8_000, 86_400n, DEN), /invalid input/);
assert.throws(() => open.lenderCredit(a0, a1, a2, WAD, 0, 8_000, 86_400n));
// Liquidation with the keeper floor at K_s (N22): 15 + a top-up of 5 from the borrower.
const liq = m.previewLiquidation(1_000n, 600n, 100n, 500n, 550n, 1_500, 1_000, 5_000, 2_000, 3_000, 20n);
assert.equal(liq.buyback + liq.keeper + liq.protocol + liq.lenders + liq.borrower, 1_000n);
assert.equal(liq.keeper, 20n);
assert.equal(liq.keeperTopup, 5n);
assert.equal(liq.lump, 25n);
assert.throws(() => m.previewClose(100n, 90n, 20n, 1_500), /underflow/);
assert.equal(m.previewSettlement(1_000n, 1, 20n, 5n, 500n, WAD, 50n, true).lenders, 980n);
// The part above ⌊Q·P_ema/WAD⌋ + R_due vests: S2, and S1 since AIR-21.
assert.equal(m.previewSettlement(1_000n, 2, 20n, 700n, 500n, WAD, 50n, true).lump, 150n);
assert.equal(m.previewSettlement(1_000n, 1, 20n, undefined, 500n, WAD, 50n, true).lump, 430n);
// AIR-09: on a venue that cannot execute the tokens count at 0, so all but R_due vests.
assert.equal(m.previewSettlement(1_000n, 3, 20n, 700n, 500n, WAD, 50n, false).lump, 650n);

// Exit bounds (v0.4.1): no liquidation reserve; expiry keeps rent and fee senior.
assert.equal(m.maxInLiq(1_000n, 200, 10_000n, 100n, 0n), 1_020n);
assert.equal(m.maxInLiq(1_000n, 200, 100n, 100n, 1n), undefined);
const piRef = m.piRef(1_000n, 200);
assert.equal(piRef, 20n);
assert.equal(m.maxInExp(1_000n, 100, 10, 1_100n, 0n, 1_000n, piRef, 0n), 1_001n);

// Settlement box: medians of the bucket extremes widened to the EMA point.
const o = new m.OracleState(2n * WAD, 1_000n, 10n);
assert.throws(() => o.settleBox(10n, true), /invalid input/);
for (const [ts, lo, hi] of [[8n, 1n, 3n], [9n, 2n, 5n], [10n, 1n, 4n]]) {
  o.push(ts, 2n * WAD, 1_000n, 0n, 0n, lo * WAD, hi * WAD, 900n, 1_100n);
}
const box = o.settleBox(11n, true);
assert.deepEqual([box.pLo, box.pHi, box.lLo, box.lHi], [WAD, 4n * WAD, 900n, 1_100n]);
assert.equal(box.isPoint, false);
// Stale for more than RING_LEN seconds: the EMA point only if the venue cannot execute (M2-SEC-1).
assert.equal(o.settleBox(10n + BigInt(m.ringLen()) + 1n, false).isPoint, true);
assert.equal(o.settleBox(10n + BigInt(m.ringLen()) + 1n, true).isPoint, false);
assert.equal(m.pprime(true, 1_250n, 1_000n, 12_500, 500, 200), 200);
assert.equal(m.amountOk(10_000n, 10_101n, 100), false);

// Parameter checks (ECON §15 = shortwin_sim.validate_params).
const params = new m.ParamSet();
assert.equal(params.validate(), undefined);
params.set("kappa_liq_bps", undefined, 2_000n);
params.set("cr_liq_bps", 1, 13_000n);
assert.deepEqual(params.violations(), ["kappa_liq_bps", "B:tier_spec_cr"]);
assert.equal(params.get("cr_liq_bps", 1), 13_000n);
assert.throws(() => params.set("nope", undefined, 1n), /invalid input/);

// Lender previews, incl. a pool-rule refusal.
const pool = new m.PoolState();
const lender = m.LenderState.newIn(pool);
const none = new m.ShareEpochs();
const now = 1_000n;
assert.equal(m.previewDeposit(pool, lender, none, now, 1n, false), 1_000n);
assert.throws(() => m.previewDeposit(pool, lender, none, now, 0n, false), /deposit too small/);
assert.throws(() => m.previewWithdraw(pool, lender, none, now, 86_400, 1n), /insufficient shares/);
pool.totalShares = 1_000_000_000_000n; pool.tokensFree = 1_000_000_000n; pool.lenderSol = 5n;
lender.shares = 1_000_000_000_000n; lender.pending = -1n;
assert.throws(() => m.previewWithdraw(pool, lender, none, now, 86_400, 1n), /negative pending/);
lender.pending = 0n;
assert.equal(m.previewAllowance(pool, lender, none, now), 1_000_000_000n);
// The whole withdrawal: tokens, then the SOL part as claim_sol right after; the program's
// last-shares check while tokens are lent (M1-SEC-3) is a flag, not a throw.
const w = m.previewWithdrawal(pool, lender, none, now, 86_400, 1_000_000_000_000n);
assert.equal(w.tokens, 1_000_000_000n);
assert.equal(w.sol, 0n);
assert.equal(w.lastSharesBorrowed, false);
assert.equal(w.lender.shares, 0n);
assert.throws(() => m.previewWithdrawal(pool, lender, none, now, 86_400, 2_000_000_000_000n), /insufficient shares/);
// A live wipe record waits for its post-wipe sweep before the next deposit (R4A-7).
const wiped = new m.PoolState();
wiped.wipeLive = true; wiped.wipeSwept = 0; // Market.flags bits 7, 8 cleared at the full wipe
const parked = m.LenderState.newIn(wiped);
assert.throws(() => m.previewDeposit(wiped, parked, none, now, 1n, false), /post-wipe sweep pending/);
wiped.wipeSwept = 1; // sweep_trader ran; the venue-reward claim is required on pump venues only
assert.equal(m.previewDeposit(wiped, parked, none, now, 1n, false), 1_000n);
assert.throws(() => m.previewDeposit(wiped, parked, none, now, 1n, true), /post-wipe sweep pending/);
// Locks end at a day boundary after max_release + lock_grace_s; lumps vest linearly.
pool.maxRelease = 1_000_000n;
assert.equal(m.previewLockUntil(pool, now, 86_400), 13n * 86_400n);
pool.vestBal = 100n; pool.vestLast = 0n; pool.vestEnd = 100n;
assert.equal(m.previewVestRelease(pool, 50n), 50n);
assert.equal(lender.lockUntil, 0n);

// Caps and tiers.
assert.equal(m.maxQPosition(1_000_000_000_000n, 500), 24_695_076_595n);
const t = new m.TierInputs();
t.venue = 0;
assert.equal(t.tier(), 3);
assert.equal(t.liquidityLamports(), undefined);
t.venue = 9;
assert.throws(() => t.tier(), /invalid input/);
assert.throws(() => t.liquidityLamports(), /invalid input/);
assert.equal(m.tierParams(3).maxDurationS, 259_200);
// OWNER-M6D-1 (ECON #106): PumpSwap sizes and tiers on its burned LP only (λ_min 100 %),
// whatever pool_lock_min_bps says; Raydium divides D by it.
assert.equal(m.pumpswapLockMinBps(), 10_000);
assert.equal(m.conservativeX0(1, 9_000_000n, 8_000_000n, WAD, 1_000_000n, 5_000), 1_000_000n);
assert.equal(m.conservativeX0(2, 9_000_000n, 8_000_000n, WAD, 1_000_000n, 5_000), 2_000_000n);
assert.equal(m.conservativeX0(3, 9_000_000n, 8_000_000n, WAD, 1_000_000n, 5_000), 8_000_000n);
assert.throws(() => m.conservativeX0(0, 1n, 1n, WAD, 1n, 5_000), /invalid input/);
const pst = new m.TierInputs();
pst.venue = 1;
pst.rNow = 40_000_000_000_000n;
pst.ySlow = 40_000_000_000_000n;
pst.pEma = WAD;
pst.lockedDepth = 2_000_000_000_000n;
pst.lambdaMinBps = 5_000;
assert.equal(pst.liquidityLamports(), 4_000_000_000_000n);
pst.venue = 2;
assert.equal(pst.liquidityLamports(), 8_000_000_000_000n);
// FE-15 (M10): the open guards, pool selection and the mint filter.
assert.equal(m.spotNearEma(12n * WAD / 10n, WAD, 2_000), true);
assert.equal(m.spotNearEma(12n * WAD / 10n + 1n, WAD, 2_000), false);
assert.equal(m.spotNearEma(WAD, 0n, 10_000), false);
assert.equal(m.pOpenMax(), 10n ** 27n);
assert.equal(m.lambdaBps(10_000n, 22n), 9_978);
assert.equal(m.lockedDepth(10_000n, 22n, 1_000_000n, 1_000_000n), 997_800n);
assert.equal(m.lockedSol(10_000_000_000_000n, WAD), 10_000_000_000_000n);
assert.equal(m.lockedFloorOk(12_500_000_000n, WAD, 5_000, 25_000_000_000n), true);
assert.equal(m.lockedFloorOk(12_499_999_999n, WAD, 5_000, 25_000_000_000n), false);
const TOKEN = Uint8Array.from([
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237, 95, 91, 55, 145,
    58, 140, 245, 133, 126, 255, 0, 169,
]);
const mintBytes = new Uint8Array(82);
mintBytes[45] = 1;
mintBytes[46] = 1; // a freeze authority
mintBytes[44] = 6;
const verdict = m.mintCheck(new Uint8Array(32).fill(7), TOKEN, mintBytes);
assert.equal(verdict.ok, false);
assert.deepEqual(verdict.codes, ["MintFreezeAuthority"]);
assert.deepEqual([...verdict.extensions], [-1]);
assert.equal(verdict.decimals, 6);
assert.equal(verdict.freezeAuthority.length, 32);
assert.equal(verdict.mintAuthority, undefined);
assert.deepEqual(m.mintCheck(new Uint8Array(32).fill(7), new Uint8Array(32), new Uint8Array(0)).codes, [
    "MintUnsupportedProgram",
]);
assert.throws(() => m.mintCheck(new Uint8Array(31), TOKEN, mintBytes), /invalid input/);
assert.deepEqual([...m.allowedExtensions()], [18, 19, 20, 21, 22, 23]);
console.log("facade smoke test: ok");
