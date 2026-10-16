//! pump.fun `BondingCurve` and `Global`, the pump-fees `FeeConfig`, and pump's
//! `UserVolumeAccumulator` (ARCH §2.5 `classify_pump`, §3.12 PumpCurve ReadSet / TradeExtras,
//! §4.1, §4.2; R-PUMP §2.1–§2.4, §3.1, §4).
//!
//! **Scope.** M3 used [`read_curve`] for detection (D1 precedence at a Raydium listing, ARCH
//! §2.5). Since M5 this module is the full reader of the curve adapter: the curve's reserves,
//! flags and creator (pricing, the S3 chain, `creator_vault` re-derived at execution), pump's
//! `Global` (the fee and buyback recipient lists short.win checks as well as pump, C0-14; the
//! `pool_migration_fee` of `B̂_str`; `creator_fee_configurable`), and the pump-fees `FeeConfig`
//! (the market-cap fee tiers, read on every quote and never cached, ARCH §4.2). The same
//! `FeeConfig` layout serves PumpSwap's fee config (`5PHirr…`, ARCH §3.12 P4), which is why its
//! reader lives here and takes the pump-fees owner. The fee arithmetic itself is
//! `shortwin_math::fees::pump_curve` / `pump_amm`; this crate only decodes.
//!
//! # `BondingCurve` (PDA `["bonding-curve", mint]`, discriminator `17b7f83760d8ac60`)
//!
//! | Offset | Field | | Offset | Field |
//! |---|---|---|---|---|
//! | 8 | `virtual_token_reserves` u64 | | 81 | `is_mayhem_mode` bool |
//! | 16 | `virtual_quote_reserves` u64 | | 82 | `is_cashback_coin` bool |
//! | 24 | `real_token_reserves` u64 | | 83 | `quote_mint` (default = SOL) |
//! | 32 | `real_quote_reserves` u64 | | 115 | `creator_fee_bps` u64 |
//! | 40 | `token_total_supply` u64 | | 123 | `can_edit_creator_fee` bool |
//! | 48 | `complete` bool | | 124 | `is_holder_reward` bool |
//! | 49 | `creator` | | 125 | `creator_fee` u64 (v3's creator-fee bucket) |
//! | | | | 133 | `protocol_fees` u64 (v3's protocol-fee bucket) |
//! | | | | 141 | `depth` u8 (0 for a SOL / USDC curve) |
//! | | | | 142 | `initial_virtual_quote_reserves` u64 |
//! | | | | 150 | `post_complete_base_out` u64 |
//! | | | | 158 | `post_complete_quote_in` u64 |
//!
//! The six fields from @125 arrived with pump's 2026-10-08 redeploy (deploy slot 454,596,459;
//! IDL `types.BondingCurve`): the v3 trades keep their fees on the curve in the two buckets until
//! pump sweeps them (outside `real_quote_reserves`); `initial_virtual_quote_reserves` is
//! `vQ − rQ` of a `create_v2` curve (pump keeps `vQ − rQ` equal to it, [`BondingCurve::
//! quote_offset_consistent`]); the buy that crosses `rT` by pump's synthetic migration writes
//! `post_complete_base_out` / `post_complete_quote_in` (`pcB` / `pcQ`): the tokens it bought from
//! the would-be PumpSwap pool and the quote it paid into it, which `migrate_v2` adds to the pool
//! on top of `rQ − pool_migration_fee` (pump-final §4, §5.4).
//!
//! **Length variants** (every one is live on mainnet and in `tests/fixtures/accounts/pump*/`;
//! fields past the end read as 0 / false / default — pump itself zero-pads them, [DOCS]
//! `PUMP_PROGRAM_README.md` — and so does this reader):
//!
//! - [`CURVE_LEN_2026_10_08`] = 166: what `create_v2` allocates since pump's 2026-10-08 redeploy,
//!   and what every v2 / v3 trade grows a shorter curve to (the layout through
//!   `post_complete_quote_in`);
//! - [`CURVE_LEN_V2`] = 125: what `create_v2` allocated before (the layout through
//!   `is_holder_reward`, no padding; R-PUMP Update 2026-09-29 U5 corrects §2.1's "151");
//! - [`CURVE_LEN_EXTENDED`] = 151: a curve grown by `extend_account` (clients prepend it, 125 →
//!   151; most live curves before 2026-10-08);
//! - [`CURVE_LEN_LEGACY_EXTENDED`] = 150: pre-2025 curves extended for the creator-fee upgrade
//!   (e.g. Fartcoin's);
//! - [`CURVE_MIN_LEN`] = 115: what the docs say `buy_v2` / `sell_v2` top a shorter curve up to
//!   (about 1.8 M live curves have it, from an earlier deploy), and the shortest curve short.win
//!   lists (ARCH §2.5 `CurveNeedsExtend`);
//! - [`CURVE_LEN_ORIGINAL`] = 49: the 2024 layout (through `complete`: no creator, hence no
//!   creator fee), still held by legacy SPL-Token curves nobody has traded since.
//!
//! pump's 2026-09 build (deploy slot 449,734,335) grew any curve shorter than 125 bytes to
//! [`CURVE_LEN_V2`] on its first v2 trade, the user topping the curve's rent reserve
//! (`lamports − real_quote_reserves`) up to `minimum_balance(125)`; the 2026-10-02 build grew it to
//! 141 and the 2026-10-08 build grows it to [`CURVE_LEN_2026_10_08`], the reserve net of the two
//! fee buckets (`tests/litesvm/tests/pump_math_it.rs`). The reader is indifferent to all of these
//! lengths.
//!
//! Lifecycle (R-PUMP §7; Corrections 1): live while `!complete`; `complete` with `vT > 0` is
//! "migrating" (every trade reverts 6005, `migrate_v2` not yet run); `complete` with `vT == 0` has
//! migrated (to its canonical PumpSwap pool if that exists, elsewhere otherwise).
//!
//! # `Global` (PDA `["global"]` = `4wTV1Y…`, discriminator `a7e8e8b1c86c727f`, 1,087 bytes)
//!
//! A borrowed view ([`Global`]): the three recipient lists alone are 768 bytes, so the program
//! reads fields in place instead of copying the account onto its stack. Offsets per [IDL]
//! `types.Global`, checked against R-PUMP §2.2's live decode: `fee_recipient` @41 plus
//! `fee_recipients[7]` @162 (the 8 normal recipients), `withdraw_authority` @113,
//! `pool_migration_fee` @146, `reserved_fee_recipient` @483 plus `reserved_fee_recipients[7]` @516
//! (mayhem coins), `buyback_fee_recipients[8]` @741, `buyback_basis_points` @997,
//! `creator_fee_configurable` @1045. Since pump's 2026-10-08 redeploy the account is 1,088 bytes
//! ([`GLOBAL_LEN_2026_10_08`]): `max_curve_depth` u8 @1087 ([`OFF_GLOBAL_MAX_CURVE_DEPTH`]), which
//! short.win does not read; [`GLOBAL_LEN`] stays the minimum.
//!
//! # pump-fees `FeeConfig` (PDA `["fee_config", program]` under `pfee`, discriminator `8f3492bbdb7b4c9b`)
//!
//! Borsh: `bump u8` @8, `admin` @9, `flat_fees` @41, `fee_tiers: Vec<FeeTier>` @65 (u32 length,
//! then 40-byte tiers `{threshold u128, lp u64, protocol u64, creator u64}`), `stable_fee_tiers:
//! Vec<FeeTier>`, `exotic_flat_fees` (R-PUMP §2.3; ARCH §4.1). The account is allocated 4,097
//! bytes; the Borsh value is a prefix of it. A tier list that runs past the end of the account is
//! `TooShort`.
//!
//! The owner is supplied by the caller and checked here; key checks (the curve PDA of the mint,
//! the `Global` and `FeeConfig` addresses) are the program's.

use crate::bytes::{has_discriminator, key_at, u8_at, u32_at, u64_at, u128_at};
use crate::{Key, ReadError};

/// pump program `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`.
pub const PROGRAM: Key = [
    1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170, 81, 137, 203, 151, 245,
    210, 255, 59, 101, 93, 43, 182, 253, 109, 24, 176,
];

/// pump-fees program `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` (owner of both `FeeConfig`s).
pub const FEE_PROGRAM: Key = [
    12, 53, 255, 169, 5, 90, 142, 86, 141, 168, 247, 188, 7, 86, 21, 39, 76, 241, 201, 44, 164, 31,
    64, 0, 156, 81, 106, 164, 20, 194, 124, 112,
];

// ---- seeds (R-PUMP §3.1; ARCH §3.12 seed gotchas) ---------------------------------------------

/// pump `Global`: `PDA(pump, ["global"])`.
pub const GLOBAL_SEED: &[u8] = b"global";
/// `PDA(pump, ["bonding-curve", mint])`.
pub const BONDING_CURVE_SEED: &[u8] = b"bonding-curve";
/// `PDA(pump, ["bonding-curve-v2", mint])` (a legacy-instruction remaining account; uninitialised).
pub const BONDING_CURVE_V2_SEED: &[u8] = b"bonding-curve-v2";
/// `PDA(pump, ["creator-vault", curve.creator])` (**hyphen**; PumpSwap's is `creator_vault`).
pub const CREATOR_VAULT_SEED: &[u8] = b"creator-vault";
/// `PDA(pump, ["pool-authority", mint])`: the canonical PumpSwap pool's `creator`.
pub const POOL_AUTHORITY_SEED: &[u8] = b"pool-authority";
/// `PDA(pump, ["user_volume_accumulator", user])`.
pub const USER_VOLUME_ACCUMULATOR_SEED: &[u8] = b"user_volume_accumulator";
/// `PDA(pump, ["global_volume_accumulator"])`.
pub const GLOBAL_VOLUME_ACCUMULATOR_SEED: &[u8] = b"global_volume_accumulator";
/// Anchor's `emit_cpi!` authority: `PDA(program, ["__event_authority"])`.
pub const EVENT_AUTHORITY_SEED: &[u8] = b"__event_authority";
/// `PDA(pfee, ["fee_config", program])`.
pub const FEE_CONFIG_SEED: &[u8] = b"fee_config";
/// `PDA(pfee, ["sharing-config", mint])` (may be uninitialised).
pub const SHARING_CONFIG_SEED: &[u8] = b"sharing-config";

