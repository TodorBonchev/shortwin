//! Asserting on transaction failures: the custom error code of the failing instruction.

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use litesvm::types::{FailedTransactionMetadata, TransactionMetadata};
use shortwin::errors::ShortwinError;
use solana_instruction::error::InstructionError;
use solana_transaction_error::TransactionError;

/// The custom error code of a failed transaction, if it failed with one.
pub fn custom_code(res: &Result<TransactionMetadata, FailedTransactionMetadata>) -> Option<u32> {
    match res {
        Err(FailedTransactionMetadata {
            err: TransactionError::InstructionError(_, InstructionError::Custom(code)),
            ..
        }) => Some(*code),
        _ => None,
    }
}

fn describe(res: &Result<TransactionMetadata, FailedTransactionMetadata>) -> String {
    match res {
        Ok(meta) => format!("succeeded; logs:\n{}", meta.pretty_logs()),
        Err(f) => format!("failed with {:?}; logs:\n{}", f.err, f.meta.pretty_logs()),
    }
}

/// Asserts the transaction failed with `expected` (a short.win error).
#[track_caller]
pub fn assert_err(
    res: &Result<TransactionMetadata, FailedTransactionMetadata>,
    expected: ShortwinError,
) {
    let want = u32::from(expected);
    assert_eq!(
        custom_code(res),
        Some(want),
        "expected {expected:?} ({want}), transaction {}",
        describe(res)
    );
}

/// Asserts the transaction failed with an Anchor framework error (e.g. `ConstraintSeeds`).
#[track_caller]
pub fn assert_anchor_err(
    res: &Result<TransactionMetadata, FailedTransactionMetadata>,
    expected: AnchorErrorCode,
) {
    let want = u32::from(expected);
    assert_eq!(
        custom_code(res),
        Some(want),
        "expected Anchor {expected:?} ({want}), transaction {}",
        describe(res)
    );
}

/// Asserts the transaction failed (any error).
#[track_caller]
pub fn assert_failed(res: &Result<TransactionMetadata, FailedTransactionMetadata>) {
    assert!(
        res.is_err(),
        "expected a failure, transaction {}",
        describe(res)
    );
}

/// Unwraps a successful transaction, printing the logs otherwise.
#[track_caller]
pub fn ok(res: Result<TransactionMetadata, FailedTransactionMetadata>) -> TransactionMetadata {
    match res {
        Ok(meta) => meta,
        Err(f) => panic!(
            "transaction failed with {:?}; logs:\n{}",
            f.err,
            f.meta.pretty_logs()
        ),
    }
}
