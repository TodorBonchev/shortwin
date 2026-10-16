//! `create_market(Auto)` on pump coins (ARCH §2.5 `classify_pump`, §3.2; D1, ADOPTED Q6; M5):
//! the exact state of a curve listing (the curve venue cache, the trader's base ATA and pump UVA,
//! the price state at the curve's spot, tier D), a migrating curve's listing, the list-only
//! outcomes (a graduated coin lists on its canonical pool since M6, a coin that graduated
//! elsewhere lists as `None`), and every refusal:
//! mayhem and non-SOL curves (Q6), a curve under 115 bytes (`CurveNeedsExtend`), a pump curve that
//! does not parse (fail closed), a Raydium hint on a live curve (D1), a missing or wrong init set;
//! and (M10 G-A3) spoofed owners at the bonding-curve and canonical-pool PDAs: no pump venue at
//! listing or at `crank_price`'s re-detection, D1 kept for a re-owned canonical pool.

use shortwin::events::MarketCreated;
use shortwin::instructions::create_market::{VENUE_HINT_AUTO, VENUE_HINT_RAYDIUM_V4};
use shortwin::state::{MARKET_FLAG_GRADUATED_ELSEWHERE, Market};
use shortwin_litesvm_tests::ix;
use shortwin_math::num::WAD;
use shortwin_venue_state::pump as vpump;

use crate::common::*;

fn listed(env: &Env, c: &PumpCurve) -> Market {
    env.market(&pda::market(&c.mint).0)
}

/// A live curve lists as `PumpCurve` with every cache entry, the trader's accounts and the price
/// state (module docs), on each live SOL coin of the dump that short.win accepts.
#[test]
fn a_live_curve_lists_as_pump_curve() {
    for coin in [
        PumpCoin::Fresh,
        PumpCoin::Near,
        PumpCoin::T22_115,
        PumpCoin::Legacy115,
    ] {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let creator = env.funded_keypair();
        let before = lamports(&env, &creator.pubkey());
        let meta = ok(env.send(
            &[pump::create_pump_market_ix(&creator.pubkey(), &c)],
            &[&creator],
        ));
        let ev: MarketCreated = events::one(&meta);
        let m = listed(&env, &c);
        let ctx = format!("{coin:?}");
        assert_eq!(m.venue, VenueKind::PumpCurve as u8, "{ctx}");
        assert_eq!(ev.venue, VenueKind::PumpCurve as u8, "{ctx}");
        assert_eq!(m.tier, shortwin_math::tier::Tier::D as u8, "{ctx}: Tier D");
        assert_eq!(ev.tier, m.tier, "{ctx}");
        assert_eq!(ev.ray_pool, Pubkey::default(), "{ctx}");
        assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE), "{ctx}");
        // The curve venue cache, derived here independently (ARCH §2.3.4).
        let market = pda::market(&c.mint).0;
        let trader = pda::trader(&market).0;
        let curve = c.state(&env.svm);
        let creator_key = Pubkey::new_from_array(curve.creator);
        let vault = fork::pump_creator_vault(&creator_key);
        let uva = fork::pump_user_volume_accumulator(&trader);
        assert_eq!(m.bonding_curve, c.curve, "{ctx}");
        assert_eq!(m.curve_base_ata, c.curve_base_ata, "{ctx}");
        assert_eq!(m.curve_quote_ata, pump::wsol_ata(&c.curve), "{ctx}");
        assert_eq!(m.sharing_config, pump::sharing_config(&c.mint), "{ctx}");
        assert_eq!(m.pump_uva, uva, "{ctx}");
        assert_eq!(m.pump_uva_quote_ata, pump::wsol_ata(&uva), "{ctx}");
        assert_eq!(m.cached_creator, creator_key, "{ctx}");
        assert_eq!(m.cached_creator_vault, vault, "{ctx}");
        assert_eq!(m.cached_creator_vault_ata, pump::wsol_ata(&vault), "{ctx}");
        // The trader's base ATA (the mint's program) and pump UVA exist.
        let ata = pda::ata(&trader, &c.mint, &c.token_program.id());
        assert_eq!(m.trader_base_ata, ata, "{ctx}");
        let acc = read::account(&env.svm, &ata).expect("trader_base_ata");
        assert_eq!(acc.owner, c.token_program.id(), "{ctx}");
        assert_eq!(tokens(&env, &ata), 0, "{ctx}");
        let uacc = read::account(&env.svm, &uva).expect("pump UVA");
        let u = vpump::read_user_volume_accumulator(&uacc.owner.to_bytes(), &uacc.data).unwrap();
        assert_eq!(u.user, trader.to_bytes(), "{ctx}: the UVA is the trader's");
        // The price state at the curve's spot (ARCH §4.4 "Initialisation"); never marked.
        let (vt, vq) = (
            u128::from(curve.virtual_token_reserves),
            u128::from(curve.virtual_quote_reserves),
        );
        assert_eq!(m.ema_price.get(), vq * WAD / vt, "{ctx}: P_ema = vQ·WAD/vT");
        let l = shortwin_math::quote::liquidity(vt, vq).unwrap();
        assert_eq!(m.ema_liq.get(), l, "{ctx}: L_ema = isqrt(vT·vQ)");
        assert_eq!(m.liq_slow.get(), l, "{ctx}");
        assert_eq!((m.ring_count, m.ema_warm, m.ema_guard), (1, 0, 0), "{ctx}");
        assert_eq!(m.created_at, env.now(), "{ctx}");
        // The creator paid the accounts, the trader's ATA and UVA, and the creation fee.
        let spent = before - lamports(&env, &creator.pubkey());
        let rent = |n: usize| env.svm.minimum_balance_for_rent_exemption(n);
        let fee = env.config().params.market_creation_fee;
        let expected = rent(Market::SPACE)
            + rent(shortwin::state::MarketVault::SPACE)
            + rent(165)
            + rent(acc.data.len())
            + rent(vpump::USER_VOLUME_ACCUMULATOR_LEN)
            + fee;
        assert_eq!(spent, expected, "{ctx}: the creator's cost");
    }
}