// ---- BondingCurve -------------------------------------------------------------------------

/// `sha256("account:BondingCurve")[..8]`.
pub const BONDING_CURVE_DISCRIMINATOR: [u8; 8] = [0x17, 0xb7, 0xf8, 0x37, 0x60, 0xd8, 0xac, 0x60];

/// ARCH §2.5: a curve shorter than this must be extended before short.win lists it (the docs'
/// `buy_v2` / `sell_v2` top-up length; the deployed ELF grows shorter curves to
/// [`CURVE_LEN_V2`]).
pub const CURVE_MIN_LEN: usize = 115;
/// What `create_v2` allocates and every v2 / v3 trade grows a shorter curve to since pump's
/// 2026-10-08 redeploy (deploy slot 454,596,459): the layout through `post_complete_quote_in`.
pub const CURVE_LEN_2026_10_08: usize = 166;
/// What `create_v2` allocated before 2026-10-08: the layout through `is_holder_reward`.
pub const CURVE_LEN_V2: usize = 125;
/// A curve grown by `extend_account` today (125 → 151).
pub const CURVE_LEN_EXTENDED: usize = 151;
/// A pre-2025 curve extended for the creator-fee upgrade.
pub const CURVE_LEN_LEGACY_EXTENDED: usize = 150;
/// The 2024 layout: discriminator, five reserves / supply, `complete`.
pub const CURVE_LEN_ORIGINAL: usize = 49;

/// `virtual_token_reserves` (u64).
pub const OFF_VIRTUAL_TOKEN_RESERVES: usize = 8;
/// `virtual_quote_reserves` (u64; formerly `virtual_sol_reserves`).
pub const OFF_VIRTUAL_QUOTE_RESERVES: usize = 16;
/// `real_token_reserves` (u64).
pub const OFF_REAL_TOKEN_RESERVES: usize = 24;
/// `real_quote_reserves` (u64).
pub const OFF_REAL_QUOTE_RESERVES: usize = 32;
/// `token_total_supply` (u64).
pub const OFF_TOKEN_TOTAL_SUPPLY: usize = 40;
/// `complete` (bool).
pub const OFF_COMPLETE: usize = 48;
/// `creator` (key).
pub const OFF_CREATOR: usize = 49;
/// `is_mayhem_mode` (bool).
pub const OFF_IS_MAYHEM_MODE: usize = 81;
/// `is_cashback_coin` (bool).
pub const OFF_IS_CASHBACK_COIN: usize = 82;
/// `quote_mint` (key; the default key means SOL).
pub const OFF_QUOTE_MINT: usize = 83;
/// `creator_fee_bps` (u64).
pub const OFF_CREATOR_FEE_BPS: usize = 115;
/// `can_edit_creator_fee` (bool; reserved, always false).
pub const OFF_CAN_EDIT_CREATOR_FEE: usize = 123;
/// `is_holder_reward` (bool).
pub const OFF_IS_HOLDER_REWARD: usize = 124;
/// `creator_fee` (u64): the creator fee a v3 trade keeps on the curve until pump sweeps it.
pub const OFF_CREATOR_FEE_BUCKET: usize = 125;
/// `protocol_fees` (u64): the protocol fee (net of the buyback) a v3 trade keeps on the curve.
pub const OFF_PROTOCOL_FEES_BUCKET: usize = 133;
/// `depth` (u8): 0 for a SOL / USDC curve, `n` for a curve quoted in a depth-`n − 1` pump coin.
pub const OFF_DEPTH: usize = 141;
/// `initial_virtual_quote_reserves` (u64): `vQ − rQ` of a `create_v2` curve.
pub const OFF_INITIAL_VIRTUAL_QUOTE_RESERVES: usize = 142;
/// `post_complete_base_out` (u64, `pcB`): written by the completing buy's synthetic pool part.
pub const OFF_POST_COMPLETE_BASE_OUT: usize = 150;
/// `post_complete_quote_in` (u64, `pcQ`): written by the completing buy's synthetic pool part.
pub const OFF_POST_COMPLETE_QUOTE_IN: usize = 158;

/// The fields of a `BondingCurve` short.win reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BondingCurve {
    pub virtual_token_reserves: u64,
    pub virtual_quote_reserves: u64,
    pub real_token_reserves: u64,
    pub real_quote_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    /// The creator-fee recipient key (`creator_vault` derives from it; admin-mutable, so it is
    /// re-read at execution). The default key on a curve without a creator (the creator fee is
    /// then 0, R-PUMP §4 step 5).
    pub creator: Key,
    pub is_mayhem_mode: bool,
    pub is_cashback_coin: bool,
    /// `Pubkey::default()` for a SOL curve.
    pub quote_mint: Key,
    /// Per-coin creator fee (custom pairs; 0 = use the schedule).
    pub creator_fee_bps: u64,
    pub can_edit_creator_fee: bool,
    pub is_holder_reward: bool,
    /// `creator_fee` @125: the creator-fee bucket (v3 trades; outside `rQ`).
    pub creator_fee_bucket: u64,
    /// `protocol_fees` @133: the protocol-fee bucket (v3 trades; outside `rQ`).
    pub protocol_fees_bucket: u64,
    /// `depth` @141: 0 for a SOL / USDC curve (a plain `u8`, not a bool).
    pub depth: u8,
    /// `initial_virtual_quote_reserves` @142 (`ivqr`): `vQ − rQ` of a `create_v2` curve; 0 on a
    /// legacy curve pump never backfilled.
    pub initial_virtual_quote_reserves: u64,
    /// `post_complete_base_out` @150 (`pcB`): the tokens the completing buy took from the
    /// would-be pool (synthetic migration); 0 without a pool part.
    pub post_complete_base_out: u64,
    /// `post_complete_quote_in` @158 (`pcQ`): the quote the completing buy paid into the would-be
    /// pool (fees excluded); `migrate_v2` adds it to the pool. 0 without a pool part.
    pub post_complete_quote_in: u64,
}

impl BondingCurve {
    /// Quoted in SOL (`quote_mint == Pubkey::default()`): the only curves short.win lists
    /// (ADOPTED Q6).
    pub fn is_sol_quote(&self) -> bool {
        self.quote_mint == [0; 32]
    }

    /// `creator == Pubkey::default()`: pump charges no creator fee (R-PUMP §4 step 5).
    pub fn creator_is_default(&self) -> bool {
        self.creator == [0; 32]
    }

    /// The curve trades (`!complete`); every buy and sell of a complete curve reverts with 6005.
    pub fn is_live(&self) -> bool {
        !self.complete
    }

    /// Complete with `vT > 0`: `migrate_v2` has not run yet (ARCH §2.5 `PumpMigrating` when no
    /// canonical pool exists).
    pub fn is_migrating(&self) -> bool {
        self.complete && self.virtual_token_reserves > 0
    }

    /// Complete with `vT == 0`: migrated (the virtual reserves are zeroed by `migrate`).
    pub fn is_migrated(&self) -> bool {
        self.complete && self.virtual_token_reserves == 0
    }

