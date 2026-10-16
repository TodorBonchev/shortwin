//! M2 CU bench and T-TRACE (ARCHITECTURE §6.1, §6.2, §8.2 T-CU / T-TRACE, §9 M2).
//!
//! Every [`Case`] builds its pre-state with the LiteSVM harness (`shortwin_litesvm_tests`:
//! short.win's **test-venue** SBF build with `VenueKind::Mock` (`scripts/build-test-venue.sh`,
//! or `SHORTWIN_TEST_VENUE_SO`), the mock venue (`MOCK_CPAMM_SO`), the SPL Token (p-token
//! under the mainnet feature set) ELF LiteSVM bundles, mainnet's Token-2022 (since M10,
//! `fork::load_mainnet_spl`; `SHORTWIN_BUNDLED_SPL=1` keeps LiteSVM's), the clock under test
//! control), then
//!
//! 1. copies every account the instruction names into a fresh Mollusk 0.15.1 and loads the
//!    same ELFs into its program cache, with LiteSVM's clock and rent;
//! 2. runs the instruction on Mollusk: the figure the budget applies to;
//! 3. runs the same instruction on LiteSVM through `Env::send` (the invariant hooks run too):
//!    a cross-check of the two harnesses, and the instruction's trace entries (the
//!    instruction plus every CPI at any depth, ARCH §6.2).
//!
//! Every M2 handler has landed, so every case must execute on both harnesses (a refusal fails
//! the bench). The scenarios of the keeper exits are **self-checked** with `shortwin-math` (the
//! position is liquidatable with a fresh reference and within the fixed bound; expired, not
//! CR-liquidatable and within the expiry bound; S3-due on a flagged route), so a case measures
//! the instruction's executing path, not a refusal.
//!
//! The worst-case transactions of T-TRACE are [`worst_case_transactions`].

use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use mollusk_svm::Mollusk;
use mollusk_svm::program::keyed_account_for_system_program;
use shortwin::PauseScope;
use shortwin::state::{PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER, ShortPosition};
use shortwin::venues::ExtrasSel;
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::drive::Actor;
use shortwin_litesvm_tests::engine::{
    Borrower, MarketSpec, MockMarket, VENUE_HINT_MOCK, add_margin_ix, close_short_ix,
    crank_price_ix, expire_ix, liquidate_ix, open_short_ix, set_route_broken_ix,
    set_venue_extras_ix, sol_settle_ix, sweep_trader_ix,
};
use shortwin_litesvm_tests::mock;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{Env, TxResult, pda, read};
use shortwin_math::bounds;
use shortwin_math::observe::Ring;
use shortwin_math::quote::{cpmm, liquidity, reprice};
use shortwin_math::risk;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_rent::Rent;
use solana_signer::Signer;

/// Positions of this size fit every default cap (≈ 3 SOL of notional on the default pool;
/// the engine tests' `Q`).
pub const Q: u64 = 3_000_000_000_000;

// ---- budgets ------------------------------------------------------------------------------

/// A compute-unit budget and where it comes from.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// Compute units.
    pub cu: u64,
    /// Source of the figure.
    pub source: &'static str,
}

/// ARCH §6.1 `open_short` RaydiumV4 / CPMM (the mock trades like ARCH §2.4.1's Raydium row).
pub const OPEN: Budget = Budget {
    cu: 150_000,
    source: "§6.1 `open_short` RaydiumV4 / CPMM",
};
/// ARCH §6.1 has no Raydium `close_short` row: the Raydium exit row (the engine's
/// `trace_cu.rs` uses the same).
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
/// `add_margin` has no §6.1 row: the lender-SOL class (the other System-transfer
/// instructions that do not write `Market`).
pub const MARGIN: Budget = Budget {
    cu: 60_000,
    source: "no §6.1 row; §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class",
};
/// Emergency flag writes (no §6.1 row; the M1 bench's admin flag class).
pub const ADMIN_FLAG: Budget = Budget {
    cu: 60_000,
    source: "no §6.1 row; admin flag class (M1 bench)",
};
/// Admin writes that validate a payload (no §6.1 row; the M1 bench's admin class).
pub const ADMIN: Budget = Budget {
    cu: 200_000,
    source: "no §6.1 row; admin class (M1 bench)",
};
/// `sweep_trader` (ARCH §3.9; built in M2 by M2-SEC-4) has no §6.1 row: the lender-SOL class
/// (a lender instruction's prologue plus System / SPL Token CPIs).
pub const SWEEP: Budget = Budget {
    cu: 60_000,
    source: "no §6.1 row (§3.9, M2-SEC-4); §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class",
};

