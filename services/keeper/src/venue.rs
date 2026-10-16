//! Per-venue account lists (ARCHITECTURE §3.0 "Remaining-accounts convention", §3.12): the
//! keys of a market's `ReadSet` and `TradeExtras`, and the extras sets of A41 with the
//! placeholders resolved (ARCH §7.1 duty 10: "always select the extras set that simulates
//! successfully").
//!
//! The program key-checks every slot; this module only produces the same lists from the
//! market's caches — for Raydium through the adapters' own key functions
//! (`shortwin::venues::raydium_v4::{read_keys, trade_keys}`, the same in `raydium_cpmm`), so the
//! keeper's lists are the program's by construction. What the program does not state as data
//! is the **writability** of each slot in a trade (ARCH §3.12's "W" marks), which this module
//! adds: a slot the venue writes must be writable in the transaction (the CPI cannot escalate
//! it), and a slot it only reads stays read-only — a CPMM `AmmConfig` is shared by every pool
//! on that config, so write-locking it would contend with all of them (ARCH §6.6).
//!
//! **Pump curve** (M5; pump v3 since pump-final §6.1): a `PumpCurve` or `PumpMigrating` market's
//! ReadSet is the curve adapter's own (`shortwin::venues::pump_curve::read_keys`: C0 curve W, C1
//! `Global`, C2 pump `FeeConfig`, C3 the canonical pool, C4 the curve's base ATA W, C5 / C6
//! PumpSwap's fee and global configs), and a `PumpCurve` trade's TradeExtras T0–T9
//! ([`trade_extras`]) are the program's fixed slots (`pump_curve::fixed_trade_keys`) and T7, the
//! one slot the program checks against the **live** state read from this tick's ReadSet accounts:
//! a buyback recipient of the live `Global` (one per market, spread by the mint's first byte so
//! the keeper's exits do not all write-lock the same recipient). pump's v3 trades name no fee
//! recipient, creator vault or pump-fees account. A `PumpMigrating` market has no trade (exits
//! pause, `VenueMigrating`).
//!
//! **The upgrade window** (pump-final §7, §15 "Release coordination"): shortwin 1.0.0's curve
//! adapter trades pump v2 with the 20-key list T0–T19 ([`pump_trade_extras_v2`]: the live
//! `Global`'s fee and buyback recipients at T6 / T8, the live creator's vault at T12 / T13). A
//! Governance upgrade executes at a slot nobody chooses, so for one release the keeper carries
//! both lists ([`CurveExtrasFormat`]) and picks the one the deployed short.win parses, per
//! short.win's ProgramData deploy slot, by simulation ([`format_evidence`]; `crate::keeper`). The
//! next release deletes V2.
//!
//! **PumpSwap** (M6): the ReadSet is the adapter's own (`shortwin::venues::pump_amm::read_keys`: P0
//! the canonical pool W, P1 its base vault W, P2 its WSOL vault W, P3 `GlobalConfig`, P4 the
//! PumpSwap `FeeConfig`, P5 the boost vault's WSOL ATA, P6 the pool's LP mint — M6R-1, owner
//! decision of 2026-09-30: `PDA(pAMM, ["pool_lp_mint", pool])`, which the program key-checks
//! against the live `pool.lp_mint`), and a trade's TradeExtras T0–T18
//! ([`trade_extras`]) come from the adapter's constants and the market's PumpSwap cache, with the
//! slots the program checks against the **live** state read from this tick's ReadSet accounts: T9
//! / T16 a protocol-fee and a buyback recipient of the live `GlobalConfig` (one per market, spread
//! by the mint's first byte as on the curve), T10 / T17 their WSOL ATAs
//! (`pump_amm::recipient_wsol_ata`: the tables, else derived), T11 / T12 the coin-creator vault of
//! the live `pool.coin_creator` (the cache's when it is the cached creator, ARCH §5.4 case 4). The
//! existence requirement (T10, T11, T14, T17 and on a cashback coin T18 initialised) is the
//! keeper's duty 6 ([`pamm_account_keys`]; `crate::graduation`).
//!
//! **List-only** (`None`, M5): the program's "ReadSet" of a list-only market is its detection set
//! `[bonding_curve, canonical_pool]` (`crank_price`, which re-runs `classify_pump` on it, and
//! `sol_settle`, which runs the S3 of a curve that graduated elsewhere with positions open on it:
//! M2-SEC-5), read-only; a list-only market has no trade.
//!
//! `VenueKind::Mock` exists only with the TEST-ONLY `test-venue` feature (A49).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use anyhow::{Context, anyhow, bail};
use shortwin::state::pump_swap_cache;
use shortwin::state::{
    ENTRY_FIXED, ENTRY_TEMPLATE, EXTRAS_MAX_SEEDS, ExtraEntry, ExtrasSet, Market,
    PLACEHOLDER_BONDING_CURVE, PLACEHOLDER_MINT, PLACEHOLDER_POOL, PLACEHOLDER_TRADER, SEED_CONST,
    SEED_PLACEHOLDER, VenueExtras, VenueKind,
};
use shortwin::venues::{
    EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS, EXTRAS_SET_STABLE, ExtrasSel, pump_amm, pump_curve,
    raydium_cpmm, raydium_v4,
};
use shortwin_services_common::AccountData;
use shortwin_venue_state::{pump, pump_amm as pamm_state};

/// Which ReadSet slots are writable in a trade (ARCH §3.12), in the parser's order:
/// AMM v4 `[pool W, coin_vault W, pc_vault W, lp_mint]`, CPMM `[pool W, amm_config,
/// token_0_vault W, token_1_vault W, lp_mint]`, the pump curve `[curve W, Global, FeeConfig,
/// canonical pool, curve_base_ata W, PumpSwap FeeConfig, PumpSwap GlobalConfig]`, PumpSwap
/// `[pool W, base vault W, WSOL vault W, GlobalConfig, FeeConfig, boost vault ATA, LP mint]` (the
/// LP mint P6 since M6R-1, owner decision of 2026-09-30), the mock
/// `[pool W, base vault W, WSOL vault W]`. `crank_price` and `sol_settle` pass every slot
/// read-only.
pub fn read_writable(kind: VenueKind) -> anyhow::Result<&'static [bool]> {
    const V4: [bool; raydium_v4::READ_LEN] = [true, true, true, false];
    const CPMM: [bool; raydium_cpmm::READ_LEN] = [true, false, true, true, false];
    const CURVE: [bool; pump_curve::READ_LEN] = [true, false, false, false, true, false, false];
    const PAMM: [bool; pump_amm::READ_LEN] = [true, true, true, false, false, false, false];
    match kind {
        VenueKind::RaydiumV4 => Ok(&V4),
        VenueKind::RaydiumCpmm => Ok(&CPMM),
        VenueKind::PumpCurve | VenueKind::PumpMigrating => Ok(&CURVE),
        VenueKind::PumpSwap => Ok(&PAMM),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Ok(&[true, true, true]),
        // The detection set: read only (a list-only market never trades).
        VenueKind::None => Ok(&[false, false]),
    }
}

/// The ReadSet keys of a market on `kind`, in the parser's order.
pub fn read_set(kind: VenueKind, market: &Market) -> anyhow::Result<Vec<Pubkey>> {
    match kind {
        VenueKind::RaydiumV4 => Ok(raydium_v4::read_keys(market).to_vec()),
        VenueKind::RaydiumCpmm => Ok(raydium_cpmm::read_keys(market).to_vec()),
        // A migrating market reads its curve's ReadSet (ARCH §3.12).
        VenueKind::PumpCurve | VenueKind::PumpMigrating => {
            Ok(pump_curve::read_keys(market).to_vec())
        }
        VenueKind::PumpSwap => Ok(pump_amm::read_keys(market).to_vec()),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => {
            use shortwin::state::raydium_cache::{RAY_POOL, RAY_VAULT_SOL, RAY_VAULT_TOK};
            Ok(vec![
                market.pool_cache[RAY_POOL],
                market.pool_cache[RAY_VAULT_TOK],
                market.pool_cache[RAY_VAULT_SOL],
            ])
        }
        // The detection set (`crank_price`, `sol_settle`; module docs).
        VenueKind::None => Ok(vec![market.bonding_curve, market.canonical_pool]),
    }
}

