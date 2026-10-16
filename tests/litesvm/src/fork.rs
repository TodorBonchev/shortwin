//! A mainnet fork of the Raydium venues (M3; ARCH §8.2 "dumped mainnet ELFs", "account
//! fixtures", "scenario builders"): the real AMM v4 and CPMM programs and pools, dumped by
//! `scripts/dump-fixtures.py` into `tests/fixtures/` with their provenance in
//! `tests/fixtures/manifest.json` (groups `raydium_programs`, `raydium_cpmm_2026_10` and
//! `raydium_state`: every venue state account at **one** mainnet slot).
//!
//! - **CPMM deploys (M10 WP-2).** CPMM was redeployed at slot 452,376,460 (group
//!   `raydium_cpmm_2026_10`: the ELF only; the pools and configs are `raydium_state`'s, whose
//!   layouts the redeploy kept). [`RaydiumProgram::Cpmm`] and every loader deploy that build
//!   ([`CpmmDeploy::CURRENT`]) with its real header, so the harness's guard slots are
//!   [`FIXTURE_GUARD_SLOTS`], not `VENUE_GUARD_SLOTS_2026_09_27`. The 445,763,504 deploy
//!   (`raydium_programs`, the one that constant names) stays loadable as [`CpmmDeploy::Sep2026`]
//!   ([`load_cpmm_deploy`]) for T-LATCH's old→new upgrade. `tests/fixtures/README.md` has the
//!   interface diff between the two builds.
//!
//! - **Loading.** [`Env::fork_raydium_v4`] / [`Env::fork_cpmm`] give an initialized short.win
//!   environment (release build, [`Env::initialized`]) with the Raydium programs deployed at
//!   their mainnet addresses from their dumped ProgramData (header ‖ ELF ‖ zeros, rebuilt and
//!   sha256-checked against the mainnet account, so the venue-upgrade latch reads the real
//!   deploy slot: T-LATCH on the dumped ProgramData), one pool with every account its swaps
//!   need, and the clock at the snapshot's slot and block time. On any other environment (e.g.
//!   the test-venue build) use [`Env::load_raydium_programs`], [`Env::load_ray_v4`],
//!   [`Env::load_cpmm`] and [`Env::set_fork_clock`]. Every fixture file is checked against its
//!   manifest sha256 when it is first read.
//! - **Pools.** [`V4Pool`] and [`CpmmPool`] hold the keys; they read the live state with the
//!   `shortwin-venue-state` readers ([`V4Pool::info`], [`CpmmPool::state`], reserves), build the
//!   venues' own swap instructions (AMM v4 tags 16/17, CPMM `swap_base_input` /
//!   `swap_base_output`, exact account orders of R-RAY §3.1–§3.2) and quote them with
//!   `shortwin-math` (`quote::ray_v4`, `quote::cpmm`) on the reader's reserves.
//! - **Users.** [`fund_wsol`] (a real WSOL account: create, airdrop, `SyncNative`),
//!   [`fund_token`] (a real token account; the balance is planted, since these mints have no
//!   mint authority), [`trader`], and [`swap`]: a direct swap signed by a normal user, through
//!   [`Env::send`] (the invariant hooks run).
//! - **Admin actions.** Byte-level scenario builders on the dumped accounts (AMM v4 `status`,
//!   `pool_open_time`, swap fee, `need_take_pnl`; CPMM `status`, `open_time`, fee counters,
//!   creator-fee fields, the pool's config, `AmmConfig` rates; vault balances; the ProgramData
//!   deploy slot), and the **real** admin instructions run by the dumped programs with the pool
//!   admin's signature forged ([`admin_send`]: LiteSVM's signature check is switched off for
//!   that one transaction; the hooks still run): AMM v4 `SetParams` (tag 6) and CPMM
//!   `update_pool_status` / `update_amm_config`, so a test can show the byte-level builder
//!   leaves exactly what the admin's own instruction leaves.
//! - **Logs.** [`ray_logs`] decodes AMM v4's `ray_log` (the reserves the venue priced on) and
//!   [`cpmm_swap_events`] CPMM's `SwapEvent` (`emit!`, "Program data:").
//!
//! **The pump fork (M5, appended below the Raydium part).** Groups `pump_programs`,
//! `pump_programs_2026_10`, `pump_programs_2026_10_13`, `pump_programs_2026_10_13b`, `pump_state`
//! and `pump_state_2026_10_13` of the same manifest: the pump, pump-fees and PumpSwap ELFs
//! (deployed from their dumped ProgramData, sha256-checked like the Raydium ones, so the latch
//! reads the real deploy slots: since the 2026-10-13 re-pin pump's redeploy of 2026-10-13
//! (456,494,698), PumpSwap's of 2026-10-08 (454,596,406) and pump-fees' of 2026-10-12
//! (456,132,171), [`PumpDeploy::CURRENT`] = [`PumpDeploy::Oct2026Chain`]; pump's 2026-10-08 build
//! ([`PumpDeploy::Oct2026Synth`], the differential's other side), the 2026-10-02 builds of
//! `pump_programs_2026_10` ([`PumpDeploy::Oct2026`]) and the 2026-09 builds of `pump_programs`
//! ([`PumpDeploy::Sep2026`]) stay loadable through [`load_pump_deploy`] for T-LATCH), and pump's
//! statics and one set of
//! accounts per coin **role** ([`PumpCoin`]) at one mainnet slot per state group (`pump_state`'s
//! roles for the older suites, `pump_state_2026_10_13`'s 166-byte curves, synthetic completions
//! and the rest; an address both groups hold — pump's `Global`, now 1,088 bytes, both
//! `FeeConfig`s, PumpSwap's `GlobalConfig` — resolves to the newer group), loaded on first use
//! apart from the Raydium fixtures. [`Env::fork_pump`] / [`Env::fork_pump_coin`], or piecewise
//! [`Env::load_pump_programs`], [`Env::load_pump_statics`], [`Env::load_pump_coin`],
//! [`Env::set_pump_rent`] (mainnet's Rent sysvar, dumped with the state) and
//! [`Env::set_pump_clock`]; [`PumpCurve`] reads a curve with the `shortwin-venue-state` readers,
//! resolves its fees from the live `FeeConfig` / `Global` with `shortwin_math::fees::pump_curve`,
//! quotes with `quote::sol_leg`, and builds pump's own `buy_v2` / `sell_v2` /
//! `buy_exact_quote_in_v2` in the IDL's exact account order (R-PUMP §3.5); [`PumpPool`] is the
//! graduated coin's canonical PumpSwap pool with its boost vault; [`pump_trade_events`] decodes
//! pump's `TradeEvent`; byte-level scenario builders rewrite curves (reserves, completion,
//! creator, per-coin fee), `Global.creator_fee_configurable` and a `FeeConfig`'s tiers. Accounts
//! the dump found absent (the event authorities, a closed boost vault, a creator vault that never
//! existed) are simply not loaded ([`is_absent`]).
//!
//! **The mainnet SPL programs (M10, T-MEMO; at the end of the file).** Group `spl_programs`:
//! [`load_mainnet_spl`] replaces LiteSVM's bundled Token-2022 and spl-memo v3 with the builds
//! mainnet runs — Token-2022 from its dumped ProgramData (real header, the ELF, the zero
//! padding; trimmed ELF [`TOKEN_2022_MAINNET_TRIMMED_SHA256`]) under the upgradeable loader,
//! spl-memo v3 under BPFLoader2 ([`SPL_MEMO_V3_MAINNET_SHA256`]). Every [`Env`] calls it right
//! after `LiteSVM::new()`, unless `SHORTWIN_BUNDLED_SPL=1` ([`bundled_spl`]).
//!
//! **Squads v4 (M10 WP-7; at the end of the file).** Group `squads_programs`: [`load_squads`]
//! deploys the immutable mainnet Squads v4 from its dumped ProgramData (deploy slot 302,582,236,
//! no upgrade authority; trimmed ELF [`SQUADS_V4_MAINNET_TRIMMED_SHA256`]) and writes its
//! `ProgramConfig`; [`Env::with_squads`] calls it, `crate::squads` drives it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use litesvm::types::TransactionMetadata;
use litesvm_token::{CreateAccount, SyncNative};
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, GUARD_RAYDIUM_CPMM,
    PUMP_AMM_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG, PUMP_FEES_PROGRAM_ID, PUMP_GLOBAL,
    PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, RAYDIUM_CPMM_AUTHORITY,
    RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, VENUE_GUARD_SLOTS_2026_09_27, VENUE_GUARD_SLOTS_2026_10_03, VENUE_PROGRAMS,
    WSOL_MINT,
};
use shortwin::state::Market;
use shortwin_math::fees::{
    self as mfees, CpmmConfig, CpmmFees, CpmmPoolFees, CurveFeeSchedule, Direction, FeeBps,
    FeeTier as MathFeeTier,
};
use shortwin_math::quote::{
    BuyQuote, CurveState, SellQuote, cpmm as qcpmm, ray_v4 as qv4, sol_leg,
};
use shortwin_venue_state::{pump, pump_amm, raydium_cpmm, raydium_v4, token_account};
use solana_account::{Account, ReadableAccount};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::env::send_direct;
use crate::tokens::TokenProgram;
use crate::{Env, TxResult, invariants, read};

// ---- fixtures ------------------------------------------------------------------------------

/// `tests/fixtures/` (`$SHORTWIN_FIXTURES` overrides).
pub fn fixtures_dir() -> PathBuf {
    std::env::var_os("SHORTWIN_FIXTURES").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures"),
        PathBuf::from,
    )
}

/// The manifest groups of the Raydium fork (loaded, and sha256-checked, on first use). CPMM's
/// program and ProgramData addresses are in both program groups; [`fixture_at`] resolves them to
/// the later one, `raydium_cpmm_2026_10` ([`CpmmDeploy::CURRENT`]), and the loaders pick a deploy
/// by name ([`CpmmDeploy`]).
pub const RAYDIUM_GROUPS: [&str; 3] = ["raydium_programs", "raydium_cpmm_2026_10", "raydium_state"];
/// The manifest groups of the pump fork (loaded separately, on first use of a pump fixture, so
/// the Raydium tests never rebuild the 10 MiB pump ProgramData accounts). The three programs'
/// addresses are in the program groups; [`pump_fixture_at`] resolves each to the latest group
/// holding it — pump to `pump_programs_2026_10_13b`, PumpSwap and pump-fees to
/// `pump_programs_2026_10_13` ([`PumpDeploy::CURRENT`]) —, and the loaders pick a deploy by
/// name. The statics both state groups hold resolve to `pump_state_2026_10_13` the same way.
pub const PUMP_GROUPS: [&str; 6] = [
    "pump_programs",
    "pump_programs_2026_10",
    "pump_state",
    "pump_programs_2026_10_13",
    "pump_state_2026_10_13",
    "pump_programs_2026_10_13b",
];
/// The pump fork's state groups, each one `getMultipleAccounts` snapshot with its coin roles:
/// `pump_state` (the older suites' roles and T-LATCH) and `pump_state_2026_10_13` (the state the
/// 2026-10-08/12 redeploys introduced).
pub const PUMP_STATE_GROUPS: [&str; 2] = ["pump_state", "pump_state_2026_10_13"];
/// The manifest groups of the PumpSwap state (M6; R-VEN Update 2026-09-30 H10): `pamm_state` (one
/// `getMultipleAccounts` at slot 451,784,607: PumpSwap's statics and five canonical-pool roles)
/// and `pamm_synthetic` (pools derived offline from `pamm_state` bytes, marked `synthetic`: the
/// negative-VQR pool). The programs are [`PUMP_GROUPS`]' ([`PumpDeploy::CURRENT`]).
pub const PAMM_GROUPS: [&str; 2] = ["pamm_state", "pamm_synthetic"];

/// One manifest entry of the managed groups, with its account rebuilt and checked.
#[derive(Clone, Debug)]
pub struct Fixture {
    /// The manifest name (e.g. `ray_v4_fartcoin_pool`).
    pub name: String,
    /// One of [`RAYDIUM_GROUPS`], [`PUMP_GROUPS`], [`PAMM_GROUPS`], [`SPL_GROUPS`] or
    /// [`SQUADS_GROUPS`].
    pub group: String,
    pub address: Pubkey,
    /// The mainnet slot the account was read at.
    pub slot: u64,
    /// The account exactly as mainnet held it (ProgramData: header ‖ ELF ‖ zero padding).
    pub account: Account,
    /// ProgramData only: the length of the ELF after the 45-byte header (the rest is zeros).
    pub elf_len: Option<usize>,
    /// `pump_state` only: the coin role the account belongs to (`None` for the statics).
    pub role: Option<String>,
}

/// The snapshot a group was read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub slot: u64,
    /// `getBlockTime(slot)`.
    pub block_time: i64,
}

/// The fixtures of one fork (its manifest groups).
struct Fixtures {
    by_name: HashMap<String, Fixture>,
    by_address: HashMap<Pubkey, String>,
    /// Accounts the dump recorded as absent (`"absent": true`; pump groups only).
    absent: std::collections::HashSet<Pubkey>,
    /// The (first) state group's snapshot.
    state: Snapshot,
    /// Every state group's snapshot, by group.
    snapshots: HashMap<String, Snapshot>,
    /// The state groups' coin (or pool) roles: role → mint (empty for the Raydium fork). Role
    /// names are unique across the state groups.
    coins: HashMap<String, Pubkey>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_fixture_file(rel: &str) -> Vec<u8> {
    let path = fixtures_dir().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The entries of `groups`: by name, name by address, and the addresses the dump recorded as
/// absent.
type Entries = (
    HashMap<String, Fixture>,
    HashMap<Pubkey, String>,
    std::collections::HashSet<Pubkey>,
);

/// `tests/fixtures/manifest.json`, parsed.
fn manifest() -> serde_json::Value {
    let path = fixtures_dir().join("manifest.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("manifest.json")
}

/// Rebuilds and sha256-checks every account of `groups` in manifest `m` (ProgramData: header ‖
/// ELF ‖ zeros up to `space`).
fn load_entries(m: &serde_json::Value, groups: &[&str]) -> Entries {
    let str_of = |e: &serde_json::Value, k: &str| -> String {
        e[k].as_str()
            .unwrap_or_else(|| panic!("manifest entry {}: no `{k}`", e["name"]))
            .to_owned()
    };
    let u64_of = |e: &serde_json::Value, k: &str| -> u64 {
        e[k].as_u64()
            .unwrap_or_else(|| panic!("manifest entry {}: no `{k}`", e["name"]))
    };
    let mut by_name: HashMap<String, Fixture> = HashMap::new();
    let mut by_address = HashMap::new();
    let mut absent = std::collections::HashSet::new();
    for e in m["accounts"].as_array().expect("accounts") {
        let group = e["group"].as_str().unwrap_or_default();
        if !groups.contains(&group) {
            continue;
        }
        let name = str_of(e, "name");
        if e["absent"].as_bool() == Some(true) {
            absent.insert(Pubkey::from_str(&str_of(e, "address")).expect("address"));
            continue;
        }
        let space = usize::try_from(u64_of(e, "space")).expect("space");
        let mut elf_len = None;
        let data = if let Some(file) = e["file"].as_str() {
            read_fixture_file(file)
        } else {
            // ProgramData: header ‖ ELF ‖ zeros up to `space`.
            let mut d = read_fixture_file(e["files"]["header"].as_str().expect("header"));
            let elf = read_fixture_file(e["files"]["elf"].as_str().expect("elf"));
            assert_eq!(hex(&sha256(&elf)), str_of(e, "elf_sha256"), "{name}: ELF");
            elf_len = Some(elf.len());
            d.extend_from_slice(&elf);
            assert!(d.len() <= space, "{name}: ELF longer than the account");
            d.resize(space, 0);
            d
        };
        assert_eq!(data.len(), space, "{name}: length");
        assert_eq!(
            hex(&sha256(&data)),
            str_of(e, "sha256"),
            "{name}: the fixture bytes differ from the manifest (re-run scripts/dump-fixtures.py)"
        );
        let address = Pubkey::from_str(&str_of(e, "address")).expect("address");
        let owner = Pubkey::from_str(&str_of(e, "owner")).expect("owner");
        let fixture = Fixture {
            name: name.clone(),
            group: group.to_owned(),
            address,
            slot: u64_of(e, "slot"),
            account: Account {
                lamports: u64_of(e, "lamports"),
                data,
                owner,
                executable: e["executable"].as_bool().expect("executable"),
                rent_epoch: u64_of(e, "rent_epoch"),
            },
            elf_len,
            role: e["role"].as_str().map(str::to_owned),
        };
        // An address held by two groups (CPMM's two deploys) resolves to the later of `groups`.
        let rank = |g: &str| groups.iter().position(|x| *x == g);
        if by_address
            .get(&address)
            .is_none_or(|prev| rank(group) >= rank(&by_name[prev].group))
        {
            by_address.insert(address, name.clone());
        }
        by_name.insert(name, fixture);
    }
    (by_name, by_address, absent)
}

/// Loads the entries of `groups` and the snapshots of `state_groups` (the first is
/// [`Fixtures::state`]), rebuilding and sha256-checking every account.
fn load_fixtures(groups: &[&str], state_groups: &[&str]) -> Fixtures {
    let m = manifest();
    let (by_name, by_address, absent) = load_entries(&m, groups);
    let mut snapshots = HashMap::new();
    let mut coins = HashMap::new();
    for state_group in state_groups {
        let snap = m["snapshots"]
            .as_array()
            .expect("snapshots")
            .iter()
            .find(|s| s["group"] == *state_group)
            .unwrap_or_else(|| {
                panic!("no `{state_group}` snapshot (scripts/dump-fixtures.py fetch)")
            });
        for (role, mint) in snap["coins"]
            .as_object()
            .or_else(|| snap["pools"].as_object())
            .into_iter()
            .flatten()
        {
            let mint = Pubkey::from_str(mint.as_str().expect("mint")).expect("mint");
            let prev = coins.insert(role.clone(), mint);
            assert!(prev.is_none(), "coin role `{role}` in two state groups");
        }
        snapshots.insert(
            (*state_group).to_owned(),
            Snapshot {
                slot: snap["slot"].as_u64().expect("slot"),
                block_time: snap["block_time"].as_i64().expect("block_time"),
            },
        );
    }
    Fixtures {
        by_name,
        by_address,
        absent,
        state: snapshots[state_groups[0]],
        snapshots,
        coins,
    }
}

/// The Raydium fork's fixtures.
fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| load_fixtures(&RAYDIUM_GROUPS, &["raydium_state"]))
}

/// The pump fork's fixtures.
fn pump_fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| load_fixtures(&PUMP_GROUPS, &PUMP_STATE_GROUPS))
}

/// The PumpSwap state's fixtures (M6).
fn pamm_fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| load_fixtures(&PAMM_GROUPS, &["pamm_state"]))
}

/// The fixture named `name`, Raydium, pump or PumpSwap (panics if the manifest has no such
/// entry).
pub fn fixture(name: &str) -> &'static Fixture {
    fixtures()
        .by_name
        .get(name)
        .or_else(|| pump_fixtures().by_name.get(name))
        .or_else(|| {
            name.starts_with("pamm_")
                .then(|| pamm_fixtures().by_name.get(name))
                .flatten()
        })
        .unwrap_or_else(|| panic!("no fixture `{name}` in manifest.json"))
}

/// The fixture at `address`: the Raydium fork's if it holds one, else the pump fork's (an
/// address in both, e.g. the WSOL mint, resolves to the Raydium entry; the pump fork's own
/// loaders use [`pump_fixture_at`]).
pub fn fixture_at(address: &Pubkey) -> Option<&'static Fixture> {
    let f = fixtures();
    f.by_address
        .get(address)
        .map(|n| &f.by_name[n])
        .or_else(|| pump_fixture_at(address))
}

/// Every Raydium fixture (every group of [`RAYDIUM_GROUPS`]).
pub fn all_fixtures() -> impl Iterator<Item = &'static Fixture> {
    fixtures().by_name.values()
}

/// Every pump fixture (both groups).
pub fn all_pump_fixtures() -> impl Iterator<Item = &'static Fixture> {
    pump_fixtures().by_name.values()
}

/// The slot and block time every venue-state fixture was read at.
pub fn state_snapshot() -> Snapshot {
    fixtures().state
}

/// Writes fixture `name` into the VM (as mainnet held it). Returns its address.
pub fn load(svm: &mut LiteSVM, name: &str) -> Pubkey {
    let f = fixture(name);
    svm.set_account(f.address, f.account.clone())
        .unwrap_or_else(|e| panic!("set_account({name}): {e:?}"));
    f.address
}

/// Writes the fixture at `address` into the VM; panics if there is none.
pub fn load_at(svm: &mut LiteSVM, address: &Pubkey) {
    let f = fixture_at(address).unwrap_or_else(|| panic!("no fixture at {address}"));
    load(svm, &f.name);
}

// ---- programs ------------------------------------------------------------------------------

/// The two Raydium programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaydiumProgram {
    AmmV4,
    Cpmm,
}

impl RaydiumProgram {
    pub const ALL: [Self; 2] = [Self::AmmV4, Self::Cpmm];

    /// The fixture name prefix (CPMM: [`CpmmDeploy::CURRENT`]'s).
    fn prefix(self) -> &'static str {
        match self {
            Self::AmmV4 => "raydium_amm_v4",
            Self::Cpmm => CpmmDeploy::CURRENT.prefix(),
        }
    }

    /// The program id.
    pub fn id(self) -> Pubkey {
        match self {
            Self::AmmV4 => RAYDIUM_V4_PROGRAM_ID,
            Self::Cpmm => RAYDIUM_CPMM_PROGRAM_ID,
        }
    }

    /// The fixture of its ProgramData account.
    pub fn program_data(self) -> &'static Fixture {
        fixture(&format!("{}_program_data", self.prefix()))
    }

    /// The fixture of its program account.
    pub fn program(self) -> &'static Fixture {
        fixture(&format!("{}_program", self.prefix()))
    }

    /// The dumped ELF (the ProgramData after its 45-byte header, up to the ELF's own end).
    pub fn elf(self) -> &'static [u8] {
        let pd = self.program_data();
        let start = shortwin_venue_state::program_data::HEADER_LEN;
        let len = pd.elf_len.expect("a ProgramData fixture");
        &pd.account.data[start..start + len]
    }
}