    /// pump's invariant `vQ − rQ == initial_virtual_quote_reserves` (vacuous when `ivqr` is 0 —
    /// legacy curves pump never backfilled — or after migration, `vT == 0`). Every trade keeps it
    /// (`rQ` excludes a synthetic buy's pool part, pump-final V5); a mayhem curve breaks it.
    pub fn quote_offset_consistent(&self) -> bool {
        self.initial_virtual_quote_reserves == 0
            || self.virtual_token_reserves == 0
            || self
                .virtual_quote_reserves
                .checked_sub(self.real_quote_reserves)
                == Some(self.initial_virtual_quote_reserves)
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

/// Reads a `BondingCurve`: owner == [`PROGRAM`], discriminator
/// [`BONDING_CURVE_DISCRIMINATOR`], every `bool` 0 or 1; fields past the end of a short
/// account read as zero (the length rule is [`CURVE_MIN_LEN`], the caller's).
pub fn read_curve(owner: &Key, data: &[u8]) -> Result<BondingCurve, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &BONDING_CURVE_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    Ok(BondingCurve {
        virtual_token_reserves: u64_at(data, OFF_VIRTUAL_TOKEN_RESERVES),
        virtual_quote_reserves: u64_at(data, OFF_VIRTUAL_QUOTE_RESERVES),
        real_token_reserves: u64_at(data, OFF_REAL_TOKEN_RESERVES),
        real_quote_reserves: u64_at(data, OFF_REAL_QUOTE_RESERVES),
        token_total_supply: u64_at(data, OFF_TOKEN_TOTAL_SUPPLY),
        complete: bool_at(data, OFF_COMPLETE)?,
        creator: key_at(data, OFF_CREATOR),
        is_mayhem_mode: bool_at(data, OFF_IS_MAYHEM_MODE)?,
        is_cashback_coin: bool_at(data, OFF_IS_CASHBACK_COIN)?,
        quote_mint: key_at(data, OFF_QUOTE_MINT),
        creator_fee_bps: u64_at(data, OFF_CREATOR_FEE_BPS),
        can_edit_creator_fee: bool_at(data, OFF_CAN_EDIT_CREATOR_FEE)?,
        is_holder_reward: bool_at(data, OFF_IS_HOLDER_REWARD)?,
        creator_fee_bucket: u64_at(data, OFF_CREATOR_FEE_BUCKET),
        protocol_fees_bucket: u64_at(data, OFF_PROTOCOL_FEES_BUCKET),
        depth: u8_at(data, OFF_DEPTH),
        initial_virtual_quote_reserves: u64_at(data, OFF_INITIAL_VIRTUAL_QUOTE_RESERVES),
        post_complete_base_out: u64_at(data, OFF_POST_COMPLETE_BASE_OUT),
        post_complete_quote_in: u64_at(data, OFF_POST_COMPLETE_QUOTE_IN),
    })
}

// ---- Global -------------------------------------------------------------------------------

/// `sha256("account:Global")[..8]`.
pub const GLOBAL_DISCRIMINATOR: [u8; 8] = [0xa7, 0xe8, 0xe8, 0xb1, 0xc8, 0x6c, 0x72, 0x7f];
/// The `Global` layout's length before pump's 2026-10-08 redeploy: the minimum [`read_global`]
/// accepts (every field short.win reads lies in it).
pub const GLOBAL_LEN: usize = 1_087;
/// `Global.max_curve_depth` (u8; 2026-10-08): the deepest quote chain `create_v2` accepts. Not
/// read by short.win (it lists SOL curves only, `depth == 0`).
pub const OFF_GLOBAL_MAX_CURVE_DEPTH: usize = 1_087;
/// The `Global` allocation since pump's 2026-10-08 redeploy (tests only).
pub const GLOBAL_LEN_2026_10_08: usize = 1_088;
/// Recipients per list (a list's head field plus its 7-entry array, or the 8-entry buyback list).
pub const RECIPIENTS: usize = 8;

/// `initialized` (bool).
pub const OFF_GLOBAL_INITIALIZED: usize = 8;
/// `authority` (key).
pub const OFF_GLOBAL_AUTHORITY: usize = 9;
/// `fee_recipient` (key): normal recipient #0.
pub const OFF_GLOBAL_FEE_RECIPIENT: usize = 41;
/// `initial_virtual_token_reserves` (u64).
pub const OFF_GLOBAL_INITIAL_VIRTUAL_TOKEN_RESERVES: usize = 73;
/// `initial_virtual_sol_reserves` (u64).
pub const OFF_GLOBAL_INITIAL_VIRTUAL_SOL_RESERVES: usize = 81;
/// `initial_real_token_reserves` (u64).
pub const OFF_GLOBAL_INITIAL_REAL_TOKEN_RESERVES: usize = 89;
/// `token_total_supply` (u64).
pub const OFF_GLOBAL_TOKEN_TOTAL_SUPPLY: usize = 97;
/// `fee_basis_points` (u64; fallback only, the fee comes from `FeeConfig`).
pub const OFF_GLOBAL_FEE_BASIS_POINTS: usize = 105;
/// `withdraw_authority` (key; passed, not signed, to `migrate_v2`).
pub const OFF_GLOBAL_WITHDRAW_AUTHORITY: usize = 113;
/// `enable_migrate` (bool).
pub const OFF_GLOBAL_ENABLE_MIGRATE: usize = 145;
/// `pool_migration_fee` (u64): lamports the curve keeps at migration (`B̂_str`'s fee).
pub const OFF_GLOBAL_POOL_MIGRATION_FEE: usize = 146;
/// `creator_fee_basis_points` (u64; fallback only).
pub const OFF_GLOBAL_CREATOR_FEE_BASIS_POINTS: usize = 154;
/// `fee_recipients[7]` (normal recipients #1–#7).
pub const OFF_GLOBAL_FEE_RECIPIENTS: usize = 162;
/// `set_creator_authority` (key).
pub const OFF_GLOBAL_SET_CREATOR_AUTHORITY: usize = 386;
/// `admin_set_creator_authority` (key).
pub const OFF_GLOBAL_ADMIN_SET_CREATOR_AUTHORITY: usize = 418;
/// `create_v2_enabled` (bool).
pub const OFF_GLOBAL_CREATE_V2_ENABLED: usize = 450;
/// `whitelist_pda` (key; the mayhem program's fee-exempt `sol-vault`).
pub const OFF_GLOBAL_WHITELIST_PDA: usize = 451;
/// `reserved_fee_recipient` (key): reserved recipient #0 (mayhem coins).
pub const OFF_GLOBAL_RESERVED_FEE_RECIPIENT: usize = 483;
/// `mayhem_mode_enabled` (bool).
pub const OFF_GLOBAL_MAYHEM_MODE_ENABLED: usize = 515;
/// `reserved_fee_recipients[7]` (reserved recipients #1–#7).
pub const OFF_GLOBAL_RESERVED_FEE_RECIPIENTS: usize = 516;
/// `is_cashback_enabled` (bool).
pub const OFF_GLOBAL_IS_CASHBACK_ENABLED: usize = 740;
/// `buyback_fee_recipients[8]`.
pub const OFF_GLOBAL_BUYBACK_FEE_RECIPIENTS: usize = 741;
/// `buyback_basis_points` (u64): the protocol fee's share routed to the buyback recipient.
pub const OFF_GLOBAL_BUYBACK_BASIS_POINTS: usize = 997;
/// `initial_virtual_quote_reserves` (u64; USDC curves).
pub const OFF_GLOBAL_INITIAL_VIRTUAL_QUOTE_RESERVES: usize = 1_005;
/// `whitelisted_quote_mints[1]`.
pub const OFF_GLOBAL_WHITELISTED_QUOTE_MINTS: usize = 1_013;
/// `creator_fee_configurable` (bool): gate of a curve's own `creator_fee_bps`.
pub const OFF_GLOBAL_CREATOR_FEE_CONFIGURABLE: usize = 1_045;
/// `max_configurable_creator_fee_bps` (u64).
pub const OFF_GLOBAL_MAX_CONFIGURABLE_CREATOR_FEE_BPS: usize = 1_046;
/// `holder_reward_claim_authority` (key).
pub const OFF_GLOBAL_HOLDER_REWARD_CLAIM_AUTHORITY: usize = 1_054;
/// `is_holder_reward_enabled` (bool).
pub const OFF_GLOBAL_IS_HOLDER_REWARD_ENABLED: usize = 1_086;

/// The `bool` fields of `Global`, validated by [`read_global`].
const GLOBAL_BOOLS: [usize; 7] = [
    OFF_GLOBAL_INITIALIZED,
    OFF_GLOBAL_ENABLE_MIGRATE,
    OFF_GLOBAL_CREATE_V2_ENABLED,
    OFF_GLOBAL_MAYHEM_MODE_ENABLED,
    OFF_GLOBAL_IS_CASHBACK_ENABLED,
    OFF_GLOBAL_CREATOR_FEE_CONFIGURABLE,
    OFF_GLOBAL_IS_HOLDER_REWARD_ENABLED,
];

/// pump `Global`, read in place (see the module docs). Built by [`read_global`], which checked
/// the owner, the discriminator, the length and every `bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Global<'a> {
    data: &'a [u8],
}

/// Reads pump's `Global`: owner == [`PROGRAM`], discriminator [`GLOBAL_DISCRIMINATOR`], at least
/// [`GLOBAL_LEN`] bytes (pump deserialises the whole struct, so a shorter account is not a
/// `Global` it can trade with), every `bool` 0 or 1.
pub fn read_global<'a>(owner: &Key, data: &'a [u8]) -> Result<Global<'a>, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &GLOBAL_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    if data.len() < GLOBAL_LEN {
        return Err(ReadError::TooShort);
    }
    for off in GLOBAL_BOOLS {
        bool_at(data, off)?;
    }
    Ok(Global { data })
}

/// An 8-entry key array at `offset`.
fn keys_at(data: &[u8], offset: usize) -> [Key; RECIPIENTS] {
    core::array::from_fn(|i| key_at(data, offset.saturating_add(i.saturating_mul(32))))
}

/// A recipient list stored as a head key at `head` and a 7-entry array at `rest`.
fn head_and_rest(data: &[u8], head: usize, rest: usize) -> [Key; RECIPIENTS] {
    core::array::from_fn(|i| match i.checked_sub(1) {
        None => key_at(data, head),
        Some(j) => key_at(data, rest.saturating_add(j.saturating_mul(32))),
    })
}

