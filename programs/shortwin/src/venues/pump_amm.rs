//! PumpSwap adapter (ARCH §2.4.1 PumpSwap row, §2.5 "On → PumpSwap", §3.2, §3.12 "PumpSwap account
//! sets", §4.1–§4.2, §5.3, §5.4; R-VEN §1–§3 and its Updates 2026-09-29 / 2026-09-30). Program
//! `pAMMBay6…`; the canonical pool of a pump coin (`market.canonical_pool`, derived at listing,
//! never passed by a caller) traded with the v1 instructions `sell` (open, exact base in) and
//! `buy` (exits, exact base out), the market's `trader` PDA as PumpSwap's `user`.
//!
//! **Account sets** (order binding; W = writable in trades; every slot mandatory and key-checked
//! against a compile-time constant, the market's PumpSwap cache (`state::pump_swap_cache`), the
//! live `GlobalConfig` lists or an on-chain re-derivation, C0-2):
//!
//! - ReadSet ([`read_keys`]): P0 the canonical pool (W), P1 its base vault (W), P2 its WSOL vault
//!   (W), P3 `GlobalConfig` (`disable_flags`, the recipient lists, `creator_fee_configurable`), P4
//!   the PumpSwap `FeeConfig` (pump-fees), P5 `ATA(boost_vault, WSOL)` (may not exist; read as 0),
//!   P6 the pool's LP mint (M6R-1, owner decision of 2026-09-30; ARCH §3.12, changelog 56):
//!   key-checked against the live `pool.lp_mint` ([`check_read`]; no cache slot, no derivation on
//!   chain), an SPL Token or Token-2022 mint whose `supply` gives the pool's locked depth.
//! - TradeExtras ([`check_trade`]): T0 pAMM, T1 pAMM `event_authority`, T2 pump-fees, T3 WSOL,
//!   T4 Tokenkeg, T5 the ATA program, T6 System, T7 `trader_base_ata` (W), T8 `trader_wsol` (W),
//!   T9 `protocol_fee_recipient` (the live list), T10 its WSOL ATA (W), T11
//!   `coin_creator_vault_ata` (W), T12 `coin_creator_vault_authority` = pAMM `["creator_vault",
//!   pool.coin_creator]` (**re-derived** when the live coin creator is not the cached one), T13
//!   pAMM's global volume accumulator, T14 the trader's pAMM UVA (W), T15 the `pool-v2` marker,
//!   T16 `buyback_fee_recipient` (the live list), T17 its WSOL ATA (W), T18 `ATA(T14, WSOL)` (W;
//!   the cashback ATA). **Existence requirement** (C0-14, C1-8, C1-9): T10, T11, T14, T17 and, on
//!   a cashback coin, T18 must already be initialised (`VenueAccountMismatch` otherwise; the
//!   client or keeper prepends `create_idempotent` / `init_user_volume_accumulator` at its own
//!   cost), so PumpSwap never creates an account at the trader's expense (R-VEN Update H6: with
//!   them all present the venue takes no rent from `user` bar a short pool's growth).
//! - CPI ([`trade_accounts`]): exactly R-VEN §2.2's `buy` (23 fixed accounts) / `sell` (21: no
//!   global and user volume accumulators), then the remaining accounts cashback (`buy`: T18;
//!   `sell`: T18, T14; only on a cashback coin — mandatory there, 6061 / 6060 on the ELF, H7) →
//!   `pool-v2` (only when `coin_creator ≠ default`) → T16 → T17, then the extras (A41; the ELF
//!   accepts trailing accounts, H5). `trader` is the only signer. Data: `buy` discriminator ‖
//!   `base_amount_out` ‖ `max_quote_amount_in` ‖ `track_volume = [false]` (25 bytes); `sell`
//!   discriminator ‖ `base_amount_in` ‖ `min_quote_amount_out` (24 bytes).
//!
//! **Legs** (ARCH §2.4.1 PumpSwap row; §2.4 rules 2–3; T-ATA resolved: PumpSwap takes the
//! non-ATA `trader_wsol`, R-VEN Update H6):
//!
//! - [`sell`] (open): `transfer_checked(pool_vault → trader_base_ata, Q)` (the market signs);
//!   `ert` from the borrower to the trader and the ephemeral `trader_wsol` (its rent from the
//!   borrower); `sell(Q, min_out)` pays WSOL into `trader_wsol`; `S = trader_wsol.amount +
//!   trader.lamports − ert` (`VenueRentOnSell` if negative), so any venue-side rent `r` (a short
//!   pool's growth) comes out of `S`; `trader_wsol` closed into the position, the trader's
//!   lamports (`ert − r`) back to the borrower, and the caller refunds `rent(165) + r` from the
//!   position after its last CPI ([`SellOut::refund`]); the `Open` event reports `r`.
//! - [`buy`] (exits): `ert` straight from the position to the trader and `rent(165) + max_in` to
//!   `trader_wsol` (direct debits, each made visible to the runtime's per-CPI balance check),
//!   `buy(Q, max_in, [false])` paid from `trader_wsol` (**the trader PDA is `user`**: T-PDA-USER,
//!   pAMM buy path), `trader_wsol` closed back into the position, the trader's lamports back to
//!   it, then every token of `trader_base_ata` (`≥ Q`, else `VenueResultMismatch`; a surplus is a
//!   donation, A62) to the pool vault. `B = position before − after ≤ max_in + ert`.
//!
//! **Pricing** ([`read_state`]; ARCH §4.1–§4.2): `X` = base vault, `Y` = WSOL vault + VQR (VQR
//! read live and signed: negative from 2026-09-30, the sum in `[0, u64::MAX]`), `R = min(WSOL
//! vault − buckets, Y)` — the real SOL a sale can be paid out of and the tier's `R_now`, each the
//! smaller of the real and the effective SOL (ECON §6.2, the negative-VQR reading, delegated
//! PROPOSED, #103), the real SOL net of the fee buckets `Pool.protocol_fees` @271 +
//! `creator_fees` @279 that pump's 2026-10-02 redeploy keeps in the vault for v2 trades and pays
//! no sale out of (6063; PUMP-2026-10-1, `pump_amm::PoolReserves::payable_quote`; buckets above
//! the vault: not executable); the tier's virtual part is the boost `max(VQR + buckets, 0)`
//! (`PoolVenue::virtual_part`), since the buckets come off the VQR; the fee `fees::pump_amm` on
//! the pre-trade `(Y, X)` with the mint's **live supply** (the tier's market cap),
//! `FeeConfig.fee_tiers`, `creator_fee_configurable` and the pool's own creator override, resolved
//! on every read.
//!
//! **Locked depth** (M6R-1, owner decision of 2026-09-30; ARCH §2.5, §2.6, changelog 56; ECON
//! #105): canonical pools take third-party LP (R-VEN Update H11), so a PumpSwap pool gets the
//! Raydium depth bound on its **burned** creation LP: `D = pool_select::locked_depth(Pool.lp_supply,
//! lp_mint.supply, X, Y + VQR) = ⌊λ_bps·isqrt(X·(Y + VQR))/10⁴⌋`, `λ_bps = ⌊(lp_supply −
//! supply)·10⁴/lp_supply⌋` — `Pool.lp_supply` counts every LP unit minted less every unit
//! `withdraw` redeemed (burns are not subtracted) and `migrate_v2` burns the whole creation LP, so
//! the difference is the burned LP; a deposit or withdrawal scales `X`, `Y + VQR` and `lp_supply`
//! together and leaves `D` unchanged (up to λ's whole-bps floor). `VenueState.locked_depth` while
//! executable; the prologue writes it into `pool_d` and caps `L_slow` at it during a warm-up; the
//! size cap, the tier and the exit reference read `pool_d` over `λ_min` as on Raydium.
//!
//! **Venue predicate** (`venue_executable`, ARCH §5.3): every ReadSet account parses (A63; the LP
//! mint P6 included since M6R-1), the pool is the mint's valid canonical pool
//! (`canonical_pool_valid`) whose vaults are the cached ones, both reserves are non-zero, and
//! **`disable_flags & DISABLE_BUY_BIT == 0`** — the buy bit T-DISABLE resolved on the dumped ELF
//! (bit 3; the hard gate C0-10). Any other non-zero value (bits 0–2, 4–7) blocks PumpSwap opens
//! (`VenueFlagged`, A24) and is **not** S3 evidence; bit 0 (`create_pool`) also blocks curve opens
//! (`GraduationHalted`, M6-SEC-3: [`PAMM_CREATE_POOL_DISABLED_BIT`], read from the curve's C6).
//!
//! **Graduation** ([`graduate`]; ARCH §2.5 "On → PumpSwap", M5-SEC-4): a curve market whose
//! canonical pool is valid moves onto it in the prologue's venue sync: the pool cache (the vault
//! keys from the pool, the rest derived at listing), `graduated_at = now`, both route chains and
//! a collection window cleared, the tier C until the next crank; the observation state is
//! rewritten to the new pool and **restarted exactly as a Raydium adoption restarts it**
//! (`observe::rewrite_liquidity(anchor, RaydiumLocked { slow_cap: anchor })` then `mark_restart`:
//! `ema_warm = 8`, `ema_guard = 1`), with the pool's `lp_supply` as the anchor — the pool's
//! constant-product depth `√(X·Y)` at its creation, read from the canonical-pool account the
//! curve's ReadSet already carries (no vault is sampled; a deposit around the transition can only
//! raise it, and `L_slow` never rises above the curve's own; since M6R-1 the first executable
//! PumpSwap observation caps `L_slow` at the pool's locked depth `D`, which undoes such a deposit
//! before any decision reads `L_slow`). `pool_d` is 0 from the transition (it reads no LP mint)
//! until that observation. `P_ema` carries over and the next eight integrations re-seed it from
//! the pool's own buckets (by at most the largest v0.4.1 step each way, M4X-1 up, M6-SEC-1 down),
//! so an atomic, self-financed `[buy_v2(rT), migrate_v2]` with the pool pushed before any
//! observation forces no exit before honest buckets exist: `liquidate` / `expire` fail
//! `EmaWarmingUp` and `sol_settle` evaluates nothing until `ema_warm == 0`, and opens wait too (the
//! 8–12 s exit delay after a graduation, accepted by the owner on 2026-09-30, OWNER-M6R-4).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::system_program::{self, Transfer};
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token_interface::{self, TransferChecked};
use shortwin_math::fees::{self, FeeBps, FeeTier, PammFeeSchedule};
use shortwin_math::num::{mul_div, Rounding, WAD};
use shortwin_math::observe::{Observation, VenueChange};
use shortwin_math::pool_select;
use shortwin_math::quote::Pricing;
use shortwin_venue_state::{mint, pump, pump_amm, token_account};

