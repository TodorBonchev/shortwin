//! `create_market(venue_hint, max_creation_fee)` (ARCH §3.2): anyone lists a mint.
//!
//! `max_creation_fee` (added after the M1 review, M1-SEC-11; ARCH §3.2 check 4)
//! bounds the `market_creation_fee` the creator pays: the fee is read from `config` at
//! execution, and a timelocked change (≤ 0.1 SOL) could land between signing and execution
//! (`CreationFeeAboveMax`), as `open_short`'s `max_open_fee` does for the open fee.
//!
//! **Scope** (ARCH §9 M1, M3, M5, M6): `venue_hint` is [`VENUE_HINT_AUTO`] — `classify_pump` on
//! the detection set decides (below): a pump coin on its live or migrating curve or (M6) on its
//! valid canonical PumpSwap pool is listed on that venue, anything else (no pump curve, a curve
//! that graduated elsewhere, a canonical pool that does not read as one) as `None` (list-only:
//! deposits and withdrawals; never terminal, ARCH §2.5) — or, since M3, [`VENUE_HINT_RAYDIUM_V4`] /
//! [`VENUE_HINT_RAYDIUM_CPMM`] with a candidate pool (below). Any other hint is
//! `NoSupportedVenue`, except in a TEST-ONLY `test-venue` build, which also accepts the mock
//! venue's hint (M2, A49).
//!
//! **Pump listing** (`Auto`, M5; ARCH §2.5, §3.2): `remaining_accounts = [bonding_curve,
//! canonical_pool] ++ [pump program, the trader's pump UVA, pump event authority]` (the init set
//! of `init_user_volume_accumulator`, key-checked). `classify_pump` refuses a mayhem curve
//! (`CurveMayhemUnsupported`) and a non-SOL quote (`CurveQuoteNotSol`; ADOPTED Q6), a curve under
//! 115 bytes (`CurveNeedsExtend`: the client prepends pump's `extend_account`), a pump-owned
//! curve account that does not parse (`PumpVenuePrecedence`: fail closed), and on a live curve
//! a cashback coin (`CurveCashbackCoin`: pump's v3 trades refuse it; pump-final §6.3, D1 (a))
//! and a quote depth other than 0 (`CurveQuoteNotSol`). A live curve lists as `PumpCurve`, a
//! completed one that has not migrated (cashback or not) as `PumpMigrating` (`migration_seen_at
//! = now`). Effects on top of a list-only listing: `trader_base_ata = ATA(trader, mint)` created
//! (`create_idempotent`, payer the creator) and validated (mint, owner `trader`, program); the
//! trader's pump UVA created (`init_user_volume_accumulator`, payer the creator, user `trader`;
//! skipped if it exists — a griefer's is harmless, it is keyed to `trader`); the curve venue
//! cache (`venues::pump_curve::write_cache`: the curve's base and WSOL ATAs, the fee-sharing
//! config, the UVA and its WSOL ATA, the creator, its vault and the vault's WSOL ATA); the price
//! state at the curve's spot (`P_ema = vQ·WAD/vT`, `L = isqrt(vT·vQ)`; pump venues are never
//! marked for the warm-up); tier D. Pump listings are never latched (ARCH §3.2 check 5).
//!
//! **PumpSwap listing** (`Auto`, M6; ARCH §3.2): a coin on its valid canonical pool;
//! `remaining_accounts = [bonding_curve, canonical_pool] ++ [pAMM program, the trader's pAMM UVA,
//! pAMM event authority]`. The trader's base ATA is created and validated as above, the trader's
//! pAMM UVA created (`init_user_volume_accumulator` with the creator paying for an unsigned
//! `trader`, R-VEN Update H6; skipped if it exists); the PumpSwap cache (`pump_amm::graduate`: the
//! pool's vaults, the `pool-v2` marker, the boost vault and its ATA, the pAMM UVA and its ATA, the
//! coin-creator vault), `graduated_at = now` (age from listing, O13), tier C. **No price at
//! listing (flagged):** ARCH §3.2's init set carries no vault, so the market's first observation
//! with the PumpSwap ReadSet starts the price state, marked for the post-change warm-up
//! (`pump_amm::start_price`: `ema_warm = 8`) — ARCH §3.2 starts it at listing, unmarked.
//!
//! **Raydium listing** (ARCH §2.5 `create_market(venue_hint)`, §2.6, §3.2; D1, D2, O13):
//! `remaining_accounts = [bonding_curve, canonical_pool] ++ ReadSet(candidate)` (AMM v4: pool,
//! coin vault, pc vault, LP mint; CPMM: pool, config, token 0 vault, token 1 vault, LP mint —
//! `venues::raydium_v4::verify`, `venues::raydium_cpmm::verify`). First `classify_pump`
//! (`venues::detect`; its refusals above come first, so a live cashback curve is
//! `CurveCashbackCoin`): a live curve, a migrating curve or a valid canonical PumpSwap pool takes
//! precedence (`PumpVenuePrecedence`, D1); a curve that graduated elsewhere (zeroed reserves, no
//! canonical pool) or no pump curve at all proceeds, and the first sets the market's
//! `graduated_elsewhere` flag. The venue-upgrade latch then must not block the Raydium mask
//! (`VenueUpgraded`; M3-SEC-2, a deviation from ARCH §3.2, which checks the latch on opens
//! only): the listing reads the candidate with the compiled readers and fixes the pool, its SOL
//! side and `graduated_at` for good, so it waits for the Emergency review of an upgrade of
//! either Raydium program, as opens do. A list-only (`Auto`) listing fixes nothing venue-side
//! and is never latched. Then the candidate is verified and must be `eligible()` at its own
//! spot (a new market has no `P_ema`: executable, fee, and the locked-depth floor —
//! M4-SEC-1 / M4-SEC-3; `PoolIneligible`, `PoolTooThin`, `VenueAccountMismatch`). The market is
//! created on that venue with the pool's keys in its pool cache, the SOL side flag, the locked
//! depth `pool_d`, the price state of the pool (at its spot, `L = min(√(X·Y), ⌊D·10⁴/λ_min⌋)`,
//! marked for the post-change warm-up, M4R: opens wait for `ema_warm == 0`), `graduated_at`
//! per O13 (AMM v4: the listing time; CPMM: `min(now, open_time)` when `open_time ≥ T_CLAMP`,
//! else the listing time) and
//! `pool_seen_at = now` (opens wait `pool_list_window_s`). Pool *selection* after the listing
//! is `propose_pool` / `execute_pool_switch` (M4, `crate::engine::pool_select`): a listing-window
//! replacement, a zero-position adoption, the fast path; a `None` market moves to Raydium only
//! through `propose_pool`.
//!
//! **Order** (ARCH §3.2 checks 1–3):
//!
//! 1. `check_mint_safety(mint)`, a hard failure (ADOPTED Q7), **before any token CPI touches
//!    the mint**. For that reason `mint` is an `UncheckedAccount` and `pool_vault` is created
//!    here rather than by an Anchor `init` field: Anchor runs `init` fields before the handler,
//!    and its token-account sizing parses the mint's TLV itself, so an unparseable mint would
//!    fail there with a generic error instead of `MintDisallowedExtension` / `MintMalformed`
//!    (R-MINT §9.3's caveat: anchor-syn 1.2.0 deserializes the non-`init` fields, then creates
//!    every `init` field, and only then runs the other fields' constraints).
//! 2. `token_program.key == mint.owner` (`AccountMismatch`).
//! 3. Venue detection: the detection set `[bonding_curve, canonical_pool]` is **always**
//!    derived on-chain and key-checked against `remaining_accounts[0..2]` (C0-15;
//!    `VenueAccountMismatch`, `BadRemainingAccounts` for a missing or extra account); both keys
//!    are stored so `crank_price` can re-detect a pump venue later (ARCH §2.5).
//!
//! **Effects:** the pool vault (a PDA-addressed token account `["pool_vault", market]`,
//! authority = the market PDA, program = the mint's owner; 165 bytes, since no allowlisted
//! extension requires an account extension), created grief-proof: an address pre-funded by a
//! third party is topped up, allocated and assigned with the PDA's signature (R-MINT §8.2,
//! ARCH §2.4.1). `Market` exactly as `tests/litesvm/src/fixtures.rs::install_market` lays it
//! out (identity, every bump, own keys, detection keys, `tier = C`, both post-wipe bits,
//! `last_accrual_ts = created_at`, the market-level parameters and versions of `config`);
//! `MarketVault` with `Π = WAD`. The `market_creation_fee` (ADOPTED Q24) moves from the creator
//! to `market_vault` by System transfer and is booked as `protocol_fees`. Event
//! `MarketCreated`.
//!
//! **Uniqueness and griefing.** The market PDA depends only on the mint, so a mint has one
//! market whatever its token program. A front-runner can only create that same market (every
//! field but `creator` is fixed by the mint and `config`, and `creator` confers nothing).
//! Every short.win PDA can be pre-funded but never allocated or assigned by anyone else, so
//! pre-funding cannot block the listing; `trader_base_ata` is only key-checked and stored.

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Allocate, Assign, CreateAccount, Transfer};
use anchor_spl::associated_token::{
    self as ata, get_associated_token_address_with_program_id, AssociatedToken,
};
use anchor_spl::token_interface::{self, InitializeAccount3, TokenInterface};
use shortwin_math::pool::{Pool, Vault};
use shortwin_math::tier::Tier;