impl Global<'_> {
    fn flag(&self, offset: usize) -> bool {
        u8_at(self.data, offset) == 1
    }

    /// `authority` (`set_params` and the toggles).
    pub fn authority(&self) -> Key {
        key_at(self.data, OFF_GLOBAL_AUTHORITY)
    }

    /// The 8 normal fee recipients: `fee_recipient` then `fee_recipients[7]` (R-PUMP §3.1
    /// recipient rules: a non-mayhem trade's `fee_recipient` must be one of them).
    pub fn fee_recipients(&self) -> [Key; RECIPIENTS] {
        head_and_rest(
            self.data,
            OFF_GLOBAL_FEE_RECIPIENT,
            OFF_GLOBAL_FEE_RECIPIENTS,
        )
    }

    /// `key` is one of the 8 normal fee recipients.
    pub fn is_fee_recipient(&self, key: &Key) -> bool {
        self.fee_recipients().contains(key)
    }

    /// The 8 reserved fee recipients (mayhem coins): `reserved_fee_recipient` then
    /// `reserved_fee_recipients[7]`.
    pub fn reserved_fee_recipients(&self) -> [Key; RECIPIENTS] {
        head_and_rest(
            self.data,
            OFF_GLOBAL_RESERVED_FEE_RECIPIENT,
            OFF_GLOBAL_RESERVED_FEE_RECIPIENTS,
        )
    }

    /// `key` is one of the 8 reserved fee recipients.
    pub fn is_reserved_fee_recipient(&self, key: &Key) -> bool {
        self.reserved_fee_recipients().contains(key)
    }

    /// `buyback_fee_recipients[8]` (a trade's `buyback_fee_recipient` must be one, error 6057).
    pub fn buyback_fee_recipients(&self) -> [Key; RECIPIENTS] {
        keys_at(self.data, OFF_GLOBAL_BUYBACK_FEE_RECIPIENTS)
    }

    /// `key` is one of the 8 buyback fee recipients.
    pub fn is_buyback_fee_recipient(&self, key: &Key) -> bool {
        self.buyback_fee_recipients().contains(key)
    }

    /// `withdraw_authority` (`migrate_v2` account 1, not a signer).
    pub fn withdraw_authority(&self) -> Key {
        key_at(self.data, OFF_GLOBAL_WITHDRAW_AUTHORITY)
    }

    /// `pool_migration_fee`: lamports the curve keeps at migration (ARCH §4.3 `B̂_str`).
    pub fn pool_migration_fee(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_POOL_MIGRATION_FEE)
    }

    /// `creator_fee_configurable`: a curve's own `creator_fee_bps` replaces the schedule's.
    pub fn creator_fee_configurable(&self) -> bool {
        self.flag(OFF_GLOBAL_CREATOR_FEE_CONFIGURABLE)
    }

    /// `max_configurable_creator_fee_bps`.
    pub fn max_configurable_creator_fee_bps(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_MAX_CONFIGURABLE_CREATOR_FEE_BPS)
    }

    /// `buyback_basis_points` (a split of the protocol fee; the trader's cost is unchanged).
    pub fn buyback_basis_points(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_BUYBACK_BASIS_POINTS)
    }

    /// `fee_basis_points` (the fallback protocol fee; `FeeConfig` decides).
    pub fn fee_basis_points(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_FEE_BASIS_POINTS)
    }

    /// `creator_fee_basis_points` (the fallback creator fee; `FeeConfig` decides).
    pub fn creator_fee_basis_points(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_CREATOR_FEE_BASIS_POINTS)
    }

    /// `initial_virtual_token_reserves` (1.073·10¹⁵ today).
    pub fn initial_virtual_token_reserves(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_INITIAL_VIRTUAL_TOKEN_RESERVES)
    }

    /// `initial_virtual_sol_reserves` (30 SOL today).
    pub fn initial_virtual_sol_reserves(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_INITIAL_VIRTUAL_SOL_RESERVES)
    }

    /// `initial_real_token_reserves` (793.1·10¹² today: the tokens a standard curve sells).
    pub fn initial_real_token_reserves(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_INITIAL_REAL_TOKEN_RESERVES)
    }

    /// `token_total_supply` (10¹⁵ today).
    pub fn token_total_supply(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_TOKEN_TOTAL_SUPPLY)
    }

    /// `initial_virtual_quote_reserves` (USDC curves).
    pub fn initial_virtual_quote_reserves(&self) -> u64 {
        u64_at(self.data, OFF_GLOBAL_INITIAL_VIRTUAL_QUOTE_RESERVES)
    }

    /// `whitelisted_quote_mints[0]` (USDC today).
    pub fn whitelisted_quote_mint(&self) -> Key {
        key_at(self.data, OFF_GLOBAL_WHITELISTED_QUOTE_MINTS)
    }

    /// `whitelist_pda` (fee-exempt trades: the mayhem program's `sol-vault`).
    pub fn whitelist_pda(&self) -> Key {
        key_at(self.data, OFF_GLOBAL_WHITELIST_PDA)
    }

    /// `set_creator_authority` (can rewrite `BondingCurve.creator`).
    pub fn set_creator_authority(&self) -> Key {
        key_at(self.data, OFF_GLOBAL_SET_CREATOR_AUTHORITY)
    }

    /// `enable_migrate` (IDL: unused).
    pub fn enable_migrate(&self) -> bool {
        self.flag(OFF_GLOBAL_ENABLE_MIGRATE)
    }

    /// `mayhem_mode_enabled`.
    pub fn mayhem_mode_enabled(&self) -> bool {
        self.flag(OFF_GLOBAL_MAYHEM_MODE_ENABLED)
    }

    /// `create_v2_enabled`.
    pub fn create_v2_enabled(&self) -> bool {
        self.flag(OFF_GLOBAL_CREATE_V2_ENABLED)
    }
}

// ---- pump-fees FeeConfig ----------------------------------------------------------------------

/// `sha256("account:FeeConfig")[..8]`.
pub const FEE_CONFIG_DISCRIMINATOR: [u8; 8] = [0x8f, 0x34, 0x92, 0xbb, 0xdb, 0x7b, 0x4c, 0x9b];
/// The allocation of both mainnet `FeeConfig`s.
pub const FEE_CONFIG_ALLOC: usize = 4_097;
/// `bump` (u8).
pub const OFF_FEE_CONFIG_BUMP: usize = 8;
/// `admin` (key).
pub const OFF_FEE_CONFIG_ADMIN: usize = 9;
/// `flat_fees` (`Fees`, 24 bytes).
pub const OFF_FEE_CONFIG_FLAT_FEES: usize = 41;
/// `fee_tiers` (u32 length, then the tiers).
pub const OFF_FEE_CONFIG_FEE_TIERS: usize = 65;
/// `Fees` = `{lp_fee_bps, protocol_fee_bps, creator_fee_bps}` (3 × u64).
pub const FEES_LEN: usize = 24;
/// `FeeTier` = `{market_cap_lamports_threshold u128, fees}`.
pub const FEE_TIER_LEN: usize = 40;

/// pump-fees `Fees` in bps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fees {
    /// `lp_fee_bps` (PumpSwap only; the curve charges none).
    pub lp: u64,
    /// `protocol_fee_bps` (including the buyback share).
    pub protocol: u64,
    /// `creator_fee_bps`.
    pub creator: u64,
}

/// One pump-fees `FeeTier`: `fees` apply from market cap `threshold` upwards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeeTier {
    /// `market_cap_lamports_threshold` (quote atoms for the stable tiers).
    pub threshold: u128,
    pub fees: Fees,
}

fn fees_at(data: &[u8], offset: usize) -> Fees {
    Fees {
        lp: u64_at(data, offset),
        protocol: u64_at(data, offset.saturating_add(8)),
        creator: u64_at(data, offset.saturating_add(16)),
    }
}

/// A `Vec<FeeTier>` read in place: exactly `len · 40` bytes, in account order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeTiers<'a> {
    bytes: &'a [u8],
}

impl<'a> FeeTiers<'a> {
    /// How many tiers.
    pub fn len(&self) -> usize {
        self.bytes.len().checked_div(FEE_TIER_LEN).unwrap_or(0)
    }

    /// No tiers.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Tier `i` (account order).
    pub fn get(&self, i: usize) -> Option<FeeTier> {
        let off = i.checked_mul(FEE_TIER_LEN)?;
        let tier = self.bytes.get(off..off.checked_add(FEE_TIER_LEN)?)?;
        Some(FeeTier {
            threshold: u128_at(tier, 0),
            fees: fees_at(tier, 16),
        })
    }

    /// The tiers in account order.
    pub fn iter(&self) -> impl Iterator<Item = FeeTier> + 'a {
        let bytes = self.bytes;
        bytes.as_chunks::<FEE_TIER_LEN>().0.iter().map(|t| FeeTier {
            threshold: u128_at(t, 0),
            fees: fees_at(t, 16),
        })
    }

    /// pump-fees `calculate_fee_tier` on these tiers, streamed (no copy): the **last** tier in
    /// account order whose threshold is `≤ mcap`, or tier 0 if there is none (on the sorted
    /// tiers pump-fees enforces: tier 0 below its threshold; R-PUMP §4 step 3; R-VEN §2.3).
    /// `None` for an empty list. Equal to
    /// `shortwin_math::fees::fee_tier` on the same tiers (differential-tested in
    /// `tests/litesvm/tests/pump_math_it.rs`).
    pub fn select(&self, mcap: u128) -> Option<Fees> {
        let first = self.get(0)?;
        let mut chosen = first.fees;
        for t in self.iter() {
            if mcap >= t.threshold {
                chosen = t.fees;
            }
        }
        Some(chosen)
    }
}

/// pump-fees `FeeConfig`, read in place. Built by [`read_fee_config`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeConfig<'a> {
    pub bump: u8,
    pub admin: Key,
    /// Non-canonical PumpSwap pools; the exotic schedule's fallback.
    pub flat_fees: Fees,
    /// SOL-like quotes (default / WSOL / Token-2022 native): the pump curve's schedule and a
    /// canonical SOL PumpSwap pool's.
    pub fee_tiers: FeeTiers<'a>,
    /// USDC quotes.
    pub stable_fee_tiers: FeeTiers<'a>,
    /// Other quotes (`flat_fees` if all zero).
    pub exotic_flat_fees: Fees,
}

