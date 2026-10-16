//! Duty 5's Raydium fast path and duty 12's pool monitor (M4; ARCH §2.5 `None`, §2.6, §7.1,
//! §9 M4) on the mainnet fork: the release short.win, the dumped AMM v4 and CPMM programs and
//! pools, and **planted** sibling pools (`shortwin_litesvm_tests::pool_select::{plant_v4,
//! plant_cpmm}`: real pool accounts cloned onto fresh keys, traded by the dumped ELFs). The
//! keeper finds every pool through its own `getProgramAccounts` discovery, ranks them with the
//! program's verifier and sends what the program's `decide` accepts; every keeper transaction
//! runs the invariant hooks (I15 included) after every instruction.
//!
//! - A thin eligible listing (Fartcoin's reversed pool, topped up to the minimum) is replaced by
//!   the main pool in the keeper's first tick, inside `pool_list_window_s`, at the 5 % margin.
//! - A list-only (`Auto`) market of a Raydium token is moved onto its deepest eligible pool (a
//!   thinner planted CPMM sibling and the ineligible reversed pool passed over); a pump coin's
//!   list-only market is left alone (D1).
//! - The incumbent halts with positions open: the keeper opens the collection window on the
//!   deepest sibling at the first armed second (`pool_fail` for 120 s) and executes it at the
//!   first second after the 60 s window; positions keep their terms and exit on the new pool.
//!   An outsider front-running the window with a shallower pool of their own is outbid by the
//!   keeper and the deepest candidate is adopted; a deeper pool created during the window is
//!   found by the fast discovery, proposed and adopted.
//! - A halted pool without positions: the keeper keeps the `pool_fail` chain itself and runs
//!   the fast path to a sibling that no zero-position adoption would take.
//! - Positions open and the incumbent healthy: nothing is proposed, however deep the sibling;
//!   the market is reported pinned (R4B-10) and the sibling is adopted in the tick the last
//!   position closes.
//! - A Raydium upgrade flagged in the latch holds every proposal of a candidate on that program
//!   (nothing is sent) until `revalidate_venue` (M4-SEC-8: the candidate's own program).
//! - M4-SEC-7: three hundred junk CPMM pools for the halted market's mint cost the keeper a
//!   bounded ranking (at most `rank_keys_per_tick` accounts a tick) and at most
//!   `max_candidates` ReadSets a tick afterwards, all after the tick's cranks; the fast path
//!   adopts the same sibling at the same second as without them.
//! - M4V-5: a freshly listed Raydium market without positions is observed every
//!   `listing_interval_s` through its listing window and every second in its last
//!   `EMA_WARMUP_BUCKETS + 2` seconds, so the first open right after the window finds a warmed-up
//!   price state at the pool's spot; a window nobody observed (the keeper down) is warmed up in
//!   the seconds after it, and the first open waits `EmaWarmingUp` until then.
//! - M4R (post-change confirmation): after the fast path the keeper observes the new pool every
//!   second through the warm-up and sends no exit before the gate; underwater positions are
//!   liquidated at adoption + 9 s.

use anchor_lang::prelude::Pubkey;
use shortwin::engine::pool_select::Proposal;
use shortwin::errors::ShortwinError;
use shortwin::events::{
    POOL_KIND_FAST, POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW, POOL_KIND_ZERO_POSITION,
};
use shortwin::state::{
    MARKET_FLAG_GRADUATED_ELSEWHERE, Market, ShortPosition, VenueKind, raydium_cache,
};
use shortwin_keeper::coverage::Due;
use shortwin_keeper::events::{PoolEvent, PoolEventKind};
use shortwin_keeper::pool_monitor::{Action, Verdict};
use shortwin_litesvm_tests::engine::{Borrower, VenueMarket};
use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, V4Fixture};
use shortwin_litesvm_tests::pda;
use shortwin_litesvm_tests::pool_select::{self as ps, plant_cpmm, plant_v4, spec_like};
use shortwin_litesvm_tests::raydium::{
    self, CurveSpec, RayFixture, RayMarket, curve_bytes, fartcoin_curve_bytes, plant_curve,
};
use shortwin_math::observe::ring_medians;

use crate::common::*;
use crate::raydium::{FART, ray_open, ray_setup};

/// Fartcoin's reversed AMM v4 pool `9GWMrDgH…` (WSOL is the pc; thin and unlocked as dumped).
const FART_REV: RayFixture = RayFixture::V4(V4Fixture::FartcoinReversed);

fn code(e: ShortwinError) -> Option<u32> {
    Some(u32::from(e))
}

/// The planted pool kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plant {
    /// AMM v4, cloned from Fartcoin's main pool.
    V4,
    /// CPMM, cloned from USELESS (config 0, 0.25 %).
    Cpmm,
}

/// A sibling of `base` (same token and price): `base`'s reserves scaled by `num/den`, WSOL
/// first or not, locked share `lambda_bps`.
fn plant(
    env: &mut Env,
    base: &AnyPool,
    kind: Plant,
    sol_first: bool,
    (num, den): (u64, u64),
    lambda_bps: u64,
) -> AnyPool {
    let spec = spec_like(&env.svm, base, sol_first, lambda_bps).scaled(num, den);
    match kind {
        Plant::V4 => AnyPool::V4(plant_v4(env, V4Fixture::Fartcoin, spec)),
        Plant::Cpmm => AnyPool::Cpmm(plant_cpmm(env, CpmmFixture::Useless, spec)),
    }
}

/// `λ_bps` of a pool now.
fn lambda_of(env: &Env, pool: &AnyPool) -> u64 {
    let counter = raydium::lp_counter(&env.svm, pool);
    let supply = raydium::mint_supply(&env.svm, &raydium::lp_mint(pool));
    u64::from(shortwin_math::pool_select::lambda_bps(counter, supply))
}

/// `D` of a pool now.
fn d_of(env: &Env, pool: &AnyPool) -> u128 {
    raydium::locked_depth(&env.svm, pool)
}

/// Fartcoin's reversed pool with its LP burned to λ = 50 %, topped up to just over the
/// locked-depth floor (`pool_min_real_sol` of real SOL and the floor's rounding; M4-SEC-1 / -3):
/// the thin eligible listing of M3-SEC-3.
fn eligible_reversed(env: &mut Env) -> AnyPool {
    let thin = env.load_ray_pool(FART_REV);
    let supply = raydium::supply_for_lambda(raydium::lp_counter(&env.svm, &thin), 5_000);
    raydium::set_mint_supply(&mut env.svm, &raydium::lp_mint(&thin), supply);
    let min = env.config().params.pool_min_real_sol;
    shortwin_litesvm_tests::pool_select::top_up_pc_to_floor(env, &thin, min);
    thin
}

/// Halts `pool` (AMM v4 status 2 = disabled; CPMM swap bit).
fn halt(env: &mut Env, pool: &AnyPool) {
    match pool {
        AnyPool::V4(p) => fork::set_v4_status(&mut env.svm, p, 2),
        AnyPool::Cpmm(p) => fork::set_cpmm_status(&mut env.svm, p, 4),
    }
}

fn market(h: &Harness, key: &Pubkey) -> Market {
    h.with_env(|env| env.market(key))
}

fn positions(h: &Harness, rm: &RayMarket, open: &[(Borrower, u64)]) -> Vec<ShortPosition> {
    h.with_env(|env| {
        open.iter()
            .map(|(b, n)| env.position(rm, &b.key(), *n))
            .collect()
    })
}

