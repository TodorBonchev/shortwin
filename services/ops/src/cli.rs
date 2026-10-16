//! `shortwin-gov`'s command line and commands (the binary is `src/main.rs`; the end-to-end test
//! drives [`run`] against LiteSVM).

use std::path::PathBuf;
use std::str::FromStr;

use anchor_lang::solana_program::instruction::Instruction;
use anyhow::{Context, bail, ensure};
use serde::Deserialize;
use shortwin::constants::{VENUE_PROGRAM_DATA, VENUE_PROGRAMS};
use shortwin::instructions::set_venue_extras::{ExtraEntryArg, SeedPartArg};
use shortwin::state::{
    ConfigChange, EmaParamsArgs, GlobalConfig, InitConfigArgs, Params, TierParams, VenueKind,
};
use shortwin_services_common::{AccountData, Chain};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use squads_v4_ix::{self as sq, Member, MultisigCreateArgs};

use crate::config::{self, Layout, MsLayout, VENUES};
use crate::ixs::{self, loader};
use crate::send::Ops;
use crate::{keys, squads, verify};

/// The usage text.
pub const USAGE: &str = "\
usage: shortwin-gov <command> --config <governance.toml> --rpc <url> [--dry-run] [options]

  create-multisigs --payer <key> --gov-create-key <key> --emg-create-key <key>
      create Governance and Emergency (autonomous; members, threshold, time lock from the
      layout; rent collector = own vault 0) and pre-fund Governance vault 1 (fee_recipient)
  init-config --authority <key>
      initialize_config by short.win's upgrade authority with the vaults as authorities
  stage-upgrade-authority --checked --key <member> [--program <name>]...
      a Governance vault transaction moving each program's upgrade authority to Governance
      vault 0 with SetAuthorityChecked (its execution is co-signed by the current authority)
  stage-authorities --key <member>
      a Governance vault transaction proposing the layout's four authorities (propose_config)
  stage-upgrade --key <member> --buffer <address> [--spill <address>] [--program <name>]
      a Governance vault transaction upgrading the program from a buffer the vault owns
  stage <admin-ix> [args] --key <member> [--ms governance|emergency]
      set-pause <global|MARKET> <on|off>          set-route-broken <global|MARKET> <venue> <on|off>
      disable-venue <venue>                       revalidate-venue <venue-guard-index> <slot>
      clear-coverage-pause <MARKET>               set-venue-extras <venue> <sell|buy|claim> <file>
      cancel-config                               propose-timelock <seconds>
      propose-enable-venues <venue>...            propose-kink <bps>
      propose-oracle-feed <account> <feed-id-hex>
      (Emergency vault 0 signs by default; propose-* are Governance's; a message too large for
      one transaction goes through a transaction buffer, or always with --force-buffer)
  approve --ms <ms> --index <n> --key <member> [--execute] [--co-signer <key>]...
  execute --ms <ms> --index <n> --key <member> [--co-signer <key>]...
  cancel --ms <ms> --index <n> --key <member>        (proposal_cancel_v2 of an Approved one)
  apply --key <payer>        (apply_config once short.win's own timelock has run)
  close-buffer --ms <ms> --key <member> [--buffer-index <n>]
      close the member's transaction buffer a failed or abandoned staging left (rent back)
  status [--ms <ms> --index <n>]
  verify                     (exit status 1 on any failed check)

Staging options: --force-buffer, --buffer-index <n> (default 0; a buffer an interrupted run left
with the start of the same message is resumed), --allow-duplicate (stage although the newest
transaction holds the same message and is still open).
Keys are keypair FILES given on the command line (never hardware paths, never printed).
--dry-run simulates and sends nothing (a command of several transactions stops after the first).
--confirm-timeout <s>: how long a sent transaction is awaited (default 90 s, past its blockhash's
lifetime, so one reported as not confirmed has expired); check `status` before rerunning one.";

/// Parsed arguments.
#[derive(Debug, Default)]
pub struct Args {
    pub command: String,
    pub positional: Vec<String>,
    pub config: Option<PathBuf>,
    pub rpc: Option<String>,
    pub dry_run: bool,
    pub key: Option<String>,
    pub payer: Option<String>,
    pub authority: Option<String>,
    pub gov_create_key: Option<String>,
    pub emg_create_key: Option<String>,
    pub co_signers: Vec<String>,
    pub ms: Option<String>,
    pub index: Option<u64>,
    pub programs: Vec<String>,
    pub checked: bool,
    pub buffer: Option<Pubkey>,
    pub spill: Option<Pubkey>,
    pub force_buffer: bool,
    pub buffer_index: u8,
    pub allow_duplicate: bool,
    pub execute: bool,
    /// `--confirm-timeout <s>` (the binary's RPC client; `None`: its default).
    pub confirm_timeout: Option<u64>,
}

/// Parses `argv` (without the program name).
pub fn parse(argv: &[String]) -> anyhow::Result<Args> {
    let mut a = Args::default();
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--config" => a.config = Some(PathBuf::from(val()?)),
            "--rpc" => a.rpc = Some(val()?),
            "--dry-run" => a.dry_run = true,
            "--key" => a.key = Some(val()?),
            "--payer" => a.payer = Some(val()?),
            "--authority" => a.authority = Some(val()?),
            "--gov-create-key" => a.gov_create_key = Some(val()?),
            "--emg-create-key" => a.emg_create_key = Some(val()?),
            "--co-signer" => a.co_signers.push(val()?),
            "--ms" => a.ms = Some(val()?),
            "--index" => a.index = Some(val()?.parse().context("--index")?),
            "--program" => a.programs.push(val()?),
            "--checked" => a.checked = true,
            "--buffer" => a.buffer = Some(config::pubkey(&val()?, "--buffer")?),
            "--spill" => a.spill = Some(config::pubkey(&val()?, "--spill")?),
            "--force-buffer" => a.force_buffer = true,
            "--buffer-index" => a.buffer_index = val()?.parse().context("--buffer-index")?,
            "--allow-duplicate" => a.allow_duplicate = true,
            "--execute" => a.execute = true,
            "--confirm-timeout" => {
                a.confirm_timeout = Some(val()?.parse().context("--confirm-timeout <seconds>")?)
            }
            "-h" | "--help" => bail!("{USAGE}"),
            s if s.starts_with("--") => bail!("unknown option {s}\n{USAGE}"),
            s if a.command.is_empty() => a.command = s.to_string(),
            s => a.positional.push(s.to_string()),
        }
    }
    ensure!(!a.command.is_empty(), "{USAGE}");
    Ok(a)
}

