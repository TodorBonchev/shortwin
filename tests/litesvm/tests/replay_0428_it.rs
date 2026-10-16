//! T-REPLAY-0428 (ARCHITECTURE §3.0 "Bounded extras", §5.3 "Venue interface breakage", §8.2,
//! §9 M6; A41, V2-6, C0-4 (d); SPEC §11 O6): pump's 2026-04-28 breaking upgrade added
//! `buyback_fee_recipient` — a **fixed** account, from the live `Global` list — as a required
//! trailing account on **buys and sells** (R-PUMP §3.2). This replays that shape of interface
//! change against short.win's exits with `set_venue_extras` (the O6 Emergency power) and confirms
//! the exits recover **through the extras sets, without a program upgrade**.
//!
//! The deployed pump and PumpSwap ELFs *accept* unexpected trailing accounts (they never *require*
//! a new one on the fixtures — R-PUMP Update U4, R-VEN Update H5), so the "requires a new trailing
//! account" behaviour is modelled on the TEST-ONLY mock venue's `required_extras` knob, exactly as
//! T-REPLAY-PDA (`engine_it::o6`) models a per-mint-PDA change. Here the added account is a fixed
//! key (the 2026-04-28 shape), on both the sell leg (opens) and the buy leg (closes / exits). On
//! the real curve since pump-final §6 (the v3 list), `pump_it::structure` shows a published extra
//! appended to the `sell_v3` and `buy_v3` CPIs after the IDL's 17 accounts — a synthetic buyback
//! past `rT` included — and ignored by pump.
//!
//! It shows: the change strands every exit (the mock's `ExtrasMismatch`); the Emergency multisig
//! publishes the fixed account for the sell and buy kinds; opens and exits recover through the
//! `current` set; a hostile `current` write is bypassed with `previous`; a Governance-pinned
//! `stable` set survives quick hostile writes (M2-SEC-2); and `none` works once the venue drops
//! the requirement. No program upgrade is needed.
//!
//! Every transaction goes through `Env::send` (the invariant hooks run after each one). Built on
//! the **test-venue** short.win (`scripts/build-test-venue.sh`, or `SHORTWIN_TEST_VENUE_SO`).

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::ExtraEntryArg;
use shortwin::events::VenueExtrasSet;
use shortwin::venues::{
    EXTRAS_SET_CURRENT, EXTRAS_SET_NONE, EXTRAS_SET_PREVIOUS, EXTRAS_SET_STABLE, ExtrasSel,
};
use shortwin_litesvm_tests::engine::{
    MarketSpec, MockMarket, VENUE_HINT_MOCK, close_short_ix, mock_extras_key, open_short_ix,
    set_venue_extras_ix,
};
use shortwin_litesvm_tests::errors::{custom_code, ok};
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use shortwin_litesvm_tests::{Env, TxResult, events};
use solana_keypair::Keypair;
use solana_signer::Signer;

/// A position that fits every default cap (≈ 3 SOL of notional on the default pool).
const Q: u64 = 3_000_000_000_000;
/// The extras-kind index of the sell leg (opens) and the buy leg (closes / exits).
const SELL: u8 = 0;
const BUY: u8 = 1;
/// The mock's error when the trailing accounts differ from its `required_extras`.
const MOCK_EXTRAS_MISMATCH: u32 = 6_011;

fn setup() -> (Env, MockMarket) {
    let mut env = Env::test_venue();
    let mm = env.mock_market(MarketSpec::default());
    (env, mm)
}

/// A fixed-key extras entry (the 2026-04-28 shape: a live-list recipient, not a PDA).
fn fixed(key: Pubkey, writable: bool) -> ExtraEntryArg {
    ExtraEntryArg {
        kind: 1,
        writable,
        key,
        seeds: vec![],
    }
}

