//! The recorded sessions (INDEXER.md §11.3): LiteSVM scenarios run with the `Env` recording hook
//! on, each returned as a [`Session`]. Owner: ANALYTICS + REPLAY.
//!
//! Every scenario records from before `initialize_config`, so the session holds the whole
//! history the indexer needs (the first `protocol_params_history` row, every market's
//! `create_market`). The checkpoint policy is `AfterEvents` (a checkpoint after every
//! transaction that emitted a short.win event other than `PriceCranked`), so every position is
//! seen right after its `Open` (its locked terms) and every accounting change has its chain
//! state to compare with.
//!
//! No scenario writes a short.win account an event projects (a test-only write has no event, so
//! the indexer could never reproduce it; [`crate::chain::check_session`] would expose one). The
//! two exceptions are refresher-only state: the mock's empty `VenueExtras` account
//! (`Env::install_mock_extras`, as `Env::test_venue` does) and the fork's fresh `OracleCache`
//! price (`Env::fresh_oracle`; `crank_sol_usd` emits no event either).
//!
//! | Session | Build, venue | Covers |
//! |---|---|---|
//! | [`mock_lifecycle`] | test-venue, mock | lenders, close, `add_margin`, CR liquidation, S1, S2, S3, expiry, `CoverageGap`, fee sweep, `claim_sol` / `withdraw` / `cover_deficit` / `sync_lender`, a full wipe with the record donation and `sweep_trader`, a flushing rebase with the record donation, a vest flush without a roll, the last shares leaving a running vest, pauses, route-broken flags, the latch, `disable_venue`, `flag_unsafe_mint` |
//! | [`mock_admin`] | test-venue, mock | every `ConfigChange` kind through the timelock, a cancel, locked terms across a `Params` change, `disable_venue` followed by an unrelated change (the bit stays cleared) then `EnableVenues`, the coverage key's pause / clear and the Emergency's clear, `set_venue_extras`, `SettleArmed` → `SettleDisarmed` |
//! | [`keeper_liquidation`] | test-venue, mock | the keeper's v1 batches, per-second cranks, its liquidation |
//! | [`keeper_coverage_pause`] | test-venue, mock | O17's drill: an outage, the keeper's pause, a liquidation under it, the automatic unpause |
//! | [`raydium_lifecycle`] | release, AMM v4 + CPMM fork | listings, a close, keeper liquidations on both venues, S3 on an admin-halted pool |
//! | [`pool_switch`] | release, AMM v4 fork | the fast path: `PoolProposed`, `PoolSwitchCancelled`, `PoolSwitched` |
//! | [`pump_synthetic_exit`] | release, pump fork | the borrower's own close past `rT`: one `buy_v3` completes the curve (pump's `TradeEvent`, `CompleteEvent`, `PostCompleteBuyEvent`; `VenueStateChanged → PumpMigrating`); its thinned copy is INGEST's fixture |
//! | [`pump_graduation`] | release, pump + PumpSwap fork | curve open / close, the keeper's synthetic expiry past `rT` (pump v3 completes the curve in the exit: `VenueStateChanged`), `migrate_v2` and the transition, an exit on PumpSwap |
//! | [`pump_graduation_raced`] | release, pump + PumpSwap fork | the keeper's expiry raced by a third party's synthetic completion (the expiry fails on chain), then the graduation and the expiry on the pool |

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use shortwin::constants::VENUE_BIT_MOCK;
use shortwin_indexer::ingest::session::Session;
use shortwin_keeper_litesvm::Harness;
use shortwin_litesvm_tests::Env;
use shortwin_litesvm_tests::engine::{Borrower, MarketSpec, MockMarket};
use shortwin_litesvm_tests::errors::ok;
use shortwin_litesvm_tests::pda;
use shortwin_litesvm_tests::record::CheckpointPolicy;

use crate::recorder;

/// Positions of this size fit every default cap of the default mock market (≈ 3 SOL).
pub const Q: u64 = 3_000_000_000_000;

/// The program build the mock scenarios run (`scripts/build-test-venue.sh`).
pub const TEST_VENUE: &str = "test-venue";

/// `Env::test_venue` with the recording started before `initialize_config`.
pub fn test_venue_env(policy: CheckpointPolicy) -> Env {
    let mut env = Env::new_test_venue_program();
    env.load_mock_cpamm();
    env.start_recording_with(policy);
    let mut args = env.default_init_args();
    args.venue_enabled |= VENUE_BIT_MOCK;
    ok(env.initialize_config(args));
    env.install_mock_extras();
    env
}

/// Stops the recording and renders it as a session.
pub fn finish(env: &mut Env, scenario: &str, build: &str, notes: Option<&str>) -> Session {
    let rec = env.take_recording();
    recorder::to_session(&rec, recorder::header(scenario, build, notes))
        .unwrap_or_else(|e| panic!("{scenario}: rendering the session: {e:?}"))
}

/// Opens `q` on `mm` for a new borrower (a crank one second before, so the keeper's first
/// observation follows no gap).
pub fn open(env: &mut Env, mm: &MockMarket, q: u64) -> (Borrower, u64) {
    env.crank(mm);
    env.warp(1);
    let mut b = env.borrower();
    let (res, nonce) = env.open(mm, &mut b, q);
    ok(res);
    (b, nonce)
}