/// Deploys `program` at its mainnet address from the dumped accounts: the ProgramData first
/// (the real header: deploy slot and upgrade authority), then the executable program account,
/// which makes LiteSVM compile the ELF. CPMM is [`CpmmDeploy::CURRENT`].
pub fn load_program(svm: &mut LiteSVM, program: RaydiumProgram) {
    let pd = program.program_data();
    load(svm, &pd.name);
    load(svm, &program.program().name);
}

/// CPMM's deploy slot since its 2026-10 redeploy (group `raydium_cpmm_2026_10`; the manifest's
/// `deploy_slot_expected`). `VENUE_GUARD_SLOTS_2026_09_27` still names 445,763,504.
pub const CPMM_DEPLOY_SLOT_2026_10: u64 = 452_376_460;

/// The venue deploy slots the harness's ProgramData carry and [`Env::default_init_args`] records
/// as `guard_slot` (A7; by `VenueGuard` index): `VENUE_GUARD_SLOTS_2026_09_27` with CPMM at
/// [`CPMM_DEPLOY_SLOT_2026_10`], pump at its 2026-10-13 redeploy, PumpSwap at its 2026-10-08 one
/// and pump-fees at its 2026-10-12 one ([`PumpDeploy::Oct2026Chain`]), the builds every fork
/// loads ([`CpmmDeploy::CURRENT`], [`PumpDeploy::CURRENT`]); `VENUE_GUARD_SLOTS_2026_10_13`'s
/// three pump slots. The tests use it wherever they mean "the deploy slot the fork holds";
/// `VENUE_GUARD_SLOTS_2026_09_27` stays the 2026-09-27 baseline and `VENUE_GUARD_SLOTS_2026_10_03`
/// the 2026-10-02 builds', which T-LATCH's old→new upgrades start from ([`CpmmDeploy::Sep2026`],
/// [`PumpDeploy::Sep2026`], [`PumpDeploy::Oct2026`]).
pub const FIXTURE_GUARD_SLOTS: [u64; VENUE_PROGRAMS] = {
    let mut s = VENUE_GUARD_SLOTS_2026_09_27;
    s[GUARD_RAYDIUM_CPMM as usize] = CPMM_DEPLOY_SLOT_2026_10;
    s[GUARD_PUMP as usize] = PUMP_DEPLOY_SLOT_2026_10_13;
    s[GUARD_PUMP_AMM as usize] = PUMP_AMM_DEPLOY_SLOT_2026_10_08;
    s[GUARD_PUMP_FEES as usize] = PUMP_FEES_DEPLOY_SLOT_2026_10_12;
    s
};

/// The CPMM builds the fixtures hold, both at `CPMMoo8L…` / ProgramData `DMawCQzb…` (the same
/// upgrade authority, the Squads v4 vault `FytDrVz…`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpmmDeploy {
    /// Deploy slot 445,763,504 (2026-09-10; group `raydium_programs`): the build R-RAY and
    /// `VENUE_GUARD_SLOTS_2026_09_27` name (`raydium-cp-swap@59fb845`, behavioural match).
    Sep2026,
    /// Deploy slot 452,376,460 ([`CPMM_DEPLOY_SLOT_2026_10`]; group `raydium_cpmm_2026_10`):
    /// the redeploy mainnet runs: `raydium-cp-swap@bdeee23` (`feat/creator-fee-share`; a
    /// solana-verify 3.1.10 rebuild is byte-identical, `tests/fixtures/README.md`). Same swap
    /// interface, same `PoolState` / `AmmConfig` layouts.
    Oct2026,
}

impl CpmmDeploy {
    /// The build every fork loads ([`RaydiumProgram::Cpmm`], [`Env::load_raydium_programs`]).
    pub const CURRENT: Self = Self::Oct2026;

    fn prefix(self) -> &'static str {
        match self {
            Self::Sep2026 => "raydium_cpmm",
            Self::Oct2026 => "raydium_cpmm_2026_10",
        }
    }

    /// The deploy slot its ProgramData header carries.
    pub fn deploy_slot(self) -> u64 {
        match self {
            Self::Sep2026 => VENUE_GUARD_SLOTS_2026_09_27[usize::from(GUARD_RAYDIUM_CPMM)],
            Self::Oct2026 => CPMM_DEPLOY_SLOT_2026_10,
        }
    }

    /// The fixture of its ProgramData account.
    pub fn program_data(self) -> &'static Fixture {
        fixture(&format!("{}_program_data", self.prefix()))
    }

    /// The fixture of its program account (the same bytes for both builds).
    pub fn program(self) -> &'static Fixture {
        fixture(&format!("{}_program", self.prefix()))
    }

    /// The dumped ELF.
    pub fn elf(self) -> &'static [u8] {
        let pd = self.program_data();
        let start = shortwin_venue_state::program_data::HEADER_LEN;
        let len = pd.elf_len.expect("a ProgramData fixture");
        &pd.account.data[start..start + len]
    }
}

/// Deploys CPMM build `deploy` at its mainnet address (as [`load_program`]), replacing whichever
/// CPMM build the VM held: LiteSVM recompiles the ELF when the program account is rewritten.
pub fn load_cpmm_deploy(svm: &mut LiteSVM, deploy: CpmmDeploy) {
    load(svm, &deploy.program_data().name);
    load(svm, &deploy.program().name);
}

/// Rewrites the deploy slot in `program`'s ProgramData header (an "upgrade" for the latch
/// tests, T-LATCH), keeping the ELF and the authority. The program keeps running the dumped
/// ELF.
pub fn set_deploy_slot(svm: &mut LiteSVM, program: RaydiumProgram, slot: u64) {
    patch(svm, &program.program_data().address, |d| {
        d[4..12].copy_from_slice(&slot.to_le_bytes());
    });
}

impl Env {
    /// Deploys both Raydium programs from the dumped ProgramData ([`load_program`]).
    pub fn load_raydium_programs(&mut self) {
        for p in RaydiumProgram::ALL {
            load_program(&mut self.svm, p);
        }
    }

    /// Sets the clock to the snapshot's slot and block time ([`state_snapshot`]); time only
    /// moves forward from there ([`Env::warp`]).
    pub fn set_fork_clock(&mut self) {
        let s = state_snapshot();
        self.set_clock(s.block_time, s.slot);
    }

    /// Loads AMM v4 pool `which` and every account its swaps need (pool, vaults, both mints,
    /// LP mint, the authority, the AMM v4 `AmmConfig`), and the AMM v4 program if missing.
    pub fn load_ray_v4(&mut self, which: V4Fixture) -> V4Pool {
        if self.svm.get_account(&RAYDIUM_V4_PROGRAM_ID).is_none() {
            load_program(&mut self.svm, RaydiumProgram::AmmV4);
        }
        let pool = load(&mut self.svm, &format!("{}_pool", which.prefix()));
        let info = raydium_v4::read(
            &raydium_v4::PROGRAM,
            &fixture(&format!("{}_pool", which.prefix())).account.data,
        )
        .expect("fixture pool");
        let keys = V4Pool::from_info(which, pool, &info);
        for k in [
            keys.coin_vault,
            keys.pc_vault,
            keys.coin_mint,
            keys.pc_mint,
            keys.lp_mint,
            RAYDIUM_V4_AUTHORITY,
        ] {
            load_at(&mut self.svm, &k);
        }
        load(&mut self.svm, "ray_v4_amm_config");
        keys
    }

    /// Loads CPMM pool `which` and every account its swaps need (pool, its `AmmConfig`, vaults,
    /// mints, LP mint, observation, the authority), all 21 live `AmmConfig`s (fee variants),
    /// and the CPMM program if missing.
    pub fn load_cpmm(&mut self, which: CpmmFixture) -> CpmmPool {
        if self.svm.get_account(&RAYDIUM_CPMM_PROGRAM_ID).is_none() {
            load_program(&mut self.svm, RaydiumProgram::Cpmm);
        }
        let name = format!("{}_pool", which.prefix());
        let pool = load(&mut self.svm, &name);
        let state = raydium_cpmm::read_pool(&raydium_cpmm::PROGRAM, &fixture(&name).account.data)
            .expect("fixture pool");
        let keys = CpmmPool::from_state(which, pool, &state);
        for k in [
            keys.amm_config,
            keys.token_0_vault,
            keys.token_1_vault,
            keys.token_0_mint,
            keys.token_1_mint,
            keys.lp_mint,
            keys.observation,
            RAYDIUM_CPMM_AUTHORITY,
        ] {
            load_at(&mut self.svm, &k);
        }
        for index in 0..CPMM_CONFIGS {
            load(&mut self.svm, &cpmm_config_name(index));
        }
        keys
    }

    /// [`Env::initialized`] (release short.win, config at the harness defaults, whose venue
    /// deploy slots are the dumped ones), both Raydium programs, the Fartcoin AMM v4 pool
    /// `Bzc9NZfM…`, the clock at the snapshot.
    pub fn fork_raydium_v4() -> (Self, V4Pool) {
        let mut env = Self::initialized();
        env.load_raydium_programs();
        let pool = env.load_ray_v4(V4Fixture::Fartcoin);
        env.set_fork_clock();
        (env, pool)
    }

    /// [`Env::initialized`], both Raydium programs, the USELESS CPMM pool `Q2sPHPdU…` (with all
    /// 21 configs), the clock at the snapshot.
    pub fn fork_cpmm() -> (Self, CpmmPool) {
        let mut env = Self::initialized();
        env.load_raydium_programs();
        let pool = env.load_cpmm(CpmmFixture::Useless);
        env.set_fork_clock();
        (env, pool)
    }
}

// ---- AMM v4 ---------------------------------------------------------------------------------

/// The dumped AMM v4 pools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V4Fixture {
    /// `Bzc9NZfM…`: Fartcoin's main pool, WSOL is the coin (R-RAY §6.4).
    Fartcoin,
    /// `9GWMrDgH…`: the tiny reversed-order Fartcoin pool, WSOL is the pc, `need_take_pnl` ≠ 0.
    FartcoinReversed,
}

impl V4Fixture {
    pub const ALL: [Self; 2] = [Self::Fartcoin, Self::FartcoinReversed];

    /// The manifest name prefix.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Fartcoin => "ray_v4_fartcoin",
            Self::FartcoinReversed => "ray_v4_fartcoin_rev",
        }
    }
}

/// An AMM v4 pool's keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V4Pool {
    pub which: V4Fixture,
    pub pool: Pubkey,
    pub coin_vault: Pubkey,
    pub pc_vault: Pubkey,
    pub coin_mint: Pubkey,
    pub pc_mint: Pubkey,
    pub lp_mint: Pubkey,
    /// WSOL is the coin (else the pc).
    pub sol_is_coin: bool,
}

/// The AMM v4 `ExceededSlippage` error (`AmmError` 0x1e).
pub const V4_ERR_EXCEEDED_SLIPPAGE: u32 = 0x1e;
/// The AMM v4 `InvalidStatus` error (`AmmError` 0x16).
pub const V4_ERR_INVALID_STATUS: u32 = 0x16;
/// The AMM v4 `InvalidInput` error (`AmmError` 0x1d).
pub const V4_ERR_INVALID_INPUT: u32 = 0x1d;
/// The AMM v4 `CheckedSubOverflow` error (`AmmError` 0x20).
pub const V4_ERR_CHECKED_SUB_OVERFLOW: u32 = 0x20;
/// The AMM v4 `InsufficientFunds` error (`AmmError` 0x28).
pub const V4_ERR_INSUFFICIENT_FUNDS: u32 = 0x28;
/// The AMM v4 `InvalidSignAccount` error (`AmmError` 0x15).
pub const V4_ERR_INVALID_SIGN_ACCOUNT: u32 = 0x15;
/// The AMM v4 `InvalidFee` error (`AmmError` 0x30; a fee fraction with `num ≥ den`).
pub const V4_ERR_INVALID_FEE: u32 = 0x30;

/// AMM v4 `SwapBaseInV2` tag.
pub const V4_TAG_SWAP_BASE_IN_V2: u8 = 16;
/// AMM v4 `SwapBaseOutV2` tag.
pub const V4_TAG_SWAP_BASE_OUT_V2: u8 = 17;
/// AMM v4 `SetParams` tag (admin).
pub const V4_TAG_SET_PARAMS: u8 = 6;

fn key(k: &shortwin_venue_state::Key) -> Pubkey {
    Pubkey::new_from_array(*k)
}

impl V4Pool {
    fn from_info(which: V4Fixture, pool: Pubkey, info: &raydium_v4::AmmInfo) -> Self {
        Self {
            which,
            pool,
            coin_vault: key(&info.coin_vault),
            pc_vault: key(&info.pc_vault),
            coin_mint: key(&info.coin_vault_mint),
            pc_mint: key(&info.pc_vault_mint),
            lp_mint: key(&info.lp_mint),
            sol_is_coin: info.sol_is_coin().expect("a SOL pool"),
        }
    }

    /// The token (non-WSOL) mint.
    pub fn token_mint(&self) -> Pubkey {
        if self.sol_is_coin {
            self.pc_mint
        } else {
            self.coin_mint
        }
    }

    /// `(input vault, output vault)` of a direction.
    pub fn io_vaults(&self, dir: Direction) -> (Pubkey, Pubkey) {
        let (sol, tok) = if self.sol_is_coin {
            (self.coin_vault, self.pc_vault)
        } else {
            (self.pc_vault, self.coin_vault)
        };
        match dir {
            Direction::Buy => (sol, tok),
            Direction::Sell => (tok, sol),
        }
    }

    /// The pool as the reader sees it now.
    pub fn info(&self, svm: &LiteSVM) -> raydium_v4::AmmInfo {
        let acc = svm.get_account(&self.pool).expect("pool");
        raydium_v4::read(&acc.owner.to_bytes(), &acc.data).expect("AmmInfo")
    }

    /// `(coin_vault.amount, pc_vault.amount)`.
    pub fn vault_amounts(&self, svm: &LiteSVM) -> (u64, u64) {
        (
            vault_amount(svm, &self.coin_vault),
            vault_amount(svm, &self.pc_vault),
        )
    }

    /// The reader's pricing reserves `(coin, pc)`; `None` if the venue cannot price.
    pub fn reserves(&self, svm: &LiteSVM) -> Option<(u64, u64)> {
        let (c, p) = self.vault_amounts(svm);
        self.info(svm).reserves(c, p)
    }

    /// `(input reserve, output reserve)` of a direction.
    pub fn io_reserves(&self, svm: &LiteSVM, dir: Direction) -> Option<(u64, u64)> {
        let (coin, pc) = self.reserves(svm)?;
        let (sol, tok) = if self.sol_is_coin {
            (coin, pc)
        } else {
            (pc, coin)
        };
        Some(match dir {
            Direction::Buy => (sol, tok),
            Direction::Sell => (tok, sol),
        })
    }

    /// `ARCH §5.3`'s predicate on the live state.
    pub fn venue_executable(&self, svm: &LiteSVM, now: i64) -> bool {
        let (c, p) = self.vault_amounts(svm);
        self.info(svm).venue_executable(now, c, p)
    }

    /// `quote::ray_v4::sell_exact_in` of `amount_in` in direction `dir` on the live state
    /// (the formula is direction-agnostic: input reserve, output reserve).
    pub fn quote_exact_in(
        &self,
        svm: &LiteSVM,
        dir: Direction,
        amount_in: u64,
    ) -> Option<SellQuote> {
        let (x, y) = self.io_reserves(svm, dir)?;
        let i = self.info(svm);
        qv4::sell_exact_in(
            amount_in,
            u128::from(x),
            u128::from(y),
            i.swap_fee_numerator,
            i.swap_fee_denominator,
        )
    }

    /// `quote::ray_v4::buy_exact_out` of `amount_out` in direction `dir` on the live state.
    pub fn quote_exact_out(
        &self,
        svm: &LiteSVM,
        dir: Direction,
        amount_out: u64,
    ) -> Option<BuyQuote> {
        let (x, y) = self.io_reserves(svm, dir)?;
        let i = self.info(svm);
        qv4::buy_exact_out(
            amount_out,
            u128::from(x),
            u128::from(y),
            i.swap_fee_numerator,
            i.swap_fee_denominator,
        )
    }

    /// The v2 swap accounts (R-RAY §3.1): `[Tokenkeg, pool W, authority, coin_vault W,
    /// pc_vault W, source W, destination W, owner S]`.
    pub fn swap_accounts(
        &self,
        owner: &Pubkey,
        source: &Pubkey,
        destination: &Pubkey,
    ) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new_readonly(RAYDIUM_V4_AUTHORITY, false),
            AccountMeta::new(self.coin_vault, false),
            AccountMeta::new(self.pc_vault, false),
            AccountMeta::new(*source, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*owner, true),
        ]
    }

    /// `swap_base_in_v2` (tag 16): `[16 | amount_in | minimum_amount_out]`.
    pub fn swap_base_in_v2_ix(
        &self,
        owner: &Pubkey,
        source: &Pubkey,
        destination: &Pubkey,
        amount_in: u64,
        minimum_amount_out: u64,
    ) -> Instruction {
        Instruction {
            program_id: RAYDIUM_V4_PROGRAM_ID,
            accounts: self.swap_accounts(owner, source, destination),
            data: tagged(V4_TAG_SWAP_BASE_IN_V2, &[amount_in, minimum_amount_out]),
        }
    }

    /// `swap_base_out_v2` (tag 17): `[17 | max_amount_in | amount_out]`.
    pub fn swap_base_out_v2_ix(
        &self,
        owner: &Pubkey,
        source: &Pubkey,
        destination: &Pubkey,
        max_amount_in: u64,
        amount_out: u64,
    ) -> Instruction {
        Instruction {
            program_id: RAYDIUM_V4_PROGRAM_ID,
            accounts: self.swap_accounts(owner, source, destination),
            data: tagged(V4_TAG_SWAP_BASE_OUT_V2, &[max_amount_in, amount_out]),
        }
    }
}

fn tagged(tag: u8, args: &[u64]) -> Vec<u8> {
    let mut d = vec![tag];
    for a in args {
        d.extend_from_slice(&a.to_le_bytes());
    }
    d
}

// ---- CPMM -----------------------------------------------------------------------------------

/// The dumped CPMM pools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpmmFixture {
    /// `Q2sPHPdU…` USELESS: WSOL token 0, config 0, creator fee off.
    Useless,
    /// `FQd8z3…`: WSOL token 0, config 0, creator fee on token 0 (= WSOL).
    CreatorFee,
    /// `7ZRtFcgf…`: token 1 under Token-2022, config 0, creator fee off.
    Token2022,
    /// `3Cv9uPpa…`: WSOL token 1, config 10, creator fee on token 1 (= WSOL).
    WsolToken1,
    /// `FrZnJAFt…`: WSOL token 0, config 0, creator fee on both tokens (always the input).
    CreatorBoth,
}

impl CpmmFixture {
    pub const ALL: [Self; 5] = [
        Self::Useless,
        Self::CreatorFee,
        Self::Token2022,
        Self::WsolToken1,
        Self::CreatorBoth,
    ];

    /// The manifest name prefix.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Useless => "cpmm_useless",
            Self::CreatorFee => "cpmm_creator_fee",
            Self::Token2022 => "cpmm_token2022",
            Self::WsolToken1 => "cpmm_wsol_token1",
            Self::CreatorBoth => "cpmm_creator_both",
        }
    }
}

/// How many live CPMM `AmmConfig`s the fixtures hold (indices 0–20, R-RAY §2.5).
pub const CPMM_CONFIGS: u16 = 21;

/// The manifest name of CPMM config `index`.
pub fn cpmm_config_name(index: u16) -> String {
    format!("cpmm_config_{index:02}")
}

/// The address of CPMM config `index` (the fixture's; equals PDA `["amm_config", index BE]`).
pub fn cpmm_config(index: u16) -> Pubkey {
    fixture(&cpmm_config_name(index)).address
}

/// `sha256("global:swap_base_input")[..8]`.
pub const CPMM_SWAP_BASE_INPUT: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];
/// `sha256("global:swap_base_output")[..8]`.
pub const CPMM_SWAP_BASE_OUTPUT: [u8; 8] = [55, 217, 98, 86, 163, 74, 180, 173];
/// `sha256("global:update_pool_status")[..8]` (admin).
pub const CPMM_UPDATE_POOL_STATUS: [u8; 8] = [130, 87, 108, 6, 46, 224, 117, 123];
/// `sha256("global:update_amm_config")[..8]` (admin).
pub const CPMM_UPDATE_AMM_CONFIG: [u8; 8] = [49, 60, 174, 136, 154, 28, 116, 200];
/// `sha256("event:SwapEvent")[..8]`.
pub const CPMM_SWAP_EVENT: [u8; 8] = [64, 198, 205, 232, 38, 8, 113, 226];

/// CPMM `NotApproved` (swap bit or before `open_time`).
pub const CPMM_ERR_NOT_APPROVED: u32 = 6000;
/// CPMM `ExceededSlippage`.
pub const CPMM_ERR_EXCEEDED_SLIPPAGE: u32 = 6005;
/// CPMM `ZeroTradingTokens` (the curve has no answer).
pub const CPMM_ERR_ZERO_TRADING_TOKENS: u32 = 6006;
/// CPMM `MathOverflow` (e.g. the fee counters' sum).
pub const CPMM_ERR_MATH_OVERFLOW: u32 = 6011;
/// CPMM `InsufficientVault` (a vault below its counters).
pub const CPMM_ERR_INSUFFICIENT_VAULT: u32 = 6012;
/// CPMM `InvalidFeeModel` (`creator_fee_on > 2`).
pub const CPMM_ERR_INVALID_FEE_MODEL: u32 = 6013;

