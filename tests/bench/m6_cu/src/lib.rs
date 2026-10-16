//! M6 CU bench, T-TRACE, T-SIZE, the graduation transition and stack depth on the PumpSwap venue
//! (ARCHITECTURE §6.1–§6.4, §8.2 T-CU / T-TRACE / T-SIZE / T-DEPTH, §9 M6).
//!
//! Every [`Case`] builds its pre-state with the LiteSVM harness on the **PumpSwap fork**
//! (`shortwin_litesvm_tests::pamm` / `fork`: the **release** short.win build —
//! `anchor build --tools-version v1.57 --arch v3`, or `SHORTWIN_SO` — the dumped PumpSwap, pump
//! and pump-fees programs deployed from their mainnet ProgramData, PumpSwap's statics, the
//! canonical pools of the `pamm_state` / `pamm_synthetic` groups, mainnet's Rent, the clock at the
//! `pamm_state` snapshot), then
//!
//! 1. copies every account the instruction names into a fresh Mollusk 0.15.1 and loads the same
//!    ELFs into its program cache (short.win, PumpSwap, pump-fees, the token programs, the ATA
//!    program), with LiteSVM's clock and rent;
//! 2. runs the instruction on Mollusk: the figure the budget applies to;
//! 3. runs the same instruction on LiteSVM through `Env::send` behind a `SetComputeUnitLimit` of
//!    1.4 M (the PumpSwap paths exceed the runtime's 200 k default; the invariant hooks run too):
//!    a cross-check of the two harnesses (the instruction's own `consumed` log line must equal
//!    Mollusk's figure), the instruction's trace entries (the instruction plus every CPI at any
//!    depth, ARCH §6.2), its peak stack height (§6.3) and the venue's own CU (its outermost
//!    frames, its pump-fees, token, System and event CPIs included).
//!
//! Every case must execute on both harnesses (a refusal fails the bench). The keeper paths are
//! prepared the way a keeper decides: the state advances second by second until the exit
//! executes, so a case measures the executing path, not a refusal.
//!
//! The worst-case transactions of T-TRACE (with their legacy and v0 + protocol-ALT sizes, the
//! `→ PumpSwap` graduation transition, the transition with a borrower's first close on the pool
//! and the post-wipe reward claim) are
//! [`worst_case_transactions`]; the stack-depth runs through the TEST-ONLY wrapper program
//! (`wrapper/`, [`WRAPPER_ID`]) are [`depth_cases`].
//!
//! **`claim_venue_rewards` was not built** (M5-SEC-1 remainder; ARCH §3.9): PumpSwap's post-wipe
//! reward claim uses the top-level `claim_cashback` + `sweep_trader` UVA-reading pattern, so this
//! bench measures the 6-account `sweep_trader` token leg and the unlocking bundle
//! `[create_idempotent ATA(trader, WSOL), pAMM claim_cashback, sweep_trader]` in its place.
//!
//! **Fixed keys.** The borrowers, the keepers and a changed creator have fixed keys, so the
//! canonical-bump searches of the position and exposure PDAs, and the adapter's re-derivations,
//! cost the same on every run: the reports are reproducible.

use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::types::TransactionMetadata;
use mollusk_svm::Mollusk;
use mollusk_svm::program::keyed_account_for_system_program;
use shortwin::constants::{
    GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_FEE_CONFIG,
    PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, PUMP_AMM_PROGRAM_ID,
    PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG, PUMP_FEES_PROGRAM_ID, PUMP_GLOBAL,
    PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};
use shortwin::events::SolSettle;
use shortwin::state::VenueKind;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpProgram};
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammPool, PammUser};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda};
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_rent::Rent;
use solana_signer::Signer;

// ---- fixtures -------------------------------------------------------------------------------

/// A Token-2022 301-byte pool with third-party LP and its boost spent (e/acc): the main 301-byte
/// open / close / exit pool (no boost wait).
pub const MATURE: PammCoin = PammCoin::Mature;
/// An SPL-Token 261-byte pool never extended.
pub const SPL261: PammCoin = PammCoin::Legacy261;
/// An SPL-Token 211-byte pool (no coin creator; the short-pool growth case, T-RENT).
pub const SPL211: PammCoin = PammCoin::Legacy211;
/// A cashback coin (Token-2022, 301 B): the cashback ATA path.
pub const CASHBACK: PammCoin = PammCoin::Cashback;
/// A fresh Token-2022 301-byte pool 65 s after its migration (its boost vault still active): the
/// listing case.
pub const FRESH: PammCoin = PammCoin::Fresh;

/// The two token-program variants (a Token-2022 301-byte pool and an SPL-Token 261-byte pool).
pub const VARIANTS: [PammCoin; 2] = [MATURE, SPL261];

/// A short label for a pool role.
pub fn label(c: PammCoin) -> &'static str {
    match c {
        PammCoin::Fresh => "Token-2022, 301 B, boost active (fresh)",
        PammCoin::Mature => "Token-2022, 301 B, boost spent (mature)",
        PammCoin::Cashback => "Token-2022, 301 B, cashback (cashback)",
        PammCoin::Legacy211 => "SPL Token, 211 B, no creator (legacy211)",
        PammCoin::Legacy261 => "SPL Token, 261 B (legacy261)",
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

/// ARCH §6.1 `open_short` PumpSwap (PROPOSED transaction limit).
pub const OPEN: Budget = Budget {
    cu: 250_000,
    source: "§6.1 `open_short` PumpSwap",
};
/// ARCH §6.1 `close_short` curve / PumpSwap.
pub const CLOSE: Budget = Budget {
    cu: 250_000,
    source: "§6.1 `close_short` curve / PumpSwap",
};
/// ARCH §6.1 `liquidate` / `expire` PumpSwap.
pub const EXIT: Budget = Budget {
    cu: 300_000,
    source: "§6.1 `liquidate` / `expire` PumpSwap",
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
/// `sweep_trader` has no §6.1 row: the lender-SOL class, as the M5 bench.
pub const SWEEP: Budget = Budget {
    cu: 60_000,
    source: "no §6.1 row (§3.9); §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class",
};
/// ARCH §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5).
pub const LATCH: Budget = Budget {
    cu: 400_000,
    source: "§6.1 `flag_venue_upgraded` / `revalidate_venue` (U5)",
};

/// ARCH §6.2's PumpSwap figure for an instruction.
#[derive(Clone, Copy, Debug)]
pub struct TraceRow {
    /// The table's count for the plain instruction.
    pub arch: usize,
    /// The table's maximum.
    pub arch_max: usize,
}

/// ARCH §6.2 `open_short` PumpSwap: 17 + 1 + 2 = 20 (≤ 23).
pub const OPEN_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 20,
    arch_max: 23,
});
/// ARCH §6.2 `close_short` / `liquidate` / `expire` PumpSwap: 15 + 1 = 16 (≤ 19).
pub const EXIT_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 16,
    arch_max: 19,
});
/// ARCH §6.2 `sol_settle`: 2 (+3 with a `ShareEpoch` record).
pub const SETTLE_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 2,
    arch_max: 5,
});
/// ARCH §6.2 `crank_price`, `flag_venue_upgraded`: 2.
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

/// The program whose own CU a case reports (`None` for the instructions that make no venue CPI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueProgram {
    /// PumpSwap (`pAMMBay6…`).
    Amm,
}

