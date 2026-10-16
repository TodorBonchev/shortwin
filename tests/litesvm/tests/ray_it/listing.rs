//! `create_market(venue_hint = RaydiumV4 | RaydiumCpmm)` (ARCH §2.5, §2.6, §3.2; D1, D2, O13):
//! the exact state of a Raydium listing on every eligible dumped pool, the pool's age, every
//! verification and eligibility refusal, and D1's precedence of the pump venues.

use shortwin::constants::{
    RAYDIUM_CPMM_PROGRAM_ID, T_CLAMP, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_2022_MINT,
    WSOL_MINT,
};
use shortwin::events::MarketCreated;
use shortwin::state::{
    MARKET_FLAG_GRADUATED_ELSEWHERE, MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN, raydium_cache,
};
use shortwin_litesvm_tests::raydium::{
    CurveSpec, create_ray_market_ix, curve_bytes, fartcoin_curve_bytes, locked_depth, lp_counter,
    lp_mint, plant_canonical_pool, plant_curve, set_mint_supply, supply_for_lambda,
};
use shortwin_math::fees::Direction;
use shortwin_math::num::WAD;
use shortwin_math::tier::Tier;
use shortwin_venue_state::{raydium_cpmm, raydium_v4};
use solana_account::Account;

use crate::common::*;

/// A fork with `fixture` loaded (not listed).
fn fork_with(fixture: RayFixture) -> (Env, AnyPool) {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(fixture);
    (env, pool)
}

/// Lists `pool` with `fixture`'s hint.
fn list(env: &mut Env, pool: &AnyPool, fixture: RayFixture) -> TxResult {
    env.list_ray(pool, fixture.hint())
}

fn sol_first(pool: &AnyPool) -> bool {
    match pool {
        AnyPool::V4(p) => p.sol_is_coin,
        AnyPool::Cpmm(p) => p.sol_is_token0,
    }
}

/// ARCH §3.2 "Effects" for a Raydium pool, field by field, against the fixture's own keys and
/// the readers' state: venue, pool cache, SOL side, locked depth, age (O13), listing window,
/// price state, tier placeholder, event.
fn assert_listed(env: &Env, pool: &AnyPool, fixture: RayFixture, meta: &TransactionMetadata) {
    let (mint, program) = pool.token();
    let market = pda::market(&mint).0;
    let m = env.market(&market);
    let now = env.now();
    assert_eq!(m.venue_kind(), Some(fixture.kind()));
    assert_eq!(m.token_program, program.id());
    assert_eq!(m.created_at, now);
    let c = |i: usize| m.pool_cache[i];
    let (vault_sol, vault_tok, amm_config, observation) = match pool {
        AnyPool::V4(p) => {
            let (s, t) = if p.sol_is_coin {
                (p.coin_vault, p.pc_vault)
            } else {
                (p.pc_vault, p.coin_vault)
            };
            (s, t, Pubkey::default(), Pubkey::default())
        }
        AnyPool::Cpmm(p) => {
            let (s, t) = if p.sol_is_token0 {
                (p.token_0_vault, p.token_1_vault)
            } else {
                (p.token_1_vault, p.token_0_vault)
            };
            (s, t, p.config_key(&env.svm), p.observation)
        }
    };
    let pool_key = match pool {
        AnyPool::V4(p) => p.pool,
        AnyPool::Cpmm(p) => p.pool,
    };
    assert_eq!(c(raydium_cache::RAY_POOL), pool_key);
    assert_eq!(c(raydium_cache::RAY_VAULT_SOL), vault_sol);
    assert_eq!(c(raydium_cache::RAY_VAULT_TOK), vault_tok);
    assert_eq!(c(raydium_cache::RAY_AMM_CONFIG), amm_config);
    assert_eq!(c(raydium_cache::RAY_OBSERVATION), observation);
    assert_eq!(c(raydium_cache::RAY_LP_MINT), lp_mint(pool));
    assert_eq!(c(raydium_cache::RAY_TOK_PROGRAM), program.id());
    for i in 7..10 {
        assert_eq!(c(i), Pubkey::default(), "unused cache slot {i}");
    }
    assert_eq!(
        m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
        sol_first(pool)
    );
    assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_eq!(m.pool_d.get(), locked_depth(&env.svm, pool), "pool_d = D");
    // O13: AMM v4 from listing; CPMM min(now, open_time) from T_CLAMP on, else from listing.
    let age_start = match pool {
        AnyPool::V4(_) => now,
        AnyPool::Cpmm(p) => {
            let open = i64::try_from(p.state(&env.svm).open_time).unwrap();
            if open < T_CLAMP { now } else { open.min(now) }
        }
    };
    assert_eq!(m.graduated_at, age_start);
    assert_eq!(m.pool_seen_at, now, "opens wait pool_list_window_s");
    assert_eq!(
        m.tier,
        Tier::C as u8,
        "C until the first tiering with an oracle"
    );
    // Price state: P_ema = P_spot, L_ema = √(X·Y), L_slow = min(√(X·Y), D) (AIR-13), one ring
    // entry, empty bucket.
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let p_spot = u128::from(y) * WAD / u128::from(x);
    let l = (u128::from(x) * u128::from(y)).isqrt();
    assert_eq!(m.ema_price.get(), p_spot);
    assert_eq!(
        (m.ema_liq.get(), m.liq_slow.get()),
        (l, l.min(m.pool_d.get()))
    );
    assert_eq!((m.ring_count, m.ema_ts, m.slow_ts), (1, now, now));
    assert_eq!((m.b_lo.get(), m.b_sec), (0, 0));
    let ev: MarketCreated = events::one(meta);
    assert_eq!(
        (ev.venue, ev.ray_pool, ev.mint, ev.tier),
        (fixture.kind() as u8, pool_key, mint, Tier::C as u8)
    );
}

