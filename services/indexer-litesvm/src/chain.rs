//! Chain state from session checkpoints, and the session's events decoded independently of
//! the INGEST track's decoder (INDEXER.md §11.4). Owner: ANALYTICS + REPLAY.
//!
//! - [`ChainState`]: a checkpoint's accounts decoded with `shortwin::state` (`Market`,
//!   `MarketVault`, `ShortPosition`, `LenderPosition`, `ShareEpoch`, `GlobalConfig`,
//!   `VenueGuard`, `VenueExtras`, `OracleCache`, `WalletExposure`): the values the projections
//!   must equal.
//! - [`decode_session`]: every transaction of a session decoded from its `getTransaction` JSON
//!   alone — short.win invocations at any stack height (names from the IDL), `emit_cpi!` events
//!   (inner instructions of short.win carrying `EVENT_IX_TAG_LE`, attributed to the nearest
//!   short.win invocation below them), `emit!` log events attributed through the log's invocation
//!   stack (short.win's), and the System transfers inside short.win
//!   invocations (the `ShareEpoch` record donation of INDEXER.md §6.4). A second, deliberately
//!   independent implementation: the harness compares the indexer's rows with it.
//! - [`Model`]: the contract's projection rules (INDEXER.md §5.3, §6) applied to those events in
//!   chain order — market accounting, the SOL ledgers of §6.4, positions and their locked terms,
//!   lenders across share epochs — plus the fee legs of §8.2 and the settlements of §8.4
//!   (`analytics::formulas`). [`Model::check_against`] compares it with a checkpoint: the
//!   contract's own rules verified against the program, before any database is involved.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AccountDeserialize, AnchorDeserialize, Discriminator};
use anyhow::{Context, anyhow, bail};
use base64::Engine;
use shortwin::events as e;
use shortwin::state::{
    GlobalConfig, LenderPosition, MARKET_FLAG_MINT_UNSAFE, MARKET_FLAG_PAUSED_COVERAGE,
    MARKET_FLAG_PAUSED_OPENS, MARKET_FLAG_ROUTE_BROKEN, MARKET_FLAG_WIPE_LIVE, Market, MarketVault,
    OracleCache, ShareEpoch, ShortPosition, VenueExtras, VenueGuard, WalletExposure,
};
use shortwin_indexer::analytics::formulas;
use shortwin_indexer::ingest::session::{AccountsCheckpoint, Session, SessionTx};
use solana_transaction_status_client_types::UiInstruction;
use solana_transaction_status_client_types::option_serializer::OptionSerializer;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// The System program.
pub const SYSTEM_PROGRAM: Pubkey = anchor_lang::system_program::ID;

// ---- checkpoints -------------------------------------------------------------------------------

/// A checkpoint's short.win accounts, decoded.
#[derive(Clone, Default)]
pub struct ChainState {
    pub slot: u64,
    pub unix_timestamp: i64,
    pub config: Option<GlobalConfig>,
    pub guard: Option<VenueGuard>,
    /// By account key.
    pub extras: BTreeMap<Pubkey, VenueExtras>,
    pub oracle: Option<OracleCache>,
    pub markets: BTreeMap<Pubkey, Market>,
    /// By the vault's own key (`Market.market_vault`).
    pub vaults: BTreeMap<Pubkey, MarketVault>,
    pub positions: BTreeMap<Pubkey, ShortPosition>,
    /// By `(market, owner)`.
    pub lenders: BTreeMap<(Pubkey, Pubkey), LenderPosition>,
    /// By `(market, epoch)`.
    pub share_epochs: BTreeMap<(Pubkey, u32), ShareEpoch>,
    pub exposures: BTreeMap<Pubkey, WalletExposure>,
    /// Lamports of every decoded account (positions hold their vault SOL as extra lamports).
    pub lamports: BTreeMap<Pubkey, u64>,
    pub closed: BTreeSet<Pubkey>,
}

fn zc<T: anchor_lang::__private::bytemuck::Pod>(data: &[u8]) -> anyhow::Result<T> {
    let len = core::mem::size_of::<T>();
    let body = data
        .get(8..8 + len)
        .ok_or_else(|| anyhow!("account shorter than {} bytes", 8 + len))?;
    Ok(anchor_lang::__private::bytemuck::pod_read_unaligned(body))
}

fn borsh<T: AccountDeserialize>(data: &[u8]) -> anyhow::Result<T> {
    T::try_deserialize(&mut &data[..]).map_err(|e| anyhow!("{e:?}"))
}

impl ChainState {
    /// Decodes `c` (accounts not owned by short.win, or of no known type, are skipped).
    pub fn from_checkpoint(c: &AccountsCheckpoint) -> anyhow::Result<Self> {
        let mut s = Self {
            slot: c.slot,
            unix_timestamp: c.unix_timestamp,
            ..Self::default()
        };
        let program = shortwin::ID.to_string();
        for a in &c.accounts {
            if a.owner != program {
                continue;
            }
            let key: Pubkey = a.pubkey.parse().map_err(|e| anyhow!("{e:?}"))?;
            let data = B64.decode(&a.data).context("account data")?;
            let disc = data.get(..8).unwrap_or_default();
            let ctx = || format!("decoding {key}");
            s.lamports.insert(key, a.lamports);
            if disc == Market::DISCRIMINATOR {
                s.markets.insert(key, zc(&data).with_context(ctx)?);
            } else if disc == MarketVault::DISCRIMINATOR {
                s.vaults.insert(key, zc(&data).with_context(ctx)?);
            } else if disc == GlobalConfig::DISCRIMINATOR {
                s.config = Some(zc(&data).with_context(ctx)?);
            } else if disc == VenueGuard::DISCRIMINATOR {
                s.guard = Some(zc(&data).with_context(ctx)?);
            } else if disc == VenueExtras::DISCRIMINATOR {
                s.extras.insert(key, zc(&data).with_context(ctx)?);
            } else if disc == ShortPosition::DISCRIMINATOR {
                s.positions.insert(key, borsh(&data).with_context(ctx)?);
            } else if disc == LenderPosition::DISCRIMINATOR {
                let l: LenderPosition = borsh(&data).with_context(ctx)?;
                s.lenders.insert((l.market, l.owner), l);
            } else if disc == ShareEpoch::DISCRIMINATOR {
                let r: ShareEpoch = borsh(&data).with_context(ctx)?;
                s.share_epochs.insert((r.market, r.epoch), r);
            } else if disc == OracleCache::DISCRIMINATOR {
                s.oracle = Some(borsh(&data).with_context(ctx)?);
            } else if disc == WalletExposure::DISCRIMINATOR {
                s.exposures.insert(key, borsh(&data).with_context(ctx)?);
            }
        }
        for k in &c.closed {
            s.closed.insert(k.parse().map_err(|e| anyhow!("{e:?}"))?);
        }
        Ok(s)
    }

    /// The vault of `market`.
    pub fn vault_of(&self, market: &Pubkey) -> Option<&MarketVault> {
        self.markets
            .get(market)
            .and_then(|m| self.vaults.get(&m.market_vault))
    }

    /// The open positions of `market`.
    pub fn positions_of<'a>(
        &'a self,
        market: &'a Pubkey,
    ) -> impl Iterator<Item = (&'a Pubkey, &'a ShortPosition)> + 'a {
        self.positions
            .iter()
            .filter(move |(_, p)| p.market == *market)
    }
}

// ---- events ------------------------------------------------------------------------------------

/// A decoded short.win event.
#[allow(clippy::large_enum_variant)]
pub enum Ev {
    MarketCreated(e::MarketCreated),
    Deposit(e::Deposit),
    Withdraw(e::Withdraw),
    ClaimSol(e::ClaimSol),
    DeficitCovered(e::DeficitCovered),
    LenderSynced(e::LenderSynced),
    Open(e::Open),
    Close(e::Close),
    Liquidate(e::Liquidate),
    Expire(e::Expire),
    SolSettle(e::SolSettle),
    SettleArmed(e::SettleArmed),
    SettleDisarmed(e::SettleDisarmed),
    EpochRolled(e::EpochRolled),
    ProtocolFeesSwept(e::ProtocolFeesSwept),
    MarginAdded(e::MarginAdded),
    PriceCranked(e::PriceCranked),
    VenueStateChanged(e::VenueStateChanged),
    RouteChainChanged(e::RouteChainChanged),
    PoolProposed(e::PoolProposed),
    PoolSwitched(e::PoolSwitched),
    PoolSwitchCancelled(e::PoolSwitchCancelled),
    TraderSwept(e::TraderSwept),
    VenueRewardsClaimed(e::VenueRewardsClaimed),
    MintSafetyTripped(e::MintSafetyTripped),
    VenueUpgradeFlagged(e::VenueUpgradeFlagged),
    VenueRevalidated(e::VenueRevalidated),
    CoverageGap(e::CoverageGap),
    CoveragePauseSet(e::CoveragePauseSet),
    ConfigProposed(e::ConfigProposed),
    ConfigApplied(e::ConfigApplied),
    ConfigCancelled(e::ConfigCancelled),
    PauseSet(e::PauseSet),
    VenueDisabled(e::VenueDisabled),
    RouteBrokenSet(e::RouteBrokenSet),
    VenueExtrasSet(e::VenueExtrasSet),
}

macro_rules! decode_one {
    ($payload:expr, $( $variant:ident => $ty:path ),* $(,)?) => {{
        $(
            if let Some(body) = $payload.strip_prefix(<$ty as Discriminator>::DISCRIMINATOR) {
                return Some(
                    <$ty as AnchorDeserialize>::try_from_slice(body)
                        .map(Ev::$variant)
                        .map_err(|err| anyhow!("{}: {err}", stringify!($variant))),
                );
            }
        )*
        None
    }};
}

macro_rules! ev_names {
    ($( $variant:ident ),* $(,)?) => {
        /// The event's type name (`events.kind`).
        pub fn name(&self) -> &'static str {
            match self {
                $( Self::$variant(_) => stringify!($variant), )*
            }
        }
    };
}

impl Ev {
    ev_names!(
        MarketCreated,
        Deposit,
        Withdraw,
        ClaimSol,
        DeficitCovered,
        LenderSynced,
        Open,
        Close,
        Liquidate,
        Expire,
        SolSettle,
        SettleArmed,
        SettleDisarmed,
        EpochRolled,
        ProtocolFeesSwept,
        MarginAdded,
        PriceCranked,
        VenueStateChanged,
        RouteChainChanged,
        PoolProposed,
        PoolSwitched,
        PoolSwitchCancelled,
        TraderSwept,
        VenueRewardsClaimed,
        MintSafetyTripped,
        VenueUpgradeFlagged,
        VenueRevalidated,
        CoverageGap,
        CoveragePauseSet,
        ConfigProposed,
        ConfigApplied,
        ConfigCancelled,
        PauseSet,
        VenueDisabled,
        RouteBrokenSet,
        VenueExtrasSet,
    );

