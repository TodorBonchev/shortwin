//! The venue drift monitor (ARCHITECTURE §7.1 duty 10, §3.1 "Venue-upgrade latch", §5.5 venue
//! admin powers): what the keeper watches besides prices, as pure functions the keeper loop
//! ([`crate::keeper`]) feeds each tick.
//!
//! **Program upgrades → `flag_venue_upgraded`.** The keeper reads the header (the first 45
//! bytes: tag, deploy slot, upgrade authority) of the six venue ProgramData accounts the
//! `VenueGuard` latch indexes — pump, pAMM, pfee, AMM v4, CPMM, Jupiter
//! ([`shortwin::constants::VENUE_PROGRAM_DATA`]) — and of Token-2022's (alert only), and
//! compares them with the latch. [`latch_action`] is the program's own rule
//! (`flag_venue_upgraded`): with the venue's bit clear, a deploy slot other than `guard_slot`
//! is an upgrade to flag; with the bit set, a deploy slot other than `flagged_slot` is a second
//! upgrade during review, whose flag refreshes the record. Anything else is not an upgrade (the
//! instruction would fail with `VenueNotUpgraded`, writing nothing). The keeper sends the flag
//! in the slot after it sees the change, at the exit priority ceiling, alerts, and pages the
//! Emergency multisig to run the fork suite against the new binary and `revalidate_venue` —
//! both Raydium programs share one latch mask, so an upgrade of either blocks opens on every
//! Raydium market (ARCH §3.1, R4B-7).
//!
//! **An extension is not an upgrade (AIR-01).** The loader's `ExtendProgram` is permissionless
//! (Agave 4.2): it appends zeros, keeps the ELF and writes the current slot into the header, so
//! the header alone cannot tell it from an `Upgrade`. The program tells them apart for the venues
//! whose ProgramData can still grow (pump-fees, AMM v4, CPMM, Jupiter) once `revalidate_venue`
//! recorded a fingerprint (`VenueGuard::guard_len` / `guard_elf`): with the bit clear, a moved
//! slot is an upgrade if the account kept the recorded length or its trimmed ELF's sha256 no
//! longer starts with the fingerprint (or is too large to hash), and a grown account with the
//! reviewed ELF is `VenueNotUpgraded`. The keeper mirrors it: [`latch_action`] answers
//! [`LatchAction::Inspect`] when only the whole account can decide. Then the flag's simulation
//! decides, before the keeper's exits — the program's rule, so an upgrade is flagged in the slot
//! it is seen and an extension answers `VenueNotUpgraded`: nothing flagged, nothing paged. That
//! answer counts only from a bank that holds the deploy the header showed (the simulation's
//! context slot at or past it): a node behind the header read still holds the deploy before, and
//! answers `VenueNotUpgraded` for an upgrade too. When that simulation cannot judge (it fails
//! otherwise, ran on such a bank, or short.win itself is not deployed in that slot, AIR-18),
//! [`latch_action_with`] decides on the whole account's [`ElfFacts`] with the program's own hash
//! (`trimmed_elf_hash_of`,
//! `elf_fingerprint_of`) — [`LatchAction::Extended`] for an extension. Without a fingerprint
//! (pump and PumpSwap, which sit at the 10 MiB cap, and any venue before its first revalidation
//! with a hash) the slot alone decides, as on chain. While the bit is set the program refreshes
//! on any later slot; the keeper refreshes and pages only when the account no longer holds the
//! deploy it reported ([`ElfFacts::is_upgrade_since`] against its own record of that deploy,
//! read whole after it reports it or, after a restart, when it first finds the review), so an
//! extension of the unreviewed ELF during review is not another upgrade either. Token-2022 (alert
//! only) has no latch record: the keeper judges its change by the same rule against the deploy it
//! last read whole ([`upgraded_since`]), so a stranger's extension of it is not paged either. The
//! keeper reads these accounts whole only after its exits and cranks (they reach 10 MiB), where
//! nothing waits on them: opens are blocked during a review, and Token-2022 is alert only.
//!
//! **Pool admin actions.** The Raydium pool admin (`GThUX1…`, a 2-of-3 Squads v3 vault with no
//! timelock, R-RAY §5) can, per AMM v4 pool, disable swaps (`SetParams` status 2–5, or 7 with a
//! future open time) or set its swap fee to anything below 100 % (`SetParams` fees); per CPMM
//! pool, disable swaps (`update_pool_status` bit 2); and per CPMM **config**, raise the trade and
//! creator fees of every pool on it at once (`update_amm_config`) — or lower them to 0, which
//! stops every buy of a pool whose creator fee is on the buy's input (`ZeroTradingTokens`; the
//! program's reader calls it not executable, and the alert's lost route says why, M3-SEC-1). A polling keeper watches the
//! accounts those instructions write — each market's adopted pool and, for CPMM, its
//! `AmmConfig` (both are in the ReadSet it reads every tick) — and reports every change of the
//! swap route, the status field, the open time and the one-way fee ([`PoolSnapshot`],
//! [`diff`]), with the instruction that writes that field and whether the new fee crosses the
//! opens cap (`venue_fee_cap_bps`: opens refused) or the market's S3-chain hard cap
//! (`venue_fee_hard_cap_bps`, or the lowest cap an open position locked when lower — AIR-19's
//! `min_open_hard_cap_bps`, [`FeeCaps::of_market`]: `s3_no_route`, the S3 chain). Every field
//! is read with the program's own readers
//! (`shortwin_venue_state::raydium_v4` / `raydium_cpmm`), so a halted pool still reports its
//! fee and a pool that stops parsing (a layout change, "not executable" per A63) is reported as
//! such. The duties themselves need no extra action: the program's route predicates already
//! put a halted market on duty 5's no-route cadence and its S3 chain ([`crate::decide`]).
//!
//! **PumpSwap** (M6; ARCH §5.3, §5.5; A24). PumpSwap's `GlobalConfig.admin` halts every pool at
//! once with `disable` (`GlobalConfig.disable_flags`, no timelock). The buy bit
//! (`PAMM_BUY_DISABLED_BIT`, bit 3; T-DISABLE) makes the venue not executable — a lost route,
//! S3 evidence; **any other non-zero value** blocks opens only (`VenueFlagged`) and is not S3
//! evidence, and ARCH has the keeper alert it: every change of the flags is reported
//! ([`AdminChange::PammFlags`], severe while non-zero). The pool's fee moves with its market-cap
//! tier (like the curve's), so a PumpSwap fee is reported only when it crosses the opens cap or
//! the hard cap.

use anchor_lang::prelude::Pubkey;
use shortwin::constants::{
    TOKEN_2022_PROGRAM_ID, VENUE_PROGRAM_DATA, VENUE_PROGRAM_IDS, VENUE_PROGRAMS,
};
use shortwin::instructions::flag_venue_upgraded::trimmed_elf_hash_of;
use shortwin::state::{GUARD_ELF_LEN, Market, VenueGuard, VenueKind, elf_fingerprint_of};
use shortwin_services_common::AccountData;
use shortwin_venue_state::{program_data, raydium_cpmm, raydium_v4};

use crate::view::MarketView;

/// Bytes of a ProgramData account the watcher reads (tag, deploy slot, upgrade authority).
pub const HEADER_LEN: usize = program_data::HEADER_LEN;