/// A trader UVA or base ATA a third party created first is harmless (it is keyed to `trader`).
#[test]
fn pre_created_trader_accounts_are_harmless() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    let market = pda::market(&c.mint).0;
    let trader = pda::trader(&market).0;
    let payer = env.payer.pubkey();
    ok(env.send(
        &[
            fork::pump_init_uva_ix(&payer, &trader),
            fork::create_ata_idempotent_ix(&payer, &trader, &c.mint, &c.token_program.id()),
        ],
        &[],
    ));
    ok(env.list_pump(&c));
    assert_eq!(listed(&env, &c).venue, VenueKind::PumpCurve as u8);
}

/// A curve that completed and has not migrated lists as `PumpMigrating` (its pause starts now).
#[test]
fn a_migrating_curve_lists_as_pump_migrating() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    fork::set_curve_complete(&mut env.svm, &c.curve, true);
    ok(env.list_pump(&c));
    let m = listed(&env, &c);
    assert_eq!(m.venue, VenueKind::PumpMigrating as u8);
    assert_eq!(m.migration_seen_at, env.now());
    assert!(
        m.ema_price.get() > 0,
        "the completed curve's price starts the EMA"
    );
}

/// A pump coin on its canonical PumpSwap pool lists on that pool (M6: `PumpSwap`, with the pAMM
/// init set; no price until its first PumpSwap observation — `pamm_it::listing` checks the rest);
/// without the init set the listing is refused (`BadRemainingAccounts`). A coin that graduated
/// elsewhere (Fartcoin: zeroed curve, no canonical pool) lists as `None` with the
/// `graduated_elsewhere` flag and takes no init set.
#[test]
fn graduated_coins_list_on_their_pool_or_as_none() {
    for (coin, elsewhere) in [
        (PumpCoin::Graduated, false),
        (PumpCoin::Legacy150, false),
        (PumpCoin::Fartcoin, true),
    ] {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let creator = env.funded_keypair();
        let plain = ix::create_market(
            &creator.pubkey(),
            &c.mint,
            &c.token_program.id(),
            VENUE_HINT_AUTO,
            &pda::canonical_pool(&c.mint),
        );
        if elsewhere {
            ok(env.send(&[plain], &[&creator]));
        } else {
            assert_err(
                &env.send(std::slice::from_ref(&plain), &[&creator]),
                E::BadRemainingAccounts,
            );
            let market = pda::market(&c.mint).0;
            let with =
                ix::with_remaining(plain, &shortwin_litesvm_tests::pamm::pamm_init_set(&market));
            ok(env.send(&[with], &[&creator]));
        }
        let m = listed(&env, &c);
        let want = if elsewhere {
            VenueKind::None
        } else {
            VenueKind::PumpSwap
        };
        assert_eq!(m.venue, want as u8, "{coin:?}");
        assert_eq!(
            m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE),
            elsewhere,
            "{coin:?}"
        );
        assert_eq!(m.ema_price.get(), 0, "{coin:?}: no price");
    }
}