/// Publishes `entries` for the mock's `ix_kind` set, signed by `authority` (Emergency =
/// `pause_authority`; Governance also pins the set as `stable`).
fn set_extras(
    env: &mut Env,
    authority: &Keypair,
    ix_kind: u8,
    entries: Vec<ExtraEntryArg>,
) -> TxResult {
    let ix = set_venue_extras_ix(&authority.pubkey(), VENUE_HINT_MOCK, ix_kind, entries);
    env.send(&[ix], &[authority])
}

fn sel(set: u8) -> ExtrasSel {
    ExtrasSel { set, bumps: [0; 4] }
}

fn open_with(
    env: &mut Env,
    mm: &MockMarket,
    b: &Keypair,
    nonce: u64,
    s: ExtrasSel,
    extras: &[AccountMeta],
) -> TxResult {
    let ix = open_short_ix(mm, &b.pubkey(), nonce, Q, 0, u64::MAX, u64::MAX, s, extras);
    env.send(&[ix], &[b])
}

fn close_with(
    env: &mut Env,
    mm: &MockMarket,
    b: &Keypair,
    nonce: u64,
    s: ExtrasSel,
    extras: &[AccountMeta],
) -> TxResult {
    let ix = close_short_ix(mm, &b.pubkey(), nonce, u64::MAX, s, extras);
    env.send(&[ix], &[b])
}

/// The trailing accounts a selected set carries: the `VenueExtras` account (consumed by short.win's
/// parser, not forwarded) then the fixed recipient (forwarded to the venue).
fn extras_metas(recipient: Pubkey) -> [AccountMeta; 2] {
    [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(recipient, false),
    ]
}

// ---- tests --------------------------------------------------------------------------------------

/// The whole replay: a position opens on the old interface; the 2026-04-28 change adds a required
/// fixed recipient on both legs, so the borrower's close is stranded (`ExtrasMismatch`); the
/// Emergency multisig publishes the recipient for the sell and buy kinds; the close and a new open
/// recover through the `current` set — no program upgrade.
#[test]
fn the_2026_04_28_added_recipient_is_recovered_through_the_current_set() {
    let (mut env, mm) = setup();
    let recipient = Pubkey::new_unique();
    let mut b = env.borrower();
    let (res, nonce) = env.open(&mm, &mut b, Q);
    ok(res);
    env.tick(&mm, 1);

    // The 2026-04-28 interface change: the venue now requires the recipient on every swap.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![recipient]
    });
    let res = close_with(&mut env, &mm, &b.kp, nonce, ExtrasSel::NONE, &[]);
    assert_eq!(
        custom_code(&res),
        Some(MOCK_EXTRAS_MISMATCH),
        "the borrower's exit is stranded by the added account"
    );

    // The Emergency multisig publishes the fixed recipient for the buy and the sell kinds.
    let ea = env.pause_authority.insecure_clone();
    let meta = ok(set_extras(
        &mut env,
        &ea,
        BUY,
        vec![fixed(recipient, false)],
    ));
    let ev: VenueExtrasSet = events::one(&meta);
    assert_eq!(ev.venue, VENUE_HINT_MOCK);
    ok(set_extras(
        &mut env,
        &ea,
        SELL,
        vec![fixed(recipient, false)],
    ));

    // A trailing account that is not the published fixed key is refused by short.win's own parser
    // (`BadExtras`), before any CPI; the published one recovers the close.
    let wrong = extras_metas(Pubkey::new_unique());
    assert_eq!(
        custom_code(&close_with(
            &mut env,
            &mm,
            &b.kp,
            nonce,
            sel(EXTRAS_SET_CURRENT),
            &wrong
        )),
        Some(u32::from(shortwin::errors::ShortwinError::BadExtras))
    );
    let good = extras_metas(recipient);
    ok(close_with(
        &mut env,
        &mm,
        &b.kp,
        nonce,
        sel(EXTRAS_SET_CURRENT),
        &good,
    ));

    // A new open (the sell leg) follows the same set — the interface change is fully recovered.
    ok(open_with(
        &mut env,
        &mm,
        &b.kp,
        7,
        sel(EXTRAS_SET_CURRENT),
        &good,
    ));
    env.tick(&mm, 1);
    ok(close_with(
        &mut env,
        &mm,
        &b.kp,
        7,
        sel(EXTRAS_SET_CURRENT),
        &good,
    ));
}