/// An upgradeable program whose ProgramData the keeper watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchedProgram {
    pub program: Pubkey,
    pub program_data: Pubkey,
    /// Its `VenueGuard` index (`flag_venue_upgraded(venue)`), or `None`: alert only
    /// (Token-2022).
    pub guard: Option<u8>,
}

/// The upgradeable loader's ProgramData address of `program`.
pub fn program_data_address(program: &Pubkey) -> Pubkey {
    let loader = Pubkey::new_from_array(program_data::LOADER_UPGRADEABLE);
    Pubkey::find_program_address(&[program.as_ref()], &loader).0
}

/// The six venue programs of the latch, in `VenueGuard` order, then Token-2022 (alert only;
/// ARCH §7.1 duty 10).
pub fn watched_programs() -> Vec<WatchedProgram> {
    let mut v: Vec<WatchedProgram> = VENUE_PROGRAM_IDS
        .iter()
        .zip(VENUE_PROGRAM_DATA.iter())
        .enumerate()
        .map(|(i, (program, pd))| WatchedProgram {
            program: *program,
            program_data: *pd,
            guard: u8::try_from(i).ok(),
        })
        .collect();
    v.push(WatchedProgram {
        program: TOKEN_2022_PROGRAM_ID,
        program_data: program_data_address(&TOKEN_2022_PROGRAM_ID),
        guard: None,
    });
    v
}

/// The deploy slot in a ProgramData header (`None` if missing, not owned by the upgradeable
/// loader, not tag 3 or too short: the same check as `flag_venue_upgraded`).
pub fn deploy_slot(acc: Option<&AccountData>) -> Option<u64> {
    let acc = acc?;
    program_data::deploy_slot(&acc.owner.to_bytes(), &acc.data).ok()
}

/// What the latch needs for one venue program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatchAction {
    /// Nothing: the deploy slot is the validated one (bit clear) or the flagged one (bit set).
    Nothing,
    /// The bit is clear and the deploy slot is not `guard_slot`: an upgrade to flag.
    Flag,
    /// The bit is set and the deploy slot is not `flagged_slot`: a second upgrade during
    /// review, whose flag refreshes `flagged_slot` / `flagged_at`.
    Refresh,
    /// AIR-01: the bit is clear, the deploy slot is not `guard_slot` and the guard holds an ELF
    /// fingerprint of the venue (`VenueGuard::elf_fingerprint`): the header cannot tell an
    /// `Upgrade` from an `ExtendProgram`, so the whole ProgramData decides — the flag's own
    /// simulation, or [`latch_action_with`] on the account read whole.
    Inspect,
    /// AIR-01: the ProgramData moved its deploy slot by growing while it still holds the ELF the
    /// latch knows — an extension, not an upgrade (with the bit clear `flag_venue_upgraded`
    /// answers `VenueNotUpgraded`): nothing to flag or page.
    Extended,
}

/// The program's `flag_venue_upgraded` rule for venue `venue` at ProgramData `deploy_slot`, as
/// far as the header can tell ([module docs](self)): [`LatchAction::Inspect`] where the program
/// reads the rest of the account (AIR-01).
pub fn latch_action(guard: &VenueGuard, venue: u8, deploy_slot: u64) -> LatchAction {
    let i = usize::from(venue);
    if i >= VENUE_PROGRAMS {
        return LatchAction::Nothing;
    }
    if guard.is_flagged(venue) {
        if guard.flagged_slot.get(i) == Some(&deploy_slot) {
            LatchAction::Nothing
        } else {
            LatchAction::Refresh
        }
    } else if guard.guard_slot.get(i) == Some(&deploy_slot) {
        LatchAction::Nothing
    } else if guard.elf_fingerprint(venue).is_some() {
        LatchAction::Inspect
    } else {
        LatchAction::Flag
    }
}

/// What the AIR-01 rule reads of a whole ProgramData account: its data length and the
/// `VenueGuard` fingerprint (the first `GUARD_ELF_LEN` bytes) of its trimmed ELF's sha256 —
/// the program's own `trimmed_elf_hash_of` and `elf_fingerprint_of`, so `None` above
/// `ELF_HASH_MAX_LEN` (not hashed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElfFacts {
    /// The account's data length (`None` past `u32`, which no ProgramData reaches).
    pub len: Option<u32>,
    pub fingerprint: Option<[u8; GUARD_ELF_LEN]>,
}

impl ElfFacts {
    /// The facts of a fetched ProgramData account (the whole account, not a header slice);
    /// `None` when its header does not parse.
    pub fn of(acc: &AccountData) -> Option<Self> {
        let hash = trimmed_elf_hash_of(&acc.owner.to_bytes(), &acc.data).ok()?;
        Some(Self {
            len: u32::try_from(acc.data.len()).ok(),
            fingerprint: hash.map(|h| elf_fingerprint_of(&h)),
        })
    }

    /// The program's `is_upgrade` (`flag_venue_upgraded`, AIR-01): whether a ProgramData with
    /// these facts, whose deploy slot moved since the deploy recorded as `(len, fingerprint)`,
    /// holds an upgrade — it kept the recorded length (only `Upgrade` moves the slot without
    /// resizing the account; `ExtendProgram` always grows it), or its trimmed ELF no longer
    /// matches the fingerprint (or is too large to hash: fail-safe).
    pub fn is_upgrade_since(&self, (len, fingerprint): (u32, [u8; GUARD_ELF_LEN])) -> bool {
        self.len == Some(len) || self.fingerprint != Some(fingerprint)
    }

    /// The `(len, fingerprint)` record of this deploy — what `revalidate_venue` naming its
    /// trimmed-ELF hash records — or `None` when its ELF is not hashed.
    pub fn record(&self) -> Option<(u32, [u8; GUARD_ELF_LEN])> {
        Some((self.len?, self.fingerprint?))
    }
}

/// AIR-01 for a watched program without a latch index (Token-2022, alert only): whether its
/// ProgramData, whose deploy slot moved since the keeper read it whole holding `before`, now
/// holding `after`, was upgraded — the program's rule ([`ElfFacts::is_upgrade_since`]) against
/// the keeper's own read; without a record of `before` (its ELF not hashed), an upgrade
/// (fail-safe).
pub fn upgraded_since(before: &ElfFacts, after: &ElfFacts) -> bool {
    before
        .record()
        .is_none_or(|reported| after.is_upgrade_since(reported))
}

/// The program's `flag_venue_upgraded` rule for venue `venue` on the whole ProgramData at
/// `deploy_slot` with `facts` (AIR-01): [`latch_action`], an [`LatchAction::Inspect`] resolved
/// against the guard's fingerprint — [`LatchAction::Flag`] for an upgrade,
/// [`LatchAction::Extended`] for an extension.
pub fn latch_action_with(
    guard: &VenueGuard,
    venue: u8,
    deploy_slot: u64,
    facts: &ElfFacts,
) -> LatchAction {
    match latch_action(guard, venue, deploy_slot) {
        LatchAction::Inspect => match guard.elf_fingerprint(venue) {
            Some(fp) if !facts.is_upgrade_since(fp) => LatchAction::Extended,
            _ => LatchAction::Flag,
        },
        a => a,
    }
}