/// The ReadSet metas of `keys` (a market's [`read_set`]) on `kind`: writable per
/// [`read_writable`] when `trading`, all read-only otherwise.
pub fn read_metas(
    kind: VenueKind,
    keys: &[Pubkey],
    trading: bool,
) -> anyhow::Result<Vec<AccountMeta>> {
    let writable = read_writable(kind)?;
    if writable.len() != keys.len() {
        bail!(
            "venue {kind:?}: {} ReadSet keys, the adapter takes {}",
            keys.len(),
            writable.len()
        );
    }
    Ok(keys
        .iter()
        .zip(writable)
        .map(|(k, w)| {
            if trading && *w {
                AccountMeta::new(*k, false)
            } else {
                AccountMeta::new_readonly(*k, false)
            }
        })
        .collect())
}

/// Which `PumpCurve` TradeExtras list a trade carries (module docs, "The upgrade window").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CurveExtrasFormat {
    /// shortwin 1.0.0's curve adapter (pump `sell_v2` / `buy_v2`): T0–T19
    /// ([`pump_trade_extras_v2`]). Kept for one release.
    V2,
    /// The v3 adapter (pump `sell_v3` / `buy_v3`, pump-final §6.1.1): T0–T9
    /// (`pump_curve::TRADE_LEN`).
    V3,
}

impl CurveExtrasFormat {
    /// shortwin 1.0.0's `pump_curve::TRADE_LEN`.
    pub const V2_TRADE_LEN: usize = 20;

    /// The list's length.
    pub const fn trade_len(self) -> usize {
        match self {
            Self::V2 => Self::V2_TRADE_LEN,
            Self::V3 => pump_curve::TRADE_LEN,
        }
    }

    /// The other format.
    pub const fn other(self) -> Self {
        match self {
            Self::V2 => Self::V3,
            Self::V3 => Self::V2,
        }
    }
}

/// What one simulated (or sent) trading exit says about the `CurveExtrasFormat` it carried
/// ([`format_evidence`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatEvidence {
    /// The deployed short.win got past account parsing (`venues::parse`, the first thing a
    /// keeper exit does after its venue and mint checks): it succeeded, or failed on a later
    /// check or inside the venue. The format is the deployed program's.
    Parsed,
    /// It failed on account parsing: `BadRemainingAccounts` (a 10-key list on 1.0.0, which takes
    /// 20) or `VenueAccountMismatch` (a 20-key list on the v3 build, whose parser takes the first
    /// 10 keys and refuses T2). The other format may be the deployed one.
    ParseFailed,
    /// Nothing is known: a failure before account parsing (an Anchor account constraint, the
    /// market's own checks: `AccountMismatch`, `ConfigVersion`, `NoRoute`, `VenueMigrating`) or
    /// without a custom error.
    Unknown,
}

/// [`FormatEvidence`] of a simulation's or transaction's outcome `out`.
pub fn format_evidence(out: &shortwin_services_common::TxOutcome) -> FormatEvidence {
    use shortwin::errors::ShortwinError as E;
    let Some(err) = out.err.as_ref() else {
        return FormatEvidence::Parsed;
    };
    // A venue's own failure (pump's 6002, a token program's): short.win parsed the lists and
    // reached the CPI.
    if crate::events::failing_program(out).is_some_and(|p| p != shortwin::ID) {
        return FormatEvidence::Parsed;
    }
    let Some(code) = err.custom_code() else {
        return FormatEvidence::Unknown;
    };
    let is = |e: E| code == u32::from(e);
    if is(E::BadRemainingAccounts) || is(E::VenueAccountMismatch) {
        FormatEvidence::ParseFailed
    } else if code < 6_000
        || is(E::AccountMismatch)
        || is(E::ConfigVersion)
        || is(E::NoRoute)
        || is(E::VenueMigrating)
    {
        FormatEvidence::Unknown
    } else {
        FormatEvidence::Parsed
    }
}

/// The TradeExtras metas of a market on `kind` (after the ReadSet in `liquidate` / `expire`);
/// `read` is this tick's ReadSet accounts (in [`read_set`]'s order), which the pump curve's
/// live-checked slots come from. A `PumpCurve` trade carries the v3 list
/// ([`CurveExtrasFormat::V3`]; [`trade_extras_as`] for the other).
pub fn trade_extras(
    kind: VenueKind,
    market: &Market,
    read: &[Option<AccountData>],
) -> anyhow::Result<Vec<AccountMeta>> {
    trade_extras_as(kind, market, read, CurveExtrasFormat::V3)
}