use crate::constants::{
    CONFIG_VERSION, MARKET_VERSION, PUMP_AMM_PROGRAM_ID, PUMP_PROGRAM_ID, WSOL_MINT,
};
use crate::engine::guard;
use crate::errors::{CheckedExt, ShortwinError};
use crate::events::MarketCreated;
use crate::mint_filter::check_mint_safety;
use crate::state::{
    GlobalConfig, Market, MarketVault, VenueGuard, VenueKind, MARKET_FLAG_GRADUATED_ELSEWHERE,
    POOL_VAULT_SEED, TRADER_SEED, TRADER_WSOL_SEED,
};
use crate::venues::detect::{self, PumpClass};
use crate::venues::{pump_curve, raydium_cpmm, raydium_v4};

/// `venue_hint` for "detect": a list-only market until the pump venues arrive (M5/M6).
pub const VENUE_HINT_AUTO: u8 = 0;
/// `venue_hint`: adopt the Raydium AMM v4 pool whose ReadSet follows the detection set.
pub const VENUE_HINT_RAYDIUM_V4: u8 = VenueKind::RaydiumV4 as u8;
/// `venue_hint`: adopt the Raydium CPMM pool whose ReadSet follows the detection set.
pub const VENUE_HINT_RAYDIUM_CPMM: u8 = VenueKind::RaydiumCpmm as u8;
/// TEST-ONLY `venue_hint`: adopt the mock venue's pool of the mint (feature `test-venue`;
/// A49). The remaining accounts are the detection set followed by the mock ReadSet
/// `[pool, base_vault, quote_vault]` (`venues::mock::adopt`).
#[cfg(feature = "test-venue")]
pub const VENUE_HINT_MOCK: u8 = crate::state::VenueKind::Mock as u8;