impl VenueProgram {
    fn id(self) -> String {
        match self {
            Self::Amm => PUMP_AMM_PROGRAM_ID.to_string(),
        }
    }
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
    /// The program whose own CU to report (`None` = no venue CPI).
    pub venue: Option<VenueProgram>,
    /// Builds the pre-state and the instruction.
    pub setup: Box<dyn Fn() -> Prepared>,
}

/// What [`measure`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// Mollusk's CU (the budgeted figure).
    pub mollusk: u64,
    /// LiteSVM's CU of the instruction (its `consumed` log line; must equal Mollusk's).
    pub litesvm: u64,
    /// LiteSVM's instruction-trace entries of the instruction.
    pub trace: usize,
    /// The deepest stack height (1 = top level).
    pub peak: u8,
    /// The venue's own CU in this instruction (its outermost frames), if it ran.
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

/// A Mollusk with the release short.win, every other program `accounts` names (PumpSwap,
/// pump-fees, the token programs, the ATA program), and LiteSVM's clock and rent.
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

// ---- the runtime's log ----------------------------------------------------------------------

/// One program invocation read off the runtime's log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The program id (base58).
    pub program: String,
    /// Its stack height (1 = top level).
    pub height: usize,
    /// The CU it consumed, its own CPIs included (`None` for builtins, which log none).
    pub consumed: Option<u64>,
    /// The program that invoked it (`None` at the top level).
    pub caller: Option<String>,
    /// The index of the top-level instruction it belongs to.
    pub top: usize,
}

/// Every invocation of a transaction, in log order (`Program <id> invoke [h]` opens a frame,
/// `consumed` fills it, `success` / `failed:` closes it).
pub fn frames(meta: &TransactionMetadata) -> Vec<Frame> {
    let mut out: Vec<Frame> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut tops = 0usize;
    for line in &meta.logs {
        let Some(rest) = line.strip_prefix("Program ") else {
            continue;
        };
        let mut w = rest.split_whitespace();
        let (Some(id), Some(verb)) = (w.next(), w.next()) else {
            continue;
        };
        match verb {
            "invoke" => {
                let height: usize = w
                    .next()
                    .and_then(|s| s.trim_start_matches('[').trim_end_matches(']').parse().ok())
                    .unwrap_or(0);
                stack.truncate(height.saturating_sub(1));
                if height == 1 {
                    tops += 1;
                }
                let caller = stack.last().map(|&i| out[i].program.clone());
                out.push(Frame {
                    program: id.to_owned(),
                    height,
                    consumed: None,
                    caller,
                    top: tops.saturating_sub(1),
                });
                stack.push(out.len() - 1);
            }
            "consumed" => {
                if let Some(&i) = stack.last()
                    && out[i].program == id
                {
                    out[i].consumed = w.next().and_then(|s| s.parse().ok());
                }
            }
            "success" | "failed:" => {
                stack.pop();
            }
            _ => {}
        }
    }
    out
}

/// The CU top-level instruction `index` consumed (its `consumed` log line).
pub fn top_level_cu(meta: &TransactionMetadata, index: usize) -> Option<u64> {
    frames(meta)
        .into_iter()
        .find(|f| f.height == 1 && f.top == index)
        .and_then(|f| f.consumed)
}

