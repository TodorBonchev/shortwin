//! `LenderPosition` (Borsh, seeds `["lender", market, owner]`, 241 B; ARCH §2.3.6).
//!
//! Created by `deposit` (`init_if_needed`, payer owner); mutated by `deposit`, `withdraw`,
//! `claim_sol`, `cover_deficit` and `sync_lender`; closed (rent to the owner) by `withdraw` and
//! `claim_sol` when `shortwin_math::pool::closable` holds (KI-67). [`LenderPosition::lender`] /
//! [`LenderPosition::set_lender`] convert to and from `shortwin_math::pool::Lender`.
//!
//! **Layout (Borsh, 241 B with the discriminator).** `owner@8`, `market@40`, `shares@72`,
//! `snap@88`, `pending@104`, `snap_r@112`, `real@128`, `claimed@136`, `epoch@144`, `lock@148`,
//! `lock_pi@164`, `lock_epoch@180`, `lock_term@184`, `lock_until@200`, `bump@208`; KI-66 full
//! took 22 of the 32 reserved bytes (no migration: not deployed): `fwd_shares@209`,
//! `fwd_epoch@225`, `fwd@229`, `fwd_lag@230`, `_reserved[10]@231`.
//!
//! **The forward chain (KI-66 full).** While `fwd != FWD_NONE` (a third party's pot-only touch
//! held the lender at a rebase record, `pool::touch_pot`), `snap` / `snap_r` are the chain's
//! snapshots in `fwd_epoch`, not the lender's own (those are implicit: the record's ends in
//! `FWD_HOLD`, 0 in `FWD_OPEN`); a reader that accrues the lender itself must go through
//! `shortwin_math::pool` (`touch`, `allowance_view`), never `shares·(acc − snap)`.

use anchor_lang::prelude::*;
use shortwin_math::pool::Lender;

/// A lender's position in one market (ARCH §2.3.6).
#[account]
#[derive(InitSpace, Debug, PartialEq, Eq)]
pub struct LenderPosition {
    pub owner: Pubkey,
    pub market: Pubkey,
    /// Shares, in units of `epoch`.
    pub shares: u128,
    /// `acc` snapshot.
    pub snap: i128,
    /// Entitlement not yet paid (negative after a shortfall debit, ECON §9.4).
    pub pending: i64,
    /// `acc_r` snapshot.
    pub snap_r: u128,
    /// Cumulative realised SOL allocated to this lender, including pot draws (V2-2).
    pub real: u64,
    /// Cumulative SOL paid to this lender (V2-2).
    pub claimed: u64,
    /// Share epoch of `shares`.
    pub epoch: u32,
    /// Locked shares at `lock_pi` (ECON §9.3).
    pub lock: u128,
    /// Π when the lock was set.
    pub lock_pi: u128,
    /// Π epoch of `lock` (`u32::MAX` = no lock).
    pub lock_epoch: u32,
    /// The lock's term in the calendar bucket of `lock_until`.
    pub lock_term: u128,
    /// When the lock expires (a calendar day end; R3B-1, R4A-2).
    pub lock_until: i64,
    pub bump: u8,
    /// KI-66 full: the forward chain's shares in `fwd_epoch` (`pool::Lender::fwd_shares`; 0
    /// without a chain).
    pub fwd_shares: u128,
    /// KI-66 full: the epoch the forward chain reached (0 without a chain).
    pub fwd_epoch: u32,
    /// KI-66 full: `pool::FWD_NONE` / `FWD_HOLD` / `FWD_OPEN`; while not `FWD_NONE`, `snap` /
    /// `snap_r` are the chain's snapshots (module docs).
    pub fwd: u8,
    /// KI-66 full, `FWD_OPEN`: `shares` minus the chain's shares in `epoch` (0 or 1).
    pub fwd_lag: u8,
    pub _reserved: [u8; 10],
}

impl LenderPosition {
    /// PDA seed prefix (`["lender", market, owner]`).
    pub const SEED: &'static [u8] = b"lender";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + Self::INIT_SPACE;

    /// The state as `shortwin_math::pool::Lender`.
    pub fn lender(&self) -> Lender {
        Lender {
            shares: self.shares,
            snap: self.snap,
            snap_r: self.snap_r,
            lock: self.lock,
            lock_pi: self.lock_pi,
            lock_term: self.lock_term,
            pending: self.pending,
            real: self.real,
            claimed: self.claimed,
            lock_until: self.lock_until,
            epoch: self.epoch,
            lock_epoch: self.lock_epoch,
            fwd_epoch: self.fwd_epoch,
            fwd: self.fwd,
            fwd_lag: self.fwd_lag,
            _pad: [0; 2],
            fwd_shares: self.fwd_shares,
        }
    }

    /// Stores `shortwin_math::pool::Lender` back.
    pub fn set_lender(&mut self, l: &Lender) {
        self.shares = l.shares;
        self.snap = l.snap;
        self.snap_r = l.snap_r;
        self.lock = l.lock;
        self.lock_pi = l.lock_pi;
        self.lock_term = l.lock_term;
        self.pending = l.pending;
        self.real = l.real;
        self.claimed = l.claimed;
        self.lock_until = l.lock_until;
        self.epoch = l.epoch;
        self.lock_epoch = l.lock_epoch;
        self.fwd_shares = l.fwd_shares;
        self.fwd_epoch = l.fwd_epoch;
        self.fwd = l.fwd;
        self.fwd_lag = l.fwd_lag;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use anchor_lang::{AccountDeserialize, AccountSerialize, Discriminator};

    /// The KI-66 full fields sit where ARCH §2.3.6 puts them: in the former reserved bytes,
    /// after `bump@208`, the size unchanged.
    #[test]
    fn forward_chain_fields_take_reserved_bytes() {
        let pos = LenderPosition {
            owner: Pubkey::new_from_array([1; 32]),
            market: Pubkey::new_from_array([2; 32]),
            shares: 3,
            snap: -4,
            pending: -5,
            snap_r: 6,
            real: 7,
            claimed: 8,
            epoch: 9,
            lock: 10,
            lock_pi: 11,
            lock_epoch: 12,
            lock_term: 13,
            lock_until: 14,
            bump: 0xfe,
            fwd_shares: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
            fwd_epoch: 0x1112_1314,
            fwd: 2,
            fwd_lag: 1,
            _reserved: [0; 10],
        };
        let mut data = Vec::new();
        pos.try_serialize(&mut data).unwrap();
        assert_eq!(data.len(), LenderPosition::SPACE);
        assert_eq!(&data[..8], LenderPosition::DISCRIMINATOR);
        assert_eq!(data[208], 0xfe, "bump@208");
        assert_eq!(
            &data[209..225],
            &pos.fwd_shares.to_le_bytes(),
            "fwd_shares@209"
        );
        assert_eq!(
            &data[225..229],
            &pos.fwd_epoch.to_le_bytes(),
            "fwd_epoch@225"
        );
        assert_eq!((data[229], data[230]), (2, 1), "fwd@229, fwd_lag@230");
        assert_eq!(&data[231..], &[0; 10], "_reserved@231");
        let back = LenderPosition::try_deserialize(&mut &data[..]).unwrap();
        assert_eq!(back, pos);
        assert_eq!(back.lender().fwd_shares, pos.fwd_shares);
    }
}
