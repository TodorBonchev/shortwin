//! The keeper exits on the fork (ARCH §3.6, §3.7; M3 exit criteria "fork E2E on Fartcoin (AMM
//! v4) and USELESS (CPMM) through open → liquidation"): a whale's real buy on the real pool
//! doubles the price, the keeper cranks while the EMA follows, and `liquidate` buys the position
//! back on the venue within the fixed bound — the buy leg delivering straight into the lending
//! vault; an expired healthy position is closed by `expire`. (S3 on a halted pool is
//! `route.rs`; the S1 / S2 arithmetic is venue-independent and runs on the mock,
//! `tests/settle_it.rs`.) The zero-lamport borrower (M10 G-H10) on both programs: its close and
//! expiry (`engine::zero_lamport_exits`), its liquidation after a real whale pump and its S3
//! under the Emergency flag.

use shortwin::events::{Expire, Liquidate, SolSettle};
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::engine::{
    drain_ix, expire_ix, liquidate_ix, set_route_broken_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::pump::cu_limit_ix;
use shortwin_math::fees::Direction;
use solana_keypair::Keypair;

use crate::common::*;

/// A whale buy of `sol` lamports of the token on `rm`'s pool (a real swap).
fn whale_buy(env: &mut Env, rm: &RayMarket, sol: u64) {
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

#[test]
fn liquidation_after_a_pump() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (open, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let pos = env.position(&rm, &b.key(), nonce);
        env.tick(&rm, 3);
        // √2.2 − 1 of the SOL reserve lifts the price ≈ 2.2× (constant product).
        let (_, y) = rm.reserves(&env.svm).unwrap();
        whale_buy(&mut env, &rm, y / 1_000 * 484);
        let keeper = env.funded_keypair();
        let mut ev = None;
        for _ in 0..30 {
            env.tick(&rm, 1);
            let pool_vault0 = tokens(&env, &rm.keys.pool_vault);
            let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
            let ix = liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce);
            let res = env.send(&[ix], &[&keeper]);
            match custom_code(&res) {
                None => {
                    let meta = ok(res);
                    let e: Liquidate = events::one(&meta);
                    assert_eq!(
                        e.buyback_cost, b_exp,
                        "{fixture:?}: B = B̂_live at the live fee"
                    );
                    assert_eq!(e.b_live, b_exp);
                    assert_eq!(tokens(&env, &rm.keys.pool_vault) - pool_vault0, q);
                    ev = Some(e);
                    break;
                }
                Some(c) if c == code(E::NotLiquidatable) || c == code(E::ExitBoundNotMet) => {}
                Some(c) => panic!("{fixture:?}: liquidate failed with {c}"),
            }
        }
        let ev = ev.unwrap_or_else(|| panic!("{fixture:?}: never liquidated"));
        assert!(ev.cr_spot_bps < u64::from(pos.cr_liq_bps));
        assert!(!ev.expired);
        assert!(
            ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout == open.v,
            "the waterfall conserves V"
        );
        assert!(read::account(&env.svm, &pda::position(&rm.market(), &b.key(), nonce).0).is_none());
        assert_eq!(read::token_delegate(&env.svm, &rm.keys.pool_vault), None);
    }
}

#[test]
fn expiry_of_a_healthy_position() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let pos = env.position(&rm, &b.key(), nonce);
        let slot = env.clock().slot;
        env.set_clock(pos.deadline + 1, slot + 1_600_000);
        env.tick(&rm, 3);
        let keeper = env.funded_keypair();
        let pool_vault0 = tokens(&env, &rm.keys.pool_vault);
        let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
        let meta = ok(env.send(
            &[expire_ix(&rm, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        ));
        let e: Expire = events::one(&meta);
        assert_eq!(e.buyback_cost, b_exp, "{fixture:?}");
        assert_eq!(tokens(&env, &rm.keys.pool_vault) - pool_vault0, q);
        assert!(e.expiry_fee > 0 && e.keeper_reward > 0);
    }
}

/// The zero-lamport borrower on AMM v4 (M10 G-H10; `engine::zero_lamport_exits`): with the wallet
/// emptied after the open (A) or inside the exit's transaction (B), the borrower's close and a
/// keeper's expiry land and credit at least the position's and the exposure record's rent.
#[test]
fn a_zero_lamport_borrower_exits_on_amm_v4() {
    shortwin_litesvm_tests::engine::zero_lamport_exits(|| setup(FART), "AMM v4");
}