/// Whether the position account exists.
pub fn position_open(env: &Env, mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> bool {
    env.svm
        .get_account(&pda::position(&mm.market(), borrower, nonce).0)
        .is_some()
}

/// **keeper liquidation**: one mock market, a position, a whale lifting the price 4 % a
/// second, the keeper observing every second until its liquidation lands (the keeper's v1
/// transactions, its cranks).
pub fn keeper_liquidation() -> Session {
    let env = test_venue_env(CheckpointPolicy::AfterEvents);
    let mut h = Harness::new(env, Default::default());
    let mm = h.with_env(|env| env.mock_market(MarketSpec::default()));
    let (b, nonce) = h.with_env(|env| open(env, &mm, Q));
    let whale = h.with_env(|env| env.actor(&mm, 0, 5_000_000_000_000));
    h.seconds(5);
    let mut done = false;
    for _ in 0..120 {
        h.with_env(|env| {
            env.warp(1);
            let spot = env.spot(&mm);
            env.push_price(&mm, &whale, spot * 104 / 100);
        });
        h.tick();
        if !h.with_env(|env| position_open(env, &mm, &b.key(), nonce)) {
            done = true;
            break;
        }
    }
    assert!(
        done,
        "keeper_liquidation: the position was never liquidated"
    );
    h.seconds(3);
    h.with_env(|env| finish(env, "keeper_liquidation", TEST_VENUE, None))
}

// ---- helpers of the scripted scenarios -----------------------------------------------------------

use anchor_lang::prelude::AccountMeta;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin::PauseScope;
use shortwin::events::{Expire as ExpireEv, Liquidate as LiquidateEv, SolSettle as SolSettleEv};
use shortwin_litesvm_tests::TxResult;
use shortwin_litesvm_tests::drive::Actor;
use shortwin_litesvm_tests::engine::{
    add_margin_ix, crank_price_ix, expire_ix, liquidate_ix, set_route_broken_ix, sol_settle_ix,
    sweep_trader_ix_for,
};
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::record::{Keypair, Signer};
use shortwin_litesvm_tests::{events, mock, tokens};

/// A lender of a mock market.
pub struct Lender {
    pub kp: Keypair,
    pub keys: LenderKeys,
}

/// A new lender of `mm` with `amount` freshly minted tokens deposited (with the epoch records
/// the program asks for while a wipe record is live).
pub fn join_lender(env: &mut Env, mm: &MockMarket, amount: u64) -> Lender {
    let kp = env.funded_keypair();
    let owner = kp.pubkey();
    let payer = env.payer.insecure_clone();
    let acc =
        tokens::create_token_account(&mut env.svm, &payer, &mm.mint(), &owner, mm.spec.program);
    tokens::mint_to(
        &mut env.svm,
        &payer,
        &mm.mint(),
        &acc,
        &mm.pool.mint_authority,
        amount,
        mm.spec.program,
    );
    let keys = LenderKeys {
        owner,
        mint: mm.mint(),
        token_program: mm.keys.token_program,
        owner_token_account: acc,
    };
    let l = Lender { kp, keys };
    ok(deposit(env, &l, amount));
    l
}

/// `deposit(amount)` by `l` with its epoch records.
pub fn deposit(env: &mut Env, l: &Lender, amount: u64) -> TxResult {
    let market = pda::market(&l.keys.mint).0;
    let epoch = env.market(&market).share_epoch;
    let d = ix::with_remaining(
        ix::deposit(&l.keys, amount, 0),
        &ix::epoch_records(&market, 0, epoch),
    );
    let kp = l.kp.insecure_clone();
    env.send(&[d], &[&kp])
}

/// The epoch records `owner` is behind on in `market`.
pub fn records_for(env: &Env, market: &Pubkey, owner: &Pubkey) -> Vec<AccountMeta> {
    let from = env.lender(market, owner).map_or(0, |l| l.epoch);
    ix::epoch_records(market, from, env.market(market).share_epoch)
}

/// `ix` with `owner`'s epoch records appended.
pub fn with_records(env: &Env, market: &Pubkey, owner: &Pubkey, ix: Instruction) -> Instruction {
    let metas = records_for(env, market, owner);
    ix::with_remaining(ix, &metas)
}

/// The seeding lender of `mm` withdraws free tokens down to `keep` (the largest share amount
/// whose tokens fit).
pub fn withdraw_down_to(env: &mut Env, mm: &MockMarket, keep: u64) {
    let m = env.market(&mm.market());
    let pool = m.pool();
    let want = m.tokens_free - keep;
    let owner = mm.lender_keys().owner;
    let shares = env.lender(&mm.market(), &owner).expect("lender").shares;
    let (mut lo, mut hi) = (0u128, shares);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if shortwin_math::pool::tokens_for_shares(&pool, mid).expect("tokens") <= u128::from(want) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let w = with_records(
        env,
        &mm.market(),
        &owner,
        ix::withdraw(&mm.lender_keys(), lo, want),
    );
    let kp = mm.lender.insecure_clone();
    ok(env.send(&[w], &[&kp]));
    assert_eq!(env.market(&mm.market()).tokens_free, keep);
}

/// A whale able to move `mm`'s price both ways.
pub fn whale(env: &mut Env, mm: &MockMarket) -> Actor {
    env.actor(
        mm,
        mm.spec.base.saturating_mul(10),
        mm.spec.quote.saturating_mul(10),
    )
}

/// `p · m` for a WAD price.
pub fn scale(p: u128, m: f64) -> u128 {
    ((p as f64) * m) as u128
}

/// Moves `mm`'s spot by `mult`.
pub fn move_by(env: &mut Env, mm: &MockMarket, whale: &Actor, mult: f64) {
    let target = scale(env.spot(mm), mult);
    env.push_price(mm, whale, target);
}

/// Each second: crank, then `ix(env)` signed by `keeper`, until it lands with an event `T` (at
/// most `max` seconds); returns the event.
pub fn every_second_until<T>(
    env: &mut Env,
    mm: &MockMarket,
    max: u32,
    keeper: &Keypair,
    mut ix: impl FnMut(&Env) -> Instruction,
) -> Option<T>
where
    T: anchor_lang::Event + anchor_lang::Discriminator + anchor_lang::AnchorDeserialize,
{
    for _ in 0..max {
        env.warp(1);
        ok(env.send(&[crank_price_ix(mm)], &[]));
        let i = ix(env);
        let kp = keeper.insecure_clone();
        if let Ok(meta) = env.send(&[i], &[&kp])
            && let Some(ev) = events::of::<T>(&meta).pop()
        {
            return Some(ev);
        }
    }
    None
}

/// Cranks `mm` every `step` seconds, `n` times.
pub fn crank_every(env: &mut Env, mm: &MockMarket, step: i64, n: u32) {
    for _ in 0..n {
        env.warp(step);
        ok(env.send(&[crank_price_ix(mm)], &[]));
    }
}

/// Warps (with a crank every `step` seconds) to `deadline + after`.
pub fn crank_to(env: &mut Env, mm: &MockMarket, step: i64, to: i64) {
    while env.now() + step < to {
        env.warp(step);
        ok(env.send(&[crank_price_ix(mm)], &[]));
    }
    let left = to - env.now();
    if left > 0 {
        env.warp(left);
    }
}

/// `set_route_broken(Market, broken)` on `mm` by the Emergency vault.
pub fn route_broken(env: &mut Env, mm: &MockMarket, broken: bool) {
    let ea = env.pause_authority.insecure_clone();
    let ix = set_route_broken_ix(
        &ea.pubkey(),
        PauseScope::Market,
        0,
        broken,
        Some(mm.market()),
    );
    ok(env.send(&[ix], &[&ea]));
}

/// Removes `bps` of the mock pool's liquidity (a thin pool: S2 territory).
pub fn drain(env: &mut Env, mm: &MockMarket, bps: u64) {
    let r = mock::reserves(&env.svm, mm.mock());
    let base = (u128::from(r.base) * u128::from(bps) / 10_000) as u64;
    let quote = (u128::from(r.quote) * u128::from(bps) / 10_000) as u64;
    ok(mock::remove_liquidity(env, &mm.pool, base, quote));
}

/// Settles `(b, nonce)` of `mm` with per-second keeper calls; panics unless it settles with
/// `condition`.
pub fn settle_until(
    env: &mut Env,
    mm: &MockMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
    max: u32,
) -> SolSettleEv {
    let (kk, bk) = (keeper.pubkey(), b.key());
    every_second_until::<SolSettleEv>(env, mm, max, keeper, |env| {
        sol_settle_ix(env, mm, &kk, &bk, nonce)
    })
    .unwrap_or_else(|| panic!("position {bk}/{nonce} never settled"))
}

/// The lamports `mock_lifecycle` sends to M5's rebase record address before the rebase (M4's
/// full wipe gets 7,000,000).
pub const M5_DONATION: u64 = 2_000_000;

/// The `mock_lifecycle` session (INDEXER.md §11.3): one scripted run on the test-venue build
/// and the mock venue through real instructions only.
///
/// | Market | Covers |
/// |---|---|
/// | M1 | three lenders; an open with `add_margin` and its close; a CR liquidation after a ×2.3 pump; an S1 after a ×4 gap; an expiry at the deadline after three days of hourly cranks (`CoverageGap`s on the way); `sweep_protocol_fees`; `claim_sol`, `withdraw`, `cover_deficit`, `sync_lender`; `set_pause` (market and global); `set_route_broken` (market and global); `flag_venue_upgraded` / `revalidate_venue`; `disable_venue`; `flag_unsafe_mint` |
/// | M2 | S2: a 98.5 % LP drain, a ×1.8 pump, the capped settlement |
/// | M3 | S3: the Emergency's route-broken flag, the 30 s chain, an expired position settled at the timer |
/// | M4 | a full wipe (`EpochRolled` num 0) with a pre-funded record address (the donation of §6.4), `sweep_trader` into the wipe record, a deposit after it, the wiped lender's `sync_lender`, then its `claim_sol`, which closes the position (KI-67) |
/// | M5 | a rebase (`EpochRolled` num 1) that flushes the vest, with a pre-funded record address whose donation vests anew ([`M5_DONATION`]), and the lender's `sync_lender` across it |
/// | M6 | a vest flush without an epoch roll (AIR-07): a trader sweep vests, then an S1 leaves ⌊Q/999⌋ free tokens, at most 1/1,000 of the claims, and no rebase; `SolSettle.vest_flushed` reports it |
/// | M7 | the last shares leave a running vest (R3B-2): a trader sweep vests, then the only lender withdraws every share and is paid the vest; `Withdraw.s_after = 0` reports it |
#[allow(clippy::too_many_lines)]
pub fn mock_lifecycle() -> Session {
    let mut env = test_venue_env(CheckpointPolicy::AfterEvents);
    env.set_all_venue_program_data();
    let keeper = env.funded_keypair();
    let kk = keeper.pubkey();

    // ---- M1: lenders, close, liquidation, S1, expiry --------------------------------------
    let m1 = env.mock_market(MarketSpec::default());
    let l2 = join_lender(&mut env, &m1, 20_000_000_000_000);
    let l3 = join_lender(&mut env, &m1, 30_000_000_000_000);
    let w1 = whale(&mut env, &m1);
    env.tick(&m1, 5);

    // An open, a margin top-up, a close.
    let (a, na) = open(&mut env, &m1, Q);
    env.tick(&m1, 3);
    let ak = a.kp.insecure_clone();
    ok(env.send(&[add_margin_ix(&m1, &a.key(), na, 1_000_000_000)], &[&ak]));
    env.tick(&m1, 20);
    ok(env.close(&m1, &a, na));
    env.tick(&m1, 5);

    // A CR liquidation after a ×2.3 pump.
    let (b, nb) = open(&mut env, &m1, Q);
    env.tick(&m1, 5);
    move_by(&mut env, &m1, &w1, 2.3);
    let (bk, kk2) = (b.key(), kk);
    let liq = every_second_until::<LiquidateEv>(&mut env, &m1, 60, &keeper, |_| {
        liquidate_ix(&m1, &kk2, &bk, nb)
    })
    .expect("CR liquidation");
    assert!(!liq.expired);
    env.tick(&m1, 5);

    // S1: a ×4 gap the vault cannot buy back.
    let (d, nd) = open(&mut env, &m1, Q);
    env.tick(&m1, 5);
    move_by(&mut env, &m1, &w1, 4.0);
    let s1 = settle_until(&mut env, &m1, &keeper, &d, nd, 120);
    assert_eq!(s1.condition, 1, "S1");
    env.tick(&m1, 5);

    // An expiry: held three days with hourly cranks, expired at the deadline.
    let (c, nc) = open(&mut env, &m1, Q / 3);
    let deadline = env.position(&m1, &c.key(), nc).deadline;
    crank_to(&mut env, &m1, 6 * 3_600, deadline - 30);
    let ck = c.key();
    let exp = every_second_until::<ExpireEv>(&mut env, &m1, 120, &keeper, |_| {
        expire_ix(&m1, &kk, &ck, nc)
    })
    .expect("expiry");
    assert!(exp.rent > 0);
    env.tick(&m1, 5);

    // Protocol fees swept; the lenders' SOL side.
    let fee_recipient = env.config().fee_recipient;
    ok(env.send(
        &[ix::sweep_protocol_fees(&m1.market(), &fee_recipient)],
        &[],
    ));
    env.warp(7_200);
    env.tick(&m1, 2);
    for l in [&l2, &l3] {
        let i = with_records(
            &env,
            &m1.market(),
            &l.keys.owner,
            ix::claim_sol(&l.keys.owner, &m1.market()),
        );
        let kp = l.kp.insecure_clone();
        ok(env.send(&[i], &[&kp]));
    }
    let shares2 = env.lender(&m1.market(), &l2.keys.owner).expect("l2").shares;
    let w = with_records(
        &env,
        &m1.market(),
        &l2.keys.owner,
        ix::withdraw(&l2.keys, shares2 / 2, 0),
    );
    let kp2 = l2.kp.insecure_clone();
    ok(env.send(&[w], &[&kp2]));
    let cov = with_records(
        &env,
        &m1.market(),
        &l3.keys.owner,
        ix::cover_deficit(&l3.keys.owner, &m1.market(), 1_000_000),
    );
    let kp3 = l3.kp.insecure_clone();
    let _ = env.send(&[cov], &[&kp3]);
    let sync = with_records(
        &env,
        &m1.market(),
        &l3.keys.owner,
        ix::sync_lender(&l3.keys.owner, &m1.market(), 8),
    );
    ok(env.send(&[sync], &[]));

    // Emergency powers.
    let ea = env.pause_authority.insecure_clone();
    let ek = ea.pubkey();
    for paused in [true, false] {
        ok(env.send(
            &[ix::set_pause(
                &ek,
                PauseScope::Market,
                paused,
                Some(m1.market()),
            )],
            &[&ea],
        ));
        ok(env.send(
            &[ix::set_pause(&ek, PauseScope::Global, paused, None)],
            &[&ea],
        ));
        route_broken(&mut env, &m1, paused);
        let bit = shortwin::constants::VENUE_BIT_RAYDIUM_CPMM.trailing_zeros() as u8;
        ok(env.send(
            &[set_route_broken_ix(
                &ek,
                PauseScope::Global,
                bit,
                paused,
                None,
            )],
            &[&ea],
        ));
    }
    // The venue-upgrade latch: a CPMM ProgramData redeploy flagged, then revalidated.
    let guard = shortwin::constants::GUARD_RAYDIUM_CPMM;
    let slot = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[usize::from(guard)] + 1;
    env.set_venue_program_data(guard, slot);
    ok(env.send(&[ix::flag_venue_upgraded(guard)], &[]));
    env.warp(60);
    ok(env.send(&[ix::revalidate_venue(&ek, guard, slot)], &[&ea]));
    // A venue bit disabled (opens only).
    let cpmm_bit = shortwin::constants::VENUE_BIT_RAYDIUM_CPMM.trailing_zeros() as u8;
    ok(env.send(&[ix::disable_venue(&ek, cpmm_bit)], &[&ea]));

    // ---- M2: S2 on a drained pool ---------------------------------------------------------
    let m2 = env.mock_market(MarketSpec::default());
    let w2 = whale(&mut env, &m2);
    env.tick(&m2, 5);
    let (e2, n2) = open(&mut env, &m2, Q);
    env.tick(&m2, 5);
    drain(&mut env, &m2, 9_850);
    env.tick(&m2, 90);
    move_by(&mut env, &m2, &w2, 1.8);
    let s2 = settle_until(&mut env, &m2, &keeper, &e2, n2, 180);
    assert_eq!(s2.condition, 2, "S2");

    // ---- M3: S3 under the Emergency's route-broken flag ----------------------------------------
    let m3 = env.mock_market(MarketSpec::default());
    env.tick(&m3, 5);
    let (e3, n3) = open(&mut env, &m3, Q);
    env.tick(&m3, 5);
    let d3 = env.position(&m3, &e3.key(), n3).deadline;
    crank_to(&mut env, &m3, 6 * 3_600, d3 - 700);
    route_broken(&mut env, &m3, true);
    let (e3k, kk3) = (e3.key(), kk);
    for _ in 0..23 {
        env.warp(30);
        ok(env.send(&[crank_price_ix(&m3)], &[]));
        let i = sol_settle_ix(&env, &m3, &kk3, &e3k, n3);
        let kp = keeper.insecure_clone();
        ok(env.send(&[i], &[&kp]));
    }
    let now = env.now();
    if d3 > now {
        env.warp(d3 - now);
    }
    let s3 = settle_until(&mut env, &m3, &keeper, &e3, n3, 30);
    assert_eq!(s3.condition, 3, "S3");
    route_broken(&mut env, &m3, false);

    // ---- M4: a full wipe, the record donation, the trader sweep, a deposit after it ------------
    let m4 = env.mock_market(MarketSpec::default());
    let w4 = whale(&mut env, &m4);
    env.tick(&m4, 5);
    let (e4, n4) = open(&mut env, &m4, Q);
    env.tick(&m4, 5);
    withdraw_down_to(&mut env, &m4, 0);
    let epoch = env.market(&m4.market()).share_epoch;
    env.fund(&pda::share_epoch(&m4.market(), epoch).0, 7_000_000);
    move_by(&mut env, &m4, &w4, 4.0);
    let wipe = settle_until(&mut env, &m4, &keeper, &e4, n4, 120);
    assert_eq!(wipe.condition, 1);
    let m = env.market(&m4.market());
    assert_eq!(m.total_shares.get(), 0, "a full wipe");
    // Lamports land on the trader PDA; the sweep credits them to the wipe record.
    env.fund(&m4.keys.trader, 3_000_000);
    ok(env.send(&[sweep_trader_ix_for(&env, &m4.market())], &[]));
    env.tick(&m4, 3);
    let late = join_lender(&mut env, &m4, 10_000_000_000_000);
    let owner4 = m4.lender_keys().owner;
    let sync4 = with_records(
        &env,
        &m4.market(),
        &owner4,
        ix::sync_lender(&owner4, &m4.market(), 8),
    );
    ok(env.send(&[sync4], &[]));
    // KI-67: the wiped lender's claim pays it in full and closes its position (rent back).
    let claim4 = with_records(
        &env,
        &m4.market(),
        &owner4,
        ix::claim_sol(&owner4, &m4.market()),
    );
    let kp4 = m4.lender.insecure_clone();
    ok(env.send(&[claim4], &[&kp4]));
    assert!(
        env.lender(&m4.market(), &owner4).is_none(),
        "KI-67: the wiped lender's claim_sol closes its position"
    );
    let _ = late;

    // ---- M5: a rebase, its record pre-funded ---------------------------------------------------
    let m5 = env.mock_market(MarketSpec::default());
    let w5 = whale(&mut env, &m5);
    env.tick(&m5, 5);
    let (e5, n5) = open(&mut env, &m5, Q);
    env.tick(&m5, 5);
    withdraw_down_to(&mut env, &m5, 1_000_000_000);
    // The settlement leaves 10⁹ of the token claims (at most 1/1,000): it flushes the vest, then
    // vests the record's donation anew for the rebased shares (settle_it
    // `a_flushing_rebase_vests_a_prefunded_record_anew`).
    let epoch5 = env.market(&m5.market()).share_epoch;
    env.fund(&pda::share_epoch(&m5.market(), epoch5).0, M5_DONATION);
    move_by(&mut env, &m5, &w5, 4.0);
    let rb = settle_until(&mut env, &m5, &keeper, &e5, n5, 120);
    assert_eq!(rb.condition, 1);
    assert!(rb.vest_flushed > 0, "a flushing rebase");
    let m = env.market(&m5.market());
    assert_eq!(
        (m.share_epoch, m.vest_bal),
        (epoch5 + 1, M5_DONATION),
        "a rebase rolled the epoch; its donation vests anew"
    );
    // Through the rebase record: signed by the lender (KI65-REV-SEC1).
    let owner5 = m5.lender_keys().owner;
    let sync5 = with_records(
        &env,
        &m5.market(),
        &owner5,
        ix::sync_lender_signed(&owner5, &m5.market(), 8),
    );
    let kp5 = m5.lender.insecure_clone();
    ok(env.send(&[sync5], &[&kp5]));

    // ---- M6: a vest flush without a roll (AIR-07, `SolSettle.vest_flushed`) --------------------
    let m6 = env.mock_market(MarketSpec::default());
    let w6 = whale(&mut env, &m6);
    env.tick(&m6, 5);
    let (e6, n6) = open(&mut env, &m6, Q);
    env.tick(&m6, 5);
    // ⌊Q/999⌋ free tokens: the settlement leaves at most 1/1,000 of the token claims (a flush),
    // and the shares stay within `R_MAX` of them (no rebase; settle_it
    // `a_flush_without_a_roll_is_reported_in_sol_settle`). A trader sweep's lamports vest first,
    // so a crank reports a running vest before the flush.
    withdraw_down_to(&mut env, &m6, Q / 999);
    env.fund(&m6.keys.trader, 3_000_000);
    ok(env.send(&[sweep_trader_ix_for(&env, &m6.market())], &[]));
    env.tick(&m6, 2);
    assert!(env.market(&m6.market()).vest_bal > 0, "a running vest");
    move_by(&mut env, &m6, &w6, 4.0);
    let fl = settle_until(&mut env, &m6, &keeper, &e6, n6, 120);
    assert_eq!(fl.condition, 1);
    assert!(fl.vest_flushed > 0, "the flush is reported");
    let m = env.market(&m6.market());
    assert_eq!(
        (m.share_epoch, m.vest_bal),
        (0, 0),
        "flushed without an epoch roll"
    );

    // ---- M7: the last shares leave a running vest (R3B-2; `s_after = 0`) -----------------------
    let m7 = env.mock_market(MarketSpec::default());
    env.tick(&m7, 5);
    env.fund(&m7.keys.trader, 3_000_000);
    ok(env.send(&[sweep_trader_ix_for(&env, &m7.market())], &[]));
    env.tick(&m7, 2);
    assert!(env.market(&m7.market()).vest_bal > 0, "a running vest");
    let owner7 = m7.lender_keys().owner;
    let shares7 = env.lender(&m7.market(), &owner7).expect("lender").shares;
    let w = with_records(
        &env,
        &m7.market(),
        &owner7,
        ix::withdraw(&m7.lender_keys(), shares7, 0),
    );
    let kp7 = m7.lender.insecure_clone();
    ok(env.send(&[w], &[&kp7]));
    let m = env.market(&m7.market());
    assert_eq!(
        (m.total_shares.get(), m.vest_bal),
        (0, 0),
        "the last shares were paid the vest"
    );

    // ---- M1: the mint turns unsafe (a freeze authority appears) --------------------------------
    let mint = m1.mint();
    let mut acc = env.svm.get_account(&mint).expect("mint");
    acc.data[46..50].copy_from_slice(&1u32.to_le_bytes());
    acc.data[50..82].copy_from_slice(&[7u8; 32]);
    env.svm.set_account(mint, acc).expect("mint");
    ok(env.send(&[ix::flag_unsafe_mint(&mint)], &[]));
    env.tick(&m1, 2);

    finish(
        &mut env,
        "mock_lifecycle",
        TEST_VENUE,
        Some("services/indexer-litesvm scenarios::mock_lifecycle"),
    )
}

// ---- governance, the Emergency, the coverage key -------------------------------------------------

/// Proposes `change` (Governance), waits out the timelock and applies it (anyone).
pub fn govern(env: &mut Env, change: shortwin::state::ConfigChange) {
    let gov = env.governance.insecure_clone();
    ok(env.send(
        &[ix::propose_config(&gov.pubkey(), change.clone())],
        &[&gov],
    ));
    let wait = i64::from(env.config().timelock_s);
    env.warp(wait);
    ok(env.send(&[ix::apply_config_for(&change)], &[]));
}

/// The `mock_admin` session: every `ConfigChange` kind proposed and applied through the
/// timelock (the `Authorities` change installs the coverage key the real way), one proposal
/// cancelled by the Emergency, a position opened before a `Params` change that raises
/// `rent_protocol_bps` and liquidated after it (its locked share applies), the coverage key's
/// pause and clear and the Emergency's clear of a second pause, `set_venue_extras` on both legs,
/// and a settlement clock armed by a gap and disarmed by the reversal (`SettleDisarmed`).
#[allow(clippy::too_many_lines)]
pub fn mock_admin() -> Session {
    use shortwin::state::ConfigChange;
    let mut env = test_venue_env(CheckpointPolicy::AfterEvents);
    let keeper = env.funded_keypair();
    let kk = keeper.pubkey();
    let m = env.mock_market(MarketSpec::default());
    let w = whale(&mut env, &m);
    env.tick(&m, 5);
    // A position opened under the initial parameters.
    let (b, nb) = open(&mut env, &m, Q);
    env.tick(&m, 3);

    // A shorter timelock first (each later change then waits one day).
    govern(
        &mut env,
        ConfigChange::Timelock {
            timelock_s: shortwin::constants::MIN_TIMELOCK_S,
        },
    );
    env.tick(&m, 2);
    // The coverage key, installed by a real Authorities change.
    let c = env.config();
    let coverage_authority = env.coverage_authority.pubkey();
    govern(
        &mut env,
        ConfigChange::Authorities {
            governance: c.governance,
            pause_authority: c.pause_authority,
            coverage_authority,
            fee_recipient: c.fee_recipient,
        },
    );
    env.tick(&m, 2);
    // Params: a higher protocol rent share and open fee for positions opened from now on.
    let c = env.config();
    let mut params = c.params;
    params.rent_protocol_bps = 2_500;
    params.open_fee_bps = 30;
    govern(
        &mut env,
        ConfigChange::Params {
            params,
            tiers: c.tiers,
        },
    );
    env.tick(&m, 2);
    // The position opened before the change is liquidated after it: its locked share applies.
    move_by(&mut env, &m, &w, 2.3);
    let bk = b.key();
    every_second_until::<LiquidateEv>(&mut env, &m, 60, &keeper, |_| {
        liquidate_ix(&m, &kk, &bk, nb)
    })
    .expect("liquidation after the Params change");
    env.tick(&m, 3);
    // The other kinds.
    let c = env.config();
    govern(
        &mut env,
        ConfigChange::EmaParams {
            ema: c.ema_params.values(),
        },
    );
    env.tick(&m, 2);
    // The Emergency disables a venue at once; a later, unrelated change (Kink) leaves it
    // disabled on chain — the projection must too (M8-SEC-7; assertion 5 at the checkpoint after
    // the Kink's `ConfigApplied`). Re-enabling goes through the timelock (`EnableVenues`).
    let ea = env.pause_authority.insecure_clone();
    let cpmm_bit = shortwin::constants::VENUE_BIT_RAYDIUM_CPMM.trailing_zeros() as u8;
    ok(env.send(&[ix::disable_venue(&ea.pubkey(), cpmm_bit)], &[&ea]));
    let kink = env.config().kink.util_kink_bps;
    govern(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: kink - 100,
        },
    );
    assert_eq!(
        env.config().venue_enabled & shortwin::constants::VENUE_BIT_RAYDIUM_CPMM,
        0,
        "the Kink change left the disabled venue disabled"
    );
    env.tick(&m, 2);
    govern(
        &mut env,
        ConfigChange::EnableVenues {
            bits: shortwin::constants::VENUE_BIT_RAYDIUM_CPMM,
        },
    );
    env.tick(&m, 2);
    let c = env.config();
    govern(
        &mut env,
        ConfigChange::OracleFeed {
            account: c.pyth_sol_usd,
            feed_id: c.pyth_feed_id,
        },
    );
    env.tick(&m, 2);
    // A proposal the Emergency cancels.
    let gov = env.governance.insecure_clone();
    ok(env.send(
        &[ix::propose_config(
            &gov.pubkey(),
            ConfigChange::Kink {
                util_kink_bps: kink,
            },
        )],
        &[&gov],
    ));
    env.warp(60);
    ok(env.send(&[ix::cancel_config(&ea.pubkey())], &[&ea]));
    env.tick(&m, 2);

    // The coverage key pauses and clears; it pauses again and the Emergency clears.
    for paused in [true, false, true] {
        ok(env.set_coverage_pause(&m.market(), paused));
        env.tick(&m, 2);
    }
    ok(env.emergency_clear_coverage(&m.market()));
    env.tick(&m, 6);

    // Venue extras on both legs of the mock (the Emergency's bounded template).
    for leg in [0u8, 1] {
        let entry = shortwin::ExtraEntryArg {
            kind: 1,
            writable: false,
            key: Pubkey::new_unique(),
            seeds: vec![],
        };
        let i = shortwin_litesvm_tests::engine::set_venue_extras_ix(
            &ea.pubkey(),
            shortwin_litesvm_tests::engine::VENUE_HINT_MOCK,
            leg,
            vec![entry],
        );
        ok(env.send(&[i], &[&ea]));
    }

    // A settlement clock armed by a gap and disarmed when the gap reverses.
    let spot0 = env.spot(&m);
    let (d, nd) = open(&mut env, &m, Q / 2);
    env.tick(&m, 5);
    move_by(&mut env, &m, &w, 4.0);
    let dk = d.key();
    every_second_until::<shortwin::events::SettleArmed>(&mut env, &m, 60, &keeper, |env| {
        sol_settle_ix(env, &m, &kk, &dk, nd)
    })
    .expect("armed");
    env.fund_base(&m, &w, m.spec.base.saturating_mul(20));
    env.push_price(&m, &w, spot0);
    every_second_until::<shortwin::events::SettleDisarmed>(&mut env, &m, 90, &keeper, |env| {
        sol_settle_ix(env, &m, &kk, &dk, nd)
    })
    .expect("disarmed");
    env.tick(&m, 3);
    ok(env.close(&m, &d, nd));
    env.tick(&m, 2);
    finish(
        &mut env,
        "mock_admin",
        TEST_VENUE,
        Some("services/indexer-litesvm scenarios::mock_admin"),
    )
}

