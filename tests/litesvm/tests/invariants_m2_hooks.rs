//! The M2 invariant hooks fire on a deliberately broken state (ARCH §8.3; the mutation tests
//! of I3, A26, I5–I7, I9–I13, I16, and since M10 I17's M6-SEC-3 graduation-halt clause on the
//! pump fork — I14 is structural, `tests/engine_it/structure.rs` and `tests/add_margin_it.rs`).
//!
//! Each test builds a real state through real instructions on the mock venue, then corrupts it
//! the way a program mutant would leave it — a stored field, an account, a token account, or
//! the events an instruction reports — and asserts that the hook names exactly the invariant
//! that was broken; the uncorrupted state passes. `check_all` is the per-market state hook,
//! `check_ix` the per-instruction hook against the snapshot just before the instruction
//! (`tests/litesvm/src/invariants.rs`, `invariants/positions.rs`).
//!
//! Rows marked **(patch)** exercise the hook parts the margin-crank-bench track added to
//! `tests/litesvm/src/invariants/positions.rs` (merged in M2): the I3 surviving-position /
//! new-position / exit-closes rows, A26 (snapshot rules), I6 exposure, I7 `pool_vault` delegate
//! after every instruction and `trader_base_ata`, I9 rent split, I13 newest-entry /
//! one-second-lag / re-quotes / `L_ema` step / exit charges, I16 route chains. The liquidation /
//! expiry rows of I11 and the persistence / S3-chain / `p'` rows of I12 have their own mutants in
//! `liquidate_it.rs`, `expire_it.rs` and `engine_it/hooks.rs`; this file's synthetic exits are
//! exact waterfalls (an expiry with the rent due and `K_e`, a settlement on an armed clock).

#![allow(clippy::result_large_err)]

// The engine tests' shared helpers (not every one is used here).
#[path = "engine_it/common.rs"]
#[allow(unused_imports)]
mod common;

use anchor_lang::prelude::Pubkey;
use shortwin::PauseScope;
use shortwin::events::{Close, Expire, Liquidate, Open, SolSettle};
use shortwin::state::{Market, ShortPosition, VenueKind, WalletExposure};
use shortwin_litesvm_tests::engine::set_route_broken_ix;
use shortwin_litesvm_tests::fixtures;
use shortwin_litesvm_tests::invariants::{self, IxEvents, TxSnapshot, check_all};
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use solana_account::Account;
use solana_signer::Signer;

use common::*;

// ---- fixtures -----------------------------------------------------------------------------

/// A corruption of the stored market.
type MarketMutant = Box<dyn Fn(&mut Market)>;
/// A corruption of a stored position.
type PositionMutant = fn(&mut ShortPosition);
/// A corruption of any state of a case.
type CaseMutant = Box<dyn Fn(&mut Case)>;

/// A market with one open position (a few cranked seconds after the open); hooks off, since
/// every test corrupts the state on purpose.
struct Case {
    env: Env,
    mm: MockMarket,
    b: Borrower,
    nonce: u64,
    key: Pubkey,
}

impl Case {
    fn new() -> Self {
        let (mut env, mm) = setup();
        env.crank(&mm);
        let mut b = env.borrower();
        let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
        env.tick(&mm, 2);
        env.check_invariants = false;
        // The `Open` events below are built by hand to aim at I3, I5, I7, I13: I19 (the open's
        // terms) has its own test, `i19_the_open_terms`.
        invariants::open_terms::allow_synthetic_opens(true);
        let key = pda::position(&mm.market(), &b.key(), nonce).0;
        Self {
            env,
            mm,
            b,
            nonce,
            key,
        }
    }

    fn market(&self) -> Market {
        self.env.market(&self.mm.market())
    }

    fn position(&self) -> ShortPosition {
        self.env.position(&self.mm, &self.b.key(), self.nonce)
    }

    fn set_market(&mut self, m: &Market) {
        let key = self.mm.market();
        let extra = lamports(&self.env, &key)
            - self
                .env
                .svm
                .minimum_balance_for_rent_exemption(Market::SPACE);
        fixtures::set_zc(&mut self.env.svm, &key, m, extra);
    }

    /// Writes `p` over the position with `lamports` in total.
    fn set_position(&mut self, p: &ShortPosition, lamports: u64) {
        let rent = self
            .env
            .svm
            .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
        fixtures::set_borsh(
            &mut self.env.svm,
            &self.key,
            p,
            ShortPosition::SPACE,
            lamports - rent,
        );
    }

    fn snapshot(&self) -> TxSnapshot {
        invariants::snapshot(&self.env.svm, &[self.mm.market()])
    }

    fn state(&self) -> Result<(), &'static str> {
        check_all(&self.env.svm, &self.mm.market()).map_err(|v| v.invariant)
    }

    fn ix(&self, pre: &TxSnapshot, ev: &IxEvents) -> Result<(), &'static str> {
        invariants::check_ix(&self.env.svm, pre, false, ev).map_err(|v| v.invariant)
    }

    /// A close event of the position that satisfies every close row (sums to `V`, the locked
    /// rent split, the charge at the live quote re-quoted on the pool now).
    fn good_close(&self) -> Close {
        self.good_close_for(&self.position())
    }

    /// [`Case::good_close`] for the position as `p` (it may be closed already).
    fn good_close_for(&self, p: &ShortPosition) -> Close {
        let b_live = quote_buy(&self.env, &self.mm, p.q);
        let rent = 1_000_000;
        Close {
            market: self.mm.market(),
            slot: 0,
            ts: 0,
            position: self.key,
            borrower: self.b.key(),
            q: p.q,
            buyback_cost: b_live,
            rent,
            rent_protocol: rent * u64::from(p.rent_protocol_bps) / 10_000,
            borrower_payout: p.v - b_live - rent,
            b_live,
            token_surplus: 0,
        }
    }

    fn open_event(&self, p: &ShortPosition, position: Pubkey) -> Open {
        Open {
            market: self.mm.market(),
            slot: 0,
            ts: 0,
            position,
            borrower: p.borrower,
            nonce: p.nonce,
            q: p.q,
            s: p.s,
            m: p.m,
            v: p.v,
            n_open: p.n_open,
            fee_open: p.fee_open,
            tier: p.tier,
            entry_price: p.entry_price,
            spot_wad: 0,
            ema_wad: 0,
            b_spot: 0,
            b_ema: 0,
            deadline: p.deadline,
            venue: VenueKind::Mock as u8,
            venue_rent: 0,
        }
    }

    /// Gives the position a live settlement clock that has run `settle_persist_n` EMA updates
    /// (I12's persistence part), so that a synthetic S1 / S2 of it is a valid one.
    fn arm(&mut self) {
        let mut m = self.market();
        m.ema_count += 100;
        self.set_market(&m);
        let mut p = self.position();
        p.s_armed = 1;
        p.s_count = m.ema_count - u64::from(p.settle_persist_n);
        p.s_last = self.env.now();
        let lamports = lamports(&self.env, &self.key);
        self.set_position(&p, lamports);
    }

    /// Removes the position account (what an exit does).
    fn close_account(&mut self) {
        self.env
            .svm
            .set_account(self.key, Account::default())
            .unwrap();
    }
}

fn k_s(p: &ShortPosition) -> u64 {
    (p.settle_keeper_fixed + p.v * u64::from(p.settle_keeper_bps) / 10_000)
        .min(p.settle_keeper_cap)
        .min(p.v)
}

