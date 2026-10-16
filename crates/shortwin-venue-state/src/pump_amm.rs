//! PumpSwap (pump AMM) `Pool`, `GlobalConfig` and the boost vault (ARCH §2.5
//! `canonical_pool_valid`, §3.12 PumpSwap ReadSet, §4.1, §4.2; R-VEN §1.2–§1.4, §2.2–§2.4).
//!
//! **Scope.** M3 used [`read_pool`] for detection (`classify_pump`: a Raydium listing honours
//! D1's precedence when the mint's canonical PumpSwap pool exists). Since M5 this module is the
//! full reader of the pool side: the pool's reserves as PumpSwap prices them (real vaults plus
//! `virtual_quote_reserves`, read live and never cached, C1-13) and pays them out (the quote
//! vault less the fee buckets, since pump's 2026-10-02 redeploy), `GlobalConfig` (the protocol
//! and buyback fee recipient lists, `disable_flags`, `creator_fee_configurable`), and the boost
//! vault's WSOL balance (the boost-window gate, A13). The post-migration pool is also what the
//! curve's straddle quote `B̂_str` prices on (ARCH §4.3). The PumpSwap `FeeConfig` has the
//! pump-fees layout: [`crate::pump::read_fee_config`].
//!
//! # `Pool` (discriminator `f19a6d0411b16dbc`; 271, 287, 300 or 301 bytes today, older pools
//! 211–261)
//!
//! | Offset | Field | | Offset | Field |
//! |---|---|---|---|---|
//! | 8 | `pool_bump` u8 | | 211 | `coin_creator` |
//! | 9 | `index` u16 | | 243 | `is_mayhem_mode` bool |
//! | 11 | `creator` | | 244 | `is_cashback_coin` bool |
//! | 43 | `base_mint` | | 245 | `virtual_quote_reserves` i128 |
//! | 75 | `quote_mint` | | 261 | `creator_fee_bps` u64 |
//! | 107 | `lp_mint` | | 269 | `can_edit_creator_fee` bool |
//! | 139 | `pool_base_token_account` | | 270 | `is_holder_reward` bool |
//! | 171 | `pool_quote_token_account` | | 271 | `protocol_fees` u64 (2026-10) |
//! | 203 | `lp_supply` u64 | | 279 | `creator_fees` u64 (2026-10) |
//!
//! Missing trailing fields of an older pool read as 0 / false (this reader zero-pads).
//! The 2026-09 build's `create_pool` allocated [`POOL_LEN_CREATE`] = 271 bytes (the layout
//! through `is_holder_reward`); pump's 2026-10-02 redeploy allocates [`POOL_LEN_2026_10`] = 287
//! (through the fee buckets) and its trades grow a shorter pool to it, the trader topping up the
//! rent; clients' `extend_account` takes pools to [`POOL_LEN`] = 301 (older ones to
//! [`POOL_LEN_EXTENDED_OLD`] = 300: the SDK's `POOL_ACCOUNT_NEW_SIZE`, not a program requirement;
//! R-VEN Update 2026-09-29 G4 corrects §1.2), whose bytes 271–287 the redeploy reads as the
//! buckets.
//!
//! **Fee buckets** (pump's 2026-10-02 redeploy, the `pump-rust-client` 0.2.0 release; ARCH §12
//! U30, PUMP-2026-10-1): `buy_v2` / `sell_v2` keep the protocol fee (less the buyback slice)
//! and the creator fee **in the quote vault**, counted in `protocol_fees` / `creator_fees` and
//! taken off `virtual_quote_reserves` (`VQR = boost − buckets`), until the permissionless
//! `sweep_protocol_fee` / `sweep_creator_fee` pay them out and add them back to the VQR. So the
//! price `vault + VQR` excludes them, and so does what a sale can be paid: a sale paying out more
//! than `vault − buckets` fails 6063 `InsufficientRealQuoteReserves`. [`PoolReserves::
//! payable_quote`] is that `vault − buckets`; buckets above the vault break pump's invariant
//! (not executable). v1 trades (short.win's) pay their fees out and leave the buckets as they
//! are.
//!
//! The canonical pool of a mint is `PDA(pAMM, ["pool", 0u16 LE, pool_authority, mint, WSOL])`
//! with `pool_authority = PDA(pump, ["pool-authority", mint])` (R-VEN §1.3): its address alone
//! makes it unspoofable, and `pool.creator == pool_authority` is the "canonical" test.
//!
//! # `GlobalConfig` (PDA `["global_config"]` = `ADyA8h…`, discriminator `95089ccaa0fcb0d9`, 949 bytes)
//!
//! A borrowed view ([`GlobalConfig`]), offsets per [IDL] `types.GlobalConfig`: `disable_flags`
//! u8 @56, `protocol_fee_recipients[8]` @57, `reserved_fee_recipient` @385 plus
//! `reserved_fee_recipients[7]` @418, `buyback_fee_recipients[8]` @643, `buyback_basis_points`
//! @899, `boost_authority` @907, `boost_enabled` @939, `creator_fee_configurable` @940. The bit
//! meaning of `disable_flags` is **resolved by T-DISABLE** (M6's hard gate, C0-10; R-VEN Update
//! 2026-09-30 H4, run on the dumped ELF with every bit set): bit 0 halts `create_pool` (and pump
//! `migrate_v2` through it), bit 1 `deposit`, bit 2 `withdraw`, **bit 3 every buy**
//! ([`DISABLE_BUY_BIT`]: `buy`, `buy_exact_quote_in`, `boost_buy_and_burn`), **bit 4 `sell`**
//! ([`DISABLE_SELL_BIT`]); bits 5–7 do nothing (the admin's `disable` cannot set them). This
//! reader exposes the raw byte; the adapter's route predicate reads the buy bit and its open gate
//! refuses any non-zero value (A24).
//!
//! # Boost vault
//!
//! `boost_vault = PDA(pAMM, ["boost_vault", pool])`, and its WSOL ATA holds the SOL pump's boost
//! authority spends on `boost_buy_and_burn` after a migration (R-VEN §1.4). The ATA is passed at
//! its derived key whether or not it exists (ARCH §3.0): [`boost_vault_amount`] reads a missing
//! account as 0.

use crate::bytes::{has_discriminator, i128_at, key_at, u8_at, u16_at, u64_at};
use crate::mint::{NATIVE_MINT, TOKEN_PROGRAM};
use crate::token_account;
use crate::{Key, ReadError};