/// A venue's own CU in a transaction: its outermost frames (its own CPIs — pump-fees, token,
/// System, its event self-CPI — are inside them).
pub fn venue_cu(meta: &TransactionMetadata, program: &str) -> Option<u64> {
    let v: Vec<u64> = frames(meta)
        .into_iter()
        .filter(|f| f.program == program && f.caller.as_deref() != Some(program))
        .filter_map(|f| f.consumed)
        .collect();
    (!v.is_empty()).then(|| v.iter().sum())
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

/// The compute-unit limit the LiteSVM cross-check requests (the transaction maximum), so no
/// case is cut by a limit: the budget check is on the measured figure.
pub const CROSS_CHECK_LIMIT: u32 = 1_400_000;

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
    let lite = env.send(&[pump::cu_limit_ix(CROSS_CHECK_LIMIT), ix], &refs);

    match (&result.raw_result, lite) {
        (Ok(()), Ok(meta)) => Ok(Measured {
            mollusk: result.compute_units_consumed,
            litesvm: top_level_cu(&meta, 1).unwrap_or(0),
            trace: trace_entries(&meta, 1),
            peak: peak_height(&meta),
            venue_cu: case.venue.and_then(|v| venue_cu(&meta, &v.id())),
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

/// One SOL in lamports.
pub const SOL: u64 = 1_000_000_000;

/// The borrower of the bench's positions.
pub const BORROWER: u8 = 1;
/// The keeper.
pub const KEEPER: u8 = 0x20;
/// The key a creator change moves the pool to (its vault starts empty).
pub const NEW_CREATOR: [u8; 32] = [0x33; 32];

/// A funded keypair with a **fixed** key (`n` selects it).
pub fn fixed(env: &mut Env, n: u8) -> Keypair {
    let kp = Keypair::new_from_array([n; 32]);
    env.fund(&kp.pubkey(), shortwin_litesvm_tests::env::FUNDING);
    kp
}

/// A funded borrower with a **fixed** key ([`fixed`]), so its `ShortPosition` and
/// `WalletExposure` addresses — and the canonical-bump searches of `open_short` — are the same on
/// every run.
pub fn borrower(env: &mut Env, n: u8) -> Borrower {
    Borrower {
        kp: fixed(env, n),
        next_nonce: 0,
    }
}

/// A position size inside every default cap on `p` now: 1 % of the base reserve, capped at
/// ≈ 8 SOL of notional (`market_max_notional` = 25 SOL), at least ≈ 0.2 SOL. (10 SOL until M6R-1:
/// on the `mature` pool, whose third-party LP puts its locked depth at 92.98 % of `√k`, `X₀` is
/// `0.93·X`, and a wallet's two 10-SOL positions exceed its wallet cap.)
pub fn q_for(env: &Env, p: &PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let y = r.effective_quote.max(1);
    let by_notional = base * u128::from(8 * SOL) / y;
    let q = (base / 100).min(by_notional);
    u64::try_from(q).expect("u64")
}

/// Lists `p`'s mint on PumpSwap (extending a 2024-layout curve first) and seeds it with `lend`
/// tokens (`Env::pamm_market_listed`: the lender, the venue accounts, 60 s of cranks).
pub fn list_on(env: &mut Env, p: &PammPool, lend: u64) -> PammMarket {
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    env.pamm_market_listed(&p.mint, lend, 60)
}

/// Cranks every 30 s until the boost gate no longer blocks opens (`boost_window_s` after
/// `graduated_at`, or the boost vault at dust).
pub fn wait_out_boost(env: &mut Env, pm: &PammMarket) {
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        let boost = pm.pool.boost_amount(&env.svm);
        if boost <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            return;
        }
        env.warp(30);
        env.crank(pm);
    }
}

/// A PumpSwap market on `coin` on a fresh fork: `40·q` lent, its price started and warmed up,
/// the boost window waited out. Returns it and `q`.
pub fn setup(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q.saturating_mul(40));
    wait_out_boost(&mut env, &pm);
    let q = q_for(&env, &pm.pool);
    (env, pm, q)
}

/// `open_short(q)` with no bounds of its own (the venue's CU limit is not prepended: [`measure`]
/// requests the transaction maximum).
pub fn open_ix(pm: &impl VenueMarket, b: &Pubkey, nonce: u64, q: u64) -> Instruction {
    open_short_ix(pm, b, nonce, q, 0, u64::MAX, u64::MAX, ExtrasSel::NONE, &[])
}

/// `close_short` with no bound of its own.
pub fn close_ix(pm: &impl VenueMarket, b: &Pubkey, nonce: u64) -> Instruction {
    close_short_ix(pm, b, nonce, u64::MAX, ExtrasSel::NONE, &[])
}

/// A PumpSwap market with an open position.
pub struct Positioned {
    pub env: Env,
    pub pm: PammMarket,
    pub b: Borrower,
    pub nonce: u64,
    pub q: u64,
}

/// A market on `coin` with one open position of `q` (the fixed borrower), a cranked second old.
pub fn with_position(coin: PammCoin) -> Positioned {
    let (mut env, pm, q) = setup(coin);
    let mut b = borrower(&mut env, BORROWER);
    let (res, nonce) = env.open(&pm, &mut b, q);
    ok(res);
    env.warp(1);
    env.crank(&pm);
    Positioned {
        env,
        pm,
        b,
        nonce,
        q,
    }
}

/// A System transfer of `lamports`.
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

/// Donates `amount` tokens (from a PumpSwap trader's planted balance) into `to`
/// (`TransferChecked`, 6 decimals — every pump / PumpSwap coin).
pub fn donate_tokens(env: &mut Env, pm: &PammMarket, to: &Pubkey, amount: u64) {
    let u = PammUser::new(env, &pm.pool, 0);
    fork::set_token_amount(&mut env.svm, &u.base, amount);
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(6);
    let ix = Instruction {
        program_id: pm.keys.token_program,
        accounts: vec![
            AccountMeta::new(u.base, false),
            AccountMeta::new_readonly(pm.keys.mint, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(u.kp.pubkey(), true),
        ],
        data,
    };
    let kp = u.kp.insecure_clone();
    ok(env.send(&[ix], &[&kp]));
}

/// Donates lamports to the trader PDA and tokens to `trader_base_ata`, so the next leg
/// normalises (ARCH §2.4 rule 5, the token side).
pub fn donate_to_trader(env: &mut Env, pm: &PammMarket) {
    let griefer = env.funded_keypair();
    ok(env.send(&[transfer(&griefer, &pm.trader(), 1_000_000)], &[&griefer]));
    donate_tokens(env, pm, &pm.trader_base_ata, 777);
}

/// Writes a pool's `coin_creator` field (offset 211; `vamm::OFF_COIN_CREATOR`).
fn set_pamm_coin_creator(env: &mut Env, pool: &Pubkey, creator: &Pubkey) {
    fork::patch(&mut env.svm, pool, |d| {
        d[vamm::OFF_COIN_CREATOR..vamm::OFF_COIN_CREATOR + 32].copy_from_slice(creator.as_ref());
    });
}

/// The costliest PumpSwap-leg path the fixtures allow (on top of normalisation): the pool's
/// creator changed after the listing to [`NEW_CREATOR`], whose vault is **empty** — the adapter
/// re-derives both creator-vault slots (T11, T12) from the live creator — and the **last** fee
/// and buyback recipients of the live `GlobalConfig` (the longest key scans). The accounts the
/// existence requirement needs for the new creator and recipients are created.
pub fn costliest_legs(env: &mut Env, pm: &mut PammMarket) {
    let creator = Pubkey::new_from_array(NEW_CREATOR);
    set_pamm_coin_creator(env, &pm.pool.pool, &creator);
    pm.refresh_creator(&env.svm);
    pm.use_recipients(&env.svm, 7, 7);
    let payer = env.payer.pubkey();
    ok(env.send(&pamm::venue_account_ixs(&env.svm, &payer, pm), &[]));
}

/// A liquidatable position: a buyer lifts the price × 2.8 (held), then the keeper cranks second
/// by second until `liquidate` executes on committed state. Returns it (the clock cranked to the
/// second before the executing exit) and the keeper.
pub fn liquidatable(coin: PammCoin) -> (Positioned, Keypair) {
    let mut p = with_position(coin);
    let u = PammUser::new(&mut p.env, &p.pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut p.env, &p.pm.pool, 28, 10));
    let keeper = fixed(&mut p.env, KEEPER);
    for _ in 0..900 {
        p.env.warp(1);
        p.env.crank(&p.pm);
        let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
        if simulate(&p.env, &[ix], &[&keeper]).is_some() {
            return (p, keeper);
        }
    }
    panic!("{coin:?}: never liquidatable");
}

/// An expired healthy position, two cranked seconds past its deadline.
pub fn expired(coin: PammCoin) -> (Positioned, Keypair) {
    let mut p = with_position(coin);
    let deadline = p.env.position(&p.pm, &p.b.key(), p.nonce).deadline;
    let now = p.env.now();
    p.env.warp(deadline + 1 - now);
    p.env.crank(&p.pm);
    p.env.warp(1);
    p.env.crank(&p.pm);
    let keeper = fixed(&mut p.env, KEEPER);
    (p, keeper)
}

/// An expired position on a venue **halted for buys** (`disable_flags` bit 3): the venue cannot
/// execute, so the crank starts the S3 chain; `s3_wait_s` later `sol_settle` settles S3 (no venue
/// CPI, ARCH §5.3). Returns the market with the clock at the settling second.
pub fn s3_buyhalt(coin: PammCoin) -> (Positioned, Keypair) {
    let (mut p, keeper) = expired(coin);
    pamm::set_disable_flags(&mut p.env.svm, 1 << 3);
    let wait = p.env.position(&p.pm, &p.b.key(), p.nonce).s3_wait_s;
    ok(p.env.send(&[crank_price_ix(&p.pm)], &[]));
    for _ in 0..(i64::from(wait) / 30 + 4) {
        p.env.warp(30);
        let ix = sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
        if let Some(meta) = simulate(&p.env, &[crank_price_ix(&p.pm), ix], &[&keeper])
            && events::of::<SolSettle>(&meta)
                .first()
                .is_some_and(|e| e.condition == 3)
        {
            return (p, keeper);
        }
        ok(p.env.send(&[crank_price_ix(&p.pm)], &[]));
    }
    panic!("{coin:?}: S3 never due under the buy halt");
}

/// `simulate_transaction` of `ixs` on the current state (nothing committed): its metadata if it
/// would succeed.
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

// ---- graduation (curve → PumpSwap) ----------------------------------------------------------

/// A curve market on the near-complete fixture curve (`q = vT/200`), `40·q` lent, warmed up.
pub fn near_market() -> (Env, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, q.saturating_mul(40)), c);
    (env, pm, q)
}

/// An external user buys the curve down to `rt_left` real tokens (0 completes it).
fn buy_down_to(env: &mut Env, pm: &PumpMarket, rt_left: u64) {
    let u = CurveUser::new(env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(env, &pm.curve, rt - rt_left, u64::MAX));
}

/// `migrate_v2` of `pm`'s completed curve, by a funded user (top-level; ARCH §5.4 tx2).
fn migrate(env: &mut Env, pm: &PumpMarket) {
    let migrator = env.funded_keypair();
    let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator]));
}

