//! The O6 Emergency powers (SPEC §11; ARCH §3.1, §3.0 "Bounded extras"; A40, A41; V2-6):
//! `set_route_broken` and `set_venue_extras`, their authorization and validation, and
//! T-REPLAY-PDA on the mock — a venue interface change that adds a per-mint PDA trailing
//! account is followed with a derivation template, and a hostile `current` set is bypassed
//! with `previous` or `none`; and (M10 G-A5) the extras denylist on all four extras sites —
//! `open_short`, `close_short`, `liquidate`, `expire` — key by key, with a landed control each,
//! since M10 batch 3 with the mock pool's own accounts on every site and the position's borrower
//! on the keeper exits.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::PauseScope;
use shortwin::errors::ShortwinError as E;
use shortwin::events::{RouteBrokenSet, VenueExtrasSet};
use shortwin::state::{
    MARKET_FLAG_ROUTE_BROKEN, PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER, VenueExtras,
    VenueKind,
};
use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_NONE, EXTRAS_SET_PREVIOUS, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{
    VENUE_HINT_MOCK, mock_extras_key, pool_denied_keys, set_route_broken_ix, set_venue_extras_ix,
    with_extras,
};
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::*;

const SELL: u8 = 0;
const BUY: u8 = 1;

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

fn template(program: Pubkey, seeds: Vec<SeedPartArg>) -> ExtraEntryArg {
    ExtraEntryArg {
        kind: 2,
        writable: false,
        key: program,
        seeds,
    }
}

fn fixed(key: Pubkey, writable: bool) -> ExtraEntryArg {
    ExtraEntryArg {
        kind: 1,
        writable,
        key,
        seeds: vec![],
    }
}

fn set_extras(env: &mut Env, ix_kind: u8, entries: Vec<ExtraEntryArg>) -> TxResult {
    let ea = env.pause_authority.insecure_clone();
    let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, ix_kind, entries);
    env.send(&[ix], &[&ea])
}

fn open_with(
    env: &mut Env,
    mm: &MockMarket,
    b: &Keypair,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> TxResult {
    let ix = open_short_ix(
        mm,
        &b.pubkey(),
        nonce,
        Q,
        0,
        u64::MAX,
        u64::MAX,
        sel,
        extras,
    );
    env.send(&[ix], &[b])
}

fn close_with(
    env: &mut Env,
    mm: &MockMarket,
    b: &Keypair,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> TxResult {
    let ix = close_short_ix(mm, &b.pubkey(), nonce, u64::MAX, sel, extras);
    env.send(&[ix], &[b])
}

fn sel(set: u8, bump0: u8) -> ExtrasSel {
    ExtrasSel {
        set,
        bumps: [bump0, 0, 0, 0],
    }
}

#[test]
fn set_route_broken_is_emergency_or_governance_only() {
    let (mut env, mm) = setup();
    let stranger = env.funded_keypair();
    let res = env.send(
        &[set_route_broken_ix(
            &stranger.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&stranger],
    );
    assert_err(&res, E::Unauthorized);
    let gov = env.governance.insecure_clone();
    let meta = ok(env.send(
        &[set_route_broken_ix(
            &gov.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&gov],
    ));
    let ev: RouteBrokenSet = events::one(&meta);
    assert!(ev.broken && ev.market == mm.market());
    assert!(env.market(&mm.market()).has_flag(MARKET_FLAG_ROUTE_BROKEN));
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Global,
            5,
            true,
            None,
        )],
        &[&ea],
    ));
    assert_eq!(env.config().venue_route_broken, 1 << 5);
    // Unknown bits and mismatched scopes are refused; the flag moves no funds (I4 runs on
    // every admin-signed instruction).
    let res = env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Global,
            9,
            true,
            None,
        )],
        &[&ea],
    );
    assert_err(&res, E::InvalidParams);
    let res = env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            None,
        )],
        &[&ea],
    );
    assert_err(&res, E::AccountMismatch);
}