/// ARCH §6.2's instruction-trace figure for a case (Raydium column; the mock trades like
/// it), when the table has one.
#[derive(Clone, Copy, Debug)]
pub struct TraceRow {
    /// The table's count for the plain instruction.
    pub arch: usize,
    /// The table's maximum (with normalisation).
    pub arch_max: usize,
}

// ---- cases --------------------------------------------------------------------------------

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
    pub case: &'static str,
    /// The limit the Mollusk figure must stay within.
    pub budget: Budget,
    /// ARCH §6.2's trace figure, when it has one.
    pub trace: Option<TraceRow>,
    /// Builds the pre-state and the instruction.
    pub setup: fn() -> Prepared,
}

/// What [`measure`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Compute units on Mollusk and on LiteSVM, and LiteSVM's trace entries.
    Measured {
        mollusk: u64,
        litesvm: u64,
        trace: usize,
    },
}

/// A path under the repository root.
pub fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

/// The accounts `ix` names, as LiteSVM holds them (missing ones as empty system accounts;
/// the System program as Mollusk's builtin account).
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
/// ProgramData after its 45-byte header). `None` for builtins.
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

/// A Mollusk with short.win's test-venue build, every other program `accounts` names (the
/// mock venue, the token programs), and LiteSVM's clock and rent.
fn mollusk_for(env: &Env, accounts: &[(Pubkey, Account)]) -> Mollusk {
    let mut mollusk = Mollusk::default();
    let so = shortwin_litesvm_tests::test_venue_so_path();
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

/// Runs one case (see the crate docs). `Err` describes a failed run.
pub fn measure(case: &Case) -> Result<Outcome, String> {
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
        (Ok(()), Ok(meta)) => Ok(Outcome::Measured {
            mollusk: result.compute_units_consumed,
            litesvm: meta.compute_units_consumed,
            trace: trace_entries(&meta, 0),
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
pub fn trace_entries(meta: &litesvm::types::TransactionMetadata, ix: usize) -> usize {
    1 + meta.inner_instructions.get(ix).map_or(0, Vec::len)
}

/// Instruction-trace entries of a whole transaction (ARCH §6.2: every top-level instruction,
/// ComputeBudget included, plus every CPI at any depth).
pub fn tx_trace_entries(meta: &litesvm::types::TransactionMetadata, top_level: usize) -> usize {
    top_level + meta.inner_instructions.iter().map(Vec::len).sum::<usize>()
}

// ---- scenario builders ----------------------------------------------------------------------

#[track_caller]
fn ok(res: TxResult) -> litesvm::types::TransactionMetadata {
    shortwin_litesvm_tests::errors::ok(res)
}

fn prepared(env: Env, ix: Instruction, signers: &[&Keypair]) -> Prepared {
    Prepared {
        env,
        ix,
        signers: signers.iter().map(|k| k.insecure_clone()).collect(),
    }
}

/// A test-venue environment with one mock market on `program`, cranked once.
pub fn market_on(program: TokenProgram) -> (Env, MockMarket) {
    let mut env = Env::test_venue();
    let mm = env.mock_market(MarketSpec {
        program,
        ..MarketSpec::default()
    });
    env.crank(&mm);
    (env, mm)
}

/// `open_short(q)` for `b` with no bounds of its own.
pub fn open_ix(mm: &MockMarket, b: &Borrower, nonce: u64, q: u64) -> Instruction {
    open_short_ix(
        mm,
        &b.key(),
        nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    )
}

/// A mock market with one open position `q = Q`, two cranked seconds after the open.
pub fn with_position(program: TokenProgram) -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm) = market_on(program);
    let mut b = env.borrower();
    let (res, nonce) = env.open(&mm, &mut b, Q);
    ok(res);
    env.tick(&mm, 2);
    (env, mm, b, nonce)
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

/// Donates to both trader PDAs, so the next leg normalises (ARCH §2.4 rule 5; the runtime
/// refuses a new account below the rent-exempt minimum, hence 1,000,000 lamports).
pub fn donate_to_traders(env: &mut Env, mm: &MockMarket) {
    let griefer = env.funded_keypair();
    ok(env.send(
        &[
            transfer(&griefer, &mm.keys.trader, 1_000_000),
            transfer(&griefer, &mm.trader_wsol, 1_000_000),
        ],
        &[&griefer],
    ));
}

/// An actor with ample base tokens and wrapped SOL.
pub fn big_actor(env: &mut Env, mm: &MockMarket) -> Actor {
    env.actor(mm, 500_000_000_000_000, 5_000_000_000_000)
}

/// The mock pool's `(X, Y)` pricing reserves and fee.
fn pool_now(env: &Env, mm: &MockMarket) -> (u128, u128, u32) {
    let r = mock::reserves(&env.svm, mm.mock());
    let fee = mock::state(&env.svm, &mm.mock().pool).fee_ppm;
    (u128::from(r.base), u128::from(r.quote), fee)
}

/// `B̂(q)` on `(x, y)` at `fee_ppm` (`quote::cpmm`, zero creator fee: the mock's arithmetic).
fn buy_on(q: u64, x: u128, y: u128, fee_ppm: u32) -> Option<u64> {
    cpmm::buy_exact_out(q, y, x, &mock::cpmm_fees(fee_ppm)).map(|b| b.cost)
}

/// `σ` of buying `q` on `(x, y)` at `fee_ppm` (SPEC S2 slippage, `risk::sigma_xy`).
fn sigma_on(q: u64, x: u128, y: u128, fee_ppm: u32) -> u64 {
    let net = cpmm::buy_exact_out(q, y, x, &mock::cpmm_fees(fee_ppm)).map(|b| b.net);
    risk::sigma_xy(net, q, x, y).unwrap_or(u64::MAX)
}

/// The position's settlement keeper reward `K_s` (locked terms).
fn k_s_of(p: &ShortPosition) -> u64 {
    risk::k_s(p.v, &p.settle_keeper_terms()).unwrap_or(0)
}

/// A liquidatable position with a fresh reference within the fixed bound (ARCH §3.6
/// preconditions 1–6), self-checked with `shortwin-math`: the price ×2.3 held for 40 cranked
/// seconds (both CR legs below `cr_liq`), the keeper's crank of this second done.
pub fn liquidatable() -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let actor = big_actor(&mut env, &mm);
    let spot = env.spot(&mm);
    env.push_price(&mm, &actor, spot * 23 / 10);
    env.tick(&mm, 40);
    // Self-check (the handler is the liquidation track's).
    let p = env.position(&mm, &b.key(), nonce);
    let m = env.market(&mm.market());
    let now = env.now();
    let (x, y, fee) = pool_now(&env, &mm);
    let risk_fee = fee.min(u32::from(p.venue_fee_hard_cap_bps) * 100);
    let b_live = buy_on(p.q, x, y, fee).expect("B̂_live");
    assert!(
        risk::cr_below(p.v, Some(b_live), p.cr_liq_bps).unwrap(),
        "spot leg: CR below cr_liq"
    );
    let obs = m.observation();
    let ema_leg = shortwin_math::persist::liq_ema_leg(
        &obs.ring,
        risk::ring_below(p.v, p.cr_liq_bps, |x, y| buy_on(p.q, x, y, risk_fee)),
        p.liq_persist_n,
    )
    .unwrap();
    assert!(ema_leg, "EMA leg on the newest liq_persist_n entries");
    assert!(obs.last.is_fresh(now, p.ref_max_age_s), "fresh reference");
    let p_ref = bounds::liq_reference(&obs.ema, &obs.ring, &obs.last, now, &p.exit_params())
        .unwrap()
        .expect("P_ref");
    let l_b = bounds::ref_liquidity(liquidity(x, y).unwrap(), &obs.ema, &obs.ring, None).unwrap();
    let (rx, ry) = reprice(l_b, p_ref).expect("reprice");
    let b_ref = buy_on(p.q, rx, ry, risk_fee).expect("B̂_ref");
    let k_s = k_s_of(&p);
    let max_in = bounds::max_in_liq(b_ref, p.delta_liq_bps, p.v, k_s, p.ert).expect("max_in");
    assert!(
        b_live <= max_in,
        "within the fixed bound: {b_live} > {max_in}"
    );
    assert!(
        risk::buyback_allowed(
            &risk::Budget {
                v: p.v,
                k_s,
                ert: p.ert
            },
            Some(b_live),
            sigma_on(p.q, x, y, fee),
            u32::from(p.max_liq_slip_bps)
        )
        .unwrap(),
        "normal buyback allowed"
    );
    (env, mm, b, nonce)
}

/// An expired position that is not CR-liquidatable, with a fresh reference within the expiry
/// bound (ARCH §3.7), self-checked.
pub fn expired() -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let p = env.position(&mm, &b.key(), nonce);
    env.warp(p.deadline - env.now() + 10);
    env.tick(&mm, 3);
    let m = env.market(&mm.market());
    let now = env.now();
    assert!(now >= p.deadline, "expired");
    let (x, y, fee) = pool_now(&env, &mm);
    let b_live = buy_on(p.q, x, y, fee).expect("B̂_live");
    assert!(
        !risk::cr_below(p.v, Some(b_live), p.cr_liq_bps).unwrap(),
        "not CR-liquidatable"
    );
    let obs = m.observation();
    let p_ref = bounds::expiry_reference(&obs.ema, &obs.ring, &obs.last, now, &p.exit_params())
        .unwrap()
        .expect("fresh reference");
    let l_b = bounds::ref_liquidity(liquidity(x, y).unwrap(), &obs.ema, &obs.ring, None).unwrap();
    let (rx, ry) = reprice(l_b, p_ref).expect("reprice");
    let risk_fee = fee.min(u32::from(p.venue_fee_hard_cap_bps) * 100);
    let b_ref = buy_on(p.q, rx, ry, risk_fee).expect("B̂_ref");
    let widened = u128::from(b_ref) * (10_000 + u128::from(p.delta_exp_bps)) / 10_000;
    assert!(u128::from(b_live) <= widened, "within ⌊B̂_ref·(1 + δ_exp)⌋");
    assert!(b_live * 10 < p.v * 9, "rent and the fee leave room (V ≫ B̂)");
    (env, mm, b, nonce)
}

/// An expired position on a market whose route Emergency flagged, with the S3 chain unbroken
/// for `s3_wait_s` + 30 s (cranks every 30 s ≤ `S3_OBS_GAP_S`): S3-due (ARCH §3.8 step 4,
/// §5.3), self-checked on the chain.
pub fn s3_due() -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm, b, nonce) = expired();
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    env.crank(&mm);
    let since = env.market(&mm.market()).no_route_since;
    assert_eq!(since, env.now(), "the flag starts the S3 chain");
    let p = env.position(&mm, &b.key(), nonce);
    let steps = (i64::from(p.s3_wait_s) + 30) / 30;
    for _ in 0..steps {
        env.warp(30);
        env.crank(&mm);
    }
    let m = env.market(&mm.market());
    assert_eq!(m.no_route_since, since, "the chain is unbroken");
    assert!(
        env.now() >= since + i64::from(p.s3_wait_s),
        "s3_wait_s elapsed"
    );
    (env, mm, b, nonce)
}