fn need<'a>(v: &'a Option<String>, flag: &str) -> anyhow::Result<&'a str> {
    v.as_deref()
        .with_context(|| format!("{flag} <keypair file> is required"))
}

fn index(a: &Args) -> anyhow::Result<u64> {
    a.index.context("--index <n> is required")
}

/// Runs the command in `argv` against `chain`, writing the report to `out`. `Ok(false)`: the
/// command ran but its checks failed (`verify`).
pub async fn run<C: Chain>(
    chain: &C,
    argv: &[String],
    out: &mut dyn std::io::Write,
) -> anyhow::Result<bool> {
    let a = parse(argv)?;
    let layout = Layout::load(a.config.as_deref().context("--config <file> is required")?)?;
    let ops = Ops::new(chain, a.dry_run, out);
    dispatch(&ops, &layout, &a).await
}

async fn dispatch<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<bool> {
    let sends = !matches!(a.command.as_str(), "status" | "verify");
    if sends {
        l.ensure_not_example()?;
    }
    match a.command.as_str() {
        "create-multisigs" => create_multisigs(ops, l, a).await?,
        "init-config" => init_config(ops, l, a).await?,
        "stage-upgrade-authority" => stage_upgrade_authority(ops, l, a).await?,
        "stage-authorities" => stage_authorities(ops, l, a).await?,
        "stage-upgrade" => stage_upgrade(ops, l, a).await?,
        "stage" => stage_admin(ops, l, a).await?,
        "approve" => {
            let ms = l.ms(a.ms.as_deref().context("--ms is required")?)?;
            let key = keys::load(need(&a.key, "--key")?, "--key")?;
            let co = co_signers(a)?;
            squads::approve(ops, ms, &key, index(a)?, a.execute, &co).await?;
        }
        "execute" => {
            let ms = l.ms(a.ms.as_deref().context("--ms is required")?)?;
            let key = keys::load(need(&a.key, "--key")?, "--key")?;
            let co = co_signers(a)?;
            squads::execute(ops, ms, &key, index(a)?, &co).await?;
        }
        "cancel" => {
            let ms = l.ms(a.ms.as_deref().context("--ms is required")?)?;
            let key = keys::load(need(&a.key, "--key")?, "--key")?;
            squads::cancel(ops, ms, &key, index(a)?).await?;
        }
        "close-buffer" => {
            let ms = l.ms(a.ms.as_deref().context("--ms is required")?)?;
            let key = keys::load(need(&a.key, "--key")?, "--key")?;
            squads::close_buffer(ops, ms, &key, a.buffer_index).await?;
        }
        "apply" => apply(ops, a).await?,
        "status" => status(ops, l, a).await?,
        "verify" => {
            let report = verify::verify(ops.chain, l).await?;
            ops.say(report.render().trim_end());
            return Ok(report.passed());
        }
        other => bail!("unknown command {other:?}\n{USAGE}"),
    }
    Ok(true)
}