#[test]
fn lists_every_eligible_pool_exactly() {
    for fixture in ELIGIBLE {
        let (mut env, pool) = fork_with(fixture);
        let meta = ok(list(&mut env, &pool, fixture));
        assert_listed(&env, &pool, fixture, &meta);
    }
}

/// `WSOL_T1` (`3Cv9uPpa…`: WSOL is token 1, config 10, creator fee on WSOL) has only the 100 LP
/// locked at creation (λ = 0, so its locked depth `D` is 0): under the eligibility floor
/// (`PoolTooThin`, M4-SEC-1 / -3); once its LP is burned (the scenario builder lowers the LP
/// mint's supply) it lists with WSOL on the token-1 side.
#[test]
fn wsol_as_token_1_after_an_lp_burn() {
    let (mut env, pool) = fork_with(WSOL_T1);
    assert_err(&list(&mut env, &pool, WSOL_T1), E::PoolTooThin);
    set_mint_supply(&mut env.svm, &lp_mint(&pool), 0);
    let meta = ok(list(&mut env, &pool, WSOL_T1));
    assert_listed(&env, &pool, WSOL_T1, &meta);
    assert!(!sol_first(&pool));
}

/// O13 on the tier (ARCH §4.7): with a fresh oracle, Fartcoin's AMM v4 market (age from
/// listing; `Λ ≈ 2·38,257 SOL`, far above θ_A) is Tier C for its first 24 h, B until day 7 and A
/// afterwards; USELESS on CPMM (`open_time` 2025-05-10) is A at once; a CPMM pool whose
/// `open_time` precedes `T_CLAMP` proves no creation time and ages from its listing (SPEC §4
/// O13), like AMM v4; one whose `open_time` is `T_CLAMP` exactly ages from `T_CLAMP`.
#[test]
fn pool_age_follows_o13() {
    let (mut env, rm, _) = setup(FART);
    let listed = env.market(&rm.market()).graduated_at;
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(env.market(&rm.market()).tier, Tier::C as u8, "age < 24 h");
    env.set_clock(listed + 86_400 + 5, env.clock().slot + 216_000);
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(
        env.market(&rm.market()).tier,
        Tier::B as u8,
        "1 d ≤ age < 7 d"
    );
    env.set_clock(listed + 604_800 + 5, env.clock().slot + 1_296_000);
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(env.market(&rm.market()).tier, Tier::A as u8, "age ≥ 7 d");

    let (mut env, rm, _) = setup(USELESS);
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(
        env.market(&rm.market()).graduated_at,
        1_746_886_997,
        "the pool's open_time"
    );
    assert_eq!(env.market(&rm.market()).tier, Tier::A as u8);

    let (mut env, pool) = fork_with(USELESS);
    let p = match &pool {
        AnyPool::Cpmm(p) => *p,
        AnyPool::V4(_) => unreachable!(),
    };
    fork::set_cpmm_open_time(&mut env.svm, &p, 1_600_000_000);
    ok(list(&mut env, &pool, USELESS));
    assert_eq!(
        env.market(&pda::market(&p.token_mint()).0).graduated_at,
        env.now(),
        "a pre-T_CLAMP open_time: age from the listing (O13)"
    );
    let (mut env, pool) = fork_with(USELESS);
    fork::set_cpmm_open_time(&mut env.svm, &p, T_CLAMP.unsigned_abs() - 1);
    ok(list(&mut env, &pool, USELESS));
    assert_eq!(
        env.market(&pda::market(&p.token_mint()).0).graduated_at,
        env.now(),
        "one second before T_CLAMP: age from the listing"
    );
    let (mut env, pool) = fork_with(USELESS);
    fork::set_cpmm_open_time(&mut env.svm, &p, T_CLAMP.unsigned_abs());
    ok(list(&mut env, &pool, USELESS));
    assert_eq!(
        env.market(&pda::market(&p.token_mint()).0).graduated_at,
        T_CLAMP,
        "from T_CLAMP on, open_time bounds the pool's age"
    );
    // A CPMM pool opening in the future is not executable: not eligible.
    let (mut env, pool) = fork_with(USELESS);
    let now = env.now();
    fork::set_cpmm_open_time(&mut env.svm, &p, u64::try_from(now + 10).unwrap());
    assert_err(&list(&mut env, &pool, USELESS), E::PoolIneligible);
}

