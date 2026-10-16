//! I14 (ARCH §8.3; C0-2): every account that feeds pricing, route, tier or guard is present and
//! key-checked. Structural test over the remaining accounts of `open_short`, `close_short` and
//! `crank_price` on the mock: every slot omitted, replaced by a foreign key, or followed by an
//! extra account makes the instruction fail — never "no route", never a silent default.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::errors::ShortwinError as E;
use shortwin::venues::ExtrasSel;

use crate::common::*;

/// Index of the first remaining account in each instruction's metas (its named accounts,
/// `event_authority` and `program` included).
fn named_len(ix: &Instruction, remaining: usize) -> usize {
    ix.accounts.len() - remaining
}

fn variants(ix: &Instruction, remaining: usize) -> Vec<(String, Instruction)> {
    let start = named_len(ix, remaining);
    let mut out = Vec::new();
    for i in 0..remaining {
        let mut omit = ix.clone();
        omit.accounts.remove(start + i);
        out.push((format!("omit slot {i}"), omit));
        let mut swap = ix.clone();
        let m = &mut swap.accounts[start + i];
        *m = AccountMeta {
            pubkey: Pubkey::new_unique(),
            is_signer: false,
            is_writable: m.is_writable,
        };
        out.push((format!("replace slot {i}"), swap));
    }
    let mut extra = ix.clone();
    extra
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    out.push(("extra trailing account".to_string(), extra));
    if remaining > 1 {
        let mut order = ix.clone();
        order.accounts.swap(start, start + 1);
        out.push(("slots 0 and 1 swapped".to_string(), order));
    }
    out
}

fn assert_all_fail(
    env: &mut Env,
    ix: &Instruction,
    remaining: usize,
    signers: &[&solana_keypair::Keypair],
) {
    for (what, bad) in variants(ix, remaining) {
        let res = env.send(&[bad], signers);
        let code = custom_code(&res);
        assert!(
            matches!(
                code,
                Some(c) if c == self::code(E::BadRemainingAccounts)
                    || c == self::code(E::VenueAccountMismatch)
            ),
            "{what}: expected BadRemainingAccounts / VenueAccountMismatch, got {code:?}"
        );
    }
}

fn code(e: E) -> u32 {
    u32::from(e)
}

#[test]
fn open_close_and_crank_check_every_venue_slot() {
    let (mut env, mm) = setup();
    let b = env.borrower();
    let open = open_short_ix(
        &mm,
        &b.key(),
        0,
        Q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    assert_all_fail(&mut env, &open, 8, &[&b.kp]);
    let meta = ok(env.send(&[open], &[&b.kp]));
    assert!(meta.compute_units_consumed > 0);
    env.tick(&mm, 1);
    let close = close_short_ix(&mm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
    assert_all_fail(&mut env, &close, 8, &[&b.kp]);
    let crank = crank_price_ix(&mm);
    assert_all_fail(&mut env, &crank, 3, &[]);
    ok(env.send(&[close], &[&b.kp]));
    ok(env.send(&[crank], &[]));
}

/// The named accounts that feed guards are key-checked too: another market's pool vault, a
/// foreign oracle cache or latch, a different trader.
#[test]
fn named_accounts_are_key_checked() {
    let (mut env, mm) = setup();
    let b = env.borrower();
    let base = open_short_ix(
        &mm,
        &b.key(),
        0,
        Q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    // [borrower, config, venue_guard, oracle_cache, market, position, exposure, mint,
    //  pool_vault, market_vault, trader, token_program, system_program, event_authority, program]
    for slot in [1usize, 2, 3, 8, 9, 10] {
        let mut ix = base.clone();
        ix.accounts[slot].pubkey = Pubkey::new_unique();
        let res = env.send(&[ix], &[&b.kp]);
        assert!(res.is_err(), "slot {slot} replaced");
    }
    ok(env.send(&[base], &[&b.kp]));
}