    /// Decodes a short.win event payload (`discriminator ‖ Borsh`): `None` for an unknown
    /// discriminator, `Some(Err)` for a known one whose body does not parse.
    pub fn decode_shortwin(payload: &[u8]) -> Option<anyhow::Result<Self>> {
        decode_one!(payload,
            MarketCreated => e::MarketCreated, Deposit => e::Deposit, Withdraw => e::Withdraw,
            ClaimSol => e::ClaimSol, DeficitCovered => e::DeficitCovered,
            LenderSynced => e::LenderSynced, Open => e::Open, Close => e::Close,
            Liquidate => e::Liquidate, Expire => e::Expire, SolSettle => e::SolSettle,
            SettleArmed => e::SettleArmed, SettleDisarmed => e::SettleDisarmed,
            EpochRolled => e::EpochRolled, ProtocolFeesSwept => e::ProtocolFeesSwept,
            MarginAdded => e::MarginAdded, PriceCranked => e::PriceCranked,
            VenueStateChanged => e::VenueStateChanged, RouteChainChanged => e::RouteChainChanged,
            PoolProposed => e::PoolProposed, PoolSwitched => e::PoolSwitched,
            PoolSwitchCancelled => e::PoolSwitchCancelled, TraderSwept => e::TraderSwept,
            VenueRewardsClaimed => e::VenueRewardsClaimed,
            MintSafetyTripped => e::MintSafetyTripped,
            VenueUpgradeFlagged => e::VenueUpgradeFlagged,
            VenueRevalidated => e::VenueRevalidated, CoverageGap => e::CoverageGap,
            CoveragePauseSet => e::CoveragePauseSet, ConfigProposed => e::ConfigProposed,
            ConfigApplied => e::ConfigApplied, ConfigCancelled => e::ConfigCancelled,
            PauseSet => e::PauseSet, VenueDisabled => e::VenueDisabled,
            RouteBrokenSet => e::RouteBrokenSet, VenueExtrasSet => e::VenueExtrasSet,
        )
    }

    /// The event's `market` field (`None` for `Pubkey::default()`).
    pub fn market(&self) -> Option<Pubkey> {
        macro_rules! m {
            ($($v:ident),*) => {
                match self {
                    $( Self::$v(x) => x.market, )*
                }
            };
        }
        let k = m!(
            MarketCreated,
            Deposit,
            Withdraw,
            ClaimSol,
            DeficitCovered,
            LenderSynced,
            Open,
            Close,
            Liquidate,
            Expire,
            SolSettle,
            SettleArmed,
            SettleDisarmed,
            EpochRolled,
            ProtocolFeesSwept,
            MarginAdded,
            PriceCranked,
            VenueStateChanged,
            RouteChainChanged,
            PoolProposed,
            PoolSwitched,
            PoolSwitchCancelled,
            TraderSwept,
            VenueRewardsClaimed,
            MintSafetyTripped,
            VenueUpgradeFlagged,
            VenueRevalidated,
            CoverageGap,
            CoveragePauseSet,
            ConfigProposed,
            ConfigApplied,
            ConfigCancelled,
            PauseSet,
            VenueDisabled,
            RouteBrokenSet,
            VenueExtrasSet
        );
        (k != Pubkey::default()).then_some(k)
    }

    /// The event's own `ts` (the program clock).
    pub fn ts(&self) -> Option<i64> {
        macro_rules! t {
            ($($v:ident),*) => {
                match self {
                    $( Self::$v(x) => Some(x.ts), )*
                }
            };
        }
        t!(
            MarketCreated,
            Deposit,
            Withdraw,
            ClaimSol,
            DeficitCovered,
            LenderSynced,
            Open,
            Close,
            Liquidate,
            Expire,
            SolSettle,
            SettleArmed,
            SettleDisarmed,
            EpochRolled,
            ProtocolFeesSwept,
            MarginAdded,
            PriceCranked,
            VenueStateChanged,
            RouteChainChanged,
            PoolProposed,
            PoolSwitched,
            PoolSwitchCancelled,
            TraderSwept,
            VenueRewardsClaimed,
            MintSafetyTripped,
            VenueUpgradeFlagged,
            VenueRevalidated,
            CoverageGap,
            CoveragePauseSet,
            ConfigProposed,
            ConfigApplied,
            ConfigCancelled,
            PauseSet,
            VenueDisabled,
            RouteBrokenSet,
            VenueExtrasSet
        )
    }
}

/// A short.win invocation (top-level or CPI).
#[derive(Clone, Debug)]
pub struct Invocation {
    pub ix_index: u16,
    /// −1 for the top-level instruction.
    pub inner_index: i32,
    pub stack_height: u32,
    /// The IDL's snake_case name, `"unknown"` for an unknown discriminator.
    pub name: String,
    /// The account the IDL names `market` (unless it is the program id: an absent optional).
    pub market: Option<Pubkey>,
    pub accounts: Vec<Pubkey>,
    pub data: Vec<u8>,
}

/// A decoded event and where it was found (INDEXER.md §3.1 row key).
pub struct EvRec {
    pub ix_index: u16,
    /// 0: `emit_cpi!` inner instruction; 1: a log line.
    pub ev_source: u8,
    /// Index in the top-level instruction's inner list (0) or in the logs (1).
    pub ev_seq: u32,
    pub program: Pubkey,
    /// The emitting invocation's name.
    pub invocation: String,
    pub ev: Ev,
}

/// A System transfer inside a short.win invocation.
#[derive(Clone, Debug)]
pub struct Transfer {
    pub ix_index: u16,
    pub inner_index: i32,
    pub from: Pubkey,
    pub to: Pubkey,
    pub lamports: u64,
    /// The short.win invocation it ran under, with its accounts.
    pub under: String,
    pub under_accounts: Vec<Pubkey>,
}

/// One session transaction, decoded.
pub struct ChainTx {
    /// Index in `session.txs`.
    pub index: usize,
    pub signature: String,
    pub slot: u64,
    pub tx_index: u32,
    pub block_time: i64,
    pub fee_payer: Pubkey,
    pub fee: u64,
    pub compute_units: Option<u64>,
    /// `None` on success; the RPC error JSON otherwise.
    pub err: Option<serde_json::Value>,
    pub ok: bool,
    pub logs_truncated: bool,
    pub invocations: Vec<Invocation>,
    pub events: Vec<EvRec>,
    pub transfers: Vec<Transfer>,
    /// Payloads that carried the event tag (or sat on a short.win log line) but did not decode.
    pub undecoded: Vec<String>,
}

/// short.win's instructions by discriminator, from the IDL (`clients/js/idl/shortwin.json`):
/// name and the index of the account named `market`.
#[derive(Clone, Debug)]
pub struct InstructionCatalog {
    by_disc: HashMap<[u8; 8], (String, Option<usize>)>,
}

impl InstructionCatalog {
    /// Reads the repository's IDL.
    pub fn load() -> anyhow::Result<Self> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../clients/js/idl/shortwin.json"
        );
        let idl: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).context("the IDL")?)?;
        let mut by_disc = HashMap::new();
        for ix in idl["instructions"].as_array().context("instructions")? {
            let name = ix["name"].as_str().context("name")?.to_string();
            let disc: Vec<u8> = ix["discriminator"]
                .as_array()
                .context("discriminator")?
                .iter()
                .filter_map(|b| b.as_u64().and_then(|v| u8::try_from(v).ok()))
                .collect();
            let disc: [u8; 8] = disc
                .try_into()
                .map_err(|_| anyhow!("{name}: discriminator"))?;
            let market = ix["accounts"]
                .as_array()
                .context("accounts")?
                .iter()
                .position(|a| a["name"] == "market");
            by_disc.insert(disc, (name, market));
        }
        Ok(Self { by_disc })
    }

    /// `(name, market account index)` of instruction data `data`.
    pub fn lookup(&self, data: &[u8]) -> Option<&(String, Option<usize>)> {
        let disc: [u8; 8] = data.get(..8)?.try_into().ok()?;
        self.by_disc.get(&disc)
    }
}

/// The account keys of a session transaction (static, then loaded writable, then read-only)
/// and the decoded transaction.
fn keys_of(
    t: &SessionTx,
) -> anyhow::Result<(
    solana_transaction::versioned::VersionedTransaction,
    Vec<Pubkey>,
)> {
    let tx = t
        .transaction
        .transaction
        .transaction
        .decode()
        .ok_or_else(|| anyhow!("slot {} #{}: undecodable transaction", t.slot, t.tx_index))?;
    let mut keys = tx.message.static_account_keys().to_vec();
    if let Some(meta) = &t.transaction.transaction.meta
        && let OptionSerializer::Some(l) = &meta.loaded_addresses
    {
        for k in l.writable.iter().chain(l.readonly.iter()) {
            keys.push(k.parse().map_err(|e| anyhow!("{e:?}"))?);
        }
    }
    Ok((tx, keys))
}

/// An inner instruction: program, accounts, data, stack height.
type InnerIx = (Pubkey, Vec<Pubkey>, Vec<u8>, u32);