/// The `keeper_coverage_pause` session (O17's drill on the in-house keeper): the coverage key
/// installed through the timelock, three positions, a 45 s keeper outage (`CoverageGap`), the
/// keeper's `set_coverage_pause(true)` in the same tick, a liquidation under the pause, the
/// automatic unpause after `recovery_s`, and an open once the clear delay has passed.
pub fn keeper_coverage_pause() -> Session {
    let mut env = test_venue_env(CheckpointPolicy::AfterEvents);
    env.enable_coverage_authority();
    let auth = env.coverage_authority.insecure_clone();
    let mut h = Harness::new(env, Default::default());
    h.keeper.config_mut().coverage_pause.enabled = true;
    h.keeper.set_coverage_authority(Some(auth));
    let mm = h.with_env(|env| env.mock_market(MarketSpec::default()));
    let key = mm.market();
    let (b1, n1) = h.with_env(|env| open(env, &mm, Q));
    let (b2, n2) = h.with_env(|env| open(env, &mm, Q));
    h.with_env(|env| {
        let kp = b2.kp.insecure_clone();
        ok(env.send(&[add_margin_ix(&mm, &b2.key(), n2, 20_000_000_000)], &[&kp]));
    });
    h.seconds(5);
    let paused = |h: &Harness| {
        h.with_env(|env| {
            env.market(&key)
                .has_flag(shortwin::state::MARKET_FLAG_PAUSED_COVERAGE)
        })
    };
    // The outage.
    h.idle(45);
    h.second();
    assert!(paused(&h), "keeper_coverage_pause: the pause did not land");
    // A liquidation under the pause.
    let whale = h.with_env(|env| env.actor(&mm, 0, 5_000_000_000_000));
    let mut liquidated = false;
    for _ in 0..120 {
        h.with_env(|env| {
            env.warp(1);
            let target = env.spot(&mm) * 104 / 100;
            env.push_price(&mm, &whale, target);
        });
        h.tick();
        if !h.with_env(|env| position_open(env, &mm, &b1.key(), n1)) {
            liquidated = true;
            break;
        }
    }
    assert!(
        liquidated,
        "keeper_coverage_pause: no liquidation under the pause"
    );
    // Recovery: the keeper unpauses on its own.
    let mut unpaused = false;
    for _ in 0..700 {
        h.second();
        if !paused(&h) {
            unpaused = true;
            break;
        }
    }
    assert!(unpaused, "keeper_coverage_pause: never unpaused");
    h.seconds(u32::try_from(shortwin::constants::COVERAGE_CLEAR_DELAY_S).expect("delay") + 1);
    h.with_env(|env| {
        let _ = open(env, &mm, Q / 3);
    });
    h.seconds(3);
    h.with_env(|env| finish(env, "keeper_coverage_pause", TEST_VENUE, None))
}