fn co_signers(a: &Args) -> anyhow::Result<Vec<Keypair>> {
    a.co_signers
        .iter()
        .map(|p| keys::load(p, "--co-signer"))
        .collect()
}

async fn account<C: Chain>(chain: &C, key: &Pubkey) -> anyhow::Result<Option<AccountData>> {
    Ok(chain.accounts(&[*key]).await?.remove(0))
}

/// Squads' `ProgramConfig`: `(multisig_creation_fee, treasury)`.
async fn squads_program_config<C: Chain>(chain: &C) -> anyhow::Result<(u64, Pubkey)> {
    let key = sq::pda::program_config();
    let acc = account(chain, &key).await?.with_context(|| {
        format!("Squads ProgramConfig {key} missing: is Squads v4 deployed here?")
    })?;
    ensure!(
        acc.owner == sq::ID && acc.data.len() >= 80,
        "Squads ProgramConfig {key}: unexpected account"
    );
    let fee = u64::from_le_bytes(acc.data[40..48].try_into()?);
    let treasury = Pubkey::new_from_array(acc.data[48..80].try_into()?);
    Ok((fee, treasury))
}

fn print_layout<C: Chain>(ops: &Ops<'_, C>, l: &Layout) {
    for ms in [&l.gov, &l.emg] {
        ops.say(format!(
            "{}: multisig {} ({}-of-{}, time lock {} s), vault 0 {}",
            ms.name,
            ms.key,
            ms.threshold,
            ms.members.len(),
            ms.time_lock,
            ms.vault(0)
        ));
    }
    ops.say(format!(
        "  governance         = {} (Governance vault 0)",
        l.governance()
    ));
    ops.say(format!(
        "  pause_authority    = {} (Emergency vault 0)",
        l.pause_authority()
    ));
    ops.say(format!("  coverage_authority = {}", l.coverage_authority));
    ops.say(format!(
        "  fee_recipient      = {} (Governance vault 1)",
        l.fee_recipient()
    ));
}

async fn create_multisigs<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<()> {
    let payer = keys::load(need(&a.payer, "--payer")?, "--payer")?;
    let gov_key = keys::load(
        need(&a.gov_create_key, "--gov-create-key")?,
        "--gov-create-key",
    )?;
    let emg_key = keys::load(
        need(&a.emg_create_key, "--emg-create-key")?,
        "--emg-create-key",
    )?;
    let (fee, treasury) = squads_program_config(ops.chain).await?;
    ops.say(format!(
        "Squads ProgramConfig: creation fee {fee} lamports to {treasury}"
    ));
    print_layout(ops, l);
    for (ms, create) in [(&l.gov, &gov_key), (&l.emg, &emg_key)] {
        ensure!(
            create.pubkey() == ms.create_key,
            "{}: the create key file holds {}, the layout names {}",
            ms.name,
            create.pubkey(),
            ms.create_key
        );
        if account(ops.chain, &ms.key).await?.is_some() {
            ops.say(format!(
                "{} multisig {} exists: skipped (verify checks it)",
                ms.name, ms.key
            ));
            continue;
        }
        let args = MultisigCreateArgs {
            config_authority: None,
            threshold: ms.threshold,
            members: ms.members.iter().map(|k| Member::full(*k)).collect(),
            time_lock: ms.time_lock,
            rent_collector: Some(ms.vault(0)),
            memo: None,
        };
        let ix = sq::ix::multisig_create_v2(&treasury, &ms.create_key, &payer.pubkey(), &args);
        ops.send(&format!("create {}", ms.name), &payer, &[ix], &[create])
            .await?;
        if ops.stop_dry_run() {
            return Ok(());
        }
    }
    let fee_vault = l.fee_recipient();
    let have = account(ops.chain, &fee_vault)
        .await?
        .map_or(0, |a| a.lamports);
    if have < verify::FEE_VAULT_PREFUND {
        let ix = ixs::system_transfer(
            &payer.pubkey(),
            &fee_vault,
            verify::FEE_VAULT_PREFUND - have,
        );
        ops.send(
            "pre-fund fee_recipient (Governance vault 1)",
            &payer,
            &[ix],
            &[],
        )
        .await?;
    } else {
        ops.say(format!(
            "fee_recipient {fee_vault} holds {have} lamports: funded"
        ));
    }
    Ok(())
}