// ---- setups -------------------------------------------------------------------------------

fn open_first(program: TokenProgram) -> Prepared {
    let (mut env, mm) = market_on(program);
    env.tick(&mm, 1);
    let b = env.borrower();
    let ix = open_ix(&mm, &b, 0, Q);
    prepared(env, ix, &[&b.kp])
}

fn open_first_spl() -> Prepared {
    open_first(TokenProgram::Spl)
}

fn open_first_t22() -> Prepared {
    open_first(TokenProgram::Token2022)
}

fn open_second_spl() -> Prepared {
    let (env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let ix = open_ix(&mm, &b, nonce + 1, Q / 2);
    prepared(env, ix, &[&b.kp])
}

fn open_normalising() -> Prepared {
    let (mut env, mm) = market_on(TokenProgram::Spl);
    env.tick(&mm, 1);
    donate_to_traders(&mut env, &mm);
    let b = env.borrower();
    let ix = open_ix(&mm, &b, 0, Q);
    prepared(env, ix, &[&b.kp])
}

fn close_on(program: TokenProgram, normalise: bool) -> Prepared {
    let (mut env, mm, b, nonce) = with_position(program);
    if normalise {
        donate_to_traders(&mut env, &mm);
    }
    let ix = close_short_ix(&mm, &b.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
    prepared(env, ix, &[&b.kp])
}

fn close_spl() -> Prepared {
    close_on(TokenProgram::Spl, false)
}

fn close_t22() -> Prepared {
    close_on(TokenProgram::Token2022, false)
}

fn close_normalising() -> Prepared {
    close_on(TokenProgram::Spl, true)
}

fn add_margin() -> Prepared {
    let (env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let ix = add_margin_ix(&mm, &b.key(), nonce, 1_000_000_000);
    prepared(env, ix, &[&b.kp])
}

fn liquidate() -> Prepared {
    let (mut env, mm, b, nonce) = liquidatable();
    let keeper = env.funded_keypair();
    let ix = liquidate_ix(&mm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn expire() -> Prepared {
    let (mut env, mm, b, nonce) = expired();
    let keeper = env.funded_keypair();
    let ix = expire_ix(&mm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn settle_noop() -> Prepared {
    let (mut env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let keeper = env.funded_keypair();
    let ix = sol_settle_ix(&env, &mm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn settle_s3() -> Prepared {
    let (mut env, mm, b, nonce) = s3_due();
    let keeper = env.funded_keypair();
    let ix = sol_settle_ix(&env, &mm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn crank_roll() -> Prepared {
    let (mut env, mm, _b, _n) = with_position(TokenProgram::Spl);
    env.warp(1);
    prepared(env, crank_price_ix(&mm), &[])
}

fn crank_same_second() -> Prepared {
    let (env, mm, _b, _n) = with_position(TokenProgram::Spl);
    prepared(env, crank_price_ix(&mm), &[])
}

fn crank_chain_start() -> Prepared {
    let (mut env, mm, _b, _n) = with_position(TokenProgram::Spl);
    mock::update_knobs(&mut env, &mm.pool, |k: &mut mock::Knobs| k.disabled = true);
    env.warp(1);
    prepared(env, crank_price_ix(&mm), &[])
}

fn crank_t22() -> Prepared {
    let (mut env, mm, _b, _n) = with_position(TokenProgram::Token2022);
    env.warp(1);
    prepared(env, crank_price_ix(&mm), &[])
}

fn crank_list_only() -> Prepared {
    let mut env = Env::test_venue();
    let (keys, _auth) = env.install_market(TokenProgram::Spl, 6);
    env.warp(1);
    let ix = shortwin_litesvm_tests::ix::with_remaining(
        Instruction {
            program_id: shortwin::ID,
            accounts: anchor_lang::ToAccountMetas::to_account_metas(
                &shortwin::accounts::CrankPrice {
                    config: pda::config(),
                    market: keys.market,
                    mint: keys.mint,
                    oracle_cache: pda::oracle_cache(),
                    event_authority: pda::event_authority(),
                    program: shortwin::ID,
                },
                None,
            ),
            data: anchor_lang::InstructionData::data(&shortwin::instruction::CrankPrice {}),
        },
        &[
            AccountMeta::new_readonly(pda::bonding_curve(&keys.mint), false),
            AccountMeta::new_readonly(pda::canonical_pool(&keys.mint), false),
        ],
    );
    prepared(env, ix, &[])
}

fn route_broken_market() -> Prepared {
    let (env, mm, _b, _n) = with_position(TokenProgram::Spl);
    let ea = env.pause_authority.insecure_clone();
    let ix = set_route_broken_ix(&ea.pubkey(), PauseScope::Market, 0, true, Some(mm.market()));
    prepared(env, ix, &[&ea])
}

fn route_broken_venue() -> Prepared {
    let (env, _mm, _b, _n) = with_position(TokenProgram::Spl);
    let ea = env.pause_authority.insecure_clone();
    let ix = set_route_broken_ix(&ea.pubkey(), PauseScope::Global, 5, true, None);
    prepared(env, ix, &[&ea])
}

fn seed_const(b: &[u8]) -> SeedPartArg {
    let mut bytes = [0u8; 32];
    bytes[..b.len()].copy_from_slice(b);
    SeedPartArg {
        kind: SEED_CONST,
        len: u8::try_from(b.len()).unwrap_or(32),
        bytes,
    }
}

/// Four derivation templates of four seeds each: the largest set `set_venue_extras` writes.
pub fn largest_extras_set() -> Vec<ExtraEntryArg> {
    (0..4u8)
        .map(|i| ExtraEntryArg {
            kind: 2,
            writable: i % 2 == 0,
            key: Pubkey::new_from_array([7 + i; 32]),
            seeds: vec![
                seed_const(b"pool-v2"),
                SeedPartArg {
                    kind: SEED_PLACEHOLDER,
                    len: PLACEHOLDER_MINT,
                    bytes: [0; 32],
                },
                seed_const(&[i; 32]),
                seed_const(&[i]),
            ],
        })
        .collect()
}

fn venue_extras_largest() -> Prepared {
    let (env, _mm, _b, _n) = with_position(TokenProgram::Spl);
    let ea = env.pause_authority.insecure_clone();
    let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, 0, largest_extras_set());
    prepared(env, ix, &[&ea])
}

fn venue_extras_rotate() -> Prepared {
    let (mut env, _mm, _b, _n) = with_position(TokenProgram::Spl);
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_venue_extras_ix(
            &ea.pubkey(),
            VENUE_HINT_MOCK,
            0,
            largest_extras_set(),
        )],
        &[&ea],
    ));
    let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, 0, largest_extras_set());
    prepared(env, ix, &[&ea])
}

/// The rotation that also promotes a week-old `current` to `stable` (M2-SEC-2): the most a
/// `set_venue_extras` writes (three 4 × 4 sets).
fn venue_extras_promote() -> Prepared {
    let (mut env, _mm, _b, _n) = with_position(TokenProgram::Spl);
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_venue_extras_ix(
            &ea.pubkey(),
            VENUE_HINT_MOCK,
            0,
            largest_extras_set(),
        )],
        &[&ea],
    ));
    env.warp(shortwin::constants::EXTRAS_STABLE_S);
    let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, 0, largest_extras_set());
    prepared(env, ix, &[&ea])
}

/// `sweep_trader` with something everywhere: the trader and the `trader_wsol` address donated
/// to, and `ATA(trader, WSOL)` created and funded (closed by the sweep); lenders present, so
/// the SOL vests.
fn sweep_trader_donated() -> Prepared {
    let (mut env, mm, _b, _n) = with_position(TokenProgram::Spl);
    donate_to_traders(&mut env, &mm);
    let payer = env.payer.insecure_clone();
    let ata = shortwin_litesvm_tests::tokens::create_ata(
        &mut env.svm,
        &payer,
        &mm.keys.trader,
        &shortwin::constants::WSOL_MINT,
        TokenProgram::Spl,
    );
    mock::wrap(&mut env, &ata, 1_000_000);
    env.warp(1);
    let ix = sweep_trader_ix(&mm.market(), None);
    prepared(env, ix, &[])
}

/// Every M2 instruction (ARCH §9 M2), in the order of ARCH §3.4–§3.9 and §3.1's O6 powers.
pub fn cases() -> Vec<Case> {
    let open_row = Some(TraceRow {
        arch: 13,
        arch_max: 15,
    });
    let exit_row = Some(TraceRow {
        arch: 9,
        arch_max: 11,
    });
    let crank_row = Some(TraceRow {
        arch: 2,
        arch_max: 2,
    });
    let settle_row = Some(TraceRow {
        arch: 2,
        arch_max: 5,
    });
    let c = |handler, case, budget, trace, setup| Case {
        handler,
        case,
        budget,
        trace,
        setup,
    };
    vec![
        c(
            "open_short",
            "first open of a wallet, SPL Token base (position + exposure created)",
            OPEN,
            open_row,
            open_first_spl as fn() -> Prepared,
        ),
        c(
            "open_short",
            "first open of a wallet, Token-2022 base",
            OPEN,
            open_row,
            open_first_t22,
        ),
        c(
            "open_short",
            "second position of a wallet (exposure exists), SPL",
            OPEN,
            open_row,
            open_second_spl,
        ),
        c(
            "open_short",
            "with normalisation (trader and trader_wsol donated to), SPL",
            OPEN,
            open_row,
            open_normalising,
        ),
        c("close_short", "SPL Token base", CLOSE, exit_row, close_spl),
        c("close_short", "Token-2022 base", CLOSE, exit_row, close_t22),
        c(
            "close_short",
            "with normalisation, SPL",
            CLOSE,
            exit_row,
            close_normalising,
        ),
        c(
            "add_margin",
            "1 SOL to an open position",
            MARGIN,
            None,
            add_margin,
        ),
        c(
            "liquidate",
            "price ×2.3 held 40 s: both legs, fresh reference, within the fixed bound",
            EXIT,
            exit_row,
            liquidate,
        ),
        c(
            "expire",
            "past the deadline, not CR-liquidatable, within the expiry bound",
            EXIT,
            exit_row,
            expire,
        ),
        c(
            "sol_settle",
            "healthy position: the no-change call (a keeper's per-second crank)",
            SETTLE,
            settle_row,
            settle_noop,
        ),
        c(
            "sol_settle",
            "S3: expired, route flagged, chain unbroken for s3_wait_s",
            SETTLE,
            settle_row,
            settle_s3,
        ),
        c(
            "crank_price",
            "roll + record (first crank of a second), SPL",
            CRANK,
            crank_row,
            crank_roll,
        ),
        c(
            "crank_price",
            "record only (same second again)",
            CRANK,
            crank_row,
            crank_same_second,
        ),
        c(
            "crank_price",
            "roll + record, Token-2022 base",
            CRANK,
            crank_row,
            crank_t22,
        ),
        c(
            "crank_price",
            "venue halted: roll, no record, both chains start (+ RouteChainChanged)",
            CRANK,
            crank_row,
            crank_chain_start,
        ),
        c(
            "crank_price",
            "list-only (`None`) market, detection set",
            CRANK,
            crank_row,
            crank_list_only,
        ),
        c(
            "set_route_broken",
            "one market",
            ADMIN_FLAG,
            None,
            route_broken_market,
        ),
        c(
            "set_route_broken",
            "venue bit (global)",
            ADMIN_FLAG,
            None,
            route_broken_venue,
        ),
        c(
            "set_venue_extras",
            "4 templates × 4 seeds (the largest set)",
            ADMIN,
            None,
            venue_extras_largest,
        ),
        c(
            "set_venue_extras",
            "rotate current → previous, 4 × 4",
            ADMIN,
            None,
            venue_extras_rotate,
        ),
        c(
            "set_venue_extras",
            "rotate a week-old current → stable and previous, 4 × 4 (M2-SEC-2)",
            ADMIN,
            None,
            venue_extras_promote,
        ),
        c(
            "sweep_trader",
            "trader, trader_wsol and ATA(trader, WSOL) donated to; S > 0 (vests) (M2-SEC-4)",
            SWEEP,
            None,
            sweep_trader_donated,
        ),
    ]
}

// ---- T-TRACE --------------------------------------------------------------------------------

/// `ComputeBudget111111111111111111111111111111`.
pub const COMPUTE_BUDGET: Pubkey =
    Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");

/// `SetComputeUnitLimit(units)` and `SetComputeUnitPrice(micro_lamports)`: the two
/// instructions every client and keeper transaction starts with (ARCH §6.1, §6.2).
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

/// A worst-case transaction of M2 and what ARCH §6.2 allows it.
pub struct TraceTx {
    /// What the transaction is.
    pub name: &'static str,
    /// ARCH §6.2's figure for it, or the reason there is none.
    pub arch: &'static str,
    /// Builds the pre-state, the instructions and the signers.
    pub setup: fn() -> (Env, Vec<Instruction>, Vec<Keypair>),
}

/// The measured trace of one [`TraceTx`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceOutcome {
    /// Entries and CU of the whole transaction, and its legacy wire size in bytes.
    Measured {
        entries: usize,
        cu: u64,
        legacy_bytes: usize,
    },
}

/// Bytes of a compact-u16 ("shortvec") length prefix.
fn shortvec_len(n: usize) -> usize {
    match n {
        0..=0x7f => 1,
        0x80..=0x3fff => 2,
        _ => 3,
    }
}

/// Wire size of `ixs` as one **legacy** transaction paid by `payer` (no address lookup
/// table): signatures, header, account keys, blockhash and compiled instructions — the figure
/// the 1,232-byte packet limit applies to (ARCH §6.4; LiteSVM does not enforce it). A
/// T-SIZE preview: the protocol ALT (A28) removes the static keys from the inline list.
pub fn legacy_size(ixs: &[Instruction], payer: &Pubkey) -> usize {
    let msg = solana_message::Message::new(ixs, Some(payer));
    let sigs = usize::from(msg.header.num_required_signatures);
    let keys = msg.account_keys.len();
    let body: usize = msg
        .instructions
        .iter()
        .map(|ci| {
            1 + shortvec_len(ci.accounts.len())
                + ci.accounts.len()
                + shortvec_len(ci.data.len())
                + ci.data.len()
        })
        .sum();
    shortvec_len(sigs)
        + 64 * sigs
        + 3
        + shortvec_len(keys)
        + 32 * keys
        + 32
        + shortvec_len(msg.instructions.len())
        + body
}

/// Runs one worst-case transaction on LiteSVM (every hook after every instruction).
pub fn run_trace(t: &TraceTx) -> Result<TraceOutcome, String> {
    let (mut env, ixs, signers) = (t.setup)();
    let legacy_bytes = legacy_size(&ixs, &env.payer.pubkey());
    let refs: Vec<&Keypair> = signers.iter().collect();
    let res = env.send(&ixs, &refs);
    match res {
        Ok(meta) => Ok(TraceOutcome::Measured {
            entries: tx_trace_entries(&meta, ixs.len()),
            cu: meta.compute_units_consumed,
            legacy_bytes,
        }),
        Err(f) => Err(format!(
            "{}: failed with {:?}; logs:\n{}",
            t.name,
            f.err,
            f.meta.pretty_logs()
        )),
    }
}

fn with_cb(mut ixs: Vec<Instruction>) -> Vec<Instruction> {
    let mut v = compute_budget(1_400_000, 1).to_vec();
    v.append(&mut ixs);
    v
}

fn tx_open_normalising() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let Prepared { env, ix, signers } = open_normalising();
    (env, with_cb(vec![ix]), signers)
}

fn tx_close_normalising() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let Prepared { env, ix, signers } = close_normalising();
    (env, with_cb(vec![ix]), signers)
}

