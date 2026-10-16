//! Pump.fun bonding-curve adapter (ARCH §2.4.1 PumpCurve row, §2.5, §3.2, §3.12 "PumpCurve account
//! sets", §4.1–§4.3, §5.4; R-PUMP §2–§4, §6, §7 and its Update 2026-09-29; pump-final §6).
//! Program `6EF8rr…`; pump's **v3** trade instructions `sell_v3` (open, exact tokens in) and
//! `buy_v3` (every exit, exact tokens out; past the curve's `rT` by pump's synthetic migration),
//! with the market's `trader` PDA as pump's `user`.
//!
//! **Account sets** (order binding; W = writable in trades; every slot mandatory and key-checked
//! against a compile-time constant, the market's curve cache or the live `Global` list, C0-2):
//!
//! - ReadSet ([`read_keys`]): C0 `bonding_curve` (W), C1 pump `Global`, C2 pump `FeeConfig`
//!   (`8Wf5Ti…`, which v3 reads itself), C3 the canonical PumpSwap pool (derived; may not exist;
//!   detection only), C4 `curve_base_ata` (W; the live `X_s` of `B̂_v3`'s pool part), C5 PumpSwap
//!   `FeeConfig` and C6 PumpSwap `GlobalConfig` (the post-migration pool's fees, which a
//!   `PumpMigrating` market's quotes and FV price; and its `disable_flags`: bit 0 halts
//!   `create_pool`, hence `migrate_v2`, so curve opens refuse `GraduationHalted` while it is set —
//!   M6-SEC-3, owner decision of 2026-09-30). A `PumpMigrating` market reads the same set.
//! - TradeExtras ([`check_trade`], pump-final §6.1.1; 10 slots): T0 pump (executable), T1 pump
//!   `event_authority`, T2 WSOL (`quote_mint`), T3 Tokenkeg (`quote_token_program`), T4
//!   `curve_quote_ata` (W; cached; a SOL coin never touches it), T5 `ATA(trader, WSOL)` (W; the
//!   market's cached `trader_wsol_ata`), T6 the trader's pump UVA (W; **must exist**, created at
//!   listing; [`check_uva`]), T7 `buyback_fee_recipient` (W; one of the live `Global`'s buyback
//!   recipients @741, checked here as well as by pump, C0-14), T8 `trader_base_ata` (W; must
//!   exist), T9 System. No fee recipient, creator vault, `sharing_config`, pump-fees or ATA
//!   program: v3 keeps the protocol and creator fees in the curve's buckets (@125 / @133,
//!   outside `rQ`) and reads `FeeConfig` itself, so nothing is derived on a trade (M5-SEC-6).
//! - CPI ([`trade_accounts`]): exactly the IDL's 17-account list, one for `buy_v3` and `sell_v3`
//!   (pump-public-docs `2293f9a6`), `trader` the only signer, then the extras (A41; pump accepts
//!   trailing accounts, R-PUMP Update U4). Data: discriminator ‖ amount ‖ bound, **exactly 24
//!   bytes**: the optional `partial_fill` byte is never sent (pump ignores it on non-mayhem
//!   coins, and short.win lists no mayhem coin).
//!
//! **Legs** (ARCH §2.4.1, §2.4 rules 2–3):
//!
//! - [`sell`] (open): `transfer_checked(pool_vault → trader_base_ata, Q)` (the market signs); a
//!   System transfer of `ert` from the borrower to the trader; `sell_v3(Q, min_out)`, which
//!   credits the proceeds straight to the trader's lamports (pump may first take the rent of a
//!   curve or UVA growth from the trader; v3 never tops up a creator vault); then
//!   `S = trader.lamports − ert` (`VenueRentOnSell` if the venue took more than it credited), `S`
//!   to the position and exactly `ert` back to the borrower (System transfers the trader signs).
//!   `trader_base_ata` must be empty again and the pool vault down by exactly `Q`.
//! - [`buy`] (exits): `max_in + ert` from the position to the trader (a direct debit; a zero
//!   System transfer from the trader to the position then shows both balances to the runtime's
//!   per-CPI check), `buy_v3(Q, max_sol_cost = max_in)` (System transfers **from the trader
//!   PDA**, T-PDA-USER), everything left back to the position, then every token of
//!   `trader_base_ata` (`≥ Q`, else `VenueResultMismatch`: a short delivery reverts the whole
//!   instruction; a surplus is a donation, A62) to the pool vault. `B = position before − after
//!   ≤ max_in + ert`. For `Q > rT` the same `buy_v3` buys `rT` on the curve, which completes,
//!   and `Q − rT` from the would-be PumpSwap pool (synthetic migration), atomically: one
//!   `TransferChecked` of `Q` and one System transfer of the whole charge, then
//!   [`sync_after_leg`] moves the market to `PumpMigrating` in the same instruction.
//!
//! **Pricing** ([`read_state`]; ARCH §4.1–§4.3; pump-final §5): `X = vT`, `Y = vQ`, `R = rQ`;
//! the fee `fees::pump_curve` on the live `(vT, vQ)` (the pump `FeeConfig` tiers, `Global.
//! creator_fee_configurable`, the curve's own `creator_fee_bps` and creator), resolved on every
//! read and never cached; quotes `quote::CurvePricing`: above `rT` a live curve prices what
//! executes, `B̂_v3` (the curve leg plus the synthetic pool leg, both at the curve's fee on the
//! mint's live supply, `CurvePricingInputs::synthetic_fees`; owner D3), while a `PumpMigrating`
//! market's EMA-leg states keep `B̂_str` (the real pool at its PumpSwap tier) and its FV the pool
//! `migrate_v2` will create, ρ-scaled after a synthetic pool part (`Quoter::fv_repriced`).
//!
//! **Guards** ([`pricing_inputs`], pump-final §6.1.5): a live curve that carries post-complete
//! fields (`pcB` / `pcQ` @150 / @158, which pump writes only in the buy that completes the curve)
//! has no pricing — not executable, A63's cap-only risk view (G-3); a migrating curve whose
//! post-complete fields are not known (one of `pcB`, `pcQ` zero, or pump's own `vQ − rQ = ivqr`
//! broken) keeps its curve quoter but has **no FV**, so an S3 gives the lenders the whole take
//! (the documented exception to A63's cap-only rule, `venues::risk_quoter`).
//!
//! **Venue predicate** (`venue_executable`, ARCH §5.3): every ReadSet account parses (A63), the
//! curve is at least 115 bytes, SOL-quoted (`depth` 0), not mayhem, **not a cashback coin** (v3
//! refuses one with 6094: owner D1 (a), `CurveCashbackCoin` at listing) and **not complete**. A
//! complete curve with `vT > 0` is migrating: not executable, still observed (ARCH §3.0 step 7;
//! a complete cashback curve too), and the prologue's [`sync`] moves the market to
//! `PumpMigrating`.

use std::rc::Rc;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::system_program::{self, Transfer};
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token_interface::{self, TransferChecked};
use shortwin_math::fees::{self, CurveFeeSchedule, FeeBps, FeeTier};
use shortwin_math::num::{mul_div, Rounding, WAD};
use shortwin_math::quote::{CurveOffsets, CurveState};
use shortwin_venue_state::{pump, pump_amm};