use super::{raydium, BuyOut, Leg, SellOut, VenueState};
use crate::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, PUMPSWAP_LOCK_MIN_BPS, PUMP_AMM_EVENT_AUTHORITY,
    PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
    PUMP_AMM_PROGRAM_ID, PUMP_FEES_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::engine::trader::{base_ata_state, token_state, Funding, TraderAccounts};
use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::lamports;
use crate::state::{pump_swap_cache as cache, Market, Params, VenueKind};

/// ReadSet length (7 since M6R-1: the LP mint P6).
pub const READ_LEN: usize = 7;
/// TradeExtras length.
pub const TRADE_LEN: usize = 19;

/// ReadSet slots.
pub const P_POOL: usize = 0;
pub const P_BASE_VAULT: usize = 1;
pub const P_QUOTE_VAULT: usize = 2;
pub const P_GLOBAL_CONFIG: usize = 3;
pub const P_FEE_CONFIG: usize = 4;
pub const P_BOOST_VAULT_ATA: usize = 5;
/// The pool's LP mint (M6R-1): key-checked against `pool.lp_mint`, read for its `supply`.
pub const P_LP_MINT: usize = 6;

/// TradeExtras slots.
pub const T_PROGRAM: usize = 0;
pub const T_EVENT_AUTHORITY: usize = 1;
pub const T_FEE_PROGRAM: usize = 2;
pub const T_WSOL_MINT: usize = 3;
pub const T_TOKEN_PROGRAM: usize = 4;
pub const T_ATA_PROGRAM: usize = 5;
pub const T_SYSTEM: usize = 6;
pub const T_TRADER_BASE_ATA: usize = 7;
pub const T_TRADER_WSOL: usize = 8;
pub const T_PROTOCOL_FEE_RECIPIENT: usize = 9;
pub const T_PROTOCOL_FEE_RECIPIENT_ATA: usize = 10;
pub const T_COIN_CREATOR_VAULT_ATA: usize = 11;
pub const T_COIN_CREATOR_VAULT_AUTHORITY: usize = 12;
pub const T_GLOBAL_VOLUME_ACCUMULATOR: usize = 13;
pub const T_UVA: usize = 14;
pub const T_POOL_V2: usize = 15;
pub const T_BUYBACK_RECIPIENT: usize = 16;
pub const T_BUYBACK_RECIPIENT_ATA: usize = 17;
pub const T_CASHBACK_ATA: usize = 18;

/// `sha256("global:buy")[..8]` (R-VEN §2.1).
pub const IX_BUY: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
/// `sha256("global:sell")[..8]`.
pub const IX_SELL: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];
/// `sha256("global:init_user_volume_accumulator")[..8]` (the same Anchor name as pump's).
pub const IX_INIT_USER_VOLUME_ACCUMULATOR: [u8; 8] =
    [0x5e, 0x06, 0xca, 0x73, 0xff, 0x60, 0xe8, 0xb7];
/// `sha256("global:claim_cashback")[..8]` (the same Anchor name as pump's; ARCH [V11]).
pub const IX_CLAIM_CASHBACK: [u8; 8] = [0x25, 0x3a, 0x23, 0x7e, 0xbe, 0x35, 0xe4, 0xc5];
/// The length of a `buy`'s data: discriminator, amount, bound, `track_volume = [false]`.
pub const BUY_DATA_LEN: usize = 25;
/// The length of a `sell`'s data: discriminator, amount, bound.
pub const SELL_DATA_LEN: usize = 24;

/// T-DISABLE (the hard gate, C0-10; R-VEN Update 2026-09-30 H4): the `GlobalConfig.disable_flags`
/// bit that halts every buy — ARCH §5.3's `PAMM_BUY_DISABLED_BIT`, the only bit an exit depends on.
pub const PAMM_BUY_DISABLED_BIT: u8 = pump_amm::DISABLE_BUY_BIT;

/// T-DISABLE: the `GlobalConfig.disable_flags` bit that halts `create_pool` — and with it pump's
/// `migrate_v2` (6017), so a curve that completes under it stays `PumpMigrating` until S3. Curve
/// opens refuse `GraduationHalted` while it is set (M6-SEC-3, owner decision of 2026-09-30; the
/// curve's ReadSet carries the PumpSwap `GlobalConfig` as C6, so no account is added).
pub const PAMM_CREATE_POOL_DISABLED_BIT: u8 = pump_amm::DISABLE_CREATE_POOL_BIT;

/// PumpSwap's protocol-fee recipient that pump's curve does not list (the live `GlobalConfig`,
/// slot 451,784,607; R-VEN §2.2 "`JCRGum…` and not `CebN5W…`"), with its WSOL ATA: with
/// `pump_curve::RECIPIENT_WSOL_ATAS` (every other PumpSwap recipient, the same keys as pump's) a
/// trade derives no recipient ATA (M5-SEC-6). `tests::the_recipient_tables_re_derive`.
pub const EXTRA_RECIPIENT_WSOL_ATAS: [(Pubkey, Pubkey); 1] = [(
    pubkey!("JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU"),
    pubkey!("DWpvfqzGWuVy9jVSKSShdM2733nrEsnnhsUStYbkj6Nn"),
)];

// ---- keys --------------------------------------------------------------------------------------

/// `ATA(owner, WSOL)` under SPL Token.
fn wsol_ata(owner: &Pubkey) -> Pubkey {
    super::pump_curve::wsol_ata(owner)
}

/// `ATA(recipient, WSOL)` of a protocol-fee or buyback recipient: the tables, else derived.
pub fn recipient_wsol_ata(recipient: &Pubkey) -> Pubkey {
    EXTRA_RECIPIENT_WSOL_ATAS
        .iter()
        .find(|(r, _)| r == recipient)
        .map_or_else(
            || super::pump_curve::recipient_wsol_ata(recipient),
            |(_, ata)| *ata,
        )
}

/// `PDA(pAMM, ["pool-v2", mint])` (a marker; need not exist).
pub fn pool_v2(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump_amm::POOL_V2_SEED, mint.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}

/// `PDA(pAMM, ["boost_vault", pool])`.
pub fn boost_vault(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump_amm::BOOST_VAULT_SEED, pool.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}

/// `PDA(pAMM, ["user_volume_accumulator", user])`.
pub fn user_volume_accumulator(user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump_amm::USER_VOLUME_ACCUMULATOR_SEED, user.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}

/// `PDA(pAMM, ["pool_lp_mint", pool])`: the LP mint `create_pool` creates (every canonical
/// pool's `pool.lp_mint`). Off-chain helper for [`read_keys`]'s P6; the program key-checks P6
/// against the live `pool.lp_mint` instead ([`check_read`]).
pub fn lp_mint(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump_amm::LP_MINT_SEED, pool.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}

