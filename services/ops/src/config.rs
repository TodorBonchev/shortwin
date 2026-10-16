//! The governance layout file (`governance.example.toml`) and the layout derived from it.
//!
//! The file names public keys only: the members, the two create keys and the keeper's
//! `coverage_authority`. Everything else is **derived** — the multisigs are Squads PDAs of the
//! create keys, `governance` is Governance vault 0, `fee_recipient` Governance vault 1,
//! `pause_authority` Emergency vault 0 — so a typo cannot point an authority at an address no
//! multisig controls. [`Layout::load`] refuses a file whose layout cannot be right: a threshold
//! of 0 or above the member count, duplicate members, a Squads time lock above 90 days, a program
//! timelock outside `[MIN_TIMELOCK_S, MAX_TIMELOCK_S]`, an unknown venue, a disabled (default)
//! `coverage_authority`, or authorities that are not pairwise distinct (SQ-G8).

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, bail, ensure};
use serde::Deserialize;
use shortwin::constants::{
    MAX_TIMELOCK_S, MIN_TIMELOCK_S, VENUE_BIT_JUPITER, VENUE_BIT_PUMP_CURVE, VENUE_BIT_PUMP_SWAP,
    VENUE_BIT_RAYDIUM_CPMM, VENUE_BIT_RAYDIUM_V4, VENUE_BITS_ENABLEABLE, VENUE_PROGRAMS,
};
use solana_pubkey::Pubkey;
use squads_v4_ix::pda as squads_pda;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    example: bool,
    governance: MsFile,
    emergency: MsFile,
    config: ConfigFile,
    #[serde(default)]
    upgrade: UpgradeFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MsFile {
    create_key: String,
    threshold: u16,
    time_lock: u32,
    members: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    coverage_authority: String,
    timelock_s: u32,
    venues: Vec<String>,
    pyth_sol_usd: String,
    pyth_feed_id: String,
    guard_slots: Option<[u64; VENUE_PROGRAMS]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpgradeFile {
    programs: Vec<String>,
}

impl Default for UpgradeFile {
    fn default() -> Self {
        Self {
            programs: vec!["shortwin".into()],
        }
    }
}

/// One multisig of the layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MsLayout {
    /// "Governance" or "Emergency".
    pub name: &'static str,
    pub create_key: Pubkey,
    /// The multisig PDA (`["multisig", "multisig", create_key]`).
    pub key: Pubkey,
    pub threshold: u16,
    pub time_lock: u32,
    pub members: Vec<Pubkey>,
}

impl MsLayout {
    /// Vault `index` of this multisig.
    pub fn vault(&self, index: u8) -> Pubkey {
        squads_pda::vault(&self.key, index)
    }
}

/// A program whose upgrade authority Governance holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeableProgram {
    pub name: String,
    pub id: Pubkey,
}

impl UpgradeableProgram {
    /// Its ProgramData account (the upgradeable loader's PDA of the program id).
    pub fn program_data(&self) -> Pubkey {
        crate::ixs::loader::program_data(&self.id)
    }
}

/// The validated layout (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// `example = true`: placeholder keys; nothing is sent with this layout.
    pub example: bool,
    pub gov: MsLayout,
    pub emg: MsLayout,
    pub coverage_authority: Pubkey,
    pub timelock_s: u32,
    pub venue_enabled: u32,
    pub pyth_sol_usd: Pubkey,
    pub pyth_feed_id: [u8; 32],
    pub guard_slots: Option<[u64; VENUE_PROGRAMS]>,
    pub programs: Vec<UpgradeableProgram>,
}

/// The venue names of `[config] venues` and their `VENUE_BIT_*`.
pub const VENUES: [(&str, u32); 5] = [
    ("pump_curve", VENUE_BIT_PUMP_CURVE),
    ("pump_swap", VENUE_BIT_PUMP_SWAP),
    ("raydium_v4", VENUE_BIT_RAYDIUM_V4),
    ("raydium_cpmm", VENUE_BIT_RAYDIUM_CPMM),
    ("jupiter", VENUE_BIT_JUPITER),
];