#[test]
fn set_venue_extras_validates_and_rotates() {
    let (mut env, _mm) = setup();
    let stranger = env.funded_keypair();
    let res = env.send(
        &[set_venue_extras_ix(
            &stranger.pubkey(),
            VENUE_HINT_MOCK,
            SELL,
            vec![],
        )],
        &[&stranger],
    );
    assert_err(&res, E::Unauthorized);
    let prog = Pubkey::new_unique();
    // Invalid shapes.
    for bad in [
        vec![fixed(prog, false); 5],
        vec![fixed(shortwin::ID, false)],
        vec![fixed(Pubkey::default(), false)],
        vec![template(prog, vec![])],
        vec![template(shortwin::ID, vec![seed_mint()])],
        vec![template(prog, vec![seed_mint(); 5])],
        vec![template(
            prog,
            vec![SeedPartArg {
                kind: SEED_CONST,
                len: 33,
                bytes: [1; 32],
            }],
        )],
        vec![template(
            prog,
            vec![SeedPartArg {
                kind: SEED_PLACEHOLDER,
                len: 9,
                bytes: [0; 32],
            }],
        )],
        vec![ExtraEntryArg {
            kind: 3,
            writable: false,
            key: prog,
            seeds: vec![],
        }],
    ] {
        assert_err(&set_extras(&mut env, SELL, bad), E::BadExtras);
    }
    let ea = env.pause_authority.insecure_clone();
    let res = env.send(
        &[set_venue_extras_ix(
            &ea.pubkey(),
            VENUE_HINT_MOCK,
            3,
            vec![],
        )],
        &[&ea],
    );
    assert_err(&res, E::BadExtras);
    // Rotation: current → previous, versions count per set.
    let a = Pubkey::new_unique();
    let b = Pubkey::new_unique();
    let meta = ok(set_extras(&mut env, SELL, vec![fixed(a, false)]));
    let ev: VenueExtrasSet = events::one(&meta);
    assert_eq!(
        (ev.venue, ev.ix, ev.version, ev.keys[0]),
        (VENUE_HINT_MOCK, SELL, 1, a)
    );
    ok(set_extras(&mut env, SELL, vec![fixed(b, true)]));
    let ve: VenueExtras = read::zc(&env.svm, &mock_extras_key()).unwrap();
    let set = &ve.sets[usize::from(SELL)];
    assert_eq!(
        (
            set.current.version,
            set.current.count,
            set.current.entries[0].key
        ),
        (2, 1, b)
    );
    assert_eq!(set.current.entries[0].writable, 1);
    assert_eq!((set.previous.version, set.previous.entries[0].key), (1, a));
    // Governance may write too.
    let gov = env.governance.insecure_clone();
    ok(env.send(
        &[set_venue_extras_ix(
            &gov.pubkey(),
            VENUE_HINT_MOCK,
            BUY,
            vec![],
        )],
        &[&gov],
    ));
}

/// T-REPLAY-PDA on the mock (ARCH §8.2): the venue starts requiring a per-mint PDA as a
/// trailing swap account (the mock's `required_extras`, like pump's 2026-04-28 change). Exits
/// fail until the Emergency multisig publishes a derivation template; then opens and closes
/// pass it through, re-derived on-chain from the market's mint and the caller's bump.
#[test]
fn a_template_follows_a_new_per_mint_pda() {
    let (mut env, mm) = setup();
    let prog = Pubkey::new_unique();
    let (pda_key, bump) = Pubkey::find_program_address(&[b"extra", mm.mint().as_ref()], &prog);
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 1);
    // The venue changes its interface.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![pda_key]
    });
    let res = close_with(&mut env, &mm, &b.kp, nonce, ExtrasSel::NONE, &[]);
    assert_eq!(
        custom_code(&res),
        Some(6_011),
        "the mock's ExtrasMismatch: exits are stuck"
    );
    // Emergency publishes the template for the buy and the sell legs.
    let t = template(prog, vec![seed_const(b"extra"), seed_mint()]);
    ok(set_extras(&mut env, BUY, vec![t.clone()]));
    ok(set_extras(&mut env, SELL, vec![t]));
    let extras = [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(pda_key, false),
    ];
    // A wrong bump or a wrong account is refused by short.win's parser.
    let res = close_with(
        &mut env,
        &mm,
        &b.kp,
        nonce,
        sel(EXTRAS_SET_CURRENT, bump.wrapping_sub(1)),
        &extras,
    );
    assert_err(&res, E::BadExtras);
    let wrong = [
        extras[0].clone(),
        AccountMeta::new_readonly(Pubkey::new_unique(), false),
    ];
    assert_err(
        &close_with(
            &mut env,
            &mm,
            &b.kp,
            nonce,
            sel(EXTRAS_SET_CURRENT, bump),
            &wrong,
        ),
        E::BadExtras,
    );
    ok(close_with(
        &mut env,
        &mm,
        &b.kp,
        nonce,
        sel(EXTRAS_SET_CURRENT, bump),
        &extras,
    ));
    // Opens follow the same set.
    ok(open_with(
        &mut env,
        &mm,
        &b.kp,
        7,
        sel(EXTRAS_SET_CURRENT, bump),
        &extras,
    ));
}

