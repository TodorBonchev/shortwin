//! The pool account (`MockPool`): a raw, hand-laid byte layout (INTERFACE.md "Pool layout").
//!
//! The layout is fixed-size ([`POOL_LEN`] bytes) and addressed by **absolute** offsets from
//! the start of the account data (the 8-byte Anchor discriminator included), exactly what a
//! venue-state reader sees. The only thing that moves is the pair of reserve counters:
//! `layout_version == 0` keeps them at [`OFF_PROTOCOL_FEE_BASE_V0`] /
//! [`OFF_PROTOCOL_FEE_QUOTE_V0`]; `layout_version == 1` keeps them at
//! [`OFF_PROTOCOL_FEE_BASE_V1`] / [`OFF_PROTOCOL_FEE_QUOTE_V1`] and zeroes the version-0
//! slots (a simulated venue layout change, ARCH §5.3 "Venue interface breakage", V1-8).
//!
//! Pricing reserves are `vault.amount − counter` per side (as Raydium CPMM's
//! `vault − (protocol + fund + creator)`, ARCH §4.1).

use anchor_lang::prelude::*;

use crate::errors::MockError;

/// The account discriminator, `sha256("account:MockPool")[..8]` (distinct from every
/// mainnet venue's `Pool`; readers without Anchor compare these bytes).
pub const POOL_DISCRIMINATOR: [u8; 8] = [0x3a, 0xc4, 0xd0, 0xf1, 0x1f, 0xd8, 0xf3, 0x30];
/// Account data length (discriminator included).
pub const POOL_LEN: usize = 384;
/// Anchor account discriminator length.
pub const DISC_LEN: usize = 8;
/// Maximum number of required trailing swap accounts.
pub const MAX_EXTRAS: usize = 4;

/// `u8` layout version (0 or 1).
pub const OFF_LAYOUT_VERSION: usize = 8;
/// `u8` bool: swaps fail while set.
pub const OFF_DISABLED: usize = 9;
/// `u32` LE fee on the input, ppm (`< 10⁶`).
pub const OFF_FEE_PPM: usize = 10;
/// `u64` LE extra output units every exact-out swap delivers.
pub const OFF_OVER_DELIVERY: usize = 14;
/// `u64` LE base-side counter (layout 0); zero under layout 1.
pub const OFF_PROTOCOL_FEE_BASE_V0: usize = 22;
/// `u64` LE quote-side counter (layout 0); zero under layout 1.
pub const OFF_PROTOCOL_FEE_QUOTE_V0: usize = 30;
/// `u8` number of required trailing swap accounts (0..=4).
pub const OFF_REQUIRED_EXTRAS_LEN: usize = 38;
/// `[Pubkey; 4]` required trailing swap accounts (unused slots zero).
pub const OFF_REQUIRED_EXTRAS: usize = 39;
/// `Pubkey` base vault.
pub const OFF_BASE_VAULT: usize = 167;
/// `Pubkey` quote vault.
pub const OFF_QUOTE_VAULT: usize = 199;
/// `u8` bump of the pool PDA.
pub const OFF_BUMP: usize = 231;
/// `Pubkey` admin (the pool creator; signs `set_knobs` and `remove_liquidity`).
pub const OFF_ADMIN: usize = 232;
/// `Pubkey` base mint.
pub const OFF_BASE_MINT: usize = 264;
/// `Pubkey` quote mint (always WSOL).
pub const OFF_QUOTE_MINT: usize = 296;
/// `Pubkey` token program of the base mint (SPL Token or Token-2022).
pub const OFF_BASE_TOKEN_PROGRAM: usize = 328;
/// `u8` bump of the global `pool_authority` PDA.
pub const OFF_AUTHORITY_BUMP: usize = 360;
/// `u8` bump of the base vault PDA.
pub const OFF_BASE_VAULT_BUMP: usize = 361;
/// `u8` bump of the quote vault PDA.
pub const OFF_QUOTE_VAULT_BUMP: usize = 362;
/// `u64` LE base-side counter (layout 1); zero under layout 0.
pub const OFF_PROTOCOL_FEE_BASE_V1: usize = 364;
/// `u64` LE quote-side counter (layout 1); zero under layout 0.
pub const OFF_PROTOCOL_FEE_QUOTE_V1: usize = 372;

