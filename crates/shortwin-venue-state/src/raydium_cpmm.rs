//! Raydium CPMM `PoolState` and `AmmConfig` (ARCH §2.6, §3.2, §3.12, §4.1, §4.2, §4.7, §5.3;
//! R-RAY §2.4–§2.5, §3.3, §5.2–§5.3, §6.2–§6.3, §7; `raydium-cp-swap@59fb845`
//! `states/pool.rs`, `states/config.rs`, `instructions/swap_base_{input,output}.rs`).
//!
//! **`PoolState`** (Anchor zero-copy, `#[repr(C, packed)]`, 637 bytes with the 8-byte
//! discriminator `f7ede3f5d7c3de46` = `sha256("account:PoolState")[..8]`):
//!
//! | Offset | Field | | Offset | Field |
//! |---|---|---|---|---|
//! | 8 | `amm_config` | | 329 | `status` u8 (bit 0 deposit, bit 1 withdraw, **bit 2 swap** disabled) |
//! | 40 | `pool_creator` | | 330, 331, 332 | `lp_mint_decimals`, `mint_0_decimals`, `mint_1_decimals` |
//! | 72, 104 | `token_0_vault`, `token_1_vault` | | 333 | `lp_supply` u64 (internal LP counter) |
//! | 136 | `lp_mint` | | 341, 349 | `protocol_fees_token_0/1` u64 |
//! | 168, 200 | `token_0_mint`, `token_1_mint` | | 357, 365 | `fund_fees_token_0/1` u64 |
//! | 232, 264 | `token_0_program`, `token_1_program` | | 373 | `open_time` u64 |
//! | 296 | `observation_key` | | 389 | `creator_fee_on` u8 (0 both, 1 token 0, 2 token 1) |
//! | 328 | `auth_bump` u8 | | 390 | `enable_creator_fee` bool |
//! | | | | 397, 405 | `creator_fees_token_0/1` u64 |
//!
//! **`AmmConfig`** (Anchor Borsh account, 236 bytes, discriminator `daf42168cbcb2b6f`): `bump`
//! @8, `disable_create_pool` @9, `index` u16 @10, `trade_fee_rate` @12, `protocol_fee_rate` @20,
//! `fund_fee_rate` @28, `create_pool_fee` @36, `protocol_owner` @44, `fund_owner` @76,
//! `creator_fee_rate` @108 (rates in ppm).
//!
//! **Length tolerance.** Anchor loads a `PoolState` from any account at least 637 bytes long
//! (the zero-copy view ignores a longer tail) and an `AmmConfig` from any account holding its
//! 236 bytes; these readers accept exactly that: shorter accounts are `TooShort` (the venue
//! cannot load them either), longer ones read their known prefix.
//!
//! **Pricing reserves** [`PoolState::reserves`] (`PoolState::vault_amount_without_fee`):
//! `r_i = vault_i.amount − (protocol_fees_token_i + fund_fees_token_i + creator_fees_token_i)`;
//! a negative side is `InsufficientVault` (6012) and an overflowing counter sum `MathOverflow`
//! in the venue, `None` here. Collection instructions move a counter and the vault by the same
//! amount, so they never change the reserves (R-RAY §2.4).
//!
//! **Route predicate** [`PoolState::venue_executable`] (ARCH §5.3): `(status & 0b100) == 0 ∧
//! now ≥ open_time ∧ vault_i ≥ counters_i` for both sides, plus the checks without which the
//! venue reverts every buy of the pool (see the method), among them M3-SEC-1's
//! `t + c_eff > 0 ∨ the creator fee is not on the buy's input` ([`PoolState::fee_splits`]).
//!
//! **Fees** (ARCH §4.2): `t = trade_fee_rate`; `c = creator_fee_rate` only if
//! `enable_creator_fee`; the one-way fee is `φ = t + c` ppm exactly ([`PoolState::fee_ppm`]);
//! the creator fee is on the input when `creator_fee_on == 0` or it names the input token
//! ([`PoolState::creator_fee_on_input`]).
//!
//! **Pool age** [`age_start`] (ARCH §3.2, §4.7; SPEC §4 O13; R-RAY §7): `open_time` has been
//! clamped above the creation time since `a7330e3` and is immutable, so a pool whose
//! `open_time` is at or after `T_CLAMP` ages from `min(now, open_time)`; a pool whose
//! `open_time` precedes `T_CLAMP` stores no provable creation time and ages from the listing
//! (`now`), as the owner decided (O13, extended by R3B-12).
//!
//! The owner is supplied by the caller and checked here; key checks are the program's job.

use crate::bytes::{has_discriminator, key_at, u8_at, u16_at, u64_at};
use crate::mint::NATIVE_MINT;
use crate::raydium_v4::reached;
use crate::{Key, ReadError};

pub use crate::raydium_v4::locked_lp_bps;

/// Program id `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`.
pub const PROGRAM: Key = [
    169, 42, 90, 139, 79, 41, 89, 82, 132, 37, 80, 170, 147, 253, 91, 149, 181, 172, 230, 168, 235,
    146, 12, 147, 148, 46, 67, 105, 12, 32, 236, 115,
];

/// Vault / LP authority `GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL` = PDA(CPMM,
/// `["vault_and_lp_mint_auth_seed"]`), bump 253 (R-RAY §1.1).
pub const AUTHORITY: Key = [
    235, 0, 217, 245, 178, 146, 180, 33, 74, 199, 208, 55, 180, 214, 240, 100, 80, 185, 100, 96,
    13, 243, 115, 5, 43, 181, 232, 79, 47, 142, 154, 103,
];

/// `sha256("account:PoolState")[..8]`.
pub const POOL_DISCRIMINATOR: [u8; 8] = [0xf7, 0xed, 0xe3, 0xf5, 0xd7, 0xc3, 0xde, 0x46];
/// `sha256("account:AmmConfig")[..8]`.
pub const CONFIG_DISCRIMINATOR: [u8; 8] = [0xda, 0xf4, 0x21, 0x68, 0xcb, 0xcb, 0x2b, 0x6f];

/// `PoolState::LEN` (discriminator included): the smallest pool the venue loads.
pub const POOL_LEN: usize = 637;
/// `AmmConfig` with its discriminator: the smallest config the venue loads.
pub const CONFIG_LEN: usize = 236;

