//! A stranger's change of a pool's coin creator (M6-SEC-4; ARCH §5.4 case 4, §3.12 PumpSwap T11 /
//! T12): pAMM's `set_coin_creator` takes no signer — a canonical pool without a coin creator takes
//! its Metaplex metadata creator (or its curve's creator) — and neither does
//! `migrate_pool_coin_creator` (to the mint's sharing config; IDL). On the dumped ELF, with the
//! `legacy211` mint's mainnet metadata planted (`pamm::LEGACY211_METADATA_B64`), the fee payer alone
//! moves the pool's coin creator from none to `4E9s4kuE…`; repeating it changes nothing.
//!
//! What it does to short.win: an exit built for the old creator fails `VenueAccountMismatch` (T11 /
//! T12 follow the live pool), and so does one built for the new creator until the new creator
//! vault's WSOL ATA exists (the existence requirement) — a one-time delay, since anyone can create
//! that ATA (the keeper puts `create_idempotent(T11)` in its exit's own transaction whenever the
//! live creator is not the cached one, M6-SEC-4; its `prepare_pamm_trader` creates it too). Before
//! the fix the market's creator cache stayed on the old creator for good, so every later trade
//! re-derived T12 / T11 (two `find_program_address`, M5-SEC-6's pattern); `crank_price` now
//! re-caches it.

use shortwin::constants::{TOKEN_PROGRAM_ID, WSOL_MINT};
use shortwin::state::pump_swap_cache as pc;

use crate::common::*;

/// The market's PumpSwap cache entry `i`.
fn cached(env: &Env, pm: &PammMarket, i: usize) -> Pubkey {
    market(env, pm).pool_cache[i]
}

/// `close_short` of `b`'s position `nonce`, preceded by `pre`, with the trade CU limit.
fn close_with(
    env: &mut Env,
    pm: &PammMarket,
    b: &Borrower,
    nonce: u64,
    pre: &[Instruction],
) -> TxResult {
    let mut ixs = vec![cu_limit_ix(pamm::PAMM_TRADE_CU)];
    ixs.extend_from_slice(pre);
    ixs.push(close_short_ix(
        pm,
        &b.key(),
        nonce,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    ));
    let kp = b.kp.insecure_clone();
    env.send(&ixs, &[&kp])
}