/// A curve market completed and migrated, one crank short of `PumpCurve → PumpSwap` (a curve that
/// completed and migrated in the same window skips `PumpMigrating`): the pre-state for the
/// measured **graduation transition** crank (the ring rewrite). Returns the env, the original
/// `PumpMarket` (whose ReadSet carries the valid canonical pool) and its open positions.
pub fn graduated_pending(with_positions: bool) -> (Env, PumpMarket, Vec<(Borrower, u64)>) {
    let (mut env, pm, q) = near_market();
    let mut positions = Vec::new();
    if with_positions {
        let mut b1 = env.borrower();
        let (res, n1) = env.open(&pm, &mut b1, q);
        ok(res);
        positions.push((b1, n1));
        let mut b2 = env.borrower();
        let (res, n2) = env.open(&pm, &mut b2, q / 2);
        ok(res);
        positions.push((b2, n2));
    }
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    migrate(&mut env, &pm);
    env.warp(1);
    (env, pm, positions)
}

/// The measured transition crank: it moves the market `PumpCurve → PumpSwap`, writes the pool
/// cache and rewrites the ring to the pool's `lp_supply` (§2.5, M5-SEC-4).
fn crank_transition() -> Prepared {
    let (env, pm, _) = graduated_pending(true);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// A pump market seen as list-only (`None`): its ReadSet is the detection set `[bonding_curve,
/// canonical_pool]` (ARCH §3.12 `None` row), no TradeExtras (as `m5_cu_bench::AsNone`).
pub struct AsNone<'a>(pub &'a PumpMarket);

impl VenueMarket for AsNone<'_> {
    fn keys(&self) -> &shortwin_litesvm_tests::fixtures::MarketKeys {
        &self.0.keys
    }
    fn read_set(&self, _writable: bool) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.0.curve.curve, false),
            AccountMeta::new_readonly(pda::canonical_pool(&self.0.keys.mint), false),
        ]
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        Vec::new()
    }
}

/// The near-complete curve **listed while its curve was unreadable** (the curve account removed
/// for the listing and restored after, as `m5_cu`'s re-detection case) and re-detected by a crank
/// (`None → PumpCurve`): that crank writes the curve cache only, so no PumpSwap cache exists
/// yet. Then completed and migrated, one crank short of `→ PumpSwap` — the transition that
/// derives the PumpSwap cache (`pump_amm::graduate` fills what is still empty; M6).
fn crank_transition_redetected() -> Prepared {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    let saved = env.svm.get_account(&c.curve).expect("curve");
    env.svm.set_account(c.curve, Account::default()).unwrap();
    let creator = fixed(&mut env, 0x44);
    let list = ix::create_market(
        &creator.pubkey(),
        &c.mint,
        &c.token_program.id(),
        shortwin::instructions::create_market::VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    ok(env.send(&[list], &[&creator]));
    env.svm.set_account(c.curve, saved).unwrap();
    let vt = c.state(&env.svm).virtual_token_reserves;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near, (vt / 200) * 40), c);
    assert_eq!(venue_of(&env, &pm), VenueKind::None);
    ok(env.send(&[crank_price_ix(&AsNone(&pm))], &[]));
    assert_eq!(venue_of(&env, &pm), VenueKind::PumpCurve);
    let m = env.market(&pm.market());
    assert_eq!(
        m.pool_cache[shortwin::state::pump_swap_cache::PAMM_UVA],
        Pubkey::default(),
        "the re-detection derives no PumpSwap cache"
    );
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    migrate(&mut env, &pm);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// A graduated market past its warm-up (eight integrations), with the open positions still open:
/// the pre-state for exits **on the pool** (the boost gate does not apply to exits, so no boost
/// wait). Returns the env, the PumpSwap market and the positions.
pub fn graduated_after_warmup() -> (Env, PammMarket, Vec<(Borrower, u64)>) {
    let (mut env, pm, positions) = graduated_pending(true);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    assert_eq!(venue_of(&env, &pm), VenueKind::PumpSwap);
    let pam = PammMarket::graduated(&env.svm, &pm);
    let payer = env.payer.pubkey();
    ok(env.send(&pamm::venue_account_ixs(&env.svm, &payer, &pam), &[]));
    for _ in 0..9 {
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pam)], &[]));
    }
    assert_eq!(env.market(&pm.market()).ema_warm, 0);
    (env, pam, positions)
}

/// The venue kind stored on the market of `pm`.
pub fn venue_of(env: &Env, pm: &impl VenueMarket) -> VenueKind {
    env.market(&pm.market()).venue_kind().expect("venue")
}

/// The pump program behind a `VenueGuard` index.
pub fn pump_program(venue: u8) -> PumpProgram {
    match venue {
        GUARD_PUMP => PumpProgram::Pump,
        GUARD_PUMP_FEES => PumpProgram::Fees,
        GUARD_PUMP_AMM => PumpProgram::Amm,
        other => panic!("not a pump venue: {other}"),
    }
}

// ---- post-wipe reward claim (claim_venue_rewards was not built; ARCH §3.9) -------------------

/// A cashback PumpSwap market with a full wipe live (the only lender withdrew every free token, a
/// ×5 pump settled the last position S1) and outstanding cashback in the trader's pAMM UVA WSOL
/// ATA. Returns the env and the market (`WIPE_LIVE`, the trader bit not yet set).
pub fn wiped_cashback() -> (Env, PammMarket) {
    let (mut env, pm, q) = setup(CASHBACK);
    // A first position accrues cashback on its open and close.
    let mut b0 = borrower(&mut env, BORROWER);
    let (res, n0) = env.open(&pm, &mut b0, q);
    ok(res);
    env.warp(1);
    env.crank(&pm);
    ok(env.close(&pm, &b0, n0));
    // The full wipe: a position, every free token withdrawn, a ×5 pump, S1.
    let mut b2 = borrower(&mut env, 2);
    let (res, n2) = env.open(&pm, &mut b2, q);
    ok(res);
    withdraw_all_free(&mut env, &pm);
    let whale = PammUser::new(&mut env, &pm.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pm.pool, 5, 1));
    let keeper = fixed(&mut env, KEEPER);
    for _ in 0..300 {
        env.warp(1);
        env.crank(&pm);
        let ix = sol_settle_ix(&env, &pm, &keeper.pubkey(), &b2.key(), n2);
        let meta = ok(env.send(&[ix], &[&keeper]));
        if !events::of::<SolSettle>(&meta).is_empty() {
            return (env, pm);
        }
    }
    panic!("cashback market never wiped");
}

/// The seeding lender withdraws every free token (as `pamm_it::gates::withdraw_all_free`).
fn withdraw_all_free(env: &mut Env, pm: &PammMarket) {
    let m = env.market(&pm.market());
    let pool = m.pool();
    let want = m.tokens_free;
    let lender = env
        .lender(&pm.market(), &pm.lender.pubkey())
        .expect("lender");
    let (mut lo, mut hi) = (0u128, lender.shares);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if shortwin_math::pool::tokens_for_shares(&pool, mid).unwrap() <= u128::from(want) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let kp = pm.lender.insecure_clone();
    ok(env.send(&[ix::withdraw(&pm.lender_keys(), lo, want)], &[&kp]));
    assert_eq!(env.market(&pm.market()).tokens_free, 0);
}

// ---- setups ---------------------------------------------------------------------------------

/// `create_market(Auto)` of a graduated PumpSwap-class coin with the pAMM init set (the trader's
/// base ATA and pAMM UVA created; the PumpSwap cache derived).
fn create_pamm(coin: PammCoin) -> Prepared {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    let creator = env.funded_keypair();
    let ix = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
    prepared(env, ix, &[&creator])
}