/// A CPMM pool's keys (the config key as loaded; [`CpmmPool::config_key`] reads the live one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpmmPool {
    pub which: CpmmFixture,
    pub pool: Pubkey,
    pub amm_config: Pubkey,
    pub observation: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub lp_mint: Pubkey,
    /// WSOL is token 0 (else token 1).
    pub sol_is_token0: bool,
}

impl CpmmPool {
    fn from_state(which: CpmmFixture, pool: Pubkey, s: &raydium_cpmm::PoolState) -> Self {
        Self {
            which,
            pool,
            amm_config: key(&s.amm_config),
            observation: key(&s.observation_key),
            token_0_vault: key(&s.token_0_vault),
            token_1_vault: key(&s.token_1_vault),
            token_0_mint: key(&s.token_0_mint),
            token_1_mint: key(&s.token_1_mint),
            token_0_program: key(&s.token_0_program),
            token_1_program: key(&s.token_1_program),
            lp_mint: key(&s.lp_mint),
            sol_is_token0: s.sol_is_token0().expect("a SOL pool"),
        }
    }

    /// The token (non-WSOL) mint.
    pub fn token_mint(&self) -> Pubkey {
        if self.sol_is_token0 {
            self.token_1_mint
        } else {
            self.token_0_mint
        }
    }

    /// The token (non-WSOL) side's program.
    pub fn token_program(&self) -> TokenProgram {
        let p = if self.sol_is_token0 {
            self.token_1_program
        } else {
            self.token_0_program
        };
        if p == TokenProgram::Token2022.id() {
            TokenProgram::Token2022
        } else {
            TokenProgram::Spl
        }
    }

    /// The input token of direction `dir` is token 0.
    pub fn input_is_token0(&self, dir: Direction) -> bool {
        (dir == Direction::Buy) == self.sol_is_token0
    }

    /// `(input vault, output vault, input mint, output mint, input program, output program)`.
    pub fn io(&self, dir: Direction) -> (Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey) {
        let zero = (self.token_0_vault, self.token_0_mint, self.token_0_program);
        let one = (self.token_1_vault, self.token_1_mint, self.token_1_program);
        let (i, o) = if self.input_is_token0(dir) {
            (zero, one)
        } else {
            (one, zero)
        };
        (i.0, o.0, i.1, o.1, i.2, o.2)
    }

    /// The pool as the reader sees it now.
    pub fn state(&self, svm: &LiteSVM) -> raydium_cpmm::PoolState {
        let acc = svm.get_account(&self.pool).expect("pool");
        raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data).expect("PoolState")
    }

    /// The config key the pool names now.
    pub fn config_key(&self, svm: &LiteSVM) -> Pubkey {
        key(&self.state(svm).amm_config)
    }

    /// The pool's `AmmConfig` as the reader sees it now.
    pub fn config(&self, svm: &LiteSVM) -> raydium_cpmm::AmmConfig {
        let acc = svm.get_account(&self.config_key(svm)).expect("amm_config");
        raydium_cpmm::read_config(&acc.owner.to_bytes(), &acc.data).expect("AmmConfig")
    }

    /// `(token_0_vault.amount, token_1_vault.amount)`.
    pub fn vault_amounts(&self, svm: &LiteSVM) -> (u64, u64) {
        (
            vault_amount(svm, &self.token_0_vault),
            vault_amount(svm, &self.token_1_vault),
        )
    }

    /// The reader's pricing reserves `(r0, r1)`; `None` if the venue cannot price.
    pub fn reserves(&self, svm: &LiteSVM) -> Option<(u64, u64)> {
        let (v0, v1) = self.vault_amounts(svm);
        self.state(svm).reserves(v0, v1)
    }

    /// `(input reserve, output reserve)` of a direction.
    pub fn io_reserves(&self, svm: &LiteSVM, dir: Direction) -> Option<(u64, u64)> {
        let (r0, r1) = self.reserves(svm)?;
        Some(if self.input_is_token0(dir) {
            (r0, r1)
        } else {
            (r1, r0)
        })
    }

    /// `ARCH §5.3`'s predicate on the live state and config (M3-SEC-1's fee split included).
    pub fn venue_executable(&self, svm: &LiteSVM, now: i64) -> bool {
        let (v0, v1) = self.vault_amounts(svm);
        self.state(svm)
            .venue_executable(&self.config(svm), now, v0, v1)
    }

    /// short.win's resolved fees for `dir` (`fees::cpmm` on the reader's config and pool
    /// fields); `None` where the venue cannot swap (`fees::cpmm`'s `InvalidInput`).
    pub fn fees(&self, svm: &LiteSVM, dir: Direction) -> Option<CpmmFees> {
        let s = self.state(svm);
        let c = self.config(svm);
        mfees::cpmm(
            &CpmmConfig {
                trade_fee_rate: c.trade_fee_rate,
                protocol_fee_rate: c.protocol_fee_rate,
                fund_fee_rate: c.fund_fee_rate,
                creator_fee_rate: c.creator_fee_rate,
            },
            &CpmmPoolFees {
                enable_creator_fee: s.enable_creator_fee,
                creator_fee_on: s.creator_fee_on,
                sol_is_token0: self.sol_is_token0,
            },
            dir,
        )
        .ok()
    }

    /// `quote::cpmm::sell_exact_in` of `amount_in` in direction `dir` on the live state (the
    /// formula is direction-agnostic: input reserve, output reserve, the direction's fees).
    pub fn quote_exact_in(
        &self,
        svm: &LiteSVM,
        dir: Direction,
        amount_in: u64,
    ) -> Option<SellQuote> {
        let (x, y) = self.io_reserves(svm, dir)?;
        qcpmm::sell_exact_in(
            amount_in,
            u128::from(x),
            u128::from(y),
            &self.fees(svm, dir)?,
        )
    }

    /// `quote::cpmm::buy_exact_out` of `amount_out` in direction `dir` on the live state.
    pub fn quote_exact_out(
        &self,
        svm: &LiteSVM,
        dir: Direction,
        amount_out: u64,
    ) -> Option<BuyQuote> {
        let (x, y) = self.io_reserves(svm, dir)?;
        qcpmm::buy_exact_out(
            amount_out,
            u128::from(x),
            u128::from(y),
            &self.fees(svm, dir)?,
        )
    }

    /// The 13 `Swap` accounts (R-RAY §3.2): `[payer S, authority, amm_config, pool W, input W,
    /// output W, input_vault W, output_vault W, input_program, output_program, input_mint,
    /// output_mint, observation W]`, with the config key the pool names now.
    pub fn swap_accounts(
        &self,
        svm: &LiteSVM,
        payer: &Pubkey,
        input: &Pubkey,
        output: &Pubkey,
        dir: Direction,
    ) -> Vec<AccountMeta> {
        let (iv, ov, im, om, ip, op) = self.io(dir);
        vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new_readonly(RAYDIUM_CPMM_AUTHORITY, false),
            AccountMeta::new_readonly(self.config_key(svm), false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(*input, false),
            AccountMeta::new(*output, false),
            AccountMeta::new(iv, false),
            AccountMeta::new(ov, false),
            AccountMeta::new_readonly(ip, false),
            AccountMeta::new_readonly(op, false),
            AccountMeta::new_readonly(im, false),
            AccountMeta::new_readonly(om, false),
            AccountMeta::new(self.observation, false),
        ]
    }

    /// `swap_base_input(amount_in, minimum_amount_out)`.
    #[allow(clippy::too_many_arguments)]
    pub fn swap_base_input_ix(
        &self,
        svm: &LiteSVM,
        payer: &Pubkey,
        input: &Pubkey,
        output: &Pubkey,
        dir: Direction,
        amount_in: u64,
        minimum_amount_out: u64,
    ) -> Instruction {
        let mut data = CPMM_SWAP_BASE_INPUT.to_vec();
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out.to_le_bytes());
        Instruction {
            program_id: RAYDIUM_CPMM_PROGRAM_ID,
            accounts: self.swap_accounts(svm, payer, input, output, dir),
            data,
        }
    }

    /// `swap_base_output(max_amount_in, amount_out)`.
    #[allow(clippy::too_many_arguments)]
    pub fn swap_base_output_ix(
        &self,
        svm: &LiteSVM,
        payer: &Pubkey,
        input: &Pubkey,
        output: &Pubkey,
        dir: Direction,
        max_amount_in: u64,
        amount_out: u64,
    ) -> Instruction {
        let mut data = CPMM_SWAP_BASE_OUTPUT.to_vec();
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out.to_le_bytes());
        Instruction {
            program_id: RAYDIUM_CPMM_PROGRAM_ID,
            accounts: self.swap_accounts(svm, payer, input, output, dir),
            data,
        }
    }
}

// ---- users and swaps --------------------------------------------------------------------------

/// The amount of a token account (panics if it is not one).
pub fn vault_amount(svm: &LiteSVM, key: &Pubkey) -> u64 {
    read::token_amount(svm, key).unwrap_or_else(|| panic!("{key} is not a token account"))
}

/// Refills LiteSVM's airdrop account when a large wrap would drain it (setup only).
fn ensure_airdrop(svm: &mut LiteSVM, lamports: u64) {
    let key = svm.airdrop_pubkey();
    let have = svm.get_account(&key).map_or(0, |a| a.lamports);
    if have < lamports.saturating_add(1_000_000_000_000) {
        svm.set_account(
            key,
            Account {
                lamports: u64::MAX / 4,
                data: vec![],
                owner: anchor_lang::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap_or_else(|e| panic!("refill airdrop: {e:?}"));
    }
}

/// A new WSOL token account (SPL Token, not an ATA) owned by `owner` holding `lamports`
/// wrapped: created by the Token program on the dumped native mint, credited, `SyncNative`.
pub fn fund_wsol(env: &mut Env, owner: &Pubkey, lamports: u64) -> Pubkey {
    let payer = env.payer.insecure_clone();
    let spl = TOKEN_PROGRAM_ID;
    let acc = send_direct(&mut env.svm, |svm| {
        CreateAccount::new(svm, &payer, &WSOL_MINT)
            .owner(owner)
            .token_program_id(&spl)
            .send()
    })
    .unwrap_or_else(|e| panic!("create WSOL account: {:?}", e.err));
    if lamports > 0 {
        wrap(env, &acc, lamports);
    }
    acc
}

/// Adds `lamports` wrapped lamports to the WSOL account `acc` (credit + `SyncNative`). Both
/// sends go through [`send_direct`] (the blockhash expires after each), so repeating the same
/// top-up is not a duplicate transaction (FUZZ-M7-24H-2).
pub fn wrap(env: &mut Env, acc: &Pubkey, lamports: u64) {
    ensure_airdrop(&mut env.svm, lamports);
    send_direct(&mut env.svm, |svm| svm.airdrop(acc, lamports))
        .unwrap_or_else(|e| panic!("airdrop to {acc}: {:?}", e.err));
    let payer = env.payer.insecure_clone();
    let spl = TOKEN_PROGRAM_ID;
    send_direct(&mut env.svm, |svm| {
        SyncNative::new(svm, &payer, acc)
            .token_program_id(&spl)
            .send()
    })
    .unwrap_or_else(|e| panic!("sync_native: {:?}", e.err));
}

/// A new token account of `mint` (under `program`) owned by `owner`, created by the token
/// program, holding `amount`. The mainnet mints have no mint authority, so the balance is
/// planted in the account's `amount` field (the mint's supply is not adjusted; no venue reads
/// it).
pub fn fund_token(
    env: &mut Env,
    owner: &Pubkey,
    mint: &Pubkey,
    program: TokenProgram,
    amount: u64,
) -> Pubkey {
    let payer = env.payer.insecure_clone();
    let id = program.id();
    let acc = send_direct(&mut env.svm, |svm| {
        CreateAccount::new(svm, &payer, mint)
            .owner(owner)
            .token_program_id(&id)
            .send()
    })
    .unwrap_or_else(|e| panic!("create token account: {:?}", e.err));
    if amount > 0 {
        set_token_amount(&mut env.svm, &acc, amount);
    }
    acc
}

/// A user with a WSOL account and a token account of the pool's token.
pub struct Trader {
    pub owner: Keypair,
    pub wsol: Pubkey,
    pub token: Pubkey,
}

impl Trader {
    /// `(source, destination)` of a direction.
    pub fn accounts(&self, dir: Direction) -> (Pubkey, Pubkey) {
        match dir {
            Direction::Buy => (self.wsol, self.token),
            Direction::Sell => (self.token, self.wsol),
        }
    }
}

/// A funded user of a pool whose token is `mint` under `program`: `wsol` lamports wrapped and
/// `tokens` raw units.
pub fn trader(
    env: &mut Env,
    mint: &Pubkey,
    program: TokenProgram,
    wsol: u64,
    tokens: u64,
) -> Trader {
    let owner = env.funded_keypair();
    let w = fund_wsol(env, &owner.pubkey(), wsol);
    let t = fund_token(env, &owner.pubkey(), mint, program, tokens);
    Trader {
        owner,
        wsol: w,
        token: t,
    }
}

/// A swap's mode and bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swap {
    /// `(amount_in, minimum_amount_out)`.
    ExactIn(u64, u64),
    /// `(max_amount_in, amount_out)`.
    ExactOut(u64, u64),
}

/// Any dumped pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnyPool {
    V4(V4Pool),
    Cpmm(CpmmPool),
}

impl AnyPool {
    /// The pool's token mint and its program.
    pub fn token(&self) -> (Pubkey, TokenProgram) {
        match self {
            Self::V4(p) => (p.token_mint(), TokenProgram::Spl),
            Self::Cpmm(p) => (p.token_mint(), p.token_program()),
        }
    }

    /// The venue's swap instruction for `trader` (who signs as source owner / payer).
    pub fn swap_ix(
        &self,
        svm: &LiteSVM,
        owner: &Pubkey,
        trader: &Trader,
        dir: Direction,
        s: Swap,
    ) -> Instruction {
        let (src, dst) = trader.accounts(dir);
        match (self, s) {
            (Self::V4(p), Swap::ExactIn(a, m)) => p.swap_base_in_v2_ix(owner, &src, &dst, a, m),
            (Self::V4(p), Swap::ExactOut(m, q)) => p.swap_base_out_v2_ix(owner, &src, &dst, m, q),
            (Self::Cpmm(p), Swap::ExactIn(a, m)) => {
                p.swap_base_input_ix(svm, owner, &src, &dst, dir, a, m)
            }
            (Self::Cpmm(p), Swap::ExactOut(m, q)) => {
                p.swap_base_output_ix(svm, owner, &src, &dst, dir, m, q)
            }
        }
    }

    /// `(input reserve, output reserve)` of a direction on the live state.
    pub fn io_reserves(&self, svm: &LiteSVM, dir: Direction) -> Option<(u64, u64)> {
        match self {
            Self::V4(p) => p.io_reserves(svm, dir),
            Self::Cpmm(p) => p.io_reserves(svm, dir),
        }
    }

    /// The exact-in quote on the live state.
    pub fn quote_exact_in(
        &self,
        svm: &LiteSVM,
        dir: Direction,
        amount_in: u64,
    ) -> Option<SellQuote> {
        match self {
            Self::V4(p) => p.quote_exact_in(svm, dir, amount_in),
            Self::Cpmm(p) => p.quote_exact_in(svm, dir, amount_in),
        }
    }

    /// The exact-out quote on the live state.
    pub fn quote_exact_out(
        &self,
        svm: &LiteSVM,
        dir: Direction,
        amount_out: u64,
    ) -> Option<BuyQuote> {
        match self {
            Self::V4(p) => p.quote_exact_out(svm, dir, amount_out),
            Self::Cpmm(p) => p.quote_exact_out(svm, dir, amount_out),
        }
    }
}

/// A direct swap on `pool` by `trader` as a normal user (the trader signs; the harness payer
/// pays the fee), through [`Env::send`].
pub fn swap(env: &mut Env, pool: &AnyPool, trader: &Trader, dir: Direction, s: Swap) -> TxResult {
    let owner = trader.owner.pubkey();
    let ix = pool.swap_ix(&env.svm, &owner, trader, dir, s);
    let signer = trader.owner.insecure_clone();
    env.send(&[ix], &[&signer])
}

// ---- scenario builders (byte-level admin actions) --------------------------------------------

/// Rewrites account `key`'s data with `f` (lamports, owner and flags unchanged).
pub fn patch(svm: &mut LiteSVM, key: &Pubkey, f: impl FnOnce(&mut Vec<u8>)) {
    let mut acc = svm
        .get_account(key)
        .unwrap_or_else(|| panic!("no account {key}"));
    f(&mut acc.data);
    svm.set_account(*key, acc)
        .unwrap_or_else(|e| panic!("set_account({key}): {e:?}"));
}

