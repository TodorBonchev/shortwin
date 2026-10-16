//! Venue trades and lifecycle events (INDEXER.md §7.2). Owner: INGEST.
//!
//! - pump `TradeEvent` (an `emit_cpi!` of the pump program): mint, sol_amount, token_amount,
//!   is_buy, user, the post-trade virtual / real reserves, fee, creator, creator_fee (the later
//!   fields are decoded tolerantly: known prefix, trailing bytes ignored). Its `ix_name` is a
//!   label, never a dispatch key: v3 trades emit `buy` / `sell` / `buy_exact_quote_in` (not the
//!   `*_v3` names pump's docs give), multi-hop ones `multi_hop_swap`; the `fee_recipient` of a v3
//!   trade is the zero key; a row on a SOL-quoted curve only (the `quote_mint` after the
//!   shareholders vector, [`pump_trade`]);
//! - pump `PostCompleteBuyEvent` (pump's 2026-10-08 redeploy): the synthetic-migration part of a
//!   buy past `rT` — the tokens a completing buy takes from the would-be pool — as a second pump
//!   trade row of the same instruction, on a SOL-quoted curve only ([`pump_post_complete_buy`]);
//! - PumpSwap `BuyEvent` / `SellEvent` (`emit_cpi!` of pAMM): base / quote amounts, lp_fee,
//!   protocol_fee, pool, user, coin_creator, coin_creator_fee, pool reserves; the pool's mints
//!   from the emitting instruction ([`pump_amm_trade_mints`]: accounts 3 and 4 of a `buy` /
//!   `sell` / `buy_v2` / `sell_v2`, the event pool's hop group of a `multi_hop_swap`);
//! - Raydium CPMM `SwapEvent` (`emit!`, discriminator `40c6cde8260871e2`, **only from the CPMM
//!   program**: CLMM and others emit an event of the same name and discriminator);
//! - Raydium AMM v4 `ray_log` (base64 bincode `SwapBaseInLog` log_type 3 / `SwapBaseOutLog`
//!   log_type 4), oriented with `venue_pools.sol_is_first` — or, for short.win's own legs, by
//!   the leg's direction (an open sells tokens, an exit buys them);
//! - lifecycle: pump `CompleteEvent` and the other pump / pAMM events → `venue_events`; BPF
//!   upgradeable loader `Upgrade` of a venue program → `venue_program_upgrades`.
//!
//! Normalised row (`venue_trades`): `side` from the trader's view (buy = tokens out of the pool),
//! `token_amount`, `sol_amount` = lamports the trader paid (buy) or received (sell) with the
//! venue's fees, the fees, `fee_creator_lamports` (a creator fee charged in tokens is valued at
//! the trade's price `⌊fee·sol_amount/token_amount⌋`), `creator`, `user_key`, the post-trade
//! reserves and `price_wad = ⌊sol_amount·WAD/token_amount⌋`.
//!
//! **Post-trade reserves are the venue's pricing reserves** — what the next trade prices on, so
//! consecutive trades of a pool chain exactly (M10 GC-F1; `tests/venue_mainnet.rs` on WP-9's
//! recorded mainnet swaps):
//! - pump: the curve's virtual reserves from the event;
//! - PumpSwap: base = the base vault; quote = the quote vault + the event's
//!   `virtual_quote_reserves` (the effective quote reserves every buy and sell prices on,
//!   `shortwin_venue_state::pump_amm::PoolReserves`);
//! - CPMM: `vault − (protocol + fund + creator counters)` (`vault_amount_without_fee`): the
//!   event's `*_vault_before` are those, and of the input the protocol and fund shares of the
//!   trade fee (`AmmConfig` rates, [`CpmmFeeRates`]) and an input-side creator fee go to the
//!   counters, an output-side creator fee likewise;
//! - AMM v4: `vault − need_take_pnl`, as `ray_log`'s `pool_coin` / `pool_pc` report them (a
//!   swap moves only the vaults: the whole input stays, the output leaves).

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;

use super::idl::key_json;
use crate::numeric::{WAD, hex};

/// Programs (mainnet ids; the fixtures run the dumped programs at the same addresses).
pub const PUMP: Pubkey = shortwin::constants::PUMP_PROGRAM_ID;
pub const PUMP_AMM: Pubkey = shortwin::constants::PUMP_AMM_PROGRAM_ID;
pub const RAYDIUM_V4: Pubkey = shortwin::constants::RAYDIUM_V4_PROGRAM_ID;
pub const RAYDIUM_CPMM: Pubkey = shortwin::constants::RAYDIUM_CPMM_PROGRAM_ID;
pub const WSOL: Pubkey = shortwin::constants::WSOL_MINT;
/// The BPF upgradeable loader.
pub const BPF_UPGRADEABLE_LOADER: Pubkey =
    solana_pubkey::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");

/// Anchor's event discriminator `sha256("event:<name>")[..8]`.
pub fn anchor_event_discriminator(name: &str) -> [u8; 8] {
    let h = Sha256::digest(format!("event:{name}").as_bytes());
    let mut d = [0u8; 8];
    d.copy_from_slice(&h[..8]);
    d
}

/// Known pump and pAMM event names (their discriminators are derived, not copied).
const PUMP_EVENTS: &[&str] = &[
    "TradeEvent",
    "CompleteEvent",
    "CreateEvent",
    "SetParamsEvent",
    "CompletePumpAmmMigrationEvent",
    "CollectCreatorFeeEvent",
    "ExtendAccountEvent",
    "SetCreatorEvent",
    "SetMetaplexCreatorEvent",
    "UpdateGlobalAuthorityEvent",
    "AdminSetCreatorEvent",
    "AdminSetIdlAuthorityEvent",
    "AdminUpdateTokenIncentivesEvent",
    "ClaimTokenIncentivesEvent",
    "InitUserVolumeAccumulatorEvent",
    "CloseUserVolumeAccumulatorEvent",
    "SyncUserVolumeAccumulatorEvent",
    // pump's 2026-10-02 redeploy (v3 trades keep their fees in the curve's buckets).
    "SweepBondingCurveFeeEvent",
    // pump's 2026-10-08 redeploy: the synthetic migration's pool part of a completing buy.
    "PostCompleteBuyEvent",
];
const PUMP_AMM_EVENTS: &[&str] = &[
    "BuyEvent",
    "SellEvent",
    "CreatePoolEvent",
    "DepositEvent",
    "WithdrawEvent",
    "DisableEvent",
    "CreateConfigEvent",
    "UpdateAdminEvent",
    "UpdateFeeConfigEvent",
    "ExtendAccountEvent",
    "CollectCoinCreatorFeeEvent",
    "SetBondingCurveCoinCreatorEvent",
    "SetMetaplexCoinCreatorEvent",
    "AdminSetCoinCreatorEvent",
    "AdminUpdateTokenIncentivesEvent",
    "ClaimTokenIncentivesEvent",
    "InitUserVolumeAccumulatorEvent",
    "CloseUserVolumeAccumulatorEvent",
    "SyncUserVolumeAccumulatorEvent",
    // PumpSwap's 2026-10-02 redeploy (v2 trades keep their fees in the pool's buckets).
    "SweepPoolFeeEvent",
];

fn table(names: &'static [&'static str]) -> Vec<([u8; 8], &'static str)> {
    names
        .iter()
        .map(|n| (anchor_event_discriminator(n), *n))
        .collect()
}

fn pump_names() -> &'static [([u8; 8], &'static str)] {
    static T: OnceLock<Vec<([u8; 8], &'static str)>> = OnceLock::new();
    T.get_or_init(|| table(PUMP_EVENTS))
}

fn pump_amm_names() -> &'static [([u8; 8], &'static str)] {
    static T: OnceLock<Vec<([u8; 8], &'static str)>> = OnceLock::new();
    T.get_or_init(|| table(PUMP_AMM_EVENTS))
}

/// The pump (`program == PUMP`) or pAMM event name of `payload` (discriminator ‖ Borsh).
pub fn venue_event_name(program: &Pubkey, payload: &[u8]) -> Option<&'static str> {
    let t = if *program == PUMP {
        pump_names()
    } else if *program == PUMP_AMM {
        pump_amm_names()
    } else {
        return None;
    };
    t.iter()
        .find(|(d, _)| payload.starts_with(d))
        .map(|(_, n)| *n)
}

/// CPMM `SwapEvent`'s discriminator (`sha256("event:SwapEvent")[..8]` = `40c6cde8260871e2`).
pub const CPMM_SWAP_EVENT: [u8; 8] = [0x40, 0xc6, 0xcd, 0xe8, 0x26, 0x08, 0x71, 0xe2];

/// The trader's direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Tokens out of the pool.
    Buy,
    /// Tokens into the pool.
    Sell,
}

impl Side {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
}

/// A normalised venue trade (`venue_trades`), before the writer adds the chain position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trade {
    pub venue: &'static str,
    pub program: Pubkey,
    pub pool: Pubkey,
    pub mint: Option<Pubkey>,
    pub side: Side,
    pub token_amount: u64,
    pub sol_amount: u64,
    pub fee_lp: Option<u64>,
    pub fee_protocol: Option<u64>,
    pub fee_creator: Option<u64>,
    pub fee_creator_lamports: Option<u64>,
    pub creator: Option<Pubkey>,
    pub user: Pubkey,
    pub pool_token_reserve: Option<u64>,
    pub pool_sol_reserve: Option<u64>,
}

impl Trade {
    /// `⌊sol_amount·WAD / token_amount⌋` (`None` for a zero token amount).
    pub fn price_wad(&self) -> Option<u128> {
        (self.token_amount > 0)
            .then(|| u128::from(self.sol_amount) * WAD / u128::from(self.token_amount))
    }
}

/// `⌊fee·sol / tokens⌋`: a token-side fee valued at the trade's price.
fn at_price(fee: u64, sol: u64, tokens: u64) -> u64 {
    if tokens == 0 {
        return 0;
    }
    u64::try_from(u128::from(fee) * u128::from(sol) / u128::from(tokens)).unwrap_or(u64::MAX)
}

/// A little-endian Borsh reader that reports running out of bytes as `None`.
struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Some(h)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i128(&mut self) -> Option<i128> {
        Some(i128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
    fn key(&mut self) -> Option<Pubkey> {
        Some(Pubkey::new_from_array(self.take(32)?.try_into().ok()?))
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.take(1)?[0] != 0)
    }
}

/// The pump curve's bonding-curve PDA of `mint` (`["bonding-curve", mint]`).
pub fn bonding_curve_of(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMP).0
}

