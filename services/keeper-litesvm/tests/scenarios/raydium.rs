//! Keeper v1 on Raydium (M3; ARCH §7.1 duties 1, 3, 7; §9 M3): the **release** short.win on
//! the mainnet fork of the program tests (`shortwin_litesvm_tests::raydium`) — the dumped AMM v4
//! and CPMM programs and pools — driven by the keeper through its own account lists
//! (`shortwin_keeper::venue`: the adapters' key functions plus ARCH §3.12's writability).
//! Every keeper transaction runs the invariant hooks after every instruction, which see Raydium
//! markets through the harness's own reader-based venue view.
//!
//! - Six Raydium markets (one AMM v4, five CPMM: SPL and Token-2022 bases, WSOL as token 0 and
//!   token 1, creator fees) share one v1 crank batch; after a v1 refusal the v0 batches are
//!   packed by size.
//! - A whale's real buy pumps an AMM v4 and a CPMM pool: the keeper liquidates both at the
//!   first second an outside oracle's simulation accepts, observing every second before.
//! - Healthy positions on AMM v4 and on a Token-2022 CPMM pool are expired at their deadlines.

#![allow(dead_code)]

use anchor_lang::prelude::Pubkey;
use shortwin::events::{Expire, Liquidate};
use shortwin_keeper::events;
use shortwin_litesvm_tests::engine::{Borrower, VenueMarket, expire_ix, liquidate_ix};
use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, V4Fixture};
use shortwin_litesvm_tests::raydium::{self, RayFixture, RayMarket, RaySpec};
use shortwin_litesvm_tests::{pda, read};
use shortwin_math::fees::Direction;
use shortwin_services_common::tx::{TxFormat, V0_MAX_TX_BYTES, V1_MAX_TX_BYTES};
use solana_signer::Signer;

use crate::common::*;

pub const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
pub const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
pub const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);
pub const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
pub const WSOL_T1: RayFixture = RayFixture::Cpmm(CpmmFixture::WsolToken1);
pub const CREATOR_BOTH: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorBoth);
/// Six listable dumped pools of six distinct mints (`WSOL_T1` once its LP is burned).
pub const ALL: [RayFixture; 6] = [FART, USELESS, CREATOR_FEE, T22, CREATOR_BOTH, WSOL_T1];