fn put_u64(d: &mut [u8], off: usize, v: u64) {
    d[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// Sets a token account's `amount`. A native (WSOL) account also gets `lamports = rent
/// reserve + amount`, the state `SyncNative` would leave.
pub fn set_token_amount(svm: &mut LiteSVM, key: &Pubkey, amount: u64) {
    let mut acc = svm
        .get_account(key)
        .unwrap_or_else(|| panic!("no account {key}"));
    let t = token_account::read(&acc.owner.to_bytes(), &acc.data)
        .unwrap_or_else(|e| panic!("{key} is not a token account: {e:?}"));
    put_u64(&mut acc.data, token_account::AMOUNT_OFFSET, amount);
    if let Some(rent) = t.is_native {
        acc.lamports = rent.checked_add(amount).expect("lamports");
    }
    svm.set_account(*key, acc)
        .unwrap_or_else(|e| panic!("set_account({key}): {e:?}"));
}

/// AMM v4 `status` (the admin's `SetParams` param 0 accepts 1..=7; any `u64` here).
pub fn set_v4_status(svm: &mut LiteSVM, pool: &V4Pool, status: u64) {
    patch(svm, &pool.pool, |d| {
        put_u64(d, raydium_v4::OFF_STATUS, status)
    });
}

/// AMM v4 `pool_open_time` (`SetParams` param 3).
pub fn set_v4_open_time(svm: &mut LiteSVM, pool: &V4Pool, open_time: u64) {
    patch(svm, &pool.pool, |d| {
        put_u64(d, raydium_v4::OFF_POOL_OPEN_TIME, open_time)
    });
}

/// AMM v4 swap fee `num/den` (the swap-fee half of `SetParams` param 2).
pub fn set_v4_swap_fee(svm: &mut LiteSVM, pool: &V4Pool, num: u64, den: u64) {
    patch(svm, &pool.pool, |d| {
        put_u64(d, raydium_v4::OFF_SWAP_FEE_NUMERATOR, num);
        put_u64(d, raydium_v4::OFF_SWAP_FEE_DENOMINATOR, den);
    });
}

/// AMM v4 `need_take_pnl_{coin,pc}` (what `deposit` / `withdraw` accrue for `withdrawpnl`).
pub fn set_v4_need_take_pnl(svm: &mut LiteSVM, pool: &V4Pool, coin: u64, pc: u64) {
    patch(svm, &pool.pool, |d| {
        put_u64(d, raydium_v4::OFF_NEED_TAKE_PNL_COIN, coin);
        put_u64(d, raydium_v4::OFF_NEED_TAKE_PNL_PC, pc);
    });
}

/// CPMM pool `status` (the admin's `update_pool_status`).
pub fn set_cpmm_status(svm: &mut LiteSVM, pool: &CpmmPool, status: u8) {
    patch(svm, &pool.pool, |d| d[raydium_cpmm::OFF_STATUS] = status);
}

/// CPMM pool `open_time` (no instruction can change it on mainnet; a scenario builder for
/// pools created with a future open time).
pub fn set_cpmm_open_time(svm: &mut LiteSVM, pool: &CpmmPool, open_time: u64) {
    patch(svm, &pool.pool, |d| {
        put_u64(d, raydium_cpmm::OFF_OPEN_TIME, open_time)
    });
}

/// CPMM fee counters `(protocol, fund, creator)` of token 0 and token 1.
pub fn set_cpmm_counters(
    svm: &mut LiteSVM,
    pool: &CpmmPool,
    token_0: (u64, u64, u64),
    token_1: (u64, u64, u64),
) {
    patch(svm, &pool.pool, |d| {
        put_u64(d, raydium_cpmm::OFF_PROTOCOL_FEES_TOKEN_0, token_0.0);
        put_u64(d, raydium_cpmm::OFF_FUND_FEES_TOKEN_0, token_0.1);
        put_u64(d, raydium_cpmm::OFF_CREATOR_FEES_TOKEN_0, token_0.2);
        put_u64(d, raydium_cpmm::OFF_PROTOCOL_FEES_TOKEN_1, token_1.0);
        put_u64(d, raydium_cpmm::OFF_FUND_FEES_TOKEN_1, token_1.1);
        put_u64(d, raydium_cpmm::OFF_CREATOR_FEES_TOKEN_1, token_1.2);
    });
}

/// CPMM `enable_creator_fee` and `creator_fee_on` (fixed at pool creation on mainnet; a
/// scenario builder for the fee variants no dumped pool has, e.g. the fee on the token side).
pub fn set_cpmm_creator_fee(svm: &mut LiteSVM, pool: &CpmmPool, enable: bool, creator_fee_on: u8) {
    patch(svm, &pool.pool, |d| {
        d[raydium_cpmm::OFF_ENABLE_CREATOR_FEE] = u8::from(enable);
        d[raydium_cpmm::OFF_CREATOR_FEE_ON] = creator_fee_on;
    });
}

/// Points the pool at another `AmmConfig` (a fee-variant builder: the pool then prices and
/// swaps with that config's rates; the swap must pass the new key).
pub fn set_cpmm_pool_config(svm: &mut LiteSVM, pool: &CpmmPool, config: &Pubkey) {
    patch(svm, &pool.pool, |d| {
        d[raydium_cpmm::OFF_AMM_CONFIG..raydium_cpmm::OFF_AMM_CONFIG + 32]
            .copy_from_slice(config.as_ref());
    });
}

/// `AmmConfig` rates `(trade, protocol, fund, creator)` in ppm (the admin's
/// `update_amm_config` params 0, 1, 2, 7; every pool on the config at once).
pub fn set_cpmm_config_rates(
    svm: &mut LiteSVM,
    config: &Pubkey,
    trade: u64,
    protocol: u64,
    fund: u64,
    creator: u64,
) {
    patch(svm, config, |d| {
        put_u64(d, raydium_cpmm::OFF_CONFIG_TRADE_FEE_RATE, trade);
        put_u64(d, raydium_cpmm::OFF_CONFIG_PROTOCOL_FEE_RATE, protocol);
        put_u64(d, raydium_cpmm::OFF_CONFIG_FUND_FEE_RATE, fund);
        put_u64(d, raydium_cpmm::OFF_CONFIG_CREATOR_FEE_RATE, creator);
    });
}

// ---- the upgradeable loader's permissionless extension ------------------------------------------

/// The upgradeable loader's `ExtendProgram { additional_bytes }` (bincode tag 6) of `program`'s
/// ProgramData, `payer` paying the rent: permissionless since Agave 4.2 (agave
/// `common_extend_program` checks no authority; at least 10 KiB under SIMD-0431, one per program
/// per slot, up to the 10 MiB account cap). The loader appends `additional_bytes` zeros, redeploys
/// the same ELF and writes the current slot into the header (AIR-01: the venue latch's deploy
/// slot moves although the program did not change).
pub fn extend_program_ix(program: &Pubkey, payer: &Pubkey, additional_bytes: u32) -> Instruction {
    use anchor_lang::solana_program::bpf_loader_upgradeable;
    let mut data = 6u32.to_le_bytes().to_vec();
    data.extend_from_slice(&additional_bytes.to_le_bytes());
    Instruction {
        program_id: bpf_loader_upgradeable::ID,
        accounts: vec![
            AccountMeta::new(
                bpf_loader_upgradeable::get_program_data_address(program),
                false,
            ),
            AccountMeta::new(*program, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new(*payer, true),
        ],
        data,
    }
}

// ---- real admin instructions (forged admin signature) -----------------------------------------

/// Sends `ix` with its signer metas `forged` signed by nobody: LiteSVM's signature check is off
/// for this one transaction (the Raydium pool admin `GThUX1…` is a Squads vault whose key no
/// test holds), the harness payer pays. The invariant hooks run as in [`Env::send`] for a
/// one-instruction transaction: [`invariants::check_ix`] against the state before, then
/// [`invariants::check_all`] on every tracked market (the instruction's own markets too).
pub fn admin_send(env: &mut Env, ix: Instruction, forged: &[Pubkey]) -> TxResult {
    assert!(
        forged
            .iter()
            .all(|k| ix.accounts.iter().any(|m| m.is_signer && m.pubkey == *k)),
        "admin_send: every forged key must be a signer meta of the instruction"
    );
    for m in &ix.accounts {
        if read::zc::<Market>(&env.svm, &m.pubkey).is_some() {
            env.track_market(m.pubkey);
        }
    }
    let admins = env.admin_keys();
    let pre = invariants::snapshot(&env.svm, env.tracked_markets());
    let payer = env.payer.insecure_clone();
    let blockhash = env.svm.latest_blockhash();
    let mut tx = Transaction::new_unsigned(Message::new(
        std::slice::from_ref(&ix),
        Some(&payer.pubkey()),
    ));
    tx.try_partial_sign(&[&payer], blockhash)
        .unwrap_or_else(|e| panic!("sign: {e:?}"));
    let sigverify = env.svm.get_sigverify();
    env.svm = std::mem::take(&mut env.svm).with_sigverify(false);
    let res = env.svm.send_transaction(tx);
    env.svm = std::mem::take(&mut env.svm).with_sigverify(sigverify);
    env.svm.expire_blockhash();
    if env.check_invariants {
        if let Ok(meta) = &res {
            let ev = invariants::IxEvents::of_instruction(meta, 0, &ix);
            let admin = invariants::is_admin_signed(&ix, &admins);
            if let Err(v) = invariants::check_ix(&env.svm, &pre, admin, &ev) {
                panic!("{v}\nlogs:\n{}", meta.pretty_logs());
            }
        }
        env.assert_invariants();
    }
    res
}

/// The pool admin of both Raydium programs (`GThUX1…`, R-RAY §1.1).
pub fn raydium_admin() -> Pubkey {
    Pubkey::new_from_array(raydium_v4::POOL_ADMIN)
}

/// AMM v4 `SetParams` (tag 6) with a `u64` value: param 0 = status, 1 = state, 3 = open time.
pub fn v4_set_params_ix(pool: &V4Pool, param: u8, value: u64) -> Instruction {
    let mut data = vec![V4_TAG_SET_PARAMS, param];
    data.extend_from_slice(&value.to_le_bytes());
    Instruction {
        program_id: RAYDIUM_V4_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool.pool, false),
            AccountMeta::new_readonly(raydium_admin(), true),
        ],
        data,
    }
}

/// AMM v4 `Fees` (8 × u64 at `AmmInfo` offset 128, R-RAY §2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V4Fees {
    pub min_separate: (u64, u64),
    pub trade_fee: (u64, u64),
    pub pnl: (u64, u64),
    pub swap_fee: (u64, u64),
}

impl V4Fees {
    /// The fees the pool holds now.
    pub fn of(svm: &LiteSVM, pool: &V4Pool) -> Self {
        let d = svm.get_account(&pool.pool).expect("pool").data;
        let at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().expect("u64"));
        Self {
            min_separate: (at(128), at(136)),
            trade_fee: (at(144), at(152)),
            pnl: (at(160), at(168)),
            swap_fee: (at(176), at(184)),
        }
    }

    fn pack(&self) -> Vec<u8> {
        [self.min_separate, self.trade_fee, self.pnl, self.swap_fee]
            .iter()
            .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
            .collect()
    }
}

/// AMM v4 `SetParams` param 2 (fees: every fraction must satisfy `num < den`).
pub fn v4_set_fees_ix(pool: &V4Pool, fees: &V4Fees) -> Instruction {
    let mut data = vec![V4_TAG_SET_PARAMS, 2];
    data.extend_from_slice(&fees.pack());
    Instruction {
        program_id: RAYDIUM_V4_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool.pool, false),
            AccountMeta::new_readonly(raydium_admin(), true),
        ],
        data,
    }
}

/// CPMM `update_pool_status(status)` (admin).
pub fn cpmm_update_pool_status_ix(pool: &CpmmPool, status: u8) -> Instruction {
    let mut data = CPMM_UPDATE_POOL_STATUS.to_vec();
    data.push(status);
    Instruction {
        program_id: RAYDIUM_CPMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(raydium_admin(), true),
            AccountMeta::new(pool.pool, false),
        ],
        data,
    }
}

/// CPMM `update_amm_config(param, value)` (admin): 0 trade fee, 1 protocol, 2 fund, 7 creator
/// fee rate (ppm).
pub fn cpmm_update_amm_config_ix(config: &Pubkey, param: u8, value: u64) -> Instruction {
    let mut data = CPMM_UPDATE_AMM_CONFIG.to_vec();
    data.push(param);
    data.extend_from_slice(&value.to_le_bytes());
    Instruction {
        program_id: RAYDIUM_CPMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(raydium_admin(), true),
            AccountMeta::new(*config, false),
        ],
        data,
    }
}

/// Runs a Raydium admin instruction as the pool admin ([`admin_send`] with `GThUX1…` forged).
pub fn raydium_admin_send(env: &mut Env, ix: Instruction) -> TxResult {
    admin_send(env, ix, &[raydium_admin()])
}

// ---- logs ------------------------------------------------------------------------------------

/// An AMM v4 `ray_log` swap record (bincode, R-RAY §3.1): `log_type` 3 = `SwapBaseIn`
/// (`amount` = amount in, `bound` = minimum out, `result` = amount out) or 4 = `SwapBaseOut`
/// (`amount` = max in, `bound` = amount out, `result` = the input deducted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RayLog {
    pub log_type: u8,
    pub amount: u64,
    pub bound: u64,
    /// 1 = PC2Coin (pc in), 2 = Coin2PC (coin in).
    pub direction: u64,
    pub user_source: u64,
    /// The coin reserve the swap priced on (`vault − need_take_pnl`).
    pub pool_coin: u64,
    /// The pc reserve the swap priced on.
    pub pool_pc: u64,
    pub result: u64,
}

/// Every `ray_log` swap record in `logs`.
pub fn ray_logs(logs: &[String]) -> Vec<RayLog> {
    logs.iter()
        .filter_map(|l| l.strip_prefix("Program log: ray_log: "))
        .filter_map(|b| base64_decode(b.trim()))
        .filter(|b| b.len() == 57 && matches!(b[0], 3 | 4))
        .map(|b| {
            let u = |i: usize| u64::from_le_bytes(b[1 + 8 * i..9 + 8 * i].try_into().expect("u64"));
            RayLog {
                log_type: b[0],
                amount: u(0),
                bound: u(1),
                direction: u(2),
                user_source: u(3),
                pool_coin: u(4),
                pool_pc: u(5),
                result: u(6),
            }
        })
        .collect()
}

/// CPMM's `SwapEvent` (`states/events.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpmmSwapEvent {
    pub pool_id: Pubkey,
    /// The input reserve the swap priced on (vault − counters).
    pub input_vault_before: u64,
    /// The output reserve the swap priced on.
    pub output_vault_before: u64,
    pub input_amount: u64,
    pub output_amount: u64,
    pub input_transfer_fee: u64,
    pub output_transfer_fee: u64,
    pub base_input: bool,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub trade_fee: u64,
    pub creator_fee: u64,
    pub creator_fee_on_input: bool,
}

/// Every CPMM `SwapEvent` in `logs` (`emit!`: "Program data: " + base64).
pub fn cpmm_swap_events(logs: &[String]) -> Vec<CpmmSwapEvent> {
    logs.iter()
        .filter_map(|l| l.strip_prefix("Program data: "))
        .filter_map(|b| base64_decode(b.trim()))
        .filter(|b| b.len() == 8 + 32 + 6 * 8 + 1 + 64 + 16 + 1 && b[..8] == CPMM_SWAP_EVENT)
        .map(|b| {
            let u = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().expect("u64"));
            let k = |o: usize| Pubkey::new_from_array(b[o..o + 32].try_into().expect("key"));
            CpmmSwapEvent {
                pool_id: k(8),
                input_vault_before: u(40),
                output_vault_before: u(48),
                input_amount: u(56),
                output_amount: u(64),
                input_transfer_fee: u(72),
                output_transfer_fee: u(80),
                base_input: b[88] != 0,
                input_mint: k(89),
                output_mint: k(121),
                trade_fee: u(153),
                creator_fee: u(161),
                creator_fee_on_input: b[169] != 0,
            }
        })
        .collect()
}

/// The logs of a successful transaction.
pub fn logs(meta: &TransactionMetadata) -> &[String] {
    &meta.logs
}

/// Standard base64 (with padding) decoding; `None` on a malformed string.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let b = n.to_be_bytes();
        out.extend_from_slice(&b[1..4 - pad]);
    }
    Some(out)
}

// ---- sha256 ----------------------------------------------------------------------------------

/// SHA-256 (FIPS 180-4), for checking the fixture bytes against the manifest without another
/// dependency.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    let mut w = [0u32; 64];
    for block in msg.as_chunks::<64>().0 {
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    let mut out = [0u8; 32];
    for (o, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *o = x.to_be_bytes();
    }
    out
}

/// Lowercase hex of `sha256(data)`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

// ---- the pump fork (M5) ------------------------------------------------------------------------

/// The slot and block time every pump venue-state fixture was read at (group `pump_state`).
pub fn pump_snapshot() -> Snapshot {
    pump_fixtures().state
}

/// The slot and block time the PumpSwap state was read at (group `pamm_state`, M6).
pub fn pamm_snapshot() -> Snapshot {
    pamm_fixtures().state
}

/// The mint of PumpSwap pool role `role` (the `pamm_state` snapshot's `pools`).
pub fn pamm_role_mint(role: &str) -> Pubkey {
    *pamm_fixtures()
        .coins
        .get(role)
        .unwrap_or_else(|| panic!("no PumpSwap pool role `{role}` in the snapshot"))
}

/// Every PumpSwap-state fixture (`pamm_state` and `pamm_synthetic`).
pub fn all_pamm_fixtures() -> impl Iterator<Item = &'static Fixture> {
    pamm_fixtures().by_name.values()
}

/// Whether the PumpSwap dump found the account at `address` absent.
pub fn is_pamm_absent(address: &Pubkey) -> bool {
    pamm_fixtures().absent.contains(address)
}

/// Whether the dump found the pump-group account at `address` absent (recorded as
/// `"absent": true`: a creator vault that never received a fee, a closed boost vault, an event
/// authority, ...). Such an account is not loaded.
pub fn is_absent(address: &Pubkey) -> bool {
    pump_fixtures().absent.contains(address)
}

/// The pump-group fixture at `address`, if the pump groups hold one.
pub fn pump_fixture_at(address: &Pubkey) -> Option<&'static Fixture> {
    let f = pump_fixtures();
    f.by_address.get(address).map(|n| &f.by_name[n])
}

/// Writes the pump-group fixture at `address` into the VM if the dump holds one (else nothing:
/// absent on mainnet at the snapshot). Returns whether it wrote one.
pub fn load_pump_at(svm: &mut LiteSVM, address: &Pubkey) -> bool {
    match pump_fixture_at(address) {
        Some(f) => {
            load(svm, &f.name);
            true
        }
        None => false,
    }
}

/// The three pump programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpProgram {
    /// The bonding curve `6EF8rr…`.
    Pump,
    /// pump-fees `pfeeUx…` (`get_fees_with_quote_mint`, the `FeeConfig`s).
    Fees,
    /// PumpSwap `pAMMBa…`.
    Amm,
}

impl PumpProgram {
    pub const ALL: [Self; 3] = [Self::Pump, Self::Fees, Self::Amm];

    /// The fixture name prefix of its [`PumpDeploy::CURRENT`] build.
    fn prefix(self) -> &'static str {
        PumpDeploy::CURRENT.prefix(self)
    }

    /// The program id.
    pub fn id(self) -> Pubkey {
        match self {
            Self::Pump => PUMP_PROGRAM_ID,
            Self::Fees => PUMP_FEES_PROGRAM_ID,
            Self::Amm => PUMP_AMM_PROGRAM_ID,
        }
    }

    /// Its `VenueGuard` index (the venue-upgrade latch, A7).
    pub fn guard(self) -> u8 {
        match self {
            Self::Pump => GUARD_PUMP,
            Self::Fees => GUARD_PUMP_FEES,
            Self::Amm => GUARD_PUMP_AMM,
        }
    }

    /// The fixture of its ProgramData account ([`PumpDeploy::CURRENT`]).
    pub fn program_data(self) -> &'static Fixture {
        fixture(&format!("{}_program_data", self.prefix()))
    }

    /// The fixture of its program account ([`PumpDeploy::CURRENT`]).
    pub fn program(self) -> &'static Fixture {
        fixture(&format!("{}_program", self.prefix()))
    }

    /// The dumped ELF (the ProgramData after its 45-byte header, up to the ELF's own end) of
    /// [`PumpDeploy::CURRENT`].
    pub fn elf(self) -> &'static [u8] {
        PumpDeploy::CURRENT.elf(self)
    }
}

/// pump's deploy slot since its 2026-10-02 redeploy (group `pump_programs_2026_10`).
pub const PUMP_DEPLOY_SLOT_2026_10: u64 = 452_654_932;
/// PumpSwap's deploy slot since its 2026-10-02 redeploy (group `pump_programs_2026_10`).
pub const PUMP_AMM_DEPLOY_SLOT_2026_10: u64 = 452_654_882;
/// pump-fees' deploy slot since its 2026-10-02 redeploy (group `pump_programs_2026_10`).
pub const PUMP_FEES_DEPLOY_SLOT_2026_10: u64 = 452_655_002;
/// What pump's 2026-10-02 redeploy grows a shorter curve to on a v2 trade (the layout through its
/// fee buckets `creator_fee` @125 and `protocol_fees` @133; `shortwin_venue_state::pump::
/// CURVE_LEN_V2` = 125, the 2026-09 build's target, is the reader's layout and stays).
pub const CURVE_LEN_2026_10: usize = 141;
/// pump's deploy slot since its 2026-10-08 redeploy (group `pump_programs_2026_10_13`).
pub const PUMP_DEPLOY_SLOT_2026_10_08: u64 = 454_596_459;
/// PumpSwap's deploy slot since its 2026-10-08 redeploy (group `pump_programs_2026_10_13`).
pub const PUMP_AMM_DEPLOY_SLOT_2026_10_08: u64 = 454_596_406;
/// pump-fees' deploy slot since its 2026-10-12 redeploy (group `pump_programs_2026_10_13`; an
/// unexamined 2026-10-08 build at 454,596,501 preceded it).
pub const PUMP_FEES_DEPLOY_SLOT_2026_10_12: u64 = 456_132_171;
/// What `create_v2` allocates and every v2 / v3 trade grows a shorter curve to since pump's
/// 2026-10-08 redeploy (the layout through `post_complete_quote_in` @158: the fee buckets @125 /
/// @133, `depth` @141, `initial_virtual_quote_reserves` @142, `post_complete_base_out` @150).
pub const CURVE_LEN_2026_10_08: usize = 166;
/// pump's deploy slot since its 2026-10-13 15:40 UTC redeploy (group `pump_programs_2026_10_13b`;
/// `VENUE_GUARD_SLOTS_2026_10_13`'s pump slot).
pub const PUMP_DEPLOY_SLOT_2026_10_13: u64 = 456_494_698;

/// A curve's fee buckets `(creator_fee @125, protocol_fees @133)` from its data (0 past the
/// account's end: a curve shorter than 141 bytes carries none).
pub fn curve_fee_buckets(data: &[u8]) -> (u64, u64) {
    let at = |o: usize| {
        data.get(o..o + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
    };
    (at(125), at(133))
}

/// The rent pump takes from a trading user to grow a curve of `len` bytes, holding `lamports`,
/// `rq` = `real_quote_reserves` and `buckets` = `creator_fee + protocol_fees`, to `grows_to`
/// bytes: its rent reserve `lamports − rQ − buckets` topped up to
/// `Rent.minimum_balance(grows_to)`; 0 on a curve at that length or with the reserve already
/// there (the dump's over-funded 115-byte curves). The harness's one model of the growth
/// ([`PumpCurve::growth_top_up`], the I13 hook's `venue_rent`, T-PUMP-MATH's checks); pump's
/// 2026-10-08 build counts the reserve without the buckets on v2 and v3 trades alike
/// (`pump_math_it::a_bucketed_short_curve_grows_with_its_reserve_net_of_the_buckets`). The
/// older builds' targets (141, 125) lie at or before the buckets' end, so no curve they grow
/// carries any.
pub fn curve_growth_top_up(
    svm: &LiteSVM,
    len: usize,
    lamports: u64,
    rq: u64,
    buckets: u64,
    grows_to: usize,
) -> u64 {
    if len >= grows_to {
        return 0;
    }
    let reserve = lamports.saturating_sub(rq).saturating_sub(buckets);
    svm.minimum_balance_for_rent_exemption(grows_to)
        .saturating_sub(reserve)
}

/// The builds of the three pump programs the fixtures hold, each at the same program and
/// ProgramData addresses with the same upgrade authority (`7gZufw…`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpDeploy {
    /// Deploy slots 449,734,335 / 446,462,733 / 446,465,969 (group `pump_programs`): the builds
    /// R-PUMP, R-VEN and `VENUE_GUARD_SLOTS_2026_09_27` name, on which M5 and M6 were built.
    Sep2026,
    /// The 2026-10-02 redeploys ([`PUMP_DEPLOY_SLOT_2026_10`], [`PUMP_AMM_DEPLOY_SLOT_2026_10`],
    /// [`PUMP_FEES_DEPLOY_SLOT_2026_10`]; group `pump_programs_2026_10`;
    /// `VENUE_GUARD_SLOTS_2026_10_03`): the release whose unreleased artifacts `pump-rust-client`
    /// 0.2.0 shipped (v2 PumpSwap and v3 curve trades with fee buckets; `tests/fixtures/
    /// README.md`). short.win's instructions and account lists are unchanged.
    Oct2026,
    /// pump / PumpSwap 2026-10-08, pump-fees 2026-10-12 ([`PUMP_DEPLOY_SLOT_2026_10_08`],
    /// [`PUMP_AMM_DEPLOY_SLOT_2026_10_08`], [`PUMP_FEES_DEPLOY_SLOT_2026_10_12`]; group
    /// `pump_programs_2026_10_13`): the synthetic migration (a `buy_v3` past `rT` completes the
    /// curve and buys the rest from the would-be pool), `multi_hop_curve_swap` and PumpSwap's
    /// `multi_hop_swap`, the 166-byte curve ([`CURVE_LEN_2026_10_08`]), the 1,088-byte `Global`,
    /// and pump-fees' undocumented `withdraw`. short.win's instructions and account lists are
    /// unchanged. Mainnet's until pump's 2026-10-13 redeploy ([`Self::Oct2026Chain`]); kept for
    /// the differential (`pump_math_it::the_2026_10_13_build_runs_short_wins_paths_as_the_2026_10_08_build`).
    Oct2026Synth,
    /// pump 2026-10-13 ([`PUMP_DEPLOY_SLOT_2026_10_13`]; group `pump_programs_2026_10_13b`),
    /// PumpSwap 2026-10-08 and pump-fees 2026-10-12 as in [`Self::Oct2026Synth`]
    /// (`VENUE_GUARD_SLOTS_2026_10_13`): the nested quote chain — `create_v2` of a coin quoted in
    /// a pump coin of depth ≥ 1 requires a bonding-curve account for every coin down to the
    /// depth-0 root (`QuoteChainAccountsRequired`, 6109) and caps the depth at 5. No instruction,
    /// account, event or layout changed; outside `create_v2` the ELF differs from the 2026-10-08
    /// one in its error tables and source line numbers only (`tests/fixtures/README.md`).
    Oct2026Chain,
}

