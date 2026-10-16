//! Direct lamport moves out of program-owned accounts (ARCH §2.1 `lamports.rs`, §2.3.1 A4,
//! §2.4).
//!
//! Position SOL (`ShortPosition`), lender SOL and protocol fees (`MarketVault`) are plain
//! lamports on accounts short.win owns, so they move by direct debit and credit, never by a
//! System CPI (an account with data cannot be a System `transfer` source) and never through
//! an external program. Every debit is checked arithmetic and leaves `from` at or above its
//! floor: the rent-exempt minimum of its data length, plus whatever tracked balance the
//! caller says must stay (`MarketVault`: `lender_sol + protocol_fees` after the move, I2).
//!
//! Crediting needs no ownership: the runtime lets a program add lamports to any writable
//! account. The runtime also checks, once per transaction, that the instruction conserved
//! lamports and left every account rent-exempt or empty [V4].

use anchor_lang::prelude::*;

use crate::errors::{CheckedExt, ShortwinError};

/// Rent-exempt minimum for `data_len` bytes (from the `Rent` sysvar; never hard-coded).
pub fn rent_min(data_len: usize) -> Result<u64> {
    Ok(Rent::get()?.minimum_balance(data_len))
}

/// Moves `amount` lamports from the program-owned `from` to `to`, keeping `from` at or above
/// `rent_min(from.data_len())`.
pub fn move_lamports(from: &AccountInfo, to: &AccountInfo, amount: u64) -> Result<()> {
    move_lamports_above(from, to, amount, 0)
}

/// Moves `amount` lamports from the program-owned `from` to `to`, keeping `from` at or above
/// `rent_min(from.data_len()) + reserved` (e.g. the vault's tracked `lender_sol +
/// protocol_fees` after the move).
///
/// Errors: `AccountMismatch` if `from` is not owned by short.win, `from == to`, or either is
/// not writable; `MathOverflow` on overflow or if the move would take `from` below its floor.
pub fn move_lamports_above(
    from: &AccountInfo,
    to: &AccountInfo,
    amount: u64,
    reserved: u64,
) -> Result<()> {
    require_keys_eq!(*from.owner, crate::ID, ShortwinError::AccountMismatch);
    require!(
        from.is_writable && to.is_writable,
        ShortwinError::AccountMismatch
    );
    require_keys_neq!(from.key(), to.key(), ShortwinError::AccountMismatch);
    if amount == 0 {
        return Ok(());
    }
    let floor = rent_min(from.data_len())?.checked_add(reserved).or_math()?;
    let from_after = from.lamports().checked_sub(amount).or_math()?;
    require_gte!(from_after, floor, ShortwinError::MathOverflow);
    let to_after = to.lamports().checked_add(amount).or_math()?;
    **from.try_borrow_mut_lamports()? = from_after;
    **to.try_borrow_mut_lamports()? = to_after;
    Ok(())
}

/// Moves `amount` lamports from the program-owned `from` to `to` **without** the rent floor
/// on `from` (only `from ≥ 0`): the funding debit of a buy leg (ARCH §2.4 rule 3). The source
/// is the `ShortPosition`, capped at its total lamports; "rent state is checked once per
/// transaction [V4], so briefly using the position's rent reserve is safe": the leg returns
/// every unspent lamport to the position in the same instruction, and every later debit of it
/// ([`move_lamports`]) and the instruction's post-conditions keep it at or above its floor
/// (or close it).
///
/// Errors: as [`move_lamports_above`], with `MathOverflow` only if `from` has fewer than
/// `amount` lamports.
pub fn move_lamports_in_leg(from: &AccountInfo, to: &AccountInfo, amount: u64) -> Result<()> {
    require_keys_eq!(*from.owner, crate::ID, ShortwinError::AccountMismatch);
    require!(
        from.is_writable && to.is_writable,
        ShortwinError::AccountMismatch
    );
    require_keys_neq!(from.key(), to.key(), ShortwinError::AccountMismatch);
    if amount == 0 {
        return Ok(());
    }
    let from_after = from.lamports().checked_sub(amount).or_math()?;
    let to_after = to.lamports().checked_add(amount).or_math()?;
    **from.try_borrow_mut_lamports()? = from_after;
    **to.try_borrow_mut_lamports()? = to_after;
    Ok(())
}

/// Lamports of `account` above its rent-exempt minimum (0 if below).
pub fn excess_over_rent(account: &AccountInfo) -> Result<u64> {
    Ok(account
        .lamports()
        .saturating_sub(rent_min(account.data_len())?))
}