/// `PDA(pAMM, ["creator_vault", coin_creator])` (underscore; ARCH §3.12 seed gotchas).
pub fn coin_creator_vault_authority(coin_creator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump_amm::CREATOR_VAULT_SEED, coin_creator.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}

/// The market's PumpSwap cache entry `i` (`state::pump_swap_cache`), the default key if absent.
pub fn cached(market: &Market, i: usize) -> Pubkey {
    market.pool_cache.get(i).copied().unwrap_or_default()
}

fn set_cached(market: &mut Market, i: usize, key: Pubkey) -> Result<()> {
    *market
        .pool_cache
        .get_mut(i)
        .ok_or(ShortwinError::MathOverflow)? = key;
    Ok(())
}

/// Derives every PumpSwap cache entry that is still empty, and the coin creator's pair when the
/// cached creator is not `coin_creator` (`find_program_address`: listing and transition only,
/// never a trade; M5-SEC-6).
fn fill_derived(market: &mut Market, coin_creator: &Pubkey) -> Result<()> {
    if cached(market, cache::POOL_V2_MARKER) == Pubkey::default() {
        let k = pool_v2(&market.mint);
        set_cached(market, cache::POOL_V2_MARKER, k)?;
    }
    if cached(market, cache::BOOST_VAULT) == Pubkey::default() {
        let v = boost_vault(&market.canonical_pool);
        set_cached(market, cache::BOOST_VAULT, v)?;
        set_cached(market, cache::BOOST_VAULT_ATA, wsol_ata(&v))?;
    }
    if cached(market, cache::PAMM_UVA) == Pubkey::default() {
        let u = user_volume_accumulator(&market.trader);
        set_cached(market, cache::PAMM_UVA, u)?;
        set_cached(market, cache::PAMM_UVA_QUOTE_ATA, wsol_ata(&u))?;
    }
    cache_creator(market, coin_creator)?;
    Ok(())
}

/// Caches `coin_creator` with its pAMM vault authority and that authority's WSOL ATA unless it is
/// already the cached creator (two `find_program_address` only when it writes). Returns whether it
/// wrote.
fn cache_creator(market: &mut Market, coin_creator: &Pubkey) -> Result<bool> {
    if cached(market, cache::CACHED_COIN_CREATOR) == *coin_creator
        && cached(market, cache::CACHED_COIN_CREATOR_VAULT_AUTH) != Pubkey::default()
    {
        return Ok(false);
    }
    let a = coin_creator_vault_authority(coin_creator);
    set_cached(market, cache::CACHED_COIN_CREATOR, *coin_creator)?;
    set_cached(market, cache::CACHED_COIN_CREATOR_VAULT_AUTH, a)?;
    set_cached(market, cache::CACHED_COIN_CREATOR_VAULT_ATA, wsol_ata(&a))?;
    Ok(true)
}

/// M6-SEC-4: re-caches the coin creator of a `PumpSwap` market whose pool (P0 of its key-checked
/// ReadSet) names another one. pAMM's `set_coin_creator` (a canonical pool without a coin creator
/// takes its Metaplex metadata creator or its curve's creator; verified on the dumped ELF with no
/// signer) and `migrate_pool_coin_creator` (to the mint's pump-fees sharing config; IDL: no signer)
/// let anyone change it; until the cache follows, every trade re-derives T12 / T11
/// (`creator_accounts`: two `find_program_address`, M5-SEC-6's pattern) and an extras template
/// naming the coin creator follows the stale cache. `crank_price` calls it after its prologue, so
/// one crank ends both. A pool that does not parse changes nothing (A63). Returns whether it wrote.
pub fn refresh_coin_creator(market: &mut Market, read: &[AccountInfo]) -> Result<bool> {
    let Some(pool) = read.get(P_POOL).and_then(parse_pool) else {
        return Ok(false);
    };
    cache_creator(market, &Pubkey::new_from_array(pool.coin_creator))
}

/// The PumpSwap cache of a **pump-curve** market, derived once at its listing: the canonical
/// pool's vaults (ATAs of the pool — `migrate_v2` creates them so, `NewPool` in the harness), the
/// `pool-v2` marker, the boost vault and its WSOL ATA, the trader's pAMM UVA and its WSOL ATA, the
/// curve creator (the pool's coin creator at migration) with its pAMM vault authority and ATA.
/// Nothing reads them before the `→ PumpSwap` transition, which then only compares them with the
/// pool (a changed creator is re-derived there). Cold path (M5-SEC-6: no derivation on a trade,
/// nor on the transition crank of a market listed on the curve). Not called by `crank_price`'s
/// `None →` curve re-detection, which would exceed the crank's CU limit with it: such a market's
/// transition derives the entries still empty ([`write_cache`]).
pub fn precompute(market: &mut Market, coin_creator: &Pubkey) -> Result<()> {
    let pool = market.canonical_pool;
    if cached(market, cache::POOL_BASE_VAULT) == Pubkey::default() {
        let base = get_associated_token_address_with_program_id(
            &pool,
            &market.mint,
            &market.token_program,
        );
        set_cached(market, cache::POOL_BASE_VAULT, base)?;
    }
    if cached(market, cache::POOL_QUOTE_VAULT) == Pubkey::default() {
        set_cached(market, cache::POOL_QUOTE_VAULT, wsol_ata(&pool))?;
    }
    fill_derived(market, coin_creator)
}

/// Writes the PumpSwap cache of a market from its (valid, canonical) pool as read now: the vault
/// keys from the pool itself, the derived entries if still empty, the coin creator's pair if the
/// creator is not the cached one (ARCH §2.5 "On → PumpSwap": pool offsets 139 / 171, the `pool-v2`
/// marker, `boost_vault_ata`, the pAMM UVA and its ATA, the coin-creator vault).
pub fn write_cache(market: &mut Market, pool: &pump_amm::Pool) -> Result<()> {
    set_cached(
        market,
        cache::POOL_BASE_VAULT,
        Pubkey::new_from_array(pool.pool_base_token_account),
    )?;
    set_cached(
        market,
        cache::POOL_QUOTE_VAULT,
        Pubkey::new_from_array(pool.pool_quote_token_account),
    )?;
    fill_derived(market, &Pubkey::new_from_array(pool.coin_creator))?;
    if market.pump_uva == Pubkey::default() {
        // `sweep_trader`'s PumpSwap token leg names the trader's pump UVA too (curve-era
        // cashback; a market listed on PumpSwap never had one: it reads as nothing claimable).
        market.pump_uva = super::pump_curve::user_volume_accumulator(&market.trader);
    }
    Ok(())
}

/// P0–P5 of the ReadSet of `market` (the keys the constants and the market's cache fix).
fn fixed_read_keys(market: &Market) -> [Pubkey; P_LP_MINT] {
    [
        market.canonical_pool,
        cached(market, cache::POOL_BASE_VAULT),
        cached(market, cache::POOL_QUOTE_VAULT),
        PUMP_AMM_GLOBAL_CONFIG,
        PUMP_AMM_FEE_CONFIG,
        cached(market, cache::BOOST_VAULT_ATA),
    ]
}

/// The ReadSet keys of `market` in order (module docs), P6 derived as `create_pool` derives it
/// ([`lp_mint`]; off-chain builders — the program checks P6 against the live pool).
pub fn read_keys(market: &Market) -> [Pubkey; READ_LEN] {
    let [p0, p1, p2, p3, p4, p5] = fixed_read_keys(market);
    [p0, p1, p2, p3, p4, p5, lp_mint(&market.canonical_pool)]
}

/// Key-checks the ReadSet (`BadRemainingAccounts` for a wrong count, `VenueAccountMismatch` for a
/// wrong key): P0–P5 against the constants and the market's cache, P6 against the live pool's
/// `lp_mint` (M6R-1; the pool P0 is the market's canonical pool). A pool that does not parse leaves
/// P6 unchecked: the venue is then not executable and nothing reads P6 (A63).
pub fn check_read(market: &Market, read: &[AccountInfo]) -> Result<()> {
    require!(read.len() == READ_LEN, ShortwinError::BadRemainingAccounts);
    let fixed = read
        .get(..P_LP_MINT)
        .ok_or(ShortwinError::BadRemainingAccounts)?;
    raydium::check_keys(fixed, &fixed_read_keys(market))?;
    if let Some(pool) = read.get(P_POOL).and_then(parse_pool) {
        require_keys_eq!(
            raydium::at(read, P_LP_MINT)?.key(),
            Pubkey::new_from_array(pool.lp_mint),
            ShortwinError::VenueAccountMismatch
        );
    }
    Ok(())
}