impl PumpDeploy {
    /// The builds every fork loads ([`PumpProgram`], [`Env::load_pump_programs`]).
    pub const CURRENT: Self = Self::Oct2026Chain;

    /// Every build the fixtures hold, oldest first.
    pub const ALL: [Self; 4] = [
        Self::Sep2026,
        Self::Oct2026,
        Self::Oct2026Synth,
        Self::Oct2026Chain,
    ];

    fn prefix(self, program: PumpProgram) -> &'static str {
        match (self, program) {
            (Self::Sep2026, PumpProgram::Pump) => "pump",
            (Self::Sep2026, PumpProgram::Fees) => "pump_fees",
            (Self::Sep2026, PumpProgram::Amm) => "pump_amm",
            (Self::Oct2026, PumpProgram::Pump) => "pump_2026_10",
            (Self::Oct2026, PumpProgram::Fees) => "pump_fees_2026_10",
            (Self::Oct2026, PumpProgram::Amm) => "pump_amm_2026_10",
            (Self::Oct2026Synth, PumpProgram::Pump) => "pump_2026_10_08",
            (Self::Oct2026Synth | Self::Oct2026Chain, PumpProgram::Fees) => "pump_fees_2026_10_12",
            (Self::Oct2026Synth | Self::Oct2026Chain, PumpProgram::Amm) => "pump_amm_2026_10_08",
            (Self::Oct2026Chain, PumpProgram::Pump) => "pump_2026_10_13",
        }
    }

    /// Whether this deploy and `other` hold the same build of `program` (PumpSwap and pump-fees
    /// of [`Self::Oct2026Synth`] and [`Self::Oct2026Chain`]).
    pub fn same_build(self, other: Self, program: PumpProgram) -> bool {
        self.prefix(program) == other.prefix(program)
    }

    /// The deploy slot `program`'s ProgramData header carries in this build.
    pub fn deploy_slot(self, program: PumpProgram) -> u64 {
        let g = usize::from(program.guard());
        match (self, program) {
            (Self::Sep2026, _) => VENUE_GUARD_SLOTS_2026_09_27[g],
            (Self::Oct2026, _) => VENUE_GUARD_SLOTS_2026_10_03[g],
            (Self::Oct2026Synth, PumpProgram::Pump) => PUMP_DEPLOY_SLOT_2026_10_08,
            (Self::Oct2026Synth, PumpProgram::Amm) => PUMP_AMM_DEPLOY_SLOT_2026_10_08,
            (Self::Oct2026Synth, PumpProgram::Fees) => PUMP_FEES_DEPLOY_SLOT_2026_10_12,
            (Self::Oct2026Chain, _) => FIXTURE_GUARD_SLOTS[g],
        }
    }

    /// The fixture of `program`'s ProgramData account in this build.
    pub fn program_data(self, program: PumpProgram) -> &'static Fixture {
        fixture(&format!("{}_program_data", self.prefix(program)))
    }

    /// The fixture of `program`'s program account in this build (the same bytes in both).
    pub fn program(self, program: PumpProgram) -> &'static Fixture {
        fixture(&format!("{}_program", self.prefix(program)))
    }

    /// `program`'s dumped ELF in this build.
    pub fn elf(self, program: PumpProgram) -> &'static [u8] {
        let pd = self.program_data(program);
        let start = shortwin_venue_state::program_data::HEADER_LEN;
        let len = pd.elf_len.expect("a ProgramData fixture");
        &pd.account.data[start..start + len]
    }

    /// The build whose ELF `program`'s ProgramData in `svm` holds — as loaded
    /// ([`load_pump_deploy`]) or as the loader's `Upgrade` wrote it (T-LATCH) — told apart by the
    /// ELF header (the 64 bytes after the loader's header: the builds' entry points and
    /// section-table offsets differ). A build two deploys share (PumpSwap and pump-fees of
    /// [`Self::Oct2026Synth`] and [`Self::Oct2026Chain`]) answers as the newer
    /// ([`Self::same_build`] compares). `None` for any other bytes: no account, a synthetic header
    /// without an ELF. Read in place, so the invariant hooks can ask on every transaction.
    pub fn held(svm: &LiteSVM, program: PumpProgram) -> Option<Self> {
        const ELF_HEADER: usize = 64;
        let start = shortwin_venue_state::program_data::HEADER_LEN;
        let acc = svm
            .accounts_db()
            .get_account_ref(&program.program_data().address)?;
        let held = acc.data().get(start..start + ELF_HEADER)?;
        Self::ALL
            .into_iter()
            .rev()
            .find(|d| &d.elf(program)[..ELF_HEADER] == held)
    }

    /// The size this build's trades grow a shorter curve to (rent topped up from the trader): 125
    /// (`shortwin_venue_state::pump::CURVE_LEN_V2`, R-PUMP U10), 141 since the 2026-10-02
    /// redeploy ([`CURVE_LEN_2026_10`]), 166 since the 2026-10-08 one ([`CURVE_LEN_2026_10_08`];
    /// the 2026-10-13 one keeps it).
    pub fn curve_len(self) -> usize {
        match self {
            Self::Sep2026 => pump::CURVE_LEN_V2,
            Self::Oct2026 => CURVE_LEN_2026_10,
            Self::Oct2026Synth | Self::Oct2026Chain => CURVE_LEN_2026_10_08,
        }
    }

    /// The size this build's trades grow a shorter PumpSwap pool to (rent topped up from the
    /// trader): 271 (`shortwin_venue_state::pump_amm::POOL_LEN_CREATE`, R-VEN H8), 287 since the
    /// 2026-10-02 redeploy ([`crate::pamm::POOL_LEN_2026_10`]; the 2026-10-08 one keeps it).
    pub fn pool_len(self) -> usize {
        match self {
            Self::Sep2026 => pump_amm::POOL_LEN_CREATE,
            Self::Oct2026 | Self::Oct2026Synth | Self::Oct2026Chain => {
                crate::pamm::POOL_LEN_2026_10
            }
        }
    }
}

/// pump's `buy_v3` (`07051dc4…`), new in its 2026-10-02 redeploy.
pub const PUMP_BUY_V3: [u8; 8] = [0x07, 0x05, 0x1d, 0xc4, 0xf5, 0x17, 0x65, 0x50];
/// pump's `multi_hop_curve_swap` (`e19a7516…`), new in its 2026-10-08 redeploy.
pub const PUMP_MULTI_HOP_CURVE_SWAP: [u8; 8] = [0xe1, 0x9a, 0x75, 0x16, 0xd7, 0x56, 0xf6, 0x67];
/// pump's `set_max_curve_depth` (`209a98db…`), new in its 2026-10-08 redeploy (an admin
/// instruction: the fallback probe if Anchor ever checks a signer before the key count).
pub const PUMP_SET_MAX_CURVE_DEPTH: [u8; 8] = [0x20, 0x9a, 0x98, 0xdb, 0x8e, 0x6c, 0xda, 0xa5];
/// PumpSwap's `multi_hop_swap` (`2b644913…`), new in its 2026-10-08 redeploy.
pub const PAMM_MULTI_HOP_SWAP: [u8; 8] = [0x2b, 0x64, 0x49, 0x13, 0xe9, 0xf6, 0x6f, 0x94];
/// pump-fees' undocumented `withdraw` (`b712469c…`, `sha256("global:withdraw")[..8]`), new in its
/// 2026-10-12 redeploy (in no IDL; PDA seed `referral-vault`).
pub const PUMP_FEES_WITHDRAW: [u8; 8] = [0xb7, 0x12, 0x46, 0x9c, 0x94, 0x6d, 0xa1, 0x22];
/// pump's `create_v2` (`d6904cec…`, pump-public-docs `2293f9a6`'s IDL).
pub const PUMP_CREATE_V2: [u8; 8] = [0xd6, 0x90, 0x4c, 0xec, 0x5f, 0x8b, 0x31, 0xb4];
/// The mayhem program `create_v2` names (`mayhem_program_id`; its PDAs `global-params`,
/// `sol-vault`, `mayhem-state`), read only for a mayhem-mode coin.
pub const PUMP_MAYHEM_PROGRAM: Pubkey =
    Pubkey::from_str_const("MAyhSmzXzV1pTf7LsNkrNwkWKTo4ougAJ1PPg47MD4e");
/// `sha256("account:QuoteControl")[..8]`.
pub const PUMP_QUOTE_CONTROL_DISC: [u8; 8] = [56, 244, 35, 238, 193, 213, 162, 201];
/// pump `CurveDepthExceeded`: a new curve deeper than `Global.max_curve_depth` (2026-10-08) or
/// than 5 (2026-10-13).
pub const PUMP_ERR_CURVE_DEPTH_EXCEEDED: u32 = 6105;
/// pump `QuoteCurveAwaitingMigration`: a quote coin (2026-10-13: any coin of its chain) is
/// complete and not yet migrated.
pub const PUMP_ERR_QUOTE_CURVE_AWAITING_MIGRATION: u32 = 6107;
/// pump `QuoteChainAccountsRequired` (new in the 2026-10-13 redeploy): `create_v2` of a coin
/// quoted in a nested pump coin lacks the bonding curve of a coin of that coin's quote chain.
pub const PUMP_ERR_QUOTE_CHAIN_ACCOUNTS_REQUIRED: u32 = 6109;
/// What the 2026-10-13 build appends to `CurveDepthExceeded`'s message: the depth cap it added
/// (`… exceeds Global.max_curve_depth or the program's cap of 5`).
pub const PUMP_DEPTH_CAP_MESSAGE: &str = "or the program's cap of 5";
/// pump `Global.max_curve_depth`: the byte the 2026-10-08 redeploy appended to the 1,087-byte
/// `Global` ([`pump::GLOBAL_LEN`]). The dumped `Global` (`pump_state_2026_10_13`, slot
/// 456,395,752) holds 1; mainnet holds 2 since slot 456,495,021 (`SetMaxCurveDepth`, tx
/// `44M1UKoP…`, 323 slots after the 2026-10-13 redeploy).
pub const PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH: usize = pump::GLOBAL_LEN;

/// pump's `QuoteControl` PDA (`["quote-control"]`).
pub fn pump_quote_control() -> Pubkey {
    Pubkey::find_program_address(&[b"quote-control"], &PUMP_PROGRAM_ID).0
}

/// A `QuoteControl` that admits no mint (`admin`, `reserves_admin`, 32 reserved bytes, `mints =
/// []`), owned by pump and rent-exempt: what `create_v2` reads before it admits a pump coin as a
/// quote (the dump holds no `QuoteControl`).
pub fn empty_quote_control(svm: &LiteSVM) -> Account {
    let mut data = PUMP_QUOTE_CONTROL_DISC.to_vec();
    data.extend_from_slice(&[0x51; 64]);
    data.extend_from_slice(&[0; 32]);
    data.extend_from_slice(&0u32.to_le_bytes());
    Account {
        lamports: svm.minimum_balance_for_rent_exemption(data.len()),
        data,
        owner: PUMP_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// pump `create_v2(name, symbol, uri, creator = user, is_mayhem_mode = false,
/// is_cashback_enabled = [false], creator_fee_bps = [0], is_holder_reward = [false])` of the
/// Token-2022 coin `mint` (a new keypair, signer) by `user`, in the IDL's account order (the
/// mayhem accounts derived, unused off mayhem mode), then `remaining`: none for a SOL quote,
/// [`pump_coin_quote_accounts`] for a pump coin's.
pub fn pump_create_v2_ix(user: &Pubkey, mint: &Pubkey, remaining: Vec<AccountMeta>) -> Instruction {
    let pid = |seeds: &[&[u8]], p: &Pubkey| Pubkey::find_program_address(seeds, p).0;
    let curve = crate::pda::bonding_curve(mint);
    let sol_vault = pid(&[b"sol-vault"], &PUMP_MAYHEM_PROGRAM);
    let mut data = PUMP_CREATE_V2.to_vec();
    for s in ["probe", "PRB", "https://example.invalid/probe.json"] {
        data.extend_from_slice(&u32::try_from(s.len()).expect("short").to_le_bytes());
        data.extend_from_slice(s.as_bytes());
    }
    data.extend_from_slice(user.as_ref());
    data.push(0);
    data.push(0);
    data.extend_from_slice(&0u64.to_le_bytes());
    data.push(0);
    let mut accounts = vec![
        AccountMeta::new(*mint, true),
        AccountMeta::new_readonly(pid(&[b"mint-authority"], &PUMP_PROGRAM_ID), false),
        AccountMeta::new(curve, false),
        AccountMeta::new(crate::pda::ata(&curve, mint, &TOKEN_2022_PROGRAM_ID), false),
        AccountMeta::new_readonly(PUMP_GLOBAL, false),
        AccountMeta::new(*user, true),
        AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
        AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
        AccountMeta::new(PUMP_MAYHEM_PROGRAM, false),
        AccountMeta::new_readonly(pid(&[b"global-params"], &PUMP_MAYHEM_PROGRAM), false),
        AccountMeta::new(sol_vault, false),
        AccountMeta::new(
            pid(&[b"mayhem-state", mint.as_ref()], &PUMP_MAYHEM_PROGRAM),
            false,
        ),
        AccountMeta::new(
            crate::pda::ata(&sol_vault, mint, &TOKEN_2022_PROGRAM_ID),
            false,
        ),
        AccountMeta::new_readonly(PUMP_EVENT_AUTHORITY, false),
        AccountMeta::new_readonly(PUMP_PROGRAM_ID, false),
    ];
    accounts.extend(remaining);
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts,
        data,
    }
}

/// `create_v2`'s remaining accounts quoting the new coin `mint` in pump coin `q` (pump-public-docs
/// `2293f9a6`): `quote_mint`, `associated_quote_bonding_curve` (the new curve's ATA of it),
/// `quote_token_program`, the `QuoteControl` PDA, `q`'s bonding curve, then `chain` (since the
/// 2026-10-13 build: the curve of every coin below `q` down to the depth-0 root).
pub fn pump_coin_quote_accounts(
    mint: &Pubkey,
    q: &PumpCurve,
    chain: &[Pubkey],
) -> Vec<AccountMeta> {
    let program = q.token_program.id();
    let curve = crate::pda::bonding_curve(mint);
    let mut v = vec![
        AccountMeta::new_readonly(q.mint, false),
        AccountMeta::new(crate::pda::ata(&curve, &q.mint, &program), false),
        AccountMeta::new_readonly(program, false),
        AccountMeta::new_readonly(pump_quote_control(), false),
        AccountMeta::new_readonly(q.curve, false),
    ];
    v.extend(chain.iter().map(|k| AccountMeta::new_readonly(*k, false)));
    v
}

/// Anchor's `InstructionFallbackNotFound`: the discriminator names no instruction.
pub const ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND: u32 = 101;
/// Anchor's `AccountNotEnoughKeys`.
pub const ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS: u32 = 3005;

impl Env {
    /// The Anchor error `program` answers instruction `disc` with when it is sent without any
    /// account: [`ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND`] if the build has no such instruction,
    /// [`ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS`] if it dispatches it and then finds no accounts.
    pub fn pump_probe(&mut self, program: PumpProgram, disc: [u8; 8]) -> Option<u32> {
        let mut data = disc.to_vec();
        data.extend_from_slice(&[0; 16]);
        let probe = Instruction {
            program_id: program.id(),
            accounts: vec![],
            data,
        };
        let res = self.send(&[probe], &[]);
        crate::errors::custom_code(&res)
    }

    /// pump's answer to `create_v2` of a new coin quoted in the `synthetic_stuck` coin (depth 1,
    /// so the new curve would be depth 2) with `Global.max_curve_depth` at 1 for the call: the
    /// dumped `Global`'s value, because mainnet's is 2 since slot 456,495,021
    /// ([`PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH`]) and at 2 every build answers 6107 instead
    /// (`synthetic_stuck`'s curve is complete and unmigrated). Every build since 2026-10-08
    /// refuses it `CurveDepthExceeded` (6105) and writes nothing, the 2026-10-13 build with a
    /// message that names its depth cap ([`PUMP_DEPTH_CAP_MESSAGE`]). Returns the custom code and
    /// the logs.
    ///
    /// Needs pump's statics as dumped since the 2026-10-08 redeploy ([`Env::load_pump_statics`]):
    /// panics without a `Global` of 1,088 bytes or more (an older one has no `max_curve_depth`).
    /// For the call only, it writes the depth into `Global`, an empty [`empty_quote_control`] at
    /// the `QuoteControl` PDA if the VM holds none, `synthetic_stuck`'s mint and curve if absent
    /// (the only accounts of the coin the call names) and a funded `user`; afterwards each of
    /// these accounts is as the probe found it, one it added removed again. What remains is what
    /// any refused transaction leaves: the fee [`Env::payer`] paid and an expired blockhash.
    pub fn pump_create_v2_depth_probe(&mut self) -> (Option<u32>, Vec<String>) {
        let q = PumpCurve::of(PumpCoin::SyntheticStuck);
        let qc = pump_quote_control();
        let user = Keypair::new();
        let found: Vec<(Pubkey, Option<Account>)> =
            [PUMP_GLOBAL, qc, q.mint, q.curve, user.pubkey()]
                .into_iter()
                .map(|k| (k, self.svm.get_account(&k)))
                .collect();
        let mut global = self.svm.get_account(&PUMP_GLOBAL).expect("pump Global");
        assert!(
            global.data.len() > PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH,
            "the depth probe needs a Global with max_curve_depth (1,088 bytes since the \
             2026-10-08 redeploy), not {} bytes",
            global.data.len()
        );
        global.data[PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH] = 1;
        self.svm.set_account(PUMP_GLOBAL, global).expect("Global");
        if self.svm.get_account(&qc).is_none() {
            let acc = empty_quote_control(&self.svm);
            self.svm.set_account(qc, acc).expect("QuoteControl");
        }
        let role = PumpCoin::SyntheticStuck.role();
        for (address, part) in [(q.mint, "mint"), (q.curve, "curve")] {
            if self.svm.get_account(&address).is_none() {
                load(&mut self.svm, &format!("pump_{role}_{part}"));
            }
        }
        let funded = Account::new(crate::env::FUNDING, 0, &anchor_lang::system_program::ID);
        self.svm.set_account(user.pubkey(), funded).expect("user");
        let mint = Keypair::new();
        let ix = pump_create_v2_ix(
            &user.pubkey(),
            &mint.pubkey(),
            pump_coin_quote_accounts(&mint.pubkey(), &q, &[]),
        );
        let res = self.send(&[crate::pump::cu_limit_ix(400_000), ix], &[&user, &mint]);
        for (address, account) in found {
            // An absent one as zero lamports, which LiteSVM drops.
            self.svm
                .set_account(address, account.unwrap_or_default())
                .expect("restore");
        }
        let logs = match &res {
            Ok(m) => m.logs.clone(),
            Err(f) => f.meta.logs.clone(),
        };
        (crate::errors::custom_code(&res), logs)
    }

    /// Which build of `program` **executes** (not only what its ProgramData holds,
    /// [`PumpDeploy::held`]), asked of the program with instructions each build added, sent
    /// without accounts ([`Env::pump_probe`]: 101 = absent, 3005 = dispatched), and for pump's
    /// last two builds, which share their instructions, with the depth cap the 2026-10-13 one
    /// added ([`Env::pump_create_v2_depth_probe`]):
    ///
    /// | Program | First probe | Then |
    /// |---|---|---|
    /// | pump | `buy_v3`: 101 → `Sep2026` | `multi_hop_curve_swap`: 101 → `Oct2026`; 3005 → `create_v2` too deep: 6105 naming the cap of 5 → `Oct2026Chain`, without it → `Oct2026Synth` |
    /// | PumpSwap | `buy_v2` ([`crate::pamm::PAMM_BUY_V2`]): 101 → `Sep2026` | `multi_hop_swap`: 101 → `Oct2026`, 3005 → `Oct2026Chain` (= `Oct2026Synth`'s build) |
    /// | pump-fees | `withdraw`: 101 → `Oct2026` (the 2026-09 and 2026-10-02 builds share their instructions), 3005 → `Oct2026Chain` (= `Oct2026Synth`'s build) | — |
    ///
    /// A build two deploys share answers as the newer ([`PumpDeploy::same_build`] compares). The
    /// probes of one call differ, so no two are duplicates; callers warp a slot between two calls
    /// on one program (the same transaction would be a duplicate). On pump's two newest builds
    /// the depth probe needs pump's statics as dumped since 2026-10-08. Every probe is a refused
    /// transaction and leaves the accounts as it found them, but for the fee [`Env::payer`] pays.
    pub fn pump_build_running(&mut self, program: PumpProgram) -> PumpDeploy {
        let answer = |code: Option<u32>, absent: PumpDeploy, present: PumpDeploy| match code {
            Some(ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND) => absent,
            Some(ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS) => present,
            other => panic!("unexpected {program:?} answer {other:?}"),
        };
        let (first, then) = match program {
            PumpProgram::Pump => (PUMP_BUY_V3, PUMP_MULTI_HOP_CURVE_SWAP),
            PumpProgram::Amm => (crate::pamm::PAMM_BUY_V2, PAMM_MULTI_HOP_SWAP),
            PumpProgram::Fees => {
                let code = self.pump_probe(program, PUMP_FEES_WITHDRAW);
                return answer(code, PumpDeploy::Oct2026, PumpDeploy::Oct2026Chain);
            }
        };
        let code = self.pump_probe(program, first);
        if answer(code, PumpDeploy::Sep2026, PumpDeploy::Oct2026) == PumpDeploy::Sep2026 {
            return PumpDeploy::Sep2026;
        }
        let code = self.pump_probe(program, then);
        let synth = answer(code, PumpDeploy::Oct2026, PumpDeploy::Oct2026Chain);
        if program != PumpProgram::Pump || synth == PumpDeploy::Oct2026 {
            return synth;
        }
        match self.pump_create_v2_depth_probe() {
            (Some(PUMP_ERR_CURVE_DEPTH_EXCEEDED), logs) => {
                if logs.iter().any(|l| l.contains(PUMP_DEPTH_CAP_MESSAGE)) {
                    PumpDeploy::Oct2026Chain
                } else {
                    PumpDeploy::Oct2026Synth
                }
            }
            (other, logs) => panic!("unexpected create_v2 depth answer {other:?}: {logs:#?}"),
        }
    }
}

/// Deploys pump program `program` at its mainnet address from the dumped ProgramData (header ‖
/// ELF ‖ zeros, sha256-checked against the mainnet account when first read), then the program
/// account, which makes LiteSVM compile the ELF. The build is [`PumpDeploy::CURRENT`].
pub fn load_pump_program(svm: &mut LiteSVM, program: PumpProgram) {
    load_pump_deploy(svm, program, PumpDeploy::CURRENT);
}

/// Deploys build `deploy` of `program` (as [`load_pump_program`]), replacing whichever build the
/// VM held: LiteSVM recompiles the ELF when the program account is rewritten.
pub fn load_pump_deploy(svm: &mut LiteSVM, program: PumpProgram, deploy: PumpDeploy) {
    load(svm, &deploy.program_data(program).name);
    load(svm, &deploy.program(program).name);
}

/// Rewrites the deploy slot in `program`'s ProgramData header (a latch "upgrade"; the program
/// keeps running the dumped ELF).
pub fn set_pump_deploy_slot(svm: &mut LiteSVM, program: PumpProgram, slot: u64) {
    patch(svm, &program.program_data().address, |d| {
        d[4..12].copy_from_slice(&slot.to_le_bytes());
    });
}

/// The pump coin roles of the dump (`scripts/dump-fixtures.py` `PUMP_COINS`, group `pump_state`,
/// and `PUMP_COINS_2026_10_13`, group `pump_state_2026_10_13`: [`PumpCoin::ALL_2026_10_13`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpCoin {
    /// A live Token-2022 curve minutes old (125 bytes, the `create_v2` allocation at the dump).
    Fresh,
    /// A live curve near completion (151 bytes, `rT ≤ 100 M` tokens): 6021 beyond `rT`, the
    /// completing buy, the straddle.
    Near,
    /// A live Token-2022 curve of 115 bytes.
    T22_115,
    /// A live legacy SPL-Token curve of 115 bytes without a creator (creator fee 0).
    Legacy115,
    /// A live legacy SPL-Token curve in the 2024 layout (49 bytes): the first v2 trade grows it
    /// to 125.
    Legacy49,
    /// A migrated curve with its canonical PumpSwap pool and an active boost vault.
    Graduated,
    /// A live mayhem-mode curve (refused by short.win, Q6).
    Mayhem,
    /// A live USDC-quoted curve (refused by short.win, Q6).
    Usdc,
    /// A legacy SPL-Token curve extended to 150 bytes, graduated to PumpSwap.
    Legacy150,
    /// Fartcoin: graduated to Raydium before PumpSwap (no canonical pool).
    Fartcoin,
    // ---- group `pump_state_2026_10_13` (the 2026-10-08/12 redeploys) ----
    /// A live SOL, non-cashback, non-mayhem 166-byte curve near completion (`rT ≤ 100 M` tokens)
    /// carrying both fee buckets: v3 synthetic exits past `rT`, the differential.
    Near166,
    /// A `create_v2` curve minutes old (166 bytes, Token-2022, `ivqr` 30e9 = `vQ − rQ`).
    Fresh166,
    /// A live 166-byte SOL curve carrying large fee buckets (@125, @133).
    Buckets166,
    /// A dormant live SOL curve of 141 bytes (last traded on the 2026-10-02 build): the next
    /// trade grows it to 166.
    Grow141,
    /// A dormant live SOL curve of 151 bytes: the next trade grows it to 166.
    Grow151,
    /// Curve `E1bYRCNq…`: complete and unmigrated after a synthetic completion, depth 1 (quoted
    /// in a pump coin), `post_complete_base_out` / `quote_in` set (refused by short.win,
    /// `CurveQuoteNotSol`).
    SyntheticStuck,
    /// 98u8RJ: a curve a synthetic buy completed (`2vVRr9eg…`), migrated, with the canonical pool
    /// `D4PACvyr…` `migrate_v2` created; the `pc*` fields stay on the curve.
    GraduatedSynthetic,
    /// Curve `8oe1sfhK…`: a live 166-byte SOL cashback coin (v3 refuses it, 6094).
    Cashback166,
    /// Curve `5jQXNPKb…`: a live 166-byte mayhem-mode curve whose `vQ − rQ` differs from its
    /// `ivqr`.
    Mayhem166,
    /// Pool `ERtQ5Uhn…`: a migrated coin whose canonical WSOL pool carries both fee buckets.
    WsolPoolBuckets,
}

