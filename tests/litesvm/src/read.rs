//! Reading program and token accounts out of LiteSVM.
//!
//! Zero-copy accounts are decoded with `bytemuck::pod_read_unaligned` after the discriminator
//! (the host's `u128` alignment is irrelevant: every 128-bit field is a `PodU128`, ARCH §2.3);
//! Borsh accounts with `AccountDeserialize` (which checks the discriminator).

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AccountDeserialize, Discriminator, ZeroCopy};
use litesvm::LiteSVM;
use solana_account::Account;

/// The raw account, if it exists.
pub fn account(svm: &LiteSVM, key: &Pubkey) -> Option<Account> {
    svm.get_account(key)
}

/// Lamports of `key` (0 if it does not exist).
pub fn lamports(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key).map_or(0, |a| a.lamports)
}

/// A zero-copy short.win account (owner and discriminator checked).
pub fn zc<T: ZeroCopy>(svm: &LiteSVM, key: &Pubkey) -> Option<T> {
    let acc = svm.get_account(key)?;
    if acc.owner != shortwin::ID || !acc.data.starts_with(T::DISCRIMINATOR) {
        return None;
    }
    let len = core::mem::size_of::<T>();
    let body = acc.data.get(8..8 + len)?;
    Some(bytemuck::pod_read_unaligned(body))
}

/// A Borsh short.win account (owner and discriminator checked).
pub fn borsh<T: AccountDeserialize>(svm: &LiteSVM, key: &Pubkey) -> Option<T> {
    let acc = svm.get_account(key)?;
    if acc.owner != shortwin::ID {
        return None;
    }
    T::try_deserialize(&mut acc.data.as_slice()).ok()
}

/// Every short.win account of type `T` (by discriminator), with its key.
pub fn all_of<T: Discriminator>(svm: &LiteSVM) -> Vec<(Pubkey, Account)> {
    svm.get_program_accounts(&shortwin::ID)
        .into_iter()
        .filter(|(_, a)| a.data.starts_with(T::DISCRIMINATOR))
        .collect()
}

/// SPL / Token-2022 token account `amount` (offset 64; both programs), if the account is a
/// token account.
pub fn token_amount(svm: &LiteSVM, key: &Pubkey) -> Option<u64> {
    let acc = svm.get_account(key)?;
    if acc.owner != shortwin::constants::TOKEN_PROGRAM_ID
        && acc.owner != shortwin::constants::TOKEN_2022_PROGRAM_ID
    {
        return None;
    }
    Some(shortwin_venue_state::bytes::u64_at(&acc.data, 64))
}

/// SPL / Token-2022 token account `delegate` (`COption<Pubkey>` at 72).
pub fn token_delegate(svm: &LiteSVM, key: &Pubkey) -> Option<Pubkey> {
    let acc = svm.get_account(key)?;
    (shortwin_venue_state::bytes::u32_at(&acc.data, 72) == 1)
        .then(|| Pubkey::new_from_array(shortwin_venue_state::bytes::key_at(&acc.data, 76)))
}
