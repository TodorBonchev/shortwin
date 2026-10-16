//! Program errors (ARCH §3.11): one `#[error_code]` enum (Anchor 1.x allows one per program
//! [R-TOOL §1.3]).
//!
//! **Codes are stable.** Anchor numbers the variants from 6000 in declaration order, so the
//! groups below follow ARCH §3.11 exactly and new codes are appended at the end (the M1
//! foundation's temporary `NotImplemented` = 6091 was removed with the last M1 stub before any
//! deployment; the M1 review's codes 6091–6098 follow, each naming its ARCH §3.11 group; M2's
//! temporary `NotImplemented` = 6099 was likewise removed when the last M2 track landed, before
//! any deployment, and M2's `UseLiquidate` took 6099; M4 appended `PoolWindowClosed` = 6100, M4R
//! `EmaWarmingUp` = 6101, "switch only if all fit" `CarriedPositionsTooLarge` = 6102, and FIT-3's
//! `PositionScanOutOfOrder` = 6103, which is **reserved** since its instruction `refresh_max_q`
//! was removed with the keeper's refresh duty, before any deployment: `ReservedScanOutOfOrder`
//! keeps the slot so no later code moves, and nothing returns it; the owner's M6-SEC-3 decision
//! of 2026-09-30 appended `GraduationHalted` = 6104, and M10's SQ-G8 (owner decision of
//! 2026-10-02) `AuthoritiesNotDistinct` = 6105; M10's KI-65 work `EpochRecordReadOnly` = 6106
//! and `RebaseNeedsOwner` = 6107; AIR-07's residual `PoolDrained` = 6108; pump's v3 adapter,
//! owner decision D1 of 2026-10-13, `CurveCashbackCoin` = 6109).
//! `tests::codes_are_stable` pins every code.

use anchor_lang::prelude::*;
use shortwin_math::MathError;

/// Every error short.win returns.
#[error_code]
#[derive(PartialEq, Eq)]
pub enum ShortwinError {
    // ---- Math / accounts (6000–6007) ----
    #[msg("Arithmetic overflow, underflow or an undefined value")]
    MathOverflow,
    #[msg("An account does not match the key, owner or state the instruction expects")]
    AccountMismatch,
    #[msg("A venue account does not match its compile-time, cached or re-derived key")]
    VenueAccountMismatch,
    #[msg("A protocol account was passed where it is denied (e.g. as a venue extra)")]
    DeniedAccount,
    #[msg("Remaining accounts are missing, extra or out of order")]
    BadRemainingAccounts,
    #[msg("A venue extras set or entry is invalid")]
    BadExtras,
    #[msg("Account layout version is not the one this program expects")]
    ConfigVersion,
    #[msg("A ShareEpoch record does not match the market or epoch")]
    EpochRecordMismatch,

    // ---- Admin (6008–6015) ----
    #[msg("Signer is not the authority this instruction requires")]
    Unauthorized,
    #[msg("The pending config change is still timelocked")]
    TimelockNotElapsed,
    #[msg("There is no pending config change")]
    NoPendingChange,
    #[msg("A parameter is outside its compiled bounds or fails a relational check")]
    InvalidParams,
    #[msg("Timelock is below the compiled minimum")]
    TimelockTooShort,
    #[msg("The venue program's deploy slot has not changed")]
    VenueNotUpgraded,
    #[msg("The venue program's deploy slot is not the expected slot")]
    VenueSlotMismatch,
    #[msg("The coverage authority is disabled")]
    CoverageAuthorityDisabled,

    // ---- Mint (6016–6022) ----
    #[msg("Mint is not owned by SPL Token or Token-2022")]
    MintUnsupportedProgram,
    #[msg("Native (wrapped SOL) mints cannot be listed")]
    MintNative,
    #[msg("Mint has an active freeze authority")]
    MintFreezeAuthority,
    #[msg("Mint carries a Token-2022 extension that is not allowlisted (or unknown)")]
    MintDisallowedExtension,
    #[msg("Mint account data is malformed or uninitialized")]
    MintMalformed,
    #[msg("Mint passes the safety filter")]
    MintSafe,
    #[msg("Mint has been flagged unsafe")]
    MintUnsafeFlagged,