/// CPMM `open_time` clamp cut-off, 2024-06-01T00:00Z (ARCH §2.3.3, §4.7; R-RAY §7; the program's
/// `constants::T_CLAMP`).
pub const T_CLAMP: i64 = 1_717_200_000;

/// `FEE_RATE_DENOMINATOR_VALUE`: rates are ppm.
pub const FEE_RATE_DENOMINATOR: u64 = 1_000_000;

/// `status` bit: deposits disabled.
pub const STATUS_BIT_DEPOSIT: u8 = 1 << 0;
/// `status` bit: withdrawals disabled.
pub const STATUS_BIT_WITHDRAW: u8 = 1 << 1;
/// `status` bit: swaps disabled (`NotApproved`, 6000).
pub const STATUS_BIT_SWAP: u8 = 1 << 2;

/// `creator_fee_on`: the creator fee is on the input token of every swap.
pub const CREATOR_FEE_ON_BOTH: u8 = 0;
/// `creator_fee_on`: the creator fee is charged in token 0 (input of 0→1, output of 1→0).
pub const CREATOR_FEE_ON_TOKEN_0: u8 = 1;
/// `creator_fee_on`: the creator fee is charged in token 1.
pub const CREATOR_FEE_ON_TOKEN_1: u8 = 2;

/// `amm_config` (key).
pub const OFF_AMM_CONFIG: usize = 8;
/// `pool_creator` (key).
pub const OFF_POOL_CREATOR: usize = 40;
/// `token_0_vault` (key).
pub const OFF_TOKEN_0_VAULT: usize = 72;
/// `token_1_vault` (key).
pub const OFF_TOKEN_1_VAULT: usize = 104;
/// `lp_mint` (key).
pub const OFF_LP_MINT: usize = 136;
/// `token_0_mint` (key).
pub const OFF_TOKEN_0_MINT: usize = 168;
/// `token_1_mint` (key).
pub const OFF_TOKEN_1_MINT: usize = 200;
/// `token_0_program` (key).
pub const OFF_TOKEN_0_PROGRAM: usize = 232;
/// `token_1_program` (key).
pub const OFF_TOKEN_1_PROGRAM: usize = 264;
/// `observation_key` (key).
pub const OFF_OBSERVATION_KEY: usize = 296;
/// `auth_bump` (u8).
pub const OFF_AUTH_BUMP: usize = 328;
/// `status` (u8).
pub const OFF_STATUS: usize = 329;
/// `lp_mint_decimals` (u8).
pub const OFF_LP_MINT_DECIMALS: usize = 330;
/// `mint_0_decimals` (u8).
pub const OFF_MINT_0_DECIMALS: usize = 331;
/// `mint_1_decimals` (u8).
pub const OFF_MINT_1_DECIMALS: usize = 332;
/// `lp_supply` (u64).
pub const OFF_LP_SUPPLY: usize = 333;
/// `protocol_fees_token_0` (u64).
pub const OFF_PROTOCOL_FEES_TOKEN_0: usize = 341;
/// `protocol_fees_token_1` (u64).
pub const OFF_PROTOCOL_FEES_TOKEN_1: usize = 349;
/// `fund_fees_token_0` (u64).
pub const OFF_FUND_FEES_TOKEN_0: usize = 357;
/// `fund_fees_token_1` (u64).
pub const OFF_FUND_FEES_TOKEN_1: usize = 365;
/// `open_time` (u64).
pub const OFF_OPEN_TIME: usize = 373;
/// `recent_epoch` (u64).
pub const OFF_RECENT_EPOCH: usize = 381;
/// `creator_fee_on` (u8).
pub const OFF_CREATOR_FEE_ON: usize = 389;
/// `enable_creator_fee` (bool).
pub const OFF_ENABLE_CREATOR_FEE: usize = 390;
/// `creator_fees_token_0` (u64).
pub const OFF_CREATOR_FEES_TOKEN_0: usize = 397;
/// `creator_fees_token_1` (u64).
pub const OFF_CREATOR_FEES_TOKEN_1: usize = 405;

/// `AmmConfig.bump` (u8).
pub const OFF_CONFIG_BUMP: usize = 8;
/// `AmmConfig.disable_create_pool` (bool).
pub const OFF_CONFIG_DISABLE_CREATE_POOL: usize = 9;
/// `AmmConfig.index` (u16).
pub const OFF_CONFIG_INDEX: usize = 10;
/// `AmmConfig.trade_fee_rate` (u64, ppm).
pub const OFF_CONFIG_TRADE_FEE_RATE: usize = 12;
/// `AmmConfig.protocol_fee_rate` (u64, ppm of the trade fee).
pub const OFF_CONFIG_PROTOCOL_FEE_RATE: usize = 20;
/// `AmmConfig.fund_fee_rate` (u64, ppm of the trade fee).
pub const OFF_CONFIG_FUND_FEE_RATE: usize = 28;
/// `AmmConfig.create_pool_fee` (u64, lamports).
pub const OFF_CONFIG_CREATE_POOL_FEE: usize = 36;
/// `AmmConfig.creator_fee_rate` (u64, ppm).
pub const OFF_CONFIG_CREATOR_FEE_RATE: usize = 108;

/// The fields of a CPMM `PoolState` short.win reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolState {
    pub amm_config: Key,
    pub pool_creator: Key,
    pub token_0_vault: Key,
    pub token_1_vault: Key,
    pub lp_mint: Key,
    pub token_0_mint: Key,
    pub token_1_mint: Key,
    pub token_0_program: Key,
    pub token_1_program: Key,
    pub observation_key: Key,
    pub auth_bump: u8,
    pub status: u8,
    pub mint_0_decimals: u8,
    pub mint_1_decimals: u8,
    /// The internal LP counter (includes LP locked at initialisation and burned LP).
    pub lp_supply: u64,
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,
    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,
    pub open_time: u64,
    /// Raw `creator_fee_on`; only 0, 1 and 2 let the venue swap ([`CREATOR_FEE_ON_BOTH`] …).
    pub creator_fee_on: u8,
    pub enable_creator_fee: bool,
    pub creator_fees_token_0: u64,
    pub creator_fees_token_1: u64,
}

