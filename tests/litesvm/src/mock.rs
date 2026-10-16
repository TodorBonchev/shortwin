//! The TEST-ONLY mock venue `mock_cpamm` (`tests/mock_cpamm`, M2; A49): loading its SBF
//! build, pools on an SPL Token or Token-2022 base mint against WSOL, liquidity, knobs,
//! reserves and swap instructions. The interface (account orders, byte offsets, errors,
//! pricing) is `tests/mock_cpamm/INTERFACE.md`.
//!
//! - [`Env::load_mock_cpamm`]: deploys `target/deploy/mock_cpamm.so` (or `$MOCK_CPAMM_SO`)
//!   at [`PROGRAM_ID`].
//! - [`new_pool`] / [`create_pool`]: a pool for a new or a given base mint; [`seed`] /
//!   [`add_liquidity`], [`remove_liquidity`]; [`set_knobs`] / [`knobs`]; [`state`],
//!   [`reserves`].
//! - [`swap_exact_in_ix`] / [`swap_exact_out_ix`] with a [`Leg`]; [`wsol_account`] and
//!   [`base_account`] for trader token accounts.
//! - [`cpmm_fees`], [`quote_exact_in`], [`quote_exact_out`]: short.win's `quote::cpmm` view of
//!   a pool (a zero creator fee), which the mock executes exactly.
//!
//! Every instruction goes through [`Env::send`] (the invariant hooks); token-account setup
//! uses LiteSVM directly, as [`crate::tokens`] does.

use std::path::PathBuf;

use anchor_lang::InstructionData;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use litesvm_token::SyncNative;
use mock_cpamm::errors::MockError;
use mock_cpamm::state::{AUTHORITY_SEED, POOL_SEED, PoolState, VAULT_SEED};
use shortwin_math::fees::CpmmFees;
use shortwin_math::quote::{BuyQuote, SellQuote, cpmm};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::env::send_direct;
use crate::errors::custom_code;
use crate::fixtures::mints;
use crate::tokens::{self, TokenProgram};
use crate::{Env, TxResult, read};

pub use mock_cpamm::ID as PROGRAM_ID;
pub use mock_cpamm::WSOL_MINT;
pub use mock_cpamm::math::PROTOCOL_FEE_RATE;
pub use mock_cpamm::state as layout;

/// Path of the mock's SBF build: `$MOCK_CPAMM_SO`, else `target/deploy/mock_cpamm.so` (the
/// output of `anchor build --tools-version v1.57 --arch v3`, which builds it next to
/// short.win).
pub fn so_path() -> PathBuf {
    std::env::var_os("MOCK_CPAMM_SO").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/deploy/mock_cpamm.so"),
        PathBuf::from,
    )
}

/// Deploys the mock at [`PROGRAM_ID`] into `svm` (idempotent).
pub fn load(svm: &mut LiteSVM) {
    let path = so_path();
    let so = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. Build it first: `anchor build --tools-version v1.57 --arch v3` \
             (or set MOCK_CPAMM_SO)",
            path.display()
        )
    });
    svm.add_program(PROGRAM_ID, &so)
        .unwrap_or_else(|e| panic!("add_program(mock_cpamm): {e:?}"));
}

impl Env {
    /// Deploys the mock venue ([`load`]) and writes the SPL Token native mint (WSOL), which
    /// LiteSVM does not create.
    pub fn load_mock_cpamm(&mut self) {
        load(&mut self.svm);
        ensure_wsol_mint(self);
    }
}

/// Writes the WSOL mint if it does not exist yet.
pub fn ensure_wsol_mint(env: &mut Env) {
    if env.svm.get_account(&WSOL_MINT).is_none() {
        mints::native_mint(env);
    }
}

// ---- addresses ---------------------------------------------------------------------------

fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &PROGRAM_ID)
}

/// The pool of `base_mint` against WSOL: `["pool", base_mint, WSOL]`.
pub fn pool_address(base_mint: &Pubkey) -> (Pubkey, u8) {
    pda(&[POOL_SEED, base_mint.as_ref(), WSOL_MINT.as_ref()])
}