/// PumpSwap program `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`.
pub const PROGRAM: Key = [
    12, 20, 222, 252, 130, 94, 198, 118, 148, 37, 8, 24, 187, 101, 64, 101, 244, 41, 141, 49, 86,
    213, 113, 180, 212, 248, 9, 12, 24, 233, 168, 99,
];

// ---- seeds (R-VEN §1.3, §2.2; ARCH §3.12 seed gotchas) -----------------------------------------

/// `PDA(pAMM, ["pool", index u16 LE, creator, base_mint, quote_mint])`.
pub const POOL_SEED: &[u8] = b"pool";
/// `PDA(pAMM, ["pool_lp_mint", pool])`.
pub const LP_MINT_SEED: &[u8] = b"pool_lp_mint";
/// `PDA(pAMM, ["global_config"])`.
pub const GLOBAL_CONFIG_SEED: &[u8] = b"global_config";
/// `PDA(pAMM, ["creator_vault", pool.coin_creator])` (**underscore**; pump's is `creator-vault`).
pub const CREATOR_VAULT_SEED: &[u8] = b"creator_vault";
/// `PDA(pAMM, ["boost_vault", pool])`.
pub const BOOST_VAULT_SEED: &[u8] = b"boost_vault";
/// `PDA(pAMM, ["pool-v2", base_mint])`: the uninitialised marker passed when
/// `coin_creator ≠ default`.
pub const POOL_V2_SEED: &[u8] = b"pool-v2";
/// `PDA(pAMM, ["user_volume_accumulator", user])`.
pub const USER_VOLUME_ACCUMULATOR_SEED: &[u8] = b"user_volume_accumulator";
/// `PDA(pAMM, ["global_volume_accumulator"])`.
pub const GLOBAL_VOLUME_ACCUMULATOR_SEED: &[u8] = b"global_volume_accumulator";

// ---- Pool ---------------------------------------------------------------------------------

/// `sha256("account:Pool")[..8]`.
pub const POOL_DISCRIMINATOR: [u8; 8] = [0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc];

/// The index of the canonical (migration) pool.
pub const CANONICAL_POOL_INDEX: u16 = 0;
/// What the 2026-09 build's `create_pool` allocated (and its trades grew a shorter pool to): the
/// layout through `is_holder_reward`.
pub const POOL_LEN_CREATE: usize = 271;
/// What pump's 2026-10-02 redeploy's `create_pool` allocates and its trades grow a shorter pool
/// to: the layout through the fee buckets.
pub const POOL_LEN_2026_10: usize = 287;
/// A pool after `extend_account` today (every boosted pool within minutes of its migration).
pub const POOL_LEN: usize = 301;
/// A pool extended to the SDK's older `POOL_ACCOUNT_NEW_SIZE`.
pub const POOL_LEN_EXTENDED_OLD: usize = 300;

/// `pool_bump` (u8).
pub const OFF_POOL_BUMP: usize = 8;
/// `index` (u16).
pub const OFF_INDEX: usize = 9;
/// `creator` (key).
pub const OFF_CREATOR: usize = 11;
/// `base_mint` (key).
pub const OFF_BASE_MINT: usize = 43;
/// `quote_mint` (key).
pub const OFF_QUOTE_MINT: usize = 75;
/// `lp_mint` (key).
pub const OFF_LP_MINT: usize = 107;
/// `pool_base_token_account` (key).
pub const OFF_POOL_BASE_TOKEN_ACCOUNT: usize = 139;
/// `pool_quote_token_account` (key).
pub const OFF_POOL_QUOTE_TOKEN_ACCOUNT: usize = 171;
/// `lp_supply` (u64).
pub const OFF_LP_SUPPLY: usize = 203;
/// `coin_creator` (key).
pub const OFF_COIN_CREATOR: usize = 211;
/// `is_mayhem_mode` (bool).
pub const OFF_IS_MAYHEM_MODE: usize = 243;
/// `is_cashback_coin` (bool).
pub const OFF_IS_CASHBACK_COIN: usize = 244;
/// `virtual_quote_reserves` (i128).
pub const OFF_VIRTUAL_QUOTE_RESERVES: usize = 245;
/// `creator_fee_bps` (u64).
pub const OFF_CREATOR_FEE_BPS: usize = 261;
/// `can_edit_creator_fee` (bool).
pub const OFF_CAN_EDIT_CREATOR_FEE: usize = 269;
/// `is_holder_reward` (bool).
pub const OFF_IS_HOLDER_REWARD: usize = 270;
/// `protocol_fees` (u64): the un-swept protocol fee of v2 trades, held in the quote vault
/// (2026-10).
pub const OFF_PROTOCOL_FEES: usize = 271;
/// `creator_fees` (u64): the un-swept creator fee of v2 trades, held in the quote vault (2026-10).
pub const OFF_CREATOR_FEES: usize = 279;

/// The fields of a PumpSwap `Pool` short.win reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pool {
    pub pool_bump: u8,
    pub index: u16,
    pub creator: Key,
    pub base_mint: Key,
    pub quote_mint: Key,
    pub lp_mint: Key,
    pub pool_base_token_account: Key,
    pub pool_quote_token_account: Key,
    pub lp_supply: u64,
    pub coin_creator: Key,
    pub is_mayhem_mode: bool,
    pub is_cashback_coin: bool,
    pub virtual_quote_reserves: i128,
    pub creator_fee_bps: u64,
    pub can_edit_creator_fee: bool,
    pub is_holder_reward: bool,
    /// The fee buckets (module docs; 0 on a pool shorter than 287 bytes).
    pub protocol_fees: u64,
    pub creator_fees: u64,
}

/// PumpSwap's pricing reserves of a pool (R-VEN §1.4, §2.4; ARCH §4.1 "Pricing reserves").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolReserves {
    /// `X`: `pool_base_token_account.amount`.
    pub base: u64,
    /// `Y`: `pool_quote_token_account.amount + virtual_quote_reserves` (every buy and sell
    /// prices on it; `virtual_quote_reserves` may be negative from 2026-09-30, the sum never).
    pub effective_quote: u128,
    /// `pool_quote_token_account.amount`, the WSOL vault.
    pub real_quote: u64,
    /// `R`'s vault part: `real_quote − protocol_fees − creator_fees`, the SOL a sale can be paid
    /// out of (module docs: PumpSwap's 6063 above it); `real_quote` while the pool holds no
    /// buckets.
    pub payable_quote: u64,
}