/// The one pool event of `r` of kind `what`, if any.
fn event(r: &TickReport, what: PoolEventKind) -> Option<PoolEvent> {
    let v: Vec<PoolEvent> = r
        .pool_events
        .iter()
        .filter(|e| e.what == what)
        .copied()
        .collect();
    assert!(v.len() <= 1, "{v:?}");
    v.first().copied()
}

/// The pool-selection alerts of `r`.
fn pool_alerts(r: &TickReport) -> Vec<Alert> {
    r.alerts
        .iter()
        .filter(|a| {
            matches!(
                a,
                Alert::PoolSwitched { .. }
                    | Alert::PoolWindow { .. }
                    | Alert::PoolSwitchCancelled { .. }
                    | Alert::PoolPinned { .. }
                    | Alert::PoolSelectRefused { .. }
            )
        })
        .cloned()
        .collect()
}

/// ARCH §9 M4 (M3-SEC-3 b, inverted): a thin eligible listing is replaced within
/// `pool_list_window_s`. Fartcoin's reversed pool, made barely eligible, is listed with
/// `create_market(RaydiumV4)`; the keeper's first tick discovers both AMM v4 pools of the mint
/// through `getProgramAccounts`, leaves the incumbent out, and proposes the main pool, which
/// the program adopts as a listing-window replacement (5 % margin) without extending the
/// window. Nothing more is proposed after; once the window has run, the market trades on the
/// main pool and the keeper covers it there.
#[test]
fn a_thin_listing_is_replaced_within_the_listing_window() {
    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let thin = eligible_reversed(&mut env);
    ok(env.list_ray(&thin, FART_REV.hint()));
    let (mint, _) = thin.token();
    let key = pda::market(&mint).0;
    let listed = env.market(&key);
    let (seen, d_thin) = (listed.pool_seen_at, listed.pool_d.get());
    let d_deep = d_of(&env, &deep);
    assert!(d_deep > d_thin * 105 / 100);
    let window = i64::from(env.config().params.pool_list_window_s);
    let mut h = Harness::new(env, KeeperConfig::default());

    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let plan = &r.pool_plans[&key];
    assert_eq!(plan.best, Some((ps::pool_key(&deep), d_deep)));
    let track = h.keeper.pool_track(&key).expect("tracked");
    let found: Vec<Pubkey> = track.candidates.iter().map(|c| c.pool).collect();
    assert!(
        found.contains(&ps::pool_key(&deep)) && found.contains(&ps::pool_key(&thin)),
        "both AMM v4 pools of the mint discovered: {found:?}"
    );
    assert_eq!(r.pool_actions.len(), 1, "{:?}", r.pool_actions);
    let a = &r.pool_actions[0];
    assert!(
        matches!(
            a.action,
            Action::Propose {
                expect: Proposal::Adopt(k),
                ..
            } if k == POOL_KIND_LISTING_WINDOW
        ),
        "{a:?}"
    );
    assert!(a.result.landed(), "{a:?}");
    let ev = event(&r, PoolEventKind::Switched).expect("PoolSwitched");
    assert_eq!(
        (ev.kind, ev.candidate, ev.d_cand, ev.d_inc, ev.window_ends),
        (
            POOL_KIND_LISTING_WINDOW,
            ps::pool_key(&deep),
            d_deep,
            d_thin,
            seen + window
        )
    );
    let m = market(&h, &key);
    assert_eq!(m.pool_cache[raydium_cache::RAY_POOL], ps::pool_key(&deep));
    assert_eq!(
        m.pool_seen_at, seen,
        "a replacement inside the window keeps its end"
    );
    assert!(h.now() < seen + window, "within pool_list_window_s");
    assert!(
        pool_alerts(&r).contains(&Alert::PoolSwitched {
            market: key,
            from: ps::pool_key(&thin),
            to: ps::pool_key(&deep),
            venue: VenueKind::RaydiumV4,
            kind: Some(POOL_KIND_LISTING_WINDOW),
            d_inc: d_thin,
            d_cand: d_deep,
            by_keeper: true,
        }),
        "{:?}",
        r.alerts
    );
    assert_eq!(h.keeper.metrics.markets[&key].pool_proposals, 1);

    // Nothing more: the thin pool is no candidate against the main one; the keeper's own
    // adoption is not reported again as someone else's.
    for s in 0..40 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(
            r.pool_actions.is_empty(),
            "second {s}: {:?}",
            r.pool_actions
        );
        assert!(pool_alerts(&r).is_empty(), "second {s}: {:?}", r.alerts);
    }

    // After the window (no position: the 25 % margin applies) the thin pool is still no
    // candidate. (Opens are sized on the carried slow liquidity of the thin listing until
    // `L_slow` converges, ARCH §2.6 rule 4 — the program's conservative choice, not the
    // keeper's.)
    h.idle(window);
    for s in 0..5 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(
            r.pool_actions.is_empty(),
            "second {s}: {:?}",
            r.pool_actions
        );
    }
    assert_eq!(
        market(&h, &key).pool_cache[raydium_cache::RAY_POOL],
        ps::pool_key(&deep)
    );
}

/// ARCH §9 M4 (M3-SEC-3 a, inverted) and §2.5 `None`: an `Auto`-listed (list-only) market of a
/// Raydium token is moved onto its pool by the keeper's `propose_pool`. Fartcoin's real pump
/// curve (it graduated to Raydium before PumpSwap) is planted, so D1 runs on the real bytes and
/// the market records `graduated_elsewhere`. The keeper evaluates the mint's three pools with
/// the program's verifier — the main pool, a planted CPMM sibling at half its depth, and the
/// reversed pool (refused: too thin) — and proposes the deepest after the detection set.
#[test]
fn a_list_only_market_is_moved_onto_its_deepest_eligible_pool() {
    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let rev = env.load_ray_pool(FART_REV);
    let (mint, program) = deep.token();
    plant_curve(&mut env.svm, &mint, fartcoin_curve_bytes());
    let lam = lambda_of(&env, &deep);
    let half = plant(&mut env, &deep, Plant::Cpmm, false, (50, 100), lam);
    ok(env.list_auto(&mint, program));
    let key = pda::market(&mint).0;
    assert_eq!(env.market(&key).venue_kind(), Some(VenueKind::None));
    let d_deep = d_of(&env, &deep);
    let mut h = Harness::new(env, KeeperConfig::default());

    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    // Since M5 a list-only market is covered with its detection set as ReadSet (a curve that
    // graduated elsewhere with positions open needs its S3 chain); without positions nothing is
    // due.
    assert!(!r.skipped.iter().any(|(k, _)| *k == key));
    assert!(
        !r.cranked.contains(&key),
        "no coverage duty without positions"
    );
    let track = h.keeper.pool_track(&key).expect("tracked");
    let verdict = |p: &AnyPool| {
        track
            .evaluated
            .iter()
            .find(|e| e.pool == ps::pool_key(p))
            .map(|e| e.verdict.clone())
            .unwrap_or_else(|| panic!("{} not evaluated", ps::pool_key(p)))
    };
    assert!(matches!(verdict(&deep), Verdict::Eligible { d, .. } if d == d_deep));
    assert!(matches!(verdict(&half), Verdict::Eligible { d, .. } if d < d_deep));
    assert!(
        matches!(verdict(&rev), Verdict::Refused { code: c, .. } if c == code(ShortwinError::PoolTooThin)),
        "{:?}",
        verdict(&rev)
    );
    assert_eq!(r.pool_actions.len(), 1, "{:?}", r.pool_actions);
    assert!(r.pool_actions[0].result.landed(), "{:?}", r.pool_actions);
    let ev = event(&r, PoolEventKind::Switched).expect("PoolSwitched");
    assert_eq!(
        (ev.kind, ev.candidate, ev.d_cand, ev.d_inc),
        (POOL_KIND_LISTING, ps::pool_key(&deep), d_deep, 0)
    );
    let m = market(&h, &key);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumV4));
    assert_eq!(m.pool_cache[raydium_cache::RAY_POOL], ps::pool_key(&deep));
    assert!(
        m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE),
        "D1 on the real curve"
    );
    assert_eq!(m.pool_seen_at, h.now());

    // From the next tick on the market is a Raydium market the duties cover; nothing more to
    // propose (the CPMM sibling is thinner).
    for _ in 0..5 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!r.skipped.iter().any(|(k, _)| *k == key));
        assert!(r.pool_actions.is_empty(), "{:?}", r.pool_actions);
    }
}