/// A `u32`-prefixed tier vector at `offset`: `(tiers, offset after it)`.
fn tiers_at(data: &[u8], offset: usize) -> Result<(FeeTiers<'_>, usize), ReadError> {
    let len = usize::try_from(u32_at(data, offset)).map_err(|_| ReadError::Malformed)?;
    let start = offset.checked_add(4).ok_or(ReadError::Malformed)?;
    let end = len
        .checked_mul(FEE_TIER_LEN)
        .and_then(|n| n.checked_add(start))
        .ok_or(ReadError::Malformed)?;
    let bytes = data.get(start..end).ok_or(ReadError::TooShort)?;
    Ok((FeeTiers { bytes }, end))
}

/// Reads a pump-fees `FeeConfig` (pump's `8Wf5Ti…` or PumpSwap's `5PHirr…`): owner ==
/// [`FEE_PROGRAM`], discriminator [`FEE_CONFIG_DISCRIMINATOR`], both tier vectors and the
/// trailing `exotic_flat_fees` inside the account (else `TooShort`).
pub fn read_fee_config<'a>(owner: &Key, data: &'a [u8]) -> Result<FeeConfig<'a>, ReadError> {
    if *owner != FEE_PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &FEE_CONFIG_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    let (fee_tiers, next) = tiers_at(data, OFF_FEE_CONFIG_FEE_TIERS)?;
    let (stable_fee_tiers, next) = tiers_at(data, next)?;
    let end = next.checked_add(FEES_LEN).ok_or(ReadError::Malformed)?;
    if data.len() < end {
        return Err(ReadError::TooShort);
    }
    Ok(FeeConfig {
        bump: u8_at(data, OFF_FEE_CONFIG_BUMP),
        admin: key_at(data, OFF_FEE_CONFIG_ADMIN),
        flat_fees: fees_at(data, OFF_FEE_CONFIG_FLAT_FEES),
        fee_tiers,
        stable_fee_tiers,
        exotic_flat_fees: fees_at(data, next),
    })
}

// ---- UserVolumeAccumulator ----------------------------------------------------------------------

/// `sha256("account:UserVolumeAccumulator")[..8]` (pump's and PumpSwap's).
pub const USER_VOLUME_ACCUMULATOR_DISCRIMINATOR: [u8; 8] =
    [0x56, 0xff, 0x70, 0x0e, 0x66, 0x35, 0x9a, 0xfa];
/// The allocation of a pump UVA (`init_user_volume_accumulator`, rent 1,346,200 lamports today).
pub const USER_VOLUME_ACCUMULATOR_LEN: usize = 137;
/// `user` (key).
pub const OFF_UVA_USER: usize = 8;
/// `needs_claim` (bool).
pub const OFF_UVA_NEEDS_CLAIM: usize = 40;
/// `current_sol_volume` (u64).
pub const OFF_UVA_CURRENT_SOL_VOLUME: usize = 57;
/// `cashback_earned` (u64; lamports owed to the user of a cashback coin).
pub const OFF_UVA_CASHBACK_EARNED: usize = 74;

/// The fields of a pump `UserVolumeAccumulator` short.win reads (ARCH §3.12 T16: the trader's
/// UVA must exist, pre-created at listing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UserVolumeAccumulator {
    /// The user it accumulates for.
    pub user: Key,
    pub needs_claim: bool,
    pub current_sol_volume: u64,
    pub cashback_earned: u64,
}

