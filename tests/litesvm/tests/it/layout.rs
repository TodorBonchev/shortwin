//! T-LAYOUT, runtime part (ARCH §2.3, §8.2). The compile-time part is
//! `programs/shortwin/src/state/layout_asserts.rs`, compiled for sbpfv3 (`anchor build`),
//! x86_64 (this build) and wasm32 (`cargo check -p shortwin --target wasm32-unknown-unknown`).
//! Here: the accounts the SBF program creates have exactly the documented sizes, and fields
//! sit at the documented byte offsets of the raw account data (what a Codama decoder reads).

use shortwin::state::{
    GlobalConfig, LenderPosition, Market, MarketVault, OracleCache, ShareEpoch, ShortPosition,
    VenueExtras, VenueGuard, VenueKind, WalletExposure,
};
use shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{Env, pda, read};
use shortwin_venue_state::bytes::{i64_at, key_at, u8_at, u16_at, u32_at, u64_at};

#[test]
fn documented_sizes() {
    assert_eq!(GlobalConfig::SPACE, 1_448);
    assert_eq!(VenueGuard::SPACE, 224);
    assert_eq!(VenueExtras::SPACE, 6_512); // 4,352 before M2-SEC-2 (`stable`, `current_since`)
    assert_eq!(OracleCache::SPACE, 45);
    assert_eq!(Market::SPACE, 3_104);
    assert_eq!(MarketVault::SPACE, 2_200);
    assert_eq!(ShortPosition::SPACE, 384);
    assert_eq!(LenderPosition::SPACE, 241);
    assert_eq!(ShareEpoch::SPACE, 143); // 127 before KI-65 (`conv_rem`, the last 16 bytes)
    assert_eq!(ShareEpoch::CONV_REM_OFFSET, 127);
    assert_eq!(WalletExposure::SPACE, 81);
}

#[test]
fn accounts_created_on_chain_have_the_documented_sizes() {
    let env = Env::initialized();
    let len = |k| read::account(&env.svm, &k).map(|a| a.data.len());
    assert_eq!(len(pda::config()), Some(GlobalConfig::SPACE));
    assert_eq!(len(pda::venue_guard()), Some(VenueGuard::SPACE));
    assert_eq!(len(pda::oracle_cache()), Some(OracleCache::SPACE));
    for kind in VenueKind::WITH_EXTRAS {
        let key = pda::venue_extras(kind);
        assert_eq!(len(key), Some(VenueExtras::SPACE));
        let e: VenueExtras = read::zc(&env.svm, &key).unwrap();
        assert_eq!(e.venue, kind as u8);
        assert_eq!(e.version, shortwin::constants::VENUE_EXTRAS_VERSION);
        assert!(e.sets.iter().all(|s| s.current.count == 0
            && s.previous.count == 0
            && s.stable.count == 0
            && s.current_since == 0));
    }
    // Every created account is rent-exempt at its size.
    for key in [pda::config(), pda::venue_guard(), pda::oracle_cache()] {
        let a = read::account(&env.svm, &key).unwrap();
        assert!(a.lamports >= env.svm.minimum_balance_for_rent_exemption(a.data.len()));
        assert_eq!(a.owner, shortwin::ID);
    }
}

#[test]
fn global_config_fields_sit_at_their_offsets() {
    let env = Env::initialized();
    let args = env.default_init_args();
    let data = read::account(&env.svm, &pda::config()).unwrap().data;
    assert_eq!(u16_at(&data, 8), shortwin::constants::CONFIG_VERSION);
    assert_eq!(u32_at(&data, 12), args.venue_enabled);
    assert_eq!(u32_at(&data, 20), args.timelock_s);
    assert_eq!(key_at(&data, 32), args.governance.to_bytes());
    assert_eq!(key_at(&data, 64), args.pause_authority.to_bytes());
    assert_eq!(key_at(&data, 128), args.fee_recipient.to_bytes());
    assert_eq!(key_at(&data, 160), args.pyth_sol_usd.to_bytes());
    assert_eq!(key_at(&data, 192), args.pyth_feed_id);
    // params @224: settle_keeper_fixed (u64 @0), open_fee_bps (u32 @96), kappa_growth_n (u16 @262)
    assert_eq!(u64_at(&data, 224), args.params.settle_keeper_fixed);
    assert_eq!(u32_at(&data, 224 + 96), args.params.open_fee_bps);
    assert_eq!(u16_at(&data, 224 + 262), args.params.kappa_growth_n);
    // tiers @488 (48 B each): tier D's cr_liq_bps
    assert_eq!(u32_at(&data, 488 + 3 * 48 + 8), args.tiers[3].cr_liq_bps);
    // ema @680, kink @712, pending @720
    assert_eq!(u32_at(&data, 680 + 4), args.ema.ema_clamp_bps);
    assert_eq!(u32_at(&data, 680 + 16), 1);
    assert_eq!(u32_at(&data, 712), args.util_kink_bps);
    assert_eq!(u8_at(&data, 720), 0);
}

#[test]
fn venue_guard_fields_sit_at_their_offsets() {
    let env = Env::initialized();
    let data = read::account(&env.svm, &pda::venue_guard()).unwrap().data;
    assert_eq!(u16_at(&data, 8), shortwin::constants::VENUE_GUARD_VERSION);
    assert_eq!(u32_at(&data, 12), 0);
    for (i, slot) in FIXTURE_GUARD_SLOTS.iter().enumerate() {
        assert_eq!(u64_at(&data, 16 + 8 * i), *slot);
    }
}

#[test]
fn market_and_vault_fields_sit_at_their_offsets() {
    let mut env = Env::initialized();
    let (k, _) = env.install_market(TokenProgram::Token2022, 9);
    let m = read::account(&env.svm, &k.market).unwrap().data;
    assert_eq!(m.len(), Market::SPACE);
    assert_eq!(u16_at(&m, 8), shortwin::constants::MARKET_VERSION);
    assert_eq!(u8_at(&m, 17), VenueKind::None as u8);
    assert_eq!(u8_at(&m, 19), 9);
    assert_eq!(i64_at(&m, 24), env.market(&k.market).created_at);
    assert_eq!(key_at(&m, 32), k.mint.to_bytes());
    assert_eq!(key_at(&m, 64), k.token_program.to_bytes());
    assert_eq!(key_at(&m, 128), k.pool_vault.to_bytes());
    assert_eq!(key_at(&m, 160), k.market_vault.to_bytes());
    assert_eq!(key_at(&m, 192), k.trader.to_bytes());
    assert_eq!(key_at(&m, 288), pda::bonding_curve(&k.mint).to_bytes());
    assert_eq!(key_at(&m, 320), pda::canonical_pool(&k.mint).to_bytes());
    assert_eq!(u32_at(&m, 2_800), env.config().ema_params.version);
    assert_eq!(u32_at(&m, 2_812), env.config().kink.util_kink_bps);

    let v = read::account(&env.svm, &k.market_vault).unwrap().data;
    assert_eq!(v.len(), MarketVault::SPACE);
    // pi = WAD (PodU128 @32, little endian)
    assert_eq!(
        shortwin_venue_state::bytes::u128_at(&v, 32),
        shortwin_math::num::WAD
    );
}
