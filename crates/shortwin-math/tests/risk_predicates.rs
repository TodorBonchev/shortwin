//! `risk` against `tests/vectors/risk/exits.json` (`gen_exits.py`: CR, the spot leg, the
//! EMA leg on the ring's cached `(x, y)`, "normal buyback allowed", σ, the D4 margin on
//! the newest ring entry's cached reserves (and on the spot quote alone when that quote is
//! undefined), open fee and minimum rent, captured from
//! `shortwin_sim.py` v0.4.1 runs — exact, no tolerance), plus properties: CR is monotone
//! in price, the buyback predicate is monotone in its quote and σ matches its definition.
//!
//! The settlement-box predicates (S1/S2 legs, refutation, "not liquidatable", amount,
//! arming) are `settle`'s and are tested there.

mod common;

use std::collections::HashMap;

use common::Dec;
use num_bigint::BigUint;
use proptest::prelude::*;
use serde::Deserialize;
use shortwin_math::fees::FeeBps;
use shortwin_math::num::WAD;
use shortwin_math::quote::Pricing;
use shortwin_math::rent::{self, RentTerms};
use shortwin_math::risk::{self, Budget};

#[derive(Deserialize)]
struct File {
    generator: String,
    spot_leg: Vec<SpotLeg>,
    cr: Vec<Cr>,
    ema_leg: Vec<EmaLeg>,
    buyback: Vec<Buyback>,
    sigma: Vec<Sigma>,
    margin: Vec<Margin>,
    margin_ema_undefined: Vec<Margin>,
    open_fee: Vec<OpenFee>,
    min_rent: Vec<MinRent>,
}

#[derive(Deserialize)]
struct SpotLeg {
    executable: bool,
    fresh: bool,
    v: Dec<u64>,
    r_due: Dec<u64>,
    b_rs: Option<Dec<u64>>,
    l_bps: Dec<u32>,
    expected: bool,
}

#[derive(Deserialize)]
struct Cr {
    v: Dec<u64>,
    r_due: Dec<u64>,
    b_hat: Option<Dec<u64>>,
    l_bps: Dec<u32>,
    cr: Dec<u64>,
    below: bool,
}

/// A ring entry's cached `(x, y)` and the position's risk quote on it.
type Entry = (Dec<u128>, Dec<u128>, Option<Dec<u64>>);

/// The ring's entries, newest first.
#[derive(Deserialize)]
struct EmaLeg {
    v: Dec<u64>,
    r_due: Dec<u64>,
    l_bps: Dec<u32>,
    n: Dec<u32>,
    entries: Vec<Entry>,
    run: Dec<u32>,
    expected: bool,
}

#[derive(Deserialize)]
struct Buyback {
    v: Dec<u64>,
    k_s: Dec<u64>,
    ert: Dec<u64>,
    b_live: Option<Dec<u64>>,
    sigma_live: Dec<u64>,
    sigma_max: Dec<u32>,
    expected: bool,
}

#[derive(Deserialize)]
struct Sigma {
    n: Option<Dec<u64>>,
    q: Dec<u64>,
    x: Dec<u128>,
    y: Dec<u128>,
    expected: Dec<u64>,
}

#[derive(Deserialize)]
struct Margin {
    s: Dec<u64>,
    b_spot: Dec<u64>,
    b_ema: Option<Dec<u64>>,
    cr_open_bps: Dec<u32>,
    expected: Dec<u64>,
}

#[derive(Deserialize)]
struct OpenFee {
    n_open: Dec<u64>,
    open_fee_bps: Dec<u32>,
    expected: Dec<u64>,
}

#[derive(Deserialize)]
struct MinRent {
    q: Dec<u64>,
    p_ema: Dec<u128>,
    terms: [Dec<u32>; 3],
    util_bps: Dec<u32>,
    kink_bps: Dec<u32>,
    min_rent_s: Dec<u32>,
    expected: Dec<u64>,
}

fn q(v: &Option<Dec<u64>>) -> Option<u64> {
    v.as_ref().map(|d| d.0)
}

fn load() -> File {
    let f: File = common::load_vectors("risk", "exits.json");
    assert_eq!(f.generator, "gen_exits.py");
    f
}

