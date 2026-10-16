//! Pyth `PriceUpdateV2` reader and the `crank_sol_usd` acceptance rules (ARCH §3.9, §4.1,
//! §4.7, A10, A29; R-VEN §5; C1-12).
//!
//! Hand-parsed, no Pyth SDK (A29). The account is the Anchor account `PriceUpdateV2` of the
//! Pyth Solana receiver `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`
//! (`pyth-network/pyth-crosschain` @ `bd2a4b6f43`,
//! `target_chains/solana/pyth_solana_receiver_sdk/src/price_update.rs`):
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 8 | discriminator `sha256("account:PriceUpdateV2")[..8]` = `22f123639d7ef4cd` |
//! | 8 | 32 | `write_authority` |
//! | 40 | 1 or 2 | `verification_level` (Borsh enum: `Partial { num_signatures: u8 }` = `00 nn`, `Full` = `01`) |
//! | 41 / 42 | 84 | `price_message { feed_id [u8;32], price i64, conf u64, exponent i32, publish_time i64, prev_publish_time i64, ema_price i64, ema_conf u64 }` |
//! | 125 / 126 | 8 | `posted_slot` u64 |
//!
//! The receiver allocates every account at the `Partial` size (134 B), so a `Full` update
//! leaves one trailing zero byte. The sponsored shard-0 SOL/USD account
//! `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` is 134 B, `Full`, feed `ef0d8b6f…b56d`
//! (mainnet dump with provenance: `tests/fixtures/accounts/pyth_sol_usd_shard0.*`,
//! `tests/fixtures/manifest.json`).
//!
//! [`read`] decodes the account: owner, discriminator, verification tag, and the whole layout
//! of that variant (a price field is never zero-padded). [`check`] applies the rules of ARCH
//! §3.9 in ARCH's order, with exact integer comparisons (no division, so no rounding):
//!
//! 1. `verification_level == Full` ([`Refusal::NotFull`]);
//! 2. feed id == the configured one ([`Refusal::WrongFeed`]), and the exponent in
//!    [[`EXPO_MIN`], [`EXPO_MAX`]] ([`Refusal::Exponent`]; M1-SEC-8: the tier arithmetic of
//!    ARCH §4.7 scales by `10^(expo + 6)`, and no USD feed publishes outside this range);
//! 3. `now − oracle_max_age_s ≤ publish_time ≤ now + oracle_max_age_s` ([`Refusal::Stale`],
//!    [`Refusal::FutureDated`]). The upper bound (M1-SEC-8) keeps a verified update dated in
//!    the future from being cached as "fresh" beyond its real age and from refusing every
//!    honest update as not newer until the clock reaches it; it reuses `oracle_max_age_s` as
//!    the tolerated skew, so a cluster clock that lags the publishers by up to the maximum age
//!    still accepts honest updates, and no new parameter is needed;
//! 4. `price > 0` ([`Refusal::NonPositivePrice`]) and `conf·10⁴ ≤ oracle_max_conf_bps·price`
//!    ([`Refusal::Confidence`]);
//! 5. `|price − cached|·10⁴ ≤ oracle_max_step_bps·cached`, both mantissas first taken to the
//!    smaller of the two exponents (exact: one multiplication by `10^|Δexpo|`), skipped while
//!    the cache is older than `oracle_stale_s` or was never written ([`Refusal::Step`]; a
//!    rescale that leaves u128 refuses, fail closed);
//! 6. `publish_time > cached.publish_time` ([`Refusal::NotNewer`]).
//!
//! The program maps a [`ReadError`] or a [`Refusal`] to its `Oracle*` error; the keeper and the
//! frontend call the same functions to predict whether a crank lands (keepers retry: the
//! sponsored feed updates on a 55 s heartbeat or a 0.5 % move, C1-12).

use crate::bytes::{array_at, has_discriminator, i32_at, i64_at, key_at, u8_at, u64_at};
use crate::{Key, ReadError};