impl PumpCoin {
    /// The roles of group `pump_state` (the older suites').
    pub const ALL: [Self; 10] = [
        Self::Fresh,
        Self::Near,
        Self::T22_115,
        Self::Legacy115,
        Self::Legacy49,
        Self::Graduated,
        Self::Mayhem,
        Self::Usdc,
        Self::Legacy150,
        Self::Fartcoin,
    ];

    /// The roles of group `pump_state_2026_10_13`.
    pub const ALL_2026_10_13: [Self; 10] = [
        Self::Near166,
        Self::Fresh166,
        Self::Buckets166,
        Self::Grow141,
        Self::Grow151,
        Self::SyntheticStuck,
        Self::GraduatedSynthetic,
        Self::Cashback166,
        Self::Mayhem166,
        Self::WsolPoolBuckets,
    ];

    /// The live SOL curves a normal user trades (group `pump_state`).
    pub const LIVE_SOL: [Self; 5] = [
        Self::Fresh,
        Self::Near,
        Self::T22_115,
        Self::Legacy115,
        Self::Legacy49,
    ];

    /// The live SOL, non-cashback, non-mayhem curves of group `pump_state_2026_10_13`.
    pub const LIVE_SOL_2026_10_13: [Self; 5] = [
        Self::Near166,
        Self::Fresh166,
        Self::Buckets166,
        Self::Grow141,
        Self::Grow151,
    ];

    /// The manifest role.
    pub fn role(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Near => "near",
            Self::T22_115 => "t22_115",
            Self::Legacy115 => "legacy115",
            Self::Legacy49 => "legacy49",
            Self::Graduated => "graduated",
            Self::Mayhem => "mayhem",
            Self::Usdc => "usdc",
            Self::Legacy150 => "legacy150",
            Self::Fartcoin => "fartcoin",
            Self::Near166 => "near166",
            Self::Fresh166 => "fresh166",
            Self::Buckets166 => "buckets166",
            Self::Grow141 => "grow141",
            Self::Grow151 => "grow151",
            Self::SyntheticStuck => "synthetic_stuck",
            Self::GraduatedSynthetic => "graduated_synthetic",
            Self::Cashback166 => "cashback166",
            Self::Mayhem166 => "mayhem166",
            Self::WsolPoolBuckets => "wsol_pool_buckets",
        }
    }

    /// The state group that holds the role (`pump_state` or `pump_state_2026_10_13`).
    pub fn group(self) -> &'static str {
        if Self::ALL_2026_10_13.contains(&self) {
            PUMP_STATE_GROUPS[1]
        } else {
            PUMP_STATE_GROUPS[0]
        }
    }

    /// The slot and block time the role's state group was read at.
    pub fn snapshot(self) -> Snapshot {
        pump_fixtures().snapshots[self.group()]
    }

    /// The role's mint (its state group snapshot's `coins`).
    pub fn mint(self) -> Pubkey {
        *pump_fixtures()
            .coins
            .get(self.role())
            .unwrap_or_else(|| panic!("no pump coin role `{}` in the snapshot", self.role()))
    }
}

/// Mainnet's Rent sysvar at the pump snapshot (the `sysvar_rent` fixture: `lamports_per_byte`
/// u64, the deprecated `exemption_threshold` bytes, `burn_percent`).
pub fn pump_rent() -> solana_rent::Rent {
    let d = &fixture("sysvar_rent").account.data;
    assert_eq!(d.len(), 17, "Rent sysvar");
    #[allow(deprecated)]
    solana_rent::Rent {
        lamports_per_byte: u64::from_le_bytes(d[..8].try_into().expect("u64")),
        exemption_threshold: d[8..16].try_into().expect("8 bytes"),
        burn_percent: d[16],
    }
}

/// pump `Global` bytes in the VM (panics if not loaded).
fn global_data(svm: &LiteSVM) -> Vec<u8> {
    svm.get_account(&PUMP_GLOBAL)
        .expect("pump Global (Env::load_pump_statics)")
        .data
}

/// Normal fee recipient `i` (0–7) of the live `Global`.
pub fn pump_fee_recipient(svm: &LiteSVM, i: usize) -> Pubkey {
    let d = global_data(svm);
    let g = pump::read_global(&pump::PROGRAM, &d).expect("Global");
    Pubkey::new_from_array(g.fee_recipients()[i])
}

/// Buyback fee recipient `i` (0–7) of the live `Global`.
pub fn pump_buyback_fee_recipient(svm: &LiteSVM, i: usize) -> Pubkey {
    let d = global_data(svm);
    let g = pump::read_global(&pump::PROGRAM, &d).expect("Global");
    Pubkey::new_from_array(g.buyback_fee_recipients()[i])
}

/// `buyback_basis_points` of the live `Global`.
pub fn pump_buyback_bps(svm: &LiteSVM) -> u64 {
    let d = global_data(svm);
    pump::read_global(&pump::PROGRAM, &d)
        .expect("Global")
        .buyback_basis_points()
}

/// `PDA(pump, ["user_volume_accumulator", user])`.
pub fn pump_user_volume_accumulator(user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump::USER_VOLUME_ACCUMULATOR_SEED, user.as_ref()],
        &PUMP_PROGRAM_ID,
    )
    .0
}

/// `PDA(pump, ["creator-vault", creator])`.
pub fn pump_creator_vault(creator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump::CREATOR_VAULT_SEED, creator.as_ref()],
        &PUMP_PROGRAM_ID,
    )
    .0
}

/// `sha256("global:buy_v2")[..8]`.
pub const PUMP_BUY_V2: [u8; 8] = [0xb8, 0x17, 0xee, 0x61, 0x67, 0xc5, 0xd3, 0x3d];
/// `sha256("global:sell_v2")[..8]`.
pub const PUMP_SELL_V2: [u8; 8] = [0x5d, 0xf6, 0x82, 0x3c, 0xe7, 0xe9, 0x40, 0xb2];
/// `sha256("global:buy_exact_quote_in_v2")[..8]`.
pub const PUMP_BUY_EXACT_QUOTE_IN_V2: [u8; 8] = [0xc2, 0xab, 0x1c, 0x46, 0x68, 0x4d, 0x5b, 0x2f];
/// `sha256("global:sell_v3")[..8]` (2026-10-02).
pub const PUMP_SELL_V3: [u8; 8] = [0x1c, 0x92, 0xde, 0x77, 0x26, 0xc4, 0x69, 0xd5];
/// `sha256("global:buy_exact_quote_in_v3")[..8]` (2026-10-02).
pub const PUMP_BUY_EXACT_QUOTE_IN_V3: [u8; 8] = [0xe1, 0xf7, 0x50, 0x1e, 0xd5, 0xb3, 0x84, 0x88];
/// `sha256("global:init_user_volume_accumulator")[..8]`.
pub const PUMP_INIT_USER_VOLUME_ACCUMULATOR: [u8; 8] =
    [0x5e, 0x06, 0xca, 0x73, 0xff, 0x60, 0xe8, 0xb7];
/// `sha256("event:TradeEvent")[..8]`.
pub const PUMP_TRADE_EVENT: [u8; 8] = [0xbd, 0xdb, 0x7f, 0xd3, 0x4e, 0xe6, 0x61, 0xee];

/// pump `TooMuchSolRequired` (a buy above `max_sol_cost`).
pub const PUMP_ERR_TOO_MUCH_SOL_REQUIRED: u32 = 6002;
/// pump `TooLittleSolReceived` (a sale below `min_sol_output`).
pub const PUMP_ERR_TOO_LITTLE_SOL_RECEIVED: u32 = 6003;
/// pump `BondingCurveComplete` (every trade after `complete`).
pub const PUMP_ERR_BONDING_CURVE_COMPLETE: u32 = 6005;
/// pump `BuyZeroAmount`.
pub const PUMP_ERR_BUY_ZERO_AMOUNT: u32 = 6020;
/// pump `NotEnoughTokensToBuy` (a buy above `rT`: no cap, no partial fill).
pub const PUMP_ERR_NOT_ENOUGH_TOKENS_TO_BUY: u32 = 6021;
/// pump `BuySlippageBelowMinTokensOut` (`buy_exact_quote_in_v2` below `min_tokens_out`).
pub const PUMP_ERR_BUY_SLIPPAGE_BELOW_MIN_TOKENS_OUT: u32 = 6042;
/// pump `NotAuthorized` (e.g. a fee recipient outside the live list).
pub const PUMP_ERR_NOT_AUTHORIZED: u32 = 6000;
/// pump `BuybackFeeRecipientNotAuthorized`.
pub const PUMP_ERR_BUYBACK_FEE_RECIPIENT_NOT_AUTHORIZED: u32 = 6057;

/// pump `CashbackCoinNotSupported` (a v3 trade on a cashback coin; 2026-10-08).
pub const PUMP_ERR_CASHBACK_COIN_NOT_SUPPORTED: u32 = 6094;

/// Which v3 trade instruction (pump's 2026-10-02 redeploy; pump-public-docs `2293f9a6`'s IDL):
/// one 17-account list for all three, data `disc ‖ u64 ‖ u64`, the buys with an optional
/// trailing `partial_fill` (`OptionBool`, one byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpV3 {
    /// `buy_v3(amount, max_sol_cost[, partial_fill])`: exact tokens out; past `rT` on the
    /// 2026-10-08 build the synthetic migration buys the rest from the would-be pool.
    Buy,
    /// `sell_v3(amount, min_sol_output)`: exact tokens in.
    Sell,
    /// `buy_exact_quote_in_v3(spendable_quote_in, min_tokens_out[, partial_fill])`.
    BuyExactQuoteIn,
}

impl PumpV3 {
    /// The discriminator.
    pub fn disc(self) -> [u8; 8] {
        match self {
            Self::Buy => PUMP_BUY_V3,
            Self::Sell => PUMP_SELL_V3,
            Self::BuyExactQuoteIn => PUMP_BUY_EXACT_QUOTE_IN_V3,
        }
    }
}

/// Which v2 trade instruction (R-PUMP §3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpV2 {
    /// `buy_v2(amount, max_sol_cost)`: exact tokens out (27 accounts).
    Buy,
    /// `sell_v2(amount, min_sol_output)`: exact tokens in (26 accounts).
    Sell,
    /// `buy_exact_quote_in_v2(spendable_quote_in, min_tokens_out)` (27 accounts, `buy_v2`'s).
    BuyExactQuoteIn,
}

/// What `buy_exact_quote_in_v2` does with `spendable` on `(vT, vQ)` at `fees` (R-PUMP §4 "Exact
/// SOL in", verified against mainnet there): `net = ⌊spendable·10⁴/(10⁴ + p + c)⌋`, lowered by
/// any excess of `net + ⌈net·p/10⁴⌉ + ⌈net·c/10⁴⌉` over `spendable`, then
/// `tokens = ⌊(net − 1)·vT/(vQ + net − 1)⌋`; the curve is credited `net`.
///
/// **Test-side reference, not `shortwin-math`:** short.win never trades exact-quote-in (the
/// protocol owes exactly `Q`, R-PUMP §9.4), so the math crate has no such function. The ELF is
/// checked against this formula, and the formula against `shortwin-math`'s exact-out charge:
/// `curve_charge_exact_out(tokens).net ≤ net ≤ curve_charge_exact_out(tokens + 1).net`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactQuoteIn {
    /// Lamports credited to the curve (`TradeEvent.sol_amount`).
    pub net: u64,
    /// Protocol fee.
    pub protocol_fee: u64,
    /// Creator fee.
    pub creator_fee: u64,
    /// Tokens bought.
    pub tokens: u64,
}

impl ExactQuoteIn {
    /// Lamports the buyer pays: `net + protocol_fee + creator_fee`.
    pub fn cost(&self) -> u64 {
        self.net + self.protocol_fee + self.creator_fee
    }
}

/// [`ExactQuoteIn`] (see there); `None` if `net ≤ 1` (no tokens are priced) or `vQ + net − 1`
/// is zero.
pub fn pump_exact_quote_in(
    spendable: u64,
    vt: u64,
    vq: u64,
    fees: &FeeBps,
) -> Option<ExactQuoteIn> {
    let bps = 10_000u128;
    let fee = |n: u64, b: u64| -> u64 {
        u64::try_from((u128::from(n) * u128::from(b)).div_ceil(bps)).expect("fee")
    };
    let mut net = u64::try_from(
        u128::from(spendable) * bps / (bps + u128::from(fees.protocol) + u128::from(fees.creator)),
    )
    .ok()?;
    let total = net + fee(net, fees.protocol) + fee(net, fees.creator);
    if total > spendable {
        net -= total - spendable;
    }
    if net <= 1 {
        return None;
    }
    let tokens = u128::from(net - 1) * u128::from(vt) / (u128::from(vq) + u128::from(net - 1));
    Some(ExactQuoteIn {
        net,
        protocol_fee: fee(net, fees.protocol),
        creator_fee: fee(net, fees.creator),
        tokens: u64::try_from(tokens).ok()?,
    })
}

/// A pump coin's curve accounts (keys; the live state is read on demand).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpCurve {
    pub coin: PumpCoin,
    pub mint: Pubkey,
    /// `PDA(pump, ["bonding-curve", mint])`.
    pub curve: Pubkey,
    /// `ATA(curve, mint, token_program)` (`associated_bonding_curve`).
    pub curve_base_ata: Pubkey,
    /// The mint's program.
    pub token_program: TokenProgram,
    /// The v2 instructions' `quote_mint` account: WSOL for a SOL curve, else the curve's quote.
    pub quote_mint: Pubkey,
}

impl PumpCurve {
    /// The curve of `coin`'s mint (keys only; nothing is loaded).
    pub fn of(coin: PumpCoin) -> Self {
        let mint = coin.mint();
        let f = fixture(&format!("pump_{}_mint", coin.role()));
        let token_program = if f.account.owner == TOKEN_2022_PROGRAM_ID {
            TokenProgram::Token2022
        } else {
            TokenProgram::Spl
        };
        let curve = crate::pda::bonding_curve(&mint);
        let c = pump::read_curve(
            &pump::PROGRAM,
            &fixture(&format!("pump_{}_curve", coin.role())).account.data,
        )
        .expect("fixture curve");
        let quote_mint = if c.is_sol_quote() {
            WSOL_MINT
        } else {
            Pubkey::new_from_array(c.quote_mint)
        };
        Self {
            coin,
            mint,
            curve,
            curve_base_ata: crate::pda::ata(&curve, &mint, &token_program.id()),
            token_program,
            quote_mint,
        }
    }

    /// The curve as the reader sees it now.
    pub fn state(&self, svm: &LiteSVM) -> pump::BondingCurve {
        let acc = svm.get_account(&self.curve).expect("curve");
        pump::read_curve(&acc.owner.to_bytes(), &acc.data).expect("BondingCurve")
    }

    /// The curve account's data length now.
    pub fn data_len(&self, svm: &LiteSVM) -> usize {
        svm.get_account(&self.curve).expect("curve").data.len()
    }

    /// What pump takes from the trading user to grow this curve on its next trade: a curve
    /// shorter than the layout of the build pump's ProgramData holds (166 bytes since the
    /// 2026-10-08 redeploy, 141 on the 2026-10-02 build, [`PumpDeploy::held`]) grows to it, its
    /// rent reserve topped up to `Rent.minimum_balance(len)` ([`curve_growth_top_up`]). As the
    /// I13 hook's `venue_rent` counts it.
    pub fn growth_top_up(&self, svm: &LiteSVM) -> u64 {
        let grows_to = PumpDeploy::held(svm, PumpProgram::Pump)
            .unwrap_or(PumpDeploy::CURRENT)
            .curve_len();
        let acc = svm.get_account(&self.curve).expect("curve");
        let (creator_fee, protocol_fees) = curve_fee_buckets(&acc.data);
        curve_growth_top_up(
            svm,
            acc.data.len(),
            acc.lamports,
            self.state(svm).real_quote_reserves,
            creator_fee + protocol_fees,
            grows_to,
        )
    }

    /// `(vT, vQ, rT, rQ, complete)` as `shortwin-math`'s `CurveState`.
    pub fn curve_state(&self, svm: &LiteSVM) -> CurveState {
        let c = self.state(svm);
        CurveState {
            vt: u128::from(c.virtual_token_reserves),
            vq: u128::from(c.virtual_quote_reserves),
            rt: u128::from(c.real_token_reserves),
            rq: u128::from(c.real_quote_reserves),
            complete: c.complete,
        }
    }

    /// The live fees (ARCH §4.2): `fees::pump_curve` on the curve's `(vT, vQ)` with the live
    /// pump `FeeConfig.fee_tiers` and `Global.creator_fee_configurable` (readers), the curve's
    /// own `creator_fee_bps` and creator. `None` on a migrated curve (`vT = 0`).
    pub fn fees(&self, svm: &LiteSVM) -> Option<FeeBps> {
        self.resolve_fees(svm, None)
    }