use super::detect::CanonicalPool;
use super::{raydium, BuyOut, CurvePricingInputs, CurveStage, CurveVenue, Leg, VenueState};
use crate::constants::{
    PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG, PUMP_EVENT_AUTHORITY, PUMP_FEES_PROGRAM_ID,
    PUMP_FEE_CONFIG, PUMP_GLOBAL, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::engine::trader::{base_ata_state, token_state};
use crate::errors::{CheckedExt, ShortwinError};
use crate::lamports;
use crate::state::{Market, VenueKind, MARKET_FLAG_GRADUATED_ELSEWHERE};

/// ReadSet length.
pub const READ_LEN: usize = 7;
/// TradeExtras length (pump-final §6.1.1).
pub const TRADE_LEN: usize = 10;

/// ReadSet slots.
pub const C_CURVE: usize = 0;
pub const C_GLOBAL: usize = 1;
pub const C_FEE_CONFIG: usize = 2;
pub const C_CANONICAL_POOL: usize = 3;
pub const C_CURVE_BASE_ATA: usize = 4;
pub const C_PAMM_FEE_CONFIG: usize = 5;
pub const C_PAMM_GLOBAL_CONFIG: usize = 6;

/// TradeExtras slots (module docs).
/// pump `6EF8rr…` (key + executable).
pub const T_PROGRAM: usize = 0;
/// pump `__event_authority`.
pub const T_EVENT_AUTHORITY: usize = 1;
/// `quote_mint` (WSOL).
pub const T_WSOL_MINT: usize = 2;
/// `quote_token_program` (Tokenkeg).
pub const T_TOKEN_PROGRAM: usize = 3;
/// W: `market.curve_quote_ata` (cached; unused by pump on a SOL curve).
pub const T_CURVE_QUOTE_ATA: usize = 4;
/// W: `trader_wsol_ata(market)` (cached; unused by pump on a SOL curve).
pub const T_TRADER_WSOL_ATA: usize = 5;
/// W: `market.pump_uva` ([`check_uva`]: exists, `user == trader`).
pub const T_UVA: usize = 6;
/// W: a live `Global.buyback_fee_recipients` entry (@741; C0-14).
pub const T_BUYBACK_RECIPIENT: usize = 7;
/// W: `market.trader_base_ata` (must exist).
pub const T_TRADER_BASE_ATA: usize = 8;
/// System.
pub const T_SYSTEM: usize = 9;

/// `sha256("global:buy_v3")[..8]` (pump-public-docs `2293f9a6`).
pub const IX_BUY_V3: [u8; 8] = [0x07, 0x05, 0x1d, 0xc4, 0xf5, 0x17, 0x65, 0x50];
/// `sha256("global:sell_v3")[..8]`.
pub const IX_SELL_V3: [u8; 8] = [0x1c, 0x92, 0xde, 0x77, 0x26, 0xc4, 0x69, 0xd5];
/// `sha256("global:init_user_volume_accumulator")[..8]` (R-PUMP §3.6).
pub const IX_INIT_USER_VOLUME_ACCUMULATOR: [u8; 8] =
    [0x5e, 0x06, 0xca, 0x73, 0xff, 0x60, 0xe8, 0xb7];
/// The length of a v3 trade's data: discriminator, amount, bound — never the optional
/// `partial_fill` byte (module docs).
pub const TRADE_DATA_LEN: usize = 24;
/// The number of accounts of a v3 trade CPI (the IDL's list, before the extras).
pub const TRADE_CPI_ACCOUNTS: usize = 17;

// ---- keys --------------------------------------------------------------------------------

/// `ATA(owner, WSOL)` under SPL Token (a pump quote-side account; seed-checked by pump).
/// `find_program_address`: listing and cold paths only (M5-SEC-6).
pub fn wsol_ata(owner: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, &WSOL_MINT, &TOKEN_PROGRAM_ID)
}

/// `(recipient, ATA(recipient, WSOL))` for pump's 8 normal fee recipients and 8 buyback fee
/// recipients as the live `Global` lists them (the M5 dump at slot 451,675,432; R-PUMP §2.2), so
/// a trade does not derive their WSOL ATAs (M5-SEC-6: `find_program_address` costs 1,500 CU per
/// bump tried, on every trade). The curve's v3 trades name no recipient ATA; PumpSwap's
/// recipients are the same keys, and `pump_amm::recipient_wsol_ata` reads this table. A
/// recipient added later is derived instead ([`recipient_wsol_ata`]);
/// `tests::the_recipient_table_re_derives` re-derives every entry.
pub const RECIPIENT_WSOL_ATAS: [(Pubkey, Pubkey); 16] = [
    (
        pubkey!("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV"),
        pubkey!("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb"),
    ),
    (
        pubkey!("7VtfL8fvgNfhz17qKRMjzQEXgbdpnHHHQRh54R9jP2RJ"),
        pubkey!("7GFUN3bWzJMKMRZ34JLsvcqdssDbXnp589SiE33KVwcC"),
    ),
    (
        pubkey!("7hTckgnGnLQR6sdH7YkqFTAA7VwTfYFaZ6EhEsU3saCX"),
        pubkey!("X5QPJcpph4mBAJDzc4hRziFftSbcygV59kRb2Fu6Je1"),
    ),
    (
        pubkey!("9rPYyANsfQZw3DnDmKE3YCQF5E8oD89UXoHn9JFEhJUz"),
        pubkey!("Bvtgim23rfocUzxVX9j9QFxTbBnH8JZxnaGLCEkXvjKS"),
    ),
    (
        pubkey!("AVmoTthdrX6tKt4nDjco2D775W2YK3sDhxPcMmzUAmTY"),
        pubkey!("FGptqdxjahafaCzpZ1T6EDtCzYMv7Dyn5MgBLyB3VUFW"),
    ),
    (
        pubkey!("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM"),
        pubkey!("CGEWR6pxwgQvYKeX4pZDqpZtWYPvyTjiAsw86SNzJtGy"),
    ),
    (
        pubkey!("FWsW1xNtWscwNmKv6wVsU1iTzRN6wmmk3MjxRP5tT7hz"),
        pubkey!("7xQYoUjUJF1Kg6WVczoTAkaNhn5syQYcbvjmFrhjWpx"),
    ),
    (
        pubkey!("G5UZAVbAf46s7cKWoyKu8kYTip9DGTpbLZ2qa9Aq69dP"),
        pubkey!("BWXT6RUhit9FfJQM3pBmqeFLPYmuxgmyhMGC5sGr8RbA"),
    ),
    (
        pubkey!("5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD"),
        pubkey!("HjQjngTDqoHE6aaGhUqfz9aQ7WZcBRjy5xB8PScLSr8i"),
    ),
    (
        pubkey!("9M4giFFMxmFGXtc3feFzRai56WbBqehoSeRE5GK7gf7"),
        pubkey!("GAFuhgcd328SkkBYHpfadzmef9hTGAFRCi9QoCnsZQug"),
    ),
    (
        pubkey!("GXPFM2caqTtQYC2cJ5yJRi9VDkpsYZXzYdwYpGnLmtDL"),
        pubkey!("AktftA98kSWAxn6kVSoqBXBELUArjKu2H9WmKB48ULFY"),
    ),
    (
        pubkey!("3BpXnfJaUTiwXnJNe7Ej1rcbzqTTQUvLShZaWazebsVR"),
        pubkey!("6rVkF4HSgy1jrnC3HogfRgPHrq4CtLg5f11URpsC4i9D"),
    ),
    (
        pubkey!("5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6"),
        pubkey!("GYH1Gae1wJytMSvMvw8JVcv7nuAbxi8i9erNVbERnzXd"),
    ),
    (
        pubkey!("EHAAiTxcdDwQ3U4bU6YcMsQGaekdzLS3B5SmYo46kJtL"),
        pubkey!("CA7v8gHfbquYXyDnDx6QxWW8hmL1H7X6Y2RYDrGLnuck"),
    ),
    (
        pubkey!("5eHhjP8JaYkz83CWwvGU2uMUXefd3AazWGx4gpcuEEYD"),
        pubkey!("CASRL2zkwDnppxEFQ4LgdwgR9pdz5Q8R8nEMKVZ9QoLp"),
    ),
    (
        pubkey!("A7hAgCzFw14fejgCp387JUJRMNyz4j89JKnhtKU8piqW"),
        pubkey!("qkYdTGRPHbWTWuBMz45bCiU6a23axRqf6sBHm9295WY"),
    ),
];