/// D1 (ARCH §2.5 "Precedence"): a list-only market of a coin whose pump curve is live keeps its
/// pump venue — the keeper runs the program's `classify_pump` on the detection set and never
/// proposes the (eligible) Raydium pool; since M5 it also cranks the market onto the curve in the
/// same tick (`crank_price`'s `None → PumpCurve` re-detection, ARCH §2.5 `None` row), after which
/// the market is no longer list-only and the pool monitor leaves it.
#[test]
fn a_pump_coin_keeps_its_pump_venue() {
    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let (mint, program) = deep.token();
    // Since M5 `create_market(Auto)` lists a live curve as `PumpCurve` (with the pump init
    // set): the list-only market here is listed first, then its mint's curve appears.
    ok(env.list_auto(&mint, program));
    plant_curve(
        &mut env.svm,
        &mint,
        curve_bytes(&CurveSpec {
            virtual_token_reserves: 1_000_000_000_000,
            virtual_quote_reserves: 30_000_000_000,
            real_token_reserves: 500_000_000_000,
            real_quote_reserves: 1,
            complete: false,
            len: 151,
            ..CurveSpec::default()
        }),
    );
    let key = pda::market(&mint).0;
    let mut h = Harness::new(env, KeeperConfig::default());
    // The first tick: the re-detection crank, and the pool monitor's evaluation on the list-only
    // view of that tick, which D1 blocks.
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.pool_actions.is_empty(), "{:?}", r.pool_actions);
    assert!(r.cranked.contains(&key), "the re-detection crank");
    let p = r.pool_plans.get(&key).expect("evaluated as list-only");
    assert_eq!(p.best.map(|b| b.0), Some(ps::pool_key(&deep)));
    assert_eq!(p.blocked, code(ShortwinError::PumpVenuePrecedence));
    assert!(
        h.keeper
            .pool_track(&key)
            .is_some_and(|t| t.last_eval.is_some())
    );
    assert_eq!(market(&h, &key).venue_kind(), Some(VenueKind::PumpCurve));
    for s in 1..40 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            r.pool_actions.is_empty(),
            "second {s}: {:?}",
            r.pool_actions
        );
        assert!(!r.pool_plans.contains_key(&key), "no longer list-only");
    }
    assert_eq!(market(&h, &key).venue_kind(), Some(VenueKind::PumpCurve));
}

/// A Fartcoin market with two open positions, and two planted siblings: an AMM v4 pool at 40 %
/// and a CPMM pool at 60 % of its locked depth (same price and λ).
fn with_positions_and_siblings() -> (Harness, RayMarket, Vec<(Borrower, u64)>, AnyPool, AnyPool) {
    let (h, ms) = ray_setup(&[FART], KeeperConfig::default());
    let (rm, q) = ms.into_iter().next().expect("market");
    let open = vec![ray_open(&h, &rm, q), ray_open(&h, &rm, q / 3)];
    let (first, best) = h.with_env(|env| {
        let lam = lambda_of(env, &rm.pool);
        (
            plant(env, &rm.pool, Plant::V4, true, (40, 100), lam),
            plant(env, &rm.pool, Plant::Cpmm, true, (60, 100), lam),
        )
    });
    (h, rm, open, first, best)
}

/// Runs the keeper one second at a time after the incumbent halted until an adoption (at most
/// `limit` seconds); `before_tick(h, since)` runs after each warp and before the tick, with the
/// `pool_fail` chain's start once known. Returns every report, the chain's start and the market
/// just before the adopting tick.
fn run_until_switched(
    h: &mut Harness,
    key: &Pubkey,
    limit: u32,
    mut before_tick: impl FnMut(&mut Harness, Option<i64>),
) -> (Vec<(i64, TickReport)>, i64, Market) {
    let mut out = Vec::new();
    let mut since: Option<i64> = None;
    let mut last = market(h, key);
    for s in 0..limit {
        h.idle(1);
        before_tick(h, since);
        let r = h.tick().clone();
        let now = h.now();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        for a in &r.pool_actions {
            assert!(a.result.landed(), "second {s}: {a:?}");
        }
        let switched = event(&r, PoolEventKind::Switched).is_some();
        out.push((now, r));
        if switched {
            return (out, since.expect("the chain started"), last);
        }
        let m = market(h, key);
        if m.pool_fail_since != 0 {
            assert_eq!(
                *since.get_or_insert(m.pool_fail_since),
                m.pool_fail_since,
                "the pool_fail chain stays continuous"
            );
        }
        last = m;
    }
    panic!("no adoption within {limit} s");
}