    /// The fees pump charges on both parts of a buy past `rT` by synthetic migration
    /// (pump-final §5.2, P1 correction (a)): `fees::pump_curve_at_supply` on the curve's
    /// `(vT, vQ)` with the mint's live supply, the inputs otherwise as [`PumpCurve::fees`].
    /// `None` on a migrated curve.
    pub fn synthetic_fees(&self, svm: &LiteSVM) -> Option<FeeBps> {
        let m = svm.get_account(&self.mint).expect("mint");
        let supply = shortwin_venue_state::mint::read_base(&m.owner.to_bytes(), &m.data)
            .expect("mint")
            .supply;
        self.resolve_fees(svm, Some(supply))
    }

    /// `fees::pump_curve` (`supply` `None`, the fixed 10¹⁵) or `fees::pump_curve_at_supply`.
    fn resolve_fees(&self, svm: &LiteSVM, supply: Option<u64>) -> Option<FeeBps> {
        let c = self.state(svm);
        let fc = svm.get_account(&PUMP_FEE_CONFIG).expect("pump FeeConfig");
        let config = pump::read_fee_config(&fc.owner.to_bytes(), &fc.data).expect("FeeConfig");
        let tiers: Vec<MathFeeTier> = config
            .fee_tiers
            .iter()
            .map(|t| MathFeeTier {
                threshold: t.threshold,
                fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
            })
            .collect();
        let g = global_data(svm);
        let global = pump::read_global(&pump::PROGRAM, &g).expect("Global");
        let (vt, vq) = (
            u128::from(c.virtual_token_reserves),
            u128::from(c.virtual_quote_reserves),
        );
        let schedule = CurveFeeSchedule {
            tiers: &tiers,
            creator_fee_configurable: global.creator_fee_configurable(),
            curve_creator_fee_bps: c.creator_fee_bps,
            creator_is_default: c.creator_is_default(),
            cap_bps: None,
        };
        match supply {
            None => mfees::pump_curve(vt, vq, &schedule),
            Some(s) => mfees::pump_curve_at_supply(vt, vq, s, &schedule),
        }
        .ok()
    }

    /// What pump charges for an exact-out buy of `q` (`sol_leg::curve_charge_exact_out`,
    /// `n = ⌊q·vQ/(vT − q)⌋ + 1` plus each fee rounded up, R-PUMP §4): `None` on a complete
    /// curve (6005), above `rT` (6021) or where the formula is undefined.
    pub fn charge_buy(&self, svm: &LiteSVM, q: u64) -> Option<BuyQuote> {
        let s = self.curve_state(svm);
        if s.complete || u128::from(q) > s.rt {
            return None;
        }
        sol_leg::curve_charge_exact_out(q, s.vt, s.vq, &self.fees(svm)?)
    }

    /// short.win's own quote of the same buy (`sol_leg::buy_exact_out` with ε₁ = 1, ECON §3.1:
    /// at most `1 + #fees` lamports above [`PumpCurve::charge_buy`]).
    pub fn quote_buy(&self, svm: &LiteSVM, q: u64) -> Option<BuyQuote> {
        let s = self.curve_state(svm);
        if s.complete || u128::from(q) > s.rt {
            return None;
        }
        sol_leg::buy_exact_out(q, s.vt, s.vq, true, &self.fees(svm)?)
    }

    /// An exact-in sale of `q` (`sol_leg::sell_exact_in` on `(vT, vQ)`, capped by `rQ`): `None`
    /// on a complete curve (6005) or where the formula is undefined.
    pub fn quote_sell(&self, svm: &LiteSVM, q: u64) -> Option<SellQuote> {
        let s = self.curve_state(svm);
        if s.complete {
            return None;
        }
        let cap = u64::try_from(s.rq).unwrap_or(u64::MAX);
        sol_leg::sell_exact_in(q, s.vt, s.vq, &self.fees(svm)?, cap)
    }

    /// [`pump_exact_quote_in`] on the live state: `None` on a complete curve (6005), above
    /// `rT` (6021) or where it is undefined.
    pub fn quote_exact_quote_in(&self, svm: &LiteSVM, spendable: u64) -> Option<ExactQuoteIn> {
        let c = self.state(svm);
        if c.complete {
            return None;
        }
        let r = pump_exact_quote_in(
            spendable,
            c.virtual_token_reserves,
            c.virtual_quote_reserves,
            &self.fees(svm)?,
        )?;
        (r.tokens <= c.real_token_reserves).then_some(r)
    }

    /// `PDA(pump, ["creator-vault", curve.creator])`, re-derived from the live curve.
    pub fn creator_vault(&self, svm: &LiteSVM) -> Pubkey {
        pump_creator_vault(&Pubkey::new_from_array(self.state(svm).creator))
    }

    /// The quote side's token program (Tokenkeg for WSOL and USDC).
    pub fn quote_token_program(&self) -> Pubkey {
        TOKEN_PROGRAM_ID
    }

    /// The v2 trade accounts in the IDL's exact order and flags (R-PUMP §3.5; `idl/pump.json`
    /// at `pump-public-docs@81091419`): 27 for the buys, 26 for `sell_v2` (no
    /// `global_volume_accumulator`). `user` signs; `user_base_ata` receives (buys) or supplies
    /// (sells) the tokens. Every quote-side ATA is only seed-checked for a SOL curve.
    pub fn v2_accounts(
        &self,
        svm: &LiteSVM,
        kind: PumpV2,
        user: &Pubkey,
        user_base_ata: &Pubkey,
        fee_recipient: &Pubkey,
        buyback_fee_recipient: &Pubkey,
    ) -> Vec<AccountMeta> {
        let qp = self.quote_token_program();
        let q = self.quote_mint;
        let ata = |owner: &Pubkey| crate::pda::ata(owner, &q, &qp);
        let creator_vault = self.creator_vault(svm);
        let uva = pump_user_volume_accumulator(user);
        let sharing_config = Pubkey::find_program_address(
            &[pump::SHARING_CONFIG_SEED, self.mint.as_ref()],
            &PUMP_FEES_PROGRAM_ID,
        )
        .0;
        let mut v = vec![
            AccountMeta::new_readonly(PUMP_GLOBAL, false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(q, false),
            AccountMeta::new_readonly(self.token_program.id(), false),
            AccountMeta::new_readonly(qp, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new(*fee_recipient, false),
            AccountMeta::new(ata(fee_recipient), false),
            AccountMeta::new(*buyback_fee_recipient, false),
            AccountMeta::new(ata(buyback_fee_recipient), false),
            AccountMeta::new(self.curve, false),
            AccountMeta::new(self.curve_base_ata, false),
            AccountMeta::new(ata(&self.curve), false),
            AccountMeta::new(*user, true),
            AccountMeta::new(*user_base_ata, false),
            AccountMeta::new(ata(user), false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new(ata(&creator_vault), false),
            AccountMeta::new_readonly(sharing_config, false),
        ];
        if kind != PumpV2::Sell {
            v.push(AccountMeta::new_readonly(
                PUMP_GLOBAL_VOLUME_ACCUMULATOR,
                false,
            ));
        }
        v.extend([
            AccountMeta::new(uva, false),
            AccountMeta::new(ata(&uva), false),
            AccountMeta::new_readonly(PUMP_FEE_CONFIG, false),
            AccountMeta::new_readonly(PUMP_FEES_PROGRAM_ID, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(PUMP_EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_PROGRAM_ID, false),
        ]);
        v
    }

    /// A v2 trade instruction: `(disc | a u64 | b u64)` with `a, b` =
    /// `(amount, max_sol_cost)` / `(amount, min_sol_output)` / `(spendable_quote_in,
    /// min_tokens_out)`.
    #[allow(clippy::too_many_arguments)]
    pub fn v2_ix(
        &self,
        svm: &LiteSVM,
        kind: PumpV2,
        user: &Pubkey,
        user_base_ata: &Pubkey,
        fee_recipient: &Pubkey,
        buyback_fee_recipient: &Pubkey,
        a: u64,
        b: u64,
    ) -> Instruction {
        let disc = match kind {
            PumpV2::Buy => PUMP_BUY_V2,
            PumpV2::Sell => PUMP_SELL_V2,
            PumpV2::BuyExactQuoteIn => PUMP_BUY_EXACT_QUOTE_IN_V2,
        };
        let mut data = disc.to_vec();
        data.extend_from_slice(&a.to_le_bytes());
        data.extend_from_slice(&b.to_le_bytes());
        Instruction {
            program_id: PUMP_PROGRAM_ID,
            accounts: self.v2_accounts(
                svm,
                kind,
                user,
                user_base_ata,
                fee_recipient,
                buyback_fee_recipient,
            ),
            data,
        }
    }

    /// The v3 trade accounts (`buy_v3`, `sell_v3`, `buy_exact_quote_in_v3`: one list) in the
    /// IDL's exact order and flags (pump-public-docs `2293f9a6`): `global`, `base_mint`,
    /// `quote_mint`, both token programs, the curve (W), its base and quote ATAs (W), `user` (S W),
    /// the user's base and quote ATAs (W), the user's UVA (W), pump's `FeeConfig`, the buyback
    /// recipient (W), System, the event authority and pump itself. No fee recipient, creator
    /// vault or pump-fees account: v3 keeps the protocol and creator fees in the curve's buckets
    /// and reads `FeeConfig` itself.
    pub fn v3_accounts(
        &self,
        user: &Pubkey,
        user_base_ata: &Pubkey,
        buyback_fee_recipient: &Pubkey,
    ) -> Vec<AccountMeta> {
        let qp = self.quote_token_program();
        let q = self.quote_mint;
        let ata = |owner: &Pubkey| crate::pda::ata(owner, &q, &qp);
        vec![
            AccountMeta::new_readonly(PUMP_GLOBAL, false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(q, false),
            AccountMeta::new_readonly(self.token_program.id(), false),
            AccountMeta::new_readonly(qp, false),
            AccountMeta::new(self.curve, false),
            AccountMeta::new(self.curve_base_ata, false),
            AccountMeta::new(ata(&self.curve), false),
            AccountMeta::new(*user, true),
            AccountMeta::new(*user_base_ata, false),
            AccountMeta::new(ata(user), false),
            AccountMeta::new(pump_user_volume_accumulator(user), false),
            AccountMeta::new_readonly(PUMP_FEE_CONFIG, false),
            AccountMeta::new(*buyback_fee_recipient, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(PUMP_EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_PROGRAM_ID, false),
        ]
    }

    /// A v3 trade instruction: `(disc | a u64 | b u64)` with `a, b` = `(amount, max_sol_cost)` /
    /// `(amount, min_sol_output)` / `(spendable_quote_in, min_tokens_out)` — 24 bytes; on a buy,
    /// `partial_fill = Some(flag)` appends the optional `OptionBool` byte (25 bytes).
    #[allow(clippy::too_many_arguments)]
    pub fn v3_ix(
        &self,
        kind: PumpV3,
        user: &Pubkey,
        user_base_ata: &Pubkey,
        buyback_fee_recipient: &Pubkey,
        a: u64,
        b: u64,
        partial_fill: Option<bool>,
    ) -> Instruction {
        let mut data = kind.disc().to_vec();
        data.extend_from_slice(&a.to_le_bytes());
        data.extend_from_slice(&b.to_le_bytes());
        if let Some(flag) = partial_fill {
            assert_ne!(kind, PumpV3::Sell, "sell_v3 has no partial_fill");
            data.push(u8::from(flag));
        }
        Instruction {
            program_id: PUMP_PROGRAM_ID,
            accounts: self.v3_accounts(user, user_base_ata, buyback_fee_recipient),
            data,
        }
    }
}

/// `init_user_volume_accumulator` (`[payer S W, user, uva W, system, event_authority,
/// program]`, R-PUMP §3.6): anyone can pre-create a user's UVA.
pub fn pump_init_uva_ix(payer: &Pubkey, user: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*user, false),
            AccountMeta::new(pump_user_volume_accumulator(user), false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(PUMP_EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_PROGRAM_ID, false),
        ],
        data: PUMP_INIT_USER_VOLUME_ACCUMULATOR.to_vec(),
    }
}

/// `create_associated_token_account_idempotent(payer, owner, mint, program)`.
pub fn create_ata_idempotent_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    program: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(crate::pda::ata(owner, mint, program), false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(*program, false),
        ],
        data: vec![1],
    }
}

/// The graduated coin's canonical PumpSwap pool (keys; state read on demand).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpPool {
    pub coin: PumpCoin,
    pub mint: Pubkey,
    pub pool: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub lp_mint: Pubkey,
    /// `PDA(pAMM, ["boost_vault", pool])`.
    pub boost_vault: Pubkey,
    /// `ATA(boost_vault, WSOL)` (ARCH §3.12 P5; may be missing).
    pub boost_vault_ata: Pubkey,
}

impl PumpPool {
    /// The canonical pool of `coin` (keys from the pool fixture).
    pub fn of(coin: PumpCoin) -> Self {
        let mint = coin.mint();
        let pool = crate::pda::canonical_pool(&mint);
        let f = fixture(&format!("pump_{}_pool", coin.role()));
        assert_eq!(f.address, pool, "the fixture pool is the canonical pool");
        let p = pump_amm::read_pool(&pump_amm::PROGRAM, &f.account.data).expect("Pool");
        let boost_vault = Pubkey::find_program_address(
            &[pump_amm::BOOST_VAULT_SEED, pool.as_ref()],
            &PUMP_AMM_PROGRAM_ID,
        )
        .0;
        Self {
            coin,
            mint,
            pool,
            base_vault: Pubkey::new_from_array(p.pool_base_token_account),
            quote_vault: Pubkey::new_from_array(p.pool_quote_token_account),
            lp_mint: Pubkey::new_from_array(p.lp_mint),
            boost_vault,
            boost_vault_ata: crate::pda::ata(&boost_vault, &WSOL_MINT, &TOKEN_PROGRAM_ID),
        }
    }

    /// The pool as the reader sees it now.
    pub fn state(&self, svm: &LiteSVM) -> pump_amm::Pool {
        let acc = svm.get_account(&self.pool).expect("pool");
        pump_amm::read_pool(&acc.owner.to_bytes(), &acc.data).expect("Pool")
    }

    /// PumpSwap's pricing reserves now (vaults + VQR).
    pub fn reserves(&self, svm: &LiteSVM) -> Option<pump_amm::PoolReserves> {
        self.state(svm).reserves(
            vault_amount(svm, &self.base_vault),
            vault_amount(svm, &self.quote_vault),
        )
    }

    /// The boost vault's WSOL balance as the boost gate reads it (0 if the ATA is missing).
    pub fn boost_amount(&self, svm: &LiteSVM) -> u64 {
        let (owner, data) = svm
            .get_account(&self.boost_vault_ata)
            .map_or(([0u8; 32], Vec::new()), |a| (a.owner.to_bytes(), a.data));
        pump_amm::boost_vault_amount(&owner, &data, &self.boost_vault.to_bytes())
            .expect("boost vault ATA")
    }
}

impl Env {
    /// Deploys pump, pump-fees and PumpSwap from their dumped ProgramData
    /// ([`load_pump_program`]).
    pub fn load_pump_programs(&mut self) {
        for p in PumpProgram::ALL {
            load_pump_program(&mut self.svm, p);
        }
    }

    /// Loads every pump-state account that belongs to no coin role: pump's `Global`, both
    /// `FeeConfig`s, PumpSwap's `GlobalConfig`, the volume accumulators, the fee, buyback and
    /// withdraw authorities, WSOL and USDC, PumpSwap's protocol-fee and buyback WSOL ATAs (the
    /// event authorities were absent at the snapshot and stay so). An address both state groups
    /// hold is loaded from the newer, `pump_state_2026_10_13` ([`pump_fixture_at`]): pump's
    /// `Global` is the 1,088-byte one of the 2026-10-08 redeploy (`max_curve_depth` @1087 = 1;
    /// its first 1,087 bytes, both `FeeConfig`s and PumpSwap's `GlobalConfig` equal
    /// `pump_state`'s). The dumped Rent sysvar is installed by [`Env::set_pump_rent`], not here.
    pub fn load_pump_statics(&mut self) {
        let addresses: Vec<Pubkey> = all_pump_fixtures()
            .filter(|f| {
                PUMP_STATE_GROUPS.contains(&f.group.as_str())
                    && f.role.is_none()
                    && f.name != "sysvar_rent"
            })
            .map(|f| f.address)
            .collect();
        for address in addresses {
            load_pump_at(&mut self.svm, &address);
        }
    }

    /// Installs mainnet's Rent sysvar as dumped with the pump state (`sysvar_rent`: 5,080
    /// lamports per byte, `minimum_balance(0)` = 650,240; LiteSVM's default is 6,960 / 890,880),
    /// so the rent pump charges a user — the creator-vault top-up to `minimum_balance(0)`, a UVA
    /// at `minimum_balance(137)`, a curve's growth to 125 bytes — is mainnet's (T-RENT).
    pub fn set_pump_rent(&mut self) {
        self.svm.set_sysvar(&pump_rent());
    }

    /// Loads every account of `coin`'s role (mint, curve, its token ATA, creator vault, and for
    /// a graduated coin its pool, vaults, LP mint and boost vault), plus the live creator's
    /// vault if another role holds it. Returns the curve's keys.
    pub fn load_pump_coin(&mut self, coin: PumpCoin) -> PumpCurve {
        let names: Vec<String> = all_pump_fixtures()
            .filter(|f| f.group == coin.group() && f.role.as_deref() == Some(coin.role()))
            .map(|f| f.name.clone())
            .collect();
        for name in names {
            load(&mut self.svm, &name);
        }
        let c = PumpCurve::of(coin);
        let vault = c.creator_vault(&self.svm);
        load_pump_at(&mut self.svm, &vault);
        c
    }

    /// Sets the clock to the pump snapshot's slot and block time (group `pump_state`).
    pub fn set_pump_clock(&mut self) {
        let s = pump_snapshot();
        self.set_clock(s.block_time, s.slot);
    }

    /// [`Env::initialized`] (release short.win at the harness's default config, whose venue
    /// deploy slots are the dumped ones), the three pump programs, the pump statics, `coin`'s
    /// accounts, mainnet's Rent ([`Env::set_pump_rent`]) and the clock at the snapshot of
    /// `coin`'s state group ([`PumpCoin::snapshot`]).
    pub fn fork_pump_coin(coin: PumpCoin) -> (Self, PumpCurve) {
        let mut env = Self::initialized();
        env.load_pump_programs();
        env.load_pump_statics();
        let c = env.load_pump_coin(coin);
        env.set_pump_rent();
        let s = coin.snapshot();
        env.set_clock(s.block_time, s.slot);
        (env, c)
    }

    /// [`Env::fork_pump_coin`] on the fresh curve.
    pub fn fork_pump() -> (Self, PumpCurve) {
        Self::fork_pump_coin(PumpCoin::Fresh)
    }
}

// ---- pump scenario builders (byte-level; ARCH §8.2) --------------------------------------------

/// Rewrites a curve's `(vT, vQ, rT, rQ)` (gaps, drains, near-completion states). The curve's
/// lamports are not adjusted.
pub fn set_curve_reserves(svm: &mut LiteSVM, curve: &Pubkey, vt: u64, vq: u64, rt: u64, rq: u64) {
    patch(svm, curve, |d| {
        put_u64(d, pump::OFF_VIRTUAL_TOKEN_RESERVES, vt);
        put_u64(d, pump::OFF_VIRTUAL_QUOTE_RESERVES, vq);
        put_u64(d, pump::OFF_REAL_TOKEN_RESERVES, rt);
        put_u64(d, pump::OFF_REAL_QUOTE_RESERVES, rq);
    });
}

/// Rewrites a curve's `complete` flag.
pub fn set_curve_complete(svm: &mut LiteSVM, curve: &Pubkey, complete: bool) {
    patch(svm, curve, |d| d[pump::OFF_COMPLETE] = u8::from(complete));
}

/// Rewrites a curve's `creator` (pump's `set_creator` / `admin_cto`; the creator vault moves).
pub fn set_curve_creator(svm: &mut LiteSVM, curve: &Pubkey, creator: &Pubkey) {
    patch(svm, curve, |d| {
        d[pump::OFF_CREATOR..pump::OFF_CREATOR + 32].copy_from_slice(creator.as_ref());
    });
}

/// Rewrites a curve's per-coin `creator_fee_bps` (a curve of at least 123 bytes).
pub fn set_curve_creator_fee_bps(svm: &mut LiteSVM, curve: &Pubkey, bps: u64) {
    patch(svm, curve, |d| put_u64(d, pump::OFF_CREATOR_FEE_BPS, bps));
}

/// Rewrites pump `Global.creator_fee_configurable`.
pub fn set_global_creator_fee_configurable(svm: &mut LiteSVM, on: bool) {
    patch(svm, &PUMP_GLOBAL, |d| {
        d[pump::OFF_GLOBAL_CREATOR_FEE_CONFIGURABLE] = u8::from(on);
    });
}

/// A pump-fees tier `(threshold, [lp, protocol, creator])`.
pub type TierSpec = (u128, [u64; 3]);

/// Rewrites a pump-fees `FeeConfig` (pump's `8Wf5Ti…` or PumpSwap's `5PHirr…`) as the admin's
/// `update_fee_config` / `upsert_fee_tiers` would leave it: `flat_fees`, `fee_tiers`,
/// `stable_fee_tiers`, `exotic_flat_fees` (Borsh), the bump and admin kept, zero-padded to the
/// allocation.
pub fn set_fee_config(
    svm: &mut LiteSVM,
    key: &Pubkey,
    flat: [u64; 3],
    tiers: &[TierSpec],
    stable: &[TierSpec],
    exotic: [u64; 3],
) {
    patch(svm, key, |d| {
        let len = d.len();
        let mut out = d[..pump::OFF_FEE_CONFIG_FLAT_FEES].to_vec();
        let fees = |out: &mut Vec<u8>, f: [u64; 3]| {
            for v in f {
                out.extend_from_slice(&v.to_le_bytes());
            }
        };
        fees(&mut out, flat);
        for list in [tiers, stable] {
            out.extend_from_slice(&u32::try_from(list.len()).expect("tiers").to_le_bytes());
            for (t, f) in list {
                out.extend_from_slice(&t.to_le_bytes());
                fees(&mut out, *f);
            }
        }
        fees(&mut out, exotic);
        assert!(
            out.len() <= len,
            "FeeConfig: {} tiers do not fit",
            tiers.len()
        );
        out.resize(len, 0);
        *d = out;
    });
}

/// The tiers of a pump-fees `FeeConfig` in the VM, as [`TierSpec`]s: `(flat, tiers, stable,
/// exotic)`.
pub fn fee_config_specs(
    svm: &LiteSVM,
    key: &Pubkey,
) -> ([u64; 3], Vec<TierSpec>, Vec<TierSpec>, [u64; 3]) {
    let acc = svm.get_account(key).expect("FeeConfig");
    let c = pump::read_fee_config(&acc.owner.to_bytes(), &acc.data).expect("FeeConfig");
    let f = |x: pump::Fees| [x.lp, x.protocol, x.creator];
    let t = |ts: pump::FeeTiers<'_>| ts.iter().map(|t| (t.threshold, f(t.fees))).collect();
    (
        f(c.flat_fees),
        t(c.fee_tiers),
        t(c.stable_fee_tiers),
        f(c.exotic_flat_fees),
    )
}

/// pump's `TradeEvent` (the fields short.win's tests read; `emit_cpi!`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PumpTradeEvent {
    pub mint: Pubkey,
    /// Lamports credited to / debited from the curve (`n` of a buy, `g` of a sale).
    pub sol_amount: u64,
    pub token_amount: u64,
    pub is_buy: bool,
    pub user: Pubkey,
    pub timestamp: i64,
    /// The post-trade `vQ`, `vT`, `rQ`, `rT`.
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub fee_recipient: Pubkey,
    pub fee_basis_points: u64,
    /// The protocol fee (including the buyback share).
    pub fee: u64,
    pub creator: Pubkey,
    pub creator_fee_basis_points: u64,
    pub creator_fee: u64,
    pub ix_name: String,
    pub mayhem_mode: bool,
    pub buyback_fee_basis_points: u64,
    pub buyback_fee: u64,
}