/// The global vault authority `["authority"]`.
pub fn authority() -> (Pubkey, u8) {
    pda(&[AUTHORITY_SEED])
}

/// A vault `["vault", pool, mint]`.
pub fn vault_address(pool: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    pda(&[VAULT_SEED, pool.as_ref(), mint.as_ref()])
}

// ---- pools -------------------------------------------------------------------------------

/// A pool's keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockPool {
    pub pool: Pubkey,
    pub base_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_program: TokenProgram,
}

impl MockPool {
    /// The keys of the pool of `base_mint` (derived; the pool need not exist).
    pub fn derive(base_mint: &Pubkey, base_program: TokenProgram) -> Self {
        let pool = pool_address(base_mint).0;
        Self {
            pool,
            base_mint: *base_mint,
            base_vault: vault_address(&pool, base_mint).0,
            quote_vault: vault_address(&pool, &WSOL_MINT).0,
            base_program,
        }
    }

    /// `(input vault, output vault, input mint, output mint, input program, output program)`
    /// of a direction.
    fn sides(&self, dir: Dir) -> (Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey) {
        let spl = TokenProgram::Spl.id();
        let base = self.base_program.id();
        match dir {
            Dir::BaseIn => (
                self.base_vault,
                self.quote_vault,
                self.base_mint,
                WSOL_MINT,
                base,
                spl,
            ),
            Dir::QuoteIn => (
                self.quote_vault,
                self.base_vault,
                WSOL_MINT,
                self.base_mint,
                spl,
                base,
            ),
        }
    }
}

/// A pool with its admin and the base mint's authority.
pub struct PoolFixture {
    pub keys: MockPool,
    /// Pool admin (signs `set_knobs`, `remove_liquidity`).
    pub admin: Keypair,
    /// Mint authority of the base mint.
    pub mint_authority: Keypair,
}

/// `initialize_pool(fee_ppm)` for `base_mint` (`admin` pays and becomes the admin).
pub fn initialize_pool_ix(
    admin: &Pubkey,
    base_mint: &Pubkey,
    base_program: TokenProgram,
    fee_ppm: u32,
) -> Instruction {
    let k = MockPool::derive(base_mint, base_program);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*admin, true),
            AccountMeta::new_readonly(*base_mint, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new(k.pool, false),
            AccountMeta::new_readonly(authority().0, false),
            AccountMeta::new(k.base_vault, false),
            AccountMeta::new(k.quote_vault, false),
            AccountMeta::new_readonly(base_program.id(), false),
            AccountMeta::new_readonly(TokenProgram::Spl.id(), false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: mock_cpamm::instruction::InitializePool { fee_ppm }.data(),
    }
}

/// Creates the pool of an existing `base_mint`; panics on failure.
pub fn create_pool(
    env: &mut Env,
    admin: &Keypair,
    base_mint: &Pubkey,
    base_program: TokenProgram,
    fee_ppm: u32,
) -> MockPool {
    ensure_wsol_mint(env);
    let ix = initialize_pool_ix(&admin.pubkey(), base_mint, base_program, fee_ppm);
    crate::errors::ok(env.send(&[ix], &[admin]));
    MockPool::derive(base_mint, base_program)
}

/// A new base mint of `program` (no extensions; `decimals`) and its pool; no liquidity.
pub fn new_pool(env: &mut Env, program: TokenProgram, decimals: u8, fee_ppm: u32) -> PoolFixture {
    let mint_authority = env.funded_keypair();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint(
        &mut env.svm,
        &payer,
        &mint_authority.pubkey(),
        decimals,
        program,
    );
    let admin = env.funded_keypair();
    let keys = create_pool(env, &admin, &mint, program, fee_ppm);
    PoolFixture {
        keys,
        admin,
        mint_authority,
    }
}