/// A list-only listing refuses the pump init set (strict remaining accounts).
#[test]
fn a_list_only_listing_takes_no_init_set() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fartcoin);
    let res = env.list_pump(&c);
    assert_err(&res, E::BadRemainingAccounts);
}

/// ADOPTED Q6 and ARCH §2.5: a mayhem curve, a USDC curve, a curve under 115 bytes and a pump
/// curve that does not parse are refused (fail closed); a Raydium hint on a live curve is refused
/// (D1) before any candidate is read.
#[test]
fn refusals() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Mayhem);
    assert_err(&env.list_pump(&c), E::CurveMayhemUnsupported);

    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Usdc);
    assert_err(&env.list_pump(&c), E::CurveQuoteNotSol);

    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Legacy49);
    assert_eq!(c.data_len(&env.svm), 49);
    assert_err(&env.list_pump(&c), E::CurveNeedsExtend);

    // A pump-owned curve whose bool is corrupted: not parseable, refused (never "not pump").
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    fork::patch(&mut env.svm, &c.curve, |d| d[vpump::OFF_COMPLETE] = 7);
    assert_err(&env.list_pump(&c), E::PumpVenuePrecedence);

    // D1: a Raydium hint on a live curve (the candidate is never read).
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    let creator = env.funded_keypair();
    let hinted = ix::with_remaining(
        ix::create_market(
            &creator.pubkey(),
            &c.mint,
            &c.token_program.id(),
            VENUE_HINT_RAYDIUM_V4,
            &pda::canonical_pool(&c.mint),
        ),
        &[
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
        ],
    );
    assert_err(&env.send(&[hinted], &[&creator]), E::PumpVenuePrecedence);
}

/// T-LISTING-CASHBACK (pump-final §6.3, owner decision D1 (a)): `create_market(Auto)` on the live
/// cashback curve `cashback166` refuses `CurveCashbackCoin` (6109): pump's v3 trades refuse a
/// cashback coin (6094), and every curve leg trades v3. The coin graduates for real — a third
/// party's `buy_v2` of the whole `rT` (v2 still trades a cashback coin), anyone's `migrate_v2` on
/// the ELF — and the same coin then lists on its canonical PumpSwap pool (the refusal sits on the
/// live branch only, C2).
#[test]
fn a_live_cashback_curve_is_refused_and_lists_after_graduation() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Cashback166);
    assert!(c.state(&env.svm).is_cashback_coin);
    assert_err(&env.list_pump(&c), E::CurveCashbackCoin);
    assert_eq!(code(E::CurveCashbackCoin), 6_109);
    let u = CurveUser::new(&mut env, &c);
    env.fund(&u.kp.pubkey(), 1_000_000_000_000);
    let rt = c.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &c, rt, u64::MAX));
    assert!(c.state(&env.svm).complete);
    let migrator = env.funded_keypair();
    let mig = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &c);
    ok(env.send(&[pump::cu_limit_ix(600_000), mig], &[&migrator]));
    assert!(c.state(&env.svm).is_migrated());
    ok(env.list_pamm(&c.mint, &c.token_program.id()));
    assert_eq!(listed(&env, &c).venue, VenueKind::PumpSwap as u8);
}

