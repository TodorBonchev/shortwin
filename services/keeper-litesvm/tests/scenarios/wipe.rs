//! Keeper duties 6 ("right after a full wipe") and 13 (the trader side) — M6; ARCH §3.3, §3.9
//! `sweep_trader` as built (M5-SEC-1 and its M6 remainder: the UVA-reading pattern), §7.1; ECON
//! §9.5; R4A-7 — on the release short.win with the dumped pump, pump-fees and PumpSwap ELFs, and on
//! the mock (test-venue). Every keeper transaction runs the invariant hooks after every
//! instruction.
//!
//! - **A full wipe on a PumpSwap cashback coin, and the market reopening**: the trader's own trades
//!   accrued cashback in `ATA(pAMM UVA(trader), WSOL)`; the only lender takes out every free token,
//!   a ×5 pump puts the last position into S1, and the keeper settles it (duty 4) — the pool is
//!   wiped. In the **same second** the keeper sends `[create_idempotent ATA(trader, WSOL), pAMM
//!   claim_cashback(user = trader), sweep_trader]` (no signature but its own): the cashback and the
//!   ATA's rent reach the wiped epoch's record and both post-wipe bits are set. A new lender
//!   deposits (refused `WipeSweepPending` before), and a new position opens: the market is live
//!   again.
//! - **A non-cashback PumpSwap market**: the post-wipe transaction is `sweep_trader` alone (the pAMM
//!   UVA's WSOL ATA does not exist), both bits at once.
//! - **A full wipe on a cashback curve** (legacy cashback coin: pump pays the trader's cashback to its
//!   pump UVA): `[pump claim_cashback(user = trader), sweep_trader]`, both bits, the deposit lands.
//! - **Duty 13 on the mock**: a donated `ATA(trader, WSOL)` and lamports sent to the `trader` PDA are
//!   swept to the lenders within `sweep.check_interval_s`, once.
//! - **The weekly cashback claim**: on a PumpSwap cashback coin with open positions, the keeper
//!   claims the accrued cashback with a sweep once it reaches `sweep.cashback_min_lamports`, and not
//!   again within `sweep.cashback_interval_s`.
//! - ★ **M6-SEC-2: a donated cashback ATA whose UVA was never created**: a curve market graduated
//!   by third parties only (nobody created the trader's pAMM accounts) is fully wiped on its pool;
//!   a stranger creates and funds `ATA(pAMM UVA(trader), WSOL)`. The pre-fix post-wipe transaction
//!   `[ATA(trader, WSOL), pAMM claim_cashback, sweep_trader]` fails 3012 (the UVA does not exist) —
//!   and with PumpSwap's `GlobalConfig` unreadable (a pAMM layout change, A63) the keeper's account
//!   preparation cannot create the UVA either. The keeper's post-wipe transaction now creates the
//!   UVA itself: it lands, both bits are set, the donation reaches the wiped epoch's record and the
//!   deposit lands.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::constants::{PUMP_AMM_GLOBAL_CONFIG, TOKEN_PROGRAM_ID, WSOL_MINT};
use shortwin::errors::ShortwinError;
use shortwin::events::{SolSettle, TraderSwept};
use shortwin::state::VenueKind;
use shortwin::state::{MARKET_FLAG_WIPE_LIVE, MARKET_FLAG_WIPE_REWARDS_CLAIMED};
use shortwin_keeper::config::SweepConfig;
use shortwin_keeper::sweep::{self, SweepFacts, SweepKind};
use shortwin_litesvm_tests::engine::{VenueMarket, crank_price_ix, sol_settle_ix};
use shortwin_litesvm_tests::errors::{assert_err, custom_code};
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::fork::{self, PumpCoin};
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammUser};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpSpec, wsol_ata};
use shortwin_litesvm_tests::read;
use shortwin_math::pool::{WIPE_REWARDS_CLAIMED, WIPE_TRADER_SWEPT};
use shortwin_services_common::AccountData;
use solana_signer::Signer;

use crate::common::*;

const SOL: u64 = 1_000_000_000;