    // ---- Venue (6023–6041) ----
    #[msg("Market has no supported venue")]
    NoSupportedVenue,
    #[msg("Pump venues take precedence for this mint")]
    PumpVenuePrecedence,
    #[msg("Mayhem-mode curves and pools are not supported")]
    CurveMayhemUnsupported,
    #[msg("Curve quote mint is not SOL")]
    CurveQuoteNotSol,
    #[msg("Bonding curve account must be extended first")]
    CurveNeedsExtend,
    #[msg("Venue is migrating")]
    VenueMigrating,
    #[msg("No executable route")]
    NoRoute,
    #[msg("Route is flagged broken")]
    RouteBroken,
    #[msg("Buyback exceeds what the curve and its post-migration pool can deliver")]
    CurveStraddle,
    #[msg("Venue is disabled for opens")]
    VenueDisabled,
    #[msg("A venue program was upgraded and not yet re-validated")]
    VenueUpgraded,
    #[msg("Venue is flagged")]
    VenueFlagged,
    #[msg("Venue fee is above the cap")]
    VenueFeeTooHigh,
    #[msg("PumpSwap boost in progress")]
    BoostInProgress,
    #[msg("Venue result does not match the quote or the expected deltas")]
    VenueResultMismatch,
    #[msg("Venue took rent on a sell beyond the tolerance")]
    VenueRentOnSell,
    #[msg("Trader state is invalid")]
    TraderStateInvalid,
    #[msg("Pool vault delegate was not cleared")]
    DelegateNotCleared,
    #[msg("An account authority was tampered with")]
    AuthorityTampered,

    // ---- Pool selection (6042–6049) ----
    #[msg("Pool is not eligible")]
    PoolIneligible,
    #[msg("Candidate pool does not dominate the incumbent")]
    PoolNotDominant,
    #[msg("Market has open positions")]
    PositionsOpen,
    #[msg("Pool switch is not due")]
    PoolSwitchNotDue,
    #[msg("No pending pool switch")]
    NoPendingSwitch,
    #[msg("Opens wait for the pool listing window")]
    PoolListWindow,
    // No longer returned since M4-SEC-1 (eligibility tests the locked depth, not `λ_now`);
    // kept so every later code keeps its number.
    #[msg("Locked LP share is too low")]
    PoolLockTooLow,
    #[msg("Pool is too thin")]
    PoolTooThin,

    // ---- Opens (6050–6065) ----
    #[msg("Opens are paused")]
    OpensPaused,
    #[msg("Opens are paused for keeper coverage")]
    CoveragePaused,
    #[msg("Market is warming up")]
    MarketWarmingUp,
    #[msg("Spot price deviates from the EMA")]
    PriceDeviatesFromEma,
    #[msg("EMA price is above the open maximum")]
    PriceAboveOpenMax,
    #[msg("Position impact cap exceeded")]
    CapPositionImpact,
    #[msg("Market open-interest cap exceeded")]
    CapMarketOi,
    #[msg("Utilisation cap exceeded")]
    CapUtilization,
    #[msg("Curve gamma cap exceeded")]
    CapCurveGamma,
    #[msg("Wallet cap exceeded")]
    CapWallet,
    #[msg("Insufficient pool inventory")]
    InsufficientInventory,
    #[msg("Notional below the minimum")]
    BelowMinNotional,
    #[msg("Market notional cap exceeded")]
    MarketNotionalCap,
    #[msg("Sale proceeds below the minimum")]
    ProceedsBelowMin,
    #[msg("Margin above the maximum")]
    MarginAboveMax,
    #[msg("Open fee above the maximum")]
    OpenFeeAboveMax,

    // ---- Exits (6066–6075) ----
    #[msg("Vault SOL cannot cover the buyback, rent and tolerance")]
    InsufficientVault,
    #[msg("Buyback cost above the maximum")]
    BuybackAboveMax,
    #[msg("Position is not liquidatable")]
    NotLiquidatable,
    #[msg("Normal buyback not allowed; use sol_settle")]
    UseSolSettle,
    #[msg("Position is expired but not CR-liquidatable; use expire")]
    UseExpire,
    #[msg("Position is not expired")]
    NotExpired,
    #[msg("No fresh completed observation bucket")]
    ReferenceStale,
    #[msg("Live buyback cost exceeds the exit bound")]
    ExitBoundNotMet,
    #[msg("SOL settlement is not allowed")]
    SettlementNotAllowed,
    #[msg("S3 timer is still running")]
    S3TimerRunning,

