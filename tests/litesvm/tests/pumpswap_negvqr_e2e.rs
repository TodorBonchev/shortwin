//! Fork E2E on a PumpSwap pool with a **negative `virtual_quote_reserves`** (M6 row "negative VQR
//! from 2026-09-30"; ARCH §4.1 PumpSwap row; ECON §1.2, §6.2 #103; R-VEN Update 2026-09-30 H2, H3,
//! H10): the **synthetic** `pamm_mature_pool_negative_vqr` fixture — the e/acc pool of the dump
//! with VQR = −⌊vault/2⌋, so its effective quote reserve `vault + VQR` is half its WSOL vault (no
//! mainnet pool is negative yet; the deployed ELF prices it on the `i128` sum, H3) — listed with
//! `create_market(Auto)` and traded end to end by the release short.win and other users on the
//! dumped PumpSwap ELF, the invariant hooks after every instruction (`Env::send`).
//!
//! | Test | Path |
//! |---|---|
//! | [`lifecycle_on_a_negative_vqr_pool`] | listing (the pAMM init set) → two lenders → the tier from `R = min(vault, vault + VQR)` (the effective half, not the vault; the program's own `engine::tier::assign` on its host reader, and what the vault would have given) → opens A, B at the pool's sale quote on `(X, vault + VQR)` → other users sell 15 %: A closes in profit at the pool's buy quote → C opens → other users buy ×3: B and C liquidated on the pool, the keeper's view (`graduation_support::pool_status`) predicting the program's refusal every second, each exit leg by leg → D held to its deadline with hourly cranks and expired → E caught in a held ×10 gap: the liquidation returns `UseSolSettle`, the keeper's `sol_settle` arms and settles S1 at `V − K_s` → both lenders exit, their tokens (less E's) and SOL pro rata |
//! | [`every_quote_prices_on_the_effective_reserve`] | a sale and a buy of 0.1 %–3 % of the base on the synthetic pool and on its positive-VQR source cost / pay exactly `quote::sol_leg` on `vault + VQR` at the effective market cap's fee (60 bps creator vs 35), and the program's `B̂` / `S` in `Open` / `Close` events equal the ELF's charge |

#![allow(clippy::result_large_err)]

#[path = "graduation_support.rs"]
mod support;

use anchor_lang::prelude::AccountInfo;
use shortwin_litesvm_tests::pamm::PammCoin;
use shortwin_math::risk;
use support::*;

/// The program's own venue state of `pm` now (the host build of `venues::read_state_with` on the
/// ReadSet as the VM holds it, with the mint's live supply).
fn host_state(env: &Env, pm: &PammMarket) -> shortwin::venues::VenueState {
    let mut accs: Vec<(Pubkey, u64, Vec<u8>, Pubkey)> = pm
        .read_set(false)
        .iter()
        .map(|a| {
            let acc = env.svm.get_account(&a.pubkey).unwrap_or_default();
            (a.pubkey, acc.lamports, acc.data, acc.owner)
        })
        .collect();
    let infos: Vec<AccountInfo> = accs
        .iter_mut()
        .map(|(k, l, d, o)| AccountInfo::new(k, false, false, l, d, o, false))
        .collect();
    let m = env.market(&pm.market());
    let mint = env.svm.get_account(&pm.keys.mint).unwrap();
    let supply = shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &mint.data)
        .unwrap()
        .supply;
    shortwin::venues::read_state_with(&m, VenueKind::PumpSwap, &infos, env.now(), Some(supply))
}

/// The harness's view of `pm`'s pool now (quotes on `(X, vault + VQR)` at the live fee).
fn view(env: &Env, pm: &PammMarket) -> venue_view::VenueView {
    venue_view::view(&env.svm, &env.market(&pm.market())).expect("a view")
}

/// A crank on the pool that must succeed.
fn crank(env: &mut Env, pm: &PammMarket) {
    ok(env.send(&[crank_price_ix(pm)], &[]));
}

/// `n` seconds of one crank each.
fn crank_secs(env: &mut Env, pm: &PammMarket, n: u32) {
    for _ in 0..n {
        env.warp(1);
        crank(env, pm);
    }
}