/// A position size inside every default cap on `p` now (`pamm_it::common::q_for`): 1 % of the
/// base reserve, at most ≈ 10 SOL of notional.
fn q_for(env: &Env, p: &pamm::PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let y = r.effective_quote.max(1);
    let by_notional = base * u128::from(10 * SOL) / y;
    u64::try_from((base / 100).min(by_notional)).expect("u64")
}

/// A PumpSwap market on `coin` with `40·q` lent, warmed up (60 s of cranks), its boost waited out.
fn pamm_setup(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let q = q_for(&env, &p);
    let curve = shortwin_litesvm_tests::pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < shortwin_venue_state::pump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 60);
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            break;
        }
        env.warp(30);
        env.crank(&pm);
    }
    let q = q_for(&env, &pm.pool);
    (env, pm, q)
}

/// The seeding lender withdraws every free token.
fn withdraw_all_free(
    env: &mut Env,
    market: &Pubkey,
    lender: &solana_keypair::Keypair,
    k: &LenderKeys,
) {
    let m = env.market(market);
    let pool = m.pool();
    let want = m.tokens_free;
    let shares = env.lender(market, &lender.pubkey()).expect("lender").shares;
    let (mut lo, mut hi) = (0u128, shares);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if shortwin_math::pool::tokens_for_shares(&pool, mid).unwrap() <= u128::from(want) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    ok(env.send(&[ix::withdraw(k, lo, want)], &[lender]));
    assert_eq!(env.market(market).tokens_free, 0);
}

/// A new lender funded with `amount` tokens of `keys`' mint, and its deposit into the market
/// (with the epoch records the program asks for while a wipe record is live).
fn new_lender(
    env: &mut Env,
    keys: &MarketKeys,
    amount: u64,
) -> (solana_keypair::Keypair, LenderKeys) {
    let kp = env.funded_keypair();
    let program = if keys.token_program == shortwin::constants::TOKEN_2022_PROGRAM_ID {
        shortwin_litesvm_tests::tokens::TokenProgram::Token2022
    } else {
        shortwin_litesvm_tests::tokens::TokenProgram::Spl
    };
    let acc = fork::fund_token(env, &kp.pubkey(), &keys.mint, program, amount);
    let k = LenderKeys {
        owner: kp.pubkey(),
        mint: keys.mint,
        token_program: keys.token_program,
        owner_token_account: acc,
    };
    (kp, k)
}

fn deposit(
    env: &mut Env,
    market: &Pubkey,
    kp: &solana_keypair::Keypair,
    k: &LenderKeys,
    amount: u64,
) -> TxResultAlias {
    let m = env.market(market);
    let d = ix::with_remaining(
        ix::deposit(k, amount, 0),
        &ix::epoch_records(market, 0, m.share_epoch),
    );
    env.send(&[d], &[kp])
}

type TxResultAlias = shortwin_litesvm_tests::TxResult;

/// The wipe: `mm`'s last position (`b`, `nonce`) is left alone with every free token withdrawn and
/// its price pushed by `push`; the keeper ticks every second until it settles the position (S1).
/// Returns the report of the settling tick.
fn keeper_wipes(h: &mut Harness, market: &Pubkey, position: &Pubkey) -> TickReport {
    for _ in 0..240 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if r.exits.iter().any(|e| {
            e.position == *position
                && e.kind == ExitKind::SolSettle
                && e.result
                    .landed()
                    .is_some_and(|o| !shortwin_keeper::events::of::<SolSettle>(o).is_empty())
        }) {
            let m = h.with_env(|env| env.market(market));
            assert_eq!(m.total_shares.get(), 0);
            assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE), "a full wipe");
            return r;
        }
    }
    panic!("the keeper never settled the position");
}

/// The post-wipe sweep of a tick report (exactly one), with its `TraderSwept` event.
fn post_wipe(r: &TickReport, market: &Pubkey) -> (usize, TraderSwept) {
    let recs: Vec<_> = r
        .sweeps
        .iter()
        .filter(|s| s.market == *market && s.kind == SweepKind::PostWipe)
        .collect();
    assert_eq!(recs.len(), 1, "{:?}", r.sweeps);
    let out = recs[0]
        .result
        .landed()
        .expect("the post-wipe transaction landed");
    let ev = shortwin_keeper::events::of::<TraderSwept>(out)
        .pop()
        .expect("TraderSwept");
    (recs[0].instructions, ev)
}

