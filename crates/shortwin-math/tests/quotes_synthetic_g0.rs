//! Gate G0's measurements of pump's synthetic migration, reproduced by the quote track's P1
//! functions (pump-final §3.4, §5; T-MATH-2/3 against the ELF). The figures are what
//! `tests/litesvm/tests/pump_math_it.rs` (T-PUMP-MATH-V3 (b), (b′), (d), (e)) measured on pump's
//! 2026-10-08 build at G0 and again on the 2026-10-13 build mainnet runs since slot 456,494,698
//! (G0b; `tests/fixtures/README.md`): a third party's `buy_v3(rT + δ)` on the dumped `near166`
//! curve (group `pump_state_2026_10_13`), with and without a token donation to its
//! `curve_base_ata`, the `migrate_v2` that followed, and the tier tripwire's seven-tier
//! `FeeConfig`. Each figure is pump's own (the lamports the user paid, the pool the program
//! created); `curve_synthetic` (`B̂_v3`) must cover each charge within §5.2's bound and equal
//! the value G0 printed for it, and `migration_boost` must give the boost pump booked.

use shortwin_math::fees::{self, CurveFeeSchedule, FeeBps, FeeTier};
use shortwin_math::quote::{self, CurveState, MigrationParams};

/// `near166` as dumped (`pump_near166_curve.bin`, 166 bytes): `(vT, vQ, rT, rQ)`.
const NEAR166: CurveState = CurveState {
    vt: 333_021_296_825_365,
    vq: 96_660_487_854,
    rt: 53_121_296_825_365,
    rq: 66_660_487_854,
    complete: false,
};
/// Its `curve_base_ata` (`pump_near166_curve_base_ata.bin`): `rT` + the standard 206.9e12.
const NEAR166_ATA: u128 = 260_021_296_825_365;
/// Its mint's supply (`pump_near166_mint.bin`): burned below 10¹⁵.
const NEAR166_SUPPLY: u64 = 999_818_465_159_445;
/// `Global.pool_migration_fee`.
const FEE_M: u64 = 15_000_001;
/// pump's curve `FeeConfig` today: one flat tier.
const CURVE: FeeBps = FeeBps::new(0, 95, 30);

fn fee_up(n: u128, bps: u64) -> u128 {
    (n * u128::from(bps)).div_ceil(10_000)
}

fn part(n: u128, f: &FeeBps) -> u128 {
    n + fee_up(n, f.protocol) + fee_up(n, f.creator)
}

/// pump's charge for `buy_v3(rT + δ)` with the curve's `X_s` (pump-final §5.2 as G0 measured
/// it): `n₁ = ⌈rT·vQ/(vT − rT)⌉`, `⌊⌋ + 1` on an exact division; `n₂ = ⌈Y_s·δ/(X_s − δ)⌉` on
/// `Y_s = rQ + n₁ − fee`; the fees `⌈⌉` per component and part, the curve part at `f1`, the
/// pool part at `f2`.
fn pump_charge(s: &CurveState, x_s: u128, delta: u64, f1: &FeeBps, f2: &FeeBps) -> u128 {
    let num = s.rt * s.vq;
    let d = s.vt - s.rt;
    let n1 = if num.is_multiple_of(d) {
        num / d + 1
    } else {
        num.div_ceil(d)
    };
    let y_s = s.rq + n1 - u128::from(FEE_M);
    let q2 = u128::from(delta);
    let n2 = (y_s * q2).div_ceil(x_s - q2);
    part(n1, f1) + part(n2, f2)
}

/// §5.2's general bound on `B̂_v3 − charge` (two non-zero components).
fn bound(delta: u64, x_s: u128, f: &FeeBps) -> u128 {
    let q2 = u128::from(delta);
    let d2 = 1 + q2.div_ceil(x_s - q2);
    (1 + fee_up(1, f.protocol) + fee_up(1, f.creator))
        + (d2 + fee_up(d2, f.protocol) + fee_up(d2, f.creator))
}

fn b_v3(s: &CurveState, x_s: u128, delta: u64, f: &FeeBps) -> u128 {
    let m = MigrationParams {
        migration_fee: FEE_M,
        pool_tokens: x_s,
        post_complete_quote_in: 0,
    };
    let q = u64::try_from(s.rt).unwrap() + delta;
    u128::from(quote::curve_synthetic(q, s, &CURVE, f, &m).unwrap().cost)
}