/// Decodes one session transaction (module docs).
pub fn decode_tx(cat: &InstructionCatalog, index: usize, t: &SessionTx) -> anyhow::Result<ChainTx> {
    let (tx, keys) = keys_of(t)?;
    let meta = t
        .transaction
        .transaction
        .meta
        .as_ref()
        .ok_or_else(|| anyhow!("no meta"))?;
    let ok = meta.err.is_none();
    let logs: Vec<String> = match &meta.log_messages {
        OptionSerializer::Some(l) => l.clone(),
        _ => Vec::new(),
    };
    let key = |i: usize| keys.get(i).copied().unwrap_or_default();
    let mut out = ChainTx {
        index,
        signature: tx
            .signatures
            .first()
            .map(ToString::to_string)
            .unwrap_or_default(),
        slot: t.slot,
        tx_index: t.tx_index,
        block_time: t.transaction.block_time.unwrap_or_default(),
        fee_payer: key(0),
        fee: meta.fee,
        compute_units: match meta.compute_units_consumed {
            OptionSerializer::Some(u) => Some(u),
            _ => None,
        },
        err: meta.err.as_ref().and_then(|e| serde_json::to_value(e).ok()),
        ok,
        logs_truncated: logs.iter().any(|l| l == "Log truncated"),
        invocations: Vec::new(),
        events: Vec::new(),
        transfers: Vec::new(),
        undecoded: Vec::new(),
    };
    // Inner instructions by top-level index.
    let mut inner: BTreeMap<u16, Vec<InnerIx>> = BTreeMap::new();
    if let OptionSerializer::Some(list) = &meta.inner_instructions {
        for group in list {
            let v = inner.entry(u16::from(group.index)).or_default();
            for ui in &group.instructions {
                let UiInstruction::Compiled(c) = ui else {
                    bail!("a parsed inner instruction (the recorder writes compiled ones)");
                };
                let data = bs58::decode(&c.data).into_vec().context("inner data")?;
                v.push((
                    key(usize::from(c.program_id_index)),
                    c.accounts.iter().map(|a| key(usize::from(*a))).collect(),
                    data,
                    c.stack_height.unwrap_or(2),
                ));
            }
        }
    }
    let invocation = |program: Pubkey, accounts: &[Pubkey], data: &[u8]| {
        (program == shortwin::ID).then(|| match cat.lookup(data) {
            Some((name, m)) => (
                name.clone(),
                m.and_then(|i| accounts.get(i).copied())
                    .filter(|k| *k != shortwin::ID),
            ),
            None => ("unknown".to_string(), None),
        })
    };
    for (i, top) in tx.message.instructions().iter().enumerate() {
        let ix_index = u16::try_from(i)?;
        let program = key(usize::from(top.program_id_index));
        let accounts: Vec<Pubkey> = top.accounts.iter().map(|a| key(usize::from(*a))).collect();
        // The short.win frames by stack height, most recent last: (height, name, accounts).
        let mut frames: Vec<(u32, String, Vec<Pubkey>)> = Vec::new();
        if let Some((name, market)) = invocation(program, &accounts, &top.data) {
            out.invocations.push(Invocation {
                ix_index,
                inner_index: -1,
                stack_height: 1,
                name: name.clone(),
                market,
                accounts: accounts.clone(),
                data: top.data.clone(),
            });
            frames.push((1, name, accounts.clone()));
        }
        for (j, (prog, accs, data, height)) in
            inner.get(&ix_index).into_iter().flatten().enumerate()
        {
            let inner_index = i32::try_from(j)?;
            frames.retain(|(h, _, _)| *h < *height);
            if *prog == shortwin::ID {
                if let Some(payload) = data.strip_prefix(anchor_lang::event::EVENT_IX_TAG_LE) {
                    if !ok {
                        continue;
                    }
                    let under = frames
                        .iter()
                        .rev()
                        .find(|(h, _, _)| *h + 1 == *height)
                        .map_or_else(|| "unknown".to_string(), |(_, n, _)| n.clone());
                    match Ev::decode_shortwin(payload) {
                        Some(Ok(ev)) => out.events.push(EvRec {
                            ix_index,
                            ev_source: 0,
                            ev_seq: u32::try_from(j)?,
                            program: shortwin::ID,
                            invocation: under,
                            ev,
                        }),
                        Some(Err(err)) => out.undecoded.push(format!("ix {i} inner {j}: {err}")),
                        None => out
                            .undecoded
                            .push(format!("ix {i} inner {j}: unknown event discriminator")),
                    }
                    continue;
                }
                if let Some((name, market)) = invocation(*prog, accs, data) {
                    out.invocations.push(Invocation {
                        ix_index,
                        inner_index,
                        stack_height: *height,
                        name: name.clone(),
                        market,
                        accounts: accs.clone(),
                        data: data.clone(),
                    });
                    frames.push((*height, name, accs.clone()));
                }
            } else if *prog == SYSTEM_PROGRAM
                && ok
                && data.len() == 12
                && data[..4] == [2, 0, 0, 0]
                && let Some((_, under, under_accounts)) =
                    frames.iter().rev().find(|(h, _, _)| *h < *height)
            {
                out.transfers.push(Transfer {
                    ix_index,
                    inner_index,
                    from: accs.first().copied().unwrap_or_default(),
                    to: accs.get(1).copied().unwrap_or_default(),
                    lamports: u64::from_le_bytes(data[4..12].try_into()?),
                    under: under.clone(),
                    under_accounts: under_accounts.clone(),
                });
            }
        }
    }
    // Log events through the invocation stack (INDEXER.md §5.2 step 3).
    if ok {
        let mut stack: Vec<Pubkey> = Vec::new();
        let mut top_ix: i32 = -1;
        for (n, line) in logs.iter().enumerate() {
            if line == "Log truncated" {
                break;
            }
            if let Some(rest) = line.strip_prefix("Program ") {
                let mut parts = rest.split_whitespace();
                let first = parts.next().unwrap_or_default();
                let second = parts.next().unwrap_or_default();
                if second == "invoke" {
                    let depth = rest
                        .rsplit('[')
                        .next()
                        .and_then(|d| d.trim_end_matches(']').parse::<usize>().ok())
                        .unwrap_or(0);
                    if depth == 1 {
                        top_ix += 1;
                        stack.clear();
                    }
                    let program: Pubkey = first.parse().unwrap_or_default();
                    stack.push(program);
                    continue;
                }
                if second == "success" || second == "failed:" || second.starts_with("failed") {
                    stack.pop();
                    continue;
                }
                if first == "data:" {
                    let Some(top) = stack.last().copied() else {
                        continue;
                    };
                    let Ok(payload) = B64.decode(second) else {
                        continue;
                    };
                    let ix_index = u16::try_from(top_ix.max(0))?;
                    let decoded = if top == shortwin::ID {
                        Ev::decode_shortwin(&payload)
                    } else {
                        None
                    };
                    let invocation_name = out
                        .invocations
                        .iter()
                        .rev()
                        .find(|v| v.ix_index == ix_index)
                        .map_or_else(|| "unknown".to_string(), |v| v.name.clone());
                    match decoded {
                        Some(Ok(ev)) => out.events.push(EvRec {
                            ix_index,
                            ev_source: 1,
                            ev_seq: u32::try_from(n)?,
                            program: top,
                            invocation: invocation_name,
                            ev,
                        }),
                        Some(Err(err)) => out.undecoded.push(format!("log line {n}: {err}")),
                        None if top == shortwin::ID => out
                            .undecoded
                            .push(format!("log line {n}: unknown short.win log payload")),
                        None => {}
                    }
                }
            }
        }
    }
    // Chain order within the transaction: (ix_index, ev_source, ev_seq).
    out.events
        .sort_by_key(|r| (r.ix_index, r.ev_source, r.ev_seq));
    Ok(out)
}

/// Every transaction of `session`, decoded (module docs).
pub fn decode_session(session: &Session) -> anyhow::Result<Vec<ChainTx>> {
    let cat = InstructionCatalog::load()?;
    session
        .txs
        .iter()
        .enumerate()
        .map(|(i, t)| decode_tx(&cat, i, t).with_context(|| format!("session tx {i}")))
        .collect()
}

// ---- the reference model ----------------------------------------------------------------------

/// Locked terms of a position the analytics need (INDEXER.md §6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terms {
    pub rent_protocol_bps: u16,
    /// Lender / protocol / keeper.
    pub penalty_split: [u16; 3],
    pub expiry_fee_bps: u16,
}

impl Terms {
    /// The terms the global parameters `p` lock at an open (INDEXER.md §6.5).
    pub fn from_params(p: &shortwin::state::Params) -> Self {
        let b = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
        Self {
            rent_protocol_bps: b(p.rent_protocol_bps),
            penalty_split: [
                b(p.penalty_lender_bps),
                b(p.penalty_protocol_bps),
                b(p.penalty_keeper_bps),
            ],
            expiry_fee_bps: b(p.expiry_fee_bps),
        }
    }

    /// From the position account.
    pub fn of(p: &ShortPosition) -> Self {
        Self {
            rent_protocol_bps: p.rent_protocol_bps,
            penalty_split: p.penalty_split,
            expiry_fee_bps: p.expiry_fee_bps,
        }
    }
}

/// The chain-order key of an event row (INDEXER.md §3.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub slot: u64,
    pub tx_index: u32,
    pub ix_index: u16,
    pub ev_source: u8,
    pub ev_seq: u32,
    pub signature: String,
}

impl Key {
    fn of(t: &ChainTx, r: &EvRec) -> Self {
        Self {
            slot: t.slot,
            tx_index: t.tx_index,
            ix_index: r.ix_index,
            ev_source: r.ev_source,
            ev_seq: r.ev_seq,
            signature: t.signature.clone(),
        }
    }
}

/// A market as the contract's rules project it (INDEXER.md §6.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarketModel {
    pub market: Pubkey,
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub creator: Pubkey,
    pub bonding_curve: Option<Pubkey>,
    pub canonical_pool: Option<Pubkey>,
    pub created_slot: u64,
    pub created_ts: i64,
    pub created_signature: String,
    pub creation_fee: u64,
    pub venue_at_listing: u8,
    pub tier_at_listing: u8,
    pub venue: u8,
    pub tier: u8,
    pub ray_pool: Option<Pubkey>,
    pub pool_d: u128,
    pub fp_candidate: Option<Pubkey>,
    pub fp_venue: Option<u8>,
    pub tokens_free: u64,
    pub borrowed_q: u64,
    pub borrowed_q_by_tier: [u64; 4],
    pub total_shares: u128,
    pub share_epoch: u32,
    pub open_positions: u32,
    pub total_notional_open: u64,
    pub max_q_hwm: u64,
    pub open_vault: u64,
    /// Signed so that a missing inflow shows as a negative ledger rather than a panic.
    pub lender_sol: i128,
    pub protocol_fees: i128,
    pub paused_opens: bool,
    pub paused_coverage: bool,
    pub coverage_cleared_at: i64,
    pub mint_unsafe: bool,
    pub route_broken: bool,
    pub wipe_live: bool,
    /// The last `P_ema` seen (PriceCranked.ema_price, Open.ema_wad) and its time.
    pub ema_price: Option<u128>,
    pub ema_ts: Option<i64>,
    pub spot_wad: Option<u128>,
    /// `vest_bal` ([E\*]): the last `PriceCranked` snapshot, and 0 after every reported flush
    /// (AIR-07): `SolSettle.vest_flushed > 0`, a full wipe's `EpochRolled` (a flushing rebase's
    /// leaves the record donation it vested anew), a lender event with `s_after = 0`.
    pub vest_bal: Option<u64>,
    /// The event whose rule last set `vest_bal` (each rule gives the program's vest as of its
    /// event): `vest_bal` is exact at a checkpoint taken right after it ([`Model::check_against`]).
    pub vest_set: Option<Key>,
    /// The watermark: the key of the last event applied.
    pub last: Key,
}

impl MarketModel {
    /// `vest_bal = vest` by the rule of the event at `key`.
    fn set_vest(&mut self, vest: u64, key: &Key) {
        self.vest_bal = Some(vest);
        self.vest_set = Some(key.clone());
    }
}

/// A position as projected (INDEXER.md §6.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionModel {
    pub position: Pubkey,
    pub market: Pubkey,
    pub borrower: Pubkey,
    pub nonce: u64,
    pub tier: u8,
    pub venue: u8,
    pub q: u64,
    pub s: u64,
    pub m: u64,
    pub v_open: u64,
    pub v: u64,
    pub margin_added: u64,
    pub n_open: u64,
    pub fee_open: u64,
    pub venue_rent: u64,
    pub entry_price: u128,
    pub spot_wad_open: u128,
    pub ema_wad_open: u128,
    pub b_spot_open: u64,
    pub b_ema_open: u64,
    pub deadline: i64,
    pub opened_ts: i64,
    pub opened_slot: u64,
    pub opened_tx_index: u32,
    pub open_signature: String,
    pub armed: bool,
    pub s_count: u64,
    pub s_since: i64,
    pub arm_count: u32,
    pub disarm_count: u32,
    pub terms: Option<Terms>,
    /// `open`, `closed`, `liquidated`, `expired`, `settled`.
    pub status: &'static str,
    pub exit_slot: Option<u64>,
    pub exit_ts: Option<i64>,
    pub exit_signature: Option<String>,
    pub exit_keeper: Option<Pubkey>,
    pub buyback_cost: Option<u64>,
    pub b_live_exit: Option<u64>,
    pub rent_paid: Option<u64>,
    pub rent_protocol: Option<u64>,
    pub penalty: Option<u64>,
    pub expiry_fee: Option<u64>,
    pub keeper_paid: Option<u64>,
    pub borrower_payout: Option<u64>,
    pub token_surplus: Option<u64>,
    pub liquidated_expired: Option<bool>,
    pub settle_condition: Option<u8>,
}