    // ---- Lenders (6076–6084) ----
    #[msg("Amount is zero")]
    ZeroAmount,
    #[msg("Shares minted below the minimum")]
    SharesBelowMin,
    #[msg("Tokens withdrawn below the minimum")]
    TokensBelowMin,
    #[msg("Insufficient shares")]
    InsufficientShares,
    #[msg("Insufficient free (unborrowed) inventory")]
    InsufficientFreeInventory,
    #[msg("Withdrawal above the pro-rata allowance")]
    AboveProRataAllowance,
    #[msg("Lender SOL balance is negative; cover the deficit first")]
    NegativePending,
    #[msg("Lender is behind the current share epoch; pass the ShareEpoch records or sync_lender")]
    LenderBehindEpochs,
    #[msg("Deposit waits for the post-wipe sweep and reward claim")]
    WipeSweepPending,

    // ---- Oracle (6085–6090) ----
    #[msg("Oracle account owner is not the Pyth receiver")]
    OracleOwner,
    #[msg("Oracle account or feed id mismatch")]
    OracleFeed,
    #[msg("Oracle price is stale or not newer than the cache")]
    OracleStale,
    #[msg("Oracle confidence too wide")]
    OracleConfidence,
    #[msg("Oracle price step too large")]
    OracleStep,
    #[msg("Oracle update is not fully verified")]
    OracleVerification,

    // ---- Appended after the M1 review (6091–6098); ARCH §3.11 groups in brackets ----
    /// [Lenders] `withdraw`: the SOL part is below the caller's `min_sol` (M1-SEC-2).
    #[msg("SOL paid below the minimum")]
    SolBelowMin,
    /// [Lenders] `withdraw`: the last shares cannot leave while tokens are borrowed (ARCH §9
    /// M0b note 4; M1-SEC-3).
    #[msg("The last shares cannot be withdrawn while tokens are borrowed")]
    LastSharesBorrowed,
    /// [Admin] `timelock_s` above the compiled maximum (M1-SEC-4).
    #[msg("Timelock is above the compiled maximum")]
    TimelockTooLong,
    /// [Admin] `apply_config` after the pending change's apply window (M1-SEC-9).
    #[msg("The pending config change expired; propose it again")]
    PendingChangeExpired,
    /// [Lenders] `withdraw` to a Token-2022 account that requires memos, without the memo
    /// program (ARCH §3.3; M1-SEC-7).
    #[msg("The destination requires a memo: pass the memo program")]
    MemoProgramRequired,
    /// [Opens/markets] `create_market`: the configured creation fee is above the creator's
    /// `max_creation_fee` (M1-SEC-11).
    #[msg("Market creation fee above the maximum")]
    CreationFeeAboveMax,
    /// [Oracle] `publish_time` more than `oracle_max_age_s` ahead of the clock (M1-SEC-8).
    #[msg("Oracle publish time is in the future")]
    OracleFutureDated,
    /// [Admin] `cancel_config`: only Governance may cancel a pending `Authorities` or
    /// `Timelock` change (owner decision on M1-SEC-1, SPEC §11).
    #[msg("Only governance may cancel an authorities or timelock change")]
    EmergencyCannotCancel,

    // ---- Appended in M2 (6099; group in brackets) ----
    /// [Exits] `expire` on a position that is expired **and** CR-liquidatable: the penalty
    /// applies, so the keeper must call `liquidate` (ECON §8.4; ARCH §3.6 step 5, §3.7). The
    /// mirror of `UseExpire`.
    #[msg("Position is expired and CR-liquidatable; use liquidate")]
    UseLiquidate,