fn liq(c: &Case, p: &ShortPosition) -> Liquidate {
    let b = p.n_open;
    let penalty = 2 * k_s(p);
    Liquidate {
        market: c.mm.market(),
        slot: 0,
        ts: 0,
        position: c.key,
        keeper: c.b.key(),
        cr_spot_bps: 0,
        cr_ema_bps: 0,
        b_live: b,
        b_ref: b,
        p_ref: 0,
        delta_bps: p.delta_liq_bps,
        buyback_cost: b,
        rent: 0,
        penalty,
        penalty_lender: penalty - k_s(p),
        penalty_protocol: 0,
        penalty_keeper: k_s(p),
        keeper_topup: 0,
        borrower_payout: p.v - b - penalty,
        expired: false,
        token_surplus: 0,
        b_live_risk: b,
        max_in: widen(b, p.delta_liq_bps),
    }
}

/// `⌊b·(10⁴ + bps)/10⁴⌋`.
fn widen(b: u64, bps: u16) -> u64 {
    b * (10_000 + u64::from(bps)) / 10_000
}

/// An exact expiry waterfall of `p` at `B = B̂_live = B̂_ref = n_open` (ARCH §3.7): the rent
/// due on the market's (post-accrual) basis, `π_ref` and the fee `⌈fee·B̂_ref/10⁴⌉`, and
/// `K_e = min(fixed + ⌊bps·B̂_ref/10⁴⌋, cap, V − B − r − e)`.
fn expire(c: &Case, p: &ShortPosition) -> Expire {
    let b = p.n_open;
    let fee = (b * u64::from(p.expiry_fee_bps)).div_ceil(10_000);
    let rent = shortwin_math::rent::due(&p.pos_rent(), &c.market().basis()).unwrap();
    let k_e = (p.expiry_keeper_fixed + b * u64::from(p.expiry_keeper_bps) / 10_000)
        .min(p.expiry_keeper_cap)
        .min(p.v - b - rent - fee);
    Expire {
        market: c.mm.market(),
        slot: 0,
        ts: 0,
        position: c.key,
        keeper: c.b.key(),
        b_live: b,
        b_ref: b,
        pi_ref: fee,
        buyback_cost: b,
        rent,
        expiry_fee: fee,
        keeper_reward: k_e,
        borrower_payout: p.v - b - rent - fee - k_e,
        b_live_risk: b,
        max_in: widen(b, p.delta_exp_bps),
        token_surplus: 0,
    }
}

fn settle(c: &Case, p: &ShortPosition, condition: u8, lenders: u64, lo: u64, hi: u64) -> SolSettle {
    SolSettle {
        market: c.mm.market(),
        slot: 0,
        ts: c.env.now(),
        position: c.key,
        keeper: c.b.key(),
        condition,
        b_live: 0,
        b_rs: 0,
        b_ema: 0,
        p_lo: 0,
        p_hi: 0,
        l_lo: 0,
        l_hi: 0,
        x_lo: lo,
        x_hi: hi,
        pprime_lo: 0,
        pprime_hi: 0,
        fv: 0,
        k_s: k_s(p),
        lender_amount: lenders,
        lump: 0,
        borrower_amount: p.v - k_s(p) - lenders,
        ema_age_s: 0,
        vest_flushed: 0,
    }
}

fn closes(e: Close) -> IxEvents {
    IxEvents {
        closes: vec![e],
        ..IxEvents::default()
    }
}

// ---- I4's coverage clause (O17) -----------------------------------------------------------

/// I4's coverage clause: a real `set_coverage_pause` signed by the installed `coverage_authority`
/// passes; the same instruction checked against a state in which it also changed another byte
/// of the market, another flag bit, a vault's lamports, another market or the config fails I4.
/// With the role disabled (the default) the clause still watches the Emergency's clear
/// (★OWNER-M7-3 (b): [`i4_coverage_emergency_clears_only`]).
#[test]
fn i4_coverage_authority_changes_only_its_flag() {
    let mut c = Case::new();
    let market = c.mm.market();
    let other = c.env.mock_market(MarketSpec::default());
    let other_key = other.market();
    let unset = c.snapshot();
    let cov = unset
        .coverage
        .as_ref()
        .expect("the Emergency's clear is watched");
    assert_eq!(
        (cov.authority, cov.emergency),
        (Pubkey::default(), c.env.pause_authority.pubkey()),
        "the role disabled, the Emergency set"
    );
    c.env.plant_coverage_authority();
    let kp = c.env.coverage_authority.insecure_clone();
    let ix = shortwin_litesvm_tests::ix::set_coverage_pause(
        &kp.pubkey(),
        &market,
        true,
        shortwin::events::COVERAGE_REASON_KEEPER,
    );
    let pre = invariants::snapshot(&c.env.svm, &[market, other.market()]);
    assert!(pre.coverage.is_some());
    let meta = ok(c.env.send(std::slice::from_ref(&ix), &[&kp]));
    let ev = IxEvents::of_instruction(&meta, 0, &ix);
    assert!(ev.coverage_pause && ev.signers.contains(&kp.pubkey()));
    let after = c.env.svm.clone();
    assert_eq!(c.ix(&pre, &ev), Ok(()), "the real instruction passes");
    let mutants: [CaseMutant; 6] = [
        Box::new(|c: &mut Case| {
            let mut m = c.market();
            m.tier ^= 1;
            c.set_market(&m);
        }),
        // M7-SEC-3: a set (not a clear) that moves `coverage_cleared_at`.
        Box::new(|c: &mut Case| {
            let mut m = c.market();
            m.coverage_cleared_at = c.env.now();
            c.set_market(&m);
        }),
        Box::new(|c: &mut Case| {
            let mut m = c.market();
            m.set_flag(shortwin::state::MARKET_FLAG_PAUSED_OPENS, true);
            c.set_market(&m);
        }),
        Box::new(|c: &mut Case| {
            let v = c.mm.keys.market_vault;
            let mut a = c.env.svm.get_account(&v).unwrap();
            a.lamports -= 1;
            c.env.svm.set_account(v, a).unwrap();
        }),
        Box::new(move |c: &mut Case| {
            let k = other.market();
            let mut a = c.env.svm.get_account(&k).unwrap();
            a.data[invariants::MARKET_FLAGS_OFFSET] |= 1 << 6;
            c.env.svm.set_account(k, a).unwrap();
        }),
        Box::new(|c: &mut Case| {
            let k = pda::config();
            let mut a = c.env.svm.get_account(&k).unwrap();
            a.data[11] ^= 1;
            c.env.svm.set_account(k, a).unwrap();
        }),
    ];
    for (i, m) in mutants.iter().enumerate() {
        c.env.svm = after.clone();
        m(&mut c);
        assert_eq!(c.ix(&pre, &ev), Err("I4"), "mutant {i}");
    }
    // M7-SEC-3: a clear of the set flag records the clock in `coverage_cleared_at` — the real
    // one passes; one that records nothing, or another second, does not.
    c.env.svm = after.clone();
    let clear = shortwin_litesvm_tests::ix::set_coverage_pause(
        &kp.pubkey(),
        &market,
        false,
        shortwin::events::COVERAGE_REASON_KEEPER,
    );
    let pre_clear = invariants::snapshot(&c.env.svm, &[market, other_key]);
    let meta = ok(c.env.send(std::slice::from_ref(&clear), &[&kp]));
    let ev_clear = IxEvents::of_instruction(&meta, 0, &clear);
    assert_eq!(c.market().coverage_cleared_at, c.env.now());
    assert_eq!(c.ix(&pre_clear, &ev_clear), Ok(()), "the real clear passes");
    let cleared = c.env.svm.clone();
    for at in [0, c.env.now() - 1] {
        c.env.svm = cleared.clone();
        let mut m = c.market();
        m.coverage_cleared_at = at;
        c.set_market(&m);
        assert_eq!(c.ix(&pre_clear, &ev_clear), Err("I4"), "cleared_at {at}");
    }
    // A landed `set_coverage_pause` signed by any other key (neither the role nor the
    // Emergency; the program refuses it, so only a broken check lets it land) violates the clause,
    // even when it changed nothing but the flag.
    c.env.svm = after;
    let foreign = IxEvents {
        signers: vec![Pubkey::new_unique()],
        ..ev
    };
    assert_eq!(c.ix(&pre, &foreign), Err("I4"));
    let governance = IxEvents {
        signers: vec![c.env.governance.pubkey()],
        ..foreign
    };
    assert_eq!(c.ix(&pre, &governance), Err("I4"));
}