fn open_first(coin: PammCoin) -> Prepared {
    let (mut env, pm, q) = setup(coin);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&pm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn open_repeat(coin: PammCoin) -> Prepared {
    let (mut env, pm, q) = setup(coin);
    let mut b = borrower(&mut env, BORROWER);
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
    env.warp(1);
    env.crank(&pm);
    let ix = open_ix(&pm, &b.key(), 1, q);
    prepared(env, ix, &[&b.kp])
}

fn open_normalising(coin: PammCoin) -> Prepared {
    let (mut env, pm, q) = setup(coin);
    donate_to_trader(&mut env, &pm);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&pm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn open_costliest() -> Prepared {
    let (mut env, mut pm, q) = setup(MATURE);
    costliest_legs(&mut env, &mut pm);
    donate_to_trader(&mut env, &pm);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&pm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn close_case(coin: PammCoin, normalise: bool) -> Prepared {
    let mut p = with_position(coin);
    if normalise {
        donate_to_trader(&mut p.env, &p.pm);
    }
    let ix = close_ix(&p.pm, &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&p.b.kp])
}

fn close_costliest() -> Prepared {
    let mut p = with_position(MATURE);
    costliest_legs(&mut p.env, &mut p.pm);
    donate_to_trader(&mut p.env, &p.pm);
    let ix = close_ix(&p.pm, &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&p.b.kp])
}

fn liquidate_case(coin: PammCoin) -> Prepared {
    let (p, keeper) = liquidatable(coin);
    let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn liquidate_costliest() -> Prepared {
    let mut p = with_position(MATURE);
    // Push the price and find the liquidatable second; then — after that second's crank, which
    // re-caches a changed coin creator (M6-SEC-4) — apply the costliest legs, so the measured
    // liquidation re-derives the creator vault (a change landing between the crank and the exit).
    let u = PammUser::new(&mut p.env, &p.pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut p.env, &p.pm.pool, 28, 10));
    let keeper = fixed(&mut p.env, KEEPER);
    for _ in 0..900 {
        p.env.warp(1);
        p.env.crank(&p.pm);
        let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
        if simulate(&p.env, std::slice::from_ref(&ix), &[&keeper]).is_some() {
            costliest_legs(&mut p.env, &mut p.pm);
            donate_to_trader(&mut p.env, &p.pm);
            let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
            assert!(
                simulate(&p.env, std::slice::from_ref(&ix), &[&keeper]).is_some(),
                "the costliest liquidation simulates"
            );
            return prepared(p.env, ix, &[&keeper]);
        }
    }
    panic!("costliest liquidate never simulates");
}

fn expire_case(coin: PammCoin) -> Prepared {
    let (p, keeper) = expired(coin);
    let ix = expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn settle_noop(coin: PammCoin) -> Prepared {
    let mut p = with_position(coin);
    let keeper = fixed(&mut p.env, KEEPER);
    p.env.warp(1);
    let ix = sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn settle_s3_buyhalt(coin: PammCoin) -> Prepared {
    let (p, keeper) = s3_buyhalt(coin);
    let ix = sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn crank_roll(coin: PammCoin) -> Prepared {
    let (mut env, pm, _) = setup(coin);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// M6-SEC-4: the first crank after a coin-creator change re-caches the creator (two
/// `find_program_address`).
fn crank_creator_change() -> Prepared {
    let (mut env, pm, _) = setup(MATURE);
    set_pamm_coin_creator(
        &mut env,
        &pm.pool.pool,
        &Pubkey::new_from_array(NEW_CREATOR),
    );
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

fn crank_same_second(coin: PammCoin) -> Prepared {
    let (mut env, pm, _) = setup(coin);
    env.warp(1);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

fn sweep(coin: PammCoin, donated: bool) -> Prepared {
    let (mut env, pm, _) = setup(coin);
    if donated {
        donate_tokens(&mut env, &pm, &pm.trader_base_ata, 777);
        let griefer = env.funded_keypair();
        ok(env.send(&[transfer(&griefer, &pm.trader(), 1_000_000)], &[&griefer]));
    }
    let ix = pamm::sweep_trader_pamm_ix(&env, &pm);
    prepared(env, ix, &[])
}

fn flag_pump(venue: u8) -> Prepared {
    let (mut env, _) = Env::fork_pamm_coin(MATURE);
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    fork::set_pump_deploy_slot(&mut env.svm, pump_program(venue), dumped + 1);
    let ix = ix::flag_venue_upgraded(venue);
    prepared(env, ix, &[])
}

fn revalidate_pump(venue: u8) -> Prepared {
    let (mut env, _) = Env::fork_pamm_coin(MATURE);
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let new = dumped + 1;
    fork::set_pump_deploy_slot(&mut env.svm, pump_program(venue), new);
    ok(env.send(&[ix::flag_venue_upgraded(venue)], &[]));
    let ea = env.pause_authority.insecure_clone();
    let ix = ix::revalidate_venue(&ea.pubkey(), venue, new);
    prepared(env, ix, &[&ea])
}

fn graduated_exit_liquidate() -> Prepared {
    let (mut env, pam, positions) = graduated_after_warmup();
    let (b, nonce) = positions.into_iter().next().expect("a position");
    let keeper = fixed(&mut env, KEEPER);
    let u = PammUser::new(&mut env, &pam.pool, 5_000 * SOL);
    ok(u.push_price(&mut env, &pam.pool, 3, 1));
    for _ in 0..900 {
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pam)], &[]));
        let ix = liquidate_ix(&pam, &keeper.pubkey(), &b.key(), nonce);
        if simulate(&env, std::slice::from_ref(&ix), &[&keeper]).is_some() {
            return prepared(env, ix, &[&keeper]);
        }
    }
    panic!("graduated liquidate never simulates");
}

// ---- the report's cases ---------------------------------------------------------------------

/// Every PumpSwap-path instruction and the graduation transition (ARCH §6.1, §8.2 T-CU).
pub fn cases() -> Vec<Case> {
    let mut v: Vec<Case> = Vec::new();
    let mut push = |handler, case: String, budget, trace, venue, setup| {
        v.push(Case {
            handler,
            case,
            budget,
            trace,
            venue,
            setup,
        });
    };

    for c in [MATURE, SPL261, FRESH, CASHBACK] {
        push(
            "create_market",
            format!("Auto, {}: PumpSwap, the init set", label(c)),
            CREATE,
            None,
            None,
            Box::new(move || create_pamm(c)),
        );
    }

    for c in [MATURE, SPL261, SPL211, CASHBACK] {
        push(
            "open_short",
            format!("{}: first open of a wallet", label(c)),
            OPEN,
            OPEN_ROW,
            Some(VenueProgram::Amm),
            Box::new(move || open_first(c)),
        );
    }
    push(
        "open_short",
        format!("{}: second position of a wallet", label(MATURE)),
        OPEN,
        OPEN_ROW,
        Some(VenueProgram::Amm),
        Box::new(|| open_repeat(MATURE)),
    );
    push(
        "open_short",
        format!("{}: first open, normalising", label(MATURE)),
        OPEN,
        OPEN_ROW,
        Some(VenueProgram::Amm),
        Box::new(|| open_normalising(MATURE)),
    );
    push(
        "open_short",
        format!(
            "{}: costliest — normalising, creator change into an empty vault, last recipients",
            label(MATURE)
        ),
        OPEN,
        OPEN_ROW,
        Some(VenueProgram::Amm),
        Box::new(open_costliest),
    );

    for c in [MATURE, SPL261, SPL211, CASHBACK] {
        push(
            "close_short",
            label(c).to_owned(),
            CLOSE,
            EXIT_ROW,
            Some(VenueProgram::Amm),
            Box::new(move || close_case(c, false)),
        );
    }
    push(
        "close_short",
        format!("{}: normalising", label(MATURE)),
        CLOSE,
        EXIT_ROW,
        Some(VenueProgram::Amm),
        Box::new(|| close_case(MATURE, true)),
    );
    push(
        "close_short",
        format!(
            "{}: costliest — normalising, creator change into an empty vault, last recipients",
            label(MATURE)
        ),
        CLOSE,
        EXIT_ROW,
        Some(VenueProgram::Amm),
        Box::new(close_costliest),
    );

    for c in VARIANTS {
        push(
            "liquidate",
            format!("{}: after a real pump (× 2.8), both legs", label(c)),
            EXIT,
            EXIT_ROW,
            Some(VenueProgram::Amm),
            Box::new(move || liquidate_case(c)),
        );
        push(
            "expire",
            format!("{}: past the deadline, healthy", label(c)),
            EXIT,
            EXIT_ROW,
            Some(VenueProgram::Amm),
            Box::new(move || expire_case(c)),
        );
    }
    push(
        "liquidate",
        format!(
            "{}: costliest — normalising, creator change, last recipients",
            label(MATURE)
        ),
        EXIT,
        EXIT_ROW,
        Some(VenueProgram::Amm),
        Box::new(liquidate_costliest),
    );
    push(
        "liquidate",
        "graduation path: on the migrated pool after the warm-up".to_owned(),
        EXIT,
        EXIT_ROW,
        Some(VenueProgram::Amm),
        Box::new(graduated_exit_liquidate),
    );

    push(
        "sol_settle",
        format!("{}: healthy, the no-change call", label(MATURE)),
        SETTLE,
        SETTLE_ROW,
        None,
        Box::new(|| settle_noop(MATURE)),
    );
    push(
        "sol_settle",
        format!(
            "{}: S3 under the buy halt (disable_flags bit 3), after s3_wait_s",
            label(MATURE)
        ),
        SETTLE,
        SETTLE_ROW,
        None,
        Box::new(|| settle_s3_buyhalt(MATURE)),
    );

    for c in VARIANTS {
        push(
            "crank_price",
            format!("{}: roll + record (first crank of a second)", label(c)),
            CRANK,
            TWO_ROW,
            None,
            Box::new(move || crank_roll(c)),
        );
    }
    push(
        "crank_price",
        format!("{}: record only (same second again)", label(MATURE)),
        CRANK,
        TWO_ROW,
        None,
        Box::new(|| crank_same_second(MATURE)),
    );
    push(
        "crank_price",
        format!(
            "{}: roll + record after a coin-creator change: the creator re-cached (M6-SEC-4)",
            label(MATURE)
        ),
        CRANK,
        TWO_ROW,
        None,
        Box::new(crank_creator_change),
    );
    push(
        "crank_price",
        "graduation transition: `PumpCurve → PumpSwap` (the pool cache, the ring rewrite, the warm-up)"
            .to_owned(),
        CRANK,
        TWO_ROW,
        None,
        Box::new(crank_transition),
    );
    push(
        "crank_price",
        "graduation transition of a market re-detected onto the curve (`None → PumpCurve`): the \
         PumpSwap cache derived at the transition"
            .to_owned(),
        CRANK,
        TWO_ROW,
        None,
        Box::new(crank_transition_redetected),
    );

    for c in VARIANTS {
        push(
            "sweep_trader",
            format!("{}: token leg — a donation to the pool vault", label(c)),
            SWEEP,
            None,
            None,
            Box::new(move || sweep(c, true)),
        );
    }
    push(
        "sweep_trader",
        format!("{}: nothing to sweep", label(MATURE)),
        SWEEP,
        None,
        None,
        Box::new(|| sweep(MATURE, false)),
    );

    push(
        "flag_venue_upgraded",
        "PumpSwap, the dumped 10,485,760-byte ProgramData".to_owned(),
        LATCH,
        TWO_ROW,
        None,
        Box::new(|| flag_pump(GUARD_PUMP_AMM)),
    );
    push(
        "revalidate_venue",
        "PumpSwap, the dumped ProgramData".to_owned(),
        LATCH,
        TWO_ROW,
        None,
        Box::new(|| revalidate_pump(GUARD_PUMP_AMM)),
    );

    v
}

// ---- the protocol ALT (A28; ARCH §6.4) ------------------------------------------------------

/// ARCH §6.4's A28 list as it applies to a PumpSwap market: the harness's Raydium part
/// (`raydium::protocol_alt_addresses`), then the pump part (pump, PumpSwap and pump-fees, their
/// statics, and the live `Global` / `GlobalConfig` fee and buyback recipients with their WSOL
/// ATAs). Read from the fork's live accounts.
pub fn protocol_alt_addresses(svm: &litesvm::LiteSVM) -> Vec<Pubkey> {
    let mut v = shortwin_litesvm_tests::raydium::protocol_alt_addresses();
    let push = |v: &mut Vec<Pubkey>, k: Pubkey| {
        if k != Pubkey::default() && !v.contains(&k) {
            v.push(k);
        }
    };
    for k in [
        PUMP_PROGRAM_ID,
        PUMP_AMM_PROGRAM_ID,
        PUMP_FEES_PROGRAM_ID,
        PUMP_GLOBAL,
        PUMP_EVENT_AUTHORITY,
        PUMP_FEE_CONFIG,
        PUMP_GLOBAL_VOLUME_ACCUMULATOR,
        PUMP_AMM_GLOBAL_CONFIG,
        PUMP_AMM_EVENT_AUTHORITY,
        PUMP_AMM_FEE_CONFIG,
        PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
    ] {
        push(&mut v, k);
    }
    let g = svm.get_account(&PUMP_GLOBAL).expect("pump Global");
    let global = vpump::read_global(&g.owner.to_bytes(), &g.data).expect("Global");
    let gc = svm
        .get_account(&PUMP_AMM_GLOBAL_CONFIG)
        .expect("PumpSwap GlobalConfig");
    let config = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data).expect("GlobalConfig");
    let lists = [
        global.fee_recipients(),
        global.buyback_fee_recipients(),
        config.protocol_fee_recipients(),
        config.buyback_fee_recipients(),
    ];
    for list in lists {
        for k in list {
            let k = Pubkey::new_from_array(k);
            if k == Pubkey::default() {
                continue;
            }
            push(&mut v, k);
            push(&mut v, pump::wsol_ata(&k));
        }
    }
    assert!(v.len() <= 256, "{} addresses exceed one table", v.len());
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
    /// `tests/litesvm/tests/trace_pumpswap_it.rs`).
    pub expected: usize,
    /// The peak stack height it must reach.
    pub peak: u8,
    /// A keeper batch sent as a **v1** transaction; otherwise the transaction must fit a v0 one
    /// with the protocol ALT.
    pub v1: bool,
    /// The CU limit the transaction must fit (ARCH §6.1's limit for it).
    pub cu_limit: u64,
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

/// The wire sizes of `ixs` paid by `payer`: legacy, and v0 through the protocol ALT.
pub fn sizes(env: &Env, ixs: &[Instruction]) -> ((usize, usize), (usize, usize)) {
    use shortwin_litesvm_tests::raydium::{legacy_size, v0_size};
    let payer = env.payer.pubkey();
    let alt = Pubkey::new_unique();
    (
        legacy_size(&payer, ixs),
        v0_size(&payer, ixs, &alt, &protocol_alt_addresses(&env.svm)),
    )
}

/// Runs one worst-case transaction on LiteSVM (every hook after every instruction).
pub fn run_trace(t: &TraceTx) -> Result<TraceMeasured, String> {
    let (mut env, ixs, signers) = (t.setup)();
    let (legacy, v0) = sizes(&env, &ixs);
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

fn single_with_cb(p: Prepared) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let Prepared { env, ix, signers } = p;
    (env, with_cb(vec![ix]), signers)
}

fn tx_keeper_expiry(coin: PammCoin) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut p, keeper) = expired(coin);
    p.env.warp(1);
    let ixs = with_cb(vec![
        crank_price_ix(&p.pm),
        expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ]);
    (p.env, ixs, vec![keeper])
}

fn tx_keeper_liq(coin: PammCoin, costliest: bool) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let mut p = with_position(coin);
    let u = PammUser::new(&mut p.env, &p.pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut p.env, &p.pm.pool, 28, 10));
    if costliest {
        costliest_legs(&mut p.env, &mut p.pm);
        donate_to_trader(&mut p.env, &p.pm);
    }
    let keeper = fixed(&mut p.env, KEEPER);
    for _ in 0..900 {
        p.env.warp(1);
        let liq = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
        let ixs = with_cb(vec![crank_price_ix(&p.pm), liq]);
        if simulate(&p.env, &ixs[2..], &[&keeper]).is_some() {
            return (p.env, ixs, vec![keeper]);
        }
        ok(p.env.send(&[crank_price_ix(&p.pm)], &[]));
    }
    panic!("{coin:?}: keeper liquidation never ready");
}

