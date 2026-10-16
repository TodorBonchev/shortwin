//! Invariant checks run after **every** instruction (ARCH §8.3; M1 exit criteria: I1, I2, I4,
//! I8, I17).
//!
//! Hook API:
//! - [`check_all`]`(svm, market)`: the per-market state invariants (I1, I2, I8). [`crate::Env`]
//!   runs it for every tracked market after every transaction (and, for a transaction of
//!   several instructions, after each prefix of it on a clone of the VM).
//! - [`snapshot`] + [`check_ix`]: the instruction-level invariants that compare the state
//!   **just before an instruction** with the state just after it (I4, the lender bounds) or read
//!   **that instruction's** events and signers (I17, I4's "admin-signed", the lender bounds).
//!   [`crate::Env::send`] snapshots before the transaction and after each proper prefix (on a
//!   clone of the VM), so instruction `k` of a transaction is checked against the state after
//!   instructions `1..k−1`, never against the transaction's start (M1-FID-3, M1-FID-5): a
//!   latch set by an earlier instruction of the same transaction counts for a later open, and a
//!   lender's own withdrawal is not blamed on an admin who signed another instruction.
//!
//! Status (M2 foundation): each check implements what can be checked on the stored state; the
//! lender pool (I1 token claims in both directions, the whole of I8 on the state the program
//! sees, the instruction-level allowance, claim, conservation and lock bounds) is [`lender`];
//! the position engine's I3, I5–I7, I9, I10, I13, I16 are [`positions`] (I11, I12: the
//! liquidation / expiry / settlement tracks; I14: a structural test); Raydium pool selection's
//! I15 is [`pool_select`] (M4); the post-change confirmation's I18 is [`confirm`] (M4R).
//! Positions are in I2 and I4, opens in I17 (and, since
//! M3-SEC-2, listings on a venue with a latch mask; since M4, pool adoptions and fast-path
//! window candidates). Every check here works on **stored** values, which is sound where noted:
//! an untouched lender's stored `real` only lags its true value, so a stored-value upper bound
//! implies the true one.

use anchor_lang::AccountDeserialize;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use litesvm::LiteSVM;
use litesvm::types::TransactionMetadata;
use shortwin::events::{
    ClaimSol, Close, DeficitCovered, Deposit, Expire, LenderSynced, Liquidate, MarketCreated, Open,
    PoolProposed, PoolSwitchCancelled, PoolSwitched, SettleArmed, SettleDisarmed, SolSettle,
    VenueStateChanged, Withdraw,
};
use shortwin::state::{LenderPosition, Market, MarketVault, ShortPosition, VenueGuard, VenueKind};
use shortwin_math::pool::{R_MAX, VIRTUAL_ASSETS, VIRTUAL_SHARES};

use crate::{events, pda, read};

pub mod confirm;
pub mod lender;
pub mod open_terms;
pub mod pool_select;
pub mod positions;
pub mod venue_view;

/// A failed invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// `"I1"`, `"I2"`, …
    pub invariant: &'static str,
    /// What failed, with the values.
    pub detail: String,
}

impl core::fmt::Display for Violation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} violated: {}", self.invariant, self.detail)
    }
}

pub(crate) fn fail(invariant: &'static str, detail: String) -> Result<(), Violation> {
    Err(Violation { invariant, detail })
}

pub(crate) fn ensure(
    ok: bool,
    invariant: &'static str,
    detail: impl FnOnce() -> String,
) -> Result<(), Violation> {
    if ok {
        Ok(())
    } else {
        fail(invariant, detail())
    }
}

/// Every `LenderPosition` of `market`.
pub fn lenders_of(svm: &LiteSVM, market: &Pubkey) -> Vec<(Pubkey, LenderPosition)> {
    read::all_of::<LenderPosition>(svm)
        .into_iter()
        .filter_map(|(k, a)| {
            LenderPosition::try_deserialize(&mut a.data.as_slice())
                .ok()
                .filter(|l| l.market == *market)
                .map(|l| (k, l))
        })
        .collect()
}

/// Every `ShortPosition` of `market`, with its lamports.
pub fn positions_of(svm: &LiteSVM, market: &Pubkey) -> Vec<(Pubkey, ShortPosition, u64)> {
    read::all_of::<ShortPosition>(svm)
        .into_iter()
        .filter_map(|(k, a)| {
            ShortPosition::try_deserialize(&mut a.data.as_slice())
                .ok()
                .filter(|p| p.market == *market)
                .map(|p| (k, p, a.lamports))
        })
        .collect()
}

/// The per-market state invariants (I1, I2, I8). A market that does not exist passes.
pub fn check_all(svm: &LiteSVM, market: &Pubkey) -> Result<(), Violation> {
    let Some(m) = read::zc::<Market>(svm, market) else {
        return Ok(());
    };
    let Some(v) = read::zc::<MarketVault>(svm, &m.market_vault) else {
        return fail("I2", format!("market {market} has no MarketVault"));
    };
    i1_token_claims(svm, market, &m)?;
    i2_vault_lamports(svm, market, &m, &v)?;
    i8_lender_pool(svm, market, &m, &v)?;
    lender::check_market(svm, market, &m, &v)?;
    positions::check_market(svm, market, &m)?;
    Ok(())
}