/// ★OWNER-M7-3 (b) (owner decision of 2026-10-01): the Emergency vault's `set_coverage_pause`
/// clear — with the role installed and with it unset — passes I4's coverage clause: the flag
/// cleared, `coverage_cleared_at` at the clock, nothing else; the same instruction checked
/// against a state in which the flag stayed (or became) set, or in which it changed another byte,
/// another market or a vault, fails. So does a no-op clear that moved `coverage_cleared_at`.
#[test]
fn i4_coverage_emergency_clears_only() {
    for installed in [true, false] {
        let mut c = Case::new();
        let market = c.mm.market();
        let other = c.env.mock_market(MarketSpec::default());
        let other_key = other.market();
        // The flag set (by the coverage key, which is then removed when `installed` is false: the
        // flag a disabled role left set).
        c.env.plant_coverage_authority();
        ok(c.env.set_coverage_pause(&market, true));
        if !installed {
            let cfg = pda::config();
            let mut a = c.env.svm.get_account(&cfg).unwrap();
            a.data[96..128].copy_from_slice(Pubkey::default().as_ref());
            c.env.svm.set_account(cfg, a).unwrap();
            assert_eq!(c.env.config().coverage_authority, Pubkey::default());
        }
        c.env.warp(3);
        let ea = c.env.pause_authority.insecure_clone();
        let clear = shortwin_litesvm_tests::ix::set_coverage_pause(
            &ea.pubkey(),
            &market,
            false,
            shortwin::events::COVERAGE_REASON_KEEPER,
        );
        let pre = invariants::snapshot(&c.env.svm, &[market, other_key]);
        assert!(pre.coverage.is_some());
        let meta = ok(c.env.send(std::slice::from_ref(&clear), &[&ea]));
        let ev = IxEvents::of_instruction(&meta, 0, &clear);
        assert!(ev.coverage_pause && ev.signers.contains(&ea.pubkey()));
        assert!(
            !c.market()
                .has_flag(shortwin::state::MARKET_FLAG_PAUSED_COVERAGE)
        );
        assert_eq!(c.market().coverage_cleared_at, c.env.now());
        let after = c.env.svm.clone();
        assert_eq!(c.ix(&pre, &ev), Ok(()), "the real Emergency clear passes");
        let mutants: [CaseMutant; 5] = [
            // The flag left set by the Emergency's instruction.
            Box::new(|c: &mut Case| {
                let mut m = c.market();
                m.set_flag(shortwin::state::MARKET_FLAG_PAUSED_COVERAGE, true);
                m.coverage_cleared_at = 0;
                c.set_market(&m);
            }),
            // The clear without its clock.
            Box::new(|c: &mut Case| {
                let mut m = c.market();
                m.coverage_cleared_at = c.env.now() - 1;
                c.set_market(&m);
            }),
            Box::new(|c: &mut Case| {
                let mut m = c.market();
                m.set_flag(shortwin::state::MARKET_FLAG_PAUSED_OPENS, true);
                c.set_market(&m);
            }),
            Box::new(|c: &mut Case| {
                let v = c.mm.keys.market_vault;
                let mut a = c.env.svm.get_account(&v).unwrap();
                a.lamports -= 1;
                c.env.svm.set_account(v, a).unwrap();
            }),
            Box::new(move |c: &mut Case| {
                let k = other.market();
                let mut a = c.env.svm.get_account(&k).unwrap();
                a.data[invariants::MARKET_FLAGS_OFFSET] |= 1 << 6;
                c.env.svm.set_account(k, a).unwrap();
            }),
        ];
        for (i, m) in mutants.iter().enumerate() {
            c.env.svm = after.clone();
            m(&mut c);
            assert_eq!(
                c.ix(&pre, &ev),
                Err("I4"),
                "installed {installed}, mutant {i}"
            );
        }
        // A set signed by the Emergency alone (the program refuses it: `Unauthorized`, or
        // `CoverageAuthorityDisabled` while the role is unset) violates the clause if it lands.
        c.env.svm = after.clone();
        let pre_set = invariants::snapshot(&c.env.svm, &[market, other_key]);
        let mut m = c.market();
        m.set_flag(shortwin::state::MARKET_FLAG_PAUSED_COVERAGE, true);
        c.set_market(&m);
        assert_eq!(
            c.ix(&pre_set, &ev),
            Err("I4"),
            "installed {installed}: a set"
        );
        // A no-op clear (the flag already clear) records nothing.
        c.env.svm = after.clone();
        c.env.warp(1);
        let pre_noop = invariants::snapshot(&c.env.svm, &[market, other_key]);
        let at = c.market().coverage_cleared_at;
        let meta = ok(c.env.send(std::slice::from_ref(&clear), &[&ea]));
        let ev_noop = IxEvents::of_instruction(&meta, 0, &clear);
        assert_eq!(c.market().coverage_cleared_at, at);
        assert_eq!(c.ix(&pre_noop, &ev_noop), Ok(()), "a no-op clear passes");
        let mut m = c.market();
        m.coverage_cleared_at = c.env.now();
        c.set_market(&m);
        assert_eq!(
            c.ix(&pre_noop, &ev_noop),
            Err("I4"),
            "installed {installed}: a no-op clear that records the clock"
        );
    }
}

// ---- check_all ----------------------------------------------------------------------------

#[test]
fn baseline_passes() {
    let c = Case::new();
    assert_eq!(c.state(), Ok(()));
    let pre = c.snapshot();
    assert_eq!(c.ix(&pre, &IxEvents::default()), Ok(()));
}

/// I2: a position below `rent + v`.
#[test]
fn i2_position_below_rent_plus_v() {
    let mut c = Case::new();
    let p = c.position();
    let rent = c
        .env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    c.set_position(&p, rent + p.v - 1);
    assert_eq!(c.state(), Err("I2"));
    c.set_position(&p, rent + p.v);
    assert_eq!(c.state(), Ok(()));
}