/// The circulating supply of the pool's LP mint (P6; SPL Token or Token-2022, `supply` @36);
/// `None` if it is not a mint of either program (A63: the venue is then not executable).
fn lp_mint_supply(info: &AccountInfo) -> Option<u64> {
    if *info.owner != TOKEN_2022_PROGRAM_ID && *info.owner != TOKEN_PROGRAM_ID {
        return None;
    }
    let data = info.try_borrow_data().ok()?;
    mint::read_base(&info.owner.to_bytes(), &data)
        .ok()
        .map(|m| m.supply)
}

/// The TradeExtras slots whose keys are fixed by a constant or the market's cache.
fn fixed_trade_keys(market: &Market) -> [(usize, Pubkey); 13] {
    [
        (T_PROGRAM, PUMP_AMM_PROGRAM_ID),
        (T_EVENT_AUTHORITY, PUMP_AMM_EVENT_AUTHORITY),
        (T_FEE_PROGRAM, PUMP_FEES_PROGRAM_ID),
        (T_WSOL_MINT, WSOL_MINT),
        (T_TOKEN_PROGRAM, TOKEN_PROGRAM_ID),
        (T_ATA_PROGRAM, ASSOCIATED_TOKEN_PROGRAM_ID),
        (T_SYSTEM, anchor_lang::system_program::ID),
        (T_TRADER_BASE_ATA, market.trader_base_ata),
        (T_TRADER_WSOL, market.trader_wsol),
        (
            T_GLOBAL_VOLUME_ACCUMULATOR,
            PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
        ),
        (T_UVA, cached(market, cache::PAMM_UVA)),
        (T_POOL_V2, cached(market, cache::POOL_V2_MARKER)),
        (T_CASHBACK_ATA, cached(market, cache::PAMM_UVA_QUOTE_ATA)),
    ]
}

/// The pool account of a key-checked ReadSet, parsed; `None` if it does not parse.
fn parse_pool(info: &AccountInfo) -> Option<pump_amm::Pool> {
    if *info.owner != PUMP_AMM_PROGRAM_ID {
        return None;
    }
    let data = info.try_borrow_data().ok()?;
    pump_amm::read_pool(&info.owner.to_bytes(), &data).ok()
}

/// `info` is an initialised SPL Token WSOL account (the existence requirement's test).
fn is_wsol_account(info: &AccountInfo) -> bool {
    if *info.owner != TOKEN_PROGRAM_ID || info.data_is_empty() {
        return false;
    }
    token_state(info).is_ok_and(|t| t.mint == WSOL_MINT.to_bytes())
}

/// The trader's pAMM UVA (T14) exists and accumulates for `trader` (the existence requirement).
fn uva_ok(uva: &AccountInfo, trader: &Pubkey) -> bool {
    if *uva.owner != PUMP_AMM_PROGRAM_ID {
        return false;
    }
    let Ok(data) = uva.try_borrow_data() else {
        return false;
    };
    pump_amm::read_user_volume_accumulator(&uva.owner.to_bytes(), &data)
        .is_ok_and(|u| u.user == trader.to_bytes())
}

/// The coin-creator vault authority and its WSOL ATA of `coin_creator`: the market's cache when
/// it is the cached creator, else re-derived (a coin-creator change, ARCH §5.4 case 4).
fn creator_accounts(market: &Market, coin_creator: &Pubkey) -> (Pubkey, Pubkey) {
    if *coin_creator == cached(market, cache::CACHED_COIN_CREATOR)
        && cached(market, cache::CACHED_COIN_CREATOR_VAULT_AUTH) != Pubkey::default()
    {
        (
            cached(market, cache::CACHED_COIN_CREATOR_VAULT_AUTH),
            cached(market, cache::CACHED_COIN_CREATOR_VAULT_ATA),
        )
    } else {
        let a = coin_creator_vault_authority(coin_creator);
        (a, wsol_ata(&a))
    }
}

/// Key-checks the TradeExtras (module docs; `BadRemainingAccounts` for a wrong count,
/// `VenueAccountMismatch` for a wrong key or a missing account the existence requirement names):
/// the fixed and cached slots; T9 / T16 against the live `GlobalConfig`'s protocol-fee and
/// buyback lists; T10 / T17 as their WSOL ATAs; T11 / T12 from the live pool's `coin_creator`;
/// T10, T11, T14 (for `trader`), T17 and — on a cashback coin — T18 initialised. A pool or
/// `GlobalConfig` that no longer parses (a venue upgrade, A63) leaves its dependent slots
/// unchecked here: the venue is then not executable and no leg runs.
pub fn check_trade(market: &Market, read: &[AccountInfo], trade: &[AccountInfo]) -> Result<()> {
    require!(
        trade.len() == TRADE_LEN,
        ShortwinError::BadRemainingAccounts
    );
    for (i, key) in fixed_trade_keys(market) {
        require_keys_eq!(
            raydium::at(trade, i)?.key(),
            key,
            ShortwinError::VenueAccountMismatch
        );
    }
    require!(
        raydium::at(trade, T_PROGRAM)?.executable,
        ShortwinError::VenueAccountMismatch
    );
    let protocol = raydium::at(trade, T_PROTOCOL_FEE_RECIPIENT)?.key();
    let buyback = raydium::at(trade, T_BUYBACK_RECIPIENT)?.key();
    if let Some(gc) = read.get(P_GLOBAL_CONFIG) {
        if let Ok(data) = gc.try_borrow_data() {
            if let Ok(g) = pump_amm::read_global_config(&gc.owner.to_bytes(), &data) {
                require!(
                    g.is_protocol_fee_recipient(&protocol.to_bytes()),
                    ShortwinError::VenueAccountMismatch
                );
                require!(
                    g.is_buyback_fee_recipient(&buyback.to_bytes()),
                    ShortwinError::VenueAccountMismatch
                );
            }
        }
    }
    for (slot, key) in [
        (T_PROTOCOL_FEE_RECIPIENT_ATA, recipient_wsol_ata(&protocol)),
        (T_BUYBACK_RECIPIENT_ATA, recipient_wsol_ata(&buyback)),
    ] {
        let info = raydium::at(trade, slot)?;
        require_keys_eq!(info.key(), key, ShortwinError::VenueAccountMismatch);
        require!(is_wsol_account(info), ShortwinError::VenueAccountMismatch);
    }
    let uva = raydium::at(trade, T_UVA)?;
    require!(
        uva_ok(uva, &market.trader),
        ShortwinError::VenueAccountMismatch
    );
    if let Some(pool) = read.get(P_POOL).and_then(parse_pool) {
        let (auth, ata) = creator_accounts(market, &Pubkey::new_from_array(pool.coin_creator));
        require_keys_eq!(
            raydium::at(trade, T_COIN_CREATOR_VAULT_AUTHORITY)?.key(),
            auth,
            ShortwinError::VenueAccountMismatch
        );
        let ata_info = raydium::at(trade, T_COIN_CREATOR_VAULT_ATA)?;
        require_keys_eq!(ata_info.key(), ata, ShortwinError::VenueAccountMismatch);
        require!(
            is_wsol_account(ata_info),
            ShortwinError::VenueAccountMismatch
        );
        if pool.is_cashback_coin {
            require!(
                is_wsol_account(raydium::at(trade, T_CASHBACK_ATA)?),
                ShortwinError::VenueAccountMismatch
            );
        }
    }
    Ok(())
}

// ---- state -------------------------------------------------------------------------------------

/// What a PumpSwap market's opens and tier read besides the pricing reserves (ARCH §3.4 steps 3–4,
/// §4.7), present whenever the pool and `GlobalConfig` parse — also while the venue cannot execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolVenue {
    /// `GlobalConfig.disable_flags` (A24: any non-zero value blocks opens; the buy bit is no
    /// route).
    pub disable_flags: u8,
    /// The boost vault's WSOL (0 when its ATA does not exist) — the boost gate (A13).
    pub boost_amount: u64,
    /// The virtual part of the effective reserve, `max(VQR + buckets, 0)` (the boost;
    /// `pump_amm::Pool::virtual_part`, PUMP-2026-10-1): what the tier's `R_slow` and the burned-LP
    /// bound subtract. `max(VQR, 0)` on a pool without fee buckets; 0 under a negative VQR.
    pub virtual_part: u64,
    /// `Pool.is_cashback_coin`.
    pub is_cashback: bool,
    /// The live fee resolved on the pool's state (execution quotes when executable; the risk
    /// view keeps it while a halt makes the venue not executable). `None` if it does not resolve.
    pub fees: Option<FeeBps>,
    /// M7-SEC-5: the one-way fee (ppm) the schedule charges at the stored `P_ema`'s market cap;
    /// `u32::MAX` without a price or when it does not resolve (`VenueState::cap_fee_ppm`).
    pub fee_ppm_at_ema: u32,
}

