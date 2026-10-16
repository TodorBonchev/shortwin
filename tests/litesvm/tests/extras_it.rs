//! T-REPLAY-PDA on the mock venue (ARCH §3.0 "Bounded extras", §5.3 "Venue interface
//! breakage", §8.2; A40, A41, V2-6; SPEC §11 O6): the venue changes its swap interface — it
//! starts requiring a per-mint PDA as a trailing account (the mock's `required_extras`, like
//! pump's 2026-04-28 change) — so every exit CPI fails. The Emergency multisig's two powers
//! recover it without a program upgrade and without moving funds:
//!
//! - it flags the route (the failure cannot be recorded on-chain): the S3 chain starts, and a
//!   liquidatable position settles in SOL after `s3_wait_s` (`sol_settle` makes no venue CPI);
//! - it publishes a derivation template: exits pass the re-derived account again; a hostile
//!   `current` set is bypassed with `previous`, and `none` always works once the venue needs
//!   no extras.

#![allow(clippy::result_large_err)]

#[path = "settle_support.rs"]
mod support;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::errors::ShortwinError as E;
use shortwin::state::{PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER};
use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{VENUE_HINT_MOCK, mock_extras_key, set_venue_extras_ix};
use solana_signer::Signer;
use support::*;

const SELL: u8 = 0;
const BUY: u8 = 1;

/// The mock's error when the trailing accounts differ from its `required_extras`.
const MOCK_EXTRAS_MISMATCH: u32 = 6_011;

fn seed_const(b: &[u8]) -> SeedPartArg {
    let mut bytes = [0u8; 32];
    bytes[..b.len()].copy_from_slice(b);
    SeedPartArg {
        kind: SEED_CONST,
        len: b.len() as u8,
        bytes,
    }
}

fn seed_mint() -> SeedPartArg {
    SeedPartArg {
        kind: SEED_PLACEHOLDER,
        len: PLACEHOLDER_MINT,
        bytes: [0; 32],
    }
}

fn set_extras(env: &mut Env, ix_kind: u8, entries: Vec<ExtraEntryArg>) {
    let ea = env.pause_authority.insecure_clone();
    let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, ix_kind, entries);
    ok(env.send(&[ix], &[&ea]));
}

fn sel(set: u8, bump0: u8) -> ExtrasSel {
    ExtrasSel {
        set,
        bumps: [bump0, 0, 0, 0],
    }
}

fn close_with(
    env: &mut Env,
    mm: &MockMarket,
    b: &Borrower,
    nonce: u64,
    s: ExtrasSel,
    extras: &[AccountMeta],
) -> TxResult {
    let ix = close_short_ix(mm, &b.key(), nonce, u64::MAX, s, extras);
    let kp = b.kp.insecure_clone();
    env.send(&[ix], &[&kp])
}