/// T-RAY-SWITCH / T-VENUE-SWITCH through the keeper (ARCH §7.1 duty 5's fast path, §9 M4 "a
/// fast path with open positions keeps locked terms and EMA continuity"): the incumbent AMM v4
/// pool halts with two positions open. The keeper starts the S3 and `pool_fail` chains at its
/// next tick and keeps them continuous (duty 5), proposes nothing before `pool_fast_route_s`,
/// opens the collection window on the **deepest** sibling (the 60 % CPMM pool, not the 40 % AMM
/// v4 one) at the first armed second, and calls `execute_pool_switch` at the first second after
/// the 60 s window. The positions are byte for byte what they were (A26), `P_ema` carries over
/// at the adoption, every ring entry's liquidity is the new pool's anchor and the state is marked
/// (M4R); the keeper observes the new pool every second through the post-change warm-up (duty 5:
/// exits wait for it), after which `L_ema` is the pool's depth, the guard clear and the settlement
/// box ≤ 1 % wide in `L`; both positions close there.
#[test]
fn a_halted_incumbent_moves_open_positions_to_the_deepest_sibling() {
    let (mut h, rm, open, first, best) = with_positions_and_siblings();
    let key = rm.market();
    h.seconds(3);
    assert!(h.reports.iter().all(|r| r.pool_actions.is_empty()));
    let d0 = market(&h, &key).pool_d.get();
    let d_best = h.with_env(|env| d_of(env, &best));
    let pos_before = positions(&h, &rm, &open);
    h.with_env(|env| halt(env, &rm.pool));
    let halted_at = h.now();

    let (reports, since, before) = run_until_switched(&mut h, &key, 400, |_, _| {});
    assert_eq!(
        since,
        halted_at + 1,
        "the chain starts at the keeper's next tick"
    );
    let fast_route = i64::from(h.with_env(|env| env.config().params.pool_fast_route_s));
    let fast_window = i64::from(h.with_env(|env| env.config().params.pool_fast_window_s));
    let actions: Vec<(i64, Action)> = reports
        .iter()
        .flat_map(|(t, r)| r.pool_actions.iter().map(|a| (*t, a.action.clone())))
        .collect();
    assert_eq!(actions.len(), 2, "{actions:?}");
    // The window: opened on the deepest sibling at the first armed second.
    let (t_open, open_action) = &actions[0];
    assert_eq!(*t_open, since + fast_route);
    assert!(
        matches!(open_action, Action::Propose { pool, expect: Proposal::OpenWindow, d, .. }
            if *pool == ps::pool_key(&best) && *d == d_best),
        "{open_action:?}"
    );
    let (_, r_open) = reports.iter().find(|(t, _)| t == t_open).unwrap();
    let proposed = event(r_open, PoolEventKind::Proposed).expect("PoolProposed");
    assert_eq!(
        (
            proposed.kind,
            proposed.candidate,
            proposed.d_inc,
            proposed.window_ends
        ),
        (
            POOL_KIND_FAST,
            ps::pool_key(&best),
            d0,
            since + fast_route + fast_window
        )
    );
    // The execution at the first second after the window.
    let (t_exec, exec_action) = &actions[1];
    assert_eq!(*t_exec, since + fast_route + fast_window);
    assert!(
        matches!(exec_action, Action::Execute { kind: VenueKind::RaydiumCpmm, candidate }
            if candidate[0] == ps::pool_key(&best)),
        "{exec_action:?}"
    );
    let (_, r_exec) = reports.last().unwrap();
    let switched = event(r_exec, PoolEventKind::Switched).unwrap();
    assert_eq!(
        (switched.kind, switched.candidate, switched.d_inc),
        (POOL_KIND_FAST, ps::pool_key(&best), d0)
    );
    assert_ne!(ps::pool_key(&first), switched.candidate);

    // The market after the switch: locked terms, EMA continuity, the ring rewrite.
    let m = market(&h, &key);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert_eq!(m.pool_cache[raydium_cache::RAY_POOL], ps::pool_key(&best));
    assert_eq!(positions(&h, &rm, &open), pos_before, "locked terms (A26)");
    assert_eq!(
        m.ema_price.get(),
        before.ema_price.get(),
        "P_ema carries over"
    );
    let l = m.ema_liq.get();
    for e in m.ring.iter().take(usize::from(m.ring_count)) {
        assert_eq!((e.l.get(), e.llo.get(), e.lhi.get()), (l, l, l));
    }
    assert_eq!((m.pool_fail_since, m.no_route_since, m.fp_t0), (0, 0, 0));
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "marked (M4R)");

    // The keeper observes the new pool every second through the post-change warm-up (8
    // integrations, the last one by the next instruction's roll), then at its usual cadence.
    let fv_dev = u128::from(pos_before[0].fv_dev_bps);
    let mut warm_seconds = 0;
    for s in 0..40 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(
            r.pool_actions.is_empty(),
            "second {s}: {:?}",
            r.pool_actions
        );
        let m = market(&h, &key);
        if s < 8 {
            assert!(
                r.cranked.contains(&key),
                "second {s}: observed during the warm-up"
            );
            warm_seconds += 1;
        }
        if m.ema_warm == 0 {
            let (_, _, l_lo, l_hi) = ring_medians(&m.ring_math()).unwrap();
            assert!(
                (l_hi - l_lo) * 10_000 <= fv_dev * l_lo,
                "second {s}: box [{l_lo}, {l_hi}]"
            );
        }
    }
    let rm2 = h.with_env(|env| rm.on_pool(&env.svm, best));
    h.with_env(|env| env.crank(&rm2));
    let m = market(&h, &key);
    let depth = h.with_env(|env| {
        let (x, y) = best
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .expect("reserves");
        (u128::from(x) * u128::from(y)).isqrt()
    });
    assert_eq!((m.ema_warm, m.ema_guard), (0, 0), "confirmed");
    assert!(
        m.ema_liq.get().abs_diff(depth) * 1_000 <= depth,
        "L_ema {} vs the pool's depth {depth}",
        m.ema_liq.get()
    );
    let (_, _, l_lo, l_hi) = ring_medians(&m.ring_math()).unwrap();
    assert!(
        (l_hi - l_lo) * 10_000 <= fv_dev * l_lo,
        "box [{l_lo}, {l_hi}]"
    );
    println!("M4R keeper: {warm_seconds} per-second observations through the warm-up");
    h.with_env(|env| {
        for (b, n) in &open {
            ok(env.close(&rm2, b, *n));
        }
    });
    assert_eq!(market(&h, &key).open_positions, 0);
}

/// M4R through the keeper (★T-SWITCH-CONFIRM, ARCH §7.1 duty 5): the incumbent halts with two
/// positions open while the token rises 2.5× on the deepest sibling, so both are underwater on
/// it. The keeper takes the fast path, observes the new pool every second through the
/// post-change warm-up without sending (or simulating into a failure) any exit — the program
/// refuses `EmaWarmingUp` then — and liquidates both at adoption + 9 s, the first second its
/// projection of the prologue shows the warm-up over.
#[test]
fn underwater_positions_are_liquidated_after_the_warm_up() {
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;

    let (mut h, rm, open, _first, best) = with_positions_and_siblings();
    let key = rm.market();
    h.seconds(3);
    h.with_env(|env| {
        halt(env, &rm.pool);
        let (mint, program) = best.token();
        let (_, y) = best
            .io_reserves(&env.svm, Direction::Sell)
            .expect("reserves");
        let a = (y as f64 * (2.5f64.sqrt() - 1.0) / 0.99) as u64;
        let whale = fork::trader(env, &mint, program, a + 1, 0);
        ok(fork::swap(
            env,
            &best,
            &whale,
            Direction::Buy,
            Swap::ExactIn(a, 0),
        ));
    });
    let _ = run_until_switched(&mut h, &key, 400, |_, _| {});
    let adopted_at = h.now();
    let mut landed = Vec::new();
    for s in 1..=30i64 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
        let m = market(&h, &key);
        if s < 9 {
            assert!(
                r.exits.is_empty(),
                "+{s} s: an exit during the warm-up: {:?}",
                r.exits
            );
            assert!(
                r.cranked.contains(&key),
                "+{s} s: observed during the warm-up"
            );
            assert!(m.ema_warm > 0, "+{s} s");
        }
        for e in r.landed(ExitKind::Liquidate) {
            landed.push((h.now() - adopted_at, e.position));
        }
        if landed.len() == open.len() {
            break;
        }
    }
    println!("M4R keeper: liquidations after the adoption (s, position): {landed:?}");
    assert_eq!(landed.len(), open.len(), "both positions liquidated");
    assert!(landed.iter().all(|(s, _)| *s == 9), "{landed:?}");
}

