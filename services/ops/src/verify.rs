//! `verify`: derives every address of the layout and asserts it on chain, and prints the report.
//!
//! **FAIL** (the command exits 1):
//! - Squads: the program is executable and **immutable** (its ProgramData has no upgrade
//!   authority — the governance relies on Squads' semantics never changing under it);
//! - each multisig: the PDA of its create key exists, is a Squads `Multisig`, **autonomous** (no
//!   config authority), with the layout's threshold, time lock and members (each with every
//!   permission), its rent collector its own vault 0;
//! - `fee_recipient` (Governance vault 1): a System account, no data, at least its rent-exempt
//!   minimum (890,880 lamports);
//! - `GlobalConfig`: exists, current version, and its four authorities are exactly the layout's
//!   (`governance` = Governance vault 0, `pause_authority` = Emergency vault 0,
//!   `coverage_authority` = the configured key, `fee_recipient` = Governance vault 1), pairwise
//!   distinct; `timelock_s` is the layout's;
//! - every program of `[upgrade] programs`: its ProgramData's upgrade authority is Governance
//!   vault 0.
//!
//! **WARN** (reported, not failing): a pending config change (what it would set, and whether that
//! is the layout); every proposal that is `Active` or `Approved` and not executed, at **every**
//! index from 1 to the multisig's `transaction_index` (read [`PROPOSAL_CHUNK`] at a time) — an
//! Approved vault transaction never expires and survives member and threshold changes (T-SQ-7),
//! so stale approvals should be cancelled (`cancel`); `venue_enabled` differing from the layout's launch
//! set (Emergency `disable_venue` or a later `EnableVenues` change legitimately move it).

use anchor_lang::ZeroCopy;
use shortwin::constants::CONFIG_VERSION;
use shortwin::state::GlobalConfig;
use shortwin_services_common::{AccountData, Chain};
use solana_pubkey::Pubkey;
use squads_v4_ix::state::{Multisig, Proposal, ProposalStatus};
use squads_v4_ix::{self as sq, pda as squads_pda, permission};

use crate::config::{AUTHORITY_NAMES, Layout, MsLayout};
use crate::ixs::{self, loader};

/// The fee vault's pre-funding: the rent-exempt minimum of a 0-byte account.
pub const FEE_VAULT_PREFUND: u64 = 890_880;

/// How many proposals `verify` reads per request (`getMultipleAccounts` takes 100 keys).
pub const PROPOSAL_CHUNK: u64 = 100;

/// One line of the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

/// The report.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub lines: Vec<(Level, String)>,
}

impl Report {
    fn check(&mut self, ok: bool, what: impl Into<String>, detail: impl std::fmt::Display) {
        let level = if ok { Level::Ok } else { Level::Fail };
        self.lines
            .push((level, format!("{}: {detail}", what.into())));
    }

    fn eq<T: PartialEq + std::fmt::Display>(&mut self, what: impl Into<String>, got: T, want: T) {
        if got == want {
            self.check(true, what, got);
        } else {
            self.check(false, what, format!("{got}, expected {want}"));
        }
    }

    fn warn(&mut self, what: impl Into<String>) {
        self.lines.push((Level::Warn, what.into()));
    }

    /// No FAIL line.
    pub fn passed(&self) -> bool {
        self.lines.iter().all(|(l, _)| *l != Level::Fail)
    }

    pub fn count(&self, level: Level) -> usize {
        self.lines.iter().filter(|(l, _)| *l == level).count()
    }

    /// The lines with a FAIL.
    pub fn failures(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|(l, _)| *l == Level::Fail)
            .map(|(_, s)| s.as_str())
            .collect()
    }

    /// The printed report.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (l, line) in &self.lines {
            let tag = match l {
                Level::Ok => "ok  ",
                Level::Warn => "WARN",
                Level::Fail => "FAIL",
            };
            s.push_str(&format!("{tag} {line}\n"));
        }
        s.push_str(&format!(
            "verify: {} ({} ok, {} warnings, {} failures)\n",
            if self.passed() { "PASSED" } else { "FAILED" },
            self.count(Level::Ok),
            self.count(Level::Warn),
            self.count(Level::Fail)
        ));
        s
    }
}

/// Decodes a zero-copy short.win account (owner and discriminator checked).
pub(crate) fn decode_zc<T: ZeroCopy>(acc: &AccountData) -> Option<T> {
    if acc.owner != shortwin::ID || !acc.data.starts_with(T::DISCRIMINATOR) {
        return None;
    }
    let body = acc.data.get(8..8 + core::mem::size_of::<T>())?;
    Some(bytemuck::pod_read_unaligned(body))
}

