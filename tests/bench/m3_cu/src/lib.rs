//! M3 CU bench, T-TRACE and stack depth on Raydium (ARCHITECTURE §6.1, §6.2, §6.3, §8.2
//! T-RAY-TRACE / T-RAY-CU / T-CU / T-TRACE, §9 M3).
//!
//! Every [`Case`] builds its pre-state with the LiteSVM harness on the **mainnet fork**
//! (`shortwin_litesvm_tests::fork` / `raydium`: the **release** short.win build —
//! `anchor build --tools-version v1.57 --arch v3`, or `SHORTWIN_SO` — the dumped AMM v4 and CPMM
//! programs deployed from their mainnet ProgramData, the dumped pools, mints and configs, the
//! SPL Token (p-token under the mainnet feature set) ELF LiteSVM bundles, mainnet's Token-2022
//! (since M10, `fork::load_mainnet_spl`; `SHORTWIN_BUNDLED_SPL=1` keeps LiteSVM's), the clock at
//! the fixtures' snapshot), then
//!
//! 1. copies every account the instruction names into a fresh Mollusk 0.15.1 and loads the same
//!    ELFs into its program cache (short.win, both Raydium programs, the token programs), with
//!    LiteSVM's clock and rent;
//! 2. runs the instruction on Mollusk: the figure the budget applies to;
//! 3. runs the same instruction on LiteSVM through `Env::send` (the invariant hooks run too): a
//!    cross-check of the two harnesses (they must agree to the unit), the instruction's trace
//!    entries (the instruction plus every CPI at any depth, ARCH §6.2), its peak stack height
//!    (ARCH §6.3) and the venue CPI's own CU (the runtime's `consumed` log line of the Raydium
//!    program, its token CPIs included).
//!
//! Every case must execute on both harnesses (a refusal fails the bench). The keeper exits are
//! prepared the way a keeper decides: the state is advanced second by second until a
//! `simulate_transaction` of the keeper's `[crank_price, exit]` succeeds (nothing is committed by
//! the simulation), so a case measures the instruction's executing path, not a refusal.
//!
//! The worst-case transactions of T-TRACE are [`worst_case_transactions`]; the stack-depth runs
//! through the TEST-ONLY wrapper program (`wrapper/`, [`WRAPPER_ID`]) are [`depth_cases`].
//!
//! **M4 (Raydium pool selection):** `propose_pool` and `execute_pool_switch` join every table
//! (section "M4" below): a list-only market's adoption (Fartcoin with its real mainnet curve, so
//! the D1 detection runs; USELESS), a listing-window replacement of Fartcoin's thin reversed pool
//! by its main pool, zero-position adoptions across the programs, a fast-path window opened and
//! improved on a halted incumbent with a position open, and its execution (adoption with the
//! ring rewrite, and a cancellation). Candidates the fixtures lack are **planted** pools
//! (`shortwin_litesvm_tests::pool_select::plant_cpmm`: a dumped pool's bytes on fresh keys, which
//! the dumped ELF trades). Every ring is full (`RING_LEN` integrated seconds) before an adoption,
//! so the rewrite is measured at its largest.

use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::types::TransactionMetadata;
use mollusk_svm::Mollusk;
use mollusk_svm::program::keyed_account_for_system_program;
use shortwin::constants::{
    GUARD_PUMP, GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4, RAYDIUM_CPMM_PROGRAM_ID,
    RAYDIUM_V4_PROGRAM_ID, TOKEN_PROGRAM_ID, VENUE_PROGRAM_DATA, WSOL_MINT,
};
use shortwin::events::SolSettle;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::fork::{
    self, AnyPool, CpmmFixture, FIXTURE_GUARD_SLOTS, RaydiumProgram, V4Fixture,
};
use shortwin_litesvm_tests::pool_select::{
    detection_set, execute_pool_switch_ix, plant_cpmm, propose_pool_ix, spec_like,
};
use shortwin_litesvm_tests::raydium::{
    RayFixture, RayMarket, RaySpec, candidate_read_set, create_ray_market_ix, fartcoin_curve_bytes,
    lp_counter, lp_mint, mint_supply, plant_curve, set_mint_supply, supply_for_lambda,
};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{Env, TxResult, events, fixtures, ix};
use shortwin_math::fees::Direction;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_rent::Rent;
use solana_signer::Signer;

// ---- fixtures -------------------------------------------------------------------------------

/// Fartcoin's AMM v4 pool `Bzc9NZfM…` (SPL Token; AMM v4 is SPL Token only).
pub const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
/// USELESS's CPMM pool `Q2sPHPdU…` (SPL Token base, WSOL as token 0).
pub const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
/// The largest Token-2022 CPMM pool `7ZRtFcgf…` (WSOL token 0, Token-2022 token 1).
pub const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
/// The creator-fee CPMM pool `FQd8z3…` (creator fee on the SOL side).
pub const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);
/// A creator-fee pool charging both sides.
pub const CREATOR_BOTH: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorBoth);
/// WSOL as token 1 (`3Cv9uPpa…`); its LP is burned first (not eligible as dumped).
pub const WSOL_T1: RayFixture = RayFixture::Cpmm(CpmmFixture::WsolToken1);

/// Both programs and both token programs: AMM v4 SPL, CPMM SPL, CPMM Token-2022.
pub const VARIANTS: [RayFixture; 3] = [FART, USELESS, T22];

/// A fixture's name in the reports.
pub fn label(f: RayFixture) -> &'static str {
    match f {
        RayFixture::V4(V4Fixture::Fartcoin) => "AMM v4, SPL (Fartcoin)",
        RayFixture::V4(V4Fixture::FartcoinReversed) => "AMM v4, SPL (Fartcoin, reversed)",
        RayFixture::Cpmm(CpmmFixture::Useless) => "CPMM, SPL (USELESS)",
        RayFixture::Cpmm(CpmmFixture::Token2022) => "CPMM, Token-2022 base",
        RayFixture::Cpmm(CpmmFixture::CreatorFee) => "CPMM, creator fee on the SOL side",
        RayFixture::Cpmm(CpmmFixture::CreatorBoth) => "CPMM, creator fee on both sides",
        RayFixture::Cpmm(CpmmFixture::WsolToken1) => "CPMM, WSOL as token 1",
    }
}

// ---- budgets --------------------------------------------------------------------------------

/// A compute-unit budget and where it comes from.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// Compute units.
    pub cu: u64,
    /// Source of the figure.
    pub source: &'static str,
}

/// ARCH §6.1 `open_short` RaydiumV4 / CPMM.
pub const OPEN: Budget = Budget {
    cu: 150_000,
    source: "§6.1 `open_short` RaydiumV4 / CPMM",
};
/// ARCH §6.1 has no Raydium `close_short` row: the Raydium exit row (as the M2 bench).
pub const CLOSE: Budget = Budget {
    cu: 200_000,
    source: "no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium",
};
/// ARCH §6.1 `liquidate` / `expire` Raydium.
pub const EXIT: Budget = Budget {
    cu: 200_000,
    source: "§6.1 `liquidate` / `expire` Raydium",
};
/// ARCH §6.1 `sol_settle`.
pub const SETTLE: Budget = Budget {
    cu: 150_000,
    source: "§6.1 `sol_settle`",
};
/// ARCH §6.1 `crank_price`.
pub const CRANK: Budget = Budget {
    cu: 60_000,
    source: "§6.1 `crank_price`",
};
/// ARCH §6.1 `create_market`.
pub const CREATE: Budget = Budget {
    cu: 250_000,
    source: "§6.1 `create_market`",
};
/// ARCH §6.1 `propose_pool` / `execute_pool_switch` (M4; est. 25–35 k; measured up to ≈ 49 k with
/// the carried-position fit).
pub const POOL: Budget = Budget {
    cu: 100_000,
    source: "§6.1 `propose_pool` / `execute_pool_switch`",
};
/// ARCH §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5).
pub const LATCH: Budget = Budget {
    cu: 400_000,
    source: "§6.1 `flag_venue_upgraded` / `revalidate_venue` (U5)",
};