/// I6: the market's aggregates against the positions — ΣB_Q (through a position's `q`),
/// by tier, `open_positions`, `total_notional_open`, `A_j`, `max_release`.
#[test]
fn i6_aggregates() {
    let mut c = Case::new();
    let good = c.market();
    let p0 = c.position();
    let l = lamports(&c.env, &c.key);
    // A position owing one more token than ΣB_Q (and its exposure) records.
    let mut p = p0.clone();
    p.q += 1;
    c.set_position(&p, l);
    assert_eq!(c.state(), Err("I6"));
    c.set_position(&p0, l);
    assert_eq!(c.state(), Ok(()));
    let tier = usize::from(p0.tier);
    let other = (tier + 1) % 4;
    for mutate in [
        (|m: &mut Market, t: usize, o: usize| {
            m.borrowed_q_by_tier[t] -= 1;
            m.borrowed_q_by_tier[o] += 1;
        }) as fn(&mut Market, usize, usize),
        |m, _, _| m.open_positions += 1,
        |m, _, _| m.total_notional_open -= 1,
        |m, _, _| m.rent_aggr[2].set(m.rent_aggr[2].get() + 1),
        |m, _, _| m.max_release -= 1,
    ] {
        let mut m = good;
        mutate(&mut m, tier, other);
        c.set_market(&m);
        assert_eq!(c.state(), Err("I6"));
    }
    c.set_market(&good);
    assert_eq!(c.state(), Ok(()));
}

/// I6 (patch): wallet exposure — `q_open` off by one, a record left at 0, a missing record,
/// a record away from its PDA.
#[test]
fn i6_wallet_exposure() {
    let mut c = Case::new();
    let key = pda::exposure(&c.mm.market(), &c.b.key()).0;
    let e0: WalletExposure = read::borsh(&c.env.svm, &key).unwrap();
    let write = |c: &mut Case, at: &Pubkey, e: &WalletExposure| {
        fixtures::set_borsh(&mut c.env.svm, at, e, WalletExposure::SPACE, 0);
    };
    let mut e = e0.clone();
    e.q_open += 1;
    write(&mut c, &key, &e);
    assert_eq!(c.state(), Err("I6"), "q_open off by one");
    write(&mut c, &key, &e0);
    assert_eq!(c.state(), Ok(()));
    // A stray record at 0 for a wallet without positions.
    let stray = Pubkey::new_unique();
    let (stray_key, bump) = pda::exposure(&c.mm.market(), &stray);
    let zero = WalletExposure {
        market: c.mm.market(),
        wallet: stray,
        q_open: 0,
        bump,
    };
    write(&mut c, &stray_key, &zero);
    assert_eq!(c.state(), Err("I6"), "a record left at 0");
    c.env
        .svm
        .set_account(stray_key, Account::default())
        .unwrap();
    // A copy of the real record away from its PDA.
    let away = Pubkey::new_unique();
    write(&mut c, &away, &e0);
    assert_eq!(c.state(), Err("I6"), "a record away from its PDA");
    c.env.svm.set_account(away, Account::default()).unwrap();
    // The record missing.
    c.env.svm.set_account(key, Account::default()).unwrap();
    assert_eq!(
        c.state(),
        Err("I6"),
        "no record for a borrower with a position"
    );
    write(&mut c, &key, &e0);
    assert_eq!(c.state(), Ok(()));
}

/// I7 (patch): `pool_vault` has no delegate after any instruction.
#[test]
fn i7_pool_vault_delegate_anytime() {
    let mut c = Case::new();
    let vault = c.mm.keys.pool_vault;
    let mut acc = c.env.svm.get_account(&vault).unwrap();
    let good = acc.clone();
    acc.data[72..76].copy_from_slice(&1u32.to_le_bytes());
    acc.data[76..108].copy_from_slice(&Pubkey::new_unique().to_bytes());
    acc.data[121..129].copy_from_slice(&5u64.to_le_bytes());
    c.env.svm.set_account(vault, acc).unwrap();
    assert_eq!(c.state(), Err("I7"));
    c.env.svm.set_account(vault, good).unwrap();
    assert_eq!(c.state(), Ok(()));
}

/// I13: a stale ring reprojection; (patch) the EMA that is not the newest entry; decision
/// state holding the current second or the future.
#[test]
fn i13_ring_and_time() {
    let mut c = Case::new();
    let good = c.market();
    let now = c.env.now();
    let head = usize::from(good.ring_head);
    let cases: Vec<(&str, MarketMutant)> = vec![
        (
            "stale (x, y)",
            Box::new(move |m| m.ring[head].y.set(m.ring[head].y.get() + 1)),
        ),
        (
            "P_ema is not the newest entry",
            Box::new(|m| m.ema_price.set(m.ema_price.get() + 1)),
        ),
        (
            "L_ema is not the newest entry",
            Box::new(|m| m.ema_liq.set(m.ema_liq.get() - 1)),
        ),
        (
            "ema_ts is not the newest entry",
            Box::new(|m| m.ema_ts -= 1),
        ),
        (
            "the last completed bucket is this second",
            Box::new(move |m| m.lb_sec = now),
        ),
        (
            "the open bucket is from the future",
            Box::new(move |m| {
                m.b_sec = now + 1;
                m.b_lo.set(1);
            }),
        ),
    ];
    for (what, mutate) in cases {
        let mut m = good;
        mutate(&mut m);
        c.set_market(&m);
        assert_eq!(c.state(), Err("I13"), "{what}");
    }
    // A future EMA with a matching newest entry: the lag row, not the entry row.
    let mut m = good;
    m.ema_ts = now + 3;
    m.ring[head].ts = now + 3;
    c.set_market(&m);
    assert_eq!(c.state(), Err("I13"), "an EMA from the future");
    c.set_market(&good);
    assert_eq!(c.state(), Ok(()));
}

// ---- check_ix -----------------------------------------------------------------------------

/// I3: position SOL leaves without an exit; (patch) `v` moves without `add_margin`'s shape;
/// a position survives the exit that names it; a position appears without an `Open`, or
/// unlike its `Open`.
#[test]
fn i3_position_sol() {
    let mut c = Case::new();
    let p0 = c.position();
    let l0 = lamports(&c.env, &c.key);
    let pre = c.snapshot();
    // Lamports leave without an exit event.
    c.set_position(&p0, l0 - 1);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Err("I3"));
    // v rises with m and lamports by the same amount: add_margin — fine.
    let mut p = p0.clone();
    p.v += 10;
    p.m += 10;
    c.set_position(&p, l0 + 10);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Ok(()));
    // v rises beyond the lamports that came in.
    c.set_position(&p, l0 + 9);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Err("I3"));
    // v rises without m.
    let mut p = p0.clone();
    p.v += 10;
    c.set_position(&p, l0 + 10);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Err("I3"));
    // v falls (SOL "leaves" the tracked balance while the lamports stay).
    let mut p = p0.clone();
    p.v -= 1;
    p.m -= 1;
    c.set_position(&p, l0);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Err("I3"));
    // A close that leaves the position alive.
    c.set_position(&p0, l0);
    assert_eq!(c.ix(&pre, &closes(c.good_close())), Err("I3"));
    c.close_account();
    assert_eq!(c.ix(&pre, &closes(c.good_close_for(&p0))), Ok(()));
    // A position appears (the old one gone): without an Open event, unlike its Open, and as
    // its Open says.
    let pre2 = c.snapshot();
    let fresh = Pubkey::new_unique();
    let mut stored = p0.clone();
    stored.s = quote_sell(&c.env, &c.mm, p0.q);
    stored.v = stored.s + stored.m;
    fixtures::set_borsh(
        &mut c.env.svm,
        &fresh,
        &stored,
        ShortPosition::SPACE,
        stored.v,
    );
    assert_eq!(c.ix(&pre2, &IxEvents::default()), Err("I3"), "no Open");
    let one = |o: Open| IxEvents {
        opens: vec![o],
        ..IxEvents::default()
    };
    assert_eq!(c.ix(&pre2, &one(c.open_event(&stored, fresh))), Ok(()));
    let mut bad = c.open_event(&stored, fresh);
    bad.deadline += 1;
    assert_eq!(c.ix(&pre2, &one(bad)), Err("I3"), "unlike its Open");
}