    // ---- Appended in M4 (6100; group in brackets) ----
    /// [Pool selection] `propose_pool` while a fast-path collection window has run its
    /// `pool_fast_window_s` without being resolved, or while the `pool_fail` chain that armed
    /// it is broken (M4-SEC-2: the window can then only be cancelled): the window takes no more
    /// candidates, and `execute_pool_switch` adopts or cancels it first (ARCH §2.6 rule 3).
    #[msg("The fast-path collection window has closed; call execute_pool_switch")]
    PoolWindowClosed,

    // ---- Appended after M4 (6101; group in brackets) ----
    /// [Opens, keeper exits] The observation state was (re)started by a venue change or a price
    /// start and has integrated fewer than `EMA_WARMUP_BUCKETS` buckets since (`Market.ema_warm >
    /// 0`); opens (`open_short` on Raydium) and keeper exits (`liquidate`, `expire`, on an
    /// executable venue) wait (M4R post-change confirmation, **PROPOSED**, owner item O18; M4V-5;
    /// ARCH §2.6 rule 4, §3.4 step 3, §3.6 precondition 1b, §3.7; `shortwin_math::observe::warmed`).
    #[msg("The price state is warming up after a venue change; opens and keeper exits wait")]
    EmaWarmingUp,

    // ---- Appended after M4 (6102; group in brackets) ----
    /// [Pool selection] "Switch only if all fit" (owner decision of 2026-09-29; ARCH §2.6, ECON
    /// §6.1): `propose_pool` of a collection window's candidate — its adoption carries the
    /// market's open positions — whose normal opening caps some open position or the market's OI
    /// would not fit (the per-position impact and fee-scaled size caps on the candidate's
    /// conservative `X₀`, bounded by `min(max_q_hwm, ΣB_Q)`; the per-market impact and
    /// fee-scaled OI caps on `ΣB_Q`; the tier the market would have on the candidate;
    /// `engine::pool_select::carried_fit`). `execute_pool_switch` cancels a window whose candidate
    /// no longer fits; with no candidate that fits, the positions exit on the failed pool through
    /// the S3 chain at fair value.
    #[msg("An open position or the market's OI does not fit the candidate pool's opening caps")]
    CarriedPositionsTooLarge,

    // ---- Appended after M4 (6103; reserved) ----
    /// **Reserved, never returned.** Was FIT-3's `PositionScanOutOfOrder` of `refresh_max_q`,
    /// which was removed with the keeper's refresh duty before any deployment (the mark
    /// `max_q_hwm` is again lowered only when `open_positions` returns to 0; ECON §6.1 discloses
    /// the residual). The variant keeps 6103 so no later code is renumbered.
    #[msg("Reserved (6103): unused")]
    ReservedScanOutOfOrder,