impl PositionModel {
    /// `borrower_payout − (m + margin_added) − fee_open` once exited (§6.5).
    pub fn borrower_pnl(&self) -> Option<i128> {
        self.borrower_payout
            .map(|p| formulas::borrower_pnl(p, self.m, self.margin_added, self.fee_open))
    }
}

/// A lender as projected (INDEXER.md §6.3): the on-chain `LenderPosition` values plus the
/// running totals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LenderModel {
    pub shares: u128,
    pub epoch: u32,
    pub lock: u128,
    pub lock_until: i64,
    pub deposited_tokens: u64,
    pub withdrawn_tokens: u64,
    pub sol_paid: u64,
    pub pot_draw: u64,
    pub deficit_covered: u64,
    pub deposits: u32,
    pub withdrawals: u32,
    pub claims: u32,
    pub first_slot: u64,
    pub first_ts: i64,
    pub last_slot: u64,
    pub last_ts: i64,
}

/// One fee leg (INDEXER.md §8.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeLeg {
    pub key: Key,
    pub fee_type: &'static str,
    pub recipient: &'static str,
    pub recipient_key: Option<Pubkey>,
    pub market: Pubkey,
    pub position: Option<Pubkey>,
    pub amount: u64,
    pub derived: bool,
    pub ts: i64,
}

/// One SOL settlement with its realised lender loss (INDEXER.md §8.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementModel {
    pub key: Key,
    pub position: Pubkey,
    pub market: Pubkey,
    pub keeper: Pubkey,
    pub condition: u8,
    pub b_live: u64,
    pub b_rs: u64,
    pub b_ema: u64,
    pub p_lo: u128,
    pub p_hi: u128,
    pub l_lo: u128,
    pub l_hi: u128,
    pub x_lo: u64,
    pub x_hi: u64,
    pub pprime_lo: u16,
    pub pprime_hi: u16,
    pub fv: u64,
    pub k_s: u64,
    pub lender_amount: u64,
    pub lump: u64,
    pub borrower_amount: u64,
    pub ema_age_s: i64,
    /// The vest the settlement flushed (AIR-07; not a `settlements` column: `events.data`).
    pub vest_flushed: u64,
    pub q: u64,
    pub n_open: u64,
    pub tier: u8,
    pub expired: bool,
    pub p_ref: u128,
    pub p_ref_source: &'static str,
    pub p_ref_age_s: Option<i64>,
    pub token_value: u128,
    pub lender_pnl: i128,
    pub realised_lender_loss: u128,
    pub ts: i64,
}

/// The analytics' per-market counts (INDEXER.md §8.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExitCounts {
    pub positions: u64,
    pub open: u64,
    pub closes: u64,
    pub liquidations_cr: u64,
    pub liquidations_expired: u64,
    pub expiries: u64,
    pub settlements_s1: u64,
    pub settlements_s2: u64,
    pub settlements_s3: u64,
}

/// The rows a writer derives from the events (collected by [`Model::collecting`]): the
/// reference writer of the harness (`crate::seed`) stores them as INDEXER.md's schema says.
#[derive(Clone, Debug, Default)]
pub struct Rows {
    /// Every event: key, kind, invocation, program, market, position, actor, `ev_slot`,
    /// `ev_ts`, block time, `data` (§4.2 JSON; the fields the harness reads).
    pub events: Vec<EventRow>,
    /// `market_state_log`: the after-state of every accounting event.
    pub logs: Vec<LogRow>,
    /// `market_snapshots` (PriceCranked).
    pub snapshots: Vec<SnapshotRow>,
    /// `coverage_events`: key, market, ts, kind, gap_s, open_positions, paused, reason, by.
    pub coverage: Vec<CoverageRow>,
    /// `protocol_sweeps`: key, market, ts, amount, recipient.
    pub sweeps: Vec<(Key, Pubkey, i64, u64, Pubkey)>,
    /// `share_epochs` (EpochRolled).
    pub epochs: Vec<EpochRow>,
}

/// A `market_snapshots` row (PriceCranked; the cranker is the transaction's fee payer).
#[derive(Clone, Debug)]
pub struct SnapshotRow {
    pub key: Key,
    pub market: Pubkey,
    pub ts: i64,
    pub spot_wad: u128,
    pub ema_price: u128,
    pub ema_liq: u128,
    pub liq_slow: u128,
    pub ema_count: u64,
    pub lb_lo: u128,
    pub lb_hi: u128,
    pub tier: u8,
    pub util_bps: u32,
    pub rent_basis: [u128; 3],
    pub vest_bal: u64,
    pub cranker: Pubkey,
}

/// A `share_epochs` row (EpochRolled).
#[derive(Clone, Debug)]
pub struct EpochRow {
    pub key: Key,
    pub market: Pubkey,
    pub ts: i64,
    pub epoch: u32,
    pub acc_end: i128,
    pub num: u8,
    pub den: u128,
    pub vest_flushed: u64,
}

/// An `events` row.
#[derive(Clone, Debug)]
pub struct EventRow {
    pub key: Key,
    pub block_time: i64,
    pub program: Pubkey,
    pub invocation: String,
    pub kind: &'static str,
    pub market: Option<Pubkey>,
    pub position: Option<Pubkey>,
    pub actor: Option<Pubkey>,
    pub ev_slot: u64,
    pub ev_ts: i64,
    pub data: serde_json::Value,
    /// `events.derived` (INGEST's `0100_ingest.sql`): the record donation of a rolling
    /// `sol_settle`'s `EpochRolled` (§6.4); `None` elsewhere.
    pub derived: Option<serde_json::Value>,
}

/// A `market_state_log` row.
#[derive(Clone, Debug)]
pub struct LogRow {
    pub key: Key,
    pub market: Pubkey,
    pub ts: i64,
    pub kind: &'static str,
    pub tokens_free: u64,
    pub borrowed_q: u64,
    pub total_shares: u128,
    pub open_positions: u32,
    pub total_notional_open: u64,
    pub open_vault: u64,
    pub lender_sol: i128,
    pub protocol_fees: i128,
    pub ema_price: Option<u128>,
}

/// A `coverage_events` row.
#[derive(Clone, Debug)]
pub struct CoverageRow {
    pub key: Key,
    pub market: Pubkey,
    pub ts: i64,
    pub kind: &'static str,
    pub gap_s: Option<i64>,
    pub open_positions: Option<u32>,
    pub paused: Option<bool>,
    pub reason: Option<u8>,
    pub by: Option<Pubkey>,
}

/// The contract's projections, applied to decoded events in chain order (module docs).
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub markets: BTreeMap<Pubkey, MarketModel>,
    pub positions: BTreeMap<Pubkey, PositionModel>,
    pub lenders: BTreeMap<(Pubkey, Pubkey), LenderModel>,
    /// `(market, epoch)` → `(num, den)`.
    pub share_epochs: BTreeMap<(Pubkey, u32), (u8, u128)>,
    pub fees: Vec<FeeLeg>,
    pub settlements: Vec<SettlementModel>,
    pub sweeps: Vec<(Pubkey, u64)>,
    /// The `ShareEpoch` record donations a rolling `sol_settle` swept into `lender_sol` (§6.4),
    /// by market.
    pub donations: BTreeMap<Pubkey, u64>,
    /// `(market, gap_s)` of every `CoverageGap`.
    pub coverage_gaps: Vec<(Pubkey, i64)>,
    /// Every event kind seen, with its count.
    pub kinds: BTreeMap<&'static str, u64>,
    /// Problems found while applying (an exit of an unknown position, missing terms).
    pub problems: Vec<String>,
    /// Transactions applied.
    pub applied: usize,
    /// The signature of the last transaction applied (failed or not).
    pub last_tx: Option<String>,
    /// The rows of [`Rows`], when collecting.
    pub rows: Option<Rows>,
    /// Locked terms by position, from the checkpoints' `ShortPosition` accounts.
    terms: HashMap<Pubkey, Terms>,
    /// The terms the parameters in force would lock (`initialize_config`'s arguments, then each
    /// applied `Params` change: INDEXER.md §6.5's `config_history` source).
    params_terms: Option<Terms>,
    /// The terms of proposed `Params` changes by `sha256(kind ‖ payload)`, from
    /// `propose_config`'s argument.
    pending_params: HashMap<[u8; 32], Terms>,
    /// Positions whose account terms differ from the parameters in force at their open.
    pub terms_mismatches: Vec<String>,
}

fn key_opt(k: Pubkey) -> Option<Pubkey> {
    (k != Pubkey::default()).then_some(k)
}

/// The events that change a market's accounting (a `market_state_log` row after each, §6.1).
pub const ACCOUNTING_EVENTS: [&str; 16] = [
    "MarketCreated",
    "Deposit",
    "Withdraw",
    "ClaimSol",
    "DeficitCovered",
    "LenderSynced",
    "Open",
    "Close",
    "Liquidate",
    "Expire",
    "SolSettle",
    "EpochRolled",
    "ProtocolFeesSwept",
    "MarginAdded",
    "TraderSwept",
    "VenueRewardsClaimed",
];

/// The §4.2 JSON of the event fields the harness reads back (`amount`, `sol_paid` of the
/// lender events; u64 as decimal strings; the `position` and `keeper` the API's read model
/// joins on, `assert::read_model`).
fn event_data(ev: &Ev) -> serde_json::Value {
    macro_rules! lender {
        ($x:expr) => {
            serde_json::json!({
                "owner": $x.owner.to_string(),
                "amount": $x.amount.to_string(),
                "shares": $x.shares.to_string(),
                "sol_paid": $x.sol_paid.to_string(),
                "pot_draw": $x.pot_draw.to_string(),
                "epoch": $x.epoch,
            })
        };
    }
    macro_rules! position {
        ($x:expr) => {
            serde_json::json!({ "position": $x.position.to_string() })
        };
    }
    macro_rules! exit {
        ($x:expr) => {
            serde_json::json!({
                "position": $x.position.to_string(),
                "keeper": $x.keeper.to_string(),
            })
        };
    }
    match ev {
        Ev::Deposit(x) => lender!(x),
        Ev::Withdraw(x) => lender!(x),
        Ev::ClaimSol(x) => lender!(x),
        Ev::DeficitCovered(x) => lender!(x),
        Ev::LenderSynced(x) => lender!(x),
        Ev::Open(x) => position!(x),
        Ev::Close(x) => position!(x),
        Ev::MarginAdded(x) => position!(x),
        Ev::SettleArmed(x) => position!(x),
        Ev::SettleDisarmed(x) => position!(x),
        Ev::Liquidate(x) => exit!(x),
        Ev::Expire(x) => exit!(x),
        Ev::SolSettle(x) => exit!(x),
        _ => serde_json::json!({}),
    }
}

