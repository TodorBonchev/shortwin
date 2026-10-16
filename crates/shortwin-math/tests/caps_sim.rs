//! `caps` against `tests/vectors/quotes/caps.json` (`gen_caps.py`: the v0.4.1 sim's cap
//! functions and example markets, default switches; pool_model's utilisation check; ECON
//! §6.1 / ARCH §4.6 transcriptions for the predicates the sims have no function for),
//! exact with no tolerance (ARCH §9 M0b), plus properties: each cap is the integer form
//! of ECON §2.1 against an independent big-integer reference.

mod common;

use common::Dec;
use num_bigint::BigUint;
use proptest::prelude::*;
use serde::Deserialize;
use shortwin_math::MathError;
use shortwin_math::caps;
use shortwin_math::consts::PUMPSWAP_LOCK_MIN_BPS;
use shortwin_math::quote;

#[derive(Deserialize)]
struct File {
    generator: String,
    impact: Vec<Impact>,
    fee: Vec<Fee>,
    predicates: Vec<Preds>,
    conservative_x0: Vec<ConsX>,
    markets: Vec<Market>,
}

#[derive(Deserialize)]
struct Impact {
    x: Dec<u64>,
    impact_pos_bps: Dec<u32>,
    impact_mkt_bps: Dec<u32>,
    max_q_position: Dec<u64>,
    max_total_q: Dec<u64>,
}

#[derive(Deserialize)]
struct Fee {
    x: Dec<u64>,
    phi_ppm: Dec<u32>,
    fee_size_cap_x100: Dec<u32>,
    fee_oi_cap_x100: Dec<u32>,
    impact_mkt_bps: Dec<u32>,
    fee_size_cap: Option<Dec<u64>>,
    fee_oi_cap: Option<Dec<u64>>,
    oi_cap: Dec<u64>,
}

#[derive(Deserialize)]
struct Preds {
    t_free: Dec<u64>,
    borrowed: Dec<u64>,
    q: Dec<u64>,
    util_cap_bps: Dec<u32>,
    util_ok: bool,
    sum_q: Dec<u64>,
    rt: Dec<u64>,
    gamma_bps: Dec<u32>,
    curve_gamma_ok: bool,
    curve_gamma_max_q: Dec<u64>,
    q_open: Dec<u64>,
    oi_cap: Dec<u64>,
    wallet_cap_bps: Dec<u32>,
    wallet_ok: bool,
    n_open: Dec<u64>,
    total_notional_open: Dec<u64>,
    min_notional: Dec<u64>,
    market_max_notional: Dec<u64>,
    notional_ok: bool,
}

#[derive(Deserialize)]
struct ConsX {
    key: String,
    x_now: Dec<u64>,
    l_slow: Dec<u128>,
    p_ema: Dec<u128>,
    raydium_lock: Option<(Dec<u128>, Dec<u32>)>,
    expected: Dec<u64>,
}

#[derive(Deserialize)]
struct Market {
    key: String,
    kind: String,
    x_now: Dec<u64>,
    l_slow: Dec<u128>,
    p_ema: Dec<u128>,
    raydium_lock: Option<(Dec<u128>, Dec<u32>)>,
    x0: Dec<u64>,
    phi_ppm: Dec<u32>,
    impact_pos_bps: Dec<u32>,
    impact_mkt_bps: Dec<u32>,
    max_q_position: Dec<u64>,
    oi_cap_at: Dec<u64>,
    oi_cap: Dec<u64>,
    fee_size_cap: Dec<u64>,
    fee_size_cap_x100: Dec<u32>,
    fee_oi_cap_x100: Dec<u32>,
    gamma_bps: Dec<u32>,
    max_open_q: Dec<u64>,
    rt: Option<Dec<u64>>,
    /// The curve's virtual SOL reserve `vQ` (curve markets only; `None` elsewhere).
    vq: Option<Dec<u64>>,
}

fn load() -> File {
    let f: File = common::load_vectors("quotes", "caps.json");
    assert_eq!(f.generator, "gen_caps.py");
    f
}

fn lock(l: &Option<(Dec<u128>, Dec<u32>)>) -> Option<(u128, u32)> {
    l.as_ref().map(|(d, l)| (d.0, l.0))
}