/// [`trade_extras`] with a `PumpCurve` trade's list in `curve` format (the other venues have one).
pub fn trade_extras_as(
    kind: VenueKind,
    market: &Market,
    read: &[Option<AccountData>],
    curve: CurveExtrasFormat,
) -> anyhow::Result<Vec<AccountMeta>> {
    let metas = |keys: &[Pubkey], writable: &[usize]| {
        keys.iter()
            .enumerate()
            .map(|(i, k)| {
                if writable.contains(&i) {
                    AccountMeta::new(*k, false)
                } else {
                    AccountMeta::new_readonly(*k, false)
                }
            })
            .collect::<Vec<_>>()
    };
    match kind {
        // X5 `trader_wsol` (W).
        VenueKind::RaydiumV4 => Ok(metas(
            &raydium_v4::trade_keys(market),
            &[raydium_v4::X_TRADER_WSOL],
        )),
        // X2 `observation` (W), X7 `trader_wsol` (W).
        VenueKind::RaydiumCpmm => Ok(metas(
            &raydium_cpmm::trade_keys(market),
            &[raydium_cpmm::X_OBSERVATION, raydium_cpmm::X_TRADER_WSOL],
        )),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => {
            use shortwin::constants::{
                MOCK_CPAMM_AUTHORITY, MOCK_CPAMM_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
            };
            Ok(vec![
                AccountMeta::new_readonly(MOCK_CPAMM_PROGRAM_ID, false),
                AccountMeta::new_readonly(MOCK_CPAMM_AUTHORITY, false),
                AccountMeta::new_readonly(WSOL_MINT, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new(market.trader_wsol, false),
            ])
        }
        VenueKind::PumpCurve => match curve {
            CurveExtrasFormat::V3 => pump_trade_extras(market, read),
            CurveExtrasFormat::V2 => pump_trade_extras_v2(market, read),
        },
        VenueKind::PumpSwap => pamm_trade_extras(market, read),
        VenueKind::PumpMigrating => bail!("a migrating curve has no trade (VenueMigrating)"),
        VenueKind::None => bail!("a list-only market has no trade (NoRoute)"),
    }
}

/// The live pump `Global` and curve of a curve market's ReadSet accounts.
fn live_curve_accounts(
    read: &[Option<AccountData>],
) -> anyhow::Result<(&AccountData, pump::BondingCurve)> {
    let acc = |i: usize| read.get(i).and_then(Option::as_ref);
    let global = acc(pump_curve::C_GLOBAL).context("pump Global not read")?;
    let curve_acc = acc(pump_curve::C_CURVE).context("bonding curve not read")?;
    let curve = pump::read_curve(&curve_acc.owner.to_bytes(), &curve_acc.data)
        .map_err(|e| anyhow!("bonding curve: {e:?}"))?;
    Ok((global, curve))
}

/// A recipient of a live `Global` / `GlobalConfig` list for `market`: the first non-default
/// entry from the mint's first byte on (every entry is one the venue accepts, C0-14).
fn recipient(list: &[[u8; 32]], market: &Market) -> anyhow::Result<Pubkey> {
    let n = list.len();
    let start = usize::from(market.mint.to_bytes()[0]);
    (0..n)
        .filter_map(|k| list.get((start + k) % n.max(1)))
        .find(|r| **r != [0u8; 32])
        .map(|r| Pubkey::new_from_array(*r))
        .context("the live list names no recipient")
}

/// A live fee recipient and buyback recipient of the curve market's `Global`.
pub fn pump_recipients(
    market: &Market,
    read: &[Option<AccountData>],
) -> anyhow::Result<(Pubkey, Pubkey)> {
    let (global, _) = live_curve_accounts(read)?;
    let g = pump::read_global(&global.owner.to_bytes(), &global.data)
        .map_err(|e| anyhow!("pump Global: {e:?}"))?;
    Ok((
        recipient(&g.fee_recipients(), market)?,
        recipient(&g.buyback_fee_recipients(), market)?,
    ))
}

/// A protocol-fee recipient and a buyback recipient of PumpSwap's live `GlobalConfig` for
/// `market` (T9 / T16; the entry from the mint's first byte on, as on the curve). `gc` is the
/// `GlobalConfig` account as read (a PumpSwap ReadSet's P3, a curve ReadSet's C6).
pub fn pamm_recipients(
    market: &Market,
    gc: Option<&AccountData>,
) -> anyhow::Result<(Pubkey, Pubkey)> {
    let gc = gc.context("PumpSwap GlobalConfig not read")?;
    let g = pamm_state::read_global_config(&gc.owner.to_bytes(), &gc.data)
        .map_err(|e| anyhow!("PumpSwap GlobalConfig: {e:?}"))?;
    Ok((
        recipient(&g.protocol_fee_recipients(), market)?,
        recipient(&g.buyback_fee_recipients(), market)?,
    ))
}

/// The coin-creator vault authority (T12) and its WSOL ATA (T11) of `coin_creator`: the market's
/// PumpSwap cache when it is the cached creator, else derived (the program's
/// `pump_amm::check_trade` accepts exactly these; ARCH §5.4 case 4).
pub fn pamm_creator_accounts(market: &Market, coin_creator: &Pubkey) -> (Pubkey, Pubkey) {
    let cached = |i: usize| pump_amm::cached(market, i);
    if *coin_creator == cached(pump_swap_cache::CACHED_COIN_CREATOR)
        && cached(pump_swap_cache::CACHED_COIN_CREATOR_VAULT_AUTH) != Pubkey::default()
    {
        (
            cached(pump_swap_cache::CACHED_COIN_CREATOR_VAULT_AUTH),
            cached(pump_swap_cache::CACHED_COIN_CREATOR_VAULT_ATA),
        )
    } else {
        let a = pump_amm::coin_creator_vault_authority(coin_creator);
        (a, pump_curve::wsol_ata(&a))
    }
}

/// M6-SEC-4: anyone can change a PumpSwap pool's coin creator (pAMM `set_coin_creator`,
/// `migrate_pool_coin_creator`: no signer), and the program's existence requirement refuses a trade
/// while the live creator's vault WSOL ATA (T11) is missing. When the live creator is not the
/// market's cached one (the change came after the market's last crank, which re-caches it), the
/// keeper's exit carries `create_idempotent(T11)` paid by `payer` in its own transaction, so it
/// does not depend on account preparation landing first. `None` off PumpSwap, for the cached
/// creator, or when the pool does not parse.
pub fn pamm_creator_vault_prep(
    payer: &Pubkey,
    market: &Market,
    kind: VenueKind,
    read: &[Option<AccountData>],
) -> Option<anchor_lang::solana_program::instruction::Instruction> {
    if kind != VenueKind::PumpSwap {
        return None;
    }
    let pool = pamm_pool(read).ok()?;
    let live = Pubkey::new_from_array(pool.coin_creator);
    if live == pump_amm::cached(market, pump_swap_cache::CACHED_COIN_CREATOR) {
        return None;
    }
    let (auth, _) = pamm_creator_accounts(market, &live);
    Some(crate::graduation::create_ata_idempotent_ix(
        payer,
        &auth,
        &shortwin::constants::WSOL_MINT,
        &shortwin::constants::TOKEN_PROGRAM_ID,
    ))
}

/// The live canonical pool of a PumpSwap market's ReadSet accounts (P0), parsed.
pub fn pamm_pool(read: &[Option<AccountData>]) -> anyhow::Result<pamm_state::Pool> {
    let acc = read
        .get(pump_amm::P_POOL)
        .and_then(Option::as_ref)
        .context("PumpSwap pool not read")?;
    pamm_state::read_pool(&acc.owner.to_bytes(), &acc.data)
        .map_err(|e| anyhow!("PumpSwap pool: {e:?}"))
}

/// What a PumpSwap market's trades need to exist (the program refuses `VenueAccountMismatch` /
/// `TraderStateInvalid` otherwise): `(key, what)` for T7 the trader's base ATA (a pump listing
/// creates it; a list-only listing re-detected onto its pool does not), then ARCH §3.12's
/// existence requirement — T14 the trader's pAMM UVA, T11 the coin-creator vault's WSOL ATA of
/// `coin_creator`, T10 / T17 the WSOL ATAs of the recipients the keeper names, and on a cashback
/// coin T18 `ATA(pAMM UVA(trader), WSOL)`. `create_market` of a PumpSwap listing creates the base
/// ATA and the UVA; nothing creates the others for the trader, and a graduation creates none
/// (duty 6).
pub fn pamm_account_keys(
    market: &Market,
    coin_creator: &Pubkey,
    recipients: (Pubkey, Pubkey),
    cashback: bool,
) -> Vec<(Pubkey, PammAccount)> {
    use shortwin::constants::{TOKEN_PROGRAM_ID, WSOL_MINT};
    let (auth, auth_ata) = pamm_creator_accounts(market, coin_creator);
    let uva = uva_of(market);
    let wsol = |owner: Pubkey| PammAccount::Ata {
        owner,
        mint: WSOL_MINT,
        token_program: TOKEN_PROGRAM_ID,
    };
    let mut v = vec![
        (
            market.trader_base_ata,
            PammAccount::Ata {
                owner: market.trader,
                mint: market.mint,
                token_program: market.token_program,
            },
        ),
        (uva, PammAccount::Uva),
        (auth_ata, wsol(auth)),
        (
            pump_amm::recipient_wsol_ata(&recipients.0),
            wsol(recipients.0),
        ),
        (
            pump_amm::recipient_wsol_ata(&recipients.1),
            wsol(recipients.1),
        ),
    ];
    if cashback {
        v.push((pump_curve::wsol_ata(&uva), wsol(uva)));
    }
    v
}

/// The trader's pAMM UVA of `market` (T14): the PumpSwap cache, else derived.
pub fn uva_of(market: &Market) -> Pubkey {
    let c = pump_amm::cached(market, pump_swap_cache::PAMM_UVA);
    if c == Pubkey::default() {
        pump_amm::user_volume_accumulator(&market.trader)
    } else {
        c
    }
}

/// One account of [`pamm_account_keys`] and how it is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PammAccount {
    /// PumpSwap `init_user_volume_accumulator(payer, user = trader)`; owned by PumpSwap.
    Uva,
    /// `create_idempotent(payer, owner, mint)` under `token_program`, which owns it.
    Ata {
        owner: Pubkey,
        mint: Pubkey,
        token_program: Pubkey,
    },
}

impl PammAccount {
    /// The program that owns the account once it exists.
    pub fn owner_program(&self) -> Pubkey {
        match self {
            Self::Uva => shortwin::constants::PUMP_AMM_PROGRAM_ID,
            Self::Ata { token_program, .. } => *token_program,
        }
    }
}