/// An open on the pool that must succeed, at the pool's sale quote.
fn open(env: &mut Env, pm: &PammMarket, q: u64) -> Pos {
    let s_exp = view(env, pm).sell(q).expect("a sale quote");
    let mut b = env.borrower();
    let (res, nonce) = env.open(pm, &mut b, q);
    let open: Open = events::one(&ok(res));
    assert_eq!(open.s, s_exp, "S = the sale quote on (X, vault + VQR)");
    assert_eq!(open.venue, VenueKind::PumpSwap as u8);
    Pos { b, nonce, open }
}

/// The keeper's liquidation attempt this second (after a crank): the keeper's view says whether
/// the program must refuse; an executed liquidation is checked leg by leg. `Some` if it executed.
fn keeper_liquidates(
    env: &mut Env,
    pm: &PammMarket,
    keeper: &Keypair,
    p: &Pos,
) -> Option<Liquidate> {
    let st = pool_status(env, pm, &p.key(), p.nonce);
    let pos = env.position(pm, &p.key(), p.nonce);
    let vault0 = tokens(env, &pm.keys.pool_vault);
    let ix = liquidate_ix(pm, &keeper.pubkey(), &p.key(), p.nonce);
    let res = env.send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), ix], &[keeper]);
    if !st.cr_liq() {
        assert_err(&res, E::NotLiquidatable);
        return None;
    }
    if res.is_err() {
        let code = custom_code(&res);
        assert!(
            code == Some(u32::from(E::ExitBoundNotMet)) || code == Some(u32::from(E::UseSolSettle)),
            "refused {code:?} while CR-liquidatable"
        );
        return None;
    }
    let ev: Liquidate = events::one(&ok(res));
    let back = tokens(env, &pm.keys.pool_vault) - vault0;
    check_pool_liquidation(&ev, &pos, st.b_live.expect("a quote"), back);
    Some(ev)
}