fn tx_sandwiched_open() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, mm) = market_on(TokenProgram::Spl);
    env.tick(&mm, 1);
    donate_to_traders(&mut env, &mm);
    let attacker = big_actor(&mut env, &mm);
    let b = env.borrower();
    let s = env.sandwich_ixs(&mm, &attacker, mock::Dir::BaseIn, 1_000_000_000_000);
    let open = open_ix(&mm, &b, 0, Q);
    let signers = vec![attacker.kp.insecure_clone(), b.kp.insecure_clone()];
    (env, with_cb(vec![s.front, open, s.back]), signers)
}

/// `k` opens of one wallet in one transaction (the first creates the exposure record).
pub fn opens_of_one_wallet(k: u64) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, mm) = market_on(TokenProgram::Spl);
    env.tick(&mm, 1);
    let b = env.borrower();
    let ixs = (0..k).map(|n| open_ix(&mm, &b, n, Q / 4)).collect();
    let signers = vec![b.kp.insecure_clone()];
    (env, with_cb(ixs), signers)
}

fn tx_four_opens() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    opens_of_one_wallet(4)
}

fn tx_crank_batch() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    // Six markets whose venues just halted: every crank also starts a route chain (3 entries,
    // the crank's worst case).
    let mut env = Env::test_venue();
    let markets: Vec<MockMarket> = (0..6)
        .map(|_| env.mock_market(MarketSpec::default()))
        .collect();
    for mm in &markets {
        env.crank(mm);
        mock::update_knobs(&mut env, &mm.pool, |k: &mut mock::Knobs| k.disabled = true);
    }
    env.warp(1);
    let ixs = markets.iter().map(crank_price_ix).collect();
    (env, with_cb(ixs), vec![])
}