/// A26 (patch): any snapshot field of a surviving position changes — a locked term, the
/// deadline, `r_min`, `j_open`, the tier, the status; the arming state may change.
#[test]
fn a26_snapshot_rules() {
    let mut c = Case::new();
    let p0 = c.position();
    let l0 = lamports(&c.env, &c.key);
    let pre = c.snapshot();
    let mutants: Vec<(&str, PositionMutant)> = vec![
        ("penalty_bps", |p| p.penalty_bps += 1),
        ("cr_liq_bps", |p| p.cr_liq_bps -= 1),
        ("rent_protocol_bps", |p| p.rent_protocol_bps += 1),
        ("delta_liq_bps", |p| p.delta_liq_bps += 1),
        ("settle_keeper_cap", |p| p.settle_keeper_cap += 1),
        ("s3_wait_s", |p| p.s3_wait_s += 1),
        ("venue_fee_hard_cap_bps", |p| p.venue_fee_hard_cap_bps += 1),
        ("deadline", |p| p.deadline += 1),
        ("r_min", |p| p.r_min += 1),
        ("j_open", |p| p.j_open[1] += 1),
        ("tier", |p| p.tier = (p.tier + 1) % 4),
        ("status", |p| p.status = 1),
        ("s (proceeds)", |p| p.s += 1),
        ("n_open", |p| p.n_open += 1),
    ];
    for (what, mutate) in mutants {
        let mut p = p0.clone();
        mutate(&mut p);
        c.set_position(&p, l0);
        assert_eq!(c.ix(&pre, &IxEvents::default()), Err("A26"), "{what}");
    }
    // The arming state is sol_settle's to change.
    let mut p = p0.clone();
    p.s_armed = 1;
    p.s_count = 9;
    p.s_since = 5;
    p.s_last = 6;
    c.set_position(&p, l0);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Ok(()));
}

/// I5: an open that leaves utilisation above the cap.
#[test]
fn i5_utilisation_after_an_open() {
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    let ev = IxEvents {
        opens: vec![c.open_event(&p, c.key)],
        ..IxEvents::default()
    };
    let mut m = c.market();
    m.tokens_free = m.borrowed_q / 20; // U ≈ 95 % > 90 %
    c.set_market(&m);
    assert_eq!(c.ix(&pre, &ev), Err("I5"));
}

/// I7: after a leg — lamports or data left on the trader, lamports on `trader_wsol`, a
/// delegate on `pool_vault`; (patch) a pump market's `trader_base_ata` not empty.
#[test]
fn i7_trader_state_after_a_leg() {
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    // An Open whose other rows hold (its proceeds are the sale quote on the pool now).
    let mut open = c.open_event(&p, c.key);
    open.s = quote_sell(&c.env, &c.mm, p.q);
    let ev = IxEvents {
        opens: vec![open],
        ..IxEvents::default()
    };
    let trader = c.mm.keys.trader;
    c.env.fund(&trader, 1_000_000);
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "lamports on the trader");
    let rent1 = c.env.svm.minimum_balance_for_rent_exemption(1);
    c.env
        .svm
        .set_account(
            trader,
            Account {
                lamports: rent1,
                data: vec![1],
                owner: anchor_lang::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "data on the trader");
    c.env.svm.set_account(trader, Account::default()).unwrap();
    assert_eq!(c.ix(&pre, &ev), Ok(()), "a clean trader");
    let wsol = c.mm.trader_wsol;
    c.env.fund(&wsol, 2_000_000);
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "lamports on trader_wsol");
    c.env.svm.set_account(wsol, Account::default()).unwrap();
    // A delegate on the pool vault.
    let vault = c.mm.keys.pool_vault;
    let good = c.env.svm.get_account(&vault).unwrap();
    let mut acc = good.clone();
    acc.data[72..76].copy_from_slice(&1u32.to_le_bytes());
    acc.data[76..108].copy_from_slice(&Pubkey::new_unique().to_bytes());
    c.env.svm.set_account(vault, acc).unwrap();
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "a delegate after the leg");
    c.env.svm.set_account(vault, good).unwrap();
    // A pump market (the venue byte rewritten) whose trader base ATA holds tokens.
    let mut m = c.market();
    m.venue = VenueKind::PumpSwap as u8;
    let ata = m.trader_base_ata;
    c.set_market(&m);
    fixtures::set_token_account(
        &mut c.env.svm,
        &ata,
        &c.mm.mint(),
        &m.trader,
        0,
        TokenProgram::Spl,
    );
    assert_eq!(c.ix(&pre, &ev), Ok(()), "an empty trader_base_ata");
    fixtures::set_token_account(
        &mut c.env.svm,
        &ata,
        &c.mm.mint(),
        &m.trader,
        5,
        TokenProgram::Spl,
    );
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "trader_base_ata holds tokens");
    fixtures::set_token_account(
        &mut c.env.svm,
        &ata,
        &c.mm.mint(),
        &Pubkey::new_unique(),
        0,
        TokenProgram::Spl,
    );
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "trader_base_ata owner changed");
}

/// I7's `trader_wsol` clause per venue (FUZZ-M7-1): a pump-curve leg never names the
/// `trader_wsol` address (it trades native lamports; ARCH §2.4 rule 2 normalises the address on
/// the WSOL venues only), so a third party's pre-funding stays through it — the clause is a delta:
/// the leg leaves those lamports as they were, System-owned with no data. A curve leg that moved
/// them either way, or left data, fails I7; after a WSOL leg the address must hold nothing.
#[test]
fn i7_trader_wsol_on_a_curve_leg_is_a_delta() {
    let mut c = Case::new();
    let p = c.position();
    let wsol = c.mm.trader_wsol;
    let donated = 3_000_000;
    c.env.fund(&wsol, donated);
    let pre = c.snapshot();
    let mut open = c.open_event(&p, c.key);
    open.s = quote_sell(&c.env, &c.mm, p.q);
    let ev = IxEvents {
        opens: vec![open],
        ..IxEvents::default()
    };
    // The mock is a WSOL venue: its leg normalises the address, so the donation after it fails.
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "a WSOL leg left lamports");
    // The same state as a curve market's (the venue byte rewritten, the base ATA empty).
    let mut m = c.market();
    m.venue = VenueKind::PumpCurve as u8;
    let ata = m.trader_base_ata;
    c.set_market(&m);
    fixtures::set_token_account(
        &mut c.env.svm,
        &ata,
        &c.mm.mint(),
        &m.trader,
        0,
        TokenProgram::Spl,
    );
    assert_eq!(
        c.ix(&pre, &ev),
        Ok(()),
        "a curve leg leaves a third party's pre-funding where it is"
    );
    let set = |c: &mut Case, lamports: u64, data: Vec<u8>, owner: Pubkey| {
        c.env
            .svm
            .set_account(
                wsol,
                Account {
                    lamports,
                    data,
                    owner,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    };
    set(
        &mut c,
        donated + 1,
        Vec::new(),
        anchor_lang::system_program::ID,
    );
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "a curve leg added lamports");
    set(
        &mut c,
        donated - 1,
        Vec::new(),
        anchor_lang::system_program::ID,
    );
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "a curve leg took lamports");
    set(&mut c, donated, vec![0; 165], anchor_spl::token::ID);
    assert_eq!(c.ix(&pre, &ev), Err("I7"), "trader_wsol left open");
    set(&mut c, donated, Vec::new(), anchor_lang::system_program::ID);
    assert_eq!(c.ix(&pre, &ev), Ok(()));
    // A migrating curve's leg is a curve leg too.
    m.venue = VenueKind::PumpMigrating as u8;
    c.set_market(&m);
    assert_eq!(c.ix(&pre, &ev), Ok(()));
}

