//! Every `open_short` refusal particular to PumpSwap (ARCH §3.4 steps 3–4, §3.12; A13, A24, C0-10,
//! C0-14; M6): the post-change warm-up (`EmaWarmingUp`), `disable_flags` (`VenueFlagged` on any
//! non-zero value, `NoRoute` under the T-DISABLE buy bit), the boost gate (`BoostInProgress`), the
//! latch on pAMM and pump-fees (`VenueUpgraded`, exits unaffected), the existence requirement and
//! account substitution in the TradeExtras (`VenueAccountMismatch`); and (M10 G-A2) a third party's
//! real non-canonical pool of the mint (`create_pool(index = 1)` on the ELF) in the ReadSet of
//! every instruction and at listing. The venue-generic refusals are `engine_it`'s and `pump_it`'s.

use shortwin::constants::{GUARD_PUMP_AMM, GUARD_PUMP_FEES};
use shortwin_litesvm_tests::ix;

use crate::common::*;

/// An open that must fail with `e`.
fn refused(env: &mut Env, pm: &PammMarket, q: u64, e: E) {
    let mut b = env.borrower();
    let (res, _) = env.open(pm, &mut b, q);
    assert_err(&res, e);
}

/// A listed market whose price has just started (one PumpSwap crank): the warm-up refuses opens
/// (after the static guards, before the listing's own `market_warmup_s`), and so it does after
/// every integration until the eighth; an open's own reverted roll integrates nothing, so the
/// warm-up counts cranked seconds, not attempts.
#[test]
fn an_open_waits_for_the_post_change_warm_up() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Legacy261);
    let q = q_for(&env, &p);
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q * 40, 0);
    env.warp(60);
    crank(&mut env, &pm);
    assert_eq!(market(&env, &pm).ema_warm, 8);
    // Refused opens alone: the counter does not move.
    for _ in 0..3 {
        env.warp(1);
        refused(&mut env, &pm, q, E::EmaWarmingUp);
    }
    assert_eq!(market(&env, &pm).ema_warm, 8);
    for warm in (1..8).rev() {
        env.warp(1);
        crank(&mut env, &pm);
        assert_eq!(market(&env, &pm).ema_warm, warm);
        refused(&mut env, &pm, q, E::EmaWarmingUp);
    }
    // The open's own roll integrates the eighth bucket: confirmed, and it opens.
    env.warp(1);
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
}

/// `disable_flags` (T-DISABLE, A24): any non-zero value refuses opens with `VenueFlagged` — the
/// sell bit, the halts of `create_pool` / `deposit` / `withdraw`, and bits 5–7, which pAMM ignores —
/// except the buy bit, which makes the venue not executable (`NoRoute`).
#[test]
fn disable_flags_refuse_opens() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    for bit in 0..8u8 {
        pamm::set_disable_flags(&mut env.svm, 1 << bit);
        env.warp(1);
        let want = if bit == 3 {
            E::NoRoute
        } else {
            E::VenueFlagged
        };
        refused(&mut env, &pm, q, want);
    }
    pamm::set_disable_flags(&mut env.svm, 0);
    env.warp(1);
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
}

/// The boost gate (A13, C1-2): on the fresh pool (its boost vault still holding 13.98 SOL) opens
/// are refused `BoostInProgress` inside `boost_window_s` of `graduated_at` (the listing); a vault
/// at exactly `boost_dust_lamports` no longer blocks, one lamport more does; after the window
/// the gate is off whatever the vault holds.
#[test]
fn the_boost_gate() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Fresh);
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q * 40);
    let dust = env.config().params.boost_dust_lamports;
    assert!(
        pm.pool.boost_amount(&env.svm) > dust,
        "the fixture's boost is running"
    );
    refused(&mut env, &pm, q, E::BoostInProgress);
    fork::set_token_amount(&mut env.svm, &pm.pool.boost_vault_ata, dust + 1);
    env.warp(1);
    refused(&mut env, &pm, q, E::BoostInProgress);
    fork::set_token_amount(&mut env.svm, &pm.pool.boost_vault_ata, dust);
    env.warp(1);
    let mut b = env.borrower();
    let (res, n) = env.open(&pm, &mut b, q);
    ok(res);
    close_ok(&mut env, &pm, &b, n);
    // The window: with the boost vault full again, only its end lifts the gate.
    fork::set_token_amount(&mut env.svm, &pm.pool.boost_vault_ata, 13 * SOL);
    env.warp(1);
    refused(&mut env, &pm, q, E::BoostInProgress);
    wait_out_boost(&mut env, &pm);
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
}