/// The init set is mandatory and key-checked: missing (`BadRemainingAccounts`), the UVA of another
/// user, another program or event authority (`VenueAccountMismatch`); a wrong detection key too.
#[test]
fn the_init_set_is_strict() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    let creator = env.funded_keypair();
    let base = ix::create_market(
        &creator.pubkey(),
        &c.mint,
        &c.token_program.id(),
        VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    assert_err(
        &env.send(std::slice::from_ref(&base), &[&creator]),
        E::BadRemainingAccounts,
    );
    let market = pda::market(&c.mint).0;
    let good = pump::pump_init_set(&market);
    for i in 0..3 {
        let mut bad = good.clone();
        bad[i].pubkey = match i {
            1 => fork::pump_user_volume_accumulator(&creator.pubkey()),
            _ => Pubkey::new_unique(),
        };
        let res = env.send(&[ix::with_remaining(base.clone(), &bad)], &[&creator]);
        assert_err(&res, E::VenueAccountMismatch);
    }
    let mut short = good.clone();
    short.pop();
    assert_err(
        &env.send(&[ix::with_remaining(base.clone(), &short)], &[&creator]),
        E::BadRemainingAccounts,
    );
    // A wrong canonical-pool key in the detection set.
    let mut wrong = ix::with_remaining(base.clone(), &good);
    wrong.accounts[base.accounts.len() - 1].pubkey = Pubkey::new_unique();
    assert_err(&env.send(&[wrong], &[&creator]), E::VenueAccountMismatch);
    ok(env.send(&[ix::with_remaining(base, &good)], &[&creator]));
}

// ---- spoofed owners at the detection PDAs (M10 G-A3) ---------------------------------------------

/// A list-only market's view for `crank_price`: the detection set `[bonding_curve,
/// canonical_pool]` as its ReadSet.
struct Detection(shortwin_litesvm_tests::fixtures::MarketKeys);

impl Detection {
    fn of(mint: &Pubkey, token_program: &Pubkey) -> Self {
        let market = pda::market(mint).0;
        Self(shortwin_litesvm_tests::fixtures::MarketKeys {
            mint: *mint,
            token_program: *token_program,
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            trader: pda::trader(&market).0,
        })
    }
}

impl VenueMarket for Detection {
    fn keys(&self) -> &shortwin_litesvm_tests::fixtures::MarketKeys {
        &self.0
    }
    fn read_set(&self, _writable: bool) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(pda::bonding_curve(&self.0.mint), false),
            AccountMeta::new_readonly(pda::canonical_pool(&self.0.mint), false),
        ]
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        Vec::new()
    }
}

/// `create_market(Auto)` with the detection set only (a list-only listing).
fn list_only(env: &mut Env, mint: &Pubkey, token_program: &Pubkey) -> TxResult {
    let creator = env.funded_keypair();
    let ix = ix::create_market(
        &creator.pubkey(),
        mint,
        token_program,
        VENUE_HINT_AUTO,
        &pda::canonical_pool(mint),
    );
    env.send(&[ix], &[&creator])
}