/// V2-7 through the keeper: an outsider opens the collection window at the first armed second
/// with a shallower pool of their own (the 40 % sibling); the keeper, in the same second, sees
/// the window, reports it, outbids it with the deepest candidate (`ImproveWindow`) without
/// extending it, and executes it at the window's end: the deepest pool seen is adopted.
#[test]
fn a_front_run_window_is_outbid_and_the_deepest_candidate_adopted() {
    let (mut h, rm, _open, first, best) = with_positions_and_siblings();
    let key = rm.market();
    h.seconds(3);
    h.with_env(|env| halt(env, &rm.pool));
    let fast_route = i64::from(h.with_env(|env| env.config().params.pool_fast_route_s));
    let fast_window = i64::from(h.with_env(|env| env.config().params.pool_fast_window_s));
    let mut front_run_at = None;
    let (reports, since, _) = run_until_switched(&mut h, &key, 400, |h, since| {
        if let Some(since) = since
            && h.now() == since + fast_route
        {
            front_run_at = Some(h.now());
            let (incumbent, own) = (rm.pool, first);
            h.with_env(|env| ok(env.propose(Some(&incumbent), &own)));
        }
    });
    let t0 = front_run_at.expect("the outsider proposed");
    assert_eq!(t0, since + fast_route);
    let (_, r0) = reports.iter().find(|(t, _)| *t == t0).unwrap();
    assert!(
        pool_alerts(r0).iter().any(|a| matches!(a,
            Alert::PoolWindow { candidate, by_keeper: false, .. } if *candidate == ps::pool_key(&first))),
        "the outsider's window is reported: {:?}",
        r0.alerts
    );
    let improve = &r0.pool_actions;
    assert_eq!(improve.len(), 1, "{improve:?}");
    assert!(
        matches!(&improve[0].action, Action::Propose { pool, expect: Proposal::ImproveWindow, .. }
            if *pool == ps::pool_key(&best)),
        "{improve:?}"
    );
    let p = event(r0, PoolEventKind::Proposed).expect("PoolProposed");
    assert_eq!(
        p.window_ends,
        t0 + fast_window,
        "an improvement keeps the window's end"
    );
    let (t_exec, r_exec) = reports.last().unwrap();
    assert_eq!(*t_exec, t0 + fast_window);
    let s = event(r_exec, PoolEventKind::Switched).unwrap();
    assert_eq!((s.kind, s.candidate), (POOL_KIND_FAST, ps::pool_key(&best)));
    assert_eq!(market(&h, &key).venue_kind(), Some(VenueKind::RaydiumCpmm));
}

/// "Collect candidates in the window, execute the best" (ARCH §2.6 rule 3, §7.1 duty 5): the
/// keeper opens the window on the deepest sibling it knows (60 %); a deeper pool (80 %) created
/// 15 s into the window is found by the fast discovery (every 10 s while the incumbent fails),
/// proposed as an improvement, and adopted when the window closes.
#[test]
fn a_deeper_pool_created_during_the_window_is_collected() {
    let (mut h, rm, _open, _first, best) = with_positions_and_siblings();
    let key = rm.market();
    h.seconds(3);
    h.with_env(|env| halt(env, &rm.pool));
    let fast_route = i64::from(h.with_env(|env| env.config().params.pool_fast_route_s));
    let fast_window = i64::from(h.with_env(|env| env.config().params.pool_fast_window_s));
    let fast_discovery = h.keeper.config().pool_select.fast_discovery_interval_s;
    let mut deeper: Option<(i64, AnyPool)> = None;
    let (reports, since, _) = run_until_switched(&mut h, &key, 400, |h, since| {
        if let Some(since) = since
            && h.now() == since + fast_route + 15
        {
            let inc = rm.pool;
            let pool = h.with_env(|env| {
                let lam = lambda_of(env, &inc);
                plant(env, &inc, Plant::V4, false, (80, 100), lam)
            });
            deeper = Some((h.now(), pool));
        }
    });
    let (planted_at, deeper) = deeper.expect("planted");
    let actions: Vec<(i64, Action)> = reports
        .iter()
        .flat_map(|(t, r)| r.pool_actions.iter().map(|a| (*t, a.action.clone())))
        .collect();
    assert_eq!(actions.len(), 3, "{actions:?}");
    assert!(
        matches!(&actions[0], (t, Action::Propose { pool, expect: Proposal::OpenWindow, .. })
            if *t == since + fast_route && *pool == ps::pool_key(&best)),
        "{actions:?}"
    );
    let (t_improve, improve) = &actions[1];
    assert!(
        matches!(improve, Action::Propose { pool, expect: Proposal::ImproveWindow, .. }
            if *pool == ps::pool_key(&deeper)),
        "{improve:?}"
    );
    assert!(
        *t_improve >= planted_at && *t_improve <= planted_at + fast_discovery,
        "found by the fast discovery: planted {planted_at}, proposed {t_improve}"
    );
    assert!(
        matches!(&actions[2], (t, Action::Execute { .. }) if *t == since + fast_route + fast_window)
    );
    let (_, r) = reports.last().unwrap();
    let s = event(r, PoolEventKind::Switched).unwrap();
    assert_eq!(
        (s.kind, s.candidate),
        (POOL_KIND_FAST, ps::pool_key(&deeper))
    );
    assert_eq!(market(&h, &key).venue_kind(), Some(VenueKind::RaydiumV4));
}

/// A halted pool on a market **without** positions, with only a thinner sibling (50 %, which no
/// zero-position adoption takes): duty 5 does not observe a market without positions, so the
/// keeper keeps the `pool_fail` chain itself (an observation at once, then every 30 s) and
/// takes the fast path — the window at the first armed second, the execution at the first
/// second after it.
#[test]
fn a_halted_pool_without_positions_takes_the_fast_path() {
    let (mut h, ms) = ray_setup(&[FART], KeeperConfig::default());
    let (rm, _) = ms.into_iter().next().expect("market");
    let key = rm.market();
    let sibling = h.with_env(|env| {
        let lam = lambda_of(env, &rm.pool);
        plant(env, &rm.pool, Plant::V4, false, (50, 100), lam)
    });
    h.seconds(3);
    for r in &h.reports {
        assert!(r.pool_actions.is_empty(), "{:?}", r.pool_actions);
        if let Some(p) = r.pool_plans.get(&key) {
            assert_eq!(p.blocked, code(ShortwinError::PoolNotDominant));
            assert!(!p.keep_chain);
        }
    }
    h.with_env(|env| halt(env, &rm.pool));
    let halted_at = h.now();
    let (reports, since, _) = run_until_switched(&mut h, &key, 400, |_, _| {});
    assert_eq!(since, halted_at + 1, "the keeper starts the chain at once");
    let mut last_crank = since;
    for (t, r) in &reports {
        if r.cranked.contains(&key) {
            assert!(t - last_crank <= 30, "crank gap {} s", t - last_crank);
            last_crank = *t;
        }
    }
    let fast_route = i64::from(h.with_env(|env| env.config().params.pool_fast_route_s));
    let fast_window = i64::from(h.with_env(|env| env.config().params.pool_fast_window_s));
    let times: Vec<i64> = reports
        .iter()
        .filter(|(_, r)| !r.pool_actions.is_empty())
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(
        times,
        vec![since + fast_route, since + fast_route + fast_window]
    );
    let s = event(&reports.last().unwrap().1, PoolEventKind::Switched).unwrap();
    assert_eq!(
        (s.kind, s.candidate),
        (POOL_KIND_FAST, ps::pool_key(&sibling))
    );
}

