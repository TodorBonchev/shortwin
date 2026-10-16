//! The protocol address lookup table (ARCHITECTURE §6.4, A28): its address list, and the
//! transactions that create, extend, freeze and check it (`shortwin-build-alt`,
//! `scripts/build-alt.sh`).
//!
//! **Contents** ([`protocol_entries`]), in this order, deduplicated:
//!
//! 1. **Static keys** ([`static_entries`]), exactly ARCH §6.4's list: the programs a short.win
//!    instruction names as accounts (pump, pAMM, pfee, AMM v4, CPMM, SPL Token, Token-2022,
//!    ATA, System) and short.win's event authority; the WSOL mint; pump `global`,
//!    `event_authority`, `fee_config` and global volume accumulator; the same four of PumpSwap;
//!    the AMM v4 and CPMM vault authorities; the 21 CPMM `AmmConfig`s (`["amm_config", index
//!    u16 BE]`, R-RAY §2.5); short.win's `config`, `venue_guard` and `oracle_cache`. ComputeBudget
//!    and every other top-level program id stay static keys (a v0 message cannot load a program
//!    id from a table), and so does short.win's own id.
//! 2. **Addition (flagged):** the four `VenueExtras` PDAs (`["venue_extras", venue]`, venues
//!    1, 3, 4, 5). They are static protocol accounts like `config`, and every trading
//!    instruction that selects an extras set names one; in the table they save 31 bytes there.
//!    T-SIZE (`tests/litesvm/tests/tx_size_raydium.rs`) proves the worst case fits without them
//!    too.
//! 3. **Live recipients** ([`recipient_entries`]): pump's 8 normal fee recipients
//!    (`Global.fee_recipient` @41 and `fee_recipients[7]` @162) and 8 buyback recipients
//!    (`buyback_fee_recipients[8]` @741), PumpSwap's 8 protocol fee recipients
//!    (`GlobalConfig.protocol_fee_recipients[8]` @57) and 8 buyback recipients
//!    (`buyback_fee_recipients[8]` @643), each with its WSOL ATA (SPL Token). The two programs
//!    share most of them (17 distinct on 2026-09-28). They are read from mainnet when the table
//!    is built, because the admins can change them; a transaction naming a recipient the table
//!    lacks only carries that key inline, so a stale table costs bytes, never correctness, and
//!    `extend` adds new ones.
//!
//! No sysvar appears: no short.win or MVP-venue instruction takes a sysvar account (clocks and
//! rent come from the `Sysvar::get` syscalls).
//!
//! **Lifecycle.** `create` derives the table from the authority and a finalized slot, creates it,
//! extends it in chunks of [`EXTEND_CHUNK`] and, with `--freeze`, freezes it (no authority can
//! then deactivate it — a deactivated table would break every v0 transaction naming it — but it
//! can no longer be extended either). Entries become usable one slot after the extension. The
//! keeper's `lookup_tables` names it; `check` verifies an existing table holds every expected
//! address.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use anyhow::{Context, anyhow, bail};
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_FEE_CONFIG,
    PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, PUMP_AMM_PROGRAM_ID,
    PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG, PUMP_FEES_PROGRAM_ID, PUMP_GLOBAL,
    PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, RAYDIUM_CPMM_AUTHORITY,
    RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use shortwin::state::{EXTRAS_IX_BUY, EXTRAS_IX_SELL, VenueKind};
use shortwin::venues::EXTRAS_SET_CURRENT;
use shortwin_services_common::tx::{self, ComputeBudget, TxFormat};
use shortwin_services_common::{AccountData, Chain, TxOutcome};
use solana_address_lookup_table_interface::{instruction as alt_ix, state::AddressLookupTable};
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;

use crate::pda;
use crate::venue;
use crate::view::MarketView;

/// Addresses one table can hold.
pub const MAX_ADDRESSES: usize =
    solana_address_lookup_table_interface::state::LOOKUP_TABLE_MAX_ADDRESSES;
/// Addresses per `extend_lookup_table` transaction (fits 1,232 B with a separate authority).
pub const EXTEND_CHUNK: usize = 24;
/// How many live CPMM `AmmConfig`s there are (indices 0–20, R-RAY §2.5).
pub const CPMM_CONFIGS: u16 = 21;