/// A pool's admin-controlled state as the keeper saw it ([module docs](self)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolSnapshot {
    /// The program's `venue_executable` (the reader's predicate on the whole ReadSet).
    pub executable: bool,
    /// The pool account parses (a Raydium venue; the mock's state is taken from its reader).
    pub parses: bool,
    /// AMM v4 `status` @0, CPMM `status` @329.
    pub status: Option<u64>,
    /// AMM v4 `pool_open_time` @224, CPMM `open_time` @373.
    pub open_time: Option<u64>,
    /// The live one-way fee φ in ppm, read even while the pool is halted (AMM v4
    /// `⌈num·10⁶/den⌉`, CPMM `t + c` of its config).
    pub fee_ppm: Option<u32>,
    /// The CPMM config the pool names.
    pub config: Option<Pubkey>,
    /// CPMM: whether a buy (WSOL in) can split its fee on the pool's config
    /// (`PoolState::fee_splits`); `Some(false)`: the trade and effective creator fee are 0 with
    /// the creator fee on the buy's input, so every buy reverts with `ZeroTradingTokens` and the
    /// route is lost although the status is clear (M3-SEC-1). `None` elsewhere.
    pub buy_fee_splits: Option<bool>,
}

fn parse_v4(acc: Option<&AccountData>) -> Option<raydium_v4::AmmInfo> {
    let a = acc?;
    raydium_v4::read(&a.owner.to_bytes(), &a.data).ok()
}

fn parse_cpmm(
    pool: Option<&AccountData>,
    config: Option<&AccountData>,
) -> Option<(raydium_cpmm::PoolState, Option<raydium_cpmm::AmmConfig>)> {
    let p = pool?;
    let state = raydium_cpmm::read_pool(&p.owner.to_bytes(), &p.data).ok()?;
    let cfg = config.and_then(|c| raydium_cpmm::read_config(&c.owner.to_bytes(), &c.data).ok());
    Some((state, cfg))
}

/// The snapshot of `mv`'s pool from this tick's ReadSet accounts.
pub fn snapshot(mv: &MarketView) -> PoolSnapshot {
    let acc = |i: usize| mv.read_accounts.get(i).and_then(Option::as_ref);
    let executable = mv.state.executable;
    match mv.kind {
        VenueKind::RaydiumV4 => match parse_v4(acc(0)) {
            Some(info) => PoolSnapshot {
                executable,
                parses: true,
                status: Some(info.status),
                open_time: Some(info.pool_open_time),
                fee_ppm: info.fee_ppm(),
                config: None,
                buy_fee_splits: None,
            },
            None => PoolSnapshot::unreadable(executable),
        },
        VenueKind::RaydiumCpmm => match parse_cpmm(acc(0), acc(1)) {
            Some((state, cfg)) => PoolSnapshot {
                executable,
                parses: true,
                status: Some(u64::from(state.status)),
                open_time: Some(state.open_time),
                fee_ppm: cfg.as_ref().and_then(|c| state.fee_ppm(c)),
                config: Some(Pubkey::new_from_array(state.amm_config)),
                buy_fee_splits: cfg.as_ref().and_then(|c| {
                    state
                        .sol_is_token0()
                        .and_then(|sol0| state.fee_splits(c, sol0))
                }),
            },
            None => PoolSnapshot::unreadable(executable),
        },
        // PumpSwap: `status` carries `GlobalConfig.disable_flags`; the fee is the one resolved on
        // the live pool, also while a halt makes it not executable.
        VenueKind::PumpSwap => match mv.state.pool {
            Some(p) => PoolSnapshot {
                executable,
                parses: true,
                status: Some(u64::from(p.disable_flags)),
                open_time: None,
                fee_ppm: p.fees.map(|f| f.oneway_ppm()),
                config: None,
                buy_fee_splits: None,
            },
            None => PoolSnapshot::unreadable(executable),
        },
        _ => PoolSnapshot {
            executable,
            parses: true,
            status: None,
            open_time: None,
            fee_ppm: executable.then_some(mv.state.fee_ppm),
            config: None,
            buy_fee_splits: None,
        },
    }
}

impl PoolSnapshot {
    fn unreadable(executable: bool) -> Self {
        Self {
            executable,
            parses: false,
            status: None,
            open_time: None,
            fee_ppm: None,
            config: None,
            buy_fee_splits: None,
        }
    }
}

/// The fee caps a fee change is measured against, in ppm (the config's `venue_fee_cap_bps` and
/// the S3 chain's hard cap × 100; ARCH §4.2 "Units").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeCaps {
    pub open_ppm: u64,
    /// The hard cap the S3 chain judges the live fee against: `venue_fee_hard_cap_bps`
    /// ([`FeeCaps::of`]), or on a market the lowest cap an open position locked when lower
    /// ([`FeeCaps::of_market`], AIR-19).
    pub hard_ppm: u64,
}

impl FeeCaps {
    /// From the global config's parameters.
    pub fn of(config: &shortwin::state::GlobalConfig) -> Self {
        Self {
            open_ppm: u64::from(config.params.venue_fee_cap_bps).saturating_mul(100),
            hard_ppm: u64::from(config.params.venue_fee_hard_cap_bps).saturating_mul(100),
        }
    }

    /// The caps of `market` (AIR-19): the opens cap, and the hard cap its S3 chain runs at — the
    /// global `venue_fee_hard_cap_bps`, or `Market.min_open_hard_cap_bps` (the lowest cap a
    /// position open since the market last emptied locked) when lower: the program's
    /// `engine::route::chain_hard_cap_bps`, which its prologue's `s3_no_route` takes.
    pub fn of_market(config: &shortwin::state::GlobalConfig, market: &Market) -> Self {
        Self {
            hard_ppm: shortwin::engine::route::chain_hard_cap_bps(config, market)
                .saturating_mul(100),
            ..Self::of(config)
        }
    }
}

/// One observed change of a pool's admin-controlled state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminChange {
    /// The venue can no longer execute (`venue_executable` turned false): swaps disabled by the
    /// admin, an open time in the future, a vault below its fee counters, or the pool no longer
    /// parses. Positions freeze; liquidatable and expired ones reach S3 through the chain.
    RouteLost { detail: String },
    /// The venue executes again.
    RouteRestored { detail: String },
    /// The status field changed (`via`: the admin instruction that writes it).
    StatusChanged {
        from: Option<u64>,
        to: Option<u64>,
        via: &'static str,
    },
    /// The open time changed (AMM v4 `SetParams` param 3; CPMM cannot change it).
    OpenTimeChanged {
        from: Option<u64>,
        to: Option<u64>,
        via: &'static str,
    },
    /// The one-way fee changed; `from` is `None` at first sight of a pool whose fee is already
    /// above the opens cap.
    FeeChanged {
        from_ppm: Option<u32>,
        to_ppm: u32,
        /// Above `venue_fee_cap_bps`: opens are refused.
        above_open_cap: bool,
        /// Above the market's S3-chain hard cap — `venue_fee_hard_cap_bps`, or the lowest cap an
        /// open position locked when lower (AIR-19, [`FeeCaps::of_market`]): `s3_no_route` (the
        /// S3 chain; risk quotes stay at the locked cap, and a position whose own cap the fee
        /// exceeds becomes S3-due).
        above_hard_cap: bool,
        via: &'static str,
    },
    /// The pool account no longer parses under the reader (a layout change): not executable
    /// (A63).
    Unreadable,
    /// PumpSwap's `GlobalConfig.disable_flags` changed (`from` `None` at first sight of a
    /// non-zero value; the admin's `disable`, every pool at once). Any non-zero value blocks opens
    /// (`VenueFlagged`, A24); `buys_halted`: the buy bit (T-DISABLE bit 3) is set — no route, S3
    /// evidence.
    PammFlags {
        from: Option<u8>,
        to: u8,
        buys_halted: bool,
    },
}