#[test]
fn cr_and_spot_leg_match_sim() {
    let f = load();
    assert!(f.spot_leg.iter().any(|c| c.expected) && f.spot_leg.iter().any(|c| !c.expected));
    for (i, c) in f.spot_leg.iter().enumerate() {
        let ve = risk::v_eff(c.v.0, c.r_due.0);
        let got = risk::spot_leg(c.executable, c.fresh, ve, q(&c.b_rs), c.l_bps.0).unwrap();
        assert_eq!(got, c.expected, "spot_leg case {i}");
    }
    for (i, c) in f.cr.iter().enumerate() {
        let ve = risk::v_eff(c.v.0, c.r_due.0);
        assert_eq!(risk::cr_bps(ve, q(&c.b_hat)), Ok(c.cr.0), "cr case {i}");
        assert_eq!(
            risk::cr_below(ve, q(&c.b_hat), c.l_bps.0),
            Ok(c.below),
            "cr_below case {i}"
        );
    }
}

/// Decision 2 (ARCH §4.5): the EMA leg holds iff the newest `liq_persist_n` ring entries
/// are all below `L` on their cached `(x, y)`; `ring_below` is the per-entry predicate
/// (sim `Market.status`: the run of newest entries below `L`).
#[test]
fn ema_leg_on_cached_entries_matches_sim() {
    let f = load();
    assert!(f.ema_leg.iter().any(|c| c.expected) && f.ema_leg.iter().any(|c| !c.expected));
    for (i, c) in f.ema_leg.iter().enumerate() {
        let quotes: HashMap<(u128, u128), Option<u64>> = c
            .entries
            .iter()
            .map(|(x, y, b)| ((x.0, y.0), q(b)))
            .collect();
        let ve = risk::v_eff(c.v.0, c.r_due.0);
        let mut below = risk::ring_below(ve, c.l_bps.0, |x, y| quotes[&(x, y)]);
        let mut run = 0u32;
        for (x, y, _) in &c.entries {
            if !below(x.0, y.0).unwrap() {
                break;
            }
            run += 1;
        }
        assert_eq!(run, c.run.0, "ema_leg case {i}: run");
        assert_eq!(run >= c.n.0, c.expected, "ema_leg case {i}");
    }
}

/// ARCH §3.6 step 4 (sim `try_exit`'s `buyback_ok`): `B̂_live + ert ≤ V − K_s ∧ σ_live ≤ σ_max`.
#[test]
fn buyback_allowed_matches_sim() {
    let f = load();
    for (i, c) in f.buyback.iter().enumerate() {
        let b = Budget {
            v: c.v.0,
            k_s: c.k_s.0,
            ert: c.ert.0,
        };
        let got = risk::buyback_allowed(&b, q(&c.b_live), c.sigma_live.0, c.sigma_max.0);
        assert_eq!(got, Ok(c.expected), "buyback case {i}");
    }
}

#[test]
fn sigma_matches_sim() {
    let f = load();
    assert!(f.sigma.len() > 100);
    for (i, c) in f.sigma.iter().enumerate() {
        let got = risk::sigma_xy(q(&c.n), c.q.0, c.x.0, c.y.0);
        assert_eq!(got, Ok(c.expected.0), "sigma case {i}");
    }
}

/// D4 (sim `Market.open`): `B̂_ema` on the newest ring entry's cached `(x, y)` after the
/// open's roll; open fee on `N_open`; `R_min` at the exact kinked rate.
#[test]
fn open_terms_match_sim() {
    let f = load();
    assert!(f.margin.len() > 50);
    for (i, c) in f.margin.iter().enumerate() {
        assert!(c.b_ema.is_some(), "margin case {i}");
        let m = risk::margin_d4(c.s.0, c.b_spot.0, q(&c.b_ema), c.cr_open_bps.0);
        assert_eq!(m, Ok(c.expected.0), "margin case {i}");
    }
    // M0B-S-3 (review): opens whose EMA-leg quote is undefined (`Market.open`'s `qe is
    // None` branch) price the margin on the spot quote alone, `B_margin = max(qs[1], qe[1]
    // if qe else qs[1])`; ARCH §3.4 with §4.3's `+∞` would refuse them instead.
    assert!(f.margin_ema_undefined.len() >= 20);
    for (i, c) in f.margin_ema_undefined.iter().enumerate() {
        assert!(c.b_ema.is_none(), "undefined-EMA margin case {i}");
        let m = risk::margin_d4(c.s.0, c.b_spot.0, None, c.cr_open_bps.0);
        assert_eq!(m, Ok(c.expected.0), "undefined-EMA margin case {i}");
    }
    for c in &f.open_fee {
        assert_eq!(
            risk::open_fee(c.n_open.0, c.open_fee_bps.0),
            Ok(c.expected.0)
        );
    }
    for (i, c) in f.min_rent.iter().enumerate() {
        let t = RentTerms {
            base_bps: c.terms[0].0,
            slope1_bps: c.terms[1].0,
            slope2_bps: c.terms[2].0,
        };
        let r = rent::min_rent(
            c.q.0,
            c.p_ema.0,
            &t,
            c.util_bps.0,
            c.kink_bps.0,
            c.min_rent_s.0,
        );
        assert_eq!(r, Ok(c.expected.0), "min_rent case {i}");
    }
}