/// ARCH §6.2's Raydium figure for an instruction.
#[derive(Clone, Copy, Debug)]
pub struct TraceRow {
    /// The table's count for the plain instruction.
    pub arch: usize,
    /// The table's maximum (with normalisation).
    pub arch_max: usize,
}

/// ARCH §6.2 `open_short` Raydium: 10 + 1 + 2 = 13 (≤ 15).
pub const OPEN_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 13,
    arch_max: 15,
});
/// ARCH §6.2 `close_short` / `liquidate` / `expire` Raydium: 8 + 1 = 9 (≤ 11).
pub const EXIT_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 9,
    arch_max: 11,
});
/// ARCH §6.2 `sol_settle`: 2 (+3 with a `ShareEpoch` record).
pub const SETTLE_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 2,
    arch_max: 5,
});
/// ARCH §6.2 `crank_price`, `propose_pool`, `flag_venue_upgraded`: 2 (`execute_pool_switch`
/// has no row; it makes the same single event CPI).
pub const TWO_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 2,
    arch_max: 2,
});

// ---- cases ----------------------------------------------------------------------------------

/// An instruction ready to run, and the environment that holds its pre-state.
pub struct Prepared {
    /// The LiteSVM environment with the pre-state.
    pub env: Env,
    /// The instruction under measurement.
    pub ix: Instruction,
    /// Its signers (besides the harness fee payer).
    pub signers: Vec<Keypair>,
}

/// One row of the report.
pub struct Case {
    /// The instruction (its handler under `programs/shortwin/src/instructions/`).
    pub handler: &'static str,
    /// What the case exercises.
    pub case: String,
    /// The limit the Mollusk figure must stay within.
    pub budget: Budget,
    /// ARCH §6.2's figure, when it has one.
    pub trace: Option<TraceRow>,
    /// Builds the pre-state and the instruction.
    pub setup: Box<dyn Fn() -> Prepared>,
}

/// What [`measure`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// Mollusk's CU (the budgeted figure).
    pub mollusk: u64,
    /// LiteSVM's CU (must equal Mollusk's).
    pub litesvm: u64,
    /// LiteSVM's instruction-trace entries.
    pub trace: usize,
    /// The deepest stack height (1 = top level).
    pub peak: u8,
    /// The Raydium program's own CU in this instruction (its token CPIs included), if it ran.
    pub venue_cu: Option<u64>,
}

/// A path under the repository root.
pub fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

/// The accounts `ix` names, as LiteSVM holds them (missing ones as empty system accounts; the
/// System program as Mollusk's builtin account).
fn snapshot(env: &Env, ix: &Instruction) -> Vec<(Pubkey, Account)> {
    let mut out: Vec<(Pubkey, Account)> = Vec::new();
    for meta in &ix.accounts {
        if out.iter().any(|(k, _)| *k == meta.pubkey) {
            continue;
        }
        let account = if meta.pubkey == anchor_lang::system_program::ID {
            keyed_account_for_system_program().1
        } else {
            env.svm.get_account(&meta.pubkey).unwrap_or_default()
        };
        out.push((meta.pubkey, account));
    }
    out
}

/// `BPFLoader2111111111111111111111111111111111` (loader v2).
const BPF_LOADER_2: Pubkey = Pubkey::from_str_const("BPFLoader2111111111111111111111111111111111");
/// `BPFLoader1111111111111111111111111111111111` (loader v1, deprecated).
const BPF_LOADER_1: Pubkey = Pubkey::from_str_const("BPFLoader1111111111111111111111111111111111");

/// The ELF of an executable account LiteSVM holds (loader v2 / v1: the data; loader v3: the
/// ProgramData after its 45-byte header, exactly what the runtime loads). `None` for builtins.
fn elf_of(env: &Env, key: &Pubkey, account: &Account) -> Option<Vec<u8>> {
    if account.owner == bpf_loader_upgradeable::ID {
        let data = env
            .svm
            .get_account(&bpf_loader_upgradeable::get_program_data_address(key))?
            .data;
        Some(data.get(45..)?.to_vec())
    } else if account.owner == BPF_LOADER_2 || account.owner == BPF_LOADER_1 {
        Some(account.data.clone())
    } else {
        None
    }
}

/// A Mollusk with the release short.win, every other program `accounts` names (the Raydium
/// programs, the token programs), and LiteSVM's clock and rent.
fn mollusk_for(env: &Env, accounts: &[(Pubkey, Account)]) -> Mollusk {
    let mut mollusk = Mollusk::default();
    let so = shortwin_litesvm_tests::env::program_so_path();
    let elf = std::fs::read(&so).unwrap_or_else(|e| panic!("read {}: {e}", so.display()));
    mollusk.add_program_with_loader_and_elf(&shortwin::ID, &bpf_loader_upgradeable::ID, &elf);
    for (key, account) in accounts {
        if !account.executable || *key == shortwin::ID {
            continue;
        }
        if let Some(elf) = elf_of(env, key, account) {
            mollusk.add_program_with_loader_and_elf(key, &account.owner, &elf);
        }
    }
    mollusk.sysvars.clock = env.clock();
    mollusk.sysvars.rent = env.svm.get_sysvar::<Rent>();
    mollusk
}

/// The Raydium program's own CU in a transaction: its `consumed` log lines (one per
/// invocation; the token CPIs it makes are inside it).
pub fn venue_cu(meta: &TransactionMetadata) -> Option<u64> {
    let mut total = None;
    for line in &meta.logs {
        for program in [RAYDIUM_V4_PROGRAM_ID, RAYDIUM_CPMM_PROGRAM_ID] {
            let prefix = format!("Program {program} consumed ");
            if let Some(rest) = line.strip_prefix(&prefix) {
                let n: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                total = Some(total.unwrap_or(0) + n);
            }
        }
    }
    total
}

/// Runs one case (see the crate docs). `Err` describes a failed run.
pub fn measure(case: &Case) -> Result<Measured, String> {
    let Prepared {
        mut env,
        ix,
        signers,
    } = (case.setup)();

    let accounts = snapshot(&env, &ix);
    let mollusk = mollusk_for(&env, &accounts);
    let result = mollusk.process_instruction(&ix, &accounts);

    let refs: Vec<&Keypair> = signers.iter().collect();
    let lite = env.send(std::slice::from_ref(&ix), &refs);

    match (&result.raw_result, lite) {
        (Ok(()), Ok(meta)) => Ok(Measured {
            mollusk: result.compute_units_consumed,
            litesvm: meta.compute_units_consumed,
            trace: trace_entries(&meta, 0),
            peak: peak_height(&meta),
            venue_cu: venue_cu(&meta),
        }),
        (m, l) => Err(format!(
            "{} ({}): Mollusk {:?} after {} CU; LiteSVM {}",
            case.handler,
            case.case,
            m,
            result.compute_units_consumed,
            match l {
                Ok(meta) => format!("succeeded ({} CU)", meta.compute_units_consumed),
                Err(f) => format!("failed with {:?}; logs:\n{}", f.err, f.meta.pretty_logs()),
            }
        )),
    }
}

/// Instruction-trace entries of top-level instruction `ix`: the instruction itself plus every
/// CPI at any depth (ARCH §6.2).
pub fn trace_entries(meta: &TransactionMetadata, ix: usize) -> usize {
    1 + meta.inner_instructions.get(ix).map_or(0, Vec::len)
}

/// Instruction-trace entries of a whole transaction (ARCH §6.2: every top-level instruction,
/// ComputeBudget included, plus every CPI at any depth).
pub fn tx_trace_entries(meta: &TransactionMetadata, top_level: usize) -> usize {
    top_level + meta.inner_instructions.iter().map(Vec::len).sum::<usize>()
}

/// The deepest stack height the transaction reached (1 = top level; ARCH §6.3).
pub fn peak_height(meta: &TransactionMetadata) -> u8 {
    meta.inner_instructions
        .iter()
        .flatten()
        .map(|i| i.stack_height)
        .max()
        .unwrap_or(1)
}