// ---- verification (ARCH §2.6) ------------------------------------------------------------------

fn v4(pool: &AnyPool) -> fork::V4Pool {
    match pool {
        AnyPool::V4(p) => *p,
        AnyPool::Cpmm(_) => panic!("AMM v4 expected"),
    }
}

fn cpmm(pool: &AnyPool) -> fork::CpmmPool {
    match pool {
        AnyPool::Cpmm(p) => *p,
        AnyPool::V4(_) => panic!("CPMM expected"),
    }
}

/// Runs `mutate` on a fresh fork of `fixture` and lists it: the result.
fn list_after(fixture: RayFixture, mutate: impl FnOnce(&mut Env, &AnyPool)) -> TxResult {
    let (mut env, pool) = fork_with(fixture);
    mutate(&mut env, &pool);
    list(&mut env, &pool, fixture)
}

/// Replaces an account's owner (data, lamports kept).
fn set_owner(env: &mut Env, key: &Pubkey, owner: Pubkey) {
    let mut acc = env.svm.get_account(key).unwrap();
    acc.owner = owner;
    env.svm.set_account(*key, acc).unwrap();
}

fn put_key(env: &mut Env, account: &Pubkey, offset: usize, key: &Pubkey) {
    fork::patch(&mut env.svm, account, |d| {
        d[offset..offset + 32].copy_from_slice(key.as_ref())
    });
}

