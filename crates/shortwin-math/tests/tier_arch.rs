//! `tier` against `tests/vectors/quotes/caps.json` (`gen_caps.py`'s `tiers`: an ARCH §4.7
//! / ECON §6.2–§6.3 v0.4.1 transcription — the sims have no tier function — with
//! `R_slow = Y(reprice(L_slow, P_ema)) − VQR` from the v0.4.1 sim's own `reprice`, the
//! Raydium term `⌊⌊D·isqrt(P_ema)/10⁹⌋·10⁴/λ_min_bps⌋`, ECON §2.1, and since ECON #105 (M6R-1)
//! the PumpSwap term `⌊max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)·10⁴/λ_min_bps⌋`, since
//! #106 (OWNER-M6D-1) at `λ_min_bps` = `PUMPSWAP_LOCK_MIN_BPS`: `max(⌊D·isqrt(P_ema)/10⁹⌋ − VQR⁺,
//! 0)`), plus properties: the locked-SOL terms are floored separately, and tiers are monotone in
//! liquidity and age.

mod common;

use common::Dec;
use num_bigint::BigUint;
use proptest::prelude::*;
use serde::Deserialize;
use shortwin_math::consts::PUMPSWAP_LOCK_MIN_BPS;
use shortwin_math::quote;
use shortwin_math::tier::{self, OraclePrice, Tier, TierInputs, TierVenue};

#[derive(Deserialize)]
struct File {
    generator: String,
    tiers: Vec<TierCase>,
}

#[derive(Deserialize)]
struct TierCase {
    venue: String,
    r_now: Dec<u64>,
    l_slow: Dec<u128>,
    y_slow: Dec<u128>,
    vqr: Dec<u64>,
    p_ema: Dec<u128>,
    now: Dec<i64>,
    graduated_at: Dec<i64>,
    price: Dec<i64>,
    expo: Dec<i32>,
    publish_time: Dec<i64>,
    oracle_stale_s: Dec<u32>,
    theta_a: Dec<u64>,
    theta_b: Dec<u64>,
    lock: Option<(Dec<u128>, Dec<u32>)>,
    tier: String,
    liquidity: Option<Dec<u128>>,
}

fn tier_inputs(c: &TierCase) -> TierInputs {
    let venue = match c.venue.as_str() {
        "curve" => TierVenue::PumpCurve,
        "pumpswap" => {
            // M6R-1 (ECON #105): the canonical pool's burned-LP depth; OWNER-M6D-1 (ECON #106):
            // over the program's compiled λ_min = PUMPSWAP_LOCK_MIN_BPS (100 %), whatever
            // `pool_lock_min_bps` is — every row carries it.
            let (d, l) = c.lock.as_ref().expect("pumpswap lock");
            assert_eq!(l.0, PUMPSWAP_LOCK_MIN_BPS, "PumpSwap's sizing λ_min (#106)");
            TierVenue::PumpSwap {
                locked_depth: d.0,
                lambda_min_bps: l.0,
            }
        }
        "raydium" => {
            let (d, l) = c.lock.as_ref().expect("raydium lock");
            TierVenue::Raydium {
                locked_depth: d.0,
                lambda_min_bps: l.0,
            }
        }
        v => panic!("venue {v}"),
    };
    TierInputs {
        venue,
        r_now: c.r_now.0,
        y_slow: c.y_slow.0,
        vqr: c.vqr.0,
        p_ema: c.p_ema.0,
        now: c.now.0,
        graduated_at: c.graduated_at.0,
        oracle: OraclePrice {
            price: c.price.0,
            conf: 0,
            expo: c.expo.0,
            publish_time: c.publish_time.0,
        },
        oracle_stale_s: c.oracle_stale_s.0,
        theta_a_usd_e6: c.theta_a.0,
        theta_b_usd_e6: c.theta_b.0,
    }
}

#[test]
fn tiers_match_arch() {
    let f: File = common::load_vectors("quotes", "caps.json");
    assert_eq!(f.generator, "gen_caps.py");
    let (mut locked_binds, mut pumpswap_binds) = (0, 0);
    for (i, c) in f.tiers.iter().enumerate() {
        // R_slow's reserve is the v0.4.1 reprojection of (L_slow, P_ema).
        assert_eq!(
            quote::reprice(c.l_slow.0, c.p_ema.0).map(|(_, y)| y),
            Some(c.y_slow.0),
            "tier case {i}"
        );
        let inp = tier_inputs(c);
        let want = match c.tier.as_str() {
            "A" => Tier::A,
            "B" => Tier::B,
            "C" => Tier::C,
            _ => Tier::D,
        };
        assert_eq!(tier::assign(&inp), want, "tier case {i}");
        if let Some(l) = &c.liquidity {
            assert_eq!(
                tier::liquidity_lamports(&inp),
                Ok(Some(l.0)),
                "tier case {i}"
            );
            let r = u128::from(c.r_now.0).min(c.y_slow.0.saturating_sub(c.vqr.0.into()));
            if l.0 < 2 * r {
                locked_binds += 1;
                pumpswap_binds += usize::from(c.venue == "pumpswap");
            }
        }
    }
    assert!(locked_binds >= 50, "{locked_binds}");
    // M6R-1: the PumpSwap bound binds on its own rows too.
    assert!(pumpswap_binds >= 20, "{pumpswap_binds}");
}