/// `add_liquidity(base, quote)` from `depositor`'s token accounts.
pub fn add_liquidity_ix(
    pool: &MockPool,
    depositor: &Pubkey,
    base_source: &Pubkey,
    quote_source: &Pubkey,
    base: u64,
    quote: u64,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*depositor, true),
            AccountMeta::new_readonly(pool.pool, false),
            AccountMeta::new(*base_source, false),
            AccountMeta::new(*quote_source, false),
            AccountMeta::new(pool.base_vault, false),
            AccountMeta::new(pool.quote_vault, false),
            AccountMeta::new_readonly(pool.base_mint, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new_readonly(pool.base_program.id(), false),
            AccountMeta::new_readonly(TokenProgram::Spl.id(), false),
        ],
        data: mock_cpamm::instruction::AddLiquidity { base, quote }.data(),
    }
}

/// `remove_liquidity(base, quote)` to `admin`-chosen destinations.
pub fn remove_liquidity_ix(
    pool: &MockPool,
    admin: &Pubkey,
    base_destination: &Pubkey,
    quote_destination: &Pubkey,
    base: u64,
    quote: u64,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new_readonly(pool.pool, false),
            AccountMeta::new_readonly(authority().0, false),
            AccountMeta::new(pool.base_vault, false),
            AccountMeta::new(pool.quote_vault, false),
            AccountMeta::new(*base_destination, false),
            AccountMeta::new(*quote_destination, false),
            AccountMeta::new_readonly(pool.base_mint, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new_readonly(pool.base_program.id(), false),
            AccountMeta::new_readonly(TokenProgram::Spl.id(), false),
        ],
        data: mock_cpamm::instruction::RemoveLiquidity { base, quote }.data(),
    }
}

/// Adds `base` and `quote` to the pool from a fresh funded depositor (base minted by
/// `mint_authority`, quote wrapped from SOL).
pub fn add_liquidity(
    env: &mut Env,
    pool: &MockPool,
    mint_authority: &Keypair,
    base: u64,
    quote: u64,
) -> TxResult {
    let depositor = env.funded_keypair();
    let base_source = base_account(env, pool, &depositor.pubkey(), mint_authority, base);
    let quote_source = wsol_account(env, &depositor.pubkey(), quote);
    let ix = add_liquidity_ix(
        pool,
        &depositor.pubkey(),
        &base_source,
        &quote_source,
        base,
        quote,
    );
    env.send(&[ix], &[&depositor])
}

/// [`add_liquidity`] that must succeed.
pub fn seed(env: &mut Env, fixture: &PoolFixture, base: u64, quote: u64) {
    crate::errors::ok(add_liquidity(
        env,
        &fixture.keys,
        &fixture.mint_authority,
        base,
        quote,
    ));
}

/// The admin pulls `base` and `quote` out of the pool into new accounts it owns.
pub fn remove_liquidity(env: &mut Env, fixture: &PoolFixture, base: u64, quote: u64) -> TxResult {
    let admin = fixture.admin.pubkey();
    let payer = env.payer.insecure_clone();
    let base_dst = tokens::create_token_account(
        &mut env.svm,
        &payer,
        &fixture.keys.base_mint,
        &admin,
        fixture.keys.base_program,
    );
    let quote_dst = wsol_account(env, &admin, 0);
    let ix = remove_liquidity_ix(&fixture.keys, &admin, &base_dst, &quote_dst, base, quote);
    let signer = fixture.admin.insecure_clone();
    env.send(&[ix], &[&signer])
}

// ---- knobs and state ---------------------------------------------------------------------

/// Every knob of `set_knobs` (INTERFACE.md "set_knobs").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Knobs {
    pub disabled: bool,
    pub fee_ppm: u32,
    pub layout_version: u8,
    pub over_delivery: u64,
    pub required_extras: Vec<Pubkey>,
}

/// `set_knobs(..)` signed by `admin`.
pub fn set_knobs_ix(pool: &Pubkey, admin: &Pubkey, k: &Knobs) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new(*pool, false),
        ],
        data: mock_cpamm::instruction::SetKnobs {
            disabled: k.disabled,
            fee_ppm: k.fee_ppm,
            layout_version: k.layout_version,
            over_delivery: k.over_delivery,
            required_extras: k.required_extras.clone(),
        }
        .data(),
    }
}

