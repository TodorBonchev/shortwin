//! The TEST-ONLY mock venue `mock_cpamm` (ARCH §9 M2, A49; `tests/mock_cpamm/INTERFACE.md`)
//! on LiteSVM, loaded from its SBF build:
//!
//! - the execution equals short.win's on-chain quote `quote::cpmm` with a zero creator fee,
//!   exactly (output, cost and post-trade reserves), on generated pools, fees and trade
//!   sequences in both directions and both kinds, on SPL Token and Token-2022 base mints;
//! - the pool layout is the documented one (byte offsets, PDAs, vault authorities);
//! - the knobs: disable, fee change, `layout_version = 1` (the counters move), exact-out
//!   over-delivery, required extras (accepted exactly, rejected otherwise);
//! - a delegate-aware source; a Token-2022 base with metadata extensions (the pump.fun shape);
//! - account validation and the admin checks.
//!
//! Every instruction goes through `Env::send` (the invariant hooks).

// `TxResult` is LiteSVM's own result type (see the harness's `lib.rs`).
#![allow(clippy::result_large_err)]

use std::cell::RefCell;

use anchor_lang::ToAccountMetas;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use mock_cpamm::errors::MockError;
use mock_cpamm::state::{self as layout, POOL_LEN, PoolState};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use shortwin_litesvm_tests::errors::{assert_anchor_err, assert_failed, ok};
use shortwin_litesvm_tests::fixtures::mints;
use shortwin_litesvm_tests::mock::{
    self, Dir, Knobs, Leg, PoolFixture, assert_mock_err, is_mock_err,
};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, TxResult, read};
use solana_signer::Signer;

const FEE: u32 = 2_500;
/// Largest amount a generated trade funds (keeps the base mint's supply, which also funds
/// every refused attempt, below `u64::MAX` over a case).
const FUND_CAP: u64 = 1 << 60;
/// Upper bound on one swap's CU (measured 18.4 k SPL, 20.4 k Token-2022 base; CPMM on
/// mainnet: 24–34 k, ARCH §6.1).
const SWAP_CU_MAX: u64 = 30_000;

fn env() -> Env {
    let mut env = Env::new();
    env.load_mock_cpamm();
    env
}

/// A seeded pool: `base`/`quote` raw units at `fee`.
fn seeded(env: &mut Env, program: TokenProgram, fee: u32, base: u64, quote: u64) -> PoolFixture {
    let f = mock::new_pool(env, program, 6, fee);
    mock::seed(env, &f, base, quote);
    f
}

fn amount(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or_else(|| panic!("no token account {key}"))
}

fn u64_at(data: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
}

fn key_at(data: &[u8], off: usize) -> Pubkey {
    Pubkey::new_from_array(data[off..off + 32].try_into().unwrap())
}

fn raw_pool(env: &Env, pool: &Pubkey) -> Vec<u8> {
    read::account(&env.svm, pool).unwrap().data
}

fn approve_ix(
    program: TokenProgram,
    source: &Pubkey,
    delegate: &Pubkey,
    owner: &Pubkey,
    amount: u64,
) -> Instruction {
    spl_token_2022_interface::instruction::approve(
        &program.id(),
        source,
        delegate,
        owner,
        &[],
        amount,
    )
    .unwrap()
}

/// An exact-in swap by a fresh trader; returns (result, leg).
fn swap_in(env: &mut Env, f: &PoolFixture, dir: Dir, a: u64, min_out: u64) -> (TxResult, Leg) {
    let (owner, leg) = mock::trader(env, f, dir, a);
    let ix = mock::swap_exact_in_ix(&f.keys, &leg, a, min_out, &[]);
    (env.send(&[ix], &[&owner]), leg)
}

/// An exact-out swap by a fresh trader funded with `fund`; returns (result, leg).
fn swap_out(
    env: &mut Env,
    f: &PoolFixture,
    dir: Dir,
    q: u64,
    max_in: u64,
    fund: u64,
) -> (TxResult, Leg) {
    let (owner, leg) = mock::trader(env, f, dir, fund);
    let ix = mock::swap_exact_out_ix(&f.keys, &leg, max_in, q, &[]);
    (env.send(&[ix], &[&owner]), leg)
}

// ---- layout and creation -----------------------------------------------------------------

/// The pool's bytes are at the offsets INTERFACE.md documents; the PDAs and vault
/// authorities are the documented ones.
#[test]
fn pool_layout_and_pdas_on_both_token_programs() {
    let mut env = env();
    for program in TokenProgram::BOTH {
        let f = mock::new_pool(&mut env, program, 6, FEE);
        let k = f.keys;
        let acc = read::account(&env.svm, &k.pool).unwrap();
        assert_eq!(acc.owner, mock::PROGRAM_ID);
        assert_eq!(acc.data.len(), POOL_LEN);
        let d = &acc.data;
        assert_eq!(d[..8], layout::POOL_DISCRIMINATOR);
        assert_eq!(d[layout::OFF_LAYOUT_VERSION], 0);
        assert_eq!(d[layout::OFF_DISABLED], 0);
        assert_eq!(
            u32::from_le_bytes(d[layout::OFF_FEE_PPM..][..4].try_into().unwrap()),
            FEE
        );
        assert_eq!(u64_at(d, layout::OFF_OVER_DELIVERY), 0);
        assert_eq!(u64_at(d, layout::OFF_PROTOCOL_FEE_BASE_V0), 0);
        assert_eq!(u64_at(d, layout::OFF_PROTOCOL_FEE_QUOTE_V0), 0);
        assert_eq!(d[layout::OFF_REQUIRED_EXTRAS_LEN], 0);
        assert_eq!(key_at(d, layout::OFF_BASE_VAULT), k.base_vault);
        assert_eq!(key_at(d, layout::OFF_QUOTE_VAULT), k.quote_vault);
        assert_eq!(d[layout::OFF_BUMP], mock::pool_address(&k.base_mint).1);
        assert_eq!(key_at(d, layout::OFF_ADMIN), f.admin.pubkey());
        assert_eq!(key_at(d, layout::OFF_BASE_MINT), k.base_mint);
        assert_eq!(key_at(d, layout::OFF_QUOTE_MINT), mock::WSOL_MINT);
        assert_eq!(key_at(d, layout::OFF_BASE_TOKEN_PROGRAM), program.id());
        assert_eq!(d[layout::OFF_AUTHORITY_BUMP], mock::authority().1);
        assert_eq!(
            d[layout::OFF_BASE_VAULT_BUMP],
            mock::vault_address(&k.pool, &k.base_mint).1
        );
        assert_eq!(
            d[layout::OFF_QUOTE_VAULT_BUMP],
            mock::vault_address(&k.pool, &mock::WSOL_MINT).1
        );
        assert!(d[363..364].iter().chain(&d[364..384]).all(|b| *b == 0));

        // Vaults: token accounts of their mint, owned by the global authority PDA, under the
        // side's token program.
        for (vault, mint, prog) in [
            (k.base_vault, k.base_mint, program),
            (k.quote_vault, mock::WSOL_MINT, TokenProgram::Spl),
        ] {
            let v = read::account(&env.svm, &vault).unwrap();
            assert_eq!(v.owner, prog.id());
            assert_eq!(key_at(&v.data, 0), mint);
            assert_eq!(key_at(&v.data, 32), mock::authority().0);
            assert_eq!(read::token_delegate(&env.svm, &vault), None);
        }

        // A second pool for the same mint pair cannot be created.
        let ix = mock::initialize_pool_ix(&f.admin.pubkey(), &k.base_mint, program, FEE);
        let admin = f.admin.insecure_clone();
        assert_failed(&env.send(&[ix], &[&admin]));
    }
}