/// **FIT-3 through the keeper, a disclosed residual** (ECON §6.1, §12.12; ARCH §7.1 duty 5; the
/// owner-accepted fallback). `max_q_hwm` falls only when `open_positions` returns to 0, so a
/// position opened at the incumbent's size cap and closed a second later leaves it above what a
/// thinner candidate lets a position be, and "switch only if all fit" refuses the candidate for
/// everyone while the other positions stay open. A planted ≈ 1 % Fartcoin AMM v4 incumbent (the
/// real pool halted) with two honest positions that fit a 40 % CPMM sibling; the attacker opens the
/// largest position the incumbent takes and closes it (the mark pinned above the sibling's cap).
/// The incumbent halts 900 s before the honest positions' deadline: the keeper keeps the chains,
/// its plan for the sibling is blocked `CarriedPositionsTooLarge` at every evaluation, it proposes
/// nothing and no window opens; it settles each position S3 at fair value (the lenders get FV in
/// full) at its due second `max(deadline, no_route_since + s3_wait_s, graduated_at +
/// grad_grace_s)` — here the deadline.
#[test]
fn a_mark_left_by_a_closed_position_blocks_the_fast_path_and_the_positions_settle_in_s3() {
    use shortwin::events::SolSettle;
    use shortwin_keeper::events;
    use shortwin_math::fees::Direction;
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let lam = lambda_of(&env, &base);
    let inc = plant(&mut env, &base, Plant::V4, true, (1, 100), lam);
    ok(env.list_ray(&inc, raydium::VENUE_HINT_RAYDIUM_V4));
    let (x, _) = inc
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let rm = env.ray_market_listed(raydium::RaySpec::new(FART, x / 5), inc);
    let key = rm.market();
    env.crank(&rm);
    for _ in 0..5 {
        env.warp(1);
        env.crank(&rm);
    }
    let cand = plant(&mut env, &inc, Plant::Cpmm, true, (40, 100), lam);
    halt(&mut env, &base);
    let c = ps::carried_caps(&env, &cand).0;
    let honest = c / 10 * 6;
    let mut open = Vec::new();
    for _ in 0..2 {
        let mut b = env.borrower();
        let (res, nonce) = env.open(&rm, &mut b, honest);
        ok(res);
        open.push((b, nonce));
    }
    env.warp(1);
    env.crank(&rm);
    // The attacker: the largest position the incumbent takes, closed a second later.
    let mut a = env.borrower();
    let mut q = x / 100;
    let nonce = loop {
        let (res, nonce) = env.open(&rm, &mut a, q);
        if res.is_ok() {
            break nonce;
        }
        q = q / 100 * 97;
    };
    env.warp(1);
    env.crank(&rm);
    ok(env.close(&rm, &a, nonce));
    let m = env.market(&key);
    assert_eq!((m.open_positions, m.max_q_hwm), (2, q));
    assert!(
        q > c,
        "the mark ({q}) is above the sibling's per-position cap {c}"
    );
    let pos: Vec<ShortPosition> = open
        .iter()
        .map(|(b, n)| env.position(&rm, &b.key(), *n))
        .collect();
    let deadline = pos.iter().map(|p| p.deadline).min().expect("positions");
    let mut h = Harness::new(env, KeeperConfig::default());
    let wait = deadline - 900 - h.now();
    h.idle(wait);
    h.seconds(3);
    h.with_env(|env| halt(env, &inc));
    let halted_at = h.now();
    let too_large = code(ShortwinError::CarriedPositionsTooLarge);
    let keys: Vec<Pubkey> = open
        .iter()
        .map(|(b, n)| pda::position(&key, &b.key(), *n).0)
        .collect();
    let mut blocked = 0u32;
    let mut settled: Vec<Option<(i64, i64, SolSettle)>> = keys.iter().map(|_| None).collect();
    for s in 1..=1_200i64 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
        let m = market(&h, &key);
        for (i, k) in keys.iter().enumerate() {
            let s3 = r
                .exits
                .iter()
                .filter(|e| e.position == *k && e.kind == ExitKind::SolSettle)
                .find_map(|e| e.result.landed())
                .and_then(|out| events::of::<SolSettle>(out).into_iter().next());
            if let Some(e) = s3 {
                settled[i] = Some((s, h.now(), e));
            }
        }
        if settled.iter().all(Option::is_some) {
            break;
        }
        // Until every honest position is settled, nothing is proposed and no window opens.
        assert!(r.pool_actions.is_empty(), "+{s} s: {:?}", r.pool_actions);
        assert_eq!(m.fp_t0, 0, "+{s} s: no window");
        if r.pool_plans
            .get(&key)
            .is_some_and(|p| p.blocked == too_large)
        {
            blocked += 1;
        }
    }
    let m = market(&h, &key);
    let p_ema = m.ema_price.get();
    for (i, (st, p)) in settled.iter().zip(&pos).enumerate() {
        let (s, at, e) = st
            .as_ref()
            .expect("an S3 settlement of every honest position");
        assert!(m.no_route_since >= halted_at, "the S3 chain since the halt");
        let due = p
            .deadline
            .max(m.no_route_since + i64::from(p.s3_wait_s))
            .max(m.graduated_at + i64::from(p.grad_grace_s));
        let owed = u128::from(p.q) * p_ema / shortwin_math::num::WAD;
        println!(
            "FIT-3 keeper residual, position {i}: attacker q {q} ({:.2} × the sibling's cap {c}); \
             {blocked} plans blocked CarriedPositionsTooLarge, no proposal; S{} at +{s} s after \
             the halt ({} s after the due second; deadline +{} s): lenders {:.4} SOL (FV {:.4}; q \
             at P_ema {:.4}), borrower {:.4} SOL",
            q as f64 / c as f64,
            e.condition,
            at - due,
            p.deadline - halted_at,
            e.lender_amount as f64 / 1e9,
            e.fv as f64 / 1e9,
            owed as f64 / 1e9,
            e.borrower_amount as f64 / 1e9
        );
        assert_eq!(e.condition, 3, "S3");
        assert!(*at >= p.deadline, "not before the deadline");
        assert!(*at - due <= 2, "the keeper settles at the due second");
        assert!(
            e.fv > 0 && e.lender_amount == e.fv,
            "the lenders get FV in full"
        );
        assert!(u128::from(e.lender_amount) >= owed, "at least q at P_ema");
        assert!(e.borrower_amount > 0);
    }
    assert!(blocked > 0, "the keeper's plan reports the refusal");
}

/// ARCH §9 M4 "adoption refused while positions are open" and §7.1 duty 12: with a position open
/// and a healthy incumbent the keeper proposes nothing, however deep the sibling (twice the
/// locked depth); the market is reported pinned once (R4B-10, after `pinned_alert_s`). The
/// position closes, and the keeper adopts the sibling in its very next tick, in the same second
/// (a zero-position adoption at the 25 % margin).
#[test]
fn nothing_is_proposed_while_positions_are_open_and_the_incumbent_is_healthy() {
    let mut cfg = KeeperConfig::default();
    cfg.pool_select.pinned_alert_s = 120;
    cfg.pool_select.eval_interval_s = 10;
    let (mut h, ms) = ray_setup(&[FART], cfg);
    let (rm, q) = ms.into_iter().next().expect("market");
    let key = rm.market();
    let (b, n) = ray_open(&h, &rm, q);
    let deep = h.with_env(|env| {
        let lam = lambda_of(env, &rm.pool);
        plant(env, &rm.pool, Plant::Cpmm, true, (2, 1), lam)
    });
    let d_deep = h.with_env(|env| d_of(env, &deep));
    let start = h.now();
    let mut pinned = Vec::new();
    let mut evaluated = 0;
    for s in 0..200 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(
            r.pool_actions.is_empty(),
            "second {s}: {:?}",
            r.pool_actions
        );
        // `pool_d` as the program refreshes it at each executable observation.
        let d_inc = market(&h, &key).pool_d.get();
        assert!(d_deep > d_inc * 2 - d_inc / 100);
        if let Some(p) = r.pool_plans.get(&key) {
            evaluated += 1;
            assert_eq!(p.action, None);
            assert_eq!(p.best, Some((ps::pool_key(&deep), d_deep)));
            assert_eq!(p.blocked, code(ShortwinError::PositionsOpen));
            assert!(p.pinned);
        }
        for a in pool_alerts(&r) {
            pinned.push((h.now(), d_inc, a));
        }
    }
    assert!(evaluated >= 19, "evaluated every 10 s: {evaluated}");
    assert_eq!(pinned.len(), 1, "{pinned:?}");
    let (t, d_at_alert, alert) = &pinned[0];
    let Alert::PoolPinned {
        market: m,
        incumbent,
        candidate,
        d_inc: di,
        d_cand,
        open_positions,
        since,
    } = alert
    else {
        panic!("{alert:?}")
    };
    assert_eq!(
        (*m, *incumbent, *candidate, *di, *d_cand, *open_positions),
        (
            key,
            ps::pool_key(&rm.pool),
            ps::pool_key(&deep),
            *d_at_alert,
            d_deep,
            1
        )
    );
    assert!(
        *since <= start + 1 && t - since >= 120 && t - since < 130,
        "{t} {since}"
    );
    assert_eq!(h.keeper.metrics.markets[&key].pool_proposals, 0);
    assert_eq!(
        market(&h, &key).pool_cache[raydium_cache::RAY_POOL],
        ps::pool_key(&rm.pool)
    );

    // The last position closes; the keeper's next tick, in the same second, adopts the sibling.
    h.with_env(|env| {
        env.crank(&rm);
        ok(env.close(&rm, &b, n));
    });
    let closed_at = h.now();
    // The incumbent's locked depth now (the close's trade grew it by its fee).
    let d_inc = h.with_env(|env| d_of(env, &rm.pool));
    let r = h.tick().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(h.now(), closed_at);
    assert_eq!(r.pool_actions.len(), 1, "{:?}", r.pool_actions);
    assert!(
        matches!(r.pool_actions[0].action, Action::Propose { expect: Proposal::Adopt(k), .. }
            if k == POOL_KIND_ZERO_POSITION),
        "{:?}",
        r.pool_actions
    );
    let ev = event(&r, PoolEventKind::Switched).expect("PoolSwitched");
    assert_eq!(
        (ev.kind, ev.candidate, ev.d_cand, ev.d_inc),
        (POOL_KIND_ZERO_POSITION, ps::pool_key(&deep), d_deep, d_inc)
    );
    let m = market(&h, &key);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert_eq!(m.pool_seen_at, closed_at, "opens wait a new listing window");
    assert_eq!(h.keeper.pool_track(&key).unwrap().pinned, None);
}