/// The latch (A7; masks v0.4.1): an upgrade of pAMM or pump-fees refuses PumpSwap opens
/// (`VenueUpgraded`) until the Emergency key re-validates it; a keeper crank, `sol_settle` and
/// the borrower's close keep working (I10).
#[test]
fn the_latch_blocks_pumpswap_opens_only() {
    for (program, guard) in [
        (fork::PumpProgram::Amm, GUARD_PUMP_AMM),
        (fork::PumpProgram::Fees, GUARD_PUMP_FEES),
    ] {
        let (mut env, pm, q) = setup(PammCoin::Legacy261);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        let slot = fork::FIXTURE_GUARD_SLOTS[usize::from(guard)];
        fork::set_pump_deploy_slot(&mut env.svm, program, slot + 7);
        ok(env.send(&[ix::flag_venue_upgraded(guard)], &[]));
        env.warp(1);
        let mut b2 = env.borrower();
        let (res, _) = env.open(&pm, &mut b2, q);
        assert_err(&res, E::VenueUpgraded);
        crank(&mut env, &pm);
        let kp = keeper(&mut env);
        ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        env.warp(1);
        close_ok(&mut env, &pm, &b, nonce);
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::revalidate_venue(&ea.pubkey(), guard, slot + 7)],
            &[&ea],
        ));
        env.warp(1);
        open_ok(&mut env, &pm, &mut b2, q);
    }
}

/// The existence requirement (C0-14, C1-8, C1-9): a trade whose trader pAMM UVA, coin-creator
/// vault ATA or recipient ATAs do not exist is refused before PumpSwap runs
/// (`VenueAccountMismatch`), and executes once anyone created them.
#[test]
fn the_existence_requirement() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Legacy261);
    let q = q_for(&env, &p);
    ok(env.list_pamm(&p.mint, &p.base_program));
    let mut pm = env.pamm_market_listed(&p.mint, q * 40, 60);
    // The trader's pAMM UVA created at listing; a protocol recipient whose ATA does not exist.
    let fresh = anchor_lang::prelude::Pubkey::new_unique();
    let mut data = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_GLOBAL_CONFIG)
        .unwrap();
    data.data[57..89].copy_from_slice(fresh.as_ref());
    env.svm
        .set_account(shortwin::constants::PUMP_AMM_GLOBAL_CONFIG, data)
        .unwrap();
    pm.protocol_recipient = fresh;
    env.warp(1);
    refused(&mut env, &pm, q, E::VenueAccountMismatch);
    let payer = env.payer.pubkey();
    ok(env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &fresh,
            &shortwin::constants::WSOL_MINT,
            &shortwin::constants::TOKEN_PROGRAM_ID,
        )],
        &[],
    ));
    env.warp(1);
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
}

/// Account substitution (I14's PumpSwap part): every TradeExtras slot is key-checked — a
/// recipient not in the live lists, another WSOL account at a recipient's ATA slot, another
/// coin-creator vault, another UVA, another `trader_wsol` — `VenueAccountMismatch`; the ReadSet
/// likewise (another pool's vault).
#[test]
fn every_slot_is_key_checked() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let other = Pubkey::new_unique();
    for slot in [
        shortwin::venues::pump_amm::T_PROGRAM,
        shortwin::venues::pump_amm::T_TRADER_WSOL,
        shortwin::venues::pump_amm::T_PROTOCOL_FEE_RECIPIENT,
        shortwin::venues::pump_amm::T_PROTOCOL_FEE_RECIPIENT_ATA,
        shortwin::venues::pump_amm::T_COIN_CREATOR_VAULT_ATA,
        shortwin::venues::pump_amm::T_COIN_CREATOR_VAULT_AUTHORITY,
        shortwin::venues::pump_amm::T_UVA,
        shortwin::venues::pump_amm::T_POOL_V2,
        shortwin::venues::pump_amm::T_BUYBACK_RECIPIENT,
        shortwin::venues::pump_amm::T_CASHBACK_ATA,
    ] {
        let b = env.borrower();
        let mut ix = open_short_ix(
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
        let n = ix.accounts.len();
        let rem_start =
            n - shortwin::venues::pump_amm::READ_LEN - shortwin::venues::pump_amm::TRADE_LEN;
        ix.accounts[rem_start + shortwin::venues::pump_amm::READ_LEN + slot].pubkey = other;
        let kp = b.kp.insecure_clone();
        let res = env.send(&[cu_limit_ix(pamm::PAMM_TRADE_CU), ix], &[&kp]);
        assert_err(&res, E::VenueAccountMismatch);
    }
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
}