/// The fields of a CPMM `AmmConfig` short.win reads (rates in ppm).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AmmConfig {
    pub bump: u8,
    pub disable_create_pool: bool,
    pub index: u16,
    /// `t`: charged on every swap.
    pub trade_fee_rate: u64,
    /// Protocol share of the trade fee (moved into the excluded counters).
    pub protocol_fee_rate: u64,
    /// Fund share of the trade fee (moved into the excluded counters).
    pub fund_fee_rate: u64,
    pub create_pool_fee: u64,
    /// `c`: charged only on pools with `enable_creator_fee`.
    pub creator_fee_rate: u64,
}

/// A `bool` byte: 0 or 1, anything else is malformed (Rust's `bool` in a zero-copy account;
/// the venue's behaviour on another byte is undefined, so the pool does not parse).
fn bool_at(data: &[u8], offset: usize) -> Result<bool, ReadError> {
    match u8_at(data, offset) {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ReadError::Malformed),
    }
}

/// Reads a `PoolState`: owner == [`PROGRAM`], discriminator [`POOL_DISCRIMINATOR`], at least
/// [`POOL_LEN`] bytes, `enable_creator_fee` ∈ {0, 1}.
pub fn read_pool(owner: &Key, data: &[u8]) -> Result<PoolState, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &POOL_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    if data.len() < POOL_LEN {
        return Err(ReadError::TooShort);
    }
    Ok(PoolState {
        amm_config: key_at(data, OFF_AMM_CONFIG),
        pool_creator: key_at(data, OFF_POOL_CREATOR),
        token_0_vault: key_at(data, OFF_TOKEN_0_VAULT),
        token_1_vault: key_at(data, OFF_TOKEN_1_VAULT),
        lp_mint: key_at(data, OFF_LP_MINT),
        token_0_mint: key_at(data, OFF_TOKEN_0_MINT),
        token_1_mint: key_at(data, OFF_TOKEN_1_MINT),
        token_0_program: key_at(data, OFF_TOKEN_0_PROGRAM),
        token_1_program: key_at(data, OFF_TOKEN_1_PROGRAM),
        observation_key: key_at(data, OFF_OBSERVATION_KEY),
        auth_bump: u8_at(data, OFF_AUTH_BUMP),
        status: u8_at(data, OFF_STATUS),
        mint_0_decimals: u8_at(data, OFF_MINT_0_DECIMALS),
        mint_1_decimals: u8_at(data, OFF_MINT_1_DECIMALS),
        lp_supply: u64_at(data, OFF_LP_SUPPLY),
        protocol_fees_token_0: u64_at(data, OFF_PROTOCOL_FEES_TOKEN_0),
        protocol_fees_token_1: u64_at(data, OFF_PROTOCOL_FEES_TOKEN_1),
        fund_fees_token_0: u64_at(data, OFF_FUND_FEES_TOKEN_0),
        fund_fees_token_1: u64_at(data, OFF_FUND_FEES_TOKEN_1),
        open_time: u64_at(data, OFF_OPEN_TIME),
        creator_fee_on: u8_at(data, OFF_CREATOR_FEE_ON),
        enable_creator_fee: bool_at(data, OFF_ENABLE_CREATOR_FEE)?,
        creator_fees_token_0: u64_at(data, OFF_CREATOR_FEES_TOKEN_0),
        creator_fees_token_1: u64_at(data, OFF_CREATOR_FEES_TOKEN_1),
    })
}

/// Reads an `AmmConfig`: owner == [`PROGRAM`], discriminator [`CONFIG_DISCRIMINATOR`], at
/// least [`CONFIG_LEN`] bytes, `disable_create_pool` ∈ {0, 1} (Borsh's `bool`).
pub fn read_config(owner: &Key, data: &[u8]) -> Result<AmmConfig, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &CONFIG_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    if data.len() < CONFIG_LEN {
        return Err(ReadError::TooShort);
    }
    Ok(AmmConfig {
        bump: u8_at(data, OFF_CONFIG_BUMP),
        disable_create_pool: bool_at(data, OFF_CONFIG_DISABLE_CREATE_POOL)?,
        index: u16_at(data, OFF_CONFIG_INDEX),
        trade_fee_rate: u64_at(data, OFF_CONFIG_TRADE_FEE_RATE),
        protocol_fee_rate: u64_at(data, OFF_CONFIG_PROTOCOL_FEE_RATE),
        fund_fee_rate: u64_at(data, OFF_CONFIG_FUND_FEE_RATE),
        create_pool_fee: u64_at(data, OFF_CONFIG_CREATE_POOL_FEE),
        creator_fee_rate: u64_at(data, OFF_CONFIG_CREATOR_FEE_RATE),
    })
}

/// The start of a CPMM pool's age (`graduated_at`, ARCH §3.2, §4.7; SPEC §4 O13; R-RAY §7):
/// `min(now, open_time)` when `open_time ≥ T_CLAMP` (a valid lower bound on the pool's age:
/// `open_time ≥ creation + 1` since the clamp), else `now` — a pool whose `open_time` precedes
/// `T_CLAMP` stores no provable creation time, so its age counts from the listing (O13, extended
/// by R3B-12: Tier C for 24 h, at most Tier B until day 7). Never in the future.
pub fn age_start(open_time: u64, now: i64) -> i64 {
    let open = i64::try_from(open_time).unwrap_or(i64::MAX);
    if open < T_CLAMP { now } else { open.min(now) }
}

impl PoolState {
    /// `protocol + fund + creator` counters of token 0, `None` on overflow (`MathOverflow`).
    pub fn counters_0(&self) -> Option<u64> {
        self.protocol_fees_token_0
            .checked_add(self.fund_fees_token_0)?
            .checked_add(self.creator_fees_token_0)
    }

    /// `protocol + fund + creator` counters of token 1, `None` on overflow (`MathOverflow`).
    pub fn counters_1(&self) -> Option<u64> {
        self.protocol_fees_token_1
            .checked_add(self.fund_fees_token_1)?
            .checked_add(self.creator_fees_token_1)
    }

    /// The pricing reserves `(r0, r1)` = `vault_i − counters_i` for vaults holding
    /// `vault_0_amount` and `vault_1_amount`; `None` if a side is negative (`InsufficientVault`)
    /// or a counter sum overflows (`MathOverflow`) — every swap reverts then.
    pub fn reserves(&self, vault_0_amount: u64, vault_1_amount: u64) -> Option<(u64, u64)> {
        Some((
            vault_0_amount.checked_sub(self.counters_0()?)?,
            vault_1_amount.checked_sub(self.counters_1()?)?,
        ))
    }