#[test]
fn impact_caps_match_sim() {
    let f = load();
    let mut wide = 0;
    for (i, c) in f.impact.iter().enumerate() {
        let p = caps::max_q_position(c.x.0, c.impact_pos_bps.0).unwrap();
        assert_eq!(p, c.max_q_position.0, "impact case {i}: per position");
        let t = caps::max_total_q(c.x.0, c.impact_mkt_bps.0).unwrap();
        assert_eq!(t, c.max_total_q.0, "impact case {i}: per market");
        // The position cap's radicand X²·(10⁴ + ι)/10⁴ exceeds u128 here.
        let x = BigUint::from(c.x.0);
        if &x * &x * (10_000u64 + u64::from(c.impact_pos_bps.0)) / 10_000u32
            > BigUint::from(u128::MAX)
        {
            wide += 1;
        }
    }
    assert!(wide >= 20, "{wide}");
}

#[test]
fn fee_caps_match_sim() {
    let f = load();
    for (i, c) in f.fee.iter().enumerate() {
        let (x, phi) = (c.x.0, c.phi_ppm.0);
        let o = |v: &Option<Dec<u64>>| v.as_ref().map(|d| d.0);
        assert_eq!(
            caps::fee_size_cap(x, phi, c.fee_size_cap_x100.0),
            Ok(o(&c.fee_size_cap)),
            "fee case {i}"
        );
        assert_eq!(
            caps::fee_oi_cap(x, phi, c.fee_oi_cap_x100.0),
            Ok(o(&c.fee_oi_cap)),
            "fee case {i}"
        );
        // Market.oi_cap_q.
        assert_eq!(
            caps::oi_cap(x, c.impact_mkt_bps.0, phi, c.fee_oi_cap_x100.0),
            Ok(c.oi_cap.0),
            "fee case {i}: oi cap"
        );
    }
}

#[test]
fn predicates_match() {
    let f = load();
    for (i, c) in f.predicates.iter().enumerate() {
        let q = c.q.0;
        assert_eq!(
            caps::util_ok(c.t_free.0, c.borrowed.0, q, c.util_cap_bps.0),
            Ok(c.util_ok),
            "util case {i}"
        );
        assert_eq!(
            caps::curve_gamma_ok(c.sum_q.0, q, c.rt.0, c.gamma_bps.0),
            Ok(c.curve_gamma_ok),
            "gamma case {i}"
        );
        assert_eq!(
            caps::curve_gamma_max_q(c.sum_q.0, c.rt.0, c.gamma_bps.0),
            Ok(c.curve_gamma_max_q.0),
            "gamma max case {i}"
        );
        assert_eq!(
            caps::wallet_ok(c.q_open.0, q, c.oi_cap.0, c.wallet_cap_bps.0),
            Ok(c.wallet_ok),
            "wallet case {i}"
        );
        assert_eq!(
            caps::notional_ok(
                c.n_open.0,
                c.total_notional_open.0,
                c.min_notional.0,
                c.market_max_notional.0
            ),
            c.notional_ok,
            "notional case {i}"
        );
    }
}

/// Whether the sim's example market `key` is a canonical PumpSwap pool (`A`, `B`, `C` and the
/// graduated `D@…` markets, kind `amm`), not Raydium (`RA`, `RB`, `RC`).
fn pumpswap_key(key: &str) -> bool {
    !key.starts_with('R')
}

/// `Market.conservative_x`: `min(X_now, ⌊L_slow·10⁹/isqrt⁺(P_ema)⌋)`, Raydium and (M6R-1) a
/// canonical PumpSwap pool also `⌊D·10⁹·10⁴/(λ_min·isqrt⁺(P_ema))⌋` (ECON §2.1) — on PumpSwap at
/// `λ_min` = `PUMPSWAP_LOCK_MIN_BPS` since OWNER-M6D-1 (ECON #106; `Market.lock_min_bps`),
/// the burned LP's own token reserve at `P_ema`, which binds on its rows too.
#[test]
fn conservative_x0_matches_sim() {
    let f = load();
    let (mut slow, mut locked, mut pumpswap) = (0, 0, 0);
    for (i, c) in f.conservative_x0.iter().enumerate() {
        let got = caps::conservative_x0(c.x_now.0, c.l_slow.0, c.p_ema.0, lock(&c.raydium_lock));
        assert_eq!(got, Ok(c.expected.0), "conservative_x0 case {i}");
        let x_slow = quote::reprice_token(c.l_slow.0, c.p_ema.0).unwrap_or(u128::MAX);
        if u128::from(c.expected.0) == x_slow && x_slow < u128::from(c.x_now.0) {
            slow += 1;
        }
        if let Some((d, l)) = &c.raydium_lock {
            if pumpswap_key(&c.key) {
                assert_eq!(
                    l.0, PUMPSWAP_LOCK_MIN_BPS,
                    "case {i}: PumpSwap's λ_min (#106)"
                );
                // λ_min = 100 %: the D term is the burned depth's own reprojection at P_ema.
                let x_d = quote::reprice_token(d.0, c.p_ema.0).unwrap_or(u128::MAX);
                let want = x_d.min(x_slow).min(c.x_now.0.into());
                assert_eq!(u128::from(c.expected.0), want, "case {i}");
            }
            if u128::from(c.expected.0) < x_slow.min(c.x_now.0.into()) {
                locked += 1;
                pumpswap += usize::from(pumpswap_key(&c.key));
            }
        }
    }
    assert!(slow >= 30 && locked >= 30, "{slow} {locked}");
    assert!(pumpswap >= 10, "{pumpswap}");
}