// ---- a real non-canonical pool (M10 G-A2) ---------------------------------------------------------

/// `ix` (whose last `remaining` metas are the venue's) with `np`'s pool, vaults, boost ATA and LP
/// mint in place of the market's ReadSet slots P0–P2, P5, P6.
fn on_pool(ix: &Instruction, remaining: usize, np: &PammPool) -> Instruction {
    use shortwin::venues::pump_amm::{
        P_BASE_VAULT, P_BOOST_VAULT_ATA, P_LP_MINT, P_POOL, P_QUOTE_VAULT,
    };
    let mut ix = ix.clone();
    let start = ix.accounts.len() - remaining;
    for (slot, key) in [
        (P_POOL, np.pool),
        (P_BASE_VAULT, np.base_vault),
        (P_QUOTE_VAULT, np.quote_vault),
        (P_BOOST_VAULT_ATA, np.boost_vault_ata),
        (P_LP_MINT, np.lp_mint),
    ] {
        ix.accounts[start + slot].pubkey = key;
    }
    ix
}

/// A third party's own, valid PumpSwap pool of the market's mint (ARCH §2.5 `canonical_pool_valid`;
/// `venues::detect::canonical_pool`; M10 G-A2): a real `create_pool(index = 1)` on the dumped pAMM
/// ELF with a WSOL quote, no mayhem, real liquidity (a trader buys on it), only its address,
/// creator and index differ from the canonical pool's. Its keys in the ReadSet (P0–P2, the boost
/// ATA, the LP mint) are refused (`VenueAccountMismatch`) on `open_short`, `crank_price`,
/// `sol_settle`, `close_short` and `liquidate`, the position unchanged; the honest instructions
/// (controls) land.
#[test]
fn a_real_non_canonical_pool_is_never_the_market_pool() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let r = pm.pool.reserves(&env.svm).unwrap();
    let (np, creator) =
        pamm::create_third_party_pool(&mut env, &pm.keys.mint, 1, r.base / 4, 20 * SOL);
    let st = np.state(&env.svm);
    assert_eq!(st.index, 1);
    assert_eq!(st.creator, creator.pubkey().to_bytes());
    assert_ne!(
        st.creator,
        pda::pump_pool_authority(&pm.keys.mint).to_bytes()
    );
    assert_eq!(
        (st.base_mint, st.quote_mint),
        (
            pm.keys.mint.to_bytes(),
            shortwin::constants::WSOL_MINT.to_bytes()
        )
    );
    assert!(!st.is_mayhem_mode);
    assert_ne!(np.pool, pm.pool.pool);
    let u = PammUser::new(&mut env, &np, 5 * SOL);
    ok(u.buy(&mut env, &np, q / 10, u64::MAX));
    assert!(np.reserves(&env.svm).unwrap().base > 0, "real liquidity");

    let all = shortwin::venues::pump_amm::READ_LEN + shortwin::venues::pump_amm::TRADE_LEN;
    let read = shortwin::venues::pump_amm::READ_LEN;
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
    let cu = cu_limit_ix(KEEPER_EXIT_CU);
    assert_err(
        &env.send(&[cu.clone(), on_pool(&open, all, &np)], &[&b.kp]),
        E::VenueAccountMismatch,
    );
    let (_, n, _) = open_ok(&mut env, &pm, &mut b, q);
    let position = pda::position(&pm.market(), &b.key(), n).0;
    let before = env.svm.get_account(&position).map(|a| a.data);
    env.warp(1);
    let crank_ix = crank_price_ix(&pm);
    assert_err(
        &env.send(&[on_pool(&crank_ix, read, &np)], &[]),
        E::VenueAccountMismatch,
    );
    crank(&mut env, &pm);
    let kp = keeper(&mut env);
    let settle_ix = sol_settle_ix(&env, &pm, &kp.pubkey(), &b.key(), n);
    assert_err(
        &env.send(&[on_pool(&settle_ix, read, &np)], &[&kp]),
        E::VenueAccountMismatch,
    );
    let liq = liquidate_ix(&pm, &kp.pubkey(), &b.key(), n);
    assert_err(
        &env.send(&[cu.clone(), on_pool(&liq, all, &np)], &[&kp]),
        E::VenueAccountMismatch,
    );
    let close = close_short_ix(&pm, &b.key(), n, u64::MAX, ExtrasSel::NONE, &[]);
    assert_err(
        &env.send(&[cu, on_pool(&close, all, &np)], &[&b.kp]),
        E::VenueAccountMismatch,
    );
    assert_eq!(env.svm.get_account(&position).map(|a| a.data), before);
    ok(settle(&mut env, &pm, &kp, &b.key(), n));
    assert_err(
        &liquidate(&mut env, &pm, &kp, &b.key(), n),
        E::NotLiquidatable,
    );
    env.warp(1);
    close_ok(&mut env, &pm, &b, n);
}