/// The event's `position` field and acting wallet (INDEXER.md §5.2 step 4).
fn position_and_actor(ev: &Ev, fee_payer: Pubkey) -> (Option<Pubkey>, Option<Pubkey>) {
    match ev {
        Ev::Deposit(x) => (None, Some(x.owner)),
        Ev::Withdraw(x) => (None, Some(x.owner)),
        Ev::ClaimSol(x) => (None, Some(x.owner)),
        Ev::DeficitCovered(x) => (None, Some(x.owner)),
        Ev::LenderSynced(x) => (None, Some(x.owner)),
        Ev::Open(x) => (Some(x.position), Some(x.borrower)),
        Ev::Close(x) => (Some(x.position), Some(x.borrower)),
        Ev::Liquidate(x) => (Some(x.position), Some(x.keeper)),
        Ev::Expire(x) => (Some(x.position), Some(x.keeper)),
        Ev::SolSettle(x) => (Some(x.position), Some(x.keeper)),
        Ev::SettleArmed(x) => (Some(x.position), None),
        Ev::SettleDisarmed(x) => (Some(x.position), None),
        Ev::MarginAdded(x) => (Some(x.position), None),
        Ev::CoveragePauseSet(x) => (None, Some(x.by)),
        Ev::PauseSet(x) => (None, Some(x.by)),
        Ev::ConfigCancelled(x) => (None, Some(x.by)),
        Ev::PriceCranked(_) => (None, Some(fee_payer)),
        _ => (None, None),
    }
}

impl Model {
    /// An empty model that takes locked terms from every `ShortPosition` any checkpoint of
    /// `session` holds (positions' terms never change; the `terms_source = 'account'` path of
    /// INDEXER.md §6.5).
    pub fn new(session: &Session) -> anyhow::Result<Self> {
        let mut terms = HashMap::new();
        let program = shortwin::ID.to_string();
        for (_, c) in &session.checkpoints {
            for a in &c.accounts {
                if a.owner != program {
                    continue;
                }
                let data = B64.decode(&a.data)?;
                if data.starts_with(ShortPosition::DISCRIMINATOR) {
                    let p: ShortPosition = borsh(&data)?;
                    terms.insert(
                        a.pubkey.parse().map_err(|e| anyhow!("{e:?}"))?,
                        Terms::of(&p),
                    );
                }
            }
        }
        Ok(Self {
            terms,
            ..Self::default()
        })
    }

    /// The same model, collecting [`Rows`].
    #[must_use]
    pub fn collecting(mut self) -> Self {
        self.rows = Some(Rows::default());
        self
    }

