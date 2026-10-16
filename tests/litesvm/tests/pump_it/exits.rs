//! Keeper exits, the trader's token leg and the venue-upgrade latch on the curve (ARCH §3.6,
//! §3.7, §3.9, §2.3.2 "Venue-upgrade latch"; M5):
//!
//! - an expiry and a liquidation buy back on the real curve: pump's charge exactly (within the
//!   quote), `q` back in the lending vault through `trader_base_ata`, the trader clean;
//! - `sweep_trader`'s pump token leg: tokens donated to `trader_base_ata` go to the lending
//!   vault as a donation (`T` unchanged), lamports donated to the trader to lenders; a token leg
//!   with a foreign account is refused;
//! - tokens donated to `trader_base_ata` before a close are normalised into the lending vault
//!   (never counted as the leg's delivery), and the close stays exact;
//! - lamports donated to the `trader_wsol` address survive curve legs (the curve never names it)
//!   until `sweep_trader` (FUZZ-M7-1);
//! - the pump, pump-fees and PumpSwap latch bits each block curve opens (`VenueUpgraded`, the
//!   curve's mask), while cranks, `sol_settle` and closes keep working (I10).
//!
//! Every instruction goes through `Env::send`: the invariant hooks run after each one.

use shortwin::constants::{GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES};
use shortwin::events::{Expire, Liquidate, TraderSwept};
use shortwin_litesvm_tests::engine::{expire_ix, liquidate_ix};
use shortwin_litesvm_tests::ix;
use solana_keypair::Keypair;

use crate::common::*;

/// An expiry by a keeper right after the deadline: the curve buyback, the waterfall's expiry
/// fee and keeper reward, `q` back to the lending vault.
#[test]
fn an_expiry_buys_back_on_the_curve() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
    crank(&mut env, &pm);
    env.warp(1);
    let kp = keeper(&mut env);
    let charge = pm.curve.charge_buy(&env.svm, q).unwrap();
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let ix = expire_ix(&pm, &kp.pubkey(), &b.key(), nonce);
    let cu = pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU);
    let meta = ok(env.send(&[cu, ix], &[&kp]));
    eprintln!("expire on the curve: {} CU", meta.compute_units_consumed);
    let ev: Expire = events::one(&meta);
    assert_eq!(ev.buyback_cost, charge.cost, "B = pump's charge");
    assert!(ev.b_live >= charge.cost && ev.b_live - charge.cost <= 3);
    assert!(ev.expiry_fee > 0 && ev.keeper_reward > 0);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
    assert_trader_clean(&env, &pm);
}

/// A liquidation after a real pump on the curve: a buyer takes 40 % of `vT` (the price × 2.8,
/// past Tier D's `CR_liq` of 175 % from 400 %); the keeper cranks every second until both legs
/// are liquidatable, then liquidates on the curve.
#[test]
fn a_liquidation_after_a_pump_buys_back_on_the_curve() {
    let (mut env, pm, q) = setup(PumpCoin::Fresh);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    let u = CurveUser::new(&mut env, &pm.curve);
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    ok(u.buy(&mut env, &pm.curve, vt / 10 * 4, u64::MAX));
    let kp = keeper(&mut env);
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let mut done = None;
    for _ in 0..900 {
        env.warp(1);
        crank(&mut env, &pm);
        let ix = liquidate_ix(&pm, &kp.pubkey(), &b.key(), nonce);
        let cu = pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU);
        match env.send(&[cu, ix], &[&kp]) {
            Ok(meta) => {
                eprintln!("liquidate on the curve: {} CU", meta.compute_units_consumed);
                done = Some(meta);
                break;
            }
            Err(f) => {
                let code = custom_code(&Err(f));
                assert_eq!(code, Some(code_of(E::NotLiquidatable)), "only not-yet");
            }
        }
    }
    let meta = done.expect("liquidated within 15 min");
    let ev: Liquidate = events::one(&meta);
    assert!(!ev.expired);
    assert!(ev.buyback_cost > 0 && ev.buyback_cost <= ev.b_live);
    assert!(ev.penalty > 0);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
    assert_trader_clean(&env, &pm);
}

fn code_of(e: E) -> u32 {
    u32::from(e)
}