/// ★ M6-SEC-4 on the dumped pAMM ELF (module docs): a stranger's `set_coin_creator` on the
/// `legacy211` pool (grown by the position's open, to 287 bytes since the 2026-10-02 redeploy, 271
/// before; on 211 bytes pAMM cannot write the field, `BorshIoError`); the close built before it and the close built after it both fail
/// `VenueAccountMismatch` while the new creator's vault ATA is missing; the next crank re-caches the
/// creator (pre-fix it stayed on the old one: every trade re-derived T12 / T11); the close with
/// `create_idempotent(T11)` in its own transaction lands.
#[test]
fn m6_sec_4_a_strangers_coin_creator_change_costs_one_retry_and_the_crank_recaches_it() {
    let (mut env, mut pm, q) = setup(PammCoin::Legacy211);
    pamm::plant_legacy211_metadata(&mut env);
    // On the 211-byte pool pAMM cannot write the coin creator.
    let res = env.send(&[pamm::set_coin_creator_ix(&pm.pool)], &[]);
    assert!(res.is_err(), "the 211-byte pool has no coin-creator field");
    let mut b = env.borrower();
    let (_, n, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(
        env.svm.get_account(&pm.pool.pool).unwrap().data.len(),
        pamm::POOL_LEN_2026_10
    );
    assert_eq!(pm.pool.coin_creator(&env.svm), Pubkey::default());
    assert_eq!(
        cached(&env, &pm, pc::CACHED_COIN_CREATOR),
        Pubkey::default()
    );

    // A stranger's `set_coin_creator`: no signature but the fee payer's.
    let ix = pamm::set_coin_creator_ix(&pm.pool);
    assert!(ix.accounts.iter().all(|a| !a.is_signer));
    ok(env.send(&[ix], &[]));
    let creator = pamm::LEGACY211_METADATA_CREATOR;
    assert_eq!(pm.pool.coin_creator(&env.svm), creator);
    ok(env.send(&[pamm::set_coin_creator_ix(&pm.pool)], &[]));
    assert_eq!(
        pm.pool.coin_creator(&env.svm),
        creator,
        "a repeat changes nothing"
    );

    // The close built for the old creator (in flight across the change): refused.
    env.warp(1);
    assert_err(
        &close_with(&mut env, &pm, &b, n, &[]),
        E::VenueAccountMismatch,
    );
    // Built for the live creator: its vault's WSOL ATA does not exist (the existence requirement).
    pm.refresh_creator(&env.svm);
    let (auth, ata) = pm.creator_vault();
    assert!(env.svm.get_account(&ata).is_none_or(|a| a.data.is_empty()));
    assert_err(
        &close_with(&mut env, &pm, &b, n, &[]),
        E::VenueAccountMismatch,
    );

    // The next crank re-caches the creator (M6-SEC-4; pre-fix the cache kept the old one).
    let meta = crank(&mut env, &pm);
    let payer = env.payer.pubkey();
    let create = fork::create_ata_idempotent_ix(&payer, &auth, &WSOL_MINT, &TOKEN_PROGRAM_ID);
    let sim = {
        let mut ixs = vec![cu_limit_ix(pamm::PAMM_TRADE_CU), create.clone()];
        ixs.push(close_short_ix(
            &pm,
            &b.key(),
            n,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        ));
        let tx = solana_transaction::Transaction::new_signed_with_payer(
            &ixs,
            Some(&payer),
            &[&env.payer, &b.kp],
            env.svm.latest_blockhash(),
        );
        env.svm
            .simulate_transaction(tx)
            .expect("the close simulates")
    };
    let (auth_bump, ata_bump) = (
        Pubkey::find_program_address(
            &[b"creator_vault", creator.as_ref()],
            &shortwin::constants::PUMP_AMM_PROGRAM_ID,
        )
        .1,
        Pubkey::find_program_address(
            &[auth.as_ref(), TOKEN_PROGRAM_ID.as_ref(), WSOL_MINT.as_ref()],
            &shortwin::constants::ASSOCIATED_TOKEN_PROGRAM_ID,
        )
        .1,
    );
    eprintln!(
        "M6-SEC-4: crank after the change {} CU; [create_idempotent(T11), close] {} CU; the new \
         creator's vault authority bump {auth_bump}, its ATA bump {ata_bump}",
        meta.compute_units_consumed, sim.meta.compute_units_consumed
    );
    assert_eq!(cached(&env, &pm, pc::CACHED_COIN_CREATOR), creator);
    assert_eq!(cached(&env, &pm, pc::CACHED_COIN_CREATOR_VAULT_AUTH), auth);
    assert_eq!(cached(&env, &pm, pc::CACHED_COIN_CREATOR_VAULT_ATA), ata);

    // The keeper's fix: `create_idempotent(T11)` in the exit's own transaction — it lands.
    let (close, _) = {
        let meta = ok(close_with(
            &mut env,
            &pm,
            &b,
            n,
            std::slice::from_ref(&create),
        ));
        (events::one::<Close>(&meta), meta)
    };
    assert_eq!(close.q, q);
    assert!(
        env.svm
            .get_account(&ata)
            .is_some_and(|a| !a.data.is_empty())
    );
    assert_trader_clean(&env, &pm);
    // From now on the market trades as before: an open and a close with the cached creator.
    env.warp(1);
    crank(&mut env, &pm);
    let (_, n2, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    crank(&mut env, &pm);
    close_ok(&mut env, &pm, &b, n2);
}