/// The curve's cap reserves `(X₀, rT_guard)` (ECON changelog #103, M5-SEC-3; ARCH §4.6; the sim's
/// `Market.curve_cap_reserves`, the program's `engine::caps::curve_reserves`): the live `(vT, rT)`
/// while `P_ema ≤ P_spot = ⌊vQ·WAD/vT⌋`, else with `x_ema = ⌊isqrt(vT·vQ)·10⁹/isqrt⁺(P_ema)⌋`
/// `(min(vT, x_ema), min(rT, max(0, x_ema − (vT − rT))))`.
fn curve_cap_reserves(vt: u64, vq: u64, rt: u64, p_ema: u128) -> (u64, u64) {
    let p_spot = u128::from(vq) * 1_000_000_000_000_000_000 / u128::from(vt);
    if p_ema <= p_spot {
        return (vt, rt);
    }
    let l = quote::liquidity(vt.into(), vq.into()).unwrap();
    let x_ema = quote::reprice_token(l, p_ema).unwrap();
    let x0 = u128::from(vt).min(x_ema);
    let guard = u128::from(rt).min(x_ema.saturating_sub(u128::from(vt - rt)));
    (x0.try_into().unwrap(), guard.try_into().unwrap())
}

/// Every example market of the sim, fresh and after an EMA-lagging path:
/// `Market.max_open_q = min(max_q_position(X₀), oi_cap(X₀ + that), fee_size_cap(X₀),
/// curve gamma)` on the conservative `X₀` (on the curve the cap reserves at the EMA price, #103,
/// with the Tier D guard on `rT_guard`).
#[test]
fn example_markets_match_sim() {
    let f = load();
    assert!(f.markets.len() >= 30);
    let mut ema_bound = 0;
    for m in &f.markets {
        let k = &m.key;
        let (x0, rt_guard) = if m.kind == "curve" {
            let (rt, vq) = (m.rt.as_ref().unwrap().0, m.vq.as_ref().unwrap().0);
            let (x0, guard) = curve_cap_reserves(m.x_now.0, vq, rt, m.p_ema.0);
            if x0 < m.x_now.0 {
                ema_bound += 1;
            }
            (x0, Some(guard))
        } else {
            // The sizing λ_min (`Market.lock_min_bps`): Raydium's `pool_lock_min_bps`, a
            // canonical PumpSwap pool's `PUMPSWAP_LOCK_MIN_BPS` since #106 (OWNER-M6D-1).
            let (_, l) = m.raydium_lock.as_ref().expect("a locked depth");
            match m.kind.as_str() {
                "amm" => assert_eq!(l.0, PUMPSWAP_LOCK_MIN_BPS, "{k}"),
                _ => assert_eq!(l.0, 5_000, "{k}: pool_lock_min_bps"),
            }
            let x0 = caps::conservative_x0(m.x_now.0, m.l_slow.0, m.p_ema.0, lock(&m.raydium_lock))
                .unwrap();
            (x0, None)
        };
        assert_eq!(x0, m.x0.0, "{k}");
        let pos = caps::max_q_position(x0, m.impact_pos_bps.0).unwrap();
        assert_eq!(pos, m.max_q_position.0, "{k}");
        assert_eq!(x0 + pos, m.oi_cap_at.0, "{k}");
        let oi = caps::oi_cap(
            x0 + pos,
            m.impact_mkt_bps.0,
            m.phi_ppm.0,
            m.fee_oi_cap_x100.0,
        )
        .unwrap();
        assert_eq!(oi, m.oi_cap.0, "{k}");
        let size = caps::fee_size_cap(x0, m.phi_ppm.0, m.fee_size_cap_x100.0)
            .unwrap()
            .unwrap();
        assert_eq!(size, m.fee_size_cap.0, "{k}");
        let mut want = pos.min(size).min(oi);
        if let Some(rt) = rt_guard {
            want = want.min(caps::curve_gamma_max_q(0, rt, m.gamma_bps.0).unwrap());
        }
        assert_eq!(want, m.max_open_q.0, "{k}");
    }
    // At least one lagging curve market sits above its EMA price, so the #103 rule binds.
    assert!(ema_bound >= 1, "{ema_bound}");
}

