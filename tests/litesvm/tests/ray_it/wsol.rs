//! T-RAY-WSOL (ARCH §2.4 rules 2 and 5, §2.4.1 "The ephemeral WSOL account"; A5, A33; C0-3,
//! C1-3; R-RAY §4.2) on both programs: a pre-funded `trader_wsol` address (which blocks
//! `create_account`, never `allocate` / `assign`), lamports donated to the trader PDA, and a
//! pre-created `ATA(trader, WSOL)` holding WSOL never block a Raydium leg; donated lamports are
//! swept to `market_vault` and vest to lenders; the ATA, which no leg uses, is closed by
//! `sweep_trader` later. Every leg still measures exact deltas.

use anchor_lang::solana_program::instruction::AccountMeta as Meta;
use shortwin::events::TraderSwept;
use shortwin_litesvm_tests::engine::{sweep_trader_ix, trader_wsol_ata};
use shortwin_litesvm_tests::pda::ata;
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

/// Donations to both trader PDAs before an open, then before a close: each leg sweeps them to
/// `market_vault` as a vesting lender lump (`TraderSwept`), funds exactly what it needs, and
/// trades at the exact quote.
#[test]
fn pre_funded_trader_pdas() {
    for fixture in [FART, USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        let griefer = env.funded_keypair();
        // Far more than the WSOL account's rent at its address: the leg sweeps all of it
        // first, then funds exactly `rent + amount` (A5).
        ok(env.send(
            &[
                transfer(&griefer, &rm.keys.trader, 1_234_567),
                transfer(&griefer, &rm.trader_wsol, 3_000_000_000),
            ],
            &[&griefer],
        ));
        let v0 = env.market_vault(&rm.market());
        let vest0 = env.market(&rm.market()).vest_bal;
        let s_exp = rm.quote_sell(&env.svm, q).unwrap().proceeds;
        let mut b = env.borrower();
        let (open, nonce, meta) = open_ok(&mut env, &rm, &mut b, q);
        assert_eq!(open.s, s_exp, "{fixture:?}");
        let swept: TraderSwept = events::one(&meta);
        assert_eq!(swept.lamports, 3_001_234_567);
        let v1 = env.market_vault(&rm.market());
        assert_eq!(v1.lender_sol, v0.lender_sol + 3_001_234_567);
        assert_eq!(v1.protocol_fees, v0.protocol_fees + open.fee_open);
        assert_eq!(env.market(&rm.market()).vest_bal, vest0 + 3_001_234_567);
        assert_eq!(lamports(&env, &rm.keys.trader), 0);
        assert_eq!(lamports(&env, &rm.trader_wsol), 0);

        env.warp(1);
        env.crank(&rm);
        env.warp(1);
        ok(env.send(
            &[
                transfer(&griefer, &rm.keys.trader, 1_000_000),
                transfer(&griefer, &rm.trader_wsol, 5_000_000),
            ],
            &[&griefer],
        ));
        let v1 = env.market_vault(&rm.market());
        let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
        let (close, meta) = close_ok(&mut env, &rm, &b, nonce);
        assert_eq!(close.buyback_cost, b_exp);
        let swept: TraderSwept = events::one(&meta);
        assert_eq!(swept.lamports, 6_000_000);
        let v2 = env.market_vault(&rm.market());
        let lender_rent = close.rent - close.rent_protocol;
        assert_eq!(v2.lender_sol, v1.lender_sol + 6_000_000 + lender_rent);
        assert_eq!(lamports(&env, &rm.trader_wsol), 0);
        assert_eq!(lamports(&env, &rm.keys.trader), 0);
    }
}

/// A third party creates `ATA(trader, WSOL)` and wraps SOL into it: no leg uses it (the legs
/// use the PDA-addressed `trader_wsol`), so opens and closes are unaffected; `sweep_trader`
/// closes it and credits lenders (M2-SEC-4's instruction, on a Raydium market).
#[test]
fn a_pre_created_trader_wsol_ata() {
    for fixture in [FART, USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        let ata_key = trader_wsol_ata(&rm.market());
        assert_eq!(
            ata_key,
            ata(
                &rm.keys.trader,
                &shortwin::constants::WSOL_MINT,
                &shortwin::constants::TOKEN_PROGRAM_ID
            )
        );
        let payer = env.payer.insecure_clone();
        let created = litesvm_token::CreateAssociatedTokenAccount::new(
            &mut env.svm,
            &payer,
            &shortwin::constants::WSOL_MINT,
        )
        .owner(&rm.keys.trader)
        .token_program_id(&shortwin::constants::TOKEN_PROGRAM_ID)
        .send()
        .expect("create the trader's WSOL ATA");
        assert_eq!(created, ata_key);
        fork::wrap(&mut env, &ata_key, 2_000_000_000);
        let ata_lamports = lamports(&env, &ata_key);
        let mut b = env.borrower();
        let (open, nonce, meta) = open_ok(&mut env, &rm, &mut b, q);
        assert!(
            events::of::<TraderSwept>(&meta).is_empty(),
            "nothing to sweep"
        );
        assert_eq!(open.q, q);
        assert_eq!(
            lamports(&env, &ata_key),
            ata_lamports,
            "untouched by the leg"
        );
        env.warp(1);
        close_ok(&mut env, &rm, &b, nonce);
        assert_eq!(lamports(&env, &ata_key), ata_lamports);
        let v0 = env.market_vault(&rm.market());
        let meta = ok(env.send(&[sweep_trader_ix(&rm.market(), None)], &[]));
        let swept: TraderSwept = events::one(&meta);
        assert_eq!(swept.lamports, ata_lamports);
        assert_eq!(lamports(&env, &ata_key), 0, "closed");
        assert_eq!(
            env.market_vault(&rm.market()).lender_sol,
            v0.lender_sol + ata_lamports
        );
    }
}