/// The venue programs' live deploy slots (`None` where the ProgramData is missing).
async fn live_guard_slots<C: Chain>(chain: &C) -> anyhow::Result<Vec<Option<u64>>> {
    Ok(chain
        .accounts(&VENUE_PROGRAM_DATA)
        .await?
        .iter()
        .map(|a| a.as_ref().and_then(verify::deploy_slot))
        .collect())
}

async fn init_config<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<()> {
    let authority = keys::load(need(&a.authority, "--authority")?, "--authority")?;
    let pd = loader::program_data(&shortwin::ID);
    let current = account(ops.chain, &pd)
        .await?
        .and_then(|a| verify::upgrade_authority(&a))
        .context("short.win's ProgramData is missing")?;
    ensure!(
        current == Some(authority.pubkey()),
        "initialize_config must be signed by short.win's upgrade authority {}; --authority is {}",
        current.map_or("(none)".to_string(), |k| k.to_string()),
        authority.pubkey()
    );
    ensure!(
        account(ops.chain, &ixs::pdas::config()).await?.is_none(),
        "GlobalConfig exists already"
    );
    let fee_vault = l.fee_recipient();
    let funded = account(ops.chain, &fee_vault)
        .await?
        .map_or(0, |a| a.lamports);
    ensure!(
        funded >= verify::FEE_VAULT_PREFUND,
        "fee_recipient {fee_vault} holds {funded} lamports (run create-multisigs first)"
    );
    // VenueGuard: the configured slots must be the live ones.
    let live = live_guard_slots(ops.chain).await?;
    let mut guard_slot = [0u64; VENUE_PROGRAMS];
    for (i, slot) in guard_slot.iter_mut().enumerate() {
        *slot = match (l.guard_slots.map(|g| g[i]), live[i]) {
            (Some(want), Some(got)) if want != got => bail!(
                "guard slot {i} ({}): the layout says {want}, the ProgramData was deployed at \
                 {got} — review the upgrade and update config.guard_slots",
                VENUE_PROGRAM_DATA[i]
            ),
            (Some(want), Some(_)) => want,
            (None, Some(got)) => got,
            (Some(want), None) => {
                ops.say(format!(
                    "warning: guard slot {i}: ProgramData {} not found on this cluster; using \
                     the configured {want}",
                    VENUE_PROGRAM_DATA[i]
                ));
                want
            }
            (None, None) => bail!(
                "guard slot {i}: ProgramData {} not found and config.guard_slots not set",
                VENUE_PROGRAM_DATA[i]
            ),
        };
    }
    let args = InitConfigArgs {
        governance: l.governance(),
        pause_authority: l.pause_authority(),
        coverage_authority: l.coverage_authority,
        fee_recipient: fee_vault,
        pyth_sol_usd: l.pyth_sol_usd,
        pyth_feed_id: l.pyth_feed_id,
        timelock_s: l.timelock_s,
        venue_enabled: l.venue_enabled,
        params: Params::defaults(),
        tiers: TierParams::defaults(),
        ema: EmaParamsArgs::defaults(),
        util_kink_bps: shortwin_math::params::Params::DEFAULT.util_kink_bps,
        guard_slot,
    };
    print_layout(ops, l);
    ops.say(format!(
        "  timelock_s {}, venue_enabled {:#b}, guard slots {guard_slot:?}",
        l.timelock_s, l.venue_enabled
    ));
    let ix = ixs::initialize_config(&authority.pubkey(), args);
    ops.send("initialize_config", &authority, &[ix], &[])
        .await?;
    Ok(())
}

fn stage_opts(a: &Args) -> squads::StageOpts {
    squads::StageOpts {
        force_buffer: a.force_buffer,
        buffer_index: a.buffer_index,
        allow_duplicate: a.allow_duplicate,
    }
}

fn member_key(a: &Args) -> anyhow::Result<Keypair> {
    keys::load(need(&a.key, "--key")?, "--key")
}

async fn report_staged<C: Chain>(ops: &Ops<'_, C>, ms: &MsLayout, index: u64) {
    if !ops.dry_run {
        ops.say(format!(
            "staged {} #{index}: {} more approval(s) needed (approve --ms {} --index {index})",
            ms.name,
            ms.threshold.saturating_sub(1),
            if ms.name == "Governance" {
                "governance"
            } else {
                "emergency"
            }
        ));
    }
}

