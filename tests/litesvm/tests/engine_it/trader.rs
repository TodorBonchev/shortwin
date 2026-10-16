//! Trader normalisation and the ephemeral WSOL account (ARCH §2.4 rules 2 and 5, §2.4.1; A5,
//! A33; C0-3, C1-3): donations to the trader or to the `trader_wsol` address (at least the
//! rent-exempt minimum of an empty account: the runtime refuses a smaller balance), and a
//! pre-created `ATA(trader, WSOL)`, never block a leg; donated lamports reach lenders as a
//! vesting lump; the leg's post-conditions assert deltas and authorities, never that an
//! account a third party can create does not exist.

use anchor_lang::solana_program::instruction::Instruction;
use shortwin::events::TraderSwept;
use shortwin_litesvm_tests::pda::ata;
use shortwin_litesvm_tests::tokens;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::*;

fn transfer(from: &Keypair, to: &anchor_lang::prelude::Pubkey, lamports: u64) -> Instruction {
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(from.pubkey(), true),
            anchor_lang::solana_program::instruction::AccountMeta::new(*to, false),
        ],
        data,
    }
}

/// Lamports donated to the trader PDA before an open are swept to `market_vault` and vest to
/// lenders (`lender_sol` and `vest_bal` grow by the donation); the open goes through.
#[test]
fn donation_to_the_trader_before_an_open() {
    let (mut env, mm) = setup();
    let griefer = env.funded_keypair();
    ok(env.send(
        &[transfer(&griefer, &mm.keys.trader, 1_234_567)],
        &[&griefer],
    ));
    assert_eq!(lamports(&env, &mm.keys.trader), 1_234_567);
    let v0 = env.market_vault(&mm.market());
    let vest0 = env.market(&mm.market()).vest_bal;
    let mut b = env.borrower();
    let (open, _, meta) = open_ok(&mut env, &mm, &mut b, Q);
    let swept: TraderSwept = events::one(&meta);
    assert_eq!(swept.lamports, 1_234_567);
    assert_eq!(lamports(&env, &mm.keys.trader), 0);
    let v1 = env.market_vault(&mm.market());
    assert_eq!(v1.lender_sol, v0.lender_sol + 1_234_567);
    assert_eq!(v1.protocol_fees, v0.protocol_fees + open.fee_open);
    assert_eq!(env.market(&mm.market()).vest_bal, vest0 + 1_234_567);
}

/// The same donation before a close; and a donation to the `trader_wsol` **address** (the
/// ephemeral account's PDA, which a third party can pre-fund but never allocate): both swept,
/// the WSOL account created grief-proof at the same address.
#[test]
fn donations_before_a_close_and_to_the_wsol_address() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 2);
    let griefer = env.funded_keypair();
    ok(env.send(
        &[
            transfer(&griefer, &mm.keys.trader, 1_000_000),
            transfer(&griefer, &mm.trader_wsol, 5_000_000),
        ],
        &[&griefer],
    ));
    let v0 = env.market_vault(&mm.market());
    let (close, meta) = close_ok(&mut env, &mm, &b, nonce);
    let swept: TraderSwept = events::one(&meta);
    assert_eq!(swept.lamports, 6_000_000);
    let v1 = env.market_vault(&mm.market());
    let lender_rent = close.rent - close.rent_protocol;
    assert_eq!(v1.lender_sol, v0.lender_sol + 6_000_000 + lender_rent);
    assert_eq!(lamports(&env, &mm.trader_wsol), 0);
    assert_eq!(lamports(&env, &mm.keys.trader), 0);
}

/// A pre-created `ATA(trader, WSOL)` holding WSOL is never touched by a leg: opens and closes
/// work and leave it as it was (ARCH §2.4 rule 2: `sweep_trader`, M6, recovers it).
#[test]
fn pre_created_trader_wsol_ata_is_harmless() {
    let (mut env, mm) = setup();
    let wsol_ata = ata(&mm.keys.trader, &mock::WSOL_MINT, &TokenProgram::Spl.id());
    let payer = env.payer.insecure_clone();
    // Create the ATA at its canonical address, owned by the trader, and fund it.
    let created = tokens::create_ata(
        &mut env.svm,
        &payer,
        &mm.keys.trader,
        &mock::WSOL_MINT,
        TokenProgram::Spl,
    );
    assert_eq!(created, wsol_ata);
    mock::wrap(&mut env, &wsol_ata, 777_000);
    let before = read::account(&env.svm, &wsol_ata).unwrap();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 1);
    close_ok(&mut env, &mm, &b, nonce);
    assert_eq!(read::account(&env.svm, &wsol_ata).unwrap(), before);
}

/// Donations in the **same transaction** as the open (a griefer's bundle): still swept.
#[test]
fn donation_bundled_with_the_open() {
    let (mut env, mm) = setup();
    let griefer = env.funded_keypair();
    let b = env.borrower();
    let open = open_short_ix(
        &mm,
        &b.key(),
        0,
        Q,
        0,
        u64::MAX,
        u64::MAX,
        shortwin::venues::ExtrasSel::NONE,
        &[],
    );
    let meta = ok(env.send(
        &[
            transfer(&griefer, &mm.trader_wsol, 3_000_000),
            transfer(&griefer, &mm.keys.trader, 1_000_000),
            open,
        ],
        &[&griefer, &b.kp],
    ));
    let swept: Vec<TraderSwept> = events::of(&meta);
    assert_eq!(swept.len(), 1);
    assert_eq!(swept[0].lamports, 4_000_000);
}