/// The reader contract (INTERFACE.md "Reader contract"): short.win's reader
/// (`shortwin_venue_state::mock_cpamm`, the adapter's view) uses exactly the mock's own program
/// id, discriminator and version-0 offsets; on a live pool it reads the same fields as the mock's
/// own decoder, and it refuses the pool once `layout_version = 1` moves the counters (a
/// simulated venue-state layout change, A63).
#[test]
fn the_reader_matches_the_mock_layout() {
    use shortwin_venue_state::mock_cpamm as reader;
    assert_eq!(reader::PROGRAM, mock::PROGRAM_ID.to_bytes());
    assert_eq!(reader::POOL_DISCRIMINATOR, layout::POOL_DISCRIMINATOR);
    for (what, r, m) in [
        (
            "layout_version",
            reader::OFF_LAYOUT_VERSION,
            layout::OFF_LAYOUT_VERSION,
        ),
        ("disabled", reader::OFF_DISABLED, layout::OFF_DISABLED),
        ("fee_ppm", reader::OFF_FEE_PPM, layout::OFF_FEE_PPM),
        (
            "protocol_fee_base",
            reader::OFF_PROTOCOL_FEE_BASE,
            layout::OFF_PROTOCOL_FEE_BASE_V0,
        ),
        (
            "protocol_fee_quote",
            reader::OFF_PROTOCOL_FEE_QUOTE,
            layout::OFF_PROTOCOL_FEE_QUOTE_V0,
        ),
        ("base_vault", reader::OFF_BASE_VAULT, layout::OFF_BASE_VAULT),
        (
            "quote_vault",
            reader::OFF_QUOTE_VAULT,
            layout::OFF_QUOTE_VAULT,
        ),
        ("base_mint", reader::OFF_BASE_MINT, layout::OFF_BASE_MINT),
        ("quote_mint", reader::OFF_QUOTE_MINT, layout::OFF_QUOTE_MINT),
        (
            "base_token_program",
            reader::OFF_BASE_TOKEN_PROGRAM,
            layout::OFF_BASE_TOKEN_PROGRAM,
        ),
    ] {
        assert_eq!(r, m, "{what}");
    }
    const { assert!(reader::MIN_LEN <= POOL_LEN) };
    let mut env = env();
    for program in TokenProgram::BOTH {
        let f = seeded(
            &mut env,
            program,
            FEE,
            1_000_000_000_000_000,
            1_000_000_000_000,
        );
        // Trade both ways so the reserve counters are non-zero.
        ok(swap_in(&mut env, &f, Dir::QuoteIn, 10_000_000_000, 0).0);
        ok(swap_in(&mut env, &f, Dir::BaseIn, 10_000_000_000_000, 0).0);
        let acc = read::account(&env.svm, &f.keys.pool).unwrap();
        let own = PoolState::decode(&acc.data).unwrap();
        let seen = reader::read(&acc.owner.to_bytes(), &acc.data).expect("the reader parses");
        assert!(own.protocol_fee_base > 0 && own.protocol_fee_quote > 0);
        assert_eq!(
            (
                seen.disabled,
                seen.fee_ppm,
                seen.protocol_fee_base,
                seen.protocol_fee_quote
            ),
            (
                own.disabled,
                own.fee_ppm,
                own.protocol_fee_base,
                own.protocol_fee_quote
            )
        );
        assert_eq!(
            [
                seen.base_vault,
                seen.quote_vault,
                seen.base_mint,
                seen.quote_mint,
                seen.base_token_program
            ],
            [
                own.base_vault.to_bytes(),
                own.quote_vault.to_bytes(),
                own.base_mint.to_bytes(),
                own.quote_mint.to_bytes(),
                own.base_token_program.to_bytes()
            ]
        );
        mock::update_knobs(&mut env, &f, |k: &mut Knobs| k.layout_version = 1);
        let acc = read::account(&env.svm, &f.keys.pool).unwrap();
        assert!(
            reader::read(&acc.owner.to_bytes(), &acc.data).is_err(),
            "layout 1 does not parse"
        );
    }
}

#[test]
fn initialize_pool_refusals() {
    let mut env = env();
    let admin = env.funded_keypair();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint(&mut env.svm, &payer, &admin.pubkey(), 6, TokenProgram::Spl);

    // fee_ppm must be below 10⁶.
    let ix = mock::initialize_pool_ix(&admin.pubkey(), &mint, TokenProgram::Spl, 1_000_000);
    assert_mock_err(&env.send(&[ix], &[&admin]), MockError::BadFee);

    // The quote mint must be WSOL (addresses derived with the other mint, so only the
    // quote-mint check can refuse).
    let other = tokens::create_mint(&mut env.svm, &payer, &admin.pubkey(), 9, TokenProgram::Spl);
    let pid = mock::PROGRAM_ID;
    let pool = Pubkey::find_program_address(&[b"pool", mint.as_ref(), other.as_ref()], &pid).0;
    let mut ix = mock::initialize_pool_ix(&admin.pubkey(), &mint, TokenProgram::Spl, FEE);
    ix.accounts[2].pubkey = other;
    ix.accounts[3].pubkey = pool;
    ix.accounts[5].pubkey = mock::vault_address(&pool, &mint).0;
    ix.accounts[6].pubkey = mock::vault_address(&pool, &other).0;
    assert_mock_err(&env.send(&[ix], &[&admin]), MockError::QuoteNotWsol);

    // Base and quote must differ (the two vault PDAs then coincide, and the second `init`
    // fails before the mock's own `SameMint` check).
    let ix = mock::initialize_pool_ix(&admin.pubkey(), &mock::WSOL_MINT, TokenProgram::Spl, FEE);
    assert_failed(&env.send(&[ix], &[&admin]));

    // The quote side is SPL Token.
    let mut ix = mock::initialize_pool_ix(&admin.pubkey(), &mint, TokenProgram::Spl, FEE);
    ix.accounts[8].pubkey = TokenProgram::Token2022.id();
    assert_failed(&env.send(&[ix], &[&admin]));

    // The base mint under the wrong token program.
    let ix = mock::initialize_pool_ix(&admin.pubkey(), &mint, TokenProgram::Token2022, FEE);
    assert_failed(&env.send(&[ix], &[&admin]));

    // fee 999,999 is the largest accepted.
    let ix = mock::initialize_pool_ix(&admin.pubkey(), &mint, TokenProgram::Spl, 999_999);
    ok(env.send(&[ix], &[&admin]));
}

