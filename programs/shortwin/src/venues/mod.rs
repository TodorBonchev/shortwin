//! Venue adapters (ARCH §2.1 `venues/`, §3.0 "Remaining-accounts convention", §3.12).
//!
//! Venue accounts arrive in `remaining_accounts` as
//!
//! ```text
//! ReadSet(venue) ++ TradeExtras(venue) [trading instructions] ++ Extras(venue, ix) [A41]
//! ```
//!
//! and are parsed by a strict per-venue parser ([`parse`]): every slot is mandatory and
//! key-checked against a compile-time constant, a `Market` cache entry or an on-chain
//! re-derivation; a missing, extra or wrong account reverts (`BadRemainingAccounts`,
//! `VenueAccountMismatch`) and never maps to "no route" (C0-2). A correctly keyed venue-state
//! account whose contents no longer parse makes the venue **not executable** (A63, V1-8):
//! [`VenueState`] then says so, and the route logic (`engine::route`) decides.
//!
//! **Extras (A41; O6).** After the adapter's own list come exactly the accounts of the extras
//! set the caller selects in the instruction data ([`ExtrasSel`]): `none` (nothing follows),
//! or `current` / `previous` / `stable` (M2-SEC-2) of `VenueExtras[venue][ix]`, in which case
//! the `VenueExtras`
//! account itself comes first and its 0–4 entries follow in order ([`parse_extras`]). A fixed
//! entry must match its key; a template entry is re-derived with `create_program_address` from
//! its constant seeds, its placeholders (`mint`, `pool`, `trader`, `bonding_curve`,
//! `coin_creator`) and the caller's bump, which must be the **canonical** one (M2-SEC-3: every
//! higher bump must fail to derive, so a caller cannot substitute a second, non-canonical PDA of
//! the template's program; ARCH says "no search" — this is a search of the bumps above the
//! caller's only, usually 0–2 extra derivations). Every extra must be a non-signer, not
//! owned by short.win, not on the denylist (ARCH §2.4 rule 4: the instruction's own list — the
//! market's PDAs and vaults, the position, the exposure record and, since M10 batch 3, the
//! position's borrower — and the adopted pool's own accounts, [`pool_denylist`]), and writable
//! exactly when its flag says so; the resolved metas are appended to the venue CPI. Because `none` and
//! `previous` stay selectable, a wrong or hostile Emergency write never bricks exits; because
//! `stable` keeps a set that worked for `EXTRAS_STABLE_S` (or that Governance published),
//! neither do several quick ones (M2-SEC-2, `set_venue_extras`).
//! *Where the `VenueExtras` account travels is this module's choice* (ARCH names the sets,
//! not the slot): first in the extras segment, only when a set is selected.
//!
//! Venue kinds with an adapter in this build: [`VenueKind::PumpCurve`] ([`pump_curve`]) since M5
//! — whose ReadSet also serves a `PumpMigrating` market (the completed curve is final and still
//! observed; exits pause, S3 after the migration pause) —, [`VenueKind::PumpSwap`]
//! ([`pump_amm`]) since M6 (the canonical pool; a curve market moves onto it in the prologue's
//! venue sync, `pump_amm::graduate`), [`VenueKind::RaydiumV4`] ([`raydium_v4`]) and
//! [`VenueKind::RaydiumCpmm`] ([`raydium_cpmm`]) since M3, sharing [`raydium`] (the delegate sell
//! out of the lending vault, the direct-to-vault buy, eligibility, adoption), and
//! [`VenueKind::Mock`] (TEST-ONLY, feature `test-venue`, [`mock`]). A list-only (`None`) market's
//! "ReadSet" is its detection set `[bonding_curve, canonical_pool]` (key-checked; `crank_price`
//! re-detects a pump venue on it, `sol_settle` runs its S3 on it, M2-SEC-5). [`detect`] is the
//! pump detection (`classify_pump`, D1 precedence).
//!
//! **Quotes** ([`Quoter`]): the constant-product venues price on `(X, Y)` with a resolved
//! [`Pricing`]; the curve on a [`CurveState`] with `shortwin_math::quote::CurvePricing` (above
//! `rT` a live curve's synthetic exit `B̂_v3`, pump-final §5.2; a `PumpMigrating` market's
//! post-migration pool `B̂_str`, whose fee tier needs the PumpSwap tier list: [`CurveVenue`]
//! carries it, which is why [`VenueState`] is `Clone` but not `Copy`).

use std::rc::Rc;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin_math::fees::{self as math_fees, FeeBps, FeeTier, PammFeeSchedule};
use shortwin_math::quote::{
    self as math_quote, BuyQuote, CurveOffsets, CurvePricing, CurveState, MigrationParams, Pricing,
    SellQuote,
};
use shortwin_math::risk::price_wad;

use crate::constants::VENUE_EXTRAS_VERSION;
use crate::engine::trader::{BaseSide, TraderAccounts, WsolSide};
use crate::errors::ShortwinError;
use crate::state::{
    Market, VenueExtras, VenueKind, ENTRY_FIXED, ENTRY_NONE, ENTRY_TEMPLATE, EXTRAS_IX_KINDS,
    EXTRAS_MAX_ENTRIES, EXTRAS_MAX_SEEDS, PLACEHOLDER_BONDING_CURVE, PLACEHOLDER_COIN_CREATOR,
    PLACEHOLDER_MINT, PLACEHOLDER_POOL, PLACEHOLDER_TRADER, SEED_CONST, SEED_NONE,
    SEED_PLACEHOLDER,
};

pub mod detect;
#[cfg(feature = "test-venue")]
pub mod mock;
pub mod pump_amm;
pub mod pump_curve;
pub mod raydium;
pub mod raydium_cpmm;
pub mod raydium_v4;

// ---- extras selection ------------------------------------------------------------------

/// `ExtrasSel.set`: no trailing accounts.
pub const EXTRAS_SET_NONE: u8 = 0;
/// `ExtrasSel.set`: the `current` set of `VenueExtras[venue][ix]`.
pub const EXTRAS_SET_CURRENT: u8 = 1;
/// `ExtrasSel.set`: the `previous` set.
pub const EXTRAS_SET_PREVIOUS: u8 = 2;
/// `ExtrasSel.set`: the `stable` set (M2-SEC-2: the newest set that stood as `current` for
/// `EXTRAS_STABLE_S`, or Governance's last publication).
pub const EXTRAS_SET_STABLE: u8 = 3;

/// The extras set a trading instruction selects (ARCH §3.0 "Bounded extras", A41): `set`
/// ([`EXTRAS_SET_NONE`], [`EXTRAS_SET_CURRENT`], [`EXTRAS_SET_PREVIOUS`],
/// [`EXTRAS_SET_STABLE`]) and, for each
/// template entry `i`, the canonical bump `bumps[i]` of the account it derives (fixed entries
/// ignore theirs; a non-canonical bump is `BadExtras`, M2-SEC-3).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtrasSel {
    pub set: u8,
    pub bumps: [u8; 4],
}

impl ExtrasSel {
    /// No trailing accounts.
    pub const NONE: Self = Self {
        set: EXTRAS_SET_NONE,
        bumps: [0; 4],
    };
}

/// The keys a template's placeholders stand for on this market (ARCH §2.3.2). A placeholder
/// the market has no value for (`None`) makes the entry unusable (`BadExtras`).
#[derive(Clone, Copy, Debug)]
pub struct Placeholders {
    pub mint: Pubkey,
    pub pool: Option<Pubkey>,
    pub trader: Pubkey,
    pub bonding_curve: Pubkey,
    pub coin_creator: Option<Pubkey>,
}

impl Placeholders {
    fn key(&self, id: u8) -> Result<Pubkey> {
        let k = match id {
            PLACEHOLDER_MINT => Some(self.mint),
            PLACEHOLDER_POOL => self.pool,
            PLACEHOLDER_TRADER => Some(self.trader),
            PLACEHOLDER_BONDING_CURVE => Some(self.bonding_curve),
            PLACEHOLDER_COIN_CREATOR => self.coin_creator,
            _ => None,
        };
        k.ok_or_else(|| error!(ShortwinError::BadExtras))
    }
}

/// Resolves the extras segment (ARCH §3.0; module docs): `accounts` is everything after the
/// adapter's own list. Returns the metas to append to the venue CPI (writable exactly as the
/// entries say, never signers) in order, together with their infos.
///
/// Errors: `BadRemainingAccounts` if the segment has the wrong length (a `none` selection with
/// trailing accounts, or fewer / more accounts than the set's entries); `BadExtras` for an
/// unknown selection, a `VenueExtras` account that is not this venue's (PDA with its stored
/// bump, owner, discriminator, version, venue byte), an entry that does not match (fixed key,
/// template re-derivation, writable flag), or a placeholder the market has no value for;
/// `DeniedAccount` for a signer, a short.win-owned account or a key on either denylist (`deny`,
/// the instruction's; `pool_deny`, the adopted pool's own accounts, [`pool_denylist`]).
pub fn parse_extras<'info>(
    kind: VenueKind,
    ix: usize,
    sel: &ExtrasSel,
    accounts: &'info [AccountInfo<'info>],
    ph: &Placeholders,
    deny: &[Pubkey],
    pool_deny: &[Pubkey],
) -> Result<(Vec<AccountMeta>, &'info [AccountInfo<'info>])> {
    if sel.set == EXTRAS_SET_NONE {
        require!(accounts.is_empty(), ShortwinError::BadRemainingAccounts);
        return Ok((Vec::new(), accounts));
    }
    require!(
        matches!(
            sel.set,
            EXTRAS_SET_CURRENT | EXTRAS_SET_PREVIOUS | EXTRAS_SET_STABLE
        ),
        ShortwinError::BadExtras
    );
    require!(ix < EXTRAS_IX_KINDS, ShortwinError::BadExtras);
    let seed = kind.extras_seed().ok_or(ShortwinError::BadExtras)?;
    let (head, entries_infos) = accounts
        .split_first()
        .ok_or(ShortwinError::BadRemainingAccounts)?;
    let loader = AccountLoader::<VenueExtras>::try_from(head)
        .map_err(|_| error!(ShortwinError::BadExtras))?;
    let ve = loader
        .load()
        .map_err(|_| error!(ShortwinError::BadExtras))?;
    let pda = Pubkey::create_program_address(&[VenueExtras::SEED, &[seed], &[ve.bump]], &crate::ID)
        .map_err(|_| error!(ShortwinError::BadExtras))?;
    require_keys_eq!(pda, head.key(), ShortwinError::BadExtras);
    require!(
        ve.version == VENUE_EXTRAS_VERSION && ve.venue == seed,
        ShortwinError::BadExtras
    );
    let ixe = ve.sets.get(ix).ok_or(ShortwinError::BadExtras)?;
    let set = match sel.set {
        EXTRAS_SET_CURRENT => &ixe.current,
        EXTRAS_SET_PREVIOUS => &ixe.previous,
        _ => &ixe.stable,
    };
    let n = usize::from(set.count);
    require!(n <= EXTRAS_MAX_ENTRIES, ShortwinError::BadExtras);
    require!(
        entries_infos.len() == n,
        ShortwinError::BadRemainingAccounts
    );
    let mut metas = Vec::with_capacity(n);
    for (i, (entry, info)) in set.entries.iter().zip(entries_infos.iter()).enumerate() {
        let expected = match entry.kind {
            ENTRY_FIXED => entry.key,
            ENTRY_TEMPLATE => {
                let bump = *sel.bumps.get(i).ok_or(ShortwinError::BadExtras)?;
                derive_template(entry, ph, bump)?
            }
            _ => return err!(ShortwinError::BadExtras),
        };
        require_keys_eq!(info.key(), expected, ShortwinError::BadExtras);
        require!(!info.is_signer, ShortwinError::DeniedAccount);
        require_keys_neq!(*info.owner, crate::ID, ShortwinError::DeniedAccount);
        require!(
            !deny.contains(info.key) && !pool_deny.contains(info.key),
            ShortwinError::DeniedAccount
        );
        let writable = entry.writable == 1;
        require!(info.is_writable == writable, ShortwinError::BadExtras);
        metas.push(AccountMeta {
            pubkey: info.key(),
            is_signer: false,
            is_writable: writable,
        });
    }
    Ok((metas, entries_infos))
}