/// The lifecycle on the negative-VQR pool (module docs).
#[test]
fn lifecycle_on_a_negative_vqr_pool() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    env.plant_negative_vqr();
    let st = p.state(&env.svm);
    let r = p.reserves(&env.svm).unwrap();
    assert!(st.virtual_quote_reserves < 0, "the synthetic pool");
    assert_eq!(r.effective_quote * 2, u128::from(r.real_quote));
    // q: 1 % of the base, a notional of at most 4 SOL (5 SOL until M6R-1: the pool's third-party
    // LP, λ 92.98 %, now bounds `L_slow` and `X₀` at its locked depth, and a 5-SOL open exceeds the
    // wallet cap on `0.93·X`).
    let q = u64::try_from(
        (u128::from(r.base) / 100)
            .min(u128::from(r.base) * u128::from(4 * SOL) / r.effective_quote),
    )
    .unwrap();

    // ---- listing, lenders ----
    ok(env.list_pamm(&p.mint, &p.base_program));
    let lend1 = 40 * q;
    let pm = env.pamm_market_listed(&p.mint, lend1, 60);
    let l2 = env.funded_keypair();
    let l2_acc = fork::fund_token(
        &mut env,
        &l2.pubkey(),
        &p.mint,
        TokenProgram::Token2022,
        lend1 / 4,
    );
    let l2_keys = ix::LenderKeys {
        owner: l2.pubkey(),
        mint: p.mint,
        token_program: p.base_program,
        owner_token_account: l2_acc,
    };
    ok(env.send(&[ix::deposit(&l2_keys, lend1 / 4, 0)], &[&l2]));
    let keeper = env.funded_keypair();

    // ---- the tier reads R = min(vault, vault + VQR): the effective half ----
    let config = env.config();
    let m = env.market(&pm.market());
    let hs = host_state(&env, &pm);
    assert!(hs.executable);
    assert_eq!(
        u128::from(hs.r),
        r.effective_quote,
        "R = min(vault, vault + VQR)"
    );
    let oracle = env.oracle_cache();
    let tier = shortwin::engine::tier::assign(&config, &m, &hs, &oracle, env.now());
    let mut on_vault = hs.clone();
    on_vault.r = r.real_quote;
    let tier_vault = shortwin::engine::tier::assign(&config, &m, &on_vault, &oracle, env.now());
    eprintln!(
        "negative VQR: R_now = {:.2} SOL (vault {:.2}); tier {tier:?} (on the vault alone: {tier_vault:?})",
        hs.r as f64 / 1e9,
        r.real_quote as f64 / 1e9
    );

    // ---- A and B open at the pool's sale quote ----
    let a = open(&mut env, &pm, q);
    let b = open(&mut env, &pm, q);
    assert_eq!(
        Some(a.open.tier),
        tier.map(|t| t as u8),
        "the tier the program assigned"
    );
    crank_secs(&mut env, &pm, 5);

    // ---- other users sell the price 15 % down over 3 s; A closes in profit ----
    let crowd = PammUser::new(&mut env, &pm.pool, 20_000 * SOL);
    let p0 = pool_spot(&env, &pm.pool);
    for k in 1..=3u64 {
        env.warp(1);
        let (x, y) = pool_xy(&env, &pm.pool);
        let target = p0 * u128::from(100 - 5 * k) / 100;
        let tx = target_base(x, y, target);
        ok(crowd.sell(&mut env, &pm.pool, u64::try_from(tx - x).unwrap(), 0));
        crank(&mut env, &pm);
    }
    crank_secs(&mut env, &pm, 3);
    let b_exp = view(&env, &pm).buy(q).expect("a quote");
    let close_a: Close = events::one(&ok(env.close(&pm, &a.b, a.nonce)));
    assert_eq!(
        close_a.buyback_cost, b_exp,
        "B = the pool's buy quote on (X, vault + VQR)"
    );
    assert!(
        close_a.buyback_cost < a.open.s,
        "a short gains when the price falls"
    );
    assert!(close_a.borrower_payout > a.open.m);

    // ---- C opens at the lower price; other users buy ×3; B and C are liquidated ----
    let c = open(&mut env, &pm, q);
    crank_secs(&mut env, &pm, 3);
    for _ in 0..5 {
        env.warp(1);
        // ×3^(1/5) per second.
        ok(crowd.push_price(&mut env, &pm.pool, 12_457, 10_000));
        crank(&mut env, &pm);
    }
    let mut open_ones = vec![b, c];
    let mut liquidated = Vec::new();
    for _ in 0..120 {
        if open_ones.is_empty() {
            break;
        }
        env.warp(1);
        crank(&mut env, &pm);
        let mut keep = Vec::new();
        for p in open_ones {
            match keeper_liquidates(&mut env, &pm, &keeper, &p) {
                Some(ev) => liquidated.push(ev),
                None => keep.push(p),
            }
        }
        open_ones = keep;
    }
    assert!(open_ones.is_empty(), "both liquidated within two minutes");

    // ---- D opens once the EMA caught up and is held to its deadline ----
    crank_secs(&mut env, &pm, 60);
    let d = open(&mut env, &pm, q);
    let deadline = env.position(&pm, &d.key(), d.nonce).deadline;
    while env.now() + 3_600 < deadline - 60 {
        env.warp(3_600);
        crank(&mut env, &pm);
    }
    let rest = deadline - 60 - env.now();
    env.warp(rest);
    while env.now() < deadline {
        crank_secs(&mut env, &pm, 1);
    }
    let st = pool_status(&env, &pm, &d.key(), d.nonce);
    assert!(st.expired && !st.cr_liq());
    let pos_d = env.position(&pm, &d.key(), d.nonce);
    let b_exec = view(&env, &pm).buy(q).expect("a quote");
    let ix = expire_ix(&pm, &keeper.pubkey(), &d.key(), d.nonce);
    let e: Expire = events::one(&ok(
        env.send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), ix], &[&keeper])
    ));
    assert_eq!(e.buyback_cost, b_exec, "B = the pool's buy quote");
    assert_eq!(e.rent, st.r_due);
    let base = e.b_live_risk.min(e.b_ref);
    let left = pos_d.v - e.buyback_cost - e.rent;
    let fee = ((u128::from(base) * u128::from(env.config().params.expiry_fee_bps)).div_ceil(10_000)
        as u64)
        .min(left);
    assert_eq!(e.expiry_fee, fee);
    let k_e = risk::k_e(e.b_ref, &pos_d.expiry_keeper_terms()).unwrap();
    assert_eq!(e.keeper_reward, k_e.min(left - fee));
    assert_eq!(e.borrower_payout, left - fee - e.keeper_reward);

    // ---- E: a held ×10 gap; UseSolSettle, then S1 at V − K_s ----
    crank_secs(&mut env, &pm, 30);
    let epos = open(&mut env, &pm, q);
    crank_secs(&mut env, &pm, 10);
    env.warp(1);
    ok(crowd.push_price(&mut env, &pm.pool, 10, 1));
    let mut s1 = None;
    for _ in 0..300 {
        env.warp(1);
        crank(&mut env, &pm);
        let st = pool_status(&env, &pm, &epos.key(), epos.nonce);
        let ix = liquidate_ix(&pm, &keeper.pubkey(), &epos.key(), epos.nonce);
        let res = env.send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), ix], &[&keeper]);
        if !st.cr_liq() {
            assert_err(&res, E::NotLiquidatable);
            continue;
        }
        assert!(!st.buyback_allowed, "V cannot buy Q back after a ×10 gap");
        assert_err(&res, E::UseSolSettle);
        let ix = sol_settle_ix(&env, &pm, &keeper.pubkey(), &epos.key(), epos.nonce);
        let meta = ok(env.send(&[ix], &[&keeper]));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            s1 = Some(ev);
            break;
        }
    }
    let s1 = s1.expect("S1 within 5 minutes");
    let pos_e = epos.open;
    assert_eq!(s1.condition, 1, "S1");
    assert_eq!(s1.lender_amount, pos_e.v - s1.k_s, "lenders V − K_s");
    assert_eq!(s1.borrower_amount, 0);
    let m = env.market(&pm.market());
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0));

    // ---- both lenders exit: tokens (less E's) and SOL pro rata ----
    let params = env.config().params;
    env.warp(i64::from(params.lock_grace_s).max(i64::from(params.lump_vest_s)) + 86_400);
    crank(&mut env, &pm);
    let market_key = pm.market();
    let l1 = pm.lender.insecure_clone();
    let (t1, sol1) = withdraw_all(&mut env, &market_key, &pm.lender_keys(), &l1);
    let (t2, sol2) = withdraw_all(&mut env, &market_key, &l2_keys, &l2);
    let deposited = lend1 + lend1 / 4;
    assert!(
        (t1 + t2).abs_diff(deposited - q) <= 2,
        "every token back but E's (settled in SOL): {t1} + {t2}"
    );
    let ratio = sol1 as f64 / sol2 as f64;
    assert!((ratio - 4.0).abs() < 0.01, "pro rata: {ratio}");
    let income: u64 =
        liquidated.iter().map(|l| l.penalty_lender).sum::<u64>() + e.expiry_fee + s1.lender_amount;
    assert!(
        sol1 + sol2 > income - 1_000,
        "rent, penalties, the fee and E's S1 reach the lenders"
    );
    eprintln!(
        "negative VQR lifecycle: {} liquidations, an expiry, S1 {:.4} SOL; lenders {t1} + {t2} tokens, {:.4} + {:.4} SOL",
        liquidated.len(),
        s1.lender_amount as f64 / 1e9,
        sol1 as f64 / 1e9,
        sol2 as f64 / 1e9
    );
}