/// The builders' account orders equal the Anchor-generated ones (so INTERFACE.md, which
/// lists the builders' orders, is what the program reads).
#[test]
fn builders_match_the_anchor_account_structs() {
    let mut env = env();
    let f = mock::new_pool(&mut env, TokenProgram::Token2022, 6, FEE);
    let k = f.keys;
    let keys = |metas: Vec<AccountMeta>| -> Vec<(Pubkey, bool, bool)> {
        metas
            .into_iter()
            .map(|m| (m.pubkey, m.is_signer, m.is_writable))
            .collect()
    };
    let (a, s, d) = (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );
    for dir in Dir::BOTH {
        let leg = Leg {
            dir,
            authority: a,
            source: s,
            destination: d,
        };
        let (iv, ov, im, om, ip, op) = match dir {
            Dir::BaseIn => (
                k.base_vault,
                k.quote_vault,
                k.base_mint,
                mock::WSOL_MINT,
                TokenProgram::Token2022.id(),
                TokenProgram::Spl.id(),
            ),
            Dir::QuoteIn => (
                k.quote_vault,
                k.base_vault,
                mock::WSOL_MINT,
                k.base_mint,
                TokenProgram::Spl.id(),
                TokenProgram::Token2022.id(),
            ),
        };
        let anchor = mock_cpamm::accounts::Swap {
            authority: a,
            pool: k.pool,
            pool_authority: mock::authority().0,
            source: s,
            destination: d,
            input_vault: iv,
            output_vault: ov,
            input_mint: im,
            output_mint: om,
            input_token_program: ip,
            output_token_program: op,
        }
        .to_account_metas(None);
        assert_eq!(anchor.len(), mock_cpamm::SWAP_FIXED_ACCOUNTS);
        assert_eq!(keys(mock::swap_accounts(&k, &leg)), keys(anchor));
    }
    let admin = f.admin.pubkey();
    let init = mock_cpamm::accounts::InitializePool {
        admin,
        base_mint: k.base_mint,
        quote_mint: mock::WSOL_MINT,
        pool: k.pool,
        pool_authority: mock::authority().0,
        base_vault: k.base_vault,
        quote_vault: k.quote_vault,
        base_token_program: TokenProgram::Token2022.id(),
        quote_token_program: TokenProgram::Spl.id(),
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    assert_eq!(
        keys(mock::initialize_pool_ix(&admin, &k.base_mint, TokenProgram::Token2022, 1).accounts),
        keys(init)
    );
    let add = mock_cpamm::accounts::AddLiquidity {
        depositor: a,
        pool: k.pool,
        base_source: s,
        quote_source: d,
        base_vault: k.base_vault,
        quote_vault: k.quote_vault,
        base_mint: k.base_mint,
        quote_mint: mock::WSOL_MINT,
        base_token_program: TokenProgram::Token2022.id(),
        quote_token_program: TokenProgram::Spl.id(),
    }
    .to_account_metas(None);
    assert_eq!(
        keys(mock::add_liquidity_ix(&k, &a, &s, &d, 1, 1).accounts),
        keys(add)
    );
    let remove = mock_cpamm::accounts::RemoveLiquidity {
        admin,
        pool: k.pool,
        pool_authority: mock::authority().0,
        base_vault: k.base_vault,
        quote_vault: k.quote_vault,
        base_destination: s,
        quote_destination: d,
        base_mint: k.base_mint,
        quote_mint: mock::WSOL_MINT,
        base_token_program: TokenProgram::Token2022.id(),
        quote_token_program: TokenProgram::Spl.id(),
    }
    .to_account_metas(None);
    assert_eq!(
        keys(mock::remove_liquidity_ix(&k, &admin, &s, &d, 1, 1).accounts),
        keys(remove)
    );
    let knobs = mock_cpamm::accounts::SetKnobs {
        admin,
        pool: k.pool,
    }
    .to_account_metas(None);
    assert_eq!(
        keys(mock::set_knobs_ix(&k.pool, &admin, &Knobs::default()).accounts),
        keys(knobs)
    );
}

// ---- the differential test ---------------------------------------------------------------

/// How a trade amount is drawn against the live reserves.
#[derive(Clone, Copy, Debug)]
enum Size {
    /// 0 (always refused).
    Zero,
    /// 1..=1,000 raw units.
    Tiny(u64),
    /// ppm of the relevant reserve, mostly below 100 %, up to 150 % (past the reserve for
    /// exact out).
    Frac(u64),
    /// Up to [`FUND_CAP`] raw units.
    Raw(u64),
}

#[derive(Clone, Copy, Debug)]
struct Trade {
    dir: Dir,
    exact_in: bool,
    size: Size,
}

#[derive(Clone, Debug)]
struct Case {
    program: TokenProgram,
    fee: u32,
    base: u64,
    quote: u64,
    trades: Vec<Trade>,
}

/// Log-uniform in `[1, 10^max_exp]`.
fn log_amount(max_exp: u32) -> impl Strategy<Value = u64> {
    (0..=max_exp, 1u64..10).prop_map(|(e, m)| m.saturating_mul(10u64.pow(e)).max(1))
}

fn case_strategy() -> impl Strategy<Value = Case> {
    let program = prop_oneof![Just(TokenProgram::Spl), Just(TokenProgram::Token2022)];
    let fee = prop_oneof![
        2 => Just(0u32),
        3 => Just(2_500u32),
        2 => Just(10_000u32),
        4 => 0u32..1_000_000,
        1 => 990_000u32..1_000_000
    ];
    let size = prop_oneof![
        1 => Just(Size::Zero),
        3 => (1u64..=1_000).prop_map(Size::Tiny),
        10 => (1u64..1_000_000).prop_map(Size::Frac),
        2 => (1_000_000u64..=1_500_000).prop_map(Size::Frac),
        3 => (1u64..=FUND_CAP).prop_map(Size::Raw),
    ];
    let dir = prop_oneof![Just(Dir::BaseIn), Just(Dir::QuoteIn)];
    let trade = (dir, any::<bool>(), size).prop_map(|(dir, exact_in, size)| Trade {
        dir,
        exact_in,
        size,
    });
    (
        program,
        fee,
        log_amount(17),
        log_amount(17),
        prop::collection::vec(trade, 1..5),
    )
        .prop_map(|(program, fee, base, quote, trades)| Case {
            program,
            fee,
            base,
            quote,
            trades,
        })
}

fn size_of(size: Size, reserve: u64) -> u64 {
    match size {
        Size::Zero => 0,
        Size::Tiny(v) | Size::Raw(v) => v,
        Size::Frac(ppm) => u64::try_from(u128::from(reserve) * u128::from(ppm) / 1_000_000)
            .unwrap_or(u64::MAX)
            .min(FUND_CAP),
    }
}

/// Runs one case: every trade executes iff `quote::cpmm` is defined, and then exactly as
/// quoted (amounts, post-trade reserves, counters); a one-unit tighter bound fails with the
/// slippage error and changes nothing.
/// Outcomes of the generated trades (the generator must exercise both kinds of outcome).
#[derive(Default, Debug)]
struct Stats {
    executed_in: u32,
    executed_out: u32,
    refused: u32,
    unfundable: u32,
}

fn run_case(env: &mut Env, c: &Case, stats: &mut Stats) -> Result<(), TestCaseError> {
    let f = mock::new_pool(env, c.program, 6, c.fee);
    mock::seed(env, &f, c.base, c.quote);
    for t in &c.trades {
        let before = mock::reserves(&env.svm, &f.keys);
        let (r_in, r_out) = mock::io_reserves(&before, t.dir);
        if t.exact_in {
            let a = size_of(t.size, r_in);
            let q = mock::quote_exact_in(&env.svm, &f.keys, t.dir, a);
            let Some(q) = q else {
                let (res, _) = swap_in(env, &f, t.dir, a, 0);
                prop_assert!(
                    is_mock_err(&res),
                    "undefined quote, mock: {:?}",
                    res.map(|_| ())
                );
                prop_assert_eq!(mock::reserves(&env.svm, &f.keys), before);
                stats.refused += 1;
                continue;
            };
            let (res, _) = swap_in(env, &f, t.dir, a, q.proceeds + 1);
            prop_assert_eq!(
                shortwin_litesvm_tests::errors::custom_code(&res),
                Some(u32::from(MockError::SlippageMinOut))
            );
            prop_assert_eq!(mock::reserves(&env.svm, &f.keys), before);
            let (res, leg) = swap_in(env, &f, t.dir, a, q.proceeds);
            prop_assert!(res.is_ok(), "defined quote {q:?}, mock failed: {res:?}");
            prop_assert_eq!(amount(env, &leg.destination), q.proceeds);
            prop_assert_eq!(amount(env, &leg.source), 0);
            let after = mock::reserves(&env.svm, &f.keys);
            let (i, o) = mock::io_reserves(&after, t.dir);
            prop_assert_eq!(u128::from(i), q.token_after);
            prop_assert_eq!(u128::from(o), q.sol_after);
            stats.executed_in += 1;
        } else {
            let out = size_of(t.size, r_out);
            let q = mock::quote_exact_out(&env.svm, &f.keys, t.dir, out);
            let Some(q) = q else {
                let (res, _) = swap_out(env, &f, t.dir, out, u64::MAX, 0);
                prop_assert!(
                    is_mock_err(&res),
                    "undefined quote, mock: {:?}",
                    res.map(|_| ())
                );
                prop_assert_eq!(mock::reserves(&env.svm, &f.keys), before);
                stats.refused += 1;
                continue;
            };
            if q.cost > FUND_CAP {
                // Too expensive to fund here (the host proptest of the mock's arithmetic
                // covers the whole u64 range).
                stats.unfundable += 1;
                continue;
            }
            let (res, _) = swap_out(env, &f, t.dir, out, q.cost - 1, q.cost);
            prop_assert_eq!(
                shortwin_litesvm_tests::errors::custom_code(&res),
                Some(u32::from(MockError::SlippageMaxIn))
            );
            prop_assert_eq!(mock::reserves(&env.svm, &f.keys), before);
            let (res, leg) = swap_out(env, &f, t.dir, out, q.cost, q.cost);
            prop_assert!(res.is_ok(), "defined quote {q:?}, mock failed: {res:?}");
            prop_assert_eq!(amount(env, &leg.destination), out);
            prop_assert_eq!(amount(env, &leg.source), 0);
            let after = mock::reserves(&env.svm, &f.keys);
            let (i, o) = mock::io_reserves(&after, t.dir);
            prop_assert_eq!(u128::from(i), q.sol_after);
            prop_assert_eq!(u128::from(o), q.token_after);
            stats.executed_out += 1;
        }
    }
    Ok(())
}

/// ARCH §9 M2: short.win's on-chain quote equals the mock's execution exactly. Cases:
/// `MOCK_CPAMM_CASES` (default 96); every case is a fresh pool on one VM.
#[test]
fn execution_equals_the_cpmm_quote() {
    let cases = std::env::var("MOCK_CPAMM_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(96);
    let env = RefCell::new(env());
    let stats = RefCell::new(Stats::default());
    let mut runner = TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&case_strategy(), |c| {
            run_case(&mut env.borrow_mut(), &c, &mut stats.borrow_mut())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let s = stats.into_inner();
    println!("{cases} cases: {s:?}");
    // Both kinds executed often (≈ 1.25 of each per case are drawn), refusals exercised too.
    assert!(
        s.executed_in >= cases / 2 && s.executed_out >= cases / 2,
        "{s:?}"
    );
    assert!(s.refused > 0, "{s:?}");
}

/// Hand-picked edges of the same property (tiny pools, the 10⁶ − 1 fee, reserve-sized trades).
#[test]
fn execution_equals_the_cpmm_quote_at_edges() {
    let mut env = env();
    let t = |dir, exact_in, size| Trade {
        dir,
        exact_in,
        size,
    };
    let cases = [
        Case {
            program: TokenProgram::Spl,
            fee: 0,
            base: 1,
            quote: 1,
            trades: vec![
                t(Dir::BaseIn, true, Size::Tiny(1)),
                t(Dir::QuoteIn, false, Size::Tiny(1)),
            ],
        },
        Case {
            program: TokenProgram::Token2022,
            fee: 999_999,
            base: 1_000_000_000,
            quote: 1_000_000_000,
            trades: vec![
                t(Dir::BaseIn, true, Size::Raw(1 << 40)),
                t(Dir::QuoteIn, false, Size::Frac(999_999)),
                t(Dir::QuoteIn, true, Size::Tiny(1_000)),
            ],
        },
        Case {
            program: TokenProgram::Spl,
            fee: FEE,
            base: 10u64.pow(17),
            quote: 2,
            trades: vec![
                t(Dir::QuoteIn, false, Size::Frac(1_000_000)),
                t(Dir::QuoteIn, false, Size::Frac(500_000)),
                t(Dir::BaseIn, false, Size::Tiny(1)),
                t(Dir::QuoteIn, true, Size::Raw(FUND_CAP)),
            ],
        },
    ];
    let mut stats = Stats::default();
    for c in &cases {
        run_case(&mut env, c, &mut stats).unwrap_or_else(|e| panic!("{c:?}: {e}"));
    }
    // Case 1: nothing is defined; case 2: only the first trade; case 3: all but the first.
    assert_eq!(
        (stats.executed_in, stats.executed_out, stats.refused),
        (2, 2, 5),
        "{stats:?}"
    );
}

// ---- delegate-aware source -----------------------------------------------------------------

/// The authority may be the source's owner or its delegate for at least the input (short.win's
/// Raydium-style sell leg spends `pool_vault` as the trader's delegate, ARCH §2.4.1); the
/// delegate clears at 0; a stranger or a short allowance is refused.
#[test]
fn delegate_aware_source() {
    let mut env = env();
    for program in TokenProgram::BOTH {
        let f = seeded(&mut env, program, FEE, 10u64.pow(12), 10u64.pow(12));
        for dir in Dir::BOTH {
            let src_program = match dir {
                Dir::BaseIn => program,
                Dir::QuoteIn => TokenProgram::Spl,
            };
            let a = 1_000_000;
            let (owner, leg) = mock::trader(&mut env, &f, dir, 3 * a);
            let delegate = env.funded_keypair();
            let stranger = env.funded_keypair();
            let as_delegate = Leg {
                authority: delegate.pubkey(),
                ..leg
            };

            // No allowance: refused.
            let ix = mock::swap_exact_in_ix(&f.keys, &as_delegate, a, 0, &[]);
            assert_mock_err(&env.send(&[ix], &[&delegate]), MockError::SourceAuthority);
            // A short allowance: refused.
            let appr = approve_ix(
                src_program,
                &leg.source,
                &delegate.pubkey(),
                &owner.pubkey(),
                a - 1,
            );
            ok(env.send(&[appr], &[&owner]));
            let ix = mock::swap_exact_in_ix(&f.keys, &as_delegate, a, 0, &[]);
            assert_mock_err(&env.send(&[ix], &[&delegate]), MockError::SourceAuthority);
            // Exactly the input: accepted; the delegate clears at 0.
            let appr = approve_ix(
                src_program,
                &leg.source,
                &delegate.pubkey(),
                &owner.pubkey(),
                a,
            );
            ok(env.send(&[appr], &[&owner]));
            let q = mock::quote_exact_in(&env.svm, &f.keys, dir, a).unwrap();
            let ix = mock::swap_exact_in_ix(&f.keys, &as_delegate, a, q.proceeds, &[]);
            ok(env.send(&[ix], &[&delegate]));
            assert_eq!(amount(&env, &leg.source), 2 * a);
            assert_eq!(amount(&env, &leg.destination), q.proceeds);
            assert_eq!(read::token_delegate(&env.svm, &leg.source), None);

            // Exact out as a delegate for exactly the quoted cost.
            let out = q.proceeds / 2;
            let b = mock::quote_exact_out(&env.svm, &f.keys, dir, out).unwrap();
            let appr = approve_ix(
                src_program,
                &leg.source,
                &delegate.pubkey(),
                &owner.pubkey(),
                b.cost,
            );
            ok(env.send(&[appr], &[&owner]));
            let ix = mock::swap_exact_out_ix(&f.keys, &as_delegate, b.cost, out, &[]);
            ok(env.send(&[ix], &[&delegate]));
            assert_eq!(read::token_delegate(&env.svm, &leg.source), None);
            assert_eq!(amount(&env, &leg.source), 2 * a - b.cost);

            // A stranger is refused; the owner may still trade.
            let as_stranger = Leg {
                authority: stranger.pubkey(),
                ..leg
            };
            let ix = mock::swap_exact_in_ix(&f.keys, &as_stranger, a, 0, &[]);
            assert_mock_err(&env.send(&[ix], &[&stranger]), MockError::SourceAuthority);
            let ix = mock::swap_exact_in_ix(&f.keys, &leg, a, 0, &[]);
            ok(env.send(&[ix], &[&owner]));
        }
    }
}

// ---- knobs ---------------------------------------------------------------------------------

#[test]
fn disabled_pool_refuses_swaps() {
    let mut env = env();
    let f = seeded(
        &mut env,
        TokenProgram::Spl,
        FEE,
        10u64.pow(12),
        10u64.pow(12),
    );
    mock::update_knobs(&mut env, &f, |k| k.disabled = true);
    assert_eq!(
        read::account(&env.svm, &f.keys.pool).unwrap().data[layout::OFF_DISABLED],
        1
    );
    for dir in Dir::BOTH {
        let (res, _) = swap_in(&mut env, &f, dir, 1_000, 0);
        assert_mock_err(&res, MockError::PoolDisabled);
        let (res, _) = swap_out(&mut env, &f, dir, 1_000, u64::MAX, 10_000);
        assert_mock_err(&res, MockError::PoolDisabled);
    }
    // Liquidity still moves.
    ok(mock::add_liquidity(
        &mut env,
        &f.keys,
        &f.mint_authority,
        5,
        5,
    ));
    ok(mock::remove_liquidity(&mut env, &f, 5, 5));
    // Only the admin sets knobs.
    let intruder = env.funded_keypair();
    let ix = mock::set_knobs_ix(&f.keys.pool, &intruder.pubkey(), &Knobs::default());
    assert_mock_err(&env.send(&[ix], &[&intruder]), MockError::Unauthorized);
    mock::update_knobs(&mut env, &f, |k| k.disabled = false);
    let (res, _) = swap_in(&mut env, &f, Dir::BaseIn, 1_000, 0);
    ok(res);
}

#[test]
fn fee_change_applies_to_the_next_swap() {
    let mut env = env();
    let f = seeded(
        &mut env,
        TokenProgram::Token2022,
        FEE,
        10u64.pow(12),
        10u64.pow(12),
    );
    let a = 10u64.pow(9);
    let mut outs = Vec::new();
    for fee in [FEE, 30_000, 0, 999_999] {
        mock::update_knobs(&mut env, &f, |k| k.fee_ppm = fee);
        assert_eq!(mock::state(&env.svm, &f.keys.pool).fee_ppm, fee);
        let q = mock::quote_exact_in(&env.svm, &f.keys, Dir::BaseIn, a).unwrap();
        let (res, leg) = swap_in(&mut env, &f, Dir::BaseIn, a, q.proceeds);
        ok(res);
        assert_eq!(amount(&env, &leg.destination), q.proceeds);
        let b = mock::quote_exact_out(&env.svm, &f.keys, Dir::QuoteIn, a / 2).unwrap();
        let (res, _) = swap_out(&mut env, &f, Dir::QuoteIn, a / 2, b.cost, b.cost);
        ok(res);
        outs.push(q.proceeds);
    }
    // A higher fee pays less on a comparable pool.
    assert!(outs[1] < outs[0] && outs[3] < outs[1]);
    let bad = Knobs {
        fee_ppm: 1_000_000,
        ..mock::knobs(&env.svm, &f.keys.pool)
    };
    assert_mock_err(&mock::set_knobs(&mut env, &f, &bad), MockError::BadFee);
}

/// `layout_version = 1` moves the reserve counters (a venue-state layout change, V1-8): the
/// version-0 slots read 0, the version-1 slots hold the counters, pricing reserves are
/// unchanged, and swaps keep executing the quote. Back to 0 moves them back.
#[test]
fn layout_version_1_moves_the_counters() {
    let mut env = env();
    let f = seeded(
        &mut env,
        TokenProgram::Spl,
        30_000,
        10u64.pow(12),
        10u64.pow(12),
    );
    for dir in Dir::BOTH {
        let (res, _) = swap_in(&mut env, &f, dir, 10u64.pow(10), 0);
        ok(res);
    }
    let r0 = mock::reserves(&env.svm, &f.keys);
    assert!(r0.protocol_fee_base > 0 && r0.protocol_fee_quote > 0);
    let raw = raw_pool(&env, &f.keys.pool);
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_BASE_V0),
        r0.protocol_fee_base
    );
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_QUOTE_V0),
        r0.protocol_fee_quote
    );

    mock::update_knobs(&mut env, &f, |k| k.layout_version = 1);
    let raw = raw_pool(&env, &f.keys.pool);
    assert_eq!(raw[layout::OFF_LAYOUT_VERSION], 1);
    // A reader still on version 0 sees zero counters (and would price on the vault balances).
    assert_eq!(u64_at(&raw, layout::OFF_PROTOCOL_FEE_BASE_V0), 0);
    assert_eq!(u64_at(&raw, layout::OFF_PROTOCOL_FEE_QUOTE_V0), 0);
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_BASE_V1),
        r0.protocol_fee_base
    );
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_QUOTE_V1),
        r0.protocol_fee_quote
    );
    assert_eq!(mock::reserves(&env.svm, &f.keys), r0);

    // The venue keeps working under the new layout, and still executes the quote.
    for dir in Dir::BOTH {
        let q = mock::quote_exact_in(&env.svm, &f.keys, dir, 10u64.pow(9)).unwrap();
        let (res, leg) = swap_in(&mut env, &f, dir, 10u64.pow(9), q.proceeds);
        ok(res);
        assert_eq!(amount(&env, &leg.destination), q.proceeds);
    }
    let r1 = mock::reserves(&env.svm, &f.keys);
    assert!(r1.protocol_fee_base > r0.protocol_fee_base);
    let raw = raw_pool(&env, &f.keys.pool);
    assert_eq!(u64_at(&raw, layout::OFF_PROTOCOL_FEE_BASE_V0), 0);
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_BASE_V1),
        r1.protocol_fee_base
    );

    mock::update_knobs(&mut env, &f, |k| k.layout_version = 0);
    let raw = raw_pool(&env, &f.keys.pool);
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_BASE_V0),
        r1.protocol_fee_base
    );
    assert_eq!(
        u64_at(&raw, layout::OFF_PROTOCOL_FEE_QUOTE_V0),
        r1.protocol_fee_quote
    );
    assert_eq!(&raw[layout::OFF_PROTOCOL_FEE_BASE_V1..][..16], &[0u8; 16]);
    assert_eq!(mock::reserves(&env.svm, &f.keys), r1);

    let bad = Knobs {
        layout_version: 2,
        ..mock::knobs(&env.svm, &f.keys.pool)
    };
    assert_mock_err(
        &mock::set_knobs(&mut env, &f, &bad),
        MockError::BadLayoutVersion,
    );
}