/// The programs `[upgrade] programs` may name: short.win alone since the straddle helper
/// `complete_curve` was retired (design pump-final §11, owner decision D2 of 2026-10-13; it was
/// never deployed).
pub fn known_program(name: &str) -> Option<Pubkey> {
    match name {
        "shortwin" => Some(shortwin::ID),
        _ => None,
    }
}

/// The programs `[upgrade] programs` named before their retirement, with the reason a layout
/// that still names one is refused.
const RETIRED_PROGRAMS: [(&str, &str); 1] = [(
    "complete_curve",
    "retired: pump v3 exits need no straddle bundle, and the helper was never deployed \
     (design pump-final §11, owner decision D2)",
)];

/// Parses a base58 public key, naming `what` in the error. The rejected input is quoted only
/// when it could be a mistyped public key (at most 44 base58 characters): a pasted secret key
/// (88 base58 characters) or a keypair file's byte array is reported by its length alone, as
/// [`crate::keys`] never quotes a key file.
pub fn pubkey(s: &str, what: &str) -> anyhow::Result<Pubkey> {
    let t = s.trim();
    Pubkey::from_str(t).with_context(|| {
        const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        if t.len() <= 44 && t.chars().all(|c| BASE58.contains(c)) {
            format!("{what}: not a public key: {t:?}")
        } else {
            format!(
                "{what}: not a public key ({} characters, not shown: a public key is 32 to 44 \
                 base58 characters)",
                t.chars().count()
            )
        }
    })
}

/// Parses 64 hex digits.
pub fn hex32(s: &str, what: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    ensure!(
        s.len() == 64 && s.is_ascii(),
        "{what}: expected 64 hex digits"
    );
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
            .with_context(|| format!("{what}: not hex"))?;
    }
    Ok(out)
}

fn ms(name: &'static str, f: &MsFile) -> anyhow::Result<MsLayout> {
    let create_key = pubkey(&f.create_key, &format!("{name} create_key"))?;
    let members = f
        .members
        .iter()
        .map(|m| pubkey(m, &format!("{name} member")))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for (i, m) in members.iter().enumerate() {
        ensure!(
            !members[..i].contains(m),
            "{name}: member {m} is listed twice"
        );
    }
    ensure!(
        f.threshold >= 1 && usize::from(f.threshold) <= members.len(),
        "{name}: threshold {} with {} members",
        f.threshold,
        members.len()
    );
    ensure!(
        f.time_lock <= squads_v4_ix::MAX_TIME_LOCK,
        "{name}: time_lock {} s is above Squads' 90-day cap",
        f.time_lock
    );
    Ok(MsLayout {
        name,
        create_key,
        key: squads_pda::multisig(&create_key),
        threshold: f.threshold,
        time_lock: f.time_lock,
        members,
    })
}

