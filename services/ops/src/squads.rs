//! The Squads v4 flows on a live multisig: staging (direct, or through a transaction buffer),
//! approving, executing what is stored, cancelling, and reading what a transaction runs.
//!
//! - **Stage** ([`stage`]): one transaction by the staging member — `vault_transaction_create`,
//!   `proposal_create` (Active) and the member's own `proposal_approve`. When that transaction
//!   would exceed 1,232 bytes (a message above about 700 bytes: the worst-case
//!   `set_venue_extras` is 1,243 bytes staged directly, T-SQ-3) or `--force-buffer` is given, the
//!   message goes through the member's **transaction buffer**: `transaction_buffer_create` with as
//!   much as fits, `transaction_buffer_extend`s, then `vault_transaction_create_from_buffer` (the
//!   hash and size checked; the buffer closes) with the proposal and the approval.
//!   - A buffer left by an interrupted or dropped run is **resumed** when it holds the start of
//!     the same message (its final hash and size are this message's): only the missing bytes
//!     are sent. A buffer holding anything else is refused; [`close_buffer`] closes it and
//!     returns its rent.
//!   - Before anything is sent, the multisig's newest transaction is read: when it holds the
//!     same message for the same vault and its proposal is still open (Draft, Active, Approved),
//!     staging is refused (`--allow-duplicate` overrides) — a rerun after a transaction that
//!     was reported dropped but landed late would otherwise stage the action twice.
//! - **Approve** ([`approve`]): prints what the transaction runs, then the member's approval;
//!   with `execute_now` (time lock 0: the Emergency) the execution joins the approval that
//!   reaches the threshold, so an Emergency action takes two transactions in all.
//! - **Execute** ([`execute`]): rebuilds the remaining accounts from the stored
//!   `VaultTransaction` (what is executed is what was approved, never a local copy), and demands
//!   the keys of the message's other signers (`--co-signer`: the current upgrade authority in a
//!   checked transfer).
//! - **Cancel** ([`cancel`]): `proposal_cancel_v2` of an `Approved` proposal (it needs
//!   `threshold` cancel votes; T-SQ-6 / T-SQ-7: an Approved vault transaction otherwise never
//!   expires).

use anchor_lang::solana_program::instruction::Instruction;
use anyhow::{Context, bail, ensure};
use sha2::{Digest, Sha256};
use shortwin_services_common::Chain;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use squads_v4_ix::state::{
    Multisig, Proposal, ProposalStatus, TransactionBuffer, VaultTransaction,
};
use squads_v4_ix::{self as sq, pda as squads_pda};

use crate::config::MsLayout;
use crate::ixs;
use crate::send::{Ops, TX_MAX, wire_size};

/// Reads and decodes a multisig.
pub async fn multisig<C: Chain>(chain: &C, ms: &MsLayout) -> anyhow::Result<Multisig> {
    let acc = chain
        .accounts(&[ms.key])
        .await?
        .remove(0)
        .with_context(|| {
            format!(
                "{} multisig {} does not exist (create-multisigs)",
                ms.name, ms.key
            )
        })?;
    ensure!(
        acc.owner == sq::ID,
        "{} multisig {} is not a Squads account",
        ms.name,
        ms.key
    );
    Multisig::decode(&acc.data)
        .with_context(|| format!("{} multisig {}: not a Multisig", ms.name, ms.key))
}

/// The proposal and the vault transaction of `index`, if they exist.
pub async fn transaction<C: Chain>(
    chain: &C,
    ms: &MsLayout,
    index: u64,
) -> anyhow::Result<(Option<Proposal>, Option<VaultTransaction>)> {
    let accs = chain
        .accounts(&[
            squads_pda::proposal(&ms.key, index),
            squads_pda::transaction(&ms.key, index),
        ])
        .await?;
    let proposal = accs[0].as_ref().and_then(|a| Proposal::decode(&a.data));
    let tx = accs[1]
        .as_ref()
        .and_then(|a| VaultTransaction::decode(&a.data));
    Ok((proposal, tx))
}