/// `over_delivery` adds units to every exact-out output (a venue rounding or semantics change,
/// V2-10); the input is still the quote's; exact-in is unaffected.
#[test]
fn over_delivery_on_exact_out_only() {
    let mut env = env();
    for program in TokenProgram::BOTH {
        let f = seeded(&mut env, program, FEE, 10u64.pow(12), 10u64.pow(12));
        mock::update_knobs(&mut env, &f, |k| k.over_delivery = 7);
        for dir in Dir::BOTH {
            let q = 10u64.pow(9);
            let before = mock::reserves(&env.svm, &f.keys);
            let b = mock::quote_exact_out(&env.svm, &f.keys, dir, q).unwrap();
            let (res, leg) = swap_out(&mut env, &f, dir, q, b.cost, b.cost);
            ok(res);
            assert_eq!(amount(&env, &leg.destination), q + 7);
            assert_eq!(amount(&env, &leg.source), 0);
            let after = mock::reserves(&env.svm, &f.keys);
            let (i, o) = mock::io_reserves(&after, dir);
            assert_eq!(u128::from(i), b.sol_after);
            assert_eq!(u128::from(o) + 7, b.token_after);
            assert_ne!(after, before);

            let s = mock::quote_exact_in(&env.svm, &f.keys, dir, q).unwrap();
            let (res, leg) = swap_in(&mut env, &f, dir, q, s.proceeds);
            ok(res);
            assert_eq!(amount(&env, &leg.destination), s.proceeds);
        }
        // The surplus cannot take the output reserve to zero.
        let r = mock::reserves(&env.svm, &f.keys);
        mock::update_knobs(&mut env, &f, |k| k.over_delivery = r.base);
        let (res, _) = swap_out(&mut env, &f, Dir::QuoteIn, 1, u64::MAX, 10u64.pow(9));
        assert_mock_err(&res, MockError::InsufficientLiquidity);
    }
}