/// I1: `T + ΣB_Q` = total lender token claims (SPEC §10); `pool_vault.amount ≥ T` (donations
/// ignored).
///
/// The claims side (Σ over the lenders on the current epoch of `⌊shares·(T + ΣB_Q + VA)/(S +
/// VS)⌋ ≤ T + ΣB_Q`, every lender touched) is [`lender::check_market`]'s.
pub fn i1_token_claims(svm: &LiteSVM, market: &Pubkey, m: &Market) -> Result<(), Violation> {
    let amount = read::token_amount(svm, &m.pool_vault).unwrap_or(0);
    ensure(amount >= m.tokens_free, "I1", || {
        format!(
            "market {market}: pool_vault.amount {amount} < T {}",
            m.tokens_free
        )
    })?;
    let by_tier: u128 = m.borrowed_q_by_tier.iter().map(|&q| u128::from(q)).sum();
    ensure(by_tier == u128::from(m.borrowed_q), "I1", || {
        format!(
            "market {market}: Σ borrowed_q_by_tier {by_tier} != ΣB_Q {}",
            m.borrowed_q
        )
    })
}

/// I2: `MarketVault.lamports ≥ rent_min + lender_sol + protocol_fees`; every `ShortPosition`
/// of the market has `lamports ≥ rent_min + v` (ARCH §2.3.1 A4).
pub fn i2_vault_lamports(
    svm: &LiteSVM,
    market: &Pubkey,
    m: &Market,
    v: &MarketVault,
) -> Result<(), Violation> {
    let lamports = read::lamports(svm, &m.market_vault);
    let rent = svm.minimum_balance_for_rent_exemption(MarketVault::SPACE);
    let need = u128::from(rent) + u128::from(v.lender_sol) + u128::from(v.protocol_fees);
    ensure(u128::from(lamports) >= need, "I2", || {
        format!(
            "market {market}: MarketVault lamports {lamports} < rent {rent} + lender_sol {} + \
             protocol_fees {}",
            v.lender_sol, v.protocol_fees
        )
    })?;
    let pos_rent = svm.minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    for (key, p, lamports) in positions_of(svm, market) {
        ensure(
            u128::from(lamports) >= u128::from(pos_rent) + u128::from(p.v),
            "I2",
            || {
                format!(
                    "position {key}: lamports {lamports} < rent {pos_rent} + v {}",
                    p.v
                )
            },
        )?;
    }
    Ok(())
}

/// I8 (= ECON I-P1–I-P7), the parts checkable on stored state:
/// - claim side, strict per lender: `claimed ≤ real`; `Σ(real − claimed) + unmatched + vest_bal
///   ≤ lender_sol` (stored `real` lags the true value, so this is implied by the true one);
/// - the calendar: `lock_norm = Σ buckets`;
/// - shares: `S + VS ≤ R_MAX·(T + ΣB_Q + VA)` or `S = 0`; Σ shares of the lenders on the
///   current epoch ≤ `S`;
/// - `receivable = 0` and `A_j = 0` with no open position.
///
/// The rest of I8 — the entitlement side after touching every lender, `Σ allowances ≤ T +
/// dust` with each lender touched alone (`pool::allowance_view`, T-POT) and with everyone
/// touched, no lock effective at or after its `lock_until`, the calendar against the live
/// locks, and the wind-down liveness — is [`lender::check_market`]'s.
pub fn i8_lender_pool(
    svm: &LiteSVM,
    market: &Pubkey,
    m: &Market,
    v: &MarketVault,
) -> Result<(), Violation> {
    let lenders = lenders_of(svm, market);
    let mut owed: u128 = 0;
    let mut current_shares: u128 = 0;
    for (key, l) in &lenders {
        ensure(l.claimed <= l.real, "I8", || {
            format!("lender {key}: claimed {} > real {}", l.claimed, l.real)
        })?;
        owed += u128::from(l.real - l.claimed);
        if l.epoch == m.share_epoch {
            current_shares += l.shares;
        }
    }
    let claim_side = owed + u128::from(v.unmatched) + u128::from(m.vest_bal);
    ensure(claim_side <= u128::from(v.lender_sol), "I8", || {
        format!(
            "market {market}: Σ(real − claimed) {owed} + unmatched {} + vest_bal {} > \
             lender_sol {}",
            v.unmatched, m.vest_bal, v.lender_sol
        )
    })?;
    let buckets: u128 = v.lock_cal.iter().map(|b| b.get()).sum();
    ensure(buckets == v.lock_norm.get(), "I8", || {
        format!(
            "market {market}: lock_norm {} != Σ calendar buckets {buckets}",
            v.lock_norm.get()
        )
    })?;
    let s = m.total_shares.get();
    let claims = u128::from(m.tokens_free) + u128::from(m.borrowed_q) + VIRTUAL_ASSETS;
    ensure(
        s == 0 || s.saturating_add(VIRTUAL_SHARES) <= R_MAX.saturating_mul(claims),
        "I8",
        || format!("market {market}: S {s} + VS > R_MAX·(T + ΣB_Q + VA) = R_MAX·{claims}"),
    )?;
    ensure(current_shares <= s, "I8", || {
        format!("market {market}: Σ current-epoch shares {current_shares} > S {s}")
    })?;
    if m.open_positions == 0 {
        let aggr_zero = m.rent_aggr.iter().all(|a| a.get() == 0);
        ensure(m.receivable == 0 && aggr_zero, "I8", || {
            format!(
                "market {market}: no open position but receivable {} / A_j {:?}",
                m.receivable, m.rent_aggr
            )
        })?;
    }
    Ok(())
}