/// The default position size on `pool`: `min(X/400, 2 SOL of notional)` (as `ray_it`).
pub fn q_of(env: &Env, pool: &AnyPool) -> u64 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture` with `create_market` (the real instruction; `WSOL_T1`'s LP burned first so it
/// is eligible), lends `40·Q`, waits out the listing window and cranks once.
pub fn add_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    if fixture == WSOL_T1 {
        raydium::set_mint_supply(&mut env.svm, &raydium::lp_mint(&pool), 0);
    }
    ok(env.list_ray(&pool, fixture.hint()));
    let q = q_of(env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}

/// The fork with a market on each of `fixtures` and a keeper with `cfg`.
pub fn ray_setup(fixtures: &[RayFixture], cfg: KeeperConfig) -> (Harness, Vec<(RayMarket, u64)>) {
    let mut env = Env::ray_fork();
    let markets = fixtures.iter().map(|f| add_market(&mut env, *f)).collect();
    (Harness::new(env, cfg), markets)
}

/// Opens `q` on `rm` for a new borrower after a crank (must succeed).
pub fn ray_open(h: &Harness, rm: &RayMarket, q: u64) -> (Borrower, u64) {
    h.with_env(|env| {
        env.crank(rm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nonce) = env.open(rm, &mut b, q);
        ok(res);
        (b, nonce)
    })
}

/// The position account of `(borrower, nonce)` on `m`.
pub fn position_of(m: &impl VenueMarket, borrower: &Pubkey, nonce: u64) -> Pubkey {
    pda::position(&m.market(), borrower, nonce).0
}

/// Whether that position is still open.
pub fn is_open(h: &Harness, m: &impl VenueMarket, borrower: &Pubkey, nonce: u64) -> bool {
    h.with_env(|env| read::account(&env.svm, &position_of(m, borrower, nonce)).is_some())
}

/// A whale's real buy of `sol` lamports on `rm`'s pool.
pub fn whale_buy(h: &Harness, rm: &RayMarket, sol: u64) {
    h.with_env(|env| {
        let (tok, program) = rm.pool.token();
        let whale = fork::trader(env, &tok, program, sol + 1_000_000_000, 0);
        ok(fork::swap(
            env,
            &rm.pool,
            &whale,
            Direction::Buy,
            fork::Swap::ExactIn(sol, 0),
        ));
    });
}

/// The same alerts without the ones every fork run carries at start (none today: missing
/// ProgramData accounts are logged, not alerted).
pub fn venue_alerts(r: &TickReport) -> Vec<Alert> {
    r.alerts
        .iter()
        .filter(|a| {
            matches!(
                a,
                Alert::VenueAdmin { .. }
                    | Alert::VenueUpgraded { .. }
                    | Alert::VenueUpgradeFlagged { .. }
                    | Alert::VenueFlagFailed { .. }
                    | Alert::ProgramUpgraded { .. }
                    | Alert::ProgramDataUnreadable { .. }
            )
        })
        .cloned()
        .collect()
}

#[test]
fn six_raydium_markets_share_one_v1_batch_and_v0_batches_are_packed_by_size() {
    let (mut h, ms) = ray_setup(&ALL, KeeperConfig::default());
    for (rm, q) in &ms {
        let _ = ray_open(&h, rm, *q);
    }
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.skipped, vec![], "every Raydium market is covered");
    assert_eq!(r.cranked.len(), 6, "{r:?}");
    let sizes: Vec<(usize, TxFormat)> = r
        .batches
        .iter()
        .map(|b| (b.markets.len(), b.format))
        .collect();
    assert_eq!(sizes, vec![(6, TxFormat::V1)]);
    let sent = h.chain().sent();
    let s = sent.last().unwrap();
    assert!(s.ok && s.instructions == 6);
    assert!(s.bytes <= V1_MAX_TX_BYTES && s.accounts <= 64, "{s:?}");
    println!(
        "v1 batch of 6 Raydium cranks (1 AMM v4 + 5 CPMM): {} B, {} accounts, {} CU",
        s.bytes, s.accounts, r.batches[0].units
    );
    // No venue alert on healthy pools.
    assert!(venue_alerts(&r).is_empty(), "{:?}", r.alerts);

    // A cluster without v1 and no lookup table: v0 batches cut by size (1,232 B).
    h.chain().set_v1(false);
    let before = h.chain().sent().len();
    let mut cranked = 0;
    for _ in 0..31 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        cranked += r.cranked.len();
        if cranked >= 6 {
            let v0: Vec<(usize, TxFormat)> = r
                .batches
                .iter()
                .map(|b| (b.markets.len(), b.format))
                .collect();
            println!("v0 batches without a lookup table: {v0:?}");
            assert!(v0.iter().all(|(_, f)| *f == TxFormat::V0));
            assert!(v0.iter().all(|(n, _)| *n >= 2), "{v0:?}");
            break;
        }
    }
    assert_eq!(cranked, 6);
    for s in &h.chain().sent()[before..] {
        if s.format == Some(TxFormat::V0) {
            assert!(s.bytes <= V0_MAX_TX_BYTES && s.ok, "{s:?}");
            println!(
                "  v0: {} cranks, {} B, {} accounts",
                s.instructions, s.bytes, s.accounts
            );
        }
    }
}

/// Duty 3 on both programs: after a whale's real buy (≈ ×2.2), each position is liquidated at
/// the first second the program accepts it (an outside oracle simulates the same liquidation
/// every second), observed every second before, with no separate crank that second (duty 7).
#[test]
fn a_pump_is_liquidated_at_the_first_executable_second_on_both_programs() {
    let (mut h, ms) = ray_setup(&[FART, USELESS], KeeperConfig::default());
    let positions: Vec<(Borrower, u64)> = ms.iter().map(|(rm, q)| ray_open(&h, rm, *q)).collect();
    let oracle = Oracle::new(&h);
    h.seconds(3);
    for (rm, _) in &ms {
        let (_, y) = rm.reserves(&h.env.borrow().svm).unwrap();
        whale_buy(&h, rm, y / 1_000 * 484);
    }
    let mut first_ok: Vec<Option<i64>> = vec![None; ms.len()];
    let mut landed: Vec<Option<i64>> = vec![None; ms.len()];
    for s in 0..60 {
        h.with_env(|env| env.warp(1));
        let now = h.now();
        for (i, ((rm, _), (b, nonce))) in ms.iter().zip(&positions).enumerate() {
            if first_ok[i].is_none()
                && landed[i].is_none()
                && oracle
                    .accepts(&h, liquidate_ix(rm, &oracle.kp.pubkey(), &b.key(), *nonce))
                    .is_ok()
            {
                first_ok[i] = Some(now);
            }
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        for (i, ((rm, _), (b, nonce))) in ms.iter().zip(&positions).enumerate() {
            let key = rm.market();
            let pos = position_of(rm, &b.key(), *nonce);
            if let Some(out) = landed_for(&r, ExitKind::Liquidate, &pos).next() {
                let l: Liquidate = events::of(out).into_iter().next().expect("Liquidate");
                assert_eq!((l.keeper, l.position), (h.keeper.payer(), pos));
                assert!(!r.cranked.contains(&key), "the exit is the observation");
                assert!(r.observed(&key));
                landed[i] = Some(now);
                println!("{:?}: liquidated {} s after the pump", rm.fixture, s + 1);
            } else if landed[i].is_none() {
                // Near a threshold: observed every second (duty 1).
                if r.evaluations
                    .get(&key)
                    .and_then(|v| v.first())
                    .is_some_and(|e| e.near)
                {
                    assert!(
                        r.observed(&key),
                        "second {s}: {:?} near, not observed",
                        rm.fixture
                    );
                }
            }
        }
        if landed.iter().all(Option::is_some) {
            break;
        }
    }
    for (i, ((rm, _), (b, nonce))) in ms.iter().zip(&positions).enumerate() {
        assert!(landed[i].is_some(), "{:?} never liquidated", rm.fixture);
        assert_eq!(
            landed[i], first_ok[i],
            "{:?}: at the first accepted second",
            rm.fixture
        );
        assert!(!is_open(&h, rm, &b.key(), *nonce));
        assert_eq!(h.keeper.metrics.markets[&rm.market()].liquidations, 1);
        // The lending vault holds no delegate afterwards (T-RAY-PDA's post-condition).
        let d = h.with_env(|env| read::token_delegate(&env.svm, &rm.keys.pool_vault));
        assert_eq!(d, None);
    }
}

/// Duty 3, expiry, on AMM v4 and on a Token-2022 CPMM pool: each healthy position is expired at
/// the first second the program accepts it, the last minute observed every second.
#[test]
fn healthy_positions_are_expired_at_their_deadlines_on_both_programs() {
    let (mut h, ms) = ray_setup(&[FART, T22], KeeperConfig::default());
    let positions: Vec<(Borrower, u64)> = ms.iter().map(|(rm, q)| ray_open(&h, rm, *q)).collect();
    let deadlines: Vec<i64> = ms
        .iter()
        .zip(&positions)
        .map(|((rm, _), (b, n))| h.with_env(|env| env.position(rm, &b.key(), *n).deadline))
        .collect();
    let oracle = Oracle::new(&h);
    h.seconds(3);
    let first = *deadlines.iter().min().unwrap();
    let wait = first - 90 - h.now();
    h.idle(wait);
    let mut first_ok: Vec<Option<i64>> = vec![None; ms.len()];
    let mut expired: Vec<Option<i64>> = vec![None; ms.len()];
    for s in 0..200 {
        h.with_env(|env| env.warp(1));
        let now = h.now();
        for (i, ((rm, _), (b, nonce))) in ms.iter().zip(&positions).enumerate() {
            if first_ok[i].is_none()
                && oracle
                    .accepts(&h, expire_ix(rm, &oracle.kp.pubkey(), &b.key(), *nonce))
                    .is_ok()
            {
                first_ok[i] = Some(now);
            }
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        for (i, ((rm, _), (b, nonce))) in ms.iter().zip(&positions).enumerate() {
            let pos = position_of(rm, &b.key(), *nonce);
            if let Some(out) = landed_for(&r, ExitKind::Expire, &pos).next() {
                let e: Expire = events::of(out).into_iter().next().expect("Expire");
                assert_eq!(e.keeper, h.keeper.payer());
                expired[i] = Some(now);
            } else if expired[i].is_none() && deadlines[i] - now <= 60 {
                assert!(
                    r.observed(&rm.market()),
                    "{:?}: last minute unobserved",
                    rm.fixture
                );
            }
            assert!(landed_for(&r, ExitKind::Liquidate, &pos).next().is_none());
        }
        if expired.iter().all(Option::is_some) {
            break;
        }
    }
    for (i, ((rm, _), (b, nonce))) in ms.iter().zip(&positions).enumerate() {
        let f = first_ok[i].unwrap_or_else(|| panic!("{:?}: never accepted", rm.fixture));
        assert!(f >= deadlines[i]);
        assert_eq!(expired[i], Some(f), "{:?}", rm.fixture);
        assert!(!is_open(&h, rm, &b.key(), *nonce));
        println!(
            "{:?}: expired {} s after the deadline",
            rm.fixture,
            f - deadlines[i]
        );
    }
}