/// `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`, the Pyth Solana receiver (owner of every
/// `PriceUpdateV2`).
pub const RECEIVER: Key = [
    12, 183, 250, 187, 82, 247, 166, 72, 187, 91, 49, 125, 154, 1, 139, 144, 87, 203, 2, 71, 116,
    250, 254, 1, 230, 196, 223, 152, 204, 56, 88, 129,
];

/// `sha256("account:PriceUpdateV2")[..8]`.
pub const DISCRIMINATOR: [u8; 8] = [0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd];

/// Offset of `write_authority`.
pub const WRITE_AUTHORITY_OFFSET: usize = 8;

/// Offset of the `verification_level` enum tag.
pub const VERIFICATION_OFFSET: usize = 40;

/// Borsh tag of `VerificationLevel::Partial { num_signatures: u8 }`.
pub const TAG_PARTIAL: u8 = 0;

/// Borsh tag of `VerificationLevel::Full`.
pub const TAG_FULL: u8 = 1;

/// Offset of `price_message` in a `Full` update.
pub const MESSAGE_OFFSET_FULL: usize = 41;

/// Offset of `price_message` in a `Partial` update.
pub const MESSAGE_OFFSET_PARTIAL: usize = 42;

/// Length of `price_message` (84 B) plus `posted_slot` (8 B).
pub const MESSAGE_AND_SLOT_LEN: usize = 92;

/// Smallest `Full` account (133 B).
pub const MIN_LEN_FULL: usize = MESSAGE_OFFSET_FULL + MESSAGE_AND_SLOT_LEN;

/// `PriceUpdateV2::LEN`, the size the receiver allocates (the `Partial` layout, 134 B).
pub const LEN: usize = MESSAGE_OFFSET_PARTIAL + MESSAGE_AND_SLOT_LEN;

/// Offsets inside `price_message` (and of `posted_slot` after it).
pub mod field {
    /// `feed_id` [u8; 32].
    pub const FEED_ID: usize = 0;
    /// `price` i64.
    pub const PRICE: usize = 32;
    /// `conf` u64.
    pub const CONF: usize = 40;
    /// `exponent` i32.
    pub const EXPONENT: usize = 48;
    /// `publish_time` i64.
    pub const PUBLISH_TIME: usize = 52;
    /// `prev_publish_time` i64.
    pub const PREV_PUBLISH_TIME: usize = 60;
    /// `ema_price` i64.
    pub const EMA_PRICE: usize = 68;
    /// `ema_conf` u64.
    pub const EMA_CONF: usize = 76;
    /// `posted_slot` u64 (right after the 84-byte message).
    pub const POSTED_SLOT: usize = 84;
}

/// Basis points per unit.
pub const BPS: u128 = 10_000;

/// Smallest exponent [`check`] accepts (rule 2; M1-SEC-8).
pub const EXPO_MIN: i32 = -18;

/// Largest exponent [`check`] accepts (rule 2; M1-SEC-8).
pub const EXPO_MAX: i32 = 0;

/// `PriceUpdateV2.verification_level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VerificationLevel {
    /// Checked against fewer guardian signatures than a quorum.
    Partial {
        /// Guardian signatures checked.
        num_signatures: u8,
    },
    /// Checked against a Wormhole guardian quorum.
    Full,
}

/// A decoded `PriceUpdateV2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PriceUpdate {
    /// `write_authority`.
    pub write_authority: Key,
    /// `verification_level`.
    pub verification: VerificationLevel,
    /// The feed id.
    pub feed_id: [u8; 32],
    /// Price, in units of `10^exponent`.
    pub price: i64,
    /// Confidence interval, same units.
    pub conf: u64,
    /// Decimal exponent.
    pub exponent: i32,
    /// Publish time (unix seconds).
    pub publish_time: i64,
    /// Publish time of the feed's previous update.
    pub prev_publish_time: i64,
    /// Pyth's EMA price (not used by short.win).
    pub ema_price: i64,
    /// Pyth's EMA confidence (not used by short.win).
    pub ema_conf: u64,
    /// Solana slot the update was posted in.
    pub posted_slot: u64,
}

impl PriceUpdate {
    /// `true` for `VerificationLevel::Full`.
    pub fn verified_full(&self) -> bool {
        self.verification == VerificationLevel::Full
    }
}