/// The `quote_mint` of a pump `TradeEvent`, read from the fields after `creator_fee` (pump's IDL
/// of the 2026-10-13 build): `track_volume`, four u64 / i64 volume fields, `ix_name` (a Borsh
/// string), `mayhem_mode`, `cashback_fee_basis_points`, `cashback`, `buyback_fee_basis_points`,
/// `buyback_fee`, the variable-length `shareholders` vector (a u32 count of 34-byte `{address,
/// share_bps: u16}`), then `quote_mint`. `None` for an event that ends before it: one emitted
/// before pump quoted curves in another token, when every curve was a SOL curve.
fn pump_trade_quote_mint(r: &mut Rd<'_>) -> Option<Pubkey> {
    let _track_volume = r.bool()?;
    for _ in 0..4 {
        r.u64()?; // total_unclaimed_tokens … last_update_timestamp
    }
    let n = usize::try_from(r.u32()?).ok()?;
    r.take(n)?; // ix_name
    let _mayhem_mode = r.bool()?;
    for _ in 0..4 {
        r.u64()?; // cashback_fee_basis_points, cashback, buyback_fee_basis_points, buyback_fee
    }
    let n = usize::try_from(r.u32()?).ok()?;
    r.take(n.checked_mul(32 + 2)?)?; // shareholders
    r.key()
}

/// pump `TradeEvent` (body after the discriminator; field order per pump's IDL). `None` unless
/// the curve is SOL-quoted: a `quote_mint` (read past the shareholders vector,
/// [`pump_trade_quote_mint`]) other than the zero key, which pump writes for a SOL curve, or
/// WSOL is a curve quoted in another token (since pump's 2026-10-08 build, e.g. the curve hop of
/// a `multi_hop_swap` through a pump coin), whose `sol_amount` and fees are in that token's
/// units, not lamports (M8-SEC-6, as [`pump_post_complete_buy`]). An event that ends before
/// `quote_mint` is a SOL curve's.
pub fn pump_trade(body: &[u8]) -> Option<Trade> {
    let mut r = Rd(body);
    let mint = r.key()?;
    let sol_amount = r.u64()?;
    let token_amount = r.u64()?;
    let is_buy = r.bool()?;
    let user = r.key()?;
    let _timestamp = r.i64()?;
    let virtual_sol = r.u64()?;
    let virtual_token = r.u64()?;
    let _real_sol = r.u64()?;
    let _real_token = r.u64()?;
    let _fee_recipient = r.key()?;
    let _fee_bps = r.u64()?;
    let fee = r.u64()?;
    let creator = r.key()?;
    let _creator_fee_bps = r.u64()?;
    let creator_fee = r.u64()?;
    if pump_trade_quote_mint(&mut r).is_some_and(|q| q != Pubkey::default() && q != WSOL) {
        return None;
    }
    let fees = fee.saturating_add(creator_fee);
    let (side, trader_sol) = if is_buy {
        (Side::Buy, sol_amount.saturating_add(fees))
    } else {
        (Side::Sell, sol_amount.saturating_sub(fees))
    };
    Some(Trade {
        venue: "pump_curve",
        program: PUMP,
        pool: bonding_curve_of(&mint),
        mint: Some(mint),
        side,
        token_amount,
        sol_amount: trader_sol,
        fee_lp: None,
        fee_protocol: Some(fee),
        fee_creator: Some(creator_fee),
        fee_creator_lamports: Some(creator_fee),
        creator: (creator != Pubkey::default()).then_some(creator),
        user,
        pool_token_reserve: Some(virtual_token),
        pool_sol_reserve: Some(virtual_sol),
    })
}

/// pump `PostCompleteBuyEvent` (pump's 2026-10-08 redeploy; body after the discriminator, 224
/// bytes: `user` @0, `mint` @32, `bonding_curve` @64, `quote_mint` @96, `timestamp` @128,
/// `base_out` @136, `quote_in` @144, `fee_basis_points` @152, `fee` @160,
/// `creator_fee_basis_points` @168, `creator_fee` @176, `buyback_fee` @184, the would-be pool's
/// `pool_base_reserves_before` / `pool_quote_reserves_before` @192 / @200 and `*_after` @208 /
/// @216). A buy past `rT` (the synthetic migration) completes the curve with its `TradeEvent`, then
/// buys `base_out` from the pool `migrate_v2` will create, paying `quote_in` plus the curve's fees
/// on it: a second pump trade row of the same instruction (`ev_seq` keeps the two apart), a buy of
/// `base_out` for `quote_in + fee + creator_fee` lamports (the buyback is inside `fee`), its
/// post-trade reserves the would-be pool's `*_after` pair. `None` unless the curve is SOL-quoted
/// (`quote_mint` the zero key, as pump writes it for a SOL curve, or WSOL): synthetic
/// completions happen on curves quoted in another token too (e.g. `E1bYRCNq…`, quoted in a pump
/// coin), whose quote amounts are not lamports (M8-SEC-6, as [`pump_amm_trade`]).
pub fn pump_post_complete_buy(body: &[u8]) -> Option<Trade> {
    let mut r = Rd(body);
    let user = r.key()?;
    let mint = r.key()?;
    let bonding_curve = r.key()?;
    let quote_mint = r.key()?;
    if quote_mint != Pubkey::default() && quote_mint != WSOL {
        return None;
    }
    let _timestamp = r.i64()?;
    let base_out = r.u64()?;
    let quote_in = r.u64()?;
    let _fee_bps = r.u64()?;
    let fee = r.u64()?;
    let _creator_fee_bps = r.u64()?;
    let creator_fee = r.u64()?;
    let _buyback_fee = r.u64()?;
    let _pool_base_before = r.u64()?;
    let _pool_quote_before = r.u64()?;
    let pool_base_after = r.u64()?;
    let pool_quote_after = r.u64()?;
    Some(Trade {
        venue: "pump_curve",
        program: PUMP,
        pool: bonding_curve,
        mint: Some(mint),
        side: Side::Buy,
        token_amount: base_out,
        sol_amount: quote_in.saturating_add(fee).saturating_add(creator_fee),
        fee_lp: None,
        fee_protocol: Some(fee),
        fee_creator: Some(creator_fee),
        fee_creator_lamports: Some(creator_fee),
        creator: None,
        user,
        pool_token_reserve: Some(pool_base_after),
        pool_sol_reserve: Some(pool_quote_after),
    })
}

/// The `virtual_quote_reserves` of a pAMM `BuyEvent` / `SellEvent`, read from the fields after
/// `coin_creator_fee` (pAMM IDL 2026-09): a `BuyEvent`'s `track_volume`, five u64 / i64 volume
/// fields and `ix_name` (a Borsh string), then on both `cashback_fee_basis_points`, `cashback`,
/// `buyback_fee_basis_points`, `buyback_fee` and `virtual_quote_reserves` (i128). `None` for an
/// event that ends before it (one emitted before the boost added the field, when every pool's
/// virtual quote reserves were 0; R-VEN §1.4).
fn pamm_virtual_quote_reserves(buy: bool, r: &mut Rd<'_>) -> Option<i128> {
    if buy {
        let _track_volume = r.bool()?;
        for _ in 0..5 {
            r.u64()?; // total_unclaimed_tokens … min_base_amount_out
        }
        let n = usize::try_from(r.u32()?).ok()?;
        r.take(n)?; // ix_name
    }
    for _ in 0..4 {
        r.u64()?; // cashback_fee_basis_points, cashback, buyback_fee_basis_points, buyback_fee
    }
    r.i128()
}

/// pAMM `BuyEvent` / `SellEvent` (`buy` = BuyEvent). `base_mint` / `quote_mint`: the pool's
/// mints from the emitting instruction's accounts (3 and 4). `None` unless one side is WSOL: a
/// pool pairing a token with anything else is not a SOL market, and its quote amounts are not
/// lamports (M8-SEC-6; the CPMM decoder checks the same).
///
/// Reserves: the event's `pool_*_token_reserves` are the vaults before the trade; the quote side
/// is stored as the effective quote reserves `vault + virtual_quote_reserves` (the event's; every
/// buy and sell prices on that sum, which pump keeps in `[0, u64::MAX]` — outside it the pool
/// breaks pump's invariant and the quote reserve is not stored).
pub fn pump_amm_trade(
    buy: bool,
    body: &[u8],
    base_mint: Option<Pubkey>,
    quote_mint: Option<Pubkey>,
) -> Option<Trade> {
    if base_mint != Some(WSOL) && quote_mint != Some(WSOL) {
        return None;
    }
    let mut r = Rd(body);
    let _timestamp = r.i64()?;
    let base_amount = r.u64()?; // base_amount_out (buy) / base_amount_in (sell)
    let _bound = r.u64()?; // max_quote_amount_in / min_quote_amount_out
    let _user_base_reserves = r.u64()?;
    let _user_quote_reserves = r.u64()?;
    let pool_base = r.u64()?;
    let pool_quote = r.u64()?;
    let _quote_amount = r.u64()?; // quote_amount_in / quote_amount_out (before fees)
    let _lp_fee_bps = r.u64()?;
    let lp_fee = r.u64()?;
    let _protocol_fee_bps = r.u64()?;
    let protocol_fee = r.u64()?;
    let quote_net_lp = r.u64()?; // quote_amount_in_with_lp_fee / quote_amount_out_without_lp_fee
    let user_quote = r.u64()?; // user_quote_amount_in / user_quote_amount_out
    let pool = r.key()?;
    let user = r.key()?;
    let _user_base_ata = r.key()?;
    let _user_quote_ata = r.key()?;
    let _protocol_fee_recipient = r.key()?;
    let _protocol_fee_recipient_ata = r.key()?;
    let coin_creator = r.key()?;
    let _coin_creator_fee_bps = r.u64()?;
    let coin_creator_fee = r.u64()?;
    let virtual_quote = pamm_virtual_quote_reserves(buy, &mut r).unwrap_or(0);
    // Post-trade vaults: the lp fee stays in the pool, the other fees leave it.
    let (base_after, quote_vault_after) = if buy {
        (
            pool_base.saturating_sub(base_amount),
            pool_quote.saturating_add(quote_net_lp),
        )
    } else {
        (
            pool_base.saturating_add(base_amount),
            pool_quote.saturating_sub(quote_net_lp),
        )
    };
    let quote_after = i128::from(quote_vault_after)
        .checked_add(virtual_quote)
        .and_then(|q| u64::try_from(q).ok());
    // Canonical pools: base = the token, quote = WSOL. A reversed pool (base = WSOL) swaps the
    // trader's view.
    let base_is_sol = base_mint == Some(WSOL);
    let (side, token_amount, sol_amount, mint, tok_res, sol_res) = if base_is_sol {
        (
            if buy { Side::Sell } else { Side::Buy },
            user_quote,
            base_amount,
            quote_mint,
            quote_after,
            Some(base_after),
        )
    } else {
        (
            if buy { Side::Buy } else { Side::Sell },
            base_amount,
            user_quote,
            base_mint.filter(|m| *m != WSOL),
            Some(base_after),
            quote_after,
        )
    };
    let creator_lamports = if base_is_sol {
        at_price(coin_creator_fee, sol_amount, token_amount)
    } else {
        coin_creator_fee
    };
    Some(Trade {
        venue: "pump_swap",
        program: PUMP_AMM,
        pool,
        mint,
        side,
        token_amount,
        sol_amount,
        fee_lp: Some(lp_fee),
        fee_protocol: Some(protocol_fee),
        fee_creator: Some(coin_creator_fee),
        fee_creator_lamports: Some(creator_lamports),
        creator: (coin_creator != Pubkey::default()).then_some(coin_creator),
        user,
        pool_token_reserve: tok_res,
        pool_sol_reserve: sol_res,
    })
}

