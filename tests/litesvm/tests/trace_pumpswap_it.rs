//! T-TRACE on the PumpSwap venue (ARCHITECTURE §6.2, §6.3, §8.2; C1-1, C1-8): the exact
//! instruction-trace composition (which program at which height, and how many entries) of every
//! PumpSwap-path instruction and the `→ PumpSwap` graduation transition, on the dumped PumpSwap,
//! pump and pump-fees ELFs, with every invariant hook after every instruction. The whole-
//! transaction counts, sizes and CU are in `tests/bench/m6_cu` (`trace.md`); this file pins the
//! per-instruction shapes the bench's expected counts rest on.
//!
//! Every count is the instruction's own: the instruction plus every CPI at any depth
//! (`MAX_INSTRUCTION_TRACE_LENGTH = 64`), and its peak stack height (1 = top level; the mainnet
//! limit is 5).

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use litesvm::types::TransactionMetadata;
use shortwin::events::VenueStateChanged;
use shortwin::state::VenueKind;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{custom_code, ok};
use shortwin_litesvm_tests::fork::{self, PumpCoin};
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammPool, PammUser};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec, cu_limit_ix};
use shortwin_litesvm_tests::{Env, events, pda};
use shortwin_venue_state::pump as vpump;
use solana_signer::Signer;

const SOL: u64 = 1_000_000_000;
const MATURE: PammCoin = PammCoin::Mature;
const SPL261: PammCoin = PammCoin::Legacy261;

// ---- helpers --------------------------------------------------------------------------------

/// The trace entries of top-level instruction `i`: the instruction plus every CPI at any depth.
fn entries(meta: &TransactionMetadata, i: usize) -> usize {
    1 + meta.inner_instructions.get(i).map_or(0, Vec::len)
}

/// The deepest stack height the transaction reached (1 = top level).
fn peak(meta: &TransactionMetadata) -> u8 {
    meta.inner_instructions
        .iter()
        .flatten()
        .map(|i| i.stack_height)
        .max()
        .unwrap_or(1)
}

/// 1 % of the base reserve, a notional of at most ≈ 8 SOL (10 SOL until M6R-1: on the `mature`
/// pool, whose third-party LP puts its locked depth at 92.98 % of `√k`, `X₀` is `0.93·X`, and a
/// wallet's two 10-SOL opens exceed its wallet cap, 25 % of the fee-scaled OI cap).
fn q_for(env: &Env, p: &PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let y = r.effective_quote.max(1);
    let q = (base / 100).min(base * u128::from(8 * SOL) / y);
    u64::try_from(q).expect("u64")
}

fn setup(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let q = q_for(&env, &p);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 60);
    wait_out_boost(&mut env, &pm);
    let q = q_for(&env, &pm.pool);
    (env, pm, q)
}

fn wait_out_boost(env: &mut Env, pm: &PammMarket) {
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            return;
        }
        env.warp(30);
        ok(env.send(&[crank_price_ix(pm)], &[]));
    }
}

fn open(env: &mut Env, pm: &PammMarket, b: &mut Borrower, q: u64) -> (TransactionMetadata, u64) {
    let nonce = b.next_nonce;
    let ix = open_short_ix(
        pm,
        &b.key(),
        nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(&[cu_limit_ix(400_000), ix], &[&kp]));
    b.next_nonce += 1;
    (meta, nonce)
}

// ---- tests ----------------------------------------------------------------------------------

/// A wallet's first PumpSwap open: the position and exposure inits, the `sell` leg (short.win 1 →
/// pAMM 2 → pump-fees / Token / System / event 3), the margin and open-fee transfers — 23 entries,
/// peak 3. A repeat borrower is 22 (no exposure init).
#[test]
fn open_short_trace() {
    for (coin, first) in [(MATURE, 23), (SPL261, 23)] {
        let (mut env, pm, q) = setup(coin);
        let mut b = env.borrower();
        let (meta, _) = open(&mut env, &pm, &mut b, q);
        assert_eq!(entries(&meta, 1), first, "{coin:?}: first open");
        assert_eq!(peak(&meta), 3, "{coin:?}: peak 3");
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let (meta, _) = open(&mut env, &pm, &mut b, q);
        assert_eq!(entries(&meta, 1), first - 1, "{coin:?}: repeat");
        assert_eq!(peak(&meta), 3);
    }
}

