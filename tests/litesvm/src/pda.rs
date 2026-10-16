//! Program-derived addresses of every short.win account (ARCH §2.3.1), host-side
//! (`find_program_address` is fine in tests; the program re-derives with stored bumps).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use shortwin::state::{
    GlobalConfig, LenderPosition, Market, MarketVault, OracleCache, POOL_VAULT_SEED, ShareEpoch,
    ShortPosition, TRADER_SEED, TRADER_WSOL_SEED, VenueExtras, VenueGuard, VenueKind,
    WalletExposure,
};

fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &shortwin::ID)
}

/// `["config"]`.
pub fn config() -> Pubkey {
    pda(&[GlobalConfig::SEED]).0
}

/// `["venue_guard"]`.
pub fn venue_guard() -> Pubkey {
    pda(&[VenueGuard::SEED]).0
}

/// `["venue_extras", venue]`.
pub fn venue_extras(kind: VenueKind) -> Pubkey {
    venue_extras_seed(kind as u8)
}

/// `["venue_extras", seed]` for a raw venue byte.
pub fn venue_extras_seed(seed: u8) -> Pubkey {
    pda(&[VenueExtras::SEED, &[seed]]).0
}

/// `["sol_usd"]`.
pub fn oracle_cache() -> Pubkey {
    pda(&[OracleCache::SEED]).0
}

/// `["market", mint]` and its bump.
pub fn market(mint: &Pubkey) -> (Pubkey, u8) {
    pda(&[Market::SEED, mint.as_ref()])
}

/// `["market_vault", market]` and its bump.
pub fn market_vault(market: &Pubkey) -> (Pubkey, u8) {
    pda(&[MarketVault::SEED, market.as_ref()])
}

/// `["pool_vault", market]` and its bump.
pub fn pool_vault(market: &Pubkey) -> (Pubkey, u8) {
    pda(&[POOL_VAULT_SEED, market.as_ref()])
}

/// `["trader", market]` and its bump.
pub fn trader(market: &Pubkey) -> (Pubkey, u8) {
    pda(&[TRADER_SEED, market.as_ref()])
}

/// `["trader_wsol", market]` and its bump.
pub fn trader_wsol(market: &Pubkey) -> (Pubkey, u8) {
    pda(&[TRADER_WSOL_SEED, market.as_ref()])
}

/// `["lender", market, owner]` and its bump.
pub fn lender(market: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    pda(&[LenderPosition::SEED, market.as_ref(), owner.as_ref()])
}

/// `["share_epoch", market, epoch u32 LE]` and its bump.
pub fn share_epoch(market: &Pubkey, epoch: u32) -> (Pubkey, u8) {
    pda(&[ShareEpoch::SEED, market.as_ref(), &epoch.to_le_bytes()])
}

/// `["position", market, borrower, nonce u64 LE]` and its bump.
pub fn position(market: &Pubkey, borrower: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    pda(&[
        ShortPosition::SEED,
        market.as_ref(),
        borrower.as_ref(),
        &nonce.to_le_bytes(),
    ])
}

/// `["exposure", market, borrower]` and its bump.
pub fn exposure(market: &Pubkey, borrower: &Pubkey) -> (Pubkey, u8) {
    pda(&[WalletExposure::SEED, market.as_ref(), borrower.as_ref()])
}

/// The `emit_cpi!` event authority `["__event_authority"]`.
pub fn event_authority() -> Pubkey {
    pda(&[b"__event_authority"]).0
}

/// short.win's own ProgramData.
pub fn program_data() -> Pubkey {
    Pubkey::find_program_address(&[shortwin::ID.as_ref()], &bpf_loader_upgradeable::ID).0
}

/// `ATA(owner, mint, token_program)`.
pub fn ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    anchor_spl::associated_token::get_associated_token_address_with_program_id(
        owner,
        mint,
        token_program,
    )
}

/// pump `["bonding-curve", mint]` (the detection key `create_market` derives, C0-15).
pub fn bonding_curve(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"bonding-curve", mint.as_ref()],
        &shortwin::constants::PUMP_PROGRAM_ID,
    )
    .0
}

/// pump's `["pool-authority", mint]`, the canonical pool's `creator` (cached by `create_market`
/// as `Market::pump_pool_authority`, M5-SEC-6).
pub fn pump_pool_authority(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"pool-authority", mint.as_ref()],
        &shortwin::constants::PUMP_PROGRAM_ID,
    )
    .0
}

/// The canonical PumpSwap pool of `mint` (R-VEN §1.3): `PDA(pAMM, ["pool", 0u16 LE,
/// PDA(pump, ["pool-authority", mint]), mint, WSOL])`.
pub fn canonical_pool(mint: &Pubkey) -> Pubkey {
    use shortwin::constants::{PUMP_AMM_PROGRAM_ID, PUMP_PROGRAM_ID, WSOL_MINT};
    let authority =
        Pubkey::find_program_address(&[b"pool-authority", mint.as_ref()], &PUMP_PROGRAM_ID).0;
    Pubkey::find_program_address(
        &[
            b"pool",
            &0u16.to_le_bytes(),
            authority.as_ref(),
            mint.as_ref(),
            WSOL_MINT.as_ref(),
        ],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}
