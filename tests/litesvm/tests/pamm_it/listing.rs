//! Listing a PumpSwap-class coin (ARCH §2.5 `classify_pump`, §3.2; M6): `create_market(Auto)` on
//! a coin whose curve migrated to its canonical pool lists it on that pool (`PumpSwap`), creates
//! the trader's base ATA and pAMM UVA (the pAMM init set), writes the PumpSwap cache from the pool
//! and starts no price: the listing carries no vault (ARCH §3.2's init set), so the first
//! observation with the PumpSwap ReadSet starts the price state, marked for the post-change
//! warm-up. A list-only market whose canonical pool turns readable is re-detected by
//! `crank_price` (`None → PumpSwap`).

use shortwin::state::pump_swap_cache as pc;

use crate::common::*;

fn pamm_uva(user: &Pubkey) -> Pubkey {
    pamm::pamm_uva(user)
}

/// Lists `coin`'s mint as a new creator and checks the market as ARCH §3.2 / §2.5 leave it.
fn list_and_check(coin: PammCoin) -> (Env, PammPool) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let creator = env.funded_keypair();
        let ix = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
        assert_err(&env.send(&[ix], &[&creator]), E::CurveNeedsExtend);
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    let meta = ok(env.list_pamm(&p.mint, &p.base_program));
    let created: shortwin::events::MarketCreated = events::one(&meta);
    assert_eq!(created.venue, VenueKind::PumpSwap as u8, "{coin:?}");
    assert_eq!(created.canonical_pool, p.pool);
    let market = pda::market(&p.mint).0;
    let m = env.market(&market);
    assert_eq!(m.venue_kind(), Some(VenueKind::PumpSwap));
    assert_eq!(m.graduated_at, env.now(), "age from listing (O13)");
    assert_eq!(m.tier, shortwin_math::tier::Tier::C as u8);
    assert_eq!(
        (m.ring_count, m.ema_price.get()),
        (0, 0),
        "no price at listing"
    );
    assert_eq!((m.ema_warm, m.ema_guard), (0, 0));
    let trader = pda::trader(&market).0;
    let bv = pamm::boost_vault(&p.pool);
    let uva = pamm_uva(&trader);
    let creator = p.coin_creator(&env.svm);
    let auth = pamm::coin_creator_vault_authority(&creator);
    let want = [
        (pc::POOL_BASE_VAULT, p.base_vault),
        (pc::POOL_QUOTE_VAULT, p.quote_vault),
        (pc::POOL_V2_MARKER, pamm::pool_v2(&p.mint)),
        (pc::BOOST_VAULT_ATA, wsol_ata(&bv)),
        (pc::PAMM_UVA, uva),
        (pc::PAMM_UVA_QUOTE_ATA, wsol_ata(&uva)),
        (pc::CACHED_COIN_CREATOR, creator),
        (pc::CACHED_COIN_CREATOR_VAULT_AUTH, auth),
        (pc::CACHED_COIN_CREATOR_VAULT_ATA, wsol_ata(&auth)),
        (pc::BOOST_VAULT, bv),
    ];
    for (i, k) in want {
        assert_eq!(m.pool_cache[i], k, "{coin:?}: pool cache slot {i}");
    }
    // The trader's base ATA and pAMM UVA exist (the creator paid).
    let ata = pda::ata(&trader, &p.mint, &p.base_program);
    let t = pamm::token_state(&env.svm, &ata).expect("trader_base_ata");
    assert_eq!((t.owner, t.amount), (trader.to_bytes(), 0));
    let u = env.svm.get_account(&uva).expect("pAMM UVA");
    let u = vamm::read_user_volume_accumulator(&u.owner.to_bytes(), &u.data).expect("UVA");
    assert_eq!(u.user, trader.to_bytes());
    (env, p)
}

/// Every PumpSwap pool role lists on its canonical pool (Token-2022 and SPL Token; 301-, 261-
/// and — after pump's `extend_account` of its 49-byte curve — 211-byte pools).
#[test]
fn a_pumpswap_class_coin_lists_on_its_canonical_pool() {
    for coin in PammCoin::ALL {
        list_and_check(coin);
    }
}