// ---- scenario builders ----------------------------------------------------------------------

#[track_caller]
fn ok(res: TxResult) -> TransactionMetadata {
    shortwin_litesvm_tests::errors::ok(res)
}

fn prepared(env: Env, ix: Instruction, signers: &[&Keypair]) -> Prepared {
    Prepared {
        env,
        ix,
        signers: signers.iter().map(|k| k.insecure_clone()).collect(),
    }
}

/// `min(X/400, 2 SOL of notional)`: the position size of the `ray_it` tests, within every
/// default cap.
pub fn q_of(env: &Env, pool: &AnyPool) -> u64 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture` in `env` (burning [`WSOL_T1`]'s LP first), lends `40·Q`, waits out the
/// listing window and cranks once; returns the market and `Q`.
pub fn add_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    if fixture == WSOL_T1 {
        set_mint_supply(&mut env.svm, &lp_mint(&pool), 0);
    }
    ok(env.list_ray(&pool, fixture.hint()));
    let q = q_of(env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}

/// A fork with one market on `fixture` ([`add_market`]).
pub fn setup(fixture: RayFixture) -> (Env, RayMarket, u64) {
    let mut env = Env::ray_fork();
    let (rm, q) = add_market(&mut env, fixture);
    (env, rm, q)
}

/// `open_short(q)` for `b` with no bounds of its own.
pub fn open_ix(rm: &RayMarket, b: &Pubkey, nonce: u64, q: u64) -> Instruction {
    open_short_ix(rm, b, nonce, q, 0, u64::MAX, u64::MAX, ExtrasSel::NONE, &[])
}

/// `close_short` with no bound of its own.
pub fn close_ix(rm: &RayMarket, b: &Pubkey, nonce: u64) -> Instruction {
    close_short_ix(rm, b, nonce, u64::MAX, ExtrasSel::NONE, &[])
}

/// A funded borrower with a **fixed** key (`n` selects it). Its `ShortPosition` and
/// `WalletExposure` addresses, and with them the canonical-bump searches of `open_short`, are the
/// same on every run, so the figures are reproducible (with random keys, as in the M2 bench, an
/// open varies by ≈ 1.5 k CU per extra bump iteration).
pub fn borrower(env: &mut Env, n: u8) -> Borrower {
    let kp = Keypair::new_from_array([n; 32]);
    env.fund(&kp.pubkey(), shortwin_litesvm_tests::env::FUNDING);
    Borrower { kp, next_nonce: 0 }
}

/// The borrower of the bench's positions.
pub const BORROWER: u8 = 1;

/// A market with one open position of `Q`, two cranked seconds old.
pub fn with_position(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, u64) {
    let (mut env, rm, q) = setup(fixture);
    let mut b = borrower(&mut env, BORROWER);
    let (res, nonce) = env.open(&rm, &mut b, q);
    ok(res);
    env.tick(&rm, 2);
    (env, rm, b, nonce, q)
}

/// A System transfer of `lamports` from `from` to `to`.
pub fn transfer(from: &Keypair, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            AccountMeta::new(from.pubkey(), true),
            AccountMeta::new(*to, false),
        ],
        data,
    }
}

/// Donates to the trader PDA and the `trader_wsol` address, so the next leg normalises (ARCH
/// §2.4 rule 5; the runtime refuses a new account below the rent-exempt minimum, hence
/// 1,000,000 lamports).
pub fn donate_to_traders(env: &mut Env, rm: &RayMarket) {
    let griefer = env.funded_keypair();
    ok(env.send(
        &[
            transfer(&griefer, &rm.keys.trader, 1_000_000),
            transfer(&griefer, &rm.trader_wsol, 1_000_000),
        ],
        &[&griefer],
    ));
}

/// `simulate_transaction` of `ixs` on the current state, paid by the harness payer (nothing is
/// committed): its metadata if it would succeed.
pub fn simulate(
    env: &Env,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> Option<TransactionMetadata> {
    let mut all: Vec<&Keypair> = vec![&env.payer];
    all.extend(signers.iter().copied());
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        ixs,
        Some(&env.payer.pubkey()),
        &all,
        env.svm.latest_blockhash(),
    );
    env.svm.simulate_transaction(tx).ok().map(|s| s.meta)
}

/// A whale's real buy of `sol` lamports of the token on `rm`'s pool.
pub fn whale_buy(env: &mut Env, rm: &RayMarket, sol: u64) {
    let (tok, program) = rm.pool.token();
    let whale = fork::trader(env, &tok, program, sol + 1_000_000_000, 0);
    ok(fork::swap(
        env,
        &rm.pool,
        &whale,
        Direction::Buy,
        fork::Swap::ExactIn(sol, 0),
    ));
}

/// A position the keeper can liquidate: a real whale buy lifts the price ≈ 2.2× (√2.2 − 1 of
/// the SOL reserve), then the keeper cranks second by second until its simulated `[crank,
/// liquidate]` of the next second succeeds. The clock is at that second, **not cranked**.
pub fn liquidatable(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, Keypair) {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    env.tick(&rm, 1);
    let (_, y) = rm.reserves(&env.svm).expect("reserves");
    whale_buy(&mut env, &rm, y / 1_000 * 484);
    let keeper = env.funded_keypair();
    for _ in 0..60 {
        env.warp(1);
        let ixs = [
            crank_price_ix(&rm),
            liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
        ];
        if simulate(&env, &ixs, &[&keeper]).is_some() {
            return (env, rm, b, nonce, keeper);
        }
        env.crank(&rm);
    }
    panic!("{fixture:?}: never liquidatable");
}

/// An expired healthy position, three cranked seconds past its deadline.
pub fn expired(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, Keypair) {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    let pos = env.position(&rm, &b.key(), nonce);
    let slot = env.clock().slot;
    env.set_clock(pos.deadline + 1, slot + 1_600_000);
    env.tick(&rm, 3);
    let keeper = env.funded_keypair();
    (env, rm, b, nonce, keeper)
}

/// Halts `rm`'s pool with the pool admin's own instruction (AMM v4 `SetParams` status 2, CPMM
/// `update_pool_status` 4 = swaps disabled; the admin's signature forged).
pub fn halt(env: &mut Env, rm: &RayMarket) {
    let ix = match &rm.pool {
        AnyPool::V4(p) => fork::v4_set_params_ix(p, 0, 2),
        AnyPool::Cpmm(p) => fork::cpmm_update_pool_status_ix(p, 4),
    };
    ok(fork::raydium_admin_send(env, ix));
}

/// An expired position on a halted pool whose S3 chain has run `s3_wait_s` (ARCH §5.3; as
/// `ray_it::route`): cranks every 30 s from just before the deadline until the keeper's
/// simulated `[crank, sol_settle]` settles S3. The clock is at that second, **not cranked**.
pub fn s3_due(fixture: RayFixture) -> (Env, RayMarket, Borrower, u64, Keypair) {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    let pos = env.position(&rm, &b.key(), nonce);
    halt(&mut env, &rm);
    let slot = env.clock().slot;
    env.set_clock(pos.deadline - 5, slot + 1_500_000);
    env.crank(&rm);
    let keeper = env.funded_keypair();
    for _ in 0..40 {
        env.warp(30);
        let ixs = [
            crank_price_ix(&rm),
            sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce),
        ];
        if let Some(meta) = simulate(&env, &ixs, &[&keeper])
            && events::of::<SolSettle>(&meta)
                .first()
                .is_some_and(|e| e.condition == 3)
        {
            return (env, rm, b, nonce, keeper);
        }
        env.crank(&rm);
    }
    panic!("{fixture:?}: S3 never due");
}

/// The Raydium program behind a `VenueGuard` index.
fn raydium_program(venue: u8) -> RaydiumProgram {
    if venue == GUARD_RAYDIUM_V4 {
        RaydiumProgram::AmmV4
    } else {
        RaydiumProgram::Cpmm
    }
}

/// Pump's ProgramData size (ARCH §6.5): the U5 worst case.
pub const PUMP_PROGRAM_DATA_LEN: usize = 10_485_760;