/// `create_program_address(seeds ++ [bump], program)`, `None` if that is on the curve.
fn derive_with_bump(seeds: &[&[u8]], bump: u8, program: &Pubkey) -> Option<Pubkey> {
    let bump_seed = [bump];
    let mut all: [&[u8]; EXTRAS_MAX_SEEDS + 1] = [&[]; EXTRAS_MAX_SEEDS + 1];
    let n = seeds.len();
    all.get_mut(..n)?.copy_from_slice(seeds);
    *all.get_mut(n)? = &bump_seed;
    Pubkey::create_program_address(all.get(..=n)?, program).ok()
}

/// `create_program_address(seeds ++ [bump], program)` of a template entry, for the
/// **canonical** bump only (M2-SEC-3): the caller's `bump` must derive an off-curve address and
/// every higher bump must fail to (what `find_program_address` returns). Without this check any
/// off-curve bump was accepted, so a caller — the keeper on `liquidate` / `expire`, whose venue
/// CPI spends the borrower's SOL — could pass a second, non-canonical PDA of the template's
/// program instead of the account the Emergency multisig meant; a venue that consumes such a
/// trailing account without re-deriving it canonically would take it. The bumps above a
/// canonical one are on the curve with probability ½ each, so this costs 0–2 extra derivations
/// in practice.
fn derive_template(
    entry: &crate::state::ExtraEntry,
    ph: &Placeholders,
    bump: u8,
) -> Result<Pubkey> {
    let n = usize::from(entry.n_seeds);
    require!(
        (1..=EXTRAS_MAX_SEEDS).contains(&n),
        ShortwinError::BadExtras
    );
    // Placeholder keys must outlive the seed slices: resolve them first.
    let mut keys = [Pubkey::default(); EXTRAS_MAX_SEEDS];
    for (k, part) in keys.iter_mut().zip(entry.seeds.iter()).take(n) {
        if part.kind == SEED_PLACEHOLDER {
            *k = ph.key(part.len)?;
        }
    }
    let mut seeds: [&[u8]; EXTRAS_MAX_SEEDS] = [&[]; EXTRAS_MAX_SEEDS];
    for ((slot, part), key) in seeds
        .iter_mut()
        .zip(entry.seeds.iter())
        .zip(keys.iter())
        .take(n)
    {
        *slot = match part.kind {
            SEED_CONST => {
                let len = usize::from(part.len);
                part.bytes.get(..len).ok_or(ShortwinError::BadExtras)?
            }
            SEED_PLACEHOLDER => key.as_ref(),
            _ => return err!(ShortwinError::BadExtras),
        };
    }
    let seeds = seeds.get(..n).ok_or(ShortwinError::BadExtras)?;
    let key = derive_with_bump(seeds, bump, &entry.key).ok_or(ShortwinError::BadExtras)?;
    let mut higher = bump;
    while let Some(b) = higher.checked_add(1) {
        require!(
            derive_with_bump(seeds, b, &entry.key).is_none(),
            ShortwinError::BadExtras
        );
        higher = b;
    }
    Ok(key)
}

/// Checks one entry of a `set_venue_extras` argument (ARCH §3.1): a fixed key or a template
/// of 1–4 seeds, never short.win itself nor the default key; constant seeds ≤ 32 bytes (zero
/// past their length), placeholders among the five known ones, unused seed slots empty.
pub fn check_entry(entry: &crate::state::ExtraEntry) -> Result<()> {
    require_keys_neq!(entry.key, crate::ID, ShortwinError::BadExtras);
    require_keys_neq!(entry.key, Pubkey::default(), ShortwinError::BadExtras);
    require!(entry.writable <= 1, ShortwinError::BadExtras);
    let n = usize::from(entry.n_seeds);
    match entry.kind {
        ENTRY_FIXED => require!(n == 0, ShortwinError::BadExtras),
        ENTRY_TEMPLATE => require!(
            (1..=EXTRAS_MAX_SEEDS).contains(&n),
            ShortwinError::BadExtras
        ),
        _ => return err!(ShortwinError::BadExtras),
    }
    for (i, part) in entry.seeds.iter().enumerate() {
        if i >= n {
            require!(
                part.kind == SEED_NONE && part.len == 0 && part.bytes == [0; 32],
                ShortwinError::BadExtras
            );
            continue;
        }
        match part.kind {
            SEED_CONST => {
                let len = usize::from(part.len);
                require!(len <= 32, ShortwinError::BadExtras);
                let tail = part.bytes.get(len..).ok_or(ShortwinError::BadExtras)?;
                require!(tail.iter().all(|b| *b == 0), ShortwinError::BadExtras);
            }
            SEED_PLACEHOLDER => {
                require!(
                    part.len <= PLACEHOLDER_COIN_CREATOR && part.bytes == [0; 32],
                    ShortwinError::BadExtras
                );
            }
            _ => return err!(ShortwinError::BadExtras),
        }
    }
    Ok(())
}

/// `true` for an empty entry slot (`ENTRY_NONE`, everything zero).
pub fn is_empty_entry(entry: &crate::state::ExtraEntry) -> bool {
    entry.kind == ENTRY_NONE
        && entry.writable == 0
        && entry.n_seeds == 0
        && entry.key == Pubkey::default()
}

// ---- venue state -------------------------------------------------------------------------

/// The live state of a market's venue (ARCH §4.1 `VenueState { X, Y, R, P, L, fee }`), read
/// from its ReadSet in the prologue (step 6).
#[derive(Clone, Debug)]
pub struct VenueState {
    /// The market's venue.
    pub kind: VenueKind,
    /// `venue_executable` (ARCH §5.3): the venue predicate holds and every venue-state
    /// account parses (A63). Only this drives `B̂_live`, closes, observation recording and the
    /// "no route ⇒ spot leg true" rule; the route-broken flag never enters it (V2-1).
    pub executable: bool,
    /// Pricing reserves `X` (token) and `Y` (SOL); 0 when the venue is not executable (the
    /// curve: `(vT, vQ)`, also while it is migrating — the completed curve is still observed).
    pub x: u64,
    pub y: u64,
    /// Real SOL `R` (tiers, pool eligibility); `Y` on the mock and Raydium, `rQ` on the curve.
    pub r: u64,
    /// The constant-product venues' fees resolved on the live state (execution quotes); `None`
    /// when not executable and on the curve (whose quotes are [`VenueState::curve`]'s).
    pub pricing: Option<Pricing>,
    /// One-way live fee φ in ppm (ARCH §4.2), 0 when not executable.
    pub fee_ppm: u32,
    /// Raydium and — since the owner's decision of 2026-09-30 (M6R-1) — PumpSwap: the pool's
    /// locked constant-product depth `D = ⌊λ_bps·isqrt(X·Y)/10⁴⌋` on the pricing reserves (ARCH
    /// §2.6; `pool_select::locked_depth`; on PumpSwap `λ_bps` from `Pool.lp_supply` and the LP
    /// mint P6, `Y` effective: `Y + VQR`); 0 on other venues and when not executable.
    pub locked_depth: u128,
    /// The pump curve (a `PumpCurve` or `PumpMigrating` ReadSet whose curve account parses):
    /// its state, its detection facts and its pricing inputs. `None` elsewhere.
    pub curve: Option<CurveVenue>,
    /// PumpSwap (a `PumpSwap` ReadSet whose pool and `GlobalConfig` parse, executable or not):
    /// `disable_flags`, the boost vault, VQR, the cashback flag and the fee (M6). `None`
    /// elsewhere.
    pub pool: Option<pump_amm::PoolVenue>,
}

/// `migrate_v2`'s token seed on a standard curve completed without a pool part: 206.9 M tokens
/// (R-PUMP §7.4; the EMA leg's deterministic `X_p`, ARCH §4.3; `shortwin_sim.PUMP_POOL_TOKENS`;
/// `shortwin_math::quote::STANDARD_POOL_TOKENS`). After a synthetic pool part the live
/// `curve_base_ata` replaces it (pump-final §5.4).
pub const STANDARD_POOL_TOKENS: u128 = 206_900_000_000_000;

/// A pump curve as a venue (ARCH §3.12 PumpCurve ReadSet, §4.1–§4.3).
#[derive(Clone, Debug)]
pub struct CurveVenue {
    /// The curve as read now (C0): `(vT, vQ, rT, rQ, complete)`.
    pub live: CurveState,
    /// The canonical PumpSwap pool (C3): absent, valid (ARCH §2.5 `canonical_pool_valid`) or
    /// there but unreadable (M5-SEC-5; `detect::canonical_pool`); read only when the curve is
    /// complete (`Absent` otherwise).
    pub canonical_pool: detect::CanonicalPool,
    /// The valid canonical pool as read (`Some` iff `canonical_pool` is `Valid`): what the
    /// `→ PumpSwap` transition writes into the market (its vaults, coin creator, the
    /// `lp_supply` anchor; `pump_amm::graduate`, M6).
    pub pool: Option<shortwin_venue_state::pump_amm::Pool>,
    /// The curve's pricing inputs; `None` when an account the quotes read no longer parses (a
    /// venue upgrade, A63) or the curve has migrated (`vT = 0`: no curve fee tier).
    pub pricing: Option<CurvePricingInputs>,
}

impl CurveVenue {
    /// Complete with `vT > 0`: the curve is final and `migrate_v2` has not run (ARCH §2.5).
    pub fn migrating(&self) -> bool {
        self.live.complete && self.live.vt > 0
    }

    /// The canonical PumpSwap pool is valid (ARCH §2.5 `canonical_pool_valid`).
    pub fn canonical_pool_valid(&self) -> bool {
        self.canonical_pool == detect::CanonicalPool::Valid
    }
}