/// Review F-A4: a gamma cap above `u64::MAX` (a non-standard curve, `rT` = 3·10¹⁸)
/// saturates like every other cap instead of failing the open with `Overflow`.
#[test]
fn gamma_cap_saturates() {
    assert_eq!(
        caps::curve_gamma_max_q(0, 3_000_000_000_000_000_000, 9_000),
        Ok(u64::MAX)
    );
    assert_eq!(caps::curve_gamma_max_q(0, u64::MAX, 9_999), Ok(u64::MAX));
    assert_eq!(caps::curve_gamma_max_q(0, u64::MAX, 5_000), Ok(u64::MAX));
    assert_eq!(
        caps::curve_gamma_max_q(0, u64::MAX, 4_000),
        Ok(u64::MAX / 3 * 2)
    );
}

#[test]
fn conservative_x0_errors() {
    assert_eq!(
        caps::conservative_x0(1, 1, 0, None),
        Err(MathError::DivByZero)
    );
    assert_eq!(
        caps::conservative_x0(1, 1, 1, Some((1, 0))),
        Err(MathError::DivByZero)
    );
}

fn b<T: Into<BigUint>>(x: T) -> BigUint {
    x.into()
}

fn isqrt_ceil(p: u128) -> BigUint {
    let r = b(p).sqrt();
    if &r * &r == b(p) { r } else { r + 1u8 }
}

