//! `WalletExposure` (Borsh, seeds `["exposure", market, borrower]`, 81 B; ARCH §2.3.6).
//! Opens add `q`, exits subtract it; closed to the borrower at 0 (M2).

use anchor_lang::prelude::*;

/// A borrower's open quantity in one market (the per-wallet cap, SPEC §4).
#[account]
#[derive(InitSpace, Debug, PartialEq, Eq)]
pub struct WalletExposure {
    pub market: Pubkey,
    pub wallet: Pubkey,
    pub q_open: u64,
    pub bump: u8,
}

impl WalletExposure {
    /// PDA seed prefix (`["exposure", market, borrower]`).
    pub const SEED: &'static [u8] = b"exposure";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + Self::INIT_SPACE;
}