/// A PumpSwap close: the `buy` leg (17 entries on a Token-2022 pool, 16 on an SPL pool without a
/// creator fee), peak 3.
#[test]
fn close_short_trace() {
    for (coin, n) in [(MATURE, 17), (SPL261, 17)] {
        let (mut env, pm, q) = setup(coin);
        let mut b = env.borrower();
        let (_, nonce) = open(&mut env, &pm, &mut b, q);
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let ix = close_short_ix(&pm, &b.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
        let kp = b.kp.insecure_clone();
        let meta = ok(env.send(&[cu_limit_ix(400_000), ix], &[&kp]));
        assert_eq!(entries(&meta, 1), n, "{coin:?}: close");
        assert_eq!(peak(&meta), 3);
    }
}

/// A liquidation after a real ×2.8 pump and an expiry: the `buy` leg, 17 entries, peak 3.
#[test]
fn liquidate_and_expire_trace() {
    let (mut env, pm, q) = setup(MATURE);
    let mut b = env.borrower();
    let (_, nonce) = open(&mut env, &pm, &mut b, q);
    env.warp(1);
    let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut env, &pm.pool, 28, 10));
    let keeper = env.funded_keypair();
    let mut liq = None;
    for _ in 0..900 {
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let ix = liquidate_ix(&pm, &keeper.pubkey(), &b.key(), nonce);
        let res = env.send(&[cu_limit_ix(400_000), ix], &[&keeper]);
        match custom_code(&res) {
            None => {
                liq = Some(ok(res));
                break;
            }
            Some(c) if c == u32::from(shortwin::errors::ShortwinError::NotLiquidatable) => {}
            Some(c) => panic!("liquidate failed with {c}"),
        }
    }
    let meta = liq.expect("liquidated");
    assert_eq!(entries(&meta, 1), 17, "liquidate");
    assert_eq!(peak(&meta), 3);

    // A fresh expiry.
    let (mut env, pm, q) = setup(MATURE);
    let mut b = env.borrower();
    let (_, nonce) = open(&mut env, &pm, &mut b, q);
    let deadline = env.position(&pm, &b.key(), nonce).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    env.warp(1);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    let keeper = env.funded_keypair();
    let ix = expire_ix(&pm, &keeper.pubkey(), &b.key(), nonce);
    let meta = ok(env.send(&[cu_limit_ix(400_000), ix], &[&keeper]));
    assert_eq!(entries(&meta, 1), 17, "expire");
    assert_eq!(peak(&meta), 3);
}

/// A steady `crank_price` is 2 entries (its `PriceCranked` event self-CPI), peak 2; `sol_settle`
/// as a no-op is 1 entry (no event), peak 1.
#[test]
fn crank_and_settle_noop_trace() {
    let (mut env, pm, q) = setup(MATURE);
    let mut b = env.borrower();
    let (_, nonce) = open(&mut env, &pm, &mut b, q);
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&pm)], &[]));
    assert_eq!(entries(&meta, 0), 2, "crank");
    assert_eq!(peak(&meta), 2);
    let keeper = env.funded_keypair();
    let ix = sol_settle_ix(&env, &pm, &keeper.pubkey(), &b.key(), nonce);
    let meta = ok(env.send(&[ix], &[&keeper]));
    assert!(events::of::<shortwin::events::SolSettle>(&meta).is_empty());
    assert_eq!(entries(&meta, 0), 1, "sol_settle no-op");
    assert_eq!(peak(&meta), 1);
}

/// A PumpSwap listing (`create_market(Auto)` with the pAMM init set): the ATA-program creation of
/// `trader_base_ata` and pAMM's `init_user_volume_accumulator` — 15 entries, peak 3.
#[test]
fn listing_trace() {
    let (mut env, p) = Env::fork_pamm_coin(MATURE);
    let creator = env.funded_keypair();
    let ix = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
    let meta = ok(env.send(&[ix], &[&creator]));
    assert_eq!(entries(&meta, 0), 15, "PumpSwap listing");
    assert_eq!(peak(&meta), 3);
}

/// `sweep_trader`'s PumpSwap token leg (a donation): 4 entries, peak 2.
#[test]
fn sweep_trader_token_leg_trace() {
    let (mut env, pm, _) = setup(MATURE);
    // Donate lamports and tokens to the trader so the token leg has work.
    let griefer = env.funded_keypair();
    let mut data = vec![2u8, 0, 0, 0];
    data.extend_from_slice(&1_000_000u64.to_le_bytes());
    ok(env.send(
        &[anchor_lang::solana_program::instruction::Instruction {
            program_id: anchor_lang::system_program::ID,
            accounts: vec![
                anchor_lang::solana_program::instruction::AccountMeta::new(griefer.pubkey(), true),
                anchor_lang::solana_program::instruction::AccountMeta::new(pm.trader(), false),
            ],
            data,
        }],
        &[&griefer],
    ));
    let u = PammUser::new(&mut env, &pm.pool, 0);
    fork::set_token_amount(&mut env.svm, &u.base, 777);
    let mut td = vec![12u8];
    td.extend_from_slice(&777u64.to_le_bytes());
    td.push(6);
    let kp = u.kp.insecure_clone();
    ok(env.send(
        &[anchor_lang::solana_program::instruction::Instruction {
            program_id: pm.keys.token_program,
            accounts: vec![
                anchor_lang::solana_program::instruction::AccountMeta::new(u.base, false),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    pm.keys.mint,
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new(
                    pm.trader_base_ata,
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    u.kp.pubkey(),
                    true,
                ),
            ],
            data: td,
        }],
        &[&kp],
    ));
    let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
    assert_eq!(entries(&meta, 0), 4, "sweep_trader token leg");
    assert_eq!(peak(&meta), 2);
}

/// The `PumpCurve → PumpSwap` graduation transition crank: it writes the pool cache, rewrites the
/// ring and clears the chains — 4 entries (`VenueStateChanged`, `RouteChainChanged`, `PriceCranked`
/// event self-CPIs), peak 2.
#[test]
fn graduation_transition_crank_trace() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm: PumpMarket = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q * 40), c);
    env.warp(1);
    // Complete and migrate the curve.
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt, u64::MAX));
    let migrator = env.funded_keypair();
    ok(env.send(
        &[
            cu_limit_ix(600_000),
            pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve),
        ],
        &[&migrator],
    ));
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&pm)], &[]));
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(
        (ev.from, ev.to),
        (VenueKind::PumpCurve as u8, VenueKind::PumpSwap as u8),
        "the transition"
    );
    assert_eq!(entries(&meta, 0), 4, "the transition crank");
    assert_eq!(peak(&meta), 2);
    assert_eq!(
        env.market(&pm.market()).venue_kind().expect("venue"),
        VenueKind::PumpSwap
    );
}