/// I9: waterfalls that do not conserve `V`; (patch) a close's rent split off the locked share.
#[test]
fn i9_waterfalls() {
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    c.close_account();
    assert_eq!(c.ix(&pre, &closes(c.good_close_for(&p))), Ok(()));
    let mut e = c.good_close_for(&p);
    e.borrower_payout += 1;
    assert_eq!(c.ix(&pre, &closes(e)), Err("I9"), "close legs");
    let mut e = c.good_close_for(&p);
    e.rent_protocol += 1;
    e.borrower_payout -= 1;
    e.rent += 1; // keeps the sum at V
    assert_eq!(c.ix(&pre, &closes(e)), Err("I9"), "rent split");
    let one = |l: Liquidate| IxEvents {
        liquidations: vec![l],
        ..IxEvents::default()
    };
    assert_eq!(c.ix(&pre, &one(liq(&c, &p))), Ok(()));
    let mut l = liq(&c, &p);
    l.borrower_payout -= 1;
    assert_eq!(c.ix(&pre, &one(l)), Err("I9"), "liquidation legs");
    let mut l = liq(&c, &p);
    l.penalty_protocol += 1;
    assert_eq!(c.ix(&pre, &one(l)), Err("I9"), "penalty split");
    let exp = |e: Expire| IxEvents {
        expiries: vec![e],
        ..IxEvents::default()
    };
    assert_eq!(c.ix(&pre, &exp(expire(&c, &p))), Ok(()));
    let mut e = expire(&c, &p);
    e.keeper_reward += 1;
    assert_eq!(c.ix(&pre, &exp(e)), Err("I9"), "expiry legs");
    let mut s = settle(&c, &p, 1, p.v - k_s(&p), 0, 0);
    s.borrower_amount = 1;
    let ev = IxEvents {
        settlements: vec![s],
        ..IxEvents::default()
    };
    assert_eq!(c.ix(&pre, &ev), Err("I9"), "settlement legs");
}

/// I10: an exit that names the oracle cache or the latch.
#[test]
fn i10_exits_read_no_oracle_or_latch() {
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    c.close_account();
    for named in [pda::oracle_cache(), pda::venue_guard()] {
        let ev = IxEvents {
            closes: vec![c.good_close_for(&p)],
            accounts: vec![c.mm.market(), named],
            ..IxEvents::default()
        };
        assert_eq!(c.ix(&pre, &ev), Err("I10"));
    }
    let ev = IxEvents {
        closes: vec![c.good_close_for(&p)],
        accounts: vec![c.mm.market()],
        ..IxEvents::default()
    };
    assert_eq!(c.ix(&pre, &ev), Ok(()));
}

/// I11 (event part): a liquidation's spend above the fixed bound, a penalty above its rate; an
/// expiry's spend above `δ_exp`, a fee above its rate.
#[test]
fn i11_exit_bounds() {
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    c.close_account();
    let one = |l: Liquidate| IxEvents {
        liquidations: vec![l],
        ..IxEvents::default()
    };
    let mut l = liq(&c, &p);
    let over = l.b_ref / 10;
    l.buyback_cost += over;
    l.b_live += over;
    l.borrower_payout -= over;
    assert_eq!(c.ix(&pre, &one(l)), Err("I11"), "spend above B̂_ref·(1 + δ)");
    let mut l = liq(&c, &p);
    let big = l.b_ref; // a 100 % penalty
    l.penalty += big;
    l.penalty_lender += big;
    l.borrower_payout -= big;
    assert_eq!(c.ix(&pre, &one(l)), Err("I11"), "penalty above its rate");
    let exp = |e: Expire| IxEvents {
        expiries: vec![e],
        ..IxEvents::default()
    };
    let mut e = expire(&c, &p);
    let over = e.b_ref / 10;
    e.buyback_cost += over;
    e.b_live += over;
    e.borrower_payout -= over;
    assert_eq!(
        c.ix(&pre, &exp(e)),
        Err("I11"),
        "spend above B̂_ref·(1 + δ_exp)"
    );
    let mut e = expire(&c, &p);
    e.expiry_fee += e.b_ref / 10;
    e.borrower_payout -= e.b_ref / 10;
    assert_eq!(c.ix(&pre, &exp(e)), Err("I11"), "fee above its rate");
}

/// I12 (event part): S1 take off `V − K_s`, an S2/S3 take outside its box, a box wider than
/// `fv_dev_bps`, `K_s` off the locked terms, an unknown condition, a liquidation that leaves
/// equity but pays the keeper below `K_s`.
#[test]
fn i12_settlements() {
    let mut c = Case::new();
    c.arm();
    let p = c.position();
    let pre = c.snapshot();
    c.close_account();
    let one = |s: SolSettle| IxEvents {
        settlements: vec![s],
        ..IxEvents::default()
    };
    let ks = k_s(&p);
    assert_eq!(c.ix(&pre, &one(settle(&c, &p, 1, p.v - ks, 0, 0))), Ok(()));
    assert_eq!(
        c.ix(&pre, &one(settle(&c, &p, 1, p.v - ks - 1, 0, 0))),
        Err("I12"),
        "S1 take"
    );
    let x = p.v / 2;
    assert_eq!(c.ix(&pre, &one(settle(&c, &p, 2, x, x, x))), Ok(()));
    assert_eq!(
        c.ix(&pre, &one(settle(&c, &p, 3, x, x + 10, x + 11))),
        Err("I12"),
        "outside the box"
    );
    assert_eq!(
        c.ix(&pre, &one(settle(&c, &p, 2, x, x, x + x / 10))),
        Err("I12"),
        "box wider than fv_dev_bps"
    );
    let mut s = settle(&c, &p, 1, p.v - ks, 0, 0);
    s.k_s -= 1;
    s.lender_amount += 1;
    assert_eq!(c.ix(&pre, &one(s)), Err("I12"), "K_s off the locked terms");
    assert_eq!(
        c.ix(&pre, &one(settle(&c, &p, 7, x, x, x))),
        Err("I12"),
        "unknown condition"
    );
    let mut l = liq(&c, &p);
    l.penalty_keeper = 1;
    l.penalty_lender = l.penalty - 1;
    let ev = IxEvents {
        liquidations: vec![l],
        ..IxEvents::default()
    };
    assert_eq!(c.ix(&pre, &ev), Err("I12"), "keeper floor");
}