/// pump `Global` discriminator (ARCH §4.1).
pub const PUMP_GLOBAL_DISCRIMINATOR: [u8; 8] = [0xa7, 0xe8, 0xe8, 0xb1, 0xc8, 0x6c, 0x72, 0x7f];
/// PumpSwap `GlobalConfig` discriminator (ARCH §4.1).
pub const PAMM_GLOBAL_CONFIG_DISCRIMINATOR: [u8; 8] =
    [0x95, 0x08, 0x9c, 0xca, 0xa0, 0xfc, 0xb0, 0xd9];
/// pump `Global.fee_recipient` (the first normal recipient).
pub const OFF_PUMP_FEE_RECIPIENT: usize = 41;
/// pump `Global.fee_recipients[7]` (the other seven).
pub const OFF_PUMP_FEE_RECIPIENTS: usize = 162;
/// pump `Global.buyback_fee_recipients[8]`.
pub const OFF_PUMP_BUYBACK_RECIPIENTS: usize = 741;
/// PumpSwap `GlobalConfig.protocol_fee_recipients[8]`.
pub const OFF_PAMM_PROTOCOL_RECIPIENTS: usize = 57;
/// PumpSwap `GlobalConfig.buyback_fee_recipients[8]`.
pub const OFF_PAMM_BUYBACK_RECIPIENTS: usize = 643;

/// One table entry and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltEntry {
    pub key: Pubkey,
    pub label: String,
}

fn e(key: Pubkey, label: impl Into<String>) -> AltEntry {
    AltEntry {
        key,
        label: label.into(),
    }
}

/// CPMM `AmmConfig` `index`: `PDA(CPMM, ["amm_config", index u16 BE])` (R-RAY §2.5).
pub fn cpmm_config(index: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[b"amm_config", &index.to_be_bytes()],
        &RAYDIUM_CPMM_PROGRAM_ID,
    )
    .0
}

/// `ATA(owner, WSOL)` under SPL Token.
pub fn wsol_ata(owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            owner.as_ref(),
            TOKEN_PROGRAM_ID.as_ref(),
            WSOL_MINT.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// The static entries (module docs, items 1 and 2).
pub fn static_entries() -> Vec<AltEntry> {
    let mut v = vec![
        e(PUMP_PROGRAM_ID, "pump program"),
        e(PUMP_AMM_PROGRAM_ID, "PumpSwap program"),
        e(PUMP_FEES_PROGRAM_ID, "pump fees program"),
        e(RAYDIUM_V4_PROGRAM_ID, "Raydium AMM v4 program"),
        e(RAYDIUM_CPMM_PROGRAM_ID, "Raydium CPMM program"),
        e(TOKEN_PROGRAM_ID, "SPL Token"),
        e(TOKEN_2022_PROGRAM_ID, "Token-2022"),
        e(
            ASSOCIATED_TOKEN_PROGRAM_ID,
            "Associated Token Account program",
        ),
        e(anchor_lang::system_program::ID, "System program"),
        e(pda::event_authority(), "short.win event authority"),
        e(WSOL_MINT, "WSOL mint"),
        e(PUMP_GLOBAL, "pump global"),
        e(PUMP_EVENT_AUTHORITY, "pump event_authority"),
        e(PUMP_FEE_CONFIG, "pump fee_config (pfee)"),
        e(
            PUMP_GLOBAL_VOLUME_ACCUMULATOR,
            "pump global volume accumulator",
        ),
        e(PUMP_AMM_GLOBAL_CONFIG, "PumpSwap global_config"),
        e(PUMP_AMM_EVENT_AUTHORITY, "PumpSwap event_authority"),
        e(PUMP_AMM_FEE_CONFIG, "PumpSwap fee_config (pfee)"),
        e(
            PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
            "PumpSwap global volume accumulator",
        ),
        e(RAYDIUM_V4_AUTHORITY, "Raydium AMM v4 authority"),
        e(RAYDIUM_CPMM_AUTHORITY, "Raydium CPMM authority"),
    ];
    v.extend((0..CPMM_CONFIGS).map(|i| e(cpmm_config(i), format!("Raydium CPMM AmmConfig {i}"))));
    v.extend([
        e(pda::config(), "short.win config"),
        e(pda::venue_guard(), "short.win venue_guard"),
        e(pda::oracle_cache(), "short.win oracle_cache"),
    ]);
    // Addition (module docs, item 2).
    for kind in VenueKind::WITH_EXTRAS {
        if let Some(seed) = kind.extras_seed() {
            v.push(e(
                pda::venue_extras(seed),
                format!("short.win venue_extras {kind:?}"),
            ));
        }
    }
    v
}

fn keys_at(data: &[u8], offset: usize, n: usize) -> anyhow::Result<Vec<Pubkey>> {
    (0..n)
        .map(|i| {
            let at = offset + 32 * i;
            let b: [u8; 32] = data
                .get(at..at + 32)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| anyhow!("account too short for a key at {at}"))?;
            Ok(Pubkey::new_from_array(b))
        })
        .collect()
}