/// Required extras simulate an interface change that adds trailing accounts (T-REPLAY-PDA,
/// C0-4): the swap needs exactly the configured list, in order.
#[test]
fn required_extras_accepted_exactly() {
    let mut env = env();
    let f = seeded(
        &mut env,
        TokenProgram::Token2022,
        FEE,
        10u64.pow(12),
        10u64.pow(12),
    );
    // A per-mint PDA (the T-REPLAY-PDA shape) and an arbitrary account.
    let k1 =
        Pubkey::find_program_address(&[b"extra", f.keys.base_mint.as_ref()], &mock::PROGRAM_ID).0;
    let k2 = Pubkey::new_unique();
    let k3 = Pubkey::new_unique();
    mock::update_knobs(&mut env, &f, |k| k.required_extras = vec![k1, k2]);
    let raw = raw_pool(&env, &f.keys.pool);
    assert_eq!(raw[layout::OFF_REQUIRED_EXTRAS_LEN], 2);
    assert_eq!(key_at(&raw, layout::OFF_REQUIRED_EXTRAS), k1);
    assert_eq!(key_at(&raw, layout::OFF_REQUIRED_EXTRAS + 32), k2);
    assert_eq!(&raw[layout::OFF_REQUIRED_EXTRAS + 64..][..64], &[0u8; 64]);

    let ro = AccountMeta::new_readonly;
    let attempt = |env: &mut Env, extras: &[AccountMeta], exact_in: bool| {
        let dir = if exact_in { Dir::BaseIn } else { Dir::QuoteIn };
        let (owner, leg) = mock::trader(env, &f, dir, 10u64.pow(9));
        let ix = if exact_in {
            mock::swap_exact_in_ix(&f.keys, &leg, 10u64.pow(6), 0, extras)
        } else {
            mock::swap_exact_out_ix(&f.keys, &leg, 10u64.pow(9), 10u64.pow(6), extras)
        };
        env.send(&[ix], &[&owner])
    };
    for exact_in in [true, false] {
        for bad in [
            vec![],
            vec![ro(k1, false)],
            vec![ro(k2, false), ro(k1, false)],
            vec![ro(k1, false), ro(k3, false)],
            vec![ro(k1, false), ro(k2, false), ro(k3, false)],
        ] {
            assert_mock_err(
                &attempt(&mut env, &bad, exact_in),
                MockError::ExtrasMismatch,
            );
        }
        ok(attempt(&mut env, &[ro(k1, false), ro(k2, false)], exact_in));
        // Flags are free: only keys and order count.
        ok(attempt(
            &mut env,
            &[AccountMeta::new(k1, false), ro(k2, false)],
            exact_in,
        ));
    }
    // Four is the maximum; clearing the list makes any extra a mismatch.
    let four = vec![k1, k2, k3, Pubkey::new_unique()];
    mock::update_knobs(&mut env, &f, |k| k.required_extras = four.clone());
    let metas: Vec<AccountMeta> = four.iter().map(|k| ro(*k, false)).collect();
    ok(attempt(&mut env, &metas, true));
    let five = Knobs {
        required_extras: vec![k1, k2, k3, Pubkey::new_unique(), Pubkey::new_unique()],
        ..mock::knobs(&env.svm, &f.keys.pool)
    };
    assert_mock_err(
        &mock::set_knobs(&mut env, &f, &five),
        MockError::TooManyExtras,
    );
    mock::update_knobs(&mut env, &f, |k| k.required_extras.clear());
    assert_mock_err(
        &attempt(&mut env, &[ro(k1, false)], true),
        MockError::ExtrasMismatch,
    );
    ok(attempt(&mut env, &[], true));
}

