//! `create_market` and `flag_unsafe_mint` (ARCH §3.2, §3.9; M1 exit criteria "one filter
//! fixture per extension" lives in `mint_filter_extensions.rs`).
//!
//! - listing: the exact state of a list-only market (equal to what
//!   `fixtures::install_market` writes), the pool vault, the creation fee (ADOPTED Q24), the
//!   `MarketCreated` event, the creator's cost;
//! - uniqueness and griefing: one market per mint, front-running, pre-funded PDAs, a
//!   pre-created trader ATA, forged or missing detection keys (C0-15), constraint violations;
//!   spoofed owners at the detection PDAs (pre-funded, a curve's bytes under another owner: M10
//!   G-A3);
//! - `flag_unsafe_mint`: `MintSafe` on a safe mint (also after the changes Token-2022 allows),
//!   the flag and `MintSafetyTripped` after an authority / extension / owner change that only
//!   a Token-2022 upgrade or a bug could make, and the one-way flag.
//!
//! Requires `target/deploy/shortwin.so` (`anchor build --tools-version v1.57 --arch v3`) or
//! `SHORTWIN_SO`.

#![allow(clippy::result_large_err)]

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use shortwin::errors::ShortwinError;
use shortwin::events::{MarketCreated, MintSafetyTripped};
use shortwin::instructions::create_market::VENUE_HINT_AUTO;
use shortwin::state::{
    GlobalConfig, MARKET_FLAG_MINT_UNSAFE, Market, MarketVault, POOL_VAULT_SEED, VenueKind,
};
use shortwin_litesvm_tests::errors::{assert_anchor_err, assert_err, assert_failed, ok};
use shortwin_litesvm_tests::fixtures::{self, mints};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda, read};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_signer::Signer;
use spl_token_2022_interface::extension::ExtensionType;
use spl_token_2022_interface::state::{Account as TokenAccountState, AccountState};

const DECIMALS: u8 = 6;

/// A funded creator and a fresh mint that passes the filter.
fn mint_for(env: &mut Env, program: TokenProgram) -> (Pubkey, Keypair) {
    let authority = Keypair::new();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint(&mut env.svm, &payer, &authority.pubkey(), DECIMALS, program);
    (mint, authority)
}

fn create(env: &mut Env, creator: &Keypair, mint: &Pubkey, program: &Pubkey) -> TxResult {
    let ix = ix::create_market(
        &creator.pubkey(),
        mint,
        program,
        VENUE_HINT_AUTO,
        &pda::canonical_pool(mint),
    );
    env.send(&[ix], &[creator])
}

/// `create_market` with the given remaining accounts instead of the detection set.
fn create_with_remaining(
    env: &mut Env,
    creator: &Keypair,
    mint: &Pubkey,
    program: &Pubkey,
    remaining: Vec<AccountMeta>,
) -> TxResult {
    let mut ix = ix::create_market(
        &creator.pubkey(),
        mint,
        program,
        VENUE_HINT_AUTO,
        &pda::canonical_pool(mint),
    );
    let named = ix.accounts.len() - 2;
    ix.accounts.truncate(named);
    ix.accounts.extend(remaining);
    env.send(&[ix], &[creator])
}

/// What `create_market` must leave: `fixtures::install_market` run on a scratch VM with the
/// same inputs, plus the creation fee in `protocol_fees`.
fn expected(
    config: &GlobalConfig,
    mint: &Pubkey,
    decimals: u8,
    program: TokenProgram,
    creator: &Pubkey,
    now: i64,
) -> (Market, MarketVault, Vec<u8>) {
    let mut scratch = LiteSVM::new();
    let keys =
        fixtures::install_market(&mut scratch, mint, decimals, program, creator, now, config);
    let m: Market = read::zc(&scratch, &keys.market).expect("scratch market");
    let mut v: MarketVault = read::zc(&scratch, &keys.market_vault).expect("scratch vault");
    v.protocol_fees = config.params.market_creation_fee;
    let pool = scratch
        .get_account(&keys.pool_vault)
        .expect("scratch pool vault");
    (m, v, pool.data)
}

/// Byte equality of two zero-copy values; on a difference, reports the offsets that differ
/// (offsets include the 8-byte discriminator, as in the ARCH §2.3 tables).
fn assert_bytes_eq<T: bytemuck::Pod>(got: &T, want: &T, what: &str) {
    let (g, w) = (bytemuck::bytes_of(got), bytemuck::bytes_of(want));
    let diffs: Vec<String> = g
        .iter()
        .zip(w)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(16)
        .map(|(i, (a, b))| format!("@{}: got {a} want {b}", i + 8))
        .collect();
    assert!(diffs.is_empty(), "{what} differs: {}", diffs.join(", "));
}