impl Pool {
    /// Quoted in WSOL.
    pub fn is_sol_quote(&self) -> bool {
        self.quote_mint == NATIVE_MINT
    }

    /// `coin_creator == Pubkey::default()`: PumpSwap charges no creator fee.
    pub fn coin_creator_is_default(&self) -> bool {
        self.coin_creator == [0; 32]
    }

    /// `protocol_fees + creator_fees`, the quote vault's SOL PumpSwap holds back from sales
    /// (module docs); `None` if the sum overflows.
    pub fn buckets(&self) -> Option<u64> {
        self.protocol_fees.checked_add(self.creator_fees)
    }

    /// PumpSwap's pricing reserves from the two vault amounts. `virtual_quote_reserves` is
    /// signed: pump announced negative values from 2026-09-30 and guarantees that
    /// `quote vault + virtual_quote_reserves` is never negative and fits a `u64`
    /// ([DOCS] `NEGATIVE_VIRTUAL_QUOTE_RESERVES.md`, `pump-public-docs@cb188ce0`; R-PUMP Update
    /// 2026-09-29 U2). `None` (not executable) if that sum is negative or exceeds `u64`, or if
    /// the fee buckets exceed the quote vault (PUMP-2026-10-1): the pool breaks pump's own
    /// invariant.
    pub fn reserves(
        &self,
        base_vault_amount: u64,
        quote_vault_amount: u64,
    ) -> Option<PoolReserves> {
        let effective = i128::from(quote_vault_amount).checked_add(self.virtual_quote_reserves)?;
        let effective = u64::try_from(effective).ok()?;
        let payable = quote_vault_amount.checked_sub(self.buckets()?)?;
        Some(PoolReserves {
            base: base_vault_amount,
            effective_quote: u128::from(effective),
            real_quote: quote_vault_amount,
            payable_quote: payable,
        })
    }

    /// The virtual part of the effective reserve the tier subtracts (ARCH §4.7; ECON §6.2 #103,
    /// #105): the boost `max(VQR + buckets, 0)` — the buckets are real SOL PumpSwap took off the
    /// VQR, not liquidity, so adding them back gives what a v1-only pool's `max(VQR, 0)` was;
    /// `None` if the sum does not fit a `u64` (never on a pool [`Self::reserves`] accepts).
    pub fn virtual_part(&self) -> Option<u64> {
        let v = self
            .virtual_quote_reserves
            .checked_add(i128::from(self.buckets()?))?;
        u64::try_from(v.max(0)).ok()
    }
}

/// An Anchor `bool` byte: 0 or 1, anything else is malformed.
fn bool_at(data: &[u8], offset: usize) -> Result<bool, ReadError> {
    match u8_at(data, offset) {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ReadError::Malformed),
    }
}

/// Reads a `Pool`: owner == [`PROGRAM`], discriminator [`POOL_DISCRIMINATOR`], every `bool` 0
/// or 1; fields past the end of a short (older) pool read as zero.
pub fn read_pool(owner: &Key, data: &[u8]) -> Result<Pool, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &POOL_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    Ok(Pool {
        pool_bump: u8_at(data, OFF_POOL_BUMP),
        index: u16_at(data, OFF_INDEX),
        creator: key_at(data, OFF_CREATOR),
        base_mint: key_at(data, OFF_BASE_MINT),
        quote_mint: key_at(data, OFF_QUOTE_MINT),
        lp_mint: key_at(data, OFF_LP_MINT),
        pool_base_token_account: key_at(data, OFF_POOL_BASE_TOKEN_ACCOUNT),
        pool_quote_token_account: key_at(data, OFF_POOL_QUOTE_TOKEN_ACCOUNT),
        lp_supply: u64_at(data, OFF_LP_SUPPLY),
        coin_creator: key_at(data, OFF_COIN_CREATOR),
        is_mayhem_mode: bool_at(data, OFF_IS_MAYHEM_MODE)?,
        is_cashback_coin: bool_at(data, OFF_IS_CASHBACK_COIN)?,
        virtual_quote_reserves: i128_at(data, OFF_VIRTUAL_QUOTE_RESERVES),
        creator_fee_bps: u64_at(data, OFF_CREATOR_FEE_BPS),
        can_edit_creator_fee: bool_at(data, OFF_CAN_EDIT_CREATOR_FEE)?,
        is_holder_reward: bool_at(data, OFF_IS_HOLDER_REWARD)?,
        protocol_fees: u64_at(data, OFF_PROTOCOL_FEES),
        creator_fees: u64_at(data, OFF_CREATOR_FEES),
    })
}

// ---- GlobalConfig -------------------------------------------------------------------------------

/// `sha256("account:GlobalConfig")[..8]`.
pub const GLOBAL_CONFIG_DISCRIMINATOR: [u8; 8] = [0x95, 0x08, 0x9c, 0xca, 0xa0, 0xfc, 0xb0, 0xd9];
/// The `GlobalConfig` layout's length (the Borsh value and the allocation).
pub const GLOBAL_CONFIG_LEN: usize = 949;
/// Recipients per list.
pub const RECIPIENTS: usize = 8;

