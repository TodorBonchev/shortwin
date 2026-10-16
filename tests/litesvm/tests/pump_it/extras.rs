//! T-V3-ADAPTER (pump-final §6.1.1–§6.1.3, §14): the curve's 10 TradeExtras for pump's v3 trades
//! and the CPI built from them, on the real pump ELF.
//!
//! - every slot T0–T9 replaced by a foreign key is `VenueAccountMismatch` on an open and on a close,
//!   with nothing written (`check_trade`: the fixed and cached keys, T0 executable, T7 in the live
//!   `Global`'s buyback list);
//! - T7 takes any buyback recipient of the live list and refuses a normal fee recipient;
//! - a pump UVA that is missing or accumulates for another user is `VenueAccountMismatch` before
//!   the CPI;
//! - the v2-era list of 20 (shortwin 1.0.0's) fails cleanly (`VenueAccountMismatch` at its T2),
//!   nothing written; a short list is `BadRemainingAccounts`;
//! - T-PDA-USER on a synthetic buy (`t_pda_user_v3`): the close of `q > rT` CPIs `buy_v3(q,
//!   max_in)` — 24 bytes, the IDL's 17 accounts, the trader PDA as `user` and the only signer
//!   (by `invoke_signed`).
//!
//! Every instruction goes through `Env::send`: the invariant hooks run after each one.

use shortwin::constants::PUMP_PROGRAM_ID;
use shortwin_litesvm_tests::engine::with_cu_limit;
use shortwin_venue_state::pump as vpump;
use solana_message::Message;

use crate::common::*;

/// The open of `q` by `b` (nonce `nonce`) with the TradeExtras `extras` in place of the market's.
fn open_with(
    pm: &PumpMarket,
    b: &Borrower,
    nonce: u64,
    q: u64,
    extras: &[AccountMeta],
) -> Instruction {
    with_trade_extras(
        pm,
        open_short_ix(
            pm,
            &b.key(),
            nonce,
            q,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        ),
        extras,
    )
}

/// The close of `b`'s position `nonce` with the TradeExtras `extras` in place of the market's.
fn close_with(pm: &PumpMarket, b: &Borrower, nonce: u64, extras: &[AccountMeta]) -> Instruction {
    with_trade_extras(
        pm,
        close_short_ix(pm, &b.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]),
        extras,
    )
}

/// `ix` (a trading instruction of `pm`, extras `none`) with its TradeExtras replaced by `extras`
/// (any length).
fn with_trade_extras(pm: &PumpMarket, mut ix: Instruction, extras: &[AccountMeta]) -> Instruction {
    let own = pm.trade_extras().len();
    let start = ix.accounts.len() - own;
    ix.accounts.truncate(start);
    ix.accounts.extend_from_slice(extras);
    ix
}

/// Sends `ix` behind the curve's CU limit, signed by `b`.
fn send(env: &mut Env, pm: &PumpMarket, b: &Borrower, ix: Instruction) -> TxResult {
    let kp = b.kp.insecure_clone();
    env.send(&with_cu_limit(pm, ix), &[&kp])
}

/// T0–T9 one by one replaced by a foreign key (writability kept): the open and the close refuse
/// `VenueAccountMismatch` and write nothing; the market's own list then lands both.
#[test]
fn v3_slot_each_wrong_key_is_venue_account_mismatch() {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    let own = pm.trade_extras();
    assert_eq!(own.len(), shortwin::venues::pump_curve::TRADE_LEN);
    assert_eq!(own.len(), 10);
    let position = pda::position(&pm.market(), &b.key(), nonce).0;
    for slot in 0..own.len() {
        let mut bad = own.clone();
        bad[slot].pubkey = Pubkey::new_unique();
        let before = env.svm.get_account(&position);
        let curve_before = env.svm.get_account(&pm.curve.curve);
        assert_err(
            &send(&mut env, &pm, &b, close_with(&pm, &b, nonce, &bad)),
            E::VenueAccountMismatch,
        );
        assert_err(
            &send(&mut env, &pm, &b, open_with(&pm, &b, nonce + 1, q, &bad)),
            E::VenueAccountMismatch,
        );
        assert_eq!(
            env.svm.get_account(&position),
            before,
            "T{slot}: nothing written"
        );
        assert_eq!(env.svm.get_account(&pm.curve.curve), curve_before);
    }
    ok(send(&mut env, &pm, &b, close_with(&pm, &b, nonce, &own)));
    assert_trader_clean(&env, &pm);
}

/// T7 (`buyback_fee_recipient`) is checked against the live `Global`'s buyback list (C0-14): a
/// normal fee recipient there is `VenueAccountMismatch`; every listed buyback recipient lands
/// (opens and closes alternating through the list).
#[test]
fn v3_slot_a_buyback_recipient_outside_the_live_list_is_refused() {
    let (mut env, mut pm, q) = setup(PumpCoin::Near166);
    let t7 = shortwin::venues::pump_curve::T_BUYBACK_RECIPIENT;
    let mut b = env.borrower();
    let mut bad = pm.trade_extras();
    bad[t7].pubkey = fork::pump_fee_recipient(&env.svm, 0);
    assert_err(
        &send(&mut env, &pm, &b, open_with(&pm, &b, 0, q, &bad)),
        E::VenueAccountMismatch,
    );
    for j in 0..8 {
        pm.buyback_recipient = fork::pump_buyback_fee_recipient(&env.svm, j);
        if pm.buyback_recipient == Pubkey::default() {
            continue;
        }
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        env.warp(1);
        pm.buyback_recipient = fork::pump_buyback_fee_recipient(&env.svm, 7 - j);
        if pm.buyback_recipient == Pubkey::default() {
            pm.buyback_recipient = fork::pump_buyback_fee_recipient(&env.svm, 0);
        }
        close_ok(&mut env, &pm, &b, nonce);
        env.warp(1);
    }
    assert_trader_clean(&env, &pm);
}

