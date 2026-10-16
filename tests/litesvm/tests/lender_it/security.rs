//! M1 security review regressions on the lender lab (`M1-SEC-2`, `M1-SEC-3`): the review's
//! reproducers (borrower side injected as the M2 exits will book it), rewritten to assert the
//! fixed behaviour. The config / account / oracle findings are `tests/security.rs`.

use shortwin::errors::ShortwinError;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_math::num::WAD;

use crate::common::{Lab, code};

const U: u64 = 1_000_000_000;

/// The review's scenario: H is the only lender, a 3 SOL lump (penalty share / expiry fee)
/// starts vesting, and 10 minutes later H exits.
fn vesting_lab() -> Lab {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("H", 1_000_000_000_000).unwrap();
    lab.borrow30("pos", 500_000_000_000).unwrap();
    lab.advance(3_600, 0);
    lab.repay("pos", 0, 3 * U);
    lab.advance(600, 0);
    lab
}

/// M1-SEC-2: `withdraw(shares, min_tokens, min_sol)`. The last lender quotes its exit (the
/// unreleased vest is flushed to it, R3B-2) and sets `min_sol` to it. A bot's dust deposit
/// landed just before makes H "not last": the payout would drop to the 10 minutes released so
/// far, so the exit reverts (`SolBelowMin`) instead of handing the vest to the bot. H then
/// simply waits for the vest to release pro rata and keeps all but the bot's dust-sized share.
#[test]
fn sec_2_min_sol_turns_the_front_run_into_a_revert() {
    // Honest: the last lender is paid the whole lump, and a min_sol equal to it passes.
    let mut lab = vesting_lab();
    let shares = lab.position("H").unwrap().shares;
    let (_, paid) = lab.withdraw_bounded("H", shares, 0, 3 * U).unwrap();
    assert_eq!(paid, 3 * U);

    // Front-run: the bot deposits 1,000 raw units in the same slot as H's exit.
    let mut lab = vesting_lab();
    lab.deposit("BOT", 1_000).unwrap();
    let shares = lab.position("H").unwrap().shares;
    assert_eq!(
        lab.withdraw_bounded("H", shares, 0, 3 * U),
        Err(code(ShortwinError::SolBelowMin))
    );
    // H waits out the vest and exits: the lump released pro rata to the shares holding it.
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let (_, mut h) = lab.withdraw("H", shares).unwrap();
    h += lab.claim("H").unwrap();
    let bot = lab.claim("BOT").unwrap();
    assert!(h >= 3 * U - 10, "H {h}");
    assert!(bot <= 10, "bot {bot}");
    // Without min_sol the review's outcome (H 83,333,333, bot 2,916,666,667) is still what a
    // careless client gets: the bound is the caller's to set (the frontend quotes it).
}

/// M1-SEC-3 (ARCH §9 M0b note 4, implemented): the last shares cannot leave while tokens are
/// borrowed. The review's setup — one raw unit borrowed and the share value a hair above the
/// initial rate after a rounding burn — would have let the last lender take every share and
/// left `S = 0 ∧ ΣB_Q = 1`, bricking the market at the next rent credit. Now the full exit is
/// refused (`LastSharesBorrowed`); all but the last share can leave, and the last one follows
/// once the position resolves.
#[test]
fn sec_3_the_last_shares_stay_while_tokens_are_borrowed() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000).unwrap();
    assert_eq!(
        lab.withdraw("A", 1).unwrap().0,
        0,
        "1 share burnt for 0 tokens"
    );
    lab.borrow30("dust", 1).unwrap();
    let shares = lab.position("A").unwrap().shares;
    assert_eq!(
        lab.withdraw("A", shares),
        Err(code(ShortwinError::LastSharesBorrowed))
    );
    let (tokens, _) = lab.withdraw("A", shares - 1).unwrap();
    assert_eq!(tokens, 999_999);
    let m = lab.market();
    assert_eq!(
        (m.total_shares.get(), m.borrowed_q, m.tokens_free),
        (1, 1, 0)
    );
    // A rent credit now has a holder: no underflow (the review's brick).
    lab.advance(60, 2 * WAD);
    assert_eq!(lab.market().receivable, 1);
    assert_eq!(
        lab.withdraw("A", 1),
        Err(code(ShortwinError::LastSharesBorrowed))
    );
    // The position resolves (its rent paid); the last share leaves.
    let owed = lab.owed("dust");
    lab.repay("dust", owed, 0);
    let (tokens, _) = lab.withdraw("A", 1).unwrap();
    assert_eq!(tokens, 0, "one share of (T + VA) / (S + VS) = 2/1,001");
    let m = lab.market();
    assert_eq!(
        (m.total_shares.get(), m.borrowed_q, m.tokens_free),
        (0, 0, 1)
    );
    // A new lender deposits as usual, after the sweep a deposit into a pool every lender left
    // waits for (AIR-08).
    assert_eq!(
        lab.deposit("B", 1_000_000),
        Err(code(ShortwinError::WipeSweepPending))
    );
    lab.sweep_sol(0);
    lab.deposit("B", 1_000_000).unwrap();
}