// ---- the mainnet-fork sessions ---------------------------------------------------------------------

use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, PumpCoin, V4Fixture};
use shortwin_litesvm_tests::raydium::{RayFixture, RayMarket, RaySpec};
use shortwin_math::fees::Direction;

/// The program build the fork sessions run (`anchor build`).
pub const RELEASE: &str = "release";

/// Fartcoin's main AMM v4 pool and USELESS's CPMM pool (the dumped fixtures).
pub const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
pub const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);

/// AMM v4 `AmmInfo.status` "liquidity only" (swaps halted; the admin's `SetParams` status 4).
pub const V4_STATUS_LIQUIDITY_ONLY: u64 = 4;
/// AMM v4 `AmmInfo.status` "swap only" (trading again).
pub const V4_STATUS_SWAP_ONLY: u64 = 6;

/// The release build with the recording started before `initialize_config` (the default
/// arguments: every MVP venue enabled, the dumped deploy slots in `VenueGuard`).
pub fn release_env(policy: CheckpointPolicy) -> Env {
    let mut env = Env::new();
    env.start_recording_with(policy);
    let args = env.default_init_args();
    ok(env.initialize_config(args));
    env
}

/// A keeper covering every market with positions (the fork positions are below the global
/// budget's 1-SOL value-at-risk floor; `keeper-litesvm`'s `covering_all`).
pub fn covering_keeper(env: Env) -> Harness {
    let mut h = Harness::new(env, Default::default());
    h.keeper.config_mut().budget.var_floor_lamports = 0;
    h
}