/// T-REPLAY-PDA, the whole replay: the interface change strands every exit; the Emergency
/// flag takes a liquidatable position to S3 (no CPI); the template recovers closes and, once the
/// flag is cleared, opens; a hostile `current` set is bypassed with `previous`; `none` works
/// again once the venue drops the requirement.
#[test]
fn a_new_per_mint_pda_is_recovered_by_the_emergency_powers() {
    let mut sc = Sc::default_market();
    // Past the listing's graduation grace, so S3 can start (ARCH §5.3).
    sc.env.warp(i64::from(sc.pos().grad_grace_s));
    sc.env.tick(&sc.mm, 5);
    // Position 1 becomes liquidatable (below S1); position 2 opens healthy afterwards.
    let p0 = sc.spot();
    let kb = sc.s1_boundary();
    sc.move_to(scale(p0, kb * 0.8));
    sc.env.tick(&sc.mm, 20);
    let mut b2 = sc.env.borrower();
    let (_o, n2, _) = open_ok(&mut sc.env, &sc.mm, &mut b2, Q);
    sc.env.tick(&sc.mm, 2);

    // The venue changes its interface: a per-mint PDA of some program joins the swap.
    let prog = Pubkey::new_unique();
    let (extra, bump) = Pubkey::find_program_address(&[b"extra", sc.mm.mint().as_ref()], &prog);
    sc.knobs(|k| k.required_extras = vec![extra]);
    let res = close_with(&mut sc.env, &sc.mm, &b2, n2, ExtrasSel::NONE, &[]);
    assert_eq!(
        custom_code(&res),
        Some(MOCK_EXTRAS_MISMATCH),
        "exits are stuck"
    );
    // The venue still reads fine: executable, observed, no S3 evidence of its own.
    sc.env.warp(1);
    let out = sc.settle_ok();
    assert!(out.settled.is_none());
    assert_eq!(sc.market().no_route_since, 0);

    // Emergency flags the route: opens stop, the S3 chain starts.
    sc.set_route_broken(true);
    let mut b3 = sc.env.borrower();
    let (res, _) = sc.env.open(&sc.mm, &mut b3, Q);
    assert_err(&res, E::RouteBroken);
    // Emergency publishes the template (buy and sell legs): closes work again at once.
    let t = ExtraEntryArg {
        kind: 2,
        writable: false,
        key: prog,
        seeds: vec![seed_const(b"extra"), seed_mint()],
    };
    set_extras(&mut sc.env, BUY, vec![t.clone()]);
    set_extras(&mut sc.env, SELL, vec![t]);
    let extras = [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(extra, false),
    ];
    // A hostile write lands on the buy set before the borrower closes: `previous` bypasses it.
    set_extras(
        &mut sc.env,
        BUY,
        vec![ExtraEntryArg {
            kind: 1,
            writable: false,
            key: Pubkey::new_unique(),
            seeds: vec![],
        }],
    );
    sc.env.warp(1);
    ok(close_with(
        &mut sc.env,
        &sc.mm,
        &b2,
        n2,
        sel(EXTRAS_SET_PREVIOUS, bump),
        &extras,
    ));

    // Position 1 (liquidatable) settles S3 once the flagged chain has run s3_wait_s.
    let pos = sc.pos();
    sc.crank_every(30, 19);
    assert!(
        sc.settle_ok().settled.is_none(),
        "the S3 timer is still running"
    );
    let (_s, ev) = sc.run_until_settled(40).expect("S3");
    assert_eq!(ev.condition, 3);
    assert_eq!(ev.lender_amount, ev.fv.min(pos.v - k_s(&pos)));
    assert!(sc.pos_opt().is_none());

    // The flag is cleared; opens follow the published sell template (current set).
    sc.set_route_broken(false);
    sc.env.warp(1);
    sc.crank();
    let ix = open_short_ix(
        &sc.mm,
        &b3.key(),
        0,
        Q,
        0,
        u64::MAX,
        u64::MAX,
        sel(EXTRAS_SET_CURRENT, bump),
        &extras,
    );
    let kp = b3.kp.insecure_clone();
    ok(sc.env.send(&[ix], &[&kp]));
    // The venue drops the requirement: `none` works whatever the sets say.
    sc.knobs(|k| k.required_extras = vec![]);
    sc.env.warp(1);
    ok(close_with(
        &mut sc.env,
        &sc.mm,
        &b3,
        0,
        ExtrasSel::NONE,
        &[],
    ));
}

/// `sol_settle` carries the ReadSet only (ARCH §3.8, C0-2): no extras segment is accepted, and
/// no extras set can reach it — a venue interface change never blocks a settlement.
#[test]
fn sol_settle_takes_no_extras() {
    let mut sc = Sc::default_market();
    sc.env.warp(1);
    let mut ix = sc.settle_ix();
    ix.accounts
        .push(AccountMeta::new_readonly(mock_extras_key(), false));
    let kp = sc.keeper.insecure_clone();
    assert_err(&sc.env.send(&[ix], &[&kp]), E::BadRemainingAccounts);
    // A hostile extras write and a venue that demands extras: sol_settle is unaffected.
    set_extras(
        &mut sc.env,
        BUY,
        vec![ExtraEntryArg {
            kind: 1,
            writable: true,
            key: Pubkey::new_unique(),
            seeds: vec![],
        }],
    );
    sc.knobs(|k| k.required_extras = vec![Pubkey::new_unique()]);
    sc.env.warp(1);
    assert!(sc.settle_ok().settled.is_none());
}