// ---- Token-2022 base -----------------------------------------------------------------------

/// A Token-2022 base mint with MetadataPointer + TokenMetadata (the pump.fun `create_v2`
/// shape, R-MINT §7.2): pool, liquidity and both directions execute the quote.
#[test]
fn token_2022_base_with_metadata() {
    let mut env = env();
    let m = mints::t22_mint(
        &mut env,
        &[spl_token_2022_interface::extension::ExtensionType::TokenMetadata],
        false,
    );
    let admin = env.funded_keypair();
    let keys = mock::create_pool(&mut env, &admin, &m.mint, TokenProgram::Token2022, FEE);
    let f = PoolFixture {
        keys,
        admin,
        mint_authority: m.authority,
    };
    mock::seed(&mut env, &f, 10u64.pow(15), 10u64.pow(11));
    for dir in Dir::BOTH {
        let a = match dir {
            Dir::BaseIn => 10u64.pow(12),
            Dir::QuoteIn => 10u64.pow(8),
        };
        let q = mock::quote_exact_in(&env.svm, &f.keys, dir, a).unwrap();
        let (res, leg) = swap_in(&mut env, &f, dir, a, q.proceeds);
        ok(res);
        assert_eq!(amount(&env, &leg.destination), q.proceeds);
        let after = mock::reserves(&env.svm, &f.keys);
        let (i, o) = mock::io_reserves(&after, dir);
        assert_eq!((u128::from(i), u128::from(o)), (q.token_after, q.sol_after));

        let out = q.proceeds / 3;
        let b = mock::quote_exact_out(&env.svm, &f.keys, dir, out).unwrap();
        let (res, leg) = swap_out(&mut env, &f, dir, out, b.cost, b.cost);
        ok(res);
        assert_eq!(amount(&env, &leg.destination), out);
        let after = mock::reserves(&env.svm, &f.keys);
        let (i, o) = mock::io_reserves(&after, dir);
        assert_eq!((u128::from(i), u128::from(o)), (b.sol_after, b.token_after));
    }
}