/// What a curve's completion state adds to its pricing (pump-final §5.2–§5.4, §6.1.5): the
/// synthetic route of a live curve, the post-complete fields of a complete one. The two never
/// coexist — pump writes `pcB` / `pcQ` only in the buy that completes the curve, and a live curve
/// that carries them has no pricing at all (G-3) — so [`CurvePricingInputs`] keeps them in one
/// word ([`CurvePricingInputs::stage`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveStage {
    /// A live curve: `synthetic_fees` is the fee of pump's synthetic route past `rT` (`buy_v3`;
    /// [`CurvePricingInputs::synthetic_fees`]), `None` when there is no such route — a cashback
    /// or mayhem curve (which v3 refuses or short.win does not list), a fee that does not resolve,
    /// or a read without the mint's supply.
    Live { synthetic_fees: Option<FeeBps> },
    /// A complete curve (`PumpMigrating`, or a just-completed `PumpCurve` market):
    /// `post_complete_quote_in` is `pcQ` @158 (`migrate_v2` adds it to the pool), `post_complete`
    /// that the completing buy had a pool part (`pcB` @150 or `pcQ` non-zero), and
    /// `post_complete_known` that both were written together and pump's own invariant
    /// `vQ − rQ = ivqr` holds, so `rQ` excludes the pool part (pump-final V5). Without a pool part
    /// the fields are trivially known.
    Complete {
        post_complete_quote_in: u64,
        post_complete: bool,
        post_complete_known: bool,
    },
}

/// [`CurvePricingInputs::stage_flags`]: the curve is complete ([`CurveStage::Complete`]).
const STAGE_COMPLETE: u8 = 1;
/// Live: the synthetic route's fee is known (`stage_word` holds it).
const STAGE_SYNTHETIC: u8 = 1 << 1;
/// Complete: the completing buy had a pool part.
const STAGE_POST_COMPLETE: u8 = 1 << 2;
/// Complete: the post-complete fields are known.
const STAGE_POST_COMPLETE_KNOWN: u8 = 1 << 3;

/// What the curve's quotes read (ARCH §4.2, §4.3; pump-final §6.2): the live curve fees, the
/// constant offsets, the live leg's `X_p`, `Global.pool_migration_fee`, the PumpSwap fee schedule
/// of the post-migration pool (`B̂_str`; the tier resolved at that pool's own market cap), and the
/// completion state ([`CurveStage`]: the synthetic route of a live curve, the post-complete
/// fields of a complete one).
#[derive(Clone, Debug)]
pub struct CurvePricingInputs {
    /// `fees::pump_curve` on the live `(vT, vQ)` with the pump `FeeConfig` tiers,
    /// `Global.creator_fee_configurable`, the curve's `creator_fee_bps` and creator.
    pub fees: FeeBps,
    /// `vT − rT`, `vQ − rQ` of the live curve.
    pub offsets: CurveOffsets,
    /// The live leg's `X_p = curve_base_ata.amount − rT` (C4; ARCH §4.3): a `u64`, as the token
    /// amount it is taken from (narrowed in pump-final P1 so that the synthetic fee fits without
    /// growing `VenueState`, G-6).
    pub live_pool_tokens: u64,
    /// `Global.pool_migration_fee` (@146).
    pub migration_fee: u64,
    /// PumpSwap `FeeConfig.fee_tiers` (C5), in account order.
    pub pool_tiers: Rc<[FeeTier]>,
    /// PumpSwap `GlobalConfig.creator_fee_configurable` (C6 @940).
    pub pool_creator_fee_configurable: bool,
    /// PumpSwap `GlobalConfig.disable_flags` (C6 @56): bit 0 halts `create_pool` and with it
    /// pump's `migrate_v2` (T-DISABLE), so curve opens refuse `GraduationHalted` while it is set
    /// (M6-SEC-3, owner decision of 2026-09-30; `pump_amm::PAMM_CREATE_POOL_DISABLED_BIT`).
    pub pool_disable_flags: u8,
    /// The curve's creator is the default key: the new pool's coin creator is too, and PumpSwap
    /// charges no creator fee.
    pub coin_creator_is_default: bool,
    /// M7-SEC-5: the one-way fee (ppm) of `fees::pump_curve` on `(vT, ⌊P_ema·vT/WAD⌋)` — the
    /// curve's fee at the stored EMA price's market cap; `u32::MAX` without a price
    /// ([`VenueState::cap_fee_ppm`]).
    pub fee_ppm_at_ema: u32,
    /// [`CurvePricingInputs::stage`]'s word: on a live curve the synthetic route's fee (protocol
    /// bps in the low 32 bits, creator bps in the high 32; the curve charges no LP fee), on a
    /// complete one `pcQ`.
    stage_word: u64,
    /// [`CurvePricingInputs::stage`]'s `STAGE_*` bits.
    stage_flags: u8,
}

impl CurvePricingInputs {
    /// The completion state (pump-final §5.2–§5.4, §6.1.5; [`CurveStage`]).
    ///
    /// Stored packed in one word and a flag byte (the synthetic fee and `pcQ` never coexist):
    /// `VenueState` keeps its size (G-6, T-SIZEOF), because `execute_pool_switch` keeps venue
    /// states on its 4 KiB stack frame (an `Option<FeeBps>` field here pushed its handler 128
    /// bytes over in P1) and the pool-selection instructions box several on the 32 KiB heap (a
    /// 48-byte growth overflowed it in M7-SEC-5).
    pub fn stage(&self) -> CurveStage {
        let flag = |bit: u8| self.stage_flags & bit != 0;
        if flag(STAGE_COMPLETE) {
            CurveStage::Complete {
                post_complete_quote_in: self.stage_word,
                post_complete: flag(STAGE_POST_COMPLETE),
                post_complete_known: flag(STAGE_POST_COMPLETE_KNOWN),
            }
        } else {
            let lo = self.stage_word & u64::from(u32::MAX);
            CurveStage::Live {
                synthetic_fees: flag(STAGE_SYNTHETIC)
                    .then(|| FeeBps::new(0, lo, self.stage_word >> 32)),
            }
        }
    }

    /// Stores [`CurvePricingInputs::stage`]. A live curve's synthetic fee with a component above
    /// `u32::MAX` bps, or a non-zero LP component (which the curve never charges), is stored as
    /// unknown: no synthetic route then.
    fn set_stage(&mut self, stage: CurveStage) {
        let (word, flags) = match stage {
            CurveStage::Live { synthetic_fees } => {
                let packed = synthetic_fees.filter(|f| f.lp == 0).and_then(|f| {
                    let protocol = u64::from(u32::try_from(f.protocol).ok()?);
                    let creator = u64::from(u32::try_from(f.creator).ok()?);
                    Some(protocol | (creator << 32))
                });
                match packed {
                    Some(w) => (w, STAGE_SYNTHETIC),
                    None => (0, 0),
                }
            }
            CurveStage::Complete {
                post_complete_quote_in,
                post_complete,
                post_complete_known,
            } => {
                let mut flags = STAGE_COMPLETE;
                if post_complete {
                    flags |= STAGE_POST_COMPLETE;
                }
                if post_complete_known {
                    flags |= STAGE_POST_COMPLETE_KNOWN;
                }
                (post_complete_quote_in, flags)
            }
        };
        self.stage_word = word;
        self.stage_flags = flags;
    }

    /// The fee pump charges on both parts of a buy past `rT` by synthetic migration (pump-final
    /// §5.2, P1 correction (a)): `fees::pump_curve_at_supply` on the live `(vT, vQ)` with the
    /// mint's **live** supply — pump's tier rule (R-PUMP Update U10), where [`fees`](Self::fees)
    /// keeps the fixed 10¹⁵ of ARCH §4.2 for every existing path. `Some` only on a live curve
    /// with a synthetic route ([`CurveStage::Live`]): `None` on a complete curve, a cashback or
    /// mayhem curve, without the mint's supply (`read_state` without one) and for a component
    /// above `u32::MAX` bps. It is `CurvePricing::synthetic`: `q > rT` prices `B̂_v3` at it.
    pub fn synthetic_fees(&self) -> Option<FeeBps> {
        match self.stage() {
            CurveStage::Live { synthetic_fees } => synthetic_fees,
            CurveStage::Complete { .. } => None,
        }
    }

    /// The post-complete fields are known (pump-final §6.1.5): always on a live curve (it carries
    /// none, G-3) and on a complete curve without a pool part; on one with a pool part, `pcB` and
    /// `pcQ` were written together and `vQ − rQ = ivqr` holds. When false, FV is undefined
    /// (`Quoter::fv_repriced`).
    pub fn post_complete_known(&self) -> bool {
        match self.stage() {
            CurveStage::Live { .. } => true,
            CurveStage::Complete {
                post_complete_known,
                ..
            } => post_complete_known,
        }
    }

    /// `CurvePricing` at the live fee (pump-final §6.2), with the post-migration pool's schedule
    /// (`base_supply` 10¹⁵, `fees::PammFeeSchedule`: a coin that has not graduated yet;
    /// `shortwin_sim.CPPool.graduate`; a new pool's own `creator_fee_bps` 0) and the migration of
    /// the EMA leg, the box, `B̂_ref`, D4 and FV:
    ///
    /// - `pool_tokens` the standard 206.9e12 (ARCH §4.3), or, on a complete curve whose
    ///   completing buy had a pool part, the live `curve_base_ata` (`live_pool_tokens`, `rT = 0`):
    ///   the tokens pump priced the pool part on and `migrate_v2` seeds, donations included
    ///   (pump-final §5.4). A live curve carries no pool part (G-3), so its EMA leg keeps 206.9e12;
    /// - `post_complete_quote_in` `pcQ` (0 without a pool part);
    /// - `synthetic` the live curve's synthetic fee ([`synthetic_fees`](Self::synthetic_fees)):
    ///   above `rT` a live curve prices `B̂_v3`, while a `PumpMigrating` market's EMA-leg ring
    ///   states (`curve_at`, `complete = false`) keep `B̂_str` on the real pool at the PumpSwap
    ///   tier (G-2).
    pub fn curve_pricing(&self) -> CurvePricing<'_> {
        let (pool_tokens, post_complete_quote_in) = match self.stage() {
            CurveStage::Complete {
                post_complete: true,
                post_complete_quote_in,
                ..
            } => (u128::from(self.live_pool_tokens), post_complete_quote_in),
            _ => (STANDARD_POOL_TOKENS, 0),
        };
        CurvePricing {
            fees: self.fees,
            offsets: self.offsets,
            migration: MigrationParams {
                migration_fee: self.migration_fee,
                pool_tokens,
                post_complete_quote_in,
            },
            pool: PammFeeSchedule {
                tiers: &self.pool_tiers,
                base_supply: u64::try_from(math_fees::PUMP_CURVE_MCAP_SUPPLY).unwrap_or(u64::MAX),
                creator_fee_configurable: self.pool_creator_fee_configurable,
                pool_creator_fee_bps: 0,
                coin_creator_is_default: self.coin_creator_is_default,
                cap_bps: None,
            },
            synthetic: self.synthetic_fees(),
        }
    }
}

/// A curve state `(vT, vQ) = (x, y)` with the live offsets: a ring entry's cached reprojection is
/// the curve at that price (ARCH §4.4: `curve_reprice` keeps the offsets; the second half of
/// `quote::curve_reprice`).
pub fn curve_at(x: u128, y: u128, offsets: CurveOffsets) -> CurveState {
    CurveState {
        vt: x,
        vq: y,
        rt: x.saturating_sub(offsets.token),
        rq: y.saturating_sub(offsets.quote),
        complete: false,
    }
}