/// `ATA(recipient, WSOL)` of a fee or buyback recipient: [`RECIPIENT_WSOL_ATAS`], else derived.
pub fn recipient_wsol_ata(recipient: &Pubkey) -> Pubkey {
    RECIPIENT_WSOL_ATAS
        .iter()
        .find(|(r, _)| r == recipient)
        .map_or_else(|| wsol_ata(recipient), |(_, ata)| *ata)
}

/// `ATA(trader, WSOL)` of `market`: the key cached at listing (`Market::trader_wsol_ata`,
/// M5-SEC-6), derived only for a market that predates the cache.
pub fn trader_wsol_ata(market: &Market) -> Pubkey {
    if market.trader_wsol_ata == Pubkey::default() {
        wsol_ata(&market.trader)
    } else {
        market.trader_wsol_ata
    }
}

/// `PDA(pump, ["creator-vault", creator])` (hyphen; R-PUMP §3.1; ARCH §3.12 seed gotchas).
pub fn creator_vault(creator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump::CREATOR_VAULT_SEED, creator.as_ref()],
        &PUMP_PROGRAM_ID,
    )
    .0
}

/// `PDA(pump, ["user_volume_accumulator", user])`.
pub fn user_volume_accumulator(user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump::USER_VOLUME_ACCUMULATOR_SEED, user.as_ref()],
        &PUMP_PROGRAM_ID,
    )
    .0
}

/// `PDA(pfee, ["sharing-config", mint])` (may be uninitialised; pump checks `data_is_empty`).
pub fn sharing_config(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump::SHARING_CONFIG_SEED, mint.as_ref()],
        &PUMP_FEES_PROGRAM_ID,
    )
    .0
}

/// The curve venue cache of a market (ARCH §2.3.4 "curve venue cache"), derived at listing (or at
/// `crank_price`'s `None →` pump re-detection): the curve's base and quote ATAs, the fee-sharing
/// config, the trader's pump UVA and its WSOL ATA, and the creator of the curve as read with its
/// vault and the vault's WSOL ATA (re-derived at execution when the creator changes, R-PUMP §8).
pub fn write_cache(market: &mut Market, curve_creator: &Pubkey) {
    let bc = market.bonding_curve;
    market.curve_base_ata =
        get_associated_token_address_with_program_id(&bc, &market.mint, &market.token_program);
    market.curve_quote_ata = wsol_ata(&bc);
    market.sharing_config = sharing_config(&market.mint);
    market.pump_uva = user_volume_accumulator(&market.trader);
    market.pump_uva_quote_ata = wsol_ata(&market.pump_uva);
    market.cached_creator = *curve_creator;
    market.cached_creator_vault = creator_vault(curve_creator);
    market.cached_creator_vault_ata = wsol_ata(&market.cached_creator_vault);
    // Listed before the derived-key cache existed (M5-SEC-6): fill it now.
    if market.trader_wsol_ata == Pubkey::default() {
        market.trader_wsol_ata = wsol_ata(&market.trader);
    }
    if market.pump_pool_authority == Pubkey::default() {
        market.pump_pool_authority = super::detect::pool_authority(&market.mint);
    }
}

/// [`write_cache`] and the PumpSwap cache the market's `→ PumpSwap` transition needs, derived now
/// (`pump_amm::precompute`, M6: no derivation on a transition crank or a trade, M5-SEC-6). The
/// listing's cache write. (`crank_price`'s `None →` curve re-detection writes the curve cache only,
/// [`write_cache`], to stay inside the crank's CU limit; that market's transition derives the
/// PumpSwap cache instead, `pump_amm::graduate`.)
pub fn write_caches(market: &mut Market, curve_creator: &Pubkey) -> Result<()> {
    write_cache(market, curve_creator);
    super::pump_amm::precompute(market, curve_creator)
}

/// The ReadSet keys of `market` in order (module docs).
pub fn read_keys(market: &Market) -> [Pubkey; READ_LEN] {
    [
        market.bonding_curve,
        PUMP_GLOBAL,
        PUMP_FEE_CONFIG,
        market.canonical_pool,
        market.curve_base_ata,
        PUMP_AMM_FEE_CONFIG,
        PUMP_AMM_GLOBAL_CONFIG,
    ]
}

/// Key-checks the ReadSet against the constants and the market's cache.
pub fn check_read(market: &Market, read: &[AccountInfo]) -> Result<()> {
    raydium::check_keys(read, &read_keys(market))
}

/// The TradeExtras slots whose keys are fixed by a constant or the market's cache, with their
/// keys (pump-final §6.1.2: T0–T6, T8, T9; T7, the buyback recipient, is checked against the live
/// `Global` by [`check_trade`]). T5 is the market's cached `ATA(trader, WSOL)`
/// ([`trader_wsol_ata`]; derived only for a market listed before the cache existed).
pub fn fixed_trade_keys(market: &Market) -> [(usize, Pubkey); 9] {
    [
        (T_PROGRAM, PUMP_PROGRAM_ID),
        (T_EVENT_AUTHORITY, PUMP_EVENT_AUTHORITY),
        (T_WSOL_MINT, WSOL_MINT),
        (T_TOKEN_PROGRAM, TOKEN_PROGRAM_ID),
        (T_CURVE_QUOTE_ATA, market.curve_quote_ata),
        (T_TRADER_WSOL_ATA, trader_wsol_ata(market)),
        (T_UVA, market.pump_uva),
        (T_TRADER_BASE_ATA, market.trader_base_ata),
        (T_SYSTEM, anchor_lang::system_program::ID),
    ]
}