/// A `PumpSwap` trade's TradeExtras T0–T18 (ARCH §3.12; module docs): W at T7, T8, T10, T11,
/// T14, T17, T18.
fn pamm_trade_extras(
    market: &Market,
    read: &[Option<AccountData>],
) -> anyhow::Result<Vec<AccountMeta>> {
    use shortwin::constants::{
        ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR,
        PUMP_AMM_PROGRAM_ID, PUMP_FEES_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
    };
    let pool = pamm_pool(read)?;
    let (protocol, buyback) = pamm_recipients(
        market,
        read.get(pump_amm::P_GLOBAL_CONFIG).and_then(Option::as_ref),
    )?;
    let (auth, auth_ata) =
        pamm_creator_accounts(market, &Pubkey::new_from_array(pool.coin_creator));
    let uva = uva_of(market);
    let cached = |i: usize| pump_amm::cached(market, i);
    let marker = match cached(pump_swap_cache::POOL_V2_MARKER) {
        k if k == Pubkey::default() => pump_amm::pool_v2(&market.mint),
        k => k,
    };
    let cashback_ata = match cached(pump_swap_cache::PAMM_UVA_QUOTE_ATA) {
        k if k == Pubkey::default() => pump_curve::wsol_ata(&uva),
        k => k,
    };
    let keys: [(Pubkey, bool); pump_amm::TRADE_LEN] = [
        (PUMP_AMM_PROGRAM_ID, false),
        (PUMP_AMM_EVENT_AUTHORITY, false),
        (PUMP_FEES_PROGRAM_ID, false),
        (WSOL_MINT, false),
        (TOKEN_PROGRAM_ID, false),
        (ASSOCIATED_TOKEN_PROGRAM_ID, false),
        (anchor_lang::system_program::ID, false),
        (market.trader_base_ata, true),
        (market.trader_wsol, true),
        (protocol, false),
        (pump_amm::recipient_wsol_ata(&protocol), true),
        (auth_ata, true),
        (auth, false),
        (PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, false),
        (uva, true),
        (marker, false),
        (buyback, false),
        (pump_amm::recipient_wsol_ata(&buyback), true),
        (cashback_ata, true),
    ];
    Ok(keys
        .into_iter()
        .map(|(k, w)| {
            if w {
                AccountMeta::new(k, false)
            } else {
                AccountMeta::new_readonly(k, false)
            }
        })
        .collect())
}

/// A `PumpCurve` trade's TradeExtras T0–T9 for pump's v3 trades (pump-final §6.1.1, ARCH §3.12;
/// module docs): W at T4–T8. T7 is a buyback recipient of the live `Global` (the selection rule
/// of [`pump_recipients`]); v3 names no fee recipient, creator vault or pump-fees account.
fn pump_trade_extras(
    market: &Market,
    read: &[Option<AccountData>],
) -> anyhow::Result<Vec<AccountMeta>> {
    use shortwin::constants::{PUMP_EVENT_AUTHORITY, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT};
    let (_, buyback) = pump_recipients(market, read)?;
    let keys: [(Pubkey, bool); pump_curve::TRADE_LEN] = [
        (PUMP_PROGRAM_ID, false),
        (PUMP_EVENT_AUTHORITY, false),
        (WSOL_MINT, false),
        (TOKEN_PROGRAM_ID, false),
        (market.curve_quote_ata, true),
        (pump_curve::trader_wsol_ata(market), true),
        (market.pump_uva, true),
        (buyback, true),
        (market.trader_base_ata, true),
        (anchor_lang::system_program::ID, false),
    ];
    Ok(keys
        .into_iter()
        .map(|(k, w)| {
            if w {
                AccountMeta::new(k, false)
            } else {
                AccountMeta::new_readonly(k, false)
            }
        })
        .collect())
}

/// shortwin 1.0.0's `PumpCurve` TradeExtras T0–T19 for pump's v2 trades (ARCH §3.12 as audited;
/// module docs, "The upgrade window"), W at T6–T13 and T16–T18: T6 / T8 a fee and a buyback
/// recipient of the live `Global` ([`pump_recipients`]), T12 / T13 the creator vault of the live
/// `curve.creator` (the cache's when it is the cached creator, R-PUMP §8) and its WSOL ATA, the
/// rest the market's curve cache and pump's constants — the list the keeper sent before the v3
/// adapter, kept for one release (pump-final §7).
pub fn pump_trade_extras_v2(
    market: &Market,
    read: &[Option<AccountData>],
) -> anyhow::Result<Vec<AccountMeta>> {
    use shortwin::constants::{
        ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_FEES_PROGRAM_ID,
        PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
    };
    let (fee_recipient, buyback) = pump_recipients(market, read)?;
    let (_, curve) = live_curve_accounts(read)?;
    let creator = Pubkey::new_from_array(curve.creator);
    let vault = if creator == market.cached_creator {
        market.cached_creator_vault
    } else {
        pump_curve::creator_vault(&creator)
    };
    let keys: [(Pubkey, bool); CurveExtrasFormat::V2_TRADE_LEN] = [
        (PUMP_PROGRAM_ID, false),
        (PUMP_EVENT_AUTHORITY, false),
        (PUMP_FEES_PROGRAM_ID, false),
        (WSOL_MINT, false),
        (TOKEN_PROGRAM_ID, false),
        (ASSOCIATED_TOKEN_PROGRAM_ID, false),
        (fee_recipient, true),
        (pump_curve::wsol_ata(&fee_recipient), true),
        (buyback, true),
        (pump_curve::wsol_ata(&buyback), true),
        (market.curve_quote_ata, true),
        (pump_curve::wsol_ata(&market.trader), true),
        (vault, true),
        (pump_curve::wsol_ata(&vault), true),
        (market.sharing_config, false),
        (PUMP_GLOBAL_VOLUME_ACCUMULATOR, false),
        (market.pump_uva, true),
        (market.pump_uva_quote_ata, true),
        (market.trader_base_ata, true),
        (anchor_lang::system_program::ID, false),
    ];
    Ok(keys
        .into_iter()
        .map(|(k, w)| {
            if w {
                AccountMeta::new(k, false)
            } else {
                AccountMeta::new_readonly(k, false)
            }
        })
        .collect())
}

/// The pool a template's `pool` placeholder stands for (`venues::pool_key`).
fn pool_placeholder(kind: VenueKind, market: &Market) -> Option<Pubkey> {
    shortwin::venues::pool_key(market, kind)
}

/// One selectable extras set of a trading instruction: the selection and the trailing metas
/// (the `VenueExtras` account first, then its entries), empty for `none`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtrasChoice {
    pub sel: ExtrasSel,
    pub metas: Vec<AccountMeta>,
}

impl ExtrasChoice {
    /// `none`: nothing follows the adapter's list.
    pub fn none() -> Self {
        Self {
            sel: ExtrasSel::NONE,
            metas: Vec::new(),
        }
    }
}

fn resolve_entry(entry: &ExtraEntry, kind: VenueKind, market: &Market) -> Option<(Pubkey, u8)> {
    match entry.kind {
        ENTRY_FIXED => Some((entry.key, 0)),
        ENTRY_TEMPLATE => {
            let n = usize::from(entry.n_seeds);
            if n == 0 || n > EXTRAS_MAX_SEEDS {
                return None;
            }
            let mut owned: Vec<Vec<u8>> = Vec::with_capacity(n);
            for part in entry.seeds.iter().take(n) {
                match part.kind {
                    SEED_CONST => owned.push(part.bytes.get(..usize::from(part.len))?.to_vec()),
                    SEED_PLACEHOLDER => {
                        let key = match part.len {
                            PLACEHOLDER_MINT => Some(market.mint),
                            PLACEHOLDER_POOL => pool_placeholder(kind, market),
                            PLACEHOLDER_TRADER => Some(market.trader),
                            PLACEHOLDER_BONDING_CURVE => Some(market.bonding_curve),
                            // The coin creator is not resolvable from the market cache.
                            _ => None,
                        }?;
                        owned.push(key.to_bytes().to_vec());
                    }
                    _ => return None,
                }
            }
            let seeds: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
            Some(Pubkey::find_program_address(&seeds, &entry.key))
        }
        _ => None,
    }
}