/// The upgrade authority in a ProgramData account (`None` if not one).
pub fn upgrade_authority(acc: &AccountData) -> Option<Option<Pubkey>> {
    if acc.owner != loader::ID {
        return None;
    }
    let h = shortwin_venue_state::program_data::read(&acc.owner.to_bytes(), &acc.data).ok()?;
    Some(h.upgrade_authority.map(Pubkey::new_from_array))
}

/// The ProgramData deploy slot (`None` if not one).
pub fn deploy_slot(acc: &AccountData) -> Option<u64> {
    if acc.owner != loader::ID {
        return None;
    }
    Some(
        shortwin_venue_state::program_data::read(&acc.owner.to_bytes(), &acc.data)
            .ok()?
            .slot,
    )
}

fn multisig_checks(r: &mut Report, ms: &MsLayout, acc: Option<&AccountData>) -> Option<Multisig> {
    let what = format!("{} multisig {}", ms.name, ms.key);
    let Some(acc) = acc else {
        r.check(false, what, "missing (create-multisigs)");
        return None;
    };
    let Some(m) = (acc.owner == sq::ID)
        .then(|| Multisig::decode(&acc.data))
        .flatten()
    else {
        r.check(false, what, "not a Squads Multisig");
        return None;
    };
    r.check(true, &what, format!("create key {}", m.create_key));
    r.eq(
        format!("{} create key", ms.name),
        m.create_key,
        ms.create_key,
    );
    r.check(
        m.config_authority == Pubkey::default(),
        format!("{} autonomous (no config authority)", ms.name),
        m.config_authority,
    );
    r.eq(format!("{} threshold", ms.name), m.threshold, ms.threshold);
    r.eq(
        format!("{} time lock (s)", ms.name),
        m.time_lock,
        ms.time_lock,
    );
    let mut got: Vec<Pubkey> = m.members.iter().map(|x| x.key).collect();
    let mut want = ms.members.clone();
    got.sort();
    want.sort();
    r.check(
        got == want,
        format!("{} members", ms.name),
        format!("{} of {} as configured", m.members.len(), ms.members.len()),
    );
    if got != want {
        for k in got.iter().filter(|k| !want.contains(k)) {
            r.check(
                false,
                format!("{} member", ms.name),
                format!("{k} is not in the layout"),
            );
        }
        for k in want.iter().filter(|k| !got.contains(k)) {
            r.check(
                false,
                format!("{} member", ms.name),
                format!("{k} is missing"),
            );
        }
    }
    let partial: Vec<String> = m
        .members
        .iter()
        .filter(|x| x.permissions != permission::ALL)
        .map(|x| format!("{} ({})", x.key, x.permissions))
        .collect();
    r.check(
        partial.is_empty(),
        format!("{} member permissions", ms.name),
        if partial.is_empty() {
            "every member Initiate | Vote | Execute".to_string()
        } else {
            format!("partial: {}", partial.join(", "))
        },
    );
    let vault0 = ms.vault(0);
    r.check(
        m.rent_collector == Some(vault0),
        format!("{} rent collector", ms.name),
        match m.rent_collector {
            Some(k) if k == vault0 => format!("{k} (its vault 0)"),
            Some(k) => format!("{k}, expected its vault 0 {vault0}"),
            None => format!("none, expected its vault 0 {vault0}"),
        },
    );
    Some(m)
}

fn proposal_warnings(
    r: &mut Report,
    ms: &MsLayout,
    m: &Multisig,
    proposals: &[(u64, Option<AccountData>)],
) {
    for (index, acc) in proposals {
        let Some(p) = acc.as_ref().and_then(|a| Proposal::decode(&a.data)) else {
            continue;
        };
        match p.status {
            ProposalStatus::Approved(at) => r.warn(format!(
                "{} #{index} is Approved and not executed (executable from {}; an Approved vault \
                 transaction never expires: `cancel` it if it is no longer wanted)",
                ms.name,
                at + i64::from(m.time_lock)
            )),
            ProposalStatus::Active(_) if *index > m.stale_transaction_index => r.warn(format!(
                "{} #{index} is Active ({} of {} approvals)",
                ms.name,
                p.approved.len(),
                m.threshold
            )),
            _ => {}
        }
    }
}

