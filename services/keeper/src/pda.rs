//! Program-derived addresses the keeper names (ARCHITECTURE §2.3.1). Host-side
//! `find_program_address`; the program re-derives with stored bumps.

use anchor_lang::prelude::Pubkey;
use shortwin::state::{
    GlobalConfig, MarketVault, OracleCache, ShareEpoch, ShortPosition, VenueExtras, VenueGuard,
    WalletExposure,
};

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &shortwin::ID).0
}

/// `["config"]`.
pub fn config() -> Pubkey {
    pda(&[GlobalConfig::SEED])
}

/// `["venue_guard"]` (the venue-upgrade latch, A7).
pub fn venue_guard() -> Pubkey {
    pda(&[VenueGuard::SEED])
}

/// `["sol_usd"]`.
pub fn oracle_cache() -> Pubkey {
    pda(&[OracleCache::SEED])
}

/// `["market_vault", market]`.
pub fn market_vault(market: &Pubkey) -> Pubkey {
    pda(&[MarketVault::SEED, market.as_ref()])
}

/// `["venue_extras", venue]`.
pub fn venue_extras(seed: u8) -> Pubkey {
    pda(&[VenueExtras::SEED, &[seed]])
}

/// `["exposure", market, borrower]`.
pub fn exposure(market: &Pubkey, borrower: &Pubkey) -> Pubkey {
    pda(&[WalletExposure::SEED, market.as_ref(), borrower.as_ref()])
}

/// `["share_epoch", market, epoch u32 LE]`.
pub fn share_epoch(market: &Pubkey, epoch: u32) -> Pubkey {
    pda(&[ShareEpoch::SEED, market.as_ref(), &epoch.to_le_bytes()])
}

/// `["position", market, borrower, nonce u64 LE]`.
pub fn position(market: &Pubkey, borrower: &Pubkey, nonce: u64) -> Pubkey {
    pda(&[
        ShortPosition::SEED,
        market.as_ref(),
        borrower.as_ref(),
        &nonce.to_le_bytes(),
    ])
}

/// The `emit_cpi!` event authority `["__event_authority"]`.
pub fn event_authority() -> Pubkey {
    pda(&[b"__event_authority"])
}