#[test]
fn amm_v4_verification_refusals() {
    // Not an AMM v4 account: another owner, another length.
    let r = list_after(FART, |env, p| set_owner(env, &v4(p).pool, TOKEN_PROGRAM_ID));
    assert_err(&r, E::PoolIneligible);
    let r = list_after(FART, |env, p| {
        fork::patch(&mut env.svm, &v4(p).pool, |d| d.push(0))
    });
    assert_err(&r, E::PoolIneligible);
    // Status outside {1, 6, 7}; status 7 before the open time (not executable).
    for status in [0, 2, 3, 4, 5, 8] {
        let r = list_after(FART, |env, p| {
            fork::set_v4_status(&mut env.svm, &v4(p), status)
        });
        assert_err(&r, E::PoolIneligible);
    }
    let r = list_after(FART, |env, p| {
        let now = env.now();
        fork::set_v4_status(&mut env.svm, &v4(p), 7);
        fork::set_v4_open_time(&mut env.svm, &v4(p), u64::try_from(now + 60).unwrap());
    });
    assert_err(&r, E::PoolIneligible);
    // Status 7 past its open time and status 1 are executable: eligible.
    ok(list_after(FART, |env, p| {
        let now = env.now();
        fork::set_v4_status(&mut env.svm, &v4(p), 7);
        fork::set_v4_open_time(&mut env.svm, &v4(p), u64::try_from(now).unwrap());
    }));
    ok(list_after(FART, |env, p| {
        fork::set_v4_status(&mut env.svm, &v4(p), 1)
    }));
    // Another authority nonce.
    let r = list_after(FART, |env, p| {
        fork::patch(&mut env.svm, &v4(p).pool, |d| {
            d[raydium_v4::OFF_NONCE..raydium_v4::OFF_NONCE + 8]
                .copy_from_slice(&253u64.to_le_bytes())
        })
    });
    assert_err(&r, E::PoolIneligible);
    // Not a WSOL pool: both sides WSOL, or the WSOL side replaced by another mint.
    let r = list_after(FART, |env, p| {
        put_key(env, &v4(p).pool, raydium_v4::OFF_PC_VAULT_MINT, &WSOL_MINT)
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(FART, |env, p| {
        put_key(
            env,
            &v4(p).pool,
            raydium_v4::OFF_COIN_VAULT_MINT,
            &Pubkey::new_unique(),
        )
    });
    assert_err(&r, E::PoolIneligible);
    // Vaults that are not the authority's accounts of the pool's mints.
    let r = list_after(FART, |env, p| {
        put_key(env, &v4(p).coin_vault, 32, &Pubkey::new_unique())
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(FART, |env, p| put_key(env, &v4(p).pc_vault, 0, &WSOL_MINT));
    assert_err(&r, E::PoolIneligible);
    let r = list_after(FART, |env, p| {
        set_owner(env, &v4(p).coin_vault, TOKEN_2022_PROGRAM_ID)
    });
    assert_err(&r, E::PoolIneligible);
    // An LP mint that does not parse.
    let r = list_after(FART, |env, p| {
        set_owner(env, &v4(p).lp_mint, TOKEN_2022_PROGRAM_ID)
    });
    assert_err(&r, E::PoolIneligible);
    // A fee fraction the venue cannot swap with, and one above venue_fee_cap_bps (2 % > 1.5 %).
    let r = list_after(FART, |env, p| {
        fork::set_v4_swap_fee(&mut env.svm, &v4(p), 10_000, 10_000)
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(FART, |env, p| {
        fork::set_v4_swap_fee(&mut env.svm, &v4(p), 151, 10_000)
    });
    assert_err(&r, E::PoolIneligible);
    ok(list_after(FART, |env, p| {
        fork::set_v4_swap_fee(&mut env.svm, &v4(p), 150, 10_000)
    }));
    // A vault below its need_take_pnl: not executable.
    let r = list_after(FART, |env, p| {
        let (c, _) = v4(p).vault_amounts(&env.svm);
        fork::set_v4_need_take_pnl(&mut env.svm, &v4(p), c + 1, 0)
    });
    assert_err(&r, E::PoolIneligible);
}

/// The eligibility floor on locked depth at a listing (M4-SEC-1 / M4-SEC-3; ARCH §2.6 as
/// amended): `⌊⌊D·isqrt(P)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` at the pool's own spot.
#[test]
fn amm_v4_eligibility_refusals() {
    use shortwin_litesvm_tests::pool_select::passes_floor;
    // The reversed Fartcoin pool: 1.77 SOL of real reserve (λ 31 bps too): too thin.
    let (mut env, pool) = fork_with(FART_REV);
    assert_err(&list(&mut env, &pool, FART_REV), E::PoolTooThin);
    // λ below λ_min is no refusal by itself: Fartcoin at λ = 49.99 % lists (its locked depth
    // is far above the floor), and so it does at 5 bps (≈ 38 SOL of locked SOL over λ_min);
    // at 3 bps (≈ 23 SOL) it is under the floor.
    for (lam, want) in [(4_999u64, None), (5, None), (3, Some(E::PoolTooThin))] {
        let r = list_after(FART, |env, p| {
            let supply = supply_for_lambda(lp_counter(&env.svm, p), lam);
            set_mint_supply(&mut env.svm, &lp_mint(p), supply);
        });
        match want {
            None => {
                ok(r);
            }
            Some(e) => assert_err(&r, e),
        }
    }
    // The floor's boundary, exact: with the LP burned to λ = 50 % the locked SOL over λ_min is
    // the real SOL less the floors' rounding; the smallest reserve that passes lists, one
    // lamport less does not.
    let min = Env::ray_fork().config().params.pool_min_real_sol;
    let (mut env, pool) = fork_with(FART);
    let supply = supply_for_lambda(lp_counter(&env.svm, &pool), 5_000);
    set_mint_supply(&mut env.svm, &lp_mint(&pool), supply);
    let coin = v4(&pool).coin_vault;
    let mut sol = min;
    loop {
        fork::set_token_amount(&mut env.svm, &coin, sol);
        if passes_floor(&env, &pool, None) {
            break;
        }
        sol += 1;
    }
    assert!(
        sol - min < 1_000,
        "the boundary {sol} is the floor plus rounding"
    );
    for (sol, want) in [(sol, None), (sol - 1, Some(E::PoolTooThin))] {
        let r = list_after(FART, |env, p| {
            let supply = supply_for_lambda(lp_counter(&env.svm, p), 5_000);
            set_mint_supply(&mut env.svm, &lp_mint(p), supply);
            fork::set_token_amount(&mut env.svm, &v4(p).coin_vault, sol);
        });
        match want {
            None => {
                ok(r);
            }
            Some(e) => assert_err(&r, e),
        }
    }
}

#[test]
fn amm_v4_account_refusals() {
    let (mut env, pool) = fork_with(FART);
    let (_, rev) = fork_with(FART_REV);
    let creator = env.funded_keypair();
    let base = create_ray_market_ix(&env.svm, &creator.pubkey(), &pool, VENUE_HINT_V4);
    let n = base.accounts.len();
    // A vault or the LP mint that is not the pool's: VenueAccountMismatch.
    let rev = v4(&rev);
    for (slot, key) in [
        (n - 3, rev.coin_vault),
        (n - 2, rev.pc_vault),
        (n - 1, rev.lp_mint),
    ] {
        let mut ix = base.clone();
        ix.accounts[slot].pubkey = key;
        assert_err(&env.send(&[ix], &[&creator]), E::VenueAccountMismatch);
    }
    // The ReadSet swapped (pc before coin).
    let mut ix = base.clone();
    ix.accounts.swap(n - 3, n - 2);
    assert_err(&env.send(&[ix], &[&creator]), E::VenueAccountMismatch);
    // Omitted or appended accounts.
    let mut ix = base.clone();
    ix.accounts.pop();
    assert_err(&env.send(&[ix], &[&creator]), E::BadRemainingAccounts);
    let mut ix = base.clone();
    ix.accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    assert_err(&env.send(&[ix], &[&creator]), E::BadRemainingAccounts);
    // The CPMM hint with an AMM v4 ReadSet, the AMM v4 hint with a CPMM ReadSet.
    let ix = create_ray_market_ix(&env.svm, &creator.pubkey(), &pool, VENUE_HINT_CPMM);
    assert_err(&env.send(&[ix], &[&creator]), E::BadRemainingAccounts);
    // An unknown hint.
    let mut ix = base.clone();
    ix.data[8] = 7;
    assert_err(&env.send(&[ix], &[&creator]), E::NoSupportedVenue);
    // The AMM v4 pool listed for another mint (USELESS's): not a pool of that mint.
    let useless = env.load_ray_pool(USELESS);
    let (other_mint, _) = useless.token();
    let mut ix = shortwin_litesvm_tests::ix::create_market(
        &creator.pubkey(),
        &other_mint,
        &TOKEN_PROGRAM_ID,
        VENUE_HINT_V4,
        &pda::canonical_pool(&other_mint),
    );
    ix.accounts
        .extend(raydium::candidate_read_set(&env.svm, &pool, false));
    assert_err(&env.send(&[ix], &[&creator]), E::PoolIneligible);
    // Finally the genuine listing.
    ok(env.send(&[base], &[&creator]));
}

/// AMM v4 is SPL Token only: a pool whose token side names a Token-2022 mint is refused.
#[test]
fn amm_v4_refuses_a_token_2022_market() {
    let (mut env, pool) = fork_with(FART);
    let t22 = env.load_ray_pool(T22);
    let (t22_mint, program) = t22.token();
    assert_eq!(program, TokenProgram::Token2022);
    let p = v4(&pool);
    put_key(&mut env, &p.pool, raydium_v4::OFF_PC_VAULT_MINT, &t22_mint);
    let creator = env.funded_keypair();
    let mut ix = shortwin_litesvm_tests::ix::create_market(
        &creator.pubkey(),
        &t22_mint,
        &TOKEN_2022_PROGRAM_ID,
        VENUE_HINT_V4,
        &pda::canonical_pool(&t22_mint),
    );
    ix.accounts
        .extend(raydium::candidate_read_set(&env.svm, &pool, false));
    assert_err(&env.send(&[ix], &[&creator]), E::PoolIneligible);
}

#[test]
fn cpmm_verification_refusals() {
    // Not a CPMM PoolState: owner, length (637 exactly), discriminator.
    let r = list_after(USELESS, |env, p| {
        set_owner(env, &cpmm(p).pool, TOKEN_PROGRAM_ID)
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(USELESS, |env, p| {
        fork::patch(&mut env.svm, &cpmm(p).pool, |d| d.push(0))
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(USELESS, |env, p| {
        fork::patch(&mut env.svm, &cpmm(p).pool, |d| d[0] ^= 1)
    });
    assert_err(&r, E::PoolIneligible);
    // A config account that is not a CPMM AmmConfig.
    let r = list_after(USELESS, |env, p| {
        let key = cpmm(p).config_key(&env.svm);
        set_owner(env, &key, TOKEN_PROGRAM_ID)
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(USELESS, |env, p| {
        let key = cpmm(p).config_key(&env.svm);
        fork::patch(&mut env.svm, &key, |d| d[0] ^= 1)
    });
    assert_err(&r, E::PoolIneligible);
    // Swaps disabled (status bit 2); deposit / withdraw bits alone are harmless.
    let r = list_after(USELESS, |env, p| {
        fork::set_cpmm_status(&mut env.svm, &cpmm(p), 4)
    });
    assert_err(&r, E::PoolIneligible);
    ok(list_after(USELESS, |env, p| {
        fork::set_cpmm_status(&mut env.svm, &cpmm(p), 3)
    }));
    // The WSOL side under Token-2022, the Token-2022 native mint as the quote.
    let r = list_after(USELESS, |env, p| {
        put_key(
            env,
            &cpmm(p).pool,
            raydium_cpmm::OFF_TOKEN_0_PROGRAM,
            &TOKEN_2022_PROGRAM_ID,
        )
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(USELESS, |env, p| {
        put_key(
            env,
            &cpmm(p).pool,
            raydium_cpmm::OFF_TOKEN_0_MINT,
            &WSOL_2022_MINT,
        )
    });
    assert_err(&r, E::PoolIneligible);
    // The token side's program is not the mint's (the Token-2022 pool naming Tokenkeg).
    let r = list_after(T22, |env, p| {
        put_key(
            env,
            &cpmm(p).pool,
            raydium_cpmm::OFF_TOKEN_1_PROGRAM,
            &TOKEN_PROGRAM_ID,
        )
    });
    assert_err(&r, E::PoolIneligible);
    // Vaults that are not the authority's accounts of the pool's mints and programs.
    let r = list_after(USELESS, |env, p| {
        put_key(env, &cpmm(p).token_1_vault, 32, &Pubkey::new_unique())
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(T22, |env, p| {
        set_owner(env, &cpmm(p).token_1_vault, TOKEN_PROGRAM_ID)
    });
    assert_err(&r, E::PoolIneligible);
    // creator_fee_on outside {0, 1, 2}; a counter above its vault: not executable.
    let r = list_after(USELESS, |env, p| {
        fork::set_cpmm_creator_fee(&mut env.svm, &cpmm(p), false, 3)
    });
    assert_err(&r, E::PoolIneligible);
    let r = list_after(USELESS, |env, p| {
        let (v0, _) = cpmm(p).vault_amounts(&env.svm);
        fork::set_cpmm_counters(&mut env.svm, &cpmm(p), (v0 + 1, 0, 0), (0, 0, 0))
    });
    assert_err(&r, E::PoolIneligible);
}

/// The fee cap `φ = t + c ≤ venue_fee_cap_bps·100` (150 bps), exactly, on the live configs:
/// config 6 (1.5 % trade, 0.05 % creator) passes with the creator fee off and fails with it on;
/// config 20 (0.25 % + 1.25 %) and 16 (0.005 % + 1.495 %) sit exactly at the cap with it on;
/// config 3 (4 %) fails.
#[test]
fn cpmm_fee_cap_on_the_live_configs() {
    let with = |index: u16, creator: bool| {
        list_after(USELESS, |env, p| {
            let p = cpmm(p);
            fork::set_cpmm_pool_config(&mut env.svm, &p, &fork::cpmm_config(index));
            fork::set_cpmm_creator_fee(&mut env.svm, &p, creator, 0);
        })
    };
    ok(with(6, false));
    assert_err(&with(6, true), E::PoolIneligible);
    ok(with(20, true));
    ok(with(16, true));
    ok(with(12, true));
    assert_err(&with(3, false), E::PoolIneligible);
    assert_err(&with(7, false), E::PoolIneligible);
}

#[test]
fn cpmm_account_refusals() {
    let (mut env, pool) = fork_with(USELESS);
    let (_, other) = fork_with(CREATOR_FEE);
    let other = cpmm(&other);
    let creator = env.funded_keypair();
    let base = create_ray_market_ix(&env.svm, &creator.pubkey(), &pool, VENUE_HINT_CPMM);
    let n = base.accounts.len();
    // The config, a vault or the LP mint that is not the pool's.
    for (slot, key) in [
        (n - 4, fork::cpmm_config(1)),
        (n - 3, other.token_0_vault),
        (n - 2, other.token_1_vault),
        (n - 1, other.lp_mint),
    ] {
        let mut ix = base.clone();
        ix.accounts[slot].pubkey = key;
        assert_err(&env.send(&[ix], &[&creator]), E::VenueAccountMismatch);
    }
    let mut ix = base.clone();
    ix.accounts.swap(n - 3, n - 2);
    assert_err(&env.send(&[ix], &[&creator]), E::VenueAccountMismatch);
    let mut ix = base.clone();
    ix.accounts.pop();
    assert_err(&env.send(&[ix], &[&creator]), E::BadRemainingAccounts);
    let ix = create_ray_market_ix(&env.svm, &creator.pubkey(), &pool, VENUE_HINT_V4);
    assert_err(&env.send(&[ix], &[&creator]), E::BadRemainingAccounts);
    ok(env.send(&[base], &[&creator]));
}

// ---- D1: the pump venues take precedence (ARCH §2.5 classify_pump) ------------------------------

/// Plants `data` as the mint's pump curve on a fresh fork of `fixture`, then lists.
fn list_with_curve(
    fixture: RayFixture,
    data: Vec<u8>,
    canonical: Option<Pubkey>,
) -> (Env, AnyPool, TxResult) {
    let (mut env, pool) = fork_with(fixture);
    let (mint, _) = pool.token();
    plant_curve(&mut env.svm, &mint, data);
    if let Some(quote) = canonical {
        plant_canonical_pool(&mut env.svm, &mint, &quote);
    }
    let r = list(&mut env, &pool, fixture);
    (env, pool, r)
}

#[test]
fn pump_venues_take_precedence() {
    let live = CurveSpec {
        virtual_token_reserves: 800_000_000_000_000,
        virtual_quote_reserves: 40_000_000_000,
        real_token_reserves: 500_000_000_000_000,
        real_quote_reserves: 10_000_000_000,
        len: 151,
        ..CurveSpec::default()
    };
    for fixture in [FART, USELESS] {
        // A live curve.
        let (_, _, r) = list_with_curve(fixture, curve_bytes(&live), None);
        assert_err(&r, E::PumpVenuePrecedence);
        // A completed curve still migrating (vT > 0, no canonical pool yet).
        let migrating = CurveSpec {
            complete: true,
            virtual_token_reserves: 279_900_000_000_000,
            real_token_reserves: 0,
            ..live
        };
        let (_, _, r) = list_with_curve(fixture, curve_bytes(&migrating), None);
        assert_err(&r, E::PumpVenuePrecedence);
        // A completed curve with a valid canonical PumpSwap pool.
        let migrated = CurveSpec {
            complete: true,
            len: 151,
            ..CurveSpec::default()
        };
        let (_, _, r) = list_with_curve(fixture, curve_bytes(&migrated), Some(WSOL_MINT));
        assert_err(&r, E::PumpVenuePrecedence);
        // An account at the canonical address that is not a valid pool (here quoted in another
        // mint: a PumpSwap layout change could make the real pool read so) is a pool short.win
        // cannot read, not the absence of one (M5-SEC-5): D1 holds, fail closed.
        let (_, _, r) =
            list_with_curve(fixture, curve_bytes(&migrated), Some(Pubkey::new_unique()));
        assert_err(&r, E::PumpVenuePrecedence);
        // Nothing at the canonical address: the curve graduated elsewhere, the Raydium listing
        // proceeds and records it.
        let (env, pool, r) = list_with_curve(fixture, curve_bytes(&migrated), None);
        ok(r);
        let m = env.market(&pda::market(&pool.token().0).0);
        assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
        // ADOPTED Q6 and the length rule.
        let (_, _, r) = list_with_curve(
            fixture,
            curve_bytes(&CurveSpec {
                is_mayhem_mode: true,
                ..migrated
            }),
            None,
        );
        assert_err(&r, E::CurveMayhemUnsupported);
        let (_, _, r) = list_with_curve(
            fixture,
            curve_bytes(&CurveSpec {
                quote_mint: Pubkey::new_unique(),
                ..migrated
            }),
            None,
        );
        assert_err(&r, E::CurveQuoteNotSol);
        let (_, _, r) = list_with_curve(
            fixture,
            curve_bytes(&CurveSpec {
                len: 114,
                ..migrated
            }),
            None,
        );
        assert_err(&r, E::CurveNeedsExtend);
        ok(list_with_curve(
            fixture,
            curve_bytes(&CurveSpec {
                len: 115,
                ..migrated
            }),
            None,
        )
        .2);
        // A pump-owned curve account that does not parse (a `bool` byte of 2).
        let mut bad = curve_bytes(&migrated);
        bad[shortwin_venue_state::pump::OFF_COMPLETE] = 2;
        let (_, _, r) = list_with_curve(fixture, bad, None);
        assert_err(&r, E::PumpVenuePrecedence);
        // An account at the curve address that pump does not own is no curve.
        let (mut env, pool) = fork_with(fixture);
        let (mint, _) = pool.token();
        let data = curve_bytes(&live);
        let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
        env.svm
            .set_account(
                pda::bonding_curve(&mint),
                Account {
                    lamports,
                    data,
                    owner: RAYDIUM_CPMM_PROGRAM_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        ok(list(&mut env, &pool, fixture));
    }
}

/// Fartcoin's real curve (`TBHe5tJn…`, the mainnet bytes): complete with zeroed reserves and no
/// canonical pool — a coin that graduated to Raydium before PumpSwap. Its Raydium listing
/// proceeds and records `graduated_elsewhere` (ARCH §2.5; Raydium-graduated pump coins map to
/// `RaydiumV4` / `RaydiumCpmm`, not to `None`).
#[test]
fn fartcoin_graduated_elsewhere_maps_to_its_raydium_pool() {
    let data = fartcoin_curve_bytes();
    assert_eq!(data.len(), 150);
    let (env, pool, r) = list_with_curve(FART, data, None);
    let meta = ok(r);
    let m = env.market(&pda::market(&pool.token().0).0);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumV4));
    assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    let ev: MarketCreated = events::one(&meta);
    assert_eq!(ev.venue, VenueKind::RaydiumV4 as u8);
    // The same mint listed `Auto` stays list-only (pump detection for `Auto` is M5's).
    let (mut env, pool) = fork_with(FART);
    let (mint, _) = pool.token();
    plant_curve(&mut env.svm, &mint, fartcoin_curve_bytes());
    let creator = env.funded_keypair();
    let ix = shortwin_litesvm_tests::ix::create_market(
        &creator.pubkey(),
        &mint,
        &TOKEN_PROGRAM_ID,
        0,
        &pda::canonical_pool(&mint),
    );
    ok(env.send(&[ix], &[&creator]));
    assert_eq!(
        env.market(&pda::market(&mint).0).venue_kind(),
        Some(VenueKind::None)
    );
}