/// Number of remaining accounts of a list-only market: the detection set.
pub const DETECTION_SET_LEN: usize = 2;

/// pump `BondingCurve` seed: `PDA(pump, ["bonding-curve", mint])` (R-PUMP §2.1).
pub const PUMP_BONDING_CURVE_SEED: &[u8] = b"bonding-curve";
/// pump pool-authority seed: `PDA(pump, ["pool-authority", mint])` (R-VEN §1.3).
pub const PUMP_POOL_AUTHORITY_SEED: &[u8] = b"pool-authority";
/// PumpSwap pool seed: `PDA(pAMM, ["pool", index u16 LE, creator, base_mint, quote_mint])`.
pub const PUMP_AMM_POOL_SEED: &[u8] = b"pool";
/// Index of the canonical (migration) PumpSwap pool.
pub const CANONICAL_POOL_INDEX: u16 = 0;

/// Size of the pool vault: a plain token account. No allowlisted mint extension requires an
/// account extension (R-MINT §8.1), and an Anchor `token::` init would pick the same size.
pub const POOL_VAULT_LEN: usize = 165;

/// `Π` of a new market's vault (`pool_model.Pool()`): the only non-zero field of
/// `shortwin_math::pool::Vault::new()`.
const NEW_VAULT_PI: u128 = Vault::new().pi;

/// The pump bonding curve of `mint` (C0-15). Not a hot path: `find_program_address` is fine.
pub fn bonding_curve_key(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PUMP_BONDING_CURVE_SEED, mint.as_ref()], &PUMP_PROGRAM_ID).0
}

/// The canonical PumpSwap pool of `mint` (R-VEN §1.3): created by pump's `pool-authority` PDA
/// at index 0, quoted in WSOL. The address alone makes it unspoofable.
pub fn canonical_pool_key(mint: &Pubkey) -> Pubkey {
    canonical_pool_key_of(mint, &crate::venues::detect::pool_authority(mint))
}

/// [`canonical_pool_key`] with the pool authority `PDA(pump, ["pool-authority", mint])` given.
pub fn canonical_pool_key_of(mint: &Pubkey, authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            PUMP_AMM_POOL_SEED,
            &CANONICAL_POOL_INDEX.to_le_bytes(),
            authority.as_ref(),
            mint.as_ref(),
            WSOL_MINT.as_ref(),
        ],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0
}