fn wide_x() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => any::<u64>(),
        2 => (u64::MAX - (1u64 << 60))..=u64::MAX,
        2 => 0u64..1_000_000,
        1 => Just(u64::MAX),
    ]
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// The position cap is the largest `q` with `(x + q)²·10⁴ ≤ x²·(10⁴ + ι)` (the sims'
    /// `isqrt(⌊x²·(10⁴ + ι)/10⁴⌋) − x`, saturating), including where the radicand exceeds
    /// `u128`; the market cap is the sims' `x − isqrt(⌊x²·10⁴/(10⁴ + ι)⌋)`, i.e. the
    /// smallest `t` with `(10⁴ + ι)·(x − t)² ≤ x²·10⁴`: the real-valued bound rounded up
    /// (at most one raw unit above it; ARCH §9 M0b).
    #[test]
    fn impact_caps_are_the_sim_forms(x in wide_x(), ip in prop_oneof![0u32..=1_000, any::<u32>()],
                                     im in prop_oneof![0u32..=5_000, any::<u32>()]) {
        let q = caps::max_q_position(x, ip).unwrap();
        let root = (b(x) * b(x) * (10_000u64 + u64::from(ip)) / 10_000u32).sqrt();
        let want = root - b(x);
        prop_assert_eq!(b(q), want.min(b(u64::MAX)));
        let t = caps::max_total_q(x, im).unwrap();
        prop_assert!(t <= x);
        let r = (b(x) * b(x) * 10_000u32 / (10_000u64 + u64::from(im))).sqrt();
        prop_assert_eq!(b(t), b(x) - &r);
        let lhs = b(x) * b(x) * 10_000u32;
        prop_assert!((b(x) - b(t)) * (b(x) - b(t)) * (10_000u64 + u64::from(im)) <= lhs);
        if t > 0 {
            prop_assert!((b(x) - b(t) + 1u8) * (b(x) - b(t) + 1u8) * (10_000u64 + u64::from(im)) > lhs);
        }
    }

    /// Fee-scaled caps are `⌊x100·φ_ppm·X/(100·10⁶)⌋` (saturating), the OI cap is the
    /// tighter of both, and the wallet cap never admits more than its share.
    #[test]
    fn fee_and_wallet_caps(x in any::<u64>(), phi in prop_oneof![0u32..=200_000, any::<u32>()],
                           k in prop_oneof![1u32..=1_000, any::<u32>()], im in 0u32..=5_000,
                           qo in 0u64..10u64.pow(12), q in 0u64..10u64.pow(12), w in 500u32..=10_000) {
        let cap = caps::fee_oi_cap(x, phi, k).unwrap().unwrap();
        let want = b(x) * b(phi) * k / 100_000_000u32;
        prop_assert_eq!(b(cap), want.min(b(u64::MAX)));
        prop_assert_eq!(caps::fee_size_cap(x, phi, k), Ok(Some(cap)));
        prop_assert_eq!(caps::fee_oi_cap(x, phi, 0), Ok(None));
        let oi = caps::oi_cap(x, im, phi, k).unwrap();
        prop_assert_eq!(oi, cap.min(caps::max_total_q(x, im).unwrap()));
        let ok = caps::wallet_ok(qo, q, oi, w).unwrap();
        prop_assert_eq!(ok, (b(qo) + b(q)) * 10_000u32 <= b(oi) * w);
    }

    /// `min(X_now, ⌊L_slow·10⁹/isqrt⁺(P)⌋, ⌊D·10¹³/(λ_min·isqrt⁺(P))⌋)`; bounds beyond
    /// `u128` do not bind.
    #[test]
    fn conservative_x0_is_the_econ_form(x_now: u64, l_slow: u128, p in 1u128..=u128::MAX,
                                        lock in proptest::option::of((any::<u128>(), 1u32..=10_000))) {
        let sp = isqrt_ceil(p);
        let cap = |v: BigUint| v.min(b(u128::MAX));
        let mut want = b(x_now).min(cap(b(l_slow) * 1_000_000_000u32 / &sp));
        if let Some((d, lam)) = lock {
            want = want.min(cap(b(d) * 10_000_000_000_000u64 / (b(lam) * &sp)));
        }
        prop_assert_eq!(b(caps::conservative_x0(x_now, l_slow, p, lock).unwrap()), want);
    }

    /// Utilisation after the open never exceeds the cap, and `curve_gamma_max_q` is the
    /// largest size the gamma guard admits.
    #[test]
    fn utilization_and_gamma(t in 0u64..10u64.pow(15), bq in 0u64..10u64.pow(15), q in 0u64..10u64.pow(15),
                             cap in 0u32..=9_500, sq in 0u64..10u64.pow(12), rt in 0u64..10u64.pow(15),
                             g in 1_000u32..=9_000) {
        if caps::util_ok(t, bq, q, cap).unwrap() && t + bq > 0 {
            let u = shortwin_math::rent::utilization_bps(t - q.min(t), bq + q).unwrap();
            prop_assert!(u <= cap);
            prop_assert!(q <= t);
        }
        let m = caps::curve_gamma_max_q(sq, rt, g).unwrap();
        if caps::curve_gamma_ok(sq, 0, rt, g).unwrap() {
            prop_assert!(caps::curve_gamma_ok(sq, m, rt, g).unwrap());
        }
        prop_assert!(!caps::curve_gamma_ok(sq, m + 1, rt, g).unwrap());
    }

    /// "Switch only if all fit" (`caps::carried_fit`): with the O(1) bound
    /// `carried_q_bound(max_q_hwm, ΣQ)` of a high-water mark at or above every open `q`, a fit
    /// is sound — every open `q` passes both per-position checks of an open on `X₀`
    /// (`max_q_position`, `fee_size_cap`, each against its big-integer form) and `ΣQ` the OI cap
    /// at `X₁ = X₀` — and with the mark at the largest open `q` it is exact. A larger `X₀` never
    /// turns a fit into a refusal.
    #[test]
    fn carried_fit_is_sound_and_exact_at_the_mark(
        qs in proptest::collection::vec(1u64..10u64.pow(13), 1..8),
        over in prop_oneof![Just(0u64), 1u64..10u64.pow(13)],
        x0 in prop_oneof![0u64..10u64.pow(15), any::<u64>()],
        grow in 0u64..10u64.pow(14),
        ip in 0u32..=1_000, im in 0u32..=5_000, phi in 0u32..=200_000,
        fs in prop_oneof![Just(0u32), 100u32..=500], fo in prop_oneof![Just(0u32), 300u32..=1_000],
    ) {
        let sum: u64 = qs.iter().sum();
        let max = *qs.iter().max().unwrap();
        let c = caps::CarriedCaps { x0, impact_pos_bps: ip, impact_mkt_bps: im, phi_ppm: phi,
                                    fee_size_cap_x100: fs, fee_oi_cap_x100: fo };
        let hwm = max.saturating_add(over);
        let bound = caps::carried_q_bound(hwm, sum);
        prop_assert!(bound >= max && bound <= sum);
        let fit = caps::carried_fit(bound, sum, &c).unwrap();
        let root = (b(x0) * b(x0) * (10_000u64 + u64::from(ip)) / 10_000u32).sqrt();
        let pos_ref = (root - b(x0)).min(b(u64::MAX));
        let fee_ref = (fs > 0).then(|| (b(x0) * b(phi) * fs / 100_000_000u32).min(b(u64::MAX)));
        let oi = caps::oi_cap(x0, im, phi, fo).unwrap();
        let q_ok = |q: u64| b(q) <= pos_ref && fee_ref.as_ref().is_none_or(|f| b(q) <= *f);
        if fit.fits() {
            prop_assert!(qs.iter().all(|&q| q_ok(q)));
            prop_assert!(sum <= oi);
        }
        let exact = caps::carried_fit(caps::carried_q_bound(max, sum), sum, &c).unwrap();
        let want = if !q_ok(max) {
            caps::CarriedFit::Position
        } else if sum > oi {
            caps::CarriedFit::Market
        } else {
            caps::CarriedFit::Fits
        };
        prop_assert_eq!(exact, want);
        if exact.fits() {
            let bigger = caps::CarriedCaps { x0: x0.saturating_add(grow), ..c };
            prop_assert!(caps::carried_fit(max, sum, &bigger).unwrap().fits());
        }
    }

    /// FIT-2 / FITS-1 (`caps::open_fee_mark`, `caps::carried_phi`): folded over the opens since the
    /// market emptied, the open-fee mark is the lowest fee an open paid (0 only if an open paid 0);
    /// the carried fit's fee is at most the candidate's, at most every open's and at most
    /// `CARRIED_FEE_CAP_PPM`, so a fit at it is a fit at the candidate's own fee, at the fee each
    /// open position posted its margin at and at the 25-bps ceiling (the fee-scaled caps grow with
    /// φ; the impact caps do not read it).
    #[test]
    fn the_carried_fee_is_never_above_an_open_fee_nor_the_candidates(
        fees in proptest::collection::vec(1u32..=100_000, 1..8),
        cand in 0u32..=100_000,
        qs in proptest::collection::vec(1u64..10u64.pow(13), 1..8),
        x0 in prop_oneof![0u64..10u64.pow(15), any::<u64>()],
        ip in 0u32..=1_000, im in 0u32..=5_000,
        fs in prop_oneof![Just(0u32), 100u32..=500], fo in prop_oneof![Just(0u32), 300u32..=1_000],
    ) {
        let mark = fees.iter().fold(0u32, |m, &f| caps::open_fee_mark(m, f));
        prop_assert_eq!(mark, *fees.iter().min().unwrap());
        let ceiling = shortwin_math::consts::CARRIED_FEE_CAP_PPM;
        let phi = caps::carried_phi(mark, cand);
        prop_assert_eq!(phi, mark.min(cand).min(ceiling));
        prop_assert_eq!(caps::carried_phi(0, cand), cand.min(ceiling));
        let sum: u64 = qs.iter().sum();
        let max = *qs.iter().max().unwrap();
        let at = |phi_ppm: u32| caps::CarriedCaps { x0, impact_pos_bps: ip, impact_mkt_bps: im,
                                                    phi_ppm, fee_size_cap_x100: fs, fee_oi_cap_x100: fo };
        if caps::carried_fit(max, sum, &at(phi)).unwrap().fits() {
            prop_assert!(caps::carried_fit(max, sum, &at(cand)).unwrap().fits());
            prop_assert!(caps::carried_fit(max, sum, &at(ceiling)).unwrap().fits());
            for &f in &fees {
                prop_assert!(caps::carried_fit(max, sum, &at(f)).unwrap().fits());
            }
        }
    }
}