/// The `trader_wsol` address pre-funded between the two instructions of one transaction (a
/// front-runner's bundle): an open right after the donation, in the same transaction, still
/// sweeps and trades at the exact quote, on both programs (CPMM: USELESS and the Token-2022
/// pool, M10 G-B1).
#[test]
fn a_donation_in_the_same_transaction() {
    for fixture in [FART, USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        let b = env.borrower();
        let open = open_short_ix(
            &rm,
            &b.key(),
            0,
            q,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        );
        let kp = b.kp.insecure_clone();
        let donate = transfer(&kp, &rm.trader_wsol, 10_000_000);
        let s_exp = rm.quote_sell(&env.svm, q).unwrap().proceeds;
        let meta = ok(env.send(&[donate, open], &[&kp]));
        let swept: TraderSwept = events::of_ix(&meta, 1).into_iter().next().expect("swept");
        assert_eq!(swept.lamports, 10_000_000, "{fixture:?}");
        let ev: Open = events::one(&meta);
        assert_eq!(ev.s, s_exp, "{fixture:?}");
        assert_eq!(lamports(&env, &rm.trader_wsol), 0);
    }
}

/// M10 G-B1/B2 on a keeper's exit on CPMM (USELESS and the Token-2022 pool): lamports donated to
/// the trader PDA and to the `trader_wsol` address before an expiry never block the keeper's
/// leg — it sweeps them into `market_vault` as a vesting lump (`TraderSwept`, credited on top of
/// the expiry's own lender amounts), funds exactly its buy-back (the pool's WSOL vault grows by
/// exactly `buyback_cost`, and `trader_wsol` ends empty: none of the donation reaches the pool and
/// nothing is left over) and buys back at the exact quote; both addresses end empty. The control:
/// the same expiry without the donations moves `lender_sol` by exactly the donation less.
#[test]
fn donations_before_a_keeper_expiry_on_cpmm() {
    use shortwin::events::Expire;
    use shortwin_litesvm_tests::engine::expire_ix;
    for fixture in [USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let pos = env.position(&rm, &b.key(), nonce);
        let slot = env.clock().slot;
        env.set_clock(pos.deadline + 1, slot + 1_600_000);
        env.tick(&rm, 3);
        let keeper = env.funded_keypair();
        let saved = env.svm.clone();
        let mut deltas = Vec::new();
        for donated in [0, 2_500_000_000u64] {
            env.svm = saved.clone();
            if donated > 0 {
                let griefer = env.funded_keypair();
                ok(env.send(
                    &[
                        transfer(&griefer, &rm.keys.trader, 500_000_000),
                        transfer(&griefer, &rm.trader_wsol, donated - 500_000_000),
                    ],
                    &[&griefer],
                ));
            }
            let v0 = env.market_vault(&rm.market());
            let vest0 = env.market(&rm.market()).vest_bal;
            let pool_vault0 = tokens(&env, &rm.keys.pool_vault);
            let venue_wsol = rm.vaults().1;
            let venue_wsol0 = tokens(&env, &venue_wsol);
            let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
            let meta = ok(env.send(
                &[expire_ix(&rm, &keeper.pubkey(), &b.key(), nonce)],
                &[&keeper],
            ));
            let e: Expire = events::one(&meta);
            assert_eq!(e.buyback_cost, b_exp, "{fixture:?}: the exact quote");
            assert_eq!(tokens(&env, &rm.keys.pool_vault) - pool_vault0, q);
            assert_eq!(
                tokens(&env, &venue_wsol) - venue_wsol0,
                e.buyback_cost,
                "{fixture:?}: the leg funds exactly its buy-back"
            );
            let swept: Vec<TraderSwept> = events::of(&meta);
            if donated > 0 {
                assert_eq!(swept.len(), 1, "{fixture:?}");
                assert_eq!(swept[0].lamports, donated);
            } else {
                assert!(swept.is_empty(), "{fixture:?}: nothing to sweep");
            }
            assert_eq!(lamports(&env, &rm.keys.trader), 0);
            assert_eq!(lamports(&env, &rm.trader_wsol), 0);
            let v1 = env.market_vault(&rm.market());
            let vest1 = env.market(&rm.market()).vest_bal;
            deltas.push((
                i128::from(v1.lender_sol) - i128::from(v0.lender_sol),
                i128::from(vest1) - i128::from(vest0),
                e,
            ));
        }
        let (plain, griefed) = (&deltas[0], &deltas[1]);
        assert_eq!(
            griefed.0 - plain.0,
            2_500_000_000,
            "{fixture:?}: lender_sol moves by the donation on top"
        );
        assert_eq!(griefed.1 - plain.1, 2_500_000_000, "{fixture:?}: it vests");
        assert_eq!(
            (griefed.2.buyback_cost, griefed.2.borrower_payout),
            (plain.2.buyback_cost, plain.2.borrower_payout),
            "{fixture:?}: the exit's own amounts unchanged"
        );
    }
}