/// The curve's quotes at one fee view (the live fee, or capped for risk quotes).
#[derive(Clone, Copy, Debug)]
pub struct CurveQuoter<'a> {
    /// The fees, offsets, the EMA-leg migration (`CurvePricingInputs::curve_pricing`), the pool
    /// schedule and the live curve's synthetic route.
    pub pricing: CurvePricing<'a>,
    /// The live curve.
    pub live: CurveState,
    /// The live leg's `X_s` (`curve_base_ata.amount − rT`, donations included as pump prices it).
    pub live_pool_tokens: u128,
    /// A `PumpMigrating` market: FV prices the post-migration pool `migrate_v2` will create
    /// (ARCH §4.5; ρ-scaled after a synthetic pool part, pump-final §5.4).
    pub migrating: bool,
    /// The curve's post-complete fields are known (`CurvePricingInputs::post_complete_known`):
    /// when false on a migrating curve FV is undefined (pump-final §6.1.5).
    pub post_complete_known: bool,
}

/// A venue's quotes at one fee view (ARCH §4.3 "two fee views"): the live fee for execution
/// quotes, [`Quoter::capped`] for risk quotes (V2-1). Every quote is `None` (`+∞`) when undefined.
// The curve variant carries its whole `CurvePricing` by value (since pump-final §5.3 also the
// synthetic exit's fee, 32 bytes more): a `Quoter` is built per quote from the `VenueState` and
// copied freely; boxing the variant would cost a heap allocation per quote and the `Copy`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug)]
pub enum Quoter<'a> {
    /// A constant-product pool on `(X, Y)`.
    Pool(Pricing),
    /// The pump curve (above `rT`: a live curve's synthetic exit `B̂_v3`, a migrating market's
    /// post-migration pool `B̂_str`).
    Curve(CurveQuoter<'a>),
}

impl Quoter<'_> {
    /// The risk view `min(φ_live, cap_bps)` (ARCH §4.3, V2-1).
    pub fn capped(self, cap_bps: u64) -> Self {
        match self {
            Self::Pool(p) => Self::Pool(p.capped(cap_bps)),
            Self::Curve(mut c) => {
                c.pricing = c.pricing.capped(cap_bps);
                Self::Curve(c)
            }
        }
    }

    /// One-way fee φ of a buy in ppm.
    pub fn oneway_ppm(&self) -> u32 {
        match self {
            Self::Pool(p) => p.oneway_ppm(),
            Self::Curve(c) => c.pricing.oneway_ppm(),
        }
    }

    /// `B̂(q)` on the **live** state: a pool on `(x, y)`; the curve on its live state with the live
    /// leg's `X_s = curve_base_ata.amount − rT` (ARCH §4.3; pump-final §5.2: `B̂_v3` above `rT`, the
    /// pool part priced on the donation-inclusive balance as pump prices it; `None` on a complete
    /// curve). A live curve buys past `rT` only by pump's synthetic migration: without that route
    /// (`CurvePricing::synthetic` unset — no fee resolved on the mint's supply) nothing executes
    /// there, so the quote is `None` (`+∞`) rather than `B̂_str`, which no instruction executes.
    pub fn buy_live(&self, q: u64, x: u128, y: u128) -> Option<BuyQuote> {
        match self {
            Self::Pool(p) => {
                if x == 0 {
                    return None;
                }
                p.buy(q, x, y)
            }
            Self::Curve(c) => {
                if !c.live.complete && c.pricing.synthetic.is_none() && u128::from(q) > c.live.rt {
                    return None;
                }
                let mut pricing = c.pricing;
                pricing.migration.pool_tokens = c.live_pool_tokens;
                pricing.buy(q, &c.live)
            }
        }
    }

    /// `B̂(q; x, y)` on a hypothetical state — a ring entry's cached reprojection (the EMA leg, the
    /// D4 margin): a pool on `(x, y)`; the curve at `(vT, vQ) = (x, y)` with the live offsets and
    /// the EMA leg's migration (above that state's `rT`: `B̂_v3` on a live curve with the
    /// standard 206.9e12, `B̂_str` on a migrating market's pool). `None` for `x = 0`.
    pub fn buy_xy(&self, q: u64, x: u128, y: u128) -> Option<BuyQuote> {
        if x == 0 {
            return None;
        }
        match self {
            Self::Pool(p) => p.buy(q, x, y),
            Self::Curve(c) => c.pricing.buy(q, &curve_at(x, y, c.pricing.offsets)),
        }
    }

    /// `B̂(q; reprice(L, P))` (the curve: `curve_reprice`, ARCH §4.3 EMA leg): the spot leg, the
    /// box corners, `B̂_ref`.
    pub fn buy_repriced(&self, q: u64, l: u128, p: u128) -> Option<BuyQuote> {
        match self {
            Self::Pool(pr) => pr.buy_repriced(q, l, p),
            Self::Curve(c) => c.pricing.buy_repriced(q, l, p),
        }
    }

    /// `B̂_FV(P, L)` (ARCH §4.5): [`Quoter::buy_repriced`], except on a stuck migration (a
    /// `PumpMigrating` market), where it prices the pool `migrate_v2` will create at `P`
    /// (pump-final §5.4, `math_quote::migrating_fv`): `(X₁, Y₁)` = `migrated_pool` with the
    /// standard 206.9e12 tokens, or the live `curve_base_ata` and `+ pcQ` after a synthetic pool
    /// part (`CurvePricingInputs::curve_pricing`), repriced to `reprice(√(X₁·Y₁), p')` at the
    /// pool's own PumpSwap tier (`shortwin_sim.Market.fv_parts`), with `p' = p` without a pool
    /// part (bit-identical to the FV before pump's v3) and `p' = ⌈⌈p·Y₁/Y₀⌉·X₀/X₁⌉` (ρ, lender
    /// side) after one.
    ///
    /// `None` (FV undefined) on a migrating curve whose post-complete fields are not known
    /// (pump-final §6.1.5): `settle::amount_at` then gives the lenders the whole take, the
    /// documented exception to A63's cap-only rule ([`risk_quoter`]).
    pub fn fv_repriced(&self, q: u64, l: u128, p: u128) -> Option<BuyQuote> {
        match self {
            Self::Curve(c) if c.migrating => {
                if !c.post_complete_known {
                    return None;
                }
                math_quote::migrating_fv(q, p, &c.live, &c.pricing.migration, &c.pricing.pool)
            }
            _ => self.buy_repriced(q, l, p),
        }
    }
}

impl VenueState {
    /// A venue that cannot execute (no accounts, a reader failure, a halt): no price, no
    /// quote.
    pub const fn not_executable(kind: VenueKind) -> Self {
        Self {
            kind,
            executable: false,
            x: 0,
            y: 0,
            r: 0,
            pricing: None,
            fee_ppm: 0,
            locked_depth: 0,
            curve: None,
            pool: None,
        }
    }

    /// The one-way fee φ the fee-scaled caps take (ARCH §4.6; M7-SEC-5): `min(φ_live, φ at
    /// P_ema)` on the pump venues, `φ_live` elsewhere. pump-fees picks the tier from the live
    /// market cap and charges more as it falls, so a sale in the open's own transaction (down to
    /// the spot–EMA guard's −10 %) moved the pool into a higher-fee tier and raised
    /// `3.0·φ·X₀` / `7.5·φ·X₁` on an `X₀` the EMA terms hold (the reproducer: 30 → 35 bps, both
    /// caps +16.7 %); at the EMA's market cap a same-transaction trade can only lower the fee
    /// the caps use.
    ///
    /// The fee at `P_ema` rides in the pump views' padding (`pump_amm::PoolVenue`,
    /// [`CurvePricingInputs`]), so `VenueState` does not grow: the pool-selection instructions
    /// box several of them on the 32 KiB heap. It is resolved only by `open_short`'s pre-trade
    /// read ([`read_state_boxed_for_open`]) and, on PumpSwap, by the keeper exits' and
    /// `sol_settle`'s ([`read_state_boxed_for_exit`], AIR-10); elsewhere it reads `u32::MAX` and
    /// this is `φ_live`.
    pub fn cap_fee_ppm(&self) -> u32 {
        let at_ema = match self.kind {
            VenueKind::PumpSwap => self.pool.as_ref().map_or(u32::MAX, |p| p.fee_ppm_at_ema),
            VenueKind::PumpCurve | VenueKind::PumpMigrating => self
                .curve
                .as_ref()
                .and_then(|c| c.pricing.as_ref())
                .map_or(u32::MAX, |p| p.fee_ppm_at_ema),
            _ => u32::MAX,
        };
        self.fee_ppm.min(at_ema)
    }

    /// The fee cap of the exit risk view — `liquidate::assess`, the keeper exits' `B̂_ref`
    /// (`engine::bounds`) and `sol_settle`'s quotes (AIR-10): the position's locked
    /// `hard_cap_bps` and, on PumpSwap once [`read_state_boxed_for_exit`] resolved it,
    /// `⌈φ at P_ema / 100⌉` — M7-SEC-5's rule for the opens' caps. pump-fees picks the tier from
    /// the live market cap and charges more as it falls, so a sale earlier in the exit's own
    /// transaction moved the pool into a higher-fee tier and raised every risk quote at once (the
    /// spot leg, the EMA leg on the ring, `B̂_ref`, the penalty base, the settlement box and
    /// FV): a position the honest state holds healthy was liquidated. At the EMA's market cap a
    /// same-transaction trade can only lower the fee the risk view takes; the execution quote
    /// (`B̂_live`, the buyback itself) keeps the live fee. φ at `P_ema` is resolved on the live
    /// mint supply, so a holder's burn earlier in the transaction still raises it (KI-72). Else
    /// the hard cap alone: Raydium's fees are fixed, the curve's on the fixed 10¹⁵ supply but
    /// its synthetic fee past `rT`, on the live one: a burn can raise that too (KI-77).
    pub fn exit_risk_cap_bps(&self, hard_cap_bps: u64) -> u64 {
        match (self.kind, self.pool.as_ref()) {
            (VenueKind::PumpSwap, Some(p)) if p.fee_ppm_at_ema != u32::MAX => {
                hard_cap_bps.min(u64::from(p.fee_ppm_at_ema).div_ceil(100))
            }
            _ => hard_cap_bps,
        }
    }

    /// Observed by the prologue's record (ARCH §3.0 step 7, A64): an executable venue, or a
    /// migrating curve (its completed state is final, so it is still recorded).
    pub fn observed(&self) -> bool {
        self.executable || self.curve.as_ref().is_some_and(CurveVenue::migrating)
    }

    /// Spot `P = ⌊Y·WAD/X⌋` of an observed venue with both reserves non-zero.
    pub fn price(&self) -> Option<u128> {
        if !self.observed() || self.x == 0 || self.y == 0 {
            return None;
        }
        price_wad(u128::from(self.x), u128::from(self.y))
            .ok()
            .filter(|p| *p > 0)
    }

    /// `L = √(X·Y)` of an observed venue (the curve's `isqrt(vT·vQ)`).
    pub fn liquidity(&self) -> Option<u128> {
        if !self.observed() {
            return None;
        }
        shortwin_math::quote::liquidity(u128::from(self.x), u128::from(self.y))
            .ok()
            .filter(|l| *l > 0)
    }

