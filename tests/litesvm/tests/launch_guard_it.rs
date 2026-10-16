//! The launch guard and the notional floor at their exact boundaries (ADOPTED Q11, Q21; ARCH §3.4
//! step 9, §4.6 "`N_open ≥ min_notional`; `Σ N_open ≤ market_max_notional` (beta 25 SOL)"; ECON
//! §6.1, §15): an open is refused `MarketNotionalCap` when the market's open notional plus its own
//! `N_open` would exceed `market_max_notional` (25 SOL) and `BelowMinNotional` when `N_open` is
//! under `min_notional` (0.1 SOL); `N_open = B̂_spot(q)` is the buyback of `q` quoted on the
//! post-open reserves.
//!
//! On the mock, `N_open(q)` is recomputed here from `shortwin_math::quote::cpmm` (the sale
//! `sell_exact_in(q)` on the pool's pricing reserves gives the post-open reserves, the buyback
//! `buy_exact_out(q)` on them `N_open`), and the pool is chosen so that `N_open` takes every
//! lamport value: an open whose notional brings the market to **exactly 25 SOL** lands, one raw
//! token more is refused; an open of exactly 0.1 SOL lands, one raw token less is refused. On the
//! real venues' forks (AMM v4, PumpSwap) `N_open` is the program's own report, probed on copies
//! of the VM with the guard lifted, and the refusal switches exactly there.

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "engine_it/common.rs"]
mod ecommon;

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod rcommon;

#[allow(unused_imports)]
#[path = "pamm_it/common.rs"]
mod pcommon;

/// The notional arithmetic and the probes.
mod n {
    use shortwin::events::Open;
    use shortwin::state::GlobalConfig;
    use shortwin_litesvm_tests::engine::{Borrower, MockMarket, VenueMarket, crank_price_ix};
    use shortwin_litesvm_tests::errors::ok;
    use shortwin_litesvm_tests::{Env, events, fixtures, mock, pda, read};
    use shortwin_math::quote::cpmm;

    /// `N_open(q)` on the mock pool now: the sale of `q`, then the buyback of `q` on the
    /// post-sale pricing reserves (the mock's fee on the input, its 12 % protocol share moved
    /// out of the pricing reserves).
    pub fn n_open(env: &Env, mm: &MockMarket, q: u64) -> Option<u64> {
        let r = mock::reserves(&env.svm, mm.mock());
        let fee = mock::state(&env.svm, &mm.mock().pool).fee_ppm;
        let fees = mock::cpmm_fees(fee);
        let s = cpmm::sell_exact_in(q, u128::from(r.base), u128::from(r.quote), &fees)?;
        cpmm::buy_exact_out(q, s.sol_after, s.token_after, &fees).map(|b| b.cost)
    }

    /// The least `q ∈ [1, hi]` with `f(q) ≥ target` (`f` non-decreasing).
    pub fn least_q(f: impl Fn(u64) -> u64, target: u64, hi: u64) -> u64 {
        assert!(f(hi) >= target, "{} < {target}", f(hi));
        let (mut lo, mut hi) = (0u64, hi);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if f(mid) >= target {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }

    /// `crank_price` one second later (the second's observation state fixed).
    pub fn next_second(env: &mut Env, mm: &impl VenueMarket) {
        env.warp(1);
        ok(env.send(&[crank_price_ix(mm)], &[]));
    }

    /// **TEST-ONLY** (on a copy of the VM): `market_max_notional` lifted to `u64::MAX`.
    fn lift_guard(env: &mut Env) {
        let key = pda::config();
        let mut c: GlobalConfig = read::zc(&env.svm, &key).expect("config");
        c.params.market_max_notional = u64::MAX;
        let extra = read::lamports(&env.svm, &key)
            - env
                .svm
                .minimum_balance_for_rent_exemption(GlobalConfig::SPACE);
        fixtures::set_zc(&mut env.svm, &key, &c, extra);
    }

    /// The program's own `N_open` of an open of `q` by `b` now, measured on a copy of the VM
    /// with the launch guard lifted (the VM restored afterwards); `None` if a cap refuses it.
    pub fn probe(env: &mut Env, mm: &impl VenueMarket, b: &mut Borrower, q: u64) -> Option<u64> {
        let saved = env.svm.clone();
        let nonce = b.next_nonce;
        env.check_invariants = false;
        lift_guard(env);
        let (res, _) = env.open(mm, b, q);
        let n = res.ok().map(|m| events::one::<Open>(&m).n_open);
        env.svm = saved;
        env.check_invariants = true;
        b.next_nonce = nonce;
        n
    }
}

/// The mock venue: the exact values.
mod mock {
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::Open;
    use shortwin_litesvm_tests::engine::MarketSpec;

