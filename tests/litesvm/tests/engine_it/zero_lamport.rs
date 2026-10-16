//! The zero-lamport borrower (ARCH §8.2 venue group, A18; M10 G-H10) on the mock, in both meanings
//! the owner's default asks for:
//!
//! - **(A) the wallet at 0 lamports after the open**: right after its open the borrower sends every
//!   lamport away (its System account is reaped) and stays at 0 for the position's life;
//! - **(B) a 0-lamport account at exit time**: the borrower empties its wallet inside the exit's own
//!   transaction, just before the exit instruction.
//!
//! Every exit lands — the borrower's own `close_short` (it signs; the fee payer is not the
//! borrower), the keeper's `expire` and `liquidate` (each under (A) and (B)), and `sol_settle` S1
//! (under (A) only; S2 and S3 are not run here) — and the borrower is paid through the position's
//! close (A18), so its account ends rent-exempt: the credit is the position's lamports (≥ the
//! position's rent) plus the exposure record's rent. No `InsufficientFundsForRent` can block an
//! exit; the hooks (I3, I9, I12) run on every instruction. The fork venues' close and expiry under
//! both meanings run through `engine::zero_lamport_exits` in `ray_it`, `pump_it` and `pamm_it`.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::errors::ShortwinError as E;
use shortwin::events::{Close, Expire, Liquidate, SolSettle};
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::engine::{drain_ix, expire_ix, liquidate_ix, sol_settle_ix};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::*;

/// Meaning (A): the borrower empties its wallet in a transaction of its own.
fn drain(env: &mut Env, b: &Keypair) {
    let ix = drain_ix(env, &b.pubkey());
    ok(env.send(&[ix], &[b]));
    assert_eq!(lamports(env, &b.pubkey()), 0, "the wallet is reaped");
    assert!(
        env.svm
            .get_account(&b.pubkey())
            .is_none_or(|a| a.lamports == 0)
    );
}

/// What the borrower is owed when position `nonce` closes and it holds no other position here: the
/// position's lamports and the exposure record's.
fn closing_credit(env: &Env, mm: &MockMarket, b: &Pubkey, nonce: u64) -> (u64, u64) {
    let pos = lamports(env, &pda::position(&mm.market(), b, nonce).0);
    let exp = lamports(env, &pda::exposure(&mm.market(), b).0);
    (pos, exp)
}

/// The borrower's account after an exit: at least the rent-exempt minimum of a 0-byte account,
/// and it holds the position's remainder and the exposure rent (`borrower_payout` is part of the
/// position's lamports before the leg; the rest of them are its rent).
fn assert_paid(env: &Env, b: &Pubkey, at_least: u64, what: &str) {
    let got = lamports(env, b);
    let min0 = env.svm.minimum_balance_for_rent_exemption(0);
    assert!(
        got >= min0,
        "{what}: {got} ≥ the 0-byte rent minimum {min0}"
    );
    assert!(got >= at_least, "{what}: {got} ≥ {at_least}");
}

/// (A) and (B) on the borrower's own close: an open, the wallet emptied (in its own transaction,
/// or inside the close's), the close lands and pays the borrower its payout plus both records'
/// rent. The control for (A): a separate 1,000-lamport payment to the reaped wallet fails
/// `InsufficientFundsForRent` — the failure a payout pushed apart from the position's close would
/// hit.
#[test]
fn a_zero_lamport_borrower_closes() {
    for meaning in [
        "A: emptied after the open",
        "B: emptied in the close's transaction",
    ] {
        let (mut env, mm) = setup();
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
        let kp = b.kp.insecure_clone();
        if meaning.starts_with('A') {
            drain(&mut env, &kp);
            // The hazard A18 avoids: a separate payment below the 0-byte rent minimum to the
            // reaped wallet is refused by the runtime.
            let payer = env.funded_keypair();
            let mut data = vec![2, 0, 0, 0];
            data.extend_from_slice(&1_000u64.to_le_bytes());
            let small = Instruction {
                program_id: anchor_lang::system_program::ID,
                accounts: vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(b.key(), false),
                ],
                data,
            };
            let res = env.send(&[small], &[&payer]);
            assert!(
                matches!(
                    res.as_ref().map_err(|f| &f.err),
                    Err(
                        solana_transaction_error::TransactionError::InsufficientFundsForRent { .. }
                    )
                ),
                "a 1,000-lamport payment to the reaped wallet: {:?}",
                res.as_ref().map_err(|f| &f.err)
            );
        }
        env.tick(&mm, 1);
        let (_, exp_rent) = closing_credit(&env, &mm, &b.key(), nonce);
        let close = close_short_ix(
            &mm,
            &b.key(),
            nonce,
            u64::MAX,
            shortwin::venues::ExtrasSel::NONE,
            &[],
        );
        let ixs = if meaning.starts_with('A') {
            vec![close]
        } else {
            vec![drain_ix(&env, &b.key()), close]
        };
        let meta = ok(env.send(&ixs, &[&kp]));
        let ev: Close = events::of_ix(&meta, ixs.len() - 1)
            .into_iter()
            .next()
            .expect("Close");
        let rent_pos = env
            .svm
            .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
        assert_paid(
            &env,
            &b.key(),
            ev.borrower_payout + rent_pos + exp_rent,
            meaning,
        );
        assert_eq!(
            lamports(&env, &b.key()),
            ev.borrower_payout + rent_pos + exp_rent,
            "{meaning}: the payout and both records' rent, nothing else"
        );
    }
}

