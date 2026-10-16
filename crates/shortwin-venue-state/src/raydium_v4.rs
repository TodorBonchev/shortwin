//! Raydium AMM v4 `AmmInfo` (ARCH §2.6, §3.12, §4.1, §4.2, §5.3; R-RAY §2.1–§2.3, §3.3,
//! §5.1, §5.3, §6.2–§6.3, §7; `raydium-amm@d26944b` `state.rs`, `processor.rs`, `math.rs`).
//!
//! **Layout.** `AmmInfo` is `#[repr(C, packed)]`, **752 bytes, no discriminator**; the program
//! loads it only if `owner == 675kPX9… ∧ data_len == 752 ∧ status != 0`
//! (`AmmInfo::load_mut_checked`). The length is therefore the type tag: `TargetOrders` is
//! 2,208 bytes, the AMM v4 `AmmConfig` 544, and no other account of the program is 752 bytes
//! (R-RAY §6.2). Unlike the Anchor venues this reader is **not** tolerant of other lengths:
//! the venue itself refuses them, so a pool of another length can never execute (ARCH §4.1
//! "exactly 752 B"). Fields read (offset, `u64` LE unless noted):
//!
//! | Offset | Field | Used for |
//! |---|---|---|
//! | 0 | `status` | the swap permission (§5.3) |
//! | 8 | `nonce` | the vault authority (must be 254: `5Q544f…`) |
//! | 176, 184 | `fees.swap_fee_numerator / _denominator` | the swap fee (the only fee a trader pays) |
//! | 192, 200 | `state_data.need_take_pnl_coin / _pc` | excluded from the reserves |
//! | 224 | `state_data.pool_open_time` | only while `status == 7` (unusable as a pool age, R-RAY §7) |
//! | 336, 368 | `coin_vault`, `pc_vault` (keys) | the vaults the swap moves |
//! | 400, 432 | `coin_vault_mint`, `pc_vault_mint` (keys) | which side is WSOL |
//! | 464 | `lp_mint` (key) | the circulating LP supply (λ) |
//! | 688 | `amm_owner` (key) | the pool admin recorded in the pool (informational) |
//! | 720 | `lp_amount` | the internal LP counter (λ) |
//!
//! **Pricing reserves** [`AmmInfo::reserves`]: `coin = coin_vault.amount − need_take_pnl_coin`,
//! `pc = pc_vault.amount − need_take_pnl_pc` (`Calculator::calc_total_without_take_pnl_no_orderbook`;
//! a negative side reverts every swap with `CheckedSubOverflow`). Swaps never change
//! `need_take_pnl`; the whole swap fee stays in the vault (R-RAY §2.2–§2.3).
//!
//! **Route predicate** [`AmmInfo::venue_executable`] (ARCH §5.3): `(status ∈ {1, 6} ∨
//! (status == 7 ∧ now ≥ pool_open_time)) ∧ coin_vault ≥ need_take_pnl_coin ∧ pc_vault ≥
//! need_take_pnl_pc`, and both reserves non-zero (see the method). Status values above 7 hit
//! `unreachable!()` in the venue (`AmmStatus::from_u64`) and are "no route".
//!
//! **Fee** [`AmmInfo::fee_ppm`]: the one-way fee `φ = ⌈num·10⁶/den⌉` ppm (ARCH §4.2), defined
//! only for `num < den` (the venue's own `validate_fraction`; otherwise every swap reverts).
//!
//! The owner is supplied by the caller and checked here; key checks (that the passed vaults
//! are the pool's, that the pool is the market's) are the program's job.

use crate::bytes::{key_at, u64_at};
use crate::mint::NATIVE_MINT;
use crate::{Key, ReadError};

/// Program id `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`.
pub const PROGRAM: Key = [
    75, 217, 73, 196, 54, 2, 195, 63, 32, 119, 144, 237, 22, 163, 82, 76, 161, 185, 151, 92, 241,
    33, 162, 169, 12, 255, 236, 125, 248, 182, 138, 205,
];

/// Vault / LP authority `5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1` =
/// `create_program_address(["amm authority", [254]], PROGRAM)` (R-RAY §1.1).
pub const AUTHORITY: Key = [
    65, 87, 176, 88, 15, 49, 197, 252, 228, 74, 98, 88, 45, 188, 249, 215, 142, 231, 89, 67, 160,
    132, 163, 147, 179, 80, 54, 141, 34, 137, 147, 8,
];

