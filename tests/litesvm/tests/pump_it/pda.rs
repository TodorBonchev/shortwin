//! T-PDA-USER, curve part (ARCH §8.2, §12 U1; R-PUMP §6.1, U6, U9; C1-7; pump-final §6.1.3):
//! pump's `buy_v3` with the **trader PDA** as `user`, signed by short.win's `invoke_signed`, on the
//! real pump ELF — read off the transaction's own inner instructions, with the sell leg for
//! comparison:
//!
//! - the buy (close): short.win (height 1) funds the trader with direct lamport moves (no inner
//!   instruction), syncs the balances with a 0-lamport System transfer, and invokes pump at height
//!   2 with the IDL's 17 accounts — `user` = the trader (#9, the only signer) and
//!   `associated_base_user` = `trader_base_ata` (#10) — and 24 data bytes (no `partial_fill`);
//!   pump's own System transfer debits the **trader** at height 3; pump's token transfer credits
//!   `trader_base_ata` at height 3; short.win then moves exactly `q` from `trader_base_ata` to the
//!   lending vault (`TransferChecked`, authority the trader) at height 2 and refunds the trader's
//!   lamports to the position;
//! - the sell (open): the lending vault pays `q` into `trader_base_ata` (`TransferChecked`,
//!   authority the market PDA) and the borrower funds `ert` (System, height 2) before pump's
//!   `sell_v3` at height 2 with the trader as `user`;
//! - nothing runs deeper than height 3 (two spare levels), and the trader PDA is the only
//!   account short.win signs for in pump's instruction.

