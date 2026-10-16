//! Raydium pool selection on the mainnet fork (M4; ARCH §2.5 `None`, §2.6, §3.9): the
//! `propose_pool` / `execute_pool_switch` builders, list-only listings, and **planted candidate
//! pools** — real AMM v4 / CPMM pool accounts cloned from a dumped template onto fresh keys, with
//! their own vaults, LP mint and (CPMM) observation, for any mint, either SOL side, any
//! reserves and any locked LP share. The dumped ELFs trade them exactly like the templates (the
//! venues identify a pool by the accounts it names, never by deriving its address), so a
//! market can switch onto a planted pool and its positions exit there.
//!
//! - [`plant_v4`] / [`plant_cpmm`] ([`PlantSpec`]): a candidate pool; [`spec_like`] copies a
//!   live pool's reserves and LP counter as a starting point.
//! - [`propose_pool_ix`] / [`execute_pool_switch_ix`]: the instructions with their remaining
//!   accounts — a list-only market's detection set `[bonding_curve, canonical_pool]` or the
//!   incumbent's ReadSet, then the candidate's ReadSet ([`crate::raydium::candidate_read_set`]).
//! - [`Env::propose`], [`Env::execute_switch`], [`Env::list_auto`] send them through
//!   [`Env::send`] (the invariant hooks, I15 included); [`RayMarket::on_pool`] is the market's
//!   view after an adoption (the builders of [`crate::engine`] then trade the new pool).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use shortwin::constants::{
    RAYDIUM_CPMM_AUTHORITY, RAYDIUM_V4_AUTHORITY, TOKEN_PROGRAM_ID, WSOL_MINT,
};
use shortwin::state::VenueKind;
use shortwin_venue_state::{mint, raydium_cpmm, raydium_v4};
use solana_account::Account;
use solana_signer::Signer;

use crate::fork::{self, AnyPool, CpmmFixture, CpmmPool, V4Fixture, V4Pool};
use crate::raydium::{self, RayFixture, RayMarket, candidate_read_set, lp_counter};
use crate::tokens::{self, TokenProgram};
use crate::{Env, TxResult, pda, read};

/// A planted pool: the token mint and its program, which side is WSOL, the pricing reserves,
/// the venue's LP counter and the permanently locked share `λ` (the LP mint's supply is
/// `supply_for_lambda(counter, lambda_bps)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantSpec {
    pub mint: Pubkey,
    pub program: TokenProgram,
    /// WSOL is the AMM v4 coin / the CPMM token 0.
    pub sol_first: bool,
    /// The WSOL vault's lamports (= the SOL reserve: no fee counters on a planted pool).
    pub sol: u64,
    /// The token vault's amount.
    pub tokens: u64,
    pub lp_counter: u64,
    pub lambda_bps: u64,
}

impl PlantSpec {
    /// The same spec with the reserves scaled by `num/den` (the price unchanged; `D` scales
    /// by the same factor at a fixed `λ`).
    pub fn scaled(self, num: u64, den: u64) -> Self {
        let s = |v: u64| (u128::from(v) * u128::from(num) / u128::from(den)) as u64;
        Self {
            sol: s(self.sol),
            tokens: s(self.tokens),
            ..self
        }
    }
}

/// A spec with `pool`'s live token, reserves and LP counter, WSOL on `sol_first`'s side, and a
/// locked share of `lambda_bps`.
pub fn spec_like(svm: &LiteSVM, pool: &AnyPool, sol_first: bool, lambda_bps: u64) -> PlantSpec {
    let (mint, program) = pool.token();
    let (x, y) = pool
        .io_reserves(svm, shortwin_math::fees::Direction::Sell)
        .expect("reserves");
    PlantSpec {
        mint,
        program,
        sol_first,
        sol: y,
        tokens: x,
        lp_counter: lp_counter(svm, pool),
        lambda_bps,
    }
}

fn put(d: &mut [u8], off: usize, b: &[u8]) {
    d[off..off + b.len()].copy_from_slice(b);
}

fn decimals_of(svm: &LiteSVM, key: &Pubkey) -> u8 {
    let acc = svm.get_account(key).expect("mint");
    mint::read_base(&acc.owner.to_bytes(), &acc.data)
        .expect("mint")
        .decimals
}