/// The trader's pump UVA (T6) must exist and accumulate for the trader (`check_uva`, before the
/// CPI): one whose `user` field names someone else, and none at all, are `VenueAccountMismatch`
/// on an open and on a close; the UVA restored, both land.
#[test]
fn v3_slot_a_foreign_or_missing_uva_is_refused() {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    let uva = fork::pump_user_volume_accumulator(&pm.trader());
    let saved = env.svm.get_account(&uva).unwrap();
    let foreign = Pubkey::new_unique();
    fork::patch(&mut env.svm, &uva, |d| {
        d[vpump::OFF_UVA_USER..vpump::OFF_UVA_USER + 32].copy_from_slice(foreign.as_ref());
    });
    assert_err(&env.close(&pm, &b, nonce), E::VenueAccountMismatch);
    assert_err(&env.open(&pm, &mut b, q).0, E::VenueAccountMismatch);
    env.svm
        .set_account(uva, solana_account::Account::default())
        .unwrap();
    assert_err(&env.close(&pm, &b, nonce), E::VenueAccountMismatch);
    assert_err(&env.open(&pm, &mut b, q).0, E::VenueAccountMismatch);
    env.svm.set_account(uva, saved).unwrap();
    close_ok(&mut env, &pm, &b, nonce);
    open_ok(&mut env, &pm, &mut b, q);
}

/// The v2-era TradeExtras (20 keys: the fee recipient, the creator vault, pump-fees, … —
/// `pump::v2_trade_extras`) on the v3 adapter fail cleanly, nothing written (pump-final §15): the
/// parser takes the first 10 as the TradeExtras and refuses their T2 (pump-fees where WSOL
/// belongs) with `VenueAccountMismatch` before it reaches the 10 left over, which would be
/// `BadRemainingAccounts` (extras `none` with trailing accounts). The list cut to its first 10
/// keys is refused the same way, and 9 keys are `BadRemainingAccounts`.
#[test]
fn v3_slot_a_v2_list_of_20_is_refused() {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    let v2 = pump::v2_trade_extras(&env, &pm);
    assert_eq!(v2.len(), 20);
    let position = pda::position(&pm.market(), &b.key(), nonce).0;
    let before = env.svm.get_account(&position);
    for list in [&v2[..], &v2[..10]] {
        assert_err(
            &send(&mut env, &pm, &b, close_with(&pm, &b, nonce, list)),
            E::VenueAccountMismatch,
        );
        assert_err(
            &send(&mut env, &pm, &b, open_with(&pm, &b, nonce + 1, q, list)),
            E::VenueAccountMismatch,
        );
    }
    let own = pm.trade_extras();
    assert_err(
        &send(&mut env, &pm, &b, close_with(&pm, &b, nonce, &own[..9])),
        E::BadRemainingAccounts,
    );
    assert_eq!(env.svm.get_account(&position), before);
    close_ok(&mut env, &pm, &b, nonce);
}

/// T-PDA-USER on a synthetic buy (pump-final §6.1.3): the close of `q = rT + q/2` invokes pump once
/// at height 2 with `buy_v3(q, max_in)` — 24 bytes, `amount = q` past `rT` — on the IDL's 17
/// accounts (the market's TradeExtras and ReadSet in pump's order), `user` = the trader PDA, which
/// the transaction never signs (short.win's `invoke_signed`); pump completes the curve inside it.
#[test]
fn t_pda_user_v3() {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(2);
    buy_down_to(&mut env, &pm, q / 2);
    env.warp(1);
    let close = close_short_ix(&pm, &b.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
    let ixs = with_cu_limit(&pm, close);
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(&ixs, &[&kp]));
    let msg = Message::new(&ixs, Some(&env.payer.pubkey()));
    let keys = &msg.account_keys;
    let calls: Vec<_> = meta.inner_instructions[ixs.len() - 1]
        .iter()
        .filter(|i| keys[usize::from(i.instruction.program_id_index)] == PUMP_PROGRAM_ID)
        .filter(|i| i.stack_height == 2)
        .collect();
    assert_eq!(calls.len(), 1, "one pump CPI");
    let call = &calls[0].instruction;
    assert_eq!(call.data.len(), 24, "no partial_fill byte");
    assert_eq!(call.data[..8], fork::PUMP_BUY_V3);
    assert_eq!(u64::from_le_bytes(call.data[8..16].try_into().unwrap()), q);
    let accounts: Vec<Pubkey> = call
        .accounts
        .iter()
        .map(|a| keys[usize::from(*a)])
        .collect();
    let want: Vec<Pubkey> = pm
        .curve
        .v3_accounts(&pm.trader(), &pm.trader_base_ata, &pm.buyback_recipient)
        .iter()
        .map(|a| a.pubkey)
        .collect();
    assert_eq!(accounts, want, "the IDL's 17 accounts");
    assert_eq!(accounts[8], pm.trader(), "user = the trader PDA");
    let at = keys.iter().position(|k| *k == pm.trader()).unwrap();
    assert!(!msg.is_signer(at), "the trader signs only by invoke_signed");
    let close: Close = events::one(&meta);
    assert_eq!(close.q, q);
    assert!(
        pm.curve.state(&env.svm).complete,
        "pump completed the curve"
    );
    assert_trader_clean(&env, &pm);
}
