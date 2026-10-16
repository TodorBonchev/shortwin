//! M5 CU bench, T-TRACE, T-SIZE preview and stack depth on the pump bonding curve
//! (ARCHITECTURE §6.1–§6.4, §8.2 T-CU / T-TRACE / T-SIZE / T-DEPTH, §9 M5). Since pump-final P6
//! the straddle bundle's helper rows (T-STRADDLE's tx1: the `complete_curve` helper) are gone
//! with the helper (owner D2); the curve's exits past `rT` are the synthetic rows, and the
//! keeper's `migrate_v2` is measured after a third party's exact completion (`pc = 0`, the
//! bundle's former tx2) and after a synthetic one (`pc ≠ 0`).
//!
//! Every [`Case`] builds its pre-state with the LiteSVM harness on the **pump fork**
//! (`shortwin_litesvm_tests::fork` / `pump`: the **release** short.win build —
//! `anchor build --tools-version v1.57 --arch v3`, or `SHORTWIN_SO` — the dumped pump, pump-fees
//! and PumpSwap programs deployed from their mainnet ProgramData, pump's statics, the coins of
//! the `pump_state` group, mainnet's Rent, the clock at the pump snapshot), then
//!
//! 1. copies every account the instruction names into a fresh Mollusk 0.15.1 and loads the same
//!    ELFs into its program cache (short.win, pump, pump-fees, PumpSwap, the token programs, the
//!    ATA program), with LiteSVM's clock and rent;
//! 2. runs the instruction on Mollusk: the figure the budget applies to;
//! 3. runs the same instruction on LiteSVM through `Env::send` behind a `SetComputeUnitLimit` of
//!    1.4 M (the curve paths exceed the runtime's 200 k default; the invariant hooks run too): a
//!    cross-check of the two harnesses (the instruction's own `consumed` log line must equal
//!    Mollusk's figure), the instruction's trace entries (the instruction plus every CPI at any
//!    depth, ARCH §6.2), its peak stack height (§6.3) and pump's own CU (its outermost frames,
//!    its pump-fees, token, System and event CPIs included).
//!
//! Every case must execute on both harnesses (a refusal fails the bench). The keeper paths are
//! prepared the way a keeper decides: the state advances second by second until a
//! `simulate_transaction` of the keeper's `[crank_price, exit]` succeeds (nothing is committed by
//! the simulation), so a case measures the executing path, not a refusal.
//!
//! The worst-case transactions of T-TRACE (with their legacy and v0 + protocol-ALT sizes) are
//! [`worst_case_transactions`]; the stack-depth runs through the TEST-ONLY wrapper program
//! (`wrapper/`, [`WRAPPER_ID`]) are [`depth_cases`].
//!
//! **Fixed keys.** The borrowers, the keepers, the sandwich attacker and a changed creator have
//! fixed keys, so the canonical-bump searches of the position and exposure PDAs, and the
//! re-derivations the curve adapter makes, cost the same on every run: the reports are
//! reproducible.

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
    PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID,
};
use shortwin::events::{SolSettle, VenueStateChanged};
use shortwin::state::VenueKind;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve, PumpProgram, PumpV2};
use shortwin_litesvm_tests::pamm;
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda};
use shortwin_math::consts::RING_LEN;
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_rent::Rent;
use solana_signer::Signer;

// ---- fixtures -------------------------------------------------------------------------------

/// A live legacy SPL-Token curve of 115 bytes without a creator (`legacy115`).
pub const SPL: PumpCoin = PumpCoin::Legacy115;
/// A live Token-2022 curve of 115 bytes with a creator (`t22_115`).
pub const T22: PumpCoin = PumpCoin::T22_115;
/// A live Token-2022 curve of 125 bytes minutes old (`fresh`; `vT/7` of buys first).
pub const FRESH: PumpCoin = PumpCoin::Fresh;
/// A live Token-2022 curve of 151 bytes near completion (`near`): the completing buyback, the
/// migration and the synthetic exits past `rT`.
pub const NEAR: PumpCoin = PumpCoin::Near;

/// The trace entries of pump's growth of [`NEAR`] on its first trade since the dump: its rent
/// reserve is `minimum_balance(151)`, and the 2026-10-08 build grows a curve to 166 bytes with
/// one System transfer from the trader at height 3 (`PumpCurve::growth_top_up`; the 115-byte
/// curves' reserves already cover it).
pub const NEAR_GROWTH: usize = 1;

/// Both token programs: SPL Token and Token-2022.
pub const VARIANTS: [PumpCoin; 2] = [SPL, T22];
/// Every live SOL curve layout a market can list (115, 125 and 151 bytes; SPL and Token-2022).
pub const CURVES: [PumpCoin; 4] = [SPL, T22, FRESH, NEAR];