/// The decimals of a dumped mint (read from its fixture, never from the VM).
fn fixture_decimals(key: &Pubkey) -> u8 {
    let f = fork::fixture_at(key).unwrap_or_else(|| panic!("no fixture at {key}"));
    mint::read_base(&f.account.owner.to_bytes(), &f.account.data)
        .expect("mint")
        .decimals
}

/// Loads the fixture at `key` unless the VM already holds the account (a test may have
/// changed it: a planted pool never rewinds the fork).
fn ensure(svm: &mut LiteSVM, key: &Pubkey) {
    if svm.get_account(key).is_none() {
        fork::load_at(svm, key);
    }
}

/// Deploys `program` unless it is deployed.
fn ensure_program(svm: &mut LiteSVM, program: fork::RaydiumProgram) {
    if svm.get_account(&program.id()).is_none() {
        fork::load_program(svm, program);
    }
}

/// A new SPL Token LP mint (`decimals`, authority `authority`) with the supply that gives
/// `spec`'s locked share.
fn plant_lp_mint(env: &mut Env, decimals: u8, authority: &Pubkey, spec: &PlantSpec) -> Pubkey {
    let payer = env.payer.insecure_clone();
    let lp = tokens::create_mint(&mut env.svm, &payer, authority, decimals, TokenProgram::Spl);
    raydium::set_mint_supply(
        &mut env.svm,
        &lp,
        raydium::supply_for_lambda(spec.lp_counter, spec.lambda_bps),
    );
    lp
}

/// The WSOL and token vaults of a planted pool, owned by `authority`.
fn plant_vaults(env: &mut Env, authority: &Pubkey, spec: &PlantSpec) -> (Pubkey, Pubkey) {
    ensure(&mut env.svm, &WSOL_MINT);
    let sol = fork::fund_wsol(env, authority, spec.sol);
    let tok = fork::fund_token(env, authority, &spec.mint, spec.program, spec.tokens);
    (sol, tok)
}