/// A token account of `program` whose mint is `mint` and token owner `owner`, parsed.
fn vault_of(
    info: &AccountInfo,
    program: &Pubkey,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Option<token_account::TokenAccount> {
    let t = raydium::token_of(info, program)?;
    (t.mint == mint.to_bytes() && t.owner == owner.to_bytes()).then_some(t)
}

/// A pump-fees `FeeTiers` list as `shortwin-math` tiers (account order).
fn math_tiers(tiers: pump::FeeTiers<'_>) -> Vec<FeeTier> {
    tiers
        .iter()
        .map(|t| FeeTier {
            threshold: t.threshold,
            fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
        })
        .collect()
}

/// The venue state of a `PumpSwap` market from its key-checked ReadSet (module docs), with the
/// mint's live `supply` (the fee tier's market cap; `None` makes the venue not executable). Never
/// fails: whatever does not parse is "not executable" (A63). `ema_fee`: also resolve the fee at the
/// stored `P_ema`'s market cap (`PoolVenue::fee_ppm_at_ema`, M7-SEC-5) — only `open_short`'s
/// pre-trade read needs it, so no other instruction pays its compute.
pub fn read_state(
    market: &Market,
    read: &[AccountInfo],
    supply: Option<u64>,
    ema_fee: bool,
) -> VenueState {
    let mut st = VenueState::not_executable(VenueKind::PumpSwap);
    let Some(pool_info) = read.get(P_POOL) else {
        return st;
    };
    let Some(pool) = parse_pool(pool_info) else {
        return st;
    };
    let authority = super::detect::pool_authority_of(market);
    let canonical = pool.index == pump_amm::CANONICAL_POOL_INDEX
        && pool.creator == authority.to_bytes()
        && pool.base_mint == market.mint.to_bytes()
        && pool.quote_mint == WSOL_MINT.to_bytes()
        && !pool.is_mayhem_mode;
    let (
        Some(base_info),
        Some(quote_info),
        Some(gc_info),
        Some(fc_info),
        Some(boost_info),
        Some(lp_info),
    ) = (
        read.get(P_BASE_VAULT),
        read.get(P_QUOTE_VAULT),
        read.get(P_GLOBAL_CONFIG),
        read.get(P_FEE_CONFIG),
        read.get(P_BOOST_VAULT_ATA),
        read.get(P_LP_MINT),
    )
    else {
        return st;
    };
    let vaults_match = pool.pool_base_token_account == base_info.key.to_bytes()
        && pool.pool_quote_token_account == quote_info.key.to_bytes();
    if !canonical || !vaults_match {
        return st;
    }
    let pool_key = pool_info.key();
    let Some(base) = vault_of(base_info, &market.token_program, &market.mint, &pool_key) else {
        return st;
    };
    let Some(quote) = vault_of(quote_info, &TOKEN_PROGRAM_ID, &WSOL_MINT, &pool_key) else {
        return st;
    };
    let Ok(gc_data) = gc_info.try_borrow_data() else {
        return st;
    };
    let Ok(gc) = pump_amm::read_global_config(&gc_info.owner.to_bytes(), &gc_data) else {
        return st;
    };
    let Ok(fc_data) = fc_info.try_borrow_data() else {
        return st;
    };
    let Ok(fc) = pump::read_fee_config(&fc_info.owner.to_bytes(), &fc_data) else {
        return st;
    };
    let boost_amount = {
        let Ok(data) = boost_info.try_borrow_data() else {
            return st;
        };
        let Ok(a) = pump_amm::boost_vault_amount(
            &boost_info.owner.to_bytes(),
            &data,
            &cached(market, cache::BOOST_VAULT).to_bytes(),
        ) else {
            return st;
        };
        a
    };
    // `None` also when the fee buckets exceed the quote vault (PUMP-2026-10-1).
    let Some(reserves) = pool.reserves(base.amount, quote.amount) else {
        return st;
    };
    let tiers = math_tiers(fc.fee_tiers);
    let schedule = supply.map(|s| PammFeeSchedule {
        tiers: &tiers,
        base_supply: s,
        creator_fee_configurable: gc.creator_fee_configurable(),
        pool_creator_fee_bps: pool.creator_fee_bps,
        coin_creator_is_default: pool.coin_creator_is_default(),
        cap_bps: None,
    });
    let fee = schedule.as_ref().and_then(|sch| {
        fees::pump_amm(reserves.effective_quote, u128::from(reserves.base), sch).ok()
    });
    // M7-SEC-5: the fee at the stored `P_ema`'s market cap (the tier's market cap is
    // `effective_quote·supply/base`, i.e. the price times the supply).
    let p_ema = market.ema_price.get();
    let fee_ema = schedule
        .as_ref()
        .filter(|_| ema_fee && p_ema > 0)
        .and_then(|sch| {
            let base = u128::from(reserves.base);
            let quote = mul_div(p_ema, base, WAD, Rounding::Floor).ok()?;
            fees::pump_amm(quote, base, sch).ok()
        });
    let disable_flags = gc.disable_flags();
    st.pool = Some(PoolVenue {
        disable_flags,
        boost_amount,
        virtual_part: pool.virtual_part().unwrap_or(u64::MAX),
        is_cashback: pool.is_cashback_coin,
        fees: fee,
        fee_ppm_at_ema: fee_ema.map_or(u32::MAX, |e| e.oneway_ppm()),
    });
    let Ok(effective) = u64::try_from(reserves.effective_quote) else {
        return st;
    };
    // M6R-1: the burned creation LP's depth on the pricing reserves (module docs). The LP mint
    // is P6, key-checked against `pool.lp_mint` by `check_read`; one that does not parse makes
    // the venue not executable (A63).
    let locked_depth = (lp_info.key.to_bytes() == pool.lp_mint)
        .then(|| lp_mint_supply(lp_info))
        .flatten()
        .and_then(|supply| {
            pool_select::locked_depth(pool.lp_supply, supply, reserves.base, effective).ok()
        });
    st.executable = reserves.base > 0
        && effective > 0
        && fee.is_some()
        && locked_depth.is_some()
        && disable_flags & PAMM_BUY_DISABLED_BIT == 0;
    if st.executable {
        if let (Some(f), Some(d)) = (fee, locked_depth) {
            st.x = reserves.base;
            st.y = effective;
            // ECON §6.2 (#103, delegated PROPOSED; built in M6): the real SOL is the smaller of
            // the payable SOL and the effective reserve — the tier's `R_now` under a negative
            // VQR, and the sale's payout cap (which cannot bind below the effective reserve).
            // Payable: the WSOL vault less the fee buckets (PUMP-2026-10-1; PumpSwap pays no sale
            // out of them, 6063).
            st.r = reserves.payable_quote.min(effective);
            st.pricing = Some(Pricing::PumpSwap(f));
            st.fee_ppm = f.oneway_ppm();
            st.locked_depth = d;
        }
    }
    st
}

/// The risk view of a PumpSwap market (`venues::risk_quoter`'s arm, V2-1, A43): while the venue
/// executes its live fee capped (the caller's `quoter()` path); a halted pool whose fee still
/// resolves (a `disable_flags` buy halt) keeps that fee, capped; otherwise the cap itself
/// (`cap_only`: a layout change, no readable `φ_live`).
pub fn fee_view(state: &VenueState, cap_bps: u64) -> Pricing {
    match state.pool.and_then(|p| p.fees) {
        Some(f) => Pricing::PumpSwap(f.capped(cap_bps)),
        None => Pricing::PumpSwap(FeeBps::new(0, cap_bps, 0)),
    }
}

/// The boost gate (ARCH §3.4 step 4, §5.4 case 3; A13, ADOPTED Q15, C1-2): opens are refused
/// while `boost_vault.amount > boost_dust_lamports ∧ now < graduated_at + boost_window_s`.
pub fn boost_blocks(pool: &PoolVenue, params: &Params, graduated_at: i64, now: i64) -> bool {
    pool.boost_amount > params.boost_dust_lamports
        && now < graduated_at.saturating_add(i64::from(params.boost_window_s))
}

/// The unclaimed PumpSwap cashback of the trader: the WSOL in `ATA(pAMM UVA(trader), WSOL)` (R-VEN
/// Update H7: PumpSwap accrues it there and `claim_cashback` moves the whole balance), 0 when the
/// account does not exist (the normal state of a trader that never traded a cashback coin), and
/// `u64::MAX` for an account there that is not a WSOL token account (fail closed: nothing proves
/// it empty). `ata` is key-checked by the caller.
pub fn cashback_claimable(ata: &AccountInfo) -> u64 {
    if ata.data_is_empty() && *ata.owner == anchor_lang::system_program::ID {
        return 0;
    }
    if *ata.owner != TOKEN_PROGRAM_ID {
        return u64::MAX;
    }
    match token_state(ata) {
        Ok(t) if t.mint == WSOL_MINT.to_bytes() => t.amount,
        _ => u64::MAX,
    }
}

// ---- graduation (ARCH §2.5 "On → PumpSwap") -----------------------------------------------------

/// The graduation's liquidity anchor (module docs): the pool's `lp_supply`, its `√(X·Y)` at
/// creation (the LP `create_pool` mints on the effective deposit; e.g. 4,193,388,282,604 on the
/// `fresh` fixture, whose `√(X·Y)` was within 0.07 % of it 65 s after its migration), read from
/// the canonical-pool account alone; `None` for a zero supply.
pub fn graduation_anchor(pool: &pump_amm::Pool) -> Option<u128> {
    (pool.lp_supply > 0).then(|| u128::from(pool.lp_supply))
}

/// The venue-side effects of `→ PumpSwap` on a market with the valid canonical `pool` (ARCH §2.5;
/// module docs): the PumpSwap cache, `venue = PumpSwap`, `graduated_at = now` (the first
/// observation; conservative, O13), `pool_d = 0` (M6R-1: no LP mint is read here), both route
/// chains and a collection window cleared (the new pool is observed afresh), the tier C until the
/// next crank or open recomputes it. The observation state is [`restart_observation`]'s (the
/// prologue's copy).
pub fn graduate(market: &mut Market, pool: &pump_amm::Pool, now: i64) -> Result<()> {
    write_cache(market, pool)?;
    market.venue = VenueKind::PumpSwap as u8;
    market.graduated_at = now;
    // M6R-1: the transition reads no LP mint; the pool's first executable observation writes
    // its locked depth (the prologue's `refresh_locked_depth`).
    market.pool_d.set(0);
    market.no_route_since = 0;
    market.last_no_route_obs = 0;
    market.pool_fail_since = 0;
    market.last_pool_fail_obs = 0;
    crate::engine::pool_select::clear_window(market);
    market.tier = shortwin_math::tier::Tier::C as u8;
    market.tier_computed_at = now;
    Ok(())
}

/// The observation side of `→ PumpSwap` (module docs; exactly a Raydium adoption's restart, M4R):
/// on a market with a price, every ring entry's `l = llo = lhi` and `L_ema` to the `anchor`,
/// `L_slow = min(L_slow, anchor)` (never raised: the curve's `√(vT·vQ)` is ≈ 1.35× a standard
/// pool's), `(x, y)` recomputed, the bucket cleared, then `ema_warm = 8`, `ema_guard = 1`. A
/// market without a price keeps none: its first PumpSwap observation starts it
/// ([`start_price`]). Without an anchor (a zero `lp_supply`, which a created pool never has) the
/// liquidity is kept and the state still restarted.
pub fn restart_observation(obs: &mut Observation, anchor: Option<u128>) -> Result<()> {
    if !priced(obs) {
        return Ok(());
    }
    if let Some(a) = anchor {
        obs.rewrite_liquidity(a, VenueChange::RaydiumLocked { slow_cap: a })
            .math()?;
    }
    obs.mark_restart();
    Ok(())
}

/// The market has a price state (a ring entry and a positive `P_ema`).
pub fn priced(obs: &Observation) -> bool {
    !obs.ring.is_empty() && obs.ema.price > 0
}

/// A PumpSwap market without a price state starts it at its first executable observation
/// (a PumpSwap listing, a `None →` PumpSwap re-detection: neither carries the pool's vaults, ARCH
/// §3.2's init set) exactly as a Raydium price start does (since M6R-1, owner decision of
/// 2026-09-30): `Observation::new_raydium(now, P_spot, √(X·(Y + VQR)), ⌊D·10⁴/λ_min⌋, D)` —
/// `P_ema = P_spot`, `L_ema = L_slow = min(√(X·(Y + VQR)), ⌊D·10⁴/λ_min⌋)` with `D` the pool's
/// locked depth, one ring entry, **marked for the post-change warm-up** (a pushed spot seeds
/// nothing an open reads before eight integrations of the pool's own buckets). `λ_min` is
/// PumpSwap's, `PUMPSWAP_LOCK_MIN_BPS` = 100 % (`VenueKind::lock_min_bps`; OWNER-M6D-1, ARCH
/// changelog 57, ECON #106; M6E-1), so the anchor is `D` itself (`≤ √(X·(Y + VQR))` on a canonical
/// pool): a deposit wrapped around the start credits nothing, and `L_ema` starts at the depth
/// every other PumpSwap term credits (the first warm-up integration re-seeds it from the pool's own
/// bucket, and the warm-up gates every exit and open until the eighth). A pool without locked
/// depth (`D = 0`: no burned LP left,
/// which a canonical pool never has) starts nothing: the market stays unpriced, so it takes no
/// open. Returns whether it started.
pub fn start_price(obs: &mut Observation, state: &VenueState, now: i64) -> Result<bool> {
    if priced(obs) || state.kind != VenueKind::PumpSwap || !state.executable {
        return Ok(false);
    }
    let (Some(p), Some(l)) = (state.price(), state.liquidity()) else {
        return Ok(false);
    };
    let cap = pool_select::depth_at_lambda(state.locked_depth, u64::from(PUMPSWAP_LOCK_MIN_BPS));
    if cap == 0 {
        return Ok(false);
    }
    // `cap = D` here, so the `L_slow ≤ D` cap (AIR-13's, for a Raydium start) never binds.
    *obs = Observation::new_raydium(now, p, l, cap, state.locked_depth).math()?;
    Ok(true)
}

// ---- legs --------------------------------------------------------------------------------------

fn meta(info: &AccountInfo, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(info.key(), false)
    } else {
        AccountMeta::new_readonly(info.key(), false)
    }
}