/// I13: a close charged above its quote; (patch) a close whose `b_live` is not the quote on
/// the pre-state, an open whose proceeds are not the sale quote, a liquidation charged far
/// below its quote, an EMA step beyond its bound (`P_ema`, `L_ema`).
#[test]
fn i13_quotes_and_steps() {
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    c.close_account();
    let mut e = c.good_close_for(&p);
    e.buyback_cost += 1;
    e.borrower_payout -= 1;
    assert_eq!(
        c.ix(&pre, &closes(e)),
        Err("I13"),
        "charged above the quote"
    );
    let mut e = c.good_close_for(&p);
    e.b_live += 3;
    assert_eq!(
        c.ix(&pre, &closes(e)),
        Err("I13"),
        "b_live is not the pre-state quote"
    );
    let mut l = liq(&c, &p);
    l.b_live += 6;
    let ev = IxEvents {
        liquidations: vec![l],
        ..IxEvents::default()
    };
    assert_eq!(
        c.ix(&pre, &ev),
        Err("I13"),
        "liquidation charge 6 below its quote"
    );
    // The open's sale re-quote (its position must look like its event).
    let mut c = Case::new();
    let p = c.position();
    let pre = c.snapshot();
    let sale = quote_sell(&c.env, &c.mm, p.q);
    let with_s = |s: u64| {
        let mut o = c.open_event(&p, c.key);
        o.s = s;
        IxEvents {
            opens: vec![o],
            ..IxEvents::default()
        }
    };
    assert_eq!(
        c.ix(&pre, &with_s(sale + 6)),
        Err("I13"),
        "proceeds off the sale quote"
    );
    assert_eq!(
        c.ix(&pre, &with_s(sale - 1)),
        Err("I13"),
        "proceeds below the quote"
    );
    assert_eq!(c.ix(&pre, &with_s(sale)), Ok(()));
    // EMA steps.
    let good = c.market();
    for (what, mutate) in [
        (
            "P_ema doubled",
            (|m: &mut Market| m.ema_price.set(m.ema_price.get() * 2)) as fn(&mut Market),
        ),
        ("L_ema halved", |m| m.ema_liq.set(m.ema_liq.get() / 2)),
    ] {
        let mut m = good;
        m.ema_count += 1;
        mutate(&mut m);
        c.set_market(&m);
        assert_eq!(c.ix(&pre, &IxEvents::default()), Err("I13"), "{what}");
    }
    c.set_market(&good);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Ok(()));
}

/// I16: an observation recorded on a venue that cannot execute; (patch) the route chains —
/// `pool_fail` hit because of the route-broken flag, the S3 chain hit with a route, the S3
/// chain cleared without one; the legitimate moves pass.
#[test]
fn i16_observation_and_route_chains() {
    let mut c = Case::new();
    // Not executable: a recorded observation is a violation.
    update_knobs(&mut c.env, &c.mm.pool, |k: &mut Knobs| k.disabled = true);
    let pre = c.snapshot();
    let mut m = c.market();
    m.b_sec = c.env.now();
    m.b_lo.set(m.ema_price.get());
    m.b_hi.set(m.ema_price.get());
    m.b_llo.set(m.ema_liq.get());
    m.b_lhi.set(m.ema_liq.get());
    c.set_market(&m);
    assert_eq!(
        c.ix(&pre, &IxEvents::default()),
        Err("I16"),
        "record while halted"
    );
    // S3 chain cleared while not executable.
    let now = c.env.now();
    let mut m = pre.positions.markets[0].state;
    m.no_route_since = now - 5;
    m.last_no_route_obs = now - 1;
    c.set_market(&m);
    let pre = c.snapshot();
    m.no_route_since = 0;
    m.last_no_route_obs = 0;
    c.set_market(&m);
    assert_eq!(
        c.ix(&pre, &IxEvents::default()),
        Err("I16"),
        "S3 cleared without a route"
    );
    // Legit: the halted venue's chains are hit.
    m.no_route_since = now - 5;
    m.last_no_route_obs = now;
    m.pool_fail_since = now;
    m.last_pool_fail_obs = now;
    c.set_market(&m);
    assert_eq!(c.ix(&pre, &IxEvents::default()), Ok(()));

    // Executable, the route-broken flag set by Emergency.
    let mut c = Case::new();
    let ea = c.env.pause_authority.insecure_clone();
    ok(c.env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(c.mm.market()),
        )],
        &[&ea],
    ));
    let pre = c.snapshot();
    let now = c.env.now();
    let base = c.market();
    let mut m = base;
    m.pool_fail_since = now;
    m.last_pool_fail_obs = now;
    c.set_market(&m);
    assert_eq!(
        c.ix(&pre, &IxEvents::default()),
        Err("I16"),
        "pool_fail hit by the flag"
    );
    let mut m = base;
    m.no_route_since = now;
    m.last_no_route_obs = now;
    c.set_market(&m);
    assert_eq!(
        c.ix(&pre, &IxEvents::default()),
        Ok(()),
        "the flag is S3 evidence"
    );
    // Executable, no flag: an S3 hit is a violation.
    let mut c = Case::new();
    let pre = c.snapshot();
    let now = c.env.now();
    let mut m = c.market();
    m.no_route_since = now;
    m.last_no_route_obs = now;
    c.set_market(&m);
    assert_eq!(
        c.ix(&pre, &IxEvents::default()),
        Err("I16"),
        "S3 hit with a route"
    );
}

/// The hooks run on every instruction of the real flows without a false alarm — including a
/// crank that starts a chain, a flagged route, a close and a top-up (a smoke run with the
/// hooks on).
#[test]
fn real_flows_pass_every_hook() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 2);
    ok(env.send(
        &[shortwin_litesvm_tests::engine::add_margin_ix(
            &mm,
            &b.key(),
            nonce,
            1_000_000,
        )],
        &[&b.kp],
    ));
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    env.tick(&mm, 2);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    env.tick(&mm, 2);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = false);
    env.tick(&mm, 2);
    close_ok(&mut env, &mm, &b, nonce);
    env.tick(&mm, 1);
    assert!(env.hook_checkpoints > 10);
}

/// A copy of an event (Anchor events do not derive `Clone`).
fn dup(o: &Open) -> Open {
    use anchor_lang::{AnchorDeserialize, AnchorSerialize};
    let mut bytes = Vec::new();
    o.serialize(&mut bytes).expect("serialize");
    Open::deserialize(&mut bytes.as_slice()).expect("deserialize")
}

