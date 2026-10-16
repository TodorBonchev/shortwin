//! A list-only market adopts its Raydium pool, and the listing window (ARCH §2.5 `None`, §2.6
//! rules 1 and 4, §3.9; M3-SEC-3; D1; O13, O13b).

use shortwin::constants::T_CLAMP;
use shortwin::events::{POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW};
use shortwin::state::{
    MARKET_FLAG_GRADUATED_ELSEWHERE, MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN, raydium_cache,
};
use shortwin_litesvm_tests::raydium::{
    CurveSpec, curve_bytes, fartcoin_curve_bytes, plant_canonical_pool, plant_curve,
};
use shortwin_math::fees::Direction;
use shortwin_math::num::WAD;
use shortwin_math::tier::Tier;

use crate::common::*;
use crate::support::*;

/// The price state a listing starts on `pool`: `P_ema = P_spot`, `L_ema = √(X·Y)` (under the
/// anchor `D/λ_min` on these pools), `L_slow = min(√(X·Y), D)` (AIR-13), one ring entry at `now`,
/// an empty bucket.
fn assert_fresh_price_state(env: &Env, pool: &AnyPool) {
    let m = market_of(env, pool);
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let l = (u128::from(x) * u128::from(y)).isqrt();
    assert_eq!(m.ema_price.get(), u128::from(y) * WAD / u128::from(x));
    assert_eq!(
        (m.ema_liq.get(), m.liq_slow.get()),
        (l, l.min(m.pool_d.get()))
    );
    assert_eq!((m.ring_count, m.ema_ts), (1, env.now()));
    assert_eq!((m.b_sec, m.b_lo.get()), (0, 0));
}

/// M3-SEC-3 (a), inverted: an `Auto` listing of Fartcoin (list-only `None`, whoever sent it
/// first) is moved onto its main AMM v4 pool by `propose_pool` — with Fartcoin's real curve (it
/// graduated to Raydium before PumpSwap) planted, so D1 is checked on the real bytes and the
/// market records `graduated_elsewhere`. Exact state: venue, pool cache, SOL side, `pool_d`,
/// age from the adoption (O13: AMM v4), the listing window, a fresh price state, the event; the
/// market then trades on the pool after the window.
#[test]
fn a_list_only_market_adopts_its_pool() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    let (mint, _) = pool.token();
    plant_curve(&mut env.svm, &mint, fartcoin_curve_bytes());
    list_auto(&mut env, &pool);
    let before = market_of(&env, &pool);
    assert_eq!(before.venue_kind(), Some(VenueKind::None));
    env.warp(30);
    let ev = adopt_ok(&mut env, None, &pool);
    let now = env.now();
    let m = market_of(&env, &pool);
    let AnyPool::V4(p) = &pool else {
        unreachable!()
    };
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumV4));
    assert_eq!(m.pool_cache[raydium_cache::RAY_POOL], p.pool);
    assert_eq!(m.pool_cache[raydium_cache::RAY_VAULT_SOL], p.coin_vault);
    assert_eq!(m.pool_cache[raydium_cache::RAY_VAULT_TOK], p.pc_vault);
    assert_eq!(m.pool_cache[raydium_cache::RAY_LP_MINT], p.lp_mint);
    assert!(
        m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
        "WSOL is the coin"
    );
    assert!(
        m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE),
        "D1 detection on the real curve"
    );
    assert_eq!(m.pool_d.get(), d_of(&env, &pool));
    assert_eq!(
        (m.graduated_at, m.pool_seen_at),
        (now, now),
        "O13: AMM v4 ages from the adoption"
    );
    assert_eq!(m.created_at, before.created_at);
    assert_eq!(m.tier, Tier::C as u8);
    assert_fresh_price_state(&env, &pool);
    assert_eq!(
        (m.no_route_since, m.last_no_route_obs, m.pool_fail_since),
        (0, 0, 0),
        "the list-only market's S3 chain cleared"
    );
    let window = i64::from(env.config().params.pool_list_window_s);
    assert_eq!(
        (ev.candidate, ev.kind, ev.d_cand, ev.d_inc, ev.window_ends),
        (p.pool, POOL_KIND_LISTING, m.pool_d.get(), 0, now + window)
    );
    // The window is ARCH §2.6 rule 1's; then the market trades on the pool.
    let (rm, q) = adopted_market(&mut env, pool);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    env.warp(5);
    env.crank(&rm);
    close_ok(&mut env, &rm, &b, nonce);
    // The market is listed now: no listing can replace it.
    assert!(env.list_ray(&rm.pool, VENUE_HINT_V4).is_err());
}