/// Key-checks the TradeExtras (module docs; pump-final §6.1.2): `BadRemainingAccounts` for a
/// wrong count, `VenueAccountMismatch` for a wrong key — the fixed and cached slots
/// ([`fixed_trade_keys`]), T0 executable, and T7 in the live `Global`'s buyback list (C0-14). A
/// v2-era list of 20 reaches this check as its first ten keys and fails at T2 (pump-fees where
/// WSOL belongs). A `Global` that no longer parses (a venue upgrade, A63) leaves T7
/// unchecked here: the venue is then not executable and no leg runs. Nothing is derived (M5-SEC-6).
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
    let buyback = raydium::at(trade, T_BUYBACK_RECIPIENT)?.key();
    if let Some(global) = read.get(C_GLOBAL) {
        if let Ok(data) = global.try_borrow_data() {
            if let Ok(g) = pump::read_global(&global.owner.to_bytes(), &data) {
                require!(
                    g.is_buyback_fee_recipient(&buyback.to_bytes()),
                    ShortwinError::VenueAccountMismatch
                );
            }
        }
    }
    Ok(())
}

// ---- state -------------------------------------------------------------------------------

/// The curve account parsed (owner pump, discriminator, bools); `None` if it is not one.
pub fn parse_curve(info: &AccountInfo) -> Option<pump::BondingCurve> {
    if *info.owner != PUMP_PROGRAM_ID {
        return None;
    }
    let data = info.try_borrow_data().ok()?;
    pump::read_curve(&info.owner.to_bytes(), &data).ok()
}

/// The curve as `shortwin-math`'s `CurveState`.
pub fn curve_state(c: &pump::BondingCurve) -> CurveState {
    CurveState {
        vt: u128::from(c.virtual_token_reserves),
        vq: u128::from(c.virtual_quote_reserves),
        rt: u128::from(c.real_token_reserves),
        rq: u128::from(c.real_quote_reserves),
        complete: c.complete,
    }
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

/// The curve's pricing inputs from the ReadSet (ARCH §4.2, §4.3; pump-final §6.1.5), `None`
/// when an account does not parse, the curve has migrated (`vT = 0`), its base ATA holds fewer
/// tokens than `rT`, or a **live** curve carries post-complete fields (G-3: pump writes `pcB` /
/// `pcQ` only in the buy that completes the curve, so on a live curve they are unknown; not
/// executable, A63's cap-only risk view). `supply` (the named mint's, when the caller has it)
/// resolves the synthetic exit's fee of a live curve (`CurvePricingInputs::synthetic_fees`); a
/// cashback or mayhem curve has no synthetic route (v3 refuses the one; short.win does not list
/// the other). A complete curve records `pcQ` and whether the post-complete fields are known:
/// `pcB` and `pcQ` written together, and pump's own invariant `vQ − rQ = ivqr`, on which the
/// migrating rule's `rQ` (excluding the pool part, pump-final V5) relies. There is no
/// `pcB ≥ 206.9e12` guard: nothing subtracts `pcB` (§5.4 prices the live `curve_base_ata`).
fn pricing_inputs(
    market: &Market,
    read: &[AccountInfo],
    curve: &pump::BondingCurve,
    live: &CurveState,
    supply: Option<u64>,
    ema_fee: bool,
) -> Option<CurvePricingInputs> {
    let (pcb, pcq) = (curve.post_complete_base_out, curve.post_complete_quote_in);
    let post_complete = pcb != 0 || pcq != 0;
    // G-3: pump writes pc* only in the buy that completes the curve; on a live curve they are
    // unknown.
    if !curve.complete && post_complete {
        return None;
    }
    let global_info = read.get(C_GLOBAL)?;
    let global_data = global_info.try_borrow_data().ok()?;
    let global = pump::read_global(&global_info.owner.to_bytes(), &global_data).ok()?;
    let fc_info = read.get(C_FEE_CONFIG)?;
    let fc_data = fc_info.try_borrow_data().ok()?;
    let fc = pump::read_fee_config(&fc_info.owner.to_bytes(), &fc_data).ok()?;
    let curve_tiers = math_tiers(fc.fee_tiers);
    let schedule = CurveFeeSchedule {
        tiers: &curve_tiers,
        creator_fee_configurable: global.creator_fee_configurable(),
        curve_creator_fee_bps: curve.creator_fee_bps,
        creator_is_default: curve.creator_is_default(),
        cap_bps: None,
    };
    let fees = fees::pump_curve(live.vt, live.vq, &schedule).ok()?;
    let stage = if curve.complete {
        // The migrating rule relies on `rQ` excluding the pool part (V5), which pump's invariant
        // `vQ − rQ == ivqr` checks, and on pcB and pcQ being written together.
        CurveStage::Complete {
            post_complete_quote_in: pcq,
            post_complete,
            post_complete_known: !post_complete
                || ((pcb != 0) == (pcq != 0) && curve.quote_offset_consistent()),
        }
    } else {
        // pump-final §5.2 (P1 correction (a)): pump charges both parts of a buy past `rT` at the
        // curve's pre-trade market cap on the mint's live supply. v3 refuses a cashback coin
        // (6094; D1 (a)) and short.win does not list mayhem: no synthetic route for either.
        let route = !curve.is_cashback_coin && !curve.is_mayhem_mode;
        CurveStage::Live {
            synthetic_fees: supply
                .filter(|_| route)
                .and_then(|s| fees::pump_curve_at_supply(live.vt, live.vq, s, &schedule).ok()),
        }
    };
    // M7-SEC-5: the fee at the stored `P_ema`'s market cap (`⌊vQ·10¹⁵/vT⌋` is the price times
    // the supply).
    let p_ema = market.ema_price.get();
    let fee_ppm_at_ema = (ema_fee && p_ema > 0)
        .then(|| mul_div(p_ema, live.vt, WAD, Rounding::Floor).ok())
        .flatten()
        .and_then(|vq| fees::pump_curve(live.vt, vq, &schedule).ok())
        .map_or(u32::MAX, |f| f.oneway_ppm());
    let offsets = CurveOffsets::of(live)?;
    // C4: the curve's base ATA (the tokens `migrate_v2` seeds the pool with, ARCH §4.3; the live
    // `X_s` of a synthetic exit's pool part, and `X₁` after a pool part, pump-final §5.4).
    let ata = read.get(C_CURVE_BASE_ATA)?;
    if *ata.owner != market.token_program {
        return None;
    }
    let t = token_state(ata).ok()?;
    if t.mint != market.mint.to_bytes() || t.owner != market.bonding_curve.to_bytes() {
        return None;
    }
    let live_pool_tokens = t.amount.checked_sub(curve.real_token_reserves)?;
    let pfc_info = read.get(C_PAMM_FEE_CONFIG)?;
    let pfc_data = pfc_info.try_borrow_data().ok()?;
    let pfc = pump::read_fee_config(&pfc_info.owner.to_bytes(), &pfc_data).ok()?;
    let gc_info = read.get(C_PAMM_GLOBAL_CONFIG)?;
    let gc_data = gc_info.try_borrow_data().ok()?;
    let gc = pump_amm::read_global_config(&gc_info.owner.to_bytes(), &gc_data).ok()?;
    let mut inputs = CurvePricingInputs {
        fees,
        offsets,
        live_pool_tokens,
        migration_fee: global.pool_migration_fee(),
        pool_tiers: Rc::from(math_tiers(pfc.fee_tiers)),
        pool_creator_fee_configurable: gc.creator_fee_configurable(),
        pool_disable_flags: gc.disable_flags(),
        coin_creator_is_default: curve.creator_is_default(),
        fee_ppm_at_ema,
        stage_word: 0,
        stage_flags: 0,
    };
    inputs.set_stage(stage);
    Some(inputs)
}

/// The venue state of a `PumpCurve` or `PumpMigrating` market from its key-checked ReadSet
/// (module docs). Never fails: whatever does not parse is "not executable" (A63); a curve that
/// parses always yields its [`CurveVenue`] (the prologue's [`sync`] reads it).
/// `supply`: the named mint's live supply, for the synthetic exit's fee
/// (`CurvePricingInputs::synthetic_fees`; `None` leaves it unresolved). `ema_fee`: also resolve
/// the fee at the stored `P_ema`'s market cap (`CurvePricingInputs::fee_ppm_at_ema`, M7-SEC-5;
/// `open_short`'s pre-trade read only).
pub fn read_state(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    supply: Option<u64>,
    ema_fee: bool,
) -> VenueState {
    let mut st = VenueState::not_executable(kind);
    let Some(curve_info) = read.get(C_CURVE) else {
        return st;
    };
    let Some(curve) = parse_curve(curve_info) else {
        return st;
    };
    let live = curve_state(&curve);
    // Only a complete curve reads its canonical pool; an account there that does not read as a
    // valid pool is `Unreadable`, never "no pool" (M5-SEC-5, A63). A valid one is kept for the
    // `→ PumpSwap` transition (M6).
    let (canonical_pool, pool) = match read.get(C_CANONICAL_POOL) {
        Some(cp) if curve.complete => super::detect::canonical_pool_read(
            cp,
            &market.mint,
            &super::detect::pool_authority_of(market),
        ),
        _ => (CanonicalPool::Absent, None),
    };
    let pricing = if live.vt > 0 {
        pricing_inputs(market, read, &curve, &live, supply, ema_fee)
    } else {
        None
    };
    // What short.win lists and observes (ARCH §2.5; ADOPTED Q6): a SOL curve, not mayhem,
    // ≥ 115 bytes; `depth` 0 is defence in depth (a SOL curve has depth 0).
    let listable = curve_info.data_len() >= pump::CURVE_MIN_LEN
        && curve.is_sol_quote()
        && !curve.is_mayhem_mode
        && curve.depth == 0;
    let reserves = live.vt > 0 && live.vq > 0;
    let venue = CurveVenue {
        live,
        canonical_pool,
        pool,
        pricing,
    };
    // D1 (a): v3 refuses a cashback coin (6094), so a live cashback curve never executes. The
    // term sits here and not on `listable` (pump-final §6.1.6, R-4): a complete cashback curve
    // is still observed and priced as `PumpMigrating` (C2: graduated cashback coins list).
    st.executable = listable
        && !curve.is_cashback_coin
        && reserves
        && !live.complete
        && venue.pricing.is_some();
    if listable && reserves && (st.executable || venue.migrating()) {
        st.x = curve.virtual_token_reserves;
        st.y = curve.virtual_quote_reserves;
        st.r = curve.real_quote_reserves;
    }
    if st.executable {
        st.fee_ppm = venue.pricing.as_ref().map_or(0, |p| p.fees.oneway_ppm());
    }
    st.curve = Some(venue);
    st
}

/// A curve venue transition of the prologue's venue sync (ARCH §2.5 `sync_venue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub from: VenueKind,
    pub to: VenueKind,
}