/// Reads a pump `UserVolumeAccumulator`: owner == [`PROGRAM`], its discriminator, `needs_claim`
/// 0 or 1; later fields of a shorter account read as zero.
pub fn read_user_volume_accumulator(
    owner: &Key,
    data: &[u8],
) -> Result<UserVolumeAccumulator, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &USER_VOLUME_ACCUMULATOR_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    Ok(UserVolumeAccumulator {
        user: key_at(data, OFF_UVA_USER),
        needs_claim: bool_at(data, OFF_UVA_NEEDS_CLAIM)?,
        current_sol_volume: u64_at(data, OFF_UVA_CURRENT_SOL_VOLUME),
        cashback_earned: u64_at(data, OFF_UVA_CASHBACK_EARNED),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
pub(crate) mod tests {
    use super::*;

    /// Fartcoin's curve `TBHe5tJnuT4CQbHorJ1uVdfUoaYGPKgfCpiv2jgesVN` as mainnet holds it
    /// (`getAccountInfo`, slot 451,197,110; 150 bytes, owner pump): complete, every reserve 0,
    /// supply 10¹⁵, no creator — a coin that graduated to Raydium before PumpSwap (ARCH §2.5
    /// "GraduatedElsewhere", ARCH `[V3]`).
    pub const FARTCOIN_CURVE_HEX: &str = "17b7f83760d8ac6000000000000000000000000000000000000000000000000000000000000000000080c6a47e8d0300010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    fn hex(s: &str) -> [u8; 150] {
        let mut out = [0u8; 150];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn reads_fartcoins_graduated_curve() {
        assert_eq!(FARTCOIN_CURVE_HEX.len(), 300);
        let data = hex(FARTCOIN_CURVE_HEX);
        let c = read_curve(&PROGRAM, &data).unwrap();
        assert!(c.complete);
        assert_eq!(
            (
                c.virtual_token_reserves,
                c.virtual_quote_reserves,
                c.real_token_reserves,
                c.real_quote_reserves
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(c.token_total_supply, 1_000_000_000_000_000);
        assert!(!c.is_mayhem_mode && !c.is_cashback_coin);
        assert_eq!(c.quote_mint, [0; 32]);
        assert_eq!(c.creator, [0; 32]);
        assert!(data.len() >= CURVE_MIN_LEN);
        assert_eq!(data.len(), CURVE_LEN_LEGACY_EXTENDED);
        assert!(c.is_migrated() && !c.is_migrating() && !c.is_live());
        assert!(c.is_sol_quote() && c.creator_is_default());
    }

    #[test]
    fn refusals_and_zero_padding() {
        let data = hex(FARTCOIN_CURVE_HEX);
        assert_eq!(read_curve(&[0; 32], &data), Err(ReadError::WrongOwner));
        let mut bad = data;
        bad[0] ^= 1;
        assert_eq!(read_curve(&PROGRAM, &bad), Err(ReadError::WrongTag));
        let mut bad = data;
        bad[OFF_COMPLETE] = 2;
        assert_eq!(read_curve(&PROGRAM, &bad), Err(ReadError::Malformed));
        let mut bad = data;
        bad[OFF_IS_MAYHEM_MODE] = 7;
        assert_eq!(read_curve(&PROGRAM, &bad), Err(ReadError::Malformed));
        for off in [OFF_CAN_EDIT_CREATOR_FEE, OFF_IS_HOLDER_REWARD] {
            let mut bad = data;
            bad[off] = 2;
            assert_eq!(read_curve(&PROGRAM, &bad), Err(ReadError::Malformed));
        }
        // A 49-byte curve (before `creator` existed) reads its trailing fields as zero.
        let short = read_curve(&PROGRAM, &data[..CURVE_LEN_ORIGINAL]).unwrap();
        assert!(short.complete && !short.is_mayhem_mode);
        assert_eq!(short.quote_mint, [0; 32]);
        assert_eq!(read_curve(&PROGRAM, &data[..7]), Err(ReadError::WrongTag));
    }

    #[test]
    fn layout_constants() {
        assert_eq!(OFF_IS_HOLDER_REWARD + 1, CURVE_LEN_V2);
        assert_eq!(CURVE_LEN_V2 + 26, CURVE_LEN_EXTENDED);
        assert_eq!(OFF_CREATOR_FEE_BPS, CURVE_MIN_LEN);
        assert_eq!(OFF_COMPLETE + 1, CURVE_LEN_ORIGINAL);
        // The 2026-10-08 fields (IDL `types.BondingCurve`): two u64 buckets, a u8, three u64s.
        assert_eq!(OFF_CREATOR_FEE_BUCKET, CURVE_LEN_V2);
        assert_eq!(OFF_CREATOR_FEE_BUCKET + 8, OFF_PROTOCOL_FEES_BUCKET);
        assert_eq!(OFF_PROTOCOL_FEES_BUCKET + 8, OFF_DEPTH);
        assert_eq!(OFF_DEPTH + 1, OFF_INITIAL_VIRTUAL_QUOTE_RESERVES);
        assert_eq!(
            OFF_INITIAL_VIRTUAL_QUOTE_RESERVES + 8,
            OFF_POST_COMPLETE_BASE_OUT
        );
        assert_eq!(OFF_POST_COMPLETE_BASE_OUT + 8, OFF_POST_COMPLETE_QUOTE_IN);
        assert_eq!(OFF_POST_COMPLETE_QUOTE_IN + 8, CURVE_LEN_2026_10_08);
        assert_eq!(CURVE_LEN_2026_10_08, 166);
        assert_eq!(OFF_GLOBAL_MAX_CURVE_DEPTH, GLOBAL_LEN);
        assert_eq!(OFF_GLOBAL_MAX_CURVE_DEPTH + 1, GLOBAL_LEN_2026_10_08);
        // Global: the IDL's field sizes add up to the live allocation.
        assert_eq!(
            OFF_GLOBAL_FEE_RECIPIENTS + 7 * 32,
            OFF_GLOBAL_SET_CREATOR_AUTHORITY
        );
        assert_eq!(
            OFF_GLOBAL_RESERVED_FEE_RECIPIENTS + 7 * 32,
            OFF_GLOBAL_IS_CASHBACK_ENABLED
        );
        assert_eq!(
            OFF_GLOBAL_BUYBACK_FEE_RECIPIENTS + 8 * 32,
            OFF_GLOBAL_BUYBACK_BASIS_POINTS
        );
        assert_eq!(OFF_GLOBAL_IS_HOLDER_REWARD_ENABLED + 1, GLOBAL_LEN);
        assert_eq!(
            OFF_FEE_CONFIG_FLAT_FEES + FEES_LEN,
            OFF_FEE_CONFIG_FEE_TIERS
        );
        assert_eq!(FEE_TIER_LEN, 16 + FEES_LEN);
    }

    fn put(d: &mut [u8], off: usize, bytes: &[u8]) {
        d[off..off + bytes.len()].copy_from_slice(bytes);
    }

    /// A `FeeConfig` with the given tier lists.
    fn fee_config(tiers: &[(u128, [u64; 3])], stable: &[(u128, [u64; 3])]) -> [u8; 4097] {
        let mut d = [0u8; 4097];
        put(&mut d, 0, &FEE_CONFIG_DISCRIMINATOR);
        d[OFF_FEE_CONFIG_BUMP] = 251;
        put(&mut d, OFF_FEE_CONFIG_ADMIN, &[3; 32]);
        for (i, v) in [0u64, 95, 30].iter().enumerate() {
            put(&mut d, OFF_FEE_CONFIG_FLAT_FEES + 8 * i, &v.to_le_bytes());
        }
        let mut off = OFF_FEE_CONFIG_FEE_TIERS;
        for list in [tiers, stable] {
            put(
                &mut d,
                off,
                &u32::try_from(list.len()).unwrap().to_le_bytes(),
            );
            off += 4;
            for (t, f) in list {
                put(&mut d, off, &t.to_le_bytes());
                for (i, v) in f.iter().enumerate() {
                    put(&mut d, off + 16 + 8 * i, &v.to_le_bytes());
                }
                off += FEE_TIER_LEN;
            }
        }
        for (i, v) in [20u64, 5, 5].iter().enumerate() {
            put(&mut d, off + 8 * i, &v.to_le_bytes());
        }
        d
    }

    #[test]
    fn fee_config_tiers_and_selection() {
        let tiers = [
            (420_000_000_000u128, [2, 93, 30]),
            (1_470_000_000_000, [20, 5, 95]),
            (98_240_000_000_000, [20, 5, 5]),
        ];
        let d = fee_config(&tiers, &[(0, [0, 95, 30])]);
        let c = read_fee_config(&FEE_PROGRAM, &d).unwrap();
        assert_eq!((c.bump, c.admin), (251, [3; 32]));
        assert_eq!(
            c.flat_fees,
            Fees {
                lp: 0,
                protocol: 95,
                creator: 30
            }
        );
        assert_eq!(
            c.exotic_flat_fees,
            Fees {
                lp: 20,
                protocol: 5,
                creator: 5
            }
        );
        assert_eq!((c.fee_tiers.len(), c.stable_fee_tiers.len()), (3, 1));
        let got: [FeeTier; 3] = core::array::from_fn(|i| c.fee_tiers.get(i).unwrap());
        for (g, (t, f)) in got.iter().zip(tiers) {
            assert_eq!(
                (g.threshold, [g.fees.lp, g.fees.protocol, g.fees.creator]),
                (t, f)
            );
        }
        assert_eq!(c.fee_tiers.get(3), None);
        assert_eq!(c.fee_tiers.iter().count(), 3);
        let sel = |m: u128| c.fee_tiers.select(m).unwrap();
        // Below tier 0's threshold: tier 0.
        assert_eq!(sel(0).protocol, 93);
        assert_eq!(sel(419_999_999_999).protocol, 93);
        assert_eq!(sel(420_000_000_000).protocol, 93);
        assert_eq!(sel(1_470_000_000_000).creator, 95);
        assert_eq!(sel(98_239_999_999_999).creator, 95);
        assert_eq!(sel(u128::MAX).creator, 5);
        // "The last tier with threshold ≤ mcap" in account order, even if unsorted.
        let d2 = fee_config(&[(10, [0, 1, 1]), (5, [0, 2, 2])], &[]);
        let c2 = read_fee_config(&FEE_PROGRAM, &d2).unwrap();
        assert_eq!(c2.fee_tiers.select(4).unwrap().protocol, 1);
        assert_eq!(c2.fee_tiers.select(7).unwrap().protocol, 2);
        assert_eq!(c2.fee_tiers.select(10).unwrap().protocol, 2);
        assert!(c2.stable_fee_tiers.is_empty());
        assert_eq!(c2.stable_fee_tiers.select(1), None);
    }

    #[test]
    fn fee_config_refusals() {
        let d = fee_config(&[(0, [0, 95, 30])], &[(0, [0, 95, 30])]);
        assert!(read_fee_config(&FEE_PROGRAM, &d).is_ok());
        assert_eq!(read_fee_config(&PROGRAM, &d), Err(ReadError::WrongOwner));
        let mut bad = d;
        bad[1] ^= 1;
        assert_eq!(
            read_fee_config(&FEE_PROGRAM, &bad),
            Err(ReadError::WrongTag)
        );
        // A tier count that runs past the account.
        let mut bad = d;
        put(&mut bad, OFF_FEE_CONFIG_FEE_TIERS, &200u32.to_le_bytes());
        assert_eq!(
            read_fee_config(&FEE_PROGRAM, &bad),
            Err(ReadError::TooShort)
        );
        let mut bad = d;
        put(&mut bad, OFF_FEE_CONFIG_FEE_TIERS, &u32::MAX.to_le_bytes());
        assert!(read_fee_config(&FEE_PROGRAM, &bad).is_err());
        // Truncated inside the exotic fees.
        let end = OFF_FEE_CONFIG_FEE_TIERS + 4 + FEE_TIER_LEN + 4 + FEE_TIER_LEN + FEES_LEN;
        assert!(read_fee_config(&FEE_PROGRAM, &d[..end]).is_ok());
        assert_eq!(
            read_fee_config(&FEE_PROGRAM, &d[..end - 1]),
            Err(ReadError::TooShort)
        );
    }

    #[test]
    fn global_refusals_and_lists() {
        let mut d = [0u8; GLOBAL_LEN];
        put(&mut d, 0, &GLOBAL_DISCRIMINATOR);
        put(&mut d, OFF_GLOBAL_FEE_RECIPIENT, &[1; 32]);
        for i in 0..7 {
            put(
                &mut d,
                OFF_GLOBAL_FEE_RECIPIENTS + 32 * i,
                &[10 + u8::try_from(i).unwrap(); 32],
            );
            put(
                &mut d,
                OFF_GLOBAL_RESERVED_FEE_RECIPIENTS + 32 * i,
                &[30 + u8::try_from(i).unwrap(); 32],
            );
        }
        put(&mut d, OFF_GLOBAL_RESERVED_FEE_RECIPIENT, &[2; 32]);
        for i in 0..8 {
            put(
                &mut d,
                OFF_GLOBAL_BUYBACK_FEE_RECIPIENTS + 32 * i,
                &[50 + u8::try_from(i).unwrap(); 32],
            );
        }
        put(
            &mut d,
            OFF_GLOBAL_POOL_MIGRATION_FEE,
            &15_000_001u64.to_le_bytes(),
        );
        d[OFF_GLOBAL_CREATOR_FEE_CONFIGURABLE] = 1;
        let g = read_global(&PROGRAM, &d).unwrap();
        assert_eq!(g.fee_recipients()[0], [1; 32]);
        assert_eq!(g.fee_recipients()[7], [16; 32]);
        assert_eq!(g.reserved_fee_recipients()[0], [2; 32]);
        assert_eq!(g.reserved_fee_recipients()[1], [30; 32]);
        assert_eq!(g.buyback_fee_recipients()[7], [57; 32]);
        assert!(g.is_fee_recipient(&[13; 32]) && !g.is_fee_recipient(&[2; 32]));
        assert!(g.is_reserved_fee_recipient(&[2; 32]) && !g.is_reserved_fee_recipient(&[1; 32]));
        assert!(g.is_buyback_fee_recipient(&[50; 32]) && !g.is_buyback_fee_recipient(&[1; 32]));
        assert_eq!(g.pool_migration_fee(), 15_000_001);
        assert!(g.creator_fee_configurable());
        assert_eq!(read_global(&[0; 32], &d), Err(ReadError::WrongOwner));
        assert_eq!(
            read_global(&PROGRAM, &d[..GLOBAL_LEN - 1]),
            Err(ReadError::TooShort)
        );
        let mut bad = d;
        bad[2] ^= 1;
        assert_eq!(read_global(&PROGRAM, &bad), Err(ReadError::WrongTag));
        for off in GLOBAL_BOOLS {
            let mut bad = d;
            bad[off] = 2;
            assert_eq!(read_global(&PROGRAM, &bad), Err(ReadError::Malformed));
        }
    }

    /// A base58 key (test helper; no dependency).
    pub fn b58(s: &str) -> Key {
        let mut out = [0u8; 32];
        for c in s.bytes() {
            let mut carry = u32::try_from(
                b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
                    .iter()
                    .position(|&a| a == c)
                    .unwrap(),
            )
            .unwrap();
            for b in out.iter_mut().rev() {
                carry += u32::from(*b) * 58;
                *b = u8::try_from(carry & 0xff).unwrap();
                carry >>= 8;
            }
            assert_eq!(carry, 0, "key overflow");
        }
        out
    }

    /// Mainnet dumps at one slot (451,675,432; provenance: `tests/fixtures/manifest.json`, group
    /// `pump_state`; `scripts/dump-fixtures.py`).
    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../tests/fixtures/accounts/pump/",
                $name,
                ".bin"
            ))
        };
    }

    /// Mainnet dumps of group `pump_state_2026_10_13` (`tests/fixtures/manifest.json`; dumped
    /// after pump's 2026-10-08 redeploy, `scripts/dump-fixtures.py`).
    macro_rules! fixture_2026_10_13 {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../tests/fixtures/accounts/pump_2026_10_13/",
                $name,
                ".bin"
            ))
        };
    }

    /// `(vT, vQ, rT, rQ)`.
    fn reserves(c: &BondingCurve) -> (u64, u64, u64, u64) {
        (
            c.virtual_token_reserves,
            c.virtual_quote_reserves,
            c.real_token_reserves,
            c.real_quote_reserves,
        )
    }

    /// Every curve length variant, decoded as the manifest records it: 125 (`create_v2` today),
    /// 151 (extended), 115 (Token-2022 and legacy SPL-Token), 49 (the 2024 layout), 150 (legacy
    /// extended), the mayhem and USDC curves short.win refuses, and three migrated curves.
    #[test]
    fn reads_every_dumped_curve() {
        let read = |d: &[u8]| read_curve(&PROGRAM, d).unwrap();
        let fresh: &[u8] = fixture!("pump_fresh_curve");
        assert_eq!(fresh.len(), CURVE_LEN_V2);
        let c = read(fresh);
        assert_eq!(
            reserves(&c),
            (
                1_073_000_000_000_000,
                30_000_000_001,
                793_100_000_000_000,
                1
            )
        );
        assert!(c.is_live() && c.is_sol_quote() && !c.is_mayhem_mode && !c.creator_is_default());
        assert_eq!(
            c.creator,
            b58("96mv1v9yhyrZxpt6SRjQ5BUgsfqRtSCTv9QJ3L1ui4qh")
        );
        assert_eq!((c.creator_fee_bps, c.is_holder_reward), (0, false));

        let near: &[u8] = fixture!("pump_near_curve");
        assert_eq!(near.len(), CURVE_LEN_EXTENDED);
        let c = read(near);
        assert_eq!(
            reserves(&c),
            (
                288_812_164_047_503,
                111_456_524_556,
                8_912_164_047_503,
                81_456_524_556
            )
        );
        assert!(c.is_live() && c.real_token_reserves < 100_000_000_000_000);

        let t22: &[u8] = fixture!("pump_t22_115_curve");
        assert_eq!(t22.len(), CURVE_MIN_LEN);
        let c = read(t22);
        assert_eq!(c.real_quote_reserves, 9_793_803_760);
        assert!(c.is_live() && !c.creator_is_default());
        // A 115-byte curve ends at `creator_fee_bps`: the flags after it read as zero.
        assert_eq!(
            (
                c.creator_fee_bps,
                c.can_edit_creator_fee,
                c.is_holder_reward
            ),
            (0, false, false)
        );

        let legacy: &[u8] = fixture!("pump_legacy115_curve");
        assert_eq!(legacy.len(), CURVE_MIN_LEN);
        let c = read(legacy);
        assert!(c.is_live() && c.creator_is_default() && c.is_sol_quote());
        assert_eq!(c.real_quote_reserves, 8_516_987_592);

        let old: &[u8] = fixture!("pump_legacy49_curve");
        assert_eq!(old.len(), CURVE_LEN_ORIGINAL);
        let c = read(old);
        assert_eq!(
            reserves(&c),
            (
                1_070_598_350_775_657,
                30_067_298_364,
                790_698_350_775_657,
                67_298_364
            )
        );
        assert!(c.is_live() && c.creator_is_default() && c.is_sol_quote());
        assert!(!c.is_mayhem_mode && !c.is_cashback_coin && c.creator_fee_bps == 0);

        let mayhem = read(fixture!("pump_mayhem_curve"));
        assert!(mayhem.is_mayhem_mode && mayhem.is_live() && mayhem.is_sol_quote());
        let usdc: &[u8] = fixture!("pump_usdc_curve");
        assert_eq!(usdc.len(), CURVE_LEN_V2);
        let c = read(usdc);
        assert!(c.is_live() && !c.is_sol_quote());
        assert_eq!(
            c.quote_mint,
            b58("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
        );

        for d in [
            &fixture!("pump_graduated_curve")[..],
            &fixture!("pump_legacy150_curve")[..],
            &fixture!("pump_fartcoin_curve")[..],
        ] {
            let c = read(d);
            assert!(c.is_migrated() && !c.is_migrating() && !c.is_live());
            assert_eq!(reserves(&c), (0, 0, 0, 0));
        }
        assert_eq!(
            fixture!("pump_legacy150_curve").len(),
            CURVE_LEN_LEGACY_EXTENDED
        );
        // Fartcoin's curve is byte-identical to the M3 hex (slot 451,197,110).
        assert_eq!(
            &fixture!("pump_fartcoin_curve")[..],
            &hex(FARTCOIN_CURVE_HEX)[..]
        );

        // Group `pump_state_2026_10_13` (after pump's 2026-10-08 redeploy): the 166-byte curves
        // `create_v2` allocates now, the 141 / 151-byte ones v3 grows, and the roles short.win
        // refuses (cashback while live, mayhem, a curve quoted in another pump coin).
        let curves: [(&[u8], usize); 10] = [
            (fixture_2026_10_13!("pump_fresh166_curve"), 166),
            (fixture_2026_10_13!("pump_near166_curve"), 166),
            (fixture_2026_10_13!("pump_buckets166_curve"), 166),
            (fixture_2026_10_13!("pump_grow141_curve"), 141),
            (fixture_2026_10_13!("pump_grow151_curve"), 151),
            (fixture_2026_10_13!("pump_synthetic_stuck_curve"), 166),
            (fixture_2026_10_13!("pump_graduated_synthetic_curve"), 166),
            (fixture_2026_10_13!("pump_cashback166_curve"), 166),
            (fixture_2026_10_13!("pump_mayhem166_curve"), 166),
            (fixture_2026_10_13!("pump_wsol_pool_buckets_curve"), 166),
        ];
        for (d, len) in curves {
            assert_eq!(d.len(), len);
            let c = read(d);
            assert_eq!(c.token_total_supply, 1_000_000_000_000_000);
            assert!(c.is_live() || c.is_migrating() || c.is_migrated());
        }
        let fresh = read(fixture_2026_10_13!("pump_fresh166_curve"));
        assert_eq!(
            reserves(&fresh),
            (
                1_052_896_801_991_732,
                30_572_796_822,
                772_996_801_991_732,
                572_796_822
            )
        );
        assert!(fresh.is_live() && fresh.is_sol_quote() && !fresh.is_mayhem_mode);
        assert_eq!(
            (fresh.depth, fresh.initial_virtual_quote_reserves),
            (0, 30_000_000_000)
        );
        assert!(fresh.quote_offset_consistent());
        let near = read(fixture_2026_10_13!("pump_near166_curve"));
        assert_eq!(
            reserves(&near),
            (
                333_021_296_825_365,
                96_660_487_854,
                53_121_296_825_365,
                66_660_487_854
            )
        );
        assert_eq!(
            (near.creator_fee_bucket, near.protocol_fees_bucket),
            (20_222_168, 32_018_427)
        );
        let buckets = read(fixture_2026_10_13!("pump_buckets166_curve"));
        assert_eq!(
            (buckets.creator_fee_bucket, buckets.protocol_fees_bucket),
            (566_758_769, 897_367_954)
        );
        assert!(buckets.is_live() && buckets.quote_offset_consistent());
        let cashback = read(fixture_2026_10_13!("pump_cashback166_curve"));
        assert!(cashback.is_cashback_coin && cashback.is_live() && cashback.is_sol_quote());
        let mayhem = read(fixture_2026_10_13!("pump_mayhem166_curve"));
        assert!(mayhem.is_mayhem_mode && mayhem.is_live());
    }

    /// T-READER (pump-final §4): the 2026-10-08 fields of the dumped curves.
    #[test]
    fn reads_post_complete_fields() {
        let read = |d: &[u8]| read_curve(&PROGRAM, d).unwrap();
        // `E1bYRCNq…` (role `synthetic_stuck`): completed by a synthetic buy and never migrated;
        // quoted in a depth-0 pump coin (short.win refuses it, `CurveQuoteNotSol`).
        let stuck = read(fixture_2026_10_13!("pump_synthetic_stuck_curve"));
        assert!(stuck.is_migrating() && !stuck.is_sol_quote());
        assert_eq!(
            reserves(&stuck),
            (
                279_900_000_000_000,
                19_072_196_698_539,
                0,
                14_097_072_881_278
            )
        );
        assert_eq!(
            (
                stuck.post_complete_base_out,
                stuck.post_complete_quote_in,
                stuck.depth,
                stuck.initial_virtual_quote_reserves
            ),
            (
                125_468_585_973_711,
                21_720_607_727_767,
                1,
                4_975_123_817_261
            )
        );
        // `rQ` excludes the pool part: `vQ − rQ` is still `ivqr` (pump-final V5).
        assert!(stuck.quote_offset_consistent());
        // 98u8RJ's curve `2ncbKy3K…` (role `graduated_synthetic`): migrated (`vT = 0`) and still
        // carrying the pool part of its completing buy `2vVRr9eg…`.
        let grad = read(fixture_2026_10_13!("pump_graduated_synthetic_curve"));
        assert!(grad.is_migrated());
        assert_eq!(
            (grad.post_complete_base_out, grad.post_complete_quote_in),
            (6_238_850_918_425, 2_642_475_552)
        );
        assert_eq!(
            (grad.depth, grad.initial_virtual_quote_reserves),
            (0, 30_000_000_000)
        );
        assert!(grad.quote_offset_consistent(), "vacuous after migration");
        // Shorter curves read the six fields as zero.
        for d in [
            &fixture_2026_10_13!("pump_grow141_curve")[..],
            &fixture_2026_10_13!("pump_grow151_curve")[..],
            &fixture!("pump_near_curve")[..],
            &fixture!("pump_legacy150_curve")[..],
            &fixture!("pump_fresh_curve")[..],
            &fixture!("pump_legacy115_curve")[..],
        ] {
            let c = read(d);
            assert_eq!(
                (
                    c.creator_fee_bucket,
                    c.protocol_fees_bucket,
                    c.depth,
                    c.initial_virtual_quote_reserves,
                    c.post_complete_base_out,
                    c.post_complete_quote_in
                ),
                (0, 0, 0, 0, 0, 0),
                "{} bytes",
                d.len()
            );
            assert!(c.quote_offset_consistent(), "vacuous without ivqr");
        }
        // A mayhem curve breaks pump's quote-offset invariant (its `vQ` is set by the mayhem
        // program): 457,699,651 − 3 ≠ 30e9.
        let mayhem = read(fixture_2026_10_13!("pump_mayhem166_curve"));
        assert_eq!(mayhem.initial_virtual_quote_reserves, 30_000_000_000);
        assert!(!mayhem.quote_offset_consistent());
        // The helper's arms on a patched live curve: consistent, broken, `vQ < rQ`.
        let mut d = *fixture_2026_10_13!("pump_near166_curve");
        let c = read(&d);
        assert!(c.is_live() && c.quote_offset_consistent());
        d[OFF_INITIAL_VIRTUAL_QUOTE_RESERVES..OFF_INITIAL_VIRTUAL_QUOTE_RESERVES + 8]
            .copy_from_slice(&29_999_999_999u64.to_le_bytes());
        assert!(!read(&d).quote_offset_consistent());
        d[OFF_REAL_QUOTE_RESERVES..OFF_REAL_QUOTE_RESERVES + 8]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(!read(&d).quote_offset_consistent());
        // `depth` is a plain u8: any value reads (no bool check).
        let mut d = *fixture_2026_10_13!("pump_fresh166_curve");
        d[OFF_DEPTH] = 5;
        assert_eq!(read(&d).depth, 5);
    }

    /// pump `Global` at the snapshot (R-PUMP §2.2, Update 2026-09-29 U3: unchanged), and the
    /// 1,088-byte account of group `pump_state_2026_10_13` (role `global_1088`): the same first
    /// 1,087 bytes, then `max_curve_depth` (1 at that dump; mainnet set 2 at slot 456,495,021).
    #[test]
    fn reads_the_dumped_global() {
        let old: &[u8] = fixture!("pump_global");
        let new: &[u8] = fixture_2026_10_13!("pump_global_1088");
        assert_eq!((old.len(), new.len()), (GLOBAL_LEN, GLOBAL_LEN_2026_10_08));
        assert_eq!(&new[..GLOBAL_LEN], old);
        assert_eq!(new[OFF_GLOBAL_MAX_CURVE_DEPTH], 1);
        for d in [old, new] {
            check_dumped_global(&read_global(&PROGRAM, d).unwrap());
        }
    }

    fn check_dumped_global(g: &Global<'_>) {
        assert_eq!(
            g.authority(),
            b58("FFWtrEQ4B4PKQoVuHYzZq8FabGkVatYzDpEVHsK5rrhF")
        );
        assert_eq!(
            g.fee_recipients()[0],
            b58("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV")
        );
        assert_eq!(
            g.fee_recipients()[7],
            b58("G5UZAVbAf46s7cKWoyKu8kYTip9DGTpbLZ2qa9Aq69dP")
        );
        assert!(g.is_fee_recipient(&b58("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM")));
        assert_eq!(
            g.buyback_fee_recipients()[0],
            b58("5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD")
        );
        assert_eq!(
            g.reserved_fee_recipients()[0],
            b58("GesfTA3X2arioaHp8bbKdjG9vJtskViWACZoYvxp4twS")
        );
        assert!(!g.is_fee_recipient(&g.reserved_fee_recipients()[0]));
        assert_eq!(
            g.withdraw_authority(),
            b58("39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg")
        );
        assert_eq!(g.pool_migration_fee(), 15_000_001);
        assert_eq!(
            (g.fee_basis_points(), g.creator_fee_basis_points()),
            (95, 5)
        );
        assert_eq!(g.buyback_basis_points(), 5_000);
        assert!(g.creator_fee_configurable());
        assert_eq!(g.max_configurable_creator_fee_bps(), 300);
        assert_eq!(
            (
                g.initial_virtual_token_reserves(),
                g.initial_virtual_sol_reserves(),
                g.initial_real_token_reserves(),
                g.token_total_supply()
            ),
            (
                1_073_000_000_000_000,
                30_000_000_000,
                793_100_000_000_000,
                1_000_000_000_000_000
            )
        );
        assert_eq!(
            g.whitelisted_quote_mint(),
            b58("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
        );
        assert_eq!(g.initial_virtual_quote_reserves(), 4_292_000_000);
        assert!(g.mayhem_mode_enabled() && g.create_v2_enabled() && g.enable_migrate());
    }

    /// Both pump-fees `FeeConfig`s at the snapshot: pump's single tier {0, 95, 30} and
    /// PumpSwap's 25 market-cap tiers (R-VEN §2.3).
    #[test]
    fn reads_the_dumped_fee_configs() {
        let pump = read_fee_config(&FEE_PROGRAM, fixture!("pump_fee_config")).unwrap();
        let curve = Fees {
            lp: 0,
            protocol: 95,
            creator: 30,
        };
        assert_eq!((pump.flat_fees, pump.exotic_flat_fees), (curve, curve));
        assert_eq!((pump.fee_tiers.len(), pump.stable_fee_tiers.len()), (1, 1));
        assert_eq!(
            pump.fee_tiers.get(0),
            Some(FeeTier {
                threshold: 0,
                fees: curve
            })
        );
        assert_eq!(pump.fee_tiers.select(0), Some(curve));
        assert_eq!(pump.fee_tiers.select(u128::MAX), Some(curve));
        assert_eq!(
            pump.admin,
            b58("FFWtrEQ4B4PKQoVuHYzZq8FabGkVatYzDpEVHsK5rrhF")
        );

        let pamm = read_fee_config(&FEE_PROGRAM, fixture!("pamm_fee_config")).unwrap();
        assert_eq!(
            pamm.flat_fees,
            Fees {
                lp: 25,
                protocol: 5,
                creator: 0
            }
        );
        assert_eq!(
            pamm.exotic_flat_fees,
            Fees {
                lp: 20,
                protocol: 5,
                creator: 5
            }
        );
        assert_eq!(
            (pamm.fee_tiers.len(), pamm.stable_fee_tiers.len()),
            (25, 25)
        );
        let sol = 1_000_000_000u128;
        assert_eq!(
            pamm.fee_tiers.select(0),
            Some(Fees {
                lp: 2,
                protocol: 93,
                creator: 30
            })
        );
        assert_eq!(pamm.fee_tiers.select(420 * sol - 1).unwrap().creator, 30);
        assert_eq!(pamm.fee_tiers.select(420 * sol).unwrap().creator, 95);
        assert_eq!(pamm.fee_tiers.select(73_681 * sol).unwrap().creator, 18);
        assert_eq!(
            pamm.fee_tiers.select(98_240 * sol),
            Some(Fees {
                lp: 20,
                protocol: 5,
                creator: 5
            })
        );
        // Thresholds strictly ascending.
        let mut last = None;
        for t in pamm.fee_tiers.iter() {
            assert!(last.is_none_or(|l| t.threshold > l));
            last = Some(t.threshold);
        }
        assert_eq!(
            pamm.stable_fee_tiers.get(1).unwrap().threshold,
            59_000_000_000
        );
    }

    #[test]
    fn user_volume_accumulator() {
        let mut d = [0u8; USER_VOLUME_ACCUMULATOR_LEN];
        put(&mut d, 0, &USER_VOLUME_ACCUMULATOR_DISCRIMINATOR);
        put(&mut d, OFF_UVA_USER, &[9; 32]);
        put(&mut d, OFF_UVA_CASHBACK_EARNED, &77u64.to_le_bytes());
        let u = read_user_volume_accumulator(&PROGRAM, &d).unwrap();
        assert_eq!(
            (u.user, u.cashback_earned, u.needs_claim),
            ([9; 32], 77, false)
        );
        assert_eq!(
            read_user_volume_accumulator(&FEE_PROGRAM, &d),
            Err(ReadError::WrongOwner)
        );
        let mut bad = d;
        bad[OFF_UVA_NEEDS_CLAIM] = 3;
        assert_eq!(
            read_user_volume_accumulator(&PROGRAM, &bad),
            Err(ReadError::Malformed)
        );
        assert_eq!(
            read_user_volume_accumulator(&PROGRAM, &d[..4]),
            Err(ReadError::WrongTag)
        );
    }
}