    /// The venue's quotes at the **live** fee: a pool's resolved pricing, or the curve's inputs
    /// (also while it is migrating). `None` when neither is available.
    pub fn quoter(&self) -> Option<Quoter<'_>> {
        if let Some(p) = self.pricing {
            return Some(Quoter::Pool(p));
        }
        let c = self.curve.as_ref()?;
        let inputs = c.pricing.as_ref()?;
        Some(Quoter::Curve(CurveQuoter {
            pricing: inputs.curve_pricing(),
            live: c.live,
            live_pool_tokens: u128::from(inputs.live_pool_tokens),
            migrating: c.migrating(),
            post_complete_known: inputs.post_complete_known(),
        }))
    }

    /// Execution quote `B̂_live(q)` at the live fee (ARCH §4.3 "two fee views"); `None` (`+∞`)
    /// when undefined or the venue is not executable. On the curve `B̂_v3` above `rT` (pump's
    /// synthetic migration, pump-final §5.2).
    pub fn buy(&self, q: u64) -> Option<BuyQuote> {
        if !self.executable {
            return None;
        }
        self.quoter()?
            .buy_live(q, u128::from(self.x), u128::from(self.y))
    }

    /// Risk quote at `min(φ_live, cap_bps)` (V2-1).
    pub fn buy_risk(&self, q: u64, cap_bps: u64) -> Option<BuyQuote> {
        if !self.executable {
            return None;
        }
        self.quoter()?
            .capped(cap_bps)
            .buy_live(q, u128::from(self.x), u128::from(self.y))
    }

    /// Exact-in sale of `q` tokens at the live fee (`S_exp`, ARCH §3.4 step 7).
    pub fn sell(&self, q: u64) -> Option<SellQuote> {
        if !self.executable {
            return None;
        }
        match self.quoter()? {
            Quoter::Pool(p) => p.sell(q, u128::from(self.x), u128::from(self.y), self.r),
            Quoter::Curve(c) => c.pricing.sell(q, &c.live),
        }
    }

    /// A quote at the live fee on a hypothetical state `(x, y)` (a ring entry's cached
    /// reprojection: the D4 margin's EMA leg, ARCH §3.4 step 9).
    pub fn buy_at(&self, q: u64, x: u128, y: u128) -> Option<BuyQuote> {
        self.quoter()?.buy_xy(q, x, y)
    }
}

// ---- per-venue dispatch ------------------------------------------------------------------

/// Length of the venue's ReadSet (ARCH §3.12). `NoSupportedVenue` for a venue without an
/// adapter in this build. A `PumpMigrating` market reads its curve's ReadSet; a list-only market
/// its detection set `[bonding_curve, canonical_pool]` (`crank_price`, `sol_settle`).
pub fn read_len(kind: VenueKind) -> Result<usize> {
    match kind {
        VenueKind::None => Ok(DETECTION_LEN),
        VenueKind::PumpCurve | VenueKind::PumpMigrating => Ok(pump_curve::READ_LEN),
        VenueKind::RaydiumV4 => Ok(raydium_v4::READ_LEN),
        VenueKind::RaydiumCpmm => Ok(raydium_cpmm::READ_LEN),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Ok(mock::READ_LEN),
        VenueKind::PumpSwap => Ok(pump_amm::READ_LEN),
    }
}

/// Length of a list-only market's "ReadSet": its detection set.
pub const DETECTION_LEN: usize = 2;

/// Length of the venue's TradeExtras (ARCH §3.12).
pub fn trade_len(kind: VenueKind) -> Result<usize> {
    match kind {
        VenueKind::PumpCurve => Ok(pump_curve::TRADE_LEN),
        VenueKind::PumpSwap => Ok(pump_amm::TRADE_LEN),
        VenueKind::RaydiumV4 => Ok(raydium_v4::TRADE_LEN),
        VenueKind::RaydiumCpmm => Ok(raydium_cpmm::TRADE_LEN),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Ok(mock::TRADE_LEN),
        _ => err!(ShortwinError::NoSupportedVenue),
    }
}

/// Key-checks the ReadSet against the market's cache (`VenueAccountMismatch`).
pub fn check_read(market: &Market, kind: VenueKind, read: &[AccountInfo]) -> Result<()> {
    match kind {
        VenueKind::None => {
            let [bc, cp] = read else {
                return err!(ShortwinError::BadRemainingAccounts);
            };
            require_keys_eq!(
                bc.key(),
                market.bonding_curve,
                ShortwinError::VenueAccountMismatch
            );
            require_keys_eq!(
                cp.key(),
                market.canonical_pool,
                ShortwinError::VenueAccountMismatch
            );
            Ok(())
        }
        VenueKind::PumpCurve | VenueKind::PumpMigrating => pump_curve::check_read(market, read),
        VenueKind::RaydiumV4 => raydium_v4::check_read(market, read),
        VenueKind::RaydiumCpmm => raydium_cpmm::check_read(market, read),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => mock::check_read(market, read),
        VenueKind::PumpSwap => pump_amm::check_read(market, read),
    }
}

/// Key-checks the TradeExtras (`VenueAccountMismatch`). The curve's recipients and creator
/// vault are checked against the live `Global` and curve of the (already key-checked) ReadSet.
pub fn check_trade(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    trade: &[AccountInfo],
) -> Result<()> {
    match kind {
        VenueKind::PumpCurve => pump_curve::check_trade(market, read, trade),
        VenueKind::PumpSwap => pump_amm::check_trade(market, read, trade),
        VenueKind::RaydiumV4 => raydium_v4::check_trade(market, trade),
        VenueKind::RaydiumCpmm => raydium_cpmm::check_trade(market, trade),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => mock::check_trade(market, trade),
        _ => {
            let _ = (market, read, trade);
            err!(ShortwinError::NoSupportedVenue)
        }
    }
}

/// Reads the venue state from `market`'s key-checked ReadSet at clock time `now` (the
/// cluster's `unix_timestamp`: the venues' own time gates — AMM v4 `pool_open_time` while
/// `status == 7`, CPMM `open_time` — compare against it), without the mint's supply: a PumpSwap
/// market then reads as not executable (its fee tier needs the supply; [`read_state_with`]).
/// Never fails: a reader failure is "not executable" (A63).
pub fn read_state(market: &Market, kind: VenueKind, read: &[AccountInfo], now: i64) -> VenueState {
    read_state_with(market, kind, read, now, None)
}

/// [`read_state`] with the mint's live `supply` (`mint.supply` @36 of the named mint): PumpSwap's
/// fee tier is chosen on the market cap `effective_quote·supply/base` (ARCH §4.2; `None` makes a
/// PumpSwap market not executable). On the curve it resolves only the synthetic exit's fee
/// (`CurvePricingInputs::synthetic_fees`, pump's tier on the live supply; pump-final §5.2): every
/// other curve fee keeps the fixed 10¹⁵ (ARCH §4.2 as built). Raydium ignores it.
pub fn read_state_with(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    now: i64,
    supply: Option<u64>,
) -> VenueState {
    read_state_ema(market, kind, read, now, supply, false)
}

/// [`read_state_with`]; with `ema_fee` the pump venues also resolve their fee at the stored
/// `P_ema`'s market cap ([`VenueState::cap_fee_ppm`], M7-SEC-5).
fn read_state_ema(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    now: i64,
    supply: Option<u64>,
    ema_fee: bool,
) -> VenueState {
    match kind {
        VenueKind::PumpCurve | VenueKind::PumpMigrating => {
            pump_curve::read_state(market, kind, read, supply, ema_fee)
        }
        VenueKind::PumpSwap => pump_amm::read_state(market, read, supply, ema_fee),
        VenueKind::RaydiumV4 => raydium_v4::read_state(market, read, now),
        VenueKind::RaydiumCpmm => raydium_cpmm::read_state(market, read, now),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => mock::read_state(read),
        VenueKind::None => {
            let _ = (market, read, now);
            VenueState::not_executable(kind)
        }
    }
}

/// [`read_state_with`] on the heap: the handlers keep only a pointer on their stack frame (SBF
/// frames are 4 KiB; the curve's state makes `VenueState` a few hundred bytes).
#[inline(never)]
pub fn read_state_boxed(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    now: i64,
    supply: Option<u64>,
) -> Box<VenueState> {
    Box::new(read_state_with(market, kind, read, now, supply))
}

/// [`read_state_boxed`] for `open_short`'s pre-trade read: the pump venues also resolve their fee at
/// the stored `P_ema`'s market cap, which the fee-scaled caps take (`VenueState::cap_fee_ppm`,
/// M7-SEC-5; ≈ 1.3–1.6 k CU, paid by opens only).
#[inline(never)]
pub fn read_state_boxed_for_open(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    now: i64,
    supply: Option<u64>,
) -> Box<VenueState> {
    Box::new(read_state_ema(market, kind, read, now, supply, true))
}

/// [`read_state_boxed`] for the keeper exits (`liquidate`, `expire`) and `sol_settle` (AIR-10):
/// on PumpSwap the fee is also resolved at the stored `P_ema`'s market cap, which caps their
/// risk view (`VenueState::exit_risk_cap_bps`; ≈ 1.3–1.6 k CU); the other venues read as
/// [`read_state_boxed`].
#[inline(never)]
pub fn read_state_boxed_for_exit(
    market: &Market,
    kind: VenueKind,
    read: &[AccountInfo],
    now: i64,
    supply: Option<u64>,
) -> Box<VenueState> {
    let ema_fee = kind == VenueKind::PumpSwap;
    Box::new(read_state_ema(market, kind, read, now, supply, ema_fee))
}

/// The supply of a mint account (`supply` @36; SPL Token or Token-2022), `None` if it does not
/// parse. The caller has checked the mint's key and program (the mint guard).
pub fn mint_supply(mint: &AccountInfo) -> Option<u64> {
    let data = mint.try_borrow_data().ok()?;
    shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &data)
        .ok()
        .map(|m| m.supply)
}

/// The quotes of the **risk** view on hypothetical states — the EMA leg on the ring's cached
/// `(x, y)`, the settlement-box corners, the refutation, FV (ARCH §4.3 "two fee views", §4.5) —
/// which `sol_settle` evaluates whether or not the venue can execute: `min(φ_live, cap_bps)`,
/// `cap_bps` the position's locked hard cap (V2-1, A43) — on PumpSwap also φ at `P_ema`
/// ([`VenueState::exit_risk_cap_bps`], AIR-10).
///
/// - While the venue executes (or, on the curve, while its pricing accounts parse: a migrating
///   curve too): its live quotes, capped.
/// - A halted venue whose fee-bearing accounts still parse (a disabled pool) keeps the fee it
///   carries: the venue's own adapter reads it (the mock's `fee_view`).
/// - A venue whose fee-bearing accounts no longer parse (a layout change, A63) has no readable
///   `φ_live`, so the cap itself is used (`min(unknown, cap) = cap`, FV's ceiling): an S3 on
///   such a venue still has a fair value (ARCH §5.3 "FV is priced at the last pre-halt
///   `P_ema`") instead of an uncapped take. **PROPOSED (M5):** the same cap-only
///   constant-product view prices a pump market whose pricing accounts do not parse, whose
///   curve has migrated (`vT = 0`, no curve fee tier) or whose **live** curve carries
///   post-complete fields (pump-final G-3: pump writes them only in the buy that completes the
///   curve, so they are unknown there), and a list-only market (`None`: a curve that graduated
///   elsewhere with positions open, M2-SEC-5): `B̂ = n + ⌈n·cap/10⁴⌉` on `reprice(L, P)`, the
///   curve's own reserves without its real-reserve bound (ARCH names no FV for these).
/// - **The documented exception (pump-final §6.1.5).** A **migrating** curve whose post-complete
///   fields are not known (one of `pcB` / `pcQ` zero, or `vQ − rQ ≠ ivqr`) keeps its curve
///   quoter, so this cap-only view is never reached for it, and its FV is undefined
///   ([`Quoter::fv_repriced`] `None`): an S3 there gives the lenders the whole take. The cap-only
///   view is a fair-value ceiling only when the venue's reserves are known and just its fee is
///   not; here the reserves of the pool that will open are what is unknown, and the curve's
///   virtual liquidity at ρ = 1 would settle a real pool part far below its value (≈ 85 % on an
///   E1bY-like completion). The EMA leg and the box keep the fields as read (`Y_s` includes
///   `pcQ`, the larger reading of `rQ`), so liquidatability errs toward the lenders too.
///
/// - PumpSwap (M6): a halted pool (the `disable_flags` buy bit) keeps its resolved fee, capped;
///   a pool whose fee no longer resolves takes the cap (`pump_amm::fee_view`).
///
/// Always `Some` in this build (`Option` for a venue without an adapter).
pub fn risk_quoter<'a>(
    market: &Market,
    state: &'a VenueState,
    read: &[AccountInfo],
    cap_bps: u64,
) -> Option<Quoter<'a>> {
    if let Some(q) = state.quoter() {
        return Some(q.capped(cap_bps));
    }
    match state.kind {
        VenueKind::RaydiumV4 => Some(Quoter::Pool(raydium_v4::fee_view(read, cap_bps))),
        VenueKind::RaydiumCpmm => Some(Quoter::Pool(raydium_cpmm::fee_view(market, read, cap_bps))),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Some(Quoter::Pool(mock::fee_view(read, cap_bps))),
        VenueKind::None | VenueKind::PumpCurve | VenueKind::PumpMigrating => {
            Some(cap_only(cap_bps))
        }
        VenueKind::PumpSwap => Some(Quoter::Pool(pump_amm::fee_view(state, cap_bps))),
    }
}