/// V2-6: a hostile `current` write never bricks exits — the caller selects `previous` (the last
/// good set); and once the venue drops the requirement, `none` works whatever the sets say.
#[test]
fn a_hostile_current_write_is_bypassed_with_previous_then_none() {
    let (mut env, mm) = setup();
    let recipient = Pubkey::new_unique();
    let ea = env.pause_authority.insecure_clone();
    ok(set_extras(
        &mut env,
        &ea,
        BUY,
        vec![fixed(recipient, false)],
    ));
    ok(set_extras(
        &mut env,
        &ea,
        SELL,
        vec![fixed(recipient, false)],
    ));
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![recipient]
    });
    let good = extras_metas(recipient);
    let b = env.borrower();
    ok(open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT),
        &good,
    ));
    env.tick(&mm, 1);

    // A hostile Emergency write moves the good set to `previous` and names junk as `current`.
    let junk = Pubkey::new_unique();
    ok(set_extras(&mut env, &ea, BUY, vec![fixed(junk, false)]));
    assert_eq!(
        custom_code(&close_with(
            &mut env,
            &mm,
            &b.kp,
            0,
            sel(EXTRAS_SET_CURRENT),
            &extras_metas(junk),
        )),
        Some(MOCK_EXTRAS_MISMATCH),
        "the venue refuses the hostile account"
    );
    // `previous` still carries the good recipient.
    ok(close_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_PREVIOUS),
        &good,
    ));

    // The venue drops the requirement: `none` works whatever the sets say.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![]
    });
    ok(open_with(
        &mut env,
        &mm,
        &b.kp,
        1,
        sel(EXTRAS_SET_NONE),
        &[],
    ));
    env.tick(&mm, 1);
    ok(close_with(&mut env, &mm, &b.kp, 1, ExtrasSel::NONE, &[]));
}

/// M2-SEC-2: a Governance write pins the set as `stable`, which quick hostile Emergency writes
/// cannot displace — an exit selecting `stable` recovers even after two hostile `current` writes.
#[test]
fn a_governance_pinned_stable_set_survives_hostile_writes() {
    let (mut env, mm) = setup();
    let recipient = Pubkey::new_unique();
    let gov = env.governance.insecure_clone();
    // Governance publishes the good recipient for both legs; the write pins it as `stable`.
    ok(set_extras(
        &mut env,
        &gov,
        BUY,
        vec![fixed(recipient, false)],
    ));
    ok(set_extras(
        &mut env,
        &gov,
        SELL,
        vec![fixed(recipient, false)],
    ));
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![recipient]
    });
    let good = extras_metas(recipient);
    let mut b = env.borrower();
    let (res, nonce) = {
        let ix = open_short_ix(
            &mm,
            &b.key(),
            0,
            Q,
            0,
            u64::MAX,
            u64::MAX,
            sel(EXTRAS_SET_CURRENT),
            &good,
        );
        let kp = b.kp.insecure_clone();
        (env.send(&[ix], &[&kp]), 0u64)
    };
    ok(res);
    b.next_nonce = 1;
    env.tick(&mm, 1);

    // Two quick hostile Emergency writes to `current` (they cannot touch `stable`).
    let ea = env.pause_authority.insecure_clone();
    ok(set_extras(
        &mut env,
        &ea,
        BUY,
        vec![fixed(Pubkey::new_unique(), false)],
    ));
    ok(set_extras(
        &mut env,
        &ea,
        BUY,
        vec![fixed(Pubkey::new_unique(), false)],
    ));

    // `stable` still carries the Governance-pinned recipient, so the exit recovers.
    ok(close_with(
        &mut env,
        &mm,
        &b.kp,
        nonce,
        sel(EXTRAS_SET_STABLE),
        &good,
    ));
}
