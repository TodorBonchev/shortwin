//! The instructions the governance sends or stages, and [`describe`], which names what a staged
//! vault transaction runs so members can check it before they approve.
//!
//! - short.win's admin instructions (ARCH §3.1), from the program's own Anchor account and
//!   instruction types (as the keeper's `ixs.rs`);
//! - the upgradeable loader's `SetAuthorityChecked`, `Upgrade` and `ExtendProgram` ([`loader`]);
//! - the System program's transfer (the fee vault's pre-funding).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{AnchorDeserialize, Discriminator, InstructionData, ToAccountMetas};
use shortwin::PauseScope;
use shortwin::instructions::set_venue_extras::ExtraEntryArg;
use shortwin::state::{
    ConfigChange, EmaParamsArgs, GlobalConfig, InitConfigArgs, OracleCache, Params, TierParams,
    VenueExtras, VenueGuard, VenueKind,
};

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &shortwin::ID).0
}

/// short.win's PDAs the admin instructions name.
pub mod pdas {
    use super::*;

    /// `["config"]`.
    pub fn config() -> Pubkey {
        pda(&[GlobalConfig::SEED])
    }

    /// `["venue_guard"]`.
    pub fn venue_guard() -> Pubkey {
        pda(&[VenueGuard::SEED])
    }

    /// `["sol_usd"]`.
    pub fn oracle_cache() -> Pubkey {
        pda(&[OracleCache::SEED])
    }

    /// `["venue_extras", [venue kind]]`.
    pub fn venue_extras(venue: u8) -> Pubkey {
        pda(&[VenueExtras::SEED, &[venue]])
    }

    /// The `emit_cpi!` event authority.
    pub fn event_authority() -> Pubkey {
        pda(&[b"__event_authority"])
    }
}