async fn stage_upgrade_authority<C: Chain>(
    ops: &Ops<'_, C>,
    l: &Layout,
    a: &Args,
) -> anyhow::Result<()> {
    ensure!(
        a.checked,
        "only the checked transfer is supported (--checked): SetAuthority without the new \
         authority's signature hands the program to whatever key is typed (T-SQ-5)"
    );
    let key = member_key(a)?;
    let vault = l.governance();
    let programs: Vec<&config::UpgradeableProgram> = if a.programs.is_empty() {
        l.programs.iter().collect()
    } else {
        a.programs
            .iter()
            .map(|n| {
                l.programs.iter().find(|p| p.name == *n).with_context(|| {
                    format!("--program {n}: not in the layout's [upgrade] programs")
                })
            })
            .collect::<anyhow::Result<_>>()?
    };
    let mut list = Vec::new();
    for p in programs {
        let pd = p.program_data();
        let current = account(ops.chain, &pd)
            .await?
            .and_then(|a| verify::upgrade_authority(&a))
            .with_context(|| format!("{}: ProgramData {pd} missing", p.name))?;
        match current {
            Some(c) if c == vault => {
                ops.say(format!("{}: Governance vault 0 holds it already", p.name))
            }
            Some(c) => {
                ops.say(format!("{}: {c} -> {vault} (checked)", p.name));
                list.push(loader::set_authority_checked(&pd, &c, &vault));
            }
            None => bail!("{}: immutable (no upgrade authority)", p.name),
        }
    }
    ensure!(!list.is_empty(), "nothing to transfer");
    let index = squads::stage(ops, &l.gov, &key, 0, &list, stage_opts(a)).await?;
    report_staged(ops, &l.gov, index).await;
    Ok(())
}

async fn read_config<C: Chain>(chain: &C) -> anyhow::Result<GlobalConfig> {
    let acc = account(chain, &ixs::pdas::config())
        .await?
        .context("GlobalConfig missing (init-config)")?;
    use anchor_lang::Discriminator;
    ensure!(
        acc.owner == shortwin::ID && acc.data.starts_with(GlobalConfig::DISCRIMINATOR),
        "GlobalConfig: unexpected account"
    );
    let body = acc
        .data
        .get(8..8 + core::mem::size_of::<GlobalConfig>())
        .context("GlobalConfig: short")?;
    Ok(bytemuck::pod_read_unaligned(body))
}

async fn stage_authorities<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<()> {
    let key = member_key(a)?;
    let c = read_config(ops.chain).await?;
    let current = [
        c.governance,
        c.pause_authority,
        c.coverage_authority,
        c.fee_recipient,
    ];
    let want = l.authorities();
    ensure!(
        current != want,
        "the config already holds the layout's authorities: nothing to change"
    );
    for ((name, cur), w) in config::AUTHORITY_NAMES.iter().zip(current).zip(want) {
        let mark = if cur == w { "  " } else { "->" };
        ops.say(format!("  {name:<18} {cur} {mark} {w}"));
    }
    let change = ConfigChange::Authorities {
        governance: want[0],
        pause_authority: want[1],
        coverage_authority: want[2],
        fee_recipient: want[3],
    };
    let ix = ixs::propose_config(&c.governance, change);
    let ms = governing_ms(l, &c.governance)?;
    let index = squads::stage(ops, ms, &key, 0, &[ix], stage_opts(a)).await?;
    report_staged(ops, ms, index).await;
    Ok(())
}

/// The layout's multisig whose vault 0 is the config's `governance`: the one that proposes.
/// Only the layout's own two multisigs are searched. Moving `governance` to a **new** multisig
/// is therefore not supported: a layout naming the new Governance create key no longer names
/// the current one, which must stage the change (runbook R10: unsupported by the tool).
fn governing_ms<'a>(l: &'a Layout, governance: &Pubkey) -> anyhow::Result<&'a MsLayout> {
    [&l.gov, &l.emg]
        .into_iter()
        .find(|ms| ms.vault(0) == *governance)
        .with_context(|| {
            format!(
                "the config's governance {governance} is no vault 0 of the layout's multisigs: \
                 moving governance to another multisig is not supported by this tool (it \
                 stages through the layout's multisigs only)"
            )
        })
}