/// One market's balances before a transaction (I4).
#[derive(Clone, Debug)]
pub struct MarketSnapshot {
    pub market: Pubkey,
    pub pool_vault_amount: u64,
    pub market_vault_lamports: u64,
    pub protocol_fees: u64,
    pub positions: Vec<(Pubkey, u64)>,
}

/// The state just before an instruction, which the instruction-level checks compare against
/// (named for the transaction because a one-instruction transaction's is its start).
#[derive(Clone)]
pub struct TxSnapshot {
    pub markets: Vec<MarketSnapshot>,
    pub guard: Option<VenueGuard>,
    /// PumpSwap `GlobalConfig.disable_flags` just before the instruction, if the account exists
    /// and parses (I17's M6-SEC-3 part: no curve open while bit 0, `create_pool`, is set).
    pub pamm_disable_flags: Option<u8>,
    pub fee_recipient: Option<(Pubkey, u64)>,
    /// The lender-pool state of the tracked markets ([`lender::check_ix`]).
    pub lender: lender::LenderSnapshot,
    /// The position-side state of the tracked markets ([`positions::check_ix`]).
    pub positions: positions::PositionsSnapshot,
    /// I4's coverage clause (O17): the instruction's possible signers — the installed
    /// `coverage_authority` and the Emergency vault, which may clear the flag (★OWNER-M7-3 (b),
    /// 2026-10-01) — and every account they could reach; `None` before `initialize_config`, and
    /// when no instruction of the transaction is `set_coverage_pause`, directly or through a
    /// Squads vault ([`snapshot_for`]).
    pub coverage: Option<CoverageSnapshot>,
}

/// The state `set_coverage_pause` could touch (I4's coverage clause, O17): every short.win
/// account and the tracked markets' pool vaults, raw.
#[derive(Clone)]
pub struct CoverageSnapshot {
    /// `GlobalConfig.coverage_authority` just before the instruction (`Pubkey::default()` while
    /// the role is inert).
    pub authority: Pubkey,
    /// `GlobalConfig.pause_authority` just before the instruction: the Emergency vault, which
    /// may **clear** a market's `paused_coverage` — with the role installed or not — and never
    /// set it (★OWNER-M7-3 (b), owner decision of 2026-10-01).
    pub emergency: Pubkey,
    /// `(key, lamports, data)`.
    pub accounts: Vec<(Pubkey, u64, Vec<u8>)>,
}

/// `Market.flags`' offset (discriminator included; bit 6 `paused_coverage` is in its low byte).
pub const MARKET_FLAGS_OFFSET: usize = 10;

/// `Market.coverage_cleared_at`'s offset (discriminator included; M7-SEC-3).
pub const MARKET_COVERAGE_CLEARED_AT_OFFSET: usize = 2_928;

/// [`CoverageSnapshot`] of `svm` once the config exists (the Emergency's clear is accepted
/// whether or not the role is installed, so the clause is never vacuous after
/// `initialize_config`).
fn coverage_snapshot(
    svm: &LiteSVM,
    config: Option<&shortwin::state::GlobalConfig>,
    markets: &[Pubkey],
) -> Option<CoverageSnapshot> {
    let c = config?;
    let (authority, emergency) = (c.coverage_authority, c.pause_authority);
    if authority == Pubkey::default() && emergency == Pubkey::default() {
        return None;
    }
    let mut accounts: Vec<(Pubkey, u64, Vec<u8>)> = svm
        .get_program_accounts(&shortwin::ID)
        .into_iter()
        .map(|(k, a)| (k, a.lamports, a.data))
        .collect();
    for market in markets {
        if let Some(m) = read::zc::<Market>(svm, market)
            && let Some(a) = svm.get_account(&m.pool_vault)
        {
            accounts.push((m.pool_vault, a.lamports, a.data));
        }
    }
    Some(CoverageSnapshot {
        authority,
        emergency,
        accounts,
    })
}

/// Snapshots `markets` (and the latch and the fee recipient) before an instruction.
pub fn snapshot(svm: &LiteSVM, markets: &[Pubkey]) -> TxSnapshot {
    snapshot_with(svm, markets, true)
}

/// [`snapshot`] before an instruction of a transaction of `ixs` ([`crate::Env::send`]'s hot path):
/// I4's coverage snapshot — every short.win account and tracked pool vault, raw — is taken only
/// when an instruction of `ixs` is `set_coverage_pause`, or a Squads `vault_transaction_execute`
/// whose `VaultTransaction` (already in `svm`) runs one ([`vault_coverage_pause`]): the only
/// instructions its clause judges ([`i4_coverage_authority_changes_only_its_flag`]), so nothing
/// it checks is skipped. (A vault transaction created by an earlier instruction of the same
/// transaction is seen by the snapshot [`crate::Env::send`] takes after that prefix.)
pub fn snapshot_for(svm: &LiteSVM, markets: &[Pubkey], ixs: &[Instruction]) -> TxSnapshot {
    let coverage = ixs
        .iter()
        .any(|ix| is_coverage_pause(ix) || vault_coverage_pause(svm, ix).is_some());
    snapshot_with(svm, markets, coverage)
}