fn checked<'a>(
    acc: &'a AccountData,
    owner: &Pubkey,
    disc: &[u8; 8],
    what: &str,
) -> anyhow::Result<&'a [u8]> {
    if acc.owner != *owner {
        bail!("{what}: owner {} is not {owner}", acc.owner);
    }
    if !acc.data.starts_with(disc) {
        bail!("{what}: wrong discriminator");
    }
    Ok(&acc.data)
}

/// The live fee and buyback recipients of pump's `Global` and PumpSwap's `GlobalConfig`, each
/// followed by its WSOL ATA (module docs, item 3). Unset (default) slots are skipped.
pub fn recipient_entries(
    pump_global: &AccountData,
    pamm_global_config: &AccountData,
) -> anyhow::Result<Vec<AltEntry>> {
    let g = checked(
        pump_global,
        &PUMP_PROGRAM_ID,
        &PUMP_GLOBAL_DISCRIMINATOR,
        "pump Global",
    )?;
    let c = checked(
        pamm_global_config,
        &PUMP_AMM_PROGRAM_ID,
        &PAMM_GLOBAL_CONFIG_DISCRIMINATOR,
        "PumpSwap GlobalConfig",
    )?;
    let mut lists: Vec<(&str, Vec<Pubkey>)> = Vec::new();
    let mut pump_fee = keys_at(g, OFF_PUMP_FEE_RECIPIENT, 1)?;
    pump_fee.extend(keys_at(g, OFF_PUMP_FEE_RECIPIENTS, 7)?);
    lists.push(("pump fee recipient", pump_fee));
    lists.push((
        "pump buyback recipient",
        keys_at(g, OFF_PUMP_BUYBACK_RECIPIENTS, 8)?,
    ));
    lists.push((
        "PumpSwap protocol fee recipient",
        keys_at(c, OFF_PAMM_PROTOCOL_RECIPIENTS, 8)?,
    ));
    lists.push((
        "PumpSwap buyback recipient",
        keys_at(c, OFF_PAMM_BUYBACK_RECIPIENTS, 8)?,
    ));
    let mut out = Vec::new();
    for (what, keys) in lists {
        for (i, k) in keys.into_iter().enumerate() {
            if k == Pubkey::default() {
                continue;
            }
            out.push(e(k, format!("{what} {i}")));
            out.push(e(wsol_ata(&k), format!("{what} {i} WSOL ATA")));
        }
    }
    Ok(out)
}

/// `static` then `recipients`, each key once (the first label wins), at most
/// [`MAX_ADDRESSES`].
pub fn protocol_entries(
    statics: Vec<AltEntry>,
    recipients: Vec<AltEntry>,
) -> anyhow::Result<Vec<AltEntry>> {
    let mut out: Vec<AltEntry> = Vec::new();
    for entry in statics.into_iter().chain(recipients) {
        if !out.iter().any(|x| x.key == entry.key) {
            out.push(entry);
        }
    }
    if out.len() > MAX_ADDRESSES {
        bail!(
            "{} addresses exceed one lookup table ({MAX_ADDRESSES})",
            out.len()
        );
    }
    Ok(out)
}

/// Reads pump `Global` and PumpSwap `GlobalConfig` from `chain` and returns the whole list.
pub async fn live_entries<C: Chain>(chain: &C) -> anyhow::Result<Vec<AltEntry>> {
    let accs = chain
        .accounts(&[PUMP_GLOBAL, PUMP_AMM_GLOBAL_CONFIG])
        .await?;
    let g = accs.first().and_then(Option::as_ref).ok_or_else(|| {
        anyhow!("pump Global {PUMP_GLOBAL} not found (not mainnet? use --static-only)")
    })?;
    let c = accs.get(1).and_then(Option::as_ref).ok_or_else(|| {
        anyhow!("PumpSwap GlobalConfig {PUMP_AMM_GLOBAL_CONFIG} not found (use --static-only)")
    })?;
    protocol_entries(static_entries(), recipient_entries(g, c)?)
}