fn tx_keeper_s3_buyhalt() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (p, keeper) = s3_buyhalt(MATURE);
    let ixs = with_cb(vec![
        crank_price_ix(&p.pm),
        sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ]);
    (p.env, ixs, vec![keeper])
}

fn tx_transition_only() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (env, pm, _) = graduated_pending(true);
    (env, with_cb(vec![crank_price_ix(&pm)]), vec![])
}

/// The transition crank and a borrower's close on the pool in one transaction: the first close
/// after a migration (it was the straddle bundle's tx3 until pump-final D2 retired the bundle).
fn tx_transition_and_close() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, pm, positions) = graduated_pending(true);
    let (b, nonce) = positions.into_iter().next().expect("a position");
    let pam = PammMarket::graduated(&env.svm, &pm);
    let payer = env.payer.pubkey();
    ok(env.send(&pamm::venue_account_ixs(&env.svm, &payer, &pam), &[]));
    let ixs = with_cb(vec![crank_price_ix(&pm), close_ix(&pam, &b.key(), nonce)]);
    let kp = b.kp.insecure_clone();
    (env, ixs, vec![kp])
}

fn tx_open(coin: PammCoin) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    single_with_cb(open_normalising(coin))
}

fn tx_open_costliest() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    single_with_cb(open_costliest())
}