/// The instructions a top-level Squads `vault_transaction_execute` `ix` runs, read from its
/// `VaultTransaction` account in `svm` and resolved against `ix`'s remaining accounts
/// ([`squads_v4_ix::state::VaultTransaction::executed_instructions`]: the executing vault and
/// any ephemeral signer are signers, as Squads makes them in its CPIs). `None` for any other
/// instruction, or when the account is missing, not Squads', does not decode or does not match
/// `ix`. Squads does not close a `VaultTransaction` on execution, so the state just after `ix`
/// reads the same.
pub fn vault_executed(svm: &LiteSVM, ix: &Instruction) -> Option<Vec<Instruction>> {
    use squads_v4_ix::ix::VAULT_TRANSACTION_EXECUTE_NAMED as NAMED;
    if !squads_v4_ix::ix::is_vault_transaction_execute(ix) {
        return None;
    }
    let transaction = svm.get_account(&ix.accounts.get(2)?.pubkey)?;
    (transaction.owner == squads_v4_ix::ID).then_some(())?;
    let t = squads_v4_ix::state::VaultTransaction::decode(&transaction.data)?;
    t.executed_instructions(ix.accounts.get(NAMED..)?)
}

/// I4's coverage clause on the vault path (M10 WP-7a): a top-level `vault_transaction_execute`
/// whose executed message runs short.win's `set_coverage_pause` — the Emergency's clear in the
/// owner's layout, where `pause_authority` is a Squads vault — is judged as that instruction:
/// `Some((markets, signers))`, the markets its `set_coverage_pause` instructions name and their
/// signer keys (the executing vault; not the member who sent the transaction, whom the inner
/// instruction never sees). The clause compares the state around the whole top-level
/// instruction, so a message that also runs another short.win instruction is judged as one
/// `set_coverage_pause` and fails it if that instruction changed anything: stage such actions as
/// separate vault transactions (the harness's `SquadsMs` never mixes them). `None` if `ix` runs
/// no `set_coverage_pause` ([`vault_executed`]).
pub fn vault_coverage_pause(svm: &LiteSVM, ix: &Instruction) -> Option<(Vec<Pubkey>, Vec<Pubkey>)> {
    let run = vault_executed(svm, ix)?;
    let pauses: Vec<&Instruction> = run.iter().filter(|i| is_coverage_pause(i)).collect();
    if pauses.is_empty() {
        return None;
    }
    let mut markets = Vec::new();
    let mut signers = Vec::new();
    for p in pauses {
        markets.extend(p.accounts.get(2).map(|m| m.pubkey));
        signers.extend(p.accounts.iter().filter(|m| m.is_signer).map(|m| m.pubkey));
    }
    Some((markets, signers))
}

/// `ix` is short.win's `set_coverage_pause` (I4's coverage clause).
pub fn is_coverage_pause(ix: &Instruction) -> bool {
    use anchor_lang::Discriminator;
    ix.program_id == shortwin::ID
        && ix
            .data
            .starts_with(shortwin::instruction::SetCoveragePause::DISCRIMINATOR)
}

fn snapshot_with(svm: &LiteSVM, markets: &[Pubkey], coverage: bool) -> TxSnapshot {
    let snaps = markets
        .iter()
        .filter_map(|market| {
            let m = read::zc::<Market>(svm, market)?;
            let v = read::zc::<MarketVault>(svm, &m.market_vault)?;
            Some(MarketSnapshot {
                market: *market,
                pool_vault_amount: read::token_amount(svm, &m.pool_vault).unwrap_or(0),
                market_vault_lamports: read::lamports(svm, &m.market_vault),
                protocol_fees: v.protocol_fees,
                positions: positions_of(svm, market)
                    .into_iter()
                    .map(|(k, _, lamports)| (k, lamports))
                    .collect(),
            })
        })
        .collect();
    let config = read::zc::<shortwin::state::GlobalConfig>(svm, &pda::config());
    TxSnapshot {
        markets: snaps,
        guard: read::zc::<VenueGuard>(svm, &pda::venue_guard()),
        pamm_disable_flags: svm
            .get_account(&shortwin::constants::PUMP_AMM_GLOBAL_CONFIG)
            .and_then(|a| {
                shortwin_venue_state::pump_amm::read_global_config(&a.owner.to_bytes(), &a.data)
                    .ok()
                    .map(|g| g.disable_flags())
            }),
        fee_recipient: config.map(|c| (c.fee_recipient, read::lamports(svm, &c.fee_recipient))),
        lender: lender::snapshot(svm, markets),
        positions: positions::snapshot(svm, markets),
        coverage: if coverage {
            coverage_snapshot(svm, config.as_ref(), markets)
        } else {
            None
        },
    }
}