/// The venue-upgrade latch (M3-SEC-2 extended to pool selection): a Raydium upgrade the keeper
/// itself flags (duty 10) holds every proposal — the keeper checks the latch and sends nothing,
/// its plan saying `VenueUpgraded` — until the Emergency multisig's `revalidate_venue`; then the
/// thin listing is replaced at the next evaluation, still inside the listing window.
#[test]
fn proposals_wait_for_the_venue_review() {
    use shortwin::constants::GUARD_RAYDIUM_V4;
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::ix;
    use solana_signer::Signer;

    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let thin = eligible_reversed(&mut env);
    ok(env.list_ray(&thin, FART_REV.hint()));
    let key = pda::market(&thin.token().0).0;
    let seen = env.market(&key).pool_seen_at;
    let window = i64::from(env.config().params.pool_list_window_s);
    let upgraded = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_V4)] + 777;
    fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, upgraded);
    let mut h = Harness::new(env, KeeperConfig::default());
    let r = h.second().clone();
    assert_eq!(
        r.flagged,
        vec![(GUARD_RAYDIUM_V4, upgraded)],
        "{:?}",
        r.errors
    );
    assert_eq!(
        r.pool_plans[&key].blocked,
        code(ShortwinError::VenueUpgraded)
    );
    assert!(r.pool_actions.is_empty(), "{:?}", r.pool_actions);
    for s in 0..60 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(
            r.pool_actions.is_empty(),
            "second {s}: {:?}",
            r.pool_actions
        );
    }
    h.with_env(|env| {
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::revalidate_venue(
                &ea.pubkey(),
                GUARD_RAYDIUM_V4,
                upgraded,
            )],
            &[&ea],
        ));
    });
    let revalidated = h.now();
    let mut adopted = None;
    for _ in 0..40 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if let Some(e) = event(&r, PoolEventKind::Switched) {
            assert_eq!(
                (e.kind, e.candidate),
                (POOL_KIND_LISTING_WINDOW, ps::pool_key(&deep))
            );
            adopted = Some(h.now());
            break;
        }
    }
    let t = adopted.expect("adopted after the review");
    let eval = h.keeper.config().pool_select.eval_interval_s;
    assert!(t - revalidated <= eval + 1, "{t} {revalidated}");
    assert!(t < seen + window);
}

/// M4-SEC-7 (keeper liveness): anyone can create CPMM pools for a mint (≈ 0.17 SOL each). With
/// 300 junk pools planted for Fartcoin (≈ 1 SOL each, a third with 1 % of their LP burned, the
/// rest none) next to the two genuine siblings, the keeper — incumbent halted, two positions
/// open — ranks the 302 discovered pools once, over several ticks at `rank_keys_per_tick`
/// accounts a tick, and then reads only the best `max_candidates` ReadSets per tick; every read
/// of a candidate comes after the tick's crank batch; the fast path opens its window at the
/// first armed second and adopts the deepest sibling at the same second as the run without the
/// junk. Before the fix every tick of an armed fast path re-read all 302 ReadSets (1,510 keys,
/// 16 sequential calls) ahead of the cranks.
#[test]
fn junk_pools_cost_a_bounded_ranking_after_the_cranks() {
    use shortwin_keeper_litesvm::Rpc;

    let mut adoptions = Vec::new();
    for junk in [0usize, 300] {
        let (mut h, rm, _open, _first, best) = with_positions_and_siblings();
        let key = rm.market();
        h.with_env(|env| {
            for i in 0..junk {
                let lam = if i % 3 == 0 { 100 } else { 0 };
                plant(env, &rm.pool, Plant::Cpmm, i % 2 == 0, (1, 38_000), lam);
            }
        });
        let cfg = h.keeper.config().pool_select.clone();
        let best_key = ps::pool_key(&best);
        // Per tick: the keys and calls of the backend, and where the first crank batch and the
        // first candidate read fall.
        type Tick = (usize, usize, Option<usize>, Option<usize>);
        let tick = |h: &mut Harness| -> (Tick, TickReport) {
            let from = h.chain().rpc_log().len();
            let r = h.second().clone();
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            let log = h.chain().rpc_log();
            let slice = &log[from..];
            let keys: usize = slice
                .iter()
                .map(|c| match c {
                    Rpc::Accounts(k) => k.len(),
                    _ => 0,
                })
                .sum();
            let calls: usize = slice.iter().map(Rpc::calls).sum();
            let first_send = slice.iter().position(|c| matches!(c, Rpc::Send { .. }));
            let first_cand = slice
                .iter()
                .position(|c| matches!(c, Rpc::Accounts(k) if k.contains(&best_key)));
            ((keys, calls, first_send, first_cand), r)
        };
        let mut per_tick: Vec<Tick> = Vec::new();
        // Discovery and the ranking.
        for _ in 0..6 {
            per_tick.push(tick(&mut h).0);
        }
        let track = h.keeper.pool_track(&key).expect("tracked");
        assert_eq!(
            track.discovered,
            junk + 3,
            "every pool discovered (the incumbent too)"
        );
        assert!(track.rank_queue.is_empty(), "ranked");
        assert!(track.candidates.len() <= cfg.max_candidates);
        assert!(track.candidates.iter().any(|c| c.pool == best_key));
        let ranking = per_tick.clone();
        per_tick.clear();
        // The incumbent halts; run until the adoption.
        h.with_env(|env| halt(env, &rm.pool));
        let halted = h.now();
        let mut adopted = None;
        for _ in 0..400 {
            let (t, r) = tick(&mut h);
            per_tick.push(t);
            if let Some(e) = event(&r, PoolEventKind::Switched) {
                assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, best_key));
                adopted = Some(h.now() - halted);
                break;
            }
        }
        let adopted = adopted.expect("adopted");
        let max_keys = per_tick.iter().map(|t| t.0).max().unwrap_or(0);
        let max_calls = per_tick.iter().map(|t| t.1).max().unwrap_or(0);
        println!(
            "M4-SEC-7 junk {junk}: ranking ticks keys {:?}; after: max {max_keys} keys / {max_calls} calls a tick; adopted {adopted} s after the halt",
            ranking.iter().map(|t| t.0).collect::<Vec<_>>()
        );
        // Every tick — the first ranking, and the re-rankings of the fast discovery every
        // `fast_discovery_interval_s` while the incumbent fails — reads at most the ranking
        // budget on top of the best candidates' ReadSets and the tick's own reads, and every
        // candidate read comes after the tick's cranks.
        let base = 64;
        let bound = cfg.rank_keys_per_tick + cfg.max_candidates * 5 + base;
        assert!(ranking.iter().all(|t| t.0 <= bound), "{ranking:?}");
        assert!(max_keys <= bound, "{per_tick:?}");
        for (i, t) in per_tick.iter().enumerate() {
            if let (Some(send), Some(cand)) = (t.2, t.3) {
                assert!(
                    send < cand,
                    "tick {i}: a candidate read before the cranks {t:?}"
                );
            }
        }
        adoptions.push((adopted, max_calls));
    }
    assert_eq!(adoptions[0].0, adoptions[1].0, "the junk delays nothing");
    let budget_calls = h_budget_calls();
    assert!(
        adoptions[1].1 <= adoptions[0].1 + budget_calls,
        "calls a tick with the junk {} vs without {}",
        adoptions[1].1,
        adoptions[0].1
    );
}