/// T-PUMP-MATH-V3 (b) and (b′): G0's synthetic buys on `near166` — `(D, δ, charge, B̂_v3)` with
/// `D` the donation to `curve_base_ata` before the buy. `δ = X_s/2` is an exact division of the
/// pool part (pump charged `⌈⌉` there).
#[test]
fn near166_synthetic_buys_reproduce_g0() {
    let cases: [(u128, u64, u128, u128); 9] = [
        (0, 1, 18_574_185_775, 18_574_185_777),
        (0, 1_000_000_000, 18_574_601_690, 18_574_601_692),
        (0, 1_000_000_000_000, 18_992_120_413, 18_992_120_415),
        (0, 10_000_000_000_000, 22_944_563_744, 22_944_563_746),
        (0, 50_000_000_000_000, 45_996_984_442, 45_996_984_445),
        (0, 100_000_000_000_000, 99_072_541_454, 99_072_541_457),
        (0, 103_450_000_000_000, 104_626_927_996, 104_626_927_999),
        (
            1_000_000_000_000,
            10_000_000_000_000,
            22_922_479_973,
            22_922_479_975,
        ),
        (
            206_900_000_000_000,
            250_000_000_000_000,
            149_912_314_929,
            149_912_314_933,
        ),
    ];
    for (d, delta, charge, b_hat) in cases {
        let x_s = NEAR166_ATA + d - NEAR166.rt;
        assert_eq!(
            pump_charge(&NEAR166, x_s, delta, &CURVE, &CURVE),
            charge,
            "D {d} δ {delta}: the charge model"
        );
        let b = b_v3(&NEAR166, x_s, delta, &CURVE);
        assert_eq!(b, b_hat, "D {d} δ {delta}: B̂_v3 as G0 printed it");
        assert!(
            b >= charge && b - charge <= bound(delta, x_s, &CURVE),
            "D {d} δ {delta}"
        );
        if 2 * u128::from(delta) <= x_s {
            assert!(b - charge <= 7, "D {d} δ {delta}");
        }
    }
    // The synthetic fee on the live supply is the flat tier too.
    let tiers = [FeeTier {
        threshold: 0,
        fees: CURVE,
    }];
    let s = CurveFeeSchedule {
        tiers: &tiers,
        creator_fee_configurable: true,
        curve_creator_fee_bps: 0,
        creator_is_default: false,
        cap_bps: None,
    };
    assert_eq!(
        fees::pump_curve_at_supply(NEAR166.vt, NEAR166.vq, NEAR166_SUPPLY, &s),
        Ok(CURVE)
    );
}

/// T-PUMP-MATH-V3 (b): G0's exact division of the curve part — `rT` a multiple of
/// `279.9e12/2¹¹` and `vQ` a multiple of 2¹¹ on `near166`'s curve — where pump charged
/// `⌊⌋ + 1` (`n₁` 18,317,767,509, `Y_s` 84,990,365,076): charge 22,917,117,698, `B̂_v3` +1.
#[test]
fn near166_exact_curve_part_reproduces_g0() {
    let ot = NEAR166.vt - NEAR166.rt;
    assert_eq!(ot % 2_048, 0);
    let unit = ot / 2_048;
    let rt = (NEAR166.rt / unit) * unit;
    let k = NEAR166.vt * NEAR166.vq;
    let vq = k.div_ceil(rt + ot).div_ceil(2_048) * 2_048;
    let s = CurveState {
        vt: rt + ot,
        vq,
        rt,
        rq: vq - (NEAR166.vq - NEAR166.rq),
        complete: false,
    };
    assert_eq!((s.rt * s.vq) % (s.vt - s.rt), 0, "an exact curve part");
    assert_eq!(
        s.rq + (s.rt * s.vq) / (s.vt - s.rt) + 1 - u128::from(FEE_M),
        84_990_365_076
    );
    let x_s = 206_900_000_000_000;
    let delta = 10_000_000_000_000;
    let charge = pump_charge(&s, x_s, delta, &CURVE, &CURVE);
    assert_eq!(charge, 22_917_117_698);
    let b = b_v3(&s, x_s, delta, &CURVE);
    assert_eq!(b, 22_917_117_699);
}

