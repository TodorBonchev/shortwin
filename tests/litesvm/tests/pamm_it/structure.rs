//! I14 on PumpSwap (ARCH §3.0 A6, §3.12 "PumpSwap account sets", §8.3; C0-2, C0-14; M10 G-A1a):
//! every account of the PumpSwap ReadSet (P0–P6) and TradeExtras (T0–T18) is present and
//! key-checked in `open_short`, `close_short`, `liquidate`, `expire` (ReadSet ++ TradeExtras) and
//! `crank_price`, `sol_settle` (ReadSet only) — each slot omitted, replaced by a foreign key, a
//! trailing extra, and pairs of slots swapped (P0↔P1, P1↔P2, P5↔P6, T7↔T8, T10↔T11, T14↔T18, the
//! protocol and buyback recipients T9↔T16 when the live lists keep them apart) make the
//! instruction fail with `BadRemainingAccounts` / `VenueAccountMismatch`, never "no route" or a
//! silent default, and leave the position as it was; then the honest instruction is accepted
//! (the control: the same instruction with the market's own accounts lands, or — `liquidate` on a
//! healthy position — passes the account checks and is refused by its body, `NotLiquidatable`).
//! Run on an SPL-Token pool (`legacy261`) and on a cashback coin (`cashback`: T18 is the
//! mandatory cashback account there).
//!
//! Modelled on `ray_it/structure.rs::variants`. Every instruction goes through `Env::send`: the
//! invariant hooks run after each one.

use shortwin::venues::pump_amm::{
    P_BASE_VAULT, P_BOOST_VAULT_ATA, P_LP_MINT, P_POOL, P_QUOTE_VAULT, READ_LEN,
    T_BUYBACK_RECIPIENT, T_CASHBACK_ATA, T_COIN_CREATOR_VAULT_ATA, T_PROTOCOL_FEE_RECIPIENT,
    T_PROTOCOL_FEE_RECIPIENT_ATA, T_TRADER_BASE_ATA, T_TRADER_WSOL, T_UVA, TRADE_LEN,
};

use crate::common::*;

/// The malformed copies of `ix` whose last `remaining` metas are the venue's accounts (ReadSet,
/// then TradeExtras when `remaining > READ_LEN`): each slot omitted, each replaced by a foreign
/// key (writability kept), a trailing extra, and the `swaps` (slot indices into the remaining
/// accounts) exchanged.
fn variants(
    ix: &Instruction,
    remaining: usize,
    swaps: &[(usize, usize)],
) -> Vec<(String, Instruction)> {
    let start = ix.accounts.len() - remaining;
    let name = |i: usize| {
        if i < READ_LEN {
            format!("P{i}")
        } else {
            format!("T{}", i - READ_LEN)
        }
    };
    let mut out = Vec::new();
    for i in 0..remaining {
        let mut omit = ix.clone();
        omit.accounts.remove(start + i);
        out.push((format!("omit {}", name(i)), omit));
        let mut foreign = ix.clone();
        let m = &mut foreign.accounts[start + i];
        *m = AccountMeta {
            pubkey: Pubkey::new_unique(),
            is_signer: false,
            is_writable: m.is_writable,
        };
        out.push((format!("foreign key at {}", name(i)), foreign));
    }
    let mut extra = ix.clone();
    extra
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    out.push(("a trailing account".to_string(), extra));
    for &(a, b) in swaps
        .iter()
        .filter(|(a, b)| *a < remaining && *b < remaining)
    {
        let mut order = ix.clone();
        order.accounts.swap(start + a, start + b);
        out.push((format!("{} and {} swapped", name(a), name(b)), order));
    }
    out
}

/// Every variant of `ix` fails with `BadRemainingAccounts` / `VenueAccountMismatch` (sent after
/// a 400 k CU limit, so no variant fails for want of compute) and leaves `watch` (a position)
/// byte for byte as it was.
fn assert_all_fail(
    env: &mut Env,
    ix: &Instruction,
    remaining: usize,
    swaps: &[(usize, usize)],
    signers: &[&solana_keypair::Keypair],
    watch: &Pubkey,
    label: &str,
) -> usize {
    let mut n = 0;
    for (what, bad) in variants(ix, remaining, swaps) {
        let before = env.svm.get_account(watch).map(|a| a.data);
        let res = env.send(&[cu_limit_ix(KEEPER_EXIT_CU), bad], signers);
        let got = custom_code(&res);
        assert!(
            matches!(
                got,
                Some(c) if c == code(E::BadRemainingAccounts) || c == code(E::VenueAccountMismatch)
            ),
            "{label}: {what}: expected BadRemainingAccounts / VenueAccountMismatch, got {got:?}"
        );
        assert_eq!(
            env.svm.get_account(watch).map(|a| a.data),
            before,
            "{label}: {what}: the position is unchanged"
        );
        n += 1;
    }
    n
}

