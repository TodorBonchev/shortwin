//! `crank_sol_usd` on LiteSVM (ARCH §3.9, §4.7, §8.2; M1 `OracleCache` + `crank_sol_usd`).
//!
//! Every case starts from the real sponsored shard-0 SOL/USD `PriceUpdateV2` dumped from
//! mainnet with `solana account` (`tests/fixtures/accounts/pyth_sol_usd_shard0.{json,bin}`,
//! provenance in `tests/fixtures/manifest.json`) and from mutated copies of it: wrong owner,
//! wrong address, partial verification, stale, wrong feed, wide confidence, large steps,
//! another exponent, a non-positive price, a wrong discriminator, truncated data. The clock is
//! set to the dump's wall-clock time and slot, so the unmodified dump is 7 s old.
//!
//! A separate test binary (`tests/oracle_*.rs`, the oracle track's files); it sends through
//! `Env::send`, so the harness's transaction invariants run on every crank.

#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use shortwin::constants::{PYTH_RECEIVER_ID, PYTH_SOL_USD_ACCOUNT, PYTH_SOL_USD_FEED_ID};
use shortwin::errors::ShortwinError;
use shortwin::state::{ConfigChange, OracleCache, Params, TierParams};
use shortwin_litesvm_tests::errors::{assert_err, ok};
use shortwin_litesvm_tests::{Env, TxResult, ix, pda, read};
use shortwin_venue_state::pyth::{self, PriceUpdate, VerificationLevel};
use solana_account::Account;
use solana_signer::Signer;

/// The mainnet dump: the account data (`base64 -d` of the JSON's `data[0]`).
const DUMP: &[u8] = include_bytes!("../../fixtures/accounts/pyth_sol_usd_shard0.bin");
/// The mainnet dump: `solana account … --output json`, verbatim.
const DUMP_JSON: &str = include_str!("../../fixtures/accounts/pyth_sol_usd_shard0.json");
/// Fixture provenance.
const MANIFEST: &str = include_str!("../../fixtures/manifest.json");
/// Wall clock of the dump, 2026-09-27T15:31:04Z.
const DUMP_TS: i64 = 1_790_523_064;
/// `solana slot` at the dump.
const DUMP_SLOT: u64 = 451_037_803;
/// Pyth BTC/USD feed id (a real feed that is not SOL/USD).
const BTC_USD: [u8; 32] = [
    0xe6, 0x2d, 0xf6, 0xc8, 0xb4, 0xa8, 0x5f, 0xe1, 0xa6, 0x7d, 0xb4, 0x4d, 0xc1, 0x2d, 0xe5, 0xdb,
    0x33, 0x0f, 0x7a, 0xc6, 0x6b, 0x72, 0xdc, 0x65, 0x8a, 0xfe, 0xdf, 0x0f, 0x4a, 0x41, 0x5b, 0x43,
];

/// The dump, decoded.
fn dump() -> PriceUpdate {
    pyth::read(&pyth::RECEIVER, DUMP).unwrap()
}

/// An initialized environment at the dump's time and slot.
fn env() -> Env {
    let mut env = Env::initialized();
    env.set_clock(DUMP_TS, DUMP_SLOT);
    env
}