/// ARCH §2.5 `sync_venue` for the curve kinds, on the state this instruction read:
///
/// ```text
/// PumpCurve     : complete ? (cp Valid ? PumpSwap : (cp Absent ∧ vT == 0 ? None* : PumpMigrating)) : stay
/// PumpMigrating : cp Valid ? PumpSwap : (cp Absent ∧ vT == 0 ? None* : stay)
/// ```
///
/// `→ PumpSwap` (M6; `pump_amm::graduate`): the canonical pool is valid — `migrate_v2` ran, in
/// this very slot perhaps, a completion and a migration in one transaction included (M5-SEC-4) —
/// so the market moves onto it: the pool cache, `graduated_at = now`, the route chains and a
/// collection window cleared, the tier C until the next crank. The observation state's rewrite
/// and restart are the prologue's (`pump_amm::restart_observation` on its copy of the state).
/// `→ PumpMigrating` sets `migration_seen_at = now`; `→ None` (a curve that completed with zeroed
/// reserves and no canonical pool: pump changed its migration target) sets the
/// `graduated_elsewhere` flag — the market then has no route, its S3 chain runs, and a Raydium
/// pool can be adopted through M4's collection window (`propose_pool`, §2.5 `None` row). A curve
/// that does not parse changes nothing (not executable, A63), and neither does a canonical-pool
/// account that does not read as a valid pool (`CanonicalPool::Unreadable`, M5-SEC-5): it is a
/// pool short.win cannot read, never the absence of one, so it counts as "cp exists" — the
/// market stays on its pump kind (not executable, the migration pause intact), and only a
/// System-owned address (nothing there) reads as "no canonical pool". Returns the transition, if
/// any.
pub fn sync(market: &mut Market, state: &VenueState, now: i64) -> Result<Option<Transition>> {
    let Some(from) = market.venue_kind() else {
        return Ok(None);
    };
    if !matches!(from, VenueKind::PumpCurve | VenueKind::PumpMigrating) {
        return Ok(None);
    }
    let Some(c) = state.curve.as_ref() else {
        return Ok(None);
    };
    if from == VenueKind::PumpCurve && !c.live.complete {
        return Ok(None);
    }
    if let (CanonicalPool::Valid, Some(pool)) = (c.canonical_pool, c.pool.as_ref()) {
        super::pump_amm::graduate(market, pool, now)?;
        return Ok(Some(Transition {
            from,
            to: VenueKind::PumpSwap,
        }));
    }
    let to = if c.canonical_pool.exists() || c.live.vt > 0 {
        VenueKind::PumpMigrating
    } else {
        VenueKind::None
    };
    if to == from {
        return Ok(None);
    }
    market.venue = to as u8;
    match to {
        VenueKind::PumpMigrating => market.migration_seen_at = now,
        _ => market.set_flag(MARKET_FLAG_GRADUATED_ELSEWHERE, true),
    }
    Ok(Some(Transition { from, to }))
}

/// The liquidity anchor of a `→ PumpSwap` transition this state carries (`pump_amm::
/// graduation_anchor` of the valid canonical pool the curve's ReadSet read).
pub fn graduation_anchor(state: &VenueState) -> Option<u128> {
    state
        .curve
        .as_ref()
        .and_then(|c| c.pool.as_ref())
        .and_then(super::pump_amm::graduation_anchor)
}