/// Writes `data` as a new account owned by `owner` at a fresh key.
fn plant_account(svm: &mut LiteSVM, owner: Pubkey, data: Vec<u8>) -> Pubkey {
    let key = Pubkey::new_unique();
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        key,
        Account {
            lamports,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("plant account");
    key
}

/// An AMM v4 pool cloned from `template`'s dumped bytes (status, nonce 254, fees, open time)
/// onto fresh keys for `spec` (module docs): new vaults under the authority `5Q544f…`, a new LP
/// mint, the mints, decimals, LP counter and vaults rewritten, `need_take_pnl` zero. SPL Token
/// only (AMM v4 is). The template is read from its fixture; nothing the VM holds is reloaded.
pub fn plant_v4(env: &mut Env, template: V4Fixture, spec: PlantSpec) -> V4Pool {
    assert_eq!(spec.program, TokenProgram::Spl, "AMM v4 is SPL Token only");
    ensure_program(&mut env.svm, fork::RaydiumProgram::AmmV4);
    ensure(&mut env.svm, &RAYDIUM_V4_AUTHORITY);
    ensure(&mut env.svm, &fork::fixture("ray_v4_amm_config").address);
    let mut d = fork::fixture(&format!("{}_pool", template.prefix()))
        .account
        .data
        .clone();
    let info = raydium_v4::read(&raydium_v4::PROGRAM, &d).expect("template pool");
    let lp_decimals = fixture_decimals(&Pubkey::new_from_array(info.lp_mint));
    let (sol_vault, tok_vault) = plant_vaults(env, &RAYDIUM_V4_AUTHORITY, &spec);
    let lp = plant_lp_mint(env, lp_decimals, &RAYDIUM_V4_AUTHORITY, &spec);
    let (coin_mint, pc_mint, coin_vault, pc_vault) = if spec.sol_first {
        (WSOL_MINT, spec.mint, sol_vault, tok_vault)
    } else {
        (spec.mint, WSOL_MINT, tok_vault, sol_vault)
    };
    let coin_dec = u64::from(decimals_of(&env.svm, &coin_mint));
    let pc_dec = u64::from(decimals_of(&env.svm, &pc_mint));
    put(&mut d, 32, &coin_dec.to_le_bytes());
    put(&mut d, 40, &pc_dec.to_le_bytes());
    put(
        &mut d,
        raydium_v4::OFF_NEED_TAKE_PNL_COIN,
        &0u64.to_le_bytes(),
    );
    put(
        &mut d,
        raydium_v4::OFF_NEED_TAKE_PNL_PC,
        &0u64.to_le_bytes(),
    );
    put(&mut d, raydium_v4::OFF_COIN_VAULT, coin_vault.as_ref());
    put(&mut d, raydium_v4::OFF_PC_VAULT, pc_vault.as_ref());
    put(&mut d, raydium_v4::OFF_COIN_VAULT_MINT, coin_mint.as_ref());
    put(&mut d, raydium_v4::OFF_PC_VAULT_MINT, pc_mint.as_ref());
    put(&mut d, raydium_v4::OFF_LP_MINT, lp.as_ref());
    put(
        &mut d,
        raydium_v4::OFF_LP_AMOUNT,
        &spec.lp_counter.to_le_bytes(),
    );
    let pool = plant_account(&mut env.svm, raydium_v4::PROGRAM.into(), d);
    V4Pool {
        which: template,
        pool,
        coin_vault,
        pc_vault,
        coin_mint,
        pc_mint,
        lp_mint: lp,
        sol_is_coin: spec.sol_first,
    }
}

/// The CPMM `ObservationState`'s `pool_id` offset (discriminator, `initialized` bool,
/// `observation_index` u16).
const OBSERVATION_POOL_ID: usize = 11;

/// A CPMM pool cloned from `template`'s dumped bytes (config, status, open time, creator-fee
/// settings) onto fresh keys for `spec` (module docs): new vaults under the authority `GpMZb…`,
/// a new LP mint, a copy of the template's observation account pointing at the new pool, the
/// mints, their programs and decimals, the LP counter and the vaults rewritten, every fee
/// counter zero. The template is read from its fixtures; nothing the VM holds is reloaded.
pub fn plant_cpmm(env: &mut Env, template: CpmmFixture, spec: PlantSpec) -> CpmmPool {
    ensure_program(&mut env.svm, fork::RaydiumProgram::Cpmm);
    ensure(&mut env.svm, &RAYDIUM_CPMM_AUTHORITY);
    let tf = fork::fixture(&format!("{}_pool", template.prefix()));
    let mut d = tf.account.data.clone();
    let st = raydium_cpmm::read_pool(&raydium_cpmm::PROGRAM, &d).expect("template pool");
    let amm_config = Pubkey::new_from_array(st.amm_config);
    ensure(&mut env.svm, &amm_config);
    let lp_decimals = fixture_decimals(&Pubkey::new_from_array(st.lp_mint));
    let (sol_vault, tok_vault) = plant_vaults(env, &RAYDIUM_CPMM_AUTHORITY, &spec);
    let lp = plant_lp_mint(env, lp_decimals, &RAYDIUM_CPMM_AUTHORITY, &spec);
    let tok_prog = spec.program.id();
    let (m0, m1, p0, p1, v0, v1) = if spec.sol_first {
        (
            WSOL_MINT,
            spec.mint,
            TOKEN_PROGRAM_ID,
            tok_prog,
            sol_vault,
            tok_vault,
        )
    } else {
        (
            spec.mint,
            WSOL_MINT,
            tok_prog,
            TOKEN_PROGRAM_ID,
            tok_vault,
            sol_vault,
        )
    };
    // The observation: a copy of the template's, re-pointed at the new pool below.
    let mut obs = fork::fixture_at(&Pubkey::new_from_array(st.observation_key))
        .expect("template observation")
        .account
        .clone();
    assert_eq!(
        &obs.data[OBSERVATION_POOL_ID..OBSERVATION_POOL_ID + 32],
        tf.address.as_ref(),
        "ObservationState.pool_id at offset {OBSERVATION_POOL_ID}"
    );
    let observation = Pubkey::new_unique();
    put(&mut d, raydium_cpmm::OFF_TOKEN_0_VAULT, v0.as_ref());
    put(&mut d, raydium_cpmm::OFF_TOKEN_1_VAULT, v1.as_ref());
    put(&mut d, raydium_cpmm::OFF_LP_MINT, lp.as_ref());
    put(&mut d, raydium_cpmm::OFF_TOKEN_0_MINT, m0.as_ref());
    put(&mut d, raydium_cpmm::OFF_TOKEN_1_MINT, m1.as_ref());
    put(&mut d, raydium_cpmm::OFF_TOKEN_0_PROGRAM, p0.as_ref());
    put(&mut d, raydium_cpmm::OFF_TOKEN_1_PROGRAM, p1.as_ref());
    put(
        &mut d,
        raydium_cpmm::OFF_OBSERVATION_KEY,
        observation.as_ref(),
    );
    d[raydium_cpmm::OFF_MINT_0_DECIMALS] = decimals_of(&env.svm, &m0);
    d[raydium_cpmm::OFF_MINT_1_DECIMALS] = decimals_of(&env.svm, &m1);
    put(
        &mut d,
        raydium_cpmm::OFF_LP_SUPPLY,
        &spec.lp_counter.to_le_bytes(),
    );
    for off in [
        raydium_cpmm::OFF_PROTOCOL_FEES_TOKEN_0,
        raydium_cpmm::OFF_PROTOCOL_FEES_TOKEN_1,
        raydium_cpmm::OFF_FUND_FEES_TOKEN_0,
        raydium_cpmm::OFF_FUND_FEES_TOKEN_1,
        raydium_cpmm::OFF_CREATOR_FEES_TOKEN_0,
        raydium_cpmm::OFF_CREATOR_FEES_TOKEN_1,
    ] {
        put(&mut d, off, &0u64.to_le_bytes());
    }
    let pool = plant_account(&mut env.svm, raydium_cpmm::PROGRAM.into(), d);
    put(&mut obs.data, OBSERVATION_POOL_ID, pool.as_ref());
    env.svm
        .set_account(observation, obs)
        .expect("plant observation");
    CpmmPool {
        which: template,
        pool,
        amm_config,
        observation,
        token_0_vault: v0,
        token_1_vault: v1,
        token_0_mint: m0,
        token_1_mint: m1,
        token_0_program: p0,
        token_1_program: p1,
        lp_mint: lp,
        sol_is_token0: spec.sol_first,
    }
}

/// `propose_pool`'s / `create_market`'s kind of a candidate (4 AMM v4, 5 CPMM).
pub fn kind_of(pool: &AnyPool) -> u8 {
    match pool {
        AnyPool::V4(_) => VenueKind::RaydiumV4 as u8,
        AnyPool::Cpmm(_) => VenueKind::RaydiumCpmm as u8,
    }
}

/// The key of a pool.
pub fn pool_key(pool: &AnyPool) -> Pubkey {
    match pool {
        AnyPool::V4(p) => p.pool,
        AnyPool::Cpmm(p) => p.pool,
    }
}

/// The [`RayFixture`] a pool's market reports (a planted pool: its template).
pub fn fixture_of(pool: &AnyPool) -> RayFixture {
    match pool {
        AnyPool::V4(p) => RayFixture::V4(p.which),
        AnyPool::Cpmm(p) => RayFixture::Cpmm(p.which),
    }
}

/// A list-only market's detection set `[bonding_curve, canonical_pool]` (read-only).
pub fn detection_set(mint: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(pda::bonding_curve(mint), false),
        AccountMeta::new_readonly(pda::canonical_pool(mint), false),
    ]
}