use shortwin::constants::{PUMP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
use solana_message::Message;

use crate::common::*;

#[derive(Clone, Debug)]
struct Inner {
    program: Pubkey,
    accounts: Vec<Pubkey>,
    data: Vec<u8>,
    height: u8,
}

/// The transaction a client sends for `ix`: ARCH §6.1's CU limit, then `ix` (instruction 1).
fn tx(ix: &Instruction) -> Vec<Instruction> {
    vec![pump::cu_limit_ix(pump::CURVE_TRADE_CU), ix.clone()]
}

/// The inner instructions of `ix`, instruction 1 of [`tx`], sent by the harness payer.
fn inner(env: &Env, ix: &Instruction, meta: &TransactionMetadata) -> Vec<Inner> {
    let msg = Message::new(&tx(ix), Some(&env.payer.pubkey()));
    let keys = msg.account_keys;
    meta.inner_instructions
        .get(1)
        .map(|list| {
            list.iter()
                .map(|i| Inner {
                    program: keys[usize::from(i.instruction.program_id_index)],
                    accounts: i
                        .instruction
                        .accounts
                        .iter()
                        .map(|a| keys[usize::from(*a)])
                        .collect(),
                    data: i.instruction.data.clone(),
                    height: i.stack_height,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// pump's v3 trade accounts for `pm`'s trader (the harness's own list, `PumpCurve::v3_accounts`,
/// with the TradeExtras' buyback recipient): what short.win's CPI must name, in order.
pub fn v3_accounts(pm: &PumpMarket) -> Vec<Pubkey> {
    pm.curve
        .v3_accounts(&pm.trader(), &pm.trader_base_ata, &pm.buyback_recipient)
        .iter()
        .map(|a| a.pubkey)
        .collect()
}

fn is_token(p: &Pubkey) -> bool {
    *p == TOKEN_PROGRAM_ID || *p == TOKEN_2022_PROGRAM_ID
}

fn system() -> Pubkey {
    anchor_lang::system_program::ID
}

/// A System `Transfer` `(from, to, lamports)`.
fn sys_transfer(i: &Inner) -> Option<(Pubkey, Pubkey, u64)> {
    (i.program == system() && i.data.len() == 12 && i.data[..4] == [2, 0, 0, 0]).then(|| {
        (
            i.accounts[0],
            i.accounts[1],
            u64::from_le_bytes(i.data[4..12].try_into().unwrap()),
        )
    })
}

/// A token `TransferChecked` `(source, destination, authority, amount)` or `Transfer`.
fn token_transfer(i: &Inner) -> Option<(Pubkey, Pubkey, Pubkey, u64)> {
    if !is_token(&i.program) {
        return None;
    }
    let amount = |o: usize| u64::from_le_bytes(i.data[o..o + 8].try_into().unwrap());
    match i.data.first()? {
        3 => Some((i.accounts[0], i.accounts[1], i.accounts[2], amount(1))),
        12 => Some((i.accounts[0], i.accounts[2], i.accounts[3], amount(1))),
        _ => None,
    }
}

fn check(coin: PumpCoin) {
    let (mut env, pm, q) = setup(coin);
    let trader = pm.trader();
    let market = pm.market();

    // ---- the sell leg (open) ----
    let mut b = env.borrower();
    let open = open_short_ix(
        &pm,
        &b.key(),
        0,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(&tx(&open), &[&kp]));
    eprintln!("{coin:?} open: {} CU", meta.compute_units_consumed);
    b.next_nonce = 1;
    let ins = inner(&env, &open, &meta);
    let pump_ix = ins
        .iter()
        .find(|i| i.program == PUMP_PROGRAM_ID)
        .expect("sell_v3");
    assert_eq!(pump_ix.height, 2);
    assert_eq!(pump_ix.data[..8], fork::PUMP_SELL_V3);
    assert_eq!(pump_ix.data.len(), 24);
    assert_eq!(pump_ix.accounts, v3_accounts(&pm), "the IDL's 17 accounts");
    assert_eq!(pump_ix.accounts[8], trader, "user = the trader PDA");
    assert_eq!(
        pump_ix.accounts[9], pm.trader_base_ata,
        "associated_base_user"
    );
    let pump_at = ins
        .iter()
        .position(|i| i.program == PUMP_PROGRAM_ID)
        .unwrap();
    let before = &ins[..pump_at];
    assert!(
        before.iter().any(|i| i.height == 2
            && token_transfer(i) == Some((pm.keys.pool_vault, pm.trader_base_ata, market, q))),
        "q out of the lending vault into trader_base_ata, the market PDA signing"
    );
    let ert = env.config().params.exec_rent_tol;
    assert!(
        before
            .iter()
            .any(|i| i.height == 2 && sys_transfer(i) == Some((b.key(), trader, ert))),
        "the borrower funds ert before the sale"
    );
    assert!(
        ins.iter().all(|i| i.height <= 3),
        "the sell peaks at height 3"
    );

    // ---- the buy leg (close): T-PDA-USER ----
    env.warp(1);
    let close = close_short_ix(&pm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
    let meta = ok(env.send(&tx(&close), &[&kp]));
    eprintln!("{coin:?} close: {} CU", meta.compute_units_consumed);
    let ins = inner(&env, &close, &meta);
    let pump_at = ins
        .iter()
        .position(|i| i.program == PUMP_PROGRAM_ID)
        .expect("buy_v3");
    let pump_ix = &ins[pump_at];
    assert_eq!(pump_ix.height, 2, "pump under short.win's CPI");
    assert_eq!(pump_ix.data[..8], fork::PUMP_BUY_V3);
    assert_eq!(pump_ix.data.len(), 24, "no partial_fill byte");
    assert_eq!(
        u64::from_le_bytes(pump_ix.data[8..16].try_into().unwrap()),
        q,
        "exactly q"
    );
    assert_eq!(pump_ix.accounts, v3_accounts(&pm), "the IDL's 17 accounts");
    assert_eq!(pump_ix.accounts[8], trader, "user = the trader PDA");
    assert_eq!(
        pump_ix.accounts[9], pm.trader_base_ata,
        "associated_base_user"
    );
    let msg = Message::new(&tx(&close), Some(&env.payer.pubkey()));
    let signer_in_tx = |k: &Pubkey| {
        msg.account_keys
            .iter()
            .position(|x| x == k)
            .is_some_and(|i| msg.is_signer(i))
    };
    assert!(
        !signer_in_tx(&trader),
        "the trader signs only by invoke_signed"
    );
    // Before pump: the 0-lamport sync transfer between the position and the trader.
    let pos_key = pda::position(&market, &b.key(), 0).0;
    assert!(
        ins[..pump_at]
            .iter()
            .any(|i| i.height == 2 && sys_transfer(i) == Some((trader, pos_key, 0))),
        "the balance sync before the CPI"
    );
    // Inside pump: System debits of the trader at height 3.
    let inside: Vec<&Inner> = ins[pump_at + 1..]
        .iter()
        .take_while(|i| i.height > 2)
        .collect();
    let debits: Vec<(Pubkey, Pubkey, u64)> = inside
        .iter()
        .filter(|i| i.height == 3)
        .filter_map(|i| sys_transfer(i))
        .filter(|(from, _, _)| *from == trader)
        .collect();
    assert!(
        debits.iter().any(|(_, to, _)| *to == pm.curve.curve),
        "pump debits the trader PDA for the curve's net: {debits:?}"
    );
    assert!(
        inside.iter().any(|i| i.height == 3
            && token_transfer(i).is_some_and(|(src, dst, auth, amt)| src
                == pm.curve.curve_base_ata
                && dst == pm.trader_base_ata
                && auth == pm.curve.curve
                && amt == q)),
        "pump delivers q into trader_base_ata"
    );
    // After pump: q to the lending vault, the trader signing as the ATA's owner.
    assert!(
        ins[pump_at + 1..].iter().any(|i| i.height == 2
            && token_transfer(i) == Some((pm.trader_base_ata, pm.keys.pool_vault, trader, q))),
        "trader_base_ata → pool_vault, exactly q"
    );
    assert!(
        ins.iter().all(|i| i.height <= 3),
        "the buy peaks at height 3"
    );
    assert_trader_clean(&env, &pm);
}

/// T-PDA-USER on a Token-2022 curve.
#[test]
fn the_trader_pda_buys_on_a_token_2022_curve() {
    check(PumpCoin::T22_115);
}

/// T-PDA-USER on a legacy SPL-Token curve.
#[test]
fn the_trader_pda_buys_on_a_legacy_spl_curve() {
    check(PumpCoin::Legacy115);
}