/// The events of one top-level instruction that the instruction-level checks read (and, for
/// I10, its account keys).
#[derive(Default)]
pub struct IxEvents {
    pub withdraws: Vec<Withdraw>,
    pub claims: Vec<ClaimSol>,
    pub deposits: Vec<Deposit>,
    pub covers: Vec<DeficitCovered>,
    pub syncs: Vec<LenderSynced>,
    pub opens: Vec<Open>,
    pub closes: Vec<Close>,
    pub liquidations: Vec<Liquidate>,
    pub expiries: Vec<Expire>,
    pub settlements: Vec<SolSettle>,
    /// Settlement-clock changes (I18's gate, M4R).
    pub armed: Vec<SettleArmed>,
    pub disarmed: Vec<SettleDisarmed>,
    /// Listings (I17's M3-SEC-2 extension).
    pub created: Vec<MarketCreated>,
    /// Pool selection (M4; I15, and I17 on adoptions): adoptions, fast-path windows opened or
    /// improved, windows cancelled.
    pub switched: Vec<PoolSwitched>,
    pub proposed: Vec<PoolProposed>,
    pub cancelled: Vec<PoolSwitchCancelled>,
    /// Venue transitions (M5: a curve's completion, graduation elsewhere, `crank_price`'s
    /// `None →` pump re-detection, which clears a collection window, I15).
    pub venue_changed: Vec<VenueStateChanged>,
    /// The instruction's account keys (empty when unknown).
    pub accounts: Vec<Pubkey>,
    /// The instruction's signers (empty when unknown; I4's coverage clause). For a Squads
    /// `vault_transaction_execute` that runs `set_coverage_pause`, that inner instruction's
    /// signers ([`IxEvents::of_instruction_in`]).
    pub signers: Vec<Pubkey>,
    /// The instruction is short.win's `set_coverage_pause`, directly or through a Squads vault
    /// (I4's coverage clause).
    pub coverage_pause: bool,
    /// The markets that `set_coverage_pause` names (its third account; one per executed
    /// instruction on the vault path).
    pub coverage_markets: Vec<Pubkey>,
}

impl IxEvents {
    /// The events top-level instruction `ix` (0-based) of the transaction emitted.
    pub fn of(meta: &TransactionMetadata, ix: usize) -> Self {
        Self {
            withdraws: events::of_ix(meta, ix),
            claims: events::of_ix(meta, ix),
            deposits: events::of_ix(meta, ix),
            covers: events::of_ix(meta, ix),
            syncs: events::of_ix(meta, ix),
            opens: events::of_ix(meta, ix),
            closes: events::of_ix(meta, ix),
            liquidations: events::of_ix(meta, ix),
            expiries: events::of_ix(meta, ix),
            settlements: events::of_ix(meta, ix),
            armed: events::of_ix(meta, ix),
            disarmed: events::of_ix(meta, ix),
            created: events::of_ix(meta, ix),
            switched: events::of_ix(meta, ix),
            proposed: events::of_ix(meta, ix),
            cancelled: events::of_ix(meta, ix),
            venue_changed: events::of_ix(meta, ix),
            accounts: Vec::new(),
            signers: Vec::new(),
            coverage_pause: false,
            coverage_markets: Vec::new(),
        }
    }

    /// [`IxEvents::of`] with the instruction itself (its account keys, for I10).
    pub fn of_instruction(
        meta: &TransactionMetadata,
        ix: usize,
        instruction: &Instruction,
    ) -> Self {
        Self {
            accounts: instruction.accounts.iter().map(|m| m.pubkey).collect(),
            signers: instruction
                .accounts
                .iter()
                .filter(|m| m.is_signer)
                .map(|m| m.pubkey)
                .collect(),
            coverage_pause: is_coverage_pause(instruction),
            coverage_markets: if is_coverage_pause(instruction) {
                instruction
                    .accounts
                    .get(2)
                    .map(|m| m.pubkey)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            },
            ..Self::of(meta, ix)
        }
    }

    /// [`IxEvents::of_instruction`], with I4's vault path ([`crate::Env::send`]): a top-level
    /// Squads `vault_transaction_execute` that runs `set_coverage_pause` (read from its
    /// `VaultTransaction` in `svm`, [`vault_coverage_pause`]) is that instruction for the
    /// coverage clause — its markets, and the vault as its signer.
    pub fn of_instruction_in(
        svm: &LiteSVM,
        meta: &TransactionMetadata,
        ix: usize,
        instruction: &Instruction,
    ) -> Self {
        let ev = Self::of_instruction(meta, ix, instruction);
        match vault_coverage_pause(svm, instruction) {
            Some((markets, signers)) => Self {
                coverage_pause: true,
                coverage_markets: markets,
                signers,
                ..ev
            },
            None => ev,
        }
    }