/// An independent fee-on-SOL-leg buy quote (ECON §3.1, one fee component) for the σ
/// property: `(n, B̂)`.
fn buy_quote(q: u64, x: u128, y: u128, fee_bps: u128) -> Option<(u64, u64)> {
    let q = u128::from(q);
    if q >= x {
        return None;
    }
    let (num, den) = (BigUint::from(y) * q, BigUint::from(x - q));
    let n = u64::try_from((num + &den - 1u8) / den).ok()?;
    let total = u128::from(n) + (u128::from(n) * fee_bps).div_ceil(10_000);
    Some((n, u64::try_from(total).ok()?))
}

/// Review F-S4: σ is ECON §5.6's fee-free premium. On a CPMM pool with the creator fee on
/// the token side the venue swaps `sw(q')`, `q'` grossed up by that fee; `BuyQuote::net`
/// is the fee-free `n(q)`, so σ does not count the creator fee (101 bps, not 255).
#[test]
fn sigma_is_fee_free_on_creator_output_pools() {
    use shortwin_math::fees::CpmmFees;
    use shortwin_math::quote::cpmm;
    let (x_tok, y_sol, q) = (
        1_000_000_000_000_000u128,
        1_000_000_000_000u128,
        10_000_000_000_000u64,
    );
    let f = CpmmFees {
        trade: 50,
        creator: 14_950,
        creator_on_input: false,
        protocol: 120_000,
        fund: 40_000,
    };
    let b = cpmm::buy_exact_out(q, y_sol, x_tok, &f).unwrap();
    let fee_free_n = (y_sol * u128::from(q)).div_ceil(x_tok - u128::from(q));
    assert_eq!(u128::from(b.net), fee_free_n);
    assert_eq!(risk::sigma_xy(Some(b.net), q, x_tok, y_sol), Ok(101));
    // The creator fee still costs: the quote covers sw(q') plus the trade fee.
    let on_input = CpmmFees {
        creator_on_input: true,
        ..f
    };
    let b_in = cpmm::buy_exact_out(q, y_sol, x_tok, &on_input).unwrap();
    assert_eq!(b_in.net, b.net);
    assert!(b.cost > b.net + 1_000_000);
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// CR on `B̂(Q; reprice(L, P))` never rises when the price rises (ECON §3.3, §5.5).
    #[test]
    fn cr_monotone_in_price(
        l in 10u128.pow(9)..10u128.pow(16),
        p1 in 10u128.pow(10)..10u128.pow(24),
        bump in 0u128..10u128.pow(24),
        q in 1u64..10u64.pow(14),
        v in 0u64..10u64.pow(15),
        r in 0u64..10u64.pow(12),
        fee in 0u64..500,
        lb in 10_500u32..=20_000,
    ) {
        let p2 = p1.saturating_add(bump);
        let ve = risk::v_eff(v, r);
        // The real composition: B̂(Q; reprice(L, P)) through `quote::Pricing`.
        let venue = Pricing::PumpSwap(FeeBps::new(0, fee, 0));
        let quote = |p| venue.buy_repriced(q, l, p).map(|b| b.cost);
        let (b1, b2) = (quote(p1), quote(p2));
        if let (Some(b1), Some(b2)) = (b1, b2) {
            prop_assert!(b2 >= b1);
        }
        // An undefined quote at the higher price reads as +∞, CR 0.
        let cr1 = risk::cr_bps(ve, b1.filter(|b| *b > 0)).unwrap_or(0);
        let cr2 = risk::cr_bps(ve, b2.filter(|b| *b > 0)).unwrap_or(0);
        prop_assert!(cr2 <= cr1);
        // cr_below agrees with the materialised CR, and is monotone too.
        if let Some(b) = b1.filter(|b| *b > 0) {
            prop_assert_eq!(risk::cr_below(ve, Some(b), lb).unwrap(), cr1 < u64::from(lb));
            if risk::cr_below(ve, Some(b), lb).unwrap() {
                prop_assert!(risk::cr_below(ve, b2.filter(|b| *b > 0), lb).unwrap());
            }
        }
        // More collateral never lowers CR.
        if let Some(b) = b1.filter(|b| *b > 0) {
            prop_assert!(risk::cr_bps(ve.saturating_add(1), Some(b)).unwrap() >= cr1);
        }
    }

    /// σ matches `n·WAD/(q·P) − 1` exactly (u128 path and 256-bit fallback alike) and is
    /// monotone in `n`.
    #[test]
    fn sigma_definition(x in 2u128..10u128.pow(16), y in 1u128..10u128.pow(16), q in 1u64..10u64.pow(15),
                        extra in 0u64..1_000_000, big_p in 10u128.pow(30)..u128::MAX) {
        let Some((n, _)) = buy_quote(q, x, y, 0) else { return Ok(()); };
        let p = y * WAD / x;
        prop_assume!(p > 0);
        let s = risk::sigma_xy(Some(n), q, x, y).unwrap();
        let ratio = BigUint::from(n) * WAD * 10_000u32 / (BigUint::from(q) * p);
        let want = u64::try_from(ratio).unwrap_or(u64::MAX).saturating_sub(10_000);
        prop_assert_eq!(s, want);
        let s2 = risk::sigma(Some(n.saturating_add(extra)), q, p).unwrap();
        prop_assert!(s2 >= s);
        prop_assert_eq!(risk::sigma(None, q, p), Ok(u64::MAX));
        // Prices where Q·P leaves u128 (the fallback path).
        let ratio = BigUint::from(n) * WAD * 10_000u32 / (BigUint::from(q) * big_p);
        let want = u64::try_from(ratio).unwrap_or(u64::MAX).saturating_sub(10_000);
        prop_assert_eq!(risk::sigma(Some(n), q, big_p), Ok(want));
    }

    /// "Normal buyback allowed" is the complement of the live settlement leg (budget or σ
    /// exceeded) and monotone in its quote: a costlier buyback is never more allowed.
    #[test]
    fn buyback_allowed_is_monotone(v in 0u64..10u64.pow(12), ks in 0u64..10u64.pow(9), ert in 0u64..10_000_000,
                                   b in prop::option::of(1u64..10u64.pow(12)), d in 0u64..10u64.pow(10),
                                   sg in 0u64..10_000, smax in 500u32..=5_000) {
        prop_assume!(ks <= v);
        let bud = Budget { v, k_s: ks, ert };
        let allowed = risk::buyback_allowed(&bud, b, sg, smax).unwrap();
        let live_leg = bud.exceeded_by(b).unwrap() || sg > u64::from(smax);
        prop_assert_eq!(allowed, !live_leg);
        let higher = b.map(|b| b.saturating_add(d));
        if risk::buyback_allowed(&bud, higher, sg, smax).unwrap() {
            prop_assert!(allowed);
        }
        prop_assert!(!risk::buyback_allowed(&bud, None, sg, smax).unwrap());
    }

    /// D4 margin makes the opening CR hold at both spot and EMA: (S + M)·10⁴ ≥ CR_open·B̂.
    #[test]
    fn margin_makes_opening_cr_hold(s in 0u64..10u64.pow(13), bs in 1u64..10u64.pow(13),
                                    be in 1u64..10u64.pow(13), cr in 10_500u32..=100_000) {
        let m = risk::margin_d4(s, bs, Some(be), cr).unwrap();
        let v = u128::from(s) + u128::from(m);
        prop_assert!(v * 10_000 >= u128::from(cr) * u128::from(bs.max(be)));
        if m > 0 {
            prop_assert!((v - 1) * 10_000 < u128::from(cr) * u128::from(bs.max(be)));
        }
        prop_assert!(risk::cr_bps(u64::try_from(v).unwrap(), Some(bs)).unwrap() >= u64::from(cr) || m == 0);
        // An undefined EMA quote prices the spot quote alone (sim `Market.open`).
        prop_assert_eq!(risk::margin_d4(s, bs, None, cr), risk::margin_d4(s, bs, Some(bs), cr));
    }
}