impl AdminChange {
    /// A change that pages (a lost route, a fee above a cap, an unreadable pool).
    pub fn is_severe(&self) -> bool {
        match self {
            Self::RouteLost { .. } | Self::Unreadable => true,
            Self::PammFlags { to, .. } => *to != 0,
            Self::FeeChanged {
                above_open_cap,
                above_hard_cap,
                ..
            } => *above_open_cap || *above_hard_cap,
            _ => false,
        }
    }
}

fn status_via(kind: VenueKind) -> &'static str {
    match kind {
        VenueKind::RaydiumV4 => "AMM v4 SetParams (param 0, status)",
        VenueKind::RaydiumCpmm => "CPMM update_pool_status",
        _ => "venue admin",
    }
}

fn fee_via(kind: VenueKind) -> &'static str {
    match kind {
        VenueKind::RaydiumV4 => "AMM v4 SetParams (param 2, fees)",
        VenueKind::RaydiumCpmm => "CPMM update_amm_config (every pool on the config)",
        VenueKind::PumpSwap => {
            "the pool's market-cap fee tier (pump-fees FeeConfig, the pool's creator fee)"
        }
        _ => "venue admin",
    }
}

/// `GlobalConfig.disable_flags` of a PumpSwap snapshot.
fn pamm_flags(s: &PoolSnapshot) -> Option<u8> {
    s.status.and_then(|v| u8::try_from(v).ok())
}

fn pamm_flags_change(from: Option<u8>, to: u8) -> AdminChange {
    AdminChange::PammFlags {
        from,
        to,
        buys_halted: to & shortwin::venues::pump_amm::PAMM_BUY_DISABLED_BIT != 0,
    }
}

/// 0 below the opens cap, 1 above it, 2 above the hard cap.
fn fee_level(fee_ppm: Option<u32>, caps: FeeCaps) -> u8 {
    match fee_ppm.map(u64::from) {
        Some(f) if f > caps.hard_ppm => 2,
        Some(f) if f > caps.open_ppm => 1,
        _ => 0,
    }
}

/// Why a pool is (not) executable, for an alert.
fn route_detail(kind: VenueKind, s: &PoolSnapshot) -> String {
    if !s.parses {
        return "the pool account no longer parses (layout change, A63)".into();
    }
    match (kind, s.status) {
        (VenueKind::PumpSwap, Some(f)) => format!(
            "PumpSwap GlobalConfig.disable_flags {f:#010b}{}",
            if f & u64::from(shortwin::venues::pump_amm::PAMM_BUY_DISABLED_BIT) != 0 {
                " (bit 3: every buy halts — no route, S3 evidence)"
            } else if f != 0 {
                " (opens blocked, VenueFlagged; not S3 evidence)"
            } else {
                ""
            }
        ),
        (VenueKind::RaydiumV4, Some(st)) => {
            let name = match st {
                raydium_v4::STATUS_INITIALIZED => "1 Initialized",
                raydium_v4::STATUS_DISABLED => "2 Disabled",
                raydium_v4::STATUS_WITHDRAW_ONLY => "3 WithdrawOnly",
                raydium_v4::STATUS_LIQUIDITY_ONLY => "4 LiquidityOnly",
                raydium_v4::STATUS_ORDERBOOK_ONLY => "5 OrderBookOnly",
                raydium_v4::STATUS_SWAP_ONLY => "6 SwapOnly",
                raydium_v4::STATUS_WAITING_TRADE => "7 WaitingTrade",
                _ => "unknown (reverts in the venue)",
            };
            format!(
                "AMM v4 status {name}, open time {}",
                s.open_time.unwrap_or(0)
            )
        }
        (VenueKind::RaydiumCpmm, Some(st)) => format!(
            "CPMM status {st:#05b} (swap bit {}), open time {}{}",
            if st & u64::from(raydium_cpmm::STATUS_BIT_SWAP) != 0 {
                "set: swaps disabled"
            } else {
                "clear"
            },
            s.open_time.unwrap_or(0),
            if s.buy_fee_splits == Some(false) {
                ", buys revert: trade + creator fee 0 with the creator fee on the buy's input \
                 (ZeroTradingTokens; CPMM update_amm_config)"
            } else {
                ""
            }
        ),
        _ => format!("venue_executable = {}", s.executable),
    }
}

fn fee_change(kind: VenueKind, from_ppm: Option<u32>, to_ppm: u32, caps: FeeCaps) -> AdminChange {
    AdminChange::FeeChanged {
        from_ppm,
        to_ppm,
        above_open_cap: u64::from(to_ppm) > caps.open_ppm,
        above_hard_cap: u64::from(to_ppm) > caps.hard_ppm,
        via: fee_via(kind),
    }
}

/// What to report at the first sight of a pool: only what is already abnormal (no route, a fee
/// above the opens cap).
pub fn first_sight(kind: VenueKind, new: &PoolSnapshot, caps: FeeCaps) -> Vec<AdminChange> {
    let mut out = Vec::new();
    if !new.parses
        && matches!(
            kind,
            VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm | VenueKind::PumpSwap
        )
    {
        out.push(AdminChange::Unreadable);
    }
    if kind == VenueKind::PumpSwap
        && let Some(f) = pamm_flags(new)
        && f != 0
    {
        out.push(pamm_flags_change(None, f));
    }
    if !new.executable {
        out.push(AdminChange::RouteLost {
            detail: route_detail(kind, new),
        });
    }
    if let Some(f) = new.fee_ppm
        && u64::from(f) > caps.open_ppm
    {
        out.push(fee_change(kind, None, f, caps));
    }
    out
}

/// The changes from `old` to `new` ([module docs](self)). `last_fee` is the last readable fee
/// (a halted mock or an unreadable pool has none; its return does not count as a change).
pub fn diff(
    kind: VenueKind,
    old: &PoolSnapshot,
    new: &PoolSnapshot,
    last_fee: Option<u32>,
    caps: FeeCaps,
) -> Vec<AdminChange> {
    let mut out = Vec::new();
    let pamm = kind == VenueKind::PumpSwap;
    let raydium = matches!(kind, VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm);
    if (raydium || pamm) && old.parses && !new.parses {
        out.push(AdminChange::Unreadable);
    }
    if old.executable && !new.executable {
        out.push(AdminChange::RouteLost {
            detail: route_detail(kind, new),
        });
    } else if !old.executable && new.executable {
        out.push(AdminChange::RouteRestored {
            detail: route_detail(kind, new),
        });
    }
    if pamm {
        if new.parses
            && let Some(to) = pamm_flags(new)
            && pamm_flags(old) != Some(to)
            && (old.parses || to != 0)
        {
            out.push(pamm_flags_change(pamm_flags(old), to));
        }
        // The fee moves with the pool's market-cap tier: reported when it crosses a cap.
        if let Some(f) = new.fee_ppm
            && fee_level(new.fee_ppm, caps) != fee_level(last_fee, caps)
        {
            out.push(fee_change(kind, last_fee, f, caps));
        }
        return out;
    }
    if new.parses && old.parses && old.status != new.status {
        out.push(AdminChange::StatusChanged {
            from: old.status,
            to: new.status,
            via: status_via(kind),
        });
    }
    if new.parses && old.parses && old.open_time != new.open_time {
        out.push(AdminChange::OpenTimeChanged {
            from: old.open_time,
            to: new.open_time,
            via: "AMM v4 SetParams (param 3, open time)",
        });
    }
    if let Some(f) = new.fee_ppm
        && last_fee.is_some_and(|l| l != f)
    {
        out.push(fee_change(kind, last_fee, f, caps));
    }
    out
}