/// The only `nonce` a market accepts: it makes the authority [`AUTHORITY`] (R-RAY §1.1, §6.2).
pub const AUTHORITY_NONCE: u64 = 254;

/// The pool admin hard-coded in the mainnet build (`config_feature::amm_owner`,
/// `GThUX1Atko4tqhN2NaiTazWSeFWMuiUvfFnyJyUghFMJ`, a Squads v3 vault; R-RAY §5.1, §12.4). It
/// can disable swaps and set the swap fee per pool, with no timelock.
pub const POOL_ADMIN: Key = [
    229, 182, 43, 101, 203, 59, 189, 166, 245, 104, 136, 230, 111, 238, 142, 100, 220, 85, 96, 25,
    156, 15, 136, 177, 31, 226, 115, 189, 5, 158, 138, 161,
];

/// `size_of::<AmmInfo>()`: the only length the venue loads.
pub const LEN: usize = 752;

/// `status` (u64).
pub const OFF_STATUS: usize = 0;
/// `nonce` (u64).
pub const OFF_NONCE: usize = 8;
/// `fees.swap_fee_numerator` (u64).
pub const OFF_SWAP_FEE_NUMERATOR: usize = 176;
/// `fees.swap_fee_denominator` (u64).
pub const OFF_SWAP_FEE_DENOMINATOR: usize = 184;
/// `state_data.need_take_pnl_coin` (u64).
pub const OFF_NEED_TAKE_PNL_COIN: usize = 192;
/// `state_data.need_take_pnl_pc` (u64).
pub const OFF_NEED_TAKE_PNL_PC: usize = 200;
/// `state_data.pool_open_time` (u64).
pub const OFF_POOL_OPEN_TIME: usize = 224;
/// `coin_vault` (key).
pub const OFF_COIN_VAULT: usize = 336;
/// `pc_vault` (key).
pub const OFF_PC_VAULT: usize = 368;
/// `coin_vault_mint` (key).
pub const OFF_COIN_VAULT_MINT: usize = 400;
/// `pc_vault_mint` (key).
pub const OFF_PC_VAULT_MINT: usize = 432;
/// `lp_mint` (key).
pub const OFF_LP_MINT: usize = 464;
/// `amm_owner` (key).
pub const OFF_AMM_OWNER: usize = 688;
/// `lp_amount`, the internal LP counter (u64).
pub const OFF_LP_AMOUNT: usize = 720;

/// `AmmStatus::Uninitialized`.
pub const STATUS_UNINITIALIZED: u64 = 0;
/// `AmmStatus::Initialized` (swaps allowed).
pub const STATUS_INITIALIZED: u64 = 1;
/// `AmmStatus::Disabled`.
pub const STATUS_DISABLED: u64 = 2;
/// `AmmStatus::WithdrawOnly`.
pub const STATUS_WITHDRAW_ONLY: u64 = 3;
/// `AmmStatus::LiquidityOnly`.
pub const STATUS_LIQUIDITY_ONLY: u64 = 4;
/// `AmmStatus::OrderBookOnly`.
pub const STATUS_ORDERBOOK_ONLY: u64 = 5;
/// `AmmStatus::SwapOnly` (swaps allowed; 705,790 of 705,975 pools, R-RAY §0).
pub const STATUS_SWAP_ONLY: u64 = 6;
/// `AmmStatus::WaitingTrade` (swaps allowed from `pool_open_time`; the first swap writes 6).
pub const STATUS_WAITING_TRADE: u64 = 7;

/// Parts per million (`φ`'s unit, ECON §2.1).
const PPM: u128 = 1_000_000;
/// Basis points in one (λ's unit).
const BPS: u128 = 10_000;

/// The fields of an `AmmInfo` short.win reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AmmInfo {
    pub status: u64,
    pub nonce: u64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub need_take_pnl_coin: u64,
    pub need_take_pnl_pc: u64,
    pub pool_open_time: u64,
    pub coin_vault: Key,
    pub pc_vault: Key,
    pub coin_vault_mint: Key,
    pub pc_vault_mint: Key,
    pub lp_mint: Key,
    pub amm_owner: Key,
    pub lp_amount: u64,
}