    /// Which side is WSOL: `Some(true)` if token 0 is WSOL and token 1 is not, `Some(false)`
    /// for the reverse, `None` otherwise. WSOL (`So111…`, SPL Token) sorts first unless the
    /// other mint's key is below `0x06…` (R-RAY §2.4). The Token-2022 native mint is never WSOL.
    pub fn sol_is_token0(&self) -> Option<bool> {
        match (
            self.token_0_mint == NATIVE_MINT,
            self.token_1_mint == NATIVE_MINT,
        ) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    }

    /// The status and time part of the route predicate: swap bit clear and `now ≥ open_time`
    /// (`NotApproved` otherwise).
    pub fn swap_enabled(&self, now: i64) -> bool {
        self.status & STATUS_BIT_SWAP == 0 && reached(now, self.open_time)
    }

    /// ARCH §5.3's venue predicate for CPMM on the pool's vault balances and its `config`:
    /// [`swap_enabled`] and `vault_i ≥ counters_i` for both sides. Also required, because the
    /// venue reverts the swaps an exit needs otherwise (tightenings of ARCH §5.3 on states in
    /// which no buyback can execute):
    ///
    /// - both reserves non-zero (`token_price_x32` divides by each);
    /// - `creator_fee_on ≤ 2` (`InvalidFeeModel`);
    /// - the pool quotes WSOL and a **buy** (WSOL in) can charge its fee on `config`
    ///   ([`fee_splits`]; M3-SEC-1): with the creator fee on the buy's input and
    ///   `t + c_eff = 0` — a config whose trade fee the admin set to 0 (`update_amm_config`
    ///   accepts it), on a pool without an enabled creator fee — every buy reverts with
    ///   `ZeroTradingTokens`. A sell-only failure (the creator fee on the token input) keeps the
    ///   route: exits still execute, and opens fail on their undefined sell quote.
    ///
    /// [`swap_enabled`]: PoolState::swap_enabled
    /// [`fee_splits`]: PoolState::fee_splits
    pub fn venue_executable(
        &self,
        config: &AmmConfig,
        now: i64,
        vault_0_amount: u64,
        vault_1_amount: u64,
    ) -> bool {
        self.swap_enabled(now)
            && self.creator_fee_on <= CREATOR_FEE_ON_TOKEN_1
            && self
                .sol_is_token0()
                .and_then(|sol0| self.fee_splits(config, sol0))
                == Some(true)
            && self
                .reserves(vault_0_amount, vault_1_amount)
                .is_some_and(|(r0, r1)| r0 > 0 && r1 > 0)
    }

    /// Whether a swap whose input is token 0 (`input_is_token0`) can charge its fee on
    /// `config` (`split_creator_fee`; M3-SEC-1). With the creator fee on the input the venue
    /// takes one combined fee `⌈a·(t + c)/10⁶⌉` and splits it as `⌊total·c/(t + c)⌋`, which
    /// fails every such swap, whatever its amount or mode, when `t + c = 0`
    /// (`ZeroTradingTokens`, 6006; `c` the effective creator rate, 0 unless the pool enabled
    /// it). A fee on the output, or any non-zero `t + c`, splits. `None` for
    /// `creator_fee_on > 2` (`InvalidFeeModel`).
    pub fn fee_splits(&self, config: &AmmConfig, input_is_token0: bool) -> Option<bool> {
        let on_input = self.creator_fee_on_input(input_is_token0)?;
        Some(!on_input || config.trade_fee_rate != 0 || self.creator_fee_rate(config) != 0)
    }

    /// The creator fee rate the pool charges: `config.creator_fee_rate` if
    /// `enable_creator_fee`, else 0 (`adjust_creator_fee_rate`).
    pub fn creator_fee_rate(&self, config: &AmmConfig) -> u64 {
        if self.enable_creator_fee {
            config.creator_fee_rate
        } else {
            0
        }
    }

    /// Whether the creator fee of a swap whose input is token 0 (`input_is_token0`) is charged
    /// on its input (`is_creator_fee_on_input`): always for `creator_fee_on == 0`, else when it
    /// names the input token. `None` for `creator_fee_on > 2` (the venue's `InvalidFeeModel`).
    pub fn creator_fee_on_input(&self, input_is_token0: bool) -> Option<bool> {
        match self.creator_fee_on {
            CREATOR_FEE_ON_BOTH => Some(true),
            CREATOR_FEE_ON_TOKEN_0 => Some(input_is_token0),
            CREATOR_FEE_ON_TOKEN_1 => Some(!input_is_token0),
            _ => None,
        }
    }

    /// The one-way fee `φ = t + c` ppm exactly (ARCH §4.2; R3A-3 iii), with `c` only if the
    /// pool enabled the creator fee; `None` unless `t + c < 10⁶` (the venue's own invariant
    /// `update_amm_config` asserts; above it `calculate_pre_fee_amount` has no answer).
    pub fn fee_ppm(&self, config: &AmmConfig) -> Option<u32> {
        let phi = config
            .trade_fee_rate
            .checked_add(self.creator_fee_rate(config))?;
        if phi >= FEE_RATE_DENOMINATOR {
            return None;
        }
        u32::try_from(phi).ok()
    }

    /// The locked LP share λ in bps for an LP mint with `lp_mint_supply` ([`locked_lp_bps`] on
    /// `lp_supply`).
    pub fn locked_lp_bps(&self, lp_mint_supply: u64) -> Option<u64> {
        locked_lp_bps(self.lp_supply, lp_mint_supply)
    }