/// The instructions a stored vault transaction runs (no lookup tables).
pub fn executed(tx: &VaultTransaction) -> anyhow::Result<Vec<Instruction>> {
    let remaining = tx
        .execute_accounts()
        .context("the transaction loads keys from address lookup tables (not supported)")?;
    tx.executed_instructions(&remaining)
        .context("the stored transaction does not resolve")
}

/// A human summary of transaction `index` of `ms`: its status, approvals and what it runs.
pub async fn describe<C: Chain>(
    chain: &C,
    ms: &MsLayout,
    index: u64,
) -> anyhow::Result<Vec<String>> {
    let (proposal, tx) = transaction(chain, ms, index).await?;
    let mut out = Vec::new();
    match &proposal {
        Some(p) => out.push(format!(
            "{} #{index}: {:?}, approved by {} of {} needed ({})",
            ms.name,
            p.status,
            p.approved.len(),
            ms.threshold,
            p.approved
                .iter()
                .map(Pubkey::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        None => out.push(format!("{} #{index}: no proposal", ms.name)),
    }
    match &tx {
        Some(t) => {
            out.push(format!(
                "  runs as vault {} ({}), created by {}",
                t.vault_index,
                t.vault(),
                t.creator
            ));
            let config = live_config(chain).await;
            for (i, ix) in executed(t)?.iter().enumerate() {
                out.push(format!("  [{i}] {}", ixs::describe(ix)));
                if let (Some(change), Some(c)) = (ixs::proposed_change(ix), &config) {
                    for d in ixs::change_diff(&change, c) {
                        out.push(format!("      vs the live config: {d}"));
                    }
                }
            }
            let co = t.co_signers();
            if !co.is_empty() {
                out.push(format!(
                    "  execution co-signed by {}",
                    co.iter()
                        .map(Pubkey::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        None => out.push("  no vault transaction at this index (a config transaction?)".into()),
    }
    Ok(out)
}

/// The live `GlobalConfig`, if there is one.
async fn live_config<C: Chain>(chain: &C) -> Option<shortwin::state::GlobalConfig> {
    let acc = chain
        .accounts(&[ixs::pdas::config()])
        .await
        .ok()?
        .remove(0)?;
    crate::verify::decode_zc(&acc)
}

fn member_of(m: &Multisig, ms: &MsLayout, key: &Keypair) -> anyhow::Result<()> {
    ensure!(
        m.is_member(&key.pubkey()),
        "{} is not a member of {} ({})",
        key.pubkey(),
        ms.name,
        ms.key
    );
    Ok(())
}

/// How [`stage`] stages (module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct StageOpts {
    /// Through the transaction buffer even when one transaction would do (`--force-buffer`).
    pub force_buffer: bool,
    /// The member's buffer to use (`--buffer-index`).
    pub buffer_index: u8,
    /// Stage although the newest transaction holds the same message (`--allow-duplicate`).
    pub allow_duplicate: bool,
}

/// Refuses to stage `message` for `vault_index` when the newest transaction of `m` holds the
/// same message and its proposal is still open (module docs).
async fn refuse_duplicate<C: Chain>(
    chain: &C,
    ms: &MsLayout,
    m: &Multisig,
    vault_index: u8,
    message: &[u8],
) -> anyhow::Result<()> {
    if m.transaction_index == 0 {
        return Ok(());
    }
    let newest = m.transaction_index;
    let (proposal, tx) = transaction(chain, ms, newest).await?;
    let Some(tx) = tx else { return Ok(()) };
    if tx.vault_index != vault_index || sq::message::decode(message).as_ref() != Some(&tx.message) {
        return Ok(());
    }
    let open = match proposal.map(|p| p.status) {
        None => true,
        Some(
            ProposalStatus::Draft(_) | ProposalStatus::Active(_) | ProposalStatus::Approved(_),
        ) => true,
        Some(_) => false,
    };
    ensure!(
        !open,
        "{} #{newest} already holds this message and is still open (a rerun after a send \
         reported as dropped that landed later?): review it with `status --ms {} --index \
         {newest}` and approve it, or stage again with --allow-duplicate",
        ms.name,
        ms.name.to_lowercase()
    );
    Ok(())
}

/// Stages `ixs` for vault `vault_index` of `ms` by `member` (module docs). Returns the
/// transaction index.
pub async fn stage<C: Chain>(
    ops: &Ops<'_, C>,
    ms: &MsLayout,
    member: &Keypair,
    vault_index: u8,
    ixs: &[Instruction],
    opts: StageOpts,
) -> anyhow::Result<u64> {
    let StageOpts {
        force_buffer,
        buffer_index,
        allow_duplicate,
    } = opts;
    let m = multisig(ops.chain, ms).await?;
    member_of(&m, ms, member)?;
    let index = m.transaction_index + 1;
    let vault = ms.vault(vault_index);
    let message = sq::compile(&vault, ixs, &[])
        .map_err(|e| anyhow::anyhow!("compiling the vault message: {e:?}"))?;
    if !allow_duplicate {
        refuse_duplicate(ops.chain, ms, &m, vault_index, &message.bytes).await?;
    }
    let c = member.pubkey();
    ops.say(format!(
        "staging {} #{index} for vault {vault_index} ({vault}), message {} B:",
        ms.name,
        message.bytes.len()
    ));
    for (i, ix) in ixs.iter().enumerate() {
        ops.say(format!("  [{i}] {}", ixs::describe(ix)));
    }
    let proposal = [
        sq::ix::proposal_create(&ms.key, index, &c, &c, false),
        sq::ix::proposal_approve(&ms.key, index, &c),
    ];
    let mut direct = vec![sq::ix::vault_transaction_create(
        &ms.key,
        index,
        &c,
        &c,
        vault_index,
        0,
        &message.bytes,
        None,
    )];
    direct.extend(proposal.iter().cloned());
    let direct_size = wire_size(&c, &direct);
    if !force_buffer && direct_size <= TX_MAX {
        ops.send(&format!("stage {} #{index}", ms.name), member, &direct, &[])
            .await?;
        return Ok(index);
    }

    // The transaction-buffer path.
    ops.say(format!(
        "  through transaction buffer {buffer_index} (staged directly: {direct_size} B)"
    ));
    let bytes = &message.bytes;
    ensure!(
        bytes.len() <= sq::MAX_BUFFER_SIZE,
        "a {}-byte message is above Squads' buffer limit",
        bytes.len()
    );
    let hash: [u8; 32] = Sha256::digest(bytes).into();
    let final_size = u16::try_from(bytes.len()).context("message size")?;
    let buffer = squads_pda::transaction_buffer(&ms.key, &c, buffer_index);
    let existing = ops.chain.accounts(&[buffer]).await?.remove(0);
    let resume_at = match existing {
        None => None,
        Some(acc) => {
            let b = TransactionBuffer::decode(&acc.data).with_context(|| {
                format!("{c}'s transaction buffer {buffer_index} ({buffer}): not a buffer")
            })?;
            let same = b.final_buffer_hash == hash
                && b.final_buffer_size == final_size
                && b.vault_index == vault_index
                && bytes.starts_with(&b.buffer);
            ensure!(
                same,
                "{c}'s transaction buffer {buffer_index} ({buffer}) holds another message ({} of \
                 {} B written, final hash {}): close it (`close-buffer --ms {} --buffer-index \
                 {buffer_index}`) or pick another --buffer-index",
                b.buffer.len(),
                b.final_buffer_size,
                hex(&b.final_buffer_hash),
                ms.name.to_lowercase()
            );
            ops.say(format!(
                "  resuming transaction buffer {buffer_index}: {} of {} B already written (an \
                 earlier run's)",
                b.buffer.len(),
                bytes.len()
            ));
            Some(b.buffer.len())
        }
    };
    let create = |chunk: &[u8]| {
        sq::ix::transaction_buffer_create(
            &ms.key,
            &c,
            &c,
            buffer_index,
            vault_index,
            hash,
            final_size,
            chunk,
        )
    };
    let extend = |chunk: &[u8]| sq::ix::transaction_buffer_extend(&ms.key, &c, buffer_index, chunk);
    let mut at = match resume_at {
        Some(at) => at,
        None => {
            let at = fit(&c, &create, bytes);
            ops.send(
                &format!("buffer {} #{index} create ({at} B)", ms.name),
                member,
                &[create(&bytes[..at])],
                &[],
            )
            .await?;
            if ops.stop_dry_run() {
                return Ok(index);
            }
            at
        }
    };
    while at < bytes.len() {
        let n = fit(&c, &extend, &bytes[at..]);
        ensure!(n > 0, "a buffer extend carries no byte");
        ops.send(
            &format!("buffer {} #{index} extend ({n} B)", ms.name),
            member,
            &[extend(&bytes[at..at + n])],
            &[],
        )
        .await?;
        if ops.stop_dry_run() {
            return Ok(index);
        }
        at += n;
    }
    let mut tx = vec![sq::ix::vault_transaction_create_from_buffer(
        &ms.key,
        index,
        &c,
        &c,
        buffer_index,
        vault_index,
        0,
        None,
    )];
    tx.extend(proposal);
    ops.send(
        &format!("stage {} #{index} from buffer", ms.name),
        member,
        &tx,
        &[],
    )
    .await?;
    Ok(index)
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// `transaction_buffer_close` of `member`'s buffer `buffer_index` on `ms`: the rent goes back to
/// `member` (a buffer a failed or abandoned staging left open; module docs).
pub async fn close_buffer<C: Chain>(
    ops: &Ops<'_, C>,
    ms: &MsLayout,
    member: &Keypair,
    buffer_index: u8,
) -> anyhow::Result<()> {
    let c = member.pubkey();
    let buffer = squads_pda::transaction_buffer(&ms.key, &c, buffer_index);
    let acc = ops
        .chain
        .accounts(&[buffer])
        .await?
        .remove(0)
        .with_context(|| {
            format!(
                "{c} has no transaction buffer {buffer_index} on {}",
                ms.name
            )
        })?;
    let b = TransactionBuffer::decode(&acc.data)
        .with_context(|| format!("{buffer}: not a transaction buffer"))?;
    ops.say(format!(
        "{}: {c}'s transaction buffer {buffer_index} ({buffer}): vault {}, {} of {} B written, \
         final hash {}, {} lamports back to {c}",
        ms.name,
        b.vault_index,
        b.buffer.len(),
        b.final_buffer_size,
        hex(&b.final_buffer_hash),
        acc.lamports
    ));
    let ix = sq::ix::transaction_buffer_close(&ms.key, &c, buffer_index);
    ops.send(
        &format!("close buffer {} {buffer_index}", ms.name),
        member,
        &[ix],
        &[],
    )
    .await?;
    Ok(())
}

/// How many bytes of `rest` fit in one transaction of `build(chunk)` paid by `payer`.
fn fit(payer: &Pubkey, build: &dyn Fn(&[u8]) -> Instruction, rest: &[u8]) -> usize {
    let size = |n: usize| wire_size(payer, &[build(&rest[..n])]);
    let mut n = rest.len();
    // One byte of chunk is one byte of transaction (bar a length prefix's growth).
    n = n.saturating_sub(size(n).saturating_sub(TX_MAX));
    while n > 0 && size(n) > TX_MAX {
        n -= 1;
    }
    n
}

/// `member`'s approval of transaction `index` (module docs); with `execute_now`, the execution
/// in the same transaction (the approval must reach the threshold and the time lock be 0).
pub async fn approve<C: Chain>(
    ops: &Ops<'_, C>,
    ms: &MsLayout,
    member: &Keypair,
    index: u64,
    execute_now: bool,
    co_signers: &[Keypair],
) -> anyhow::Result<()> {
    let m = multisig(ops.chain, ms).await?;
    member_of(&m, ms, member)?;
    for line in describe(ops.chain, ms, index).await? {
        ops.say(line);
    }
    let mut ixs = vec![sq::ix::proposal_approve(&ms.key, index, &member.pubkey())];
    let mut label = format!("approve {} #{index} by {}", ms.name, member.pubkey());
    let refs: Vec<&Keypair> = co_signers.iter().collect();
    if execute_now {
        ensure!(
            m.time_lock == 0,
            "--execute: {} has a {} s time lock; execute after it",
            ms.name,
            m.time_lock
        );
        ixs.push(execute_ix(ops.chain, ms, &m, member, index, co_signers).await?);
        label.push_str(" + execute");
    }
    ops.send(&label, member, &ixs, &refs).await?;
    Ok(())
}

async fn execute_ix<C: Chain>(
    chain: &C,
    ms: &MsLayout,
    m: &Multisig,
    member: &Keypair,
    index: u64,
    co_signers: &[Keypair],
) -> anyhow::Result<Instruction> {
    member_of(m, ms, member)?;
    let (_, tx) = transaction(chain, ms, index).await?;
    let tx = tx.with_context(|| format!("{} #{index}: no vault transaction", ms.name))?;
    let remaining = tx
        .execute_accounts()
        .context("the transaction loads keys from address lookup tables (not supported)")?;
    let missing: Vec<String> = tx
        .co_signers()
        .iter()
        .filter(|k| !co_signers.iter().any(|s| s.pubkey() == **k))
        .map(Pubkey::to_string)
        .collect();
    if !missing.is_empty() {
        bail!(
            "{} #{index}: its execution needs the signature of {} (--co-signer <keypair file>)",
            ms.name,
            missing.join(", ")
        );
    }
    Ok(sq::ix::vault_transaction_execute_with(
        &ms.key,
        index,
        &member.pubkey(),
        remaining,
    ))
}

/// `vault_transaction_execute` of transaction `index` by `member` (module docs).
pub async fn execute<C: Chain>(
    ops: &Ops<'_, C>,
    ms: &MsLayout,
    member: &Keypair,
    index: u64,
    co_signers: &[Keypair],
) -> anyhow::Result<()> {
    let m = multisig(ops.chain, ms).await?;
    for line in describe(ops.chain, ms, index).await? {
        ops.say(line);
    }
    if let (Some(p), _) = transaction(ops.chain, ms, index).await?
        && let ProposalStatus::Approved(at) = p.status
    {
        let now = ops.chain.clock().await?.unix_timestamp;
        let ready = at + i64::from(m.time_lock);
        if now < ready {
            ops.say(format!(
                "  time lock: executable at {ready} (in {} s); Squads will refuse before",
                ready - now
            ));
        }
    }
    let ix = execute_ix(ops.chain, ms, &m, member, index, co_signers).await?;
    let refs: Vec<&Keypair> = co_signers.iter().collect();
    ops.send(
        &format!("execute {} #{index}", ms.name),
        member,
        &[ix],
        &refs,
    )
    .await?;
    Ok(())
}

/// `proposal_cancel_v2` of transaction `index` by `member`.
pub async fn cancel<C: Chain>(
    ops: &Ops<'_, C>,
    ms: &MsLayout,
    member: &Keypair,
    index: u64,
) -> anyhow::Result<()> {
    let m = multisig(ops.chain, ms).await?;
    member_of(&m, ms, member)?;
    for line in describe(ops.chain, ms, index).await? {
        ops.say(line);
    }
    let ix = sq::ix::proposal_cancel_v2(&ms.key, index, &member.pubkey());
    ops.send(
        &format!("cancel {} #{index} by {}", ms.name, member.pubkey()),
        member,
        &[ix],
        &[],
    )
    .await?;
    Ok(())
}