fn build(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction {
        program_id: shortwin::ID,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// `propose_pool(candidate_kind)` for `mint`'s market with `remaining` as remaining accounts
/// (the caller's: any order or content, for the refusal tests).
pub fn propose_pool_raw(
    mint: &Pubkey,
    candidate_kind: u8,
    remaining: &[AccountMeta],
) -> Instruction {
    let ix = build(
        shortwin::accounts::ProposePool {
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            market: pda::market(mint).0,
            mint: *mint,
            oracle_cache: pda::oracle_cache(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ProposePool { candidate_kind },
    );
    crate::ix::with_remaining(ix, remaining)
}

/// `propose_pool` of `candidate` for `mint`'s market: `prefix` (the detection set of a
/// list-only market, [`detection_set`], or the incumbent's ReadSet) then the candidate's
/// ReadSet (read-only).
pub fn propose_pool_ix(
    svm: &LiteSVM,
    mint: &Pubkey,
    prefix: &[AccountMeta],
    candidate: &AnyPool,
) -> Instruction {
    let mut remaining = prefix.to_vec();
    remaining.extend(candidate_read_set(svm, candidate, false));
    propose_pool_raw(mint, kind_of(candidate), &remaining)
}

/// `execute_pool_switch()` for `mint`'s market with `remaining` as remaining accounts.
pub fn execute_pool_switch_raw(mint: &Pubkey, remaining: &[AccountMeta]) -> Instruction {
    let ix = build(
        shortwin::accounts::ExecutePoolSwitch {
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            market: pda::market(mint).0,
            mint: *mint,
            oracle_cache: pda::oracle_cache(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ExecutePoolSwitch {},
    );
    crate::ix::with_remaining(ix, remaining)
}

/// `execute_pool_switch` for `mint`'s market: the incumbent's ReadSet, then the window
/// candidate's.
pub fn execute_pool_switch_ix(
    svm: &LiteSVM,
    mint: &Pubkey,
    incumbent: &AnyPool,
    candidate: &AnyPool,
) -> Instruction {
    let mut remaining = candidate_read_set(svm, incumbent, false);
    remaining.extend(candidate_read_set(svm, candidate, false));
    execute_pool_switch_raw(mint, &remaining)
}

impl Env {
    /// A list-only (`Auto`) listing of `mint` under `program` by a new funded creator.
    pub fn list_auto(&mut self, mint: &Pubkey, program: TokenProgram) -> TxResult {
        let creator = self.funded_keypair();
        let ix = crate::ix::create_market(
            &creator.pubkey(),
            mint,
            &program.id(),
            0,
            &pda::canonical_pool(mint),
        );
        self.send(&[ix], &[&creator])
    }

    /// `propose_pool` of `candidate` on its mint's market: after the detection set when
    /// `incumbent` is `None` (a list-only market), else after the incumbent's ReadSet.
    pub fn propose(&mut self, incumbent: Option<&AnyPool>, candidate: &AnyPool) -> TxResult {
        let (mint, _) = candidate.token();
        let prefix = match incumbent {
            None => detection_set(&mint),
            Some(p) => candidate_read_set(&self.svm, p, false),
        };
        let ix = propose_pool_ix(&self.svm, &mint, &prefix, candidate);
        self.send(&[ix], &[])
    }

    /// `execute_pool_switch` on `candidate`'s mint's market.
    pub fn execute_switch(&mut self, incumbent: &AnyPool, candidate: &AnyPool) -> TxResult {
        let (mint, _) = candidate.token();
        let ix = execute_pool_switch_ix(&self.svm, &mint, incumbent, candidate);
        self.send(&[ix], &[])
    }

    /// `execute_pool_switch` on a list-only market's window (M4-SEC-4): the detection set, then
    /// the window candidate's ReadSet.
    pub fn execute_switch_list_only(&mut self, candidate: &AnyPool) -> TxResult {
        let (mint, _) = candidate.token();
        let mut remaining = detection_set(&mint);
        remaining.extend(candidate_read_set(&self.svm, candidate, false));
        let ix = execute_pool_switch_raw(&mint, &remaining);
        self.send(&[ix], &[])
    }
}

impl RayMarket {
    /// The same market after it adopted `pool`: its keys, lender and `trader_wsol` unchanged,
    /// the venue's account lists those of `pool` (the CPMM config it names now).
    pub fn on_pool(&self, svm: &LiteSVM, pool: AnyPool) -> RayMarket {
        let amm_config = match &pool {
            AnyPool::Cpmm(p) => Some(raydium::cpmm_config_key(svm, p)),
            AnyPool::V4(_) => None,
        };
        RayMarket {
            fixture: fixture_of(&pool),
            pool,
            keys: self.keys,
            trader_wsol: self.trader_wsol,
            lender: self.lender.insecure_clone(),
            lender_account: self.lender_account,
            amm_config,
        }
    }
}

/// The pool a market's cache names now (its `ray_pool`).
pub fn cached_pool(svm: &LiteSVM, market: &Pubkey) -> Pubkey {
    read::zc::<shortwin::state::Market>(svm, market)
        .expect("market")
        .pool_cache[shortwin::state::raydium_cache::RAY_POOL]
}

/// Whether `pool` passes the eligibility floor on locked depth (M4-SEC-1 / M4-SEC-3:
/// `⌊⌊D·isqrt(P)/10⁹⌋·10⁴/pool_lock_min_bps⌋ ≥ pool_min_real_sol`) at `p_ref`, or at its own spot
/// when `p_ref` is `None` — the price a market without one (a listing) judges it at.
pub fn passes_floor(env: &Env, pool: &AnyPool, p_ref: Option<u128>) -> bool {
    let params = env.config().params;
    let Some((x, y)) = pool.io_reserves(&env.svm, shortwin_math::fees::Direction::Sell) else {
        return false;
    };
    let spot = u128::from(y) * shortwin_math::num::WAD / u128::from(x.max(1));
    shortwin_math::pool_select::locked_floor_ok(
        raydium::locked_depth(&env.svm, pool),
        p_ref.unwrap_or(spot),
        u64::from(params.pool_lock_min_bps),
        params.pool_min_real_sol,
    )
}

/// Whether `pool` passes the **window floor** of a collection window's candidate (M4V-1: its
/// locked SOL `⌊D·isqrt(P)/10⁹⌋ ≥ pool_min_real_sol`, no `/λ_min`) at `p_ref`, or at its own spot
/// when `p_ref` is `None`.
pub fn passes_window_floor(env: &Env, pool: &AnyPool, p_ref: Option<u128>) -> bool {
    let params = env.config().params;
    let Some((x, y)) = pool.io_reserves(&env.svm, shortwin_math::fees::Direction::Sell) else {
        return false;
    };
    let spot = u128::from(y) * shortwin_math::num::WAD / u128::from(x.max(1));
    shortwin_math::pool_select::position_floor_ok(
        raydium::locked_depth(&env.svm, pool),
        p_ref.unwrap_or(spot),
        params.pool_min_real_sol,
    )
}

/// "Switch only if all fit" (owner decision of 2026-09-29): the caps `(per position, per market)`
/// for the market of `pool`'s mint, as stored, to carry its open positions onto `pool` — the
/// smaller at `min(P_ema, P_cand)` and at `P_ema`, on the tier the market would have on it
/// ([`crate::invariants::pool_select::carried_caps`]). Sizes a test's position to fit (or not) a
/// planned candidate.
pub fn carried_caps(env: &Env, pool: &AnyPool) -> (u64, u64) {
    let (mint, _) = pool.token();
    let m = env.market(&pda::market(&mint).0);
    let kind = VenueKind::from_u8(kind_of(pool)).expect("a Raydium kind");
    crate::invariants::pool_select::carried_caps(&env.svm, &m, kind, &pool_key(pool))
        .expect("the candidate reads")
}

/// Tops the WSOL vault of the AMM v4 `pool` (WSOL the pc) up to at least `sol` of real SOL
/// (above its `need_take_pnl`), then by 1,000 lamports at a time until it passes the floor at
/// its own spot ([`passes_floor`]): the locked-depth floor rounds `D`, `isqrt(P)` and the
/// locked SOL down, so a pool at exactly `R = pool_min_real_sol`, `λ = λ_min` sits a few
/// lamports under it. Returns the real SOL it holds then.
pub fn top_up_pc_to_floor(env: &mut Env, pool: &AnyPool, sol: u64) -> u64 {
    let AnyPool::V4(p) = pool else {
        panic!("an AMM v4 pool with WSOL as the pc");
    };
    let pnl = p.info(&env.svm).need_take_pnl_pc;
    let (_, pc) = p.vault_amounts(&env.svm);
    let mut real = sol.max(pc.saturating_sub(pnl));
    loop {
        fork::set_token_amount(&mut env.svm, &p.pc_vault, real + pnl);
        if passes_floor(env, pool, None) {
            return real;
        }
        real += 1_000;
    }
}