/// Per-market monitor state.
#[derive(Clone, Debug, Default)]
pub struct PoolWatch {
    /// The pool watched. A market that adopts another pool (M4, [`crate::pool_monitor`]) starts
    /// a new watch: the new pool's first sight, never a "change" of the old one.
    pub pool: Option<Pubkey>,
    pub last: Option<PoolSnapshot>,
    /// The last readable fee.
    pub last_fee: Option<u32>,
}

impl PoolWatch {
    /// Records `new` and returns what changed since the last tick ([`first_sight`] the first
    /// time).
    pub fn observe(
        &mut self,
        kind: VenueKind,
        new: PoolSnapshot,
        caps: FeeCaps,
    ) -> Vec<AdminChange> {
        let out = match &self.last {
            None => first_sight(kind, &new, caps),
            Some(old) => diff(kind, old, &new, self.last_fee, caps),
        };
        if new.fee_ppm.is_some() {
            self.last_fee = new.fee_ppm;
        }
        self.last = Some(new);
        out
    }
}

// ---- pump curves (pump-final §7) ------------------------------------------------------------

/// What duty 10 reports about a pump curve market (pump-final §7, §6.1.5): pump's post-complete
/// fields and the invariant they rely on, read with the program's own reader
/// (`shortwin_venue_state::pump`) and priced with the program's own math
/// (`shortwin_math::quote::migrated_pool`, the market's `CurvePricingInputs`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CurveFindings {
    /// A live curve whose `vQ − rQ ≠ ivqr` (`BondingCurve::quote_offset_consistent` false):
    /// `(vQ, rQ, ivqr)`.
    pub quote_offset_inconsistent: Option<(u64, u64, u64)>,
    /// A live curve carrying post-complete fields (G-3: not executable, A63's cap-only risk
    /// view): `(pcB, pcQ)`.
    pub live_post_complete: Option<(u64, u64)>,
    /// A complete, unmigrated curve whose post-complete fields are not known (§6.1.5: FV
    /// undefined, an S3 gives the lenders the whole take): `(pcB, pcQ)`.
    pub migrating_fv_undefined: Option<(u64, u64)>,
    /// On a complete, unmigrated curve with known fields: `ρ − 1` in bps
    /// ([`rho_minus_one_bps`] on §5.4's `(X₀, Y₀)` and `(X₁, Y₁)`; 0 without a pool part).
    pub synthetic_completion_delta_bps: Option<i64>,
    /// On a complete, unmigrated curve: `A + pcB − 206.9e12`, the tokens donated to
    /// `curve_base_ata` (`A` its live balance, `rT = 0`).
    pub curve_base_ata_donation: Option<i128>,
}

/// `ρ − 1` in bps, `⌊Y₁·X₀·10⁴ / (X₁·Y₀)⌋ − 10⁴` (pump-final §5.4: `ρ = P(X₁,Y₁)/P(X₀,Y₀)`, the
/// price of the pool `migrate_v2` will create over the same curve's without its pool part or a
/// donation). `None` on a zero denominator or an overflow.
pub fn rho_minus_one_bps(x0: u128, y0: u128, x1: u128, y1: u128) -> Option<i64> {
    let num = y1.checked_mul(x0)?.checked_mul(10_000)?;
    let den = x1.checked_mul(y0)?;
    let ratio = num.checked_div(den)?;
    i64::try_from(ratio).ok()?.checked_sub(10_000)
}