fn build(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction {
        program_id: shortwin::ID,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// `initialize_config(args)`, signed by short.win's upgrade authority (who pays the accounts).
pub fn initialize_config(authority: &Pubkey, args: InitConfigArgs) -> Instruction {
    use shortwin::state::VenueKind;
    build(
        shortwin::accounts::InitializeConfig {
            authority: *authority,
            config: pdas::config(),
            venue_guard: pdas::venue_guard(),
            venue_extras_pump_curve: pdas::venue_extras(VenueKind::PumpCurve as u8),
            venue_extras_pump_swap: pdas::venue_extras(VenueKind::PumpSwap as u8),
            venue_extras_raydium_v4: pdas::venue_extras(VenueKind::RaydiumV4 as u8),
            venue_extras_raydium_cpmm: pdas::venue_extras(VenueKind::RaydiumCpmm as u8),
            oracle_cache: pdas::oracle_cache(),
            program_data: loader::program_data(&shortwin::ID),
            system_program: anchor_lang::system_program::ID,
        },
        shortwin::instruction::InitializeConfig { args },
    )
}

/// `propose_config(change)`, signed by `governance`.
pub fn propose_config(governance: &Pubkey, change: ConfigChange) -> Instruction {
    build(
        shortwin::accounts::ProposeConfig {
            governance: *governance,
            config: pdas::config(),
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ProposeConfig { change },
    )
}

/// `apply_config()` (anyone) with the optional accounts `change` needs: the new
/// `fee_recipient` of an `Authorities` change, the `OracleCache` of an `OracleFeed` change.
pub fn apply_config(change: &ConfigChange) -> Instruction {
    let (fee_recipient, oracle_cache) = match change {
        ConfigChange::Authorities { fee_recipient, .. } => (Some(*fee_recipient), None),
        ConfigChange::OracleFeed { .. } => (None, Some(pdas::oracle_cache())),
        _ => (None, None),
    };
    build(
        shortwin::accounts::ApplyConfig {
            config: pdas::config(),
            fee_recipient,
            oracle_cache,
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ApplyConfig {},
    )
}

/// `cancel_config()`, signed by `governance` or `pause_authority`.
pub fn cancel_config(authority: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::CancelConfig {
            authority: *authority,
            config: pdas::config(),
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CancelConfig {},
    )
}

/// `set_pause(scope, paused)`; `market` for `PauseScope::Market` only.
pub fn set_pause(authority: &Pubkey, market: Option<Pubkey>, paused: bool) -> Instruction {
    let scope = if market.is_some() {
        PauseScope::Market
    } else {
        PauseScope::Global
    };
    build(
        shortwin::accounts::SetPause {
            authority: *authority,
            config: pdas::config(),
            market,
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetPause { scope, paused },
    )
}

/// `set_route_broken(scope, venue_bit, broken)`; `market` for `PauseScope::Market` only.
pub fn set_route_broken(
    authority: &Pubkey,
    market: Option<Pubkey>,
    venue_bit: u8,
    broken: bool,
) -> Instruction {
    let scope = if market.is_some() {
        PauseScope::Market
    } else {
        PauseScope::Global
    };
    build(
        shortwin::accounts::SetRouteBroken {
            authority: *authority,
            config: pdas::config(),
            market,
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetRouteBroken {
            scope,
            venue_bit,
            broken,
        },
    )
}

/// `set_venue_extras(venue, ix, entries)`.
pub fn set_venue_extras(
    authority: &Pubkey,
    venue: u8,
    ix: u8,
    entries: Vec<ExtraEntryArg>,
) -> Instruction {
    build(
        shortwin::accounts::SetVenueExtras {
            authority: *authority,
            config: pdas::config(),
            venue_extras: pdas::venue_extras(venue),
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetVenueExtras { venue, ix, entries },
    )
}

/// `disable_venue(bit)`, signed by `pause_authority`.
pub fn disable_venue(pause_authority: &Pubkey, bit: u8) -> Instruction {
    build(
        shortwin::accounts::DisableVenue {
            pause_authority: *pause_authority,
            config: pdas::config(),
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::DisableVenue { bit },
    )
}

/// `revalidate_venue(venue, expected_slot, expected_elf_hash)`, signed by `pause_authority`, with
/// the venue's ProgramData: `expected_elf_hash` is the sha256 of the reviewed deploy's trimmed ELF
/// ([`trimmed_elf_sha256`]; AIR-01: it lets the revalidation survive extensions landing before it
/// executes, and records the fingerprint that tells a later extension from an upgrade).
pub fn revalidate_venue(
    pause_authority: &Pubkey,
    venue: u8,
    expected_slot: u64,
    expected_elf_hash: [u8; 32],
) -> Instruction {
    let program_data = shortwin::constants::venue_program_data(venue).unwrap_or_default();
    build(
        shortwin::accounts::RevalidateVenue {
            pause_authority: *pause_authority,
            config: pdas::config(),
            venue_guard: pdas::venue_guard(),
            program_data,
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::RevalidateVenue {
            venue,
            expected_slot,
            expected_elf_hash,
        },
    )
}

/// The sha256 of a ProgramData account's trimmed ELF (`shortwin_venue_state::program_data::
/// trimmed_elf`: the bytes after the 45-byte header without the zero tail — what `solana-verify
/// get-program-hash` prints and `revalidate_venue` checks; AIR-01). `None` if the account is not a
/// ProgramData.
pub fn trimmed_elf_sha256(owner: &Pubkey, data: &[u8]) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let elf = shortwin_venue_state::program_data::trimmed_elf(&owner.to_bytes(), data).ok()?;
    Some(Sha256::digest(elf).into())
}

/// `set_coverage_pause(false, COVERAGE_REASON_KEEPER)` on `market`: the Emergency vault's clear
/// (OWNER-M7-3 (b); it can never set the flag).
pub fn clear_coverage_pause(pause_authority: &Pubkey, market: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::SetCoveragePause {
            coverage_authority: *pause_authority,
            config: pdas::config(),
            market: *market,
            event_authority: pdas::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetCoveragePause {
            paused: false,
            reason: shortwin::events::COVERAGE_REASON_KEEPER,
        },
    )
}

/// The System program's `Transfer { lamports }` (tag 2).
pub fn system_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

/// The upgradeable loader's instructions (`UpgradeableLoaderInstruction`, bincode: a `u32`
/// tag, then the fields) and its ProgramData layout.
pub mod loader {
    use super::*;
    use anchor_lang::solana_program::bpf_loader_upgradeable;

    /// The loader program.
    pub const ID: Pubkey = bpf_loader_upgradeable::ID;
    const RENT: Pubkey =
        anchor_lang::prelude::pubkey!("SysvarRent111111111111111111111111111111111");
    const CLOCK: Pubkey =
        anchor_lang::prelude::pubkey!("SysvarC1ock11111111111111111111111111111111");
    /// `UpgradeableLoaderState::Buffer` metadata: tag 1, `Option<authority>` (37 bytes).
    pub const BUFFER_METADATA: usize = 37;
    /// `UpgradeableLoaderState::ProgramData` metadata: tag 3, slot, `Option<authority>`.
    pub const PROGRAM_DATA_METADATA: usize = 45;

    /// `program`'s ProgramData (`[program]` under the loader).
    pub fn program_data(program: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[program.as_ref()], &ID).0
    }

    /// `SetAuthorityChecked` (tag 7): `current` hands `account`'s authority to `new`; **both
    /// sign**, so a mistyped `new` cannot receive it (T-SQ-5).
    pub fn set_authority_checked(account: &Pubkey, current: &Pubkey, new: &Pubkey) -> Instruction {
        Instruction {
            program_id: ID,
            accounts: vec![
                AccountMeta::new(*account, false),
                AccountMeta::new_readonly(*current, true),
                AccountMeta::new_readonly(*new, true),
            ],
            data: 7u32.to_le_bytes().to_vec(),
        }
    }

    /// `Upgrade` (tag 3) of `program` from `buffer`, the buffer's lamports and any ProgramData
    /// surplus to `spill`, signed by `authority`.
    pub fn upgrade(
        program: &Pubkey,
        buffer: &Pubkey,
        spill: &Pubkey,
        authority: &Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: ID,
            accounts: vec![
                AccountMeta::new(program_data(program), false),
                AccountMeta::new(*program, false),
                AccountMeta::new(*buffer, false),
                AccountMeta::new(*spill, false),
                AccountMeta::new_readonly(RENT, false),
                AccountMeta::new_readonly(CLOCK, false),
                AccountMeta::new_readonly(*authority, true),
            ],
            data: 3u32.to_le_bytes().to_vec(),
        }
    }

    /// A Buffer account's authority (`None`: none, or not a buffer).
    pub fn buffer_authority(owner: &Pubkey, data: &[u8]) -> Option<Option<Pubkey>> {
        if *owner != ID || data.len() < BUFFER_METADATA || data[..4] != 1u32.to_le_bytes() {
            return None;
        }
        Some(match data[4] {
            1 => Some(Pubkey::new_from_array(data[5..37].try_into().ok()?)),
            _ => None,
        })
    }
}

/// The name of a short.win instruction by its Anchor discriminator.
fn shortwin_name(data: &[u8]) -> Option<&'static str> {
    use shortwin::instruction as i;
    let d = data.get(..8)?;
    let table: [(&[u8], &str); 12] = [
        (i::InitializeConfig::DISCRIMINATOR, "initialize_config"),
        (i::ProposeConfig::DISCRIMINATOR, "propose_config"),
        (i::ApplyConfig::DISCRIMINATOR, "apply_config"),
        (i::CancelConfig::DISCRIMINATOR, "cancel_config"),
        (i::SetPause::DISCRIMINATOR, "set_pause"),
        (i::SetCoveragePause::DISCRIMINATOR, "set_coverage_pause"),
        (i::DisableVenue::DISCRIMINATOR, "disable_venue"),
        (i::RevalidateVenue::DISCRIMINATOR, "revalidate_venue"),
        (i::SetRouteBroken::DISCRIMINATOR, "set_route_broken"),
        (i::SetVenueExtras::DISCRIMINATOR, "set_venue_extras"),
        (i::SweepProtocolFees::DISCRIMINATOR, "sweep_protocol_fees"),
        (i::FlagVenueUpgraded::DISCRIMINATOR, "flag_venue_upgraded"),
    ];
    table.iter().find(|(disc, _)| *disc == d).map(|(_, n)| *n)
}

/// `$ty`'s fields as `(name, value)`, by an exhaustive destructuring: a field added to the
/// program's type fails this crate's build instead of going missing from a review.
macro_rules! fields {
    ($ty:ident, $v:expr; $($f:ident),* $(,)?) => {{
        let $ty { $($f),* } = $v;
        vec![$((stringify!($f), u64::from($f))),*]
    }};
}

/// `Params`' fields as `(name, value)` (every one, in declaration order).
pub fn params_fields(p: &Params) -> Vec<(&'static str, u64)> {
    fields!(Params, *p;
        settle_keeper_fixed, settle_keeper_cap, expiry_keeper_fixed, expiry_keeper_cap,
        min_notional, market_max_notional, market_creation_fee, boost_dust_lamports,
        exec_rent_tol, theta_a_usd_e6, theta_b_usd_e6, pool_min_real_sol,
        open_fee_bps, rent_protocol_bps, penalty_lender_bps, penalty_protocol_bps,
        penalty_keeper_bps, expiry_fee_bps, settle_keeper_bps, expiry_keeper_bps, util_cap_bps,
        wallet_cap_bps, curve_oi_gamma_bps, fee_size_cap_x100, fee_oi_cap_x100, s3_wait_s,
        grad_grace_s, max_migration_pause_s, boost_window_s, open_ema_dev_bps, market_warmup_s,
        ema_force_delay_s, settle_persist_n, settle_arm_gap_s, fv_dev_bps, lump_vest_s,
        lock_grace_s, oracle_max_age_s, oracle_stale_s, oracle_max_conf_bps, oracle_max_step_bps,
        venue_fee_cap_bps, venue_fee_hard_cap_bps, pool_lock_min_bps, pool_switch_margin_bps,
        pool_list_window_s, pool_list_margin_bps, pool_fast_route_s, pool_fast_min_bps,
        pool_fast_window_s,
        kappa_liq_bps, kappa_base_bps, delta_liq_bps, delta_exp_bps, exec_tol_bps,
        ref_max_age_s, liq_persist_n, kappa_growth_n,
    )
}

/// `TierParams`' fields as `(name, value)`.
pub fn tier_fields(t: &TierParams) -> Vec<(&'static str, u64)> {
    fields!(TierParams, *t;
        m_bps, cr_open_bps, cr_liq_bps, penalty_bps, max_duration_s, impact_pos_bps,
        impact_mkt_bps, max_liq_slip_bps, rate_base_bps, rate_slope1_bps, rate_slope2_bps,
        min_rent_s,
    )
}

/// `EmaParamsArgs`' fields as `(name, value)`.
pub fn ema_fields(e: &EmaParamsArgs) -> Vec<(&'static str, u64)> {
    fields!(EmaParamsArgs, *e; ema_dtmax_s, ema_clamp_bps, liq_slow_tau_s, liq_slow_dtmax_s)
}

/// The tiers' names (`tiers[i]`).
pub const TIER_NAMES: [&str; 4] = ["A", "B", "C", "D"];

fn list(fields: &[(&str, u64)]) -> String {
    fields
        .iter()
        .map(|(n, v)| format!("{n}: {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A one-line description of a `ConfigChange`, every value it would set included (the
/// `feed_id` in hex: the program does not check it against the account, so a review is the
/// only check of it).
pub fn describe_change(change: &ConfigChange) -> String {
    match change {
        ConfigChange::Params { params, tiers } => {
            let tiers: Vec<String> = TIER_NAMES
                .iter()
                .zip(tiers)
                .map(|(n, t)| format!("{n} {{ {} }}", list(&tier_fields(t))))
                .collect();
            format!(
                "Params {{ {}, tiers: [{}] }}",
                list(&params_fields(params)),
                tiers.join(", ")
            )
        }
        ConfigChange::EmaParams { ema } => format!("EmaParams {{ {} }}", list(&ema_fields(ema))),
        ConfigChange::Kink { util_kink_bps } => {
            format!("Kink {{ util_kink_bps: {util_kink_bps} }}")
        }
        ConfigChange::Authorities {
            governance,
            pause_authority,
            coverage_authority,
            fee_recipient,
        } => format!(
            "Authorities {{ governance: {governance}, pause_authority: {pause_authority}, \
             coverage_authority: {coverage_authority}, fee_recipient: {fee_recipient} }}"
        ),
        ConfigChange::Timelock { timelock_s } => format!("Timelock {{ timelock_s: {timelock_s} }}"),
        ConfigChange::EnableVenues { bits } => {
            format!(
                "EnableVenues {{ bits: {bits:#b} ({}) }}",
                venue_names(*bits)
            )
        }
        ConfigChange::OracleFeed { account, feed_id } => format!(
            "OracleFeed {{ account: {account}, feed_id: {} }}",
            hex(feed_id)
        ),
    }
}

/// The `ConfigChange` of a `propose_config` instruction (`None` for anything else).
pub fn proposed_change(ix: &Instruction) -> Option<ConfigChange> {
    if ix.program_id != shortwin::ID || shortwin_name(&ix.data) != Some("propose_config") {
        return None;
    }
    shortwin::instruction::ProposeConfig::try_from_slice(ix.data.get(8..)?)
        .ok()
        .map(|p| p.change)
}

/// What `change` would move against the live config `c`, one `name: old -> new` per field
/// (empty when it changes nothing; `Authorities` are listed by `stage-authorities` itself).
pub fn change_diff(change: &ConfigChange, c: &GlobalConfig) -> Vec<String> {
    let diff = |prefix: &str, old: &[(&str, u64)], new: &[(&str, u64)]| -> Vec<String> {
        old.iter()
            .zip(new)
            .filter(|(o, n)| o.1 != n.1)
            .map(|((name, o), (_, n))| format!("{prefix}{name}: {o} -> {n}"))
            .collect()
    };
    match change {
        ConfigChange::Params { params, tiers } => {
            let mut out = diff("", &params_fields(&c.params), &params_fields(params));
            for ((name, old), new) in TIER_NAMES.iter().zip(&c.tiers).zip(tiers) {
                out.extend(diff(
                    &format!("tier {name} "),
                    &tier_fields(old),
                    &tier_fields(new),
                ));
            }
            out
        }
        ConfigChange::EmaParams { ema } => {
            diff("", &ema_fields(&c.ema_params.values()), &ema_fields(ema))
        }
        ConfigChange::Kink { util_kink_bps } => diff(
            "",
            &[("util_kink_bps", u64::from(c.kink.util_kink_bps))],
            &[("util_kink_bps", u64::from(*util_kink_bps))],
        ),
        ConfigChange::Timelock { timelock_s } => diff(
            "",
            &[("timelock_s", u64::from(c.timelock_s))],
            &[("timelock_s", u64::from(*timelock_s))],
        ),
        ConfigChange::EnableVenues { bits } => vec![format!(
            "venue_enabled: {:#b} -> {:#b}",
            c.venue_enabled,
            c.venue_enabled | bits
        )],
        ConfigChange::OracleFeed { account, feed_id } => {
            let mut out = Vec::new();
            if *account != c.pyth_sol_usd {
                out.push(format!("pyth_sol_usd: {} -> {account}", c.pyth_sol_usd));
            }
            if *feed_id != c.pyth_feed_id {
                out.push(format!(
                    "pyth_feed_id: {} -> {}",
                    hex(&c.pyth_feed_id),
                    hex(feed_id)
                ));
            }
            out
        }
        ConfigChange::Authorities { .. } => Vec::new(),
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `[config] venues` names of `bits` (`VENUE_BIT_*`).
fn venue_names(bits: u32) -> String {
    let names: Vec<&str> = crate::config::VENUES
        .iter()
        .filter(|(_, b)| bits & b != 0)
        .map(|(n, _)| *n)
        .collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(" ")
    }
}

/// A `VenueKind`'s name.
fn venue_kind_name(v: u8) -> String {
    [
        VenueKind::PumpCurve,
        VenueKind::PumpSwap,
        VenueKind::RaydiumV4,
        VenueKind::RaydiumCpmm,
    ]
    .iter()
    .find(|k| **k as u8 == v)
    .map_or_else(|| format!("venue kind {v}"), |k| format!("{k:?}"))
}

/// An Anchor optional account: `None` is passed as the program's own id.
fn optional(ix: &Instruction, n: usize) -> Option<Pubkey> {
    ix.accounts
        .get(n)
        .map(|a| a.pubkey)
        .filter(|k| *k != shortwin::ID)
}

fn scope_text(scope: PauseScope, market: Option<Pubkey>) -> String {
    match (scope, market) {
        (PauseScope::Global, None) => "scope Global".into(),
        (PauseScope::Market, Some(m)) => format!("scope Market {m}"),
        (s, m) => format!(
            "scope {s:?} with market account {} (MISMATCHED: the program refuses it)",
            m.map_or("(none)".into(), |k| k.to_string())
        ),
    }
}

/// The decoded arguments (and the accounts that matter) of a short.win admin instruction.
fn shortwin_detail(name: &str, ix: &Instruction) -> Option<String> {
    use shortwin::instruction as i;
    let args = ix.data.get(8..)?;
    Some(match name {
        "propose_config" => describe_change(&i::ProposeConfig::try_from_slice(args).ok()?.change),
        "set_pause" => {
            let a = i::SetPause::try_from_slice(args).ok()?;
            format!(
                "{}, paused {}",
                scope_text(a.scope, optional(ix, 2)),
                a.paused
            )
        }
        "set_route_broken" => {
            let a = i::SetRouteBroken::try_from_slice(args).ok()?;
            format!(
                "{}, venue_bit {} ({}), broken {}",
                scope_text(a.scope, optional(ix, 2)),
                a.venue_bit,
                venue_names(1u32.checked_shl(a.venue_bit.into()).unwrap_or(0)),
                a.broken
            )
        }
        "disable_venue" => {
            let a = i::DisableVenue::try_from_slice(args).ok()?;
            format!(
                "bit {} ({})",
                a.bit,
                venue_names(1u32.checked_shl(a.bit.into()).unwrap_or(0))
            )
        }
        "revalidate_venue" => {
            let a = i::RevalidateVenue::try_from_slice(args).ok()?;
            let hash: String = a
                .expected_elf_hash
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!(
                "venue-guard index {}, expected_slot {}, trimmed-ELF sha256 {hash}, ProgramData {}",
                a.venue,
                a.expected_slot,
                ix.accounts.get(3)?.pubkey
            )
        }
        "set_coverage_pause" => {
            let a = i::SetCoveragePause::try_from_slice(args).ok()?;
            format!(
                "market {}, paused {}, reason {}",
                ix.accounts.get(2)?.pubkey,
                a.paused,
                a.reason
            )
        }
        "set_venue_extras" => {
            let a = i::SetVenueExtras::try_from_slice(args).ok()?;
            let which = match a.ix {
                0 => "sell".to_string(),
                1 => "buy".to_string(),
                2 => "claim".to_string(),
                n => format!("ix {n}"),
            };
            let entries: Vec<String> = a
                .entries
                .iter()
                .map(|e| {
                    let w = if e.writable { " writable" } else { "" };
                    match e.kind {
                        1 => format!("fixed {}{w}", e.key),
                        2 => format!(
                            "PDA of {}{w} from [{}]",
                            e.key,
                            e.seeds
                                .iter()
                                .map(|p| match p.kind {
                                    1 => hex(&p.bytes[..usize::from(p.len).min(32)]),
                                    2 => format!("<placeholder {}>", p.len),
                                    k => format!("<seed kind {k}>"),
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        k => format!("kind {k} {}{w}", e.key),
                    }
                })
                .collect();
            format!(
                "{} {which}, {} entries: {}",
                venue_kind_name(a.venue),
                a.entries.len(),
                entries.join("; ")
            )
        }
        _ => return None,
    })
}

/// `ix`'s accounts with their flags (`w` writable, `s` signer).
fn accounts_text(ix: &Instruction) -> String {
    ix.accounts
        .iter()
        .map(|a| {
            let flags = match (a.is_writable, a.is_signer) {
                (true, true) => " (w, s)",
                (true, false) => " (w)",
                (false, true) => " (s)",
                (false, false) => "",
            };
            format!("{}{flags}", a.pubkey)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What `ix` does, in one line (module docs): the instruction's name for short.win, the loader,
/// the System program and Squads, its decoded arguments and the accounts that matter for a
/// review; for any other program, its data and every account with its flags.
pub fn describe(ix: &Instruction) -> String {
    let keys = |n: usize| {
        ix.accounts
            .get(n)
            .map(|a| a.pubkey.to_string())
            .unwrap_or_default()
    };
    if ix.program_id == shortwin::ID {
        let name = shortwin_name(&ix.data).unwrap_or("(unknown short.win instruction)");
        let detail = match name {
            "propose_config" | "set_pause" | "set_route_broken" | "disable_venue"
            | "revalidate_venue" | "set_coverage_pause" | "set_venue_extras" => {
                shortwin_detail(name, ix).unwrap_or_else(|| {
                    format!(
                        "(UNDECODABLE arguments {})",
                        hex(ix.data.get(8..).unwrap_or_default())
                    )
                })
            }
            "cancel_config" | "apply_config" => String::new(),
            _ => format!(
                "data {}; accounts {}",
                hex(ix.data.get(8..).unwrap_or_default()),
                accounts_text(ix)
            ),
        };
        return format!("short.win {name} (signer {}) {detail}", keys(0))
            .trim_end()
            .to_string();
    }
    if ix.program_id == loader::ID {
        let tag = ix
            .data
            .get(..4)
            .map(|t| u32::from_le_bytes([t[0], t[1], t[2], t[3]]));
        return match tag {
            Some(7) => format!(
                "loader SetAuthorityChecked: {} from {} to {}",
                keys(0),
                keys(1),
                keys(2)
            ),
            Some(4) => format!(
                "loader SetAuthority (UNCHECKED) of {}; accounts {}",
                keys(0),
                accounts_text(ix)
            ),
            Some(3) => format!(
                "loader Upgrade: program {} from buffer {} (spill {}, authority {})",
                keys(1),
                keys(2),
                keys(3),
                keys(6)
            ),
            Some(6) => format!("loader ExtendProgram of {}", keys(0)),
            _ => format!(
                "loader instruction {}; accounts {}",
                hex(&ix.data),
                accounts_text(ix)
            ),
        };
    }
    if ix.program_id == anchor_lang::system_program::ID && ix.data.get(..4) == Some(&[2, 0, 0, 0]) {
        let lamports = ix
            .data
            .get(4..12)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_le_bytes)
            .unwrap_or_default();
        return format!(
            "system transfer {lamports} lamports {} -> {}",
            keys(0),
            keys(1)
        );
    }
    format!(
        "program {} data {}; accounts {}",
        ix.program_id,
        hex(&ix.data),
        accounts_text(ix)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_governance_instructions() {
        let g = Pubkey::new_unique();
        let change = ConfigChange::Timelock { timelock_s: 90_000 };
        let d = describe(&propose_config(&g, change));
        assert!(
            d.contains("propose_config") && d.contains("timelock_s: 90000"),
            "{d}"
        );
        let pd = loader::program_data(&shortwin::ID);
        let d = describe(&loader::set_authority_checked(
            &pd,
            &g,
            &Pubkey::new_unique(),
        ));
        assert!(d.starts_with("loader SetAuthorityChecked") && d.contains(&pd.to_string()));
        let d = describe(&set_pause(&g, None, true));
        assert!(d.starts_with("short.win set_pause"), "{d}");
        assert!(describe(&system_transfer(&g, &pd, 5)).contains("5 lamports"));
    }

    #[test]
    fn a_review_shows_every_value_a_change_sets() {
        let g = Pubkey::new_unique();
        let feed = Pubkey::new_unique();
        let mut feed_id = [0u8; 32];
        feed_id[0] = 0xab;
        feed_id[31] = 0xcd;
        let d = describe(&propose_config(
            &g,
            ConfigChange::OracleFeed {
                account: feed,
                feed_id,
            },
        ));
        let want = format!("account: {feed}, feed_id: ab{}cd", "00".repeat(30));
        assert!(d.contains(&want), "{d}");

        let mut params = Params::defaults();
        params.open_fee_bps = 1_234;
        let mut tiers = TierParams::defaults();
        tiers[2].cr_liq_bps = 4_321;
        let d = describe_change(&ConfigChange::Params { params, tiers });
        assert!(d.contains("open_fee_bps: 1234") && d.contains("kappa_growth_n: "));
        assert!(
            d.contains("C { m_bps: ") && d.contains("cr_liq_bps: 4321"),
            "{d}"
        );
        assert_eq!(params_fields(&params).len(), 58);
        assert_eq!(tier_fields(&tiers[0]).len(), 12);
        let ema = EmaParamsArgs {
            ema_dtmax_s: 61,
            ema_clamp_bps: 62,
            liq_slow_tau_s: 63,
            liq_slow_dtmax_s: 64,
        };
        assert_eq!(
            describe_change(&ConfigChange::EmaParams { ema }),
            "EmaParams { ema_dtmax_s: 61, ema_clamp_bps: 62, liq_slow_tau_s: 63, \
             liq_slow_dtmax_s: 64 }"
        );

        // Against the live config: only what moves.
        let mut c: GlobalConfig = bytemuck::Zeroable::zeroed();
        c.params = Params::defaults();
        c.tiers = TierParams::defaults();
        let default_fee = Params::defaults().open_fee_bps;
        assert_eq!(
            change_diff(&ConfigChange::Params { params, tiers }, &c),
            vec![
                format!("open_fee_bps: {default_fee} -> 1234"),
                format!(
                    "tier C cr_liq_bps: {} -> 4321",
                    TierParams::defaults()[2].cr_liq_bps
                ),
            ]
        );
        assert_eq!(
            change_diff(&ConfigChange::Kink { util_kink_bps: 9 }, &c),
            vec!["util_kink_bps: 0 -> 9".to_string()]
        );
        assert_eq!(
            change_diff(
                &ConfigChange::OracleFeed {
                    account: feed,
                    feed_id
                },
                &c
            )
            .len(),
            2
        );
    }

    #[test]
    fn a_review_decodes_the_admin_arguments_and_accounts() {
        let a = Pubkey::new_unique();
        let market = Pubkey::new_unique();
        let d = describe(&set_pause(&a, Some(market), true));
        assert!(
            d.ends_with(&format!("scope Market {market}, paused true")),
            "{d}"
        );
        assert!(describe(&set_pause(&a, None, false)).ends_with("scope Global, paused false"));
        let d = describe(&set_route_broken(&a, None, 3, true));
        assert!(
            d.ends_with("scope Global, venue_bit 3 (raydium_cpmm), broken true"),
            "{d}"
        );
        let d = describe(&disable_venue(&a, 0));
        assert!(d.ends_with("bit 0 (pump_curve)"), "{d}");
        let d = describe(&revalidate_venue(&a, 2, 77, [0xab; 32]));
        assert!(d.contains("venue-guard index 2, expected_slot 77"), "{d}");
        assert!(
            d.contains(&format!("trimmed-ELF sha256 {}", "ab".repeat(32))),
            "{d}"
        );
        let d = describe(&clear_coverage_pause(&a, &market));
        assert!(d.contains(&format!("market {market}, paused false")), "{d}");
        let prog = Pubkey::new_unique();
        let fixed = Pubkey::new_unique();
        let mut bytes = [0u8; 32];
        bytes[..2].copy_from_slice(&[7, 8]);
        let entries = vec![
            ExtraEntryArg {
                kind: 1,
                writable: true,
                key: fixed,
                seeds: vec![],
            },
            ExtraEntryArg {
                kind: 2,
                writable: false,
                key: prog,
                seeds: vec![
                    shortwin::instructions::set_venue_extras::SeedPartArg {
                        kind: 1,
                        len: 2,
                        bytes,
                    },
                    shortwin::instructions::set_venue_extras::SeedPartArg {
                        kind: 2,
                        len: 1,
                        bytes: [0; 32],
                    },
                ],
            },
        ];
        let d = describe(&set_venue_extras(&a, VenueKind::PumpSwap as u8, 1, entries));
        assert!(
            d.ends_with(&format!(
                "PumpSwap buy, 2 entries: fixed {fixed} writable; PDA of {prog} from [0708, \
                 <placeholder 1>]"
            )),
            "{d}"
        );

        // Any other program: its data and every account with its flags.
        let other = Instruction {
            program_id: prog,
            accounts: vec![
                AccountMeta::new(fixed, false),
                AccountMeta::new_readonly(a, true),
            ],
            data: vec![1, 2],
        };
        assert_eq!(
            describe(&other),
            format!("program {prog} data 0102; accounts {fixed} (w), {a} (s)")
        );
    }

    #[test]
    fn reads_a_buffer_authority() {
        let a = Pubkey::new_unique();
        let mut data = vec![1, 0, 0, 0, 1];
        data.extend_from_slice(a.as_ref());
        data.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        assert_eq!(loader::buffer_authority(&loader::ID, &data), Some(Some(a)));
        data[4] = 0;
        assert_eq!(loader::buffer_authority(&loader::ID, &data), Some(None));
        assert_eq!(loader::buffer_authority(&Pubkey::default(), &data), None);
        data[0] = 3;
        assert_eq!(loader::buffer_authority(&loader::ID, &data), None);
    }
}