/// `admin` (key).
pub const OFF_GC_ADMIN: usize = 8;
/// `lp_fee_basis_points` (u64; fallback, `FeeConfig` decides).
pub const OFF_GC_LP_FEE_BASIS_POINTS: usize = 40;
/// `protocol_fee_basis_points` (u64; fallback).
pub const OFF_GC_PROTOCOL_FEE_BASIS_POINTS: usize = 48;
/// `disable_flags` (u8; bits resolved by T-DISABLE: [`DISABLE_BUY_BIT`], [`DISABLE_SELL_BIT`]).
pub const OFF_GC_DISABLE_FLAGS: usize = 56;
/// `protocol_fee_recipients[8]`.
pub const OFF_GC_PROTOCOL_FEE_RECIPIENTS: usize = 57;
/// `coin_creator_fee_basis_points` (u64; fallback).
pub const OFF_GC_COIN_CREATOR_FEE_BASIS_POINTS: usize = 313;
/// `admin_set_coin_creator_authority` (key).
pub const OFF_GC_ADMIN_SET_COIN_CREATOR_AUTHORITY: usize = 321;
/// `whitelist_pda` (key).
pub const OFF_GC_WHITELIST_PDA: usize = 353;
/// `reserved_fee_recipient` (key): reserved recipient #0 (mayhem pools).
pub const OFF_GC_RESERVED_FEE_RECIPIENT: usize = 385;
/// `mayhem_mode_enabled` (bool).
pub const OFF_GC_MAYHEM_MODE_ENABLED: usize = 417;
/// `reserved_fee_recipients[7]`.
pub const OFF_GC_RESERVED_FEE_RECIPIENTS: usize = 418;
/// `is_cashback_enabled` (bool).
pub const OFF_GC_IS_CASHBACK_ENABLED: usize = 642;
/// `buyback_fee_recipients[8]`.
pub const OFF_GC_BUYBACK_FEE_RECIPIENTS: usize = 643;
/// `buyback_basis_points` (u64).
pub const OFF_GC_BUYBACK_BASIS_POINTS: usize = 899;
/// `boost_authority` (key; the only signer of `boost_buy_and_burn`).
pub const OFF_GC_BOOST_AUTHORITY: usize = 907;
/// `boost_enabled` (bool).
pub const OFF_GC_BOOST_ENABLED: usize = 939;
/// `creator_fee_configurable` (bool): gate of a pool's own `creator_fee_bps`.
pub const OFF_GC_CREATOR_FEE_CONFIGURABLE: usize = 940;
/// `max_configurable_creator_fee_bps` (u64).
pub const OFF_GC_MAX_CONFIGURABLE_CREATOR_FEE_BPS: usize = 941;

/// The `bool` fields of `GlobalConfig`, validated by [`read_global_config`].
const GC_BOOLS: [usize; 4] = [
    OFF_GC_MAYHEM_MODE_ENABLED,
    OFF_GC_IS_CASHBACK_ENABLED,
    OFF_GC_BOOST_ENABLED,
    OFF_GC_CREATOR_FEE_CONFIGURABLE,
];

/// PumpSwap `GlobalConfig`, read in place (see the module docs). Built by
/// [`read_global_config`], which checked the owner, the discriminator, the length and every
/// `bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalConfig<'a> {
    data: &'a [u8],
}

/// Reads PumpSwap's `GlobalConfig`: owner == [`PROGRAM`], discriminator
/// [`GLOBAL_CONFIG_DISCRIMINATOR`], at least [`GLOBAL_CONFIG_LEN`] bytes, every `bool` 0 or 1.
pub fn read_global_config<'a>(owner: &Key, data: &'a [u8]) -> Result<GlobalConfig<'a>, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &GLOBAL_CONFIG_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    if data.len() < GLOBAL_CONFIG_LEN {
        return Err(ReadError::TooShort);
    }
    for off in GC_BOOLS {
        bool_at(data, off)?;
    }
    Ok(GlobalConfig { data })
}

/// An 8-entry key array at `offset`.
fn keys_at(data: &[u8], offset: usize) -> [Key; RECIPIENTS] {
    core::array::from_fn(|i| key_at(data, offset.saturating_add(i.saturating_mul(32))))
}

impl GlobalConfig<'_> {
    fn flag(&self, offset: usize) -> bool {
        u8_at(self.data, offset) == 1
    }

    /// `admin`.
    pub fn admin(&self) -> Key {
        key_at(self.data, OFF_GC_ADMIN)
    }

    /// `disable_flags`, raw ([`DISABLE_BUY_BIT`], [`DISABLE_SELL_BIT`]; T-DISABLE).
    pub fn disable_flags(&self) -> u8 {
        u8_at(self.data, OFF_GC_DISABLE_FLAGS)
    }

    /// `protocol_fee_recipients[8]` (a non-mayhem trade's `protocol_fee_recipient`).
    pub fn protocol_fee_recipients(&self) -> [Key; RECIPIENTS] {
        keys_at(self.data, OFF_GC_PROTOCOL_FEE_RECIPIENTS)
    }

    /// `key` is one of the 8 protocol fee recipients.
    pub fn is_protocol_fee_recipient(&self, key: &Key) -> bool {
        self.protocol_fee_recipients().contains(key)
    }

    /// The 8 reserved fee recipients (mayhem pools): `reserved_fee_recipient` then
    /// `reserved_fee_recipients[7]`.
    pub fn reserved_fee_recipients(&self) -> [Key; RECIPIENTS] {
        core::array::from_fn(|i| match i.checked_sub(1) {
            None => key_at(self.data, OFF_GC_RESERVED_FEE_RECIPIENT),
            Some(j) => key_at(
                self.data,
                OFF_GC_RESERVED_FEE_RECIPIENTS.saturating_add(j.saturating_mul(32)),
            ),
        })
    }

    /// `buyback_fee_recipients[8]`.
    pub fn buyback_fee_recipients(&self) -> [Key; RECIPIENTS] {
        keys_at(self.data, OFF_GC_BUYBACK_FEE_RECIPIENTS)
    }

    /// `key` is one of the 8 buyback fee recipients.
    pub fn is_buyback_fee_recipient(&self, key: &Key) -> bool {
        self.buyback_fee_recipients().contains(key)
    }

    /// `buyback_basis_points` (a split of the protocol fee).
    pub fn buyback_basis_points(&self) -> u64 {
        u64_at(self.data, OFF_GC_BUYBACK_BASIS_POINTS)
    }

    /// `boost_authority`.
    pub fn boost_authority(&self) -> Key {
        key_at(self.data, OFF_GC_BOOST_AUTHORITY)
    }

    /// `boost_enabled`.
    pub fn boost_enabled(&self) -> bool {
        self.flag(OFF_GC_BOOST_ENABLED)
    }

    /// `creator_fee_configurable`: a pool's own `creator_fee_bps > 0` replaces the schedule's.
    pub fn creator_fee_configurable(&self) -> bool {
        self.flag(OFF_GC_CREATOR_FEE_CONFIGURABLE)
    }

    /// `max_configurable_creator_fee_bps`.
    pub fn max_configurable_creator_fee_bps(&self) -> u64 {
        u64_at(self.data, OFF_GC_MAX_CONFIGURABLE_CREATOR_FEE_BPS)
    }

    /// `(lp, protocol, coin_creator)` fallback fees in bps (unused while `FeeConfig` exists).
    pub fn fallback_fees_bps(&self) -> (u64, u64, u64) {
        (
            u64_at(self.data, OFF_GC_LP_FEE_BASIS_POINTS),
            u64_at(self.data, OFF_GC_PROTOCOL_FEE_BASIS_POINTS),
            u64_at(self.data, OFF_GC_COIN_CREATOR_FEE_BASIS_POINTS),
        )
    }

    /// `mayhem_mode_enabled`.
    pub fn mayhem_mode_enabled(&self) -> bool {
        self.flag(OFF_GC_MAYHEM_MODE_ENABLED)
    }

    /// `is_cashback_enabled`.
    pub fn is_cashback_enabled(&self) -> bool {
        self.flag(OFF_GC_IS_CASHBACK_ENABLED)
    }
}