/// `min(X/400, 2 SOL of notional)` on `pool` (the keeper scenarios' size).
pub fn q_of(env: &Env, pool: &AnyPool) -> u64 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture`, lends `40·q`, waits out the listing window, cranks once.
pub fn add_ray_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    ok(env.list_ray(&pool, fixture.hint()));
    let q = q_of(env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}

/// Opens `q` on `m` for a new borrower after a crank.
pub fn open_on(env: &mut Env, m: &impl VenueMarket, q: u64) -> (Borrower, u64) {
    env.crank(m);
    env.warp(1);
    let mut b = env.borrower();
    let (res, nonce) = env.open(m, &mut b, q);
    ok(res);
    (b, nonce)
}

/// Whether `(borrower, nonce)` of `m` is still open.
pub fn is_open(env: &Env, m: &impl VenueMarket, borrower: &Pubkey, nonce: u64) -> bool {
    env.svm
        .get_account(&pda::position(&m.market(), borrower, nonce).0)
        .is_some()
}

/// A real buy of `sol` lamports on `rm`'s pool by a fresh trader.
pub fn ray_buy(env: &mut Env, rm: &RayMarket, sol: u64) {
    let (tok, program) = rm.pool.token();
    let whale = fork::trader(env, &tok, program, sol + 1_000_000_000, 0);
    ok(fork::swap(
        env,
        &rm.pool,
        &whale,
        Direction::Buy,
        fork::Swap::ExactIn(sol, 0),
    ));
}

use shortwin_litesvm_tests::engine::VenueMarket;

/// The `raydium_lifecycle` session (the release build on the mainnet fork, the dumped AMM v4
/// and CPMM programs and pools, the keeper): Fartcoin's AMM v4 pool and USELESS's CPMM pool
/// listed by `create_market`; a close on AMM v4; real buys pump both pools and the keeper
/// liquidates a position on each (`ray_log` and CPMM `SwapEvent` legs); a position held to its
/// deadline while the Raydium admin halts the AMM v4 pool 700 s before it settles in S3 once the
/// continuous chain has run `s3_wait_s` (the keeper's 30 s cadence, `RouteChainChanged`); the
/// pool re-enabled.
pub fn raydium_lifecycle() -> Session {
    let mut env = release_env(CheckpointPolicy::AfterEvents);
    env.load_raydium_programs();
    env.set_fork_clock();
    let (v4, q4) = add_ray_market(&mut env, FART);
    let (cp, qc) = add_ray_market(&mut env, USELESS);
    env.fresh_oracle();
    let mut h = covering_keeper(env);
    // A close on AMM v4.
    let (a, na) = h.with_env(|env| open_on(env, &v4, q4));
    h.seconds(5);
    h.with_env(|env| ok(env.close(&v4, &a, na)));
    h.seconds(2);
    // A liquidation on each pool after real buys.
    let (b, nb) = h.with_env(|env| open_on(env, &v4, q4));
    let (c, nc) = h.with_env(|env| open_on(env, &cp, qc));
    h.seconds(5);
    let mut done = (false, false);
    for s in 0..240 {
        if s % 5 == 0 {
            h.with_env(|env| {
                if !done.0 {
                    let (_, y) = v4.reserves(&env.svm).expect("reserves");
                    ray_buy(env, &v4, y / 15);
                }
                if !done.1 {
                    let (_, y) = cp.reserves(&env.svm).expect("reserves");
                    ray_buy(env, &cp, y / 15);
                }
            });
        }
        h.second();
        done = h.with_env(|env| {
            (
                !is_open(env, &v4, &b.key(), nb),
                !is_open(env, &cp, &c.key(), nc),
            )
        });
        if done.0 && done.1 {
            break;
        }
    }
    assert!(done.0 && done.1, "raydium_lifecycle: liquidations {done:?}");
    h.seconds(3);
    // S3 on the halted AMM v4 pool.
    let (d, nd) = h.with_env(|env| open_on(env, &v4, q4 / 2));
    h.seconds(3);
    let deadline = h.with_env(|env| env.position(&v4, &d.key(), nd).deadline);
    let wait = deadline - 700 - h.now();
    h.idle(wait);
    h.seconds(3);
    h.with_env(|env| {
        ok(fork::raydium_admin_send(
            env,
            fork::v4_set_params_ix(v4.v4(), 0, V4_STATUS_LIQUIDITY_ONLY),
        ));
    });
    let mut settled = false;
    for _ in 0..1_000 {
        h.second();
        if !h.with_env(|env| is_open(env, &v4, &d.key(), nd)) {
            settled = true;
            break;
        }
    }
    assert!(settled, "raydium_lifecycle: no S3");
    h.with_env(|env| {
        ok(fork::raydium_admin_send(
            env,
            fork::v4_set_params_ix(v4.v4(), 0, V4_STATUS_SWAP_ONLY),
        ));
    });
    h.seconds(5);
    h.with_env(|env| finish(env, "raydium_lifecycle", RELEASE, None))
}

/// The `pool_switch` session (Raydium pool selection, ARCH §2.6, through the keeper): Fartcoin's
/// AMM v4 market with a planted AMM v4 sibling at half its depth; the incumbent is halted and
/// the keeper's fast path proposes the sibling (`PoolProposed`, the collection window); the
/// incumbent recovers inside the window, so the window closes without adoption
/// (`PoolSwitchCancelled`); halted again for good, the keeper proposes again and the sibling is
/// adopted at the window's end (`PoolSwitched`, kind fast), then a position opens on it.
pub fn pool_switch() -> Session {
    let mut env = release_env(CheckpointPolicy::AfterEvents);
    env.load_raydium_programs();
    env.set_fork_clock();
    let (rm, q) = add_ray_market(&mut env, FART);
    env.fresh_oracle();
    let key = rm.market();
    let lambda = {
        let counter = shortwin_litesvm_tests::raydium::lp_counter(&env.svm, &rm.pool);
        let supply = shortwin_litesvm_tests::raydium::mint_supply(
            &env.svm,
            &shortwin_litesvm_tests::raydium::lp_mint(&rm.pool),
        );
        u64::from(shortwin_math::pool_select::lambda_bps(counter, supply))
    };
    let spec = shortwin_litesvm_tests::pool_select::spec_like(&env.svm, &rm.pool, false, lambda)
        .scaled(50, 100);
    let sibling =
        shortwin_litesvm_tests::pool_select::plant_v4(&mut env, V4Fixture::Fartcoin, spec);
    let mut h = covering_keeper(env);
    h.seconds(3);
    let halt = |h: &Harness, status: u64| {
        h.with_env(|env| fork::set_v4_status(&mut env.svm, rm.v4(), status));
    };
    let fp_open = |h: &Harness| h.with_env(|env| env.market(&key).fp_keys[0] != Pubkey::default());
    let current = |h: &Harness| {
        h.with_env(|env| env.market(&key).pool_cache[shortwin::state::raydium_cache::RAY_POOL])
    };
    // Halted: the fast path proposes the sibling.
    halt(&h, 2);
    let mut proposed = false;
    for _ in 0..600 {
        h.second();
        if fp_open(&h) {
            proposed = true;
            break;
        }
    }
    assert!(proposed, "pool_switch: no proposal");
    // The incumbent recovers inside the window: no adoption.
    halt(&h, V4_STATUS_SWAP_ONLY);
    let mut cancelled = false;
    for _ in 0..600 {
        h.second();
        if !fp_open(&h) {
            cancelled = true;
            break;
        }
    }
    assert!(cancelled, "pool_switch: the window never closed");
    assert_eq!(current(&h), rm.v4().pool, "no adoption after the recovery");
    h.seconds(5);
    // Halted for good: proposed again and adopted.
    halt(&h, 2);
    let mut switched = false;
    for _ in 0..1_200 {
        h.second();
        if current(&h) == sibling.pool {
            switched = true;
            break;
        }
    }
    assert!(switched, "pool_switch: never switched");
    h.seconds(60);
    let adopted = shortwin_litesvm_tests::raydium::RayMarket {
        fixture: rm.fixture,
        pool: AnyPool::V4(sibling),
        keys: rm.keys,
        trader_wsol: rm.trader_wsol,
        lender: rm.lender.insecure_clone(),
        lender_account: rm.lender_account,
        amm_config: None,
    };
    h.with_env(|env| {
        let mut b = env.borrower();
        let _ = env.open(&adopted, &mut b, q / 4);
    });
    h.seconds(3);
    h.with_env(|env| finish(env, "pool_switch", RELEASE, None))
}

/// The `pump_synthetic_exit` session (pump-final §9; the release build on the mainnet fork with
/// the dumped pump, pump-fees and PumpSwap ELFs; no keeper): a pump-curve market on the
/// near-complete curve and a position of `q`; an outside buyer takes the curve down to
/// `rT = q/2`; the borrower's own close past `rT` buys `rT` on the curve and `q − rT` from the
/// would-be pool in one `buy_v3` (pump v3's synthetic migration: pump's `TradeEvent`,
/// `CompleteEvent` and `PostCompleteBuyEvent` under short.win's `close_short`) and moves the
/// market to `PumpMigrating`; a crank of the migrating market follows. Its thinned copy is
/// INGEST's fixture `pump_synthetic_exit.jsonl` ([`recorder::INGEST_FIXTURES`]), which replaces
/// the retired straddle bundle's capture.
pub fn pump_synthetic_exit() -> Session {
    use shortwin_litesvm_tests::pump::{CurveUser, PumpSpec};
    let mut env = release_env(CheckpointPolicy::AfterEvents);
    env.load_pump_programs();
    env.load_pump_statics();
    let c = env.load_pump_coin(PumpCoin::Near);
    env.set_pump_rent();
    env.set_pump_clock();
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q * 40), c);
    let (b, nb) = open_on(&mut env, &pm, q);
    env.warp(2);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt - q / 2, u64::MAX));
    env.warp(1);
    ok(env.close(&pm, &b, nb));
    let s = pm.curve.state(&env.svm);
    assert!(
        s.complete && s.post_complete_base_out == q - q / 2,
        "the close completed the curve synthetically (pcB {})",
        s.post_complete_base_out
    );
    assert!(!is_open(&env, &pm, &b.key(), nb));
    env.warp(1);
    env.crank(&pm);
    finish(&mut env, "pump_synthetic_exit", RELEASE, None)
}

/// The `pump_graduation` session (the release build on the mainnet fork with the dumped pump,
/// pump-fees and PumpSwap ELFs; the keeper with its default configuration): a pump-curve market on
/// the near-complete curve; an open and a close on the curve; a position past `rT` (`q > rT`, an
/// outside buyer took the curve down to `q/2`) and expired — the keeper's `expire` buys `rT` on the
/// curve and `q − rT` from the would-be pool by pump v3's synthetic migration, completing the curve
/// (`VenueStateChanged → PumpMigrating`), and in the same tick the keeper's `migrate_v2` and
/// transition move the market onto PumpSwap (pump-final §7; the straddle bundle and
/// `complete_curve` are retired); once PumpSwap's boost window is over, an open on the pool, a ×3
/// pump and the keeper's exit of it.
pub fn pump_graduation() -> Session {
    pump_session("pump_graduation", false)
}

/// [`pump_graduation`] with the keeper's expiry raced: a third party's `buy_v3(rT + δ)` completes
/// the curve synthetically after the keeper's simulation, before its expiry lands — the expiry fails
/// on chain (`VenueMigrating`, nothing written); the keeper's next tick migrates and transitions,
/// and it expires the position on the pool after the warm-up.
pub fn pump_graduation_raced() -> Session {
    pump_session("pump_graduation_raced", true)
}

fn pump_session(name: &str, raced: bool) -> Session {
    use shortwin_litesvm_tests::pamm::{PammMarket, PammUser};
    use shortwin_litesvm_tests::pump::{CurveUser, PumpSpec};
    let mut env = release_env(CheckpointPolicy::AfterEvents);
    env.load_pump_programs();
    env.load_pump_statics();
    let c = env.load_pump_coin(PumpCoin::Near);
    env.set_pump_rent();
    env.set_pump_clock();
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q * 40), c);
    let mut h = covering_keeper(env);
    h.chain().set_priority_fees(vec![5_000]);
    // An open and a close on the curve.
    let (a, na) = h.with_env(|env| open_on(env, &pm, q));
    h.seconds(3);
    h.with_env(|env| ok(env.close(&pm, &a, na)));
    h.seconds(2);
    // Past `rT`: an outsider buys the curve down to q/2; the position expires (the market cranked
    // at most every 10 minutes up to a second after the deadline, so the keeper's next tick holds a
    // fresh reference and its first send is the expiry).
    let (b, nb) = h.with_env(|env| open_on(env, &pm, q));
    h.with_env(|env| {
        env.warp(2);
        let u = CurveUser::new(env, &pm.curve);
        let rt = pm.curve.state(&env.svm).real_token_reserves;
        ok(u.buy(env, &pm.curve, rt - q / 2, u64::MAX));
        let deadline = env.position(&pm, &b.key(), nb).deadline;
        while env.now() < deadline + 1 {
            let step = (deadline + 1 - env.now()).min(600);
            env.warp(step);
            env.crank(&pm);
        }
    });
    if raced {
        // The third party's synthetic completion lands between the keeper's simulation and its
        // transaction, on the same bank (its accounts made first; recorded, with the blockhash the
        // keeper's transaction carries left valid).
        let c = pm.curve;
        let third = h.with_env(|env| {
            let u = CurveUser::new(env, &c);
            env.fund(&u.kp.pubkey(), 2_000 * 1_000_000_000);
            u
        });
        h.chain().before_next_send(move |env| {
            let rt = c.state(&env.svm).real_token_reserves;
            let ix = third.buy_v3_ix(&env.svm, &c, rt + rt / 4, u64::MAX);
            let tx = solana_transaction::Transaction::new_signed_with_payer(
                &[shortwin_litesvm_tests::pump::cu_limit_ix(400_000), ix],
                Some(&third.kp.pubkey()),
                &[&third.kp],
                env.svm.latest_blockhash(),
            );
            env.execute_recorded(tx.into())
                .expect("the third party's synthetic completion");
            assert!(
                c.state(&env.svm).complete,
                "the third party completed the curve"
            );
        });
    }
    let mut expired = false;
    for _ in 0..120 {
        h.second();
        if !h.with_env(|env| is_open(env, &pm, &b.key(), nb)) {
            expired = true;
            break;
        }
    }
    assert!(expired, "{name}: the position past rT was never expired");
    // Raced, the keeper's expiry is the one send that failed (on chain, `VenueMigrating`).
    let failed = h.chain().sent().iter().filter(|s| !s.ok).count();
    assert_eq!(
        failed,
        usize::from(raced),
        "{name}: the keeper's failed sends"
    );
    let pam = h.with_env(|env| PammMarket::graduated(&env.svm, &pm));
    h.seconds(10);
    // Opens wait out PumpSwap's boost window (`BoostInProgress`): the boost drained to dust or
    // `boost_window_s` after the graduation.
    for _ in 0..400 {
        let boosting = h.with_env(|env| {
            let params = env.config().params;
            let m = env.market(&pam.market());
            pam.pool.boost_amount(&env.svm) > params.boost_dust_lamports
                && env.now() < m.graduated_at + i64::from(params.boost_window_s)
        });
        if !boosting {
            break;
        }
        h.idle(29);
        h.second();
    }
    // On the pool: an open, a ×3 pump, the keeper's liquidation.
    let qp = q / 4;
    let (d, nd) = h.with_env(|env| open_on(env, &pam, qp));
    h.seconds(3);
    h.with_env(|env| {
        let whale = PammUser::new(env, &pam.pool, 20_000 * 1_000_000_000);
        ok(whale.push_price(env, &pam.pool, 3, 1));
    });
    let mut liquidated = false;
    for _ in 0..300 {
        h.second();
        if !h.with_env(|env| is_open(env, &pam, &d.key(), nd)) {
            liquidated = true;
            break;
        }
    }
    assert!(liquidated, "{name}: no exit on the pool");
    h.seconds(3);
    h.with_env(|env| finish(env, name, RELEASE, None))
}