/// Reads an `AmmInfo`: owner == [`PROGRAM`] and exactly [`LEN`] bytes (shorter: `TooShort`;
/// longer: `WrongTag`, another account type of the program or a layout the venue would not
/// load). Every `u64` and key is a valid value, so nothing else can fail; an uninitialized
/// pool (status 0) parses and is simply not executable.
pub fn read(owner: &Key, data: &[u8]) -> Result<AmmInfo, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if data.len() < LEN {
        return Err(ReadError::TooShort);
    }
    if data.len() > LEN {
        return Err(ReadError::WrongTag);
    }
    Ok(AmmInfo {
        status: u64_at(data, OFF_STATUS),
        nonce: u64_at(data, OFF_NONCE),
        swap_fee_numerator: u64_at(data, OFF_SWAP_FEE_NUMERATOR),
        swap_fee_denominator: u64_at(data, OFF_SWAP_FEE_DENOMINATOR),
        need_take_pnl_coin: u64_at(data, OFF_NEED_TAKE_PNL_COIN),
        need_take_pnl_pc: u64_at(data, OFF_NEED_TAKE_PNL_PC),
        pool_open_time: u64_at(data, OFF_POOL_OPEN_TIME),
        coin_vault: key_at(data, OFF_COIN_VAULT),
        pc_vault: key_at(data, OFF_PC_VAULT),
        coin_vault_mint: key_at(data, OFF_COIN_VAULT_MINT),
        pc_vault_mint: key_at(data, OFF_PC_VAULT_MINT),
        lp_mint: key_at(data, OFF_LP_MINT),
        amm_owner: key_at(data, OFF_AMM_OWNER),
        lp_amount: u64_at(data, OFF_LP_AMOUNT),
    })
}

/// `AmmStatus::swap_permission`: true only for 1 (Initialized), 6 (SwapOnly) and 7
/// (WaitingTrade, subject to the open time). Values above 7 make the venue panic
/// (`unreachable!()`), so they are false here.
pub fn swap_permission(status: u64) -> bool {
    matches!(
        status,
        STATUS_INITIALIZED | STATUS_SWAP_ONLY | STATUS_WAITING_TRADE
    )
}

/// `now ≥ t` the way the venues compare (`clock.unix_timestamp as u64 < t` fails); a negative
/// `now` (impossible on a live cluster) is treated as before every time.
pub(crate) fn reached(now: i64, t: u64) -> bool {
    u64::try_from(now).is_ok_and(|now| now >= t)
}

/// `⌊(counter − circulating)·10⁴/counter⌋`: the permanently locked LP share λ in bps (ARCH
/// §2.6; R-RAY §6.3). `lp_counter` is the venue's internal counter (AMM v4 `lp_amount` @720,
/// CPMM `lp_supply` @333), `lp_mint_supply` the LP mint's `supply`. The counters change only
/// inside the venue and LP burned outside it stays counted, so `counter − supply` can never
/// be redeemed. A supply above the counter (an accounting break) saturates to 0, the
/// conservative answer. `None` for a zero counter (no pool to speak of).
pub fn locked_lp_bps(lp_counter: u64, lp_mint_supply: u64) -> Option<u64> {
    if lp_counter == 0 {
        return None;
    }
    let locked = u128::from(lp_counter.saturating_sub(lp_mint_supply));
    let bps = locked
        .checked_mul(BPS)?
        .checked_div(u128::from(lp_counter))?;
    u64::try_from(bps).ok()
}

impl AmmInfo {
    /// Which side is WSOL: `Some(true)` if the coin is WSOL and the pc is not, `Some(false)`
    /// for the reverse, `None` if neither or both are (not a SOL pool). Base/quote order is
    /// arbitrary on AMM v4 (R-RAY §2.1): Fartcoin's main pool has WSOL as the coin, WIF's as
    /// the pc.
    pub fn sol_is_coin(&self) -> Option<bool> {
        match (
            self.coin_vault_mint == NATIVE_MINT,
            self.pc_vault_mint == NATIVE_MINT,
        ) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    }

    /// The pricing reserves `(coin, pc)` = `(coin_vault − need_take_pnl_coin, pc_vault −
    /// need_take_pnl_pc)` for vaults holding `coin_vault_amount` and `pc_vault_amount`;
    /// `None` if either side is negative (the venue reverts every swap with
    /// `CheckedSubOverflow`).
    pub fn reserves(&self, coin_vault_amount: u64, pc_vault_amount: u64) -> Option<(u64, u64)> {
        Some((
            coin_vault_amount.checked_sub(self.need_take_pnl_coin)?,
            pc_vault_amount.checked_sub(self.need_take_pnl_pc)?,
        ))
    }