#[event_cpi]
#[derive(Accounts)]
pub struct CreateMarket<'info> {
    /// Pays for every account and the creation fee. Confers nothing: `Market.creator` is
    /// informational.
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// The venue-upgrade latch (A7; M3-SEC-2): a Raydium listing is refused while a bit of the
    /// Raydium mask is set (`VenueUpgraded`), as opens are. Read only.
    #[account(seeds = [VenueGuard::SEED], bump = venue_guard.load()?.bump)]
    pub venue_guard: AccountLoader<'info, VenueGuard>,
    /// One market per mint, whatever its token program.
    #[account(
        init,
        payer = creator,
        space = Market::SPACE,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        init,
        payer = creator,
        space = MarketVault::SPACE,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    /// CHECK: created and initialized by the handler, after the mint filter (module docs).
    /// The PDA-addressed pool vault: a token account of `mint` owned by `token_program`,
    /// authority = the market PDA. Not an ATA: a pre-created ATA would block the listing
    /// forever (R-MINT §8.2).
    #[account(mut, seeds = [POOL_VAULT_SEED, market.key().as_ref()], bump)]
    pub pool_vault: UncheckedAccount<'info>,
    /// CHECK: parsed by `check_mint_safety` first thing in the handler.
    /// The listed mint: owned by SPL Token or Token-2022 (and by `token_program`), not a
    /// native mint, well-formed, no freeze authority, allowlisted extensions only.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: only its key and bump are stored (seeds checked).
    /// The market's `trader` PDA (`["trader", market]`): system-owned, zero data, never
    /// allocated (ARCH §2.4).
    #[account(seeds = [TRADER_SEED, market.key().as_ref()], bump)]
    pub trader: UncheckedAccount<'info>,
    /// CHECK: key-checked; on a list-only market only its key is stored.
    /// `ATA(trader, mint, token_program)`: created with `create_idempotent` and then
    /// validated on the pump venues (M5).
    #[account(
        mut,
        address = get_associated_token_address_with_program_id(
            &trader.key(),
            &mint.key(),
            &token_program.key()
        ) @ ShortwinError::AccountMismatch
    )]
    pub trader_base_ata: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Checks the detection set `[bonding_curve, canonical_pool]`: exactly these two accounts, in
/// this order (C0-15; ARCH §3.0 "every slot is mandatory and key-checked").
fn check_detection_set(
    remaining: &[AccountInfo],
    bonding_curve: &Pubkey,
    canonical_pool: &Pubkey,
) -> Result<()> {
    let [bc, cp] = remaining else {
        return err!(ShortwinError::BadRemainingAccounts);
    };
    require_keys_eq!(*bc.key, *bonding_curve, ShortwinError::VenueAccountMismatch);
    require_keys_eq!(
        *cp.key,
        *canonical_pool,
        ShortwinError::VenueAccountMismatch
    );
    Ok(())
}

/// Creates the pool vault at its PDA and initializes it as a token account of `mint` owned by
/// the market PDA. Grief-proof: anyone can send lamports to the address, but only this
/// program can sign `allocate` / `assign` for it, so a pre-funded address is topped up to the
/// rent minimum and then allocated and assigned (the fund / allocate / assign path of
/// Anchor's `init`).
fn create_pool_vault(accs: &CreateMarket, market: &Pubkey, bump: u8) -> Result<()> {
    let vault = accs.pool_vault.to_account_info();
    let creator = accs.creator.to_account_info();
    let system = accs.system_program.key();
    let token_program = accs.token_program.key();
    let space = u64::try_from(POOL_VAULT_LEN).map_err(|_| error!(ShortwinError::MathOverflow))?;
    let rent_min = Rent::get()?.minimum_balance(POOL_VAULT_LEN);
    let bump_seed = [bump];
    let seeds: &[&[u8]] = &[POOL_VAULT_SEED, market.as_ref(), &bump_seed];
    let signer: &[&[&[u8]]] = &[seeds];

    let current = vault.lamports();
    if current == 0 {
        system_program::create_account(
            CpiContext::new_with_signer(
                system,
                CreateAccount {
                    from: creator,
                    to: vault.clone(),
                },
                signer,
            ),
            rent_min,
            space,
            &token_program,
        )?;
    } else {
        let top_up = rent_min.saturating_sub(current);
        if top_up > 0 {
            system_program::transfer(
                CpiContext::new(
                    system,
                    Transfer {
                        from: creator,
                        to: vault.clone(),
                    },
                ),
                top_up,
            )?;
        }
        system_program::allocate(
            CpiContext::new_with_signer(
                system,
                Allocate {
                    account_to_allocate: vault.clone(),
                },
                signer,
            ),
            space,
        )?;
        system_program::assign(
            CpiContext::new_with_signer(
                system,
                Assign {
                    account_to_assign: vault.clone(),
                },
                signer,
            ),
            &token_program,
        )?;
    }
    token_interface::initialize_account3(CpiContext::new(
        token_program,
        InitializeAccount3 {
            account: vault,
            mint: accs.mint.to_account_info(),
            authority: accs.market.to_account_info(),
        },
    ))
}

/// The pool adopted at listing, for `MarketCreated.ray_pool` (`Pubkey::default()` for a
/// list-only market).
fn ray_pool_of(venue: VenueKind, hint_accounts: &[AccountInfo]) -> Pubkey {
    match venue {
        VenueKind::None | VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap => {
            Pubkey::default()
        }
        _ => hint_accounts.first().map_or(Pubkey::default(), |a| a.key()),
    }
}

