//! `set_venue_extras(venue, ix, entries)` (ARCH §2.3.2, §3.0 "Bounded extras", §3.1; A41,
//! C0-4 a, V2-6; **O6**, recorded in SPEC §0, §11, §14): the Emergency vault (or Governance)
//! publishes the trailing accounts a venue's `ix` CPI (sell, buy, claim) passes, so an
//! **additive** venue interface change — new trailing accounts, including per-mint or per-pool
//! PDAs — can be followed without a program upgrade. No timelock.
//!
//! The call moves the `current` set to `previous` and writes `entries` (0–4 fixed keys or
//! derivation templates) as the new `current` with `version = old current version + 1`.
//! Callers of the trading instructions select `none`, `current`, `previous` or `stable` in
//! their instruction data (`ExtrasSel`), so **one wrong or hostile write cannot brick exits**.
//! It moves no funds.
//!
//! **`stable` (M2-SEC-2).** `previous` survives one write only: two quick writes by a
//! compromised (or twice-mistaken) Emergency key would push a working set out of both
//! `current` and `previous`, and a venue that requires its new trailing accounts would then
//! refuse every exit until Governance re-published the set behind its 48 h Squads time lock.
//! So each instruction kind also keeps a `stable` set, which only two things write:
//!
//! 1. a write that displaces a `current` which has stood for at least
//!    [`EXTRAS_STABLE_S`] (7 days) copies it into `stable` first — a set that worked for a week
//!    becomes the escape that quick writes cannot touch;
//! 2. a write by **Governance** also pins its new set as `stable` (the slow, reviewed path
//!    ratifies an Emergency publication at once).
//!
//! The Emergency key can therefore displace `stable` only by keeping a set of its own
//! `current`, unchallenged, for `EXTRAS_STABLE_S` — longer than Governance needs to
//! re-publish (and re-pin) the working set or to rotate the Emergency key through a
//! timelocked `Authorities` change. ARCH §2.3.2 / §3.0 describe two sets and state the
//! one-write guarantee only; `stable` is this fix's addition (flagged for the doc owner).
//!
//! **Checks here** (`venues::check_entry`): at most 4 entries; each a fixed key or a template of
//! 1–4 seeds; no key and no template program equal to short.win or the default key; constant
//! seeds of at most 32 bytes (zero past their length); placeholders among `mint`, `pool`,
//! `trader`, `bonding_curve`, `coin_creator`. **At use time** (`venues::parse_extras`) each
//! extra must also be a non-signer, not owned by short.win and not on the instruction's
//! denylist (the market's own PDAs and vaults, the position, the borrower, …) or the adopted
//! pool's (its pool account and reserve vaults, `venues::pool_denylist`), and writable exactly
//! as its flag says. A set this instruction accepts can therefore still be refused at use time:
//! its caller then selects `previous`, `stable` or `none`.

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_VERSION, EXTRAS_STABLE_S, VENUE_EXTRAS_VERSION};
use crate::errors::{CheckedExt, ShortwinError};
use crate::events::VenueExtrasSet;
use crate::state::{
    ExtraEntry, ExtrasSet, GlobalConfig, SeedPart, VenueExtras, VenueKind, EXTRAS_IX_KINDS,
    EXTRAS_MAX_ENTRIES, EXTRAS_MAX_SEEDS,
};
use crate::venues::check_entry;

/// One seed of a template, as the argument encoding of [`SeedPart`]: `kind` 1 = constant
/// (`len` bytes of `bytes`), 2 = placeholder (`len` = the placeholder id, `bytes` zero).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedPartArg {
    pub kind: u8,
    pub len: u8,
    pub bytes: [u8; 32],
}

/// One trailing account, as the argument encoding of [`ExtraEntry`]: `kind` 1 = a fixed `key`,
/// 2 = a template derived under program `key` from `seeds`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExtraEntryArg {
    pub kind: u8,
    pub writable: bool,
    pub key: Pubkey,
    pub seeds: Vec<SeedPartArg>,
}

