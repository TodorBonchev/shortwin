//! T-RAY-PDA (ARCH §2.4, §2.4.1, §8.2; U10; R-RAY §4.1–§4.2): on both programs and on a
//! Token-2022 base, read off the transaction's own inner instructions —
//!
//! - the sell's source is the **PDA-owned lending vault**: the market PDA approves the trader
//!   PDA for exactly `Q` (`ApproveChecked`), and the venue's token transfer out of `pool_vault`
//!   is signed by the trader as its **delegate**; afterwards the allowance is 0 and the delegate
//!   gone (auto-cleared, [V12]), the vault's authority still the market;
//! - the buy delivers **straight into the lending vault**: the venue's transfer out of its own
//!   vault names `pool_vault` as destination (the venues do not check its owner), signed by the
//!   venue's authority, and the trader pays from `trader_wsol` as its owner;
//! - the trader PDA is the venue's only signer (`owner` / `payer` via `invoke_signed`), and every
//!   leg peaks at stack height 3 (short.win 1 → venue 2 → token 3): two spare levels.

use shortwin::constants::{
    RAYDIUM_CPMM_AUTHORITY, RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_PROGRAM_ID,
    TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use solana_message::Message;

use crate::common::*;

/// One inner instruction, resolved to keys.
#[derive(Clone, Debug)]
struct Inner {
    program: Pubkey,
    accounts: Vec<Pubkey>,
    data: Vec<u8>,
    height: u8,
}

/// The inner instructions of instruction 0 of a transaction `ix` sent by the harness payer.
fn inner(env: &Env, ix: &Instruction, meta: &TransactionMetadata) -> Vec<Inner> {
    let msg = Message::new(std::slice::from_ref(ix), Some(&env.payer.pubkey()));
    let keys = msg.account_keys;
    meta.inner_instructions
        .first()
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

fn is_token(p: &Pubkey) -> bool {
    *p == TOKEN_PROGRAM_ID || *p == TOKEN_2022_PROGRAM_ID
}

fn amount_at(data: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
}

/// A token transfer `(source, destination, authority, amount)` of an inner instruction:
/// `Transfer` (3; AMM v4) or `TransferChecked` (12; CPMM).
fn transfer(i: &Inner) -> Option<(Pubkey, Pubkey, Pubkey, u64)> {
    if !is_token(&i.program) {
        return None;
    }
    match i.data.first()? {
        3 => Some((
            i.accounts[0],
            i.accounts[1],
            i.accounts[2],
            amount_at(&i.data, 1),
        )),
        12 => Some((
            i.accounts[0],
            i.accounts[2],
            i.accounts[3],
            amount_at(&i.data, 1),
        )),
        _ => None,
    }
}

fn check(fixture: RayFixture) {
    let (mut env, rm, q) = setup(fixture);
    let (venue, authority) = match rm.kind() {
        VenueKind::RaydiumV4 => (RAYDIUM_V4_PROGRAM_ID, RAYDIUM_V4_AUTHORITY),
        _ => (RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_CPMM_AUTHORITY),
    };
    let (tok_vault, sol_vault) = rm.vaults();
    let pool_vault = rm.keys.pool_vault;
    let (market, trader) = (rm.market(), rm.keys.trader);

    // ---- the sell: a delegate spend out of the PDA-owned lending vault ----
    let b = env.borrower();
    let ix = open_short_ix(
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
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(std::slice::from_ref(&ix), &[&kp]));
    let ins = inner(&env, &ix, &meta);
    let approve = ins
        .iter()
        .find(|i| is_token(&i.program) && i.data.first() == Some(&13))
        .expect("ApproveChecked");
    assert_eq!(approve.height, 2);
    assert_eq!(
        (
            approve.accounts[0],
            approve.accounts[1],
            approve.accounts[2],
            approve.accounts[3]
        ),
        (pool_vault, rm.mint(), trader, market),
        "the market PDA approves the trader PDA on the lending vault"
    );
    assert_eq!(amount_at(&approve.data, 1), q, "for exactly Q");
    let swap = ins
        .iter()
        .find(|i| i.program == venue)
        .expect("the venue CPI");
    assert_eq!(swap.height, 2);
    let spend = ins
        .iter()
        .filter_map(|i| transfer(i).map(|t| (t, i.height)))
        .find(|((src, _, _, _), _)| *src == pool_vault)
        .expect("the venue's transfer out of the lending vault");
    assert_eq!(spend.1, 3, "short.win 1 → venue 2 → token 3");
    assert_eq!(
        spend.0,
        (pool_vault, tok_vault, trader, q),
        "the trader spends Q of the lending vault as its delegate"
    );
    let paid = ins
        .iter()
        .filter_map(transfer)
        .find(|(src, _, _, _)| *src == sol_vault)
        .expect("the venue pays out of its WSOL vault");
    assert_eq!((paid.1, paid.2), (rm.trader_wsol, authority));
    assert!(ins.iter().all(|i| i.height <= 3), "two spare stack levels");
    let pv = read::account(&env.svm, &pool_vault).unwrap();
    let t = shortwin_venue_state::token_account::read(&pv.owner.to_bytes(), &pv.data).unwrap();
    assert_eq!(t.delegate, None, "the delegate cleared at a zero allowance");
    assert_eq!(t.delegated_amount, 0);
    assert_eq!(
        t.owner,
        market.to_bytes(),
        "the vault's authority is still the market"
    );

    // ---- the buy: delivered straight into the lending vault ----
    env.warp(1);
    let ix = close_short_ix(&rm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
    let meta = ok(env.send(std::slice::from_ref(&ix), &[&kp]));
    let ins = inner(&env, &ix, &meta);
    let delivery = ins
        .iter()
        .filter_map(|i| transfer(i).map(|t| (t, i.height)))
        .find(|((_, dst, _, _), _)| *dst == pool_vault)
        .expect("the venue's transfer into the lending vault");
    assert_eq!(delivery.1, 3);
    assert_eq!(
        delivery.0,
        (tok_vault, pool_vault, authority, q),
        "exactly Q, from the venue's vault, signed by the venue's authority"
    );
    let pay = ins
        .iter()
        .filter_map(transfer)
        .find(|(src, _, _, _)| *src == rm.trader_wsol)
        .expect("the trader pays from trader_wsol");
    assert_eq!((pay.1, pay.2), (sol_vault, trader));
    assert!(ins.iter().all(|i| i.height <= 3));
}

#[test]
fn amm_v4_spl() {
    check(FART);
}

#[test]
fn cpmm_spl() {
    check(USELESS);
}

/// U10: a Token-2022 lending vault as the delegated source and as the buy's destination.
#[test]
fn cpmm_token_2022() {
    check(T22);
}