fn choice_of(
    set: &ExtrasSet,
    which: u8,
    extras_key: Pubkey,
    kind: VenueKind,
    market: &Market,
) -> Option<ExtrasChoice> {
    let n = usize::from(set.count);
    if n == 0 {
        return None;
    }
    let mut metas = vec![AccountMeta::new_readonly(extras_key, false)];
    let mut bumps = [0u8; 4];
    for (i, entry) in set.entries.iter().take(n).enumerate() {
        let (key, bump) = resolve_entry(entry, kind, market)?;
        if let Some(b) = bumps.get_mut(i) {
            *b = bump;
        }
        metas.push(AccountMeta {
            pubkey: key,
            is_signer: false,
            is_writable: entry.writable == 1,
        });
    }
    Some(ExtrasChoice {
        sel: ExtrasSel { set: which, bumps },
        metas,
    })
}

/// The extras sets to try for instruction kind `ix` (`EXTRAS_IX_*`), in order: `current`
/// (the Emergency's latest), `none`, `previous`, `stable` (M2-SEC-2: the set that stood for a
/// week, or Governance's last) — a set with no entries is only ever `none`. A set whose
/// template cannot be resolved from the market is left out.
pub fn extras_candidates(
    kind: VenueKind,
    ix: usize,
    market: &Market,
    extras: Option<&VenueExtras>,
) -> Vec<ExtrasChoice> {
    let mut out = Vec::with_capacity(4);
    let (Some(ve), Some(seed)) = (extras, kind.extras_seed()) else {
        return vec![ExtrasChoice::none()];
    };
    let key = crate::pda::venue_extras(seed);
    let Some(ixe) = ve.sets.get(ix) else {
        return vec![ExtrasChoice::none()];
    };
    if let Some(c) = choice_of(&ixe.current, EXTRAS_SET_CURRENT, key, kind, market) {
        out.push(c);
    }
    out.push(ExtrasChoice::none());
    if let Some(c) = choice_of(&ixe.previous, EXTRAS_SET_PREVIOUS, key, kind, market) {
        out.push(c);
    }
    if let Some(c) = choice_of(&ixe.stable, EXTRAS_SET_STABLE, key, kind, market) {
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shortwin::state::raydium_cache::{
        RAY_AMM_CONFIG, RAY_LP_MINT, RAY_OBSERVATION, RAY_POOL, RAY_VAULT_SOL, RAY_VAULT_TOK,
    };

    fn market(sol_first: bool, token_program: Pubkey) -> Market {
        let mut m: Market = bytemuck::Zeroable::zeroed();
        for i in [
            RAY_POOL,
            RAY_VAULT_SOL,
            RAY_VAULT_TOK,
            RAY_AMM_CONFIG,
            RAY_OBSERVATION,
            RAY_LP_MINT,
        ] {
            m.pool_cache[i] = Pubkey::new_unique();
        }
        m.trader_wsol = Pubkey::new_unique();
        m.token_program = token_program;
        if sol_first {
            m.flags |= shortwin::state::MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN;
        }
        m
    }

    fn flags(metas: &[AccountMeta]) -> Vec<bool> {
        metas.iter().map(|m| m.is_writable).collect()
    }

    /// ARCH §3.12: the keeper's lists are the adapters' own keys, with the "W" marks of the
    /// table, and no signer anywhere.
    #[test]
    fn raydium_lists_are_the_adapters_with_archs_writability() {
        use shortwin::constants::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
        for sol_first in [false, true] {
            for program in [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
                let m = market(sol_first, program);
                // AMM v4: pool, coin vault, pc vault (WSOL side per the flag), LP mint.
                let keys = read_set(VenueKind::RaydiumV4, &m).unwrap();
                assert_eq!(keys, raydium_v4::read_keys(&m).to_vec());
                let (sol, tok) = (m.pool_cache[RAY_VAULT_SOL], m.pool_cache[RAY_VAULT_TOK]);
                let (coin, pc) = if sol_first { (sol, tok) } else { (tok, sol) };
                assert_eq!(
                    keys,
                    vec![m.pool_cache[RAY_POOL], coin, pc, m.pool_cache[RAY_LP_MINT]]
                );
                let trade = read_metas(VenueKind::RaydiumV4, &keys, true).unwrap();
                assert_eq!(flags(&trade), [true, true, true, false]);
                let crank = read_metas(VenueKind::RaydiumV4, &keys, false).unwrap();
                assert!(crank.iter().all(|m| !m.is_writable));
                let x = trade_extras(VenueKind::RaydiumV4, &m, &[]).unwrap();
                assert_eq!(
                    x.iter().map(|m| m.pubkey).collect::<Vec<_>>(),
                    raydium_v4::trade_keys(&m).to_vec()
                );
                assert_eq!(flags(&x), [false, false, false, false, false, true]);
                assert_eq!(x[raydium_v4::X_TRADER_WSOL].pubkey, m.trader_wsol);

                // CPMM: pool, config, token 0 vault, token 1 vault, LP mint.
                let keys = read_set(VenueKind::RaydiumCpmm, &m).unwrap();
                assert_eq!(keys, raydium_cpmm::read_keys(&m).to_vec());
                let (v0, v1) = if sol_first { (sol, tok) } else { (tok, sol) };
                assert_eq!(
                    keys,
                    vec![
                        m.pool_cache[RAY_POOL],
                        m.pool_cache[RAY_AMM_CONFIG],
                        v0,
                        v1,
                        m.pool_cache[RAY_LP_MINT]
                    ]
                );
                let trade = read_metas(VenueKind::RaydiumCpmm, &keys, true).unwrap();
                assert_eq!(flags(&trade), [true, false, true, true, false]);
                let x = trade_extras(VenueKind::RaydiumCpmm, &m, &[]).unwrap();
                assert_eq!(
                    x.iter().map(|m| m.pubkey).collect::<Vec<_>>(),
                    raydium_cpmm::trade_keys(&m).to_vec()
                );
                assert_eq!(
                    flags(&x),
                    [false, false, true, false, false, false, false, true]
                );
                assert_eq!(
                    x[raydium_cpmm::X_OBSERVATION].pubkey,
                    m.pool_cache[RAY_OBSERVATION]
                );
                assert_eq!(x[raydium_cpmm::X_TOKEN_SIDE_PROGRAM].pubkey, program);
                for meta in trade.iter().chain(&x) {
                    assert!(!meta.is_signer);
                }
            }
        }
    }

    #[test]
    fn venues_without_a_trade_are_refused_with_a_reason() {
        let m = market(false, Pubkey::new_unique());
        // A PumpSwap trade needs the live pool and GlobalConfig (the recipients, the creator).
        assert!(trade_extras(VenueKind::PumpSwap, &m, &[]).is_err());
        assert!(trade_extras(VenueKind::PumpMigrating, &m, &[]).is_err());
        assert!(trade_extras(VenueKind::None, &m, &[]).is_err());
        // A ReadSet of the wrong length is refused, never padded.
        assert!(read_metas(VenueKind::RaydiumV4, &[Pubkey::new_unique()], true).is_err());
        assert!(read_metas(VenueKind::PumpSwap, &[Pubkey::new_unique()], true).is_err());
    }

    /// A PumpSwap `GlobalConfig` of 949 bytes with `protocol` / `buyback` as its recipient lists
    /// (the reader's offsets @57 and @643).
    fn pamm_global_config(protocol: [Pubkey; 8], buyback: [Pubkey; 8]) -> AccountData {
        let mut d = vec![0u8; pamm_state::GLOBAL_CONFIG_LEN];
        d[..8].copy_from_slice(&pamm_state::GLOBAL_CONFIG_DISCRIMINATOR);
        for (i, k) in protocol.iter().enumerate() {
            let o = pamm_state::OFF_GC_PROTOCOL_FEE_RECIPIENTS + 32 * i;
            d[o..o + 32].copy_from_slice(k.as_ref());
        }
        for (i, k) in buyback.iter().enumerate() {
            let o = pamm_state::OFF_GC_BUYBACK_FEE_RECIPIENTS + 32 * i;
            d[o..o + 32].copy_from_slice(k.as_ref());
        }
        AccountData {
            lamports: 1,
            owner: shortwin::constants::PUMP_AMM_PROGRAM_ID,
            executable: false,
            data: d,
        }
    }

    /// A canonical 301-byte PumpSwap pool of `mint` with `coin_creator` and the cashback flag.
    fn pamm_pool_account(mint: &Pubkey, coin_creator: &Pubkey, cashback: bool) -> AccountData {
        let mut d = vec![0u8; pamm_state::POOL_LEN];
        d[..8].copy_from_slice(&pamm_state::POOL_DISCRIMINATOR);
        d[pamm_state::OFF_BASE_MINT..pamm_state::OFF_BASE_MINT + 32].copy_from_slice(mint.as_ref());
        d[pamm_state::OFF_QUOTE_MINT..pamm_state::OFF_QUOTE_MINT + 32]
            .copy_from_slice(shortwin::constants::WSOL_MINT.as_ref());
        d[pamm_state::OFF_COIN_CREATOR..pamm_state::OFF_COIN_CREATOR + 32]
            .copy_from_slice(coin_creator.as_ref());
        d[pamm_state::OFF_IS_CASHBACK_COIN] = u8::from(cashback);
        AccountData {
            lamports: 1,
            owner: shortwin::constants::PUMP_AMM_PROGRAM_ID,
            executable: false,
            data: d,
        }
    }

    /// ARCH §3.12 PumpSwap: the ReadSet is the adapter's (`pump_amm::read_keys`), P0–P2 writable
    /// in trades; the TradeExtras are T0–T18 with the live `GlobalConfig`'s recipients (the
    /// mint's first byte picks the entry), their WSOL ATAs (the adapter's table or derived), the
    /// live coin creator's vault (the cache's when it is the cached creator), the cached UVA,
    /// marker and cashback ATA, writable at T7, T8, T10, T11, T14, T17, T18; the existence
    /// requirement's list names T14, T11, T10, T17 and on a cashback coin T18.
    #[test]
    fn pump_swap_lists_are_the_adapters_with_the_live_recipients() {
        use shortwin::constants::{PUMP_AMM_PROGRAM_ID, TOKEN_PROGRAM_ID};
        // The pool cache is a union: a PumpSwap market's holds only PumpSwap entries.
        let mut m: Market = bytemuck::Zeroable::zeroed();
        m.token_program = TOKEN_PROGRAM_ID;
        m.trader_wsol = Pubkey::new_unique();
        m.mint = Pubkey::new_from_array([5u8; 32]);
        m.canonical_pool = Pubkey::new_unique();
        m.trader = Pubkey::new_unique();
        m.trader_base_ata = Pubkey::new_unique();
        let cached_creator = Pubkey::new_unique();
        pump_amm::precompute(&mut m, &cached_creator).unwrap();
        let keys = read_set(VenueKind::PumpSwap, &m).unwrap();
        assert_eq!(keys, pump_amm::read_keys(&m).to_vec());
        assert_eq!(keys[pump_amm::P_POOL], m.canonical_pool);
        assert_eq!(
            keys[pump_amm::P_LP_MINT],
            pump_amm::lp_mint(&m.canonical_pool)
        );
        let metas = read_metas(VenueKind::PumpSwap, &keys, true).unwrap();
        assert_eq!(
            flags(&metas),
            [true, true, true, false, false, false, false]
        );
        let protocol: [Pubkey; 8] = core::array::from_fn(|_| Pubkey::new_unique());
        let buyback: [Pubkey; 8] = core::array::from_fn(|_| Pubkey::new_unique());
        for (creator, cashback) in [(cached_creator, false), (Pubkey::new_unique(), true)] {
            let mut read: Vec<Option<AccountData>> = vec![None; pump_amm::READ_LEN];
            read[pump_amm::P_POOL] = Some(pamm_pool_account(&m.mint, &creator, cashback));
            read[pump_amm::P_GLOBAL_CONFIG] = Some(pamm_global_config(protocol, buyback));
            let x = trade_extras(VenueKind::PumpSwap, &m, &read).unwrap();
            assert_eq!(x.len(), pump_amm::TRADE_LEN);
            assert_eq!(x[pump_amm::T_PROGRAM].pubkey, PUMP_AMM_PROGRAM_ID);
            // The mint's first byte (5) picks entry 5 of each list.
            assert_eq!(x[pump_amm::T_PROTOCOL_FEE_RECIPIENT].pubkey, protocol[5]);
            assert_eq!(x[pump_amm::T_BUYBACK_RECIPIENT].pubkey, buyback[5]);
            assert_eq!(
                x[pump_amm::T_PROTOCOL_FEE_RECIPIENT_ATA].pubkey,
                pump_amm::recipient_wsol_ata(&protocol[5])
            );
            assert_eq!(
                x[pump_amm::T_BUYBACK_RECIPIENT_ATA].pubkey,
                pump_amm::recipient_wsol_ata(&buyback[5])
            );
            let auth = pump_amm::coin_creator_vault_authority(&creator);
            assert_eq!(x[pump_amm::T_COIN_CREATOR_VAULT_AUTHORITY].pubkey, auth);
            assert_eq!(
                x[pump_amm::T_COIN_CREATOR_VAULT_ATA].pubkey,
                pump_curve::wsol_ata(&auth)
            );
            let uva = pump_amm::user_volume_accumulator(&m.trader);
            assert_eq!(x[pump_amm::T_UVA].pubkey, uva);
            assert_eq!(
                x[pump_amm::T_CASHBACK_ATA].pubkey,
                pump_curve::wsol_ata(&uva)
            );
            assert_eq!(x[pump_amm::T_POOL_V2].pubkey, pump_amm::pool_v2(&m.mint));
            assert_eq!(x[pump_amm::T_TRADER_BASE_ATA].pubkey, m.trader_base_ata);
            assert_eq!(x[pump_amm::T_TRADER_WSOL].pubkey, m.trader_wsol);
            let w: Vec<usize> = x
                .iter()
                .enumerate()
                .filter(|(_, a)| a.is_writable)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(w, [7, 8, 10, 11, 14, 17, 18]);
            assert!(x.iter().all(|a| !a.is_signer));
            // The existence requirement's accounts.
            let need = pamm_account_keys(&m, &creator, (protocol[5], buyback[5]), cashback);
            let need_keys: Vec<Pubkey> = need.iter().map(|(k, _)| *k).collect();
            let mut want = vec![
                x[pump_amm::T_TRADER_BASE_ATA].pubkey,
                x[pump_amm::T_UVA].pubkey,
                x[pump_amm::T_COIN_CREATOR_VAULT_ATA].pubkey,
                x[pump_amm::T_PROTOCOL_FEE_RECIPIENT_ATA].pubkey,
                x[pump_amm::T_BUYBACK_RECIPIENT_ATA].pubkey,
            ];
            if cashback {
                want.push(x[pump_amm::T_CASHBACK_ATA].pubkey);
            }
            assert_eq!(need_keys, want);
            assert_eq!(need[1].1, PammAccount::Uva);
            assert_eq!(
                need[2].1,
                PammAccount::Ata {
                    owner: auth,
                    mint: shortwin::constants::WSOL_MINT,
                    token_program: TOKEN_PROGRAM_ID
                }
            );
        }
        // No GlobalConfig read: no trade.
        let mut read: Vec<Option<AccountData>> = vec![None; pump_amm::READ_LEN];
        read[pump_amm::P_POOL] = Some(pamm_pool_account(&m.mint, &cached_creator, false));
        assert!(trade_extras(VenueKind::PumpSwap, &m, &read).is_err());
    }

    /// A list-only market's ReadSet is its detection set, read-only even in a trade's position
    /// (the program's `crank_price` / `sol_settle` on a `None` market, M2-SEC-5).
    #[test]
    fn a_list_only_market_reads_its_detection_set() {
        let mut m = market(false, Pubkey::new_unique());
        m.bonding_curve = Pubkey::new_unique();
        m.canonical_pool = Pubkey::new_unique();
        let keys = read_set(VenueKind::None, &m).unwrap();
        assert_eq!(keys, vec![m.bonding_curve, m.canonical_pool]);
        assert_eq!(keys.len(), shortwin::venues::DETECTION_LEN);
        for trading in [false, true] {
            let metas = read_metas(VenueKind::None, &keys, trading).unwrap();
            assert!(metas.iter().all(|a| !a.is_writable && !a.is_signer));
        }
    }

    /// A pump `Global` of 1,087 bytes with `fee` / `buyback` as its recipient lists (the
    /// reader's offsets: `fee_recipient` @41, `fee_recipients[7]` @162, buyback @741).
    fn global(fee: [Pubkey; 8], buyback: [Pubkey; 8]) -> AccountData {
        let mut d = vec![0u8; 1_087];
        d[..8].copy_from_slice(&[0xa7, 0xe8, 0xe8, 0xb1, 0xc8, 0x6c, 0x72, 0x7f]);
        d[41..73].copy_from_slice(fee[0].as_ref());
        for (i, k) in fee[1..].iter().enumerate() {
            d[162 + 32 * i..194 + 32 * i].copy_from_slice(k.as_ref());
        }
        for (i, k) in buyback.iter().enumerate() {
            d[741 + 32 * i..773 + 32 * i].copy_from_slice(k.as_ref());
        }
        AccountData {
            lamports: 1,
            owner: shortwin::constants::PUMP_PROGRAM_ID,
            executable: false,
            data: d,
        }
    }

    /// A live 151-byte curve with `creator`.
    fn curve(creator: Pubkey) -> AccountData {
        let mut d = vec![0u8; 151];
        d[..8].copy_from_slice(&pump::BONDING_CURVE_DISCRIMINATOR);
        d[8..16].copy_from_slice(&1_000_000u64.to_le_bytes());
        d[16..24].copy_from_slice(&1_000u64.to_le_bytes());
        d[24..32].copy_from_slice(&500_000u64.to_le_bytes());
        d[49..81].copy_from_slice(creator.as_ref());
        AccountData {
            lamports: 1,
            owner: shortwin::constants::PUMP_PROGRAM_ID,
            executable: false,
            data: d,
        }
    }

    /// ARCH §3.12 PumpCurve (pump-final §6.1.1): the ReadSet is the adapter's, C0 and C4
    /// writable in trades; the TradeExtras are the v3 list T0–T9 with the live `Global`'s
    /// buyback recipient and the cached curve keys, writable at T4–T8, the same whoever the
    /// curve's creator is (v3 names no creator vault).
    #[test]
    fn pump_curve_lists_are_the_adapters_with_the_live_recipients() {
        use shortwin::constants::PUMP_PROGRAM_ID;
        let mut m = market(false, Pubkey::new_unique());
        m.bonding_curve = Pubkey::new_unique();
        m.canonical_pool = Pubkey::new_unique();
        m.trader = Pubkey::new_unique();
        m.trader_base_ata = Pubkey::new_unique();
        m.mint = Pubkey::new_from_array([3u8; 32]);
        let cached = Pubkey::new_unique();
        pump_curve::write_cache(&mut m, &cached);
        for kind in [VenueKind::PumpCurve, VenueKind::PumpMigrating] {
            assert_eq!(
                read_set(kind, &m).unwrap(),
                pump_curve::read_keys(&m).to_vec()
            );
            let metas = read_metas(kind, &read_set(kind, &m).unwrap(), true).unwrap();
            assert_eq!(
                flags(&metas),
                [true, false, false, false, true, false, false]
            );
        }
        let fee: [Pubkey; 8] = core::array::from_fn(|_| Pubkey::new_unique());
        let bb: [Pubkey; 8] = core::array::from_fn(|_| Pubkey::new_unique());
        let mut lists = Vec::new();
        for creator in [cached, Pubkey::new_unique()] {
            let mut read: Vec<Option<AccountData>> = vec![None; pump_curve::READ_LEN];
            read[pump_curve::C_CURVE] = Some(curve(creator));
            read[pump_curve::C_GLOBAL] = Some(global(fee, bb));
            let x = trade_extras(VenueKind::PumpCurve, &m, &read).unwrap();
            assert_eq!(x.len(), pump_curve::TRADE_LEN);
            assert_eq!(x.len(), 10);
            assert_eq!(x[pump_curve::T_PROGRAM].pubkey, PUMP_PROGRAM_ID);
            // The mint's first byte (3) picks entry 3 of the buyback list.
            assert_eq!(x[pump_curve::T_BUYBACK_RECIPIENT].pubkey, bb[3]);
            assert_eq!(x[pump_curve::T_CURVE_QUOTE_ATA].pubkey, m.curve_quote_ata);
            assert_eq!(x[pump_curve::T_UVA].pubkey, m.pump_uva);
            assert_eq!(x[pump_curve::T_TRADER_BASE_ATA].pubkey, m.trader_base_ata);
            assert_eq!(
                x[pump_curve::T_TRADER_WSOL_ATA].pubkey,
                pump_curve::wsol_ata(&m.trader)
            );
            // The program's own fixed slots, key for key.
            for (i, key) in pump_curve::fixed_trade_keys(&m) {
                assert_eq!(x[i].pubkey, key, "T{i}");
            }
            let w: Vec<usize> = x
                .iter()
                .enumerate()
                .filter(|(_, a)| a.is_writable)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(w, [4, 5, 6, 7, 8]);
            assert!(x.iter().all(|a| !a.is_signer));
            lists.push(x);
        }
        assert_eq!(lists[0], lists[1], "v3 names no creator vault");
        assert_eq!(
            lists[0],
            trade_extras_as(
                VenueKind::PumpCurve,
                &m,
                &{
                    let mut read: Vec<Option<AccountData>> = vec![None; pump_curve::READ_LEN];
                    read[pump_curve::C_CURVE] = Some(curve(cached));
                    read[pump_curve::C_GLOBAL] = Some(global(fee, bb));
                    read
                },
                CurveExtrasFormat::V3
            )
            .unwrap(),
            "trade_extras is the V3 list"
        );
        // No Global read: no trade.
        assert!(trade_extras(VenueKind::PumpCurve, &m, &[]).is_err());
    }

    /// pump-final §7, the upgrade window: the V2 list is shortwin 1.0.0's T0–T19 — the live
    /// `Global`'s fee and buyback recipients (the mint's first byte picks the entry) with their WSOL
    /// ATAs, the live creator's vault (the cache's when it is the cached creator, else derived) and
    /// its WSOL ATA, the cached curve keys — writable at T6–T13 and T16–T18, no signer; the other
    /// venues have one list whatever the format.
    #[test]
    fn the_v2_curve_list_is_shortwin_1_0_0s() {
        use shortwin::constants::{
            ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_FEES_PROGRAM_ID,
            PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
        };
        let mut m = market(false, Pubkey::new_unique());
        m.bonding_curve = Pubkey::new_unique();
        m.canonical_pool = Pubkey::new_unique();
        m.trader = Pubkey::new_unique();
        m.trader_base_ata = Pubkey::new_unique();
        m.mint = Pubkey::new_from_array([3u8; 32]);
        let cached = Pubkey::new_unique();
        pump_curve::write_cache(&mut m, &cached);
        let fee: [Pubkey; 8] = core::array::from_fn(|_| Pubkey::new_unique());
        let bb: [Pubkey; 8] = core::array::from_fn(|_| Pubkey::new_unique());
        for (creator, vault) in [
            (cached, m.cached_creator_vault),
            (Pubkey::new_unique(), Pubkey::default()),
        ] {
            let mut read: Vec<Option<AccountData>> = vec![None; pump_curve::READ_LEN];
            read[pump_curve::C_CURVE] = Some(curve(creator));
            read[pump_curve::C_GLOBAL] = Some(global(fee, bb));
            let x =
                trade_extras_as(VenueKind::PumpCurve, &m, &read, CurveExtrasFormat::V2).unwrap();
            assert_eq!(x.len(), CurveExtrasFormat::V2.trade_len());
            assert_eq!(x.len(), 20);
            let vault = if vault == Pubkey::default() {
                pump_curve::creator_vault(&creator)
            } else {
                vault
            };
            let want = [
                PUMP_PROGRAM_ID,
                PUMP_EVENT_AUTHORITY,
                PUMP_FEES_PROGRAM_ID,
                WSOL_MINT,
                TOKEN_PROGRAM_ID,
                ASSOCIATED_TOKEN_PROGRAM_ID,
                fee[3],
                pump_curve::wsol_ata(&fee[3]),
                bb[3],
                pump_curve::wsol_ata(&bb[3]),
                m.curve_quote_ata,
                pump_curve::wsol_ata(&m.trader),
                vault,
                pump_curve::wsol_ata(&vault),
                m.sharing_config,
                PUMP_GLOBAL_VOLUME_ACCUMULATOR,
                m.pump_uva,
                m.pump_uva_quote_ata,
                m.trader_base_ata,
                anchor_lang::system_program::ID,
            ];
            assert_eq!(x.iter().map(|a| a.pubkey).collect::<Vec<_>>(), want);
            let w: Vec<usize> = x
                .iter()
                .enumerate()
                .filter(|(_, a)| a.is_writable)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(w, [6, 7, 8, 9, 10, 11, 12, 13, 16, 17, 18]);
            assert!(x.iter().all(|a| !a.is_signer));
        }
        assert!(
            trade_extras_as(VenueKind::PumpCurve, &m, &[], CurveExtrasFormat::V2).is_err(),
            "no Global read: no trade"
        );
        for f in [CurveExtrasFormat::V2, CurveExtrasFormat::V3] {
            assert_eq!(f.other().other(), f);
            assert_eq!(
                trade_extras_as(VenueKind::RaydiumV4, &m, &[], f).unwrap(),
                trade_extras(VenueKind::RaydiumV4, &m, &[]).unwrap()
            );
        }
        assert_eq!(CurveExtrasFormat::V3.trade_len(), pump_curve::TRADE_LEN);
    }

    /// pump-final §7: what an exit's simulation says about the TradeExtras format it carried —
    /// success, a later refusal or a venue's own failure got past account parsing; the parser's
    /// two refusals did not; a failure before it (an Anchor constraint, the market's own checks)
    /// or without a custom error says nothing.
    #[test]
    fn format_evidence_reads_the_account_parsing_refusals() {
        use shortwin::errors::ShortwinError as E;
        use shortwin_services_common::{TxError, TxOutcome};
        let refused = |program: Pubkey, code: Option<u32>| TxOutcome {
            err: Some(match code {
                Some(code) => TxError::Custom { index: 1, code },
                None => TxError::Instruction {
                    index: 1,
                    detail: "InvalidAccountData".into(),
                },
            }),
            logs: vec![
                format!("Program {program} invoke [1]"),
                format!("Program {program} failed: custom program error"),
            ],
            ..TxOutcome::default()
        };
        let sw = shortwin::ID;
        let c = |e: E| Some(u32::from(e));
        assert_eq!(
            format_evidence(&TxOutcome::default()),
            FormatEvidence::Parsed
        );
        for e in [E::BadRemainingAccounts, E::VenueAccountMismatch] {
            assert_eq!(
                format_evidence(&refused(sw, c(e))),
                FormatEvidence::ParseFailed
            );
        }
        for e in [
            E::ExitBoundNotMet,
            E::UseSolSettle,
            E::NotLiquidatable,
            E::EmaWarmingUp,
        ] {
            assert_eq!(format_evidence(&refused(sw, c(e))), FormatEvidence::Parsed);
        }
        // pump's 6002 behind short.win's CPI: pump is the failing program the logs name first.
        let pump = shortwin::constants::PUMP_PROGRAM_ID;
        assert_eq!(
            format_evidence(&refused(pump, Some(6_002))),
            FormatEvidence::Parsed
        );
        for code in [
            c(E::AccountMismatch),
            c(E::ConfigVersion),
            c(E::NoRoute),
            c(E::VenueMigrating),
            Some(2_006),
            None,
        ] {
            assert_eq!(
                format_evidence(&refused(sw, code)),
                FormatEvidence::Unknown,
                "{code:?}"
            );
        }
    }

    /// M6-SEC-4: the exit's `create_idempotent(T11)` goes out only on a PumpSwap market whose live
    /// pool names another coin creator than the cache, for the live creator's vault authority.
    #[test]
    fn a_changed_coin_creator_gets_its_vault_ata_in_the_exit() {
        use shortwin_venue_state::pump_amm as vamm;
        let mut m: Market = bytemuck::Zeroable::zeroed();
        let cached_creator = Pubkey::new_unique();
        let a = pump_amm::coin_creator_vault_authority(&cached_creator);
        m.pool_cache[pump_swap_cache::CACHED_COIN_CREATOR] = cached_creator;
        m.pool_cache[pump_swap_cache::CACHED_COIN_CREATOR_VAULT_AUTH] = a;
        m.pool_cache[pump_swap_cache::CACHED_COIN_CREATOR_VAULT_ATA] = pump_curve::wsol_ata(&a);
        let pool_with = |creator: &Pubkey| {
            let mut d = vec![0u8; vamm::POOL_LEN];
            d[..8].copy_from_slice(&vamm::POOL_DISCRIMINATOR);
            d[vamm::OFF_COIN_CREATOR..vamm::OFF_COIN_CREATOR + 32]
                .copy_from_slice(creator.as_ref());
            vec![Some(AccountData {
                lamports: 1,
                owner: shortwin::constants::PUMP_AMM_PROGRAM_ID,
                executable: false,
                data: d,
            })]
        };
        let payer = Pubkey::new_unique();
        assert!(
            pamm_creator_vault_prep(&payer, &m, VenueKind::PumpSwap, &pool_with(&cached_creator))
                .is_none()
        );
        let live = Pubkey::new_unique();
        let ix = pamm_creator_vault_prep(&payer, &m, VenueKind::PumpSwap, &pool_with(&live))
            .expect("a changed creator");
        let auth = pump_amm::coin_creator_vault_authority(&live);
        assert_eq!(
            ix.program_id,
            shortwin::constants::ASSOCIATED_TOKEN_PROGRAM_ID
        );
        assert_eq!(ix.data, vec![1], "create_idempotent");
        assert_eq!(ix.accounts[1].pubkey, pump_curve::wsol_ata(&auth));
        assert_eq!(ix.accounts[2].pubkey, auth);
        assert!(
            ix.accounts
                .iter()
                .all(|a| !a.is_signer || a.pubkey == payer)
        );
        assert!(
            pamm_creator_vault_prep(&payer, &m, VenueKind::PumpCurve, &pool_with(&live)).is_none()
        );
        assert!(pamm_creator_vault_prep(&payer, &m, VenueKind::PumpSwap, &[None]).is_none());
    }
}