/// An exit's venue checks after its prologue (ARCH §3.5 step 2, §3.6 precondition 1), every
/// venue: a venue sync that just moved the market off the curve refuses the exit
/// (`VenueMigrating` for `PumpMigrating` or `PumpSwap`, else `NoRoute`); the venue must be
/// executable (`NoRoute`). A keeper exit (`liquidate`, `expire`) checks nothing more here: a
/// buyback the curve and its would-be pool cannot deliver has no execution quote, so its
/// "buyback allowed" fails with `UseSolSettle` (pump-final §6.7) and `sol_settle` decides.
pub fn exit_venue_checks(
    pro: &crate::engine::prologue::Prologue,
    state: &VenueState,
) -> Result<()> {
    if let Some(t) = pro.transition {
        return transition_refusal(t);
    }
    require!(state.executable, ShortwinError::NoRoute);
    Ok(())
}

/// A borrower's close: [`exit_venue_checks`], then on a live curve with `q > rT` the synthetic
/// route must exist and fit (pump-final §6.1.7): `buy_v3` buys `rT` on the curve and `q − rT`
/// from the would-be pool, which holds `X_s = curve_base_ata − rT` (`live_pool_tokens`); pump
/// refuses `q − rT ≥ X_s` (`NotEnoughTokensToBuy`), and a curve without a synthetic route (its
/// fee not resolved on the mint's supply) cannot buy past `rT` at all. Else `CurveStraddle`. The
/// `q − rT ≥ X_s` branch is unreachable under the current caps (pump-final C8: the per-market
/// cap keeps `q ≤ ≈ 4.99e13` while `X_s ≥ 2.069e14`) and stays as a guard.
pub fn close_venue_checks(
    pro: &crate::engine::prologue::Prologue,
    state: &VenueState,
    q: u64,
) -> Result<()> {
    exit_venue_checks(pro, state)?;
    if let (VenueKind::PumpCurve, Some(c)) = (state.kind, state.curve.as_ref()) {
        if u128::from(q) > c.live.rt {
            let fits = c.pricing.as_ref().is_some_and(|p| {
                p.synthetic_fees().is_some()
                    && u128::from(q).saturating_sub(c.live.rt) < u128::from(p.live_pool_tokens)
            });
            // Beyond what the curve and its post-migration pool hold.
            require!(fits, ShortwinError::CurveStraddle);
        }
    }
    Ok(())
}

/// The refusal of a trade whose own prologue moved the market off its venue: `VenueMigrating`
/// onto `PumpMigrating` or `PumpSwap` (the ReadSet it carried is the old venue's; the transition
/// reverts with it and the next crank persists it), `NoRoute` onto `None`.
pub fn transition_refusal<T>(t: Transition) -> Result<T> {
    if matches!(t.to, VenueKind::PumpMigrating | VenueKind::PumpSwap) {
        err!(ShortwinError::VenueMigrating)
    } else {
        err!(ShortwinError::NoRoute)
    }
}

/// After a buy leg on the curve (ARCH §3.5 step 7, §5.4 case 1; pump-final §6.1.7): a buyback
/// of `q ≥ rT` completed the curve (exactly `rT`, or past it by pump's synthetic migration), so
/// the venue sync moves the market to `PumpMigrating` at once (`migration_seen_at = now`). Reads
/// only the curve (C0); `None` off the curve or while it is live.
pub fn sync_after_leg(
    market: &mut Market,
    kind: VenueKind,
    read: &[AccountInfo],
    now: i64,
) -> Option<Transition> {
    if kind != VenueKind::PumpCurve {
        return None;
    }
    let c = read.get(C_CURVE).and_then(parse_curve)?;
    let mut st = VenueState::not_executable(kind);
    st.curve = Some(CurveVenue {
        live: curve_state(&c),
        canonical_pool: CanonicalPool::Absent,
        pool: None,
        pricing: None,
    });
    sync(market, &st, now).ok().flatten()
}

/// The `VenueStateChanged` event of a transition (ARCH §3.10): the canonical pool (the curve
/// kinds' pool key) and `graduated_at`.
pub fn transition_event(
    market: &Market,
    t: Transition,
    market_key: Pubkey,
    clock: &Clock,
) -> crate::events::VenueStateChanged {
    crate::events::VenueStateChanged {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        from: t.from as u8,
        to: t.to as u8,
        pool: market.canonical_pool,
        graduated_at: market.graduated_at,
    }
}

// ---- legs --------------------------------------------------------------------------------

/// The trader's pump UVA (T6) exists and accumulates for `trader` (ARCH §3.12: pre-created at
/// listing, so no leg pays its rent; a missing or foreign one is `VenueAccountMismatch`, before
/// the CPI).
fn check_uva(leg: &Leg) -> Result<()> {
    let uva = raydium::at(leg.trade, T_UVA)?;
    require_keys_eq!(
        *uva.owner,
        PUMP_PROGRAM_ID,
        ShortwinError::VenueAccountMismatch
    );
    let data = uva.try_borrow_data()?;
    let u = pump::read_user_volume_accumulator(&uva.owner.to_bytes(), &data)
        .map_err(|_| error!(ShortwinError::VenueAccountMismatch))?;
    require!(
        u.user == leg.trader.trader.key.to_bytes(),
        ShortwinError::VenueAccountMismatch
    );
    Ok(())
}

fn meta(info: &AccountInfo, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(info.key(), false)
    } else {
        AccountMeta::new_readonly(info.key(), false)
    }
}

/// Where an account of the v3 trade CPI comes from ([`TRADE_CPI_ORDER`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpiSource {
    /// A ReadSet slot (`C_*`).
    Read(usize),
    /// A TradeExtras slot (`T_*`).
    Trade(usize),
    /// The instruction's named base mint.
    Mint,
    /// The instruction's named base token program.
    TokenProgram,
    /// The market's trader PDA: pump's `user`, the only signer.
    Trader,
}

/// The v3 trade's accounts in the IDL's order (pump-final §6.1.3; one list for `buy_v3`,
/// `sell_v3` and `buy_exact_quote_in_v3`, pump-public-docs `2293f9a6`), with their writability:
///
/// | # | IDL name | from | W |
/// |---|---|---|---|
/// | 1 | `global` | C1 | |
/// | 2 | `base_mint` | the named mint | |
/// | 3 | `quote_mint` | T2 | |
/// | 4 | `base_token_program` | the named token program | |
/// | 5 | `quote_token_program` | T3 | |
/// | 6 | `bonding_curve` | C0 | W |
/// | 7 | `associated_base_bonding_curve` | C4 | W |
/// | 8 | `associated_quote_bonding_curve` | T4 | W |
/// | 9 | `user` | the trader PDA (the only signer) | W |
/// | 10 | `associated_base_user` | T8 | W |
/// | 11 | `associated_quote_user` | T5 | W |
/// | 12 | `user_volume_accumulator` | T6 | W |
/// | 13 | `fee_config` | C2 | |
/// | 14 | `buyback_fee_recipient` | T7 | W |
/// | 15 | `system_program` | T9 | |
/// | 16 | `event_authority` | T1 | |
/// | 17 | `program` | T0 | |
pub const TRADE_CPI_ORDER: [(CpiSource, bool); TRADE_CPI_ACCOUNTS] = [
    (CpiSource::Read(C_GLOBAL), false),
    (CpiSource::Mint, false),
    (CpiSource::Trade(T_WSOL_MINT), false),
    (CpiSource::TokenProgram, false),
    (CpiSource::Trade(T_TOKEN_PROGRAM), false),
    (CpiSource::Read(C_CURVE), true),
    (CpiSource::Read(C_CURVE_BASE_ATA), true),
    (CpiSource::Trade(T_CURVE_QUOTE_ATA), true),
    (CpiSource::Trader, true),
    (CpiSource::Trade(T_TRADER_BASE_ATA), true),
    (CpiSource::Trade(T_TRADER_WSOL_ATA), true),
    (CpiSource::Trade(T_UVA), true),
    (CpiSource::Read(C_FEE_CONFIG), false),
    (CpiSource::Trade(T_BUYBACK_RECIPIENT), true),
    (CpiSource::Trade(T_SYSTEM), false),
    (CpiSource::Trade(T_EVENT_AUTHORITY), false),
    (CpiSource::Trade(T_PROGRAM), false),
];