/// Every quote prices on the effective reserve (module docs).
#[test]
fn every_quote_prices_on_the_effective_reserve() {
    for negative in [false, true] {
        let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
        if negative {
            env.plant_negative_vqr();
        }
        let r = p.reserves(&env.svm).unwrap();
        let q = u64::try_from(u128::from(r.base) / 2_000).unwrap();
        ok(env.list_pamm(&p.mint, &p.base_program));
        let pm = env.pamm_market_listed(&p.mint, 100 * q, 60);
        let v = view(&env, &pm);
        let fees = match v.pricing {
            Some(Pricing::PumpSwap(f)) => f,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            fees.creator,
            if negative { 60 } else { 35 },
            "the effective market cap's tier"
        );
        let mut pos = Vec::new();
        for k in [1u64, 2, 3, 4] {
            let p0 = open(&mut env, &pm, q * k);
            pos.push(p0);
            crank_secs(&mut env, &pm, 2);
        }
        for p0 in pos {
            env.warp(1);
            crank(&mut env, &pm);
            let (x, y) = pool_xy(&env, &pm.pool);
            let want = Pricing::PumpSwap(fees)
                .buy(p0.open.q, x, y)
                .expect("sol_leg")
                .cost;
            let close: Close = events::one(&ok(env.close(&pm, &p0.b, p0.nonce)));
            assert_eq!(close.buyback_cost, want, "B = sol_leg on (X, vault + VQR)");
        }
    }
}