/// Runs every check (module docs).
pub async fn verify<C: Chain>(chain: &C, l: &Layout) -> anyhow::Result<Report> {
    let mut r = Report::default();
    let squads_pd = loader::program_data(&sq::ID);
    let mut keys = vec![
        sq::ID,
        squads_pd,
        l.gov.key,
        l.emg.key,
        l.fee_recipient(),
        ixs::pdas::config(),
    ];
    let programs_at = keys.len();
    keys.extend(l.programs.iter().map(|p| p.program_data()));
    let accs = chain.accounts(&keys).await?;
    let acc = |i: usize| accs.get(i).and_then(|a| a.as_ref());

    // Squads.
    r.check(
        acc(0).is_some_and(|a| a.executable),
        "Squads v4 program",
        sq::ID,
    );
    match acc(1).and_then(upgrade_authority) {
        Some(None) => r.check(true, "Squads v4 immutable", "no upgrade authority"),
        Some(Some(a)) => r.check(
            false,
            "Squads v4 immutable",
            format!("upgrade authority {a}"),
        ),
        None => r.check(
            false,
            "Squads v4 ProgramData",
            format!("{squads_pd} missing"),
        ),
    }

    // The multisigs.
    let gov = multisig_checks(&mut r, &l.gov, acc(2));
    let emg = multisig_checks(&mut r, &l.emg, acc(3));

    // The fee vault.
    let fee = l.fee_recipient();
    match acc(4) {
        Some(a) => r.check(
            a.owner == anchor_lang::system_program::ID
                && a.data.is_empty()
                && a.lamports >= FEE_VAULT_PREFUND,
            "fee_recipient (Governance vault 1)",
            format!(
                "{fee}: {} lamports, owner {}, {} data bytes",
                a.lamports,
                a.owner,
                a.data.len()
            ),
        ),
        None => r.check(
            false,
            "fee_recipient (Governance vault 1)",
            format!("{fee} unfunded (needs {FEE_VAULT_PREFUND} lamports)"),
        ),
    }

    // GlobalConfig.
    match acc(5).and_then(decode_zc::<GlobalConfig>) {
        None => r.check(
            false,
            "GlobalConfig",
            format!("{} missing (init-config)", ixs::pdas::config()),
        ),
        Some(c) => {
            r.eq("GlobalConfig version", c.version, CONFIG_VERSION);
            let got = [
                c.governance,
                c.pause_authority,
                c.coverage_authority,
                c.fee_recipient,
            ];
            let want = l.authorities();
            for ((name, g), w) in AUTHORITY_NAMES.iter().zip(got).zip(want) {
                r.eq(format!("config.{name}"), g, w);
            }
            let distinct = (0..4).all(|i| (0..i).all(|j| got[i] != got[j]));
            r.check(distinct, "authorities pairwise distinct (SQ-G8)", distinct);
            r.eq("config.timelock_s", c.timelock_s, l.timelock_s);
            if c.venue_enabled != l.venue_enabled {
                r.warn(format!(
                    "config.venue_enabled {:#b} differs from the launch set {:#b}",
                    c.venue_enabled, l.venue_enabled
                ));
            }
            if c.pending.is_some() {
                let what = c
                    .pending
                    .change()
                    .map(|ch| ixs::describe_change(&ch))
                    .unwrap_or_else(|_| format!("kind {}", c.pending.kind));
                r.warn(format!(
                    "pending config change {what}, applicable from {}",
                    c.pending.not_before
                ));
            }
        }
    }

    // Upgrade authorities.
    let vault0 = l.governance();
    for (i, p) in l.programs.iter().enumerate() {
        let what = format!("{} upgrade authority", p.name);
        match acc(programs_at + i).and_then(upgrade_authority) {
            Some(Some(a)) if a == vault0 => {
                r.check(true, what, format!("{a} (Governance vault 0)"))
            }
            Some(Some(a)) => r.check(
                false,
                what,
                format!("{a}, expected Governance vault 0 {vault0}"),
            ),
            Some(None) => r.check(false, what, "none (immutable), expected Governance vault 0"),
            None => r.check(
                false,
                what,
                format!("ProgramData {} missing", p.program_data()),
            ),
        }
    }

    // Open proposals: every index, since an Approved one never expires (T-SQ-7).
    for (ms, m) in [(&l.gov, gov), (&l.emg, emg)] {
        let Some(m) = m else { continue };
        let mut from = 1;
        while from <= m.transaction_index {
            let to = m
                .transaction_index
                .min(from.saturating_add(PROPOSAL_CHUNK - 1));
            let indexes: Vec<u64> = (from..=to).collect();
            let keys: Vec<Pubkey> = indexes
                .iter()
                .map(|i| squads_pda::proposal(&ms.key, *i))
                .collect();
            let accs = chain.accounts(&keys).await?;
            let list: Vec<(u64, Option<AccountData>)> = indexes.into_iter().zip(accs).collect();
            proposal_warnings(&mut r, ms, &m, &list);
            from = to + 1;
        }
    }
    Ok(r)
}