/// `sweep_trader` on a curve market (ARCH §3.9, M5 token leg): donated tokens move from
/// `trader_base_ata` to the lending vault (a donation, `T` unchanged), donated lamports to the
/// lenders; the event reports both. A token leg naming another account is refused.
#[test]
fn sweep_trader_recovers_the_token_leg() {
    let (mut env, pm, _) = setup(PumpCoin::T22_115);
    donate_tokens(&mut env, &pm, &pm.trader_base_ata, 1_234_567);
    env.fund(&pm.trader(), 7_000_000);
    // A foreign account in the token leg.
    let mut bad = pump::sweep_trader_pump_ix(&env, &pm);
    let at = bad
        .accounts
        .iter()
        .position(|a| a.pubkey == pm.trader_base_ata)
        .unwrap();
    bad.accounts[at].pubkey = Pubkey::new_unique();
    assert_err(&env.send(&[bad], &[]), E::AccountMismatch);
    let t0 = market(&env, &pm).tokens_free;
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let meta = ok(env.send(&[pump::sweep_trader_pump_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!((ev.tokens, ev.lamports), (1_234_567, 7_000_000));
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, 1_234_567);
    assert_eq!(market(&env, &pm).tokens_free, t0, "a donation: T unchanged");
    assert_trader_clean(&env, &pm);
    // Nothing left: an empty sweep succeeds and reports nothing.
    env.warp(1);
    let meta = ok(env.send(&[pump::sweep_trader_pump_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!((ev.tokens, ev.lamports), (0, 0));
}

/// FUZZ-M7-1 (the fuzzer's minimised reproducer `tests/fuzz/corpus/fuzz-m7-1-curve-trader-wsol.json`,
/// `[donate trader_wsol, open]`, as a regression): lamports sent to a curve market's `trader_wsol`
/// address survive the curve's legs — the curve trades native lamports and never names that
/// address (ARCH §2.4 rule 2 normalises it on the WSOL venues only) — so I7's clause there is a
/// delta (§2.4 rule 5): an open and a close land under the hooks, the donation where it was;
/// `sweep_trader` then moves it to the lenders as a lump.
#[test]
fn a_donation_to_the_trader_wsol_address_survives_curve_legs_until_swept() {
    let (mut env, pm, q) = setup(PumpCoin::Legacy115);
    let wsol = pda::trader_wsol(&pm.market()).0;
    let donated = 36_000_001;
    env.fund(&wsol, donated);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(lamports(&env, &wsol), donated, "the curve open left it");
    env.warp(1);
    close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(lamports(&env, &wsol), donated, "the curve close left it");
    assert_trader_clean(&env, &pm);
    let meta = ok(env.send(&[pump::sweep_trader_pump_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!((ev.tokens, ev.lamports), (0, donated));
    assert_eq!(lamports(&env, &wsol), 0, "swept to the lenders");
}

/// Tokens donated to `trader_base_ata` before a close are moved to the lending vault by the
/// leg's normalisation — never counted as pump's delivery (the post-condition is exact: `q`
/// through the trader, the donation beside it).
#[test]
fn a_token_donation_before_a_close_is_normalised() {
    let (mut env, pm, q) = setup(PumpCoin::Legacy115);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    donate_tokens(&mut env, &pm, &pm.trader_base_ata, 99);
    let charge = pm.curve.charge_buy(&env.svm, q).unwrap();
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, charge.cost);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q + 99);
    assert_trader_clean(&env, &pm);
}

/// The latch (A7, masks v0.4.1): an upgrade of pump, pump-fees or PumpSwap — the dumped
/// ProgramData's deploy slot moved, `flag_venue_upgraded` by anyone — blocks curve opens
/// (`VenueUpgraded`); a crank, `sol_settle` and the close of an open position keep working;
/// `revalidate_venue` for the new slot reopens.
#[test]
fn the_latch_blocks_curve_opens_only() {
    for (program, guard) in [
        (fork::PumpProgram::Pump, GUARD_PUMP),
        (fork::PumpProgram::Fees, GUARD_PUMP_FEES),
        (fork::PumpProgram::Amm, GUARD_PUMP_AMM),
    ] {
        let (mut env, pm, q) = setup(PumpCoin::T22_115);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        let slot = fork::FIXTURE_GUARD_SLOTS[usize::from(guard)];
        assert_err(
            &env.send(&[ix::flag_venue_upgraded(guard)], &[]),
            E::VenueNotUpgraded,
        );
        fork::set_pump_deploy_slot(&mut env.svm, program, slot + 7);
        let meta = ok(env.send(&[ix::flag_venue_upgraded(guard)], &[]));
        assert!(!events::raw(&meta).is_empty());
        assert_ne!(env.venue_guard().upgraded & (1 << guard), 0);
        env.warp(1);
        let mut b2 = env.borrower();
        let (res, _) = env.open(&pm, &mut b2, q);
        assert_err(&res, E::VenueUpgraded);
        crank(&mut env, &pm);
        let kp: Keypair = keeper(&mut env);
        ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        env.warp(1);
        close_ok(&mut env, &pm, &b, nonce);
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::revalidate_venue(&ea.pubkey(), guard, slot + 7)],
            &[&ea],
        ));
        env.warp(1);
        open_ok(&mut env, &pm, &mut b2, q);
    }
}

/// `sweep_trader`'s pump token leg is strict (ARCH §3.9; M5; M10 G-A1c): with tokens donated to
/// `trader_base_ata`, each of the leg's five accounts (`trader_base_ata`, `pool_vault`, the mint,
/// its token program, the trader's pump UVA) omitted or replaced by a foreign key, by another
/// holder's real account of the same kind (the lender's token account of the mint at slots 0 and
/// 1, the WSOL mint, the other token program, another user's pump UVA), the first two swapped, a
/// trailing account, the whole leg missing, and another owner's `ATA(·, WSOL)` as the named
/// `trader_wsol_ata` are each refused (`AccountMismatch` / `BadRemainingAccounts`) with the
/// donation where it was; the honest sweep (the control) then moves it to the lending vault.
#[test]
fn sweep_trader_token_leg_account_rules() {
    let (mut env, pm, _) = setup(PumpCoin::Legacy115);
    donate_tokens(&mut env, &pm, &pm.trader_base_ata, 31_337);
    let other = CurveUser::new(&mut env, &pm.curve);
    let honest = pump::sweep_trader_pump_ix(&env, &pm);
    let n = honest.accounts.len();
    let start = n - 5;
    let other_program = if pm.keys.token_program == shortwin::constants::TOKEN_PROGRAM_ID {
        shortwin::constants::TOKEN_2022_PROGRAM_ID
    } else {
        shortwin::constants::TOKEN_PROGRAM_ID
    };
    let others = [
        pm.lender_account,
        pm.lender_account,
        shortwin::constants::WSOL_MINT,
        other_program,
        fork::pump_user_volume_accumulator(&other.kp.pubkey()),
    ];
    let mut cases: Vec<(String, Instruction, E)> = Vec::new();
    for (i, other_key) in others.into_iter().enumerate() {
        let mut ix = honest.clone();
        ix.accounts.remove(start + i);
        cases.push((format!("omit leg {i}"), ix, E::BadRemainingAccounts));
        let mut ix = honest.clone();
        ix.accounts[start + i].pubkey = Pubkey::new_unique();
        cases.push((format!("foreign leg {i}"), ix, E::AccountMismatch));
        let mut ix = honest.clone();
        ix.accounts[start + i].pubkey = other_key;
        cases.push((format!("another holder's leg {i}"), ix, E::AccountMismatch));
    }
    let mut ix = honest.clone();
    ix.accounts.swap(start, start + 1);
    cases.push(("leg 0 and 1 swapped".into(), ix, E::AccountMismatch));
    let mut ix = honest.clone();
    ix.accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    cases.push(("a trailing account".into(), ix, E::BadRemainingAccounts));
    let mut ix = honest.clone();
    ix.accounts.truncate(start);
    cases.push(("no token leg".into(), ix, E::BadRemainingAccounts));
    let mut ix = honest.clone();
    let named = ix
        .accounts
        .iter()
        .position(|a| a.pubkey == pump::wsol_ata(&pm.trader()))
        .expect("trader_wsol_ata");
    ix.accounts[named].pubkey = pump::wsol_ata(&other.kp.pubkey());
    cases.push(("another owner's WSOL ATA".into(), ix, E::AccountMismatch));
    for (what, ix, want) in cases {
        let res = env.send(&[ix], &[]);
        assert_eq!(custom_code(&res), Some(code_of(want)), "{what}");
        assert_eq!(tokens(&env, &pm.trader_base_ata), 31_337, "{what}: kept");
    }
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let meta = ok(env.send(&[honest], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.tokens, 31_337);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, 31_337);
    assert_trader_clean(&env, &pm);
}

/// The zero-lamport borrower on the curve (M10 G-H10; `engine::zero_lamport_exits`): with the
/// wallet emptied after the open (A) or inside the exit's transaction (B), the borrower's close and
/// a keeper's expiry land and credit at least the position's and the exposure record's rent.
#[test]
fn a_zero_lamport_borrower_exits_on_the_curve() {
    shortwin_litesvm_tests::engine::zero_lamport_exits(|| setup(PumpCoin::T22_115), "curve");
}