/// Whether the live pool (P0) is a cashback coin and has a coin creator.
fn pool_flags(leg: &Leg) -> Result<(bool, bool)> {
    let pool = raydium::at(leg.read, P_POOL)
        .ok()
        .and_then(parse_pool)
        .ok_or(ShortwinError::NoRoute)?;
    Ok((pool.is_cashback_coin, !pool.coin_creator_is_default()))
}

/// The trade's accounts in R-VEN §2.2's order (module docs): 23 fixed for `buy`, 21 for `sell`,
/// then the remaining accounts; `trader` the only signer.
fn trade_accounts<'info>(
    leg: &Leg<'info>,
    buy: bool,
) -> Result<(Vec<AccountMeta>, Vec<AccountInfo<'info>>)> {
    let r = |i: usize| raydium::at(leg.read, i);
    let t = |i: usize| raydium::at(leg.trade, i);
    let (cashback, has_creator) = pool_flags(leg)?;
    let trader = &leg.trader.trader;
    let mut list: Vec<(&AccountInfo<'info>, bool)> = vec![(r(P_POOL)?, true)];
    let user_at = list.len();
    list.push((trader, true));
    list.extend([
        (r(P_GLOBAL_CONFIG)?, false),
        (&leg.mint, false),
        (t(T_WSOL_MINT)?, false),
        (t(T_TRADER_BASE_ATA)?, true),
        (t(T_TRADER_WSOL)?, true),
        (r(P_BASE_VAULT)?, true),
        (r(P_QUOTE_VAULT)?, true),
        (t(T_PROTOCOL_FEE_RECIPIENT)?, false),
        (t(T_PROTOCOL_FEE_RECIPIENT_ATA)?, true),
        (&leg.token_program, false),
        (t(T_TOKEN_PROGRAM)?, false),
        (t(T_SYSTEM)?, false),
        (t(T_ATA_PROGRAM)?, false),
        (t(T_EVENT_AUTHORITY)?, false),
        (t(T_PROGRAM)?, false),
        (t(T_COIN_CREATOR_VAULT_ATA)?, true),
        (t(T_COIN_CREATOR_VAULT_AUTHORITY)?, false),
    ]);
    if buy {
        list.push((t(T_GLOBAL_VOLUME_ACCUMULATOR)?, false));
        list.push((t(T_UVA)?, true));
    }
    list.extend([(r(P_FEE_CONFIG)?, false), (t(T_FEE_PROGRAM)?, false)]);
    if cashback {
        list.push((t(T_CASHBACK_ATA)?, true));
        if !buy {
            list.push((t(T_UVA)?, true));
        }
    }
    if has_creator {
        list.push((t(T_POOL_V2)?, false));
    }
    list.extend([
        (t(T_BUYBACK_RECIPIENT)?, false),
        (t(T_BUYBACK_RECIPIENT_ATA)?, true),
    ]);
    let metas = list
        .iter()
        .enumerate()
        .map(|(i, (info, w))| {
            let mut m = meta(info, *w);
            m.is_signer = i == user_at;
            m
        })
        .collect();
    let infos = list.iter().map(|(info, _)| (*info).clone()).collect();
    Ok((metas, infos))
}

/// `buy(amount, max_quote_in, [false])` or `sell(amount, min_quote_out)`, `trader` signing, the
/// extras appended (A41).
fn trade_cpi(leg: &Leg, buy: bool, amount: u64, bound: u64) -> Result<()> {
    let (metas, infos) = trade_accounts(leg, buy)?;
    let mut data = Vec::with_capacity(BUY_DATA_LEN);
    data.extend_from_slice(if buy { &IX_BUY } else { &IX_SELL });
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&bound.to_le_bytes());
    if buy {
        data.push(0);
    }
    raydium::invoke_as_trader(leg, PUMP_AMM_PROGRAM_ID, data, metas, infos)
}