/// ★ Duty 6, post-wipe, PumpSwap cashback coin (M5-SEC-1's M6 remainder; T-CASHBACK pAMM part):
/// the whole cycle from the wipe to the reopening, with the keeper doing every step it owns. The
/// post-wipe duty is switched off for the wipe itself, so the test sees the deposit refused
/// (`WipeSweepPending`) first; switched on, the keeper's next tick sends the post-wipe transaction.
#[test]
fn a_full_wipe_on_a_pumpswap_cashback_coin_is_unlocked_and_the_market_reopens() {
    let (mut env, pm, q) = pamm_setup(PammCoin::Cashback);
    let market = pm.market();
    // The trader's own trades accrue cashback.
    let mut b = env.borrower();
    let (res, n0) = env.open(&pm, &mut b, q);
    ok(res);
    env.warp(1);
    env.crank(&pm);
    ok(env.close(&pm, &b, n0));
    let mut b2 = env.borrower();
    let (res, n2) = env.open(&pm, &mut b2, q);
    ok(res);
    let lender = pm.lender.insecure_clone();
    withdraw_all_free(&mut env, &market, &lender, &pm.lender_keys());
    let whale = PammUser::new(&mut env, &pm.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pm.pool, 5, 1));
    let cashback = read::token_amount(&env.svm, &pm.cashback_ata()).unwrap_or(0);
    assert!(cashback > 0, "the trader's trades accrued cashback");
    let position = shortwin_litesvm_tests::pda::position(&market, &b2.key(), n2).0;
    // The wipe scenarios' last positions are below the 1-SOL value-at-risk floor: covered all the
    // same (the per-second `sol_settle` of the wipe is a covered market's duty;
    // `common::covering_all`).
    let mut h = Harness::new(
        env,
        covering_all(KeeperConfig {
            // The post-wipe duty is checked alone first: a deposit waits for it.
            sweep: SweepConfig {
                post_wipe: false,
                ..SweepConfig::default()
            },
            ..KeeperConfig::default()
        }),
    );
    let lender_sol0 = h.with_env(|env| env.market_vault(&market).lender_sol);
    let r = keeper_wipes(&mut h, &market, &position);
    assert!(r.sweeps.iter().all(|s| s.kind != SweepKind::PostWipe));
    let amount = q.saturating_mul(40);
    let (kp, k) = h.with_env(|env| new_lender(env, &pm.keys, amount));
    h.with_env(|env| {
        assert_err(
            &deposit(env, &market, &kp, &k, amount),
            ShortwinError::WipeSweepPending,
        )
    });
    h.keeper.config_mut().sweep.post_wipe = true;
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    // The post-wipe transaction in the same second: the ATA, the claim, the sweep.
    let (n, ev) = post_wipe(&r, &market);
    assert_eq!(
        n, 3,
        "create_idempotent ATA(trader, WSOL), pAMM claim_cashback, sweep_trader"
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    let ata_rent = h.with_env(|env| env.svm.minimum_balance_for_rent_exemption(165));
    assert_eq!(
        ev.lamports,
        cashback + ata_rent,
        "the cashback and the ATA's rent"
    );
    h.with_env(|env| {
        assert_eq!(
            read::token_amount(&env.svm, &pm.cashback_ata()).unwrap_or(0),
            0
        );
        let m = env.market(&market);
        assert!(m.has_flag(MARKET_FLAG_WIPE_REWARDS_CLAIMED));
        assert!(
            env.svm
                .get_account(&wsol_ata(&pm.trader()))
                .is_none_or(|a| a.data.is_empty())
        );
        assert!(env.market_vault(&market).lender_sol >= lender_sol0 + cashback);
    });

    // The market reopens: the deposit lands, then an open.
    h.with_env(|env| ok(deposit(env, &market, &kp, &k, amount)));
    assert!(
        !h.with_env(|env| env.market(&market))
            .has_flag(MARKET_FLAG_WIPE_LIVE)
    );
    for _ in 0..3 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            r.sweeps.iter().all(|s| s.kind != SweepKind::PostWipe),
            "once"
        );
    }
    h.with_env(|env| {
        env.crank(&pm);
        env.warp(1);
        let q2 = q_for(env, &pm.pool);
        let mut b3 = env.borrower();
        let (res, _) = env.open(&pm, &mut b3, q2);
        ok(res);
    });
}