// ---- setups ---------------------------------------------------------------------------------

fn create_market(fixture: RayFixture) -> Prepared {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(fixture);
    if fixture == WSOL_T1 {
        set_mint_supply(&mut env.svm, &lp_mint(&pool), 0);
    }
    let creator = env.funded_keypair();
    let ix = create_ray_market_ix(&env.svm, &creator.pubkey(), &pool, fixture.hint());
    prepared(env, ix, &[&creator])
}

fn open_first(fixture: RayFixture) -> Prepared {
    let (mut env, rm, q) = setup(fixture);
    env.tick(&rm, 1);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&rm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn open_repeat(fixture: RayFixture) -> Prepared {
    let (env, rm, b, nonce, q) = with_position(fixture);
    let ix = open_ix(&rm, &b.key(), nonce + 1, q / 2);
    prepared(env, ix, &[&b.kp])
}

fn open_normalising(fixture: RayFixture) -> Prepared {
    let (mut env, rm, q) = setup(fixture);
    env.tick(&rm, 1);
    donate_to_traders(&mut env, &rm);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&rm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn close(fixture: RayFixture, normalise: bool) -> Prepared {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    if normalise {
        donate_to_traders(&mut env, &rm);
    }
    let ix = close_ix(&rm, &b.key(), nonce);
    prepared(env, ix, &[&b.kp])
}

fn liquidate(fixture: RayFixture) -> Prepared {
    let (mut env, rm, b, nonce, keeper) = liquidatable(fixture);
    env.crank(&rm);
    let ix = liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn expire(fixture: RayFixture) -> Prepared {
    let (env, rm, b, nonce, keeper) = expired(fixture);
    let ix = expire_ix(&rm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn settle_noop(fixture: RayFixture) -> Prepared {
    let (mut env, rm, b, nonce, _) = with_position(fixture);
    let keeper = env.funded_keypair();
    let ix = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn settle_s3(fixture: RayFixture) -> Prepared {
    let (mut env, rm, b, nonce, keeper) = s3_due(fixture);
    env.crank(&rm);
    let ix = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn crank_roll(fixture: RayFixture) -> Prepared {
    let (mut env, rm, _, _, _) = with_position(fixture);
    env.warp(1);
    let ix = crank_price_ix(&rm);
    prepared(env, ix, &[])
}

fn crank_same_second(fixture: RayFixture) -> Prepared {
    let (env, rm, _, _, _) = with_position(fixture);
    let ix = crank_price_ix(&rm);
    prepared(env, ix, &[])
}

fn crank_halted(fixture: RayFixture) -> Prepared {
    let (mut env, rm, _, _, _) = with_position(fixture);
    halt(&mut env, &rm);
    env.warp(1);
    let ix = crank_price_ix(&rm);
    prepared(env, ix, &[])
}

/// `flag_venue_upgraded` on the dumped ProgramData whose deploy slot changed.
fn flag_raydium(venue: u8) -> Prepared {
    let mut env = Env::ray_fork();
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    fork::set_deploy_slot(&mut env.svm, raydium_program(venue), dumped + 1);
    prepared(env, ix::flag_venue_upgraded(venue), &[])
}

/// A second upgrade while flagged: the refresh of `flagged_slot` / `flagged_at`.
fn flag_refresh() -> Prepared {
    let mut env = Env::ray_fork();
    let venue = GUARD_RAYDIUM_V4;
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, dumped + 1);
    ok(env.send(&[ix::flag_venue_upgraded(venue)], &[]));
    env.warp(30);
    fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, dumped + 2);
    prepared(env, ix::flag_venue_upgraded(venue), &[])
}

/// U5's worst case: a ProgramData of pump's size (10 MiB; synthetic, pump is not dumped) at
/// pump's compile-time address.
fn flag_ten_mib() -> Prepared {
    let mut env = Env::ray_fork();
    let key = VENUE_PROGRAM_DATA[usize::from(GUARD_PUMP)];
    let slot = FIXTURE_GUARD_SLOTS[usize::from(GUARD_PUMP)] + 1;
    fixtures::set_program_data(
        &mut env.svm,
        &key,
        slot,
        Some(&Pubkey::new_unique()),
        PUMP_PROGRAM_DATA_LEN - 45,
    );
    prepared(env, ix::flag_venue_upgraded(GUARD_PUMP), &[])
}

fn revalidate_raydium(venue: u8) -> Prepared {
    let Prepared { mut env, ix, .. } = flag_raydium(venue);
    ok(env.send(&[ix], &[]));
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let ea = env.pause_authority.insecure_clone();
    let ix = ix::revalidate_venue(&ea.pubkey(), venue, dumped + 1);
    prepared(env, ix, &[&ea])
}

// ---- M4: pool selection ---------------------------------------------------------------------

/// Fartcoin's tiny reversed-order AMM v4 pool `9GWMrDgH…` (WSOL as pc; thin, not eligible as
/// dumped).
pub const FART_REV: RayFixture = RayFixture::V4(V4Fixture::FartcoinReversed);

/// Integrated seconds that fill the observation ring (`RING_LEN` = 8) with a margin, so an
/// adoption rewrites every entry (ARCH §2.6 rule 4, A72).
pub const RING_FILL: u32 = shortwin_math::consts::RING_LEN as u32 + 2;

/// `λ_bps` of `pool` now.
pub fn lambda_of(env: &Env, pool: &AnyPool) -> u64 {
    let supply = mint_supply(&env.svm, &lp_mint(pool));
    u64::from(shortwin_math::pool_select::lambda_bps(
        lp_counter(&env.svm, pool),
        supply,
    ))
}

/// A planted CPMM pool (USELESS's bytes, config 0) of `base`'s token, WSOL as token 0, with
/// `base`'s reserves scaled by `num/den` (its price) and its locked share: `D` scales by the same
/// factor.
pub fn plant_like(env: &mut Env, base: &AnyPool, num: u64, den: u64) -> AnyPool {
    let lam = lambda_of(env, base);
    let spec = spec_like(&env.svm, base, true, lam).scaled(num, den);
    AnyPool::Cpmm(plant_cpmm(env, CpmmFixture::Useless, spec))
}

/// A list-only (`Auto`) market of `fixture`'s token; Fartcoin's with its real mainnet bonding
/// curve at the curve PDA, so `propose_pool`'s D1 detection reads a curve that graduated
/// elsewhere (ARCH §2.5).
fn list_only(fixture: RayFixture) -> (Env, AnyPool) {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(fixture);
    let (mint, program) = pool.token();
    if fixture == FART {
        plant_curve(&mut env.svm, &mint, fartcoin_curve_bytes());
    }
    ok(env.list_auto(&mint, program));
    (env, pool)
}

fn propose_list_only(fixture: RayFixture) -> Prepared {
    let (env, pool) = list_only(fixture);
    let mint = pool.token().0;
    let ix = propose_pool_ix(&env.svm, &mint, &detection_set(&mint), &pool);
    prepared(env, ix, &[])
}

/// Fartcoin's thin reversed pool listed first — its LP burned to λ = 50 %, topped up to just
/// over the locked-depth floor, i.e. `pool_min_real_sol` and the floor's rounding (`ray_sec_it`'s
/// M3-SEC-3 case; M4-SEC-1 / -3) — with a lender, cranked for [`RING_FILL`] seconds; the main
/// pool loaded. Returns the market on the thin pool and the main pool.
fn thin_listing() -> (Env, RayMarket, AnyPool) {
    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let thin = env.load_ray_pool(FART_REV);
    let min = env.config().params.pool_min_real_sol;
    let supply = supply_for_lambda(lp_counter(&env.svm, &thin), 5_000);
    set_mint_supply(&mut env.svm, &lp_mint(&thin), supply);
    shortwin_litesvm_tests::pool_select::top_up_pc_to_floor(&mut env, &thin, min);
    ok(env.list_ray(&thin, FART_REV.hint()));
    let q = q_of(&env, &thin);
    let spec = RaySpec {
        warmup: 0,
        ..RaySpec::new(FART_REV, q.saturating_mul(40))
    };
    let rm = env.ray_market_listed(spec, thin);
    env.tick(&rm, RING_FILL);
    (env, rm, deep)
}

/// A listing-window replacement (ARCH §2.6 rule 1): the thin listing, [`RING_FILL`] seconds
/// old, replaced by Fartcoin's main pool (5 % margin, no position; the ring rewritten).
fn propose_listing_window() -> Prepared {
    let (mut env, rm, deep) = thin_listing();
    env.warp(1);
    let mint = deep.token().0;
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let ix = propose_pool_ix(&env.svm, &mint, &inc, &deep);
    prepared(env, ix, &[])
}

/// A zero-position adoption after the listing window (ARCH §2.6 rule 2) from `fixture`'s pool
/// onto a planted CPMM pool twice as deep, the ring full.
fn propose_zero_position(fixture: RayFixture) -> Prepared {
    let (mut env, rm, _) = setup(fixture);
    env.tick(&rm, RING_FILL);
    let cand = plant_like(&mut env, &rm.pool, 2, 1);
    env.warp(1);
    let mint = rm.pool.token().0;
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let ix = propose_pool_ix(&env.svm, &mint, &inc, &cand);
    prepared(env, ix, &[])
}

/// The fast path armed (ARCH §2.6 rule 3) on `fixture` with one position open: the ring full,
/// two planted CPMM candidates at 40 % and 60 % of the incumbent's `D`, then the pool halted by
/// its admin's own instruction and observed every 30 s for 150 s (`pool_fail` ≥
/// `pool_fast_route_s`). The clock is one second past the last crank.
fn fast_path_armed(fixture: RayFixture) -> (Env, RayMarket, AnyPool, AnyPool) {
    let (mut env, rm, _, _, _) = with_position(fixture);
    env.tick(&rm, RING_FILL);
    let a = plant_like(&mut env, &rm.pool, 40, 100);
    let c = plant_like(&mut env, &rm.pool, 60, 100);
    halt(&mut env, &rm);
    for _ in 0..5 {
        env.warp(30);
        env.crank(&rm);
    }
    env.warp(1);
    (env, rm, a, c)
}

fn propose_ix_on(env: &Env, rm: &RayMarket, cand: &AnyPool) -> Instruction {
    let mint = rm.pool.token().0;
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    propose_pool_ix(&env.svm, &mint, &inc, cand)
}

/// The first armed proposal opens a collection window on the 40 % candidate.
fn propose_open_window(fixture: RayFixture) -> Prepared {
    let (env, rm, a, _) = fast_path_armed(fixture);
    let ix = propose_ix_on(&env, &rm, &a);
    prepared(env, ix, &[])
}

/// A deeper candidate (60 %) replaces the window's (40 %) five seconds later.
fn propose_improve_window(fixture: RayFixture) -> Prepared {
    let (mut env, rm, a, c) = fast_path_armed(fixture);
    ok(env.send(&[propose_ix_on(&env, &rm, &a)], &[]));
    env.warp(5);
    let ix = propose_ix_on(&env, &rm, &c);
    prepared(env, ix, &[])
}

/// A window of the 60 % candidate that has run `pool_fast_window_s` (the incumbent observed
/// every 30 s meanwhile); `resume` reopens the incumbent's pool first (a cancellation).
fn window_due(fixture: RayFixture, resume: bool) -> Prepared {
    let (mut env, rm, _, c) = fast_path_armed(fixture);
    ok(env.send(&[propose_ix_on(&env, &rm, &c)], &[]));
    let window = i64::from(env.config().params.pool_fast_window_s);
    env.warp(window / 2);
    env.crank(&rm);
    if resume {
        let ix = match &rm.pool {
            AnyPool::V4(p) => fork::v4_set_params_ix(p, 0, 6),
            AnyPool::Cpmm(p) => fork::cpmm_update_pool_status_ix(p, 0),
        };
        ok(fork::raydium_admin_send(&mut env, ix));
    }
    env.warp(window - window / 2);
    let mint = rm.pool.token().0;
    let ix = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &c);
    prepared(env, ix, &[])
}

/// The last post-change warm-up integration (M4R): a fast adoption of the 60 % candidate with a
/// position open, then eight per-second cranks of the new pool (the first records, the next seven
/// integrate: `ema_warm` 1), and one second later the measured crank, which re-seeds the EMA over
/// the eight post-change entries and clears the guard at the gate.
fn crank_warm_up(fixture: RayFixture) -> Prepared {
    let (mut env, rm, _, c) = fast_path_armed(fixture);
    ok(env.send(&[propose_ix_on(&env, &rm, &c)], &[]));
    let window = i64::from(env.config().params.pool_fast_window_s);
    env.warp(window / 2);
    env.crank(&rm);
    env.warp(window - window / 2);
    let mint = rm.pool.token().0;
    ok(env.send(
        &[execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &c)],
        &[],
    ));
    let rm2 = rm.on_pool(&env.svm, c);
    for _ in 0..8 {
        env.warp(1);
        env.crank(&rm2);
    }
    assert_eq!(
        env.market(&rm2.keys.market).ema_warm,
        1,
        "the last warm-up step"
    );
    env.warp(1);
    let ix = crank_price_ix(&rm2);
    prepared(env, ix, &[])
}

/// The Raydium pool-selection instructions (ARCH §9 M4) on both programs and both token
/// programs: every `propose_pool` outcome that writes (a list-only adoption, a listing-window
/// replacement, a zero-position adoption, a window opened, a window improved) and both
/// `execute_pool_switch` outcomes (the fast adoption with a position open, a cancellation).
pub fn pool_cases() -> Vec<Case> {
    let mut v: Vec<Case> = Vec::new();
    let mut add = |handler: &'static str, case: String, setup: Box<dyn Fn() -> Prepared>| {
        v.push(Case {
            handler,
            case,
            budget: POOL,
            trace: TWO_ROW,
            setup,
        });
    };
    add(
        "propose_pool",
        format!(
            "{}: list-only market adopts it (Fartcoin's real curve: D1 detection)",
            label(FART)
        ),
        Box::new(|| propose_list_only(FART)),
    );
    add(
        "propose_pool",
        format!("{}: list-only market adopts it", label(USELESS)),
        Box::new(|| propose_list_only(USELESS)),
    );
    add(
        "propose_pool",
        "AMM v4: listing-window replacement of the thin reversed Fartcoin pool by the main pool, ring rewritten".into(),
        Box::new(propose_listing_window),
    );
    for f in VARIANTS {
        add(
            "propose_pool",
            format!(
                "{}: zero-position adoption of a planted CPMM pool 2× as deep, ring rewritten",
                label(f)
            ),
            Box::new(move || propose_zero_position(f)),
        );
    }
    for f in VARIANTS {
        add(
            "propose_pool",
            format!(
                "{}: halted incumbent, a position open: fast-path window opened (CPMM candidate at 40 % of D)",
                label(f)
            ),
            Box::new(move || propose_open_window(f)),
        );
        add(
            "propose_pool",
            format!(
                "{}: fast-path window improved (60 % replaces 40 %)",
                label(f)
            ),
            Box::new(move || propose_improve_window(f)),
        );
    }
    for f in VARIANTS {
        add(
            "execute_pool_switch",
            format!(
                "{}: fast adoption after the window, a position open, ring rewritten",
                label(f)
            ),
            Box::new(move || window_due(f, false)),
        );
        add(
            "execute_pool_switch",
            format!("{}: window cancelled (the incumbent resumed)", label(f)),
            Box::new(move || window_due(f, true)),
        );
    }
    for f in VARIANTS {
        v.push(Case {
            handler: "crank_price",
            case: format!(
                "{}: the last post-change warm-up integration (seed over 8 entries, guard cleared; M4R)",
                label(f)
            ),
            budget: CRANK,
            trace: TWO_ROW,
            setup: Box::new(move || crank_warm_up(f)),
        });
    }
    v
}

/// Every Raydium-path instruction (ARCH §9 M3) on both programs, SPL and Token-2022, with the
/// fee and side variants of the CPMM adapter where they change the path; the latch on the
/// dumped ProgramData and on a 10 MiB one.
pub fn cases() -> Vec<Case> {
    let mut v: Vec<Case> = Vec::new();
    let mut add = |handler: &'static str,
                   case: String,
                   budget: Budget,
                   trace: Option<TraceRow>,
                   setup: Box<dyn Fn() -> Prepared>| {
        v.push(Case {
            handler,
            case,
            budget,
            trace,
            setup,
        });
    };
    for f in [FART, USELESS, T22, CREATOR_FEE, WSOL_T1] {
        add(
            "create_market",
            label(f).into(),
            CREATE,
            None,
            Box::new(move || create_market(f)),
        );
    }
    for f in [FART, USELESS, T22, CREATOR_FEE, WSOL_T1] {
        add(
            "open_short",
            format!(
                "{}: first open of a wallet (position + exposure created)",
                label(f)
            ),
            OPEN,
            OPEN_ROW,
            Box::new(move || open_first(f)),
        );
    }
    for f in [FART, T22] {
        add(
            "open_short",
            format!("{}: second position of a wallet", label(f)),
            OPEN,
            OPEN_ROW,
            Box::new(move || open_repeat(f)),
        );
        add(
            "open_short",
            format!(
                "{}: first open, normalising (trader and trader_wsol donated to)",
                label(f)
            ),
            OPEN,
            OPEN_ROW,
            Box::new(move || open_normalising(f)),
        );
    }
    for f in [FART, USELESS, T22, CREATOR_FEE, WSOL_T1] {
        add(
            "close_short",
            label(f).into(),
            CLOSE,
            EXIT_ROW,
            Box::new(move || close(f, false)),
        );
    }
    for f in [FART, T22] {
        add(
            "close_short",
            format!("{}: normalising", label(f)),
            CLOSE,
            EXIT_ROW,
            Box::new(move || close(f, true)),
        );
    }
    for f in VARIANTS {
        add(
            "liquidate",
            format!(
                "{}: after a real ≈ 2.2× pump, both legs, within the fixed bound",
                label(f)
            ),
            EXIT,
            EXIT_ROW,
            Box::new(move || liquidate(f)),
        );
    }
    for f in VARIANTS {
        add(
            "expire",
            format!("{}: past the deadline, healthy", label(f)),
            EXIT,
            EXIT_ROW,
            Box::new(move || expire(f)),
        );
    }
    for f in VARIANTS {
        add(
            "sol_settle",
            format!("{}: healthy, the no-change call", label(f)),
            SETTLE,
            SETTLE_ROW,
            Box::new(move || settle_noop(f)),
        );
        add(
            "sol_settle",
            format!(
                "{}: S3 on a halted pool at the deadline, chain run s3_wait_s",
                label(f)
            ),
            SETTLE,
            SETTLE_ROW,
            Box::new(move || settle_s3(f)),
        );
    }
    for f in VARIANTS {
        add(
            "crank_price",
            format!("{}: roll + record (first crank of a second)", label(f)),
            CRANK,
            TWO_ROW,
            Box::new(move || crank_roll(f)),
        );
        add(
            "crank_price",
            format!(
                "{}: pool halted by its admin: roll, no record, both chains start",
                label(f)
            ),
            CRANK,
            TWO_ROW,
            Box::new(move || crank_halted(f)),
        );
    }
    add(
        "crank_price",
        format!("{}: record only (same second again)", label(FART)),
        CRANK,
        TWO_ROW,
        Box::new(|| crank_same_second(FART)),
    );
    add(
        "flag_venue_upgraded",
        "AMM v4, the dumped 1,406,429-byte ProgramData".into(),
        LATCH,
        TWO_ROW,
        Box::new(|| flag_raydium(GUARD_RAYDIUM_V4)),
    );
    add(
        "flag_venue_upgraded",
        "CPMM, the dumped 793,869-byte ProgramData".into(),
        LATCH,
        TWO_ROW,
        Box::new(|| flag_raydium(GUARD_RAYDIUM_CPMM)),
    );
    add(
        "flag_venue_upgraded",
        "AMM v4, a second upgrade while flagged (refresh)".into(),
        LATCH,
        TWO_ROW,
        Box::new(flag_refresh),
    );
    add(
        "flag_venue_upgraded",
        "a 10,485,760-byte ProgramData (pump's size, synthetic): U5".into(),
        LATCH,
        TWO_ROW,
        Box::new(flag_ten_mib),
    );
    add(
        "revalidate_venue",
        "AMM v4, the dumped ProgramData".into(),
        LATCH,
        TWO_ROW,
        Box::new(|| revalidate_raydium(GUARD_RAYDIUM_V4)),
    );
    add(
        "revalidate_venue",
        "CPMM, the dumped ProgramData".into(),
        LATCH,
        TWO_ROW,
        Box::new(|| revalidate_raydium(GUARD_RAYDIUM_CPMM)),
    );
    v.extend(pool_cases());
    v
}

// ---- T-TRACE --------------------------------------------------------------------------------

/// `ComputeBudget111111111111111111111111111111`.
pub const COMPUTE_BUDGET: Pubkey =
    Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");

/// `SetComputeUnitLimit(units)` and `SetComputeUnitPrice(micro_lamports)`: the two instructions
/// every client and keeper transaction starts with (ARCH §6.1, §6.2).
pub fn compute_budget(units: u32, micro_lamports: u64) -> [Instruction; 2] {
    let mut limit = vec![2u8];
    limit.extend_from_slice(&units.to_le_bytes());
    let mut price = vec![3u8];
    price.extend_from_slice(&micro_lamports.to_le_bytes());
    [
        Instruction {
            program_id: COMPUTE_BUDGET,
            accounts: vec![],
            data: limit,
        },
        Instruction {
            program_id: COMPUTE_BUDGET,
            accounts: vec![],
            data: price,
        },
    ]
}

fn with_cb(mut ixs: Vec<Instruction>) -> Vec<Instruction> {
    let mut v = compute_budget(1_400_000, 1).to_vec();
    v.append(&mut ixs);
    v
}

/// A worst-case transaction and what ARCH §6.2 allows it.
pub struct TraceTx {
    /// What the transaction is.
    pub name: String,
    /// ARCH §6.2's figure for it, or the reason there is none.
    pub arch: &'static str,
    /// The exact entries this bench expects (the composition is pinned in
    /// `tests/litesvm/tests/trace_raydium_it.rs`).
    pub expected: usize,
    /// A keeper batch sent as a **v1** transaction (ARCH §6.4: 4,096 B, every key inline, no
    /// lookup table); otherwise the transaction must fit a v0 one with the protocol ALT.
    pub v1: bool,
    /// Builds the pre-state, the instructions and the signers.
    #[allow(clippy::type_complexity)]
    pub setup: Box<dyn Fn() -> (Env, Vec<Instruction>, Vec<Keypair>)>,
}

/// The measured trace of one [`TraceTx`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceMeasured {
    /// Entries of the whole transaction.
    pub entries: usize,
    /// Its CU.
    pub cu: u64,
    /// The deepest stack height.
    pub peak: u8,
    /// Legacy wire size (B) and accounts.
    pub legacy: (usize, usize),
    /// v0 wire size with the protocol ALT (A28) and accounts (inline + looked up).
    pub v0: (usize, usize),
}

/// Runs one worst-case transaction on LiteSVM (every hook after every instruction).
pub fn run_trace(t: &TraceTx) -> Result<TraceMeasured, String> {
    use shortwin_litesvm_tests::raydium::{legacy_size, protocol_alt_addresses, v0_size};
    let (mut env, ixs, signers) = (t.setup)();
    let payer = env.payer.pubkey();
    let legacy = legacy_size(&payer, &ixs);
    let alt = Pubkey::new_unique();
    let v0 = v0_size(&payer, &ixs, &alt, &protocol_alt_addresses());
    let refs: Vec<&Keypair> = signers.iter().collect();
    match env.send(&ixs, &refs) {
        Ok(meta) => Ok(TraceMeasured {
            entries: tx_trace_entries(&meta, ixs.len()),
            cu: meta.compute_units_consumed,
            peak: peak_height(&meta),
            legacy,
            v0,
        }),
        Err(f) => Err(format!(
            "{}: failed with {:?}; logs:\n{}",
            t.name,
            f.err,
            f.meta.pretty_logs()
        )),
    }
}

fn tx_open_normalising(f: RayFixture) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let Prepared { env, ix, signers } = open_normalising(f);
    (env, with_cb(vec![ix]), signers)
}