/// A market's own lookup table (ARCH §6.4's per-market fallback; M7 OWNER-M7-1): the per-market
/// keys that the market's trading transactions (`open_short`, `close_short`, `add_margin`,
/// `liquidate`, `expire`) and its cranks (`crank_price`, `sol_settle`) name and the protocol table
/// (`protocol`: its addresses) leaves inline — the market, its mint, pool vault, market vault and
/// trader, the venue's ReadSet, the trade extras (`trader_wsol`, the venue's per-market accounts:
/// on PumpSwap the trader's user-volume accumulator, cashback ATA and coin-creator vault), and the
/// keys the venue's published `current` extras sets resolve to for this market (buy and sell) —
/// deduplicated, in that order. Position- and wallet-level keys (the position, the exposure, the
/// borrower and its token accounts, the keeper) cannot be shared and stay inline; so do signers
/// and invoked program ids (a v0 message loads neither from a table).
///
/// The table is extendable (not frozen) so the keeper can add the keys of a new extras set or of a
/// pool switch; `shortwin-build-alt market` creates it. Rent: [`table_rent_lamports`].
pub fn market_entries(mv: &MarketView, protocol: &[Pubkey]) -> anyhow::Result<Vec<AltEntry>> {
    let m = &mv.market;
    let mut out: Vec<AltEntry> = Vec::new();
    let mut add = |key: Pubkey, label: String| {
        if key != Pubkey::default()
            && key != shortwin::ID
            && !protocol.contains(&key)
            && !out.iter().any(|x| x.key == key)
        {
            out.push(e(key, label));
        }
    };
    add(mv.key, "market".into());
    add(m.mint, "mint".into());
    add(m.pool_vault, "pool_vault".into());
    add(m.market_vault, "market_vault".into());
    add(m.trader, "trader".into());
    for (i, k) in mv.read_keys.iter().enumerate() {
        add(*k, format!("ReadSet {i}"));
    }
    for (i, meta) in venue::trade_extras(mv.kind, m, &mv.read_accounts)?
        .iter()
        .enumerate()
    {
        add(meta.pubkey, format!("trade extra {i}"));
    }
    for ix in [EXTRAS_IX_BUY, EXTRAS_IX_SELL] {
        let choices = venue::extras_candidates(mv.kind, ix, m, mv.extras.as_deref());
        if let Some(c) = choices.iter().find(|c| c.sel.set == EXTRAS_SET_CURRENT) {
            for (i, meta) in c.metas.iter().enumerate() {
                add(meta.pubkey, format!("extras (ix {ix}) {i}"));
            }
        }
    }
    if out.len() > MAX_ADDRESSES {
        bail!(
            "{}: {} per-market addresses exceed one table",
            mv.key,
            out.len()
        );
    }
    Ok(out)
}

/// Mainnet's rent-exempt price per byte (ARCH V5: the Rent sysvar's 5,080 lamports per byte-year
/// at threshold 1.0; `getMinimumBalanceForRentExemption`).
pub const MAINNET_LAMPORTS_PER_BYTE: u64 = 5_080;

/// Rent-exempt lamports of a lookup table holding `n` addresses on mainnet: its 56-byte header and
/// `32·n` bytes of addresses, plus the 128 bytes of account overhead, at
/// [`MAINNET_LAMPORTS_PER_BYTE`]. Recovered when the table is deactivated and closed.
pub fn table_rent_lamports(n: usize) -> u64 {
    (128 + 56 + 32 * n as u64) * MAINNET_LAMPORTS_PER_BYTE
}

/// The keys of `entries`.
pub fn keys(entries: &[AltEntry]) -> Vec<Pubkey> {
    entries.iter().map(|x| x.key).collect()
}

/// The table's derived address and its `create_lookup_table` instruction (the authority need
/// not sign; the payer funds the account).
pub fn create_ix(authority: &Pubkey, payer: &Pubkey, recent_slot: u64) -> (Instruction, Pubkey) {
    alt_ix::create_lookup_table(*authority, *payer, recent_slot)
}