/// The zero-lamport borrower on CPMM (M10 G-H10; `engine::zero_lamport_exits`), on USELESS and on
/// the Token-2022 pool: with the wallet emptied after the open (A) or inside the exit's
/// transaction (B), the borrower's close and a keeper's expiry land and credit at least the
/// position's and the exposure record's rent.
#[test]
fn a_zero_lamport_borrower_exits_on_cpmm() {
    shortwin_litesvm_tests::engine::zero_lamport_exits(|| setup(USELESS), "CPMM USELESS");
    shortwin_litesvm_tests::engine::zero_lamport_exits(|| setup(T22), "CPMM Token-2022");
}

/// What the borrower is owed when position `nonce` closes, besides its payout: the position's rent
/// and the exposure record's lamports (A18; the borrower holds no other position here).
fn records(env: &Env, rm: &RayMarket, b: &Pubkey, nonce: u64) -> u64 {
    lamports(env, &pda::position(&rm.market(), b, nonce).0).min(
        env.svm
            .minimum_balance_for_rent_exemption(ShortPosition::SPACE),
    ) + lamports(env, &pda::exposure(&rm.market(), b).0)
}

/// Meaning (A): the borrower empties its wallet in a transaction of its own. The control: the
/// hazard A18 avoids — a separate 1,000-lamport payment (below the 0-byte rent minimum) to the
/// reaped wallet is refused by the runtime with `InsufficientFundsForRent`.
fn reap(env: &mut Env, b: &Keypair) {
    let ix = drain_ix(env, &b.pubkey());
    ok(env.send(&[ix], &[b]));
    assert_eq!(lamports(env, &b.pubkey()), 0, "reaped");
    let payer = env.funded_keypair();
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&1_000u64.to_le_bytes());
    let small = Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(b.pubkey(), false),
        ],
        data,
    };
    let res = env.send(&[small], &[&payer]);
    assert!(
        matches!(
            res.as_ref().map_err(|f| &f.err),
            Err(solana_transaction_error::TransactionError::InsufficientFundsForRent { .. })
        ),
        "a 1,000-lamport payment to the reaped wallet: {:?}",
        res.as_ref().map_err(|f| &f.err)
    );
}

/// A keeper's exit `ix` under meaning (A) — the wallet already at 0 — or (B) — the borrower's
/// drain of its wallet first, in the same transaction — with a 400k CU limit.
fn exit_tx(env: &Env, meaning: char, b: &Keypair, ix: Instruction) -> Vec<Instruction> {
    let mut ixs = vec![cu_limit_ix(400_000)];
    if meaning == 'B' {
        ixs.push(drain_ix(env, &b.pubkey()));
    }
    ixs.push(ix);
    ixs
}