fn tx_close_normalising(f: RayFixture) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let Prepared { env, ix, signers } = close(f, true);
    (env, with_cb(vec![ix]), signers)
}

/// An atomic sandwich of a normalising open with the venue's own swaps (a front-run sale of
/// `X/50` tokens and a back-run buy with what it paid), by an attacker holding both sides.
fn tx_sandwiched_open(f: RayFixture) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, rm, q) = setup(f);
    let (tok, program) = rm.pool.token();
    let x = rm.reserves(&env.svm).expect("reserves").0;
    let attacker = fixed_trader(&mut env, &tok, program, 1_000_000_000_000, x / 20);
    let sol_out = rm
        .pool
        .quote_exact_in(&env.svm, Direction::Sell, x / 50)
        .expect("quote")
        .proceeds;
    let a = attacker.owner.pubkey();
    let front = rm.pool.swap_ix(
        &env.svm,
        &a,
        &attacker,
        Direction::Sell,
        fork::Swap::ExactIn(x / 50, 0),
    );
    let back = rm.pool.swap_ix(
        &env.svm,
        &a,
        &attacker,
        Direction::Buy,
        fork::Swap::ExactIn(sol_out, 0),
    );
    env.tick(&rm, 1);
    donate_to_traders(&mut env, &rm);
    let v = borrower(&mut env, BORROWER);
    let ixs = with_cb(vec![front, open_ix(&rm, &v.key(), 0, q / 2), back]);
    (
        env,
        ixs,
        vec![attacker.owner.insecure_clone(), v.kp.insecure_clone()],
    )
}

