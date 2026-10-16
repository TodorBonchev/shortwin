//! Owner: scaffold. The error type of every fallible `shortwin-math` function (ARCHITECTURE §2.2).

use core::fmt;

/// Why a `shortwin-math` computation failed.
///
/// Codes 0–5 are arithmetic and domain failures. Codes 6–13 are lender-pool rule
/// refusals (`pool`), one per program error (6–12 ARCH §3.11's lender group, 13 appended
/// for AIR-07), so the program maps them one to one without re-deriving the reason.
///
/// Codes are stable: the program maps them to its own error enum and the keeper and
/// frontend log them. Append new variants at the end with the next code; never
/// renumber or reuse one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MathError {
    /// A result or intermediate does not fit its type. Also returned when a negative
    /// value is converted to an unsigned type.
    Overflow = 0,
    /// A subtraction would go below zero.
    Underflow = 1,
    /// Division or remainder by zero.
    DivByZero = 2,
    /// An argument is outside the function's documented domain.
    InvalidInput = 3,
    /// The quantity is undefined for this state. Used by functions that return
    /// `Result`; the quote API signals "undefined" with `None` instead (ARCH §2.2).
    Undefined = 4,
    /// An internal consistency check failed (e.g. a waterfall whose legs do not sum to
    /// `V`, I-E4). Never caused by inputs: it signals a bug in this crate.
    Invariant = 5,
    /// Lender pool: a withdrawal while the lender's SOL balance `pending` is negative
    /// (C0-13; program `NegativePending`: cover the deficit first).
    NegativePending = 6,
    /// Lender pool: a withdrawal of 0 shares, of more shares than the lender holds in the
    /// current share epoch, or by a lender parked on a live wipe record (program
    /// `InsufficientShares`).
    InsufficientShares = 7,
    /// Lender pool: tokens asked for exceed the unborrowed inventory `T` (a withdrawal,
    /// or a borrow above `T`; program `InsufficientFreeInventory`).
    InsufficientInventory = 8,
    /// Lender pool: a withdrawal above the lender's pro-rata allowance (ECON §9.3, C0-9;
    /// program `AboveProRataAllowance`).
    AboveAllowance = 9,
    /// Lender pool: the lender is behind the current share epoch and the `ShareEpoch`
    /// records passed do not reach it (program `LenderBehindEpochs`: `sync_lender`).
    LenderBehind = 10,
    /// Lender pool: a deposit that would mint 0 shares (ECON §9.2).
    DepositTooSmall = 11,
    /// Lender pool: a deposit into a pool without shares (a live full-wipe record or, since
    /// AIR-08, after the last lender's exit) before the sweeps since `S` reached 0 ran
    /// (`sweep_trader`, and `claim_venue_rewards` on a pump venue that can execute; R4A-7;
    /// program `WipeSweepPending`).
    WipeSweepPending = 12,
    /// Lender pool: a deposit while the pool is drained: lenders hold shares, tokens are lent
    /// out (`ΣB_Q > 0`) and at most `1/VEST_FLUSH_RATIO` of the token claims is unborrowed
    /// (`1,000·T ≤ T + ΣB_Q`). A settlement from that state can leave at most 1/1,000 of the
    /// claims and flush the vest to the shares that held them; a deposit made before it would
    /// keep its own tokens as claims and stop the flush (AIR-07's residual; program
    /// `PoolDrained`).
    PoolDrained = 13,
}

impl MathError {
    /// Every variant, in code order.
    pub const ALL: [Self; 14] = [
        Self::Overflow,
        Self::Underflow,
        Self::DivByZero,
        Self::InvalidInput,
        Self::Undefined,
        Self::Invariant,
        Self::NegativePending,
        Self::InsufficientShares,
        Self::InsufficientInventory,
        Self::AboveAllowance,
        Self::LenderBehind,
        Self::DepositTooSmall,
        Self::WipeSweepPending,
        Self::PoolDrained,
    ];

    /// Stable numeric code of the variant.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// The variant with code `code`, if any.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Overflow),
            1 => Some(Self::Underflow),
            2 => Some(Self::DivByZero),
            3 => Some(Self::InvalidInput),
            4 => Some(Self::Undefined),
            5 => Some(Self::Invariant),
            6 => Some(Self::NegativePending),
            7 => Some(Self::InsufficientShares),
            8 => Some(Self::InsufficientInventory),
            9 => Some(Self::AboveAllowance),
            10 => Some(Self::LenderBehind),
            11 => Some(Self::DepositTooSmall),
            12 => Some(Self::WipeSweepPending),
            13 => Some(Self::PoolDrained),
            _ => None,
        }
    }

    /// Short, stable, human-readable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::Underflow => "underflow",
            Self::DivByZero => "division by zero",
            Self::InvalidInput => "invalid input",
            Self::Undefined => "undefined",
            Self::Invariant => "invariant violated",
            Self::NegativePending => "negative pending",
            Self::InsufficientShares => "insufficient shares",
            Self::InsufficientInventory => "insufficient free inventory",
            Self::AboveAllowance => "above pro-rata allowance",
            Self::LenderBehind => "lender behind share epochs",
            Self::DepositTooSmall => "deposit too small",
            Self::WipeSweepPending => "post-wipe sweep pending",
            Self::PoolDrained => "pool drained",
        }
    }
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl core::error::Error for MathError {}

#[cfg(test)]
mod tests {
    use super::MathError;

    #[test]
    fn codes_are_stable() {
        let all = [
            (MathError::Overflow, 0),
            (MathError::Underflow, 1),
            (MathError::DivByZero, 2),
            (MathError::InvalidInput, 3),
            (MathError::Undefined, 4),
            (MathError::Invariant, 5),
            (MathError::NegativePending, 6),
            (MathError::InsufficientShares, 7),
            (MathError::InsufficientInventory, 8),
            (MathError::AboveAllowance, 9),
            (MathError::LenderBehind, 10),
            (MathError::DepositTooSmall, 11),
            (MathError::WipeSweepPending, 12),
            (MathError::PoolDrained, 13),
        ];
        assert_eq!(all.len(), MathError::ALL.len());
        for ((e, code), listed) in all.into_iter().zip(MathError::ALL) {
            assert_eq!(e, listed);
            assert_eq!(e.code(), code);
            assert_eq!(MathError::from_code(code), Some(e));
            assert!(!e.as_str().is_empty());
        }
        assert_eq!(MathError::from_code(14), None);
        let names: std::collections::BTreeSet<_> =
            MathError::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(names.len(), MathError::ALL.len());
    }
}