/// Duty 6 on a non-cashback PumpSwap market: the pAMM UVA's WSOL ATA does not exist, so the
/// post-wipe transaction is `sweep_trader` alone and it sets both bits; the deposit lands.
#[test]
fn a_full_wipe_on_a_non_cashback_pumpswap_market_needs_only_the_sweep() {
    let (mut env, pm, q) = pamm_setup(PammCoin::Legacy261);
    let market = pm.market();
    let mut b = env.borrower();
    let (res, n) = env.open(&pm, &mut b, q);
    ok(res);
    let lender = pm.lender.insecure_clone();
    withdraw_all_free(&mut env, &market, &lender, &pm.lender_keys());
    let whale = PammUser::new(&mut env, &pm.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pm.pool, 5, 1));
    let position = shortwin_litesvm_tests::pda::position(&market, &b.key(), n).0;
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));
    let r = keeper_wipes(&mut h, &market, &position);
    let (n, ev) = post_wipe(&r, &market);
    assert_eq!(n, 1, "sweep_trader alone");
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    let amount = q;
    let (kp, k) = h.with_env(|env| new_lender(env, &pm.keys, amount));
    h.with_env(|env| ok(deposit(env, &market, &kp, &k, amount)));
}

/// Duty 6 with cashback outstanding on a **curve** market: after the keeper's S1 the trader's pump
/// UVA holds lamports above its rent, a deposit waits; the keeper's post-wipe transaction is `[pump
/// claim_cashback(user = trader), sweep_trader]` — the cashback reaches the wiped epoch's record,
/// both bits are set, and the deposit lands. T-WIPE-GATE (pump-final §14, C6): pump's v3 trades
/// refuse a live cashback coin and short.win no longer lists one (owner D1 (a)), so the balance is
/// seeded into the UVA of a non-cashback curve after the open, where a legacy cashback coin's
/// trades paid it; pump's `claim_cashback` pays any balance above the rent, donated lamports
/// included, and the wipe gate stays needed. The name is the audit tables'.
#[test]
fn a_full_wipe_on_a_cashback_curve_is_unlocked_by_the_pump_claim() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
    // The crowd buys 30 % of the curve first: a fresh curve holds no real SOL to pay a sale.
    let crowd = pump::CurveUser::new(&mut env, &c);
    let vt0 = c.state(&env.svm).virtual_token_reserves;
    ok(crowd.buy(&mut env, &c, vt0 / 10 * 3, u64::MAX));
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 400;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Fresh, q * 40), c);
    let market = pm.market();
    assert!(!c.state(&env.svm).is_cashback_coin, "a non-cashback curve");
    env.crank(&pm);
    env.warp(1);
    let mut b = env.borrower();
    let (res, n) = env.open(&pm, &mut b, q);
    ok(res);
    let uva = fork::pump_user_volume_accumulator(&pm.trader());
    let claimable = |env: &Env| {
        let a = env.svm.get_account(&uva).expect("the trader's pump UVA");
        a.lamports - env.svm.minimum_balance_for_rent_exemption(a.data.len())
    };
    assert_eq!(claimable(&env), 0, "a v3 sale pays no cashback");
    // The claimable balance (a donation to the UVA, which pump's claim pays out like cashback):
    // at least a 0-byte account's rent-exempt minimum — a claim below it leaves the trader PDA
    // rent-paying until the sweep in the same transaction drains it, valid as the keeper sends it,
    // but the harness's per-instruction replay (each prefix as a transaction of its own) cannot
    // run the claim alone.
    env.fund(&uva, 1_000_000);
    let cashback = claimable(&env);
    assert_eq!(cashback, 1_000_000);
    let lender = pm.lender.insecure_clone();
    withdraw_all_free(&mut env, &market, &lender, &pm.lender_keys());
    // The curve pumped ×5 (its real tokens allow it): the crowd buys on.
    let vt = c.state(&env.svm).virtual_token_reserves;
    let target = (u128::from(vt) * u128::from(vt) / 5).isqrt();
    let out = vt - u64::try_from(target).unwrap();
    ok(crowd.buy(&mut env, &c, out, u64::MAX));
    assert!(!c.state(&env.svm).complete);
    let position = shortwin_litesvm_tests::pda::position(&market, &b.key(), n).0;
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));
    let r = keeper_wipes(&mut h, &market, &position);
    let (n, ev) = post_wipe(&r, &market);
    assert_eq!(n, 2, "pump claim_cashback, sweep_trader");
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert!(ev.lamports >= cashback, "the cashback, through the trader");
    let amount = q;
    let (kp, k) = h.with_env(|env| new_lender(env, &pm.keys, amount));
    h.with_env(|env| ok(deposit(env, &market, &kp, &k, amount)));
}