/// The cap-only constant-product risk view (`risk_quoter`'s fallback): one combined fee at
/// `cap_bps` on the SOL leg, no `ε₁`.
pub fn cap_only(cap_bps: u64) -> Quoter<'static> {
    Quoter::Pool(Pricing::PumpSwap(FeeBps::new(0, cap_bps, 0)))
}

/// The market's pool for the `pool` placeholder of extras templates.
pub fn pool_key(market: &Market, kind: VenueKind) -> Option<Pubkey> {
    match kind {
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Some(mock::pool(market)),
        VenueKind::PumpSwap => Some(market.canonical_pool),
        VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => market
            .pool_cache
            .get(crate::state::raydium_cache::RAY_POOL)
            .copied(),
        _ => None,
    }
}

/// The adopted pool's own accounts that no extra may name (ARCH §2.4 rule 4; M10 batch 3, the
/// owner's decision of 2026-10-03): the pool and the two accounts that hold its reserves — the
/// mock pool and its vaults; the AMM v4 pool and its coin / pc vaults; the CPMM pool and its
/// token 0 / 1 vaults; the canonical PumpSwap pool and its base / quote vaults; the bonding curve
/// (its SOL reserve), `curve_base_ata` and `curve_quote_ata`. The venue names them in its own
/// account list already (key-checked, the ReadSet and TradeExtras); an Emergency extras set that
/// named one again could only hand the venue a second, writable copy of its own reserves. Keys
/// from the market's cache, which the ReadSet is checked against: no derivation.
pub fn pool_denylist(market: &Market, kind: VenueKind) -> Result<[Pubkey; 3]> {
    use crate::state::{pump_swap_cache as ps, raydium_cache as rc};
    match kind {
        VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => Ok([
            raydium::cached(market, rc::RAY_POOL),
            raydium::cached(market, rc::RAY_VAULT_SOL),
            raydium::cached(market, rc::RAY_VAULT_TOK),
        ]),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Ok([
            raydium::cached(market, rc::RAY_POOL),
            raydium::cached(market, rc::RAY_VAULT_SOL),
            raydium::cached(market, rc::RAY_VAULT_TOK),
        ]),
        VenueKind::PumpSwap => Ok([
            market.canonical_pool,
            pump_amm::cached(market, ps::POOL_BASE_VAULT),
            pump_amm::cached(market, ps::POOL_QUOTE_VAULT),
        ]),
        VenueKind::PumpCurve => Ok([
            market.bonding_curve,
            market.curve_base_ata,
            market.curve_quote_ata,
        ]),
        _ => err!(ShortwinError::NoSupportedVenue),
    }
}

/// The remaining accounts of a trading or cranking instruction, split and checked.
pub struct Remaining<'info> {
    /// The ReadSet (key-checked).
    pub read: &'info [AccountInfo<'info>],
    /// The TradeExtras (key-checked; empty when the instruction carries none).
    pub trade: &'info [AccountInfo<'info>],
    /// The resolved extras metas and their infos.
    pub extra_metas: Vec<AccountMeta>,
    pub extra_infos: &'info [AccountInfo<'info>],
}

/// What an instruction carries after its ReadSet.
#[derive(Clone, Copy, Debug)]
pub enum Carry {
    /// `ReadSet` only (`crank_price`, `sol_settle`).
    ReadSet,
    /// `ReadSet ++ TradeExtras ++ Extras(ix)` (trading instructions), with the caller's
    /// selection.
    Trade { ix: usize, sel: ExtrasSel },
}

/// Splits and checks `remaining` (module docs): the ReadSet and, for a trading instruction,
/// the TradeExtras and the extras segment. `deny` is the instruction's denylist (ARCH §2.4
/// rule 4) for the extras.
pub fn parse<'info>(
    market: &Market,
    kind: VenueKind,
    remaining: &'info [AccountInfo<'info>],
    carry: Carry,
    deny: &[Pubkey],
) -> Result<Remaining<'info>> {
    let rl = read_len(kind)?;
    require!(remaining.len() >= rl, ShortwinError::BadRemainingAccounts);
    let (read, rest) = remaining.split_at(rl);
    check_read(market, kind, read)?;
    match carry {
        Carry::ReadSet => {
            require!(rest.is_empty(), ShortwinError::BadRemainingAccounts);
            Ok(Remaining {
                read,
                trade: &[],
                extra_metas: Vec::new(),
                extra_infos: &[],
            })
        }
        Carry::Trade { ix, sel } => {
            let tl = trade_len(kind)?;
            require!(rest.len() >= tl, ShortwinError::BadRemainingAccounts);
            let (trade, extras) = rest.split_at(tl);
            check_trade(market, kind, read, trade)?;
            let ph = Placeholders {
                mint: market.mint,
                pool: pool_key(market, kind),
                trader: market.trader,
                bonding_curve: market.bonding_curve,
                // PumpSwap: the pool's coin creator as cached (a creator change is re-derived by
                // the adapter's own key check; a template naming it follows the cache).
                coin_creator: (kind == VenueKind::PumpSwap).then(|| {
                    pump_amm::cached(market, crate::state::pump_swap_cache::CACHED_COIN_CREATOR)
                }),
            };
            let pool_deny = pool_denylist(market, kind)?;
            let (extra_metas, extra_infos) =
                parse_extras(kind, ix, &sel, extras, &ph, deny, &pool_deny)?;
            Ok(Remaining {
                read,
                trade,
                extra_metas,
                extra_infos,
            })
        }
    }
}

// ---- legs --------------------------------------------------------------------------------

/// Everything a venue leg needs (ARCH §2.4.1): the market (it signs the pool vault's
/// `transfer_checked` / `approve_checked`), the pool vault, the base mint and its token
/// program, the trader accounts and the parsed remaining accounts.
pub struct Leg<'info> {
    pub kind: VenueKind,
    pub market: AccountInfo<'info>,
    pub market_bump: u8,
    pub pool_vault: AccountInfo<'info>,
    pub mint: AccountInfo<'info>,
    pub decimals: u8,
    pub token_program: AccountInfo<'info>,
    /// Raydium: WSOL is the AMM v4 coin / the CPMM token 0 (`Market.flags` bit 3; the CPMM
    /// swap names its input and output vaults by side). Unused by the mock and the curve.
    pub sol_first: bool,
    /// The leg's venue-side rent tolerance `ert` (ARCH §2.4 rule 3): `exec_rent_tol` on an open
    /// of a pump venue, the position's locked value on an exit; 0 on Raydium and the mock.
    pub ert: u64,
    pub trader: TraderAccounts<'info>,
    pub read: &'info [AccountInfo<'info>],
    pub trade: &'info [AccountInfo<'info>],
    pub extra_metas: Vec<AccountMeta>,
    pub extra_infos: &'info [AccountInfo<'info>],
}

/// What a sell leg measured and what the caller still owes the borrower (ARCH §2.4 rule 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellOut {
    /// `S`: the proceeds now on the position, net of venue-side rent.
    pub s: u64,
    /// Lamports the caller moves from the position to the borrower after its last CPI naming
    /// either: the ephemeral WSOL account's rent on the WSOL venues, plus on PumpSwap the
    /// venue-side rent `r` the leg netted out of `S` (the borrower funded it inside `ert`).
    pub refund: u64,
    /// The venue-side rent `r` the venue took from the trader: `Open.venue_rent` (PumpSwap; 0
    /// elsewhere — on the curve it cannot be separated from the proceeds).
    pub venue_rent: u64,
}

/// What a buy leg measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuyOut {
    /// `B`: lamports the position spent (incl. venue-side rent on pump venues).
    pub spent: u64,
    /// Tokens the pool vault received (`≥ q`; the surplus is a donation).
    pub received: u64,
}

/// The accounts of a leg that the instruction names besides `trader` and `market_vault`: System,
/// and the token side — the pool vault, the base mint (with its decimals) and its token program.
pub struct Named<'info> {
    pub system_program: AccountInfo<'info>,
    pub pool_vault: AccountInfo<'info>,
    pub mint: AccountInfo<'info>,
    pub decimals: u8,
    pub token_program: AccountInfo<'info>,
}