// ---- disable_flags (T-DISABLE) ------------------------------------------------------------------

/// `GlobalConfig.disable_flags` bit 3: every buy fails 6020 `DisabledBuy` (`buy`,
/// `buy_exact_quote_in`, `boost_buy_and_burn`). T-DISABLE on the dumped ELF (R-VEN Update
/// 2026-09-30 H4): the only bit an exit (a buy) depends on, so ARCH §5.3's PumpSwap venue
/// predicate is `disable_flags & DISABLE_BUY_BIT == 0` (`PAMM_BUY_DISABLED_BIT`).
pub const DISABLE_BUY_BIT: u8 = 1 << 3;
/// `GlobalConfig.disable_flags` bit 4: `sell` fails 6021 `DisabledSell` (T-DISABLE, H4). Opens sell;
/// they refuse any non-zero `disable_flags` anyway (A24).
pub const DISABLE_SELL_BIT: u8 = 1 << 4;
/// Bit 0: `create_pool` fails 6017, and with it pump's `migrate_v2` (a stuck migration, ARCH §5.3).
pub const DISABLE_CREATE_POOL_BIT: u8 = 1 << 0;
/// Bit 1: `deposit` fails 6018.
pub const DISABLE_DEPOSIT_BIT: u8 = 1 << 1;
/// Bit 2: `withdraw` fails 6019.
pub const DISABLE_WITHDRAW_BIT: u8 = 1 << 2;

// ---- UserVolumeAccumulator ----------------------------------------------------------------------

/// Reads a **PumpSwap** `UserVolumeAccumulator` (owner [`PROGRAM`]; the same Anchor account as
/// pump's — discriminator, 137 bytes, `user` @8, `cashback_earned` @74 — `crate::pump`'s layout):
/// the trader's pAMM UVA must exist before a PumpSwap buy (ARCH §3.12 T14; a missing one costs
/// the user 1,346,200, R-VEN Update H6).
pub fn read_user_volume_accumulator(
    owner: &Key,
    data: &[u8],
) -> Result<crate::pump::UserVolumeAccumulator, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    crate::pump::read_user_volume_accumulator(&crate::pump::PROGRAM, data)
}

// ---- boost vault --------------------------------------------------------------------------------