/// O13b: a CPMM pool with a provable `open_time` (≥ `T_CLAMP`) ages from it when a list-only
/// market adopts it (USELESS, 2025-05-10: Tier A at once with a fresh oracle); one whose
/// `open_time` precedes `T_CLAMP` ages from the adoption (O13).
#[test]
fn a_cpmm_adoption_ages_from_open_time() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(USELESS);
    list_auto(&mut env, &pool);
    adopt_ok(&mut env, None, &pool);
    let m = market_of(&env, &pool);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert_eq!(m.graduated_at, 1_746_886_997, "the pool's open_time");
    assert!(
        !m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE),
        "no pump curve"
    );
    assert_fresh_price_state(&env, &pool);
    let (rm, _) = adopted_market(&mut env, pool);
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(env.market(&rm.market()).tier, Tier::A as u8);

    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(USELESS);
    let AnyPool::Cpmm(p) = &pool else {
        unreachable!()
    };
    fork::set_cpmm_open_time(&mut env.svm, p, T_CLAMP.unsigned_abs() - 1);
    list_auto(&mut env, &pool);
    adopt_ok(&mut env, None, &pool);
    assert_eq!(market_of(&env, &pool).graduated_at, env.now(), "O13");
}

/// D1 (ARCH §2.5 "Precedence"): a list-only market of a pump coin whose curve is live, still
/// migrating, or completed with a valid canonical PumpSwap pool refuses every Raydium
/// candidate (`PumpVenuePrecedence`); a mayhem curve and a non-SOL curve refuse with their own
/// codes (ADOPTED Q6), a curve account shorter than 115 bytes with `CurveNeedsExtend`. The
/// market stays list-only.
#[test]
fn pump_coins_keep_their_pump_venue() {
    let wsol = shortwin::constants::WSOL_MINT;
    let curve = |complete: bool, vt: u64| CurveSpec {
        virtual_token_reserves: vt,
        virtual_quote_reserves: 30_000_000_000,
        real_token_reserves: vt / 2,
        real_quote_reserves: 1,
        complete,
        len: 151,
        ..CurveSpec::default()
    };
    let cases: [(&str, CurveSpec, bool, E); 6] = [
        (
            "live curve",
            curve(false, 1_000_000_000_000),
            false,
            E::PumpVenuePrecedence,
        ),
        (
            "migrating",
            curve(true, 279_900_000_000_000),
            false,
            E::PumpVenuePrecedence,
        ),
        (
            "canonical pool",
            curve(true, 0),
            true,
            E::PumpVenuePrecedence,
        ),
        (
            "mayhem",
            CurveSpec {
                is_mayhem_mode: true,
                ..curve(false, 1)
            },
            false,
            E::CurveMayhemUnsupported,
        ),
        (
            "USDC curve",
            CurveSpec {
                quote_mint: Pubkey::new_unique(),
                ..curve(false, 1)
            },
            false,
            E::CurveQuoteNotSol,
        ),
        (
            "curve under 115 bytes",
            CurveSpec {
                len: 114,
                ..curve(true, 0)
            },
            false,
            E::CurveNeedsExtend,
        ),
    ];
    for (what, spec, canonical, err) in cases {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(FART);
        let (mint, _) = pool.token();
        // Since M5 `create_market(Auto)` lists a live or migrating curve as its pump venue
        // (and refuses mayhem, non-SOL and short curves): the list-only market here is listed
        // first, then its mint's curve (and pool) appear.
        list_auto(&mut env, &pool);
        plant_curve(&mut env.svm, &mint, curve_bytes(&spec));
        if canonical {
            plant_canonical_pool(&mut env.svm, &mint, &wsol);
        }
        let res = env.propose(None, &pool);
        assert_eq!(custom_code(&res), Some(code(err)), "{what}");
        assert_eq!(
            market_of(&env, &pool).venue_kind(),
            Some(VenueKind::None),
            "{what}"
        );
    }
}

