//! `MarketVault` (zero-copy since v0.4.1, seeds `["market_vault", market]`, 2,200 B; ARCH
//! §2.3.6, A9, A70, A75).
//!
//! Its lamports are the market's SOL float: `lamports ≥ rent_min + lender_sol + protocol_fees`
//! (I2). The pot `unmatched` and the unreleased vest `vest_bal` are parts of `lender_sol`, not
//! extra lamports. It also holds the lock state (Π, `pi_epoch`, `lock_norm`, `b_min`) and the
//! lock expiry calendar, so `claim_sol`, `cover_deficit` and `sync_lender` run the full lender
//! touch without write-locking `Market` (C0-17).
//!
//! | Offset | Bytes | Fields |
//! |---|---|---|
//! | 8 | 24 | `lender_sol`, `unmatched`, `protocol_fees` u64 |
//! | 32 | 32 | `pi`, `lock_norm` PodU128 |
//! | 64 | 16 | `b_min` u64, `pi_epoch` u32, `bump` u8, `_pad0` [u8;3] |
//! | 80 | 8 | `cal_day` i64 |
//! | 88 | 2,048 | `lock_cal` [PodU128; 128] (bucket `d mod 128` = lock terms ending day `d`) |
//! | 2,136 | 64 | `_reserved` |

use anchor_lang::prelude::*;
use shortwin_math::pool::Vault;

use crate::state::pod::{set_all, PodU128};

/// The market's SOL float and lock state.
#[account(zero_copy)]
pub struct MarketVault {
    /// Realised lender SOL (ECON `sol_vault`), including the pot and the unreleased vest.
    pub lender_sol: u64,
    /// The pot: realised SOL beyond its receivers' entitlement (R3A-2).
    pub unmatched: u64,
    /// Protocol fees awaiting `sweep_protocol_fees`.
    pub protocol_fees: u64,
    /// Π (WAD).
    pub pi: PodU128,
    /// Σ of the live calendar buckets.
    pub lock_norm: PodU128,
    /// Running minimum of `ΣB_Q` since the first lock of the Π epoch (V2-3).
    pub b_min: u64,
    pub pi_epoch: u32,
    /// PDA bump of `["market_vault", market]`.
    pub bump: u8,
    pub _pad0: [u8; 3],
    /// Oldest calendar day not yet dropped.
    pub cal_day: i64,
    /// Lock expiry calendar (128 day buckets).
    pub lock_cal: [PodU128; 128],
    pub _reserved: [u8; 64],
}

impl MarketVault {
    /// PDA seed prefix (`["market_vault", market]`).
    pub const SEED: &'static [u8] = b"market_vault";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + core::mem::size_of::<Self>();

    /// The state as `shortwin_math::pool::Vault`, on the heap: the calendar makes it 2,128 B,
    /// half of an SBF stack frame, so handlers keep it boxed.
    pub fn vault(&self) -> Box<Vault> {
        let mut v = Box::new(Vault::new());
        self.load_vault(&mut v);
        v
    }

    /// Copies the state into `v` (field by field; no 2 KB temporary).
    pub fn load_vault(&self, v: &mut Vault) {
        for (o, b) in v.lock_cal.iter_mut().zip(self.lock_cal.iter()) {
            *o = b.get();
        }
        v.pi = self.pi.get();
        v.lock_norm = self.lock_norm.get();
        v.lender_sol = self.lender_sol;
        v.unmatched = self.unmatched;
        v.protocol_fees = self.protocol_fees;
        v.b_min = self.b_min;
        v.cal_day = self.cal_day;
        v.pi_epoch = self.pi_epoch;
        v._pad = [0; 4];
    }

    /// Stores `shortwin_math::pool::Vault` back (the inverse of [`MarketVault::vault`]).
    pub fn set_vault(&mut self, v: &Vault) {
        set_all(&mut self.lock_cal, &v.lock_cal);
        self.pi.set(v.pi);
        self.lock_norm.set(v.lock_norm);
        self.lender_sol = v.lender_sol;
        self.unmatched = v.unmatched;
        self.protocol_fees = v.protocol_fees;
        self.b_min = v.b_min;
        self.cal_day = v.cal_day;
        self.pi_epoch = v.pi_epoch;
    }

    /// `lender_sol + protocol_fees`: the lamports above the rent floor the vault must hold (I2).
    pub fn tracked_lamports(&self) -> Option<u64> {
        self.lender_sol.checked_add(self.protocol_fees)
    }
}