/// A pool user like `fork::trader` (a WSOL account holding `wsol` lamports, a token account
/// holding `tokens`), but with **fixed** keys for the owner and both accounts, so the venue's
/// key comparisons, and with them the CU of its swaps, are the same on every run.
fn fixed_trader(
    env: &mut Env,
    mint: &Pubkey,
    program: TokenProgram,
    wsol: u64,
    tokens: u64,
) -> fork::Trader {
    let owner = Keypair::new_from_array([ATTACKER; 32]);
    env.fund(&owner.pubkey(), shortwin_litesvm_tests::env::FUNDING);
    let payer = env.payer.insecure_clone();
    let wsol_account = litesvm_token::CreateAccount::new(&mut env.svm, &payer, &WSOL_MINT)
        .owner(&owner.pubkey())
        .token_program_id(&TOKEN_PROGRAM_ID)
        .account_kp(Keypair::new_from_array([ATTACKER + 1; 32]))
        .send()
        .unwrap_or_else(|e| panic!("create WSOL account: {:?}", e.err));
    fork::wrap(env, &wsol_account, wsol);
    let token = litesvm_token::CreateAccount::new(&mut env.svm, &payer, mint)
        .owner(&owner.pubkey())
        .token_program_id(&program.id())
        .account_kp(Keypair::new_from_array([ATTACKER + 2; 32]))
        .send()
        .unwrap_or_else(|e| panic!("create token account: {:?}", e.err));
    fork::set_token_amount(&mut env.svm, &token, tokens);
    fork::Trader {
        owner,
        wsol: wsol_account,
        token,
    }
}