/// `extend_lookup_table` instructions adding `addresses` in chunks of [`EXTEND_CHUNK`].
pub fn extend_ixs(
    table: &Pubkey,
    authority: &Pubkey,
    payer: &Pubkey,
    addresses: &[Pubkey],
) -> Vec<Instruction> {
    addresses
        .chunks(EXTEND_CHUNK)
        .map(|c| alt_ix::extend_lookup_table(*table, *authority, Some(*payer), c.to_vec()))
        .collect()
}

/// `freeze_lookup_table`: no authority afterwards (never deactivated, never extended).
pub fn freeze_ix(table: &Pubkey, authority: &Pubkey) -> Instruction {
    alt_ix::freeze_lookup_table(*table, *authority)
}

/// A table as read from the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableState {
    pub key: Pubkey,
    pub addresses: Vec<Pubkey>,
    /// `None`: frozen.
    pub authority: Option<Pubkey>,
    /// `u64::MAX` while active.
    pub deactivation_slot: u64,
}

impl TableState {
    /// Parses a lookup-table account (owner checked).
    pub fn parse(key: Pubkey, acc: &AccountData) -> anyhow::Result<Self> {
        let parsed = tx::parse_lookup_table(key, &acc.owner, &acc.data)?;
        let t = AddressLookupTable::deserialize(&acc.data)
            .map_err(|e| anyhow!("{key}: not a lookup table ({e:?})"))?;
        Ok(Self {
            key,
            addresses: parsed.addresses,
            authority: t.meta.authority,
            deactivation_slot: t.meta.deactivation_slot,
        })
    }

    /// The table for message compilation.
    pub fn account(&self) -> AddressLookupTableAccount {
        AddressLookupTableAccount {
            key: self.key,
            addresses: self.addresses.clone(),
        }
    }

    /// The expected addresses the table lacks, in `expected`'s order.
    pub fn missing(&self, expected: &[Pubkey]) -> Vec<Pubkey> {
        expected
            .iter()
            .filter(|k| !self.addresses.contains(k))
            .copied()
            .collect()
    }
}

/// Reads the table at `key`.
pub async fn read_table<C: Chain>(chain: &C, key: &Pubkey) -> anyhow::Result<TableState> {
    let acc = chain
        .accounts(std::slice::from_ref(key))
        .await?
        .into_iter()
        .next()
        .flatten()
        .ok_or_else(|| anyhow!("lookup table {key} not found"))?;
    TableState::parse(*key, &acc)
}

/// Simulates, then sends `ixs` as one v0 transaction (no tables) paid by `payer`, with the
/// compute-unit limit `⌈simulated × 1.1⌉` and `price` micro-lamports per CU; fails on any
/// error.
pub async fn send<C: Chain>(
    chain: &C,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    price: u64,
) -> anyhow::Result<TxOutcome> {
    let bh = chain.latest_blockhash().await?;
    let sim_budget = ComputeBudget {
        unit_price: price,
        ..ComputeBudget::SIMULATION
    };
    let t = tx::build(TxFormat::V0, payer, signers, ixs, &sim_budget, bh, &[])?;
    if !tx::fits(TxFormat::V0, &t) {
        bail!("transaction too large: {} B", tx::size(&t));
    }
    let sim = chain.simulate(&t).await?;
    if let Some(err) = &sim.err {
        bail!("simulation failed: {err:?}\n{}", sim.logs.join("\n"));
    }
    let budget = ComputeBudget {
        unit_limit: tx::cu_limit(sim.units_consumed),
        unit_price: price,
    };
    let t = tx::build(TxFormat::V0, payer, signers, ixs, &budget, bh, &[])?;
    let out = chain.send(&t).await?;
    if let Some(err) = &out.err {
        bail!("transaction failed: {err:?}\n{}", out.logs.join("\n"));
    }
    Ok(out)
}

