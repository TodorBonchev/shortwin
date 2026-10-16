//! SPL Token / Token-2022 token accounts (ARCH §4.1: `mint @0`, `owner @32`, `amount @64`,
//! `delegate COption @72`, `delegated_amount @121`, `close_authority COption @129`).
//!
//! The program measures every venue leg on token-account deltas and authorities (ARCH §2.4
//! rules 3 and 5): the pool vault's amount before and after a leg, its delegate after a sell
//! leg (auto-cleared at a zero allowance [V12]), the ephemeral `trader_wsol` amount, a venue's
//! vault balances for pricing reserves. This reader gives those fields from raw bytes, the
//! same for the program, the keeper and the WASM frontend.
//!
//! **Layout** (`spl_token::state::Account`, 165 bytes; Token-2022 appends `AccountType` = 2 at
//! 165 and TLV extensions after it):
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 32 | `mint` |
//! | 32 | 32 | `owner` (the token authority) |
//! | 64 | 8 | `amount` u64 |
//! | 72 | 36 | `delegate`: `COption<Pubkey>` (u32 LE tag 0/1, then the key) |
//! | 108 | 1 | `state` (0 uninitialized, 1 initialized, 2 frozen) |
//! | 109 | 12 | `is_native`: `COption<u64>` (the rent-exempt reserve of a native account) |
//! | 121 | 8 | `delegated_amount` u64 |
//! | 129 | 36 | `close_authority`: `COption<Pubkey>` |

use crate::bytes::{key_at, u8_at, u32_at, u64_at};
use crate::mint::{ACCOUNT_LEN, TOKEN_2022_PROGRAM, TOKEN_PROGRAM};
use crate::{Key, ReadError};

/// Offset of `mint`.
pub const MINT_OFFSET: usize = 0;
/// Offset of `owner`.
pub const OWNER_OFFSET: usize = 32;
/// Offset of `amount`.
pub const AMOUNT_OFFSET: usize = 64;
/// Offset of the `delegate` `COption` tag.
pub const DELEGATE_OFFSET: usize = 72;
/// Offset of `state`.
pub const STATE_OFFSET: usize = 108;
/// Offset of the `is_native` `COption` tag.
pub const IS_NATIVE_OFFSET: usize = 109;
/// Offset of `delegated_amount`.
pub const DELEGATED_AMOUNT_OFFSET: usize = 121;
/// Offset of the `close_authority` `COption` tag.
pub const CLOSE_AUTHORITY_OFFSET: usize = 129;
/// Token-2022 `AccountType` of a token account.
pub const ACCOUNT_TYPE_ACCOUNT: u8 = 2;

/// `state` of an initialized account.
pub const STATE_INITIALIZED: u8 = 1;
/// `state` of a frozen account.
pub const STATE_FROZEN: u8 = 2;

/// The fields of an initialized token account that short.win reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TokenAccount {
    pub mint: Key,
    /// The token authority (`owner` field; not the account's program owner).
    pub owner: Key,
    pub amount: u64,
    pub delegate: Option<Key>,
    pub delegated_amount: u64,
    pub close_authority: Option<Key>,
    /// `Some(rent reserve)` for a wrapped-SOL account.
    pub is_native: Option<u64>,
    /// `state == 2`.
    pub frozen: bool,
}

fn coption_key(data: &[u8], offset: usize) -> Result<Option<Key>, ReadError> {
    match u32_at(data, offset) {
        0 => Ok(None),
        1 => Ok(Some(key_at(data, offset.saturating_add(4)))),
        _ => Err(ReadError::Malformed),
    }
}

fn coption_u64(data: &[u8], offset: usize) -> Result<Option<u64>, ReadError> {
    match u32_at(data, offset) {
        0 => Ok(None),
        1 => Ok(Some(u64_at(data, offset.saturating_add(4)))),
        _ => Err(ReadError::Malformed),
    }
}