    /// The status part of the route predicate: `status ∈ {1, 6} ∨ (status == 7 ∧ now ≥
    /// pool_open_time)` (ARCH §5.3; the venue's `swap_permission` plus its `WaitingTrade`
    /// open-time check).
    pub fn swap_enabled(&self, now: i64) -> bool {
        match self.status {
            STATUS_INITIALIZED | STATUS_SWAP_ONLY => true,
            STATUS_WAITING_TRADE => reached(now, self.pool_open_time),
            _ => false,
        }
    }

    /// ARCH §5.3's venue predicate for AMM v4 on the pool's vault balances: [`swap_enabled`]
    /// and both vaults at or above their `need_take_pnl`. It also requires both reserves to be
    /// non-zero — a tightening of ARCH §5.3 on a state the venue cannot trade in either
    /// (an empty output side makes every exact-in output 0 or the whole side, which the venue
    /// refuses with `InvalidInput` / `InsufficientFunds`), the same rule as the mock venue's
    /// reader contract.
    ///
    /// [`swap_enabled`]: AmmInfo::swap_enabled
    pub fn venue_executable(&self, now: i64, coin_vault_amount: u64, pc_vault_amount: u64) -> bool {
        self.swap_enabled(now)
            && self
                .reserves(coin_vault_amount, pc_vault_amount)
                .is_some_and(|(c, p)| c > 0 && p > 0)
    }

    /// The one-way fee `φ = ⌈num·10⁶/den⌉` ppm (ARCH §4.2; `shortwin_math::fees::RayV4Fee::
    /// oneway_ppm`), `None` unless `num < den` (the venue's `validate_fraction`; a pool
    /// outside it reverts every swap, so no quote exists).
    pub fn fee_ppm(&self) -> Option<u32> {
        let (num, den) = (self.swap_fee_numerator, self.swap_fee_denominator);
        if num >= den {
            return None;
        }
        let scaled = u128::from(num).checked_mul(PPM)?;
        let den = u128::from(den);
        let ppm = scaled.div_ceil(den);
        u32::try_from(ppm).ok()
    }