async fn stage_upgrade<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<()> {
    let key = member_key(a)?;
    let name = a.programs.first().map_or("shortwin", String::as_str);
    let program = l
        .programs
        .iter()
        .find(|p| p.name == name)
        .with_context(|| format!("--program {name}: not in the layout's [upgrade] programs"))?;
    let buffer = a.buffer.context("--buffer <address> is required")?;
    let spill = a.spill.unwrap_or(key.pubkey());
    let vault = l.governance();
    let accs = ops
        .chain
        .accounts(&[buffer, program.program_data()])
        .await?;
    let b = accs[0]
        .as_ref()
        .with_context(|| format!("buffer {buffer} does not exist"))?;
    match ixs::loader::buffer_authority(&b.owner, &b.data) {
        Some(Some(auth)) if auth == vault => {}
        Some(Some(auth)) => bail!(
            "buffer {buffer}'s authority is {auth}, not Governance vault 0 {vault}: the upgrade \
             would fail at execution, after the time lock (`solana program set-buffer-authority \
             {buffer} --new-buffer-authority {vault}`)"
        ),
        Some(None) => bail!("buffer {buffer} has no authority (immutable)"),
        None => bail!("{buffer} is not a loader buffer"),
    }
    let pd = accs[1]
        .as_ref()
        .with_context(|| format!("{}: ProgramData missing", program.name))?;
    // The vault must hold the program's upgrade authority by now (R4), or the loader refuses
    // the upgrade at execution, after the time lock.
    match verify::upgrade_authority(pd) {
        Some(Some(auth)) if auth == vault => {}
        Some(Some(auth)) => bail!(
            "{}'s upgrade authority is {auth}, not Governance vault 0 {vault}: move it first \
             (stage-upgrade-authority --checked), or the upgrade fails at execution, after the \
             time lock",
            program.name
        ),
        Some(None) => bail!("{}: immutable (no upgrade authority)", program.name),
        None => bail!(
            "{}: {} is not a ProgramData account",
            program.name,
            program.program_data()
        ),
    }
    let elf = b.data.len() - loader::BUFFER_METADATA;
    let room = pd.data.len().saturating_sub(loader::PROGRAM_DATA_METADATA);
    ensure!(
        elf <= room,
        "the new ELF ({elf} B) does not fit {}'s ProgramData ({room} B): extend it by {} bytes \
         first (`solana program extend {} {}`; permissionless)",
        program.name,
        elf - room,
        program.id,
        elf - room
    );
    ops.say(format!(
        "{}: upgrade from buffer {buffer} ({elf} B ELF; ProgramData room {room} B), spill {spill}",
        program.name
    ));
    let ix = loader::upgrade(&program.id, &buffer, &spill, &vault);
    let index = squads::stage(ops, &l.gov, &key, 0, &[ix], stage_opts(a)).await?;
    report_staged(ops, &l.gov, index).await;
    Ok(())
}

fn on_off(s: &str) -> anyhow::Result<bool> {
    match s {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        other => bail!("expected on or off, got {other:?}"),
    }
}

fn scope(s: &str) -> anyhow::Result<Option<Pubkey>> {
    if s == "global" {
        Ok(None)
    } else {
        Ok(Some(config::pubkey(s, "market")?))
    }
}

/// A `[config] venues` name's bit index.
fn venue_bit(name: &str) -> anyhow::Result<u8> {
    VENUES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, bit)| bit.trailing_zeros() as u8)
        .with_context(|| format!("unknown venue {name:?}"))
}