/// A coin's name in the reports.
pub fn label(c: PumpCoin) -> &'static str {
    match c {
        PumpCoin::Legacy115 => "SPL Token, 115 B curve, no creator (legacy115)",
        PumpCoin::T22_115 => "Token-2022, 115 B curve (t22_115)",
        PumpCoin::Fresh => "Token-2022, 125 B curve (fresh)",
        PumpCoin::Near => "Token-2022, 151 B curve near completion (near)",
        PumpCoin::Graduated => "PumpSwap-class coin (graduated)",
        PumpCoin::Fartcoin => "graduated elsewhere (Fartcoin)",
        other => panic!("no label for {other:?}"),
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

/// ARCH §6.1 `open_short` curve (PROPOSED transaction limit).
pub const OPEN: Budget = Budget {
    cu: 250_000,
    source: "§6.1 `open_short` curve",
};
/// ARCH §6.1 `close_short` curve / PumpSwap.
pub const CLOSE: Budget = Budget {
    cu: 250_000,
    source: "§6.1 `close_short` curve / PumpSwap",
};
/// ARCH §6.1 `liquidate` / `expire` curve.
pub const EXIT: Budget = Budget {
    cu: 300_000,
    source: "§6.1 `liquidate` / `expire` curve",
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
/// `sweep_trader` has no §6.1 row: the lender-SOL class, as the M2 bench.
pub const SWEEP: Budget = Budget {
    cu: 60_000,
    source: "no §6.1 row (§3.9); §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class",
};
/// ARCH §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5).
pub const LATCH: Budget = Budget {
    cu: 400_000,
    source: "§6.1 `flag_venue_upgraded` / `revalidate_venue` (U5)",
};

/// ARCH §6.2's curve figure for an instruction.
#[derive(Clone, Copy, Debug)]
pub struct TraceRow {
    /// The table's count for the plain instruction.
    pub arch: usize,
    /// The table's maximum (with normalisation).
    pub arch_max: usize,
}

/// ARCH §6.2 `open_short` curve: 9 + 1 + 2 = 12 (≤ 15).
pub const OPEN_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 12,
    arch_max: 15,
});
/// ARCH §6.2 `close_short` / `liquidate` / `expire` curve: 11 + 1 = 12 (≤ 15).
pub const EXIT_ROW: Option<TraceRow> = Some(TraceRow {
    arch: 12,
    arch_max: 15,
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
    /// LiteSVM's CU of the instruction (its `consumed` log line; must equal Mollusk's).
    pub litesvm: u64,
    /// LiteSVM's instruction-trace entries of the instruction.
    pub trace: usize,
    /// The deepest stack height (1 = top level).
    pub peak: u8,
    /// Pump's own CU in this instruction (its outermost frames), if it ran.
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

/// A Mollusk with the release short.win, every other program `accounts` names (pump,
/// pump-fees, PumpSwap, the token programs, the ATA program), and LiteSVM's clock and rent.
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

/// Pump's own CU in a transaction: its outermost frames (a pump frame's own CPIs — pump-fees,
/// token, System, its event self-CPI — are inside it).
pub fn venue_cu(meta: &TransactionMetadata) -> Option<u64> {
    let pump = PUMP_PROGRAM_ID.to_string();
    let v: Vec<u64> = frames(meta)
        .into_iter()
        .filter(|f| f.program == pump && f.caller.as_deref() != Some(pump.as_str()))
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

/// A funded keypair with a **fixed** key (`n` selects it).
pub fn fixed(env: &mut Env, n: u8) -> Keypair {
    let kp = Keypair::new_from_array([n; 32]);
    env.fund(&kp.pubkey(), shortwin_litesvm_tests::env::FUNDING);
    kp
}

/// The borrower of the bench's positions.
pub const BORROWER: u8 = 1;
/// The keeper.
pub const KEEPER: u8 = 0x20;
/// The sandwich attacker.
pub const ATTACKER: u8 = 0x40;
/// A market's creator.
pub const CREATOR: u8 = 0x50;
/// The key a creator change moves the curve to (its vault starts empty).
pub const NEW_CREATOR: [u8; 32] = [0x33; 32];

/// A funded borrower with a **fixed** key ([`fixed`]): its `ShortPosition` and `WalletExposure`
/// addresses, and with them the canonical-bump searches of `open_short`, are the same on every
/// run.
pub fn borrower(env: &mut Env, n: u8) -> Borrower {
    Borrower {
        kp: fixed(env, n),
        next_nonce: 0,
    }
}

/// `vT/200`: 0.5 % of the virtual token reserve, within every default cap (the `pump_it` size).
pub fn q_of(env: &Env, c: &PumpCurve) -> u64 {
    c.state(&env.svm).virtual_token_reserves / 200
}

/// Lists `c` (a live curve), lends `40·Q`, waits out the warm-up and cranks once; returns the
/// market and `Q`. A fresh curve (its `rQ` one lamport) first gets `vT/7` of buys by a normal
/// user, so it can pay for a sale (as `pump_it`).
pub fn add_market(env: &mut Env, c: PumpCurve) -> (PumpMarket, u64) {
    if c.coin == FRESH {
        let u = CurveUser::new(env, &c);
        let q = u64::try_from(c.curve_state(&env.svm).vt / 7).expect("q");
        ok(u.buy(env, &c, q, u64::MAX));
    }
    ok(env.list_pump(&c));
    let q = q_of(env, &c);
    let pm = env.pump_market_listed(PumpSpec::new(c.coin, q.saturating_mul(40)), c);
    env.crank(&pm);
    (pm, q)
}

/// A fork with one market on `coin` ([`add_market`]).
pub fn setup(coin: PumpCoin) -> (Env, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(coin);
    let (pm, q) = add_market(&mut env, c);
    (env, pm, q)
}

/// `open_short(q)` with no bounds of its own.
pub fn open_ix(pm: &impl VenueMarket, b: &Pubkey, nonce: u64, q: u64) -> Instruction {
    open_short_ix(pm, b, nonce, q, 0, u64::MAX, u64::MAX, ExtrasSel::NONE, &[])
}

/// `close_short` with no bound of its own.
pub fn close_ix(pm: &impl VenueMarket, b: &Pubkey, nonce: u64) -> Instruction {
    close_short_ix(pm, b, nonce, u64::MAX, ExtrasSel::NONE, &[])
}

/// A market with an open position.
pub struct Positioned {
    pub env: Env,
    pub pm: PumpMarket,
    pub b: Borrower,
    pub nonce: u64,
    pub q: u64,
}

/// A market with one open position of `q` (the fixed borrower), two cranked seconds old.
pub fn with_position(coin: PumpCoin) -> Positioned {
    let (mut env, pm, q) = setup(coin);
    let mut b = borrower(&mut env, BORROWER);
    env.tick(&pm, 1);
    let (res, nonce) = env.open(&pm, &mut b, q);
    ok(res);
    env.tick(&pm, 2);
    Positioned {
        env,
        pm,
        b,
        nonce,
        q,
    }
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

/// Moves `amount` tokens from a curve user's (planted) balance into `to` (`TransferChecked`,
/// signed by the user): a donation anyone can make.
pub fn donate_tokens(env: &mut Env, pm: &PumpMarket, to: &Pubkey, amount: u64) {
    let u = CurveUser::new(env, &pm.curve);
    fork::set_token_amount(&mut env.svm, &u.ata, amount);
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(6);
    let ix = Instruction {
        program_id: pm.keys.token_program,
        accounts: vec![
            AccountMeta::new(u.ata, false),
            AccountMeta::new_readonly(pm.keys.mint, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(u.kp.pubkey(), true),
        ],
        data,
    };
    ok(env.send(&[ix], &[&u.kp]));
}

/// Donates lamports to the trader PDA and tokens to `trader_base_ata`, so the next leg
/// normalises (ARCH §2.4 rule 5, the pump venues' token side; 1,000,000 lamports: the runtime
/// refuses a new System account below its rent-exempt minimum).
pub fn donate_to_trader(env: &mut Env, pm: &PumpMarket) {
    let griefer = env.funded_keypair();
    ok(env.send(&[transfer(&griefer, &pm.trader(), 1_000_000)], &[&griefer]));
    donate_tokens(env, pm, &pm.trader_base_ata, 777);
}

/// The costliest curve-leg path the fixtures allow (on top of normalisation): the curve's creator
/// changed after the listing to [`NEW_CREATOR`], whose vault is **empty** — since pump-final §6.1
/// (`sell_v3` / `buy_v3`) neither short.win nor pump names or tops up a creator vault, so this
/// costs nothing more and stays only to show it — and the **last** buyback recipient of the live
/// `Global` (TradeExtras T7: the longest key scans, short.win's and pump's).
pub fn costliest_legs(env: &mut Env, pm: &mut PumpMarket) {
    let creator = Pubkey::new_from_array(NEW_CREATOR);
    fork::set_curve_creator(&mut env.svm, &pm.curve.curve, &creator);
    pm.refresh_creator(&env.svm);
    assert_eq!(
        env.svm
            .get_account(&pm.creator_vault)
            .map_or(0, |a| a.lamports),
        0,
        "the new creator's vault is empty"
    );
    pm.use_recipients(&env.svm, 7, 7);
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

/// An external user buys `rT − rt_left` on the curve (a real `buy_v2`), leaving `rt_left`; 0
/// completes it.
pub fn buy_down_to(env: &mut Env, c: &PumpCurve, rt_left: u64) {
    let u = CurveUser::new(env, c);
    let rt = c.state(&env.svm).real_token_reserves;
    ok(u.buy(env, c, rt - rt_left, u64::MAX));
    assert_eq!(c.state(&env.svm).real_token_reserves, rt_left);
}

/// A position the keeper can liquidate (as `pump_it::exits`): a buyer takes 40 % of `vT` (the
/// price × 2.8, past Tier D's `CR_liq`), then the keeper cranks second by second until its
/// simulated `[crank, liquidate]` of the next second succeeds. The clock is at that second,
/// **not cranked**.
pub fn liquidatable(coin: PumpCoin) -> (Positioned, Keypair) {
    let mut p = with_position(coin);
    p.env.tick(&p.pm, 1);
    let u = CurveUser::new(&mut p.env, &p.pm.curve);
    let vt = p.pm.curve.state(&p.env.svm).virtual_token_reserves;
    ok(u.buy(&mut p.env, &p.pm.curve, vt / 10 * 4, u64::MAX));
    let keeper = fixed(&mut p.env, KEEPER);
    for _ in 0..900 {
        p.env.warp(1);
        let ixs = [
            pump::cu_limit_ix(CROSS_CHECK_LIMIT),
            crank_price_ix(&p.pm),
            liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
        ];
        if simulate(&p.env, &ixs, &[&keeper]).is_some() {
            return (p, keeper);
        }
        p.env.crank(&p.pm);
    }
    panic!("{coin:?}: never liquidatable");
}

/// An expired healthy position, three cranked seconds past its deadline.
pub fn expired(coin: PumpCoin) -> (Positioned, Keypair) {
    let mut p = with_position(coin);
    let deadline = p.env.position(&p.pm, &p.b.key(), p.nonce).deadline;
    let now = p.env.now();
    p.env.warp(deadline + 1 - now);
    p.env.tick(&p.pm, 3);
    let keeper = fixed(&mut p.env, KEEPER);
    (p, keeper)
}

/// A pump market seen as list-only (`None`): its ReadSet is the detection set `[bonding_curve,
/// canonical_pool]` (ARCH §3.12 `None` row), no TradeExtras (as `pump_it::common::AsNone`).
pub struct AsNone<'a>(pub &'a PumpMarket);

impl VenueMarket for AsNone<'_> {
    fn keys(&self) -> &MarketKeys {
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

/// The venue kind stored on `pm`'s market.
pub fn venue_of(env: &Env, pm: &PumpMarket) -> VenueKind {
    env.market(&pm.market()).venue_kind().expect("venue")
}

/// A stuck migration (ARCH §5.3, §5.4; `pump_it::graduation`): a position on the near-complete
/// curve, a third party completes the curve 30 min before the deadline, the next crank moves the
/// market to `PumpMigrating`; the keeper cranks and simulates `[crank, sol_settle]` every 30 s
/// until S3 settles (at `migration_seen_at + max_migration_pause_s + s3_wait_s`, 70 min with
/// the defaults). The clock is at that second, **not cranked**.
pub fn s3_migrating() -> (Positioned, Keypair) {
    let mut p = with_position(NEAR);
    let keeper = fixed(&mut p.env, KEEPER);
    let deadline = p.env.position(&p.pm, &p.b.key(), p.nonce).deadline;
    let now = p.env.now();
    p.env.warp(deadline - 1_800 - now);
    p.env.crank(&p.pm);
    p.env.warp(1);
    buy_down_to(&mut p.env, &p.pm.curve, 0);
    p.env.warp(1);
    let meta = ok(p.env.send(&[crank_price_ix(&p.pm)], &[]));
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    s3_loop(&mut p, &keeper, false);
    (p, keeper)
}

/// A curve that completed and migrated **elsewhere** (`pump_it::graduation`: reserves zeroed,
/// no canonical pool) with an expired position: `PumpMigrating → None` on the next crank, then
/// the keeper cranks the detection set and simulates `[crank, sol_settle]` every 30 s until S3
/// settles (`no_route_since + s3_wait_s`; M2-SEC-5). The clock is at that second, **not
/// cranked**.
pub fn s3_none() -> (Positioned, Keypair) {
    let mut p = with_position(NEAR);
    let keeper = fixed(&mut p.env, KEEPER);
    let deadline = p.env.position(&p.pm, &p.b.key(), p.nonce).deadline;
    let now = p.env.now();
    p.env.warp(deadline + 1 - now);
    p.env.crank(&p.pm);
    p.env.warp(1);
    buy_down_to(&mut p.env, &p.pm.curve, 0);
    p.env.warp(1);
    p.env.crank(&p.pm);
    to_none(&mut p.env, &p.pm);
    s3_loop(&mut p, &keeper, true);
    (p, keeper)
}

/// Pump migrates the complete curve to another venue (reserves zeroed, no canonical pool;
/// planted on pump's account as `pump_it::graduation`), and the next crank moves the market
/// `PumpMigrating → None` (`graduated_elsewhere`).
pub fn to_none(env: &mut Env, pm: &PumpMarket) {
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(pm)], &[]));
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::None as u8);
}

fn s3_loop(p: &mut Positioned, keeper: &Keypair, none: bool) {
    for _ in 0..200 {
        p.env.warp(30);
        let (crank, settle) = if none {
            let n = AsNone(&p.pm);
            (
                crank_price_ix(&n),
                sol_settle_ix(&p.env, &n, &keeper.pubkey(), &p.b.key(), p.nonce),
            )
        } else {
            (
                crank_price_ix(&p.pm),
                sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
            )
        };
        if let Some(meta) = simulate(&p.env, &[crank.clone(), settle], &[keeper])
            && events::of::<SolSettle>(&meta)
                .first()
                .is_some_and(|e| e.condition == 3)
        {
            return;
        }
        ok(p.env.send(&[crank], &[]));
    }
    panic!("S3 never due");
}

// ---- synthetic exits (pump-final §5.2, §6.6, §12) ----------------------------------------------

/// pump `Global`'s initial reserves (`vT`, `vQ`, `rT`: ECON's `PumpCurve.at_progress(0)`).
pub const PUMP_VT0: u64 = 1_073_000_000_000_000;
pub const PUMP_VS0: u64 = 30_000_000_000;
pub const PUMP_RT0: u64 = 793_100_000_000_000;
/// ECON's `D@80%` market: the tokens a fresh curve sold (`int(PUMP_RT0·0.8)`;
/// `tests/litesvm/tests/synthetic_exit_it.rs`'s T-SYNTH-2).
pub const D80_SOLD: u64 = 634_480_000_000_000;

/// The ring entries of `pm`'s market whose curve state held less than `q` (`rT_i = x_i − (vT −
/// rT)` on the live offsets, `venues::curve_at`): every quote on such an entry (the EMA leg, the
/// box corners, `B̂_ref`) takes the synthetic path `B̂_v3`. Returns `(past rT, entries)`.
pub fn ring_past_rt(env: &Env, pm: &PumpMarket, q: u64) -> (usize, usize) {
    let m = env.market(&pm.market());
    let c = pm.curve.state(&env.svm);
    let offset = u128::from(c.virtual_token_reserves - c.real_token_reserves);
    let n = usize::from(m.ring_count);
    let past = m
        .ring
        .iter()
        .take(n)
        .filter(|e| e.x.get().saturating_sub(offset) < u128::from(q))
        .count();
    (past, n)
}

/// pump-final §6.6's worst ring: every ring entry past `rT` (the ring holds the EMA's cached
/// reprojection, which follows the crowd's buy-down over a few `τ`, so this takes the keeper's
/// cranks of ≈ 20 s after it, not [`RING_LEN`] alone).
fn worst_ring_reached(env: &Env, pm: &PumpMarket, q: u64) -> bool {
    ring_past_rt(env, pm, q) == (RING_LEN, RING_LEN)
}

fn assert_worst_ring(env: &Env, pm: &PumpMarket, q: u64) {
    assert_eq!(
        ring_past_rt(env, pm, q),
        (RING_LEN, RING_LEN),
        "every ring entry past rT"
    );
}

/// A straddled position (pump-final §5.2): a position of `q` on the near-complete curve, two
/// cranked seconds old; the crowd leaves `rT = q/2`, so its buyback takes `rT` on the curve and
/// `q − rT` from the would-be pool, completing the curve in the exit. `worst`: the keeper then
/// cranks second by second until every ring entry is past `rT` ([`worst_ring_reached`]).
pub fn straddled_position(worst: bool) -> Positioned {
    let mut p = with_position(NEAR);
    buy_down_to(&mut p.env, &p.pm.curve, p.q / 2);
    if worst {
        for _ in 0..600 {
            if worst_ring_reached(&p.env, &p.pm, p.q) {
                return p;
            }
            p.env.tick(&p.pm, 1);
        }
        panic!("the ring never passed rT");
    }
    p
}

/// ECON's `D@80%` market (as T-SYNTH-2): the fresh fixture curve set to pump's initial reserves
/// (the fixture's 1-lamport creation buy undone in the reserves), the crowd's real `buy_v2` of
/// [`D80_SOLD`], the listing with the init set, `40·Q` lent (`Q` = [`q_of`]: T-SYNTH-2's
/// ECON-sized position needs the sim's lifted wallet cap), the warm-up and a crank. Returns `Q`.
pub fn d80_market() -> (Env, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(FRESH);
    fork::set_curve_reserves(&mut env.svm, &c.curve, PUMP_VT0, PUMP_VS0, PUMP_RT0, 0);
    let crowd = CurveUser::new(&mut env, &c);
    env.fund(&crowd.kp.pubkey(), 2_000_000_000_000);
    ok(crowd.buy(&mut env, &c, D80_SOLD, u64::MAX));
    ok(env.list_pump(&c));
    let q = q_of(&env, &c);
    let pm = env.pump_market_listed(PumpSpec::new(FRESH, q * 40), c);
    env.crank(&pm);
    (env, pm, q)
}

/// A straddled position the keeper liquidates (T-SYNTH-2's path): a position of `Q` on ECON's
/// `D@80%` market, 30 cranked seconds old; the crowd leaves `rT = Q/2` (the price × 2.4, past Tier
/// D's `CR_liq`); the keeper cranks second by second until its simulated `[crank_price,
/// liquidate]` of the next second succeeds — `worst`: and every ring entry is past `rT`
/// ([`worst_ring_reached`]). The clock is at that second, **not cranked**.
pub fn liquidatable_synthetic(worst: bool) -> (Positioned, Keypair) {
    let (mut env, pm, q) = d80_market();
    let mut b = borrower(&mut env, BORROWER);
    env.tick(&pm, 1);
    let (res, nonce) = env.open(&pm, &mut b, q);
    ok(res);
    env.tick(&pm, 30);
    buy_down_to(&mut env, &pm.curve, q / 2);
    let keeper = fixed(&mut env, KEEPER);
    let mut p = Positioned {
        env,
        pm,
        b,
        nonce,
        q,
    };
    for _ in 0..900 {
        p.env.warp(1);
        let ixs = [
            pump::cu_limit_ix(CROSS_CHECK_LIMIT),
            crank_price_ix(&p.pm),
            liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
        ];
        if (!worst || worst_ring_reached(&p.env, &p.pm, p.q))
            && simulate(&p.env, &ixs, &[&keeper]).is_some()
        {
            return (p, keeper);
        }
        p.env.crank(&p.pm);
    }
    panic!("the straddled position was never liquidatable");
}

/// An expired straddled position: three cranked seconds past its deadline on the near-complete
/// curve, then the crowd leaves `rT = q/2`; the keeper cranks second by second until its simulated
/// `[crank_price, expire]` succeeds (the EMA has followed the crowd's buy-down) — `worst`: and
/// every ring entry is past `rT`. The clock is at that second, **not cranked**.
pub fn expired_synthetic(worst: bool) -> (Positioned, Keypair) {
    let (mut p, keeper) = expired(NEAR);
    buy_down_to(&mut p.env, &p.pm.curve, p.q / 2);
    for _ in 0..600 {
        p.env.warp(1);
        let ixs = [
            pump::cu_limit_ix(CROSS_CHECK_LIMIT),
            crank_price_ix(&p.pm),
            expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
        ];
        if (!worst || worst_ring_reached(&p.env, &p.pm, p.q))
            && simulate(&p.env, &ixs, &[&keeper]).is_some()
        {
            return (p, keeper);
        }
        p.env.crank(&p.pm);
    }
    panic!("the straddled expiry never met its bound");
}

/// A third party completes `c` by synthetic migration: its `buy_v3` of `rT + extra` (`extra` from
/// the would-be pool: `pcB = extra`, `pcQ` its quote).
pub fn complete_synthetically(env: &mut Env, c: &PumpCurve, extra: u64) {
    let u = CurveUser::new(env, c);
    env.fund(&u.kp.pubkey(), 2_000_000_000_000);
    let rt = c.state(&env.svm).real_token_reserves;
    let ix = u.buy_v3_ix(&env.svm, c, rt + extra, u64::MAX);
    ok(u.send(env, ix));
    let s = c.state(&env.svm);
    assert!(s.complete && s.post_complete_base_out == extra, "pcB");
}

/// A stuck synthetic migration (pump-final §5.4, §12's "S3 on `PumpMigrating` with `pc ≠ 0`"):
/// as [`s3_migrating`], but the third party completes the curve with a `buy_v3` past `rT` (a pool
/// part of `q`): the next crank moves the market to `PumpMigrating` with the post-complete fields
/// set, and S3's FV prices the pool `migrate_v2` will create (`X₁` the live `curve_base_ata`, `Y₁`
/// with `pcQ`, ρ-scaled). The clock is at the second S3 settles, **not cranked**.
pub fn s3_migrating_synthetic() -> (Positioned, Keypair) {
    let mut p = with_position(NEAR);
    let keeper = fixed(&mut p.env, KEEPER);
    let deadline = p.env.position(&p.pm, &p.b.key(), p.nonce).deadline;
    let now = p.env.now();
    p.env.warp(deadline - 1_800 - now);
    p.env.crank(&p.pm);
    p.env.warp(1);
    let q = p.q;
    complete_synthetically(&mut p.env, &p.pm.curve, q);
    p.env.warp(1);
    let meta = ok(p.env.send(&[crank_price_ix(&p.pm)], &[]));
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    s3_loop(&mut p, &keeper, false);
    (p, keeper)
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

// ---- setups ---------------------------------------------------------------------------------

/// `create_market(Auto)` of a live curve with the pump init set (the trader's base ATA and pump
/// UVA created by CPI).
fn create_curve(coin: PumpCoin) -> Prepared {
    let (mut env, c) = Env::fork_pump_coin(coin);
    let creator = fixed(&mut env, CREATOR);
    let ix = pump::create_pump_market_ix(&creator.pubkey(), &c);
    prepared(env, ix, &[&creator])
}

/// `create_market(Auto)` of the near-complete curve after a third party completed it (a real
/// `buy_v2` of all `rT`): `PumpMigrating`, with the init set.
fn create_migrating() -> Prepared {
    let (mut env, c) = Env::fork_pump_coin(NEAR);
    buy_down_to(&mut env, &c, 0);
    let creator = fixed(&mut env, CREATOR);
    let ix = pump::create_pump_market_ix(&creator.pubkey(), &c);
    prepared(env, ix, &[&creator])
}

/// `create_market(Auto)` of a graduated coin: a PumpSwap-class coin lists on its canonical pool
/// with the pAMM init set (M6; the trader's pAMM UVA created by CPI; `pamm::pamm_init_set`) —
/// the M6 bench `tests/bench/m6_cu` measures every PumpSwap listing —; Fartcoin, graduated
/// elsewhere, as a list-only `None` with the flag (the detection set only).
fn create_none(coin: PumpCoin) -> Prepared {
    let (mut env, c) = Env::fork_pump_coin(coin);
    let creator = fixed(&mut env, CREATOR);
    let ix = ix::create_market(
        &creator.pubkey(),
        &c.mint,
        &c.token_program.id(),
        shortwin::instructions::create_market::VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    let ix = if coin == PumpCoin::Graduated {
        let market = pda::market(&c.mint).0;
        ix::with_remaining(ix, &pamm::pamm_init_set(&market))
    } else {
        ix
    };
    prepared(env, ix, &[&creator])
}

fn open_first(coin: PumpCoin) -> Prepared {
    let (mut env, pm, q) = setup(coin);
    env.tick(&pm, 1);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&pm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn open_repeat(coin: PumpCoin) -> Prepared {
    let Positioned {
        env,
        pm,
        b,
        nonce,
        q,
        ..
    } = with_position(coin);
    let ix = open_ix(&pm, &b.key(), nonce + 1, q);
    prepared(env, ix, &[&b.kp])
}

fn open_normalising(coin: PumpCoin) -> Prepared {
    let (mut env, pm, q) = setup(coin);
    env.tick(&pm, 1);
    donate_to_trader(&mut env, &pm);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&pm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

/// `vT/100`: the costliest open's size (twice [`q_of`]). It once had to cover v2's creator-vault
/// top-up within `exec_tol_bps`; `sell_v3` makes none (pump-final §6.1).
pub fn q_top_up(env: &Env, c: &PumpCurve) -> u64 {
    c.state(&env.svm).virtual_token_reserves / 100
}

/// The costliest open: a wallet's first, normalising, on the 151-byte curve after a creator change
/// into an empty vault with the last recipients ([`costliest_legs`]).
fn open_costliest() -> Prepared {
    let (mut env, mut pm, _) = setup(NEAR);
    let q = q_top_up(&env, &pm.curve);
    costliest_legs(&mut env, &mut pm);
    env.tick(&pm, 1);
    donate_to_trader(&mut env, &pm);
    let b = borrower(&mut env, BORROWER);
    let ix = open_ix(&pm, &b.key(), 0, q);
    prepared(env, ix, &[&b.kp])
}

fn close(coin: PumpCoin, normalise: bool) -> Prepared {
    let Positioned {
        mut env,
        pm,
        b,
        nonce,
        ..
    } = with_position(coin);
    if normalise {
        donate_to_trader(&mut env, &pm);
    }
    let ix = close_ix(&pm, &b.key(), nonce);
    prepared(env, ix, &[&b.kp])
}

/// The costliest close: normalising, after a creator change into an empty vault (no top-up
/// inside `B`), the last recipients.
fn close_costliest(coin: PumpCoin) -> Prepared {
    let Positioned {
        mut env,
        mut pm,
        b,
        nonce,
        ..
    } = with_position(coin);
    costliest_legs(&mut env, &mut pm);
    donate_to_trader(&mut env, &pm);
    let ix = close_ix(&pm, &b.key(), nonce);
    prepared(env, ix, &[&b.kp])
}

/// A completing buyback (ARCH §5.4 case 1): a third party leaves `rT = q`, the close buys it and
/// the venue sync after the leg moves the market to `PumpMigrating` in the same instruction
/// (`VenueStateChanged`).
fn close_completing() -> Prepared {
    let Positioned {
        mut env,
        pm,
        b,
        nonce,
        q,
    } = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, q);
    env.warp(1);
    let ix = close_ix(&pm, &b.key(), nonce);
    prepared(env, ix, &[&b.kp])
}

fn liquidate(coin: PumpCoin) -> Prepared {
    let (mut p, keeper) = liquidatable(coin);
    p.env.crank(&p.pm);
    let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

/// The costliest liquidation: normalising, a creator change into an empty vault, the last
/// recipients.
fn liquidate_costliest(coin: PumpCoin) -> Prepared {
    let (mut p, keeper) = liquidatable(coin);
    costliest_legs(&mut p.env, &mut p.pm);
    donate_to_trader(&mut p.env, &p.pm);
    p.env.crank(&p.pm);
    let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn expire(coin: PumpCoin) -> Prepared {
    let (p, keeper) = expired(coin);
    let ix = expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

/// The costliest expiry: normalising, a creator change into an empty vault, the last recipients.
fn expire_costliest(coin: PumpCoin) -> Prepared {
    let (mut p, keeper) = expired(coin);
    costliest_legs(&mut p.env, &mut p.pm);
    donate_to_trader(&mut p.env, &p.pm);
    let ix = expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

/// An expiry whose buyback completes the curve (`rT = q` left by a third party):
/// `PumpCurve → PumpMigrating` in the same instruction. The third party's buy lifts the price
/// above the expiry bound's reference, so the keeper cranks second by second until its
/// simulated `[crank, expire]` succeeds (the EMA has followed), cranks, and expires.
fn expire_completing() -> Prepared {
    let (mut p, keeper) = expired(NEAR);
    buy_down_to(&mut p.env, &p.pm.curve, p.q);
    for _ in 0..600 {
        p.env.warp(1);
        let ixs = [
            pump::cu_limit_ix(CROSS_CHECK_LIMIT),
            crank_price_ix(&p.pm),
            expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
        ];
        if simulate(&p.env, &ixs, &[&keeper]).is_some() {
            p.env.crank(&p.pm);
            let ix = expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
            return prepared(p.env, ix, &[&keeper]);
        }
        p.env.crank(&p.pm);
    }
    panic!("the completing expiry never met its bound");
}

/// A synthetic close: [`straddled_position`], then the borrower's close one second later
/// (`buy_v3(q)`: `rT` on the curve, `q − rT` from the would-be pool; `PumpCurve → PumpMigrating`
/// in the same instruction).
fn close_synthetic(worst: bool) -> Prepared {
    let Positioned {
        mut env,
        pm,
        b,
        nonce,
        ..
    } = straddled_position(worst);
    env.warp(1);
    let ix = close_ix(&pm, &b.key(), nonce);
    prepared(env, ix, &[&b.kp])
}

/// A synthetic liquidation ([`liquidatable_synthetic`]), cranked in its second.
fn liquidate_synthetic(worst: bool) -> Prepared {
    let (mut p, keeper) = liquidatable_synthetic(worst);
    p.env.crank(&p.pm);
    if worst {
        assert_worst_ring(&p.env, &p.pm, p.q);
    }
    let ix = liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

/// A synthetic expiry ([`expired_synthetic`]), cranked in its second.
fn expire_synthetic(worst: bool) -> Prepared {
    let (mut p, keeper) = expired_synthetic(worst);
    p.env.crank(&p.pm);
    if worst {
        assert_worst_ring(&p.env, &p.pm, p.q);
    }
    let ix = expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn settle_noop(coin: PumpCoin) -> Prepared {
    let Positioned {
        mut env,
        pm,
        b,
        nonce,
        ..
    } = with_position(coin);
    let keeper = fixed(&mut env, KEEPER);
    let ix = sol_settle_ix(&env, &pm, &keeper.pubkey(), &b.key(), nonce);
    prepared(env, ix, &[&keeper])
}

fn settle_s3_migrating() -> Prepared {
    let (mut p, keeper) = s3_migrating();
    p.env.crank(&p.pm);
    let ix = sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn settle_s3_migrating_synthetic() -> Prepared {
    let (mut p, keeper) = s3_migrating_synthetic();
    p.env.crank(&p.pm);
    let ix = sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce);
    prepared(p.env, ix, &[&keeper])
}

fn settle_s3_none() -> Prepared {
    let (mut p, keeper) = s3_none();
    p.env.crank(&AsNone(&p.pm));
    let ix = sol_settle_ix(
        &p.env,
        &AsNone(&p.pm),
        &keeper.pubkey(),
        &p.b.key(),
        p.nonce,
    );
    prepared(p.env, ix, &[&keeper])
}

fn crank_roll(coin: PumpCoin) -> Prepared {
    let Positioned { mut env, pm, .. } = with_position(coin);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

fn crank_same_second(coin: PumpCoin) -> Prepared {
    let Positioned { env, pm, .. } = with_position(coin);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// The crank that sees a curve a third party completed: `PumpCurve → PumpMigrating` (the
/// migration pause starts, the S3 and `pool_fail` chains start), a position open.
fn crank_to_migrating() -> Prepared {
    let Positioned { mut env, pm, .. } = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, 0);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// The crank that sees a curve a third party completed by synthetic migration (its `buy_v3` past
/// `rT`, a pool part of `q`): `PumpCurve → PumpMigrating` with the post-complete fields set
/// (pump-final §12), a position open.
fn crank_to_migrating_synthetic() -> Prepared {
    let Positioned { mut env, pm, q, .. } = with_position(NEAR);
    complete_synthetically(&mut env, &pm.curve, q);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// A crank of the market short.win's own synthetic close moved to `PumpMigrating` (`pc ≠ 0`; a
/// second position open): roll + record on the post-complete fields, one second after the close.
fn crank_migrating_after_synthetic_exit() -> Prepared {
    let Positioned {
        mut env,
        pm,
        b,
        nonce,
        q,
    } = with_position(NEAR);
    // The other position opens before the crowd's buy-down (Tier D's γ guard refuses an open
    // against a curve with `rT` this low, `CapCurveGamma`).
    let mut other = borrower(&mut env, BORROWER + 1);
    let (res, _) = env.open(&pm, &mut other, q / 4);
    ok(res);
    env.tick(&pm, 1);
    buy_down_to(&mut env, &pm.curve, q / 2);
    env.warp(1);
    let meta = ok(env.close(&pm, &b, nonce));
    let ev: VenueStateChanged = events::one(&meta);
    assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// The crank that sees the complete curve migrated elsewhere: `PumpMigrating → None`
/// (`graduated_elsewhere`), a position open.
fn crank_to_none() -> Prepared {
    let Positioned { mut env, pm, .. } = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, 0);
    env.warp(1);
    env.crank(&pm);
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    let ix = crank_price_ix(&pm);
    prepared(env, ix, &[])
}

/// A crank of a graduated-elsewhere market on its detection set (the S3 chain running), a
/// position open.
fn crank_none() -> Prepared {
    let Positioned { mut env, pm, .. } = with_position(NEAR);
    buy_down_to(&mut env, &pm.curve, 0);
    env.warp(1);
    env.crank(&pm);
    to_none(&mut env, &pm);
    env.warp(1);
    let ix = crank_price_ix(&AsNone(&pm));
    prepared(env, ix, &[])
}

/// `None → PumpCurve` (ARCH §2.5 `None` row; `pump_it::graduation`): a mint listed before its
/// curve was readable (the curve account removed for the listing, restored after) is re-detected
/// by the crank on its detection set — the curve cache written, the price started, Tier D, the
/// chains and the collection window cleared.
fn crank_redetect() -> Prepared {
    let (mut env, c) = Env::fork_pump_coin(T22);
    let saved = env.svm.get_account(&c.curve).expect("curve");
    env.svm.set_account(c.curve, Account::default()).unwrap();
    let creator = fixed(&mut env, CREATOR);
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
    let pm = env.pump_market_listed(PumpSpec::new(T22, (vt / 200) * 40), c);
    assert_eq!(venue_of(&env, &pm), VenueKind::None);
    let ix = crank_price_ix(&AsNone(&pm));
    prepared(env, ix, &[])
}

fn sweep(coin: PumpCoin, donated: bool) -> Prepared {
    let (mut env, pm, _) = setup(coin);
    if donated {
        donate_to_trader(&mut env, &pm);
    }
    let ix = pump::sweep_trader_pump_ix(&env, &pm);
    prepared(env, ix, &[])
}

/// `flag_venue_upgraded` on the dumped ProgramData whose deploy slot changed (the header
/// rewritten; `tests/litesvm/tests/latch_pump_it.rs` runs the loader's real `Upgrade`).
fn flag_pump(venue: u8) -> Prepared {
    let (mut env, _) = Env::fork_pump_coin(T22);
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    fork::set_pump_deploy_slot(&mut env.svm, pump_program(venue), dumped + 1);
    prepared(env, ix::flag_venue_upgraded(venue), &[])
}

fn revalidate_pump(venue: u8) -> Prepared {
    let Prepared { mut env, ix, .. } = flag_pump(venue);
    ok(env.send(&[ix], &[]));
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let ea = env.pause_authority.insecure_clone();
    let ix = ix::revalidate_venue(&ea.pubkey(), venue, dumped + 1);
    prepared(env, ix, &[&ea])
}

/// The label suffix of pump-final §6.6's worst-ring cases ([`assert_worst_ring`]).
fn worst_ring(worst: bool) -> &'static str {
    if worst {
        ", worst ring (every ring entry past rT)"
    } else {
        ""
    }
}

/// Every curve-path instruction (ARCH §9 M5) on SPL Token and Token-2022 curves, the three
/// venue transitions the crank drives and the two exits make, both S3 paths (M2-SEC-5), the
/// token leg of `sweep_trader`, the latch on the dumped ProgramData.
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
    for c in CURVES {
        add(
            "create_market",
            format!(
                "Auto, {}: `PumpCurve`, the trader's base ATA and pump UVA created",
                label(c)
            ),
            CREATE,
            None,
            Box::new(move || create_curve(c)),
        );
    }
    add(
        "create_market",
        format!(
            "Auto, {}, completed by a third party: `PumpMigrating`, with the init set",
            label(NEAR)
        ),
        CREATE,
        None,
        Box::new(create_migrating),
    );
    add(
        "create_market",
        format!(
            "Auto, {}: `PumpSwap` on its canonical pool, the pAMM init set (M6)",
            label(PumpCoin::Graduated)
        ),
        CREATE,
        None,
        Box::new(|| create_none(PumpCoin::Graduated)),
    );
    add(
        "create_market",
        format!(
            "Auto, {}: list-only `None` (detection set only)",
            label(PumpCoin::Fartcoin)
        ),
        CREATE,
        None,
        Box::new(|| create_none(PumpCoin::Fartcoin)),
    );
    for c in CURVES {
        add(
            "open_short",
            format!(
                "{}: first open of a wallet (position + exposure created)",
                label(c)
            ),
            OPEN,
            OPEN_ROW,
            Box::new(move || open_first(c)),
        );
    }
    for c in VARIANTS {
        add(
            "open_short",
            format!("{}: second position of a wallet", label(c)),
            OPEN,
            OPEN_ROW,
            Box::new(move || open_repeat(c)),
        );
        add(
            "open_short",
            format!(
                "{}: first open, normalising (lamports to the trader, tokens to trader_base_ata)",
                label(c)
            ),
            OPEN,
            OPEN_ROW,
            Box::new(move || open_normalising(c)),
        );
    }
    add(
        "open_short",
        format!(
            "{}: costliest — first open, normalising, after a creator change into an empty vault, the last buyback recipient",
            label(NEAR)
        ),
        OPEN,
        OPEN_ROW,
        Box::new(open_costliest),
    );
    for c in CURVES {
        add(
            "close_short",
            label(c).into(),
            CLOSE,
            EXIT_ROW,
            Box::new(move || close(c, false)),
        );
    }
    for c in VARIANTS {
        add(
            "close_short",
            format!("{}: normalising", label(c)),
            CLOSE,
            EXIT_ROW,
            Box::new(move || close(c, true)),
        );
    }
    for c in [T22, NEAR] {
        add(
            "close_short",
            format!(
                "{}: costliest — normalising, after a creator change into an empty vault, the last buyback recipient",
                label(c)
            ),
            CLOSE,
            EXIT_ROW,
            Box::new(move || close_costliest(c)),
        );
    }
    add(
        "close_short",
        format!(
            "{}: completing buyback (q = rT; `PumpCurve → PumpMigrating` in the same instruction)",
            label(NEAR)
        ),
        CLOSE,
        EXIT_ROW,
        Box::new(close_completing),
    );
    for worst in [false, true] {
        add(
            "close_short",
            format!(
                "{}: synthetic past rT (the crowd left rT = q/2: `buy_v3(q)` takes rT on the curve and q − rT from the would-be pool; `PumpCurve → PumpMigrating`){}",
                label(NEAR),
                worst_ring(worst)
            ),
            CLOSE,
            EXIT_ROW,
            Box::new(move || close_synthetic(worst)),
        );
    }
    for c in [SPL, T22, FRESH] {
        add(
            "liquidate",
            format!(
                "{}: after a real pump (40 % of vT bought), both legs",
                label(c)
            ),
            EXIT,
            EXIT_ROW,
            Box::new(move || liquidate(c)),
        );
    }
    add(
        "liquidate",
        format!(
            "{}: costliest — normalising, creator change into an empty vault, last recipients",
            label(T22)
        ),
        EXIT,
        EXIT_ROW,
        Box::new(|| liquidate_costliest(T22)),
    );
    for worst in [false, true] {
        add(
            "liquidate",
            format!(
                "ECON's D@80 % (fresh, Token-2022): synthetic past rT (the crowd left rT = q/2, the price × 2.4), both legs{}",
                worst_ring(worst)
            ),
            EXIT,
            EXIT_ROW,
            Box::new(move || liquidate_synthetic(worst)),
        );
    }
    for c in CURVES {
        add(
            "expire",
            format!("{}: past the deadline, healthy", label(c)),
            EXIT,
            EXIT_ROW,
            Box::new(move || expire(c)),
        );
    }
    add(
        "expire",
        format!(
            "{}: costliest — normalising, creator change into an empty vault, last recipients",
            label(NEAR)
        ),
        EXIT,
        EXIT_ROW,
        Box::new(|| expire_costliest(NEAR)),
    );
    add(
        "expire",
        format!(
            "{}: completing buyback (q = rT; `PumpCurve → PumpMigrating`)",
            label(NEAR)
        ),
        EXIT,
        EXIT_ROW,
        Box::new(expire_completing),
    );
    for worst in [false, true] {
        add(
            "expire",
            format!(
                "{}: synthetic past rT (the crowd left rT = q/2 after the deadline; `PumpCurve → PumpMigrating`){}",
                label(NEAR),
                worst_ring(worst)
            ),
            EXIT,
            EXIT_ROW,
            Box::new(move || expire_synthetic(worst)),
        );
    }
    for c in VARIANTS {
        add(
            "sol_settle",
            format!("{}: healthy, the no-change call", label(c)),
            SETTLE,
            SETTLE_ROW,
            Box::new(move || settle_noop(c)),
        );
    }
    add(
        "sol_settle",
        "`PumpMigrating` (stuck migration): S3 at migration_seen_at + 70 min, FV on the post-migration pool".into(),
        SETTLE,
        SETTLE_ROW,
        Box::new(settle_s3_migrating),
    );
    add(
        "sol_settle",
        "`PumpMigrating` after a third party's synthetic completion (`pc ≠ 0`): S3 at migration_seen_at + 70 min, FV ρ-scaled on the pool migrate_v2 will create".into(),
        SETTLE,
        SETTLE_ROW,
        Box::new(settle_s3_migrating_synthetic),
    );
    add(
        "sol_settle",
        "`None` (graduated elsewhere; the detection set as ReadSet): S3 after s3_wait_s (M2-SEC-5)"
            .into(),
        SETTLE,
        SETTLE_ROW,
        Box::new(settle_s3_none),
    );
    for c in CURVES {
        add(
            "crank_price",
            format!("{}: roll + record (first crank of a second)", label(c)),
            CRANK,
            TWO_ROW,
            Box::new(move || crank_roll(c)),
        );
    }
    add(
        "crank_price",
        format!("{}: record only (same second again)", label(T22)),
        CRANK,
        TWO_ROW,
        Box::new(|| crank_same_second(T22)),
    );
    add(
        "crank_price",
        format!(
            "{}: `PumpCurve → PumpMigrating` (completed by a third party), chains start",
            label(NEAR)
        ),
        CRANK,
        TWO_ROW,
        Box::new(crank_to_migrating),
    );
    add(
        "crank_price",
        format!(
            "{}: `PumpCurve → PumpMigrating` after a third party's synthetic completion (`buy_v3` past rT, `pc ≠ 0`), chains start",
            label(NEAR)
        ),
        CRANK,
        TWO_ROW,
        Box::new(crank_to_migrating_synthetic),
    );
    add(
        "crank_price",
        format!(
            "{}: `PumpMigrating` after short.win's own synthetic close (`pc ≠ 0`), roll + record",
            label(NEAR)
        ),
        CRANK,
        TWO_ROW,
        Box::new(crank_migrating_after_synthetic_exit),
    );
    add(
        "crank_price",
        "`PumpMigrating → None` (migrated elsewhere; `graduated_elsewhere`)".into(),
        CRANK,
        TWO_ROW,
        Box::new(crank_to_none),
    );
    add(
        "crank_price",
        "`None` (graduated elsewhere), the detection set, the S3 chain running".into(),
        CRANK,
        TWO_ROW,
        Box::new(crank_none),
    );
    add(
        "crank_price",
        format!(
            "{}: `None → PumpCurve` re-detection (curve cache, price start, Tier D, chains and fp_* cleared)",
            label(T22)
        ),
        CRANK,
        TWO_ROW,
        Box::new(crank_redetect),
    );
    for c in VARIANTS {
        add(
            "sweep_trader",
            format!(
                "{}: token leg — lamports on the trader, tokens on trader_base_ata",
                label(c)
            ),
            SWEEP,
            None,
            Box::new(move || sweep(c, true)),
        );
    }
    add(
        "sweep_trader",
        format!("{}: nothing to sweep", label(T22)),
        SWEEP,
        None,
        Box::new(|| sweep(T22, false)),
    );
    for (venue, what) in [
        (GUARD_PUMP, "pump, the dumped 10,485,760-byte ProgramData"),
        (
            GUARD_PUMP_FEES,
            "pump-fees, the dumped 948,989-byte ProgramData",
        ),
        (
            GUARD_PUMP_AMM,
            "PumpSwap, the dumped 10,485,760-byte ProgramData",
        ),
    ] {
        add(
            "flag_venue_upgraded",
            what.into(),
            LATCH,
            TWO_ROW,
            Box::new(move || flag_pump(venue)),
        );
    }
    for (venue, what) in [
        (GUARD_PUMP, "pump, the dumped ProgramData"),
        (GUARD_PUMP_FEES, "pump-fees, the dumped ProgramData"),
    ] {
        add(
            "revalidate_venue",
            what.into(),
            LATCH,
            TWO_ROW,
            Box::new(move || revalidate_pump(venue)),
        );
    }
    v
}

// ---- the protocol ALT (A28; ARCH §6.4) ------------------------------------------------------

/// ARCH §6.4's A28 list as it applies to a curve market: the harness's Raydium part
/// (`raydium::protocol_alt_addresses`: the programs a short.win instruction names — SPL Token,
/// Token-2022, ATA, System — WSOL, short.win's `config`, `venue_guard`, `oracle_cache` and event
/// authority, the Raydium authorities and `AmmConfig`s), then the pump part: pump, PumpSwap and
/// pump-fees, pump `global`, `event_authority`, `fee_config` and global volume accumulator, the
/// same four of PumpSwap, and the live `Global`'s 8 fee and 8 buyback recipients and
/// `GlobalConfig`'s 8 protocol and 8 buyback recipients, each with its WSOL ATA
/// (`shortwin_keeper::alt`'s order, deduplicated). Read from the fork's live accounts.
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
    /// `tests/litesvm/tests/trace_pump_it.rs`).
    pub expected: usize,
    /// The peak stack height it must reach.
    pub peak: u8,
    /// A keeper batch sent as a **v1** transaction (ARCH §6.4: 4,096 B, every key inline, no
    /// lookup table); otherwise the transaction must fit a v0 one with the protocol ALT.
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
    /// v0 wire size with the protocol ALT (A28, the pump part included) and accounts (inline +
    /// looked up).
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

/// An atomic sandwich of a normalising open with the venue's own trades: a front-run `buy_v2`
/// of `vT/50` and a back-run `sell_v2` of the same tokens, by an attacker with a fixed key.
fn tx_sandwiched_open(coin: PumpCoin) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, pm, q) = setup(coin);
    let attacker = fixed(&mut env, ATTACKER);
    let payer = env.payer.pubkey();
    let program = pm.curve.token_program.id();
    ok(env.send(
        &[
            fork::create_ata_idempotent_ix(&payer, &attacker.pubkey(), &pm.keys.mint, &program),
            fork::pump_init_uva_ix(&payer, &attacker.pubkey()),
        ],
        &[],
    ));
    let ata = pda::ata(&attacker.pubkey(), &pm.keys.mint, &program);
    let x = pm.curve.state(&env.svm).virtual_token_reserves / 50;
    let fr = fork::pump_fee_recipient(&env.svm, 1);
    let bb = fork::pump_buyback_fee_recipient(&env.svm, 1);
    let front = pm.curve.v2_ix(
        &env.svm,
        PumpV2::Buy,
        &attacker.pubkey(),
        &ata,
        &fr,
        &bb,
        x,
        u64::MAX,
    );
    let back = pm.curve.v2_ix(
        &env.svm,
        PumpV2::Sell,
        &attacker.pubkey(),
        &ata,
        &fr,
        &bb,
        x,
        0,
    );
    env.tick(&pm, 1);
    donate_to_trader(&mut env, &pm);
    let v = borrower(&mut env, BORROWER);
    let ixs = with_cb(vec![front, open_ix(&pm, &v.key(), 0, q), back]);
    (env, ixs, vec![attacker, v.kp.insecure_clone()])
}

/// `k` opens of one wallet in one transaction on the near-complete curve (the first creates the
/// exposure record), each of `vT/800` (0.125 %: five stay under the wallet cap and above the
/// notional floor; the other curves' prices put `vT/800` under the floor).
pub fn opens_of_one_wallet(k: u64) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, pm, _) = setup(NEAR);
    let q = pm.curve.state(&env.svm).virtual_token_reserves / 800;
    env.tick(&pm, 1);
    let b = borrower(&mut env, BORROWER);
    let ixs = (0..k).map(|n| open_ix(&pm, &b.key(), n, q)).collect();
    (env, with_cb(ixs), vec![b.kp.insecure_clone()])
}

/// The four live SOL curves of the dump listed in one fork (a crank batch's markets).
pub fn four_markets() -> (Env, Vec<PumpMarket>) {
    let (mut env, first) = Env::fork_pump_coin(CURVES[0]);
    let mut markets = vec![add_market(&mut env, first).0];
    for coin in &CURVES[1..] {
        let c = env.load_pump_coin(*coin);
        markets.push(add_market(&mut env, c).0);
    }
    (env, markets)
}

/// A keeper batch of cranks on the four curve markets, each curve just completed by a third
/// party: every crank moves its market to `PumpMigrating` (a crank's worst case).
pub fn tx_crank_batch() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, markets) = four_markets();
    for pm in &markets {
        env.crank(pm);
        buy_down_to(&mut env, &pm.curve, 0);
    }
    env.warp(1);
    let ixs = markets.iter().map(crank_price_ix).collect();
    (env, with_cb(ixs), vec![])
}

fn tx_keeper_liquidation(coin: PumpCoin, costliest: bool) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut p, keeper) = liquidatable(coin);
    if costliest {
        costliest_legs(&mut p.env, &mut p.pm);
        donate_to_trader(&mut p.env, &p.pm);
    }
    let ixs = vec![
        crank_price_ix(&p.pm),
        liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_expiry(coin: PumpCoin) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut p, keeper) = expired(coin);
    p.env.warp(1);
    let ixs = vec![
        crank_price_ix(&p.pm),
        expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_s3_migrating() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (p, keeper) = s3_migrating();
    let ixs = vec![
        crank_price_ix(&p.pm),
        sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

/// The keeper's `[crank_price, liquidate]` of a synthetic exit ([`liquidatable_synthetic`]).
fn tx_keeper_liquidation_synthetic(worst: bool) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (p, keeper) = liquidatable_synthetic(worst);
    let ixs = vec![
        crank_price_ix(&p.pm),
        liquidate_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

/// The keeper's `[crank_price, expire]` of a synthetic exit ([`expired_synthetic`]).
fn tx_keeper_expiry_synthetic(worst: bool) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (p, keeper) = expired_synthetic(worst);
    let ixs = vec![
        crank_price_ix(&p.pm),
        expire_ix(&p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

fn tx_keeper_s3_migrating_synthetic() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (p, keeper) = s3_migrating_synthetic();
    let ixs = vec![
        crank_price_ix(&p.pm),
        sol_settle_ix(&p.env, &p.pm, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

/// How the curve completed before the keeper's top-level `migrate_v2` (pump-final §6.6's
/// `migrate_v2` row; T-TRACE's `migrate_v2` rows).
#[derive(Clone, Copy, Debug)]
pub enum Completion {
    /// A third party's `buy_v2` of exactly `rT` ([`buy_down_to`]; `pc = 0`): the crowd's usual
    /// completion. (Until pump-final P6 this row was the straddle bundle's tx2; it measures no
    /// helper and stays, as the keeper migrates such a curve.)
    Exact,
    /// A third party's `buy_v3` of `rT + q` ([`complete_synthetically`]; `pc ≠ 0`).
    Synthetic,
    /// short.win's own synthetic close (`pc ≠ 0`; the keeper migrates after an exit's
    /// `→ PumpMigrating`, `src/keeper.rs`).
    OwnClose,
}

/// The keeper's top-level `migrate_v2` after the curve completed as `how` says. After a
/// synthetic completion the pool it creates holds the live `curve_base_ata` and `rQ + pcQ − fee`
/// (T-SYNTH-3).
fn tx_migrate_after(how: Completion) -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let mut p = match how {
        Completion::OwnClose => {
            let mut p = straddled_position(false);
            p.env.warp(1);
            let meta = ok(p.env.close(&p.pm, &p.b, p.nonce));
            let ev: VenueStateChanged = events::one(&meta);
            assert_eq!(ev.to, VenueKind::PumpMigrating as u8);
            p
        }
        Completion::Synthetic => {
            let mut p = with_position(NEAR);
            let q = p.q;
            complete_synthetically(&mut p.env, &p.pm.curve, q);
            p
        }
        Completion::Exact => {
            let mut p = with_position(NEAR);
            buy_down_to(&mut p.env, &p.pm.curve, 0);
            p
        }
    };
    let s = p.pm.curve.state(&p.env.svm);
    let synthetic = !matches!(how, Completion::Exact);
    assert!(
        s.complete && (s.post_complete_base_out > 0) == synthetic,
        "{how:?}: complete {}, pcB {}",
        s.complete,
        s.post_complete_base_out
    );
    let keeper = fixed(&mut p.env, KEEPER);
    let ix = pump::migrate_v2_ix(&p.env.svm, &keeper.pubkey(), &p.pm.curve);
    (p.env, with_cb(vec![ix]), vec![keeper])
}

fn tx_keeper_s3_none() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (p, keeper) = s3_none();
    let n = AsNone(&p.pm);
    let ixs = vec![
        crank_price_ix(&n),
        sol_settle_ix(&p.env, &n, &keeper.pubkey(), &p.b.key(), p.nonce),
    ];
    (p.env, with_cb(ixs), vec![keeper])
}

/// The keeper's latch duty (ARCH §7.1 duty 10): pump, pump-fees and PumpSwap upgraded in one
/// batch (one upgrade authority holds all three), flagged in one transaction.
fn tx_flag_three() -> (Env, Vec<Instruction>, Vec<Keypair>) {
    let (mut env, _) = Env::fork_pump_coin(T22);
    for venue in [GUARD_PUMP, GUARD_PUMP_FEES, GUARD_PUMP_AMM] {
        let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_pump_deploy_slot(&mut env.svm, pump_program(venue), dumped + 1);
    }
    let ixs = vec![
        ix::flag_venue_upgraded(GUARD_PUMP),
        ix::flag_venue_upgraded(GUARD_PUMP_FEES),
        ix::flag_venue_upgraded(GUARD_PUMP_AMM),
    ];
    (env, with_cb(ixs), vec![])
}

/// ARCH §6.1's limit for a curve open or close.
pub const CURVE_TRADE: u64 = 250_000;
/// ARCH §6.1's limit for a curve liquidation or expiry (and the keeper's `[crank, exit]`).
pub const CURVE_KEEPER: u64 = 300_000;
/// The keeper's limit for its top-level `migrate_v2` (`services/keeper/src/graduation.rs`
/// `MIGRATE_CU`; pump-final §6.6's row).
pub const MIGRATE_CU: u64 = 400_000;

/// The worst-case curve transactions (T-TRACE): each must stay ≤ 64 entries, reach its exact
/// expected count, fit its ARCH §6.1 CU limit, and fit a v0 transaction through the protocol
/// ALT (a keeper batch: v1).
pub fn worst_case_transactions() -> Vec<TraceTx> {
    let mut v: Vec<TraceTx> = Vec::new();
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + open_short, first open, normalising",
                label(c)
            ),
            arch: "2 + ≤ 15 (§6.2 curve `open_short` max)",
            expected: 2 + 16,
            peak: 3,
            v1: false,
            cu_limit: CURVE_TRADE,
            setup: Box::new(move || single_with_cb(open_normalising(c))),
        });
        v.push(TraceTx {
            name: format!("{}: ComputeBudget ×2 + close_short, normalising", label(c)),
            arch: "2 + ≤ 15 (§6.2 curve exit max)",
            expected: 2 + 12,
            peak: 3,
            v1: false,
            cu_limit: CURVE_TRADE,
            setup: Box::new(move || single_with_cb(close(c, true))),
        });
    }
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + the costliest open (first, normalising, creator change into an empty vault, the last buyback recipient)",
            label(NEAR)
        ),
        arch: "2 + ≤ 15 (§6.2 curve `open_short` max)",
        expected: 2 + 16 + NEAR_GROWTH,
        peak: 3,
        v1: false,
        cu_limit: CURVE_TRADE,
        setup: Box::new(|| single_with_cb(open_costliest())),
    });
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + the costliest close (normalising, creator change into an empty vault, the last buyback recipient)",
            label(NEAR)
        ),
        arch: "2 + ≤ 15 (§6.2 curve exit max)",
        expected: 2 + 12,
        peak: 3,
        v1: false,
        cu_limit: CURVE_TRADE,
        setup: Box::new(|| single_with_cb(close_costliest(NEAR))),
    });
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + completing close (`→ PumpMigrating` in the instruction)",
            label(NEAR)
        ),
        arch: "2 + ≤ 15 (§6.2 curve exit max)",
        expected: 2 + 11,
        peak: 3,
        v1: false,
        cu_limit: CURVE_TRADE,
        setup: Box::new(|| single_with_cb(close_completing())),
    });
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + synthetic close past rT (rT = q/2; `→ PumpMigrating` in the instruction)",
            label(NEAR)
        ),
        arch: "2 + ≤ 15 (§6.2 curve exit max; pump-final §6.6: a synthetic buy + 2)",
        expected: 2 + 12,
        peak: 3,
        v1: false,
        cu_limit: CURVE_TRADE,
        setup: Box::new(|| single_with_cb(close_synthetic(false))),
    });
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + [pump buy_v2, open_short normalising, pump sell_v2]",
                label(c)
            ),
            arch: "no row (an attacker's atomic sandwich, §8.2; v1: it does not fit v0 through the protocol ALT)",
            // A user's `buy_v2` is 7 entries (8 with a creator fee), its `sell_v2` 4.
            expected: 2 + if c == SPL { 7 } else { 8 } + 16 + 4,
            peak: 3,
            v1: true,
            cu_limit: 1_400_000,
            setup: Box::new(move || tx_sandwiched_open(c)),
        });
    }
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + five opens of one wallet (the most that fit: 2 + 13 + 1 + 4·12 = 64, the first sale growing the curve)",
            label(NEAR)
        ),
        arch: "no row (v1: five do not fit v0 through the protocol ALT; `t_trace::opens_per_v0_transaction`)",
        expected: 2 + 13 + NEAR_GROWTH + 4 * 12,
        peak: 3,
        v1: true,
        cu_limit: 1_400_000,
        setup: Box::new(|| opens_of_one_wallet(5)),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price ×4 on four curve markets, each just completed (`→ PumpMigrating`)"
            .into(),
        arch: "2 + 2k = 10 (§6.2, k = 4)",
        expected: 2 + 4 * 4,
        peak: 2,
        v1: false,
        cu_limit: 250_000,
        setup: Box::new(tx_crank_batch),
    });
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + liquidate (keeper)",
                label(c)
            ),
            arch: "2 + 2 + ≤ 15 on the curve (§6.2)",
            expected: 2 + 2 + 9,
            peak: 3,
            v1: false,
            cu_limit: CURVE_KEEPER,
            setup: Box::new(move || tx_keeper_liquidation(c, false)),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + expire (keeper)",
                label(c)
            ),
            arch: "2 + 2 + ≤ 15 on the curve (§6.2)",
            expected: 2 + 2 + 9,
            peak: 3,
            v1: false,
            cu_limit: CURVE_KEEPER,
            setup: Box::new(move || tx_keeper_expiry(c)),
        });
    }
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + crank_price + the costliest liquidate (keeper)",
            label(T22)
        ),
        arch: "2 + 2 + ≤ 15 on the curve (§6.2)",
        expected: 2 + 2 + 12,
        peak: 3,
        v1: false,
        cu_limit: CURVE_KEEPER,
        setup: Box::new(|| tx_keeper_liquidation(T22, true)),
    });
    for worst in [false, true] {
        v.push(TraceTx {
            name: format!(
                "ECON's D@80 %: ComputeBudget ×2 + crank_price + synthetic liquidate past rT (keeper){}",
                worst_ring(worst)
            ),
            arch: "2 + 2 + ≤ 15 on the curve (§6.2); pump-final §6.6: ≤ 245 k CU",
            expected: 2 + 2 + 12,
            peak: 3,
            v1: false,
            cu_limit: CURVE_KEEPER,
            setup: Box::new(move || tx_keeper_liquidation_synthetic(worst)),
        });
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + crank_price + synthetic expire past rT (keeper){}",
                label(NEAR),
                worst_ring(worst)
            ),
            arch: "2 + 2 + ≤ 15 on the curve (§6.2)",
            expected: 2 + 2 + 12,
            peak: 3,
            v1: false,
            cu_limit: CURVE_KEEPER,
            setup: Box::new(move || tx_keeper_expiry_synthetic(worst)),
        });
    }
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price + sol_settle S3 (keeper; `PumpMigrating`, 70 min)"
            .into(),
        arch: "2 + 2 + 2 (§6.2)",
        expected: 2 + 2 + 2,
        peak: 2,
        v1: false,
        cu_limit: 150_000,
        setup: Box::new(tx_keeper_s3_migrating),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price + sol_settle S3 (keeper; `PumpMigrating` after a synthetic completion, `pc ≠ 0`, 70 min)"
            .into(),
        arch: "2 + 2 + 2 (§6.2)",
        expected: 2 + 2 + 2,
        peak: 2,
        v1: false,
        cu_limit: 150_000,
        setup: Box::new(tx_keeper_s3_migrating_synthetic),
    });
    v.push(TraceTx {
        name: "ComputeBudget ×2 + crank_price + sol_settle S3 (keeper; `None`, detection set)"
            .into(),
        arch: "2 + 2 + 2 (§6.2)",
        expected: 2 + 2 + 2,
        peak: 2,
        v1: false,
        cu_limit: 150_000,
        setup: Box::new(tx_keeper_s3_none),
    });
    for how in [
        Completion::Exact,
        Completion::Synthetic,
        Completion::OwnClose,
    ] {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + migrate_v2 (keeper, top level, with the boost accounts) after {}",
                label(NEAR),
                match how {
                    Completion::Exact => "a third party's exact completion (`pc = 0`)",
                    Completion::Synthetic => "a third party's synthetic completion (`pc ≠ 0`)",
                    Completion::OwnClose => "short.win's own synthetic close (`pc ≠ 0`)",
                }
            ),
            arch: "51 + 2 = 53 (`migrate_v2` top level; peak 4, A15); the keeper's `MIGRATE_CU` (pump-final §6.6)",
            expected: 53,
            peak: 4,
            v1: false,
            cu_limit: MIGRATE_CU,
            setup: Box::new(move || tx_migrate_after(how)),
        });
    }
    v.push(TraceTx {
        name: "ComputeBudget ×2 + flag_venue_upgraded ×3 (pump, pump-fees, PumpSwap; the dumped ProgramData)"
            .into(),
        arch: "2 + 2 + 2 + 2 (§6.2)",
        expected: 2 + 2 * 3,
        peak: 2,
        v1: false,
        cu_limit: 400_000,
        setup: Box::new(tx_flag_three),
    });
    for c in VARIANTS {
        v.push(TraceTx {
            name: format!(
                "{}: ComputeBudget ×2 + create_market (Auto, `PumpCurve`, init set)",
                label(c)
            ),
            arch: "no row",
            expected: 2 + 15,
            peak: 3,
            v1: false,
            cu_limit: 250_000,
            setup: Box::new(move || single_with_cb(create_curve(c))),
        });
    }
    v.push(TraceTx {
        name: format!(
            "{}: ComputeBudget ×2 + sweep_trader (token leg)",
            label(T22)
        ),
        arch: "no row",
        expected: 2 + 4,
        peak: 2,
        v1: false,
        cu_limit: 60_000,
        setup: Box::new(|| single_with_cb(sweep(T22, true))),
    });
    v
}