/// M5-SEC-5 (D1, fail closed): a list-only market of a pump coin whose migrated curve's
/// canonical-pool address holds an account that does not read as a valid pool — a changed
/// discriminator (a PumpSwap upgrade), a pool whose fields a shifted layout moved (planted here
/// with a non-WSOL quote), a bool byte other than 0/1 — refuses every Raydium candidate
/// (`PumpVenuePrecedence`): only an address holding nothing is "graduated elsewhere". Pre-fix
/// each was `GraduatedElsewhere` and a Raydium pool was adopted against D1. The same address
/// holding nothing: the proposal proceeds (adopted).
#[test]
fn an_unreadable_canonical_pool_keeps_the_pump_venue() {
    let migrated = CurveSpec {
        complete: true,
        len: 151,
        ..CurveSpec::default()
    };
    type Plant = fn(&mut litesvm::LiteSVM, &Pubkey);
    let cases: [(&str, Plant); 3] = [
        ("another quote", |svm, mint| {
            plant_canonical_pool(svm, mint, &Pubkey::new_unique())
        }),
        ("discriminator", |svm, mint| {
            plant_canonical_pool(svm, mint, &shortwin::constants::WSOL_MINT);
            fork::patch(svm, &pda::canonical_pool(mint), |d| d[0] ^= 0xff);
        }),
        ("a bool byte 2", |svm, mint| {
            plant_canonical_pool(svm, mint, &shortwin::constants::WSOL_MINT);
            fork::patch(svm, &pda::canonical_pool(mint), |d| {
                d[shortwin_venue_state::pump_amm::OFF_IS_CASHBACK_COIN] = 2
            });
        }),
    ];
    for (what, plant) in cases {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(FART);
        let (mint, _) = pool.token();
        list_auto(&mut env, &pool);
        plant_curve(&mut env.svm, &mint, curve_bytes(&migrated));
        plant(&mut env.svm, &mint);
        let res = env.propose(None, &pool);
        assert_eq!(
            custom_code(&res),
            Some(code(E::PumpVenuePrecedence)),
            "{what}"
        );
        assert_eq!(
            market_of(&env, &pool).venue_kind(),
            Some(VenueKind::None),
            "{what}"
        );
    }
    // Nothing at the address: graduated elsewhere, the candidate is adopted.
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    let (mint, _) = pool.token();
    list_auto(&mut env, &pool);
    plant_curve(&mut env.svm, &mint, curve_bytes(&migrated));
    adopt_ok(&mut env, None, &pool);
}

/// Base/quote order (ARCH §2.6 "Verification", rule 4): the reversed Fartcoin pool (WSOL is
/// the pc) made eligible is adopted with the SOL side it records, then — inside the listing
/// window, at the 5 % margin — the main pool (WSOL is the coin) replaces it, and a planted CPMM
/// pool with WSOL as token 1 after that; each adoption flips the SOL-side flag. After the window
/// the market's opens are still sized on the thin first pool's liquidity (`L_slow` only ever
/// fell to `min(√(X·Y)_new, current)`, ARCH §2.6 rule 4): conservative, as the rule intends.
#[test]
fn candidates_record_their_sol_side() {
    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let rev = eligible_reversed(&mut env);
    list_auto(&mut env, &rev);
    let e1 = adopt_ok(&mut env, None, &rev);
    assert_eq!(e1.kind, POOL_KIND_LISTING);
    let m = market_of(&env, &rev);
    assert!(
        !m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
        "WSOL is the pc"
    );
    let seen = m.pool_seen_at;
    env.warp(60);
    let e2 = adopt_ok(&mut env, Some(&rev), &deep);
    assert_eq!(
        (e2.kind, e2.d_inc),
        (POOL_KIND_LISTING_WINDOW, m.pool_d.get())
    );
    let m = market_of(&env, &deep);
    assert!(
        m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
        "WSOL is the coin"
    );
    assert_eq!(m.pool_seen_at, seen, "inside the window: not extended");
    env.warp(60);
    let lam = lambda_of(&env, &deep);
    let cpmm = plant(&mut env, &deep, Plant::Cpmm, false, (11, 10), lam);
    let e3 = adopt_ok(&mut env, Some(&deep), &cpmm);
    assert_eq!(e3.kind, POOL_KIND_LISTING_WINDOW);
    let m = market_of(&env, &cpmm);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert!(
        !m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
        "WSOL is token 1"
    );
    assert_eq!(m.pool_seen_at, seen);
    // `P_ema` carried over from the reversed pool's own price (ARCH §2.6 rule 4: "same asset,
    // arbitraged" — this dumped pool was not): per-second cranks bring it to the new pool's
    // before the open guard `|P_spot/P_ema − 1| ≤ open_ema_dev_bps` lets an open through.
    let (rm, _) = adopted_market(&mut env, cpmm);
    crank_every(&mut env, &rm, 1, 60);
    // `L_ema`, `L_slow` fell to the thin first pool's liquidity at the first adoption and each
    // later one kept `min(√(X·Y)_new, current)`: the slow EMA (τ 1 h) still sizes opens as if
    // on the thin pool, so the cap binds on it, not on the deep pool's reserve.
    let m = market_of(&env, &rm.pool);
    let (x, _) = rm.reserves(&env.svm).unwrap();
    let x0 = shortwin_math::caps::conservative_x0(
        x,
        m.liq_slow.get(),
        m.ema_price.get(),
        Some((m.pool_d.get(), env.config().params.pool_lock_min_bps)),
    )
    .unwrap();
    assert!(x0 < x / 10, "sized on the carried slow liquidity");
    let mut b = env.borrower();
    assert_err(&env.open(&rm, &mut b, x0 / 10).0, E::CapPositionImpact);
}