/// A pump venue's market state (module docs): the curve cache, the price state at the curve's
/// spot, tier D; a migrating curve starts the migration pause now. Its own frame: the observation
/// state is 1.4 KB.
#[inline(never)]
fn init_pump_market(
    market: &mut Market,
    bc: &AccountInfo,
    venue: VenueKind,
    now: i64,
) -> Result<()> {
    let c = pump_curve::parse_curve(bc).ok_or(ShortwinError::PumpVenuePrecedence)?;
    pump_curve::write_caches(market, &Pubkey::new_from_array(c.creator))?;
    let (p, l) = pump_curve::spot(&c).ok_or(ShortwinError::NoRoute)?;
    let obs = Box::new(
        shortwin_math::observe::Observation::new(now, p, l)
            .map_err(|_| error!(ShortwinError::MathOverflow))?,
    );
    market.set_observation(&obs);
    market.tier = Tier::D as u8;
    if venue == VenueKind::PumpMigrating {
        market.migration_seen_at = now;
    }
    Ok(())
}

/// A PumpSwap listing's market state (module docs; M6): the PumpSwap cache from the valid
/// canonical pool (`pump_amm::graduate`: `graduated_at = now`, age from listing, O13), tier C, and
/// no price yet — the listing carries no vault (ARCH §3.2's init set), so the first PumpSwap
/// observation starts the price state, marked for the warm-up (`pump_amm::start_price`).
#[inline(never)]
fn init_pamm_market(market: &mut Market, cp: &AccountInfo, now: i64) -> Result<()> {
    let authority = market.pump_pool_authority;
    let (_, pool) = detect::canonical_pool_read(cp, &market.mint, &authority);
    let pool = pool.ok_or(ShortwinError::PumpVenuePrecedence)?;
    crate::venues::pump_amm::graduate(market, &pool, now)
}

/// ARCH §3.2 "Effects" for the TEST-ONLY mock venue (a venue with a price): the adopted pool's
/// keys, the price state `P_ema = P_spot`, `L_ema = L_slow = √(X·Y)`, one ring entry, an empty
/// bucket; age from listing (O13's conservative reading); opens wait for the warm-up. The tier
/// stays the conservative C until the first open or crank reads the oracle (`create_market` does
/// not carry `oracle_cache`). Its own frame (SBF frames are 4 KiB).
#[cfg(feature = "test-venue")]
#[inline(never)]
fn init_mock_market(
    m: &mut Market,
    hint_accounts: &[AccountInfo],
    mint_key: &Pubkey,
    token_program: &Pubkey,
    now: i64,
) -> Result<()> {
    let state = crate::venues::mock::adopt(m, hint_accounts, mint_key, token_program)?;
    let obs = crate::engine::observe::initial(now, &state)?;
    m.set_observation(&obs);
    m.graduated_at = now;
    m.pool_seen_at = now;
    m.venue = VenueKind::Mock as u8;
    Ok(())
}

/// The pump listing's CPIs (module docs, ARCH §3.2 "Effects"): `create_idempotent(trader_base_ata)`
/// paid by the creator, then its validation (a token account of the mint under its program, owned
/// by `trader`, no delegate, no close authority: `TraderStateInvalid`), then the trader's UVA of the
/// listed venue — pump's (`pump_curve::init_uva`) on a curve, PumpSwap's (`pump_amm::init_uva`, M6)
/// on the canonical pool; skipped if it exists — from `init_accounts` = `[the venue's program, the
/// UVA, its event authority]` (key-checked). Its own frame (SBF frames are 4 KiB).
#[inline(never)]
fn pump_listing<'info>(
    accs: &CreateMarket<'info>,
    init_accounts: &[AccountInfo<'info>],
    venue: VenueKind,
) -> Result<()> {
    ata::create_idempotent(CpiContext::new(
        accs.associated_token_program.key(),
        ata::Create {
            payer: accs.creator.to_account_info(),
            associated_token: accs.trader_base_ata.to_account_info(),
            authority: accs.trader.to_account_info(),
            mint: accs.mint.to_account_info(),
            system_program: accs.system_program.to_account_info(),
            token_program: accs.token_program.to_account_info(),
        },
    ))?;
    crate::engine::trader::base_ata_state(
        &accs.trader.to_account_info(),
        &crate::engine::trader::BaseSide {
            trader_base_ata: accs.trader_base_ata.to_account_info(),
            pool_vault: accs.pool_vault.to_account_info(),
            mint: accs.mint.to_account_info(),
            decimals: 0,
            token_program: accs.token_program.to_account_info(),
        },
    )?;
    if venue == VenueKind::PumpSwap {
        let uva = crate::venues::pump_amm::user_volume_accumulator(&accs.trader.key());
        return crate::venues::pump_amm::init_uva(
            &accs.creator.to_account_info(),
            &accs.trader.to_account_info(),
            &accs.system_program.to_account_info(),
            init_accounts,
            &uva,
        );
    }
    let uva = pump_curve::user_volume_accumulator(&accs.trader.key());
    pump_curve::init_uva(
        &accs.creator.to_account_info(),
        &accs.trader.to_account_info(),
        &accs.system_program.to_account_info(),
        init_accounts,
        &uva,
    )
}