/// Asserts the market of `mint` is exactly the list-only market `creator` would create now,
/// and returns its key.
fn assert_listed(
    env: &Env,
    mint: &Pubkey,
    decimals: u8,
    program: TokenProgram,
    creator: &Pubkey,
) -> Pubkey {
    let (market, _) = pda::market(mint);
    let config = env.config();
    let (want_m, want_v, want_pool) =
        expected(&config, mint, decimals, program, creator, env.now());
    assert_bytes_eq(&env.market(&market), &want_m, "Market");
    assert_bytes_eq(&env.market_vault(&market), &want_v, "MarketVault");

    let pool = env
        .svm
        .get_account(&pda::pool_vault(&market).0)
        .expect("pool vault");
    assert_eq!(pool.owner, program.id());
    assert_eq!(pool.data.len(), TokenAccountState::LEN);
    assert_eq!(pool.data, want_pool, "pool vault bytes");
    let state = TokenAccountState::unpack(&pool.data).expect("token account");
    assert_eq!(state.mint, *mint);
    assert_eq!(state.owner, market);
    assert_eq!(state.amount, 0);
    assert_eq!(state.delegate, COption::None);
    assert_eq!(state.close_authority, COption::None);
    assert_eq!(state.state, AccountState::Initialized);

    let vault_lamports = read::lamports(&env.svm, &pda::market_vault(&market).0);
    let rent = env
        .svm
        .minimum_balance_for_rent_exemption(MarketVault::SPACE);
    assert!(vault_lamports >= rent + config.params.market_creation_fee);
    market
}

#[test]
fn creates_a_list_only_market_for_both_token_programs() {
    for program in TokenProgram::BOTH {
        let mut env = Env::initialized();
        let (mint, _) = mint_for(&mut env, program);
        let creator = env.funded_keypair();
        let before = read::lamports(&env.svm, &creator.pubkey());
        let meta = ok(create(&mut env, &creator, &mint, &program.id()));
        let market = assert_listed(&env, &mint, DECIMALS, program, &creator.pubkey());
        env.track_market(market);
        env.assert_invariants();

        // The creator pays the three accounts' rent and the fee, nothing else (the harness
        // payer pays the transaction fee).
        let fee = env.config().params.market_creation_fee;
        let rent = |len| env.svm.minimum_balance_for_rent_exemption(len);
        let cost = rent(Market::SPACE) + rent(MarketVault::SPACE) + rent(TokenAccountState::LEN);
        assert_eq!(
            before - read::lamports(&env.svm, &creator.pubkey()),
            cost + fee
        );
        let vault_lamports = read::lamports(&env.svm, &pda::market_vault(&market).0);
        assert_eq!(vault_lamports, rent(MarketVault::SPACE) + fee);

        // The trader PDA and its ATA are only keys on a list-only market.
        let m = env.market(&market);
        assert_eq!(read::lamports(&env.svm, &m.trader), 0);
        assert!(
            env.svm
                .get_account(&m.trader_base_ata)
                .is_none_or(|a| a.lamports == 0)
        );

        let e: MarketCreated = events::one(&meta);
        assert_eq!(e.market, market);
        assert_eq!(e.mint, mint);
        assert_eq!(e.token_program, program.id());
        assert_eq!(e.venue, VenueKind::None as u8);
        assert_eq!(e.creator, creator.pubkey());
        assert_eq!(e.bonding_curve, pda::bonding_curve(&mint));
        assert_eq!(e.canonical_pool, pda::canonical_pool(&mint));
        assert_eq!(e.ray_pool, Pubkey::default());
        assert_eq!(e.tier, shortwin_math::tier::Tier::C as u8);
        assert_eq!(e.creation_fee, fee);
        assert_eq!((e.slot, e.ts), (env.clock().slot, env.now()));
    }
}

#[test]
fn lists_a_pump_style_token_2022_mint() {
    let mut env = Env::initialized();
    let fx = mints::pump_style_mint(&mut env, 1_000_000_000_000_000);
    let creator = env.funded_keypair();
    ok(create(&mut env, &creator, &fx.mint, &mints::t22_id()));
    let market = assert_listed(
        &env,
        &fx.mint,
        6,
        TokenProgram::Token2022,
        &creator.pubkey(),
    );
    env.track_market(market);
    env.assert_invariants();
}

#[test]
fn creation_fee_follows_the_config() {
    for fee in [0u64, 1, 100_000_000] {
        let mut env = Env::new();
        let mut args = env.default_init_args();
        args.params.market_creation_fee = fee;
        ok(env.initialize_config(args));
        let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
        let creator = env.funded_keypair();
        let meta = ok(create(&mut env, &creator, &mint, &TokenProgram::Spl.id()));
        let market = assert_listed(&env, &mint, DECIMALS, TokenProgram::Spl, &creator.pubkey());
        assert_eq!(env.market_vault(&market).protocol_fees, fee);
        assert_eq!(events::one::<MarketCreated>(&meta).creation_fee, fee);
        // One inner System transfer carries the fee; none with a zero fee.
        let rent = env
            .svm
            .minimum_balance_for_rent_exemption(MarketVault::SPACE);
        assert_eq!(
            read::lamports(&env.svm, &pda::market_vault(&market).0),
            rent + fee
        );
    }
}