/// [`CurveFindings`] of a `PumpCurve` or `PumpMigrating` market (`None` on any other venue or
/// when its curve does not parse). A live curve is checked for pump's invariant and for
/// post-complete fields; a complete, unmigrated one for known post-complete fields, its `ρ` and
/// the donation to `curve_base_ata`; a migrated one (`vT = 0`) reports nothing.
pub fn curve_findings(mv: &MarketView) -> Option<CurveFindings> {
    use shortwin_math::quote::{MigrationParams, STANDARD_POOL_TOKENS, migrated_pool};
    if !matches!(mv.kind, VenueKind::PumpCurve | VenueKind::PumpMigrating) {
        return None;
    }
    let acc = mv
        .read_accounts
        .get(shortwin::venues::pump_curve::C_CURVE)?
        .as_ref()?;
    let curve = shortwin_venue_state::pump::read_curve(&acc.owner.to_bytes(), &acc.data).ok()?;
    let mut f = CurveFindings::default();
    let (pcb, pcq) = (curve.post_complete_base_out, curve.post_complete_quote_in);
    if !curve.complete {
        if !curve.quote_offset_consistent() {
            f.quote_offset_inconsistent = Some((
                curve.virtual_quote_reserves,
                curve.real_quote_reserves,
                curve.initial_virtual_quote_reserves,
            ));
        }
        if pcb != 0 || pcq != 0 {
            f.live_post_complete = Some((pcb, pcq));
        }
        return Some(f);
    }
    let Some(c) = mv.state.curve.as_ref().filter(|c| c.migrating()) else {
        return Some(f);
    };
    let Some(inputs) = c.pricing.as_ref() else {
        return Some(f);
    };
    f.curve_base_ata_donation = Some(
        i128::from(inputs.live_pool_tokens) + i128::from(pcb)
            - i128::try_from(STANDARD_POOL_TOKENS).unwrap_or(i128::MAX),
    );
    if !inputs.post_complete_known() {
        f.migrating_fv_undefined = Some((pcb, pcq));
        return Some(f);
    }
    let m = inputs.curve_pricing().migration;
    let base = MigrationParams {
        migration_fee: m.migration_fee,
        pool_tokens: STANDARD_POOL_TOKENS,
        post_complete_quote_in: 0,
    };
    f.synthetic_completion_delta_bps =
        match (migrated_pool(&c.live, &base), migrated_pool(&c.live, &m)) {
            (Some((x0, y0)), Some((x1, y1))) => rho_minus_one_bps(x0, y0, x1, y1),
            _ => None,
        };
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shortwin::constants::{
        GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4, RAYDIUM_V4_PROGRAM_ID, VENUE_GUARD_SLOTS_2026_10_13,
    };

    /// The latch as the 2026-10-13 re-pin baselines it (`VENUE_GUARD_SLOTS_2026_10_13`;
    /// pump-final §7: duty 10's baseline).
    fn guard() -> VenueGuard {
        let mut g: VenueGuard = bytemuck::Zeroable::zeroed();
        g.guard_slot = VENUE_GUARD_SLOTS_2026_10_13;
        g
    }

    /// pump-final §5.4: `ρ − 1` on 98u8RJ's synthetic completion (V4: `(X₀, Y₀)` = (206.9e12,
    /// 84,990,359,189), `(X₁, Y₁)` = (200,661,149,081,575, 87,632,834,741), ρ = 1.063149…) is
    /// 631 bps; without a pool part 0; a donation alone (`X₁` above 206.9e12, `Y₁ = Y₀`) is
    /// negative; zero reserves give nothing.
    #[test]
    fn rho_minus_one_is_the_post_complete_pools_price_over_the_curves() {
        let (x0, y0) = (206_900_000_000_000u128, 84_990_359_189u128);
        let (x1, y1) = (200_661_149_081_575u128, 87_632_834_741u128);
        assert_eq!(rho_minus_one_bps(x0, y0, x1, y1), Some(631));
        assert_eq!(rho_minus_one_bps(x0, y0, x0, y0), Some(0));
        assert_eq!(
            rho_minus_one_bps(x0, y0, x0 + 1_000_000_000_000, y0),
            Some(-49)
        );
        assert_eq!(rho_minus_one_bps(x0, y0, 0, y1), None);
        assert_eq!(rho_minus_one_bps(x0, 0, x1, y1), None);
    }

    /// A 166-byte pump curve with the given reserves and post-complete fields (the reader's
    /// offsets), as a market's ReadSet C0 on `kind`.
    fn curve_market(
        kind: VenueKind,
        complete: bool,
        (vq, rq, ivqr): (u64, u64, u64),
        (pcb, pcq): (u64, u64),
    ) -> MarketView {
        use shortwin_venue_state::pump;
        let mut d = vec![0u8; pump::CURVE_LEN_2026_10_08];
        d[..8].copy_from_slice(&pump::BONDING_CURVE_DISCRIMINATOR);
        d[8..16].copy_from_slice(&500_000_000_000_000u64.to_le_bytes());
        d[16..24].copy_from_slice(&vq.to_le_bytes());
        d[24..32].copy_from_slice(&100_000_000_000_000u64.to_le_bytes());
        d[32..40].copy_from_slice(&rq.to_le_bytes());
        d[48] = u8::from(complete);
        let at = |d: &mut Vec<u8>, o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
        at(&mut d, pump::OFF_INITIAL_VIRTUAL_QUOTE_RESERVES, ivqr);
        at(&mut d, pump::OFF_POST_COMPLETE_BASE_OUT, pcb);
        at(&mut d, pump::OFF_POST_COMPLETE_QUOTE_IN, pcq);
        let m: Market = bytemuck::Zeroable::zeroed();
        let mut read = vec![None; shortwin::venues::pump_curve::READ_LEN];
        read[shortwin::venues::pump_curve::C_CURVE] = Some(AccountData {
            lamports: 1,
            owner: shortwin::constants::PUMP_PROGRAM_ID,
            executable: false,
            data: d,
        });
        MarketView {
            key: Pubkey::new_unique(),
            market: Box::new(m),
            vault: Box::new(shortwin_math::pool::Vault::new()),
            kind,
            read_keys: Vec::new(),
            read_accounts: read,
            state: shortwin::venues::VenueState::not_executable(kind),
            positions: Vec::new(),
            extras: None,
            supply: None,
        }
    }

    /// Duty 10 on live curves (pump-final §7): pump's invariant `vQ − rQ = ivqr` (vacuous for
    /// `ivqr = 0`) and post-complete fields on a live curve (G-3) are reported; a curve without
    /// either reports nothing, as does another venue.
    #[test]
    fn duty_10_reports_a_live_curves_broken_invariant_and_post_complete_fields() {
        let k = VenueKind::PumpCurve;
        let ok = curve_market(
            k,
            false,
            (40_000_000_000, 10_000_000_000, 30_000_000_000),
            (0, 0),
        );
        assert_eq!(curve_findings(&ok), Some(CurveFindings::default()));
        let legacy = curve_market(k, false, (40_000_000_000, 10_000_000_000, 0), (0, 0));
        assert_eq!(curve_findings(&legacy), Some(CurveFindings::default()));
        let broken = curve_market(
            k,
            false,
            (40_000_000_000, 10_000_000_001, 30_000_000_000),
            (0, 0),
        );
        assert_eq!(
            curve_findings(&broken).unwrap().quote_offset_inconsistent,
            Some((40_000_000_000, 10_000_000_001, 30_000_000_000))
        );
        let pc = curve_market(
            k,
            false,
            (40_000_000_000, 10_000_000_000, 30_000_000_000),
            (5, 0),
        );
        let f = curve_findings(&pc).unwrap();
        assert_eq!(f.live_post_complete, Some((5, 0)));
        assert_eq!(f.quote_offset_inconsistent, None);
        // A complete curve's fields are not a live curve's: its state has no pricing here (the
        // migrating findings need the program's state), so nothing is reported.
        let done = curve_market(
            VenueKind::PumpMigrating,
            true,
            (40_000_000_000, 1, 30_000_000_000),
            (5, 0),
        );
        assert_eq!(curve_findings(&done), Some(CurveFindings::default()));
        let mut other = ok.clone();
        other.kind = VenueKind::PumpSwap;
        assert_eq!(curve_findings(&other), None);
        let mut unread = ok;
        unread.read_accounts = Vec::new();
        assert_eq!(curve_findings(&unread), None);
    }

    #[test]
    fn the_watched_programs_are_the_latch_order_then_token_2022() {
        let w = watched_programs();
        assert_eq!(w.len(), VENUE_PROGRAMS + 1);
        for (i, p) in w.iter().take(VENUE_PROGRAMS).enumerate() {
            assert_eq!(p.guard, Some(i as u8));
            assert_eq!(p.program_data, VENUE_PROGRAM_DATA[i]);
            assert_eq!(program_data_address(&p.program), p.program_data);
        }
        assert_eq!(
            w[usize::from(GUARD_RAYDIUM_V4)].program,
            RAYDIUM_V4_PROGRAM_ID
        );
        let t22 = w.last().unwrap();
        assert_eq!((t22.program, t22.guard), (TOKEN_2022_PROGRAM_ID, None));
    }

    /// The program's rule, all four cases (ARCH §3.1; T-LATCH's host twin).
    #[test]
    fn latch_actions_follow_flag_venue_upgraded() {
        let mut g = guard();
        let v = GUARD_RAYDIUM_CPMM;
        let slot = g.guard_slot[usize::from(v)];
        assert_eq!(latch_action(&g, v, slot), LatchAction::Nothing);
        assert_eq!(latch_action(&g, v, slot + 777), LatchAction::Flag);
        // Flagged at slot + 777.
        g.upgraded |= 1 << v;
        g.flagged_slot[usize::from(v)] = slot + 777;
        assert_eq!(latch_action(&g, v, slot + 777), LatchAction::Nothing);
        assert_eq!(latch_action(&g, v, slot + 999), LatchAction::Refresh);
        // Even a return to the validated slot is a change while flagged (the program refreshes).
        assert_eq!(latch_action(&g, v, slot), LatchAction::Refresh);
        // Other venues are independent; an out-of-range index is never acted on.
        let o = GUARD_RAYDIUM_V4;
        assert_eq!(
            latch_action(&g, o, g.guard_slot[usize::from(o)]),
            LatchAction::Nothing
        );
        assert_eq!(latch_action(&g, 6, 1), LatchAction::Nothing);
    }

    /// A ProgramData account at `slot` holding `elf`, zero-padded to `len` data bytes.
    fn program_data_account(slot: u64, elf: &[u8], len: usize) -> AccountData {
        let mut data = program_data::encode_header(slot, Some(&[7u8; 32])).to_vec();
        data.extend_from_slice(elf);
        data.resize(len, 0);
        AccountData {
            lamports: 1,
            owner: Pubkey::new_from_array(program_data::LOADER_UPGRADEABLE),
            executable: false,
            data,
        }
    }

    fn facts(acc: &AccountData) -> ElfFacts {
        ElfFacts::of(acc).expect("a ProgramData")
    }

    /// AIR-01, the program's `flag_venue_upgraded` rule on whole accounts: once CPMM was
    /// revalidated naming its trimmed ELF's sha256 (the guard records the length and the
    /// fingerprint), the loader's `ExtendProgram` — the slot moved, the account grew, the ELF
    /// kept — is no upgrade, every changed ELF is (in place, in an extended account, a larger one,
    /// one too large to hash), and so is a redeploy in place (the length decides). Without a
    /// fingerprint, and on pump (no fingerprint slot), the slot alone decides. During a review the
    /// keeper's own record of the deploy it reported tells an extension of it from another
    /// upgrade, and Token-2022's last whole read its extension from its upgrade.
    #[test]
    fn air01_an_extension_is_not_an_upgrade_and_every_changed_elf_is() {
        use shortwin::constants::GUARD_PUMP;
        use shortwin::instructions::flag_venue_upgraded::ELF_HASH_MAX_LEN;

        let v = GUARD_RAYDIUM_CPMM;
        let i = usize::from(v);
        let elf: Vec<u8> = (0..5_000u32).map(|k| (k % 251) as u8 | 1).collect();
        let s0 = 452_376_460;
        // The deploy's own zero tail past the ELF.
        let len0 = program_data::HEADER_LEN + elf.len() + 300;
        let reviewed = program_data_account(s0, &elf, len0);
        let hash = trimmed_elf_hash_of(&reviewed.owner.to_bytes(), &reviewed.data)
            .unwrap()
            .unwrap();
        // What `solana-verify get-program-hash` prints: the sha256 of the ELF without its tail.
        assert_eq!(hash, solana_sha256_hasher::hashv(&[&elf]).to_bytes());
        let mut g = guard();
        g.guard_slot[i] = s0;
        let ext = program_data_account(s0 + 10, &elf, len0 + 10_240);
        // Before a revalidation with a hash the slot alone decides (an extension is flagged, as
        // on chain: the ops runbook revalidates every growable venue after `initialize_config`).
        assert_eq!(latch_action(&g, v, s0 + 10), LatchAction::Flag);
        assert_eq!(
            latch_action_with(&g, v, s0 + 10, &facts(&ext)),
            LatchAction::Flag
        );
        g.set_elf_fingerprint(
            v,
            Some((u32::try_from(len0).unwrap(), elf_fingerprint_of(&hash))),
        );
        assert_eq!(latch_action(&g, v, s0), LatchAction::Nothing);
        assert_eq!(latch_action(&g, v, s0 + 10), LatchAction::Inspect);
        assert_eq!(
            latch_action_with(&g, v, s0 + 10, &facts(&ext)),
            LatchAction::Extended
        );
        let ext2 = program_data_account(s0 + 20, &elf, len0 + 20_480);
        assert_eq!(
            latch_action_with(&g, v, s0 + 20, &facts(&ext2)),
            LatchAction::Extended,
            "a second extension"
        );
        assert_eq!(
            latch_action_with(&g, v, s0, &facts(&ext)),
            LatchAction::Nothing,
            "the validated slot"
        );
        let upgrade = |elf: &[u8], len: usize| {
            latch_action_with(
                &g,
                v,
                s0 + 30,
                &facts(&program_data_account(s0 + 30, elf, len)),
            )
        };
        let mut changed = elf.clone();
        changed[17] ^= 0x40;
        assert_eq!(upgrade(&changed, len0), LatchAction::Flag, "in place");
        assert_eq!(
            upgrade(&elf, len0),
            LatchAction::Flag,
            "redeployed in place"
        );
        assert_eq!(
            upgrade(&changed, len0 + 10_240),
            LatchAction::Flag,
            "after an extension"
        );
        let mut larger = elf.clone();
        larger.extend(std::iter::repeat_n(3u8, 8_000));
        assert_eq!(
            upgrade(&larger, len0 + 10_240),
            LatchAction::Flag,
            "a larger ELF in the extension"
        );
        let huge = vec![1u8; ELF_HASH_MAX_LEN + 1];
        let big = program_data_account(s0 + 40, &huge, program_data::HEADER_LEN + huge.len() + 64);
        assert_eq!(facts(&big).fingerprint, None, "not hashed");
        assert_eq!(
            latch_action_with(&g, v, s0 + 40, &facts(&big)),
            LatchAction::Flag,
            "too large to hash: an upgrade (fail-safe)"
        );
        // Pump: no fingerprint slot.
        let p = usize::from(GUARD_PUMP);
        assert_eq!(
            latch_action(&g, GUARD_PUMP, g.guard_slot[p] + 1),
            LatchAction::Flag
        );
        // Flagged at the in-place upgrade: the program refreshes on any later slot; against the
        // keeper's record of that deploy an extension of it is no new upgrade, another ELF is.
        g.upgraded |= 1 << v;
        g.flagged_slot[i] = s0 + 30;
        assert_eq!(latch_action(&g, v, s0 + 50), LatchAction::Refresh);
        let reported = facts(&program_data_account(s0 + 30, &changed, len0))
            .record()
            .unwrap();
        assert!(
            !facts(&program_data_account(s0 + 50, &changed, len0 + 10_240))
                .is_upgrade_since(reported)
        );
        assert!(
            facts(&program_data_account(s0 + 50, &elf, len0 + 10_240)).is_upgrade_since(reported)
        );
        assert_eq!(facts(&big).record(), None, "no record without a hash");
        // Token-2022 (no latch record): the same rule against the keeper's last whole read.
        let t22 = facts(&program_data_account(s0, &elf, len0));
        assert!(!upgraded_since(
            &t22,
            &facts(&program_data_account(s0 + 60, &elf, len0 + 10_240))
        ));
        assert!(upgraded_since(
            &t22,
            &facts(&program_data_account(s0 + 60, &changed, len0 + 10_240))
        ));
        assert!(
            upgraded_since(&t22, &facts(&program_data_account(s0 + 60, &elf, len0))),
            "redeployed in place"
        );
        assert!(
            upgraded_since(&facts(&big), &facts(&big)),
            "no record of the last read: an upgrade"
        );
        // A header that does not parse has no facts.
        let mut bad = ext;
        bad.data[0] = 2;
        assert_eq!(ElfFacts::of(&bad), None);
    }

    #[test]
    fn deploy_slots_parse_only_real_program_data_headers() {
        let loader = Pubkey::new_from_array(program_data::LOADER_UPGRADEABLE);
        let acc = |owner, data: Vec<u8>| AccountData {
            lamports: 1,
            owner,
            executable: false,
            data,
        };
        let header = program_data::encode_header(445_763_204, None).to_vec();
        assert_eq!(
            deploy_slot(Some(&acc(loader, header.clone()))),
            Some(445_763_204)
        );
        assert_eq!(
            deploy_slot(Some(&acc(Pubkey::new_unique(), header.clone()))),
            None
        );
        let mut bad = header;
        bad[0] = 2; // tag Program, not ProgramData
        assert_eq!(deploy_slot(Some(&acc(loader, bad))), None);
        assert_eq!(deploy_slot(None), None);
    }

    fn snap(executable: bool, status: u64, fee: u32) -> PoolSnapshot {
        PoolSnapshot {
            executable,
            parses: true,
            status: Some(status),
            open_time: Some(0),
            fee_ppm: Some(fee),
            config: None,
            buy_fee_splits: None,
        }
    }

    const CAPS: FeeCaps = FeeCaps {
        open_ppm: 15_000,
        hard_ppm: 50_000,
    };

    /// AIR-19: a market's S3 chain judges the live fee at the lower of the global hard cap and
    /// the lowest cap an open position locked (`min_open_hard_cap_bps`, 0 = unset), so a fee
    /// between the two starts the chain and is reported above the hard cap; against the global
    /// cap alone (the monitor before AIR-19) it was not.
    #[test]
    fn air19_the_hard_cap_is_the_markets_chain_cap() {
        let mut config: shortwin::state::GlobalConfig = bytemuck::Zeroable::zeroed();
        config.params.venue_fee_cap_bps = 150;
        config.params.venue_fee_hard_cap_bps = 1_000;
        let mut m: Market = bytemuck::Zeroable::zeroed();
        let global = FeeCaps {
            open_ppm: 15_000,
            hard_ppm: 100_000,
        };
        assert_eq!(FeeCaps::of(&config), global);
        assert_eq!(FeeCaps::of_market(&config, &m), global, "no mark");
        m.min_open_hard_cap_bps = 2_000;
        assert_eq!(FeeCaps::of_market(&config, &m), global, "a mark above");
        m.min_open_hard_cap_bps = 500;
        let caps = FeeCaps::of_market(&config, &m);
        assert_eq!(
            caps,
            FeeCaps {
                open_ppm: 15_000,
                hard_ppm: 50_000,
            }
        );
        // A CPMM fee raised to 8 %: above the chain's 5 %, under the global 10 %.
        let k = VenueKind::RaydiumCpmm;
        let mut w = PoolWatch::default();
        assert!(w.observe(k, snap(true, 0, 2_500), caps).is_empty());
        let c = w.observe(k, snap(true, 0, 80_000), caps);
        assert_eq!(
            c,
            vec![AdminChange::FeeChanged {
                from_ppm: Some(2_500),
                to_ppm: 80_000,
                above_open_cap: true,
                above_hard_cap: true,
                via: "CPMM update_amm_config (every pool on the config)",
            }]
        );
        assert!(matches!(
            fee_change(k, Some(2_500), 80_000, global),
            AdminChange::FeeChanged {
                above_hard_cap: false,
                ..
            }
        ));
    }

    #[test]
    fn a_halt_a_fee_hike_and_a_resume_are_reported_once_each() {
        let k = VenueKind::RaydiumV4;
        let mut w = PoolWatch::default();
        assert!(w.observe(k, snap(true, 1, 2_500), CAPS).is_empty());
        assert!(w.observe(k, snap(true, 1, 2_500), CAPS).is_empty());
        // SetParams status 2.
        let c = w.observe(k, snap(false, 2, 2_500), CAPS);
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(
            matches!(&c[0], AdminChange::RouteLost { detail } if detail.contains("2 Disabled"))
        );
        assert!(c[0].is_severe());
        assert!(matches!(
            c[1],
            AdminChange::StatusChanged {
                from: Some(1),
                to: Some(2),
                ..
            }
        ));
        // SetParams fees while halted: 3 % (above the 1.5 % opens cap, below the 5 % hard cap).
        let c = w.observe(k, snap(false, 2, 30_000), CAPS);
        assert_eq!(
            c,
            vec![AdminChange::FeeChanged {
                from_ppm: Some(2_500),
                to_ppm: 30_000,
                above_open_cap: true,
                above_hard_cap: false,
                via: "AMM v4 SetParams (param 2, fees)",
            }]
        );
        // Resume.
        let c = w.observe(k, snap(true, 1, 30_000), CAPS);
        assert!(matches!(c[0], AdminChange::RouteRestored { .. }), "{c:?}");
        assert!(!c[0].is_severe());
    }

    #[test]
    fn first_sight_reports_only_what_is_already_abnormal() {
        let k = VenueKind::RaydiumCpmm;
        assert!(first_sight(k, &snap(true, 0, 2_500), CAPS).is_empty());
        let c = first_sight(k, &snap(false, 4, 60_000), CAPS);
        assert!(
            matches!(&c[0], AdminChange::RouteLost { detail } if detail.contains("swaps disabled"))
        );
        assert!(matches!(
            c[1],
            AdminChange::FeeChanged {
                from_ppm: None,
                to_ppm: 60_000,
                above_open_cap: true,
                above_hard_cap: true,
                ..
            }
        ));
    }

    /// M3-SEC-1: `update_amm_config(0, 0)` on a pool whose creator fee is on the buy's input
    /// leaves the status clear but every buy reverts (`ZeroTradingTokens`): the program's reader
    /// calls it not executable, and the alert is a lost route that says why, besides the fee
    /// change to 0.
    #[test]
    fn a_zero_fee_route_loss_says_why() {
        let k = VenueKind::RaydiumCpmm;
        let mut w = PoolWatch::default();
        assert!(w.observe(k, snap(true, 0, 2_500), CAPS).is_empty());
        let zero = PoolSnapshot {
            buy_fee_splits: Some(false),
            ..snap(false, 0, 0)
        };
        let c = w.observe(k, zero, CAPS);
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(
            matches!(&c[0], AdminChange::RouteLost { detail }
                if detail.contains("swap bit clear") && detail.contains("ZeroTradingTokens")),
            "{c:?}"
        );
        assert!(c[0].is_severe());
        assert_eq!(
            c[1],
            AdminChange::FeeChanged {
                from_ppm: Some(2_500),
                to_ppm: 0,
                above_open_cap: false,
                above_hard_cap: false,
                via: "CPMM update_amm_config (every pool on the config)",
            }
        );
        // A halt's detail does not blame the fee.
        let halted = first_sight(k, &snap(false, 4, 2_500), CAPS);
        assert!(
            matches!(&halted[0], AdminChange::RouteLost { detail } if !detail.contains("ZeroTradingTokens")),
            "{halted:?}"
        );
    }

    #[test]
    fn a_pool_that_stops_parsing_is_unreadable_and_not_a_fee_change() {
        let k = VenueKind::RaydiumCpmm;
        let mut w = PoolWatch::default();
        w.observe(k, snap(true, 0, 2_500), CAPS);
        let c = w.observe(k, PoolSnapshot::unreadable(false), CAPS);
        assert_eq!(c[0], AdminChange::Unreadable);
        assert!(
            matches!(&c[1], AdminChange::RouteLost { detail } if detail.contains("no longer parses"))
        );
        assert_eq!(c.len(), 2, "{c:?}");
        // It parses again at the same fee: the route is back, the fee did not change.
        let c = w.observe(k, snap(true, 0, 2_500), CAPS);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(matches!(c[0], AdminChange::RouteRestored { .. }));
    }
}