/// Fartcoin's reversed pool `9GWMrDgH…` with its LP burned to λ = 50 % and topped up to just
/// over the eligibility floor — `pool_min_real_sol` of real SOL and the few lamports the
/// locked-depth floor rounds away (M4-SEC-1 / -3; `pool_select::top_up_pc_to_floor`): M3-SEC-3's
/// thin eligible pool.
pub fn eligible_reversed(env: &mut Env) -> AnyPool {
    let thin = env.load_ray_pool(FART_REV);
    let supply = raydium::supply_for_lambda(raydium::lp_counter(&env.svm, &thin), 5_000);
    raydium::set_mint_supply(&mut env.svm, &raydium::lp_mint(&thin), supply);
    let min = env.config().params.pool_min_real_sol;
    let real = shortwin_litesvm_tests::pool_select::top_up_pc_to_floor(env, &thin, min);
    assert!(real - min < 1_000_000, "{real} lamports of real SOL");
    thin
}

/// M3-SEC-3 (b), inverted, with the margins of rule 1: a thin eligible listing (the reversed
/// pool) is replaced within `pool_list_window_s` by a candidate with ≥ 5 % more locked depth
/// and not by one below that; the listing window keeps its start through a chain of
/// replacements; after the window the 25 % margin applies instead.
#[test]
fn the_listing_window_replaces_a_thin_listing() {
    let mut env = Env::ray_fork();
    let _deep = env.load_ray_pool(FART);
    let thin = eligible_reversed(&mut env);
    ok(env.list_ray(&thin, VENUE_HINT_V4));
    let seen = market_of(&env, &thin).pool_seen_at;
    let d0 = d_of(&env, &thin);
    // A planted pool of the same price and λ: D scales with the reserves.
    let lam = lambda_of(&env, &thin);
    let below = plant(&mut env, &thin, Plant::V4, false, (1_040, 1_000), lam);
    let above = plant(&mut env, &thin, Plant::V4, false, (1_060, 1_000), lam);
    let d_below = d_of(&env, &below);
    let d_above = d_of(&env, &above);
    assert!(d_below * 10_000 < d0 * 10_500 && d_above * 10_000 >= d0 * 10_500);
    env.warp(10);
    assert_err(&env.propose(Some(&thin), &below), E::PoolNotDominant);
    let e = adopt_ok(&mut env, Some(&thin), &above);
    assert_eq!(
        (e.kind, e.d_cand, e.d_inc),
        (POOL_KIND_LISTING_WINDOW, d_above, d0)
    );
    // A chain of proposals: 5 % over the new incumbent each, the window never extended.
    let mut inc = above;
    for i in 0..3u64 {
        env.warp(600);
        let next = plant(&mut env, &inc, Plant::V4, true, (1_100, 1_000), lam);
        adopt_ok(&mut env, Some(&inc), &next);
        assert_eq!(market_of(&env, &next).pool_seen_at, seen, "proposal {i}");
        inc = next;
    }
    let window = i64::from(env.config().params.pool_list_window_s);
    let slot = env.clock().slot;
    env.set_clock(seen + window, slot + 20_000);
    // After the window: 10 % is no longer enough; 25 % is (no position is open).
    let ten = plant(&mut env, &inc, Plant::V4, true, (1_100, 1_000), lam);
    assert_err(&env.propose(Some(&inc), &ten), E::PoolNotDominant);
    let quarter = plant(&mut env, &inc, Plant::V4, true, (1_260, 1_000), lam);
    let e = adopt_ok(&mut env, Some(&inc), &quarter);
    assert_eq!(e.kind, shortwin::events::POOL_KIND_ZERO_POSITION);
    assert_eq!(
        market_of(&env, &quarter).pool_seen_at,
        env.now(),
        "a new window"
    );
}