/// The same pool at listing (ARCH §2.5, §3.2; `pump_amm.rs` canonical-pool rule; M10 G-A2), on a
/// fresh fork of the `mature` coin: given as the detection set's canonical-pool account it is
/// refused (`VenueAccountMismatch`: the key is derived on chain). Planted at the canonical address
/// (its bytes; mainnet cannot get it there, the address is pAMM's PDA of index 0 by pump's pool
/// authority) it is **not** valid — with its own index 1 and creator, with the index alone set to 0,
/// with the creator alone set to pump's pool authority — so the PumpSwap listing is refused
/// (`BadRemainingAccounts`) and the list-only listing lands as `None` without
/// `graduated_elsewhere` (D1). The control: index 0 **and** pump's authority make the same bytes a
/// valid canonical pool, and the PumpSwap listing lands on it.
#[test]
fn a_real_non_canonical_pool_is_never_listed() {
    use shortwin_venue_state::pump_amm::{OFF_CREATOR, OFF_INDEX};
    let (mut env0, p0) = Env::fork_pamm_coin(PammCoin::Mature);
    let r = p0.reserves(&env0.svm).unwrap();
    let (np, _) = pamm::create_third_party_pool(&mut env0, &p0.mint, 1, r.base / 4, 20 * SOL);
    let authority = pda::pump_pool_authority(&p0.mint);
    let creator = env0.funded_keypair();
    let market = pda::market(&p0.mint).0;
    let mut by_key = pamm::create_pamm_market_ix(&creator.pubkey(), &p0.mint, &p0.base_program);
    let det = by_key.accounts.len() - 4;
    by_key.accounts[det].pubkey = np.pool;
    assert_err(&env0.send(&[by_key], &[&creator]), E::VenueAccountMismatch);

    type Patch<'a> = (&'a str, &'a dyn Fn(&mut Vec<u8>), bool);
    let patches: [Patch; 4] = [
        ("its own index 1 and creator", &|_| {}, false),
        (
            "index 0, its own creator",
            &|d| d[OFF_INDEX..OFF_INDEX + 2].fill(0),
            false,
        ),
        (
            "index 1, pump's authority",
            &|d| d[OFF_CREATOR..OFF_CREATOR + 32].copy_from_slice(authority.as_ref()),
            false,
        ),
        (
            "index 0 and pump's authority (the control)",
            &|d| {
                d[OFF_INDEX..OFF_INDEX + 2].fill(0);
                d[OFF_CREATOR..OFF_CREATOR + 32].copy_from_slice(authority.as_ref());
            },
            true,
        ),
    ];
    for (what, patch, valid) in patches {
        let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
        let (np2, _) = pamm::create_third_party_pool(&mut env, &p.mint, 1, r.base / 4, 20 * SOL);
        let mut acc = env.svm.get_account(&np2.pool).unwrap();
        assert_eq!(acc.data[OFF_INDEX..OFF_INDEX + 2], 1u16.to_le_bytes());
        patch(&mut acc.data);
        env.svm.set_account(p.pool, acc).unwrap();
        let creator = env.funded_keypair();
        let full = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
        if valid {
            ok(env.send(&[full], &[&creator]));
            assert_eq!(
                env.market(&market).venue_kind(),
                Some(VenueKind::PumpSwap),
                "{what}"
            );
            continue;
        }
        assert_err(
            &env.send(std::slice::from_ref(&full), &[&creator]),
            E::BadRemainingAccounts,
        );
        let mut plain = full;
        plain.accounts.truncate(plain.accounts.len() - 3);
        ok(env.send(&[plain], &[&creator]));
        let m = env.market(&market);
        assert_eq!(m.venue_kind(), Some(VenueKind::None), "{what}");
        assert!(
            !m.has_flag(shortwin::state::MARKET_FLAG_GRADUATED_ELSEWHERE),
            "{what}: D1 holds"
        );
    }
}
