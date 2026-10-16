//! `VenueExtras` (zero-copy, seeds `["venue_extras", venue u8]`, 6,512 B): bounded trailing
//! accounts for a venue's CPIs (ARCH §2.3.2, §3.0, A41, A55; V2-6; **gated by O6**).
//!
//! `initialize_config` creates one per CPI venue kind (`PumpCurve`, `PumpSwap`, `RaydiumV4`,
//! `RaydiumCpmm`; the seed byte is the `VenueKind` value) with empty sets. The setter
//! (`set_venue_extras`) and the trading-side parser belong to M2 (O6 is recorded in SPEC §14
//! but the powers are built with the position engine); nothing reads these accounts in M1.
//!
//! Per venue: for each instruction kind (sell, buy, claim) a `current`, a `previous` and a
//! `stable` set of up to 4 entries with a `version` each, and the time `current` was written.
//! An entry is a fixed key or a derivation template `{program, seeds: [SeedPart; ≤ 4],
//! writable}`; a `SeedPart` is constant bytes (≤ 32) or a placeholder among `{mint, pool,
//! trader, bonding_curve, coin_creator}`. Callers select `none`, `current`, `previous` or
//! `stable`:
//!
//! - `previous` is the set the last write displaced, so one hostile or wrong write can never
//!   brick exits (ARCH §2.3.2, SPEC §11);
//! - `stable` (M2-SEC-2) is the newest set that stood as `current` for at least
//!   [`crate::constants::EXTRAS_STABLE_S`] before a write displaced it, or the set Governance
//!   published last. Two quick writes shift a working set out of `previous`; they cannot shift
//!   it out of `stable`: that takes a set the attacker keeps `current`, unchallenged, for
//!   `EXTRAS_STABLE_S` (7 days) — longer than Governance needs to re-publish (48 h Squads time
//!   lock) or to rotate the Emergency key through a timelocked `Authorities` change.
//!
//! **Layout** (ARCH §2.3.1 / §2.3.2 as synced after M1, M1-FID-13, **grown by M2-SEC-2**):
//! `SeedPart` 34 B, `ExtraEntry` 176 B, `ExtrasSet` 712 B, `IxExtras` 2,144 B (was 1,424 B:
//! `stable` and `current_since` added), the account 6,512 B (was 4,352 B; rent ≈ 0.0337 SOL ×
//! 4). M2's extras parser (`venues::parse`) and `set_venue_extras` read and write it.

use anchor_lang::prelude::*;

/// Instruction kinds with extras sets, in `VenueExtras.sets` order.
pub const EXTRAS_IX_SELL: usize = 0;
/// Buy (close, liquidate, expire).
pub const EXTRAS_IX_BUY: usize = 1;
/// Venue reward claim.
pub const EXTRAS_IX_CLAIM: usize = 2;
/// Number of instruction kinds.
pub const EXTRAS_IX_KINDS: usize = 3;
/// Entries per set.
pub const EXTRAS_MAX_ENTRIES: usize = 4;
/// Seed parts per template.
pub const EXTRAS_MAX_SEEDS: usize = 4;

/// `SeedPart.kind`: unused.
pub const SEED_NONE: u8 = 0;
/// `SeedPart.kind`: constant bytes (`len` bytes of `bytes`).
pub const SEED_CONST: u8 = 1;
/// `SeedPart.kind`: a placeholder (`len` = `PLACEHOLDER_*`).
pub const SEED_PLACEHOLDER: u8 = 2;
/// Placeholder: the market's mint.
pub const PLACEHOLDER_MINT: u8 = 0;
/// Placeholder: the market's pool.
pub const PLACEHOLDER_POOL: u8 = 1;
/// Placeholder: the market's trader PDA.
pub const PLACEHOLDER_TRADER: u8 = 2;
/// Placeholder: the pump bonding curve.
pub const PLACEHOLDER_BONDING_CURVE: u8 = 3;
/// Placeholder: the coin creator.
pub const PLACEHOLDER_COIN_CREATOR: u8 = 4;

/// `ExtraEntry.kind`: empty slot.
pub const ENTRY_NONE: u8 = 0;
/// `ExtraEntry.kind`: a fixed key (`key`).
pub const ENTRY_FIXED: u8 = 1;
/// `ExtraEntry.kind`: a template derived under program `key` from `seeds`.
pub const ENTRY_TEMPLATE: u8 = 2;

/// One seed of a derivation template (34 B).
#[zero_copy]
#[derive(PartialEq, Eq)]
pub struct SeedPart {
    /// `SEED_NONE`, `SEED_CONST` or `SEED_PLACEHOLDER`.
    pub kind: u8,
    /// Constant: number of bytes used; placeholder: `PLACEHOLDER_*`.
    pub len: u8,
    pub bytes: [u8; 32],
}

/// One trailing account (176 B).
#[zero_copy]
#[derive(PartialEq, Eq)]
pub struct ExtraEntry {
    /// `ENTRY_NONE`, `ENTRY_FIXED` or `ENTRY_TEMPLATE`.
    pub kind: u8,
    /// 1 if the account is passed writable.
    pub writable: u8,
    /// Seed parts used (template).
    pub n_seeds: u8,
    pub _pad: [u8; 5],
    /// The fixed key, or the template's program.
    pub key: Pubkey,
    pub seeds: [SeedPart; 4],
}

/// A set of up to 4 entries with its version (712 B).
#[zero_copy]
#[derive(PartialEq, Eq)]
pub struct ExtrasSet {
    pub version: u32,
    /// Entries used.
    pub count: u8,
    pub _pad: [u8; 3],
    pub entries: [ExtraEntry; 4],
}

/// The selectable sets of one instruction kind (2,144 B).
#[zero_copy]
#[derive(PartialEq, Eq)]
pub struct IxExtras {
    pub current: ExtrasSet,
    /// The set the last write displaced.
    pub previous: ExtrasSet,
    /// The newest set that stood as `current` for `EXTRAS_STABLE_S` before being displaced,
    /// or Governance's last publication (M2-SEC-2; module docs).
    pub stable: ExtrasSet,
    /// When `current` was written (unix seconds; 0: never, the empty set of
    /// `initialize_config`).
    pub current_since: i64,
}

/// Bounded venue extras of one venue kind.
#[account(zero_copy)]
pub struct VenueExtras {
    /// Layout version ([`crate::constants::VENUE_EXTRAS_VERSION`]).
    pub version: u16,
    /// PDA bump of `["venue_extras", venue]`.
    pub bump: u8,
    /// The `VenueKind` these sets apply to.
    pub venue: u8,
    pub _pad0: [u8; 4],
    /// Sell, buy, claim.
    pub sets: [IxExtras; 3],
    pub _reserved: [u8; 64],
}

impl VenueExtras {
    /// PDA seed prefix (`["venue_extras", venue u8]`).
    pub const SEED: &'static [u8] = b"venue_extras";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + core::mem::size_of::<Self>();
}
