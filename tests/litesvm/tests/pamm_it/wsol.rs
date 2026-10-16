//! Trader griefing on PumpSwap (ARCH §2.4 rules 2 and 5, §2.4.1 PumpSwap row, §3.9; A5, A33, A62;
//! C0-3, C1-3; M10 G-B1..4), mirroring `ray_it/wsol.rs` and `pump_it/exits.rs` on the dumped pAMM:
//!
//! - lamports donated to the `trader` PDA and to the `trader_wsol` address — before an open, before
//!   a close, and in the open's own transaction — never block a PumpSwap leg: the leg sweeps them
//!   to `market_vault` as a vesting lender lump (`TraderSwept`), then trades at the exact quote;
//! - a pre-created `ATA(trader, WSOL)` holding WSOL (no PumpSwap leg names it: the legs use the
//!   PDA-addressed `trader_wsol`, T-ATA) leaves opens and closes alone, and `sweep_trader` closes it
//!   into the lenders;
//! - tokens donated to `trader_base_ata` (the account PumpSwap pays the trader's tokens into) before
//!   an open, a close, an expiry and a liquidation are normalised into `pool_vault` as a donation —
//!   `T` (`tokens_free`) unchanged by it, never counted as PumpSwap's delivery (`Close.token_surplus`
//!   0, the buyback exact) — and the trader side ends clean (I7's PumpSwap clause: `trader_base_ata`
//!   empty after every leg, its owner, delegate and close authority unchanged).
//!
//! Every instruction goes through `Env::send`: the invariant hooks run after each one.

use anchor_lang::solana_program::instruction::AccountMeta as Meta;
use shortwin::constants::{TOKEN_PROGRAM_ID, WSOL_MINT};
use shortwin::events::{Expire, Liquidate, TraderSwept};
use shortwin_litesvm_tests::engine::trader_wsol_ata;
use solana_keypair::Keypair;

use crate::common::*;

fn transfer(from: &Keypair, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![Meta::new(from.pubkey(), true), Meta::new(*to, false)],
        data,
    }
}

/// The trader side's owner, delegate and close authority of `trader_base_ata` (I7: never changed).
fn base_ata_authorities(env: &Env, pm: &PammMarket) -> (Pubkey, bool, bool) {
    let t = pamm::token_state(&env.svm, &pm.trader_base_ata).expect("trader_base_ata");
    (
        Pubkey::new_from_array(t.owner),
        t.delegate.is_some(),
        t.close_authority.is_some(),
    )
}

/// Donations to both trader PDAs before an open, then before a close (SPL Token and Token-2022
/// pools): each leg sweeps them to `market_vault` as a vesting lender lump (`TraderSwept`), funds
/// exactly what it needs, and trades at the exact quote.
#[test]
fn pre_funded_trader_pdas() {
    for coin in [PammCoin::Legacy261, PammCoin::Mature] {
        let (mut env, pm, q) = setup(coin);
        let trader_wsol = pda::trader_wsol(&pm.market()).0;
        let griefer = env.funded_keypair();
        // Far more than the WSOL account's rent at its address: the leg sweeps all of it first,
        // then funds exactly `rent + amount` (A5).
        ok(env.send(
            &[
                transfer(&griefer, &pm.keys.trader, 1_234_567),
                transfer(&griefer, &trader_wsol, 3_000_000_000),
            ],
            &[&griefer],
        ));
        let v0 = env.market_vault(&pm.market());
        let vest0 = market(&env, &pm).vest_bal;
        let s_exp = quote_sell(&env, &pm.pool, q).expect("quote");
        let mut b = env.borrower();
        let (open, nonce, meta) = open_ok(&mut env, &pm, &mut b, q);
        assert_eq!(open.s, s_exp, "{coin:?}: S = the pool's quote");
        let swept: TraderSwept = events::one(&meta);
        assert_eq!(swept.lamports, 3_001_234_567, "{coin:?}");
        let v1 = env.market_vault(&pm.market());
        assert_eq!(v1.lender_sol, v0.lender_sol + 3_001_234_567);
        assert_eq!(v1.protocol_fees, v0.protocol_fees + open.fee_open);
        assert_eq!(market(&env, &pm).vest_bal, vest0 + 3_001_234_567);
        assert_trader_clean(&env, &pm);

        env.warp(1);
        crank(&mut env, &pm);
        env.warp(1);
        ok(env.send(
            &[
                transfer(&griefer, &pm.keys.trader, 1_000_000),
                transfer(&griefer, &trader_wsol, 5_000_000),
            ],
            &[&griefer],
        ));
        let v1 = env.market_vault(&pm.market());
        let b_exp = quote_buy(&env, &pm.pool, q).expect("quote");
        let (close, meta) = close_ok(&mut env, &pm, &b, nonce);
        assert_eq!(close.buyback_cost, b_exp, "{coin:?}: B = the pool's quote");
        let swept: TraderSwept = events::one(&meta);
        assert_eq!(swept.lamports, 6_000_000);
        let v2 = env.market_vault(&pm.market());
        let lender_rent = close.rent - close.rent_protocol;
        assert_eq!(v2.lender_sol, v1.lender_sol + 6_000_000 + lender_rent);
        assert_trader_clean(&env, &pm);
    }
}