/// V2-6: one wrong or hostile Emergency write never bricks exits — the caller selects the
/// `previous` set (or `none` when the venue needs no extras).
#[test]
fn a_hostile_current_set_is_bypassed() {
    let (mut env, mm) = setup();
    let prog = Pubkey::new_unique();
    let (pda_key, bump) = Pubkey::find_program_address(&[b"extra", mm.mint().as_ref()], &prog);
    let t = template(prog, vec![seed_const(b"extra"), seed_mint()]);
    ok(set_extras(&mut env, BUY, vec![t.clone()]));
    ok(set_extras(&mut env, SELL, vec![t]));
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![pda_key]
    });
    let good = [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(pda_key, false),
    ];
    let b = env.borrower();
    ok(open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, bump),
        &good,
    ));
    env.tick(&mm, 1);
    // A hostile write: the buy set now names a random account.
    let junk = Pubkey::new_unique();
    ok(set_extras(&mut env, BUY, vec![fixed(junk, false)]));
    let hostile = [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(junk, false),
    ];
    let res = close_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        &hostile,
    );
    assert_eq!(
        custom_code(&res),
        Some(6_011),
        "the venue refuses the hostile account"
    );
    ok(close_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_PREVIOUS, bump),
        &good,
    ));
    // A venue that needs no extras: `none` always works, whatever the sets say.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![]
    });
    ok(open_with(&mut env, &mm, &b.kp, 1, ExtrasSel::NONE, &[]));
    env.tick(&mm, 1);
    ok(close_with(&mut env, &mm, &b.kp, 1, ExtrasSel::NONE, &[]));
}

/// ARCH §2.4 rule 4 at use time: extras must be non-signers, not short.win-owned, not on the
/// instruction's denylist, writable exactly as flagged; the segment must match the selection.
#[test]
fn extras_parser_is_strict() {
    let (mut env, mm) = setup();
    let b = env.borrower();
    let ve = AccountMeta::new_readonly(mock_extras_key(), false);
    // `none` with trailing accounts; an unknown selection; a missing entry.
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        ExtrasSel::NONE,
        std::slice::from_ref(&ve),
    );
    assert_err(&res, E::BadRemainingAccounts);
    // 4 is the first unknown selection (3 is `stable` since M2-SEC-2).
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(4, 0),
        std::slice::from_ref(&ve),
    );
    assert_err(&res, E::BadExtras);
    ok(set_extras(
        &mut env,
        SELL,
        vec![fixed(mm.keys.pool_vault, false)],
    ));
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        std::slice::from_ref(&ve),
    );
    assert_err(&res, E::BadRemainingAccounts);
    // The market's pool vault (denylisted) and the market itself (short.win-owned).
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        &[
            ve.clone(),
            AccountMeta::new_readonly(mm.keys.pool_vault, false),
        ],
    );
    assert_err(&res, E::DeniedAccount);
    ok(set_extras(&mut env, SELL, vec![fixed(mm.market(), false)]));
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        &[ve.clone(), AccountMeta::new_readonly(mm.market(), false)],
    );
    assert_err(&res, E::DeniedAccount);
    // Writable flag mismatch.
    let key = Pubkey::new_unique();
    ok(set_extras(&mut env, SELL, vec![fixed(key, true)]));
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        &[ve.clone(), AccountMeta::new_readonly(key, false)],
    );
    assert_err(&res, E::BadExtras);
    // A signer extra.
    let signer = env.funded_keypair();
    ok(set_extras(
        &mut env,
        SELL,
        vec![fixed(signer.pubkey(), false)],
    ));
    let ix = open_short_ix(
        &mm,
        &b.key(),
        0,
        Q,
        0,
        u64::MAX,
        u64::MAX,
        sel(EXTRAS_SET_CURRENT, 0),
        &[ve.clone(), AccountMeta::new_readonly(signer.pubkey(), true)],
    );
    assert_err(&env.send(&[ix], &[&b.kp, &signer]), E::DeniedAccount);
    // The wrong VenueExtras account (another venue's).
    let other = AccountMeta::new_readonly(pda::venue_extras_seed(5), false);
    let res = open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        &[other, AccountMeta::new_readonly(signer.pubkey(), false)],
    );
    assert_err(&res, E::BadExtras);
    // An empty `current` set selected explicitly: just the VenueExtras account, then works.
    ok(set_extras(&mut env, SELL, vec![]));
    ok(open_with(
        &mut env,
        &mm,
        &b.kp,
        0,
        sel(EXTRAS_SET_CURRENT, 0),
        &[ve],
    ));
    let _ = EXTRAS_SET_NONE;
}