/// The seed byte of the sandwich attacker's keys ([`fixed_trader`]).
const ATTACKER: u8 = 0x40;

/// `k` opens of one wallet in one transaction (the first creates the exposure record).
pub fn opens_of_one_wallet(f: RayFixture, k: u64) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, rm, q) = setup(f);
    env.tick(&rm, 1);
    let b = borrower(&mut env, BORROWER);
    let ixs = (0..k).map(|n| open_ix(&rm, &b.key(), n, q / 8)).collect();
    (env, with_cb(ixs), vec![b.kp.insecure_clone()])
}

/// `k ≤ 6` Raydium markets (AMM v4 first, then CPMM) whose pools were all just halted by their
/// admin: every crank starts its route chains (3 entries, a crank's worst case).
pub fn tx_crank_batch(k: usize) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let mut env = Env::ray_fork();
    let markets: Vec<RayMarket> = [FART, USELESS, T22, CREATOR_FEE, CREATOR_BOTH, WSOL_T1]
        .into_iter()
        .take(k)
        .map(|f| add_market(&mut env, f).0)
        .collect();
    for rm in &markets {
        env.crank(rm);
        halt(&mut env, rm);
    }
    env.warp(1);
    let ixs = markets.iter().map(crank_price_ix).collect();
    (env, with_cb(ixs), vec![])
}

fn tx_keeper_liquidation(f: RayFixture) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (env, rm, b, nonce, keeper) = liquidatable(f);
    let ixs = vec![
        crank_price_ix(&rm),
        liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
    ];
    (env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_expiry(f: RayFixture) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, rm, b, nonce, keeper) = expired(f);
    env.warp(1);
    let ixs = vec![
        crank_price_ix(&rm),
        expire_ix(&rm, &keeper.pubkey(), &b.key(), nonce),
    ];
    (env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_settlement(f: RayFixture) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (env, rm, b, nonce, keeper) = s3_due(f);
    let ixs = vec![
        crank_price_ix(&rm),
        sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce),
    ];
    (env, with_cb(ixs), vec![keeper])
}

/// The keeper's latch duty (ARCH §7.1 duty 10): both Raydium programs upgraded in one Squads
/// batch, flagged in one transaction.
fn tx_flag_both() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let mut env = Env::ray_fork();
    for venue in [GUARD_RAYDIUM_V4, GUARD_RAYDIUM_CPMM] {
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_deploy_slot(&mut env.svm, raydium_program(venue), dumped + 1);
    }
    let ixs = vec![
        ix::flag_venue_upgraded(GUARD_RAYDIUM_V4),
        ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM),
    ];
    (env, with_cb(ixs), vec![])
}