fn tx_close(coin: PammCoin) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    single_with_cb(close_case(coin, true))
}

fn tx_close_costliest() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    single_with_cb(close_costliest())
}

fn tx_listing(coin: PammCoin) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    single_with_cb(create_pamm(coin))
}

fn tx_sweep() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    single_with_cb(sweep(MATURE, true))
}

/// The post-wipe reward claim bundle (claim_venue_rewards was not built): `[create_idempotent
/// ATA(trader, WSOL), pAMM claim_cashback(user = trader, unsigned), sweep_trader]` on a wiped
/// cashback market with the venue halted for buys.
fn tx_claim_rewards() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, pm) = wiped_cashback();
    pamm::set_disable_flags(&mut env.svm, 1 << 3);
    let trader = pm.trader();
    let dest = pump::wsol_ata(&trader);
    let payer = env.payer.pubkey();
    let ixs = with_cb(vec![
        fork::create_ata_idempotent_ix(&payer, &trader, &WSOL_MINT, &TOKEN_PROGRAM_ID),
        pamm::pamm_claim_cashback_ix(&trader, &dest),
        pamm::sweep_trader_pamm_ix(&env, &pm),
    ]);
    (env, ixs, vec![])
}

fn tx_flag_two() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, _) = Env::fork_pamm_coin(MATURE);
    for venue in [GUARD_PUMP_AMM, GUARD_PUMP_FEES] {
        let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_pump_deploy_slot(&mut env.svm, pump_program(venue), dumped + 1);
    }
    let ixs = with_cb(vec![
        ix::flag_venue_upgraded(GUARD_PUMP_AMM),
        ix::flag_venue_upgraded(GUARD_PUMP_FEES),
    ]);
    (env, ixs, vec![])
}

/// ARCH §6.1's limits.
pub const PAMM_TRADE: u64 = 250_000;
pub const PAMM_KEEPER: u64 = 300_000;