/// [`TRADE_CPI_ORDER`]'s metas and infos for `leg`: `trader` the only signer.
fn trade_accounts<'info>(leg: &Leg<'info>) -> Result<(Vec<AccountMeta>, Vec<AccountInfo<'info>>)> {
    let mut metas = Vec::with_capacity(TRADE_CPI_ACCOUNTS);
    let mut infos = Vec::with_capacity(TRADE_CPI_ACCOUNTS);
    for (source, writable) in TRADE_CPI_ORDER {
        let info = match source {
            CpiSource::Read(i) => raydium::at(leg.read, i)?,
            CpiSource::Trade(i) => raydium::at(leg.trade, i)?,
            CpiSource::Mint => &leg.mint,
            CpiSource::TokenProgram => &leg.token_program,
            CpiSource::Trader => &leg.trader.trader,
        };
        let mut m = meta(info, writable);
        m.is_signer = source == CpiSource::Trader;
        metas.push(m);
        infos.push(info.clone());
    }
    Ok((metas, infos))
}

/// The 24 data bytes of a v3 trade: `(buy ? buy_v3 : sell_v3) ‖ amount ‖ bound`, never the
/// optional `partial_fill` byte. `bound` is `max_sol_cost = max_in` on a buy and
/// `min_sol_output = min_out` on a sale.
pub fn trade_data(buy: bool, amount: u64, bound: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(TRADE_DATA_LEN);
    data.extend_from_slice(if buy { &IX_BUY_V3 } else { &IX_SELL_V3 });
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&bound.to_le_bytes());
    data
}

/// `buy_v3(amount, max_sol_cost)` or `sell_v3(amount, min_sol_output)`, `trader` signing, the
/// extras appended (A41; `raydium::invoke_as_trader`).
fn trade_cpi(leg: &Leg, buy: bool, amount: u64, bound: u64) -> Result<()> {
    let (metas, infos) = trade_accounts(leg)?;
    let data = trade_data(buy, amount, bound);
    raydium::invoke_as_trader(leg, PUMP_PROGRAM_ID, data, metas, infos)
}

fn base_side<'a, 'info>(leg: &'a Leg<'info>) -> Result<&'a crate::engine::trader::BaseSide<'info>> {
    leg.trader
        .base
        .as_ref()
        .ok_or_else(|| error!(ShortwinError::BadRemainingAccounts))
}

/// The sell leg of an open (module docs; ARCH §2.4.1 PumpCurve row): `sell_v3(q, min_out)`;
/// returns `S`, the proceeds now on the position, net of any venue-side rent pump took from the
/// trader (a curve or UVA growth; v3 never tops up a creator vault); the borrower got its `ert`
/// back.
pub fn sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    min_out: u64,
) -> Result<u64> {
    check_uva(leg)?;
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
    // 2. The rent buffer `ert` from the borrower (ARCH §2.4 rule 3: `sell_v3` may take venue-side
    //    rent — a curve or UVA growth — from `user` before crediting the proceeds).
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
    // 3. sell_v3(Q, min_out): the proceeds are credited to the trader's lamports.
    trade_cpi(leg, false, q, min_out)?;
    // 4. Pump took exactly Q, and the pool vault gave exactly Q.
    require!(
        base_ata_state(trader, base)?.amount == 0,
        ShortwinError::VenueResultMismatch
    );
    let vault_after = token_state(&leg.pool_vault)?.amount;
    require!(
        vault_before.checked_sub(vault_after) == Some(q),
        ShortwinError::VenueResultMismatch
    );
    // 5. S = trader − ert; S to the position, ert back to the borrower.
    let s = trader
        .lamports()
        .checked_sub(leg.ert)
        .ok_or(ShortwinError::VenueRentOnSell)?;
    leg.trader.pay_from_trader(position, s)?;
    leg.trader.pay_from_trader(borrower, leg.ert)?;
    Ok(s)
}

/// The buy leg of an exit (module docs; ARCH §2.4.1 PumpCurve row): `buy_v3(q, max_in)` paid by
/// the trader PDA from `max_in + ert` of the position; every token bought goes to the pool vault.
/// For `q > rT` pump completes the curve and buys `q − rT` from the would-be pool in the same
/// CPI (synthetic migration): one delivery of `q`, one charge, all or nothing — a short delivery
/// reverts the instruction (`bought ≥ q`).
pub fn buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
) -> Result<BuyOut> {
    check_uva(leg)?;
    let base = base_side(leg)?;
    let trader = &leg.trader.trader;
    let lamports_before = position.lamports();
    let fund = max_in.checked_add(leg.ert).or_math()?;
    // 1. Fund the trader from the position (a direct debit, ARCH §2.4 rule 3), then a zero System
    //    transfer between them so the runtime's per-CPI balance check sees both.
    lamports::move_lamports_in_leg(position, trader, fund)?;
    leg.trader.pay_from_trader(position, 0)?;
    // 2. buy_v3(q, max_sol_cost = max_in): System transfers from the trader PDA (T-PDA-USER).
    trade_cpi(leg, true, q, max_in)?;
    // 3. Every lamport left goes back to the position.
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
    require_gte!(fund, spent, ShortwinError::VenueResultMismatch);
    Ok(BuyOut { spent, received })
}

// ---- listing -----------------------------------------------------------------------------