/// PumpSwap `multi_hop_swap`'s discriminator (`sha256("global:multi_hop_swap")[..8]`, PumpSwap's
/// 2026-10-08 redeploy).
pub const PUMP_AMM_MULTI_HOP_SWAP: [u8; 8] = [0x2b, 0x64, 0x49, 0x13, 0xe9, 0xf6, 0x6f, 0x94];
/// `multi_hop_swap`'s fixed accounts (user … `pump_event_authority`); the hop groups follow.
pub const MULTI_HOP_FIXED_ACCOUNTS: usize = 16;
/// Accounts per `multi_hop_swap` hop group: `[base_mint, quote_mint, pool (or curve), base vault,
/// quote vault]` (pump-public-docs `MULTI_HOP_SWAP.md`).
pub const MULTI_HOP_GROUP: usize = 5;

/// The `pool` of a pAMM `BuyEvent` / `SellEvent` body (after the 14 u64 fields).
pub fn pump_amm_event_pool(body: &[u8]) -> Option<Pubkey> {
    let mut r = Rd(body);
    r.take(14 * 8)?;
    r.key()
}

/// `(base_mint, quote_mint)` of the pool a pAMM `BuyEvent` / `SellEvent` names, from the emitting
/// instruction (`data`, `accounts`): a `multi_hop_swap` holds one group per hop after its 16 fixed
/// accounts, and the event's pool is the group's third account — the mints are the group's first
/// two (`(None, None)` when no group names the pool); a `buy` / `sell` / `buy_v2` / `sell_v2` has
/// them at accounts 3 and 4. The caller falls back to `venue_pools` for a pool it cannot place.
pub fn pump_amm_trade_mints(
    data: &[u8],
    accounts: &[Pubkey],
    body: &[u8],
) -> (Option<Pubkey>, Option<Pubkey>) {
    if !data.starts_with(&PUMP_AMM_MULTI_HOP_SWAP) {
        return (accounts.get(3).copied(), accounts.get(4).copied());
    }
    let Some(pool) = pump_amm_event_pool(body) else {
        return (None, None);
    };
    accounts
        .get(MULTI_HOP_FIXED_ACCOUNTS..)
        .unwrap_or_default()
        .as_chunks::<MULTI_HOP_GROUP>()
        .0
        .iter()
        .find(|g| g[2] == pool)
        .map_or((None, None), |g| (Some(g[0]), Some(g[1])))
}

/// The shares of a CPMM trade fee that leave the pricing reserves: `AmmConfig.protocol_fee_rate`
/// and `fund_fee_rate` (ppm of the trade fee). The venue moves `⌊trade_fee·rate/10⁶⌋` of each
/// into the pool's protocol and fund counters, which `vault_amount_without_fee` subtracts.
/// `creator_fee_rate` is not needed (the event carries the creator fee itself), nor is the 2026-10
/// build's `creator_fee_share_rate` (it splits a collected creator fee between two counters
/// that are both excluded: the swap path is unchanged, tests/fixtures/README.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CpmmFeeRates {
    pub protocol: u64,
    pub fund: u64,
}

impl CpmmFeeRates {
    /// Every mainnet `AmmConfig` (indexes 0–20, `tests/fixtures/accounts/raydium/cpmm_config_*`,
    /// equal to the live configs at slot 452,488,420): 12 % protocol, 4 % fund. Used for a
    /// config the venue admin watch has not observed (`venue_admin_states`).
    pub const MAINNET: Self = Self {
        protocol: 120_000,
        fund: 40_000,
    };
    /// The rates' denominator (CPMM's `FEE_RATE_DENOMINATOR_VALUE`).
    pub const DENOMINATOR: u64 = 1_000_000;

    /// The part of `trade_fee` moved into the protocol and fund counters (each share floored, as
    /// the venue's `Fees::protocol_fee` / `fund_fee`).
    pub fn counters_share(self, trade_fee: u64) -> u64 {
        let share = |rate: u64| {
            u64::try_from(u128::from(trade_fee) * u128::from(rate) / u128::from(Self::DENOMINATOR))
                .unwrap_or(u64::MAX)
        };
        share(self.protocol).saturating_add(share(self.fund))
    }
}

/// `venue_admin_states.field` of the watched `AmmConfig` rates (the refresher's venue admin watch
/// writes them, the writer reads the two the reserves need).
pub const CPMM_PROTOCOL_FEE_RATE: &str = "protocol_fee_rate";
pub const CPMM_FUND_FEE_RATE: &str = "fund_fee_rate";

/// Raydium CPMM `SwapEvent` (the whole "Program data:" payload, discriminator included).
/// `user`: the swap instruction's payer (its account 0); `rates`: the pool's `AmmConfig` rates
/// (the swap's account 2; [`PoolCtx::cpmm_rates`]).
///
/// Post-trade (pricing) reserves: `input_vault_before + input_amount − (protocol + fund shares
/// of trade_fee) − (creator_fee if on the input)` and `output_vault_before − output_amount −
/// (creator_fee if on the output)`. `input_amount` is what reached the input vault (a Token-2022
/// transfer fee already deducted) and `output_amount` what left the output vault.
pub fn cpmm_swap(payload: &[u8], user: Pubkey, rates: CpmmFeeRates) -> Option<Trade> {
    let body = payload.strip_prefix(&CPMM_SWAP_EVENT)?;
    let mut r = Rd(body);
    let pool = r.key()?;
    let input_before = r.u64()?;
    let output_before = r.u64()?;
    let input_amount = r.u64()?;
    let output_amount = r.u64()?;
    let _input_transfer_fee = r.u64()?;
    let _output_transfer_fee = r.u64()?;
    let _base_input = r.bool()?;
    let input_mint = r.key()?;
    let output_mint = r.key()?;
    let trade_fee = r.u64()?;
    let creator_fee = r.u64()?;
    let creator_fee_on_input = r.bool()?;
    let buy = input_mint == WSOL;
    if !buy && output_mint != WSOL {
        return None; // not a SOL pool
    }
    let (side, mint, token_amount, sol_amount) = if buy {
        (Side::Buy, output_mint, output_amount, input_amount)
    } else {
        (Side::Sell, input_mint, input_amount, output_amount)
    };
    // The trade fee is charged on the input; the creator fee on the input or the output.
    let trade_fee_lamports = if buy {
        trade_fee
    } else {
        at_price(trade_fee, sol_amount, token_amount)
    };
    let creator_on_sol = creator_fee_on_input == buy;
    let creator_lamports = if creator_on_sol {
        creator_fee
    } else {
        at_price(creator_fee, sol_amount, token_amount)
    };
    let (creator_in, creator_out) = if creator_fee_on_input {
        (creator_fee, 0)
    } else {
        (0, creator_fee)
    };
    let input_after = input_before
        .saturating_add(input_amount)
        .saturating_sub(rates.counters_share(trade_fee))
        .saturating_sub(creator_in);
    let output_after = output_before
        .saturating_sub(output_amount)
        .saturating_sub(creator_out);
    let (tok_res, sol_res) = if buy {
        (output_after, input_after)
    } else {
        (input_after, output_after)
    };
    Some(Trade {
        venue: "raydium_cpmm",
        program: RAYDIUM_CPMM,
        pool,
        mint: Some(mint),
        side,
        token_amount,
        sol_amount,
        fee_lp: Some(trade_fee_lamports),
        fee_protocol: None,
        fee_creator: Some(creator_fee),
        fee_creator_lamports: Some(creator_lamports),
        creator: None,
        user,
        pool_token_reserve: Some(tok_res),
        pool_sol_reserve: Some(sol_res),
    })
}

/// A decoded AMM v4 `ray_log` swap record (R-RAY §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RayLog {
    /// 3 = SwapBaseIn, 4 = SwapBaseOut.
    pub log_type: u8,
    /// Amount in (3) / max in (4).
    pub amount: u64,
    /// Minimum out (3) / amount out (4).
    pub bound: u64,
    /// 1 = PC2Coin (pc in), 2 = Coin2PC (coin in).
    pub direction: u64,
    pub pool_coin: u64,
    pub pool_pc: u64,
    /// Amount out (3) / the input deducted (4).
    pub result: u64,
}

impl RayLog {
    /// `(amount in, amount out)`.
    pub fn amounts(&self) -> (u64, u64) {
        if self.log_type == 3 {
            (self.amount, self.result)
        } else {
            (self.result, self.bound)
        }
    }

    /// `true` if the coin side is the input.
    pub fn coin_in(&self) -> bool {
        self.direction == 2
    }
}

/// Decodes a `ray_log` payload (57 bytes: log type + 7 u64); other log types are not swaps.
pub fn ray_log(b: &[u8]) -> Option<RayLog> {
    if b.len() != 57 || !matches!(b[0], 3 | 4) {
        return None;
    }
    let u = |i: usize| u64::from_le_bytes(b[1 + 8 * i..9 + 8 * i].try_into().unwrap_or([0; 8]));
    Some(RayLog {
        log_type: b[0],
        amount: u(0),
        bound: u(1),
        direction: u(2),
        pool_coin: u(4),
        pool_pc: u(5),
        result: u(6),
    })
}