    // ---- Appended by M6-SEC-3 (6104; group in brackets) ----
    /// [Opens] `open_short` on a `PumpCurve` market while PumpSwap's `GlobalConfig.disable_flags`
    /// bit 0 (`create_pool`) is set, read from the curve ReadSet's C6 (owner decision of
    /// 2026-09-30, M6-SEC-3; ARCH §3.4 step 3, §3.11, §5.5): the bit halts pAMM `create_pool` and
    /// with it pump's `migrate_v2` (6017), so a curve that completes while it is set cannot
    /// graduate and stays `PumpMigrating` (S3). New curve positions are refused until it is
    /// re-enabled; open positions are unaffected (their exits and settlements never read it).
    #[msg(
        "PumpSwap pool creation is disabled (GlobalConfig.disable_flags bit 0), so this curve cannot graduate: new curve positions are refused until it is re-enabled; open positions are unaffected"
    )]
    GraduationHalted,

    // ---- Appended by M10 SQ-G8 (6105; group in brackets) ----
    /// [Admin] `initialize_config`, or an `Authorities` change at `propose_config` /
    /// `apply_config`, whose `governance`, `pause_authority`, `fee_recipient` and installed
    /// `coverage_authority` (an unset one, `Pubkey::default()`, is skipped) are not pairwise
    /// distinct (owner decision of 2026-10-02, SQ-G8; ARCH §3.1, §8.5;
    /// `state::config::check_authorities`).
    #[msg(
        "governance, pause_authority, fee_recipient and coverage_authority must be pairwise distinct"
    )]
    AuthoritiesNotDistinct,

    // ---- Appended by M10 KI-65 (6106; group in brackets) ----
    /// [Lender] `deposit`, `withdraw`, `claim_sol`, `cover_deficit` or `sync_lender` converted
    /// the lender through a rebase record whose conversion remainder `conv_rem` changed, and
    /// that record was passed read-only (ECON §9.5, KI-65: no rebase orphans; ARCH §2.3.6,
    /// §6.2). A record the conversion does not change may stay read-only.
    #[msg("A ShareEpoch record the lender converts through must be passed writable")]
    EpochRecordReadOnly,

    // ---- Appended by M10 KI-65, owner-signed rebase conversions (6107; group in brackets) ----
    /// [Lender] `sync_lender` converted the lender through a rebase record (it entered a
    /// `1/1,000^k` record holding shares, a lock or a lock term) and the position's owner did
    /// not sign (owner decision of 2026-10-07, KI65-REV-SEC1; ECON §9.5; ARCH §3.3): the order
    /// of the conversions through a rebase record decides who receives its rounding units, so
    /// a third party may not choose it for another owner's position. Since KI-66 an unsigned
    /// sync stops at a rebase record with the pot-only touch (`pool::touch_pot`), so this is a
    /// post-condition the program does not reach (`engine::lender::reads_rebase`; since KI-66
    /// full also a record the unsigned sync changed); the code stays allocated.
    #[msg(
        "Only the position's owner may convert it through a rebase record: sync_lender must be signed by the owner"
    )]
    RebaseNeedsOwner,

    // ---- Appended for AIR-07's residual (6108; group in brackets) ----
    /// [Lender] `deposit` while the pool is drained: lenders hold shares, tokens are lent out
    /// and at most 1/1,000 of the token claims is unborrowed (`S > 0 ∧ ΣB_Q > 0 ∧ 1,000·T ≤ T +
    /// ΣB_Q`; `MathError::PoolDrained`).
    /// A settlement from that state can leave at most 1/1,000 of the claims, which flushes the
    /// unreleased vest to the lenders that held them (`pool::VEST_FLUSH_RATIO`); a deposit before
    /// it would keep its own tokens as claims, stop the flush and leave the vest to a later
    /// deposit at the collapsed share price. Deposits resume once repayments or settlements
    /// bring the unborrowed part above 1/1,000.
    #[msg(
        "The pool is drained (at most 1/1,000 of its tokens unborrowed): deposits wait until repayments or settlements free more"
    )]
    PoolDrained,

    // ---- Appended for pump's v3 adapter (6109; group in brackets) ----
    /// [Venue] A **live** pump curve of a cashback coin (`is_cashback_coin` @82): pump's v3
    /// trades refuse it (pump 6094 `CashbackCoinNotSupported`), and every curve leg trades v3,
    /// so short.win neither lists nor executes one (owner decision D1 (a) of 2026-10-13;
    /// pump-final §6.3). A complete cashback curve classifies as before, so the coin lists on
    /// PumpSwap once it graduates.
    #[msg(
        "pump.fun's current trades do not support cashback coins on the curve. This coin can be shorted once it graduates to PumpSwap."
    )]
    CurveCashbackCoin,
}

impl From<MathError> for ShortwinError {
    /// `shortwin-math` refusals map one to one onto the lender codes (the crate's codes 6–12
    /// mirror ARCH §3.11's lender group, 13 the appended `PoolDrained`); arithmetic and domain
    /// failures are `MathOverflow`, malformed inputs `InvalidParams`.
    fn from(e: MathError) -> Self {
        match e {
            MathError::Overflow
            | MathError::Underflow
            | MathError::DivByZero
            | MathError::Undefined
            | MathError::Invariant => Self::MathOverflow,
            MathError::InvalidInput => Self::InvalidParams,
            MathError::NegativePending => Self::NegativePending,
            MathError::InsufficientShares => Self::InsufficientShares,
            MathError::InsufficientInventory => Self::InsufficientFreeInventory,
            MathError::AboveAllowance => Self::AboveProRataAllowance,
            MathError::LenderBehind => Self::LenderBehindEpochs,
            MathError::DepositTooSmall => Self::ZeroAmount,
            MathError::WipeSweepPending => Self::WipeSweepPending,
            MathError::PoolDrained => Self::PoolDrained,
        }
    }
}