/// The `trader` and `trader_wsol` addresses pre-funded in the open's own transaction (a
/// front-runner's bundle): the open still sweeps both and trades at the quote.
#[test]
fn a_donation_in_the_same_transaction() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let trader_wsol = pda::trader_wsol(&pm.market()).0;
    let b = env.borrower();
    let open = open_short_ix(
        &pm,
        &b.key(),
        0,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let s_exp = quote_sell(&env, &pm.pool, q).expect("quote");
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(
        &[
            cu_limit_ix(pamm::PAMM_TRADE_CU),
            transfer(&kp, &trader_wsol, 10_000_000),
            transfer(&kp, &pm.keys.trader, 1_000_000),
            open,
        ],
        &[&kp],
    ));
    let swept: TraderSwept = events::of_ix(&meta, 3).into_iter().next().expect("swept");
    assert_eq!(swept.lamports, 11_000_000);
    let ev: Open = events::of_ix(&meta, 3).into_iter().next().expect("open");
    assert_eq!(ev.s, s_exp);
    assert_trader_clean(&env, &pm);
}

/// A third party creates `ATA(trader, WSOL)` and wraps SOL into it: no PumpSwap leg names it, so
/// an open and a close are exact and leave it alone; `sweep_trader` closes it into the lenders.
#[test]
fn a_pre_created_trader_wsol_ata() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let ata_key = trader_wsol_ata(&pm.market());
    assert_eq!(ata_key, wsol_ata(&pm.trader()));
    let payer = env.payer.pubkey();
    ok(env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &pm.trader(),
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        )],
        &[],
    ));
    fork::wrap(&mut env, &ata_key, 2_000_000_000);
    let ata_lamports = lamports(&env, &ata_key);
    let s_exp = quote_sell(&env, &pm.pool, q).expect("quote");
    let mut b = env.borrower();
    let (open, nonce, meta) = open_ok(&mut env, &pm, &mut b, q);
    assert!(
        events::of::<TraderSwept>(&meta).is_empty(),
        "nothing to sweep"
    );
    assert_eq!(open.s, s_exp);
    assert_eq!(
        lamports(&env, &ata_key),
        ata_lamports,
        "untouched by the sale"
    );
    env.warp(1);
    crank(&mut env, &pm);
    let b_exp = quote_buy(&env, &pm.pool, q).expect("quote");
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, b_exp);
    assert_eq!(
        lamports(&env, &ata_key),
        ata_lamports,
        "untouched by the buy"
    );
    let v0 = env.market_vault(&pm.market());
    let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
    let swept: TraderSwept = events::one(&meta);
    assert_eq!((swept.lamports, swept.tokens), (ata_lamports, 0));
    assert_eq!(lamports(&env, &ata_key), 0, "closed");
    assert_eq!(
        env.market_vault(&pm.market()).lender_sol,
        v0.lender_sol + ata_lamports
    );
    assert_trader_clean(&env, &pm);
}