#[test]
fn a_creator_that_cannot_pay_the_fee_lists_nothing() {
    let mut env = Env::initialized();
    let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
    let creator = Keypair::new();
    // Rent for the three accounts but not the fee.
    let rent = |len| env.svm.minimum_balance_for_rent_exemption(len);
    let enough_for_rent = rent(Market::SPACE) + rent(MarketVault::SPACE) + rent(165) + 1;
    env.fund(&creator.pubkey(), enough_for_rent);
    assert_failed(&create(&mut env, &creator, &mint, &TokenProgram::Spl.id()));
    assert!(env.svm.get_account(&pda::market(&mint).0).is_none());
}

#[test]
fn one_market_per_mint_whatever_the_creator_or_program() {
    for program in TokenProgram::BOTH {
        let mut env = Env::initialized();
        let (mint, _) = mint_for(&mut env, program);
        let first = env.funded_keypair();
        ok(create(&mut env, &first, &mint, &program.id()));
        let market = pda::market(&mint).0;
        env.track_market(market);
        let snapshot = env.market(&market);

        let second = env.funded_keypair();
        assert_failed(&create(&mut env, &second, &mint, &program.id()));
        assert_failed(&create(&mut env, &first, &mint, &program.id()));
        let other = if program == TokenProgram::Spl {
            TokenProgram::Token2022
        } else {
            TokenProgram::Spl
        };
        assert_failed(&create(&mut env, &second, &mint, &other.id()));
        assert_bytes_eq(
            &env.market(&market),
            &snapshot,
            "Market after re-creation attempts",
        );
    }
}

/// A front-runner can only list the same market: every field but `creator` is fixed by the
/// mint and the config, and the market works the same (C0-15; R-MINT §8.2).
#[test]
fn front_running_create_market_is_harmless() {
    let mut env = Env::initialized();
    let (mint, _) = mint_for(&mut env, TokenProgram::Token2022);
    let attacker = env.funded_keypair();
    let victim = env.funded_keypair();
    ok(create(&mut env, &attacker, &mint, &mints::t22_id()));
    assert_failed(&create(&mut env, &victim, &mint, &mints::t22_id()));
    let market = assert_listed(
        &env,
        &mint,
        DECIMALS,
        TokenProgram::Token2022,
        &attacker.pubkey(),
    );
    env.track_market(market);

    let mut as_victim = env.market(&market);
    as_victim.creator = victim.pubkey();
    let (want, _, _) = expected(
        &env.config(),
        &mint,
        DECIMALS,
        TokenProgram::Token2022,
        &victim.pubkey(),
        env.now(),
    );
    assert_bytes_eq(&as_victim, &want, "Market apart from the creator");
    // The market is live for everyone.
    assert_err(
        &env.send(&[ix::flag_unsafe_mint(&mint)], &[]),
        ShortwinError::MintSafe,
    );
}

/// Anyone can send lamports to a short.win PDA (and create the trader's ATA), but only
/// short.win can allocate or assign its PDAs, so none of it can block a listing (R-MINT §8.2;
/// ARCH §2.4 "grief-proof creation").
#[test]
fn pre_funded_pdas_and_a_pre_created_trader_ata_do_not_block_listing() {
    for program in TokenProgram::BOTH {
        // A new system account must be rent-exempt, so the smallest donation is the 0-byte
        // minimum (890,880 lamports today); the others exceed one PDA's rent but not another's,
        // or every PDA's.
        for donation in [890_880u64, 5_000_000, 50_000_000_000] {
            let mut env = Env::initialized();
            let (mint, _) = mint_for(&mut env, program);
            let (market, _) = pda::market(&mint);
            let griefer = env.funded_keypair();
            let targets = [
                market,
                pda::market_vault(&market).0,
                pda::pool_vault(&market).0,
                pda::trader(&market).0,
                pda::trader_wsol(&market).0,
            ];
            for t in targets {
                env.fund(&t, donation);
            }
            // The trader's base ATA, created by the ATA program for anyone who pays.
            let trader = pda::trader(&market).0;
            let ata_ix = anchor_spl::associated_token::spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &griefer.pubkey(),
                &trader,
                &mint,
                &program.id(),
            );
            ok(env.send(&[ata_ix], &[&griefer]));
            let ata = pda::ata(&trader, &mint, &program.id());
            assert!(read::token_amount(&env.svm, &ata).is_some());

            let creator = env.funded_keypair();
            let before = read::lamports(&env.svm, &creator.pubkey());
            ok(create(&mut env, &creator, &mint, &program.id()));
            env.track_market(market);
            env.assert_invariants();

            let m = env.market(&market);
            assert_eq!(m.mint, mint);
            assert_eq!(m.trader_base_ata, ata);
            let pool = env.svm.get_account(&pda::pool_vault(&market).0).unwrap();
            let state = TokenAccountState::unpack(&pool.data).unwrap();
            assert_eq!((state.mint, state.owner), (mint, market));
            assert_eq!(pool.owner, program.id());
            // The creator pays at most the full price: only the shortfall of each PDA.
            let rent = |len| env.svm.minimum_balance_for_rent_exemption(len);
            let full = rent(Market::SPACE) + rent(MarketVault::SPACE) + rent(165);
            let fee = env.config().params.market_creation_fee;
            let paid = before - read::lamports(&env.svm, &creator.pubkey());
            assert!(paid <= full + fee, "paid {paid} > {}", full + fee);
            assert_eq!(env.market_vault(&market).protocol_fees, fee);
        }
    }
}