fn at(base: usize, rel: usize) -> usize {
    base.saturating_add(rel)
}

/// Decodes a `PriceUpdateV2`: owner == [`RECEIVER`] ([`ReadError::WrongOwner`]), the
/// discriminator ([`ReadError::TooShort`] below 8 bytes, else [`ReadError::WrongTag`]), a
/// `verification_level` tag of 0 or 1 ([`ReadError::Malformed`]), and the whole layout of that
/// variant present ([`ReadError::TooShort`]).
pub fn read(owner: &Key, data: &[u8]) -> Result<PriceUpdate, ReadError> {
    if *owner != RECEIVER {
        return Err(ReadError::WrongOwner);
    }
    if data.len() < DISCRIMINATOR.len() {
        return Err(ReadError::TooShort);
    }
    if !has_discriminator(data, &DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    if data.len() <= VERIFICATION_OFFSET {
        return Err(ReadError::TooShort);
    }
    let (verification, msg) = match u8_at(data, VERIFICATION_OFFSET) {
        TAG_PARTIAL => (
            VerificationLevel::Partial {
                num_signatures: u8_at(data, at(VERIFICATION_OFFSET, 1)),
            },
            MESSAGE_OFFSET_PARTIAL,
        ),
        TAG_FULL => (VerificationLevel::Full, MESSAGE_OFFSET_FULL),
        _ => return Err(ReadError::Malformed),
    };
    if data.len() < at(msg, MESSAGE_AND_SLOT_LEN) {
        return Err(ReadError::TooShort);
    }
    Ok(PriceUpdate {
        write_authority: key_at(data, WRITE_AUTHORITY_OFFSET),
        verification,
        feed_id: array_at(data, at(msg, field::FEED_ID)),
        price: i64_at(data, at(msg, field::PRICE)),
        conf: u64_at(data, at(msg, field::CONF)),
        exponent: i32_at(data, at(msg, field::EXPONENT)),
        publish_time: i64_at(data, at(msg, field::PUBLISH_TIME)),
        prev_publish_time: i64_at(data, at(msg, field::PREV_PUBLISH_TIME)),
        ema_price: i64_at(data, at(msg, field::EMA_PRICE)),
        ema_conf: u64_at(data, at(msg, field::EMA_CONF)),
        posted_slot: u64_at(data, at(msg, field::POSTED_SLOT)),
    })
}

/// The oracle limits of `GlobalConfig.params` (ARCH §2.3.3; ECON §15).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    /// `oracle_max_age_s`: oldest `publish_time` accepted, seconds before `now`.
    pub max_age_s: u32,
    /// `oracle_stale_s`: a cache older than this does not bound the step.
    pub stale_s: u32,
    /// `oracle_max_conf_bps`: widest `conf/price` accepted.
    pub max_conf_bps: u32,
    /// `oracle_max_step_bps`: largest `|price/cached − 1|` accepted against a fresh cache.
    pub max_step_bps: u32,
}

/// The cached price the step and ordering rules compare with (`OracleCache`; all zero before
/// the first crank).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Cached {
    /// Cached price mantissa (≤ 0 only before the first crank).
    pub price: i64,
    /// Cached exponent.
    pub expo: i32,
    /// Cached publish time (0 before the first crank).
    pub publish_time: i64,
}

/// Why [`check`] refused an update (the program maps each to an `Oracle*` error).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// Not `VerificationLevel::Full` (`OracleVerification`).
    NotFull,
    /// The feed id differs from the configured one (`OracleFeed`).
    WrongFeed,
    /// The exponent is outside [`EXPO_MIN`, `EXPO_MAX`] (`OracleFeed`).
    Exponent,
    /// `publish_time < now − oracle_max_age_s` (`OracleStale`).
    Stale,
    /// `publish_time > now + oracle_max_age_s` (`OracleFutureDated`).
    FutureDated,
    /// `price ≤ 0`: no confidence ratio exists (`OracleConfidence`).
    NonPositivePrice,
    /// `conf/price > oracle_max_conf_bps` (`OracleConfidence`).
    Confidence,
    /// `|price/cached − 1| > oracle_max_step_bps` against a fresh cache, or not comparable
    /// within u128 after the exponent rescale (`OracleStep`).
    Step,
    /// `publish_time ≤ cached.publish_time` (`OracleStale`).
    NotNewer,
}