    use crate::ecommon::*;
    use crate::n;

    const SOL: u64 = 1_000_000_000;

    /// A pool deep enough that every cap is far above 25 SOL of notional (10,000 SOL against
    /// 10¹⁶ raw tokens at a 1 % fee: the wallet cap alone is ≈ 187 SOL), and fine enough that
    /// `N_open` moves by less than a lamport per raw token (`P = 10⁻³` lamport).
    fn deep() -> (Env, MockMarket) {
        let mut env = Env::test_venue();
        let mm = env.mock_market(MarketSpec {
            fee_ppm: 10_000,
            base: 10_000_000_000_000_000,
            quote: 10_000_000_000_000,
            lend: 1_000_000_000_000_000,
            ..MarketSpec::default()
        });
        n::next_second(&mut env, &mm);
        (env, mm)
    }

    /// `market_max_notional` (25 SOL): one position of exactly 10 SOL, then one bringing the
    /// market to exactly 25 SOL lands; one raw token more is refused; the full market refuses
    /// even the smallest open.
    #[test]
    fn exactly_25_sol() {
        let (mut env, mm) = deep();
        let max = env.config().params.market_max_notional;
        assert_eq!(max, 25 * SOL, "the beta launch guard (ADOPTED Q21)");
        let hi = 100_000_000_000_000;
        // 10 SOL exactly.
        let q1 = n::least_q(|q| n::n_open(&env, &mm, q).unwrap(), 10 * SOL, hi);
        assert_eq!(
            n::n_open(&env, &mm, q1),
            Some(10 * SOL),
            "N_open hits 10 SOL"
        );
        let mut b1 = env.borrower();
        let (o1, _, _) = open_ok(&mut env, &mm, &mut b1, q1);
        assert_eq!(o1.n_open, 10 * SOL);
        assert_eq!(env.market(&mm.market()).total_notional_open, 10 * SOL);
        // The rest: 15 SOL exactly. `N_open` moves by less than a lamport per raw token, so the
        // boundary is the largest `q` with `N_open ≤ room`: one below the least `q` over it.
        let room = max - 10 * SOL;
        let q2 = n::least_q(|q| n::n_open(&env, &mm, q).unwrap(), room + 1, hi) - 1;
        assert_eq!(n::n_open(&env, &mm, q2), Some(room));
        assert!(n::n_open(&env, &mm, q2 + 1).unwrap() > room);
        let mut b2 = env.borrower();
        let (res, _) = env.open(&mm, &mut b2, q2 + 1);
        assert_err(&res, E::MarketNotionalCap);
        let (res, _) = env.open(&mm, &mut b2, q2);
        let o2: Open = events::one(&ok(res));
        assert_eq!(o2.n_open, room);
        assert_eq!(
            env.market(&mm.market()).total_notional_open,
            max,
            "exactly 25 SOL open"
        );
        // Full: the smallest open above the floor is refused.
        let q_min = n::least_q(
            |q| n::n_open(&env, &mm, q).unwrap_or(0),
            env.config().params.min_notional,
            hi,
        );
        let mut b3 = env.borrower();
        let (res, _) = env.open(&mm, &mut b3, q_min);
        assert_err(&res, E::MarketNotionalCap);
        // A close frees its notional.
        env.warp(1);
        env.crank(&mm);
        close_ok(&mut env, &mm, &b1, 0);
        assert_eq!(env.market(&mm.market()).total_notional_open, room);
        n::next_second(&mut env, &mm);
        let (o, _, _) = open_ok(&mut env, &mm, &mut b3, q_min);
        assert!(o.n_open >= env.config().params.min_notional);
    }