    fn position_facts(&self) -> positions::IxFacts<'_> {
        positions::IxFacts {
            opens: &self.opens,
            closes: &self.closes,
            liquidations: &self.liquidations,
            expiries: &self.expiries,
            settlements: &self.settlements,
            switched: &self.switched,
            venue_changed: &self.venue_changed,
            accounts: &self.accounts,
        }
    }

    fn confirm_facts(&self) -> confirm::ConfirmFacts<'_> {
        confirm::ConfirmFacts {
            created: &self.created,
            switched: &self.switched,
            venue_changed: &self.venue_changed,
            opens: &self.opens,
            liquidations: &self.liquidations,
            expiries: &self.expiries,
            settlements: &self.settlements,
            armed: &self.armed,
            disarmed: &self.disarmed,
        }
    }

    fn pool_facts(&self) -> pool_select::PoolFacts<'_> {
        pool_select::PoolFacts {
            created: &self.created,
            opens: &self.opens,
            switched: &self.switched,
            proposed: &self.proposed,
            cancelled: &self.cancelled,
            venue_changed: &self.venue_changed,
        }
    }
}

/// `true` if one of `admins` signs `ix` itself (a signer meta of that instruction; I4 is decided
/// per instruction, M1-FID-5: an admin co-signing another instruction of the transaction
/// does not make a lender's own withdrawal "admin-signed").
///
/// **The vault path (M10 WP-7).** The authorities are Squads vaults: a PDA never signs the
/// transaction; Squads' `vault_transaction_execute` signs for it in the CPIs it makes, and
/// passes it to the top-level instruction as a non-signer remaining account. So a top-level
/// `vault_transaction_execute` that carries an admin key among its remaining accounts is
/// admin-signed too (conservatively: any admin key in its message, not only the executing
/// vault — at worst a stricter check). Everything that instruction executes is checked
/// against the state just before it.
pub fn is_admin_signed(ix: &Instruction, admins: &[Pubkey]) -> bool {
    ix.accounts
        .iter()
        .any(|m| m.is_signer && admins.contains(&m.pubkey))
        || is_admin_vault_execute(ix, admins)
}

/// `true` if `ix` is a Squads `vault_transaction_execute` whose remaining accounts (the
/// executed message's keys, after the four named accounts) include one of `admins`
/// ([`is_admin_signed`]'s vault path).
pub fn is_admin_vault_execute(ix: &Instruction, admins: &[Pubkey]) -> bool {
    squads_v4_ix::ix::is_vault_transaction_execute(ix)
        && ix
            .accounts
            .iter()
            .skip(squads_v4_ix::ix::VAULT_TRANSACTION_EXECUTE_NAMED)
            .any(|m| admins.contains(&m.pubkey))
}

/// The instruction-level invariants (I4, I17, and the lender bounds of [`lender::check_ix`])
/// for one instruction: `pre` is the state just before it, `svm` the state just after it.
/// `admin_signed`: an admin key (upgrade authority, governance, Emergency, coverage
/// authority) signs this instruction ([`is_admin_signed`]).
pub fn check_ix(
    svm: &LiteSVM,
    pre: &TxSnapshot,
    admin_signed: bool,
    ev: &IxEvents,
) -> Result<(), Violation> {
    if admin_signed {
        i4_admin_moves_no_funds(svm, pre)?;
    }
    i4_coverage_authority_changes_only_its_flag(svm, pre, ev)?;
    i17_no_open_on_flagged_venue(pre, &ev.opens)?;
    i17_no_curve_open_while_graduation_halted(pre, &ev.opens)?;
    i17_no_listing_on_flagged_venue(pre, &ev.created)?;
    i17_no_adoption_on_flagged_venue(svm, pre, &ev.switched, &ev.proposed)?;
    positions::check_ix(svm, &pre.positions, &ev.position_facts())?;
    pool_select::check_ix(svm, &pre.positions, &ev.pool_facts())?;
    confirm::check_ix(svm, &pre.positions, &ev.confirm_facts())?;
    lender::check_ix(svm, &pre.lender, ev)
}