/// A venue name's `VenueKind` (the `VenueExtras` venues).
fn venue_kind(name: &str) -> anyhow::Result<u8> {
    Ok(match name {
        "pump_curve" => VenueKind::PumpCurve,
        "pump_swap" => VenueKind::PumpSwap,
        "raydium_v4" => VenueKind::RaydiumV4,
        "raydium_cpmm" => VenueKind::RaydiumCpmm,
        other => bail!("set-venue-extras: unknown venue {other:?}"),
    } as u8)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtrasFile {
    #[serde(default)]
    entry: Vec<EntryFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryFile {
    /// 1 = a fixed key, 2 = a template derived under program `key`.
    kind: u8,
    #[serde(default)]
    writable: bool,
    key: String,
    #[serde(default)]
    seeds: Vec<SeedFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedFile {
    /// 1 = constant bytes (`hex`), 2 = a placeholder (`len` = its code).
    kind: u8,
    len: Option<u8>,
    hex: Option<String>,
}

/// Parses a `set-venue-extras` entries file (TOML: `[[entry]]` tables of `kind`, `writable`,
/// `key` and `seeds = [{ kind, len, hex }]`, with the program's numeric kinds).
pub fn parse_extras(text: &str) -> anyhow::Result<Vec<ExtraEntryArg>> {
    let f: ExtrasFile = toml::from_str(text).context("parsing the extras file")?;
    f.entry
        .iter()
        .map(|e| {
            let seeds = e
                .seeds
                .iter()
                .map(|s| {
                    let mut bytes = [0u8; 32];
                    let len = match &s.hex {
                        Some(h) => {
                            let h = h.trim();
                            ensure!(
                                h.len() % 2 == 0 && h.len() <= 64,
                                "seed hex: at most 32 bytes"
                            );
                            for i in 0..h.len() / 2 {
                                bytes[i] = u8::from_str_radix(&h[2 * i..2 * i + 2], 16)
                                    .context("seed hex")?;
                            }
                            s.len.unwrap_or((h.len() / 2) as u8)
                        }
                        None => s.len.context("a seed needs len or hex")?,
                    };
                    Ok(SeedPartArg {
                        kind: s.kind,
                        len,
                        bytes,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(ExtraEntryArg {
                kind: e.kind,
                writable: e.writable,
                key: config::pubkey(&e.key, "entry.key")?,
                seeds,
            })
        })
        .collect()
}

async fn stage_admin<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<()> {
    let key = member_key(a)?;
    let (name, rest) = a
        .positional
        .split_first()
        .context("stage <admin-ix>: which instruction?")?;
    let arg = |i: usize| -> anyhow::Result<&str> {
        rest.get(i)
            .map(String::as_str)
            .with_context(|| format!("stage {name}: missing argument {}\n{USAGE}", i + 1))
    };
    let governance_only = name.starts_with("propose-");
    let default_ms = if governance_only {
        "governance"
    } else {
        "emergency"
    };
    let ms = l.ms(a.ms.as_deref().unwrap_or(default_ms))?;
    ensure!(
        !governance_only || ms.key == l.gov.key,
        "stage {name}: only Governance proposes config changes"
    );
    let vault = ms.vault(0);
    let emergency_only = matches!(
        name.as_str(),
        "disable-venue" | "revalidate-venue" | "clear-coverage-pause"
    );
    ensure!(
        !emergency_only || ms.key == l.emg.key,
        "stage {name}: only the Emergency vault (pause_authority) may"
    );
    let ix: Instruction = match name.as_str() {
        "set-pause" => ixs::set_pause(&vault, scope(arg(0)?)?, on_off(arg(1)?)?),
        "set-route-broken" => ixs::set_route_broken(
            &vault,
            scope(arg(0)?)?,
            venue_bit(arg(1)?)?,
            on_off(arg(2)?)?,
        ),
        "disable-venue" => ixs::disable_venue(&vault, venue_bit(arg(0)?)?),
        "revalidate-venue" => {
            let venue: u8 = arg(0)?.parse().context("venue-guard index (0..5)")?;
            ensure!(
                usize::from(venue) < VENUE_PROGRAMS,
                "venue-guard index 0..{}",
                VENUE_PROGRAMS - 1
            );
            let slot: u64 = arg(1)?.parse().context("slot")?;
            // AIR-01: the revalidation names the reviewed deploy's trimmed-ELF sha256, read from
            // the ProgramData now (an extension landing before the execution then no longer
            // refuses it, and the guard records the fingerprint that tells extensions apart).
            let pd = VENUE_PROGRAM_DATA[usize::from(venue)];
            let acc = account(ops.chain, &pd)
                .await?
                .with_context(|| format!("venue ProgramData {pd} not found"))?;
            let deployed = verify::deploy_slot(&acc)
                .with_context(|| format!("{pd} is not a ProgramData account"))?;
            let hash = ixs::trimmed_elf_sha256(&acc.owner, &acc.data)
                .with_context(|| format!("{pd} is not a ProgramData account"))?;
            let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            ops.say(format!(
                "venue-guard index {venue}: ProgramData {pd} deployed at slot {deployed}, \
                 trimmed-ELF sha256 {hex} (compare with the reviewed build)"
            ));
            ensure!(
                deployed == slot,
                "the ProgramData's deploy slot is {deployed}, not {slot}: review that deploy"
            );
            ixs::revalidate_venue(&vault, venue, slot, hash)
        }
        "clear-coverage-pause" => {
            ixs::clear_coverage_pause(&vault, &config::pubkey(arg(0)?, "market")?)
        }
        "set-venue-extras" => {
            let venue = venue_kind(arg(0)?)?;
            let which = match arg(1)? {
                "sell" => 0,
                "buy" => 1,
                "claim" => 2,
                other => bail!("set-venue-extras: sell, buy or claim, not {other:?}"),
            };
            let path = arg(2)?;
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
            ixs::set_venue_extras(&vault, venue, which, parse_extras(&text)?)
        }
        "cancel-config" => ixs::cancel_config(&vault),
        "propose-timelock" => ixs::propose_config(
            &vault,
            ConfigChange::Timelock {
                timelock_s: arg(0)?.parse().context("seconds")?,
            },
        ),
        "propose-enable-venues" => {
            ensure!(!rest.is_empty(), "propose-enable-venues <venue>...");
            let bits = rest.iter().try_fold(0u32, |acc, v| {
                Ok::<_, anyhow::Error>(acc | 1 << venue_bit(v)?)
            })?;
            ixs::propose_config(&vault, ConfigChange::EnableVenues { bits })
        }
        "propose-kink" => ixs::propose_config(
            &vault,
            ConfigChange::Kink {
                util_kink_bps: arg(0)?.parse().context("bps")?,
            },
        ),
        "propose-oracle-feed" => ixs::propose_config(
            &vault,
            ConfigChange::OracleFeed {
                account: config::pubkey(arg(0)?, "account")?,
                feed_id: config::hex32(arg(1)?, "feed id")?,
            },
        ),
        other => bail!("stage: unknown admin instruction {other:?}\n{USAGE}"),
    };
    let index = squads::stage(ops, ms, &key, 0, &[ix], stage_opts(a)).await?;
    report_staged(ops, ms, index).await;
    Ok(())
}

async fn apply<C: Chain>(ops: &Ops<'_, C>, a: &Args) -> anyhow::Result<()> {
    let payer = keys::load(need(&a.key, "--key")?, "--key")?;
    let c = read_config(ops.chain).await?;
    ensure!(c.pending.is_some(), "no pending config change");
    let change = c
        .pending
        .change()
        .map_err(|e| anyhow::anyhow!("decoding the pending change: {e:?}"))?;
    let now = ops.chain.clock().await?.unix_timestamp;
    ops.say(format!(
        "pending: {} (applicable from {}, now {now}; the window closes {} s after it)",
        ixs::describe_change(&change),
        c.pending.not_before,
        shortwin::constants::CONFIG_APPLY_WINDOW_S
    ));
    let ix = ixs::apply_config(&change);
    ops.send("apply_config", &payer, &[ix], &[]).await?;
    Ok(())
}

async fn status<C: Chain>(ops: &Ops<'_, C>, l: &Layout, a: &Args) -> anyhow::Result<()> {
    print_layout(ops, l);
    if let (Some(name), Some(index)) = (&a.ms, a.index) {
        for line in squads::describe(ops.chain, l.ms(name)?, index).await? {
            ops.say(line);
        }
        return Ok(());
    }
    for ms in [&l.gov, &l.emg] {
        match squads::multisig(ops.chain, ms).await {
            Ok(m) => {
                ops.say(format!(
                    "{}: transaction index {}, stale below {}, threshold {}, time lock {} s",
                    ms.name,
                    m.transaction_index,
                    m.stale_transaction_index,
                    m.threshold,
                    m.time_lock
                ));
                let from = m.transaction_index.saturating_sub(4).max(1);
                for i in from..=m.transaction_index {
                    for line in squads::describe(ops.chain, ms, i).await? {
                        ops.say(format!("  {line}"));
                    }
                }
            }
            Err(e) => ops.say(format!("{}: {e:#}", ms.name)),
        }
    }
    match read_config(ops.chain).await {
        Ok(c) if c.pending.is_some() => {
            let what = c
                .pending
                .change()
                .map(|ch| ixs::describe_change(&ch))
                .unwrap_or_else(|_| format!("kind {}", c.pending.kind));
            ops.say(format!(
                "pending config change: {what}, applicable from {}",
                c.pending.not_before
            ));
        }
        Ok(_) => ops.say("no pending config change"),
        Err(e) => ops.say(format!("{e:#}")),
    }
    Ok(())
}

impl FromStr for Args {
    type Err = anyhow::Error;

    /// Parses a whitespace-separated command line (tests).
    fn from_str(s: &str) -> anyhow::Result<Self> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_positionals() {
        let a: Args = "stage set-pause global on --key k.json --ms emergency --force-buffer"
            .parse()
            .unwrap();
        assert_eq!(a.command, "stage");
        assert_eq!(a.positional, vec!["set-pause", "global", "on"]);
        assert_eq!(
            (a.key.as_deref(), a.ms.as_deref(), a.force_buffer),
            (Some("k.json"), Some("emergency"), true)
        );
        assert!("verify --bogus".parse::<Args>().is_err());
        assert!("approve --index x".parse::<Args>().is_err());
    }

    #[test]
    fn parses_an_extras_file() {
        let e = parse_extras(
            r#"
            [[entry]]
            kind = 2
            key = "11111111111111111111111111111111"
            seeds = [{ kind = 1, hex = "0707" }, { kind = 2, len = 1 }]
            [[entry]]
            kind = 1
            writable = true
            key = "SysvarRent111111111111111111111111111111111"
            "#,
        )
        .unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(
            (e[0].seeds[0].len, e[0].seeds[0].bytes[1], e[0].seeds[1].len),
            (2, 7, 1)
        );
        assert!(e[1].writable && e[1].seeds.is_empty());
        assert!(parse_extras("[[entry]]\nkind = 1\nkey = \"x\"").is_err());
    }
}