/// A Borsh reader over an event body.
struct Cursor<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let b: &'a [u8] = self.b;
        let s = &b[self.o..self.o + n];
        self.o += n;
        s
    }
    fn key(&mut self) -> Pubkey {
        Pubkey::new_from_array(self.take(32).try_into().expect("key"))
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().expect("u64"))
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take(8).try_into().expect("i64"))
    }
    fn bool(&mut self) -> bool {
        self.take(1)[0] != 0
    }
    fn string(&mut self) -> String {
        let n = u32::from_le_bytes(self.take(4).try_into().expect("len")) as usize;
        String::from_utf8_lossy(self.take(n)).into_owned()
    }
}

/// Every pump `TradeEvent` a transaction emitted (`emit_cpi!`: Borsh after `EVENT_IX_TAG ‖
/// discriminator`; field order per `idl/pump.json` `types.TradeEvent`).
pub fn pump_trade_events(meta: &TransactionMetadata) -> Vec<PumpTradeEvent> {
    crate::events::raw(meta)
        .iter()
        .filter_map(|p| p.strip_prefix(&PUMP_TRADE_EVENT[..]))
        .map(|b| {
            let mut c = Cursor { b, o: 0 };
            let mint = c.key();
            let sol_amount = c.u64();
            let token_amount = c.u64();
            let is_buy = c.bool();
            let user = c.key();
            let timestamp = c.i64();
            let virtual_sol_reserves = c.u64();
            let virtual_token_reserves = c.u64();
            let real_sol_reserves = c.u64();
            let real_token_reserves = c.u64();
            let fee_recipient = c.key();
            let fee_basis_points = c.u64();
            let fee = c.u64();
            let creator = c.key();
            let creator_fee_basis_points = c.u64();
            let creator_fee = c.u64();
            // track_volume, total_unclaimed_tokens, total_claimed_tokens, current_sol_volume,
            // last_update_timestamp
            c.take(1 + 8 * 4);
            let ix_name = c.string();
            let mayhem_mode = c.bool();
            c.take(8 * 2); // cashback_fee_basis_points, cashback
            let buyback_fee_basis_points = c.u64();
            let buyback_fee = c.u64();
            PumpTradeEvent {
                mint,
                sol_amount,
                token_amount,
                is_buy,
                user,
                timestamp,
                virtual_sol_reserves,
                virtual_token_reserves,
                real_sol_reserves,
                real_token_reserves,
                fee_recipient,
                fee_basis_points,
                fee,
                creator,
                creator_fee_basis_points,
                creator_fee,
                ix_name,
                mayhem_mode,
                buyback_fee_basis_points,
                buyback_fee,
            }
        })
        .collect()
}

/// `sha256("event:CompleteEvent")[..8]`.
pub const PUMP_COMPLETE_EVENT: [u8; 8] = [0x5f, 0x72, 0x61, 0x9c, 0xd4, 0x2e, 0x98, 0x08];
/// `sha256("event:PostCompleteBuyEvent")[..8]` (pump's 2026-10-08 redeploy).
pub const PUMP_POST_COMPLETE_BUY_EVENT: [u8; 8] = [0x6f, 0xb0, 0x6d, 0x8b, 0x31, 0x6c, 0xd5, 0xfb];

/// pump's `PostCompleteBuyEvent` (2026-10-08): the synthetic migration's pool part of a buy past
/// `rT` (IDL `types.PostCompleteBuyEvent`, 224 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PumpPostCompleteBuyEvent {
    pub user: Pubkey,
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub quote_mint: Pubkey,
    pub timestamp: i64,
    /// Tokens bought from the would-be pool (`post_complete_base_out`).
    pub base_out: u64,
    /// The pool part's pre-fee quote (`post_complete_quote_in`).
    pub quote_in: u64,
    pub fee_basis_points: u64,
    /// The protocol fee on `quote_in` (the buyback included).
    pub fee: u64,
    pub creator_fee_basis_points: u64,
    pub creator_fee: u64,
    pub buyback_fee: u64,
    pub pool_base_reserves_before: u64,
    pub pool_quote_reserves_before: u64,
    pub pool_base_reserves_after: u64,
    pub pool_quote_reserves_after: u64,
}

/// Every pump `PostCompleteBuyEvent` a transaction emitted.
pub fn pump_post_complete_buy_events(meta: &TransactionMetadata) -> Vec<PumpPostCompleteBuyEvent> {
    crate::events::raw(meta)
        .iter()
        .filter_map(|p| p.strip_prefix(&PUMP_POST_COMPLETE_BUY_EVENT[..]))
        .map(|b| {
            let mut c = Cursor { b, o: 0 };
            PumpPostCompleteBuyEvent {
                user: c.key(),
                mint: c.key(),
                bonding_curve: c.key(),
                quote_mint: c.key(),
                timestamp: c.i64(),
                base_out: c.u64(),
                quote_in: c.u64(),
                fee_basis_points: c.u64(),
                fee: c.u64(),
                creator_fee_basis_points: c.u64(),
                creator_fee: c.u64(),
                buyback_fee: c.u64(),
                pool_base_reserves_before: c.u64(),
                pool_quote_reserves_before: c.u64(),
                pool_base_reserves_after: c.u64(),
                pool_quote_reserves_after: c.u64(),
            }
        })
        .collect()
}

/// The pump events a transaction emitted, in order, by name (`TradeEvent`, `CompleteEvent`,
/// `PostCompleteBuyEvent`; any other pump event as `"other"`).
pub fn pump_event_names(meta: &TransactionMetadata) -> Vec<&'static str> {
    crate::events::raw(meta)
        .iter()
        .filter_map(|p| {
            let d: [u8; 8] = p.get(..8)?.try_into().ok()?;
            Some(match d {
                PUMP_TRADE_EVENT => "TradeEvent",
                PUMP_COMPLETE_EVENT => "CompleteEvent",
                PUMP_POST_COMPLETE_BUY_EVENT => "PostCompleteBuyEvent",
                _ => "other",
            })
        })
        .collect()
}

// ---- the mainnet SPL programs (M10, T-MEMO) ---------------------------------------------------

/// The manifest group of the mainnet SPL programs (M10): Token-2022 as deployed (upgradeable;
/// its ProgramData at deploy slot 427,147,035 with the live upgrade authority, `header ‖ ELF ‖
/// zeros` up to 1,382,061 bytes) and spl-memo v3 (BPFLoader2: the account data is the ELF).
pub const SPL_GROUPS: [&str; 1] = ["spl_programs"];

/// The deployed Token-2022's executable hash: sha256 of its ProgramData after the 45-byte
/// header with the trailing zeros trimmed (solana-verify's hash). It equals OtterSec's verified
/// build of `solana-program/token-2022@9bc02757` (program v11.0.0).
pub const TOKEN_2022_MAINNET_TRIMMED_SHA256: &str =
    "7ea94a027005b39196fa08e6d0ddcd55ec32eb43266179921a834a1ba2eb1947";

/// sha256 of the deployed spl-memo v3 program account (74,800 bytes; its data is the ELF).
pub const SPL_MEMO_V3_MAINNET_SHA256: &str =
    "f520eaf096361abbb9639ea4dc3e5388a87b9330e121f476607b87c46ef67954";

/// The opt-out: `SHORTWIN_BUNDLED_SPL=1` keeps LiteSVM's bundled Token-2022 and spl-memo v3
/// builds in every [`Env`] instead of [`load_mainnet_spl`]'s.
pub const BUNDLED_SPL_ENV: &str = "SHORTWIN_BUNDLED_SPL";

/// Whether [`BUNDLED_SPL_ENV`] is `1` (the environments keep LiteSVM's bundled SPL builds).
pub fn bundled_spl() -> bool {
    std::env::var_os(BUNDLED_SPL_ENV).is_some_and(|v| v == "1")
}

/// sha256 (lowercase hex) of an upgradeable program's ELF as solana-verify hashes it: the
/// ProgramData after its 45-byte header, trailing zero bytes trimmed.
pub fn trimmed_elf_sha256(program_data: &[u8]) -> String {
    let elf = program_data.get(45..).unwrap_or_default();
    let end = elf.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    sha256_hex(&elf[..end])
}

/// The mainnet SPL fixtures, rebuilt and sha256-checked against the manifest once per process,
/// and held to the pins above.
fn spl_fixtures() -> &'static HashMap<String, Fixture> {
    static F: OnceLock<HashMap<String, Fixture>> = OnceLock::new();
    F.get_or_init(|| {
        let (by_name, _, absent) = load_entries(&manifest(), &SPL_GROUPS);
        assert!(absent.is_empty(), "spl_programs: absent entries {absent:?}");
        let pd = by_name.get("token_2022_program_data").expect(
            "no token_2022_program_data (scripts/dump-fixtures.py fetch --group spl_programs)",
        );
        assert_eq!(
            trimmed_elf_sha256(&pd.account.data),
            TOKEN_2022_MAINNET_TRIMMED_SHA256,
            "token_2022_program_data: not the pinned mainnet Token-2022"
        );
        let memo = by_name
            .get("spl_memo_v3_program")
            .expect("no spl_memo_v3_program");
        assert_eq!(
            sha256_hex(&memo.account.data),
            SPL_MEMO_V3_MAINNET_SHA256,
            "spl_memo_v3_program: not the pinned mainnet spl-memo v3"
        );
        by_name
    })
}

/// The mainnet SPL fixture `name` (group `spl_programs`).
pub fn spl_fixture(name: &str) -> &'static Fixture {
    spl_fixtures()
        .get(name)
        .unwrap_or_else(|| panic!("no fixture `{name}` in group spl_programs"))
}

/// Replaces LiteSVM's bundled Token-2022 and spl-memo v3 with the programs mainnet runs
/// (T-MEMO, U17), exactly as mainnet holds their accounts: Token-2022 under the upgradeable
/// loader from its dumped ProgramData (real header: deploy slot 427,147,035 and the live
/// upgrade authority; the ELF; the zero padding), and spl-memo v3 under BPFLoader2. Each
/// account is checked against the manifest's sha256 and the pins
/// ([`TOKEN_2022_MAINNET_TRIMMED_SHA256`], [`SPL_MEMO_V3_MAINNET_SHA256`]).
///
/// [`Env`]'s constructors call it right after `LiteSVM::new()` unless [`bundled_spl`]. SPL
/// Token (p-token under the mainnet feature set), the ATA program and spl-memo v1 stay
/// LiteSVM's.
pub fn load_mainnet_spl(svm: &mut LiteSVM) {
    // The ProgramData first: setting the program account compiles the ELF it names.
    for name in [
        "token_2022_program_data",
        "token_2022_program",
        "spl_memo_v3_program",
    ] {
        let f = spl_fixture(name);
        svm.set_account(f.address, f.account.clone())
            .unwrap_or_else(|e| panic!("set_account({name}): {e:?}"));
    }
}

// ---- Squads v4 (M10 WP-7) --------------------------------------------------------------------

/// The manifest group of Squads v4 (M10): its program account (upgradeable loader), its
/// ProgramData (`header ‖ ELF ‖ zeros` up to 1,470,461 bytes; deploy slot 302,582,236, **no**
/// upgrade authority) and its `ProgramConfig` PDA (creation fee 0, the mainnet treasury).
pub const SQUADS_GROUPS: [&str; 1] = ["squads_programs"];

/// The deployed Squads v4's executable hash (the trimmed ELF, solana-verify's hash). Source
/// UNVERIFIED (probably `Squads-Protocol/v4@64af733`, program 2.1.0; tests/fixtures/README.md).
pub const SQUADS_V4_MAINNET_TRIMMED_SHA256: &str =
    "d48660833989ecea3145ff726164fe640bd90696f03ce00dfd0cda258cbf2fac";

/// Squads v4's deploy slot (its ProgramData header; immutable since).
pub const SQUADS_V4_DEPLOY_SLOT: u64 = 302_582_236;

/// The Squads fixtures, rebuilt and sha256-checked against the manifest once per process, and
/// held to the pins above (trimmed ELF, deploy slot, no upgrade authority, the program's own
/// address and `ProgramConfig` PDA).
fn squads_fixtures() -> &'static HashMap<String, Fixture> {
    static F: OnceLock<HashMap<String, Fixture>> = OnceLock::new();
    F.get_or_init(|| {
        let (by_name, _, absent) = load_entries(&manifest(), &SQUADS_GROUPS);
        assert!(
            absent.is_empty(),
            "squads_programs: absent entries {absent:?}"
        );
        let pd = by_name.get("squads_v4_program_data").expect(
            "no squads_v4_program_data (scripts/dump-fixtures.py fetch --group squads_programs)",
        );
        assert_eq!(
            trimmed_elf_sha256(&pd.account.data),
            SQUADS_V4_MAINNET_TRIMMED_SHA256,
            "squads_v4_program_data: not the pinned mainnet Squads v4"
        );
        let header = shortwin_venue_state::program_data::read(
            &pd.account.owner.to_bytes(),
            &pd.account.data,
        )
        .expect("squads_v4_program_data: a ProgramData header");
        assert_eq!(
            (header.slot, header.upgrade_authority),
            (SQUADS_V4_DEPLOY_SLOT, None),
            "squads_v4_program_data: deploy slot / authority"
        );
        assert_eq!(by_name["squads_v4_program"].address, squads_v4_ix::ID);
        let pc = &by_name["squads_program_config"];
        assert_eq!(
            (pc.address, pc.account.owner),
            (squads_v4_ix::pda::program_config(), squads_v4_ix::ID)
        );
        by_name
    })
}

/// The Squads fixture `name` (group `squads_programs`).
pub fn squads_fixture(name: &str) -> &'static Fixture {
    squads_fixtures()
        .get(name)
        .unwrap_or_else(|| panic!("no fixture `{name}` in group squads_programs"))
}

/// Deploys Squads v4 exactly as mainnet holds it — its ProgramData (real header: deploy slot
/// 302,582,236, no upgrade authority; the ELF; the zero padding) and program account under the
/// upgradeable loader — and its `ProgramConfig` (`multisig_create_v2` reads the creation fee
/// and the treasury from it). [`Env::with_squads`] calls it.
pub fn load_squads(svm: &mut LiteSVM) {
    // The ProgramData first: setting the program account compiles the ELF it names.
    for name in [
        "squads_v4_program_data",
        "squads_v4_program",
        "squads_program_config",
    ] {
        let f = squads_fixture(name);
        svm.set_account(f.address, f.account.clone())
            .unwrap_or_else(|e| panic!("set_account({name}): {e:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// T-REPIN-2: the build probe tells every build the fixtures hold apart, each loaded as the
    /// loader would (ProgramData, then the program account): pump's four (`buy_v3`, then
    /// `multi_hop_curve_swap`, then — the 2026-10-13 build adds no instruction — `create_v2` one
    /// level too deep, whose `CurveDepthExceeded` names the cap of 5 on the 2026-10-13 build
    /// only), PumpSwap's three (`buy_v2`, then `multi_hop_swap`) and pump-fees' 2026-10-12 build
    /// from the older ones (`withdraw`); PumpSwap and pump-fees answer as the newest deploy that
    /// holds their build. It also settles each probe's error order: an instruction a build lacks
    /// is `InstructionFallbackNotFound` (101), one it has is `AccountNotEnoughKeys` (3005) without
    /// accounts — before any signer check, so the admin `set_max_curve_depth` answers 3005 on the
    /// 2026-10-08 and 2026-10-13 pump too — and the depth probe answers alike and leaves every
    /// account it writes for its call as it found it: `Global`, whichever `max_curve_depth` it
    /// holds (the dump's 1 or mainnet's 2, since slot 456,495,021), the `QuoteControl` PDA, and
    /// `synthetic_stuck`'s mint and curve, absent (2026-10-13 build) or loaded (2026-10-08).
    #[test]
    fn pump_build_running_tells_three_builds() {
        let (mut env, _) = Env::fork_pump_coin(PumpCoin::Fresh166);
        let (absent, present) = (
            Some(ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND),
            Some(ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS),
        );
        let rank = |d: PumpDeploy| PumpDeploy::ALL.iter().position(|x| *x == d);
        for deploy in PumpDeploy::ALL.into_iter().rev() {
            for p in PumpProgram::ALL {
                load_pump_deploy(&mut env.svm, p, deploy);
                let held = PumpDeploy::held(&env.svm, p).expect("a known build");
                assert!(held.same_build(deploy, p), "{deploy:?} {p:?}: {held:?}");
                assert!(
                    PumpDeploy::ALL
                        .iter()
                        .all(|d| !d.same_build(deploy, p) || rank(*d) <= rank(held)),
                    "{deploy:?} {p:?}: the newest deploy holding the build"
                );
            }
            env.warp(1);
            let newer = matches!(deploy, PumpDeploy::Oct2026Synth | PumpDeploy::Oct2026Chain);
            assert_eq!(env.pump_build_running(PumpProgram::Pump), deploy);
            let amm = env.pump_build_running(PumpProgram::Amm);
            assert!(
                amm.same_build(deploy, PumpProgram::Amm),
                "{deploy:?}: {amm:?}"
            );
            // pump-fees: `withdraw` tells the 2026-10-12 build from the older two, which share
            // their instructions (both answer `Oct2026`).
            let fees = env.pump_build_running(PumpProgram::Fees);
            if newer {
                assert!(fees.same_build(deploy, PumpProgram::Fees), "{deploy:?}");
            } else {
                assert_eq!(fees, PumpDeploy::Oct2026, "{deploy:?}");
            }
            let code = env.pump_probe(PumpProgram::Pump, PUMP_SET_MAX_CURVE_DEPTH);
            assert_eq!(code, if newer { present } else { absent }, "{deploy:?}");
            let code = env.pump_probe(PumpProgram::Amm, PUMP_MULTI_HOP_CURVE_SWAP);
            assert_eq!(code, absent, "{deploy:?}: pump's discriminator on PumpSwap");
            if newer {
                // The 2026-10-13 build (probed first) without synthetic_stuck's accounts, the
                // 2026-10-08 one with them; mainnet's max_curve_depth (2), then the dump's (1),
                // which stays.
                let with_coin = deploy == PumpDeploy::Oct2026Synth;
                if with_coin {
                    env.load_pump_coin(PumpCoin::SyntheticStuck);
                }
                let q = PumpCurve::of(PumpCoin::SyntheticStuck);
                let written = [PUMP_GLOBAL, pump_quote_control(), q.mint, q.curve];
                for depth in [2u8, 1] {
                    patch(&mut env.svm, &PUMP_GLOBAL, |d| {
                        d[PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH] = depth;
                    });
                    let found: Vec<_> = written.iter().map(|k| env.svm.get_account(k)).collect();
                    let what = format!("{deploy:?}, max_curve_depth {depth}");
                    assert!(found[1].is_none(), "{what}: the dump holds no QuoteControl");
                    assert_eq!(found[2].is_some(), with_coin, "{what}: the coin's mint");
                    assert_eq!(found[3].is_some(), with_coin, "{what}: the coin's curve");
                    env.warp(1);
                    let (code, logs) = env.pump_create_v2_depth_probe();
                    assert_eq!(code, Some(PUMP_ERR_CURVE_DEPTH_EXCEEDED), "{what}");
                    let cap = logs.iter().any(|l| l.contains(PUMP_DEPTH_CAP_MESSAGE));
                    assert_eq!(cap, deploy == PumpDeploy::Oct2026Chain, "{what}: {logs:#?}");
                    let left: Vec<_> = written.iter().map(|k| env.svm.get_account(k)).collect();
                    assert_eq!(
                        left, found,
                        "{what}: the probe leaves Global, QuoteControl and the coin's mint and \
                         curve as it found them"
                    );
                    env.warp(1);
                    assert_eq!(env.pump_build_running(PumpProgram::Pump), deploy, "{what}");
                }
            }
        }
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm9"), None);
        assert_eq!(base64_decode("Zm9!"), None);
    }
}