    /// Applies `txs` (in order).
    pub fn apply_all<'a>(&mut self, txs: impl IntoIterator<Item = &'a ChainTx>) {
        for t in txs {
            self.apply(t);
        }
    }

    fn fee(&mut self, t: &ChainTx, r: &EvRec, leg: (&'static str, &'static str), x: FeeX) {
        if x.amount == 0 {
            return;
        }
        self.fees.push(FeeLeg {
            key: Key::of(t, r),
            fee_type: leg.0,
            recipient: leg.1,
            recipient_key: x.key,
            market: x.market,
            position: x.position,
            amount: x.amount,
            derived: x.derived,
            ts: x.ts,
        });
    }

    /// R3B-2: a lender event that leaves `S = 0` reports a flush (the last shares were paid the
    /// vest first; the program holds no vest while `S = 0`).
    fn last_shares(&mut self, market: &Pubkey, s_after: u128, key: &Key) {
        if s_after == 0
            && let Some(m) = self.markets.get_mut(market)
        {
            m.set_vest(0, key);
        }
    }

    fn lender_event(
        &mut self,
        t: &ChainTx,
        market: Pubkey,
        owner: Pubkey,
        f: LenderFields,
        kind: LenderKind,
    ) {
        let epochs = &self.share_epochs;
        let l = self
            .lenders
            .entry((market, owner))
            .or_insert_with(|| LenderModel {
                first_slot: t.slot,
                first_ts: f.ts,
                ..LenderModel::default()
            });
        // The program's lazy conversion (`touch`): every epoch the row is behind on.
        let mut shares = l.shares;
        for ep in l.epoch..f.epoch {
            if let Some((num, den)) = epochs.get(&(market, ep)) {
                shares = formulas::convert_shares(shares, *num, *den);
            }
        }
        match kind {
            LenderKind::Deposit => {
                shares += f.shares;
                l.deposited_tokens += f.amount;
                l.deposits += 1;
            }
            LenderKind::Withdraw => {
                shares = shares.saturating_sub(f.shares);
                l.withdrawn_tokens += f.amount;
                l.withdrawals += 1;
            }
            LenderKind::Claim => l.claims += 1,
            LenderKind::Cover => l.deficit_covered += f.amount,
            LenderKind::Other => {}
        }
        l.shares = shares;
        l.epoch = f.epoch;
        l.lock = f.lock;
        l.lock_until = f.lock_until;
        l.sol_paid += f.sol_paid;
        l.pot_draw += f.pot_draw;
        l.last_slot = t.slot;
        l.last_ts = f.ts;
        if let Some(m) = self.markets.get_mut(&market) {
            m.lender_sol -= i128::from(f.sol_paid);
        }
    }

    /// Closes the market side of an exit of `position` (`T += q` unless settled).
    fn exit(&mut self, position: &Pubkey, returns_tokens: bool) -> Option<PositionModel> {
        let Some(p) = self.positions.get(position).cloned() else {
            self.problems
                .push(format!("exit of unknown position {position}"));
            return None;
        };
        if let Some(m) = self.markets.get_mut(&p.market) {
            if returns_tokens {
                m.tokens_free += p.q;
            }
            m.borrowed_q = m.borrowed_q.saturating_sub(p.q);
            if let Some(b) = m.borrowed_q_by_tier.get_mut(usize::from(p.tier)) {
                *b = b.saturating_sub(p.q);
            }
            m.open_positions = m.open_positions.saturating_sub(1);
            m.total_notional_open = m.total_notional_open.saturating_sub(p.n_open);
            m.open_vault = m.open_vault.saturating_sub(p.v);
            if m.open_positions == 0 {
                m.max_q_hwm = 0;
            }
        }
        Some(p)
    }

    fn terms_of(&mut self, p: &PositionModel) -> Terms {
        p.terms.unwrap_or_else(|| {
            self.problems
                .push(format!("no locked terms for position {}", p.position));
            Terms {
                rent_protocol_bps: 0,
                penalty_split: [0; 3],
                expiry_fee_bps: 0,
            }
        })
    }

    fn exited(&mut self, t: &ChainTx, position: &Pubkey, status: &'static str, ts: i64) {
        if let Some(p) = self.positions.get_mut(position) {
            p.status = status;
            p.exit_slot = Some(t.slot);
            p.exit_ts = Some(ts);
            p.exit_signature = Some(t.signature.clone());
            p.armed = false;
        }
    }

    /// Applies one transaction's events (a failed transaction has none).
    pub fn apply(&mut self, t: &ChainTx) {
        self.applied += 1;
        self.last_tx = Some(t.signature.clone());
        if !t.ok {
            return;
        }
        for inv in t
            .invocations
            .iter()
            .filter(|i| i.name == "initialize_config")
        {
            if let Some(args) = inv
                .data
                .get(8..)
                .and_then(|mut d| shortwin::instruction::InitializeConfig::deserialize(&mut d).ok())
            {
                self.params_terms = Some(Terms::from_params(&args.args.params));
            }
        }
        for r in &t.events {
            self.apply_event(t, r);
        }
    }

    /// Applies one event of `t` (and collects its rows).
    #[allow(clippy::too_many_lines)]
    pub fn apply_event(&mut self, t: &ChainTx, r: &EvRec) {
        let kind = r.ev.name();
        *self.kinds.entry(kind).or_default() += 1;
        let mk = r.ev.market();
        let key = Key::of(t, r);
        let ts = r.ev.ts().unwrap_or(t.block_time);
        let mut derived = None;
        match &r.ev {
            Ev::MarketCreated(x) => {
                self.markets.insert(
                    x.market,
                    MarketModel {
                        market: x.market,
                        mint: x.mint,
                        token_program: x.token_program,
                        creator: x.creator,
                        bonding_curve: key_opt(x.bonding_curve),
                        canonical_pool: key_opt(x.canonical_pool),
                        created_slot: x.slot,
                        created_ts: x.ts,
                        created_signature: t.signature.clone(),
                        creation_fee: x.creation_fee,
                        venue_at_listing: x.venue,
                        tier_at_listing: x.tier,
                        venue: x.venue,
                        tier: x.tier,
                        ray_pool: key_opt(x.ray_pool),
                        protocol_fees: i128::from(x.creation_fee),
                        ..MarketModel::default()
                    },
                );
                self.fee(
                    t,
                    r,
                    ("creation_fee", "protocol"),
                    FeeX::new(x.market, x.ts, x.creation_fee),
                );
            }
            Ev::Deposit(x) => {
                self.last_shares(&x.market, x.s_after, &key);
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.tokens_free = x.t_after;
                    m.total_shares = x.s_after;
                    m.wipe_live = m.wipe_live && x.s_after == 0;
                }
                self.lender_event(
                    t,
                    x.market,
                    x.owner,
                    LenderFields::of_deposit(x),
                    LenderKind::Deposit,
                );
            }
            Ev::Withdraw(x) => {
                self.last_shares(&x.market, x.s_after, &key);
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.tokens_free = x.t_after;
                    m.total_shares = x.s_after;
                }
                self.lender_event(
                    t,
                    x.market,
                    x.owner,
                    LenderFields::of_withdraw(x),
                    LenderKind::Withdraw,
                );
            }
            Ev::ClaimSol(x) => {
                self.last_shares(&x.market, x.s_after, &key);
                self.lender_event(
                    t,
                    x.market,
                    x.owner,
                    LenderFields::of_claim(x),
                    LenderKind::Claim,
                );
            }
            Ev::DeficitCovered(x) => {
                self.last_shares(&x.market, x.s_after, &key);
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.lender_sol += i128::from(x.amount);
                }
                self.lender_event(
                    t,
                    x.market,
                    x.owner,
                    LenderFields::of_cover(x),
                    LenderKind::Cover,
                );
            }
            Ev::LenderSynced(x) => {
                self.last_shares(&x.market, x.s_after, &key);
                self.lender_event(
                    t,
                    x.market,
                    x.owner,
                    LenderFields::of_sync(x),
                    LenderKind::Other,
                );
            }
            Ev::Open(x) => {
                let account = self.terms.get(&x.position).copied();
                if let (Some(a), Some(h)) = (account, self.params_terms)
                    && a != h
                {
                    self.terms_mismatches.push(format!(
                        "position {}: account terms {a:?} ≠ parameters in force {h:?}",
                        x.position
                    ));
                }
                let terms = account.or(self.params_terms);
                self.positions.insert(
                    x.position,
                    PositionModel {
                        position: x.position,
                        market: x.market,
                        borrower: x.borrower,
                        nonce: x.nonce,
                        tier: x.tier,
                        venue: x.venue,
                        q: x.q,
                        s: x.s,
                        m: x.m,
                        v_open: x.v,
                        v: x.v,
                        margin_added: 0,
                        n_open: x.n_open,
                        fee_open: x.fee_open,
                        venue_rent: x.venue_rent,
                        entry_price: x.entry_price,
                        spot_wad_open: x.spot_wad,
                        ema_wad_open: x.ema_wad,
                        b_spot_open: x.b_spot,
                        b_ema_open: x.b_ema,
                        deadline: x.deadline,
                        opened_ts: x.ts,
                        opened_slot: t.slot,
                        opened_tx_index: t.tx_index,
                        open_signature: t.signature.clone(),
                        armed: false,
                        s_count: 0,
                        s_since: 0,
                        arm_count: 0,
                        disarm_count: 0,
                        terms,
                        status: "open",
                        exit_slot: None,
                        exit_ts: None,
                        exit_signature: None,
                        exit_keeper: None,
                        buyback_cost: None,
                        b_live_exit: None,
                        rent_paid: None,
                        rent_protocol: None,
                        penalty: None,
                        expiry_fee: None,
                        keeper_paid: None,
                        borrower_payout: None,
                        token_surplus: None,
                        liquidated_expired: None,
                        settle_condition: None,
                    },
                );
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.tokens_free = m.tokens_free.saturating_sub(x.q);
                    m.borrowed_q += x.q;
                    if let Some(b) = m.borrowed_q_by_tier.get_mut(usize::from(x.tier)) {
                        *b += x.q;
                    }
                    m.open_positions += 1;
                    m.total_notional_open += x.n_open;
                    m.max_q_hwm = m.max_q_hwm.max(x.q);
                    m.open_vault += x.v;
                    m.protocol_fees += i128::from(x.fee_open);
                    m.tier = x.tier;
                    m.spot_wad = Some(x.spot_wad);
                    if x.ema_wad > 0 {
                        m.ema_price = Some(x.ema_wad);
                        m.ema_ts = Some(x.ts);
                    }
                }
                let f = FeeX::new(x.market, x.ts, x.fee_open).position(x.position);
                self.fee(t, r, ("open_fee", "protocol"), f);
                let f = FeeX::new(x.market, x.ts, x.venue_rent).position(x.position);
                self.fee(t, r, ("venue_rent", "venue"), f);
            }
            Ev::Close(x) => {
                if self.exit(&x.position, true).is_some() {
                    self.exited(t, &x.position, "closed", x.ts);
                    let pp = self.positions.get_mut(&x.position).expect("known");
                    pp.buyback_cost = Some(x.buyback_cost);
                    pp.b_live_exit = Some(x.b_live);
                    pp.borrower_payout = Some(x.borrower_payout);
                    pp.rent_paid = Some(x.rent);
                    pp.rent_protocol = Some(x.rent_protocol);
                    pp.token_surplus = Some(x.token_surplus);
                }
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.lender_sol += i128::from(x.rent) - i128::from(x.rent_protocol);
                    m.protocol_fees += i128::from(x.rent_protocol);
                }
                let f = FeeX::new(x.market, x.ts, x.rent.saturating_sub(x.rent_protocol))
                    .position(x.position);
                self.fee(t, r, ("rent_lender", "lenders"), f);
                let f = FeeX::new(x.market, x.ts, x.rent_protocol).position(x.position);
                self.fee(t, r, ("rent_protocol", "protocol"), f);
            }
            Ev::Liquidate(x) => {
                let mut share = 0;
                if let Some(p) = self.exit(&x.position, true) {
                    let terms = self.terms_of(&p);
                    share = formulas::rent_protocol_share(x.rent, terms.rent_protocol_bps);
                    self.exited(t, &x.position, "liquidated", x.ts);
                    let pp = self.positions.get_mut(&x.position).expect("known");
                    pp.exit_keeper = Some(x.keeper);
                    pp.buyback_cost = Some(x.buyback_cost);
                    pp.b_live_exit = Some(x.b_live);
                    pp.borrower_payout = Some(x.borrower_payout);
                    pp.liquidated_expired = Some(x.expired);
                    pp.rent_paid = Some(x.rent);
                    pp.rent_protocol = Some(share);
                    pp.penalty = Some(x.penalty);
                    pp.keeper_paid = Some(x.penalty_keeper + x.keeper_topup);
                    pp.token_surplus = Some(x.token_surplus);
                }
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.lender_sol +=
                        i128::from(x.rent) - i128::from(share) + i128::from(x.penalty_lender);
                    m.protocol_fees += i128::from(share) + i128::from(x.penalty_protocol);
                }
                let base = |a: u64| FeeX::new(x.market, x.ts, a).position(x.position);
                self.fee(
                    t,
                    r,
                    ("rent_lender", "lenders"),
                    base(x.rent - share).derived(),
                );
                self.fee(t, r, ("rent_protocol", "protocol"), base(share).derived());
                self.fee(
                    t,
                    r,
                    ("liq_penalty_lender", "lenders"),
                    base(x.penalty_lender),
                );
                self.fee(
                    t,
                    r,
                    ("liq_penalty_protocol", "protocol"),
                    base(x.penalty_protocol),
                );
                self.fee(
                    t,
                    r,
                    ("liq_penalty_keeper", "keeper"),
                    base(x.penalty_keeper).key(x.keeper),
                );
                self.fee(
                    t,
                    r,
                    ("liq_keeper_topup", "keeper"),
                    base(x.keeper_topup).key(x.keeper),
                );
            }
            Ev::Expire(x) => {
                let mut share = 0;
                if let Some(p) = self.exit(&x.position, true) {
                    let terms = self.terms_of(&p);
                    share = formulas::rent_protocol_share(x.rent, terms.rent_protocol_bps);
                    self.exited(t, &x.position, "expired", x.ts);
                    let pp = self.positions.get_mut(&x.position).expect("known");
                    pp.exit_keeper = Some(x.keeper);
                    pp.buyback_cost = Some(x.buyback_cost);
                    pp.b_live_exit = Some(x.b_live);
                    pp.borrower_payout = Some(x.borrower_payout);
                    pp.rent_paid = Some(x.rent);
                    pp.rent_protocol = Some(share);
                    pp.expiry_fee = Some(x.expiry_fee);
                    pp.keeper_paid = Some(x.keeper_reward);
                    pp.token_surplus = Some(x.token_surplus);
                }
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.lender_sol +=
                        i128::from(x.rent) - i128::from(share) + i128::from(x.expiry_fee);
                    m.protocol_fees += i128::from(share);
                }
                let base = |a: u64| FeeX::new(x.market, x.ts, a).position(x.position);
                self.fee(
                    t,
                    r,
                    ("rent_lender", "lenders"),
                    base(x.rent - share).derived(),
                );
                self.fee(t, r, ("rent_protocol", "protocol"), base(share).derived());
                self.fee(t, r, ("expiry_fee", "lenders"), base(x.expiry_fee));
                self.fee(
                    t,
                    r,
                    ("expiry_keeper_reward", "keeper"),
                    base(x.keeper_reward).key(x.keeper),
                );
            }
            Ev::SolSettle(x) => {
                let last_ema = self.markets.get(&x.market).and_then(|m| m.ema_price);
                let last_ts = self.markets.get(&x.market).and_then(|m| m.ema_ts);
                if let Some(p) = self.exit(&x.position, false) {
                    let (p_ref, src) = formulas::p_ref(last_ema, x.p_lo, x.p_hi);
                    let o = formulas::settlement_outcome(x.lender_amount, p.q, p_ref);
                    self.settlements.push(SettlementModel {
                        key: key.clone(),
                        position: x.position,
                        market: x.market,
                        keeper: x.keeper,
                        condition: x.condition,
                        b_live: x.b_live,
                        b_rs: x.b_rs,
                        b_ema: x.b_ema,
                        p_lo: x.p_lo,
                        p_hi: x.p_hi,
                        l_lo: x.l_lo,
                        l_hi: x.l_hi,
                        x_lo: x.x_lo,
                        x_hi: x.x_hi,
                        pprime_lo: x.pprime_lo,
                        pprime_hi: x.pprime_hi,
                        fv: x.fv,
                        k_s: x.k_s,
                        lender_amount: x.lender_amount,
                        lump: x.lump,
                        borrower_amount: x.borrower_amount,
                        ema_age_s: x.ema_age_s,
                        vest_flushed: x.vest_flushed,
                        q: p.q,
                        n_open: p.n_open,
                        tier: p.tier,
                        expired: x.ts >= p.deadline,
                        p_ref,
                        p_ref_source: src.as_str(),
                        p_ref_age_s: (src == formulas::PRefSource::CrankClamped)
                            .then(|| last_ts.map(|ts| x.ts - ts))
                            .flatten(),
                        token_value: o.token_value,
                        lender_pnl: o.lender_pnl,
                        realised_lender_loss: o.realised_lender_loss,
                        ts: x.ts,
                    });
                    self.exited(t, &x.position, "settled", x.ts);
                    let pp = self.positions.get_mut(&x.position).expect("known");
                    pp.exit_keeper = Some(x.keeper);
                    pp.b_live_exit = Some(x.b_live);
                    pp.borrower_payout = Some(x.borrower_amount);
                    pp.keeper_paid = Some(x.k_s);
                    pp.settle_condition = Some(x.condition);
                }
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.lender_sol += i128::from(x.lender_amount);
                    // AIR-07: the settlement flushed the vest, with or without a roll.
                    if x.vest_flushed > 0 {
                        m.set_vest(0, &key);
                    }
                }
                let f = FeeX::new(x.market, x.ts, x.k_s)
                    .position(x.position)
                    .key(x.keeper);
                self.fee(t, r, ("settle_keeper_reward", "keeper"), f);
            }
            Ev::SettleArmed(x) => {
                if let Some(p) = self.positions.get_mut(&x.position) {
                    p.armed = true;
                    p.s_count = x.s_count;
                    p.s_since = x.s_since;
                    p.arm_count += 1;
                }
            }
            Ev::SettleDisarmed(x) => {
                if let Some(p) = self.positions.get_mut(&x.position) {
                    p.armed = false;
                    p.s_count = x.s_count;
                    p.s_since = x.s_since;
                    p.disarm_count += 1;
                }
            }
            Ev::EpochRolled(x) => {
                self.share_epochs
                    .insert((x.market, x.epoch), (x.num, x.den));
                // The record donation (§6.4): a System transfer from the record's address
                // (`sol_settle`'s account 8) to the market vault (account 6), inside the
                // `sol_settle` that rolled.
                let donation: u64 = t
                    .transfers
                    .iter()
                    .filter(|tr| {
                        tr.ix_index == r.ix_index
                            && tr.under == "sol_settle"
                            && tr.under_accounts.get(8) == Some(&tr.from)
                            && tr.under_accounts.get(6) == Some(&tr.to)
                    })
                    .map(|tr| tr.lamports)
                    .sum();
                if donation > 0 {
                    *self.donations.entry(x.market).or_default() += donation;
                }
                if r.invocation == "sol_settle" {
                    derived = Some(serde_json::json!({ "record_donation": donation.to_string() }));
                }
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.total_shares = formulas::convert_shares(m.total_shares, x.num, x.den);
                    m.share_epoch = x.epoch + 1;
                    m.wipe_live = x.num == 0;
                    m.lender_sol += i128::from(donation);
                    // A full wipe holds no vest (its donation tops up the wipe record); a rebase
                    // after a flush holds only the donation it vested anew.
                    if x.num == 0 {
                        m.set_vest(0, &key);
                    } else if x.vest_flushed > 0 {
                        m.set_vest(donation, &key);
                    }
                }
                if let Some(rows) = self.rows.as_mut() {
                    rows.epochs.push(EpochRow {
                        key: key.clone(),
                        market: x.market,
                        ts: x.ts,
                        epoch: x.epoch,
                        acc_end: x.acc_end,
                        num: x.num,
                        den: x.den,
                        vest_flushed: x.vest_flushed,
                    });
                }
            }
            Ev::ProtocolFeesSwept(x) => {
                self.sweeps.push((x.market, x.amount));
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.protocol_fees -= i128::from(x.amount);
                }
                if let Some(rows) = self.rows.as_mut() {
                    rows.sweeps
                        .push((key.clone(), x.market, x.ts, x.amount, x.recipient));
                }
            }
            Ev::MarginAdded(x) => {
                if let Some(p) = self.positions.get_mut(&x.position) {
                    p.margin_added += x.amount;
                    p.v += x.amount;
                }
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.open_vault += x.amount;
                }
            }
            Ev::PriceCranked(x) => {
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.spot_wad = Some(x.spot_wad);
                    m.tier = x.tier;
                    m.set_vest(x.vest_bal, &key);
                    if x.ema_price > 0 {
                        m.ema_price = Some(x.ema_price);
                        m.ema_ts = Some(x.ts);
                    }
                }
                if let Some(rows) = self.rows.as_mut() {
                    rows.snapshots.push(SnapshotRow {
                        key: key.clone(),
                        market: x.market,
                        ts: x.ts,
                        spot_wad: x.spot_wad,
                        ema_price: x.ema_price,
                        ema_liq: x.ema_liq,
                        liq_slow: x.liq_slow,
                        ema_count: x.ema_count,
                        lb_lo: x.lb_lo,
                        lb_hi: x.lb_hi,
                        tier: x.tier,
                        util_bps: x.util_bps,
                        rent_basis: x.rent_basis,
                        vest_bal: x.vest_bal,
                        cranker: t.fee_payer,
                    });
                }
            }
            Ev::VenueStateChanged(x) => {
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.venue = x.to;
                }
            }
            Ev::PoolProposed(x) => {
                let kind = candidate_kind(t, r);
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.fp_candidate = key_opt(x.candidate);
                    m.fp_venue = kind.or(m.fp_venue);
                    m.pool_d = x.d_inc;
                }
            }
            Ev::PoolSwitched(x) => {
                let kind = candidate_kind(t, r);
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.ray_pool = key_opt(x.candidate);
                    if let Some(v) = kind.or(m.fp_venue) {
                        m.venue = v;
                    }
                    m.pool_d = x.d_cand;
                    m.fp_candidate = None;
                    m.fp_venue = None;
                }
            }
            Ev::PoolSwitchCancelled(x) => {
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.fp_candidate = None;
                    m.fp_venue = None;
                }
            }
            Ev::TraderSwept(x) => {
                let (leg, to_lenders) = match x.to {
                    e::SWEEP_TO_VEST => (("trader_sweep_lenders", "lenders"), true),
                    e::SWEEP_TO_WIPE_RECORD => (("trader_sweep_wipe", "lenders"), true),
                    _ => (("trader_sweep_protocol", "protocol"), false),
                };
                if let Some(m) = self.markets.get_mut(&x.market) {
                    if to_lenders {
                        m.lender_sol += i128::from(x.lamports);
                    } else {
                        m.protocol_fees += i128::from(x.lamports);
                    }
                }
                self.fee(t, r, leg, FeeX::new(x.market, x.ts, x.lamports));
            }
            Ev::VenueRewardsClaimed(x) => {
                let (leg, to_lenders) = match x.to {
                    e::SWEEP_TO_VEST => (("venue_rewards_lenders", "lenders"), true),
                    e::SWEEP_TO_WIPE_RECORD => (("venue_rewards_wipe", "lenders"), true),
                    _ => (("venue_rewards_protocol", "protocol"), false),
                };
                if let Some(m) = self.markets.get_mut(&x.market) {
                    if to_lenders {
                        m.lender_sol += i128::from(x.lamports);
                    } else {
                        m.protocol_fees += i128::from(x.lamports);
                    }
                }
                self.fee(t, r, leg, FeeX::new(x.market, x.ts, x.lamports));
            }
            Ev::MintSafetyTripped(x) => {
                if r.ev_source == 0
                    && let Some(m) = self.markets.get_mut(&x.market)
                {
                    m.mint_unsafe = true;
                }
            }
            Ev::CoverageGap(x) => {
                self.coverage_gaps.push((x.market, x.gap_s));
                if let Some(rows) = self.rows.as_mut() {
                    rows.coverage.push(CoverageRow {
                        key: key.clone(),
                        market: x.market,
                        ts: x.ts,
                        kind: "gap",
                        gap_s: Some(x.gap_s),
                        open_positions: Some(x.open_positions),
                        paused: None,
                        reason: None,
                        by: None,
                    });
                }
            }
            Ev::CoveragePauseSet(x) => {
                if let Some(m) = self.markets.get_mut(&x.market) {
                    m.paused_coverage = x.paused;
                    if !x.paused {
                        m.coverage_cleared_at = x.ts;
                    }
                }
                if let Some(rows) = self.rows.as_mut() {
                    rows.coverage.push(CoverageRow {
                        key: key.clone(),
                        market: x.market,
                        ts: x.ts,
                        kind: "pause_set",
                        gap_s: None,
                        open_positions: None,
                        paused: Some(x.paused),
                        reason: Some(x.reason),
                        by: Some(x.by),
                    });
                }
            }
            Ev::PauseSet(x) => {
                if x.scope == e::SCOPE_MARKET
                    && let Some(m) = mk.and_then(|k| self.markets.get_mut(&k))
                {
                    m.paused_opens = x.paused;
                }
            }
            Ev::RouteBrokenSet(x) => {
                if x.scope == e::SCOPE_MARKET
                    && let Some(m) = mk.and_then(|k| self.markets.get_mut(&k))
                {
                    m.route_broken = x.broken;
                }
            }
            Ev::ConfigProposed(x) => {
                let change = t
                    .invocations
                    .iter()
                    .rev()
                    .find(|v| v.ix_index == r.ix_index && v.name == "propose_config")
                    .and_then(|v| v.data.get(8..))
                    .and_then(|mut d| {
                        shortwin::instruction::ProposeConfig::deserialize(&mut d).ok()
                    });
                if let Some(shortwin::instruction::ProposeConfig {
                    change: shortwin::state::ConfigChange::Params { params, .. },
                }) = change
                {
                    self.pending_params
                        .insert(x.hash, Terms::from_params(&params));
                }
            }
            Ev::ConfigApplied(x) => {
                if let Some(t) = self.pending_params.remove(&x.hash) {
                    self.params_terms = Some(t);
                }
            }
            _ => {}
        }
        // The watermark of every market event, the after-state of every accounting one.
        if let Some(m) = mk.and_then(|k| self.markets.get_mut(&k)) {
            m.last = key.clone();
        }
        let Some(rows) = self.rows.as_mut() else {
            return;
        };
        let (position, actor) = position_and_actor(&r.ev, t.fee_payer);
        rows.events.push(EventRow {
            key: key.clone(),
            block_time: t.block_time,
            program: r.program,
            invocation: r.invocation.clone(),
            kind,
            market: mk,
            position,
            actor,
            ev_slot: event_slot(&r.ev).unwrap_or(t.slot),
            ev_ts: ts,
            data: event_data(&r.ev),
            derived,
        });
        if ACCOUNTING_EVENTS.contains(&kind)
            && let Some(m) = mk.and_then(|k| self.markets.get(&k))
        {
            rows.logs.push(LogRow {
                key,
                market: m.market,
                ts,
                kind,
                tokens_free: m.tokens_free,
                borrowed_q: m.borrowed_q,
                total_shares: m.total_shares,
                open_positions: m.open_positions,
                total_notional_open: m.total_notional_open,
                open_vault: m.open_vault,
                lender_sol: m.lender_sol,
                protocol_fees: m.protocol_fees,
                ema_price: m.ema_price,
            });
        }
    }

    /// Exit counts of `market` (INDEXER.md §8.3).
    pub fn exits(&self, market: &Pubkey) -> ExitCounts {
        let mut c = ExitCounts::default();
        for p in self.positions.values().filter(|p| p.market == *market) {
            c.positions += 1;
            match (p.status, p.liquidated_expired, p.settle_condition) {
                ("open", _, _) => c.open += 1,
                ("closed", _, _) => c.closes += 1,
                ("liquidated", Some(true), _) => c.liquidations_expired += 1,
                ("liquidated", _, _) => c.liquidations_cr += 1,
                ("expired", _, _) => c.expiries += 1,
                ("settled", _, Some(1)) => c.settlements_s1 += 1,
                ("settled", _, Some(2)) => c.settlements_s2 += 1,
                ("settled", _, Some(3)) => c.settlements_s3 += 1,
                _ => {}
            }
        }
        c
    }

    /// Lenders of `market` whose shares, converted to the market's current epoch, are > 0
    /// (`markets.lender_count`, §6.2).
    pub fn lender_count(&self, market: &Pubkey) -> u32 {
        let Some(m) = self.markets.get(market) else {
            return 0;
        };
        let n = self
            .lenders
            .iter()
            .filter(|((mk, _), l)| {
                *mk == *market && {
                    let recs = (l.epoch..m.share_epoch)
                        .filter_map(|ep| self.share_epochs.get(&(*market, ep)).copied());
                    formulas::shares_at_epoch(l.shares, recs) > 0
                }
            })
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// Compares the model with the chain state `c` (every [E] value of INDEXER.md §6 the
    /// accounts hold); returns the mismatches.
    #[allow(clippy::too_many_lines)]
    pub fn check_against(&self, c: &ChainState) -> Vec<String> {
        let mut out = Vec::new();
        let mut eq = |what: String, model: String, chain: String| {
            if model != chain {
                out.push(format!("{what}: model {model} ≠ chain {chain}"));
            }
        };
        for (k, m) in &c.markets {
            let Some(mm) = self.markets.get(k) else {
                eq(format!("market {k}"), "absent".into(), "present".into());
                continue;
            };
            let f = |b: u16| m.flags & b != 0;
            let s = |what: &str| format!("market {k} {what}");
            eq(
                s("tokens_free"),
                mm.tokens_free.to_string(),
                m.tokens_free.to_string(),
            );
            eq(
                s("borrowed_q"),
                mm.borrowed_q.to_string(),
                m.borrowed_q.to_string(),
            );
            eq(
                s("borrowed_q_by_tier"),
                format!("{:?}", mm.borrowed_q_by_tier),
                format!("{:?}", m.borrowed_q_by_tier),
            );
            eq(
                s("total_shares"),
                mm.total_shares.to_string(),
                m.total_shares.get().to_string(),
            );
            eq(
                s("share_epoch"),
                mm.share_epoch.to_string(),
                m.share_epoch.to_string(),
            );
            eq(
                s("open_positions"),
                mm.open_positions.to_string(),
                m.open_positions.to_string(),
            );
            eq(
                s("total_notional_open"),
                mm.total_notional_open.to_string(),
                m.total_notional_open.to_string(),
            );
            eq(
                s("max_q_hwm"),
                mm.max_q_hwm.to_string(),
                m.max_q_hwm.to_string(),
            );
            eq(s("venue"), mm.venue.to_string(), m.venue.to_string());
            eq(
                s("paused_opens"),
                mm.paused_opens.to_string(),
                f(MARKET_FLAG_PAUSED_OPENS).to_string(),
            );
            eq(
                s("paused_coverage"),
                mm.paused_coverage.to_string(),
                f(MARKET_FLAG_PAUSED_COVERAGE).to_string(),
            );
            eq(
                s("mint_unsafe"),
                mm.mint_unsafe.to_string(),
                f(MARKET_FLAG_MINT_UNSAFE).to_string(),
            );
            eq(
                s("route_broken"),
                mm.route_broken.to_string(),
                f(MARKET_FLAG_ROUTE_BROKEN).to_string(),
            );
            eq(
                s("wipe_live"),
                mm.wipe_live.to_string(),
                f(MARKET_FLAG_WIPE_LIVE).to_string(),
            );
            eq(
                s("coverage_cleared_at"),
                mm.coverage_cleared_at.to_string(),
                m.coverage_cleared_at.to_string(),
            );
            // `vest_bal` is [E*]. Exact right after the event whose rule set it (a crank's
            // snapshot, a reported flush, a flushing rebase's record donation vested anew): the
            // checkpoint follows that event's transaction, and no later event of the market came
            // in it (a release at the same second is a no-op; every lump comes with an event).
            let vest_exact = mm
                .vest_set
                .as_ref()
                .is_some_and(|k| *k == mm.last && self.last_tx.as_ref() == Some(&k.signature));
            if vest_exact {
                eq(
                    s("vest_bal (as of the event that set it)"),
                    format!("{:?}", mm.vest_bal),
                    format!("{:?}", Some(m.vest_bal)),
                );
            }
            // Exact also where a flush left none (AIR-07): a vest at 0 before its end was
            // flushed — a settlement that left at most 1/1,000 of the token claims, a full wipe,
            // or the last shares leaving — and nothing has vested since (a release that reaches
            // the end leaves `vest_last ≥ vest_end`). Every flush is reported, so the contract's
            // `vest_bal` is 0 too.
            if m.vest_bal == 0 && m.vest_end > m.vest_last {
                eq(
                    s("vest_bal (flushed)"),
                    format!("{:?}", mm.vest_bal),
                    format!("{:?}", Some(0u64)),
                );
            }
            // `pool_d` is not compared: every observation's prologue refreshes it from the
            // incumbent's locked depth (`engine/prologue.rs`), so it is [E*] (exact only as of a
            // PoolProposed / PoolSwitched), not [E] as INDEXER.md §6.2 lists it.
            if mm.venue == 4 || mm.venue == 5 {
                eq(
                    s("ray_pool"),
                    format!("{:?}", mm.ray_pool),
                    format!(
                        "{:?}",
                        key_opt(m.pool_cache[shortwin::state::raydium_cache::RAY_POOL])
                    ),
                );
            }
            eq(
                s("fp_candidate"),
                format!("{:?}", mm.fp_candidate),
                format!("{:?}", key_opt(m.fp_keys[0])),
            );
            let open_v: u64 = c.positions_of(k).map(|(_, p)| p.v).sum();
            eq(
                s("open_vault (Σ open v)"),
                mm.open_vault.to_string(),
                open_v.to_string(),
            );
            match c.vault_of(k) {
                Some(v) => {
                    eq(
                        s("lender_sol"),
                        mm.lender_sol.to_string(),
                        v.lender_sol.to_string(),
                    );
                    eq(
                        s("protocol_fees"),
                        mm.protocol_fees.to_string(),
                        v.protocol_fees.to_string(),
                    );
                }
                None => eq(s("vault"), "present".into(), "absent".into()),
            }
        }
        for (k, p) in &c.positions {
            let Some(pm) = self.positions.get(k) else {
                eq(format!("position {k}"), "absent".into(), "open".into());
                continue;
            };
            let s = |what: &str| format!("position {k} {what}");
            eq(s("status"), pm.status.into(), "open".into());
            eq(s("q"), pm.q.to_string(), p.q.to_string());
            eq(s("s"), pm.s.to_string(), p.s.to_string());
            // `add_margin` grows `ShortPosition.m` as well as `v`: the chain's `m` is the
            // `Open`'s plus every `MarginAdded` (positions.m + positions.margin_added).
            eq(
                s("m (Open.m + margin_added)"),
                (pm.m + pm.margin_added).to_string(),
                p.m.to_string(),
            );
            eq(s("v"), pm.v.to_string(), p.v.to_string());
            eq(s("n_open"), pm.n_open.to_string(), p.n_open.to_string());
            eq(
                s("fee_open"),
                pm.fee_open.to_string(),
                p.fee_open.to_string(),
            );
            eq(
                s("deadline"),
                pm.deadline.to_string(),
                p.deadline.to_string(),
            );
            eq(s("tier"), pm.tier.to_string(), p.tier.to_string());
            eq(
                s("armed"),
                pm.armed.to_string(),
                (p.s_armed != 0).to_string(),
            );
            if p.s_armed != 0 {
                eq(s("s_count"), pm.s_count.to_string(), p.s_count.to_string());
                eq(s("s_since"), pm.s_since.to_string(), p.s_since.to_string());
            }
            eq(
                s("terms"),
                format!("{:?}", pm.terms),
                format!("{:?}", Some(Terms::of(p))),
            );
        }
        for (k, pm) in &self.positions {
            if pm.status == "open" && !c.positions.contains_key(k) && c.closed.contains(k) {
                eq(format!("position {k}"), "open".into(), "closed".into());
            }
        }
        for ((market, owner), l) in &c.lenders {
            let Some(lm) = self.lenders.get(&(*market, *owner)) else {
                eq(
                    format!("lender {owner}@{market}"),
                    "absent".into(),
                    "present".into(),
                );
                continue;
            };
            let s = |what: &str| format!("lender {owner}@{market} {what}");
            eq(s("shares"), lm.shares.to_string(), l.shares.to_string());
            eq(s("epoch"), lm.epoch.to_string(), l.epoch.to_string());
            eq(s("lock"), lm.lock.to_string(), l.lock.to_string());
            eq(
                s("lock_until"),
                lm.lock_until.to_string(),
                l.lock_until.to_string(),
            );
        }
        for ((market, epoch), r) in &c.share_epochs {
            let got = self.share_epochs.get(&(*market, *epoch));
            let den = 1000u128.checked_pow(u32::from(r.den_log1000)).unwrap_or(0);
            eq(
                format!("share epoch {market}#{epoch}"),
                format!("{got:?}"),
                format!("{:?}", Some((r.num, den))),
            );
        }
        out
    }
}

