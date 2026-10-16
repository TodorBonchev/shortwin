//! T-RAY-TRACE, T-RAY-CU and T-SIZE on Raydium (ARCH §6.1, §6.2, §6.4, §8.2; U4, U25), measured
//! on the real programs under LiteSVM's mainnet feature set (p-token), printed with
//! `-- --nocapture`.
//!
//! **Trace** (ARCH §6.2's Raydium column plus, as measured on the mock in M2, the two account
//! creations of a first open — the position `init` and the exposure `init_if_needed`): an open
//! is 15 entries (a repeat borrower's 14), a close 9, as the mock; `crank_price` 2; a sell leg
//! of either program is approve 1 + WSOL creation 3 + swap 3 + close 1, a buy leg WSOL creation
//! 3 + swap 3 + close 1. **CU** within the ARCH §6.1 limits (open 150 k, close / liquidation /
//! expiry 200 k, crank 60 k, `create_market` 250 k). **Size**: every Raydium trading transaction
//! fits 1,232 B as a v0 transaction with the protocol ALT (A28); AMM v4's also as a legacy one.

use shortwin_litesvm_tests::engine::{liquidate_ix, sol_settle_ix};
use shortwin_litesvm_tests::raydium::{
    create_ray_market_ix, install_alt, legacy_size, protocol_alt_addresses, v0_size,
};

use crate::common::*;

const OPEN_TRACE: usize = 15;
const OPEN_TRACE_REPEAT: usize = 14;
const CLOSE_TRACE: usize = 9;
const OPEN_CU_MAX: u64 = 150_000;
const EXIT_CU_MAX: u64 = 200_000;
const CRANK_CU_MAX: u64 = 60_000;
const CREATE_CU_MAX: u64 = 250_000;
const TX_MAX: usize = 1_232;

fn trace_entries(meta: &TransactionMetadata) -> usize {
    1 + meta.inner_instructions.first().map_or(0, Vec::len)
}

#[test]
fn trace_and_cu() {
    for fixture in [FART, USELESS, T22, CREATOR_FEE] {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(fixture);
        let creator = env.funded_keypair();
        let ix = create_ray_market_ix(&env.svm, &creator.pubkey(), &pool, fixture.hint());
        let meta = ok(env.send(&[ix], &[&creator]));
        println!(
            "{fixture:?} create_market: {} trace entries, {} CU",
            trace_entries(&meta),
            meta.compute_units_consumed
        );
        assert!(meta.compute_units_consumed <= CREATE_CU_MAX);
        let q = q_of(&env, &pool);
        let rm = env.ray_market_listed(RaySpec::new(fixture, q * 40), pool);
        let meta = ok(env.send(&[crank_price_ix(&rm)], &[]));
        println!(
            "{fixture:?} crank_price: {} trace entries, {} CU",
            trace_entries(&meta),
            meta.compute_units_consumed
        );
        assert_eq!(trace_entries(&meta), 2);
        assert!(meta.compute_units_consumed <= CRANK_CU_MAX);

        let mut b = env.borrower();
        let (_, n0, meta) = open_ok(&mut env, &rm, &mut b, q);
        let (t, cu) = (trace_entries(&meta), meta.compute_units_consumed);
        println!("{fixture:?} open_short: {t} trace entries, {cu} CU");
        assert_eq!(t, OPEN_TRACE, "logs:\n{}", meta.pretty_logs());
        assert!(cu <= OPEN_CU_MAX, "open_short {cu} CU");
        let (_, n1, meta) = open_ok(&mut env, &rm, &mut b, q / 2);
        assert_eq!(trace_entries(&meta), OPEN_TRACE_REPEAT, "a repeat borrower");

        env.tick(&rm, 2);
        let (_, meta) = close_ok(&mut env, &rm, &b, n0);
        let (t, cu) = (trace_entries(&meta), meta.compute_units_consumed);
        println!("{fixture:?} close_short: {t} trace entries, {cu} CU");
        assert_eq!(t, CLOSE_TRACE, "logs:\n{}", meta.pretty_logs());
        assert!(cu <= EXIT_CU_MAX, "close_short {cu} CU");

        // A keeper's calls on a healthy position: refused by the program, not by a budget.
        let keeper = env.funded_keypair();
        env.tick(&rm, 1);
        let settle = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), n1);
        let meta = ok(env.send(&[settle], &[&keeper]));
        println!(
            "{fixture:?} sol_settle (no-op): {} trace entries, {} CU",
            trace_entries(&meta),
            meta.compute_units_consumed
        );
        assert!(meta.compute_units_consumed <= 150_000);
        close_ok(&mut env, &rm, &b, n1);
    }
}

/// T-SIZE: the Raydium trading transactions with the protocol ALT (A28) — `open_short`,
/// `close_short`, `liquidate` (the widest: keeper and borrower both named), `crank_price` —
/// fit 1,232 B and 64 accounts; an open and a close **execute** as v0 transactions through the
/// table (the invariant hooks run). AMM v4's also fit as legacy transactions (ARCH §6.4: "≈ 950
/// B even without the ALT"); CPMM's are measured and printed.
#[test]
fn transaction_sizes() {
    let alt = Pubkey::new_unique();
    let addresses = protocol_alt_addresses();
    for fixture in [FART, USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        install_alt(&mut env.svm, &alt, &addresses);
        let b = env.borrower();
        let keeper = env.funded_keypair();
        let payer = env.payer.pubkey();
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
        let close = close_short_ix(&rm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
        let liq = liquidate_ix(&rm, &keeper.pubkey(), &b.key(), 0);
        for (what, ix) in [
            ("open_short", &open),
            ("close_short", &close),
            ("liquidate", &liq),
            ("crank_price", &crank_price_ix(&rm)),
        ] {
            let (legacy, lkeys) = legacy_size(&payer, std::slice::from_ref(ix));
            let (v0, vkeys) = v0_size(&payer, std::slice::from_ref(ix), &alt, &addresses);
            println!(
                "{fixture:?} {what}: legacy {legacy} B / {lkeys} keys, v0 + ALT {v0} B / {vkeys} keys"
            );
            assert!(v0 <= TX_MAX && vkeys <= 64, "{fixture:?} {what}: {v0} B");
            if rm.kind() == VenueKind::RaydiumV4 {
                assert!(legacy <= TX_MAX, "{fixture:?} {what}: legacy {legacy} B");
            }
        }
        let meta = ok(env.send_v0(open, &[&b.kp], &alt, &addresses));
        let ev: Open = events::one(&meta);
        assert_eq!(ev.q, q);
        env.tick(&rm, 1);
        let meta = ok(env.send_v0(close, &[&b.kp], &alt, &addresses));
        let ev: Close = events::one(&meta);
        assert_eq!(ev.q, q);
    }
}