#[test]
fn detection_keys_are_derived_on_chain_and_checked() {
    let mut env = Env::initialized();
    let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
    let creator = env.funded_keypair();
    let spl = TokenProgram::Spl.id();
    let bc = AccountMeta::new_readonly(pda::bonding_curve(&mint), false);
    let cp = AccountMeta::new_readonly(pda::canonical_pool(&mint), false);
    let fake = AccountMeta::new_readonly(Pubkey::new_unique(), false);

    let cases: [(Vec<AccountMeta>, ShortwinError); 7] = [
        (vec![], ShortwinError::BadRemainingAccounts),
        (vec![bc.clone()], ShortwinError::BadRemainingAccounts),
        (
            vec![bc.clone(), cp.clone(), fake.clone()],
            ShortwinError::BadRemainingAccounts,
        ),
        (
            vec![cp.clone(), bc.clone()],
            ShortwinError::VenueAccountMismatch,
        ),
        (
            vec![fake.clone(), cp.clone()],
            ShortwinError::VenueAccountMismatch,
        ),
        (
            vec![bc.clone(), fake.clone()],
            ShortwinError::VenueAccountMismatch,
        ),
        // Another mint's (real) detection keys.
        (
            vec![
                AccountMeta::new_readonly(pda::bonding_curve(&Pubkey::new_unique()), false),
                cp.clone(),
            ],
            ShortwinError::VenueAccountMismatch,
        ),
    ];
    for (remaining, want) in cases {
        let res = create_with_remaining(&mut env, &creator, &mint, &spl, remaining);
        assert_err(&res, want);
    }
    // Writable detection accounts are accepted (they are only key-checked).
    let res = create_with_remaining(
        &mut env,
        &creator,
        &mint,
        &spl,
        vec![
            AccountMeta::new(bc.pubkey, false),
            AccountMeta::new(cp.pubkey, false),
        ],
    );
    ok(res);
    let m = env.market(&pda::market(&mint).0);
    assert_eq!(m.bonding_curve, pda::bonding_curve(&mint));
    assert_eq!(m.canonical_pool, pda::canonical_pool(&mint));
}

/// `venue_hint`: `Auto` lists as `None`; since M3 the Raydium hints (4, 5) adopt a candidate
/// pool, which they need (without its ReadSet: `BadRemainingAccounts`; the Raydium listing
/// itself is `tests/ray_it/listing.rs`); every other hint — the pump kinds on a mint pump did
/// not create, the test-only mock on the release build, unknown values — is `NoSupportedVenue`.
#[test]
fn venue_hints() {
    let mut env = Env::initialized();
    let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
    let creator = env.funded_keypair();
    for (hint, want) in [
        (1u8, ShortwinError::NoSupportedVenue),
        (2, ShortwinError::NoSupportedVenue),
        (3, ShortwinError::NoSupportedVenue),
        (4, ShortwinError::BadRemainingAccounts),
        (5, ShortwinError::BadRemainingAccounts),
        (6, ShortwinError::NoSupportedVenue),
        (255, ShortwinError::NoSupportedVenue),
    ] {
        let ix = ix::create_market(
            &creator.pubkey(),
            &mint,
            &TokenProgram::Spl.id(),
            hint,
            &pda::canonical_pool(&mint),
        );
        assert_err(&env.send(&[ix], &[&creator]), want);
    }
}