/// Writes `data` at `key` with `owner` (rent-exempt; the dump's own balance is irrelevant).
fn put(env: &mut Env, key: Pubkey, owner: Pubkey, data: Vec<u8>) {
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            key,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

/// Writes `u` (re-encoded in the receiver's 134-byte layout) at the configured address.
fn put_update(env: &mut Env, u: &PriceUpdate) {
    put(
        env,
        PYTH_SOL_USD_ACCOUNT,
        PYTH_RECEIVER_ID,
        pyth::encode(u).to_vec(),
    );
}

fn crank(env: &mut Env) -> TxResult {
    env.send(&[ix::crank_sol_usd(&PYTH_SOL_USD_ACCOUNT)], &[])
}

fn cache(env: &Env) -> OracleCache {
    env.oracle_cache()
}

/// `u` published `dt` seconds after the dump, at `price`.
fn later(u: &PriceUpdate, dt: i64, price: i64) -> PriceUpdate {
    PriceUpdate {
        price,
        publish_time: u.publish_time + dt,
        prev_publish_time: u.publish_time + dt - 1,
        posted_slot: u.posted_slot.saturating_add_signed(dt * 5 / 2),
        ..*u
    }
}

/// Cranks the unmodified dump into the cache.
fn seed(env: &mut Env) -> PriceUpdate {
    let u = dump();
    put_update(env, &u);
    ok(crank(env));
    u
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---- the fixture -----------------------------------------------------------------------

/// The committed bytes are the JSON dump's data, and the manifest records what they decode to.
#[test]
fn the_fixture_is_the_recorded_mainnet_dump() {
    assert_eq!(DUMP.len(), 134);
    assert!(DUMP_JSON.contains(&format!("\"{}\"", base64(DUMP))));
    assert!(DUMP_JSON.contains(&format!("\"pubkey\": \"{PYTH_SOL_USD_ACCOUNT}\"")));
    assert!(DUMP_JSON.contains(&format!("\"owner\": \"{PYTH_RECEIVER_ID}\"")));
    assert!(DUMP_JSON.contains("\"space\": 134"));
    let u = dump();
    assert_eq!(u.verification, VerificationLevel::Full);
    assert_eq!(u.feed_id, PYTH_SOL_USD_FEED_ID);
    for (field, value) in [
        ("price", u.price.to_string()),
        ("conf", u.conf.to_string()),
        ("exponent", u.exponent.to_string()),
        ("publish_time", u.publish_time.to_string()),
        ("posted_slot", u.posted_slot.to_string()),
        ("slot", DUMP_SLOT.to_string()),
    ] {
        assert!(
            MANIFEST.contains(&format!("\"{field}\": {value}")),
            "manifest does not record {field} = {value}"
        );
    }
    assert!(MANIFEST.contains("2026-09-27T15:31:04Z"));
    assert_eq!(DUMP_TS - u.publish_time, 7);
}

// ---- acceptance ---------------------------------------------------------------------------

#[test]
fn the_mainnet_update_is_cached() {
    let mut env = env();
    let before = read::account(&env.svm, &pda::config()).unwrap().data;
    let empty = cache(&env);
    assert_eq!(
        (empty.price, empty.publish_time, empty.updated_slot),
        (0, 0, 0)
    );

    let u = dump();
    put(
        &mut env,
        PYTH_SOL_USD_ACCOUNT,
        PYTH_RECEIVER_ID,
        DUMP.to_vec(),
    );
    ok(crank(&mut env));
    let c = cache(&env);
    assert_eq!(
        c,
        OracleCache {
            price: 12_179_606_511,
            conf: 1_399_451,
            expo: -8,
            publish_time: u.publish_time,
            updated_slot: DUMP_SLOT,
            bump: empty.bump,
        }
    );
    // Only the cache is written.
    assert_eq!(
        read::account(&env.svm, &pda::config()).unwrap().data,
        before
    );
    assert_eq!(
        read::account(&env.svm, &PYTH_SOL_USD_ACCOUNT).unwrap().data,
        DUMP
    );
}

#[test]
fn anyone_cranks_and_a_newer_update_replaces_the_cache() {
    let mut env = env();
    let u = seed(&mut env);
    // No signer: the harness fee payer is a third party.
    let crank_ix = ix::crank_sol_usd(&PYTH_SOL_USD_ACCOUNT);
    assert!(crank_ix.accounts.iter().all(|m| !m.is_signer));
    let next = later(&u, 55, u.price + 60_000_000); // +$0.60 after a 55 s heartbeat
    put_update(&mut env, &next);
    env.warp(55);
    let slot = env.clock().slot;
    ok(env.send(&[crank_ix], &[]));
    let c = cache(&env);
    assert_eq!(
        (c.price, c.publish_time, c.updated_slot),
        (next.price, next.publish_time, slot)
    );
}

// ---- owner and address --------------------------------------------------------------------

#[test]
fn wrong_owner_is_refused() {
    let mut env = env();
    for owner in [
        Pubkey::new_unique(),
        anchor_lang::system_program::ID,
        shortwin::ID,
    ] {
        put(&mut env, PYTH_SOL_USD_ACCOUNT, owner, DUMP.to_vec());
        assert_err(&crank(&mut env), ShortwinError::OracleOwner);
    }
    // No account at all.
    env.svm
        .set_account(PYTH_SOL_USD_ACCOUNT, Account::default())
        .unwrap();
    assert_err(&crank(&mut env), ShortwinError::OracleOwner);
    assert_eq!(cache(&env).publish_time, 0);
}

#[test]
fn another_receiver_account_is_refused() {
    let mut env = env();
    // Shard 1 of the same feed, and an arbitrary receiver account with the same bytes.
    let shard1: Pubkey = "6bWEn5B8eJCRAek5acd3R7d4Sx3e7JWvt84srqqfYgt"
        .parse()
        .unwrap();
    for key in [shard1, Pubkey::new_unique()] {
        put(&mut env, key, PYTH_RECEIVER_ID, DUMP.to_vec());
        let res = env.send(&[ix::crank_sol_usd(&key)], &[]);
        assert_err(&res, ShortwinError::OracleFeed);
    }
}

// ---- contents -------------------------------------------------------------------------------

#[test]
fn partial_verification_is_refused() {
    let mut env = env();
    for num_signatures in [0, 5, 13, 19] {
        let u = PriceUpdate {
            verification: VerificationLevel::Partial { num_signatures },
            ..dump()
        };
        put_update(&mut env, &u);
        assert_err(&crank(&mut env), ShortwinError::OracleVerification);
    }
    // An unknown verification tag is not Full either.
    let mut data = DUMP.to_vec();
    data[pyth::VERIFICATION_OFFSET] = 2;
    put(&mut env, PYTH_SOL_USD_ACCOUNT, PYTH_RECEIVER_ID, data);
    assert_err(&crank(&mut env), ShortwinError::OracleVerification);
    assert_eq!(cache(&env).publish_time, 0);
}

#[test]
fn a_wrong_feed_is_refused() {
    let mut env = env();
    let u = PriceUpdate {
        feed_id: BTC_USD,
        ..dump()
    };
    put_update(&mut env, &u);
    assert_err(&crank(&mut env), ShortwinError::OracleFeed);
    let mut one_bit = dump();
    one_bit.feed_id[31] ^= 1;
    put_update(&mut env, &one_bit);
    assert_err(&crank(&mut env), ShortwinError::OracleFeed);
}

#[test]
fn a_stale_update_is_refused() {
    let mut env = env();
    let u = dump();
    // A copy published 121 s before the clock.
    let old = PriceUpdate {
        publish_time: DUMP_TS - 121,
        ..u
    };
    put_update(&mut env, &old);
    assert_err(&crank(&mut env), ShortwinError::OracleStale);

    // The dump itself: accepted at exactly oracle_max_age_s (120 s), refused 1 s later.
    put_update(&mut env, &u);
    env.set_clock(u.publish_time + 121, DUMP_SLOT + 300);
    assert_err(&crank(&mut env), ShortwinError::OracleStale);
    env.set_clock(u.publish_time + 120, DUMP_SLOT + 300);
    ok(crank(&mut env));
    assert_eq!(cache(&env).publish_time, u.publish_time);
}

#[test]
fn an_update_not_newer_than_the_cache_is_refused() {
    let mut env = env();
    let u = seed(&mut env);
    // The same update again (a keeper retry before the next heartbeat).
    assert_err(&crank(&mut env), ShortwinError::OracleStale);
    // An older one.
    put_update(&mut env, &later(&u, -1, u.price));
    assert_err(&crank(&mut env), ShortwinError::OracleStale);
    let c = cache(&env);
    assert_eq!((c.publish_time, c.price), (u.publish_time, u.price));
    // One second newer is enough.
    put_update(&mut env, &later(&u, 1, u.price));
    ok(crank(&mut env));
    assert_eq!(cache(&env).publish_time, u.publish_time + 1);
}

#[test]
fn a_wide_confidence_is_refused() {
    let mut env = env();
    let u = dump();
    // oracle_max_conf_bps = 200: conf·10⁴ ≤ 200·price.
    let max_conf = u64::try_from(u.price).unwrap() * 200 / 10_000;
    put_update(
        &mut env,
        &PriceUpdate {
            conf: max_conf + 1,
            ..u
        },
    );
    assert_err(&crank(&mut env), ShortwinError::OracleConfidence);
    put_update(
        &mut env,
        &PriceUpdate {
            conf: max_conf,
            ..u
        },
    );
    ok(crank(&mut env));
    assert_eq!(cache(&env).conf, max_conf);
}

#[test]
fn a_non_positive_price_is_refused() {
    let mut env = env();
    for price in [0, -1, -12_179_606_511] {
        put_update(
            &mut env,
            &PriceUpdate {
                price,
                conf: 0,
                ..dump()
            },
        );
        assert_err(&crank(&mut env), ShortwinError::OracleConfidence);
    }
}

#[test]
fn a_large_step_against_a_fresh_cache_is_refused() {
    let mut env = env();
    let u = seed(&mut env);
    env.warp(55);
    // oracle_max_step_bps = 2,000: |p − c|·10⁴ ≤ 2,000·c.
    let c = u.price;
    let up = c + c / 5; // c is not a multiple of 5: ⌊c/5⌋ is just inside
    put_update(&mut env, &later(&u, 55, up + 1));
    assert_err(&crank(&mut env), ShortwinError::OracleStep);
    let down = c - c / 5;
    put_update(&mut env, &later(&u, 55, down - 1));
    assert_err(&crank(&mut env), ShortwinError::OracleStep);
    put_update(&mut env, &later(&u, 55, up));
    ok(crank(&mut env));
    assert_eq!(cache(&env).price, up);
}

#[test]
fn the_step_is_not_bounded_by_a_stale_cache() {
    let mut env = env();
    let u = seed(&mut env);
    // oracle_stale_s = 600 (the default since the owner's decision of 2026-10-01, OWNER-M7-15;
    // was 3,600): at exactly 600 s the cache still bounds the step …
    let stale = i64::from(env.config().params.oracle_stale_s);
    assert_eq!(stale, 600);
    let jump = later(&u, stale, u.price * 2);
    put_update(&mut env, &jump);
    env.set_clock(u.publish_time + stale, DUMP_SLOT + 9_000);
    assert_err(&crank(&mut env), ShortwinError::OracleStep);
    // … one second later it does not.
    let jump = later(&u, stale + 1, u.price * 2);
    put_update(&mut env, &jump);
    env.set_clock(u.publish_time + stale + 1, DUMP_SLOT + 9_003);
    ok(crank(&mut env));
    assert_eq!(cache(&env).price, u.price * 2);
}

#[test]
fn a_new_exponent_is_compared_at_a_common_scale() {
    let mut env = env();
    let u = seed(&mut env);
    // The same USD price at exponent −9, then at −6 (+0.6 bps after rounding the mantissa).
    let e9 = PriceUpdate {
        exponent: -9,
        conf: u.conf * 10,
        ..later(&u, 1, u.price * 10)
    };
    put_update(&mut env, &e9);
    ok(crank(&mut env));
    let c = cache(&env);
    assert_eq!((c.price, c.expo), (u.price * 10, -9));
    let e6 = PriceUpdate {
        exponent: -6,
        conf: u.conf / 100,
        ..later(&u, 2, 121_796_066)
    };
    put_update(&mut env, &e6);
    ok(crank(&mut env));
    assert_eq!((cache(&env).price, cache(&env).expo), (121_796_066, -6));
    // The step is measured in USD, not in mantissa units: a ×1,000 mantissa at exponent −9
    // is the same price (accepted); the same mantissa at exponent −6 is ×1,000 (refused).
    let jump = PriceUpdate {
        exponent: -9,
        conf: u.conf * 10,
        ..later(&u, 3, 121_796_066_000)
    };
    put_update(&mut env, &jump);
    ok(crank(&mut env));
    let huge = PriceUpdate {
        exponent: -6,
        ..later(&u, 4, 121_796_066_000)
    };
    put_update(&mut env, &huge);
    assert_err(&crank(&mut env), ShortwinError::OracleStep);
    // An exponent outside [−18, 0] is refused before any arithmetic (M1-SEC-8: `OracleFeed`;
    // the rescale's u128 fail-closed path is `pyth::common_scale`'s unit test, unreachable
    // on-chain inside that range).
    for expo in [40, 1, -19] {
        let absurd = PriceUpdate {
            exponent: expo,
            conf: 0,
            ..later(&u, 5, 1)
        };
        put_update(&mut env, &absurd);
        assert_err(&crank(&mut env), ShortwinError::OracleFeed);
    }
    assert_eq!((cache(&env).price, cache(&env).expo), (121_796_066_000, -9));
}

#[test]
fn an_account_that_is_not_a_price_update_is_refused() {
    let mut env = env();
    let mut bad_disc = DUMP.to_vec();
    bad_disc[0] ^= 0xff;
    let cases: [Vec<u8>; 4] = [
        bad_disc,
        DUMP[..100].to_vec(),
        DUMP[..132].to_vec(),
        Vec::new(),
    ];
    for data in cases {
        put(&mut env, PYTH_SOL_USD_ACCOUNT, PYTH_RECEIVER_ID, data);
        assert_err(&crank(&mut env), ShortwinError::OracleFeed);
    }
    // 133 bytes are a complete Full update.
    put(
        &mut env,
        PYTH_SOL_USD_ACCOUNT,
        PYTH_RECEIVER_ID,
        DUMP[..133].to_vec(),
    );
    ok(crank(&mut env));
}

// ---- configuration ------------------------------------------------------------------------

fn propose_and_apply(env: &mut Env, change: ConfigChange) {
    let gov = env.governance.insecure_clone();
    ok(env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov]));
    env.warp(i64::from(env.config().timelock_s));
    let apply = env.apply_config_ix();
    ok(env.send(&[apply], &[]));
}