/// The event's own `slot` field.
fn event_slot(ev: &Ev) -> Option<u64> {
    macro_rules! s {
        ($($v:ident),*) => {
            match ev {
                $( Ev::$v(x) => Some(x.slot), )*
            }
        };
    }
    s!(
        MarketCreated,
        Deposit,
        Withdraw,
        ClaimSol,
        DeficitCovered,
        LenderSynced,
        Open,
        Close,
        Liquidate,
        Expire,
        SolSettle,
        SettleArmed,
        SettleDisarmed,
        EpochRolled,
        ProtocolFeesSwept,
        MarginAdded,
        PriceCranked,
        VenueStateChanged,
        RouteChainChanged,
        PoolProposed,
        PoolSwitched,
        PoolSwitchCancelled,
        TraderSwept,
        VenueRewardsClaimed,
        MintSafetyTripped,
        VenueUpgradeFlagged,
        VenueRevalidated,
        CoverageGap,
        CoveragePauseSet,
        ConfigProposed,
        ConfigApplied,
        ConfigCancelled,
        PauseSet,
        VenueDisabled,
        RouteBrokenSet,
        VenueExtrasSet
    )
}

/// `propose_pool`'s `candidate_kind` argument for an event `r` of `t` it emitted.
fn candidate_kind(t: &ChainTx, r: &EvRec) -> Option<u8> {
    if r.invocation != "propose_pool" {
        return None;
    }
    let inv = t
        .invocations
        .iter()
        .rev()
        .find(|v| v.ix_index == r.ix_index && v.name == "propose_pool")?;
    let args = shortwin::instruction::ProposePool::deserialize(&mut inv.data.get(8..)?).ok()?;
    Some(args.candidate_kind)
}