fn base_side<'a, 'info>(leg: &'a Leg<'info>) -> Result<&'a crate::engine::trader::BaseSide<'info>> {
    leg.trader
        .base
        .as_ref()
        .ok_or_else(|| error!(ShortwinError::BadRemainingAccounts))
}

/// The sell leg of an open (module docs; ARCH §2.4.1 PumpSwap row).
pub fn sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    min_out: u64,
) -> Result<SellOut> {
    let base = base_side(leg)?;
    let trader = &leg.trader.trader;
    let vault_before = token_state(&leg.pool_vault)?.amount;
    // 1. Q from the lending vault to the trader's base ATA (the market signs).
    let market_seeds: &[&[u8]] = &[Market::SEED, leg.mint.key.as_ref(), &[leg.market_bump]];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            leg.token_program.key(),
            TransferChecked {
                from: leg.pool_vault.clone(),
                mint: leg.mint.clone(),
                to: base.trader_base_ata.clone(),
                authority: leg.market.clone(),
            },
            &[market_seeds],
        ),
        q,
        leg.decimals,
    )?;
    // 2. `ert` from the borrower (a short pool's growth is topped up from `user`, R-VEN Update
    //    H8), then the ephemeral WSOL account, its rent from the borrower.
    if leg.ert > 0 {
        system_program::transfer(
            CpiContext::new(
                leg.trader.system_program.key(),
                Transfer {
                    from: borrower.clone(),
                    to: trader.clone(),
                },
            ),
            leg.ert,
        )?;
    }
    leg.trader.open_wsol(Funding::Signer(borrower), 0)?;
    // 3. sell(Q, min_out): WSOL into `trader_wsol`.
    trade_cpi(leg, false, q, min_out)?;
    // 4. PumpSwap took exactly Q, and the pool vault gave exactly Q.
    require!(
        base_ata_state(trader, base)?.amount == 0,
        ShortwinError::VenueResultMismatch
    );
    let vault_after = token_state(&leg.pool_vault)?.amount;
    require!(
        vault_before.checked_sub(vault_after) == Some(q),
        ShortwinError::VenueResultMismatch
    );
    // 5. S = trader_wsol + trader − ert; `trader_wsol` into the position; the trader's lamports
    //    (≤ ert) back to the borrower, any excess to the position.
    let wsol = leg.trader.wsol_amount()?;
    let held = trader.lamports();
    let s = wsol
        .checked_add(held)
        .and_then(|v| v.checked_sub(leg.ert))
        .ok_or(ShortwinError::VenueRentOnSell)?;
    let to_borrower = held.min(leg.ert);
    let to_position = held.checked_sub(to_borrower).or_math()?;
    let venue_rent = leg.ert.checked_sub(to_borrower).or_math()?;
    leg.trader.close_wsol(position)?;
    leg.trader.pay_from_trader(borrower, to_borrower)?;
    leg.trader.pay_from_trader(position, to_position)?;
    let refund = TraderAccounts::wsol_rent()?
        .checked_add(venue_rent)
        .or_math()?;
    Ok(SellOut {
        s,
        refund,
        venue_rent,
    })
}

/// The buy leg of an exit (module docs; ARCH §2.4.1 PumpSwap row): `buy(q, max_in, [false])` paid
/// from `trader_wsol` by the trader PDA; every token bought goes to the pool vault.
pub fn buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
) -> Result<BuyOut> {
    let base = base_side(leg)?;
    let trader = &leg.trader.trader;
    let lamports_before = position.lamports();
    // 1. `ert` to the trader (a direct debit; a zero System transfer then shows both balances to
    //    the runtime's per-CPI check), then `trader_wsol` with `max_in` wrapped, straight from the
    //    position (ARCH §2.4 rule 3).
    lamports::move_lamports_in_leg(position, trader, leg.ert)?;
    leg.trader.pay_from_trader(position, 0)?;
    leg.trader.open_wsol(Funding::Position(position), max_in)?;
    // 2. buy(q, max_quote_in = max_in): the trader PDA pays from `trader_wsol` (T-PDA-USER).
    trade_cpi(leg, true, q, max_in)?;
    // 3. Everything left back to the position.
    leg.trader.close_wsol(position)?;
    leg.trader.pay_from_trader(position, trader.lamports())?;
    // 4. At least q tokens, all of them to the pool vault (a surplus is a donation, A62).
    let bought = base_ata_state(trader, base)?.amount;
    require_gte!(bought, q, ShortwinError::VenueResultMismatch);
    let vault_before = token_state(&leg.pool_vault)?.amount;
    let seeds: &[&[u8]] = &[
        crate::state::TRADER_SEED,
        leg.trader.market.as_ref(),
        &[leg.trader.trader_bump],
    ];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            leg.token_program.key(),
            TransferChecked {
                from: base.trader_base_ata.clone(),
                mint: leg.mint.clone(),
                to: leg.pool_vault.clone(),
                authority: trader.clone(),
            },
            &[seeds],
        ),
        bought,
        leg.decimals,
    )?;
    let received = token_state(&leg.pool_vault)?
        .amount
        .checked_sub(vault_before)
        .ok_or(ShortwinError::VenueResultMismatch)?;
    require_gte!(received, q, ShortwinError::VenueResultMismatch);
    let spent = lamports_before.checked_sub(position.lamports()).or_math()?;
    let bound = max_in.checked_add(leg.ert).or_math()?;
    require_gte!(bound, spent, ShortwinError::VenueResultMismatch);
    Ok(BuyOut { spent, received })
}

// ---- listing -----------------------------------------------------------------------------------