// ---- stack depth ----------------------------------------------------------------------------

/// The TEST-ONLY wrapper program's id in the bench (`wrapper/`; never deployed): the 32 bytes of
/// `short.win-m5-cpi-wrapper-program`.
pub const WRAPPER_ID: Pubkey = Pubkey::new_from_array(*b"short.win-m5-cpi-wrapper-program");

/// The wrapper's SBF build: `$M5_WRAPPER_SO`, else `tests/bench/m5_cu/target/deploy/
/// m5_cpi_wrapper.so` (`cargo-build-sbf --tools-version v1.57 --arch v3 --manifest-path
/// tests/bench/m5_cu/wrapper/Cargo.toml --sbf-out-dir tests/bench/m5_cu/target/deploy`).
pub fn wrapper_so_path() -> PathBuf {
    std::env::var_os("M5_WRAPPER_SO").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/m5_cpi_wrapper.so"),
        PathBuf::from,
    )
}

/// Deploys the wrapper into `env`.
pub fn load_wrapper(env: &mut Env) {
    let path = wrapper_so_path();
    let so = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}. Build it: cargo-build-sbf --tools-version v1.57 --arch v3 \
             --manifest-path tests/bench/m5_cu/wrapper/Cargo.toml --sbf-out-dir \
             tests/bench/m5_cu/target/deploy (or set M5_WRAPPER_SO)",
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