/// (A) and (B) on the keeper's expiry after the deadline: it lands and the borrower's account is
/// re-created by the credit (the position's remainder and rent, the exposure rent).
#[test]
fn a_zero_lamport_borrower_is_expired() {
    for meaning in ["A", "B"] {
        let (mut env, mm) = setup();
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
        let kp = b.kp.insecure_clone();
        if meaning == "A" {
            drain(&mut env, &kp);
        }
        let deadline = env.position(&mm, &b.key(), nonce).deadline;
        let now = env.now();
        env.warp(deadline - now + 1);
        env.tick(&mm, 3);
        let k = env.funded_keypair();
        let (pos_l, exp_rent) = closing_credit(&env, &mm, &b.key(), nonce);
        let exit = expire_ix(&mm, &k.pubkey(), &b.key(), nonce);
        let meta = if meaning == "A" {
            ok(env.send(&[exit], &[&k]))
        } else {
            ok(env.send(&[drain_ix(&env, &b.key()), exit], &[&kp, &k]))
        };
        let ev: Expire = events::of_ix(&meta, usize::from(meaning == "B"))
            .into_iter()
            .next()
            .expect("Expire");
        assert!(ev.buyback_cost > 0);
        assert_paid(&env, &b.key(), exp_rent, meaning);
        assert!(lamports(&env, &b.key()) <= pos_l + exp_rent, "{meaning}");
    }
}

/// (A) and (B) on the keeper's liquidation after a genuine 2.4× rise, and (A) on `sol_settle` S1
/// after a held ×10 rise the position's value cannot buy back: both land on a borrower at 0
/// lamports, and the borrower ends rent-exempt. Before the liquidation lands, the keeper's
/// once-a-second attempts are refused only with a "not yet" code (`ReferenceStale`,
/// `NotLiquidatable`, `ExitBoundNotMet`) — never `InsufficientFundsForRent` or another refusal —
/// and under (B) a refused attempt rolls its drain back with it.
#[test]
fn a_zero_lamport_borrower_is_liquidated_and_settled() {
    // The liquidation.
    for meaning in ["A", "B"] {
        let (mut env, mm) = setup();
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
        let kp = b.kp.insecure_clone();
        if meaning == "A" {
            drain(&mut env, &kp);
        }
        env.tick(&mm, 2);
        let actor = env.actor(&mm, 400_000_000_000_000, 3_000_000_000_000);
        let p0 = env.spot(&mm) as f64;
        for s in 1..=30 {
            env.warp(1);
            env.push_price(&mm, &actor, (p0 * 2.4f64.powf(f64::from(s) / 30.0)) as u128);
            env.crank(&mm);
        }
        let k = env.funded_keypair();
        let mut landed = None;
        for _ in 0..120 {
            env.tick(&mm, 1);
            let (_, exp_rent) = closing_credit(&env, &mm, &b.key(), nonce);
            let wallet = lamports(&env, &b.key());
            let exit = liquidate_ix(&mm, &k.pubkey(), &b.key(), nonce);
            let res = if meaning == "A" {
                env.send(&[exit], &[&k])
            } else {
                env.send(&[drain_ix(&env, &b.key()), exit], &[&kp, &k])
            };
            match res {
                Ok(meta) => {
                    landed = Some((meta, exp_rent));
                    break;
                }
                Err(f) => {
                    let got = custom_code(&Err(f));
                    assert!(
                        [E::ReferenceStale, E::NotLiquidatable, E::ExitBoundNotMet]
                            .iter()
                            .any(|e| got == Some(code(*e))),
                        "{meaning}: only not-yet: {got:?}"
                    );
                    assert_eq!(lamports(&env, &b.key()), wallet, "{meaning}: rolled back");
                }
            }
        }
        let (meta, exp_rent) = landed.expect("liquidated");
        let ev: Liquidate = events::of_ix(&meta, usize::from(meaning == "B"))
            .into_iter()
            .next()
            .expect("Liquidate");
        assert!(ev.buyback_cost > 0);
        assert_paid(&env, &b.key(), exp_rent, meaning);
    }

    // S1.
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    let kp = b.kp.insecure_clone();
    drain(&mut env, &kp);
    env.tick(&mm, 2);
    let actor = env.actor(&mm, 400_000_000_000_000, 30_000_000_000_000);
    let p0 = env.spot(&mm);
    env.warp(1);
    env.push_price(&mm, &actor, p0 * 10);
    let k = env.funded_keypair();
    let mut settled = None;
    for _ in 0..300 {
        env.tick(&mm, 1);
        let (_, exp_rent) = closing_credit(&env, &mm, &b.key(), nonce);
        let ix = sol_settle_ix(&env, &mm, &k.pubkey(), &b.key(), nonce);
        let meta = ok(env.send(&[ix], &[&k]));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            settled = Some((ev, exp_rent));
            break;
        }
    }
    let (ev, exp_rent) = settled.expect("settled within 5 min");
    assert_eq!(ev.condition, 1, "S1");
    assert_eq!(
        ev.borrower_amount, 0,
        "S1 pays the borrower nothing but the records' rent"
    );
    assert_paid(&env, &b.key(), exp_rent, "S1");
}