/// I4's coverage clause (O17; ARCH §8.3: "`coverage_authority` can change only
/// `paused_coverage`"; ★OWNER-M7-3 (b), owner decision of 2026-10-01: "`pause_authority` can
/// change it too, but only by clearing a set flag"): a landed `set_coverage_pause` is signed by
/// the installed `coverage_authority` or the Emergency vault (`pause_authority`) — no other key
/// — and leaves every short.win account and every tracked market's `pool_vault` byte for byte and
/// lamport for lamport as it was, except bit 6 of the named market's `flags` and — when it
/// cleared a set flag, and only then — the market's `coverage_cleared_at`, which it sets to the
/// clock's second (M7-SEC-3: the clear delay, after either signer's clear). Signed by the
/// Emergency alone, the flag is clear afterwards (it never sets one). (The coverage key's only
/// power is that instruction — no other instruction reads `config.coverage_authority` — and the
/// general I4 clause applies to anything else it signs, as to every admin key; the Emergency's
/// other powers are §3.1's.)
pub fn i4_coverage_authority_changes_only_its_flag(
    svm: &LiteSVM,
    pre: &TxSnapshot,
    ev: &IxEvents,
) -> Result<(), Violation> {
    let Some(cov) = &pre.coverage else {
        return Ok(());
    };
    if !ev.coverage_pause {
        return Ok(());
    }
    let by_role = cov.authority != Pubkey::default() && ev.signers.contains(&cov.authority);
    let by_emergency = cov.emergency != Pubkey::default() && ev.signers.contains(&cov.emergency);
    ensure(by_role || by_emergency, "I4", || {
        format!(
            "set_coverage_pause landed signed by {:?}: neither the installed coverage_authority \
             ({}) nor the Emergency ({})",
            ev.signers, cov.authority, cov.emergency
        )
    })?;
    let bit = u8::try_from(shortwin::state::MARKET_FLAG_PAUSED_COVERAGE).unwrap_or(0);
    for (key, lamports, data) in &cov.accounts {
        let (l, d) = svm
            .get_account(key)
            .map_or((0, Vec::new()), |a| (a.lamports, a.data));
        ensure(l == *lamports, "I4", || {
            format!("set_coverage_pause changed the lamports of {key}: {lamports} → {l}")
        })?;
        if ev.coverage_markets.contains(key) {
            ensure(d.len() == data.len(), "I4", || {
                format!("set_coverage_pause resized market {key}")
            })?;
            let at = MARKET_COVERAGE_CLEARED_AT_OFFSET..MARKET_COVERAGE_CLEARED_AT_OFFSET + 8;
            for (i, (a, b)) in data.iter().zip(&d).enumerate() {
                let mask = if i == MARKET_FLAGS_OFFSET {
                    !bit
                } else if at.contains(&i) {
                    0
                } else {
                    0xff
                };
                ensure(a & mask == b & mask, "I4", || {
                    format!(
                        "set_coverage_pause changed byte {i} of market {key} ({a:#04x} → \
                         {b:#04x}); only paused_coverage (and coverage_cleared_at) may change"
                    )
                })?;
            }
            // M7-SEC-3: `coverage_cleared_at` moves only to the clock's second, and only with a
            // clear of a set flag.
            let was_set = data.get(MARKET_FLAGS_OFFSET).is_some_and(|f| f & bit != 0);
            let now_set = d.get(MARKET_FLAGS_OFFSET).is_some_and(|f| f & bit != 0);
            let word = |v: &[u8]| {
                v.get(at.clone())
                    .and_then(|s| <[u8; 8]>::try_from(s).ok())
                    .map(i64::from_le_bytes)
            };
            let (t0, t1) = (word(data), word(&d));
            let now = svm
                .get_sysvar::<anchor_lang::solana_program::clock::Clock>()
                .unix_timestamp;
            let cleared = was_set && !now_set;
            ensure(
                if cleared { t1 == Some(now) } else { t0 == t1 },
                "I4",
                || {
                    format!(
                        "set_coverage_pause on market {key}: flag {was_set} → {now_set}, \
                         coverage_cleared_at {t0:?} → {t1:?} at clock {now}; only a clear of a \
                         set flag records the clock, and it must"
                    )
                },
            )?;
            // ★OWNER-M7-3 (b): the Emergency only clears.
            ensure(by_role || !now_set, "I4", || {
                format!(
                    "set_coverage_pause signed by the Emergency alone left market {key}'s \
                     paused_coverage set ({was_set} → {now_set}); it may only clear it"
                )
            })?;
        } else {
            ensure(d == *data, "I4", || {
                format!("set_coverage_pause changed account {key}")
            })?;
        }
    }
    Ok(())
}

/// I4: no admin-signed instruction decreases `pool_vault`, `ShortPosition` or `MarketVault`
/// balances, except `sweep_protocol_fees` by ≤ `protocol_fees` to `config.fee_recipient`.
/// (No instruction can change a venue program id: they are compile-time constants, C0-8.) The
/// coverage clause is [`i4_coverage_authority_changes_only_its_flag`].
pub fn i4_admin_moves_no_funds(svm: &LiteSVM, pre: &TxSnapshot) -> Result<(), Violation> {
    let recipient_gain = pre
        .fee_recipient
        .map(|(k, before)| read::lamports(svm, &k).saturating_sub(before))
        .unwrap_or(0);
    for s in &pre.markets {
        let Some(m) = read::zc::<Market>(svm, &s.market) else {
            continue;
        };
        let amount = read::token_amount(svm, &m.pool_vault).unwrap_or(0);
        ensure(amount >= s.pool_vault_amount, "I4", || {
            format!(
                "admin-signed tx decreased pool_vault of {} from {} to {amount}",
                s.market, s.pool_vault_amount
            )
        })?;
        let lamports = read::lamports(svm, &m.market_vault);
        let drop = s.market_vault_lamports.saturating_sub(lamports);
        ensure(
            drop == 0 || (drop <= s.protocol_fees && drop <= recipient_gain),
            "I4",
            || {
                format!(
                    "admin-signed tx took {drop} lamports from MarketVault of {} (protocol fees \
                     {}, recipient gained {recipient_gain})",
                    s.market, s.protocol_fees
                )
            },
        )?;
        for (key, before) in &s.positions {
            let after = read::lamports(svm, key);
            ensure(after >= *before || after == 0, "I4", || {
                format!("admin-signed tx decreased position {key} from {before} to {after}")
            })?;
        }
    }
    Ok(())
}