    /// The locked LP share λ in bps for an LP mint with `lp_mint_supply` ([`locked_lp_bps`]
    /// on `lp_amount`).
    pub fn locked_lp_bps(&self, lp_mint_supply: u64) -> Option<u64> {
        locked_lp_bps(self.lp_amount, lp_mint_supply)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::mint::{TOKEN_PROGRAM, read_base};
    use crate::token_account;

    /// Mainnet dumps at one slot (provenance: `tests/fixtures/manifest.json`, group
    /// `raydium_state`; `scripts/dump-fixtures.py`).
    const FART_POOL: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_pool.bin");
    const FART_COIN_VAULT: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_coin_vault.bin");
    const FART_PC_VAULT: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_pc_vault.bin");
    const FART_LP_MINT: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_lp_mint.bin");
    const REV_POOL: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_rev_pool.bin");
    const REV_COIN_VAULT: &[u8] = include_bytes!(
        "../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_rev_coin_vault.bin"
    );
    const REV_PC_VAULT: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_rev_pc_vault.bin");
    const REV_LP_MINT: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_fartcoin_rev_lp_mint.bin");
    const V4_CONFIG: &[u8] =
        include_bytes!("../../../tests/fixtures/accounts/raydium/ray_v4_amm_config.bin");

    /// `F6iWqisguZYprVwp916BgGR7d5ahP6Ev5E213k8y3MEb` (Fartcoin pool, coin = WSOL vault).
    const FART_COIN_VAULT_KEY: Key = [
        209, 122, 118, 42, 199, 182, 60, 75, 200, 252, 155, 138, 39, 82, 229, 254, 32, 54, 41, 140,
        9, 165, 84, 182, 115, 225, 119, 197, 235, 69, 48, 212,
    ];
    /// `9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump` (Fartcoin).
    const FARTCOIN: Key = [
        121, 120, 183, 20, 69, 60, 211, 232, 122, 235, 31, 192, 155, 240, 103, 249, 108, 210, 212,
        214, 155, 87, 19, 149, 170, 155, 241, 134, 175, 249, 218, 63,
    ];

    fn fart() -> AmmInfo {
        read(&PROGRAM, FART_POOL).unwrap()
    }

    fn rev() -> AmmInfo {
        read(&PROGRAM, REV_POOL).unwrap()
    }

    fn amount(data: &[u8]) -> u64 {
        token_account::read(&TOKEN_PROGRAM, data).unwrap().amount
    }

    #[test]
    fn layout_constants() {
        assert_eq!(LEN, 752);
        assert_eq!(OFF_LP_AMOUNT + 8, 728);
        assert_eq!(OFF_PC_VAULT - OFF_COIN_VAULT, 32);
    }

    /// The Fartcoin pool decodes to the values the manifest records (R-RAY §2.1's table: status
    /// 6, nonce 254, fee 25/10,000, WSOL as the coin, `pool_open_time` 0 on a pump graduation
    /// pool), and its vaults are the dumped vault accounts with the pool's mints and the
    /// authority `5Q544f…` as their owner.
    #[test]
    fn reads_the_fartcoin_pool() {
        let p = fart();
        assert_eq!(p.status, STATUS_SWAP_ONLY);
        assert_eq!(p.nonce, AUTHORITY_NONCE);
        assert_eq!((p.swap_fee_numerator, p.swap_fee_denominator), (25, 10_000));
        assert_eq!((p.need_take_pnl_coin, p.need_take_pnl_pc), (0, 0));
        assert_eq!(p.pool_open_time, 0);
        assert_eq!(p.coin_vault, FART_COIN_VAULT_KEY);
        assert_eq!(p.coin_vault_mint, NATIVE_MINT);
        assert_eq!(p.pc_vault_mint, FARTCOIN);
        assert_eq!(p.amm_owner, POOL_ADMIN);
        assert_eq!(p.lp_amount, 4_051_966_758_784);
        assert_eq!(p.sol_is_coin(), Some(true));
        for (vault, mint) in [(FART_COIN_VAULT, NATIVE_MINT), (FART_PC_VAULT, FARTCOIN)] {
            let v = token_account::read(&TOKEN_PROGRAM, vault).unwrap();
            assert_eq!((v.mint, v.owner), (mint, AUTHORITY));
        }
        let (coin, pc) = p
            .reserves(amount(FART_COIN_VAULT), amount(FART_PC_VAULT))
            .unwrap();
        // need_take_pnl is 0 here: the reserves are the vault balances.
        assert_eq!((coin, pc), (38_257_803_921_700, 24_095_698_311_693));
        assert!(p.venue_executable(0, coin, pc));
        assert_eq!(p.fee_ppm(), Some(2_500));
        // λ = 1 − 8,906,225,417 / 4,051,966,758,784 = 99.78 % (R-RAY §6.3 table).
        let lp = read_base(&TOKEN_PROGRAM, FART_LP_MINT).unwrap();
        assert_eq!(lp.supply, 8_906_225_417);
        assert_eq!(p.locked_lp_bps(lp.supply), Some(9_978));
    }

    /// The reversed-order pool: WSOL is the pc, and `need_take_pnl` is non-zero on both
    /// sides, so the reserves are strictly below the vault balances.
    #[test]
    fn reads_the_reversed_pool_and_excludes_need_take_pnl() {
        let p = rev();
        assert_eq!(p.sol_is_coin(), Some(false));
        assert_eq!(p.coin_vault_mint, FARTCOIN);
        assert_eq!(p.pc_vault_mint, NATIVE_MINT);
        assert_eq!(
            (p.need_take_pnl_coin, p.need_take_pnl_pc),
            (14_461_465, 34_798_465)
        );
        let (cv, pv) = (amount(REV_COIN_VAULT), amount(REV_PC_VAULT));
        assert_eq!((cv, pv), (1_126_100_763, 1_805_485_317));
        assert_eq!(
            p.reserves(cv, pv),
            Some((1_126_100_763 - 14_461_465, 1_805_485_317 - 34_798_465))
        );
        // One unit below either counter: the venue reverts (CheckedSubOverflow).
        assert_eq!(p.reserves(p.need_take_pnl_coin - 1, pv), None);
        assert_eq!(p.reserves(cv, p.need_take_pnl_pc - 1), None);
        assert!(p.venue_executable(1, cv, pv));
        assert!(!p.venue_executable(1, cv, p.need_take_pnl_pc - 1));
        // Vaults exactly at the counters: executable per ARCH §5.3's inequality, but an
        // empty side cannot trade (the tightening documented on `venue_executable`).
        assert_eq!(
            p.reserves(p.need_take_pnl_coin, pv),
            Some((0, pv - 34_798_465))
        );
        assert!(!p.venue_executable(1, p.need_take_pnl_coin, pv));
        let lp = read_base(&TOKEN_PROGRAM, REV_LP_MINT).unwrap();
        // Only the 10^6 LP withheld at initialisation are locked: λ ≈ 0.3 % (R-RAY §6.1).
        assert_eq!(p.lp_amount - lp.supply, 1_000_000);
        assert_eq!(p.locked_lp_bps(lp.supply), Some(31));
    }

    /// Only the 752-byte `AmmInfo` of the AMM v4 program parses.
    #[test]
    fn refusals() {
        assert_eq!(read(&[0; 32], FART_POOL), Err(ReadError::WrongOwner));
        assert_eq!(read(&TOKEN_PROGRAM, FART_POOL), Err(ReadError::WrongOwner));
        assert_eq!(
            read(&PROGRAM, &FART_POOL[..LEN - 1]),
            Err(ReadError::TooShort)
        );
        assert_eq!(read(&PROGRAM, &[]), Err(ReadError::TooShort));
        let mut long = FART_POOL.to_vec();
        long.push(0);
        assert_eq!(read(&PROGRAM, &long), Err(ReadError::WrongTag));
        // The program's AmmConfig (544 B) is not a pool.
        assert_eq!(V4_CONFIG.len(), 544);
        assert_eq!(read(&PROGRAM, V4_CONFIG), Err(ReadError::TooShort));
    }

    fn with_status(status: u64, open_time: u64) -> AmmInfo {
        AmmInfo {
            status,
            pool_open_time: open_time,
            ..fart()
        }
    }

    /// R-RAY §3.3 / §5.3: the status gate, value by value, including WaitingTrade's open time
    /// and the values above 7 that make the venue panic.
    #[test]
    fn status_predicate() {
        for s in 0..=9u64 {
            let allowed = matches!(s, 1 | 6 | 7);
            assert_eq!(swap_permission(s), allowed, "status {s}");
        }
        assert!(!swap_permission(u64::MAX));
        let (c, p) = (1_000, 1_000);
        for s in [0, 2, 3, 4, 5, 8, 9, u64::MAX] {
            assert!(!with_status(s, 0).swap_enabled(i64::MAX), "status {s}");
            assert!(!with_status(s, 0).venue_executable(i64::MAX, c, p));
        }
        for s in [1, 6] {
            // The open time is ignored outside WaitingTrade (the venue reads it only there).
            assert!(with_status(s, u64::MAX).swap_enabled(0));
            assert!(with_status(s, u64::MAX).venue_executable(0, c, p));
        }
        let open = 1_790_000_000;
        let w = with_status(STATUS_WAITING_TRADE, open);
        assert!(!w.swap_enabled(1_789_999_999));
        assert!(w.swap_enabled(1_790_000_000));
        assert!(w.swap_enabled(1_790_000_001));
        assert!(!w.swap_enabled(-1));
        assert!(with_status(STATUS_WAITING_TRADE, 0).swap_enabled(0));
        assert!(!w.venue_executable(1_789_999_999, c, p));
        assert!(w.venue_executable(1_790_000_000, c, p));
    }

    /// `φ = ⌈num·10⁶/den⌉`, defined only for `num < den`.
    #[test]
    fn fee_ppm() {
        let f = |num, den| {
            AmmInfo {
                swap_fee_numerator: num,
                swap_fee_denominator: den,
                ..fart()
            }
            .fee_ppm()
        };
        assert_eq!(f(25, 10_000), Some(2_500));
        assert_eq!(f(0, 10_000), Some(0));
        assert_eq!(f(1, 3), Some(333_334));
        assert_eq!(f(9_999, 10_000), Some(999_900));
        assert_eq!(f(u64::MAX - 1, u64::MAX), Some(1_000_000));
        assert_eq!(f(1, u64::MAX), Some(1));
        assert_eq!(f(10_000, 10_000), None);
        assert_eq!(f(10_001, 10_000), None);
        assert_eq!(f(0, 0), None);
    }

    #[test]
    fn locked_lp_share() {
        assert_eq!(locked_lp_bps(0, 0), None);
        assert_eq!(locked_lp_bps(100, 0), Some(10_000));
        assert_eq!(locked_lp_bps(100, 100), Some(0));
        assert_eq!(locked_lp_bps(100, 101), Some(0));
        assert_eq!(locked_lp_bps(10_000, 1), Some(9_999));
        assert_eq!(locked_lp_bps(u64::MAX, 0), Some(10_000));
        assert_eq!(locked_lp_bps(u64::MAX, u64::MAX / 2), Some(5_000));
    }
}