fn tx_margin_batch() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let ixs = (0..20)
        .map(|k| add_margin_ix(&mm, &b.key(), nonce, 1_000_000 + k))
        .collect();
    (env, with_cb(ixs), vec![b.kp.insecure_clone()])
}

fn tx_keeper_liquidation() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, mm, b, nonce) = liquidatable();
    env.warp(1);
    let keeper = env.funded_keypair();
    let ixs = vec![
        crank_price_ix(&mm),
        liquidate_ix(&mm, &keeper.pubkey(), &b.key(), nonce),
    ];
    (env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_expiry() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, mm, b, nonce) = expired();
    env.warp(1);
    let keeper = env.funded_keypair();
    let ixs = vec![
        crank_price_ix(&mm),
        expire_ix(&mm, &keeper.pubkey(), &b.key(), nonce),
    ];
    (env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_settlement() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, mm, b, nonce) = s3_due();
    let keeper = env.funded_keypair();
    let ixs = vec![
        crank_price_ix(&mm),
        sol_settle_ix(&env, &mm, &keeper.pubkey(), &b.key(), nonce),
    ];
    (env, with_cb(ixs), vec![keeper])
}

/// The worst-case transactions of M2 (T-TRACE): each must stay ≤ 64 entries.
pub fn worst_case_transactions() -> Vec<TraceTx> {
    vec![
        TraceTx {
            name: "ComputeBudget ×2 + open_short with normalisation (first open)",
            arch: "2 + ≤ 15 (§6.2 Raydium `open_short` max)",
            setup: tx_open_normalising,
        },
        TraceTx {
            name: "ComputeBudget ×2 + close_short with normalisation",
            arch: "2 + ≤ 11 (§6.2 Raydium exit max)",
            setup: tx_close_normalising,
        },
        TraceTx {
            name: "ComputeBudget ×2 + [front-run swap, open_short with normalisation, back-run swap]",
            arch: "no row (atomic sandwich bundle, §8.2)",
            setup: tx_sandwiched_open,
        },
        TraceTx {
            name: "ComputeBudget ×2 + four opens of one wallet (the most that fit: 2 + 15 + 3·14)",
            arch: "no row",
            setup: tx_four_opens,
        },
        TraceTx {
            name: "ComputeBudget ×2 + crank_price ×6, every crank starting a route chain",
            arch: "2 + 2k = 14 (§6.2, k = 6)",
            setup: tx_crank_batch,
        },
        TraceTx {
            name: "ComputeBudget ×2 + add_margin ×20",
            arch: "no row (3 per add_margin)",
            setup: tx_margin_batch,
        },
        TraceTx {
            name: "ComputeBudget ×2 + crank_price + liquidate (keeper)",
            arch: "≤ 2 + 2 + 19 = 23 (§6.2; 2 + 2 + 11 on Raydium)",
            setup: tx_keeper_liquidation,
        },
        TraceTx {
            name: "ComputeBudget ×2 + crank_price + expire (keeper)",
            arch: "≤ 2 + 2 + 11 on Raydium (§6.2)",
            setup: tx_keeper_expiry,
        },
        TraceTx {
            name: "ComputeBudget ×2 + crank_price + sol_settle S3 (keeper)",
            arch: "2 + 2 + 2 (+3 with a ShareEpoch record) (§6.2)",
            setup: tx_keeper_settlement,
        },
    ]
}

/// The ring of a market (self-checks and reports).
pub fn ring_of(env: &Env, mm: &MockMarket) -> Ring {
    env.market(&mm.market()).ring_math()
}

/// Lamports of `key`.
pub fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}