    /// [`age_start`] of this pool.
    pub fn age_start(&self, now: i64) -> i64 {
        age_start(self.open_time, now)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::mint::{TOKEN_2022_PROGRAM, TOKEN_PROGRAM, read_base};
    use crate::token_account;

    /// Mainnet dumps at one slot (provenance: `tests/fixtures/manifest.json`, group
    /// `raydium_state`; `scripts/dump-fixtures.py`).
    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../tests/fixtures/accounts/raydium/",
                $name,
                ".bin"
            ))
        };
    }

    const USELESS: &[u8] = fixture!("cpmm_useless_pool");
    const USELESS_V0: &[u8] = fixture!("cpmm_useless_token_0_vault");
    const USELESS_V1: &[u8] = fixture!("cpmm_useless_token_1_vault");
    const USELESS_LP: &[u8] = fixture!("cpmm_useless_lp_mint");
    const CF: &[u8] = fixture!("cpmm_creator_fee_pool");
    const CF_V0: &[u8] = fixture!("cpmm_creator_fee_token_0_vault");
    const CF_V1: &[u8] = fixture!("cpmm_creator_fee_token_1_vault");
    const T22: &[u8] = fixture!("cpmm_token2022_pool");
    const T22_V1: &[u8] = fixture!("cpmm_token2022_token_1_vault");
    const T22_MINT: &[u8] = fixture!("cpmm_token2022_token_mint");
    const T22_LP: &[u8] = fixture!("cpmm_token2022_lp_mint");
    const WT1: &[u8] = fixture!("cpmm_wsol_token1_pool");
    const WT1_V0: &[u8] = fixture!("cpmm_wsol_token1_token_0_vault");
    const WT1_V1: &[u8] = fixture!("cpmm_wsol_token1_token_1_vault");
    const WT1_LP: &[u8] = fixture!("cpmm_wsol_token1_lp_mint");
    const BOTH: &[u8] = fixture!("cpmm_creator_both_pool");
    const OBSERVATION: &[u8] = fixture!("cpmm_useless_observation");

    /// Every live `AmmConfig`, by index (R-RAY §2.5).
    const CONFIGS: [&[u8]; 21] = [
        fixture!("cpmm_config_00"),
        fixture!("cpmm_config_01"),
        fixture!("cpmm_config_02"),
        fixture!("cpmm_config_03"),
        fixture!("cpmm_config_04"),
        fixture!("cpmm_config_05"),
        fixture!("cpmm_config_06"),
        fixture!("cpmm_config_07"),
        fixture!("cpmm_config_08"),
        fixture!("cpmm_config_09"),
        fixture!("cpmm_config_10"),
        fixture!("cpmm_config_11"),
        fixture!("cpmm_config_12"),
        fixture!("cpmm_config_13"),
        fixture!("cpmm_config_14"),
        fixture!("cpmm_config_15"),
        fixture!("cpmm_config_16"),
        fixture!("cpmm_config_17"),
        fixture!("cpmm_config_18"),
        fixture!("cpmm_config_19"),
        fixture!("cpmm_config_20"),
    ];

    /// R-RAY §2.5's table: (trade fee, creator fee) in ppm of configs 0–20.
    const RATES: [(u64, u64); 21] = [
        (2_500, 500),
        (10_000, 500),
        (20_000, 500),
        (40_000, 500),
        (5_000, 500),
        (3_000, 500),
        (15_000, 500),
        (25_000, 500),
        (2_500, 5_500),
        (2_500, 7_500),
        (5_000, 4_000),
        (2_500, 10_000),
        (3_000, 12_000),
        (300, 9_700),
        (7_500, 5_000),
        (5_000, 7_500),
        (50, 14_950),
        (4_000, 8_500),
        (7_000, 8_000),
        (3_500, 6_500),
        (2_500, 12_500),
    ];

    /// `D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2` (config index 0).
    const CONFIG_0: Key = [
        179, 33, 63, 186, 139, 249, 200, 127, 169, 30, 71, 129, 150, 40, 195, 131, 224, 11, 234,
        126, 152, 199, 160, 62, 3, 186, 16, 105, 207, 195, 246, 243,
    ];
    /// `HgNPDD8bpbSrGyHegiCT5xrYxHTfwLfZydwGkjNCJRKA` (USELESS token 0 vault, WSOL).
    const USELESS_VAULT_0: Key = [
        247, 208, 209, 244, 220, 247, 30, 178, 47, 45, 127, 187, 98, 166, 118, 47, 148, 5, 61, 198,
        239, 168, 238, 99, 238, 56, 4, 207, 80, 58, 22, 245,
    ];
    /// `9xsCiNwYQXM3ZeHFSVj9JQdP1vREJREpN23f6wvxA1ty` (USELESS token 1 vault).
    const USELESS_VAULT_1: Key = [
        133, 45, 52, 20, 198, 47, 141, 113, 87, 105, 186, 77, 148, 179, 80, 155, 240, 200, 130,
        140, 175, 50, 35, 185, 63, 35, 253, 176, 51, 202, 219, 190,
    ];
    /// `Dz9mQ9NzkBcCsuGPFJ3r1bS4wgqKMHBPiVuniW8Mbonk` (USELESS).
    const USELESS_MINT: Key = [
        192, 240, 66, 170, 172, 159, 130, 55, 70, 204, 16, 225, 4, 211, 99, 85, 145, 207, 45, 122,
        52, 113, 174, 111, 7, 39, 192, 143, 219, 39, 77, 133,
    ];
    /// `4UdSz2kMddtX4woMmdgkWg75fdBP8FgYwqfkh4ri7mnD` (USELESS observation).
    const USELESS_OBSERVATION: Key = [
        51, 166, 244, 89, 200, 181, 133, 234, 123, 154, 28, 30, 146, 51, 12, 223, 29, 217, 13, 255,
        118, 84, 10, 193, 218, 168, 166, 231, 95, 227, 226, 78,
    ];
    /// `5rASbyrUYh4eVmZpgN6MxVY2w83dC4PFg9U9WYc9HW7g` (USELESS LP mint).
    const USELESS_LP_MINT: Key = [
        72, 6, 225, 255, 252, 93, 36, 125, 57, 240, 122, 22, 22, 166, 99, 118, 212, 152, 245, 25,
        34, 182, 32, 51, 180, 93, 74, 218, 60, 207, 206, 183,
    ];

    fn pool(data: &[u8]) -> PoolState {
        read_pool(&PROGRAM, data).unwrap()
    }

    fn config(i: usize) -> AmmConfig {
        read_config(&PROGRAM, CONFIGS[i]).unwrap()
    }

    fn amount(program: &Key, data: &[u8]) -> u64 {
        token_account::read(program, data).unwrap().amount
    }

    #[test]
    fn layout_constants() {
        assert_eq!(POOL_LEN, 8 + 10 * 32 + 5 + 8 * 7 + 2 + 6 + 2 * 8 + 8 * 28);
        assert_eq!(OFF_CREATOR_FEES_TOKEN_1 + 8 + 28 * 8, POOL_LEN);
        assert_eq!(CONFIG_LEN, 8 + 1 + 1 + 2 + 4 * 8 + 2 * 32 + 8 + 15 * 8);
        assert_eq!(OFF_CONFIG_CREATOR_FEE_RATE, 44 + 64);
    }

    /// USELESS decodes to R-RAY §2.4's table (keys, decimals, open time, the status and
    /// creator-fee fields), with the dump's counters; the reserves exclude the counters.
    #[test]
    fn reads_the_useless_pool() {
        let p = pool(USELESS);
        assert_eq!(p.amm_config, CONFIG_0);
        assert_eq!(
            (p.token_0_vault, p.token_1_vault),
            (USELESS_VAULT_0, USELESS_VAULT_1)
        );
        assert_eq!(
            (p.token_0_mint, p.token_1_mint),
            (NATIVE_MINT, USELESS_MINT)
        );
        assert_eq!(
            (p.token_0_program, p.token_1_program),
            (TOKEN_PROGRAM, TOKEN_PROGRAM)
        );
        assert_eq!(p.observation_key, USELESS_OBSERVATION);
        assert_eq!(p.lp_mint, USELESS_LP_MINT);
        assert_eq!((p.auth_bump, p.status), (253, 0));
        assert_eq!((p.mint_0_decimals, p.mint_1_decimals), (9, 6));
        assert_eq!(p.open_time, 1_746_886_997);
        assert_eq!((p.creator_fee_on, p.enable_creator_fee), (0, false));
        assert_eq!((p.creator_fees_token_0, p.creator_fees_token_1), (0, 0));
        assert_eq!(p.sol_is_token0(), Some(true));
        assert_eq!(
            (p.protocol_fees_token_0, p.fund_fees_token_0),
            (12_468_617, 33_032_085)
        );
        assert_eq!(
            (p.protocol_fees_token_1, p.fund_fees_token_1),
            (3_439_330, 26_984_368)
        );
        for (v, mint) in [(USELESS_V0, NATIVE_MINT), (USELESS_V1, USELESS_MINT)] {
            let v = token_account::read(&TOKEN_PROGRAM, v).unwrap();
            assert_eq!((v.mint, v.owner), (mint, AUTHORITY));
        }
        let (v0, v1) = (
            amount(&TOKEN_PROGRAM, USELESS_V0),
            amount(&TOKEN_PROGRAM, USELESS_V1),
        );
        assert_eq!((v0, v1), (23_484_576_849_442, 10_533_423_083_329));
        assert_eq!(
            p.reserves(v0, v1),
            Some((
                23_484_576_849_442 - 12_468_617 - 33_032_085,
                10_533_423_083_329 - 3_439_330 - 26_984_368
            ))
        );
        assert!(p.venue_executable(&config(0), 1_790_563_273, v0, v1));
        assert_eq!(p.fee_ppm(&config(0)), Some(2_500));
        // λ = 92.00 % (R-RAY §6.3 table).
        let lp = read_base(&TOKEN_PROGRAM, USELESS_LP).unwrap();
        assert_eq!(p.locked_lp_bps(lp.supply), Some(9_200));
        // open_time is after the clamp: the pool's age starts at its open time.
        assert_eq!(p.age_start(1_790_563_273), 1_746_886_997);
    }

    /// The creator-fee pool: the fee is on token 0 = WSOL (buys pay it on the input, sells on
    /// the output), and the uncollected creator counter is a large part of the WSOL vault
    /// (R-RAY §2.4: raw vault balances overstate such pools).
    #[test]
    fn reads_the_creator_fee_pool() {
        let p = pool(CF);
        assert_eq!(p.sol_is_token0(), Some(true));
        assert_eq!(
            (p.creator_fee_on, p.enable_creator_fee),
            (CREATOR_FEE_ON_TOKEN_0, true)
        );
        assert_eq!(p.creator_fees_token_0, 245_489_989_302);
        let c = config(0);
        assert_eq!(p.creator_fee_rate(&c), 500);
        assert_eq!(p.fee_ppm(&c), Some(3_000));
        // SOL in (token 0 in): on the input; token in: on the (SOL) output.
        assert_eq!(p.creator_fee_on_input(true), Some(true));
        assert_eq!(p.creator_fee_on_input(false), Some(false));
        let (v0, v1) = (amount(&TOKEN_PROGRAM, CF_V0), amount(&TOKEN_PROGRAM, CF_V1));
        let (r0, r1) = p.reserves(v0, v1).unwrap();
        assert_eq!(r0, v0 - 36_250_655 - 14_693_908 - 245_489_989_302);
        assert_eq!(r1, v1 - 1_558_685_129_618 - 746_923_876_303);
        // The counters hold 29.95 % of the WSOL vault.
        assert!(r0 * 100 < v0 * 71 && r0 * 100 > v0 * 70);
    }

    /// The Token-2022 pool: token 1 lives under Token-2022 (its vault too), its mint carries
    /// only MetadataPointer (18) and TokenMetadata (19), and all its LP is burned (λ = 100 %).
    #[test]
    fn reads_the_token2022_pool() {
        let p = pool(T22);
        assert_eq!(
            (p.token_0_program, p.token_1_program),
            (TOKEN_PROGRAM, TOKEN_2022_PROGRAM)
        );
        assert_eq!(p.sol_is_token0(), Some(true));
        let v1 = token_account::read(&TOKEN_2022_PROGRAM, T22_V1).unwrap();
        assert_eq!((v1.mint, v1.owner), (p.token_1_mint, AUTHORITY));
        assert_eq!(
            read_base(&TOKEN_2022_PROGRAM, T22_MINT).unwrap().decimals,
            9
        );
        // It passes the SPEC §3 filter (pump.fun's Token-2022 shape).
        assert!(crate::mint::check(&p.token_1_mint, &TOKEN_2022_PROGRAM, T22_MINT).is_ok());
        let lp = read_base(&TOKEN_PROGRAM, T22_LP).unwrap();
        assert_eq!(lp.supply, 0);
        assert_eq!(p.locked_lp_bps(lp.supply), Some(10_000));
    }

    /// WSOL as token 1, creator fee on token 1 (= WSOL) at config 10's 0.40 %; LP is only the
    /// 100 withheld at initialisation, so λ rounds to 0 (a locker-launched pool, R-RAY §12.3).
    #[test]
    fn reads_the_wsol_token1_pool() {
        let p = pool(WT1);
        assert_eq!(p.sol_is_token0(), Some(false));
        assert_eq!(p.token_1_mint, NATIVE_MINT);
        assert_eq!(
            (p.creator_fee_on, p.enable_creator_fee),
            (CREATOR_FEE_ON_TOKEN_1, true)
        );
        let c = read_config(&PROGRAM, CONFIGS[10]).unwrap();
        assert_eq!(p.amm_config, {
            // The pool names config 10.
            let mut k = [0u8; 32];
            k.copy_from_slice(&WT1[OFF_AMM_CONFIG..OFF_AMM_CONFIG + 32]);
            k
        });
        assert_eq!(c.index, 10);
        assert_eq!(p.fee_ppm(&c), Some(9_000));
        // Buy = WSOL (token 1) in: the fee is on the input; sell = token 0 in: on the output.
        assert_eq!(p.creator_fee_on_input(false), Some(true));
        assert_eq!(p.creator_fee_on_input(true), Some(false));
        let (v0, v1) = (
            amount(&TOKEN_PROGRAM, WT1_V0),
            amount(&TOKEN_PROGRAM, WT1_V1),
        );
        assert_eq!(
            p.reserves(v0, v1),
            Some((v0 - 6_966_209_843 - 952_493, v1 - 3_305_550 - 241_313_606))
        );
        let lp = read_base(&TOKEN_PROGRAM, WT1_LP).unwrap();
        assert_eq!(p.lp_supply - lp.supply, 100);
        assert_eq!(p.locked_lp_bps(lp.supply), Some(0));
    }

    /// `creator_fee_on == 0`: the creator fee is on the input of both directions.
    #[test]
    fn reads_the_both_tokens_pool() {
        let p = pool(BOTH);
        assert_eq!(
            (p.creator_fee_on, p.enable_creator_fee),
            (CREATOR_FEE_ON_BOTH, true)
        );
        assert_eq!(p.creator_fee_on_input(true), Some(true));
        assert_eq!(p.creator_fee_on_input(false), Some(true));
        assert_eq!(p.fee_ppm(&config(0)), Some(3_000));
    }

    /// All 21 live configs parse, carry their index, and match R-RAY §2.5's table: protocol 12 %,
    /// fund 4 %, creation fee 0.15 SOL, creation enabled.
    #[test]
    fn reads_every_config() {
        for (i, (t, c)) in RATES.iter().enumerate() {
            let cfg = config(i);
            assert_eq!(usize::from(cfg.index), i);
            assert_eq!(
                (cfg.trade_fee_rate, cfg.creator_fee_rate),
                (*t, *c),
                "config {i}"
            );
            assert_eq!(
                (cfg.protocol_fee_rate, cfg.fund_fee_rate),
                (120_000, 40_000)
            );
            assert_eq!(cfg.create_pool_fee, 150_000_000);
            assert!(!cfg.disable_create_pool);
        }
    }

    #[test]
    fn refusals() {
        assert_eq!(read_pool(&[0; 32], USELESS), Err(ReadError::WrongOwner));
        assert_eq!(read_pool(&PROGRAM, CONFIGS[0]), Err(ReadError::WrongTag));
        assert_eq!(read_pool(&PROGRAM, OBSERVATION), Err(ReadError::WrongTag));
        assert_eq!(
            read_pool(&PROGRAM, &USELESS[..POOL_LEN - 1]),
            Err(ReadError::TooShort)
        );
        assert_eq!(read_pool(&PROGRAM, &USELESS[..7]), Err(ReadError::WrongTag));
        // A longer account (an appended layout) reads its known prefix.
        let mut long = USELESS.to_vec();
        long.extend_from_slice(&[0xab; 64]);
        assert_eq!(read_pool(&PROGRAM, &long), Ok(pool(USELESS)));
        let mut bad = USELESS.to_vec();
        bad[OFF_ENABLE_CREATOR_FEE] = 2;
        assert_eq!(read_pool(&PROGRAM, &bad), Err(ReadError::Malformed));

        assert_eq!(
            read_config(&[0; 32], CONFIGS[0]),
            Err(ReadError::WrongOwner)
        );
        assert_eq!(read_config(&PROGRAM, USELESS), Err(ReadError::WrongTag));
        assert_eq!(
            read_config(&PROGRAM, &CONFIGS[0][..CONFIG_LEN - 1]),
            Err(ReadError::TooShort)
        );
        let mut cfg = CONFIGS[0].to_vec();
        cfg.push(7);
        assert_eq!(read_config(&PROGRAM, &cfg), Ok(config(0)));
        cfg[OFF_CONFIG_DISABLE_CREATE_POOL] = 3;
        assert_eq!(read_config(&PROGRAM, &cfg), Err(ReadError::Malformed));
    }

    /// R-RAY §5.3: every status byte, the open time, the counters and the fee model.
    #[test]
    fn route_predicate() {
        let base = pool(USELESS);
        let (v0, v1) = (
            amount(&TOKEN_PROGRAM, USELESS_V0),
            amount(&TOKEN_PROGRAM, USELESS_V1),
        );
        let now = 1_790_563_273;
        let cfg = config(0);
        for status in 0..=u8::MAX {
            let p = PoolState { status, ..base };
            let open = status & STATUS_BIT_SWAP == 0;
            assert_eq!(p.swap_enabled(now), open, "status {status}");
            assert_eq!(
                p.venue_executable(&cfg, now, v0, v1),
                open,
                "status {status}"
            );
        }
        // Deposit / withdraw bits alone do not stop swaps.
        let p = PoolState {
            status: STATUS_BIT_DEPOSIT | STATUS_BIT_WITHDRAW,
            ..base
        };
        assert!(p.venue_executable(&cfg, now, v0, v1));
        // open_time: exactly at it swaps work, one second before they do not.
        let p = PoolState {
            open_time: 1_800_000_000,
            ..base
        };
        assert!(!p.swap_enabled(1_799_999_999));
        assert!(p.swap_enabled(1_800_000_000));
        assert!(!p.swap_enabled(-5));
        // Counters: a vault one unit below its counters is InsufficientVault.
        let c0 = base.counters_0().unwrap();
        let c1 = base.counters_1().unwrap();
        assert_eq!(base.reserves(c0 - 1, v1), None);
        assert_eq!(base.reserves(v0, c1 - 1), None);
        assert!(!base.venue_executable(&cfg, now, c0 - 1, v1));
        // At the counters the side is empty: ARCH's inequality holds, the venue divides by 0.
        assert_eq!(base.reserves(c0, v1), Some((0, v1 - c1)));
        assert!(!base.venue_executable(&cfg, now, c0, v1));
        assert!(base.venue_executable(&cfg, now, c0 + 1, c1 + 1));
        // Overflowing counters: MathOverflow in the venue.
        let o = PoolState {
            protocol_fees_token_1: u64::MAX,
            fund_fees_token_1: 1,
            ..base
        };
        assert_eq!(o.counters_1(), None);
        assert_eq!(o.reserves(v0, u64::MAX), None);
        // An unknown fee model reverts every swap (InvalidFeeModel).
        for on in 0..=u8::MAX {
            let p = PoolState {
                creator_fee_on: on,
                ..base
            };
            assert_eq!(
                p.venue_executable(&cfg, now, v0, v1),
                on <= 2,
                "creator_fee_on {on}"
            );
            assert_eq!(p.creator_fee_on_input(true).is_some(), on <= 2);
        }
    }

    /// M3-SEC-1: a zero trade fee (`update_amm_config(0, 0)`) on a pool whose creator fee is
    /// not enabled, or is enabled at rate 0, leaves the venue's creator-fee split with nothing
    /// to divide by: every swap with the creator fee on its input reverts
    /// (`ZeroTradingTokens`). The predicate is the buy's (WSOL in): WSOL as token 0 or 1, every
    /// `creator_fee_on`, the creator fee on or off, `(t, c)` zero or not. Differential against
    /// the ELF: `tests/litesvm/tests/ray_sec_it`.
    #[test]
    fn zero_fee_splits_only_off_the_input() {
        let (v0, v1) = (
            amount(&TOKEN_PROGRAM, USELESS_V0),
            amount(&TOKEN_PROGRAM, USELESS_V1),
        );
        let now = 1_790_563_273;
        let useless = pool(USELESS);
        // WSOL as token 1: the same pool with its sides swapped.
        let sol1 = PoolState {
            token_0_mint: useless.token_1_mint,
            token_1_mint: useless.token_0_mint,
            ..useless
        };
        assert_eq!(sol1.sol_is_token0(), Some(false));
        for (p, sol0) in [(useless, true), (sol1, false)] {
            for (t, c) in [(0u64, 0u64), (0, 500), (1, 0), (2_500, 0)] {
                let cfg = AmmConfig {
                    trade_fee_rate: t,
                    creator_fee_rate: c,
                    ..config(0)
                };
                for (enable, on) in [(false, 0u8), (true, 0), (true, 1), (true, 2)] {
                    let p = PoolState {
                        enable_creator_fee: enable,
                        creator_fee_on: on,
                        ..p
                    };
                    let c_eff = if enable { c } else { 0 };
                    for input0 in [true, false] {
                        let on_input = on == 0 || (on == 1) == input0;
                        assert_eq!(
                            p.fee_splits(&cfg, input0),
                            Some(!(on_input && t + c_eff == 0)),
                            "t {t} c {c} enable {enable} on {on} input0 {input0}"
                        );
                    }
                    // The buy's input is the WSOL side.
                    let buy_on_input = on == 0 || (on == 1) == sol0;
                    assert_eq!(
                        p.venue_executable(&cfg, now, v0, v1),
                        !(buy_on_input && t + c_eff == 0),
                        "sol0 {sol0} t {t} c {c} enable {enable} on {on}"
                    );
                }
            }
        }
        // USELESS as dumped (config 0, creator fee off, fee model 0) with the trade fee at 0.
        let zero = AmmConfig {
            trade_fee_rate: 0,
            ..config(0)
        };
        assert!(useless.venue_executable(&config(0), now, v0, v1));
        assert!(!useless.venue_executable(&zero, now, v0, v1));
        assert_eq!(useless.fee_ppm(&zero), Some(0), "the fee itself resolves");
        assert_eq!(useless.fee_splits(&zero, true), Some(false));
        // An unknown fee model never splits (InvalidFeeModel).
        let bad = PoolState {
            creator_fee_on: 3,
            ..useless
        };
        assert_eq!(bad.fee_splits(&config(0), true), None);
        // A pool without a WSOL side has no buy.
        let no_sol = PoolState {
            token_0_mint: useless.token_1_mint,
            ..useless
        };
        assert!(!no_sol.venue_executable(&config(0), now, v0, v1));
    }

    /// `φ = t + c` with `c` only when enabled, and only below 10⁶.
    #[test]
    fn fee_ppm() {
        let p = pool(USELESS);
        let on = PoolState {
            enable_creator_fee: true,
            ..p
        };
        for (i, (t, c)) in RATES.iter().enumerate() {
            let cfg = config(i);
            assert_eq!(p.fee_ppm(&cfg), Some(u32::try_from(*t).unwrap()));
            assert_eq!(on.fee_ppm(&cfg), Some(u32::try_from(t + c).unwrap()));
        }
        let edge = |t, c| AmmConfig {
            trade_fee_rate: t,
            creator_fee_rate: c,
            ..config(0)
        };
        assert_eq!(on.fee_ppm(&edge(999_999, 0)), Some(999_999));
        assert_eq!(on.fee_ppm(&edge(999_999, 1)), None);
        assert_eq!(p.fee_ppm(&edge(999_999, 1)), Some(999_999));
        assert_eq!(on.fee_ppm(&edge(u64::MAX, 1)), None);
        assert_eq!(p.fee_ppm(&edge(1_000_000, 0)), None);
    }

    /// `min(now, open_time)` from `T_CLAMP` on; the listing time (`now`) before it (O13).
    #[test]
    fn age_start_clamps() {
        let now = 1_790_000_000;
        assert_eq!(age_start(0, now), now);
        assert_eq!(age_start(1_700_000_000, now), now);
        assert_eq!(age_start(1_717_199_999, now), now);
        assert_eq!(age_start(1_717_200_000, now), T_CLAMP);
        assert_eq!(age_start(1_717_200_001, now), 1_717_200_001);
        assert_eq!(age_start(1_746_886_997, now), 1_746_886_997);
        // A future open time (a pool that cannot trade yet) never dates the age ahead of now.
        assert_eq!(age_start(1_800_000_000, now), now);
        assert_eq!(age_start(u64::MAX, now), now);
        assert_eq!(age_start(0, 1_000), 1_000);
    }
}