/// Seed of the pool PDA: `["pool", base_mint, quote_mint]`.
pub const POOL_SEED: &[u8] = b"pool";
/// Seed of the global vault authority PDA: `["authority"]`.
pub const AUTHORITY_SEED: &[u8] = b"authority";
/// Seed of a vault PDA: `["vault", pool, mint]`.
pub const VAULT_SEED: &[u8] = b"vault";

/// The Anchor account type behind the raw layout (owner and discriminator checks; the body
/// is read and written only through [`PoolState`] at the absolute offsets above).
#[account(zero_copy)]
pub struct MockPool {
    /// Bytes `8..POOL_LEN` of the account.
    pub raw: [u8; POOL_LEN - DISC_LEN],
}

/// Index of the authority bump in [`MockPool::raw`].
const AUTHORITY_BUMP_RAW: usize = OFF_AUTHORITY_BUMP - DISC_LEN;

impl MockPool {
    /// The recorded bump of the global `pool_authority` PDA.
    pub fn authority_bump(&self) -> u8 {
        self.raw[AUTHORITY_BUMP_RAW]
    }
}

/// The decoded pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub layout_version: u8,
    pub disabled: bool,
    pub fee_ppm: u32,
    pub over_delivery: u64,
    pub protocol_fee_base: u64,
    pub protocol_fee_quote: u64,
    pub required_extras_len: u8,
    pub required_extras: [Pubkey; MAX_EXTRAS],
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub bump: u8,
    pub admin: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_token_program: Pubkey,
    pub authority_bump: u8,
    pub base_vault_bump: u8,
    pub quote_vault_bump: u8,
}

fn slice<const N: usize>(data: &[u8], off: usize) -> Result<[u8; N]> {
    let end = off.checked_add(N).ok_or(MockError::BadPoolData)?;
    let s = data.get(off..end).ok_or(MockError::BadPoolData)?;
    <[u8; N]>::try_from(s).map_err(|_| error!(MockError::BadPoolData))
}

fn put(data: &mut [u8], off: usize, bytes: &[u8]) -> Result<()> {
    let end = off.checked_add(bytes.len()).ok_or(MockError::BadPoolData)?;
    data.get_mut(off..end)
        .ok_or(MockError::BadPoolData)?
        .copy_from_slice(bytes);
    Ok(())
}

impl PoolState {
    /// Counter offsets `(base, quote)` of a layout version.
    pub fn counter_offsets(layout_version: u8) -> Result<(usize, usize)> {
        match layout_version {
            0 => Ok((OFF_PROTOCOL_FEE_BASE_V0, OFF_PROTOCOL_FEE_QUOTE_V0)),
            1 => Ok((OFF_PROTOCOL_FEE_BASE_V1, OFF_PROTOCOL_FEE_QUOTE_V1)),
            _ => err!(MockError::BadLayoutVersion),
        }
    }

    /// Decodes the full account data (discriminator included; not checked here).
    pub fn decode(data: &[u8]) -> Result<Self> {
        require_gte!(data.len(), POOL_LEN, MockError::BadPoolData);
        let layout_version = slice::<1>(data, OFF_LAYOUT_VERSION)?[0];
        let (ob, oq) = Self::counter_offsets(layout_version)?;
        let key = |off: usize| slice::<32>(data, off).map(Pubkey::new_from_array);
        let required_extras_len = slice::<1>(data, OFF_REQUIRED_EXTRAS_LEN)?[0];
        require_gte!(
            MAX_EXTRAS,
            usize::from(required_extras_len),
            MockError::BadPoolData
        );
        let mut required_extras = [Pubkey::default(); MAX_EXTRAS];
        for (i, slot) in required_extras.iter_mut().enumerate() {
            let off = i
                .checked_mul(32)
                .and_then(|d| d.checked_add(OFF_REQUIRED_EXTRAS))
                .ok_or(MockError::BadPoolData)?;
            *slot = key(off)?;
        }
        Ok(Self {
            layout_version,
            disabled: slice::<1>(data, OFF_DISABLED)?[0] != 0,
            fee_ppm: u32::from_le_bytes(slice(data, OFF_FEE_PPM)?),
            over_delivery: u64::from_le_bytes(slice(data, OFF_OVER_DELIVERY)?),
            protocol_fee_base: u64::from_le_bytes(slice(data, ob)?),
            protocol_fee_quote: u64::from_le_bytes(slice(data, oq)?),
            required_extras_len,
            required_extras,
            base_vault: key(OFF_BASE_VAULT)?,
            quote_vault: key(OFF_QUOTE_VAULT)?,
            bump: slice::<1>(data, OFF_BUMP)?[0],
            admin: key(OFF_ADMIN)?,
            base_mint: key(OFF_BASE_MINT)?,
            quote_mint: key(OFF_QUOTE_MINT)?,
            base_token_program: key(OFF_BASE_TOKEN_PROGRAM)?,
            authority_bump: slice::<1>(data, OFF_AUTHORITY_BUMP)?[0],
            base_vault_bump: slice::<1>(data, OFF_BASE_VAULT_BUMP)?[0],
            quote_vault_bump: slice::<1>(data, OFF_QUOTE_VAULT_BUMP)?[0],
        })
    }