/// ARCH §2.5 `classify_pump` (`bc.owner == pump`; `venues::pump_curve::parse_curve`), M10 G-A3: an
/// account at a live coin's bonding-curve PDA that pump does not own — a System account pre-funded
/// by a griefer (the reachable case), and plants mainnet cannot reach: the real curve's bytes
/// under the System program, under pAMM, under an unknown program — is no pump curve. The listing
/// with the pump init set is refused (`BadRemainingAccounts`: a list-only outcome takes none), the
/// list-only listing lands as `None` (no `graduated_elsewhere`, no price), and `crank_price`'s
/// re-detection keeps it there (no `VenueStateChanged`). The control: the real pump-owned curve put
/// back, the next crank moves the market `None → PumpCurve`.
#[test]
fn a_spoofed_curve_owner_is_not_a_pump_venue() {
    let unknown = Pubkey::new_unique();
    for (what, owner, keep_bytes) in [
        (
            "a System account pre-funded",
            anchor_lang::system_program::ID,
            false,
        ),
        (
            "the curve's bytes, System-owned",
            anchor_lang::system_program::ID,
            true,
        ),
        (
            "the curve's bytes, pAMM-owned",
            shortwin::constants::PUMP_AMM_PROGRAM_ID,
            true,
        ),
        ("the curve's bytes, an unknown owner", unknown, true),
    ] {
        let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
        let saved = env.svm.get_account(&c.curve).expect("curve");
        let mut spoof = saved.clone();
        spoof.owner = owner;
        if !keep_bytes {
            spoof.data.clear();
            spoof.lamports = 1_000_000;
        }
        env.svm.set_account(c.curve, spoof).unwrap();
        assert_err(&env.list_pump(&c), E::BadRemainingAccounts);
        let program = c.token_program.id();
        ok(list_only(&mut env, &c.mint, &program));
        let m = listed(&env, &c);
        assert_eq!(m.venue_kind(), Some(VenueKind::None), "{what}");
        assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE), "{what}");
        assert_eq!(m.ema_price.get(), 0, "{what}: no price");
        let det = Detection::of(&c.mint, &program);
        env.warp(1);
        no_venue_change(&crank(&mut env, &det));
        assert_eq!(
            listed(&env, &c).venue_kind(),
            Some(VenueKind::None),
            "{what}"
        );
        env.svm.set_account(c.curve, saved).unwrap();
        env.warp(1);
        venue_changed(
            &crank(&mut env, &det),
            VenueKind::None,
            VenueKind::PumpCurve,
        );
    }
}

/// ARCH §2.5 `canonical_pool_valid` (`cp.owner == pAMM`; `venues::detect::canonical_pool`), M10
/// G-A3: the canonical pool of a graduated coin re-owned — its bytes under pump, under an unknown
/// program (plants mainnet cannot reach: only pAMM can create an account at its PDA) — is not a
/// PumpSwap pool, yet still a pump venue for D1 (M5-SEC-5's `Unreadable`): the PumpSwap listing is
/// refused (`BadRemainingAccounts`), a Raydium hint is refused before its candidate is read
/// (`PumpVenuePrecedence`), the list-only listing lands as `None` **without** `graduated_elsewhere`,
/// and the crank keeps it there. The control: the real pool put back, the next crank moves the
/// market `None → PumpSwap`.
#[test]
fn a_spoofed_canonical_pool_owner_is_not_a_pumpswap_pool() {
    for (what, owner) in [
        ("pump-owned", shortwin::constants::PUMP_PROGRAM_ID),
        ("an unknown owner", Pubkey::new_unique()),
    ] {
        let (mut env, c) = Env::fork_pump_coin(PumpCoin::Graduated);
        let pool = pda::canonical_pool(&c.mint);
        let saved = env.svm.get_account(&pool).expect("canonical pool");
        assert_eq!(saved.owner, shortwin::constants::PUMP_AMM_PROGRAM_ID);
        let mut spoof = saved.clone();
        spoof.owner = owner;
        env.svm.set_account(pool, spoof).unwrap();
        let program = c.token_program.id();
        let creator = env.funded_keypair();
        let market = pda::market(&c.mint).0;
        let plain = ix::create_market(&creator.pubkey(), &c.mint, &program, VENUE_HINT_AUTO, &pool);
        let with = ix::with_remaining(plain, &shortwin_litesvm_tests::pamm::pamm_init_set(&market));
        assert_err(&env.send(&[with], &[&creator]), E::BadRemainingAccounts);
        let hinted = ix::with_remaining(
            ix::create_market(
                &creator.pubkey(),
                &c.mint,
                &program,
                VENUE_HINT_RAYDIUM_V4,
                &pool,
            ),
            &[
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
            ],
        );
        assert_err(&env.send(&[hinted], &[&creator]), E::PumpVenuePrecedence);
        ok(list_only(&mut env, &c.mint, &program));
        let m = listed(&env, &c);
        assert_eq!(m.venue_kind(), Some(VenueKind::None), "{what}");
        assert!(
            !m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE),
            "{what}: D1 holds"
        );
        let det = Detection::of(&c.mint, &program);
        env.warp(1);
        no_venue_change(&crank(&mut env, &det));
        env.svm.set_account(pool, saved).unwrap();
        env.warp(1);
        venue_changed(&crank(&mut env, &det), VenueKind::None, VenueKind::PumpSwap);
    }
}