#[test]
fn locked_sol_floors_first() {
    // ⌊D·isqrt(P)/10⁹⌋ = 2·10⁹ (exact 2,000,000,000.99…), then ⌊·10⁴/3,000⌋.
    let p = 1_000_000_001u128 * 1_000_000_001;
    assert_eq!(tier::locked_sol(1_999_999_999, p), 2_000_000_000);
    let inp = TierInputs {
        venue: TierVenue::Raydium {
            locked_depth: 1_999_999_999,
            lambda_min_bps: 3_000,
        },
        r_now: u64::MAX,
        y_slow: u128::MAX,
        vqr: 0,
        p_ema: p,
        now: 0,
        graduated_at: 0,
        oracle: OraclePrice::default(),
        oracle_stale_s: 0,
        theta_a_usd_e6: 0,
        theta_b_usd_e6: 0,
    };
    assert_eq!(tier::liquidity_lamports(&inp), Ok(Some(2 * 6_666_666_666)));
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// `R_eff = min(R_now, R_slow, ⌊⌊D·isqrt(P)/10⁹⌋·10⁴/λ_min⌋)` exactly (ECON §2.1).
    #[test]
    fn raydium_liquidity_is_the_econ_form(r_now: u64, y_slow: u128, d: u128, p: u128,
                                          lam in 1u32..=10_000) {
        let inp = TierInputs {
            venue: TierVenue::Raydium { locked_depth: d, lambda_min_bps: lam },
            r_now,
            y_slow,
            vqr: 0,
            p_ema: p,
            now: 0,
            graduated_at: 0,
            oracle: OraclePrice::default(),
            oracle_stale_s: 0,
            theta_a_usd_e6: 0,
            theta_b_usd_e6: 0,
        };
        let locked = BigUint::from(d) * BigUint::from(p).sqrt() / 1_000_000_000u32;
        let locked = locked.min(BigUint::from(u128::MAX));
        let bound = (locked * 10_000u32 / lam).min(BigUint::from(u128::MAX));
        let r = BigUint::from(r_now).min(BigUint::from(y_slow)).min(bound);
        let got = tier::liquidity_lamports(&inp).unwrap().unwrap();
        prop_assert_eq!(BigUint::from(got), (r * 2u8).min(BigUint::from(u128::MAX)));
    }

    /// PumpSwap (M6R-1, ECON #105): `R_eff = min(R_now, R_slow − VQR⁺, ⌊max(⌊D·isqrt(P)/10⁹⌋ −
    /// VQR⁺, 0)·10⁴/λ_min⌋)` exactly (`VQR⁺` the input `vqr`).
    #[test]
    fn pumpswap_liquidity_is_the_econ_form(r_now: u64, y_slow: u128, vqr: u64, d: u128, p: u128,
                                           lam in 1u32..=10_000) {
        let inp = TierInputs {
            venue: TierVenue::PumpSwap { locked_depth: d, lambda_min_bps: lam },
            r_now,
            y_slow,
            vqr,
            p_ema: p,
            now: 0,
            graduated_at: 0,
            oracle: OraclePrice::default(),
            oracle_stale_s: 0,
            theta_a_usd_e6: 0,
            theta_b_usd_e6: 0,
        };
        let v = BigUint::from(vqr);
        let locked = BigUint::from(d) * BigUint::from(p).sqrt() / 1_000_000_000u32;
        let locked = locked.min(BigUint::from(u128::MAX));
        let locked = if locked > v { locked - &v } else { BigUint::from(0u8) };
        let bound = (locked * 10_000u32 / lam).min(BigUint::from(u128::MAX));
        let y = BigUint::from(y_slow);
        let r_slow = if y > v { y - &v } else { BigUint::from(0u8) };
        let r = BigUint::from(r_now).min(r_slow).min(bound);
        let got = tier::liquidity_lamports(&inp).unwrap().unwrap();
        prop_assert_eq!(BigUint::from(got), (r * 2u8).min(BigUint::from(u128::MAX)));
    }

    /// More liquidity or more age never gives a riskier tier (A < B < C), and a stale
    /// oracle always gives C.
    #[test]
    fn tier_monotone(r in 0u64..10u64.pow(14), extra in 0u64..10u64.pow(14), age in 0i64..10_000_000,
                     more in 0i64..10_000_000, price in 1i64..10i64.pow(12), stale: bool) {
        let now = 1_800_000_000i64;
        let mk = |r: u64, age: i64| TierInputs {
            venue: TierVenue::Unlocked,
            r_now: r,
            y_slow: u128::from(r),
            vqr: 0,
            p_ema: 1,
            now,
            graduated_at: now - age,
            oracle: OraclePrice { price, conf: 0, expo: -8, publish_time: if stale { now - 3_601 } else { now } },
            oracle_stale_s: 3_600,
            theta_a_usd_e6: 500_000_000_000,
            theta_b_usd_e6: 100_000_000_000,
        };
        let t0 = tier::assign(&mk(r, age));
        prop_assert!(tier::assign(&mk(r + extra, age)) <= t0);
        prop_assert!(tier::assign(&mk(r, age + more)) <= t0);
        if stale {
            prop_assert_eq!(t0, Tier::C);
        }
    }
}
