//! Account-constraint cases for the lender instructions and `sweep_protocol_fees` that the
//! behaviour suites (`tests/lender_it`, `tests/market_create.rs`, `tests/oracle_crank.rs`) do
//! not repeat. Until M1 integration this file held the foundation's `NotImplemented` stubs; every
//! M1 handler is now real and each track's tests replaced its stub case.

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use anchor_lang::prelude::Pubkey;
use shortwin::errors::ShortwinError;
use shortwin::state::LenderPosition;
use shortwin_litesvm_tests::errors::{assert_anchor_err, assert_err};
use shortwin_litesvm_tests::fixtures::{self, MarketKeys};
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::tokens::{self, Holder, TokenProgram};
use shortwin_litesvm_tests::{Env, pda};
use solana_signer::Signer;

fn market_with_holder(env: &mut Env, program: TokenProgram) -> (MarketKeys, Holder) {
    let (k, authority) = env.install_market(program, 6);
    let payer = env.payer.insecure_clone();
    let h = tokens::holder(
        &mut env.svm,
        &payer,
        &k.mint,
        &authority,
        1_000_000,
        program,
    );
    (k, h)
}

fn lender_keys(k: &MarketKeys, h: &Holder) -> LenderKeys {
    LenderKeys {
        owner: h.owner.pubkey(),
        mint: k.mint,
        token_program: k.token_program,
        owner_token_account: h.token_account,
    }
}

/// Writes an empty `LenderPosition` for `owner` (what `deposit` will create).
fn install_lender(env: &mut Env, market: &Pubkey, owner: &Pubkey) {
    let (key, bump) = pda::lender(market, owner);
    let l = LenderPosition {
        owner: *owner,
        market: *market,
        shares: 0,
        snap: 0,
        pending: 0,
        snap_r: 0,
        real: 0,
        claimed: 0,
        epoch: 0,
        lock: 0,
        lock_pi: shortwin_math::num::WAD,
        lock_epoch: shortwin_math::pool::NO_LOCK_EPOCH,
        lock_term: 0,
        lock_until: 0,
        bump,
        fwd_shares: 0,
        fwd_epoch: 0,
        fwd: 0,
        fwd_lag: 0,
        _reserved: [0; 10],
    };
    fixtures::set_borsh(&mut env.svm, &key, &l, LenderPosition::SPACE, 0);
}

#[test]
fn lender_constraints_bite() {
    let mut env = Env::initialized();
    let (k, h) = market_with_holder(&mut env, TokenProgram::Spl);
    let (other, _) = env.install_market(TokenProgram::Spl, 6);
    let owner = h.owner.insecure_clone();
    let lk = lender_keys(&k, &h);

    // Another market's pool vault.
    let mut ix = ix::deposit(&lk, 1_000, 0);
    ix.accounts[7].pubkey = other.pool_vault;
    assert!(env.send(&[ix], &[&owner]).is_err());

    // A token account of another mint.
    let payer = env.payer.insecure_clone();
    let foreign = tokens::create_token_account(
        &mut env.svm,
        &payer,
        &other.mint,
        &owner.pubkey(),
        TokenProgram::Spl,
    );
    let mut bad = lk;
    bad.owner_token_account = foreign;
    let res = env.send(&[ix::deposit(&bad, 1_000, 0)], &[&owner]);
    assert_anchor_err(&res, AnchorErrorCode::ConstraintTokenMint);

    // Someone else's lender position.
    install_lender(&mut env, &k.market, &owner.pubkey());
    let thief = env.funded_keypair();
    let mut ix = ix::claim_sol(&thief.pubkey(), &k.market);
    ix.accounts[2].pubkey = pda::lender(&k.market, &owner.pubkey()).0;
    let res = env.send(&[ix], &[&thief]);
    assert_anchor_err(&res, AnchorErrorCode::ConstraintSeeds);
}

#[test]
fn sweep_protocol_fees_checks_the_recipient() {
    let mut env = Env::initialized();
    let (k, _) = env.install_market(TokenProgram::Spl, 6);
    let recipient = env.fee_recipient.pubkey();
    // Nothing to sweep: succeeds and moves nothing (the lender track's tests cover the rest).
    let res = env.send(&[ix::sweep_protocol_fees(&k.market, &recipient)], &[]);
    assert!(res.is_ok());
    let res = env.send(
        &[ix::sweep_protocol_fees(&k.market, &Pubkey::new_unique())],
        &[],
    );
    assert_err(&res, ShortwinError::AccountMismatch);
}