/// The `getMultipleAccounts` calls the default ranking budget can add to a tick.
fn h_budget_calls() -> usize {
    KeeperConfig::default()
        .pool_select
        .rank_keys_per_tick
        .div_ceil(100)
}

/// The listing-window observations of a Raydium market with no position (the M4V-5 keeper
/// duty): the times the keeper observed `key` and why, over `ticks` ticks `step` seconds apart.
fn observe_for(h: &mut Harness, key: &Pubkey, step: i64, ticks: u32) -> Vec<(i64, Option<Due>)> {
    let mut out = Vec::new();
    for _ in 0..ticks {
        h.idle(step - 1);
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let now = h.now();
        out.extend(
            r.observed
                .iter()
                .filter(|(m, _)| m == key)
                .map(|(_, d)| (now, *d)),
        );
    }
    out
}

/// A freshly listed Fartcoin market (lent, no position) and the end of its listing window.
fn listed_fart() -> (Env, RayMarket, u64, i64) {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    ok(env.list_ray(&pool, FART.hint()));
    let q = crate::raydium::q_of(&env, &pool);
    let spec = raydium::RaySpec {
        fixture: FART,
        lend: q.saturating_mul(40),
        warmup: 0,
    };
    let rm = env.ray_market_listed(spec, pool);
    let m = env.market(&rm.market());
    let end = m.pool_seen_at + i64::from(env.config().params.pool_list_window_s);
    (env, rm, q, end)
}

/// M4V-5, the keeper half (ARCH §7.1 duty 1): a Raydium market with no open position is observed
/// on the listing cadence through its listing window (every `listing_interval_s` = 60 s) and
/// every second in its last `LISTING_WARMUP_S` seconds; after the window, its post-change warm-up
/// over (M4R) and without a position, it is left alone again. The first open right after the
/// window passes the warm-up (it would be refused `EmaWarmingUp` otherwise) at a `P_ema` equal to
/// the pool's spot.
#[test]
fn a_listing_window_is_observed_and_the_first_open_is_warm() {
    let (env, rm, q, end) = listed_fart();
    let key = rm.market();
    let mut h = Harness::new(env, KeeperConfig::default());
    let every = KeeperConfig::default().coverage.listing_interval_s;
    let warm_s = shortwin_keeper::decide::LISTING_WARMUP_S;
    // The window's body: a tick every 30 s.
    let ticks = u32::try_from((end - warm_s - 5 - h.now()) / 30).unwrap();
    let body = observe_for(&mut h, &key, 30, ticks);
    assert!(
        body.iter().all(|(_, d)| *d == Some(Due::Listing)),
        "{body:?}"
    );
    let gaps: Vec<i64> = body.windows(2).map(|w| w[1].0 - w[0].0).collect();
    assert!(
        gaps.iter().all(|g| *g >= every && *g <= every + 30),
        "{gaps:?}"
    );
    assert!(
        body.len() >= 55,
        "{} observations in the window",
        body.len()
    );
    // Its last `LISTING_WARMUP_S` seconds, `[end − 10, end)`: every second.
    h.idle(end - warm_s - 1 - h.now());
    let tail = observe_for(&mut h, &key, 1, u32::try_from(warm_s).unwrap());
    assert_eq!(tail.len(), usize::try_from(warm_s).unwrap(), "{tail:?}");
    assert!(
        tail.iter().all(|(_, d)| *d == Some(Due::Second)),
        "{tail:?}"
    );
    assert_eq!(h.now(), end - 1);
    // Past the window, warmed up and without a position: not observed.
    let after = observe_for(&mut h, &key, 1, 5);
    assert!(after.is_empty(), "{after:?}");
    let m = market(&h, &key);
    assert_eq!(m.ema_warm, 0, "the post-change warm-up is over (M4R)");
    let spot = h.with_env(|env| {
        let (x, y) = rm
            .pool
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .expect("reserves");
        u128::from(y) * shortwin_math::num::WAD / u128::from(x)
    });
    assert!(
        m.ema_price.get().abs_diff(spot) * 1_000 <= spot,
        "P_ema {} vs spot {spot}",
        m.ema_price.get()
    );
    h.with_env(|env| {
        let mut b = env.borrower();
        ok(env.open(&rm, &mut b, q).0);
    });
    println!(
        "M4V-5 keeper: {} listing observations {every} s apart in the window, {} in its last {warm_s} s; the first open passed the warm-up (ema_warm {})",
        body.len(),
        tail.len(),
        m.ema_warm
    );
}

/// M4V-5, the keeper half: a listing window nobody observed (the keeper was down) leaves the
/// price state where the listing started it; the first open is refused `EmaWarmingUp`, the
/// keeper observes the market every second from its first tick until the next instruction's
/// roll completes the warm-up (its projection of the prologue), and the open then lands.
#[test]
fn an_unobserved_listing_window_is_warmed_up_after_it() {
    use shortwin_litesvm_tests::errors::assert_err;

    let (env, rm, q, end) = listed_fart();
    let key = rm.market();
    let mut h = Harness::new(env, KeeperConfig::default());
    h.idle(end - h.now());
    h.with_env(|env| {
        let mut b = env.borrower();
        assert_err(&env.open(&rm, &mut b, q).0, ShortwinError::EmaWarmingUp);
    });
    let mut seconds = 0u64;
    loop {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if !r.observed.contains(&(key, Some(Due::Second))) {
            break;
        }
        seconds += 1;
        assert!(seconds <= 12, "still observing after {seconds} s");
    }
    assert_eq!(
        seconds,
        u64::from(shortwin_math::consts::EMA_WARMUP_BUCKETS),
        "one observation a second until the warm-up (the next instruction's roll completes it)"
    );
    h.with_env(|env| {
        let mut b = env.borrower();
        ok(env.open(&rm, &mut b, q).0);
    });
    assert_eq!(market(&h, &key).ema_warm, 0);
    println!("M4V-5 keeper: an unobserved listing window warmed up {seconds} s after it");
}