/// The swaps tried (indices into `ReadSet ++ TradeExtras`): the pool and its vaults, the boost
/// ATA and the LP mint, the trader's base ATA and `trader_wsol`, a recipient ATA and the
/// creator-vault ATA, the UVA and its cashback ATA; the two recipients when the live
/// `GlobalConfig` lists do not share the chosen keys (a swap of two keys each list holds would be
/// a legitimate choice).
fn swaps(env: &Env, pm: &PammMarket) -> Vec<(usize, usize)> {
    let t = |s: usize| READ_LEN + s;
    let mut v = vec![
        (P_POOL, P_BASE_VAULT),
        (P_BASE_VAULT, P_QUOTE_VAULT),
        (P_BOOST_VAULT_ATA, P_LP_MINT),
        (t(T_TRADER_BASE_ATA), t(T_TRADER_WSOL)),
        (t(T_PROTOCOL_FEE_RECIPIENT_ATA), t(T_COIN_CREATOR_VAULT_ATA)),
        (t(T_UVA), t(T_CASHBACK_ATA)),
    ];
    let protocol: Vec<Pubkey> = (0..8)
        .map(|i| pamm::protocol_fee_recipient(&env.svm, i))
        .collect();
    let buyback: Vec<Pubkey> = (0..8)
        .map(|i| pamm::buyback_fee_recipient(&env.svm, i))
        .collect();
    if !buyback.contains(&pm.protocol_recipient) && !protocol.contains(&pm.buyback_recipient) {
        v.push((t(T_PROTOCOL_FEE_RECIPIENT), t(T_BUYBACK_RECIPIENT)));
    }
    v
}

/// Every slot of the six instructions on `coin`'s market (module docs).
fn every_slot_on(coin: PammCoin) {
    let (mut env, pm, q) = setup(coin);
    let all = READ_LEN + TRADE_LEN;
    assert_eq!(pm.read_set(false).len(), READ_LEN);
    assert_eq!(pm.trade_extras().len(), TRADE_LEN);
    let sw = swaps(&env, &pm);
    let label = format!("{coin:?}");
    let mut tried = 0;

    // open_short: two positions (one closed by its borrower, one exited by a keeper below).
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
    let pos0 = pda::position(&pm.market(), &b.key(), 0).0;
    tried += assert_all_fail(
        &mut env,
        &open,
        all,
        &sw,
        &[&b.kp],
        &pos0,
        &format!("{label} open"),
    );
    let (_, n0, _) = open_ok(&mut env, &pm, &mut b, q);
    let mut b2 = env.borrower();
    let (_, n1, _) = open_ok(&mut env, &pm, &mut b2, q);
    let pos1 = pda::position(&pm.market(), &b2.key(), n1).0;
    env.warp(1);

    // crank_price and sol_settle: the ReadSet only.
    let crank_ix = crank_price_ix(&pm);
    tried += assert_all_fail(
        &mut env,
        &crank_ix,
        READ_LEN,
        &sw,
        &[],
        &pm.market(),
        &format!("{label} crank"),
    );
    crank(&mut env, &pm);
    let kp = keeper(&mut env);
    env.warp(1);
    let settle_ix = sol_settle_ix(&env, &pm, &kp.pubkey(), &b2.key(), n1);
    tried += assert_all_fail(
        &mut env,
        &settle_ix,
        READ_LEN,
        &sw,
        &[&kp],
        &pos1,
        &format!("{label} sol_settle"),
    );
    let meta = ok(settle(&mut env, &pm, &kp, &b2.key(), n1));
    assert!(
        events::of::<shortwin::events::SolSettle>(&meta).is_empty(),
        "healthy: a no-op"
    );

    // close_short of the first position.
    env.warp(1);
    let close = close_short_ix(&pm, &b.key(), n0, u64::MAX, ExtrasSel::NONE, &[]);
    let pos0 = pda::position(&pm.market(), &b.key(), n0).0;
    tried += assert_all_fail(
        &mut env,
        &close,
        all,
        &sw,
        &[&b.kp],
        &pos0,
        &format!("{label} close"),
    );
    close_ok(&mut env, &pm, &b, n0);

    // liquidate of the second, healthy: every variant fails on the accounts; the honest one gets
    // past them and is refused by the body.
    let liq = liquidate_ix(&pm, &kp.pubkey(), &b2.key(), n1);
    tried += assert_all_fail(
        &mut env,
        &liq,
        all,
        &sw,
        &[&kp],
        &pos1,
        &format!("{label} liquidate"),
    );
    assert_err(
        &liquidate(&mut env, &pm, &kp, &b2.key(), n1),
        E::NotLiquidatable,
    );

    // expire of the second after its deadline: every variant fails, the honest one lands.
    warp_to_deadline(&mut env, &pm, &b2.key(), n1, 1);
    crank(&mut env, &pm);
    env.warp(1);
    crank(&mut env, &pm);
    let exp = expire_ix(&pm, &kp.pubkey(), &b2.key(), n1);
    tried += assert_all_fail(
        &mut env,
        &exp,
        all,
        &sw,
        &[&kp],
        &pos1,
        &format!("{label} expire"),
    );
    ok(expire(&mut env, &pm, &kp, &b2.key(), n1));
    assert_trader_clean(&env, &pm);
    assert_eq!(market(&env, &pm).open_positions, 0);
    eprintln!("{label}: {tried} malformed account lists refused");
}

/// Every slot on the SPL-Token 261-byte pool.
#[test]
fn every_pumpswap_slot_is_mandatory_and_key_checked_on_legacy261() {
    every_slot_on(PammCoin::Legacy261);
}

/// Every slot on a cashback coin (Token-2022), where T18 is the mandatory cashback account.
#[test]
fn every_pumpswap_slot_is_mandatory_and_key_checked_on_a_cashback_coin() {
    every_slot_on(PammCoin::Cashback);
}