/// Rule 3: `publish_time ≥ now − max_age_s`, written as the Pyth SDK's
/// `get_price_no_older_than` does (`publish_time + max_age ≥ now`, saturating).
pub fn is_fresh(publish_time: i64, now: i64, max_age_s: u32) -> bool {
    publish_time.saturating_add(i64::from(max_age_s)) >= now
}

/// Rule 3, upper bound: `publish_time ≤ now + max_age_s` (saturating): an update is not
/// accepted from further in the future than it may be old.
pub fn not_future(publish_time: i64, now: i64, max_age_s: u32) -> bool {
    publish_time <= now.saturating_add(i64::from(max_age_s))
}

/// Rule 2, exponent: `EXPO_MIN ≤ exponent ≤ EXPO_MAX`.
pub fn exponent_ok(exponent: i32) -> bool {
    (EXPO_MIN..=EXPO_MAX).contains(&exponent)
}

/// Rule 4: `price > 0` and `conf·10⁴ ≤ max_conf_bps·price`.
pub fn confidence_ok(price: i64, conf: u64, max_conf_bps: u32) -> Result<(), Refusal> {
    let price = match u64::try_from(price) {
        Ok(p) if p > 0 => u128::from(p),
        _ => return Err(Refusal::NonPositivePrice),
    };
    let lhs = u128::from(conf).checked_mul(BPS);
    let rhs = u128::from(max_conf_bps).checked_mul(price);
    match (lhs, rhs) {
        (Some(l), Some(r)) if l <= r => Ok(()),
        _ => Err(Refusal::Confidence),
    }
}

/// `true` if the step rule is skipped: the cache was never written (`price ≤ 0`) or is older
/// than `stale_s` (`now − cached.publish_time > stale_s`).
pub fn cache_is_stale(cached: &Cached, now: i64, stale_s: u32) -> bool {
    cached.price <= 0 || now.saturating_sub(cached.publish_time) > i64::from(stale_s)
}

/// Two positive mantissas at the smaller of their exponents: the one with the larger exponent
/// is multiplied by `10^|Δexpo|` (exact). `None` if a mantissa is not positive or the rescale
/// leaves u128.
pub fn common_scale(a: i64, a_expo: i32, b: i64, b_expo: i32) -> Option<(u128, u128)> {
    let a = u128::from(u64::try_from(a).ok().filter(|v| *v > 0)?);
    let b = u128::from(u64::try_from(b).ok().filter(|v| *v > 0)?);
    let shift = i64::from(a_expo).checked_sub(i64::from(b_expo))?;
    let pow = 10u128.checked_pow(u32::try_from(shift.unsigned_abs()).ok()?)?;
    if shift >= 0 {
        Some((a.checked_mul(pow)?, b))
    } else {
        Some((a, b.checked_mul(pow)?))
    }
}

/// Rule 5 against a fresh cache: `|price − cached|·10⁴ ≤ max_step_bps·cached` at a common
/// exponent ([`common_scale`]). Anything not comparable refuses.
pub fn step_ok(price: i64, expo: i32, cached: &Cached, max_step_bps: u32) -> Result<(), Refusal> {
    let (p, c) = common_scale(price, expo, cached.price, cached.expo).ok_or(Refusal::Step)?;
    let lhs = p.abs_diff(c).checked_mul(BPS);
    let rhs = c.checked_mul(u128::from(max_step_bps));
    match (lhs, rhs) {
        (Some(l), Some(r)) if l <= r => Ok(()),
        _ => Err(Refusal::Step),
    }
}