/// Creates a table owned by `authority` at `recent_slot` (a slot in the cluster's
/// `SlotHashes`), extends it with `addresses` and optionally freezes it; returns its address.
pub async fn create<C: Chain>(
    chain: &C,
    payer: &Keypair,
    authority: &Keypair,
    recent_slot: u64,
    addresses: &[Pubkey],
    freeze: bool,
    price: u64,
) -> anyhow::Result<Pubkey> {
    use solana_signer::Signer;
    if addresses.len() > MAX_ADDRESSES {
        bail!("{} addresses exceed one table", addresses.len());
    }
    let (ix, table) = create_ix(&authority.pubkey(), &payer.pubkey(), recent_slot);
    send(chain, payer, &[authority], &[ix], price)
        .await
        .context("create_lookup_table")?;
    extend(chain, payer, authority, &table, addresses, price).await?;
    if freeze {
        send(
            chain,
            payer,
            &[authority],
            &[freeze_ix(&table, &authority.pubkey())],
            price,
        )
        .await
        .context("freeze_lookup_table")?;
    }
    Ok(table)
}

/// Adds the addresses of `addresses` that `table` lacks; returns how many were added.
pub async fn extend<C: Chain>(
    chain: &C,
    payer: &Keypair,
    authority: &Keypair,
    table: &Pubkey,
    addresses: &[Pubkey],
    price: u64,
) -> anyhow::Result<usize> {
    use solana_signer::Signer;
    let state = read_table(chain, table).await?;
    if state.authority != Some(authority.pubkey()) {
        bail!(
            "{table}: authority {:?} is not {} (frozen tables cannot be extended)",
            state.authority,
            authority.pubkey()
        );
    }
    let missing = state.missing(addresses);
    if state.addresses.len() + missing.len() > MAX_ADDRESSES {
        bail!(
            "{table} holds {} addresses; {} more exceed {MAX_ADDRESSES}",
            state.addresses.len(),
            missing.len()
        );
    }
    for (i, ix) in extend_ixs(table, &authority.pubkey(), &payer.pubkey(), &missing)
        .into_iter()
        .enumerate()
    {
        send(chain, payer, &[authority], &[ix], price)
            .await
            .with_context(|| format!("extend_lookup_table chunk {i}"))?;
    }
    Ok(missing.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_signer::Signer;

    fn acc(owner: Pubkey, data: &[u8]) -> AccountData {
        AccountData {
            lamports: 1,
            owner,
            executable: false,
            data: data.to_vec(),
        }
    }

    fn mainnet() -> (AccountData, AccountData) {
        (
            acc(
                PUMP_PROGRAM_ID,
                include_bytes!("../tests/data/pump_global.bin"),
            ),
            acc(
                PUMP_AMM_PROGRAM_ID,
                include_bytes!("../tests/data/pamm_global_config.bin"),
            ),
        )
    }

    #[test]
    fn static_entries_are_archs_list_plus_the_extras_pdas() {
        let s = static_entries();
        // 9 programs + event authority + WSOL + 4 pump + 4 pAMM + 2 authorities + 21 configs
        // + 3 short.win accounts + 4 VenueExtras PDAs.
        assert_eq!(s.len(), 9 + 1 + 1 + 4 + 4 + 2 + 21 + 3 + 4);
        let keys = keys(&s);
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "no duplicates");
        // Never a top-level-only program: ComputeBudget and short.win itself stay static.
        assert!(!keys.contains(&shortwin::ID));
        assert!(!keys.contains(&solana_pubkey::pubkey!(
            "ComputeBudget111111111111111111111111111111"
        )));
        // The CPMM configs re-derive to R-RAY §2.5's first and last.
        assert_eq!(
            cpmm_config(0).to_string()[..8],
            *"D4FPEruK",
            "config 0 is D4FPEruK…"
        );
        assert_eq!(cpmm_config(20).to_string()[..8], *"627Jx6eN");
    }

    /// The recipient lists decode from the mainnet dumps exactly as the research notes record
    /// them (pump §2.2; PumpSwap §2.2: `JCRGumoE…` in place of `CebN5WGQ…`).
    #[test]
    fn recipients_decode_from_the_mainnet_dumps() {
        let (g, c) = mainnet();
        let r = recipient_entries(&g, &c).unwrap();
        assert_eq!(r.len(), 4 * 8 * 2, "32 recipients, each with its WSOL ATA");
        let label = |k: &str| {
            let key: Pubkey = k.parse().unwrap();
            r.iter()
                .find(|x| x.key == key)
                .map(|x| x.label.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            label("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV"),
            "pump fee recipient 0"
        );
        assert_eq!(
            label("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM"),
            "pump fee recipient 5"
        );
        assert_eq!(
            label("5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD"),
            "pump buyback recipient 0"
        );
        assert_eq!(
            label("JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU"),
            "PumpSwap protocol fee recipient 7"
        );
        assert_eq!(
            label("A7hAgCzFw14fejgCp387JUJRMNyz4j89JKnhtKU8piqW"),
            "pump buyback recipient 7"
        );
        // The WSOL ATAs graduated-venues.md §2.2 records for a mainnet PumpSwap trade.
        assert_eq!(
            label("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb"),
            "pump fee recipient 0 WSOL ATA"
        );
        assert_eq!(
            label("HjQjngTDqoHE6aaGhUqfz9aQ7WZcBRjy5xB8PScLSr8i"),
            "pump buyback recipient 0 WSOL ATA"
        );
        let all = protocol_entries(static_entries(), r).unwrap();
        // 17 distinct recipients (the lists overlap) and their ATAs.
        assert_eq!(all.len(), static_entries().len() + 2 * 17);
        assert!(all.len() <= MAX_ADDRESSES);
    }

    #[test]
    fn foreign_or_short_accounts_are_refused() {
        let (g, c) = mainnet();
        assert!(recipient_entries(&c, &g).is_err(), "swapped accounts");
        let mut short = g.clone();
        short.data.truncate(800);
        assert!(recipient_entries(&short, &c).is_err());
        let mut foreign = g;
        foreign.owner = Pubkey::new_unique();
        assert!(recipient_entries(&foreign, &c).is_err());
    }

    #[test]
    fn instructions_address_the_derived_table_in_chunks() {
        let authority = Keypair::new();
        let payer = Keypair::new();
        let (ix, table) = create_ix(&authority.pubkey(), &payer.pubkey(), 451_000_000);
        let (derived, _) = Pubkey::find_program_address(
            &[authority.pubkey().as_ref(), &451_000_000u64.to_le_bytes()],
            &tx::ALT_PROGRAM_ID,
        );
        assert_eq!(table, derived);
        assert_eq!(ix.program_id, tx::ALT_PROGRAM_ID);
        assert_eq!(ix.accounts[0].pubkey, table);
        let addrs: Vec<Pubkey> = (0..60).map(|_| Pubkey::new_unique()).collect();
        let ext = extend_ixs(&table, &authority.pubkey(), &payer.pubkey(), &addrs);
        assert_eq!(ext.len(), 3);
        // Every chunk fits a v0 transaction with a separate authority signer.
        for ix in &ext {
            let t = tx::build(
                TxFormat::V0,
                &payer,
                &[&authority],
                std::slice::from_ref(ix),
                &ComputeBudget {
                    unit_limit: 50_000,
                    unit_price: 1_000,
                },
                solana_hash::Hash::default(),
                &[],
            )
            .unwrap();
            assert!(tx::fits(TxFormat::V0, &t), "{} B", tx::size(&t));
        }
    }

    #[test]
    fn table_state_parses_meta_and_finds_missing_keys() {
        let key = Pubkey::new_unique();
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let mut data = vec![0u8; tx::ALT_META_SIZE];
        data[0] = 1;
        data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        data[21] = 1; // Some(authority)
        data[22..54].copy_from_slice(authority.as_ref());
        data.extend_from_slice(a.as_ref());
        let t = TableState::parse(key, &acc(tx::ALT_PROGRAM_ID, &data)).unwrap();
        assert_eq!(t.addresses, vec![a]);
        assert_eq!(t.authority, Some(authority));
        assert_eq!(t.deactivation_slot, u64::MAX);
        assert_eq!(t.missing(&[a, b]), vec![b]);
        data[21] = 0; // frozen
        let t = TableState::parse(key, &acc(tx::ALT_PROGRAM_ID, &data)).unwrap();
        assert_eq!(t.authority, None);
    }

    /// The rent of a table on mainnet: 17 addresses (the PumpSwap per-market table of
    /// `tx_size_pumpswap_it`) cost 3,698,240 lamports (≈ 0.0037 SOL); the empty table 934,720;
    /// 162,560 per address.
    #[test]
    fn table_rent_is_the_rent_exempt_minimum() {
        assert_eq!(table_rent_lamports(0), 934_720);
        assert_eq!(table_rent_lamports(17), 3_698_240);
        assert_eq!(table_rent_lamports(1) - table_rent_lamports(0), 162_560);
    }
}