// ---- account validation and liquidity --------------------------------------------------------

#[test]
fn swap_account_validation() {
    let mut env = env();
    let f = seeded(
        &mut env,
        TokenProgram::Spl,
        FEE,
        10u64.pow(12),
        10u64.pow(12),
    );
    let g = seeded(
        &mut env,
        TokenProgram::Spl,
        FEE,
        10u64.pow(12),
        10u64.pow(12),
    );
    let (owner, leg) = mock::trader(&mut env, &f, Dir::BaseIn, 10u64.pow(6));
    let send = |env: &mut Env, ix: Instruction| env.send(&[ix], &[&owner]);
    let base_ix = || mock::swap_exact_in_ix(&f.keys, &leg, 10u64.pow(6), 0, &[]);

    // Another pool's quote vault as the output vault.
    let mut ix = base_ix();
    ix.accounts[6].pubkey = g.keys.quote_vault;
    assert_mock_err(&send(&mut env, ix), MockError::VaultMismatch);
    // Vaults in swapped roles: the input mint no longer matches the input vault.
    let mut ix = base_ix();
    ix.accounts.swap(5, 6);
    assert_mock_err(&send(&mut env, ix), MockError::MintMismatch);
    // The pool's own vault as the destination (Anchor 1.x refuses the duplicate writable
    // account before the mock's own `VaultAsUserAccount` check).
    let mut ix = base_ix();
    ix.accounts[4].pubkey = f.keys.quote_vault;
    assert_anchor_err(
        &send(&mut env, ix),
        anchor_lang::error::ErrorCode::ConstraintDuplicateMutableAccount,
    );
    // Another pool's account in the pool slot: its vaults do not match.
    let mut ix = base_ix();
    ix.accounts[1].pubkey = g.keys.pool;
    assert_mock_err(&send(&mut env, ix), MockError::VaultMismatch);
    // A non-pool account in the pool slot.
    let mut ix = base_ix();
    ix.accounts[1].pubkey = f.keys.base_vault;
    assert_anchor_err(
        &send(&mut env, ix),
        anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram,
    );
    // A wrong pool authority.
    let mut ix = base_ix();
    ix.accounts[2].pubkey = Pubkey::new_unique();
    assert_anchor_err(
        &send(&mut env, ix),
        anchor_lang::error::ErrorCode::ConstraintSeeds,
    );
    // A destination of the wrong mint.
    let mut ix = base_ix();
    ix.accounts[4].pubkey = leg.source;
    assert_failed(&send(&mut env, ix));
    // The wrong token program for the input side.
    let mut ix = base_ix();
    ix.accounts[9].pubkey = TokenProgram::Token2022.id();
    assert_failed(&send(&mut env, ix));
    // The untouched instruction succeeds.
    ok(send(&mut env, base_ix()));

    // A vault below its counter (a drained vault; direct write): swaps fail as on CPMM.
    let r = mock::reserves(&env.svm, &f.keys);
    assert!(r.protocol_fee_base > 0);
    mints::edit_account(&mut env, &f.keys.base_vault, |a| {
        let v = r.protocol_fee_base - 1;
        a.data[64..72].copy_from_slice(&v.to_le_bytes());
    });
    let (res, _) = swap_in(&mut env, &f, Dir::BaseIn, 10u64.pow(6), 0);
    assert_mock_err(&res, MockError::InsufficientVault);
}