/// I17: no open is accepted while `venue_guard.upgraded` has a bit of the market's venue mask
/// set (A7; masks v0.4.1). Reads one instruction's `Open` events against the latch **just
/// before that instruction** (so `[flag_venue_upgraded, open_short]` in one transaction is
/// checked against the flagged latch, and `[revalidate_venue, open_short]` against the cleared
/// one; M1-FID-3); vacuous until `open_short` exists (M2).
pub fn i17_no_open_on_flagged_venue(pre: &TxSnapshot, opens: &[Open]) -> Result<(), Violation> {
    let Some(guard) = pre.guard else {
        return Ok(());
    };
    for open in opens {
        // The event's venue, and the market's venue just before the open (they must agree).
        let before = pre
            .positions
            .markets
            .iter()
            .find(|m| m.market == open.market)
            .map(|m| m.state.venue);
        if let Some(v) = before {
            ensure(v == open.venue, "I17", || {
                format!(
                    "open {} reports venue {} on a market whose venue was {v}",
                    open.position, open.venue
                )
            })?;
        }
        let mask = VenueKind::from_u8(open.venue).map_or(0, VenueKind::guard_mask);
        ensure(!guard.blocks(mask), "I17", || {
            format!(
                "open {} accepted on venue {} while VenueGuard.upgraded = {:#b}",
                open.position, open.venue, guard.upgraded
            )
        })?;
    }
    Ok(())
}

/// I17, extended by M6-SEC-3 (owner decision of 2026-09-30; ARCH §3.4 step 3): no open on a
/// pump curve (`Open.venue == PumpCurve`) while PumpSwap's `GlobalConfig.disable_flags` bit 0
/// (`create_pool`, and with it pump's `migrate_v2`) is set just before the instruction — the
/// curve could not graduate (`GraduationHalted`). Vacuous where the `GlobalConfig` is absent (no
/// pump fork).
pub fn i17_no_curve_open_while_graduation_halted(
    pre: &TxSnapshot,
    opens: &[Open],
) -> Result<(), Violation> {
    let Some(flags) = pre.pamm_disable_flags else {
        return Ok(());
    };
    let halted = flags & shortwin::venues::pump_amm::PAMM_CREATE_POOL_DISABLED_BIT != 0;
    for open in opens {
        ensure(
            !(halted && open.venue == VenueKind::PumpCurve as u8),
            "I17",
            || {
                format!(
                    "curve open {} accepted while PumpSwap's disable_flags = {flags:#010b} halts \
                     create_pool (M6-SEC-3)",
                    open.position
                )
            },
        )?;
    }
    Ok(())
}

/// I17, extended by M3-SEC-2 (beyond ARCH §8.3's wording, which names opens only): no market is
/// listed on a venue whose mask is flagged in `venue_guard.upgraded` just before the listing's
/// instruction — a Raydium listing fixes its pool, SOL side and `graduated_at` with the
/// compiled readers. A list-only (`None`) listing has an empty mask and is never latched.
pub fn i17_no_listing_on_flagged_venue(
    pre: &TxSnapshot,
    created: &[MarketCreated],
) -> Result<(), Violation> {
    let Some(guard) = pre.guard else {
        return Ok(());
    };
    for c in created {
        let mask = VenueKind::from_u8(c.venue).map_or(0, VenueKind::guard_mask);
        ensure(!guard.blocks(mask), "I17", || {
            format!(
                "market {} listed on venue {} while VenueGuard.upgraded = {:#b}",
                c.market, c.venue, guard.upgraded
            )
        })?;
    }
    Ok(())
}

/// I17, extended by M4 (as the listings of M3-SEC-2; beyond ARCH §8.3's wording): no Raydium
/// pool is adopted (`PoolSwitched`) and no fast-path window records a candidate
/// (`PoolProposed`) while **the candidate's own program** is flagged in `venue_guard.upgraded`
/// just before the instruction — the candidate is verified with that program's compiled reader
/// and an adoption fixes the pool and its SOL side (M4-SEC-8: the other Raydium program's flag
/// does not hold up the fast path; a pool whose owner is neither program is checked against
/// the whole Raydium mask). A cancelled window (`PoolSwitchCancelled`) adopts nothing and is
/// not latched; a window whose candidate's program is flagged is cancelled (M4V-4).
pub fn i17_no_adoption_on_flagged_venue(
    svm: &LiteSVM,
    pre: &TxSnapshot,
    switched: &[PoolSwitched],
    proposed: &[PoolProposed],
) -> Result<(), Violation> {
    let Some(guard) = pre.guard else {
        return Ok(());
    };
    let pools = switched
        .iter()
        .map(|e| (e.market, e.candidate))
        .chain(proposed.iter().map(|e| (e.market, e.candidate)));
    for (market, pool) in pools {
        let owner = svm.get_account(&pool).map(|a| a.owner);
        let mask = match owner {
            Some(o) if o == shortwin::constants::RAYDIUM_V4_PROGRAM_ID => {
                1u32 << shortwin::constants::GUARD_RAYDIUM_V4
            }
            Some(o) if o == shortwin::constants::RAYDIUM_CPMM_PROGRAM_ID => {
                1u32 << shortwin::constants::GUARD_RAYDIUM_CPMM
            }
            _ => VenueKind::RaydiumV4.guard_mask(),
        };
        ensure(!guard.blocks(mask), "I17", || {
            format!(
                "market {market} adopted or proposed Raydium pool {pool} (owner {owner:?}) while \
                 VenueGuard.upgraded = {:#b}",
                guard.upgraded
            )
        })?;
    }
    Ok(())
}