/// I19 (M7-SEC-6): the open's terms recomputed. A real open passes; the review's mutants — the
/// D4 margin without its EMA leg, a tier one step too generous, a wallet beyond its cap, the
/// open fee off by one, the deadline off by one, `N_open` off the venue's buyback — each fail it
/// (event and position kept consistent with each other, so I3 does not fire first).
#[test]
fn i19_the_open_terms() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    env.warp(1);
    env.check_invariants = false;
    invariants::open_terms::allow_synthetic_opens(false);
    let market = mm.market();
    let mut b = env.borrower();
    let pre = invariants::snapshot(&env.svm, &[market]);
    let (res, nonce) = env.open(&mm, &mut b, Q);
    let meta = ok(res);
    let o: Open = shortwin_litesvm_tests::events::one(&meta);
    let key = pda::position(&market, &b.key(), nonce).0;
    let p0 = env.position(&mm, &b.key(), nonce);
    let lamports0 = lamports(&env, &key);
    let run = |env: &Env, o: &Open| {
        let ev = IxEvents {
            opens: vec![dup(o)],
            ..IxEvents::default()
        };
        invariants::check_ix(&env.svm, &pre, false, &ev).map_err(|v| v.invariant)
    };
    assert_eq!(run(&env, &o), Ok(()), "the real open");
    let put = |env: &mut Env, p: &ShortPosition| {
        let rent = env
            .svm
            .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
        fixtures::set_borsh(
            &mut env.svm,
            &key,
            p,
            ShortPosition::SPACE,
            lamports0 - rent,
        );
    };
    // (1) M without the EMA leg (`margin_d4(s, n_open, None, …)`): M7-SEC-6's mutant A.
    let tp = env.config().tiers[usize::from(o.tier)];
    let m_spot = shortwin_math::risk::margin_d4(o.s, o.n_open, None, tp.cr_open_bps).unwrap();
    assert!(
        m_spot < o.m,
        "the EMA leg binds on an honest open ({m_spot} vs {})",
        o.m
    );
    let mut e = dup(&o);
    let mut p = p0.clone();
    (e.m, e.v, p.m, p.v) = (m_spot, o.s + m_spot, m_spot, o.s + m_spot);
    put(&mut env, &p);
    assert_eq!(run(&env, &e), Err("I19"), "D4 without its EMA leg");
    // (2) A tier one step better (C → B: the tier's terms with it).
    let better = o.tier.saturating_sub(1);
    let tb = env.config().tiers[usize::from(better)];
    let mut e = dup(&o);
    let mut p = p0.clone();
    e.tier = better;
    p.tier = better;
    p.cr_liq_bps = tb.cr_liq_bps;
    p.penalty_bps = u16::try_from(tb.penalty_bps).unwrap();
    p.max_liq_slip_bps = u16::try_from(tb.max_liq_slip_bps).unwrap();
    (p.rate_base_bps, p.rate_slope1_bps, p.rate_slope2_bps) =
        (tb.rate_base_bps, tb.rate_slope1_bps, tb.rate_slope2_bps);
    e.deadline = p0.t_open + i64::from(tb.max_duration_s);
    p.deadline = e.deadline;
    put(&mut env, &p);
    assert_ne!(better, o.tier);
    assert_eq!(run(&env, &e), Err("I19"), "a tier too generous");
    // (3) The open fee and (4) the deadline, off by one.
    for mutate in [
        (|e: &mut Open, p: &mut ShortPosition| {
            e.fee_open += 1;
            p.fee_open += 1;
        }) as fn(&mut Open, &mut ShortPosition),
        |e, p| {
            e.deadline += 1;
            p.deadline += 1;
        },
        |e, p| {
            e.n_open += 1;
            e.b_spot += 1;
            p.n_open += 1;
        },
    ] {
        let mut e = dup(&o);
        let mut p = p0.clone();
        mutate(&mut e, &mut p);
        put(&mut env, &p);
        assert_eq!(run(&env, &e), Err("I19"));
    }
    put(&mut env, &p0);
    assert_eq!(run(&env, &o), Ok(()));
    // (5) The wallet beyond `wallet_cap_bps` of OI_cap (M7-SEC-6's "wallet cap removed").
    let ek = pda::exposure(&market, &b.key()).0;
    let mut ex: WalletExposure = shortwin_litesvm_tests::read::borsh(&env.svm, &ek).unwrap();
    let saved = ex.clone();
    ex.q_open = ex.q_open.saturating_add(10_000 * Q);
    let lam = lamports(&env, &ek);
    let rent = env
        .svm
        .minimum_balance_for_rent_exemption(WalletExposure::SPACE);
    fixtures::set_borsh(&mut env.svm, &ek, &ex, WalletExposure::SPACE, lam - rent);
    assert_eq!(run(&env, &o), Err("I19"), "the wallet cap");
    fixtures::set_borsh(&mut env.svm, &ek, &saved, WalletExposure::SPACE, lam - rent);
    assert_eq!(run(&env, &o), Ok(()));
}

/// I17's M6-SEC-3 clause (ARCH §3.4 step 3, §8.3; the owner's decision of 2026-09-30) on the pump
/// fork (M10, G-I17): no curve open while PumpSwap's `GlobalConfig.disable_flags` bit 0
/// (`create_pool`, and with it `migrate_v2`) is set just before it. The real program refuses such
/// an open `GraduationHalted`; a real curve open with the flags clear passes every hook — also
/// against a snapshot with bit 3 alone (the pools' trading halt, which a curve open does not
/// read) — and the same open against the snapshot a program without the check would have met,
/// bit 0 set, fails I17. A PumpSwap open under bit 0 is not this clause's.
#[test]
fn i17_no_curve_open_while_graduation_halted() {
    use shortwin_litesvm_tests::engine::with_cu_limit;
    use shortwin_litesvm_tests::fork::PumpCoin;
    use shortwin_litesvm_tests::pamm;
    use shortwin_litesvm_tests::pump::PumpSpec;

    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q.saturating_mul(40)), c);
    let market = pm.market();
    let mut b = env.borrower();
    // The program: refused under bit 0, nothing kept.
    pamm::set_disable_flags(&mut env.svm, 0x01);
    let before = env.market(&market);
    assert_err(
        &env.open(&pm, &mut b, q).0,
        shortwin::errors::ShortwinError::GraduationHalted,
    );
    assert_eq!(env.market(&market).open_positions, before.open_positions);
    // The snapshots a program mutant could have met, then the real open (flags clear).
    let mut snap = |flags: u8| {
        pamm::set_disable_flags(&mut env.svm, flags);
        invariants::snapshot(&env.svm, &[market])
    };
    let halted = snap(0x01);
    let trading_halted = snap(0x08);
    let clear = snap(0x00);
    assert_eq!(
        (
            halted.pamm_disable_flags,
            trading_halted.pamm_disable_flags,
            clear.pamm_disable_flags
        ),
        (Some(0x01), Some(0x08), Some(0x00))
    );
    let ixs = with_cu_limit(
        &pm,
        open_short_ix(
            &pm,
            &b.key(),
            b.next_nonce,
            q,
            0,
            u64::MAX,
            u64::MAX,
            shortwin::venues::ExtrasSel::NONE,
            &[],
        ),
    );
    let meta = ok(env.send(&ixs, &[&b.kp]));
    let last = ixs.len() - 1;
    let ev = IxEvents::of_instruction(&meta, last, &ixs[last]);
    assert_eq!(ev.opens.len(), 1);
    assert_eq!(ev.opens[0].venue, VenueKind::PumpCurve as u8);
    let run = |pre: &TxSnapshot, ev: &IxEvents| {
        invariants::check_ix(&env.svm, pre, false, ev).map_err(|v| v.invariant)
    };
    assert_eq!(run(&clear, &ev), Ok(()), "the real open");
    assert_eq!(run(&trading_halted, &ev), Ok(()), "bit 3 alone");
    assert_eq!(run(&halted, &ev), Err("I17"), "a curve open under bit 0");
    // Every other bit with bit 0 set still fails; a PumpSwap open is not gated by this clause.
    let mut all = halted.clone();
    all.pamm_disable_flags = Some(0xff);
    assert_eq!(run(&all, &ev), Err("I17"));
    let mut pamm_open = dup(&ev.opens[0]);
    pamm_open.venue = VenueKind::PumpSwap as u8;
    assert_eq!(
        invariants::i17_no_curve_open_while_graduation_halted(&halted, &[pamm_open])
            .map_err(|v| v.invariant),
        Ok(())
    );
    // Without a PumpSwap `GlobalConfig` (no pump fork) the clause is vacuous.
    let mut none = halted.clone();
    none.pamm_disable_flags = None;
    assert_eq!(run(&none, &ev), Ok(()));
}