/// PumpSwap's `init_user_volume_accumulator(payer, user = trader)` (R-VEN Update H6: any payer, an
/// unsigned user; ARCH §3.2 "Effects"): `[payer S W, user, user_volume_accumulator W, system,
/// event_authority, program]`. Skipped when the UVA already exists (anyone may create it first:
/// it is keyed to `trader`, and only the trader could close it, 3010). `accounts` is `[pAMM
/// program, the trader's pAMM UVA, pAMM event authority]`, key-checked here
/// (`VenueAccountMismatch`).
pub fn init_uva<'info>(
    payer: &AccountInfo<'info>,
    trader: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    accounts: &[AccountInfo<'info>],
    uva_key: &Pubkey,
) -> Result<()> {
    let [program, uva, event_authority] = accounts else {
        return err!(ShortwinError::BadRemainingAccounts);
    };
    require_keys_eq!(
        program.key(),
        PUMP_AMM_PROGRAM_ID,
        ShortwinError::VenueAccountMismatch
    );
    require!(program.executable, ShortwinError::VenueAccountMismatch);
    require_keys_eq!(uva.key(), *uva_key, ShortwinError::VenueAccountMismatch);
    require_keys_eq!(
        event_authority.key(),
        PUMP_AMM_EVENT_AUTHORITY,
        ShortwinError::VenueAccountMismatch
    );
    if *uva.owner == PUMP_AMM_PROGRAM_ID && !uva.data_is_empty() {
        return Ok(());
    }
    let ix = Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(payer.key(), true),
            AccountMeta::new_readonly(trader.key(), false),
            AccountMeta::new(uva.key(), false),
            AccountMeta::new_readonly(system_program.key(), false),
            AccountMeta::new_readonly(event_authority.key(), false),
            AccountMeta::new_readonly(program.key(), false),
        ],
        data: IX_INIT_USER_VOLUME_ACCUMULATOR.to_vec(),
    };
    invoke_signed(
        &ix,
        &[
            payer.clone(),
            trader.clone(),
            uva.clone(),
            system_program.clone(),
            event_authority.clone(),
            program.clone(),
        ],
        &[],
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    /// ARCH §3.12's discriminators (R-VEN §2.1) and the account-set sizes.
    #[test]
    fn discriminators_and_lengths() {
        let disc = |name: &str| {
            let h = solana_sha256_hasher::hash(format!("global:{name}").as_bytes());
            <[u8; 8]>::try_from(&h.to_bytes()[..8]).unwrap()
        };
        assert_eq!(disc("buy"), IX_BUY);
        assert_eq!(disc("sell"), IX_SELL);
        assert_eq!(
            disc("init_user_volume_accumulator"),
            IX_INIT_USER_VOLUME_ACCUMULATOR
        );
        assert_eq!(disc("claim_cashback"), IX_CLAIM_CASHBACK);
        assert_eq!(BUY_DATA_LEN, 8 + 8 + 8 + 1);
        assert_eq!(SELL_DATA_LEN, 8 + 8 + 8);
        assert_eq!(READ_LEN, P_LP_MINT + 1);
        assert_eq!(P_LP_MINT, P_BOOST_VAULT_ATA + 1);
        assert_eq!(TRADE_LEN, T_CASHBACK_ATA + 1);
        assert_eq!(PAMM_BUY_DISABLED_BIT, 1 << 3);
        assert_eq!(PAMM_CREATE_POOL_DISABLED_BIT, 1);
    }

    /// Every recipient table entry is the WSOL ATA of its recipient; an unknown one is derived.
    #[test]
    fn the_recipient_tables_re_derive() {
        for (recipient, ata) in EXTRA_RECIPIENT_WSOL_ATAS {
            assert_eq!(wsol_ata(&recipient), ata, "{recipient}");
            assert_eq!(recipient_wsol_ata(&recipient), ata);
        }
        for (recipient, ata) in super::super::pump_curve::RECIPIENT_WSOL_ATAS {
            assert_eq!(recipient_wsol_ata(&recipient), ata);
        }
        let other = Pubkey::new_unique();
        assert_eq!(recipient_wsol_ata(&other), wsol_ata(&other));
    }

    /// The PumpSwap PDAs this adapter derives (R-VEN §1.3, §2.2; ARCH §3.12 seed gotchas).
    #[test]
    fn pamm_seeds() {
        let k = Pubkey::new_unique();
        let pda = |seeds: &[&[u8]]| Pubkey::find_program_address(seeds, &PUMP_AMM_PROGRAM_ID).0;
        assert_eq!(pool_v2(&k), pda(&[b"pool-v2", k.as_ref()]));
        assert_eq!(boost_vault(&k), pda(&[b"boost_vault", k.as_ref()]));
        assert_eq!(
            user_volume_accumulator(&k),
            pda(&[b"user_volume_accumulator", k.as_ref()])
        );
        assert_eq!(
            coin_creator_vault_authority(&k),
            pda(&[b"creator_vault", k.as_ref()])
        );
        assert_eq!(lp_mint(&k), pda(&[b"pool_lp_mint", k.as_ref()]));
    }

    /// The precomputed cache of a curve market: the vaults are ATAs of the canonical pool, the
    /// derived entries as their seeds say; a creator change re-derives only its pair.
    #[test]
    fn precompute_and_write_cache() {
        let mut m = <Market as bytemuck::Zeroable>::zeroed();
        m.mint = Pubkey::new_unique();
        m.token_program = TOKEN_PROGRAM_ID;
        m.canonical_pool = Pubkey::new_unique();
        m.trader = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        precompute(&mut m, &creator).unwrap();
        assert_eq!(
            cached(&m, cache::POOL_BASE_VAULT),
            get_associated_token_address_with_program_id(
                &m.canonical_pool,
                &m.mint,
                &TOKEN_PROGRAM_ID
            )
        );
        assert_eq!(
            cached(&m, cache::POOL_QUOTE_VAULT),
            wsol_ata(&m.canonical_pool)
        );
        let bv = boost_vault(&m.canonical_pool);
        assert_eq!(cached(&m, cache::BOOST_VAULT), bv);
        assert_eq!(cached(&m, cache::BOOST_VAULT_ATA), wsol_ata(&bv));
        let uva = user_volume_accumulator(&m.trader);
        assert_eq!(cached(&m, cache::PAMM_UVA), uva);
        assert_eq!(cached(&m, cache::PAMM_UVA_QUOTE_ATA), wsol_ata(&uva));
        assert_eq!(cached(&m, cache::CACHED_COIN_CREATOR), creator);
        let auth = coin_creator_vault_authority(&creator);
        assert_eq!(cached(&m, cache::CACHED_COIN_CREATOR_VAULT_AUTH), auth);
        assert_eq!(
            cached(&m, cache::CACHED_COIN_CREATOR_VAULT_ATA),
            wsol_ata(&auth)
        );
        // The pool's own vaults and a new coin creator overwrite; nothing else moves.
        let mut pool = pump_amm::Pool {
            pool_bump: 0,
            index: 0,
            creator: [0; 32],
            base_mint: m.mint.to_bytes(),
            quote_mint: WSOL_MINT.to_bytes(),
            lp_mint: [0; 32],
            pool_base_token_account: [7; 32],
            pool_quote_token_account: [8; 32],
            lp_supply: 5,
            coin_creator: [9; 32],
            is_mayhem_mode: false,
            is_cashback_coin: false,
            virtual_quote_reserves: 0,
            creator_fee_bps: 0,
            can_edit_creator_fee: false,
            is_holder_reward: false,
            protocol_fees: 0,
            creator_fees: 0,
        };
        write_cache(&mut m, &pool).unwrap();
        assert_eq!(cached(&m, cache::POOL_BASE_VAULT).to_bytes(), [7; 32]);
        assert_eq!(cached(&m, cache::POOL_QUOTE_VAULT).to_bytes(), [8; 32]);
        assert_eq!(cached(&m, cache::BOOST_VAULT), bv);
        let c2 = Pubkey::new_from_array([9; 32]);
        assert_eq!(cached(&m, cache::CACHED_COIN_CREATOR), c2);
        assert_eq!(
            cached(&m, cache::CACHED_COIN_CREATOR_VAULT_AUTH),
            coin_creator_vault_authority(&c2)
        );
        assert_eq!(
            creator_accounts(&m, &c2).0,
            coin_creator_vault_authority(&c2)
        );
        assert_eq!(
            creator_accounts(&m, &creator).0,
            coin_creator_vault_authority(&creator)
        );
        assert_eq!(graduation_anchor(&pool), Some(5));
        pool.lp_supply = 0;
        assert_eq!(graduation_anchor(&pool), None);
    }

    /// M6-SEC-4: the creator triple is rewritten only when the creator differs (or was never
    /// cached), and nothing else in the cache moves.
    #[test]
    fn cache_creator_writes_only_on_a_change() {
        let mut m = <Market as bytemuck::Zeroable>::zeroed();
        m.mint = Pubkey::new_unique();
        m.token_program = TOKEN_PROGRAM_ID;
        m.canonical_pool = Pubkey::new_unique();
        m.trader = Pubkey::new_unique();
        let c1 = Pubkey::new_unique();
        precompute(&mut m, &c1).unwrap();
        let before = m.pool_cache;
        assert!(!cache_creator(&mut m, &c1).unwrap(), "the cached creator");
        assert_eq!(m.pool_cache, before);
        let c2 = Pubkey::new_unique();
        assert!(cache_creator(&mut m, &c2).unwrap());
        let auth = coin_creator_vault_authority(&c2);
        assert_eq!(cached(&m, cache::CACHED_COIN_CREATOR), c2);
        assert_eq!(cached(&m, cache::CACHED_COIN_CREATOR_VAULT_AUTH), auth);
        assert_eq!(
            cached(&m, cache::CACHED_COIN_CREATOR_VAULT_ATA),
            wsol_ata(&auth)
        );
        let creator_slots = [
            cache::CACHED_COIN_CREATOR,
            cache::CACHED_COIN_CREATOR_VAULT_AUTH,
            cache::CACHED_COIN_CREATOR_VAULT_ATA,
        ];
        for (i, (now, was)) in m.pool_cache.iter().zip(before.iter()).enumerate() {
            if !creator_slots.contains(&i) {
                assert_eq!(now, was, "entry {i}");
            }
        }
        assert_eq!(creator_accounts(&m, &c2), (auth, wsol_ata(&auth)));
        // A default (never set) coin creator is a creator like any other.
        assert!(cache_creator(&mut m, &Pubkey::default()).unwrap());
        assert_eq!(cached(&m, cache::CACHED_COIN_CREATOR), Pubkey::default());
        assert!(!cache_creator(&mut m, &Pubkey::default()).unwrap());
    }
}