/// The price starts at the first PumpSwap observation, marked (`ema_warm = 8`, `ema_guard = 1`),
/// at the pool's spot and depth; eight integrations of the pool's own buckets confirm it. Since
/// M6R-1 (owner decision of 2026-09-30) as a Raydium start: `L_ema = min(√(X·(Y + VQR)),
/// ⌊D·10⁴/λ_min⌋)` and — the warm-up's depth cap in the same prologue — `L_slow = min(L_ema, D)`,
/// `D` the burned LP's locked depth, written into `pool_d`; PumpSwap's `λ_min` is 100 %
/// (OWNER-M6D-1; the anchor since M6E-1), so the anchor is `D` itself. On e/acc's `mature` pool
/// (third-party LP, λ 92.98 %) `L_ema` and `L_slow` start at its locked depth, below the pool's
/// `√k` (at row 56's `⌊D·10⁴/pool_lock_min_bps⌋` = 1.86·√k `L_ema` started at `√k`); the first
/// warm-up integration re-seeds `L_ema` from the pool's own bucket, `√k`.
#[test]
fn the_first_pumpswap_observation_starts_the_price_marked_for_the_warm_up() {
    let (mut env, p) = list_and_check(PammCoin::Mature);
    let lender = env.funded_keypair();
    let pm = PammMarket::new(&env.svm, &p.mint, lender, Pubkey::default());
    env.warp(1);
    let r = p.reserves(&env.svm).unwrap();
    let (lam, d) = p.locked_depth(&env.svm);
    assert_eq!(lam, 9_298, "e/acc's burned share (R-VEN Update H11)");
    crank(&mut env, &pm);
    let m = market(&env, &pm);
    let spot = r.effective_quote * shortwin_math::num::WAD / u128::from(r.base);
    let l = shortwin_math::quote::liquidity(u128::from(r.base), r.effective_quote).unwrap();
    assert_eq!(m.ema_price.get(), spot);
    // λ_min 100 %: the anchor is D (< √k: the pool has third-party LP); L_slow is the depth.
    assert!(d < l);
    assert_eq!(
        (m.ema_liq.get(), m.liq_slow.get(), m.pool_d.get()),
        (d, d, d)
    );
    assert_eq!((m.ema_warm, m.ema_guard, m.ring_count), (8, 1, 1));
    // The first warm-up integration re-seeds L_ema from the pool's own bucket (its √k).
    crank_secs(&mut env, &pm, 1);
    let m = market(&env, &pm);
    assert_eq!((m.ema_warm, m.ema_liq.get()), (7, l));
    crank_secs(&mut env, &pm, 7);
    let m = market(&env, &pm);
    assert_eq!(m.ema_warm, 0);
    assert_eq!(
        m.liq_slow.get(),
        d,
        "no integration of L_slow in the warm-up"
    );
}

/// The pAMM init set is key-checked (`VenueAccountMismatch`): pump's init set in its place, or
/// another user's UVA, is refused; the wrong count is `BadRemainingAccounts`.
#[test]
fn the_pamm_init_set_is_key_checked() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let creator = env.funded_keypair();
    let market = pda::market(&p.mint).0;
    let base = shortwin_litesvm_tests::ix::create_market(
        &creator.pubkey(),
        &p.mint,
        &p.base_program,
        shortwin::instructions::create_market::VENUE_HINT_AUTO,
        &p.pool,
    );
    let with = |set: &[AccountMeta]| shortwin_litesvm_tests::ix::with_remaining(base.clone(), set);
    assert_err(
        &env.send(&[with(&pump::pump_init_set(&market))], &[&creator]),
        E::VenueAccountMismatch,
    );
    let mut foreign = pamm::pamm_init_set(&market);
    foreign[1] = AccountMeta::new(pamm_uva(&creator.pubkey()), false);
    assert_err(
        &env.send(&[with(&foreign)], &[&creator]),
        E::VenueAccountMismatch,
    );
    let short = &pamm::pamm_init_set(&market)[..2];
    assert_err(
        &env.send(&[with(short)], &[&creator]),
        E::BadRemainingAccounts,
    );
    ok(env.send(&[with(&pamm::pamm_init_set(&market))], &[&creator]));
}

/// A list-only market of a coin whose canonical pool did not read as one at its listing
/// (M5-SEC-5: `None`, no `graduated_elsewhere`) moves onto the pool once it reads again:
/// `crank_price` on the detection set emits `None → PumpSwap`, writes the PumpSwap cache and
/// `graduated_at`, and the first PumpSwap crank starts the price, marked for the warm-up.
#[test]
fn a_list_only_market_is_redetected_onto_its_canonical_pool() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let saved = env.svm.get_account(&p.pool).expect("pool");
    fork::patch(&mut env.svm, &p.pool, |d| d[0] ^= 0xff);
    let creator = env.funded_keypair();
    let ix = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
    // The unreadable pool lists list-only: no init set (a list-only listing takes none).
    let mut ix = ix;
    ix.accounts.truncate(ix.accounts.len() - 3);
    ok(env.send(&[ix], &[&creator]));
    let market = pda::market(&p.mint).0;
    let m = env.market(&market);
    assert_eq!(m.venue_kind(), Some(VenueKind::None));
    assert!(!m.has_flag(shortwin::state::MARKET_FLAG_GRADUATED_ELSEWHERE));
    env.svm.set_account(p.pool, saved).unwrap();
    struct Detection(shortwin_litesvm_tests::fixtures::MarketKeys, Pubkey, Pubkey);
    impl VenueMarket for Detection {
        fn keys(&self) -> &shortwin_litesvm_tests::fixtures::MarketKeys {
            &self.0
        }
        fn read_set(&self, _w: bool) -> Vec<AccountMeta> {
            vec![
                AccountMeta::new_readonly(self.1, false),
                AccountMeta::new_readonly(self.2, false),
            ]
        }
        fn trade_extras(&self) -> Vec<AccountMeta> {
            Vec::new()
        }
    }
    let lender = env.funded_keypair();
    let pm = PammMarket::new(&env.svm, &p.mint, lender, Pubkey::default());
    let det = Detection(pm.keys, pda::bonding_curve(&p.mint), p.pool);
    env.warp(1);
    let meta = crank(&mut env, &det);
    venue_changed(&meta, VenueKind::None, VenueKind::PumpSwap);
    let m = env.market(&market);
    assert_eq!(m.venue_kind(), Some(VenueKind::PumpSwap));
    assert_eq!(m.graduated_at, env.now());
    assert_eq!(m.pool_cache[pc::POOL_BASE_VAULT], p.base_vault);
    assert_eq!(m.ring_count, 0, "no price yet");
    env.warp(1);
    crank(&mut env, &pm);
    let m = env.market(&market);
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert!(m.ema_price.get() > 0);
}