#[test]
fn account_constraints_bite() {
    let mut env = Env::initialized();
    let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
    let (other_mint, _) = mint_for(&mut env, TokenProgram::Spl);
    let creator = env.funded_keypair();
    let spl = TokenProgram::Spl.id();
    let base = || {
        ix::create_market(
            &creator.pubkey(),
            &mint,
            &spl,
            VENUE_HINT_AUTO,
            &pda::canonical_pool(&mint),
        )
    };
    let with = |i: usize, key: Pubkey| {
        let mut ix: Instruction = base();
        ix.accounts[i].pubkey = key;
        ix
    };
    // Account order: creator, config, venue_guard (M3-SEC-2), market, market_vault, pool_vault,
    // mint, trader, trader_base_ata, token_program, associated_token_program, system_program,
    // event_authority, program.
    let other_market = pda::market(&other_mint).0;
    let seeds = [
        (3, other_market),
        (4, pda::market_vault(&other_market).0),
        (5, pda::pool_vault(&other_market).0),
        (7, pda::trader(&other_market).0),
    ];
    for (i, key) in seeds {
        assert_anchor_err(
            &env.send(&[with(i, key)], &[&creator]),
            AnchorErrorCode::ConstraintSeeds,
        );
    }
    // The pool vault must be the PDA, not the market's ATA (R-MINT §8.2).
    let market = pda::market(&mint).0;
    assert_anchor_err(
        &env.send(&[with(5, pda::ata(&market, &mint, &spl))], &[&creator]),
        AnchorErrorCode::ConstraintSeeds,
    );
    assert_err(
        &env.send(
            &[with(
                8,
                pda::ata(&pda::trader(&other_market).0, &mint, &spl),
            )],
            &[&creator],
        ),
        ShortwinError::AccountMismatch,
    );
    // The token program must own the mint.
    let t22 = TokenProgram::Token2022.id();
    let mut wrong_program = ix::create_market(
        &creator.pubkey(),
        &mint,
        &t22,
        VENUE_HINT_AUTO,
        &pda::canonical_pool(&mint),
    );
    assert_err(
        &env.send(&[wrong_program.clone()], &[&creator]),
        ShortwinError::AccountMismatch,
    );
    // …and be a token program at all.
    wrong_program.accounts[9].pubkey = anchor_lang::system_program::ID;
    wrong_program.accounts[8].pubkey = pda::ata(
        &pda::trader(&market).0,
        &mint,
        &anchor_lang::system_program::ID,
    );
    assert_anchor_err(
        &env.send(&[wrong_program], &[&creator]),
        AnchorErrorCode::InvalidProgramId,
    );
    // A config at another address.
    let fake_config = Pubkey::new_unique();
    let data = env.svm.get_account(&pda::config()).unwrap();
    env.svm.set_account(fake_config, data).unwrap();
    assert_anchor_err(
        &env.send(&[with(1, fake_config)], &[&creator]),
        AnchorErrorCode::ConstraintSeeds,
    );
    // A venue guard at another address (M3-SEC-2: the latch is key-checked like the config).
    let fake_guard = Pubkey::new_unique();
    let data = env.svm.get_account(&pda::venue_guard()).unwrap();
    env.svm.set_account(fake_guard, data).unwrap();
    assert_anchor_err(
        &env.send(&[with(2, fake_guard)], &[&creator]),
        AnchorErrorCode::ConstraintSeeds,
    );
    // Nothing was created by any of the failures.
    assert!(env.svm.get_account(&market).is_none());
    ok(env.send(&[base()], &[&creator]));
}

#[test]
fn create_market_needs_an_initialized_config() {
    let mut env = Env::new();
    let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
    let creator = env.funded_keypair();
    assert_failed(&create(&mut env, &creator, &mint, &TokenProgram::Spl.id()));
}

// ---- flag_unsafe_mint ----------------------------------------------------------------------

fn listed_pump_style(env: &mut Env) -> (Pubkey, mints::MintFixture) {
    let fx = mints::pump_style_mint(env, 1_000_000_000);
    let creator = env.funded_keypair();
    ok(create(env, &creator, &fx.mint, &mints::t22_id()));
    let market = pda::market(&fx.mint).0;
    env.track_market(market);
    (market, fx)
}

fn assert_flagged(env: &mut Env, mint: &Pubkey, market: &Pubkey) {
    let before = env.market(market);
    assert!(!before.has_flag(MARKET_FLAG_MINT_UNSAFE));
    let meta = ok(env.send(&[ix::flag_unsafe_mint(mint)], &[]));
    let e: MintSafetyTripped = events::one(&meta);
    assert_eq!((e.market, e.mint), (*market, *mint));
    assert_eq!((e.slot, e.ts), (env.clock().slot, env.now()));
    let mut after = env.market(market);
    assert!(after.has_flag(MARKET_FLAG_MINT_UNSAFE));
    // Nothing else changed.
    after.set_flag(MARKET_FLAG_MINT_UNSAFE, false);
    assert_bytes_eq(&after, &before, "Market besides the flag");
    // One-way and idempotent: a second call changes nothing.
    assert_err(
        &env.send(&[ix::flag_unsafe_mint(mint)], &[]),
        ShortwinError::MintUnsafeFlagged,
    );
}