/// The zero-lamport borrower's keeper exits beyond the close and the expiry (M10 G-H10; ARCH
/// §8.2 venue group, A18) on `fixture`, in both meanings — (A) the wallet emptied right after
/// the open and kept at 0 (the reaped wallet refuses a separate payment below the rent minimum,
/// [`reap`]'s control, so the credit must ride on the position's close), (B) emptied inside the
/// exit's own transaction:
///
/// - `liquidate` after a real whale pump (≈ 2.2× the price): before it lands, the keeper's
///   once-a-second attempts are refused only with a "not yet" code (`NotLiquidatable`,
///   `ExitBoundNotMet`) and under (B) roll their drain back; when it lands the reaped wallet is
///   credited exactly the payout plus the position's and the exposure record's rent;
/// - `sol_settle` S3: the market's Emergency flag set 300 s before the deadline, the S3 chain's
///   `s3_wait_s` run, and the expired position settled S3 at the timer onto the reaped wallet,
///   credited exactly `V − K_s − x` plus both records' rent.
///
/// S1 and S2 are not run here (S1 runs on the mock's zero-lamport borrower,
/// `engine_it::zero_lamport`; their arithmetic is venue-independent). Every instruction goes
/// through `Env::send`: the hooks (I3, I9, I12) run on each one.
fn zero_lamport_keeper_exits(fixture: RayFixture) {
    for meaning in ['A', 'B'] {
        let what = format!("{fixture:?} ({meaning})");

        // The liquidation.
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let kp = b.kp.insecure_clone();
        if meaning == 'A' {
            reap(&mut env, &kp);
        }
        env.tick(&rm, 3);
        let (_, y) = rm.reserves(&env.svm).unwrap();
        whale_buy(&mut env, &rm, y / 1_000 * 484);
        let keeper = env.funded_keypair();
        let mut landed = None;
        for _ in 0..30 {
            env.tick(&rm, 1);
            let owed = records(&env, &rm, &b.key(), nonce);
            let wallet = lamports(&env, &b.key());
            let ixs = exit_tx(
                &env,
                meaning,
                &kp,
                liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
            );
            let res = if meaning == 'A' {
                env.send(&ixs, &[&keeper])
            } else {
                env.send(&ixs, &[&kp, &keeper])
            };
            match custom_code(&res) {
                None => {
                    landed = Some((ok(res), owed));
                    break;
                }
                Some(c) if c == code(E::NotLiquidatable) || c == code(E::ExitBoundNotMet) => {
                    assert_eq!(lamports(&env, &b.key()), wallet, "{what}: rolled back");
                }
                Some(c) => panic!("{what}: liquidate failed with {c}"),
            }
        }
        let (meta, owed) = landed.unwrap_or_else(|| panic!("{what}: never liquidated"));
        let ev: Liquidate = events::one(&meta);
        assert!(ev.buyback_cost > 0);
        let got = lamports(&env, &b.key());
        assert_eq!(
            got,
            owed + ev.borrower_payout,
            "{what}: the liquidation credits the payout and both records' rent"
        );
        assert!(
            got >= env.svm.minimum_balance_for_rent_exemption(0),
            "{what}: rent-exempt"
        );
        assert!(read::account(&env.svm, &pda::position(&rm.market(), &b.key(), nonce).0).is_none());

        // S3 under the market's Emergency flag.
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let kp = b.kp.insecure_clone();
        if meaning == 'A' {
            reap(&mut env, &kp);
        }
        let pos = env.position(&rm, &b.key(), nonce);
        let slot = env.clock().slot;
        env.set_clock(pos.deadline - 300, slot + 1_500_000);
        env.tick(&rm, 3);
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[set_route_broken_ix(
                &ea.pubkey(),
                shortwin::PauseScope::Market,
                0,
                true,
                Some(rm.market()),
            )],
            &[&ea],
        ));
        env.warp(1);
        env.crank(&rm);
        let since = env.market(&rm.market()).no_route_since;
        assert_eq!(since, env.now(), "{what}: the S3 chain starts");
        let due = (since + i64::from(pos.s3_wait_s)).max(pos.deadline) + 1;
        let keeper = env.funded_keypair();
        while env.now() < due {
            let step = (due - env.now()).min(30);
            env.warp(step);
            env.crank(&rm);
        }
        let owed = records(&env, &rm, &b.key(), nonce);
        let ixs = exit_tx(
            &env,
            meaning,
            &kp,
            sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce),
        );
        let meta = if meaning == 'A' {
            ok(env.send(&ixs, &[&keeper]))
        } else {
            ok(env.send(&ixs, &[&kp, &keeper]))
        };
        let ev: SolSettle = events::one(&meta);
        assert_eq!(ev.condition, 3, "{what}: S3");
        let got = lamports(&env, &b.key());
        assert_eq!(
            got,
            owed + ev.borrower_amount,
            "{what}: S3 credits V − K_s − x and both records' rent"
        );
        assert!(
            got >= env.svm.minimum_balance_for_rent_exemption(0),
            "{what}: rent-exempt"
        );
        assert!(read::account(&env.svm, &pda::position(&rm.market(), &b.key(), nonce).0).is_none());
    }
}

/// G-H10's keeper exits on AMM v4 (Fartcoin; [`zero_lamport_keeper_exits`]).
#[test]
fn a_zero_lamport_borrower_is_liquidated_and_settled_on_amm_v4() {
    zero_lamport_keeper_exits(FART);
}

/// G-H10's keeper exits on CPMM, on USELESS and on the Token-2022 pool
/// ([`zero_lamport_keeper_exits`]).
#[test]
fn a_zero_lamport_borrower_is_liquidated_and_settled_on_cpmm() {
    zero_lamport_keeper_exits(USELESS);
    zero_lamport_keeper_exits(T22);
}