/// The worst-case PumpSwap transactions (T-TRACE): each must stay ≤ 64 entries, reach its exact
/// expected count, fit its ARCH §6.1 CU limit, and fit a v0 transaction through the protocol ALT.
pub fn worst_case_transactions() -> Vec<TraceTx> {
    let mut v: Vec<TraceTx> = Vec::new();
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!("{}: ComputeBudget ×2 + open_short, normalising", label(c)),
            arch: "2 + 26 (above §6.2's ≤ 23 on the normalising path)",
            expected: 2 + 26,
            peak: 3,
            v1: false,
            cu_limit: PAMM_TRADE,
            setup: Box::new(move || tx_open(c)),
        });
        v.push(TraceTx {
            name: format!("{}: ComputeBudget ×2 + close_short, normalising", label(c)),
            arch: "2 + 20 (above §6.2's ≤ 19 on the normalising path)",
            expected: 2 + 20,
            peak: 3,
            v1: false,
            cu_limit: PAMM_TRADE,
            setup: Box::new(move || tx_close(c)),
        });
    }
    v.push(TraceTx {
        name: format!("{}: ComputeBudget ×2 + the costliest open", label(MATURE)),
        arch: "2 + 26 (above §6.2's ≤ 23; creator change and normalisation)",
        expected: 2 + 26,
        peak: 3,
        v1: false,
        cu_limit: PAMM_TRADE,
        setup: Box::new(tx_open_costliest),
    });
    v.push(TraceTx {
        name: format!("{}: ComputeBudget ×2 + the costliest close", label(MATURE)),
        arch: "2 + 20 (above §6.2's ≤ 19; creator change and normalisation)",
        expected: 2 + 20,
        peak: 3,
        v1: false,
        cu_limit: PAMM_TRADE,
        setup: Box::new(tx_close_costliest),
    });
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + liquidate (keeper)",
                label(c)
            ),
            arch: "2 + 2 + 17 (≤ 19 on PumpSwap; §6.2)",
            expected: 2 + 2 + 17,
            peak: 3,
            v1: false,
            cu_limit: PAMM_KEEPER,
            setup: Box::new(move || tx_keeper_liq(c, false)),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + expire (keeper)",
                label(c)
            ),
            arch: "2 + 2 + 17 (≤ 19 on PumpSwap; §6.2)",
            expected: 2 + 2 + 17,
            peak: 3,
            v1: false,
            cu_limit: PAMM_KEEPER,
            setup: Box::new(move || tx_keeper_expiry(c)),
        });
    }
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + crank_price + the costliest liquidate (keeper)",
            label(MATURE)
        ),
        arch: "2 + 2 + 20 (above §6.2's ≤ 19; creator change and normalisation)",
        expected: 2 + 2 + 20,
        peak: 3,
        v1: false,
        cu_limit: PAMM_KEEPER,
        setup: Box::new(|| tx_keeper_liq(MATURE, true)),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price + sol_settle S3 (keeper; the buy halt)".to_owned(),
        arch: "2 + 2 + 2 (§6.2)",
        expected: 2 + 2 + 2,
        peak: 2,
        v1: false,
        cu_limit: 150_000,
        setup: Box::new(tx_keeper_s3_buyhalt),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price (graduation transition → PumpSwap)".to_owned(),
        arch: "no row (§2.5; the ring rewrite, VenueStateChanged + RouteChainChanged)",
        expected: 2 + 4,
        peak: 2,
        v1: false,
        cu_limit: 250_000,
        setup: Box::new(tx_transition_only),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price (→ PumpSwap) + close_short (borrower, first close on the pool)"
            .to_owned(),
        arch: "no row (the transition crank then the borrower's close on the pool)",
        expected: 2 + 4 + 17,
        peak: 3,
        v1: false,
        cu_limit: 400_000,
        setup: Box::new(tx_transition_and_close),
    });
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + create_market (Auto, PumpSwap, init set)",
                label(c)
            ),
            arch: "no row",
            expected: 2 + 15,
            peak: 3,
            v1: false,
            cu_limit: 250_000,
            setup: Box::new(move || tx_listing(c)),
        });
    }
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + sweep_trader (token leg)",
            label(MATURE)
        ),
        arch: "no row (§3.9; the post-wipe token leg)",
        expected: 2 + 4,
        peak: 2,
        v1: false,
        cu_limit: 60_000,
        setup: Box::new(tx_sweep),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + create ATA(trader, WSOL) + pAMM claim_cashback + sweep_trader (post-wipe reward claim)"
            .to_owned(),
        arch: "no row (§3.9; claim_venue_rewards was not built — the UVA-reading pattern)",
        expected: 2 + 1 + 7 + 4,
        peak: 2,
        v1: false,
        cu_limit: 150_000,
        setup: Box::new(tx_claim_rewards),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + flag_venue_upgraded ×2 (PumpSwap, pump-fees; the dumped ProgramData)"
            .to_owned(),
        arch: "2 + 2 + 2 (§6.2)",
        expected: 2 + 2 * 2,
        peak: 2,
        v1: false,
        cu_limit: 400_000,
        setup: Box::new(tx_flag_two),
    });
    v
}

// ---- stack depth ----------------------------------------------------------------------------

/// The TEST-ONLY wrapper program's id (`wrapper/`; never deployed): the 32 bytes of
/// `short.win-m6-cpi-wrapper-program`.
pub const WRAPPER_ID: Pubkey = Pubkey::new_from_array(*b"short.win-m6-cpi-wrapper-program");

/// The wrapper's SBF build: `$M6_WRAPPER_SO`, else `tests/bench/m6_cu/target/deploy/
/// m6_cpi_wrapper.so`.
pub fn wrapper_so_path() -> PathBuf {
    std::env::var_os("M6_WRAPPER_SO").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/m6_cpi_wrapper.so"),
        PathBuf::from,
    )
}

/// Deploys the wrapper into `env`.
pub fn load_wrapper(env: &mut Env) {
    let path = wrapper_so_path();
    let so = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}. Build it: cargo-build-sbf --tools-version v1.57 --arch v3 \
             --manifest-path tests/bench/m6_cu/wrapper/Cargo.toml --sbf-out-dir \
             tests/bench/m6_cu/target/deploy (or set M6_WRAPPER_SO)",
            path.display()
        )
    });
    env.svm
        .add_program(WRAPPER_ID, &so)
        .unwrap_or_else(|e| panic!("add the wrapper: {e:?}"));
}

/// `ix` called through the wrapper `levels` times.
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

/// One stack-depth row.
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

/// The PumpSwap-path instructions through 0–3 wrapper levels: every trading leg peaks at 3
/// (short.win 1 → pAMM 2 → pump-fees / Token / System / pAMM's event 3), as does a PumpSwap
/// listing (short.win 1 → ATA program / pAMM 2 → Token / System / pAMM's event 3), so two
/// wrapper levels fit (peak 5 on mainnet) and three do not; the instructions without a venue CPI
/// peak at 2 and fit three.
pub fn depth_cases() -> Vec<DepthCase> {
    let mut v = Vec::new();
    for c in VARIANTS {
        v.push(DepthCase {
            handler: "open_short",
            case: format!("{}: first open, normalising", label(c)),
            direct_peak: 3,
            setup: Box::new(move || open_normalising(c)),
        });
        v.push(DepthCase {
            handler: "close_short",
            case: label(c).to_owned(),
            direct_peak: 3,
            setup: Box::new(move || close_case(c, false)),
        });
        v.push(DepthCase {
            handler: "liquidate",
            case: label(c).to_owned(),
            direct_peak: 3,
            setup: Box::new(move || liquidate_case(c)),
        });
        v.push(DepthCase {
            handler: "expire",
            case: label(c).to_owned(),
            direct_peak: 3,
            setup: Box::new(move || expire_case(c)),
        });
        v.push(DepthCase {
            handler: "create_market",
            case: format!("Auto, {}: PumpSwap, init set", label(c)),
            direct_peak: 3,
            setup: Box::new(move || create_pamm(c)),
        });
        v.push(DepthCase {
            handler: "sweep_trader",
            case: format!("{}: token leg", label(c)),
            direct_peak: 2,
            setup: Box::new(move || sweep(c, true)),
        });
    }
    v.push(DepthCase {
        handler: "open_short",
        case: format!("{}: the costliest open (creator change)", label(MATURE)),
        direct_peak: 3,
        setup: Box::new(open_costliest),
    });
    v.push(DepthCase {
        handler: "sol_settle",
        case: "S3 under the buy halt".to_owned(),
        direct_peak: 2,
        setup: Box::new(|| settle_s3_buyhalt(MATURE)),
    });
    v.push(DepthCase {
        handler: "crank_price",
        case: "graduation transition: `PumpCurve → PumpSwap`".to_owned(),
        direct_peak: 2,
        setup: Box::new(crank_transition),
    });
    v.push(DepthCase {
        handler: "flag_venue_upgraded",
        case: "PumpSwap, the dumped ProgramData".to_owned(),
        direct_peak: 2,
        setup: Box::new(|| flag_pump(GUARD_PUMP_AMM)),
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
    let ixs = [pump::cu_limit_ix(CROSS_CHECK_LIMIT), wrap(&ix, levels)];
    match env.send(&ixs, &refs) {
        Ok(meta) => Ok(peak_height(&meta)),
        Err(f) => Err(format!("{:?}", f.err)),
    }
}