/// Reads an initialized SPL Token or Token-2022 token account: owner (program) ∈ {SPL Token,
/// Token-2022}; at least 165 bytes; a longer account must be a Token-2022 account whose
/// `AccountType` is `Account` (a mint or a multisig is refused); `state` initialized or
/// frozen; every `COption` tag valid.
pub fn read(program_owner: &Key, data: &[u8]) -> Result<TokenAccount, ReadError> {
    if *program_owner != TOKEN_PROGRAM && *program_owner != TOKEN_2022_PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if data.len() < ACCOUNT_LEN {
        return Err(ReadError::TooShort);
    }
    if data.len() > ACCOUNT_LEN
        && (*program_owner != TOKEN_2022_PROGRAM
            || u8_at(data, ACCOUNT_LEN) != ACCOUNT_TYPE_ACCOUNT)
    {
        return Err(ReadError::WrongTag);
    }
    let frozen = match u8_at(data, STATE_OFFSET) {
        STATE_INITIALIZED => false,
        STATE_FROZEN => true,
        _ => return Err(ReadError::Malformed),
    };
    Ok(TokenAccount {
        mint: key_at(data, MINT_OFFSET),
        owner: key_at(data, OWNER_OFFSET),
        amount: u64_at(data, AMOUNT_OFFSET),
        delegate: coption_key(data, DELEGATE_OFFSET)?,
        delegated_amount: u64_at(data, DELEGATED_AMOUNT_OFFSET),
        close_authority: coption_key(data, CLOSE_AUTHORITY_OFFSET)?,
        is_native: coption_u64(data, IS_NATIVE_OFFSET)?,
        frozen,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn account(amount: u64, delegate: Option<Key>) -> [u8; 165] {
        let mut d = [0u8; 165];
        d[..32].copy_from_slice(&[7; 32]);
        d[32..64].copy_from_slice(&[9; 32]);
        d[64..72].copy_from_slice(&amount.to_le_bytes());
        if let Some(k) = delegate {
            d[72..76].copy_from_slice(&1u32.to_le_bytes());
            d[76..108].copy_from_slice(&k);
            d[121..129].copy_from_slice(&5u64.to_le_bytes());
        }
        d[108] = STATE_INITIALIZED;
        d
    }

    #[test]
    fn reads_the_fields() {
        let d = account(42, Some([3; 32]));
        let a = read(&TOKEN_PROGRAM, &d).unwrap();
        assert_eq!(a.mint, [7; 32]);
        assert_eq!(a.owner, [9; 32]);
        assert_eq!(a.amount, 42);
        assert_eq!(a.delegate, Some([3; 32]));
        assert_eq!(a.delegated_amount, 5);
        assert_eq!(a.close_authority, None);
        assert_eq!(a.is_native, None);
        assert!(!a.frozen);
        let a = read(&TOKEN_2022_PROGRAM, &account(1, None)).unwrap();
        assert_eq!(a.delegate, None);
    }

    #[test]
    fn refuses_what_is_not_a_token_account() {
        let d = account(1, None);
        assert_eq!(read(&[1; 32], &d), Err(ReadError::WrongOwner));
        assert_eq!(read(&TOKEN_PROGRAM, &d[..164]), Err(ReadError::TooShort));
        let mut u = d;
        u[108] = 0;
        assert_eq!(read(&TOKEN_PROGRAM, &u), Err(ReadError::Malformed));
        let mut bad = d;
        bad[72] = 2;
        assert_eq!(read(&TOKEN_PROGRAM, &bad), Err(ReadError::Malformed));
        // An extended account: Token-2022 with AccountType 2 only.
        let mut ext = [0u8; 170];
        ext[..165].copy_from_slice(&d);
        assert_eq!(read(&TOKEN_PROGRAM, &ext), Err(ReadError::WrongTag));
        assert_eq!(read(&TOKEN_2022_PROGRAM, &ext), Err(ReadError::WrongTag));
        ext[165] = ACCOUNT_TYPE_ACCOUNT;
        assert!(read(&TOKEN_2022_PROGRAM, &ext).is_ok());
        ext[165] = 1; // a mint
        assert_eq!(read(&TOKEN_2022_PROGRAM, &ext), Err(ReadError::WrongTag));
    }
}