// ---- the denylist on every leg instruction (M10 G-A5) ------------------------------------------

/// Every key the denylist of an extras site must keep out of the venue CPI (ARCH §2.4 rule 4;
/// `open_short.rs` / `close_short.rs` / `liquidate.rs` deny arrays, the owner rule for the rest):
/// the config, the venue guard, the oracle cache, the market, its vaults, the trader PDAs, the
/// position and the exposure record (also before they exist: the open's), the seeding lender's
/// position and any `ShareEpoch` record (none on a fresh market), and since M10 batch 3 the
/// adopted pool's own accounts (the mock pool and its two vaults, `venues::pool_denylist`); the
/// caller adds the signers and, on the keeper exits, the position's borrower.
fn denied_keys(env: &Env, mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> Vec<(String, Pubkey)> {
    let market = mm.market();
    let epoch = env.market(&market).share_epoch;
    let lender = pda::lender(&market, &mm.lender.pubkey()).0;
    assert!(
        env.svm.get_account(&lender).is_some(),
        "the lender position exists"
    );
    let mut keys = vec![
        ("config".into(), pda::config()),
        ("venue_guard".into(), pda::venue_guard()),
        ("oracle_cache".into(), pda::oracle_cache()),
        ("market".into(), market),
        ("market_vault".into(), mm.keys.market_vault),
        ("pool_vault".into(), mm.keys.pool_vault),
        ("trader".into(), mm.keys.trader),
        ("trader_wsol".into(), mm.trader_wsol),
        ("position".into(), pda::position(&market, borrower, nonce).0),
        ("exposure".into(), pda::exposure(&market, borrower).0),
        ("lender position".into(), lender),
    ];
    keys.extend(pool_denied_keys(VenueKind::Mock, mm));
    // `ShareEpoch` records exist only once an epoch has rolled; an empty address of a future
    // record is not on the list (nothing there for a venue to touch, and only short.win can
    // create an account at its PDA).
    for e in [epoch, epoch.saturating_sub(1)] {
        let record = pda::share_epoch(&market, e).0;
        if env
            .svm
            .get_account(&record)
            .is_some_and(|a| a.owner == shortwin::ID)
        {
            keys.push((format!("share_epoch record {e}"), record));
        }
    }
    keys
}

/// Publishes `[fixed key]` (writable exactly as the instruction names the key) as the `ix_kind`
/// set and sends `build(sel, extras)` signed by `signers`, for every key of `keys`: each is
/// `DeniedAccount`.
fn assert_denied(
    env: &mut Env,
    label: &str,
    ix_kind: u8,
    keys: &[(String, Pubkey)],
    build: &dyn Fn(&Env, ExtrasSel, &[AccountMeta]) -> Instruction,
    signers: &[&Keypair],
) {
    let ve = AccountMeta::new_readonly(mock_extras_key(), false);
    let named = build(env, sel(EXTRAS_SET_CURRENT, 0), &[]).accounts;
    for (what, key) in keys {
        // Writable exactly as the instruction names the key: the flag check passes, so only the
        // denylist (or the owner and signer rules) can refuse it.
        let w = named.iter().any(|m| m.pubkey == *key && m.is_writable);
        ok(set_extras(env, ix_kind, vec![fixed(*key, w)]));
        let meta = AccountMeta {
            pubkey: *key,
            is_signer: false,
            is_writable: w,
        };
        let ix = build(env, sel(EXTRAS_SET_CURRENT, 0), &[ve.clone(), meta]);
        let res = env.send(&[ix], signers);
        assert_eq!(
            custom_code(&res),
            Some(code(E::DeniedAccount)),
            "{label}: {what} ({key}) as an extra"
        );
    }
}

/// The control of [`assert_denied`]: a fresh key in the same set shape, which the mock also
/// requires as its trailing account (its default requires none: an extra reaching it would fail
/// `ExtrasMismatch`, 6011). [`no_extras`] restores the mock.
fn harmless(env: &mut Env, mm: &MockMarket, ix_kind: u8) -> [AccountMeta; 2] {
    let key = Pubkey::new_unique();
    ok(set_extras(env, ix_kind, vec![fixed(key, false)]));
    update_knobs(env, &mm.pool, |k: &mut Knobs| k.required_extras = vec![key]);
    [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(key, false),
    ]
}

/// The mock back to requiring no extras.
fn no_extras(env: &mut Env, mm: &MockMarket) {
    update_knobs(env, &mm.pool, |k: &mut Knobs| k.required_extras = vec![]);
}

/// ARCH §2.4 rule 4 on all four extras sites (M10 G-A5; `extras_parser_is_strict` is the open's
/// pool vault and market only): every short.win account and market PDA of the denylist, the mock
/// pool and its vaults (M10 batch 3) — and the instruction's signers (the borrower on `open_short`
/// / `close_short`, the keeper on `liquidate` / `expire`), and on the keeper exits the position's
/// borrower, not a signer there (M10 batch 3) — published as a fixed extra is `DeniedAccount` on
/// `open_short` (the sell set), `close_short`, `liquidate` and `expire` (the buy set), the
/// position unchanged; the control on each site — the same set shape with a fresh key — lands
/// (the open, the close, a liquidation after a genuine 2.4× rise, an expiry after the deadline).
#[test]
fn the_denylist_holds_on_every_leg_instruction() {
    use shortwin_litesvm_tests::engine::{expire_ix, liquidate_ix};
    let (mut env, mm) = setup();
    let b = env.borrower();
    let bk = b.kp.insecure_clone();

    // open_short (sell set): the position and exposure do not exist yet.
    let mut keys = denied_keys(&env, &mm, &b.key(), 0);
    keys.push(("the borrower (signer)".into(), b.key()));
    let open_build = |_: &Env, s: ExtrasSel, x: &[AccountMeta]| {
        open_short_ix(&mm, &bk.pubkey(), 0, Q, 0, u64::MAX, u64::MAX, s, x)
    };
    assert_denied(&mut env, "open_short", SELL, &keys, &open_build, &[&bk]);
    let x = harmless(&mut env, &mm, SELL);
    ok(env.send(&[open_build(&env, sel(EXTRAS_SET_CURRENT, 0), &x)], &[&bk]));
    no_extras(&mut env, &mm);
    // The liquidation's position (another wallet: the wallet cap).
    let mut b2 = env.borrower();
    let (_, n1, _) = open_ok(&mut env, &mm, &mut b2, Q);
    env.tick(&mm, 1);

    // close_short (buy set) of position 0.
    let mut keys = denied_keys(&env, &mm, &b.key(), 0);
    keys.push(("the borrower (signer)".into(), b.key()));
    let close_build = |_: &Env, s: ExtrasSel, x: &[AccountMeta]| {
        close_short_ix(&mm, &bk.pubkey(), 0, u64::MAX, s, x)
    };
    let pos0 = pda::position(&mm.market(), &b.key(), 0).0;
    let before = env.svm.get_account(&pos0).map(|a| a.data);
    assert_denied(&mut env, "close_short", BUY, &keys, &close_build, &[&bk]);
    assert_eq!(env.svm.get_account(&pos0).map(|a| a.data), before);
    let x = harmless(&mut env, &mm, BUY);
    ok(env.send(&[close_build(&env, sel(EXTRAS_SET_CURRENT, 0), &x)], &[&bk]));
    no_extras(&mut env, &mm);

    // liquidate (buy set) of position 1 after a genuine 2.4× rise over 30 s.
    let k = env.funded_keypair();
    let kk = k.insecure_clone();
    let mut keys = denied_keys(&env, &mm, &b2.key(), n1);
    keys.push(("the keeper (signer)".into(), k.pubkey()));
    keys.push(("the borrower (not a signer)".into(), b2.key()));
    let bkey = b2.key();
    let liq_build = |_: &Env, s: ExtrasSel, x: &[AccountMeta]| {
        with_extras(liquidate_ix(&mm, &kk.pubkey(), &bkey, n1), s, x)
    };
    let actor = env.actor(&mm, 400_000_000_000_000, 3_000_000_000_000);
    let p0 = env.spot(&mm) as f64;
    for s in 1..=30 {
        env.warp(1);
        env.push_price(&mm, &actor, (p0 * 2.4f64.powf(f64::from(s) / 30.0)) as u128);
        env.crank(&mm);
    }
    env.tick(&mm, 20);
    let pos1 = pda::position(&mm.market(), &b2.key(), n1).0;
    let before = env.svm.get_account(&pos1).map(|a| a.data);
    assert_denied(&mut env, "liquidate", BUY, &keys, &liq_build, &[&k]);
    assert_eq!(env.svm.get_account(&pos1).map(|a| a.data), before);
    // The control: the keeper's attempts once a second (the reference needs its fresh buckets)
    // with a harmless extra, until the liquidation lands.
    let x = harmless(&mut env, &mm, BUY);
    let mut landed = None;
    for _ in 0..120 {
        env.tick(&mm, 1);
        match env.send(&[liq_build(&env, sel(EXTRAS_SET_CURRENT, 0), &x)], &[&k]) {
            Ok(meta) => {
                landed = Some(meta);
                break;
            }
            Err(f) => {
                let got = custom_code(&Err(f));
                assert!(
                    [E::ReferenceStale, E::NotLiquidatable, E::ExitBoundNotMet]
                        .iter()
                        .any(|e| got == Some(code(*e))),
                    "only not-yet: {got:?}"
                );
            }
        }
    }
    let _: shortwin::events::Liquidate = events::one(&landed.expect("liquidated"));
    no_extras(&mut env, &mm);

    // expire (buy set) of a fresh position after its deadline (a third wallet).
    let mut b3 = env.borrower();
    let (_, n2, _) = open_ok(&mut env, &mm, &mut b3, Q);
    let deadline = env.position(&mm, &b3.key(), n2).deadline;
    let now = env.now();
    env.warp(deadline - now + 1);
    env.crank(&mm);
    let mut keys = denied_keys(&env, &mm, &b3.key(), n2);
    keys.push(("the keeper (signer)".into(), k.pubkey()));
    keys.push(("the borrower (not a signer)".into(), b3.key()));
    let b3key = b3.key();
    let exp_build = |_: &Env, s: ExtrasSel, x: &[AccountMeta]| {
        with_extras(expire_ix(&mm, &kk.pubkey(), &b3key, n2), s, x)
    };
    let pos2 = pda::position(&mm.market(), &b3.key(), n2).0;
    let before = env.svm.get_account(&pos2).map(|a| a.data);
    assert_denied(&mut env, "expire", BUY, &keys, &exp_build, &[&k]);
    assert_eq!(env.svm.get_account(&pos2).map(|a| a.data), before);
    // The control after a few honest seconds (the reference needs completed buckets after the
    // jump to the deadline).
    env.tick(&mm, 3);
    let x = harmless(&mut env, &mm, BUY);
    let meta = ok(env.send(&[exp_build(&env, sel(EXTRAS_SET_CURRENT, 0), &x)], &[&k]));
    let _: shortwin::events::Expire = events::one(&meta);
}