#[test]
fn liquidity_add_and_remove() {
    let mut env = env();
    let f = mock::new_pool(&mut env, TokenProgram::Token2022, 6, FEE);
    // Anyone adds; zero is refused; either side alone is fine.
    assert_mock_err(
        &mock::add_liquidity(&mut env, &f.keys, &f.mint_authority, 0, 0),
        MockError::ZeroAmount,
    );
    ok(mock::add_liquidity(
        &mut env,
        &f.keys,
        &f.mint_authority,
        1_000,
        0,
    ));
    ok(mock::add_liquidity(
        &mut env,
        &f.keys,
        &f.mint_authority,
        0,
        2_000,
    ));
    let r = mock::reserves(&env.svm, &f.keys);
    assert_eq!((r.base, r.quote), (1_000, 2_000));
    // Wrong vault.
    let depositor = env.funded_keypair();
    let bs = mock::base_account(
        &mut env,
        &f.keys,
        &depositor.pubkey(),
        &f.mint_authority,
        10,
    );
    let qs = mock::wsol_account(&mut env, &depositor.pubkey(), 10);
    let decoy = mock::base_account(
        &mut env,
        &f.keys,
        &mock::authority().0,
        &f.mint_authority,
        0,
    );
    let mut ix = mock::add_liquidity_ix(&f.keys, &depositor.pubkey(), &bs, &qs, 10, 10);
    ix.accounts[4].pubkey = decoy;
    assert_mock_err(&env.send(&[ix], &[&depositor]), MockError::VaultMismatch);

    // Only the admin removes, at most the reserves.
    assert_mock_err(
        &mock::remove_liquidity(&mut env, &f, 1_001, 0),
        MockError::InsufficientLiquidity,
    );
    let intruder = PoolFixture {
        keys: f.keys,
        admin: env.funded_keypair(),
        mint_authority: f.mint_authority.insecure_clone(),
    };
    assert_mock_err(
        &mock::remove_liquidity(&mut env, &intruder, 1, 1),
        MockError::Unauthorized,
    );
    ok(mock::remove_liquidity(&mut env, &f, 400, 2_000));
    let r = mock::reserves(&env.svm, &f.keys);
    assert_eq!((r.base, r.quote), (600, 0));
    // An empty side cannot trade.
    let (res, _) = swap_in(&mut env, &f, Dir::BaseIn, 10, 0);
    assert_mock_err(&res, MockError::EmptyPool);
}

/// The decoded state is what the raw bytes say (the reader of INTERFACE.md).
#[test]
fn state_decodes_the_raw_account() {
    let mut env = env();
    let f = seeded(&mut env, TokenProgram::Spl, FEE, 10u64.pow(9), 10u64.pow(9));
    let extras = vec![Pubkey::new_unique()];
    mock::update_knobs(&mut env, &f, |k| {
        k.over_delivery = 3;
        k.required_extras = extras.clone();
    });
    let raw = raw_pool(&env, &f.keys.pool);
    let s = PoolState::decode(&raw).unwrap();
    assert_eq!(s, mock::state(&env.svm, &f.keys.pool));
    assert_eq!(u64_at(&raw, layout::OFF_OVER_DELIVERY), 3);
    assert_eq!(s.extras(), extras.as_slice());
}

/// CU and instruction trace of a swap (T-TRACE / §6.2 inputs for the M2 engine): the mock
/// adds 1 entry plus its 2 `transfer_checked` CPIs, as CPMM does.
#[test]
fn swap_cost_and_trace() {
    let mut env = env();
    for program in TokenProgram::BOTH {
        let f = seeded(&mut env, program, FEE, 10u64.pow(12), 10u64.pow(12));
        for dir in Dir::BOTH {
            for exact_in in [true, false] {
                let (res, _) = if exact_in {
                    swap_in(&mut env, &f, dir, 10u64.pow(6), 0)
                } else {
                    swap_out(&mut env, &f, dir, 10u64.pow(6), u64::MAX, 10u64.pow(7))
                };
                let meta = ok(res);
                let inner = meta.inner_instructions.first().map_or(0, Vec::len);
                println!(
                    "{program:?} {dir:?} exact_in={exact_in}: {} CU, {inner} inner",
                    meta.compute_units_consumed
                );
                assert_eq!(inner, 2);
                assert!(
                    meta.compute_units_consumed <= SWAP_CU_MAX,
                    "{}",
                    meta.compute_units_consumed
                );
            }
        }
    }
}