/// The oracle limits are read from `GlobalConfig.params` on every crank.
#[test]
fn the_limits_follow_governance() {
    let mut env = env();
    let mut ps = env.config().param_set().unwrap();
    ps.params.oracle_max_age_s = 600;
    ps.params.oracle_max_conf_bps = 10;
    propose_and_apply(
        &mut env,
        ConfigChange::Params {
            params: Params::from_math(&ps.params),
            tiers: ps.tiers.map(|t| TierParams::from_math(&t)),
        },
    );
    let now = env.now();
    let u = dump();
    // 300 s old: refused at 120 s, accepted at 600 s. conf 1.15 bps ≤ 10 bps.
    let aged = PriceUpdate {
        publish_time: now - 300,
        ..u
    };
    put_update(&mut env, &aged);
    ok(crank(&mut env));
    // 11 bps of confidence is now too wide.
    let wide = PriceUpdate {
        publish_time: now - 299,
        conf: u64::try_from(u.price).unwrap() * 11 / 10_000,
        ..u
    };
    put_update(&mut env, &wide);
    assert_err(&crank(&mut env), ShortwinError::OracleConfidence);
}

/// `OracleFeed{account, feed_id}` moves the crank to another account and feed id.
#[test]
fn the_feed_follows_governance() {
    let mut env = env();
    let other = Pubkey::new_unique();
    propose_and_apply(
        &mut env,
        ConfigChange::OracleFeed {
            account: other,
            feed_id: BTC_USD,
        },
    );
    let now = env.now();
    let btc = PriceUpdate {
        feed_id: BTC_USD,
        publish_time: now - 10,
        ..dump()
    };
    put(
        &mut env,
        other,
        PYTH_RECEIVER_ID,
        pyth::encode(&btc).to_vec(),
    );
    // The old address and the old feed id are refused …
    let sol = PriceUpdate {
        publish_time: now - 10,
        ..dump()
    };
    put_update(&mut env, &sol);
    assert_err(&crank(&mut env), ShortwinError::OracleFeed);
    put(
        &mut env,
        other,
        PYTH_RECEIVER_ID,
        pyth::encode(&sol).to_vec(),
    );
    let res = env.send(&[ix::crank_sol_usd(&other)], &[]);
    assert_err(&res, ShortwinError::OracleFeed);
    // … the new pair is accepted.
    put(
        &mut env,
        other,
        PYTH_RECEIVER_ID,
        pyth::encode(&btc).to_vec(),
    );
    ok(env.send(&[ix::crank_sol_usd(&other)], &[]));
    assert_eq!(cache(&env).publish_time, now - 10);
}
