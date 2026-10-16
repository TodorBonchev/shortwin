//! Duty 10's extras rule (ARCH §7.1, §3.0 "Bounded extras"; A41, O6): the venue adds a
//! trailing per-mint PDA account to its swap (the mock's `required_extras`); the Emergency
//! multisig publishes a derivation template. The keeper's liquidation selects the extras set
//! that simulates — `current` — and, once a hostile `current` replaces it, `previous`; a
//! `current` the program refuses as `DeniedAccount` (M10 batch 3: a borrower's wallet or the
//! adopted pool's own accounts) does not stall it either.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::state::VenueKind;
use shortwin::state::{EXTRAS_IX_BUY, PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER};
use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_NONE, EXTRAS_SET_PREVIOUS};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_keeper::ExitResult;
use shortwin_litesvm_tests::engine::{VENUE_HINT_MOCK, pool_denied_keys, set_venue_extras_ix};
use solana_signer::Signer;

use crate::common::*;

fn template(program: Pubkey) -> ExtraEntryArg {
    let mut bytes = [0u8; 32];
    bytes[..5].copy_from_slice(b"extra");
    ExtraEntryArg {
        kind: 2,
        writable: false,
        key: program,
        seeds: vec![
            SeedPartArg {
                kind: SEED_CONST,
                len: 5,
                bytes,
            },
            SeedPartArg {
                kind: SEED_PLACEHOLDER,
                len: PLACEHOLDER_MINT,
                bytes: [0; 32],
            },
        ],
    }
}

fn publish(h: &Harness, entries: Vec<ExtraEntryArg>) {
    h.with_env(|env| {
        let ea = env.pause_authority.insecure_clone();
        let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, EXTRAS_IX_BUY as u8, entries);
        ok(env.send(&[ix], &[&ea]));
    });
}

/// Pumps `mm` +4 %/s (the whale's swaps carry the pool's new trailing account `extra`, if any)
/// until the keeper's liquidation of `pos` lands.
fn liquidate_after_pump(
    h: &mut Harness,
    mm: &MockMarket,
    pos: &Pubkey,
    extra: Option<Pubkey>,
) -> ExitResult {
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    for _ in 0..60 {
        h.with_env(|env| {
            env.warp(1);
            let target = env.spot(mm) * 104 / 100;
            let (_, _, mut ix) = env.push_price_ix(mm, &whale, target).unwrap();
            if let Some(extra) = extra {
                ix.accounts.push(AccountMeta::new_readonly(extra, false));
            }
            let kp = whale.kp.insecure_clone();
            ok(env.send(&[ix], &[&kp]));
        });
        let r = h.tick().clone();
        if let Some(x) = r.exits.iter().find(|e| {
            e.kind == ExitKind::Liquidate && e.position == *pos && e.result.landed().is_some()
        }) {
            return x.result.clone();
        }
    }
    panic!("no liquidation");
}

#[test]
fn liquidations_follow_a_published_template_and_bypass_a_hostile_one() {
    let (mut h, mms) = setup(2, KeeperConfig::default());
    let prog = Pubkey::new_unique();
    let (b0, n0) = open(&h, &mms[0], Q);
    let (b1, n1) = open(&h, &mms[1], Q);
    h.seconds(3);
    // Both pools add the per-mint PDA; Emergency publishes the template.
    let pdas: Vec<Pubkey> = mms
        .iter()
        .map(|mm| Pubkey::find_program_address(&[b"extra", mm.mint().as_ref()], &prog).0)
        .collect();
    for (mm, pda) in mms.iter().zip(&pdas) {
        h.with_env(|env| mock::update_knobs(env, &mm.pool, |k| k.required_extras = vec![*pda]));
    }
    publish(&h, vec![template(prog)]);
    let res = liquidate_after_pump(
        &mut h,
        &mms[0],
        &position_key(&mms[0], &b0.key(), n0),
        Some(pdas[0]),
    );
    match res {
        ExitResult::Landed { extras, .. } => assert_eq!(extras.set, EXTRAS_SET_CURRENT),
        other => panic!("{other:?}"),
    }
    // A hostile `current` (a wrong fixed key) moves the good template to `previous`.
    publish(
        &h,
        vec![ExtraEntryArg {
            kind: 1,
            writable: false,
            key: Pubkey::new_unique(),
            seeds: vec![],
        }],
    );
    let res = liquidate_after_pump(
        &mut h,
        &mms[1],
        &position_key(&mms[1], &b1.key(), n1),
        Some(pdas[1]),
    );
    match res {
        ExitResult::Landed { extras, .. } => assert_eq!(extras.set, EXTRAS_SET_PREVIOUS),
        other => panic!("{other:?}"),
    }
}

/// M10 batch 3 (ARCH §2.4 rule 4; the security review): an extra naming the position's borrower
/// or the adopted pool's own accounts is refused `DeniedAccount`, a refusal of the set itself.
/// A hostile `current` buy set naming one borrower's wallet stalls nothing but that set: the
/// keeper's liquidation of that borrower moves on and lands with `none` (M2-SEC-2: one wrong or
/// hostile Emergency write never bricks exits). Nor does a `current` naming the mock pool — every
/// market of the venue — stall the other market's liquidation.
#[test]
fn liquidations_bypass_a_current_set_the_program_denies() {
    let (mut h, mms) = setup(2, KeeperConfig::default());
    let (b0, n0) = open(&h, &mms[0], Q);
    let (b1, n1) = open(&h, &mms[1], Q);
    h.seconds(3);
    let fixed = |key: Pubkey| ExtraEntryArg {
        kind: 1,
        writable: true,
        key,
        seeds: vec![],
    };
    // The borrower's wallet (named writable by `liquidate`).
    publish(&h, vec![fixed(b0.key())]);
    let res = liquidate_after_pump(&mut h, &mms[0], &position_key(&mms[0], &b0.key(), n0), None);
    match res {
        ExitResult::Landed { extras, .. } => assert_eq!(extras.set, EXTRAS_SET_NONE),
        other => panic!("{other:?}"),
    }
    // The adopted pool itself, venue-wide.
    let pool = pool_denied_keys(VenueKind::Mock, &mms[1])[0].1;
    publish(&h, vec![fixed(pool)]);
    let res = liquidate_after_pump(&mut h, &mms[1], &position_key(&mms[1], &b1.key(), n1), None);
    match res {
        ExitResult::Landed { extras, .. } => assert_eq!(extras.set, EXTRAS_SET_NONE),
        other => panic!("{other:?}"),
    }
}