pub fn handler<'info>(
    ctx: Context<'info, CreateMarket<'info>>,
    venue_hint: u8,
    max_creation_fee: u64,
) -> Result<()> {
    let accs = &ctx.accounts;
    let mint_key = accs.mint.key();
    let token_program = accs.token_program.key();

    // 1. The allowlist filter, before anything else reads the mint (ADOPTED Q7).
    let mint_info = accs.mint.to_account_info();
    check_mint_safety(&mint_info)?;
    // 2. The token program owns the mint.
    require_keys_eq!(
        *mint_info.owner,
        token_program,
        ShortwinError::AccountMismatch
    );
    let decimals = {
        let data = mint_info.try_borrow_data()?;
        shortwin_venue_state::mint::read_base(&mint_info.owner.to_bytes(), &data)
            .map_err(|_| error!(ShortwinError::MintMalformed))?
            .decimals
    };
    // 3. Venue: list-only, a verified and eligible Raydium pool (after the D1 precedence
    //    check), or, in a test-venue build, the mock; the detection keys are always derived and
    //    checked (C0-15).
    let bonding_curve = bonding_curve_key(&mint_key);
    let pool_authority = detect::pool_authority(&mint_key);
    let canonical_pool = canonical_pool_key_of(&mint_key, &pool_authority);
    let detection_len = DETECTION_SET_LEN.min(ctx.remaining_accounts.len());
    let (detection, hint_accounts) = ctx.remaining_accounts.split_at(detection_len);
    check_detection_set(detection, &bonding_curve, &canonical_pool)?;
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let mut graduated_elsewhere = false;
    let (venue, adoption) = match venue_hint {
        VENUE_HINT_AUTO => {
            let [bc, cp] = detection else {
                return err!(ShortwinError::BadRemainingAccounts);
            };
            let class = detect::classify_pump_with(bc, cp, &mint_key, &pool_authority)?;
            let venue = match class {
                PumpClass::PumpCurve => VenueKind::PumpCurve,
                PumpClass::PumpMigrating => VenueKind::PumpMigrating,
                // M6: a graduated pump coin lists on its canonical PumpSwap pool.
                PumpClass::PumpSwap => VenueKind::PumpSwap,
                // A canonical-pool account that does not read as a valid pool (M5-SEC-5) is a
                // pump venue short.win cannot trade now: list-only, and never "graduated
                // elsewhere" (D1 keeps Raydium out).
                PumpClass::PumpPoolUnreadable | PumpClass::NotPump => VenueKind::None,
                PumpClass::GraduatedElsewhere => {
                    graduated_elsewhere = true;
                    VenueKind::None
                }
            };
            if venue == VenueKind::None {
                require!(
                    hint_accounts.is_empty(),
                    ShortwinError::BadRemainingAccounts
                );
            }
            (venue, None)
        }
        VENUE_HINT_RAYDIUM_V4 | VENUE_HINT_RAYDIUM_CPMM => {
            let [bc, cp] = detection else {
                return err!(ShortwinError::BadRemainingAccounts);
            };
            let class = detect::classify_pump_with(bc, cp, &mint_key, &pool_authority)?;
            require!(!class.pump_venue(), ShortwinError::PumpVenuePrecedence);
            graduated_elsewhere = class == PumpClass::GraduatedElsewhere;
            // The latch (M3-SEC-2): the listing verifies the candidate with the compiled readers
            // and fixes the pool, its SOL side and `graduated_at`, so none happens while an
            // upgrade of either Raydium program awaits review — the mask `open_short` uses on
            // these markets (R4B-7). (`propose_pool` / `execute_pool_switch` latch on the
            // candidate's own program only, M4-SEC-8: the fast path must not wait for the review
            // of the incumbent's program; a listing is never urgent.)
            guard::latch(&*accs.venue_guard.load()?, VenueKind::RaydiumV4)?;
            let config = accs.config.load()?;
            // Eligibility at the candidate's own spot: a new market has no price yet (the
            // listing starts `P_ema` there; opens wait `pool_list_window_s`).
            let adoption = if venue_hint == VENUE_HINT_RAYDIUM_V4 {
                raydium_v4::verify(
                    hint_accounts,
                    &mint_key,
                    &token_program,
                    &config.params,
                    now,
                    None,
                )?
            } else {
                raydium_cpmm::verify(
                    hint_accounts,
                    &mint_key,
                    &token_program,
                    &config.params,
                    now,
                    None,
                )?
            };
            (adoption.kind, Some(adoption))
        }
        #[cfg(feature = "test-venue")]
        VENUE_HINT_MOCK => {
            // The mock carries the Raydium latch mask (A49): the same rule as a Raydium listing.
            guard::latch(&*accs.venue_guard.load()?, VenueKind::Mock)?;
            (VenueKind::Mock, None)
        }
        _ => return err!(ShortwinError::NoSupportedVenue),
    };
    // 4. The creation fee the creator agreed to (M1-SEC-11).
    let fee = accs.config.load()?.params.market_creation_fee;
    require_gte!(max_creation_fee, fee, ShortwinError::CreationFeeAboveMax);

    // Token and System CPIs first, while no account data is borrowed.
    let market_key = accs.market.key();
    create_pool_vault(accs, &market_key, ctx.bumps.pool_vault)?;
    if fee > 0 {
        system_program::transfer(
            CpiContext::new(
                accs.system_program.key(),
                Transfer {
                    from: accs.creator.to_account_info(),
                    to: accs.market_vault.to_account_info(),
                },
            ),
            fee,
        )?;
    }

    // The pump venues' trader accounts (ARCH §3.2 "Effects"): the base ATA and the pump UVA,
    // both paid by the creator, so no leg ever pays their rent.
    let pump = matches!(venue, VenueKind::PumpCurve | VenueKind::PumpMigrating);
    let pamm = venue == VenueKind::PumpSwap;
    if pump || pamm {
        pump_listing(accs, hint_accounts, venue)?;
    }
    let (trader_wsol, trader_wsol_bump) =
        Pubkey::find_program_address(&[TRADER_WSOL_SEED, market_key.as_ref()], &crate::ID);
    let trader = accs.trader.key();
    {
        let config = accs.config.load()?;
        let mut m = accs.market.load_init()?;
        m.version = MARKET_VERSION;
        m.bump = ctx.bumps.market;
        m.pool_vault_bump = ctx.bumps.pool_vault;
        m.market_vault_bump = ctx.bumps.market_vault;
        m.trader_bump = ctx.bumps.trader;
        m.trader_wsol_bump = trader_wsol_bump;
        m.venue = venue as u8;
        // A list-only market takes no opens; C is the conservative placeholder until a venue
        // appears and `tier::assign` runs (ARCH §4.7).
        m.tier = Tier::C as u8;
        m.decimals = decimals;
        m.created_at = now;
        m.mint = mint_key;
        m.token_program = token_program;
        m.creator = accs.creator.key();
        m.pool_vault = accs.pool_vault.key();
        m.market_vault = accs.market_vault.key();
        m.trader = trader;
        m.trader_base_ata = accs.trader_base_ata.key();
        m.trader_wsol = trader_wsol;
        m.bonding_curve = bonding_curve;
        m.canonical_pool = canonical_pool;
        // Derived once here, never on a trade (M5-SEC-6): `ATA(trader, WSOL)` and pump's pool
        // authority of the mint.
        m.trader_wsol_ata = pump_curve::wsol_ata(&trader);
        m.pump_pool_authority = pool_authority;
        m.set_flag(MARKET_FLAG_GRADUATED_ELSEWHERE, graduated_elsewhere);
        if pump {
            let [bc, _] = detection else {
                return err!(ShortwinError::BadRemainingAccounts);
            };
            init_pump_market(&mut m, bc, venue, now)?;
        }
        if pamm {
            let [_, cp] = detection else {
                return err!(ShortwinError::BadRemainingAccounts);
            };
            init_pamm_market(&mut m, cp, now)?;
        }
        // The lender pool of a new market (`pool_model.Pool()`): no shares, both post-wipe
        // bits set (nothing to sweep before the first deposit).
        m.set_pool(&Pool::new());
        m.last_accrual_ts = now;
        if let Some(a) = adoption.as_ref() {
            // ARCH §3.2 "Effects" for a Raydium pool (module docs): the pool cache, the SOL
            // side, `pool_d`, `graduated_at` (O13), `pool_seen_at`, and the price state
            // `P_ema = P_spot`, `L_ema = min(√(X·Y), ⌊D·10⁴/λ_min⌋)`, `L_slow = min(L_ema, D)`
            // (AIR-13: `observe::initial_raydium`), one ring entry,
            // an empty bucket, marked for the post-change warm-up (M4R: `ema_warm`, `ema_guard`).
            // The tier stays the conservative C until the first open or crank reads the oracle
            // (`create_market` does not carry `oracle_cache`).
            a.apply(&mut m, now)?;
            let cap = shortwin_math::pool_select::depth_at_lambda(
                a.state.locked_depth,
                u64::from(config.params.pool_lock_min_bps),
            );
            let obs = crate::engine::observe::initial_raydium(now, &a.state, cap)?;
            m.set_observation(&obs);
            m.set_flag(MARKET_FLAG_GRADUATED_ELSEWHERE, graduated_elsewhere);
        }
        #[cfg(feature = "test-venue")]
        if venue == VenueKind::Mock {
            init_mock_market(&mut m, hint_accounts, &mint_key, &token_program, now)?;
        }
        m.ema_params_version = config.ema_params.version;
        m.ema_dtmax_s = config.ema_params.ema_dtmax_s;
        m.ema_clamp_bps = config.ema_params.ema_clamp_bps;
        m.liq_slow_tau_s = config.ema_params.liq_slow_tau_s;
        m.liq_slow_dtmax_s = config.ema_params.liq_slow_dtmax_s;
        m.kink_version = config.kink.version;
        m.util_kink_bps = config.kink.util_kink_bps;
    }
    {
        let mut v = accs.market_vault.load_init()?;
        v.bump = ctx.bumps.market_vault;
        v.pi.set(NEW_VAULT_PI);
        v.protocol_fees = v.protocol_fees.checked_add(fee).or_math()?;
    }

    emit_cpi!(MarketCreated {
        market: market_key,
        slot: clock.slot,
        ts: now,
        mint: mint_key,
        token_program,
        venue: venue as u8,
        creator: accs.creator.key(),
        bonding_curve,
        canonical_pool,
        ray_pool: ray_pool_of(venue, hint_accounts),
        tier: if pump { Tier::D as u8 } else { Tier::C as u8 },
        creation_fee: fee,
    });
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::program_pack::Pack;
    use anchor_spl::token_2022::spl_token_2022::state::Account as TokenAccountState;
    use bytemuck::Zeroable;

    fn key(s: &str) -> Pubkey {
        s.parse().unwrap()
    }

    /// Mainnet vectors: Fartcoin's curve and (never created) canonical pool address (ARCH
    /// [V3]; R-VEN "Review corrections", RPC slot 450,811,173), and the migrated canonical pool
    /// `32UVHz…` of mint `F4awKX…` (R-VEN §1.2–§1.3, created by `migrate_v2`, owned by pAMM).
    #[test]
    fn detection_keys_match_mainnet() {
        let fartcoin = key("9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump");
        assert_eq!(
            bonding_curve_key(&fartcoin),
            key("TBHe5tJnuT4CQbHorJ1uVdfUoaYGPKgfCpiv2jgesVN")
        );
        assert_eq!(
            canonical_pool_key(&fartcoin),
            key("EVtHHoEb2V35V1fpzUZcaASgFn6N4yGWoaZ1twNDVmjW")
        );
        assert_eq!(
            canonical_pool_key(&key("F4awKX7vwGP6KnCysp2dUsAudPUeGTDcWx17jXsgX8fx")),
            key("32UVHz1WQ44Azr26EJTJZbGUp2Eb6QjWKy21BrGf3k4e")
        );
    }

    #[test]
    fn vault_and_pool_of_a_new_market() {
        assert_eq!(POOL_VAULT_LEN, TokenAccountState::LEN);
        // Writing `pi` (and the bump) into a zeroed account equals `set_vault(&Vault::new())`.
        let mut expected = MarketVault::zeroed();
        expected.set_vault(&Vault::new());
        let mut ours = MarketVault::zeroed();
        ours.pi.set(NEW_VAULT_PI);
        assert!(bytemuck::bytes_of(&expected) == bytemuck::bytes_of(&ours));
        assert_eq!(NEW_VAULT_PI, shortwin_math::num::WAD);
    }

    #[test]
    fn detection_set_is_strict() {
        let (bc, cp) = (Pubkey::new_unique(), Pubkey::new_unique());
        let owner = Pubkey::default();
        let (mut l1, mut l2, mut l3) = (0u64, 0u64, 0u64);
        let (mut d1, mut d2, mut d3) = ([0u8; 0], [0u8; 0], [0u8; 0]);
        let other = Pubkey::new_unique();
        let a = AccountInfo::new(&bc, false, false, &mut l1, &mut d1, &owner, false);
        let b = AccountInfo::new(&cp, false, false, &mut l2, &mut d2, &owner, false);
        let c = AccountInfo::new(&other, false, false, &mut l3, &mut d3, &owner, false);
        let code = |r: Result<()>| match r {
            Err(anchor_lang::error::Error::AnchorError(e)) => Some(e.error_code_number),
            _ => None,
        };
        assert!(check_detection_set(&[a.clone(), b.clone()], &bc, &cp).is_ok());
        let bad = Some(u32::from(ShortwinError::BadRemainingAccounts));
        let mismatch = Some(u32::from(ShortwinError::VenueAccountMismatch));
        assert_eq!(
            code(check_detection_set(std::slice::from_ref(&a), &bc, &cp)),
            bad
        );
        assert_eq!(
            code(check_detection_set(
                &[a.clone(), b.clone(), c.clone()],
                &bc,
                &cp
            )),
            bad
        );
        assert_eq!(
            code(check_detection_set(&[b.clone(), a.clone()], &bc, &cp)),
            mismatch
        );
        assert_eq!(code(check_detection_set(&[a, c], &bc, &cp)), mismatch);
    }
}