    /// Encodes into the full account data (bytes `8..POOL_LEN`; the discriminator is left
    /// alone). The counters go to the slots of `layout_version`; the other version's slots
    /// and every reserved byte are zeroed.
    pub fn encode(&self, data: &mut [u8]) -> Result<()> {
        require_gte!(data.len(), POOL_LEN, MockError::BadPoolData);
        require_gte!(
            MAX_EXTRAS,
            usize::from(self.required_extras_len),
            MockError::TooManyExtras
        );
        let (ob, oq) = Self::counter_offsets(self.layout_version)?;
        data.get_mut(DISC_LEN..POOL_LEN)
            .ok_or(MockError::BadPoolData)?
            .fill(0);
        put(data, OFF_LAYOUT_VERSION, &[self.layout_version])?;
        put(data, OFF_DISABLED, &[u8::from(self.disabled)])?;
        put(data, OFF_FEE_PPM, &self.fee_ppm.to_le_bytes())?;
        put(data, OFF_OVER_DELIVERY, &self.over_delivery.to_le_bytes())?;
        put(data, ob, &self.protocol_fee_base.to_le_bytes())?;
        put(data, oq, &self.protocol_fee_quote.to_le_bytes())?;
        put(data, OFF_REQUIRED_EXTRAS_LEN, &[self.required_extras_len])?;
        for (i, k) in self
            .required_extras
            .iter()
            .take(usize::from(self.required_extras_len))
            .enumerate()
        {
            let off = i
                .checked_mul(32)
                .and_then(|d| d.checked_add(OFF_REQUIRED_EXTRAS))
                .ok_or(MockError::BadPoolData)?;
            put(data, off, k.as_ref())?;
        }
        put(data, OFF_BASE_VAULT, self.base_vault.as_ref())?;
        put(data, OFF_QUOTE_VAULT, self.quote_vault.as_ref())?;
        put(data, OFF_BUMP, &[self.bump])?;
        put(data, OFF_ADMIN, self.admin.as_ref())?;
        put(data, OFF_BASE_MINT, self.base_mint.as_ref())?;
        put(data, OFF_QUOTE_MINT, self.quote_mint.as_ref())?;
        put(
            data,
            OFF_BASE_TOKEN_PROGRAM,
            self.base_token_program.as_ref(),
        )?;
        put(data, OFF_AUTHORITY_BUMP, &[self.authority_bump])?;
        put(data, OFF_BASE_VAULT_BUMP, &[self.base_vault_bump])?;
        put(data, OFF_QUOTE_VAULT_BUMP, &[self.quote_vault_bump])?;
        Ok(())
    }