    /// `min_notional` (0.1 SOL): an open of exactly 0.1 SOL lands; one raw token less is refused.
    #[test]
    fn exactly_the_minimum() {
        let (mut env, mm) = deep();
        let min = env.config().params.min_notional;
        assert_eq!(min, SOL / 10);
        let q = n::least_q(
            |q| n::n_open(&env, &mm, q).unwrap_or(0),
            min,
            1_000_000_000_000,
        );
        assert_eq!(n::n_open(&env, &mm, q), Some(min), "N_open hits 0.1 SOL");
        assert!(n::n_open(&env, &mm, q - 1).unwrap() < min);
        let mut b = env.borrower();
        let (res, _) = env.open(&mm, &mut b, q - 1);
        assert_err(&res, E::BelowMinNotional);
        let (res, _) = env.open(&mm, &mut b, q);
        let o: Open = events::one(&ok(res));
        assert_eq!(o.n_open, min);
    }
}

/// The real venues: the program's own `N_open`, the refusal switching exactly there.
mod venues {
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::Open;
    use shortwin_litesvm_tests::engine::{Borrower, VenueMarket};
    use shortwin_litesvm_tests::errors::{assert_err, ok};
    use shortwin_litesvm_tests::{Env, events};

    use crate::n;

    /// On `mm` now: the largest `q` whose `N_open` fits the remaining room lands, one more raw
    /// token is refused `MarketNotionalCap`; the market is then at most 25 SOL open, and the
    /// refused open's own `N_open` would have exceeded it.
    fn exact_guard(env: &mut Env, mm: &impl VenueMarket, lo: u64, hi: u64) {
        let max = env.config().params.market_max_notional;
        let total = env.market(&mm.market()).total_notional_open;
        let room = max - total;
        let mut b: Borrower = env.borrower();
        env.fund(&b.key(), 50_000_000_000_000);
        // The largest q with N_open(q) ≤ room (a q a cap refuses counts as too large; the
        // refusal below then shows the guard, not a cap, sits at the boundary).
        let fits = |env: &mut Env, b: &mut Borrower, q: u64| {
            n::probe(env, mm, b, q).is_some_and(|n| n <= room)
        };
        let (mut lo, mut up) = (lo, hi);
        assert!(fits(env, &mut b, lo));
        assert!(!fits(env, &mut b, up));
        while up - lo > 1 {
            let mid = lo + (up - lo) / 2;
            if fits(env, &mut b, mid) {
                lo = mid;
            } else {
                up = mid;
            }
        }
        let over = n::probe(env, mm, &mut b, lo + 1).expect("no cap at the boundary");
        assert!(over > room);
        let (res, _) = env.open(mm, &mut b, lo + 1);
        assert_err(&res, E::MarketNotionalCap);
        let (res, _) = env.open(mm, &mut b, lo);
        let o: Open = events::one(&ok(res));
        let after = env.market(&mm.market()).total_notional_open;
        assert_eq!(after, total + o.n_open);
        assert!(after <= max && total + over > max);
        eprintln!(
            "launch guard: q {lo} → N_open {} (room {room}); q + 1 → {over}",
            o.n_open
        );
    }

    /// Fartcoin on AMM v4 (every cap far above 25 SOL of notional on this pool).
    #[test]
    fn amm_v4() {
        use crate::rcommon::*;
        let (mut env, rm, q) = setup(FART);
        n::next_second(&mut env, &rm);
        exact_guard(&mut env, &rm, q / 2, q * 40);
    }

    /// The mature PumpSwap pool, a first position open.
    #[test]
    fn pumpswap() {
        use crate::pcommon::*;
        let (mut env, pm, q) = setup(PammCoin::Mature);
        let mut b = env.borrower();
        open_ok(&mut env, &pm, &mut b, q);
        n::next_second(&mut env, &pm);
        exact_guard(&mut env, &pm, q / 2, q * 4);
    }
}