/// The trader-side accounts of a trading instruction on `kind`: the named `trader` and
/// `market_vault`; on the WSOL venues the WSOL mint, SPL Token and `trader_wsol` from the venue's
/// TradeExtras; on the curve `trader_base_ata` (TradeExtras T8) with the named pool vault, mint
/// and token program (ARCH §2.4 rule 2).
pub fn trader_accounts<'info>(
    kind: VenueKind,
    market_key: Pubkey,
    market: &Market,
    trader: AccountInfo<'info>,
    market_vault: AccountInfo<'info>,
    trade: &'info [AccountInfo<'info>],
    named: Named<'info>,
) -> Result<TraderAccounts<'info>> {
    let at = |i: usize| -> Result<AccountInfo<'info>> {
        trade
            .get(i)
            .cloned()
            .ok_or_else(|| error!(ShortwinError::BadRemainingAccounts))
    };
    let wsol_slots = match kind {
        VenueKind::PumpCurve => None,
        VenueKind::PumpSwap => Some((
            pump_amm::T_TRADER_WSOL,
            pump_amm::T_WSOL_MINT,
            pump_amm::T_TOKEN_PROGRAM,
        )),
        VenueKind::RaydiumV4 => Some((
            raydium_v4::X_TRADER_WSOL,
            raydium_v4::X_WSOL_MINT,
            raydium_v4::X_TOKEN_PROGRAM,
        )),
        VenueKind::RaydiumCpmm => Some((
            raydium_cpmm::X_TRADER_WSOL,
            raydium_cpmm::X_WSOL_MINT,
            raydium_cpmm::X_WSOL_PROGRAM,
        )),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Some((mock::T_TRADER_WSOL, mock::T_WSOL_MINT, mock::T_WSOL_PROGRAM)),
        _ => return err!(ShortwinError::NoSupportedVenue),
    };
    let wsol = match wsol_slots {
        Some((w, m, p)) => Some(WsolSide {
            trader_wsol: at(w)?,
            trader_wsol_bump: market.trader_wsol_bump,
            wsol_mint: at(m)?,
            wsol_token_program: at(p)?,
        }),
        None => None,
    };
    let base_slot = match kind {
        VenueKind::PumpCurve => Some(pump_curve::T_TRADER_BASE_ATA),
        VenueKind::PumpSwap => Some(pump_amm::T_TRADER_BASE_ATA),
        _ => None,
    };
    let base = match base_slot {
        Some(slot) => Some(BaseSide {
            trader_base_ata: at(slot)?,
            pool_vault: named.pool_vault,
            mint: named.mint,
            decimals: named.decimals,
            token_program: named.token_program,
        }),
        None => None,
    };
    Ok(TraderAccounts {
        market: market_key,
        trader,
        trader_bump: market.trader_bump,
        market_vault,
        system_program: named.system_program,
        wsol,
        base,
    })
}

/// What the position refunds the borrower after a sell leg: the ephemeral WSOL account's rent
/// on the WSOL venues (PumpSwap, Raydium, the mock), 0 on the curve (its leg refunds `ert` itself).
pub fn sell_refund(kind: VenueKind) -> Result<u64> {
    match kind {
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => TraderAccounts::wsol_rent(),
        VenueKind::PumpSwap | VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => {
            TraderAccounts::wsol_rent()
        }
        _ => Ok(0),
    }
}

/// The venue-side rent tolerance of a venue (`ert`, ARCH §2.4 rule 3): `exec_rent_tol` on the
/// pump venues, 0 on Raydium and the mock.
pub fn effective_ert(kind: VenueKind, exec_rent_tol: u64) -> u64 {
    match kind {
        VenueKind::PumpCurve | VenueKind::PumpSwap => exec_rent_tol,
        _ => 0,
    }
}

/// The sell leg of an open (exact in `q`, venue slippage bound `min_out`): `S`, the lamports of
/// proceeds credited to the position, and the refund the caller owes the borrower. On the WSOL
/// venues the position also holds the ephemeral account's rent afterwards
/// (`TraderAccounts::wsol_rent`), which the borrower paid, and on PumpSwap the venue-side rent
/// `r` netted out of `S`: the caller refunds [`SellOut::refund`] by a direct lamport move **after
/// its last CPI that names the position or the borrower** (a direct move is visible to the
/// runtime's per-CPI balance check only when both accounts are in that CPI). The curve funds the
/// trader with `ert` from the borrower and refunds it inside the leg (`pump_curve::sell`).
pub fn sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    min_out: u64,
) -> Result<SellOut> {
    let plain = |s: u64| -> Result<SellOut> {
        Ok(SellOut {
            s,
            refund: sell_refund(leg.kind)?,
            venue_rent: 0,
        })
    };
    match leg.kind {
        VenueKind::PumpCurve => plain(pump_curve::sell(leg, borrower, position, q, min_out)?),
        VenueKind::PumpSwap => pump_amm::sell(leg, borrower, position, q, min_out),
        VenueKind::RaydiumV4 => plain(raydium_v4::sell(leg, borrower, position, q, min_out)?),
        VenueKind::RaydiumCpmm => plain(raydium_cpmm::sell(leg, borrower, position, q, min_out)?),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => plain(mock::sell(leg, borrower, position, q, min_out)?),
        _ => {
            let _ = (borrower, position, q, min_out);
            err!(ShortwinError::NoSupportedVenue)
        }
    }
}