impl Layout {
    /// Reads and validates a layout file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Parses and validates a layout (module docs).
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let f: File = toml::from_str(text).context("parsing the layout")?;
        let gov = ms("Governance", &f.governance)?;
        let emg = ms("Emergency", &f.emergency)?;
        ensure!(
            gov.create_key != emg.create_key,
            "Governance and Emergency share a create key (one multisig)"
        );
        let c = &f.config;
        let coverage_authority = pubkey(&c.coverage_authority, "config.coverage_authority")?;
        ensure!(
            coverage_authority != Pubkey::default(),
            "config.coverage_authority: the default key disables the role"
        );
        ensure!(
            (MIN_TIMELOCK_S..=MAX_TIMELOCK_S).contains(&c.timelock_s),
            "config.timelock_s {} outside [{MIN_TIMELOCK_S}, {MAX_TIMELOCK_S}]",
            c.timelock_s
        );
        let mut venue_enabled = 0u32;
        for v in &c.venues {
            let Some((_, bit)) = VENUES.iter().find(|(n, _)| n == v) else {
                bail!("config.venues: unknown venue {v:?}");
            };
            ensure!(
                bit & VENUE_BITS_ENABLEABLE != 0,
                "config.venues: {v} cannot be enabled by this build"
            );
            venue_enabled |= bit;
        }
        let programs = f
            .upgrade
            .programs
            .iter()
            .map(|name| {
                if let Some((_, why)) = RETIRED_PROGRAMS.iter().find(|(n, _)| n == name) {
                    bail!("upgrade.programs: {name:?} is {why}");
                }
                Ok(UpgradeableProgram {
                    name: name.clone(),
                    id: known_program(name)
                        .with_context(|| format!("upgrade.programs: unknown program {name:?}"))?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let layout = Self {
            example: f.example,
            gov,
            emg,
            coverage_authority,
            timelock_s: c.timelock_s,
            venue_enabled,
            pyth_sol_usd: pubkey(&c.pyth_sol_usd, "config.pyth_sol_usd")?,
            pyth_feed_id: hex32(&c.pyth_feed_id, "config.pyth_feed_id")?,
            guard_slots: c.guard_slots,
            programs,
        };
        let a = layout.authorities();
        for (i, x) in a.iter().enumerate() {
            if let Some(j) = a[..i].iter().position(|y| y == x) {
                bail!(
                    "the authorities must be pairwise distinct (SQ-G8): {} = {}",
                    AUTHORITY_NAMES[i],
                    AUTHORITY_NAMES[j]
                );
            }
        }
        Ok(layout)
    }

    /// `governance`: Governance vault 0 (also the upgrade authority).
    pub fn governance(&self) -> Pubkey {
        self.gov.vault(0)
    }

    /// `fee_recipient`: Governance vault 1.
    pub fn fee_recipient(&self) -> Pubkey {
        self.gov.vault(1)
    }

    /// `pause_authority`: Emergency vault 0.
    pub fn pause_authority(&self) -> Pubkey {
        self.emg.vault(0)
    }

    /// `(governance, pause_authority, coverage_authority, fee_recipient)`, in
    /// [`AUTHORITY_NAMES`] order (`GlobalConfig`'s field order).
    pub fn authorities(&self) -> [Pubkey; 4] {
        [
            self.governance(),
            self.pause_authority(),
            self.coverage_authority,
            self.fee_recipient(),
        ]
    }

    /// The multisig called `name` (`governance` / `gov`, `emergency` / `emg`).
    pub fn ms(&self, name: &str) -> anyhow::Result<&MsLayout> {
        match name {
            "governance" | "gov" => Ok(&self.gov),
            "emergency" | "emg" => Ok(&self.emg),
            other => bail!("--ms {other:?}: expected governance or emergency"),
        }
    }

    /// Refuses to send with the example file's placeholder keys.
    pub fn ensure_not_example(&self) -> anyhow::Result<()> {
        ensure!(
            !self.example,
            "this is the example layout (`example = true`): its keys are placeholders; copy the \
             file, put the real public keys in and drop the `example` line"
        );
        Ok(())
    }
}

/// The names of [`Layout::authorities`].
pub const AUTHORITY_NAMES: [&str; 4] = [
    "governance",
    "pause_authority",
    "coverage_authority",
    "fee_recipient",
];

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../governance.example.toml");

    #[test]
    fn the_example_is_the_owners_layout() {
        let l = Layout::parse(EXAMPLE).unwrap();
        assert!(l.example);
        assert_eq!(
            (l.gov.threshold, l.gov.members.len(), l.gov.time_lock),
            (3, 5, 172_800)
        );
        assert_eq!(
            (l.emg.threshold, l.emg.members.len(), l.emg.time_lock),
            (2, 4, 0)
        );
        assert_eq!(l.timelock_s, shortwin::constants::DEFAULT_TIMELOCK_S);
        assert_eq!(
            l.programs.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![shortwin::ID],
            "Governance's upgrade set is short.win alone (complete_curve retired, D2)"
        );
        assert_eq!(
            l.pyth_feed_id,
            shortwin::constants::PYTH_SOL_USD_FEED_ID,
            "the program's default feed"
        );
        assert_eq!(l.pyth_sol_usd, shortwin::constants::PYTH_SOL_USD_ACCOUNT);
        assert!(l.ensure_not_example().is_err());
    }

    #[test]
    fn a_rejected_key_is_quoted_only_when_it_looks_like_a_public_key() {
        let e = format!("{:#}", pubkey("ExXri76Ezzz", "--buffer").unwrap_err());
        assert!(e.contains("\"ExXri76Ezzz\""), "{e}");
        // A pasted base58 secret key (64 bytes) or a keypair file's byte array: never echoed.
        let secret = bs58_of(&[7u8; 64]);
        assert!(secret.len() > 44);
        let e = format!("{:#}", pubkey(&secret, "--spill").unwrap_err());
        assert!(
            !e.contains(&secret[..20]) && e.contains("characters, not shown"),
            "{e}"
        );
        let bytes = format!("{:?}", [7u8; 64].to_vec());
        let e = format!("{:#}", pubkey(&bytes, "market").unwrap_err());
        assert!(!e.contains("7, 7") && e.contains("not shown"), "{e}");
    }

    /// Base58 of `b` (a test helper; the tool never encodes secrets).
    fn bs58_of(b: &[u8]) -> String {
        const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut digits: Vec<u8> = Vec::new();
        for &byte in b {
            let mut carry = u32::from(byte);
            for d in digits.iter_mut() {
                carry += u32::from(*d) << 8;
                *d = (carry % 58) as u8;
                carry /= 58;
            }
            while carry > 0 {
                digits.push((carry % 58) as u8);
                carry /= 58;
            }
        }
        digits
            .iter()
            .rev()
            .map(|d| A[usize::from(*d)] as char)
            .collect()
    }

    fn edited(from: &str, to: &str) -> anyhow::Result<Layout> {
        assert!(EXAMPLE.contains(from), "{from}");
        Layout::parse(&EXAMPLE.replacen(from, to, 1))
    }

    fn err(r: anyhow::Result<Layout>) -> String {
        format!("{:#}", r.expect_err("refused"))
    }

    #[test]
    fn refuses_layouts_that_cannot_be_right() {
        assert!(err(edited("threshold = 3", "threshold = 6")).contains("threshold 6 with 5"));
        assert!(err(edited("threshold = 2", "threshold = 0")).contains("threshold 0"));
        assert!(
            err(edited(
                "GovMember2111111111111111111111111111111111",
                "GovMember1111111111111111111111111111111111"
            ))
            .contains("listed twice")
        );
        assert!(err(edited("time_lock = 172800", "time_lock = 7776001")).contains("90-day"));
        assert!(err(edited("timelock_s = 172800", "timelock_s = 3600")).contains("outside"));
        assert!(err(edited("\"raydium_cpmm\"]", "\"orca\"]")).contains("unknown venue"));
        assert!(
            err(edited(
                "CoverageKeeperHotKey11111111111111111111111",
                "11111111111111111111111111111111"
            ))
            .contains("disables the role")
        );
        assert!(
            err(edited(
                "EmgCreateKey1111111111111111111111111111111",
                "GovCreateKey1111111111111111111111111111111"
            ))
            .contains("share a create key")
        );
        assert!(err(edited("pyth_feed_id = \"ef", "pyth_feed_id = \"zz")).contains("not hex"));
        assert!(
            err(edited(
                "\nexample = true\n",
                "\nexample = true\nrpc = \"x\"\n"
            ))
            .contains("unknown field")
        );
        assert!(err(edited("[\"shortwin\"]", "[\"short-win\"]")).contains("unknown program"));
        // The retired helper is refused by name, with the reason (D2).
        let e = err(edited(
            "[\"shortwin\"]",
            "[\"shortwin\", \"complete_curve\"]",
        ));
        assert!(
            e.contains("\"complete_curve\" is retired") && e.contains("pump-final §11"),
            "{e}"
        );
    }

    #[test]
    fn the_coverage_key_may_not_be_a_vault() {
        let l = Layout::parse(EXAMPLE).unwrap();
        let vault = l.fee_recipient().to_string();
        let e = err(edited(
            "CoverageKeeperHotKey11111111111111111111111",
            &vault,
        ));
        assert!(
            e.contains("pairwise distinct") && e.contains("fee_recipient = coverage_authority"),
            "{e}"
        );
    }
}