/// Every `crank_sol_usd` rule of ARCH §3.9 after the owner and address checks, in ARCH's
/// order (module docs). `Ok` means the update may replace `cached`.
pub fn check(
    update: &PriceUpdate,
    feed_id: &[u8; 32],
    cached: &Cached,
    now: i64,
    limits: &Limits,
) -> Result<(), Refusal> {
    if !update.verified_full() {
        return Err(Refusal::NotFull);
    }
    if update.feed_id != *feed_id {
        return Err(Refusal::WrongFeed);
    }
    if !exponent_ok(update.exponent) {
        return Err(Refusal::Exponent);
    }
    if !is_fresh(update.publish_time, now, limits.max_age_s) {
        return Err(Refusal::Stale);
    }
    if !not_future(update.publish_time, now, limits.max_age_s) {
        return Err(Refusal::FutureDated);
    }
    confidence_ok(update.price, update.conf, limits.max_conf_bps)?;
    if !cache_is_stale(cached, now, limits.stale_s) {
        step_ok(update.price, update.exponent, cached, limits.max_step_bps)?;
    }
    if update.publish_time <= cached.publish_time {
        return Err(Refusal::NotNewer);
    }
    Ok(())
}

/// Encodes `u` in the receiver's layout ([`LEN`] bytes, `Partial` or `Full`): the inverse of
/// [`read`], for fixtures and tests.
pub fn encode(u: &PriceUpdate) -> [u8; LEN] {
    let mut out = [0u8; LEN];
    let mut put = |offset: usize, bytes: &[u8]| {
        if let Some(dst) = out.get_mut(offset..at(offset, bytes.len())) {
            dst.copy_from_slice(bytes);
        }
    };
    put(0, &DISCRIMINATOR);
    put(WRITE_AUTHORITY_OFFSET, &u.write_authority);
    let msg = match u.verification {
        VerificationLevel::Full => {
            put(VERIFICATION_OFFSET, &[TAG_FULL]);
            MESSAGE_OFFSET_FULL
        }
        VerificationLevel::Partial { num_signatures } => {
            put(VERIFICATION_OFFSET, &[TAG_PARTIAL, num_signatures]);
            MESSAGE_OFFSET_PARTIAL
        }
    };
    put(at(msg, field::FEED_ID), &u.feed_id);
    put(at(msg, field::PRICE), &u.price.to_le_bytes());
    put(at(msg, field::CONF), &u.conf.to_le_bytes());
    put(at(msg, field::EXPONENT), &u.exponent.to_le_bytes());
    put(at(msg, field::PUBLISH_TIME), &u.publish_time.to_le_bytes());
    put(
        at(msg, field::PREV_PUBLISH_TIME),
        &u.prev_publish_time.to_le_bytes(),
    );
    put(at(msg, field::EMA_PRICE), &u.ema_price.to_le_bytes());
    put(at(msg, field::EMA_CONF), &u.ema_conf.to_le_bytes());
    put(at(msg, field::POSTED_SLOT), &u.posted_slot.to_le_bytes());
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    /// The sponsored shard-0 SOL/USD account dumped from mainnet (provenance:
    /// `tests/fixtures/manifest.json`).
    const MAINNET: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/pyth_sol_usd_shard0.bin");

    const SOL_USD: [u8; 32] = [
        0xef, 0x0d, 0x8b, 0x6f, 0xda, 0x2c, 0xeb, 0xa4, 0x1d, 0xa1, 0x5d, 0x40, 0x95, 0xd1, 0xda,
        0x39, 0x2a, 0x0d, 0x2f, 0x8e, 0xd0, 0xc6, 0xc7, 0xbc, 0x0f, 0x4c, 0xfa, 0xc8, 0xc2, 0x80,
        0xb5, 0x6d,
    ];

    /// `7UVimffx…` itself: the sponsored feed account is its own write authority.
    const SHARD0: Key = [
        0x60, 0x31, 0x47, 0x04, 0x34, 0x0d, 0xed, 0xdf, 0x37, 0x1f, 0xd4, 0x24, 0x72, 0x14, 0x8f,
        0x24, 0x8e, 0x9d, 0x1a, 0x6d, 0x1a, 0x5e, 0xb2, 0xac, 0x3a, 0xcd, 0x8b, 0x7f, 0xd5, 0xd6,
        0xb2, 0x43,
    ];

    const LIMITS: Limits = Limits {
        max_age_s: 120,
        stale_s: 3_600,
        max_conf_bps: 200,
        max_step_bps: 2_000,
    };

    fn mainnet() -> PriceUpdate {
        read(&RECEIVER, MAINNET).unwrap()
    }

    #[test]
    fn layout_constants() {
        assert_eq!(MIN_LEN_FULL, 133);
        assert_eq!(LEN, 134);
        assert_eq!(MESSAGE_AND_SLOT_LEN, 32 + 8 + 8 + 4 + 8 + 8 + 8 + 8 + 8);
    }

    /// The mainnet dump decodes to the values recorded in `tests/fixtures/manifest.json`, and
    /// [`encode`] reproduces it byte for byte.
    #[test]
    fn reads_the_mainnet_account() {
        assert_eq!(MAINNET.len(), LEN);
        let u = mainnet();
        assert_eq!(
            u,
            PriceUpdate {
                write_authority: SHARD0,
                verification: VerificationLevel::Full,
                feed_id: SOL_USD,
                price: 12_179_606_511,
                conf: 1_399_451,
                exponent: -8,
                publish_time: 1_790_523_057,
                prev_publish_time: 1_790_523_056,
                ema_price: 12_221_353_350,
                ema_conf: 1_476_255,
                posted_slot: 451_037_781,
            }
        );
        assert!(u.verified_full());
        assert_eq!(MAINNET[LEN - 1], 0, "Full leaves the last byte unused");
        assert_eq!(encode(&u).as_slice(), MAINNET);
    }

    #[test]
    fn reads_a_partial_update() {
        let mut u = mainnet();
        u.verification = VerificationLevel::Partial { num_signatures: 5 };
        let bytes = encode(&u);
        assert_eq!(bytes[40..42], [TAG_PARTIAL, 5]);
        assert_eq!(read(&RECEIVER, &bytes), Ok(u));
        assert!(!read(&RECEIVER, &bytes).unwrap().verified_full());
        assert_eq!(read(&RECEIVER, &bytes[..133]), Err(ReadError::TooShort));
    }

    #[test]
    fn read_refusals() {
        assert_eq!(read(&[0u8; 32], MAINNET), Err(ReadError::WrongOwner));
        assert_eq!(read(&RECEIVER, &MAINNET[..7]), Err(ReadError::TooShort));
        assert_eq!(read(&RECEIVER, &MAINNET[..40]), Err(ReadError::TooShort));
        assert_eq!(read(&RECEIVER, &MAINNET[..132]), Err(ReadError::TooShort));
        assert_eq!(read(&RECEIVER, &MAINNET[..133]), Ok(mainnet()));
        let mut bad = [0u8; LEN];
        bad.copy_from_slice(MAINNET);
        bad[0] ^= 1;
        assert_eq!(read(&RECEIVER, &bad), Err(ReadError::WrongTag));
        let mut bad = [0u8; LEN];
        bad.copy_from_slice(MAINNET);
        bad[VERIFICATION_OFFSET] = 2;
        assert_eq!(read(&RECEIVER, &bad), Err(ReadError::Malformed));
    }

    #[test]
    fn check_accepts_the_mainnet_update() {
        let u = mainnet();
        let now = u.publish_time + 7; // the dump's age against wall clock
        assert_eq!(
            check(&u, &SOL_USD, &Cached::default(), now, &LIMITS),
            Ok(())
        );
    }

    #[test]
    fn check_order_and_rules() {
        let u = mainnet();
        let now = u.publish_time;
        let fresh = Cached {
            price: u.price,
            expo: u.exponent,
            publish_time: u.publish_time - 55,
        };
        assert_eq!(check(&u, &SOL_USD, &fresh, now, &LIMITS), Ok(()));

        let mut p = u;
        p.verification = VerificationLevel::Partial { num_signatures: 13 };
        p.feed_id[0] ^= 1; // verification is checked first
        assert_eq!(
            check(&p, &SOL_USD, &fresh, now, &LIMITS),
            Err(Refusal::NotFull)
        );
        let mut f = u;
        f.feed_id[31] ^= 1;
        f.publish_time = 0; // the feed is checked before the age
        assert_eq!(
            check(&f, &SOL_USD, &fresh, now, &LIMITS),
            Err(Refusal::WrongFeed)
        );

        // Age: accepted at exactly max_age, refused one second later.
        assert_eq!(check(&u, &SOL_USD, &fresh, now + 120, &LIMITS), Ok(()));
        assert_eq!(
            check(&u, &SOL_USD, &fresh, now + 121, &LIMITS),
            Err(Refusal::Stale)
        );

        // Confidence before step.
        let mut c = u;
        c.conf = u64::try_from(c.price).unwrap(); // 100 %
        c.price *= 2;
        assert_eq!(
            check(&c, &SOL_USD, &fresh, now, &LIMITS),
            Err(Refusal::Confidence)
        );

        // Not newer than the cache.
        let same = Cached {
            publish_time: u.publish_time,
            ..fresh
        };
        assert_eq!(
            check(&u, &SOL_USD, &same, now, &LIMITS),
            Err(Refusal::NotNewer)
        );
        let later = Cached {
            publish_time: u.publish_time + 1,
            ..fresh
        };
        assert_eq!(
            check(&u, &SOL_USD, &later, now, &LIMITS),
            Err(Refusal::NotNewer)
        );
    }

    #[test]
    fn confidence_boundary() {
        // 200 bps of 100.00000000 is exactly 2.00000000.
        assert_eq!(confidence_ok(10_000_000_000, 200_000_000, 200), Ok(()));
        assert_eq!(
            confidence_ok(10_000_000_000, 200_000_001, 200),
            Err(Refusal::Confidence)
        );
        assert_eq!(confidence_ok(1, 0, 10), Ok(()));
        assert_eq!(confidence_ok(0, 0, 200), Err(Refusal::NonPositivePrice));
        assert_eq!(confidence_ok(-5, 0, 200), Err(Refusal::NonPositivePrice));
        assert_eq!(
            confidence_ok(i64::MAX, u64::MAX, 1_000),
            Err(Refusal::Confidence)
        );
        let max = u64::try_from(i64::MAX).unwrap();
        assert_eq!(confidence_ok(i64::MAX, max / 10_000, 1), Ok(()));
        assert_eq!(
            confidence_ok(i64::MAX, max / 10_000 + 1, 1),
            Err(Refusal::Confidence)
        );
    }

    #[test]
    fn step_boundary() {
        let c = Cached {
            price: 10_000_000_000,
            expo: -8,
            publish_time: 1,
        };
        // +20 % and −20 % are accepted, one unit further is not.
        assert_eq!(step_ok(12_000_000_000, -8, &c, 2_000), Ok(()));
        assert_eq!(step_ok(8_000_000_000, -8, &c, 2_000), Ok(()));
        assert_eq!(step_ok(12_000_000_001, -8, &c, 2_000), Err(Refusal::Step));
        assert_eq!(step_ok(7_999_999_999, -8, &c, 2_000), Err(Refusal::Step));
        assert_eq!(step_ok(0, -8, &c, 2_000), Err(Refusal::Step));
        assert_eq!(step_ok(-1, -8, &c, 2_000), Err(Refusal::Step));
    }

    /// Exponent handling: the same USD price at another exponent is a step of 0; a real move
    /// is measured exactly at the common exponent; a rescale that leaves u128 refuses.
    #[test]
    fn step_across_exponents() {
        let c = Cached {
            price: 12_179_606_511, // $121.79606511
            expo: -8,
            publish_time: 1,
        };
        assert_eq!(step_ok(121_796_065_110, -9, &c, 0), Ok(()));
        assert_eq!(step_ok(12_179_606_511_000, -11, &c, 0), Ok(()));
        // $121.7960 is 0.53 bps below $121.79606511.
        assert_eq!(step_ok(1_217_960, -4, &c, 0), Err(Refusal::Step));
        assert_eq!(step_ok(1_217_960, -4, &c, 1), Ok(()));
        // +20 % at exponent −6: 146.155278 is inside, 146.155279 outside.
        assert_eq!(step_ok(146_155_278, -6, &c, 2_000), Ok(()));
        assert_eq!(step_ok(146_155_279, -6, &c, 2_000), Err(Refusal::Step));
        assert_eq!(common_scale(1, 0, 1, -38), Some((10u128.pow(38), 1)));
        assert_eq!(common_scale(1, -38, 1, 0), Some((1, 10u128.pow(38))));
        assert_eq!(common_scale(1_000_000, 0, 1, -38), None);
        assert_eq!(common_scale(1, i32::MAX, 1, i32::MIN), None);
        assert_eq!(common_scale(0, 0, 1, 0), None);
        assert_eq!(step_ok(1, 30, &c, 5_000), Err(Refusal::Step));
    }

    #[test]
    fn step_is_skipped_on_a_stale_or_empty_cache() {
        let u = mainnet();
        let now = u.publish_time;
        let jump = PriceUpdate {
            price: u.price * 3,
            ..u
        };
        let old = Cached {
            price: u.price,
            expo: -8,
            publish_time: now - 3_601,
        };
        assert!(cache_is_stale(&old, now, 3_600));
        assert_eq!(check(&jump, &SOL_USD, &old, now, &LIMITS), Ok(()));
        let edge = Cached {
            publish_time: now - 3_600,
            ..old
        };
        assert!(!cache_is_stale(&edge, now, 3_600));
        assert_eq!(
            check(&jump, &SOL_USD, &edge, now, &LIMITS),
            Err(Refusal::Step)
        );
        assert!(cache_is_stale(&Cached::default(), 0, 3_600));
        assert_eq!(
            check(&jump, &SOL_USD, &Cached::default(), now, &LIMITS),
            Ok(())
        );
    }

    /// M1-SEC-8: an update dated further ahead than `oracle_max_age_s` is refused (a lagging
    /// cluster clock up to that skew still accepts it), and so is an exponent outside
    /// [`EXPO_MIN`, `EXPO_MAX`], checked right after the feed id.
    #[test]
    fn future_dated_and_exponent_rules() {
        let u = mainnet();
        let now = u.publish_time;
        let ahead = |secs: i64| PriceUpdate {
            publish_time: now + secs,
            ..u
        };
        let c = Cached::default();
        assert_eq!(check(&ahead(120), &SOL_USD, &c, now, &LIMITS), Ok(()));
        assert_eq!(
            check(&ahead(121), &SOL_USD, &c, now, &LIMITS),
            Err(Refusal::FutureDated)
        );
        assert_eq!(
            check(&ahead(3_600), &SOL_USD, &c, now, &LIMITS),
            Err(Refusal::FutureDated)
        );
        assert!(not_future(i64::MAX, i64::MAX, 60));
        assert!(!not_future(i64::MAX, 0, 600));

        for (expo, ok) in [(-18, true), (0, true), (-8, true), (-19, false), (1, false)] {
            let e = PriceUpdate {
                exponent: expo,
                ..u
            };
            assert_eq!(exponent_ok(expo), ok, "{expo}");
            let want = if ok { Ok(()) } else { Err(Refusal::Exponent) };
            assert_eq!(check(&e, &SOL_USD, &c, now, &LIMITS), want, "{expo}");
        }
        // The exponent is checked after the feed id and before the age.
        let mut both = ahead(-10_000);
        both.exponent = i32::MIN;
        assert_eq!(
            check(&both, &SOL_USD, &c, now, &LIMITS),
            Err(Refusal::Exponent)
        );
        both.feed_id[0] ^= 1;
        assert_eq!(
            check(&both, &SOL_USD, &c, now, &LIMITS),
            Err(Refusal::WrongFeed)
        );
    }

    #[test]
    fn freshness_saturates() {
        assert!(is_fresh(i64::MAX, i64::MAX, 120));
        assert!(!is_fresh(i64::MIN, 0, 600));
        assert!(is_fresh(0, 60, 60));
        assert!(!is_fresh(0, 61, 60));
    }
}