    /// The required trailing swap accounts, in order.
    pub fn extras(&self) -> &[Pubkey] {
        self.required_extras
            .get(..usize::from(self.required_extras_len))
            .unwrap_or(&[])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample() -> PoolState {
        PoolState {
            layout_version: 0,
            disabled: true,
            fee_ppm: 2_500,
            over_delivery: 7,
            protocol_fee_base: 0x0102_0304_0506_0708,
            protocol_fee_quote: 0x1112_1314_1516_1718,
            required_extras_len: 2,
            required_extras: [
                Pubkey::new_from_array([1; 32]),
                Pubkey::new_from_array([2; 32]),
                Pubkey::default(),
                Pubkey::default(),
            ],
            base_vault: Pubkey::new_from_array([3; 32]),
            quote_vault: Pubkey::new_from_array([4; 32]),
            bump: 250,
            admin: Pubkey::new_from_array([5; 32]),
            base_mint: Pubkey::new_from_array([6; 32]),
            quote_mint: Pubkey::new_from_array([7; 32]),
            base_token_program: Pubkey::new_from_array([8; 32]),
            authority_bump: 251,
            base_vault_bump: 252,
            quote_vault_bump: 253,
        }
    }

    /// Fields are contiguous, do not overlap and end inside `POOL_LEN` (the table of
    /// INTERFACE.md).
    #[test]
    fn offsets_are_contiguous() {
        let fields: [(usize, usize); 21] = [
            (OFF_LAYOUT_VERSION, 1),
            (OFF_DISABLED, 1),
            (OFF_FEE_PPM, 4),
            (OFF_OVER_DELIVERY, 8),
            (OFF_PROTOCOL_FEE_BASE_V0, 8),
            (OFF_PROTOCOL_FEE_QUOTE_V0, 8),
            (OFF_REQUIRED_EXTRAS_LEN, 1),
            (OFF_REQUIRED_EXTRAS, 32 * MAX_EXTRAS),
            (OFF_BASE_VAULT, 32),
            (OFF_QUOTE_VAULT, 32),
            (OFF_BUMP, 1),
            (OFF_ADMIN, 32),
            (OFF_BASE_MINT, 32),
            (OFF_QUOTE_MINT, 32),
            (OFF_BASE_TOKEN_PROGRAM, 32),
            (OFF_AUTHORITY_BUMP, 1),
            (OFF_BASE_VAULT_BUMP, 1),
            (OFF_QUOTE_VAULT_BUMP, 1),
            // one reserved byte at 363
            (OFF_PROTOCOL_FEE_BASE_V1 - 1, 1),
            (OFF_PROTOCOL_FEE_BASE_V1, 8),
            (OFF_PROTOCOL_FEE_QUOTE_V1, 8),
        ];
        let mut at = DISC_LEN;
        for (off, len) in fields {
            assert_eq!(off, at);
            at = off + len;
        }
        // four reserved bytes at 380..384
        assert_eq!(at + 4, POOL_LEN);
        assert_eq!(DISC_LEN + core::mem::size_of::<MockPool>(), POOL_LEN);
        assert_eq!(
            <MockPool as anchor_lang::Discriminator>::DISCRIMINATOR,
            &POOL_DISCRIMINATOR
        );
    }

    #[test]
    fn roundtrip_and_layout_move() {
        let mut data = vec![0xAAu8; POOL_LEN];
        let s = sample();
        s.encode(&mut data).unwrap();
        // discriminator untouched
        assert_eq!(&data[..8], &[0xAA; 8]);
        assert_eq!(PoolState::decode(&data).unwrap(), s);
        assert_eq!(
            data[OFF_PROTOCOL_FEE_BASE_V0..][..8],
            s.protocol_fee_base.to_le_bytes()
        );
        assert_eq!(data[OFF_PROTOCOL_FEE_BASE_V1..][..16], [0; 16]);
        assert_eq!(data[380..384], [0; 4]);

        let v1 = PoolState {
            layout_version: 1,
            ..s.clone()
        };
        v1.encode(&mut data).unwrap();
        assert_eq!(PoolState::decode(&data).unwrap(), v1);
        assert_eq!(data[OFF_PROTOCOL_FEE_BASE_V0..][..16], [0; 16]);
        assert_eq!(
            data[OFF_PROTOCOL_FEE_QUOTE_V1..][..8],
            s.protocol_fee_quote.to_le_bytes()
        );
        assert_eq!(v1.extras(), &s.required_extras[..2]);

        data[OFF_LAYOUT_VERSION] = 2;
        assert!(PoolState::decode(&data).is_err());
        assert!(PoolState::decode(&data[..POOL_LEN - 1]).is_err());
    }
}