/// An AMM v4 trade from its `ray_log`, with SOL on the coin side (`sol_is_coin`) or not.
pub fn ray_v4_trade(log: &RayLog, pool: Pubkey, user: Pubkey, sol_is_coin: bool) -> Trade {
    let (amount_in, amount_out) = log.amounts();
    let sol_in = log.coin_in() == sol_is_coin;
    let (side, token_amount, sol_amount) = if sol_in {
        (Side::Buy, amount_out, amount_in)
    } else {
        (Side::Sell, amount_in, amount_out)
    };
    let (coin_after, pc_after) = if log.coin_in() {
        (
            log.pool_coin.saturating_add(amount_in),
            log.pool_pc.saturating_sub(amount_out),
        )
    } else {
        (
            log.pool_coin.saturating_sub(amount_out),
            log.pool_pc.saturating_add(amount_in),
        )
    };
    let (sol_res, tok_res) = if sol_is_coin {
        (coin_after, pc_after)
    } else {
        (pc_after, coin_after)
    };
    Trade {
        venue: "raydium_v4",
        program: RAYDIUM_V4,
        pool,
        mint: None,
        side,
        token_amount,
        sol_amount,
        fee_lp: None,
        fee_protocol: None,
        fee_creator: None,
        fee_creator_lamports: None,
        creator: None,
        user,
        pool_token_reserve: Some(tok_res),
        pool_sol_reserve: Some(sol_res),
    }
}

/// `sol_is_coin` of an AMM v4 pool from a short.win leg: the leg's direction is known (an open
/// sells tokens, an exit buys them), so the log's direction tells which side SOL is on.
pub fn ray_v4_orientation_from_leg(log: &RayLog, shortwin_ix: &str) -> Option<bool> {
    let trader_sells = match shortwin_ix {
        "open_short" => true,
        "close_short" | "liquidate" | "expire" => false,
        _ => return None,
    };
    // Selling tokens: the input is the token side, so SOL is the coin iff pc is the input.
    Some(if trader_sells {
        !log.coin_in()
    } else {
        log.coin_in()
    })
}

/// A lifecycle (non-trade) venue event (`venue_events`).
#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleEvent {
    pub venue: &'static str,
    pub program: Pubkey,
    pub kind: &'static str,
    pub pool: Option<Pubkey>,
    pub mint: Option<Pubkey>,
    pub data: Value,
}

/// A pump / pAMM lifecycle event: the decoded prefix where the layout is known (pump
/// `CompleteEvent`: user, mint, bonding_curve, timestamp; pAMM `CreatePoolEvent`: through
/// `pool`; the 2026-10-02 redeploys' sweeps — pump `SweepBondingCurveFeeEvent` and pAMM
/// `SweepPoolFeeEvent`, `pump-rust-client` 0.2.0's IDLs: the bucket a permissionless
/// `sweep_protocol_fee` / `sweep_creator_fee` paid out, `amount` lamports to `recipient`; a sweep
/// moves the vault and the VQR oppositely, so no price or reserve row changes), always the raw
/// body as hex.
pub fn lifecycle(program: &Pubkey, kind: &'static str, body: &[u8]) -> LifecycleEvent {
    let venue = if *program == PUMP {
        "pump_curve"
    } else {
        "pump_swap"
    };
    let mut data = json!({ "raw": hex(body) });
    let (mut pool, mut mint) = (None, None);
    let mut r = Rd(body);
    match (venue, kind) {
        ("pump_curve", "CompleteEvent") => {
            if let (Some(user), Some(m), Some(curve), Some(ts)) =
                (r.key(), r.key(), r.key(), r.i64())
            {
                data["user"] = key_json(&user);
                data["mint"] = key_json(&m);
                data["bonding_curve"] = key_json(&curve);
                data["timestamp"] = Value::String(ts.to_string());
                pool = Some(curve);
                mint = Some(m);
            }
        }
        ("pump_swap", "CreatePoolEvent") => {
            let decoded = (|| {
                let ts = r.i64()?;
                let _index = r.take(2)?;
                let creator = r.key()?;
                let base_mint = r.key()?;
                let quote_mint = r.key()?;
                let _decimals = r.take(2)?;
                for _ in 0..7 {
                    r.u64()?;
                }
                let _bump = r.take(1)?;
                let pool = r.key()?;
                let lp_mint = r.key()?;
                Some((ts, creator, base_mint, quote_mint, pool, lp_mint))
            })();
            if let Some((ts, creator, base_mint, quote_mint, p, lp_mint)) = decoded {
                data["timestamp"] = Value::String(ts.to_string());
                data["creator"] = key_json(&creator);
                data["base_mint"] = key_json(&base_mint);
                data["quote_mint"] = key_json(&quote_mint);
                data["pool"] = key_json(&p);
                data["lp_mint"] = key_json(&lp_mint);
                pool = Some(p);
                mint = Some(if base_mint == WSOL {
                    quote_mint
                } else {
                    base_mint
                });
            }
        }
        ("pump_curve", "SweepBondingCurveFeeEvent") => {
            // timestamp, mint, bonding_curve, quote_mint, recipient, amount, bucket.
            let decoded = (|| {
                let ts = r.i64()?;
                let m = r.key()?;
                let curve = r.key()?;
                let quote_mint = r.key()?;
                let recipient = r.key()?;
                let amount = r.u64()?;
                let bucket = r.take(1)?[0];
                Some((ts, m, curve, quote_mint, recipient, amount, bucket))
            })();
            if let Some((ts, m, curve, quote_mint, recipient, amount, bucket)) = decoded {
                data["timestamp"] = Value::String(ts.to_string());
                data["mint"] = key_json(&m);
                data["bonding_curve"] = key_json(&curve);
                data["quote_mint"] = key_json(&quote_mint);
                data["recipient"] = key_json(&recipient);
                data["amount"] = Value::String(amount.to_string());
                data["bucket"] = Value::from(bucket);
                pool = Some(curve);
                mint = Some(m);
            }
        }
        ("pump_swap", "SweepPoolFeeEvent") => {
            // timestamp, pool, base_mint, quote_mint, recipient, payer, amount, bucket.
            let decoded = (|| {
                let ts = r.i64()?;
                let p = r.key()?;
                let base_mint = r.key()?;
                let quote_mint = r.key()?;
                let recipient = r.key()?;
                let payer = r.key()?;
                let amount = r.u64()?;
                let bucket = r.take(1)?[0];
                Some((
                    ts, p, base_mint, quote_mint, recipient, payer, amount, bucket,
                ))
            })();
            if let Some((ts, p, base_mint, quote_mint, recipient, payer, amount, bucket)) = decoded
            {
                data["timestamp"] = Value::String(ts.to_string());
                data["pool"] = key_json(&p);
                data["base_mint"] = key_json(&base_mint);
                data["quote_mint"] = key_json(&quote_mint);
                data["recipient"] = key_json(&recipient);
                data["payer"] = key_json(&payer);
                data["amount"] = Value::String(amount.to_string());
                data["bucket"] = Value::from(bucket);
                pool = Some(p);
                mint = Some(if base_mint == WSOL {
                    quote_mint
                } else {
                    base_mint
                });
            }
        }
        _ => {}
    }
    LifecycleEvent {
        venue,
        program: *program,
        kind,
        pool,
        mint,
        data,
    }
}

/// A BPF upgradeable loader `Upgrade` of a venue program (`venue_program_upgrades`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramUpgrade {
    /// `VenueGuard` index.
    pub venue: u8,
    pub program: Pubkey,
    pub programdata: Pubkey,
}

/// `Upgrade` (instruction 3: `[programdata, program, buffer, spill, rent, clock, authority]`)
/// of one of the six venue programs.
pub fn venue_upgrade(
    program_id: &Pubkey,
    data: &[u8],
    accounts: &[Pubkey],
) -> Option<ProgramUpgrade> {
    if *program_id != BPF_UPGRADEABLE_LOADER || data.get(..4) != Some(&3u32.to_le_bytes()[..]) {
        return None;
    }
    let programdata = *accounts.first()?;
    let program = *accounts.get(1)?;
    let venue = shortwin::constants::VENUE_PROGRAM_IDS
        .iter()
        .position(|p| *p == program)?;
    Some(ProgramUpgrade {
        venue: u8::try_from(venue).ok()?,
        program,
        programdata,
    })
}

/// What the decoder knows about pools (from `venue_pools`): AMM v4 orientation, mints and
/// markets of pools whose events do not carry them; and the CPMM `AmmConfig` rates the venue
/// admin watch observed (`venue_admin_states`), by config.
#[derive(Clone, Debug, Default)]
pub struct PoolCtx {
    pub pools: HashMap<Pubkey, PoolInfo>,
    pub cpmm_configs: HashMap<Pubkey, CpmmFeeRates>,
}

impl PoolCtx {
    /// The rates of the CPMM `amm_config` (a swap's account 2): the observed ones, else
    /// [`CpmmFeeRates::MAINNET`].
    pub fn cpmm_rates(&self, amm_config: Option<&Pubkey>) -> CpmmFeeRates {
        amm_config
            .and_then(|c| self.cpmm_configs.get(c))
            .copied()
            .unwrap_or(CpmmFeeRates::MAINNET)
    }
}