/// `.math()?` on a `shortwin-math` result: maps [`MathError`] through
/// `From<MathError> for ShortwinError`.
pub trait MathResultExt<T> {
    /// The value, or the mapped program error.
    fn math(self) -> Result<T>;
}

impl<T> MathResultExt<T> for core::result::Result<T, MathError> {
    fn math(self) -> Result<T> {
        self.map_err(|e| error!(ShortwinError::from(e)))
    }
}

/// `.or_math()?` on a checked-arithmetic `Option` (`None` = overflow).
pub trait CheckedExt<T> {
    /// The value, or `MathOverflow`.
    fn or_math(self) -> Result<T>;
}

impl<T> CheckedExt<T> for Option<T> {
    fn or_math(self) -> Result<T> {
        self.ok_or_else(|| error!(ShortwinError::MathOverflow))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(e: ShortwinError) -> u32 {
        u32::from(e)
    }

    /// ARCH §3.11 codes are stable: the first and last of every group.
    #[test]
    fn codes_are_stable() {
        let pins = [
            (ShortwinError::MathOverflow, 6000),
            (ShortwinError::EpochRecordMismatch, 6007),
            (ShortwinError::Unauthorized, 6008),
            (ShortwinError::CoverageAuthorityDisabled, 6015),
            (ShortwinError::MintUnsupportedProgram, 6016),
            (ShortwinError::MintUnsafeFlagged, 6022),
            (ShortwinError::NoSupportedVenue, 6023),
            (ShortwinError::AuthorityTampered, 6041),
            (ShortwinError::PoolIneligible, 6042),
            (ShortwinError::PoolTooThin, 6049),
            (ShortwinError::OpensPaused, 6050),
            (ShortwinError::OpenFeeAboveMax, 6065),
            (ShortwinError::InsufficientVault, 6066),
            (ShortwinError::S3TimerRunning, 6075),
            (ShortwinError::ZeroAmount, 6076),
            (ShortwinError::WipeSweepPending, 6084),
            (ShortwinError::OracleOwner, 6085),
            (ShortwinError::OracleVerification, 6090),
            (ShortwinError::SolBelowMin, 6091),
            (ShortwinError::LastSharesBorrowed, 6092),
            (ShortwinError::TimelockTooLong, 6093),
            (ShortwinError::PendingChangeExpired, 6094),
            (ShortwinError::MemoProgramRequired, 6095),
            (ShortwinError::CreationFeeAboveMax, 6096),
            (ShortwinError::OracleFutureDated, 6097),
            (ShortwinError::EmergencyCannotCancel, 6098),
            (ShortwinError::UseLiquidate, 6099),
            (ShortwinError::PoolWindowClosed, 6100),
            (ShortwinError::EmaWarmingUp, 6101),
            (ShortwinError::CarriedPositionsTooLarge, 6102),
            (ShortwinError::ReservedScanOutOfOrder, 6103),
            (ShortwinError::GraduationHalted, 6104),
            (ShortwinError::AuthoritiesNotDistinct, 6105),
            (ShortwinError::EpochRecordReadOnly, 6106),
            (ShortwinError::RebaseNeedsOwner, 6107),
            (ShortwinError::PoolDrained, 6108),
            (ShortwinError::CurveCashbackCoin, 6109),
            (ShortwinError::CurveStraddle, 6031),
        ];
        for (e, c) in pins {
            assert_eq!(code(e), c, "{e:?}");
        }
    }

    #[test]
    fn math_errors_map_to_lender_codes() {
        for e in MathError::ALL {
            let mapped = ShortwinError::from(e);
            match e.code() {
                0..=2 | 4 | 5 => assert_eq!(mapped, ShortwinError::MathOverflow),
                3 => assert_eq!(mapped, ShortwinError::InvalidParams),
                // AIR-07's residual, appended after the lender group.
                13 => assert_eq!(mapped, ShortwinError::PoolDrained),
                _ => assert!(code(mapped) >= 6076 && code(mapped) <= 6084, "{e:?}"),
            }
        }
    }
}