#[test]
fn flag_unsafe_mint_refuses_a_safe_mint() {
    let mut env = Env::initialized();
    for program in TokenProgram::BOTH {
        let (mint, _) = mint_for(&mut env, program);
        let creator = env.funded_keypair();
        ok(create(&mut env, &creator, &mint, &program.id()));
        env.track_market(pda::market(&mint).0);
        assert_err(
            &env.send(&[ix::flag_unsafe_mint(&mint)], &[]),
            ShortwinError::MintSafe,
        );
    }
    let (market, fx) = listed_pump_style(&mut env);
    assert_err(
        &env.send(&[ix::flag_unsafe_mint(&fx.mint)], &[]),
        ShortwinError::MintSafe,
    );
    assert!(!env.market(&market).has_flag(MARKET_FLAG_MINT_UNSAFE));
}

/// The changes Token-2022 allows on a listed mint keep it listable (R-MINT §4 "mutability
/// rules"): metadata appended after init and edited, metadata authority revoked, mint
/// authority revoked, a group appended.
#[test]
fn allowed_mint_changes_stay_safe() {
    let mut env = Env::initialized();
    let fx = mints::t22_mint(
        &mut env,
        &[ExtensionType::MetadataPointer, ExtensionType::GroupPointer],
        false,
    );
    let creator = env.funded_keypair();
    ok(create(&mut env, &creator, &fx.mint, &mints::t22_id()));
    let market = pda::market(&fx.mint).0;
    env.track_market(market);
    let auth = fx.authority.pubkey();
    let steps: Vec<Instruction> = vec![
        mints::token_metadata_initialize(
            &fx.mint,
            &auth,
            &auth,
            "Later",
            "LTR",
            "https://x.invalid",
        ),
        mints::token_metadata_update_name(&fx.mint, &auth, "Renamed after listing"),
        mints::token_group_initialize(&fx.mint, &auth, &auth, 10),
        mints::token_metadata_update_authority(&fx.mint, &auth, None),
        spl_token_2022_interface::instruction::set_authority(
            &mints::t22_id(),
            &fx.mint,
            None,
            spl_token_2022_interface::instruction::AuthorityType::MintTokens,
            &auth,
            &[],
        )
        .unwrap(),
    ];
    for step in steps {
        ok(env.send(&[step], &[&fx.authority]));
        assert_err(
            &env.send(&[ix::flag_unsafe_mint(&fx.mint)], &[]),
            ShortwinError::MintSafe,
        );
    }
    assert_eq!(
        mints::extension_types_of(&env, &fx.mint),
        [
            ExtensionType::MetadataPointer,
            ExtensionType::GroupPointer,
            ExtensionType::TokenMetadata,
            ExtensionType::TokenGroup
        ]
    );
}

/// A freeze authority appearing on a listed mint (impossible under today's Token-2022 —
/// `None` can never become `Some` — so the account is rewritten, as an upgrade or a bug would).
#[test]
fn flag_after_a_freeze_authority_appears() {
    for program in TokenProgram::BOTH {
        let mut env = Env::initialized();
        let (mint, _) = mint_for(&mut env, program);
        let creator = env.funded_keypair();
        ok(create(&mut env, &creator, &mint, &program.id()));
        let market = pda::market(&mint).0;
        env.track_market(market);
        let freezer = Pubkey::new_unique();
        mints::edit_account(&mut env, &mint, |a| {
            a.data[46..50].copy_from_slice(&1u32.to_le_bytes());
            a.data[50..82].copy_from_slice(freezer.as_ref());
        });
        assert_flagged(&mut env, &mint, &market);
    }
}

/// A dangerous extension appearing on a listed mint (again only possible through an upgrade
/// or a bug: Token-2022 accepts dangerous extensions only before `InitializeMint`).
#[test]
fn flag_after_an_extension_appears() {
    let appended = [
        u16::from(ExtensionType::TransferHook),
        u16::from(ExtensionType::PermanentDelegate),
        u16::from(ExtensionType::TransferFeeConfig),
        u16::from(ExtensionType::Pausable),
        u16::from(ExtensionType::PermissionedBurn),
        29, // unknown to every interface today
    ];
    for raw in appended {
        let mut env = Env::initialized();
        let (market, fx) = listed_pump_style(&mut env);
        mints::edit_account(&mut env, &fx.mint, |a| {
            let len = mints::forged_value_len(raw);
            a.data.extend_from_slice(&raw.to_le_bytes());
            a.data
                .extend_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
            a.data.resize(a.data.len() + len, 0);
        });
        assert_flagged(&mut env, &fx.mint, &market);
    }
}