/// One `venue_pools` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolInfo {
    pub mint: Pubkey,
    pub market: Option<Pubkey>,
    pub sol_is_first: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminators_derive_from_the_event_names() {
        assert_eq!(
            anchor_event_discriminator("TradeEvent"),
            [0xbd, 0xdb, 0x7f, 0xd3, 0x4e, 0xe6, 0x61, 0xee]
        );
        assert_eq!(anchor_event_discriminator("SwapEvent"), CPMM_SWAP_EVENT);
        let d = anchor_event_discriminator("CompleteEvent");
        assert_eq!(venue_event_name(&PUMP, &d), Some("CompleteEvent"));
        assert_eq!(venue_event_name(&RAYDIUM_CPMM, &d), None);
    }

    /// The 2026-10-02 redeploys' new events (pump `SweepBondingCurveFeeEvent`, pAMM
    /// `SweepPoolFeeEvent`) are named on their own program only, and each discriminator is one
    /// the dumped redeploy emits and the 2026-09 build does not (`tests/fixtures/programs`: the
    /// 8 bytes, or an `lddw` pair holding them, in the ELF); no recorded transaction carries a
    /// sweep yet, so the body is built from `pump-rust-client` 0.2.0's IDL layout.
    #[test]
    fn the_2026_10_sweep_events_are_named_and_decoded() {
        let elf = |name: &str| {
            std::fs::read(format!(
                "{}/../../tests/fixtures/programs/{name}.so",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("a dumped ELF")
        };
        // The discriminator as bytes, or split over an `lddw` (imm32 at +4 and +12).
        let emits = |elf: &[u8], d: &[u8; 8]| {
            elf.windows(8).any(|w| w == d)
                || elf
                    .windows(16)
                    .any(|w| w[4..8] == d[..4] && w[12..16] == d[4..])
        };
        for (program, name, old, new) in [
            (PUMP, "SweepBondingCurveFeeEvent", "pump", "pump_2026_10"),
            (
                PUMP_AMM,
                "SweepPoolFeeEvent",
                "pump_amm",
                "pump_amm_2026_10",
            ),
        ] {
            let d = anchor_event_discriminator(name);
            assert_eq!(venue_event_name(&program, &d), Some(name));
            let other = if program == PUMP { PUMP_AMM } else { PUMP };
            assert_eq!(venue_event_name(&other, &d), None);
            assert!(emits(&elf(new), &d), "{name} in {new}.so");
            assert!(!emits(&elf(old), &d), "{name} not in {old}.so");
        }

        let (mint, curve, pool, to, payer) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let mut b = 1_791_000_000i64.to_le_bytes().to_vec();
        for k in [mint, curve, WSOL, to] {
            b.extend(k.to_bytes());
        }
        b.extend(81_280u64.to_le_bytes());
        b.push(1);
        let ev = lifecycle(&PUMP, "SweepBondingCurveFeeEvent", &b);
        assert_eq!(
            (ev.venue, ev.pool, ev.mint),
            ("pump_curve", Some(curve), Some(mint))
        );
        assert_eq!(ev.data["amount"], "81280");
        assert_eq!(ev.data["bucket"], 1);
        assert_eq!(ev.data["recipient"], key_json(&to));

        let mut b = 1_791_000_000i64.to_le_bytes().to_vec();
        for k in [pool, mint, WSOL, to, payer] {
            b.extend(k.to_bytes());
        }
        b.extend(452_066u64.to_le_bytes());
        b.push(0);
        let ev = lifecycle(&PUMP_AMM, "SweepPoolFeeEvent", &b);
        assert_eq!(
            (ev.venue, ev.pool, ev.mint),
            ("pump_swap", Some(pool), Some(mint))
        );
        assert_eq!(ev.data["amount"], "452066");
        assert_eq!(ev.data["payer"], key_json(&payer));
        // A truncated body keeps the raw hex only.
        let ev = lifecycle(&PUMP_AMM, "SweepPoolFeeEvent", &b[..40]);
        assert_eq!((ev.pool, ev.mint), (None, None));
        assert!(ev.data.get("amount").is_none());
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn key(s: &str) -> Pubkey {
        s.parse().expect("a base58 key")
    }

    /// A dumped pump ELF of `tests/fixtures/programs`.
    fn elf(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../tests/fixtures/programs/{name}.so",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("a dumped ELF")
    }

    /// The discriminator as bytes, or split over an `lddw` (imm32 at +4 and +12).
    fn emits(elf: &[u8], d: &[u8; 8]) -> bool {
        elf.windows(8).any(|w| w == d)
            || elf
                .windows(16)
                .any(|w| w[4..8] == d[..4] && w[12..16] == d[4..])
    }

    /// Mainnet transaction `2vVRr9eg…` (slot 456,311,576): a `buy_exact_quote_in_v3` that
    /// completed 98u8RJ's curve by the synthetic migration. Its pump events, discriminator ‖
    /// body, in emission order: `TradeEvent` (the curve part; `ix_name` "buy_exact_quote_in", the
    /// zero `fee_recipient`, 117 bytes after `ix_name` up to `creator_fee_unclaimed`), then
    /// `CompleteEvent`, then `PostCompleteBuyEvent` (the pool part).
    const SYNTH_TRADE: &str = concat!(
        "bddb7fd34ee661ee78e380968c2d567a4f249d6af042c1fd47eff2f8c76970980592f0a059faf59f21efcc49",
        "0000000066918142c502000001bb7dbbb7571973ec7aa5a970693da98d5e264a865b9371564c1933f48e5b4c",
        "e0bab4cd6a000000005684dac61a0000000098124c91fe000056d8b6ca130000000000000000000000000000",
        "00000000000000000000000000000000000000000000000000000000005f000000000000009e7bb300000000",
        "00bd65312ad6f576e789e7ea65ecc229335cc0458a1faee981cf69a0d422b0f7ee1e00000000000000c6ad38",
        "0000000000000000000000000000000000000000000000000000000000000000000000000000120000006275",
        "795f65786163745f71756f74655f696e00000000000000000000000000000000008813000000000000cfbd59",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000021efcc",
        "49000000005684dac61a00000056d8b6ca1300000000000000000000000000000000000000203db410000000",
        "00",
    );
    const SYNTH_POST: &str = concat!(
        "6fb06d8b316cd5fbbb7dbbb7571973ec7aa5a970693da98d5e264a865b9371564c1933f48e5b4ce078e38096",
        "8c2d567a4f249d6af042c1fd47eff2f8c76970980592f0a059faf59f1a8b113188240c560a9f8f8eca444fdb",
        "b9662a3053663c678662ad6b3446036900000000000000000000000000000000000000000000000000000000",
        "00000000bab4cd6a0000000019287e98ac05000020fa809d000000005f000000000000009e0c7f0100000000",
        "1e0000000000000083f67800000000004f86bf0000000000000801a92cbc000095f6d1c913000000e7df8210",
        "80b60000b5f0526714000000",
    );

    /// T-INDEXER `post_complete_buy_event_row`: `2vVRr9eg…`'s `PostCompleteBuyEvent` is named on
    /// pump only (its discriminator in the 2026-10-08 ELF and the 2026-10-13 one mainnet runs since
    /// the re-pin, not the 2026-10-02 one) and becomes a
    /// second pump trade row: a buy of `base_out` = 6,238,850,918,425 for `quote_in + fee +
    /// creator_fee` = 2,642,475,552 + 25,103,518 + 7,927,427 lamports, on the curve's key, priced
    /// on the would-be pool's reserves after the buy (200,661,149,081,575, 87,632,834,741) — the
    /// pool `migrate_v2` then created; the curve part's `TradeEvent` row bought `rT` before it.
    /// The same event with a non-SOL `quote_mint` gives no row.
    #[test]
    fn post_complete_buy_event_row() {
        let d = anchor_event_discriminator("PostCompleteBuyEvent");
        assert_eq!(d, [0x6f, 0xb0, 0x6d, 0x8b, 0x31, 0x6c, 0xd5, 0xfb]);
        for build in ["pump_2026_10_08", "pump_2026_10_13"] {
            assert!(emits(&elf(build), &d), "{build}");
        }
        assert!(!emits(&elf("pump_2026_10"), &d));
        let post = unhex(SYNTH_POST);
        assert_eq!(venue_event_name(&PUMP, &post), Some("PostCompleteBuyEvent"));
        assert_eq!(venue_event_name(&PUMP_AMM, &post), None);
        assert_eq!(post.len(), 8 + 224);
        let t = pump_post_complete_buy(&post[8..]).expect("a row");
        let (mint, curve) = (
            key("98u8RJzuhezFwkCFVdSe1bPoQ6e2DdRG79kQ19vkpump"),
            key("2ncbKy3KtAhiG2gWR2y6Mu5AtujUJMM8VAtpnwQFWJnC"),
        );
        assert_eq!(
            (t.venue, t.program, t.pool, t.mint, t.side),
            ("pump_curve", PUMP, curve, Some(mint), Side::Buy)
        );
        assert_eq!(t.pool, bonding_curve_of(&mint));
        assert_eq!(t.user, key("DctSSWvMLboJzaf96L9fKzFYrP1KgjY5CqYno4aQcTz7"));
        assert_eq!(t.token_amount, 6_238_850_918_425);
        assert_eq!(t.sol_amount, 2_642_475_552 + 25_103_518 + 7_927_427);
        assert_eq!(
            (
                t.fee_lp,
                t.fee_protocol,
                t.fee_creator,
                t.fee_creator_lamports
            ),
            (None, Some(25_103_518), Some(7_927_427), Some(7_927_427))
        );
        assert_eq!(
            (t.pool_token_reserve, t.pool_sol_reserve),
            (Some(200_661_149_081_575), Some(87_632_834_741))
        );
        assert_eq!(
            t.price_wad(),
            Some(u128::from(t.sol_amount) * WAD / 6_238_850_918_425)
        );
        assert!(
            pump_post_complete_buy(&post[8..8 + 223]).is_none(),
            "truncated"
        );
        // A SOL curve's event names the zero key as its `quote_mint` (@96). A curve quoted in
        // another token completes the same way (`E1bYRCNq…`, quoted in the pump coin
        // `W5vQGEpV…pump`), but its amounts are not lamports: no row (M8-SEC-6). WSOL is SOL.
        assert_eq!(post[8 + 96..8 + 128], [0u8; 32]);
        let quoted = |m: &Pubkey| {
            let mut p = post.clone();
            p[8 + 96..8 + 128].copy_from_slice(m.as_ref());
            pump_post_complete_buy(&p[8..])
        };
        assert_eq!(
            quoted(&key("W5vQGEpVR9wmWjWdKuEug1GmfwrUio44365KrDupump")),
            None
        );
        assert_eq!(quoted(&WSOL), Some(t.clone()));
        // The curve part, the same instruction's TradeEvent: `rT` = 3,046,247,600,486 tokens.
        let trade = unhex(SYNTH_TRADE);
        assert_eq!(venue_event_name(&PUMP, &trade), Some("TradeEvent"));
        let c = pump_trade(&trade[8..]).expect("the curve part");
        assert_eq!(
            (c.pool, c.side, c.token_amount),
            (curve, Side::Buy, 3_046_247_600_486)
        );
        assert_eq!(c.sol_amount, 1_238_167_329 + 11_762_590 + 3_714_502);
    }

    /// Mainnet transaction `2b9feozi…` (slot 456,312,196): a PumpSwap `multi_hop_swap` (data and
    /// its 26 accounts: 16 fixed, then two hop groups — a pump-curve hop `[CKL7…, H6Jj…, curve
    /// 5rw8…, …]` and a pool hop `[H6Jj…, PUMP pumpCmXq…, pool 8KPE…, …]`), the pool hop's
    /// `SellEvent`, and the curve hop's pump `TradeEvent` (`ix_name` "multi_hop_swap").
    const MULTI_HOP_DATA: &str = "2b644913e9f66f9415695644c62800000100000000000000";
    const MULTI_HOP_ACCOUNTS: [&str; 26] = [
        "3muYDxhtxKLvBvmwxo2DNVNsjG38LLWMmvhuewvyYPnK",
        "FMWFYnZGCNRMaUa9A2Wod4578DLsHwYt2Jers97bhm19",
        "5Mz744qqtHomwPraYiApeU4W8yUTVpjEETV1Lg7mDvMD",
        "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw",
        "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",
        "5vik3ikcH7cdgZpJK7CJNpfQ7vjGCRDZiPVdnwXySr6h",
        "GXiV2QopYESCgVAVBQZkotmMTRNGCKQddfg8Uabz8L9c",
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
        "11111111111111111111111111111111",
        "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR",
        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
        "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf",
        "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt",
        "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1",
        "CKL7wxBLZsBvH5PEvTkNPSUxX9NRfPv2qhhjW6SZpump",
        "H6JjfjdR1X4ANrmMEVoXJ5LecWcVtETbY1ncYQn4pump",
        "5rw8nszaTWWPjNbiRbm63UgrkUADpCq7Bjb3uPFTQguy",
        "99WtMX9uSCnSVYx6Fdxo5egeg3YJbiA3cUfqqiN9hv2C",
        "FhRTm9gEJftEXMMa6DW6uB4ZHXMmT8LBc5mHqbdh4NMM",
        "H6JjfjdR1X4ANrmMEVoXJ5LecWcVtETbY1ncYQn4pump",
        "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
        "8KPEBDrQrYgp7A6KE1VTPbc6LpqW3EAo6vNxcE5GTzYn",
        "A5hPoqGB3h2fbuSst2ENqkhrgepkFAgTzL46Xo2zNdAf",
        "FeG2m8fwYhNaQ3YzRzyncndFJevGmofqEdqe3DTVK6Wg",
    ];
    const MULTI_HOP_SELL: &str = concat!(
        "3e2f370aa503dc2a42b5cd6a000000001bd5f51c9500000001000000000000000000000000000000925c11e8",
        "02000000db739b945d67000092817912f7020000f37f8fab0400000000000000000000000000000000000000",
        "05000000000000002108990000000000f37f8fab04000000d277f6aa040000006cb76538b20d94d4affdbfb0",
        "8c91836abf8214f338c0aa64bc7f7ff1cdf4ac77293855b157a619774a0e2851f39f9708f358a41e4499c829",
        "20320d2fc32359d4000000000000000000000000000000000000000000000000000000000000000040ceee97",
        "0cd0fe2570a2cfd11c8944e1495eed1eafddb1131b9453b99c5ccaa800000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "00000000a287a4321962feede3b599cf034da82ce463855169260b2d9a992b3d978bcafb0000000000000000",
        "000000000000000000000000000000000000000000000000881300000000000010844c0000000000541dfb5c",
        "4a00000000000000000000000123061d7ab873030000000000000000000000000000000000c6e7c7eb0d0000",
        "00",
    );
    const MULTI_HOP_TRADE: &str = concat!(
        "bddb7fd34ee661eea822a12aecd83b1d66ba38d7c144014861a5685a68604e81a8bebfac4a94db2f00000000",
        "0000000015695644c628000000293855b157a619774a0e2851f39f9708f358a41e4499c82920320d2fc32359",
        "d442b5cd6a000000000000000000000000c0a61206cea203000000000000000000c00e00ba3ca40200000000",
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0090cfe4c0976e994deab13ac9ad0550141b5767fb2179bac36829d060e7bde4fa64000000000000008ae095",
        "81010000000000000000000000000000000000000000000000000000000000000000000000000e0000006d75",
        "6c74695f686f705f737761700000000000000000000000000000000000881300000000000000000000000000",
        "0000000000ef16f2d9a04b264a137105a2e9135dabd6cd28874d94d7a9232d10fbd533995fa5b58b9e960000",
        "006edc9cbcd70c000097f525e29700000064000000000000008ae095810100000062e6a5ea17000000",
    );

    /// T-INDEXER `multi_hop_swap_pool_hop_mints`: a pAMM event emitted by `multi_hop_swap` takes
    /// its pool's mints from the hop group whose third account is the event's pool (`2b9feozi…`:
    /// pool `8KPE…` → H6Jj… / PUMP), not from accounts 3 and 4 (there the `GlobalConfig` and
    /// `FeeConfig`, which made every multi-hop pAMM event undecodable); that pool is not a SOL
    /// pool, so it gives no trade row (M8-SEC-6), while the same hop paired with WSOL does. A
    /// `buy_v2` / `sell_v2` keeps accounts 3 and 4; a pool no group names is left to `venue_pools`.
    #[test]
    fn multi_hop_swap_pool_hop_mints() {
        let data = unhex(MULTI_HOP_DATA);
        let mut accounts: Vec<Pubkey> = MULTI_HOP_ACCOUNTS.iter().map(|s| key(s)).collect();
        let sell = unhex(MULTI_HOP_SELL);
        assert_eq!(venue_event_name(&PUMP_AMM, &sell), Some("SellEvent"));
        assert!(data.starts_with(&PUMP_AMM_MULTI_HOP_SWAP));
        assert!(emits(&elf("pump_amm_2026_10_08"), &PUMP_AMM_MULTI_HOP_SWAP));
        assert!(!emits(&elf("pump_amm_2026_10"), &PUMP_AMM_MULTI_HOP_SWAP));
        let body = &sell[8..];
        let pool = key("8KPEBDrQrYgp7A6KE1VTPbc6LpqW3EAo6vNxcE5GTzYn");
        assert_eq!(pump_amm_event_pool(body), Some(pool));
        let (h6, pump_mint) = (
            key("H6JjfjdR1X4ANrmMEVoXJ5LecWcVtETbY1ncYQn4pump"),
            key("pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn"),
        );
        assert_eq!(
            pump_amm_trade_mints(&data, &accounts, body),
            (Some(h6), Some(pump_mint))
        );
        assert_eq!(
            (accounts[3], accounts[4]),
            (
                shortwin::constants::PUMP_AMM_GLOBAL_CONFIG,
                shortwin::constants::PUMP_AMM_FEE_CONFIG
            ),
            "what the decoder read as the mints before"
        );
        assert!(pump_amm_trade(false, body, Some(h6), Some(pump_mint)).is_none());
        // The same hop against WSOL: a sale of H6Jj… on the hop's pool.
        accounts[16 + 5 + 1] = WSOL;
        let (base, quote) = pump_amm_trade_mints(&data, &accounts, body);
        let t = pump_amm_trade(false, body, base, quote).expect("a SOL pool");
        assert_eq!((t.pool, t.mint, t.side), (pool, Some(h6), Side::Sell));
        // A pool no hop group names; a single-pool instruction.
        accounts[16 + 5 + 2] = Pubkey::new_unique();
        assert_eq!(pump_amm_trade_mints(&data, &accounts, body), (None, None));
        let mut buy_v2 = data.clone();
        buy_v2[..8].copy_from_slice(&[0xb8, 0x17, 0xee, 0x61, 0x67, 0xc5, 0xd3, 0x3d]);
        assert_eq!(
            pump_amm_trade_mints(&buy_v2, &accounts, body),
            (Some(accounts[3]), Some(accounts[4]))
        );
    }

    /// `2b9feozi…`'s invocation tree as `getTransaction` returns it (slot 456,312,196): four
    /// top-level instructions; in the third, an aggregator (`FLASHX8D…`) invokes PumpSwap's
    /// `multi_hop_swap` as inner instruction 0 (height 2: the data and 26 accounts above), which
    /// invokes pump's `multi_hop_curve_swap` (height 3; its `TradeEvent` self-CPI at height 4)
    /// and emits its `SellEvent` by self-CPI at height 3, then a Whirlpool swap (height 2). The
    /// program ids, heights and both event payloads are the transaction's; the token
    /// transfers' and the other calls' data and accounts are left out (the decoder reads only
    /// the venue events and their parent nodes). `hop` replaces the pool hop group (accounts
    /// 21..26) before the transaction is built.
    fn multi_hop_tx(hop: impl FnOnce(&mut [Pubkey])) -> crate::ingest::raw::RawTx {
        use crate::ingest::raw::{Commitment, RawIx, RawTopIx, RawTx};
        let (flash, whirlpool, token, token22, system) = (
            key("FLASHX8DrLbgeR8FcfNV1F5krxYcYMUdBkrP1EPBtxB9"),
            key("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"),
            key("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
            key("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
            key("11111111111111111111111111111111"),
        );
        let node = |program: Pubkey, h: u32, data: Vec<u8>, accounts: Vec<Pubkey>| RawIx {
            program_id: program,
            accounts,
            data,
            stack_height: h,
        };
        let bare = |program: Pubkey, h: u32| node(program, h, vec![], vec![]);
        let event = |program: Pubkey, h: u32, hex: &str| {
            let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
            data.extend(unhex(hex));
            node(program, h, data, vec![])
        };
        let mut accounts: Vec<Pubkey> = MULTI_HOP_ACCOUNTS.iter().map(|s| key(s)).collect();
        hop(&mut accounts[MULTI_HOP_FIXED_ACCOUNTS + MULTI_HOP_GROUP..]);
        let top = |ix: RawIx, inner: Vec<RawIx>| RawTopIx { ix, inner };
        RawTx {
            signature: solana_signature::Signature::from([9u8; 64]),
            slot: 456_312_196,
            tx_index: 0,
            block_time: Some(1_791_866_178),
            commitment: Commitment::Finalized,
            fee_payer: key(MULTI_HOP_ACCOUNTS[0]),
            fee: 5_000,
            compute_units: Some(181_393),
            err: None,
            account_keys: vec![],
            instructions: vec![
                top(bare(flash, 1), vec![bare(system, 2), bare(token, 2)]),
                top(bare(flash, 1), vec![]),
                top(
                    bare(flash, 1),
                    vec![
                        node(PUMP_AMM, 2, unhex(MULTI_HOP_DATA), accounts),
                        node(
                            PUMP,
                            3,
                            unhex("e19a7516d756f66715695644c62800000100000002"),
                            vec![],
                        ),
                        bare(token22, 4),
                        bare(token22, 4),
                        event(PUMP, 4, MULTI_HOP_TRADE),
                        bare(token22, 3),
                        bare(token22, 3),
                        event(PUMP_AMM, 3, MULTI_HOP_SELL),
                        bare(whirlpool, 2),
                        bare(token22, 3),
                        bare(token, 3),
                        bare(token, 2),
                        bare(flash, 2),
                        bare(system, 2),
                    ],
                ),
                top(bare(system, 1), vec![]),
            ],
            logs: vec![],
        }
    }

    /// T-INDEXER `multi_hop_swap_decodes_through_the_aggregator_tree`: `decode()` on
    /// `2b9feozi…`'s tree ([`multi_hop_tx`]). The pool hop's `SellEvent` takes the data and
    /// accounts of its parent node (the inner `multi_hop_swap`, not the aggregator's top-level
    /// instruction): its pool `8KPE…` resolves to the hop's H6Jj… / PUMP mints, no SOL pool, so
    /// no PumpSwap row, although `venue_pools` here knows `8KPE…` as an H6Jj… / WSOL pool (the hop
    /// group wins over the fallback). With the hop paired with WSOL, the sale of H6Jj… on `8KPE…`.
    /// With a pool no hop group names, the fallback to `venue_pools`: the same sale, and no row
    /// when `venue_pools` does not know the pool either. The curve hop's pump `TradeEvent` (a
    /// curve quoted in H6Jj…, not SOL: its `quote_mint` past the shareholders vector) gives no
    /// `pump_curve` row in any case (M8-SEC-6, `trade_events_of_non_sol_curves_give_no_row`);
    /// with its `quote_mint` the zero key, a SOL curve's, the curve hop's row at its place.
    #[test]
    fn multi_hop_swap_decodes_through_the_aggregator_tree() {
        use crate::model::EvSource;
        let ids = crate::ingest::ProgramIds::default();
        let (pool, h6) = (
            key("8KPEBDrQrYgp7A6KE1VTPbc6LpqW3EAo6vNxcE5GTzYn"),
            key("H6JjfjdR1X4ANrmMEVoXJ5LecWcVtETbY1ncYQn4pump"),
        );
        let known = PoolCtx {
            pools: HashMap::from([(
                pool,
                PoolInfo {
                    mint: h6,
                    market: None,
                    sol_is_first: Some(false),
                },
            )]),
            ..PoolCtx::default()
        };
        let decode = |t: &crate::ingest::raw::RawTx, pools: &PoolCtx| {
            let d = super::super::decode(t, &ids, pools, &|_| None);
            assert!(d.quarantine.is_empty(), "{:?}", d.quarantine);
            let (amm, curve): (Vec<_>, Vec<_>) = d
                .trades
                .into_iter()
                .partition(|r| r.trade.program == PUMP_AMM);
            (amm, curve)
        };
        // The curve hop: a curve quoted in H6Jj…, no row in any case (M8-SEC-6).
        let trade = unhex(MULTI_HOP_TRADE);
        assert!(pump_trade(&trade[8..]).is_none(), "a non-SOL curve");
        let curve_row = |rows: &[super::super::TradeRow]| {
            assert!(rows.is_empty(), "{rows:?}");
        };

        // As on mainnet: no PumpSwap row, the fallback not taken.
        let real = multi_hop_tx(|_| {});
        for pools in [&known, &PoolCtx::default()] {
            let (amm, curve) = decode(&real, pools);
            assert!(amm.is_empty(), "{amm:?}");
            curve_row(&curve);
        }
        // Control: the same tree with the curve hop SOL-quoted gives its row at its place.
        let mut sol_tx = multi_hop_tx(|_| {});
        let hop = &mut sol_tx.instructions[2].inner[4];
        let body_at = anchor_lang::event::EVENT_IX_TAG_LE.len() + 8;
        let at = body_at + trade_quote_mint_at(&trade[8..]);
        assert_eq!(hop.data[at..at + 32], h6.to_bytes());
        hop.data[at..at + 32].fill(0);
        let sol_hop = pump_trade(&hop.data[body_at..]).expect("the SOL curve hop");
        let (amm, curve) = decode(&sol_tx, &PoolCtx::default());
        assert!(amm.is_empty(), "{amm:?}");
        assert_eq!(curve.len(), 1, "{curve:?}");
        let r = &curve[0];
        assert_eq!(
            (r.ix_index, r.ev_source, r.ev_seq, r.is_shortwin, r.market),
            (2, EvSource::Cpi, 4, false, None)
        );
        assert_eq!(r.trade, sol_hop);
        assert_eq!(
            r.trade.mint,
            Some(key("CKL7wxBLZsBvH5PEvTkNPSUxX9NRfPv2qhhjW6SZpump"))
        );

        // The hop paired with WSOL: the sale, from the hop group's mints.
        let (amm, curve) = decode(&multi_hop_tx(|g| g[1] = WSOL), &PoolCtx::default());
        curve_row(&curve);
        assert_eq!(amm.len(), 1, "{amm:?}");
        let sale = &amm[0];
        assert_eq!(
            (sale.ix_index, sale.ev_source, sale.ev_seq, sale.is_shortwin),
            (2, EvSource::Cpi, 7, false)
        );
        assert_eq!(
            (
                sale.trade.venue,
                sale.trade.pool,
                sale.trade.mint,
                sale.trade.side
            ),
            ("pump_swap", pool, Some(h6), Side::Sell)
        );
        let sell = unhex(MULTI_HOP_SELL);
        assert_eq!(
            Some(sale.trade.clone()),
            pump_amm_trade(false, &sell[8..], Some(h6), Some(WSOL))
        );

        // A pool no hop group names: `venue_pools` places it, or no row.
        let unnamed = multi_hop_tx(|g| g[2] = Pubkey::new_unique());
        let (amm, curve) = decode(&unnamed, &known);
        curve_row(&curve);
        assert_eq!(amm.len(), 1, "{amm:?}");
        assert_eq!(amm[0].trade, sale.trade, "the venue_pools fallback");
        let (amm, curve) = decode(&unnamed, &PoolCtx::default());
        curve_row(&curve);
        assert!(amm.is_empty(), "{amm:?}");
    }

    /// T-INDEXER (trailing bytes, `ix_name`, `fee_recipient`): the v3 `TradeEvent`s decode on
    /// their known prefix whatever their `ix_name` ("buy_exact_quote_in" of a v3 buy, not the
    /// docs' `*_v3`; "multi_hop_swap" of a multi-hop hop; any other label), with the zero
    /// `fee_recipient` v3 trades carry and the trailing fields up to `creator_fee_unclaimed`
    /// ignored (the multi-hop hop's curve, quoted in a pump coin, made SOL-quoted here: its
    /// `quote_mint` set to the zero key); a body cut inside the prefix does not decode.
    #[test]
    fn trade_events_decode_on_their_prefix_whatever_the_ix_name() {
        let prefix = TRADE_PREFIX;
        let fee_recipient = 32 + 8 + 8 + 1 + 32 + 8 + 4 * 8;
        let name_at = prefix + 1 + 4 * 8;
        for (hex, name) in [
            (SYNTH_TRADE, "buy_exact_quote_in"),
            (MULTI_HOP_TRADE, "multi_hop_swap"),
        ] {
            let mut payload = unhex(hex);
            let at = 8 + trade_quote_mint_at(&payload[8..]);
            payload[at..at + 32].fill(0);
            let body = &payload[8..];
            assert_eq!(body[fee_recipient..fee_recipient + 32], [0u8; 32], "{name}");
            let n = u32::from_le_bytes(body[name_at..name_at + 4].try_into().unwrap()) as usize;
            assert_eq!(&body[name_at + 4..name_at + 4 + n], name.as_bytes());
            assert!(body.len() > name_at + 4 + n + 8, "{name}: trailing fields");
            let t = pump_trade(body).expect("decodes");
            assert_eq!(
                pump_trade(&body[..prefix]),
                Some(t.clone()),
                "{name}: prefix only"
            );
            assert!(pump_trade(&body[..prefix - 1]).is_none());
            for other in ["buy_v3", "sell_v3", "buy_exact_quote_in_v3", "buy", "x"] {
                let mut b = body[..name_at].to_vec();
                b.extend(u32::try_from(other.len()).unwrap().to_le_bytes());
                b.extend(other.as_bytes());
                b.extend(&body[name_at + 4 + n..]);
                assert_eq!(pump_trade(&b), Some(t.clone()), "{name} as {other}");
            }
        }
    }

    /// A `TradeEvent` body's prefix up to `creator_fee`: what [`pump_trade`] needs to decode.
    const TRADE_PREFIX: usize = 32 + 8 + 8 + 1 + 32 + 8 + 4 * 8 + 32 + 8 + 8 + 32 + 8 + 8;

    /// Where a `TradeEvent` body's `quote_mint` starts: past `ix_name` and the shareholders
    /// vector (the walk of [`pump_trade_quote_mint`], counted here independently).
    fn trade_quote_mint_at(body: &[u8]) -> usize {
        let u32_at = |o: usize| u32::from_le_bytes(body[o..o + 4].try_into().unwrap()) as usize;
        let name_at = TRADE_PREFIX + 1 + 4 * 8;
        let shareholders_at = name_at + 4 + u32_at(name_at) + 1 + 4 * 8;
        shareholders_at + 4 + 34 * u32_at(shareholders_at)
    }

    /// T-INDEXER `trade_events_of_non_sol_curves_give_no_row` (M8-SEC-6): a pump `TradeEvent`
    /// gives a row on a SOL-quoted curve only. Its `quote_mint` follows the variable-length
    /// shareholders vector: the zero key on `2vVRr9eg…`'s SOL curve, `H6Jj…pump` on `2b9feozi…`'s
    /// curve hop, a curve quoted in a pump coin. That hop's event names `sol_amount` 0 and its
    /// amount, 646,905,050,533 H6Jj… units, only in `quote_amount`, but its `creator_fee`
    /// 6,469,050,506 is in H6Jj… units too: its row was a sale for 0 lamports with that "lamport"
    /// creator fee. The hop gives no row; with the zero key or WSOL in its place, either event
    /// gives the row of its prefix, and with a pump coin there, none. A shareholder entry (34
    /// bytes) moves `quote_mint` and is skipped. An event that ends before `quote_mint` (pump's
    /// layouts before it quoted curves in another token) is a SOL curve's.
    #[test]
    fn trade_events_of_non_sol_curves_give_no_row() {
        let h6 = key("H6JjfjdR1X4ANrmMEVoXJ5LecWcVtETbY1ncYQn4pump");
        let w5 = key("W5vQGEpVR9wmWjWdKuEug1GmfwrUio44365KrDupump");
        let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        for (hex, quote, sol_amount, quote_amount) in [
            (SYNTH_TRADE, Pubkey::default(), 1_238_167_329, 1_238_167_329),
            (MULTI_HOP_TRADE, h6, 0, 646_905_050_533),
        ] {
            let payload = unhex(hex);
            let body = &payload[8..];
            let at = trade_quote_mint_at(body);
            assert_eq!(body[at - 4..at], [0u8; 4], "{quote}: no shareholder");
            assert_eq!(body[at..at + 32], quote.to_bytes(), "{quote}");
            assert_eq!(
                body.len(),
                at + 32 + 6 * 8,
                "{quote}: the fields after quote_mint"
            );
            assert_eq!(
                (u64_at(body, 32), u64_at(body, at + 32)),
                (sol_amount, quote_amount),
                "{quote}: sol_amount, quote_amount"
            );
            let row = pump_trade(&body[..TRADE_PREFIX]).expect("the prefix decodes");
            let sol = quote == Pubkey::default();
            if !sol {
                assert_eq!(
                    (row.side, row.sol_amount, row.fee_creator_lamports),
                    (Side::Sell, 0, Some(6_469_050_506)),
                    "the row the hop gave before"
                );
            }
            assert_eq!(pump_trade(body), sol.then(|| row.clone()), "{quote}");
            let quoted = |m: &Pubkey, shareholders: u32| {
                let mut b = body[..at - 4].to_vec();
                b.extend(shareholders.to_le_bytes());
                for k in 0..shareholders {
                    b.extend(Pubkey::new_unique().to_bytes());
                    b.extend(u16::try_from(5_000 + k).unwrap().to_le_bytes());
                }
                b.extend(m.to_bytes());
                b.extend(&body[at + 32..]);
                pump_trade(&b)
            };
            for shareholders in [0, 1, 3] {
                assert_eq!(quoted(&quote, shareholders), pump_trade(body), "{quote}");
                assert_eq!(quoted(&Pubkey::default(), shareholders), Some(row.clone()));
                assert_eq!(quoted(&WSOL, shareholders), Some(row.clone()));
                assert_eq!(quoted(&h6, shareholders), None);
                assert_eq!(quoted(&w5, shareholders), None);
            }
            assert_eq!(
                pump_trade(&body[..at]),
                Some(row.clone()),
                "ends before quote_mint"
            );
        }
    }

    #[test]
    fn ray_logs_orient_by_the_pool_or_the_leg() {
        // SwapBaseIn, coin in (direction 2), 100 in, 7 out; coin = token (SOL is pc).
        let mut b = vec![3u8];
        for v in [100u64, 5, 2, 0, 1_000, 70, 7] {
            b.extend(v.to_le_bytes());
        }
        let log = ray_log(&b).expect("a swap");
        assert_eq!(log.amounts(), (100, 7));
        let t = ray_v4_trade(&log, Pubkey::default(), Pubkey::default(), false);
        assert_eq!((t.side, t.token_amount, t.sol_amount), (Side::Sell, 100, 7));
        assert_eq!(t.pool_token_reserve, Some(1_100));
        assert_eq!(t.pool_sol_reserve, Some(63));
        assert_eq!(ray_v4_orientation_from_leg(&log, "open_short"), Some(false));
        assert_eq!(ray_v4_orientation_from_leg(&log, "close_short"), Some(true));
        // SOL as the coin: coin in = SOL in = a buy.
        let t = ray_v4_trade(&log, Pubkey::default(), Pubkey::default(), true);
        assert_eq!((t.side, t.token_amount, t.sol_amount), (Side::Buy, 7, 100));
        assert!(ray_log(&b[..56]).is_none());
    }

    #[test]
    fn cpmm_swaps_value_a_token_side_creator_fee_at_the_trade_price() {
        let mut p = CPMM_SWAP_EVENT.to_vec();
        let tok = Pubkey::new_from_array([5; 32]);
        p.extend([9u8; 32]); // pool
        for v in [1_000u64, 500, 100, 40, 0, 0] {
            p.extend(v.to_le_bytes());
        }
        p.push(1); // base_input
        p.extend(tok.to_bytes()); // input: the token (a sell)
        p.extend(WSOL.to_bytes());
        p.extend(1u64.to_le_bytes()); // trade fee (tokens)
        p.extend(10u64.to_le_bytes()); // creator fee
        p.push(1); // on the input (tokens)
        let t = cpmm_swap(&p, Pubkey::default(), CpmmFeeRates::MAINNET).expect("a SOL swap");
        assert_eq!(
            (t.side, t.token_amount, t.sol_amount),
            (Side::Sell, 100, 40)
        );
        assert_eq!(t.fee_creator_lamports, Some(4));
        assert_eq!(t.mint, Some(tok));
        assert_eq!(t.price_wad(), Some(40 * WAD / 100));
    }

    /// A CPMM `SwapEvent` payload: SOL in (`buy`) or tokens in, the vaults before, the amounts and
    /// the fees.
    fn cpmm_event(
        buy: bool,
        before: (u64, u64),
        amounts: (u64, u64),
        fees: (u64, u64, bool),
    ) -> Vec<u8> {
        let tok = Pubkey::new_from_array([5; 32]);
        let mut p = CPMM_SWAP_EVENT.to_vec();
        p.extend([9u8; 32]); // pool
        for v in [before.0, before.1, amounts.0, amounts.1, 0, 0] {
            p.extend(v.to_le_bytes());
        }
        p.push(1); // base_input
        let (input, output) = if buy { (WSOL, tok) } else { (tok, WSOL) };
        p.extend(input.to_bytes());
        p.extend(output.to_bytes());
        p.extend(fees.0.to_le_bytes()); // trade fee (on the input)
        p.extend(fees.1.to_le_bytes()); // creator fee
        p.push(u8::from(fees.2)); // creator fee on the input
        p
    }

    #[test]
    fn cpmm_reserves_leave_the_counters_shares_out() {
        let rates = CpmmFeeRates {
            protocol: 120_000,
            fund: 40_000,
        };
        // trade fee 1_000: protocol 120 + fund 40 leave the input.
        assert_eq!(rates.counters_share(1_000), 160);
        assert_eq!(rates.counters_share(9), 1); // ⌊1.08⌋ + ⌊0.36⌋
        let r = |t: &Trade| (t.pool_sol_reserve, t.pool_token_reserve);
        // A buy, creator fee on the input (SOL): sol = 10_000 + 2_000 − 160 − 30.
        let p = cpmm_event(true, (10_000, 50_000), (2_000, 8_000), (1_000, 30, true));
        let t = cpmm_swap(&p, Pubkey::default(), rates).expect("a buy");
        assert_eq!(r(&t), (Some(11_810), Some(42_000)));
        // The same buy, creator fee on the output (tokens): tokens = 50_000 − 8_000 − 30.
        let p = cpmm_event(true, (10_000, 50_000), (2_000, 8_000), (1_000, 30, false));
        let t = cpmm_swap(&p, Pubkey::default(), rates).expect("a buy");
        assert_eq!(r(&t), (Some(11_840), Some(41_970)));
        // A sale (tokens in), creator fee on the output (SOL).
        let p = cpmm_event(false, (50_000, 10_000), (8_000, 1_500), (1_000, 30, false));
        let t = cpmm_swap(&p, Pubkey::default(), rates).expect("a sale");
        assert_eq!(r(&t), (Some(8_470), Some(57_840)));
        // M8's formula is the zero-rate case: the input keeps the whole trade fee.
        let zero = CpmmFeeRates {
            protocol: 0,
            fund: 0,
        };
        let t = cpmm_swap(&p, Pubkey::default(), zero).expect("a sale");
        assert_eq!(r(&t), (Some(8_470), Some(58_000)));
        // Unknown configs fall back to the mainnet rates.
        let ctx = PoolCtx::default();
        assert_eq!(ctx.cpmm_rates(None), CpmmFeeRates::MAINNET);
        assert_eq!(
            ctx.cpmm_rates(Some(&Pubkey::default())),
            CpmmFeeRates::MAINNET
        );
    }

    /// A pAMM event body: the 14 u64 prefix, the 7 keys, the creator fee fields, then (when `tail`)
    /// the 2026-09 fields up to `virtual_quote_reserves`.
    fn pamm_body(
        buy: bool,
        pool_base: u64,
        pool_quote: u64,
        base: u64,
        net: u64,
        vqr: Option<i128>,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [
            0, base, 0, 0, 0, pool_base, pool_quote, 0, 0, 0, 0, 0, net, net,
        ] {
            b.extend(u64::to_le_bytes(v));
        }
        b.extend([1u8; 32 * 7]);
        b.extend([0u8; 16]); // coin_creator_fee_basis_points, coin_creator_fee
        if let Some(vqr) = vqr {
            if buy {
                b.push(1); // track_volume
                b.extend([0u8; 40]);
                b.extend(3u32.to_le_bytes());
                b.extend(b"buy");
            }
            b.extend([0u8; 32]); // cashback and buyback fields
            b.extend(vqr.to_le_bytes());
            b.push(1); // can_boost
            b.extend([0u8; 24]);
        }
        b
    }

    #[test]
    fn pump_swap_quote_reserves_add_the_virtual_quote_reserves() {
        let tok = Some(Pubkey::new_from_array([5; 32]));
        let w = Some(WSOL);
        let r = |t: Trade| (t.pool_sol_reserve, t.pool_token_reserve);
        let buy = pamm_body(true, 1_000, 500, 10, 7, Some(170));
        let t = pump_amm_trade(true, &buy, tok, w).expect("a buy");
        assert_eq!(r(t), (Some(500 + 7 + 170), Some(990)));
        let sell = pamm_body(false, 1_000, 500, 10, 7, Some(-300));
        let t = pump_amm_trade(false, &sell, tok, w).expect("a sale");
        assert_eq!(r(t), (Some(500 - 7 - 300), Some(1_010)));
        // An event without the field (before the boost): the vault alone.
        let old = pamm_body(true, 1_000, 500, 10, 7, None);
        let t = pump_amm_trade(true, &old, tok, w).expect("a buy");
        assert_eq!(r(t), (Some(507), Some(990)));
        // A sum outside [0, u64::MAX] breaks pump's invariant: no quote reserve.
        let broken = pamm_body(false, 1_000, 500, 10, 7, Some(-1_000));
        let t = pump_amm_trade(false, &broken, tok, w).expect("a sale");
        assert_eq!(r(t), (None, Some(1_010)));
        // A reversed pool (base = WSOL): the virtual reserves are on the token (quote) side.
        let t = pump_amm_trade(true, &buy, w, tok).expect("a reversed sale");
        assert_eq!(r(t), (Some(990), Some(677)));
    }

    #[test]
    fn upgrades_of_venue_programs_only() {
        let mut data = 3u32.to_le_bytes().to_vec();
        data.extend([0u8; 4]);
        let pd = Pubkey::new_from_array([1; 32]);
        let accounts = vec![pd, RAYDIUM_CPMM];
        let u = venue_upgrade(&BPF_UPGRADEABLE_LOADER, &data, &accounts).expect("an upgrade");
        assert_eq!(u.venue, shortwin::constants::GUARD_RAYDIUM_CPMM);
        assert_eq!(u.programdata, pd);
        assert!(venue_upgrade(&BPF_UPGRADEABLE_LOADER, &data, &[pd, Pubkey::default()]).is_none());
        assert!(venue_upgrade(&PUMP, &data, &accounts).is_none());
    }
}