/// Duty 13 on the mock: somebody creates and funds `ATA(trader, WSOL)` (no leg uses it) and sends
/// lamports to the `trader` PDA; within `sweep.check_interval_s` the keeper's `sweep_trader` closes
/// the ATA into the trader and moves everything to the lenders (vesting: `S > 0`); once.
#[test]
fn the_keeper_sweeps_what_lands_on_the_trader_side() {
    let mut env = Env::test_venue();
    let mm = env.mock_market(MarketSpec::default());
    let market = mm.market();
    let trader = mm.keys().trader;
    let donor = env.funded_keypair();
    let ata = wsol_ata(&trader);
    let payer = env.payer.pubkey();
    ok(env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &trader,
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        )],
        &[],
    ));
    env.fund(&ata, 5_000_000);
    ok(env.send(
        &[transfer_ix(&donor.pubkey(), &trader, 1_000_000)],
        &[&donor],
    ));
    let ata_lamports = read::lamports(&env.svm, &ata);
    let cfg = KeeperConfig {
        sweep: SweepConfig {
            check_interval_s: 10,
            ..SweepConfig::default()
        },
        ..KeeperConfig::default()
    };
    let mut h = Harness::new(env, cfg);
    let mut swept = None;
    for s in 0..12 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for rec in r.sweeps.iter().filter(|x| x.market == market) {
            assert_eq!(rec.kind, SweepKind::Sweep);
            assert!(swept.is_none(), "once");
            let out = rec.result.landed().expect("landed");
            let ev = shortwin_keeper::events::of::<TraderSwept>(out)
                .pop()
                .unwrap();
            assert_eq!(ev.lamports, ata_lamports + 1_000_000);
            swept = Some(s);
        }
    }
    assert!(swept.is_some_and(|s| s <= 10), "within the check interval");
    h.with_env(|env| {
        assert!(env.svm.get_account(&ata).is_none_or(|a| a.lamports == 0));
        assert_eq!(read::lamports(&env.svm, &trader), 0);
    });
}

/// Duty 13's cashback claim on a PumpSwap cashback coin with a position open: the trader's trades
/// accrue cashback; once it reaches `sweep.cashback_min_lamports` the keeper sends the claim with a
/// sweep (vesting to the lenders: `S > 0`), and not again within `sweep.cashback_interval_s`.
#[test]
fn the_cashback_claim_runs_once_per_interval() {
    let (mut env, pm, q) = pamm_setup(PammCoin::Cashback);
    let market = pm.market();
    let mut b = env.borrower();
    let (res, n) = env.open(&pm, &mut b, q);
    ok(res);
    env.warp(1);
    env.crank(&pm);
    ok(env.close(&pm, &b, n));
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
    let cashback = read::token_amount(&env.svm, &pm.cashback_ata()).unwrap_or(0);
    assert!(cashback > 0);
    let cfg = KeeperConfig {
        sweep: SweepConfig {
            check_interval_s: 5,
            cashback_min_lamports: cashback,
            ..SweepConfig::default()
        },
        ..KeeperConfig::default()
    };
    let mut h = Harness::new(env, cfg);
    let mut claims = 0;
    for _ in 0..30 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for rec in r.sweeps.iter().filter(|x| x.market == market) {
            assert_eq!(rec.kind, SweepKind::Cashback);
            assert_eq!(rec.instructions, 3);
            let out = rec.result.landed().expect("landed");
            let ev = shortwin_keeper::events::of::<TraderSwept>(out)
                .pop()
                .unwrap();
            assert!(ev.lamports >= cashback);
            claims += 1;
        }
    }
    assert_eq!(claims, 1, "once per interval");
    h.with_env(|env| {
        assert_eq!(
            read::token_amount(&env.svm, &pm.cashback_ata()).unwrap_or(0),
            0
        );
    });
}