#[test]
fn flag_after_the_mint_moves_or_breaks() {
    // A mint that left the market's token program (the bytes alone still pass the filter).
    {
        let mut env = Env::initialized();
        let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
        let creator = env.funded_keypair();
        ok(create(&mut env, &creator, &mint, &TokenProgram::Spl.id()));
        let market = pda::market(&mint).0;
        env.track_market(market);
        mints::edit_account(&mut env, &mint, |a| a.owner = mints::t22_id());
        assert_flagged(&mut env, &mint, &market);
    }
    // A closed mint (system-owned, empty).
    {
        let mut env = Env::initialized();
        let (market, fx) = listed_pump_style(&mut env);
        env.svm
            .set_account(
                fx.mint,
                Account {
                    lamports: 0,
                    data: vec![],
                    owner: anchor_lang::system_program::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        assert_flagged(&mut env, &fx.mint, &market);
    }
    // Garbage in the padding.
    {
        let mut env = Env::initialized();
        let (market, fx) = listed_pump_style(&mut env);
        mints::edit_account(&mut env, &fx.mint, |a| a.data[100] = 7);
        assert_flagged(&mut env, &fx.mint, &market);
    }
}

/// A market that exists for an unsafe mint (installed directly, as a listing under an older
/// filter would be) is flagged at once.
#[test]
fn flag_a_market_installed_for_a_freeze_authority_mint() {
    let mut env = Env::initialized();
    let authority = Keypair::new();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint_with_freeze(
        &mut env.svm,
        &payer,
        &authority.pubkey(),
        DECIMALS,
        TokenProgram::Spl,
    );
    let config = env.config();
    let now = env.now();
    let keys = fixtures::install_market(
        &mut env.svm,
        &mint,
        DECIMALS,
        TokenProgram::Spl,
        &payer.pubkey(),
        now,
        &config,
    );
    env.track_market(keys.market);
    assert_flagged(&mut env, &mint, &keys.market);
}

#[test]
fn flag_unsafe_mint_constraints() {
    let mut env = Env::initialized();
    let (market, fx) = listed_pump_style(&mut env);
    let (other, _) = mint_for(&mut env, TokenProgram::Spl);
    // Another mint than the market's.
    let mut ix = ix::flag_unsafe_mint(&fx.mint);
    ix.accounts[1].pubkey = other;
    assert_anchor_err(&env.send(&[ix], &[]), AnchorErrorCode::ConstraintSeeds);
    // No market for that mint.
    assert_failed(&env.send(&[ix::flag_unsafe_mint(&other)], &[]));
    // A market account at a non-PDA address.
    let fake = Pubkey::new_unique();
    let data = env.svm.get_account(&market).unwrap();
    env.svm.set_account(fake, data).unwrap();
    let mut ix = ix::flag_unsafe_mint(&fx.mint);
    ix.accounts[0].pubkey = fake;
    assert_anchor_err(&env.send(&[ix], &[]), AnchorErrorCode::ConstraintSeeds);
}

#[test]
fn cu_create_market_and_flag_unsafe_mint() {
    let mut env = Env::initialized();
    let mut rows = Vec::new();
    for program in TokenProgram::BOTH {
        let (mint, _) = mint_for(&mut env, program);
        let creator = env.funded_keypair();
        let meta = ok(create(&mut env, &creator, &mint, &program.id()));
        rows.push((
            format!("create_market ({program:?})"),
            meta.compute_units_consumed,
        ));
    }
    let fx = mints::pump_style_mint(&mut env, 1);
    let creator = env.funded_keypair();
    let meta = ok(create(&mut env, &creator, &fx.mint, &mints::t22_id()));
    rows.push((
        "create_market (pump-style T22)".into(),
        meta.compute_units_consumed,
    ));
    let market = pda::market(&fx.mint).0;
    let res = env.send(&[ix::flag_unsafe_mint(&fx.mint)], &[]);
    assert_err(&res, ShortwinError::MintSafe);
    mints::edit_account(&mut env, &fx.mint, |a| a.data[46] = 1);
    let meta = ok(env.send(&[ix::flag_unsafe_mint(&fx.mint)], &[]));
    rows.push((
        "flag_unsafe_mint (flags)".into(),
        meta.compute_units_consumed,
    ));
    assert!(env.market(&market).has_flag(MARKET_FLAG_MINT_UNSAFE));
    for (name, cu) in &rows {
        println!("{name:40} {cu:>8} CU");
        // ARCH §6.1: create_market 70–120 k estimated, 250 k limit.
        assert!(*cu <= 120_000, "{name}: {cu} CU");
    }
}

/// The pool vault's PDA seeds are the ones `pda::pool_vault` uses (and the IDL documents).
#[test]
fn pool_vault_address() {
    let mint = Pubkey::new_unique();
    let market = pda::market(&mint).0;
    let (key, _) = Pubkey::find_program_address(&[POOL_VAULT_SEED, market.as_ref()], &shortwin::ID);
    assert_eq!(key, pda::pool_vault(&market).0);
}

/// Spoofed owners at the detection PDAs (ARCH §2.5 `classify_pump`, C0-15; M10 G-A3), on a fresh
/// SPL mint with no pump programs loaded: a griefer pre-funding the mint's bonding-curve and
/// canonical-pool addresses (System accounts, the reachable case), and a real pump curve's bytes
/// (the dumped `t22_115` curve) planted at the curve PDA under another owner — the System
/// program, the token program, an unknown program (unreachable on mainnet: only pump can create an
/// account at its PDA). Each is no pump venue: the list-only listing lands as `None`, no
/// `graduated_elsewhere`. The control: the same bytes under pump are a live curve, so the same
/// list-only listing is refused (`BadRemainingAccounts`: a `PumpCurve` listing needs the pump
/// init set).
#[test]
fn spoofed_owners_at_the_detection_pdas_list_only() {
    let curve_bytes = {
        let mut env = Env::initialized();
        let c = env.load_pump_coin(shortwin_litesvm_tests::fork::PumpCoin::T22_115);
        env.svm.get_account(&c.curve).expect("the dumped curve")
    };
    let spl = TokenProgram::Spl.id();
    let unknown = Pubkey::new_unique();
    let pre_funded = Account {
        lamports: 5_000_000,
        ..Account::default()
    };
    let planted = |owner: Pubkey| Account {
        owner,
        ..curve_bytes.clone()
    };
    let cases: [(&str, Option<Account>, Option<Account>); 4] = [
        (
            "both PDAs pre-funded",
            Some(pre_funded.clone()),
            Some(pre_funded),
        ),
        (
            "curve bytes, System-owned",
            Some(planted(anchor_lang::system_program::ID)),
            None,
        ),
        ("curve bytes, token-program-owned", Some(planted(spl)), None),
        (
            "curve bytes, an unknown owner",
            Some(planted(unknown)),
            None,
        ),
    ];
    for (what, at_curve, at_pool) in cases {
        let mut env = Env::initialized();
        let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
        if let Some(a) = at_curve {
            env.svm.set_account(pda::bonding_curve(&mint), a).unwrap();
        }
        if let Some(a) = at_pool {
            env.svm.set_account(pda::canonical_pool(&mint), a).unwrap();
        }
        let creator = env.funded_keypair();
        ok(create(&mut env, &creator, &mint, &spl));
        let m = env.market(&pda::market(&mint).0);
        assert_eq!(m.venue_kind(), Some(VenueKind::None), "{what}");
        assert!(
            !m.has_flag(shortwin::state::MARKET_FLAG_GRADUATED_ELSEWHERE),
            "{what}"
        );
    }
    // The control: pump's own curve at the PDA is detected.
    let mut env = Env::initialized();
    let (mint, _) = mint_for(&mut env, TokenProgram::Spl);
    env.svm
        .set_account(pda::bonding_curve(&mint), curve_bytes)
        .unwrap();
    let creator = env.funded_keypair();
    assert_err(
        &create(&mut env, &creator, &mint, &spl),
        ShortwinError::BadRemainingAccounts,
    );
}

/// T-LISTING (pump-final §6.3, owner decision D1 (a)): the dumped live cashback curve
/// (`cashback166`, `8oe1sfhK…`) is refused at listing with `CurveCashbackCoin` = 6109 — pump's v3
/// trades refuse a cashback coin (6094) and every curve leg trades v3 — with and without the pump
/// init set (the class decides first); a live curve of quote-chain depth 1 is not a SOL curve
/// (`CurveQuoteNotSol`); mayhem stays `CurveMayhemUnsupported`. The control: the same curve with
/// the flag cleared lists as `PumpCurve`.
#[test]
fn cashback_curve_is_refused() {
    use shortwin_litesvm_tests::fork::{self, PumpCoin};
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Cashback166);
    let st = c.state(&env.svm);
    assert!(st.is_cashback_coin && !st.complete && st.is_sol_quote());
    assert_err(&env.list_pump(&c), ShortwinError::CurveCashbackCoin);
    assert_eq!(u32::from(ShortwinError::CurveCashbackCoin), 6_109);
    let creator = env.funded_keypair();
    assert_err(
        &create(&mut env, &creator, &c.mint, &c.token_program.id()),
        ShortwinError::CurveCashbackCoin,
    );
    fork::patch(&mut env.svm, &c.curve, |d| {
        d[shortwin_venue_state::pump::OFF_IS_CASHBACK_COIN] = 0
    });
    ok(env.list_pump(&c));
    assert_eq!(
        env.market(&pda::market(&c.mint).0).venue_kind(),
        Some(VenueKind::PumpCurve)
    );

    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near166);
    fork::patch(&mut env.svm, &c.curve, |d| {
        d[shortwin_venue_state::pump::OFF_DEPTH] = 1
    });
    assert_err(&env.list_pump(&c), ShortwinError::CurveQuoteNotSol);

    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Mayhem166);
    assert_err(&env.list_pump(&c), ShortwinError::CurveMayhemUnsupported);
}