/// T-PUMP-MATH-V3 (e): the tier tripwire. Under G0's seven-tier curve `FeeConfig`, pump charged
/// both parts at the tier of the curve's pre-trade market cap on the mint's live supply
/// (290,200,481,257: 80 / 25 bps), 45,906,126,201 lamports for `rT + 50e12`. P1's synthetic fee
/// (`fees::pump_curve_at_supply`) resolves that tier and `B̂_v3` covers the charge by 4; the
/// fixed 10¹⁵ basis (`fees::pump_curve`, 290,253,172,320: 70 / 22 bps) would have quoted
/// 45,847,068,348, 59,057,853 below it (G0's finding, P1 correction (a)). Under the same tiers
/// `B̂_v3` steps up at `rT` (known-issues KI-77 (c)): 18,513,647,686 at `rT`, 18,537,496,028 at
/// `rT + 1`.
#[test]
fn tier_tripwire_reproduces_g0() {
    let raw: [(u128, u64, u64); 7] = [
        (0, 90, 28),
        (290_174_135_726, 80, 25),
        (290_226_826_789, 70, 22),
        (350_479_254_060, 60, 19),
        (410_755_463_916, 50, 16),
        (562_490_950_388, 40, 13),
        (865_861_667_099, 30, 10),
    ];
    let tiers = raw.map(|(threshold, p, c)| FeeTier {
        threshold,
        fees: FeeBps::new(0, p, c),
    });
    let s = CurveFeeSchedule {
        tiers: &tiers,
        creator_fee_configurable: true,
        curve_creator_fee_bps: 0,
        creator_is_default: false,
        cap_bps: None,
    };
    let phi_s = fees::pump_curve_at_supply(NEAR166.vt, NEAR166.vq, NEAR166_SUPPLY, &s).unwrap();
    let phi = fees::pump_curve(NEAR166.vt, NEAR166.vq, &s).unwrap();
    assert_eq!(
        (phi_s, phi),
        (FeeBps::new(0, 80, 25), FeeBps::new(0, 70, 22))
    );
    let x_s = NEAR166_ATA - NEAR166.rt;
    let delta = 50_000_000_000_000;
    let charge = pump_charge(&NEAR166, x_s, delta, &phi_s, &phi_s);
    assert_eq!(charge, 45_906_126_201);
    let b = b_v3(&NEAR166, x_s, delta, &phi_s);
    assert_eq!(b, 45_906_126_205);
    assert!(b - charge <= bound(delta, x_s, &phi_s));
    assert_eq!(b_v3(&NEAR166, x_s, delta, &phi), 45_847_068_348);
    assert_eq!(charge - 45_847_068_348, 59_057_853);
    // Known-issues KI-77 (c): under these tiers `B̂_v3` steps up at `rT`. `q = rT` is the curve
    // path's quote at the fixed 10¹⁵ basis's `φ` (70 / 22 bps), `q = rT + 1` the synthetic buy at
    // `φ_s` (80 / 25 bps) on both parts.
    let m = MigrationParams {
        migration_fee: FEE_M,
        pool_tokens: x_s,
        post_complete_quote_in: 0,
    };
    let rt = u64::try_from(NEAR166.rt).unwrap();
    let at = |q| {
        quote::curve_synthetic(q, &NEAR166, &phi, &phi_s, &m)
            .unwrap()
            .cost
    };
    assert_eq!((at(rt), at(rt + 1)), (18_513_647_686, 18_537_496_028));
}

/// T-PUMP-MATH-V3 (d): the pools G0's `migrate_v2` calls created, `(X₁, Y₁)` = (the live
/// `curve_base_ata`, `rQ + pcQ − fee`), and the boost pump booked into `virtual_quote_reserves`
/// (vault + VQR = Y₁): `⌊X₁·Y₁/10¹⁵⌋` with and without donations (correction (b)); pump-final
/// §3.4's `⌊(Y₁ − pcQ)·2069/10⁴⌋` holds only without one (17,584,506,040 for every case here).
#[test]
fn migrate_v2_boost_reproduces_g0() {
    let cases: [(&str, u128, u128, u128); 6] = [
        (
            "δ 10e12",
            196_900_000_000_000,
            89_306_785_376,
            17_584_506_040,
        ),
        (
            "after (c) 5e9",
            195_538_453_116_689,
            89_928_634_293,
            17_584_506_040,
        ),
        (
            "D 1e12 before",
            197_900_000_000_000,
            89_284_974_245,
            17_669_496_403,
        ),
        (
            "D 206.9e12 before",
            163_800_000_000_000,
            214_707_033_460,
            35_169_012_080,
        ),
        (
            "D 3e12 after",
            199_900_000_000_000,
            89_306_785_376,
            17_852_426_396,
        ),
        (
            "D 2e12, no pool part",
            208_900_000_000_000,
            84_990_362_689,
            17_754_486_765,
        ),
    ];
    for (what, x1, y1, vqr) in cases {
        assert_eq!(quote::migration_boost(x1, y1), Some(vqr), "{what}");
    }
    // Without a donation the old rule: ⌊(Y₁ − pcQ)·2069/10⁴⌋ on Y₁ − pcQ = 84,990,362,689.
    assert_eq!(84_990_362_689u128 * 2_069 / 10_000, 17_584_506_040);
}