/// The buy leg of an exit (exact out `q`, venue slippage bound `max_in`), paid from the
/// position.
pub fn buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
) -> Result<BuyOut> {
    match leg.kind {
        VenueKind::PumpCurve => pump_curve::buy(leg, position, q, max_in),
        VenueKind::PumpSwap => pump_amm::buy(leg, position, q, max_in),
        VenueKind::RaydiumV4 => raydium_v4::buy(leg, position, q, max_in),
        VenueKind::RaydiumCpmm => raydium_cpmm::buy(leg, position, q, max_in),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => mock::buy(leg, position, q, max_in),
        _ => {
            let _ = (position, q, max_in);
            err!(ShortwinError::NoSupportedVenue)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::state::{ExtraEntry, SeedPart};

    fn seed(kind: u8, len: u8, bytes: &[u8]) -> SeedPart {
        let mut b = [0u8; 32];
        b[..bytes.len()].copy_from_slice(bytes);
        SeedPart {
            kind,
            len,
            bytes: b,
        }
    }

    /// AIR-10: the exit risk view's cap is the hard cap and, on PumpSwap once resolved, `⌈φ at
    /// P_ema/100⌉` — also on a halted pool (its fee still resolves) — and the hard cap alone
    /// without a fee at `P_ema` and on every other venue.
    #[test]
    fn exit_risk_cap_takes_the_fee_at_p_ema_on_pumpswap_only() {
        let pool = |at_ema: u32| pump_amm::PoolVenue {
            disable_flags: 0,
            boost_amount: 0,
            virtual_part: 0,
            is_cashback: false,
            fees: Some(FeeBps::new(40, 7, 3)),
            fee_ppm_at_ema: at_ema,
        };
        let with = |kind: VenueKind, at_ema: u32| {
            let mut st = VenueState::not_executable(kind);
            st.pool = Some(pool(at_ema));
            st
        };
        assert_eq!(with(VenueKind::PumpSwap, 3_000).exit_risk_cap_bps(500), 30);
        assert_eq!(with(VenueKind::PumpSwap, 2_950).exit_risk_cap_bps(500), 30);
        assert_eq!(with(VenueKind::PumpSwap, 3_000).exit_risk_cap_bps(25), 25);
        assert_eq!(
            with(VenueKind::PumpSwap, u32::MAX).exit_risk_cap_bps(500),
            500
        );
        assert_eq!(
            VenueState::not_executable(VenueKind::PumpSwap).exit_risk_cap_bps(500),
            500
        );
        assert_eq!(
            with(VenueKind::RaydiumCpmm, 3_000).exit_risk_cap_bps(500),
            500
        );
        assert_eq!(
            with(VenueKind::PumpCurve, 3_000).exit_risk_cap_bps(500),
            500
        );
    }

    fn inputs() -> CurvePricingInputs {
        CurvePricingInputs {
            fees: FeeBps::new(0, 95, 30),
            offsets: CurveOffsets { token: 1, quote: 1 },
            live_pool_tokens: 200_661_149_081_575,
            migration_fee: 15_000_001,
            pool_tiers: Rc::from(Vec::new()),
            pool_creator_fee_configurable: true,
            pool_disable_flags: 0,
            coin_creator_is_default: false,
            fee_ppm_at_ema: u32::MAX,
            stage_word: 0,
            stage_flags: 0,
        }
    }

    /// pump-final §5.2 / §6.1.5 / §6.2: the completion state round-trips through its packed form.
    /// A live curve's synthetic fee (P1 correction (a)) — a component above `u32::MAX` bps or an
    /// LP part leaves it unknown (no synthetic route); a complete curve's `pcQ` and its two flags;
    /// and what `curve_pricing` makes of each (the synthetic route only live, the live
    /// `curve_base_ata` and `pcQ` only after a pool part).
    #[test]
    fn the_curve_stage_round_trips_and_prices() {
        // The literal the web's constants test reads is shortwin-math's.
        assert_eq!(STANDARD_POOL_TOKENS, math_quote::STANDARD_POOL_TOKENS);
        let mut c = inputs();
        assert_eq!(
            c.stage(),
            CurveStage::Live {
                synthetic_fees: None
            }
        );
        for f in [
            FeeBps::new(0, 80, 25),
            FeeBps::ZERO,
            FeeBps::new(0, u64::from(u32::MAX), u64::from(u32::MAX)),
        ] {
            c.set_stage(CurveStage::Live {
                synthetic_fees: Some(f),
            });
            assert_eq!(c.synthetic_fees(), Some(f));
            assert!(c.post_complete_known());
            let p = c.curve_pricing();
            assert_eq!(p.synthetic, Some(f));
            assert_eq!(p.migration.pool_tokens, STANDARD_POOL_TOKENS);
            assert_eq!(p.migration.post_complete_quote_in, 0);
        }
        for f in [
            Some(FeeBps::new(0, u64::from(u32::MAX) + 1, 30)),
            Some(FeeBps::new(0, 95, u64::MAX)),
            Some(FeeBps::new(1, 95, 30)),
            None,
        ] {
            c.set_stage(CurveStage::Live { synthetic_fees: f });
            assert_eq!(c.synthetic_fees(), None, "{f:?}");
            assert_eq!(c.curve_pricing().synthetic, None);
        }
        for (pcq, post_complete, known) in [
            (2_642_475_552, true, true),
            (u64::MAX, true, false),
            (0, true, false),
            (0, false, true),
        ] {
            let stage = CurveStage::Complete {
                post_complete_quote_in: pcq,
                post_complete,
                post_complete_known: known,
            };
            c.set_stage(stage);
            assert_eq!(c.stage(), stage);
            assert_eq!(c.synthetic_fees(), None);
            assert_eq!(c.post_complete_known(), known);
            let p = c.curve_pricing();
            assert_eq!(p.synthetic, None);
            let want = if post_complete {
                (u128::from(c.live_pool_tokens), pcq)
            } else {
                (STANDARD_POOL_TOKENS, 0)
            };
            assert_eq!(
                (p.migration.pool_tokens, p.migration.post_complete_quote_in),
                want
            );
        }
    }

    /// T-SIZEOF (pump-final G-6): `CurvePricingInputs` and `VenueState` keep the sizes they had at
    /// the start of P2 (the spec allows 16 bytes more; the completion state's packing adds none).
    /// `execute_pool_switch` keeps venue states on its 4 KiB SBF stack frame (an `Option<FeeBps>`
    /// field pushed its handler 128 bytes over in P1) and the pool-selection instructions box
    /// several on the 32 KiB heap (a 48-byte growth overflowed it in M7-SEC-5), so any growth
    /// has to be re-measured there (`m3_cu`) before this pin moves.
    #[test]
    fn venue_state_size() {
        // The layout at the start of P2 (P1's: the synthetic fee as two `u32` and a flag).
        #[allow(dead_code)]
        struct AtP2Start {
            fees: FeeBps,
            offsets: CurveOffsets,
            live_pool_tokens: u64,
            migration_fee: u64,
            pool_tiers: Rc<[FeeTier]>,
            pool_creator_fee_configurable: bool,
            pool_disable_flags: u8,
            coin_creator_is_default: bool,
            fee_ppm_at_ema: u32,
            synthetic_protocol_bps: u32,
            synthetic_creator_bps: u32,
            synthetic_known: bool,
        }
        use core::mem::size_of;
        assert_eq!(size_of::<CurvePricingInputs>(), size_of::<AtP2Start>());
        assert_eq!(
            size_of::<Option<CurvePricingInputs>>(),
            size_of::<Option<AtP2Start>>()
        );
        // Measured at the start of P2 on the host (x86-64).
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(
                size_of::<CurvePricingInputs>(),
                CURVE_PRICING_INPUTS_SIZE_AT_P2
            );
            assert_eq!(size_of::<VenueState>(), VENUE_STATE_SIZE_AT_P2);
        }
    }

    /// A `PumpMigrating` state of 98u8RJ's curve (pump-final V4) as read before `migrate_v2`:
    /// complete (`rT = 0`), `rQ` 85,005,359,190, `curve_base_ata` 206.9e12 − pcB with
    /// `(pcB, pcQ)` = `pc`, `vQ − rQ` = `offset_quote`.
    fn migrating_98u8rj(pc: (u64, u64), offset_quote: u128) -> VenueState {
        let rq: u128 = 85_005_359_190;
        let live = CurveState {
            vt: 279_900_000_000_000,
            vq: rq + offset_quote,
            rt: 0,
            rq,
            complete: true,
        };
        let mut inputs = inputs();
        inputs.offsets = CurveOffsets::of(&live).unwrap();
        inputs.live_pool_tokens = 206_900_000_000_000 - pc.0;
        inputs.pool_tiers = Rc::from(vec![FeeTier {
            threshold: 0,
            fees: FeeBps::new(20, 5, 0),
        }]);
        let post_complete = pc != (0, 0);
        let known =
            !post_complete || ((pc.0 != 0) == (pc.1 != 0) && offset_quote == 30_000_000_000);
        inputs.set_stage(CurveStage::Complete {
            post_complete_quote_in: pc.1,
            post_complete,
            post_complete_known: known,
        });
        let mut st = VenueState::not_executable(VenueKind::PumpMigrating);
        st.curve = Some(CurveVenue {
            live,
            canonical_pool: detect::CanonicalPool::Absent,
            pool: None,
            pricing: Some(inputs),
        });
        st
    }

    /// T-MATH-4, the program half (pump-final §6.1.5, §6.2): on a migrating curve FV is
    /// `migrating_fv` — bit-identical to the pre-v3 FV without a pool part, ρ-scaled on the live
    /// `(X₁, Y₁)` after one — and **undefined** when the post-complete fields are not known (one of
    /// `pcB` / `pcQ` zero, or `vQ − rQ ≠ ivqr`), while the other quotes stay defined and
    /// `risk_quoter` keeps the curve quoter (never `cap_only`: the documented A63 exception).
    #[test]
    fn fv_on_unknown_post_complete_fields_is_undefined() {
        use bytemuck::Zeroable;
        let market = Market::zeroed();
        let (q, cap) = (40_000_000_000_000u64, 500u64);
        let p = price_wad(279_900_000_000_000, 115_005_359_190).unwrap();
        let l = math_quote::liquidity(279_900_000_000_000, 115_005_359_190).unwrap();
        let (pcb, pcq) = (6_238_850_918_425u64, 2_642_475_552u64);
        let fv = |st: &VenueState| {
            let r = risk_quoter(&market, st, &[], cap).unwrap();
            assert!(matches!(r, Quoter::Curve(_)), "never cap_only");
            (
                r.fv_repriced(q, l, p),
                r.buy_xy(q, 279_900_000_000_000, 115_005_359_190),
            )
        };
        // No pool part: the pre-v3 FV on the standard pool, to the lamport.
        let plain = migrating_98u8rj((0, 0), 30_000_000_000);
        let (got, xy) = fv(&plain);
        let Quoter::Curve(c) = risk_quoter(&market, &plain, &[], cap).unwrap() else {
            unreachable!()
        };
        let (xp, yp) = math_quote::migrated_pool(
            &c.live,
            &MigrationParams {
                migration_fee: 15_000_001,
                pool_tokens: STANDARD_POOL_TOKENS,
                post_complete_quote_in: 0,
            },
        )
        .unwrap();
        let old = {
            let fees = math_fees::pump_amm(yp, xp, &c.pricing.pool).unwrap();
            let (x, y) = math_quote::reprice(math_quote::liquidity(xp, yp).unwrap(), p).unwrap();
            math_quote::sol_leg::buy_exact_out(q, x, y, false, &fees).unwrap()
        };
        assert_eq!(got, Some(old));
        // The EMA leg's ring state of the completed curve (`rT = 0`): `B̂_str` on the pool (G-2).
        assert!(xy.is_some_and(|b| b.cost > 0));
        // A known pool part: ρ-scaled on (X₁, Y₁) = (206.9e12 − pcB, Y₀ + pcQ), above the plain FV.
        let synth = migrating_98u8rj((pcb, pcq), 30_000_000_000);
        let Quoter::Curve(c1) = risk_quoter(&market, &synth, &[], cap).unwrap() else {
            unreachable!()
        };
        assert_eq!(
            math_quote::migrated_pool(&c1.live, &c1.pricing.migration),
            Some((200_661_149_081_575, 87_632_834_741))
        );
        let fv1 = fv(&synth).0.unwrap();
        assert_eq!(
            Some(fv1),
            math_quote::migrating_fv(q, p, &c1.live, &c1.pricing.migration, &c1.pricing.pool)
        );
        assert!(fv1.cost > old.cost);
        // Not known: FV undefined, every other quote defined.
        for (pc, offset) in [
            ((pcb, pcq), 30_000_000_001),
            ((pcb, 0), 30_000_000_000),
            ((0, pcq), 30_000_000_000),
        ] {
            let st = migrating_98u8rj(pc, offset);
            assert_eq!(fv(&st).0, None, "{pc:?} {offset}");
            let r = risk_quoter(&market, &st, &[], cap).unwrap();
            assert!(r.buy_repriced(q, l, p).is_some());
            assert!(fv(&st).1.is_some());
            assert!(r.buy_xy(q, 300_000_000_000_000, 100_000_000_000).is_some());
        }
    }

    /// `size_of::<CurvePricingInputs>()` at the start of P2 (host, x86-64).
    #[cfg(target_arch = "x86_64")]
    const CURVE_PRICING_INPUTS_SIZE_AT_P2: usize = 112;
    /// `size_of::<VenueState>()` at the start of P2 (host, x86-64).
    #[cfg(target_arch = "x86_64")]
    const VENUE_STATE_SIZE_AT_P2: usize = 688;

    /// `["extra", mint]` under `program`.
    fn per_mint(program: Pubkey) -> ExtraEntry {
        let none = seed(SEED_NONE, 0, &[]);
        ExtraEntry {
            kind: ENTRY_TEMPLATE,
            writable: 0,
            n_seeds: 2,
            _pad: [0; 5],
            key: program,
            seeds: [
                seed(SEED_CONST, 5, b"extra"),
                seed(SEED_PLACEHOLDER, PLACEHOLDER_MINT, &[]),
                none,
                none,
            ],
        }
    }

    /// M2-SEC-3: a template derives with the canonical bump (what `find_program_address`
    /// returns) and refuses every other one — the lower off-curve bumps a caller could pick,
    /// and the higher ones, which are on the curve.
    #[test]
    fn templates_take_the_canonical_bump_only() {
        let mint = Pubkey::new_unique();
        let ph = Placeholders {
            mint,
            pool: None,
            trader: Pubkey::new_unique(),
            bonding_curve: Pubkey::new_unique(),
            coin_creator: None,
        };
        let mut non_canonical = 0;
        for _ in 0..32 {
            let program = Pubkey::new_unique();
            let entry = per_mint(program);
            let (canon, bump) = Pubkey::find_program_address(&[b"extra", mint.as_ref()], &program);
            assert_eq!(derive_template(&entry, &ph, bump).unwrap(), canon);
            for b in 0..=u8::MAX {
                if b == bump {
                    continue;
                }
                let off_curve =
                    Pubkey::create_program_address(&[b"extra", mint.as_ref(), &[b]], &program)
                        .is_ok();
                non_canonical += usize::from(off_curve);
                assert!(
                    derive_template(&entry, &ph, b).is_err(),
                    "bump {b} (canonical {bump}, off curve {off_curve})"
                );
            }
        }
        assert!(
            non_canonical > 0,
            "the sample must contain non-canonical PDAs"
        );
    }

    /// M10 batch 3: the pool denylist is the adopted pool and its two reserve accounts, read
    /// from the cache the ReadSet (and, for the curve's quote ATA, the TradeExtras) is checked
    /// against — on both Raydium layouts whichever side WSOL is on, and on both pump venues.
    #[test]
    fn the_pool_denylist_is_the_pool_and_its_reserves() {
        use crate::state::{
            pump_swap_cache as ps, raydium_cache as rc, MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN,
        };
        use bytemuck::Zeroable;
        let mut m = Market::zeroed();
        for slot in m.pool_cache.iter_mut() {
            *slot = Pubkey::new_unique();
        }
        m.bonding_curve = Pubkey::new_unique();
        m.curve_base_ata = Pubkey::new_unique();
        m.curve_quote_ata = Pubkey::new_unique();
        m.canonical_pool = Pubkey::new_unique();
        let reserves = [
            m.pool_cache[rc::RAY_POOL],
            m.pool_cache[rc::RAY_VAULT_SOL],
            m.pool_cache[rc::RAY_VAULT_TOK],
        ];
        for sol_first in [false, true] {
            if sol_first {
                m.flags |= MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN;
            }
            let v4 = raydium_v4::read_keys(&m);
            let cpmm = raydium_cpmm::read_keys(&m);
            for (kind, read, slots) in [
                (
                    VenueKind::RaydiumV4,
                    &v4[..],
                    [
                        raydium_v4::R_POOL,
                        raydium_v4::R_COIN_VAULT,
                        raydium_v4::R_PC_VAULT,
                    ],
                ),
                (
                    VenueKind::RaydiumCpmm,
                    &cpmm[..],
                    [
                        raydium_cpmm::R_POOL,
                        raydium_cpmm::R_TOKEN_0_VAULT,
                        raydium_cpmm::R_TOKEN_1_VAULT,
                    ],
                ),
            ] {
                let deny = pool_denylist(&m, kind).unwrap();
                assert_eq!(deny, reserves, "{kind:?}");
                for i in slots {
                    assert!(deny.contains(&read[i]), "{kind:?} slot {i}");
                }
            }
        }
        let pamm = pump_amm::read_keys(&m);
        let deny = pool_denylist(&m, VenueKind::PumpSwap).unwrap();
        assert_eq!(
            deny,
            [
                pamm[pump_amm::P_POOL],
                pamm[pump_amm::P_BASE_VAULT],
                pamm[pump_amm::P_QUOTE_VAULT]
            ]
        );
        assert_eq!(deny[1], m.pool_cache[ps::POOL_BASE_VAULT]);
        let curve = pump_curve::read_keys(&m);
        assert_eq!(
            pool_denylist(&m, VenueKind::PumpCurve).unwrap(),
            [
                curve[pump_curve::C_CURVE],
                curve[pump_curve::C_CURVE_BASE_ATA],
                m.curve_quote_ata
            ]
        );
        // No trading venue, no list (the instruction refused before the extras).
        for kind in [VenueKind::None, VenueKind::PumpMigrating] {
            assert!(pool_denylist(&m, kind).is_err(), "{kind:?}");
        }
    }
}