/// The worst-case Raydium transactions (T-TRACE): each must stay ≤ 64 entries.
pub fn worst_case_transactions() -> Vec<TraceTx> {
    let mut v: Vec<TraceTx> = Vec::new();
    for f in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + open_short, first open, normalising",
                label(f)
            ),
            arch: "2 + ≤ 15 (§6.2 Raydium `open_short` max)",
            expected: 2 + 18,
            v1: false,
            setup: Box::new(move || tx_open_normalising(f)),
        });
        v.push(TraceTx {
            name: format!("{}: ComputeBudget ×2 + close_short, normalising", label(f)),
            arch: "2 + ≤ 11 (§6.2 Raydium exit max)",
            expected: 2 + 12,
            v1: false,
            setup: Box::new(move || tx_close_normalising(f)),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + [venue swap, open_short normalising, venue swap]",
                label(f)
            ),
            arch: "no row (atomic sandwich bundle, §8.2)",
            expected: 2 + 3 + 18 + 3,
            v1: false,
            setup: Box::new(move || tx_sandwiched_open(f)),
        });
    }
    for f in [FART, T22] {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + four opens of one wallet (the most that fit: 2 + 15 + 3·14)",
                label(f)
            ),
            arch: "no row",
            expected: 2 + 15 + 3 * 14,
            v1: false,
            setup: Box::new(move || opens_of_one_wallet(f, 4)),
        });
    }
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price ×4 on four Raydium markets, every pool just halted (the most a v0 + ALT transaction holds)"
            .into(),
        arch: "2 + 2k = 10 (§6.2, k = 4)",
        expected: 2 + 3 * 4,
        v1: false,
        setup: Box::new(|| tx_crank_batch(4)),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price ×6 on six Raydium markets, every pool just halted (a v1 keeper batch)"
            .into(),
        arch: "2 + 2k = 14 (§6.2, k = 6)",
        expected: 2 + 3 * 6,
        v1: true,
        setup: Box::new(|| tx_crank_batch(6)),
    });
    for f in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + liquidate (keeper)",
                label(f)
            ),
            arch: "2 + 2 + 11 on Raydium (§6.2)",
            expected: 2 + 2 + 9,
            v1: false,
            setup: Box::new(move || tx_keeper_liquidation(f)),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + expire (keeper)",
                label(f)
            ),
            arch: "2 + 2 + 11 on Raydium (§6.2)",
            expected: 2 + 2 + 9,
            v1: false,
            setup: Box::new(move || tx_keeper_expiry(f)),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + sol_settle S3 (keeper, halted pool)",
                label(f)
            ),
            arch: "2 + 2 + 2 (§6.2)",
            expected: 2 + 2 + 2,
            v1: false,
            setup: Box::new(move || tx_keeper_settlement(f)),
        });
    }
    v.push(TraceTx {
        name: "ComputeBudget ×2 + flag_venue_upgraded ×2 (both Raydium programs, the dumped ProgramData)"
            .into(),
        arch: "2 + 2 + 2 (§6.2)",
        expected: 2 + 2 + 2,
        v1: false,
        setup: Box::new(tx_flag_both),
    });
    // M4: the keeper's pool-selection transactions (ARCH §7.1 duties 5 and 12).
    for f in [FART, T22] {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + propose_pool (zero-position adoption, ring rewritten)",
                label(f)
            ),
            arch: "2 + 2 (§6.2 `propose_pool`)",
            expected: 2 + 2,
            v1: false,
            setup: Box::new(move || single_with_cb(propose_zero_position(f))),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + execute_pool_switch (fast adoption, a position open)",
                label(f)
            ),
            arch: "2 + 2 (no row; as `propose_pool`)",
            expected: 2 + 2,
            v1: false,
            setup: Box::new(move || single_with_cb(window_due(f, false))),
        });
    }
    v
}

/// One prepared instruction with the two ComputeBudget instructions in front.
fn single_with_cb(p: Prepared) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let Prepared { env, ix, signers } = p;
    (env, with_cb(vec![ix]), signers)
}

// ---- stack depth ----------------------------------------------------------------------------

/// The TEST-ONLY wrapper program's id in the bench (`wrapper/`; never deployed): the 32 bytes of
/// `short.win-m3-cpi-wrapper-program`.
pub const WRAPPER_ID: Pubkey = Pubkey::new_from_array(*b"short.win-m3-cpi-wrapper-program");

/// The wrapper's SBF build: `$M3_WRAPPER_SO`, else `tests/bench/m3_cu/target/deploy/
/// m3_cpi_wrapper.so` (`cargo-build-sbf --tools-version v1.57 --arch v3 --manifest-path
/// tests/bench/m3_cu/wrapper/Cargo.toml --sbf-out-dir tests/bench/m3_cu/target/deploy`).
pub fn wrapper_so_path() -> PathBuf {
    std::env::var_os("M3_WRAPPER_SO").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/m3_cpi_wrapper.so"),
        PathBuf::from,
    )
}

/// Deploys the wrapper into `env`.
pub fn load_wrapper(env: &mut Env) {
    let path = wrapper_so_path();
    let so = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}. Build it: cargo-build-sbf --tools-version v1.57 --arch v3 \
             --manifest-path tests/bench/m3_cu/wrapper/Cargo.toml --sbf-out-dir \
             tests/bench/m3_cu/target/deploy (or set M3_WRAPPER_SO)",
            path.display()
        )
    });
    env.svm
        .add_program(WRAPPER_ID, &so)
        .unwrap_or_else(|e| panic!("add the wrapper: {e:?}"));
}

/// `ix` called through the wrapper `levels` times: `[WRAPPER, …, WRAPPER, ix.program_id,
/// ix.accounts…]` with `ix.data` (each level forwards everything after its first account).
pub fn wrap(ix: &Instruction, levels: usize) -> Instruction {
    let mut out = ix.clone();
    for _ in 0..levels {
        let mut accounts = vec![AccountMeta::new_readonly(out.program_id, false)];
        accounts.extend(out.accounts.iter().cloned());
        out = Instruction {
            program_id: WRAPPER_ID,
            accounts,
            data: out.data.clone(),
        };
    }
    out
}

/// One stack-depth row: an instruction whose pre-state [`DepthCase::setup`] builds afresh for
/// every wrapping level.
pub struct DepthCase {
    /// The instruction.
    pub handler: &'static str,
    /// The market it runs on.
    pub case: String,
    /// Its peak stack height when called directly.
    pub direct_peak: u8,
    /// Builds the pre-state and the instruction.
    pub setup: Box<dyn Fn() -> Prepared>,
}

/// The Raydium-path instructions through 0–3 wrapper levels: every trading path peaks at 3
/// (short.win 1 → venue 2 → token 3), so two wrapper levels fit (peak 5 =
/// `MAX_INSTRUCTION_STACK_DEPTH` on mainnet) and three do not; the instructions without a
/// venue CPI peak at 2 and fit three.
pub fn depth_cases() -> Vec<DepthCase> {
    let mut v = Vec::new();
    for f in VARIANTS {
        v.push(DepthCase {
            handler: "open_short",
            case: format!("{}: first open, normalising", label(f)),
            direct_peak: 3,
            setup: Box::new(move || open_normalising(f)),
        });
        v.push(DepthCase {
            handler: "close_short",
            case: label(f).into(),
            direct_peak: 3,
            setup: Box::new(move || close(f, false)),
        });
        v.push(DepthCase {
            handler: "liquidate",
            case: label(f).into(),
            direct_peak: 3,
            setup: Box::new(move || liquidate(f)),
        });
        v.push(DepthCase {
            handler: "expire",
            case: label(f).into(),
            direct_peak: 3,
            setup: Box::new(move || expire(f)),
        });
        v.push(DepthCase {
            handler: "sol_settle",
            case: format!("{}: S3 on a halted pool", label(f)),
            direct_peak: 2,
            setup: Box::new(move || settle_s3(f)),
        });
        v.push(DepthCase {
            handler: "crank_price",
            case: format!("{}: pool halted, chains start", label(f)),
            direct_peak: 2,
            setup: Box::new(move || crank_halted(f)),
        });
    }
    v.push(DepthCase {
        handler: "create_market",
        case: label(T22).into(),
        direct_peak: 2,
        setup: Box::new(|| create_market(T22)),
    });
    v.push(DepthCase {
        handler: "flag_venue_upgraded",
        case: "AMM v4, the dumped ProgramData".into(),
        direct_peak: 2,
        setup: Box::new(|| flag_raydium(GUARD_RAYDIUM_V4)),
    });
    // M4: pool selection makes only its event CPI.
    v.push(DepthCase {
        handler: "propose_pool",
        case: format!("{}: zero-position adoption", label(T22)),
        direct_peak: 2,
        setup: Box::new(|| propose_zero_position(T22)),
    });
    v.push(DepthCase {
        handler: "execute_pool_switch",
        case: format!("{}: fast adoption, a position open", label(FART)),
        direct_peak: 2,
        setup: Box::new(|| window_due(FART, false)),
    });
    v
}

/// The outcome of one wrapped run: the peak height it reached, or the error it failed with.
pub fn run_wrapped(case: &DepthCase, levels: usize) -> Result<u8, String> {
    let Prepared {
        mut env,
        ix,
        signers,
    } = (case.setup)();
    load_wrapper(&mut env);
    let refs: Vec<&Keypair> = signers.iter().collect();
    match env.send(&[wrap(&ix, levels)], &refs) {
        Ok(meta) => Ok(peak_height(&meta)),
        Err(f) => Err(format!("{:?}", f.err)),
    }
}