impl ExtraEntryArg {
    /// The account encoding (`BadExtras` for more than 4 seeds).
    pub fn to_entry(&self) -> Result<ExtraEntry> {
        require!(
            self.seeds.len() <= EXTRAS_MAX_SEEDS,
            ShortwinError::BadExtras
        );
        let mut seeds = [SeedPart {
            kind: 0,
            len: 0,
            bytes: [0; 32],
        }; 4];
        for (slot, s) in seeds.iter_mut().zip(self.seeds.iter()) {
            *slot = SeedPart {
                kind: s.kind,
                len: s.len,
                bytes: s.bytes,
            };
        }
        Ok(ExtraEntry {
            kind: self.kind,
            writable: u8::from(self.writable),
            n_seeds: u8::try_from(self.seeds.len())
                .map_err(|_| error!(ShortwinError::BadExtras))?,
            _pad: [0; 5],
            key: self.key,
            seeds,
        })
    }
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(venue: u8)]
pub struct SetVenueExtras<'info> {
    /// `pause_authority` or `governance`.
    pub authority: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion,
        constraint = config.load()?.is_pause_or_governance(authority.key)
            @ ShortwinError::Unauthorized
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [VenueExtras::SEED, &[venue]],
        bump = venue_extras.load()?.bump
    )]
    pub venue_extras: AccountLoader<'info, VenueExtras>,
}

pub fn handler(
    ctx: Context<SetVenueExtras>,
    venue: u8,
    ix: u8,
    entries: Vec<ExtraEntryArg>,
) -> Result<()> {
    let kind = VenueKind::from_u8(venue).ok_or(ShortwinError::BadExtras)?;
    require!(kind.extras_seed() == Some(venue), ShortwinError::BadExtras);
    let ix_i = usize::from(ix);
    require!(ix_i < EXTRAS_IX_KINDS, ShortwinError::BadExtras);
    require!(
        entries.len() <= EXTRAS_MAX_ENTRIES,
        ShortwinError::BadExtras
    );
    let mut new_set = ExtrasSet {
        version: 0,
        count: u8::try_from(entries.len()).map_err(|_| error!(ShortwinError::BadExtras))?,
        _pad: [0; 3],
        entries: [ExtraEntry {
            kind: 0,
            writable: 0,
            n_seeds: 0,
            _pad: [0; 5],
            key: Pubkey::default(),
            seeds: [SeedPart {
                kind: 0,
                len: 0,
                bytes: [0; 32],
            }; 4],
        }; 4],
    };
    let mut keys = [Pubkey::default(); 4];
    for ((slot, key), arg) in new_set
        .entries
        .iter_mut()
        .zip(keys.iter_mut())
        .zip(entries.iter())
    {
        let e = arg.to_entry()?;
        check_entry(&e)?;
        *slot = e;
        *key = arg.key;
    }
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let by_governance = ctx.accounts.config.load()?.governance == ctx.accounts.authority.key();
    let (version, stable_version) = {
        let mut ve = ctx.accounts.venue_extras.load_mut()?;
        require!(
            ve.version == VENUE_EXTRAS_VERSION && ve.venue == venue,
            ShortwinError::BadExtras
        );
        let set = ve.sets.get_mut(ix_i).ok_or(ShortwinError::BadExtras)?;
        new_set.version = set.current.version.checked_add(1).or_math()?;
        // M2-SEC-2 (module docs): a `current` that stood for `EXTRAS_STABLE_S` becomes the
        // `stable` escape before it is displaced; a Governance write pins its own set.
        if now.saturating_sub(set.current_since) >= EXTRAS_STABLE_S {
            set.stable = set.current;
        }
        set.previous = set.current;
        set.current = new_set;
        set.current_since = now;
        if by_governance {
            set.stable = new_set;
        }
        (new_set.version, set.stable.version)
    };
    emit_cpi!(VenueExtrasSet {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: now,
        venue,
        ix,
        version,
        keys,
        stable_version,
    });
    Ok(())
}