/// SPL Token `Transfer` (discriminator 3) of `amount` from `src` to `dst`, signed by `owner`.
fn spl_transfer_ix(src: &Pubkey, dst: &Pubkey, owner: &Pubkey, amount: u64) -> Instruction {
    let mut data = vec![3u8];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*src, false),
            AccountMeta::new(*dst, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data,
    }
}

/// The keeper's [`sweep::fact_keys`] accounts of `market` as the VM holds them, read as
/// [`SweepFacts`].
fn sweep_facts(env: &Env, market: &shortwin::state::Market) -> SweepFacts {
    let accs: Vec<Option<AccountData>> = sweep::fact_keys(market)
        .iter()
        .map(|k| {
            env.svm.get_account(k).map(|a| AccountData {
                lamports: a.lamports,
                owner: a.owner,
                executable: a.executable,
                data: a.data,
            })
        })
        .collect();
    SweepFacts::of(market, &accs)
}

/// ★ M6-SEC-2 (the security review's test shape). A curve market whose graduation only third
/// parties touched — an outside buy completes the curve, an outside `migrate_v2`, an outside bare
/// `crank_price` moves the market onto its pool — has no pAMM UVA for its trader (`crank_price`
/// creates no account). Its last position settles S1 on the pool with every free token withdrawn:
/// a full wipe. A stranger then creates `ATA(pAMM UVA(trader), WSOL)` (an ATA of any address) and
/// puts 0.001 SOL of WSOL in it: `sweep_trader` leaves the rewards bit clear, a deposit waits
/// (`WipeSweepPending`), and the post-wipe transaction as the keeper built it before the fix,
/// `[create_idempotent ATA(trader, WSOL), pAMM claim_cashback, sweep_trader]`, fails 3012 —
/// `AccountNotInitialized` on the UVA (R-VEN Update H7). The keeper's `prepare_pamm_trader` would
/// create the UVA of a PumpSwap market whose pool and `GlobalConfig` parse; here PumpSwap's
/// `GlobalConfig` is made unreadable (its discriminator flipped: a pAMM layout change, A63), so
/// nothing but the post-wipe transaction can. The keeper's post-wipe transaction now starts with
/// `init_user_volume_accumulator(payer = keeper, user = trader)`: it lands in the keeper's first
/// second, sets both bits, credits the donation (and the keeper's ATA rent) to the wiped epoch's
/// record, and the deposit lands.
#[test]
fn m6_sec_2_a_donated_cashback_ata_without_the_pamm_uva_is_unlocked_by_the_keeper() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q * 40), c);
    let market = pm.market();
    env.crank(&pm);
    env.warp(1);
    let mut b = env.borrower();
    let (res, n) = env.open(&pm, &mut b, q);
    ok(res);
    let lender = pm.lender.insecure_clone();
    withdraw_all_free(&mut env, &market, &lender, &pm.lender_keys());

    // Third parties graduate it; nobody creates the trader's pAMM accounts.
    env.warp(1);
    let crowd = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(crowd.buy(&mut env, &pm.curve, rt, u64::MAX));
    let migrator = env.funded_keypair();
    let mig = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[pump::cu_limit_ix(600_000), mig], &[&migrator]));
    env.warp(1);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    assert_eq!(env.market(&market).venue_kind(), Some(VenueKind::PumpSwap));
    let pam = PammMarket::graduated(&env.svm, &pm);
    let uva = pam.trader_uva();
    assert!(
        env.svm.get_account(&uva).is_none_or(|a| a.data.is_empty()),
        "the trader has no pAMM UVA"
    );

    // A ×5 pump held on the pool; S1 after the warm-up wipes the pool.
    let whale = PammUser::new(&mut env, &pam.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pam.pool, 5, 1));
    let kp = env.funded_keypair();
    let mut s1 = None;
    for _ in 0..180 {
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pam)], &[]));
        let ix = sol_settle_ix(&env, &pam, &kp.pubkey(), &b.key(), n);
        let meta = ok(env.send(&[ix], &[&kp]));
        if let Some(ev) = shortwin_litesvm_tests::events::of::<SolSettle>(&meta).pop() {
            s1 = Some(ev);
            break;
        }
    }
    assert_eq!(s1.expect("settled").condition, 1, "S1");
    let m = env.market(&market);
    assert_eq!(m.total_shares.get(), 0);
    assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE), "a full wipe");
    assert!(
        env.svm.get_account(&uva).is_none_or(|a| a.data.is_empty()),
        "still no pAMM UVA"
    );

    // The stranger's donation into the UVA's WSOL ATA.
    let donation = 1_000_000;
    let stranger = env.funded_keypair();
    ok(env.send(
        &[fork::create_ata_idempotent_ix(
            &stranger.pubkey(),
            &uva,
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        )],
        &[&stranger],
    ));
    let w = fork::fund_wsol(&mut env, &stranger.pubkey(), donation);
    ok(env.send(
        &[spl_transfer_ix(
            &w,
            &pam.cashback_ata(),
            &stranger.pubkey(),
            donation,
        )],
        &[&stranger],
    ));

    // The sweep alone sets the trader bit only; a deposit waits.
    let amount = q;
    let (dkp, dk) = new_lender(&mut env, &pm.keys, amount);
    assert_err(
        &deposit(&mut env, &market, &dkp, &dk, amount),
        ShortwinError::WipeSweepPending,
    );
    let m = env.market(&market);
    let facts = sweep_facts(&env, &m);
    assert_eq!(facts.pamm_claimable, donation);
    assert!(!facts.pamm_uva);

    // The pre-fix post-wipe transaction: no UVA creation, so pAMM's claim fails 3012.
    let payer = env.payer.pubkey();
    let pre_fix = sweep::post_wipe_ixs(
        &payer,
        &market,
        &m,
        &SweepFacts {
            pamm_uva: true,
            ..facts
        },
    );
    assert_eq!(pre_fix.len(), 3);
    let res = env.send(&pre_fix, &[]);
    assert_eq!(
        custom_code(&res),
        Some(3012),
        "AccountNotInitialized: {:?}",
        res.as_ref().err().map(|e| e.meta.logs.clone())
    );
    assert_err(
        &deposit(&mut env, &market, &dkp, &dk, amount),
        ShortwinError::WipeSweepPending,
    );

    // PumpSwap's GlobalConfig unreadable (a layout change): the keeper cannot prepare the pool
    // accounts; its post-wipe transaction creates the UVA itself and lands.
    fork::patch(&mut env.svm, &PUMP_AMM_GLOBAL_CONFIG, |d| d[0] ^= 0xff);
    let lender_sol0 = env.market_vault(&market).lender_sol;
    let ata_rent = env.svm.minimum_balance_for_rent_exemption(165);
    let mut h = Harness::new(env, KeeperConfig::default());
    let r = h.second().clone();
    assert!(
        r.pamm_accounts.iter().all(|(k, _)| *k != market),
        "no account preparation: {:?}",
        r.pamm_accounts
    );
    let (n_ixs, ev) = post_wipe(&r, &market);
    assert_eq!(
        n_ixs, 4,
        "init_user_volume_accumulator, ATA(trader, WSOL), pAMM claim_cashback, sweep_trader"
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert_eq!(
        ev.lamports,
        donation + ata_rent,
        "the donation and the keeper's ATA rent"
    );
    h.with_env(|env| {
        assert!(
            env.svm
                .get_account(&uva)
                .is_some_and(|a| a.owner == shortwin::constants::PUMP_AMM_PROGRAM_ID),
            "the UVA exists"
        );
        assert_eq!(
            read::token_amount(&env.svm, &pam.cashback_ata()).unwrap_or(0),
            0
        );
        assert_eq!(
            env.market_vault(&market).lender_sol - lender_sol0,
            donation + ata_rent,
            "credited to the wiped epoch's record"
        );
        ok(deposit(env, &market, &dkp, &dk, amount));
        assert!(!env.market(&market).has_flag(MARKET_FLAG_WIPE_LIVE));
    });
}