/// Pump's `init_user_volume_accumulator(payer, user = trader)` (R-PUMP §3.6; ARCH §3.2 "Effects"):
/// `[payer S W, user, user_volume_accumulator W, system, event_authority, program]`. Skipped when
/// the UVA already exists (anyone may create it first: it is keyed to `trader`, harmless).
/// `accounts` is `[pump program, the trader's UVA, pump event authority]`, key-checked here
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
        PUMP_PROGRAM_ID,
        ShortwinError::VenueAccountMismatch
    );
    require!(program.executable, ShortwinError::VenueAccountMismatch);
    require_keys_eq!(uva.key(), *uva_key, ShortwinError::VenueAccountMismatch);
    require_keys_eq!(
        event_authority.key(),
        PUMP_EVENT_AUTHORITY,
        ShortwinError::VenueAccountMismatch
    );
    if *uva.owner == PUMP_PROGRAM_ID && !uva.data_is_empty() {
        return Ok(());
    }
    let ix = Instruction {
        program_id: PUMP_PROGRAM_ID,
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

/// `P = ⌊vQ·WAD/vT⌋` and `L = isqrt(vT·vQ)` of a curve (ARCH §4.4 "Initialisation": a pump
/// listing starts its price state at the curve's spot); `None` if either is undefined.
pub fn spot(c: &pump::BondingCurve) -> Option<(u128, u128)> {
    let (x, y) = (
        u128::from(c.virtual_token_reserves),
        u128::from(c.virtual_quote_reserves),
    );
    let p = shortwin_math::risk::price_wad(x, y)
        .ok()
        .filter(|p| *p > 0)?;
    let l = shortwin_math::quote::liquidity(x, y)
        .ok()
        .filter(|l| *l > 0)?;
    Some((p, l))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    /// pump-final §6.1.9: the v3 discriminators re-derive, the data is 24 bytes (no
    /// `partial_fill` byte, ever), and the TradeExtras are 10 slots.
    #[test]
    fn discriminators_and_data_length() {
        let disc = |name: &str| {
            let h = solana_sha256_hasher::hash(format!("global:{name}").as_bytes());
            <[u8; 8]>::try_from(&h.to_bytes()[..8]).unwrap()
        };
        assert_eq!(disc("buy_v3"), IX_BUY_V3);
        assert_eq!(disc("sell_v3"), IX_SELL_V3);
        assert_eq!(
            disc("init_user_volume_accumulator"),
            IX_INIT_USER_VOLUME_ACCUMULATOR
        );
        assert_eq!(TRADE_DATA_LEN, 8 + 8 + 8);
        assert_eq!(READ_LEN, 7);
        assert_eq!(TRADE_LEN, T_SYSTEM + 1);
        assert_eq!(TRADE_LEN, 10);
        let d = trade_data(true, 7, 9);
        assert_eq!(d.len(), TRADE_DATA_LEN);
        assert_eq!(d[..8], IX_BUY_V3);
        assert_eq!(d[8..16], 7u64.to_le_bytes());
        assert_eq!(d[16..], 9u64.to_le_bytes());
        assert_eq!(trade_data(false, 1, 2)[..8], IX_SELL_V3);
        // Every slot is named once: the nine fixed ones and the live-checked buyback recipient.
        let mut m = <Market as bytemuck::Zeroable>::zeroed();
        m.trader = Pubkey::new_unique();
        let mut slots: Vec<usize> = fixed_trade_keys(&m).iter().map(|(i, _)| *i).collect();
        slots.push(T_BUYBACK_RECIPIENT);
        slots.sort_unstable();
        assert_eq!(slots, (0..TRADE_LEN).collect::<Vec<_>>());
    }

    /// pump-final §6.1.3 (`trade_accounts_v3_order`): the CPI's 17 accounts in the IDL's order
    /// for `buy_v3` / `sell_v3` (pump-public-docs `2293f9a6`), with their sources, writability and
    /// the trader PDA as the single signer; every TradeExtras slot and the ReadSet's `Global`,
    /// curve, base ATA and `FeeConfig` reach the CPI, nothing else does.
    #[test]
    fn trade_accounts_v3_order() {
        use CpiSource::{Mint, Read, TokenProgram, Trade, Trader};
        let idl: [(&str, CpiSource, bool); TRADE_CPI_ACCOUNTS] = [
            ("global", Read(C_GLOBAL), false),
            ("base_mint", Mint, false),
            ("quote_mint", Trade(T_WSOL_MINT), false),
            ("base_token_program", TokenProgram, false),
            ("quote_token_program", Trade(T_TOKEN_PROGRAM), false),
            ("bonding_curve", Read(C_CURVE), true),
            (
                "associated_base_bonding_curve",
                Read(C_CURVE_BASE_ATA),
                true,
            ),
            (
                "associated_quote_bonding_curve",
                Trade(T_CURVE_QUOTE_ATA),
                true,
            ),
            ("user", Trader, true),
            ("associated_base_user", Trade(T_TRADER_BASE_ATA), true),
            ("associated_quote_user", Trade(T_TRADER_WSOL_ATA), true),
            ("user_volume_accumulator", Trade(T_UVA), true),
            ("fee_config", Read(C_FEE_CONFIG), false),
            ("buyback_fee_recipient", Trade(T_BUYBACK_RECIPIENT), true),
            ("system_program", Trade(T_SYSTEM), false),
            ("event_authority", Trade(T_EVENT_AUTHORITY), false),
            ("program", Trade(T_PROGRAM), false),
        ];
        for (i, ((name, source, writable), got)) in idl.iter().zip(TRADE_CPI_ORDER).enumerate() {
            assert_eq!((*source, *writable), got, "#{} {name}", i + 1);
        }
        let signers: Vec<usize> = TRADE_CPI_ORDER
            .iter()
            .enumerate()
            .filter(|(_, (s, _))| *s == Trader)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(signers, [8]);
        let mut trade: Vec<usize> = TRADE_CPI_ORDER
            .iter()
            .filter_map(|(s, _)| match s {
                Trade(i) => Some(*i),
                _ => None,
            })
            .collect();
        trade.sort_unstable();
        assert_eq!(trade, (0..TRADE_LEN).collect::<Vec<_>>());
        let mut read: Vec<usize> = TRADE_CPI_ORDER
            .iter()
            .filter_map(|(s, _)| match s {
                Read(i) => Some(*i),
                _ => None,
            })
            .collect();
        read.sort_unstable();
        assert_eq!(read, [C_CURVE, C_GLOBAL, C_FEE_CONFIG, C_CURVE_BASE_ATA]);
    }

    /// M5-SEC-6: every entry of the recipient table is the WSOL ATA of its recipient, and an
    /// unknown recipient is derived.
    #[test]
    fn the_recipient_table_re_derives() {
        for (recipient, ata) in RECIPIENT_WSOL_ATAS {
            assert_eq!(wsol_ata(&recipient), ata, "{recipient}");
            assert_eq!(recipient_wsol_ata(&recipient), ata);
        }
        let other = Pubkey::new_unique();
        assert_eq!(recipient_wsol_ata(&other), wsol_ata(&other));
        // The cached trader ATA is used as is; an empty cache is derived.
        let mut m = <Market as bytemuck::Zeroable>::zeroed();
        m.trader = Pubkey::new_unique();
        assert_eq!(trader_wsol_ata(&m), wsol_ata(&m.trader));
        m.trader_wsol_ata = Pubkey::new_unique();
        assert_eq!(trader_wsol_ata(&m), m.trader_wsol_ata);
    }

    /// The static keys the adapter checks re-derive (ARCH §2.3.2).
    #[test]
    fn pump_uva_and_vault_seeds() {
        let user = Pubkey::new_unique();
        assert_eq!(
            user_volume_accumulator(&user),
            Pubkey::find_program_address(
                &[b"user_volume_accumulator", user.as_ref()],
                &PUMP_PROGRAM_ID
            )
            .0
        );
        let creator = Pubkey::new_unique();
        assert_eq!(
            creator_vault(&creator),
            Pubkey::find_program_address(&[b"creator-vault", creator.as_ref()], &PUMP_PROGRAM_ID).0
        );
    }
}