/// The WSOL balance of a pool's boost-vault ATA as the boost gate reads it (ARCH §3.4 step 4,
/// A13; R-VEN §1.4): 0 for a missing account (no data: never created, or closed after the
/// boost), else the token account's `amount`. `owner` / `data` are the account at
/// `ATA(boost_vault, WSOL)` (key-checked by the caller) and `boost_vault` the PDA
/// `["boost_vault", pool]`.
///
/// Errors: an account with data that is not an SPL Token account of WSOL whose token owner is
/// `boost_vault` (`WrongOwner` / `WrongTag` / `Malformed` / `TooShort` from the token-account
/// reader, `Malformed` for a wrong mint or owner). The ATA program only ever creates the right
/// account at that address, so this cannot happen on mainnet.
pub fn boost_vault_amount(owner: &Key, data: &[u8], boost_vault: &Key) -> Result<u64, ReadError> {
    if data.is_empty() {
        return Ok(0);
    }
    if *owner != TOKEN_PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    let t = token_account::read(owner, data)?;
    if t.mint != NATIVE_MINT || t.owner != *boost_vault {
        return Err(ReadError::Malformed);
    }
    Ok(t.amount)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn put(d: &mut [u8], off: usize, bytes: &[u8]) {
        d[off..off + bytes.len()].copy_from_slice(bytes);
    }

    #[test]
    fn reads_the_documented_offsets() {
        let mut d = [0u8; 301];
        put(&mut d, 0, &POOL_DISCRIMINATOR);
        d[OFF_POOL_BUMP] = 254;
        put(&mut d, OFF_INDEX, &7u16.to_le_bytes());
        put(&mut d, OFF_CREATOR, &[1; 32]);
        put(&mut d, OFF_BASE_MINT, &[2; 32]);
        put(&mut d, OFF_QUOTE_MINT, &[3; 32]);
        put(&mut d, OFF_LP_MINT, &[4; 32]);
        put(&mut d, OFF_POOL_BASE_TOKEN_ACCOUNT, &[5; 32]);
        put(&mut d, OFF_POOL_QUOTE_TOKEN_ACCOUNT, &[6; 32]);
        put(&mut d, OFF_LP_SUPPLY, &9u64.to_le_bytes());
        put(&mut d, OFF_COIN_CREATOR, &[7; 32]);
        d[OFF_IS_MAYHEM_MODE] = 1;
        put(
            &mut d,
            OFF_VIRTUAL_QUOTE_RESERVES,
            &17_584_505_333i128.to_le_bytes(),
        );
        put(&mut d, OFF_CREATOR_FEE_BPS, &30u64.to_le_bytes());
        d[OFF_IS_HOLDER_REWARD] = 1;
        let p = read_pool(&PROGRAM, &d).unwrap();
        assert_eq!((p.pool_bump, p.index), (254, 7));
        assert_eq!(
            (p.creator, p.base_mint, p.quote_mint, p.lp_mint),
            ([1; 32], [2; 32], [3; 32], [4; 32])
        );
        assert_eq!(
            (p.pool_base_token_account, p.pool_quote_token_account),
            ([5; 32], [6; 32])
        );
        assert_eq!((p.lp_supply, p.coin_creator), (9, [7; 32]));
        assert!(p.is_mayhem_mode && !p.is_cashback_coin);
        assert_eq!(p.virtual_quote_reserves, 17_584_505_333);
        assert_eq!(p.creator_fee_bps, 30);
        assert!(p.is_holder_reward && !p.can_edit_creator_fee);
        assert!(!p.is_sol_quote() && !p.coin_creator_is_default());
        let r = p.reserves(1_000, 2_000).unwrap();
        assert_eq!(
            (r.base, r.effective_quote, r.real_quote, r.payable_quote),
            (1_000, 17_584_507_333, 2_000, 2_000)
        );
        assert_eq!((p.protocol_fees, p.creator_fees), (0, 0));
        // The redeploy's buckets @271 / @279 (on a 287-, 300- or 301-byte pool).
        put(&mut d, OFF_PROTOCOL_FEES, &300u64.to_le_bytes());
        put(&mut d, OFF_CREATOR_FEES, &200u64.to_le_bytes());
        let p = read_pool(&PROGRAM, &d).unwrap();
        assert_eq!(
            (p.protocol_fees, p.creator_fees, p.buckets()),
            (300, 200, Some(500))
        );
        assert_eq!(read_pool(&PROGRAM, &d[..POOL_LEN_2026_10]).unwrap(), p);
        // An older 211-byte pool reads the appended fields as zero.
        let old = read_pool(&PROGRAM, &d[..211]).unwrap();
        assert!(!old.is_mayhem_mode && !old.is_holder_reward);
        assert_eq!((old.coin_creator, old.virtual_quote_reserves), ([0; 32], 0));
        assert!(old.coin_creator_is_default());
        assert_eq!(old.reserves(5, 6).unwrap().effective_quote, 6);
        // So does the 2026-09 build's 271-byte pool's: no buckets.
        let create = read_pool(&PROGRAM, &d[..POOL_LEN_CREATE]).unwrap();
        assert_eq!((create.protocol_fees, create.creator_fees), (0, 0));
        assert_eq!(create.reserves(5, 6).unwrap().payable_quote, 6);
    }

    /// PUMP-2026-10-1: the buckets stay in the quote vault and out of the VQR (`VQR = boost −
    /// buckets`), so the price `vault + VQR` excludes them, a sale is paid out of `vault −
    /// buckets`, and the tier's virtual part is the boost `max(VQR + buckets, 0)`.
    #[test]
    fn the_fee_buckets_are_not_payable_sol() {
        let mut d = [0u8; POOL_LEN_2026_10];
        put(&mut d, 0, &POOL_DISCRIMINATOR);
        let set = |d: &mut [u8; POOL_LEN_2026_10], vqr: i128, pf: u64, cf: u64| {
            put(d, OFF_VIRTUAL_QUOTE_RESERVES, &vqr.to_le_bytes());
            put(d, OFF_PROTOCOL_FEES, &pf.to_le_bytes());
            put(d, OFF_CREATOR_FEES, &cf.to_le_bytes());
            read_pool(&PROGRAM, &d[..]).unwrap()
        };
        // A boosted pool (boost 1,000) whose v2 trades left 300 + 100 in the vault.
        let p = set(&mut d, 1_000 - 400, 300, 100);
        let r = p.reserves(7, 5_000).unwrap();
        assert_eq!(
            (r.effective_quote, r.real_quote, r.payable_quote),
            (5_600, 5_000, 4_600)
        );
        assert_eq!(p.virtual_part(), Some(1_000), "the boost");
        // Buckets above the boost: VQR < 0, the virtual part 0, the price still vault + VQR.
        let p = set(&mut d, 1_000 - 1_500, 1_000, 500);
        let r = p.reserves(7, 5_000).unwrap();
        assert_eq!((r.effective_quote, r.payable_quote), (4_500, 3_500));
        assert_eq!(p.virtual_part(), Some(1_000));
        // A negative-VQR pool (−2,000 before any bucket) with buckets: the virtual part stays 0.
        let p = set(&mut d, -2_000 - 400, 300, 100);
        let r = p.reserves(7, 5_000).unwrap();
        assert_eq!((r.effective_quote, r.payable_quote), (2_600, 4_600));
        assert_eq!(p.virtual_part(), Some(0));
        // Buckets equal to the vault: nothing payable; above it: not executable.
        let p = set(&mut d, 0, 3_000, 2_000);
        assert_eq!(p.reserves(7, 5_000).unwrap().payable_quote, 0);
        assert_eq!(p.reserves(7, 4_999), None);
        let p = set(&mut d, 0, u64::MAX, 1);
        assert_eq!((p.buckets(), p.reserves(7, u64::MAX)), (None, None));
        assert_eq!(p.virtual_part(), None);
        // No buckets: the 2026-09 reading, `max(VQR, 0)`.
        let p = set(&mut d, 1_000, 0, 0);
        assert_eq!(p.reserves(7, 5_000).unwrap().payable_quote, 5_000);
        assert_eq!(p.virtual_part(), Some(1_000));
        assert_eq!(set(&mut d, -1, 0, 0).virtual_part(), Some(0));
    }

    #[test]
    fn refusals() {
        let mut d = [0u8; 300];
        put(&mut d, 0, &POOL_DISCRIMINATOR);
        assert!(read_pool(&PROGRAM, &d).is_ok());
        assert_eq!(read_pool(&[9; 32], &d), Err(ReadError::WrongOwner));
        let mut bad = d;
        bad[7] ^= 1;
        assert_eq!(read_pool(&PROGRAM, &bad), Err(ReadError::WrongTag));
        for off in [
            OFF_IS_MAYHEM_MODE,
            OFF_IS_CASHBACK_COIN,
            OFF_CAN_EDIT_CREATOR_FEE,
            OFF_IS_HOLDER_REWARD,
        ] {
            let mut bad = d;
            bad[off] = 2;
            assert_eq!(read_pool(&PROGRAM, &bad), Err(ReadError::Malformed));
        }
        // A negative VQR prices on the (non-negative) sum; a negative or oversized sum has no
        // reserves.
        let mut neg = d;
        put(
            &mut neg,
            OFF_VIRTUAL_QUOTE_RESERVES,
            &(-400i128).to_le_bytes(),
        );
        let p = read_pool(&PROGRAM, &neg).unwrap();
        let r = p.reserves(7, 1_000).unwrap();
        assert_eq!((r.base, r.effective_quote, r.real_quote), (7, 600, 1_000));
        assert_eq!(p.reserves(7, 400).unwrap().effective_quote, 0);
        assert_eq!(p.reserves(7, 399), None);
        let mut big = d;
        put(&mut big, OFF_VIRTUAL_QUOTE_RESERVES, &1i128.to_le_bytes());
        let p = read_pool(&PROGRAM, &big).unwrap();
        assert_eq!(p.reserves(1, u64::MAX), None);
        assert_eq!(
            p.reserves(1, u64::MAX - 1).unwrap().effective_quote,
            u128::from(u64::MAX)
        );
        put(
            &mut big,
            OFF_VIRTUAL_QUOTE_RESERVES,
            &i128::MIN.to_le_bytes(),
        );
        assert_eq!(
            read_pool(&PROGRAM, &big).unwrap().reserves(1, u64::MAX),
            None
        );
    }

    #[test]
    fn layout_constants() {
        assert_eq!(OFF_IS_HOLDER_REWARD + 1, POOL_LEN_CREATE);
        assert_eq!(POOL_LEN_CREATE, OFF_PROTOCOL_FEES);
        assert_eq!(OFF_PROTOCOL_FEES + 8, OFF_CREATOR_FEES);
        assert_eq!(OFF_CREATOR_FEES + 8, POOL_LEN_2026_10);
        assert_eq!(POOL_LEN_CREATE + 30, POOL_LEN);
        assert_eq!(
            OFF_GC_PROTOCOL_FEE_RECIPIENTS + 8 * 32,
            OFF_GC_COIN_CREATOR_FEE_BASIS_POINTS
        );
        assert_eq!(
            OFF_GC_RESERVED_FEE_RECIPIENTS + 7 * 32,
            OFF_GC_IS_CASHBACK_ENABLED
        );
        assert_eq!(
            OFF_GC_BUYBACK_FEE_RECIPIENTS + 8 * 32,
            OFF_GC_BUYBACK_BASIS_POINTS
        );
        assert_eq!(
            OFF_GC_MAX_CONFIGURABLE_CREATOR_FEE_BPS + 8,
            GLOBAL_CONFIG_LEN
        );
    }

    #[test]
    fn global_config_view_and_refusals() {
        let mut d = [0u8; GLOBAL_CONFIG_LEN];
        put(&mut d, 0, &GLOBAL_CONFIG_DISCRIMINATOR);
        d[OFF_GC_DISABLE_FLAGS] = 0b101;
        for i in 0..8 {
            put(
                &mut d,
                OFF_GC_PROTOCOL_FEE_RECIPIENTS + 32 * i,
                &[10 + u8::try_from(i).unwrap(); 32],
            );
            put(
                &mut d,
                OFF_GC_BUYBACK_FEE_RECIPIENTS + 32 * i,
                &[50 + u8::try_from(i).unwrap(); 32],
            );
        }
        put(&mut d, OFF_GC_RESERVED_FEE_RECIPIENT, &[2; 32]);
        put(&mut d, OFF_GC_RESERVED_FEE_RECIPIENTS + 6 * 32, &[3; 32]);
        put(&mut d, OFF_GC_BUYBACK_BASIS_POINTS, &5_000u64.to_le_bytes());
        d[OFF_GC_CREATOR_FEE_CONFIGURABLE] = 1;
        d[OFF_GC_BOOST_ENABLED] = 1;
        let g = read_global_config(&PROGRAM, &d).unwrap();
        assert_eq!(g.disable_flags(), 0b101);
        assert_eq!(g.protocol_fee_recipients()[7], [17; 32]);
        assert!(g.is_protocol_fee_recipient(&[10; 32]) && !g.is_protocol_fee_recipient(&[50; 32]));
        assert!(g.is_buyback_fee_recipient(&[57; 32]));
        assert_eq!(g.reserved_fee_recipients()[0], [2; 32]);
        assert_eq!(g.reserved_fee_recipients()[7], [3; 32]);
        assert_eq!(g.buyback_basis_points(), 5_000);
        assert!(g.creator_fee_configurable() && g.boost_enabled());
        assert_eq!(read_global_config(&[0; 32], &d), Err(ReadError::WrongOwner));
        assert_eq!(
            read_global_config(&PROGRAM, &d[..GLOBAL_CONFIG_LEN - 1]),
            Err(ReadError::TooShort)
        );
        let mut bad = d;
        bad[0] ^= 1;
        assert_eq!(read_global_config(&PROGRAM, &bad), Err(ReadError::WrongTag));
        for off in GC_BOOLS {
            let mut bad = d;
            bad[off] = 2;
            assert_eq!(
                read_global_config(&PROGRAM, &bad),
                Err(ReadError::Malformed)
            );
        }
    }

    use crate::pump::tests::b58;

    /// Mainnet dumps at one slot (451,675,432; `tests/fixtures/manifest.json`, group
    /// `pump_state`).
    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../tests/fixtures/accounts/pump/",
                $name,
                ".bin"
            ))
        };
    }

    fn amount(d: &[u8]) -> u64 {
        token_account::read(&TOKEN_PROGRAM, d)
            .or_else(|_| token_account::read(&crate::mint::TOKEN_2022_PROGRAM, d))
            .unwrap()
            .amount
    }

    /// The graduated coin's canonical pool (301 bytes, 79 slots after its `migrate_v2`): the
    /// boost is running (VQR ≈ 17.58 SOL, the boost vault's WSOL ATA holds 16.5 SOL).
    #[test]
    fn reads_the_graduated_pool_and_its_boost_vault() {
        let d: &[u8] = fixture!("pump_graduated_pool");
        assert_eq!(d.len(), POOL_LEN);
        let p = read_pool(&PROGRAM, d).unwrap();
        assert_eq!(p.index, CANONICAL_POOL_INDEX);
        assert_eq!(
            p.base_mint,
            b58("7UfcTJD2RSRg9AJ4k2V2rs71Ew8Zy2avaU5roc8bpump")
        );
        assert!(p.is_sol_quote() && !p.is_mayhem_mode && !p.coin_creator_is_default());
        assert_eq!(
            p.creator,
            b58("8whJbitjkdYhkSySbPjkMJofQwBRWiFXLgoS1ob7kAbZ")
        );
        assert_eq!(
            p.coin_creator,
            b58("4MLyyVLVoNCcykXnYKFUG7woeRS89ePzQet7VsxSSQWb")
        );
        assert_eq!(
            (p.pool_base_token_account, p.pool_quote_token_account),
            (
                b58("EtUH2LhrHwXYrhLFmUudUNnb71BNdgLYXmJ9G14JzWLt"),
                b58("EwkSZ1kn1fAeLA3xEHJV84uLe5QAQRoYyvfyHYf2AFmp")
            )
        );
        assert_eq!(p.virtual_quote_reserves, 17_584_505_981);
        let base = amount(fixture!("pump_graduated_pool_base_vault"));
        let quote = amount(fixture!("pump_graduated_pool_quote_vault"));
        assert_eq!((base, quote), (201_657_242_940_632, 69_672_839_939));
        let r = p.reserves(base, quote).unwrap();
        assert_eq!(r.effective_quote, 69_672_839_939 + 17_584_505_981);
        assert_eq!((r.real_quote, r.payable_quote), (quote, quote));
        assert_eq!(
            (p.protocol_fees, p.creator_fees),
            (0, 0),
            "dumped before the redeploy"
        );
        let boost_vault = b58("7TWNzBs7g1PByMaHd9xxKkgG2YDXFqqqZ1wsgGh2XAN");
        assert_eq!(
            boost_vault_amount(
                &TOKEN_PROGRAM,
                fixture!("pump_graduated_boost_vault_ata"),
                &boost_vault
            ),
            Ok(16_533_089_683)
        );
        // Another pool's boost vault is not this ATA's owner.
        assert_eq!(
            boost_vault_amount(
                &TOKEN_PROGRAM,
                fixture!("pump_graduated_boost_vault_ata"),
                &p.creator
            ),
            Err(ReadError::Malformed)
        );
    }

    /// A 300-byte canonical pool from before the boost (legacy SPL-Token coin, VQR 0).
    #[test]
    fn reads_the_legacy_pool() {
        let d: &[u8] = fixture!("pump_legacy150_pool");
        assert_eq!(d.len(), POOL_LEN_EXTENDED_OLD);
        let p = read_pool(&PROGRAM, d).unwrap();
        assert_eq!(p.virtual_quote_reserves, 0);
        assert_eq!(
            p.base_mint,
            b58("Coyj3LtKn1BNSgWc9HsGK5SKoGfEoDaymig4wrN6pump")
        );
        let base = amount(fixture!("pump_legacy150_pool_base_vault"));
        let quote = amount(fixture!("pump_legacy150_pool_quote_vault"));
        let r = p.reserves(base, quote).unwrap();
        assert_eq!(
            (r.base, r.effective_quote),
            (968_881_689_481_479, 18_712_415_490)
        );
    }

    /// PumpSwap `GlobalConfig` at the snapshot (R-VEN §2.3): `disable_flags` 0, the live
    /// protocol-fee list (with `JCRGum…`, not the docs' `CebN5W…`).
    #[test]
    fn reads_the_dumped_global_config() {
        let g = read_global_config(&PROGRAM, fixture!("pamm_global_config")).unwrap();
        assert_eq!(g.disable_flags(), 0);
        assert_eq!(
            g.admin(),
            b58("FFWtrEQ4B4PKQoVuHYzZq8FabGkVatYzDpEVHsK5rrhF")
        );
        assert_eq!(
            g.protocol_fee_recipients()[7],
            b58("JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU")
        );
        assert!(!g.is_protocol_fee_recipient(&b58("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM")));
        assert!(g.is_buyback_fee_recipient(&b58("5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD")));
        assert_eq!(g.buyback_basis_points(), 5_000);
        assert_eq!(
            g.boost_authority(),
            b58("HTVZVEQMBsNanubDPTs3CxDAEGNFQHJY8c1441iy2S5r")
        );
        assert!(g.boost_enabled() && g.creator_fee_configurable());
        assert_eq!(g.max_configurable_creator_fee_bps(), 300);
        assert_eq!(g.fallback_fees_bps(), (20, 5, 5));
        assert!(g.mayhem_mode_enabled() && g.is_cashback_enabled());
    }

    /// The pAMM UVA reader: pAMM's owner, pump's layout.
    #[test]
    fn reads_a_pamm_user_volume_accumulator() {
        let mut d = [0u8; crate::pump::USER_VOLUME_ACCUMULATOR_LEN];
        put(
            &mut d,
            0,
            &crate::pump::USER_VOLUME_ACCUMULATOR_DISCRIMINATOR,
        );
        put(&mut d, crate::pump::OFF_UVA_USER, &[4; 32]);
        let u = read_user_volume_accumulator(&PROGRAM, &d).unwrap();
        assert_eq!(u.user, [4; 32]);
        assert_eq!(
            read_user_volume_accumulator(&crate::pump::PROGRAM, &d),
            Err(ReadError::WrongOwner)
        );
        d[0] ^= 1;
        assert_eq!(
            read_user_volume_accumulator(&PROGRAM, &d),
            Err(ReadError::WrongTag)
        );
        assert_eq!(DISABLE_BUY_BIT, 0x08);
        assert_eq!(DISABLE_SELL_BIT, 0x10);
    }

    #[test]
    fn boost_vault() {
        let vault = [9u8; 32];
        assert_eq!(boost_vault_amount(&[0; 32], &[], &vault), Ok(0));
        let mut d = [0u8; 165];
        put(&mut d, 0, &NATIVE_MINT);
        put(&mut d, 32, &vault);
        put(&mut d, 64, &17_000_000_000u64.to_le_bytes());
        d[108] = 1;
        put(&mut d, 109, &1u32.to_le_bytes());
        put(&mut d, 113, &2_039_280u64.to_le_bytes());
        assert_eq!(
            boost_vault_amount(&TOKEN_PROGRAM, &d, &vault),
            Ok(17_000_000_000)
        );
        assert_eq!(
            boost_vault_amount(&TOKEN_PROGRAM, &d, &[8; 32]),
            Err(ReadError::Malformed)
        );
        assert_eq!(
            boost_vault_amount(&crate::mint::TOKEN_2022_PROGRAM, &d, &vault),
            Err(ReadError::WrongOwner)
        );
        let mut other = d;
        put(&mut other, 0, &[1; 32]);
        assert_eq!(
            boost_vault_amount(&TOKEN_PROGRAM, &other, &vault),
            Err(ReadError::Malformed)
        );
    }
}