/// Tokens donated to `trader_base_ata` before an open, a close and an expiry (SPL Token and
/// Token-2022 pools): each leg moves the donation to `pool_vault` beside its own `q` — `T`
/// unchanged by it, the sale and the buybacks exact, `Close.token_surplus` 0 (the donation is not
/// PumpSwap's delivery) — and the trader side ends clean with its authorities unchanged (I7).
#[test]
fn token_donations_to_trader_base_ata_are_normalised() {
    for coin in [PammCoin::Legacy261, PammCoin::Mature] {
        let (mut env, pm, q) = setup(coin);
        let auth0 = base_ata_authorities(&env, &pm);
        assert_eq!(auth0, (pm.trader(), false, false));

        // Before an open.
        donate_tokens(&mut env, &pm, &pm.trader_base_ata, 1_234_567);
        let t0 = market(&env, &pm).tokens_free;
        let vault0 = tokens(&env, &pm.keys.pool_vault);
        let s_exp = quote_sell(&env, &pm.pool, q).expect("quote");
        let mut b = env.borrower();
        let (open, n0, _) = open_ok(&mut env, &pm, &mut b, q);
        assert_eq!(open.s, s_exp, "{coin:?}: the sale is exactly q");
        assert_eq!(
            tokens(&env, &pm.keys.pool_vault) + q,
            vault0 + 1_234_567,
            "{coin:?}: q out, the donation in"
        );
        assert_eq!(
            market(&env, &pm).tokens_free,
            t0 - q,
            "{coin:?}: T moves by q only"
        );
        assert_trader_clean(&env, &pm);

        // Before a close.
        env.warp(1);
        crank(&mut env, &pm);
        donate_tokens(&mut env, &pm, &pm.trader_base_ata, 99);
        let t1 = market(&env, &pm).tokens_free;
        let vault1 = tokens(&env, &pm.keys.pool_vault);
        let b_exp = quote_buy(&env, &pm.pool, q).expect("quote");
        let (close, _) = close_ok(&mut env, &pm, &b, n0);
        assert_eq!(
            close.buyback_cost, b_exp,
            "{coin:?}: B = PumpSwap's charge for q"
        );
        assert_eq!(close.token_surplus, 0, "{coin:?}: not the venue's delivery");
        assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault1, q + 99);
        assert_eq!(
            market(&env, &pm).tokens_free,
            t1 + q,
            "{coin:?}: T moves by q only"
        );
        assert_trader_clean(&env, &pm);

        // Before an expiry (lamports to the trader as well).
        let (_, n1, _) = open_ok(&mut env, &pm, &mut b, q);
        warp_to_deadline(&mut env, &pm, &b.key(), n1, 1);
        crank(&mut env, &pm);
        env.warp(1);
        crank(&mut env, &pm);
        donate_tokens(&mut env, &pm, &pm.trader_base_ata, 4_242);
        env.fund(&pm.trader(), 3_000_000);
        let t2 = market(&env, &pm).tokens_free;
        let vault2 = tokens(&env, &pm.keys.pool_vault);
        let charge = quote_buy(&env, &pm.pool, q).expect("quote");
        let kp = keeper(&mut env);
        let meta = ok(expire(&mut env, &pm, &kp, &b.key(), n1));
        let ev: Expire = events::one(&meta);
        assert_eq!(
            ev.buyback_cost, charge,
            "{coin:?}: B = PumpSwap's charge for q"
        );
        let swept: TraderSwept = events::one(&meta);
        assert_eq!(swept.lamports, 3_000_000);
        assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault2, q + 4_242);
        assert_eq!(market(&env, &pm).tokens_free, t2 + q);
        assert_trader_clean(&env, &pm);
        assert_eq!(base_ata_authorities(&env, &pm), auth0, "{coin:?}: I7");
    }
}

/// Tokens and lamports donated to the trader side while a position waits to be liquidated (a buyer
/// lifts the price × 2.8, held): every failed attempt leaves them; the liquidation that lands
/// moves the tokens to `pool_vault` as a donation and the lamports to the lenders, the buyback
/// exact for `q`.
#[test]
fn token_donations_before_a_liquidation_are_normalised() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut env, &pm.pool, 28, 10));
    donate_tokens(&mut env, &pm, &pm.trader_base_ata, 777_777);
    env.fund(&pm.trader(), 2_500_000);
    let kp = keeper(&mut env);
    let mut done = None;
    for _ in 0..900 {
        env.warp(1);
        crank(&mut env, &pm);
        let t = market(&env, &pm).tokens_free;
        let vault = tokens(&env, &pm.keys.pool_vault);
        let charge = quote_buy(&env, &pm.pool, q).expect("quote");
        match liquidate(&mut env, &pm, &kp, &b.key(), nonce) {
            Ok(meta) => {
                done = Some((meta, t, vault, charge));
                break;
            }
            Err(f) => {
                let got = custom_code(&Err(f));
                assert_eq!(got, Some(code(E::NotLiquidatable)), "only not-yet");
                assert_eq!(
                    tokens(&env, &pm.trader_base_ata),
                    777_777,
                    "kept until the leg"
                );
            }
        }
    }
    let (meta, t, vault, charge) = done.expect("liquidated within 15 min");
    let ev: Liquidate = events::one(&meta);
    assert_eq!(
        ev.buyback_cost, charge,
        "B = PumpSwap's charge for q (not for q + the donation)"
    );
    assert!(ev.buyback_cost <= ev.b_live);
    let swept: TraderSwept = events::one(&meta);
    assert_eq!(swept.lamports, 2_500_000);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault, q + 777_777);
    assert_eq!(market(&env, &pm).tokens_free, t + q, "T moves by q only");
    assert_trader_clean(&env, &pm);
}
