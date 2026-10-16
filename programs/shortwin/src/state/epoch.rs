//! `ShareEpoch` (Borsh, seeds `["share_epoch", market, epoch u32 LE]`, 143 B; ARCH §2.3.6, A39).
//!
//! Written by `sol_settle` when it rolls the epoch (M2; payer keeper, grief-proof creation).
//! Immutable once written, except that
//! - the record of a **full wipe** is topped up by SOL credited while the market has no
//!   shares, until the next deposit finalises it (V2-12);
//! - the **conversion remainder** `conv_rem` of a rebase record is written by every lender
//!   instruction that converts a lender through it (KI-65: conversions round cumulatively, so
//!   the old epoch's holders end with exactly the new `S`, no rebase orphans). Those
//!   instructions (`deposit`, `withdraw`, `claim_sol`, `cover_deficit`, `sync_lender`) write
//!   the 16 bytes at [`ShareEpoch::CONV_REM_OFFSET`] directly
//!   (`engine::lender::store_records`) and need such a record writable
//!   (`EpochRecordReadOnly`).
//!
//! Lender instructions read the records a lender is behind on from `remaining_accounts`,
//! key-checked against [`ShareEpoch::address`].

use anchor_lang::prelude::*;
use shortwin_math::pool::ShareEpoch as MathShareEpoch;

/// A share-epoch record (ARCH §2.3.6).
#[account]
#[derive(InitSpace, Debug, PartialEq, Eq)]
pub struct ShareEpoch {
    pub market: Pubkey,
    /// The epoch this record closes.
    pub epoch: u32,
    /// `acc` at the roll (topped up while it is a live full wipe).
    pub acc_end: i128,
    /// `acc_r` at the roll.
    pub acc_r_end: u128,
    /// Shares at the roll (> 0).
    pub s_old: u128,
    /// Top-up remainder of `acc_end`.
    pub carry: i128,
    /// Top-up remainder of `acc_r_end`.
    pub carry_r: u128,
    /// Conversion numerator: 0 (full wipe) or 1.
    pub num: u8,
    /// `k` of the denominator `1,000^k`.
    pub den_log1000: u8,
    pub bump: u8,
    /// Conversion remainder (KI-65): the old-epoch shares converted through this record so
    /// far, modulo the denominator, in `[0, 1,000^k)`; 0 when written and for a full wipe.
    /// The last field, at [`Self::CONV_REM_OFFSET`], so a lender instruction rewrites it in
    /// place.
    pub conv_rem: u128,
}

impl ShareEpoch {
    /// PDA seed prefix (`["share_epoch", market, epoch u32 LE]`).
    pub const SEED: &'static [u8] = b"share_epoch";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + Self::INIT_SPACE;
    /// Byte offset of `conv_rem` (u128 LE) in the account data, discriminator included:
    /// the last 16 bytes (`state::layout_asserts`).
    pub const CONV_REM_OFFSET: usize = 127;

    /// The record's address and bump (host only: clients and tests; on-chain, handlers
    /// re-derive with the stored bump via `create_program_address`, never a bump search).
    #[cfg(not(target_os = "solana"))]
    pub fn address(market: &Pubkey, epoch: u32) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[Self::SEED, market.as_ref(), &epoch.to_le_bytes()],
            &crate::ID,
        )
    }

    /// The record as `shortwin_math::pool::ShareEpoch`.
    pub fn record(&self) -> MathShareEpoch {
        MathShareEpoch {
            acc_end: self.acc_end,
            acc_r_end: self.acc_r_end,
            s_old: self.s_old,
            carry: self.carry,
            carry_r: self.carry_r,
            conv_rem: self.conv_rem,
            epoch: self.epoch,
            num: self.num,
            den_log1000: self.den_log1000,
            _pad: [0; 10],
        }
    }

    /// Stores `shortwin_math::pool::ShareEpoch` back (market and bump are the caller's).
    pub fn set_record(&mut self, r: &MathShareEpoch) {
        self.acc_end = r.acc_end;
        self.acc_r_end = r.acc_r_end;
        self.s_old = r.s_old;
        self.carry = r.carry;
        self.carry_r = r.carry_r;
        self.conv_rem = r.conv_rem;
        self.epoch = r.epoch;
        self.num = r.num;
        self.den_log1000 = r.den_log1000;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use anchor_lang::AccountSerialize;

    /// KI-65: `conv_rem` is the last 16 bytes of the record, where
    /// `engine::lender::store_records` rewrites it in place; `record` / `set_record` carry it.
    #[test]
    fn conv_rem_is_the_last_field() {
        let mut rec = ShareEpoch {
            market: Pubkey::new_unique(),
            epoch: 7,
            acc_end: -3,
            acc_r_end: 4,
            s_old: 5_000_123,
            carry: -6,
            carry_r: 7,
            num: 1,
            den_log1000: 2,
            bump: 254,
            conv_rem: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
        };
        let mut data = Vec::new();
        rec.try_serialize(&mut data).unwrap();
        assert_eq!(data.len(), ShareEpoch::SPACE);
        assert_eq!(
            data[ShareEpoch::CONV_REM_OFFSET..],
            rec.conv_rem.to_le_bytes()
        );
        let mut r = rec.record();
        assert_eq!(r.conv_rem, rec.conv_rem);
        r.conv_rem = 999_999;
        rec.set_record(&r);
        assert_eq!(rec.conv_rem, 999_999);
    }
}