/// The curve-path instructions through 0–3 wrapper levels: every curve leg peaks at 3
/// (short.win 1 → pump 2 → pump-fees / Token / System / pump's event 3), as does a curve listing
/// (short.win 1 → ATA program / pump 2 → Token / System / pump's event 3), so two wrapper levels
/// fit (peak 5 = `MAX_INSTRUCTION_STACK_DEPTH` on mainnet) and three do not; the instructions
/// without a venue CPI peak at 2 and fit three.
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
            case: label(c).into(),
            direct_peak: 3,
            setup: Box::new(move || close(c, false)),
        });
        v.push(DepthCase {
            handler: "liquidate",
            case: label(c).into(),
            direct_peak: 3,
            setup: Box::new(move || liquidate(c)),
        });
        v.push(DepthCase {
            handler: "expire",
            case: label(c).into(),
            direct_peak: 3,
            setup: Box::new(move || expire(c)),
        });
        v.push(DepthCase {
            handler: "create_market",
            case: format!("Auto, {}: `PumpCurve`, init set", label(c)),
            direct_peak: 3,
            setup: Box::new(move || create_curve(c)),
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
        case: format!(
            "{}: the costliest open (normalising, growth, the last buyback recipient)",
            label(NEAR)
        ),
        direct_peak: 3,
        setup: Box::new(open_costliest),
    });
    v.push(DepthCase {
        handler: "close_short",
        case: format!("{}: completing buyback (`→ PumpMigrating`)", label(NEAR)),
        direct_peak: 3,
        setup: Box::new(close_completing),
    });
    v.push(DepthCase {
        handler: "close_short",
        case: format!(
            "{}: synthetic past rT (`buy_v3(q)`, `→ PumpMigrating`)",
            label(NEAR)
        ),
        direct_peak: 3,
        setup: Box::new(|| close_synthetic(false)),
    });
    v.push(DepthCase {
        handler: "liquidate",
        case: "ECON's D@80 %: synthetic past rT, worst ring".into(),
        direct_peak: 3,
        setup: Box::new(|| liquidate_synthetic(true)),
    });
    v.push(DepthCase {
        handler: "expire",
        case: format!("{}: synthetic past rT, worst ring", label(NEAR)),
        direct_peak: 3,
        setup: Box::new(|| expire_synthetic(true)),
    });
    v.push(DepthCase {
        handler: "sol_settle",
        case: "`PumpMigrating`: S3 at 70 min".into(),
        direct_peak: 2,
        setup: Box::new(settle_s3_migrating),
    });
    v.push(DepthCase {
        handler: "sol_settle",
        case: "`PumpMigrating` after a synthetic completion (`pc ≠ 0`): S3 at 70 min".into(),
        direct_peak: 2,
        setup: Box::new(settle_s3_migrating_synthetic),
    });
    v.push(DepthCase {
        handler: "sol_settle",
        case: "`None` (graduated elsewhere): S3".into(),
        direct_peak: 2,
        setup: Box::new(settle_s3_none),
    });
    v.push(DepthCase {
        handler: "crank_price",
        case: format!("{}: `PumpCurve → PumpMigrating`", label(NEAR)),
        direct_peak: 2,
        setup: Box::new(crank_to_migrating),
    });
    v.push(DepthCase {
        handler: "crank_price",
        case: format!(
            "{}: `PumpCurve → PumpMigrating` after a synthetic completion (`pc ≠ 0`)",
            label(NEAR)
        ),
        direct_peak: 2,
        setup: Box::new(crank_to_migrating_synthetic),
    });
    v.push(DepthCase {
        handler: "crank_price",
        case: format!("{}: `None → PumpCurve` re-detection", label(T22)),
        direct_peak: 2,
        setup: Box::new(crank_redetect),
    });
    v.push(DepthCase {
        handler: "flag_venue_upgraded",
        case: "pump, the dumped ProgramData".into(),
        direct_peak: 2,
        setup: Box::new(|| flag_pump(GUARD_PUMP)),
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