/// Sets the pool's knobs (the fixture's admin signs).
pub fn set_knobs(env: &mut Env, fixture: &PoolFixture, k: &Knobs) -> TxResult {
    let ix = set_knobs_ix(&fixture.keys.pool, &fixture.admin.pubkey(), k);
    let admin = fixture.admin.insecure_clone();
    env.send(&[ix], &[&admin])
}

/// Reads the knobs, changes them with `f` and writes them back; panics on failure.
pub fn update_knobs(env: &mut Env, fixture: &PoolFixture, f: impl FnOnce(&mut Knobs)) {
    let mut k = knobs(&env.svm, &fixture.keys.pool);
    f(&mut k);
    crate::errors::ok(set_knobs(env, fixture, &k));
}

/// The decoded pool (panics if it is not a mock pool).
pub fn state(svm: &LiteSVM, pool: &Pubkey) -> PoolState {
    let acc = svm
        .get_account(pool)
        .unwrap_or_else(|| panic!("no mock pool {pool}"));
    assert_eq!(acc.owner, PROGRAM_ID, "{pool} is not a mock pool");
    assert_eq!(
        acc.data[..8],
        layout::POOL_DISCRIMINATOR,
        "{pool}: discriminator"
    );
    PoolState::decode(&acc.data).unwrap_or_else(|e| panic!("decode {pool}: {e:?}"))
}

/// The pool's knobs.
pub fn knobs(svm: &LiteSVM, pool: &Pubkey) -> Knobs {
    let s = state(svm, pool);
    Knobs {
        disabled: s.disabled,
        fee_ppm: s.fee_ppm,
        layout_version: s.layout_version,
        over_delivery: s.over_delivery,
        required_extras: s.extras().to_vec(),
    }
}

/// Vault balances, counters and pricing reserves (`vault − counter`) of a pool, read with
/// the layout its `layout_version` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reserves {
    pub base_vault: u64,
    pub quote_vault: u64,
    pub protocol_fee_base: u64,
    pub protocol_fee_quote: u64,
    /// Base pricing reserve `X` (token).
    pub base: u64,
    /// Quote pricing reserve `Y` (SOL).
    pub quote: u64,
}

/// The pool's reserves.
pub fn reserves(svm: &LiteSVM, pool: &MockPool) -> Reserves {
    let s = state(svm, &pool.pool);
    let amount = |k: &Pubkey| read::token_amount(svm, k).unwrap_or_else(|| panic!("vault {k}"));
    let base_vault = amount(&pool.base_vault);
    let quote_vault = amount(&pool.quote_vault);
    Reserves {
        base_vault,
        quote_vault,
        protocol_fee_base: s.protocol_fee_base,
        protocol_fee_quote: s.protocol_fee_quote,
        base: base_vault
            .checked_sub(s.protocol_fee_base)
            .expect("base vault below its counter"),
        quote: quote_vault
            .checked_sub(s.protocol_fee_quote)
            .expect("quote vault below its counter"),
    }
}

// ---- trader accounts ---------------------------------------------------------------------