#[derive(Clone, Copy)]
struct FeeX {
    market: Pubkey,
    ts: i64,
    amount: u64,
    position: Option<Pubkey>,
    key: Option<Pubkey>,
    derived: bool,
}

impl FeeX {
    fn new(market: Pubkey, ts: i64, amount: u64) -> Self {
        Self {
            market,
            ts,
            amount,
            position: None,
            key: None,
            derived: false,
        }
    }
    fn position(mut self, p: Pubkey) -> Self {
        self.position = Some(p);
        self
    }
    fn key(mut self, k: Pubkey) -> Self {
        self.key = Some(k);
        self
    }
    fn derived(mut self) -> Self {
        self.derived = true;
        self
    }
}

#[derive(Clone, Copy)]
enum LenderKind {
    Deposit,
    Withdraw,
    Claim,
    Cover,
    Other,
}

#[derive(Clone, Copy)]
struct LenderFields {
    ts: i64,
    amount: u64,
    shares: u128,
    sol_paid: u64,
    pot_draw: u64,
    lock: u128,
    lock_until: i64,
    epoch: u32,
}

macro_rules! lender_fields {
    ($($f:ident => $t:ty),*) => {
        impl LenderFields {
            $(
                fn $f(x: &$t) -> Self {
                    Self {
                        ts: x.ts,
                        amount: x.amount,
                        shares: x.shares,
                        sol_paid: x.sol_paid,
                        pot_draw: x.pot_draw,
                        lock: x.lock,
                        lock_until: x.lock_until,
                        epoch: x.epoch,
                    }
                }
            )*
        }
    };
}

lender_fields!(
    of_deposit => e::Deposit,
    of_withdraw => e::Withdraw,
    of_claim => e::ClaimSol,
    of_cover => e::DeficitCovered,
    of_sync => e::LenderSynced
);

/// Runs the model over `session` checkpoint by checkpoint and returns every mismatch with the
/// chain, prefixed by the checkpoint (the contract's rules verified against the program),
/// plus every payload that did not decode.
pub fn check_session(session: &Session) -> anyhow::Result<(Model, Vec<String>)> {
    let txs = decode_session(session)?;
    let mut model = Model::new(session)?;
    let mut problems = Vec::new();
    for t in &txs {
        for u in &t.undecoded {
            problems.push(format!("tx {} ({}): undecoded {u}", t.index, t.signature));
        }
    }
    let mut next = 0usize;
    for (k, c) in &session.checkpoints {
        model.apply_all(&txs[next..*k]);
        next = *k;
        let state = ChainState::from_checkpoint(c)?;
        for p in model.check_against(&state) {
            problems.push(format!("checkpoint after tx {k}: {p}"));
        }
    }
    model.apply_all(&txs[next..]);
    problems.extend(model.problems.iter().cloned());
    problems.extend(model.terms_mismatches.iter().cloned());
    Ok((model, problems))
}