/// Refills LiteSVM's airdrop account when it runs low (large WSOL reserves in the proptests
/// would otherwise drain its 10⁶ SOL). Test setup only.
fn ensure_airdrop(svm: &mut LiteSVM, lamports: u64) {
    let key = svm.airdrop_pubkey();
    let have = svm.get_account(&key).map_or(0, |a| a.lamports);
    if have < lamports.saturating_add(1_000_000_000_000) {
        svm.set_account(
            key,
            Account {
                lamports: u64::MAX / 4,
                data: vec![],
                owner: anchor_lang::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap_or_else(|e| panic!("refill airdrop: {e:?}"));
    }
}

/// A new WSOL token account (not an ATA) owned by `owner` holding `amount` wrapped lamports.
pub fn wsol_account(env: &mut Env, owner: &Pubkey, amount: u64) -> Pubkey {
    ensure_wsol_mint(env);
    let payer = env.payer.insecure_clone();
    let acc =
        tokens::create_token_account(&mut env.svm, &payer, &WSOL_MINT, owner, TokenProgram::Spl);
    if amount > 0 {
        wrap(env, &acc, amount);
    }
    acc
}

/// Adds `amount` wrapped lamports to the WSOL account `acc` (lamports + `SyncNative`). Both
/// sends go through [`send_direct`]: a `SyncNative` of `acc` is the same transaction on every
/// call, so each must run on a fresh blockhash (FUZZ-M7-24H-2).
pub fn wrap(env: &mut Env, acc: &Pubkey, amount: u64) {
    ensure_airdrop(&mut env.svm, amount);
    send_direct(&mut env.svm, |svm| svm.airdrop(acc, amount))
        .unwrap_or_else(|e| panic!("airdrop to {acc}: {:?}", e.err));
    let payer = env.payer.insecure_clone();
    let spl = TokenProgram::Spl.id();
    send_direct(&mut env.svm, |svm| {
        SyncNative::new(svm, &payer, acc)
            .token_program_id(&spl)
            .send()
    })
    .unwrap_or_else(|e| panic!("sync_native: {:?}", e.err));
}

/// A new base-mint token account owned by `owner` with `amount` minted by `mint_authority`.
pub fn base_account(
    env: &mut Env,
    pool: &MockPool,
    owner: &Pubkey,
    mint_authority: &Keypair,
    amount: u64,
) -> Pubkey {
    let payer = env.payer.insecure_clone();
    let acc = tokens::create_token_account(
        &mut env.svm,
        &payer,
        &pool.base_mint,
        owner,
        pool.base_program,
    );
    if amount > 0 {
        tokens::mint_to(
            &mut env.svm,
            &payer,
            &pool.base_mint,
            &acc,
            mint_authority,
            amount,
            pool.base_program,
        );
    }
    acc
}

// ---- swaps -------------------------------------------------------------------------------

/// Swap direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Base in, WSOL out (short.win's open leg).
    BaseIn,
    /// WSOL in, base out (short.win's exit legs).
    QuoteIn,
}

impl Dir {
    /// Both directions.
    pub const BOTH: [Self; 2] = [Self::BaseIn, Self::QuoteIn];
}

/// The trader side of a swap: who signs, and the token accounts.
#[derive(Clone, Copy, Debug)]
pub struct Leg {
    pub dir: Dir,
    /// Owner or delegate of `source`.
    pub authority: Pubkey,
    pub source: Pubkey,
    pub destination: Pubkey,
}

/// The 11 fixed swap accounts, in the binding order (INTERFACE.md "Swap accounts").
pub fn swap_accounts(pool: &MockPool, leg: &Leg) -> Vec<AccountMeta> {
    let (in_vault, out_vault, in_mint, out_mint, in_prog, out_prog) = pool.sides(leg.dir);
    vec![
        AccountMeta::new_readonly(leg.authority, true),
        AccountMeta::new(pool.pool, false),
        AccountMeta::new_readonly(authority().0, false),
        AccountMeta::new(leg.source, false),
        AccountMeta::new(leg.destination, false),
        AccountMeta::new(in_vault, false),
        AccountMeta::new(out_vault, false),
        AccountMeta::new_readonly(in_mint, false),
        AccountMeta::new_readonly(out_mint, false),
        AccountMeta::new_readonly(in_prog, false),
        AccountMeta::new_readonly(out_prog, false),
    ]
}

/// `swap_exact_in(amount_in, min_out)` with `extras` appended as remaining accounts.
pub fn swap_exact_in_ix(
    pool: &MockPool,
    leg: &Leg,
    amount_in: u64,
    min_out: u64,
    extras: &[AccountMeta],
) -> Instruction {
    let mut accounts = swap_accounts(pool, leg);
    accounts.extend_from_slice(extras);
    Instruction {
        program_id: PROGRAM_ID,
        accounts,
        data: mock_cpamm::instruction::SwapExactIn { amount_in, min_out }.data(),
    }
}

/// `swap_exact_out(max_in, amount_out)` with `extras` appended as remaining accounts.
pub fn swap_exact_out_ix(
    pool: &MockPool,
    leg: &Leg,
    max_in: u64,
    amount_out: u64,
    extras: &[AccountMeta],
) -> Instruction {
    let mut accounts = swap_accounts(pool, leg);
    accounts.extend_from_slice(extras);
    Instruction {
        program_id: PROGRAM_ID,
        accounts,
        data: mock_cpamm::instruction::SwapExactOut { max_in, amount_out }.data(),
    }
}

/// A trader with a funded source and an empty destination for `dir` (`amount` in the
/// source). The trader owns both accounts.
pub fn trader(env: &mut Env, fixture: &PoolFixture, dir: Dir, amount: u64) -> (Keypair, Leg) {
    let owner = env.funded_keypair();
    let base = |env: &mut Env, amt| {
        base_account(
            env,
            &fixture.keys,
            &owner.pubkey(),
            &fixture.mint_authority,
            amt,
        )
    };
    let (source, destination) = match dir {
        Dir::BaseIn => {
            let s = base(env, amount);
            (s, wsol_account(env, &owner.pubkey(), 0))
        }
        Dir::QuoteIn => {
            let s = wsol_account(env, &owner.pubkey(), amount);
            (s, base(env, 0))
        }
    };
    let leg = Leg {
        dir,
        authority: owner.pubkey(),
        source,
        destination,
    };
    (owner, leg)
}

// ---- short.win's view ----------------------------------------------------------------------

/// short.win's `quote::cpmm` fees for a mock pool: trade fee `fee_ppm`, **zero creator fee**
/// (on the output side, so `fee_ppm = 0` is defined too), protocol share
/// [`PROTOCOL_FEE_RATE`], no fund share. With these the mock executes the quote exactly.
pub fn cpmm_fees(fee_ppm: u32) -> CpmmFees {
    CpmmFees {
        trade: u64::from(fee_ppm),
        creator: 0,
        creator_on_input: false,
        protocol: PROTOCOL_FEE_RATE,
        fund: 0,
    }
}

/// `(input reserve, output reserve)` of a direction.
pub fn io_reserves(r: &Reserves, dir: Dir) -> (u64, u64) {
    match dir {
        Dir::BaseIn => (r.base, r.quote),
        Dir::QuoteIn => (r.quote, r.base),
    }
}

/// The exact-in quote on the pool's live state (`sell_exact_in` is direction-agnostic:
/// input reserve, output reserve).
pub fn quote_exact_in(
    svm: &LiteSVM,
    pool: &MockPool,
    dir: Dir,
    amount_in: u64,
) -> Option<SellQuote> {
    let (i, o) = io_reserves(&reserves(svm, pool), dir);
    let fee = state(svm, &pool.pool).fee_ppm;
    cpmm::sell_exact_in(amount_in, u128::from(i), u128::from(o), &cpmm_fees(fee))
}

/// The exact-out quote on the pool's live state (`buy_exact_out`: input reserve, output
/// reserve).
pub fn quote_exact_out(
    svm: &LiteSVM,
    pool: &MockPool,
    dir: Dir,
    amount_out: u64,
) -> Option<BuyQuote> {
    let (i, o) = io_reserves(&reserves(svm, pool), dir);
    let fee = state(svm, &pool.pool).fee_ppm;
    cpmm::buy_exact_out(amount_out, u128::from(i), u128::from(o), &cpmm_fees(fee))
}

// ---- errors --------------------------------------------------------------------------------

/// Asserts the transaction failed with the mock error `expected`.
#[track_caller]
pub fn assert_mock_err(res: &TxResult, expected: MockError) {
    let want = u32::from(expected);
    let logs = match res {
        Ok(m) => m.pretty_logs(),
        Err(f) => f.meta.pretty_logs(),
    };
    assert_eq!(
        custom_code(res),
        Some(want),
        "expected mock {expected:?} ({want}); logs:\n{logs}"
    );
}

/// Whether the failure is one of the mock's own errors (not a token-program or runtime one).
pub fn is_mock_err(res: &TxResult) -> bool {
    custom_code(res).is_some_and(|c| (6000..6000 + 64).contains(&c))
}
