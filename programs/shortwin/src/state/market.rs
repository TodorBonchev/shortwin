//! `Market` (zero-copy, seeds `["market", mint]`, 3,104 B; ARCH §2.3.4) and `VenueKind`
//! (ARCH §2.5, A1).
//!
//! Exact layout, field by field (every offset a multiple of its alignment, all padding
//! explicit, identical on sbpfv3, x86_64 and wasm32; `layout_asserts.rs` asserts each offset,
//! and `docs/design/arch/layout.py` generates the same table). Offsets include the 8-byte
//! discriminator.
//!
//! | Group | Offset | Bytes |
//! |---|---|---|
//! | header | 8 | 24 |
//! | identity (`mint`, `token_program`, `creator`) | 32 | 96 |
//! | own accounts | 128 | 160 |
//! | pump detection keys | 288 | 64 |
//! | curve venue cache | 352 | 256 |
//! | pool venue cache | 608 | 320 |
//! | pool selection | 928 | 248 |
//! | venue timing | 1,176 | 24 |
//! | route chains | 1,200 | 32 |
//! | lender pool (market part) | 1,232 | 192 |
//! | rent | 1,424 | 104 |
//! | observation | 1,528 | 1,272 |
//! | market-level params | 2,800 | 32 |
//! | OI (`max_q_hwm` @2,848 and `min_open_fee_ppm` @2,856, from `_reserved`) | 2,832 | 32 |
//! | derived keys (`trader_wsol_ata` @2,864, `pump_pool_authority` @2,896; M5-SEC-6, from `_reserved`) | 2,864 | 64 |
//! | `coverage_cleared_at` (M7-SEC-3, from `_reserved`) | 2,928 | 8 |
//! | `lender_credit_rem` (AIR-03, from `_reserved`) | 2,936 | 16 |
//! | `min_open_hard_cap_bps` @2,952 (AIR-19, from `_reserved`) | 2,952 | 8 |
//! | `_reserved` | 2,960 | 144 |
//!
//! The math crate's state types (`pool::Pool`, `rent::{Basis, Aggr}`, `observe::*`) hold native
//! `u128`/`i128`; the conversions below ([`Market::pool`], [`Market::set_pool`], …) copy field by
//! field at load and store (ARCH §2.2 "math types are not account layouts").

use anchor_lang::prelude::*;
use shortwin_math::observe::{
    Bucket, Ema, EmaParams, LastBucket, Observation, Ring, RingEntry as MathRingEntry, RING_LEN,
};
use shortwin_math::pool::{Pool, WIPE_REWARDS_CLAIMED, WIPE_TRADER_SWEPT};
use shortwin_math::rent::{Aggr, Basis};

use crate::constants::{
    GUARD_MASK_PUMP_CURVE, GUARD_MASK_PUMP_SWAP, GUARD_MASK_RAYDIUM, PUMPSWAP_LOCK_MIN_BPS,
    VENUE_BIT_PUMP_CURVE, VENUE_BIT_PUMP_SWAP, VENUE_BIT_RAYDIUM_CPMM, VENUE_BIT_RAYDIUM_V4,
};
use crate::state::pod::{get_all, set_all, PodI128, PodU128};

/// Venue state of a market (ARCH §2.5, A1). `None` = list-only: deposits and withdrawals
/// only, never terminal. Stored as `Market.venue` (`u8`); never serialized as an enum (so a
/// `test-venue` build can add [`VenueKind::Mock`] without changing the IDL).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VenueKind {
    /// List-only (no eligible constant-product SOL pool, CLMM/DLMM-only tokens, …).
    None = 0,
    /// pump.fun coin on its live bonding curve (Tier D).
    PumpCurve = 1,
    /// Completed curve, canonical pool not yet created: exits pause, S3 after the pause.
    PumpMigrating = 2,
    /// pump.fun graduate on its canonical PumpSwap pool.
    PumpSwap = 3,
    /// Adopted Raydium AMM v4 SOL pool.
    RaydiumV4 = 4,
    /// Adopted Raydium CPMM SOL pool.
    RaydiumCpmm = 5,
    /// TEST-ONLY: a pool of the mock venue `tests/mock_cpamm` (feature `test-venue`; A49).
    /// A CPMM stand-in: fee on the input, WSOL side under SPL Token, delegate sells from the
    /// pool vault and direct-to-vault buys (ARCH §2.4.1 Raydium row); no venue-side rent.
    #[cfg(feature = "test-venue")]
    Mock = 6,
}

impl VenueKind {
    /// The kind stored as `u8` (`Market.venue`), or `None` for an unknown value.
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::PumpCurve),
            2 => Some(Self::PumpMigrating),
            3 => Some(Self::PumpSwap),
            4 => Some(Self::RaydiumV4),
            5 => Some(Self::RaydiumCpmm),
            #[cfg(feature = "test-venue")]
            6 => Some(Self::Mock),
            _ => None,
        }
    }

    /// The `VenueGuard.upgraded` mask that blocks opens on this venue (ARCH §3.1; I17), 0 for
    /// venues that take no opens. The mock is a CPMM stand-in and uses the Raydium mask, so
    /// a flagged CPMM ProgramData blocks mock opens in the tests (I17 on the mock).
    pub const fn guard_mask(self) -> u32 {
        match self {
            Self::PumpCurve => GUARD_MASK_PUMP_CURVE,
            Self::PumpSwap => GUARD_MASK_PUMP_SWAP,
            Self::RaydiumV4 | Self::RaydiumCpmm => GUARD_MASK_RAYDIUM,
            #[cfg(feature = "test-venue")]
            Self::Mock => GUARD_MASK_RAYDIUM,
            Self::None | Self::PumpMigrating => 0,
        }
    }

    /// The `GlobalConfig.venue_enabled` / `venue_route_broken` bit of this venue (ARCH
    /// §2.3.2), 0 for venues without one (`None`, `PumpMigrating`: no opens).
    pub const fn venue_bit(self) -> u32 {
        match self {
            Self::PumpCurve => VENUE_BIT_PUMP_CURVE,
            Self::PumpSwap => VENUE_BIT_PUMP_SWAP,
            Self::RaydiumV4 => VENUE_BIT_RAYDIUM_V4,
            Self::RaydiumCpmm => VENUE_BIT_RAYDIUM_CPMM,
            #[cfg(feature = "test-venue")]
            Self::Mock => crate::constants::VENUE_BIT_MOCK,
            Self::None | Self::PumpMigrating => 0,
        }
    }

    /// Whether the venue has a CPI adapter (opens and buybacks): every kind but `None` and
    /// `PumpMigrating` (ARCH §3.4 step 1, §3.5 step 2).
    pub const fn trades(self) -> bool {
        !matches!(self, Self::None | Self::PumpMigrating)
    }

    /// One of the two Raydium kinds (AMM v4, CPMM): pool adopted by verification and
    /// eligibility (ARCH §2.6), locked depth `D`, the listing window, the Raydium latch mask.
    pub const fn is_raydium(self) -> bool {
        matches!(self, Self::RaydiumV4 | Self::RaydiumCpmm)
    }

    /// Whether the venue has a locked depth `D` (`Market.pool_d`, refreshed at every executable
    /// observation) that bounds the size cap's `X₀` and the tier's `R_eff` at `D/λ_min` (ARCH §2.6,
    /// §4.6, §4.7; `λ_min` = [`Self::lock_min_bps`]) and the exit reference's EMA terms at `D`
    /// itself (§3.6; `engine::bounds::ref_depth_cap`, AIR-14):
    /// the Raydium kinds (V2-8, M4F-1) and — since the owner's decision of 2026-09-30 (M6R-1, ARCH
    /// changelog 56, ECON #105) — PumpSwap, whose `D` is the depth of its burned creation LP
    /// (credited at `λ_min` = 100 % since OWNER-M6D-1, ARCH changelog 57, ECON #106). Not the
    /// curve (no LP) nor the mock (no LP accounting).
    pub const fn has_locked_depth(self) -> bool {
        matches!(self, Self::RaydiumV4 | Self::RaydiumCpmm | Self::PumpSwap)
    }

    /// The `λ_min` that divides this venue's locked depth `D` (`Market.pool_d`) wherever the
    /// program credits it to a size or a tier (ARCH §2.3.3, §2.5, §4.6, §4.7): `X₀`'s `D` term and
    /// with it every cap on `X₀ + q` (`open_short`), the tier's `R_eff` (`engine::tier`) and a
    /// price start's anchor. The exit reference's cap is `D` itself on both venues (AIR-14:
    /// `engine::bounds::ref_depth_cap`, at `λ` = 100 %). Raydium:
    /// the config's `pool_lock_min_bps`. PumpSwap: [`PUMPSWAP_LOCK_MIN_BPS`] = 10,000 (100 %)
    /// whatever `pool_lock_min_bps` is — the owner's decision of 2026-09-30 (OWNER-M6D-1, option
    /// (a); ARCH changelog 57, ECON #106): a canonical pool's lock is its burned creation LP, so
    /// nothing a third party deposited is credited, to a size, a tier or an exit's reference (the
    /// reference cap and the price start's anchor since M6E-1: a cap at `2D` let a deposit pulled
    /// at the deadline hold up an expiry for up to an hour). `None` for a venue without a locked
    /// depth ([`Self::has_locked_depth`]).
    pub const fn lock_min_bps(self, pool_lock_min_bps: u32) -> Option<u32> {
        match self {
            Self::PumpSwap => Some(PUMPSWAP_LOCK_MIN_BPS),
            Self::RaydiumV4 | Self::RaydiumCpmm => Some(pool_lock_min_bps),
            _ => None,
        }
    }

    /// Whether the venue runs the Raydium fast-path chain `pool_fail` (ARCH §2.6 rule 3, §3.0
    /// step 8): the Raydium kinds, and the mock as their stand-in.
    pub const fn has_pool_fail_chain(self) -> bool {
        match self {
            Self::RaydiumV4 | Self::RaydiumCpmm => true,
            #[cfg(feature = "test-venue")]
            Self::Mock => true,
            _ => false,
        }
    }

    /// The seed byte of this venue's `VenueExtras` account (`["venue_extras", venue]`), or
    /// `None` for a venue without venue CPIs. `initialize_config` creates the four of
    /// [`VenueKind::WITH_EXTRAS`]; a `test-venue` build's `Mock` account (seed 6) is installed
    /// by the test harness.
    pub const fn extras_seed(self) -> Option<u8> {
        match self {
            Self::PumpCurve | Self::PumpSwap | Self::RaydiumV4 | Self::RaydiumCpmm => {
                Some(self as u8)
            }
            #[cfg(feature = "test-venue")]
            Self::Mock => Some(self as u8),
            Self::None | Self::PumpMigrating => None,
        }
    }

    /// The venue kinds that have a `VenueExtras` account (a venue CPI), in creation order.
    pub const WITH_EXTRAS: [Self; 4] = [
        Self::PumpCurve,
        Self::PumpSwap,
        Self::RaydiumV4,
        Self::RaydiumCpmm,
    ];
}

/// `Market.flags` bit 0: new opens paused on this market (SPEC §11).
pub const MARKET_FLAG_PAUSED_OPENS: u16 = 1 << 0;
/// Bit 1: the mint failed the safety filter (`flag_unsafe_mint`; blocks opens and deposits).
pub const MARKET_FLAG_MINT_UNSAFE: u16 = 1 << 1;
/// Bit 2: Emergency route-broken flag (A40; opens and S3 evidence only; O6, built in M2).
pub const MARKET_FLAG_ROUTE_BROKEN: u16 = 1 << 2;
/// Bit 3: on Raydium, SOL is token 0 (CPMM) / the coin side (AMM v4).
pub const MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN: u16 = 1 << 3;
/// Bit 4: the pump curve completed with zeroed reserves and no canonical pool.
pub const MARKET_FLAG_GRADUATED_ELSEWHERE: u16 = 1 << 4;
/// Bit 5: the latest `ShareEpoch` record is a full wipe still open for top-ups (V2-12).
pub const MARKET_FLAG_WIPE_LIVE: u16 = 1 << 5;
/// Bit 6: opens paused for keeper coverage (degraded mode, A69; O17).
pub const MARKET_FLAG_PAUSED_COVERAGE: u16 = 1 << 6;
/// Bit 7: `sweep_trader` ran since the last full wipe (R4A-7).
pub const MARKET_FLAG_WIPE_TRADER_SWEPT: u16 = 1 << 7;
/// Bit 8: `claim_venue_rewards` ran since the last full wipe (R4A-7).
pub const MARKET_FLAG_WIPE_REWARDS_CLAIMED: u16 = 1 << 8;

/// Indices into `Market.pool_cache` while the venue is `PumpSwap` (ARCH §2.3.4). Since M6 a
/// **pump-curve** market also holds them (derived once at listing, `venues::pump_amm::precompute`),
/// so its `→ PumpSwap` transition derives nothing; nothing reads them before that transition. (A
/// market `crank_price` re-detects onto the curve derives them at its transition instead.)
pub mod pump_swap_cache {
    /// The canonical pool's base vault (`Pool` @139; an ATA of the pool).
    pub const POOL_BASE_VAULT: usize = 0;
    /// Its WSOL vault (`Pool` @171).
    pub const POOL_QUOTE_VAULT: usize = 1;
    /// `PDA(pAMM, ["pool-v2", mint])` (T15; a marker that need not exist).
    pub const POOL_V2_MARKER: usize = 2;
    /// `ATA(boost_vault, WSOL)` (P5; may not exist).
    pub const BOOST_VAULT_ATA: usize = 3;
    /// The trader's pAMM UVA `PDA(pAMM, ["user_volume_accumulator", trader])` (T14).
    pub const PAMM_UVA: usize = 4;
    /// `ATA(pAMM UVA, WSOL)`: where PumpSwap accrues the trader's cashback (T18).
    pub const PAMM_UVA_QUOTE_ATA: usize = 5;
    /// The pool's `coin_creator` the two keys below derive from.
    pub const CACHED_COIN_CREATOR: usize = 6;
    /// `PDA(pAMM, ["creator_vault", coin_creator])` (T12; underscore).
    pub const CACHED_COIN_CREATOR_VAULT_AUTH: usize = 7;
    /// `ATA(T12, WSOL)` (T11).
    pub const CACHED_COIN_CREATOR_VAULT_ATA: usize = 8;
    /// `PDA(pAMM, ["boost_vault", pool])`, the token owner the boost gate's reader checks (M6: the
    /// tenth slot of the union, so the gate derives nothing on a trade; ARCH §2.3.4 lists nine).
    pub const BOOST_VAULT: usize = 9;
}

/// Indices into `Market.pool_cache` while the venue is `RaydiumV4` / `RaydiumCpmm`.
pub mod raydium_cache {
    pub const RAY_POOL: usize = 0;
    pub const RAY_VAULT_SOL: usize = 1;
    pub const RAY_VAULT_TOK: usize = 2;
    pub const RAY_AMM_CONFIG: usize = 3;
    pub const RAY_OBSERVATION: usize = 4;
    pub const RAY_LP_MINT: usize = 5;
    pub const RAY_TOK_PROGRAM: usize = 6;
}

/// One EMA ring entry (ARCH §2.3.4; 136 B): `(x, y) = reprice(l, p)` cached at integration;
/// `(lo, hi, llo, lhi)` the integrated bucket's price and liquidity extremes (R4B-2).
#[zero_copy]
#[derive(Default, PartialEq, Eq)]
pub struct RingEntry {
    pub ts: i64,
    pub p: PodU128,
    pub l: PodU128,
    pub x: PodU128,
    pub y: PodU128,
    pub lo: PodU128,
    pub hi: PodU128,
    pub llo: PodU128,
    pub lhi: PodU128,
}

/// A market: one per mint (ARCH §2.3.4).
#[account(zero_copy)]
pub struct Market {
    // ---- header @8 (24) ----
    /// Layout version ([`crate::constants::MARKET_VERSION`]).
    pub version: u16,
    /// `MARKET_FLAG_*`.
    pub flags: u16,
    pub bump: u8,
    pub pool_vault_bump: u8,
    pub market_vault_bump: u8,
    pub trader_bump: u8,
    pub trader_wsol_bump: u8,
    /// [`VenueKind`] as `u8`.
    pub venue: u8,
    /// Cached tier (`shortwin_math::tier::Tier` as `u8`).
    pub tier: u8,
    /// Mint decimals.
    pub decimals: u8,
    pub ring_head: u8,
    pub ring_count: u8,
    /// Integrations left in the post-change warm-up (M4R, **PROPOSED**, owner item O18; was
    /// `_pad0[0]`): `EMA_WARMUP_BUCKETS` after a restart (a Raydium adoption or listing), counted
    /// down by the prologue's roll; `0` = confirmed (every account created before M4R). While it
    /// is positive on an executable venue opens, `liquidate` and `expire` refuse `EmaWarmingUp`
    /// and `sol_settle` evaluates nothing (`shortwin_math::observe::Ema::warm`).
    pub ema_warm: u8,
    /// `1` while the post-change reference guard is set (M4R; was `_pad0[1]`): the keeper exits'
    /// reference liquidity reads `min(L_ema, L_mlo)` (`shortwin_math::bounds::ref_liquidity`).
    pub ema_guard: u8,
    pub created_at: i64,
    // ---- identity @32 (96) ----
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub creator: Pubkey,
    // ---- own accounts @128 (160) ----
    pub pool_vault: Pubkey,
    pub market_vault: Pubkey,
    pub trader: Pubkey,
    pub trader_base_ata: Pubkey,
    pub trader_wsol: Pubkey,
    // ---- pump detection keys @288 (64): always derived and stored at creation (C0-15) ----
    pub bonding_curve: Pubkey,
    pub canonical_pool: Pubkey,
    // ---- curve venue cache @352 (256) ----
    pub curve_base_ata: Pubkey,
    pub curve_quote_ata: Pubkey,
    pub sharing_config: Pubkey,
    pub pump_uva: Pubkey,
    pub pump_uva_quote_ata: Pubkey,
    pub cached_creator: Pubkey,
    pub cached_creator_vault: Pubkey,
    pub cached_creator_vault_ata: Pubkey,
    // ---- pool venue cache @608 (320): union by venue (`pump_swap_cache`, `raydium_cache`) ----
    pub pool_cache: [Pubkey; 10],
    // ---- pool selection @928 (248) ----
    pub pool_seen_at: i64,
    /// Incumbent locked depth `D`.
    pub pool_d: PodU128,
    /// Fast-path window: best candidate's keys.
    pub fp_keys: [Pubkey; 6],
    /// Fast-path window: best candidate's `D`.
    pub fp_d: PodU128,
    pub fp_t0: i64,
    pub fp_venue: u8,
    pub _pad1: u8,
    /// Retired `fp_lambda_bps` (M4-SEC-5's record of the candidate's `λ`; M4R: an adoption reads
    /// no sample of the pool): padding, zeroed by `clear_window`.
    pub _pad1a: [u8; 2],
    pub _pad1b: [u8; 4],
    // ---- venue timing @1176 (24); 0 = unset ----
    pub migration_seen_at: i64,
    pub graduated_at: i64,
    pub tier_computed_at: i64,
    // ---- route chains @1200 (32) ----
    pub no_route_since: i64,
    pub last_no_route_obs: i64,
    pub pool_fail_since: i64,
    pub last_pool_fail_obs: i64,
    // ---- lender pool, market part @1232 (192) ----
    /// `T`: unborrowed tokens (internal counter; donations ignored).
    pub tokens_free: u64,
    /// `ΣB_Q`.
    pub borrowed_q: u64,
    pub borrowed_q_by_tier: [u64; 4],
    /// `S`.
    pub total_shares: PodU128,
    pub acc: PodI128,
    pub carry: PodI128,
    pub acc_r: PodU128,
    pub carry_r: PodU128,
    pub dust: PodI128,
    pub receivable: u64,
    pub max_release: i64,
    pub vest_bal: u64,
    pub vest_last: i64,
    pub vest_end: i64,
    pub share_epoch: u32,
    pub _pad2: [u8; 4],
    // ---- rent @1424 (104) ----
    /// `J0, J1, J2`.
    pub rent_basis: [PodU128; 3],
    /// `A_j = Σ Q·coef_j·(10⁴ − p_pos)`.
    pub rent_aggr: [PodU128; 3],
    pub last_accrual_ts: i64,
    // ---- observation @1528 (1,272) ----
    pub b_sec: i64,
    pub b_lo: PodU128,
    pub b_hi: PodU128,
    pub b_llo: PodU128,
    pub b_lhi: PodU128,
    pub lb_sec: i64,
    pub lb_lo: PodU128,
    pub lb_hi: PodU128,
    pub ema_price: PodU128,
    pub ema_liq: PodU128,
    pub liq_slow: PodU128,
    pub ema_ts: i64,
    pub slow_ts: i64,
    pub ema_count: u64,
    pub ring: [RingEntry; 8],
    // ---- market-level params @2800 (32) ----
    pub ema_params_version: u32,
    pub kink_version: u32,
    pub ema_clamp_bps: u32,
    pub util_kink_bps: u32,
    pub liq_slow_tau_s: u32,
    pub liq_slow_dtmax_s: u32,
    pub ema_dtmax_s: u16,
    pub _pad3: [u8; 6],
    // ---- OI @2832 (32) ----
    pub open_positions: u32,
    pub _pad4: [u8; 4],
    pub total_notional_open: u64,
    /// High-water mark of `q` over the positions opened since `open_positions` last returned to 0
    /// (was `_reserved[0..8]`; owner decision of 2026-09-29, "switch only if all fit"): raised by
    /// every open (`engine::waterfall::add_position`), reset to 0 by the exit that takes
    /// `open_positions` to 0 (`remove_position`), so it is at least every open position's `q` —
    /// conservative by design (a position that has closed may have set it). A pool adoption that
    /// carries open positions checks `min(max_q_hwm, ΣB_Q)` against the candidate's per-position
    /// caps in O(1) (`shortwin_math::caps::carried_fit`; `engine::pool_select::carried_fit`).
    pub max_q_hwm: u64,
    /// Low-water mark of the one-way venue fee φ (ppm) the opens since `open_positions` last
    /// returned to 0 paid — the fee their caps were computed at and their margin posted at (was
    /// `_reserved[8..12]`; FIT-2): set by the first open after the market emptied, lowered by every
    /// later one (`engine::waterfall::add_position`, `shortwin_math::caps::open_fee_mark`), reset
    /// to 0 with `max_q_hwm` (`remove_position`). The carried fit's fee-scaled caps take
    /// `min(min_open_fee_ppm, φ_cand, CARRIED_FEE_CAP_PPM)` (`shortwin_math::caps::carried_phi`;
    /// `engine::pool_select::carried_caps`; FITS-1: 25 bps), so a position carried onto a
    /// higher-fee pool stays in the size class of the fee it was margined at, and none above the
    /// 25-bps class.
    pub min_open_fee_ppm: u32,
    pub _pad5: [u8; 4],
    // ---- derived keys @2864 (64; M5-SEC-6, from `_reserved`): written at listing ----
    /// `ATA(trader, WSOL)` under SPL Token (the pump curve's TradeExtras T11, `sweep_trader`'s
    /// stray-ATA slot): derived once at listing instead of on every trade. The trader is the
    /// market's PDA, so a coin's creator who grinds the mint can give this ATA a low bump, and
    /// `find_program_address` charges 1,500 CU per bump it tries (M5-SEC-6).
    pub trader_wsol_ata: Pubkey,
    /// `PDA(pump, ["pool-authority", mint])`, the canonical PumpSwap pool's `creator`, which
    /// `canonical_pool_valid` compares (ARCH §2.5); derived once at listing for the same reason.
    pub pump_pool_authority: Pubkey,
    // ---- misc @2928 (176) ----
    /// M7-SEC-3 (was `_reserved[0..8]`): the unix second `set_coverage_pause(false)` last cleared
    /// a set `paused_coverage` flag (signed by the coverage key or, since OWNER-M7-3, the Emergency
    /// vault). `open_short` refuses until `COVERAGE_CLEAR_DELAY_S` after it
    /// (`CoveragePaused`; `engine::guard::open_static`), so the `coverage_authority` key cannot
    /// bracket an open on a coverage-paused market — `[clear, open_short, set]` in one
    /// transaction or one second — and the flag stays visibly cleared for the keeper's reads
    /// before any open can land. 0 on existing accounts (no delay).
    pub coverage_cleared_at: i64,
    /// AIR-03 (was `_reserved[0..16]`): the remainder of the last accrual's lender rent credit,
    /// `(Σ_j A_j·δJ_j + rem) mod (10⁴·10⁴·31,536,000·WAD)` (`shortwin_math::rent::
    /// lender_credit_carried`, `engine::prologue::accrue`), carried into the next accrual so the
    /// credits sum to the floor of their exact total instead of flooring each accrual alone (a
    /// market cranked every second lost up to a lamport a second, which the exits' true-ups
    /// left in `lender_sol`). Always below that denominator; 0 on existing accounts.
    pub lender_credit_rem: PodU128,
    /// AIR-19 (was `_reserved[16..18]`): low-water mark of the `venue_fee_hard_cap_bps` the
    /// positions opened since `open_positions` last returned to 0 locked — set by the first open
    /// after the market emptied, lowered by every later one (`engine::waterfall::add_position`),
    /// reset to 0 by the exit that takes `open_positions` to 0 (`remove_position`), so it is at
    /// most every open position's locked cap (conservative by design: a position that has
    /// closed may have set it). The market's S3 chain counts a live fee above
    /// `min(global cap, this mark)` as no route (`engine::route::s3_no_route`), so a position
    /// whose locked cap is below a raised global cap reaches S3 when the fee passes its own cap.
    /// 0 (unset) on existing accounts: the global cap alone, as before.
    pub min_open_hard_cap_bps: u16,
    pub _pad6: [u8; 6],
    pub _reserved: [u8; 144],
}

impl Market {
    /// PDA seed prefix (`["market", mint]`).
    pub const SEED: &'static [u8] = b"market";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + core::mem::size_of::<Self>();

    /// `true` if `key` is this market's PDA `["market", mint]` with the stored bump
    /// (`create_program_address`, no bump search; ARCH §2.3). The Accounts constraint of the
    /// instructions that take `market` without its `mint` account (M1-SEC-6 / M1-FID-6): a
    /// `seeds = [.., market.load()?.mint]` constraint would put a seed read from the account
    /// itself into the IDL, which clients cannot resolve (Codama reports a `market -> market`
    /// cycle).
    pub fn is_pda(&self, key: &Pubkey) -> bool {
        Pubkey::create_program_address(&[Self::SEED, self.mint.as_ref(), &[self.bump]], &crate::ID)
            .is_ok_and(|pda| pda == *key)
    }

    /// The venue, or `None` for a corrupt value (never written).
    pub fn venue_kind(&self) -> Option<VenueKind> {
        VenueKind::from_u8(self.venue)
    }

    /// `true` if every bit of `flag` is set.
    pub fn has_flag(&self, flag: u16) -> bool {
        self.flags & flag == flag
    }

    /// Sets or clears `flag`.
    pub fn set_flag(&mut self, flag: u16, on: bool) {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
    }

    /// The lender-pool state as `shortwin_math::pool::Pool` (ARCH §2.3.4 lender group and the
    /// wipe bits of `flags`).
    pub fn pool(&self) -> Pool {
        let mut wipe_swept = 0u8;
        if self.has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT) {
            wipe_swept |= WIPE_TRADER_SWEPT;
        }
        if self.has_flag(MARKET_FLAG_WIPE_REWARDS_CLAIMED) {
            wipe_swept |= WIPE_REWARDS_CLAIMED;
        }
        Pool {
            total_shares: self.total_shares.get(),
            acc: self.acc.get(),
            carry: self.carry.get(),
            acc_r: self.acc_r.get(),
            carry_r: self.carry_r.get(),
            dust: self.dust.get(),
            tokens_free: self.tokens_free,
            borrowed_q: self.borrowed_q,
            receivable: self.receivable,
            max_release: self.max_release,
            vest_bal: self.vest_bal,
            vest_last: self.vest_last,
            vest_end: self.vest_end,
            share_epoch: self.share_epoch,
            wipe_live: u8::from(self.has_flag(MARKET_FLAG_WIPE_LIVE)),
            wipe_swept,
            _pad: [0; 2],
        }
    }

    /// Stores `shortwin_math::pool::Pool` back (the inverse of [`Market::pool`]).
    pub fn set_pool(&mut self, p: &Pool) {
        self.total_shares.set(p.total_shares);
        self.acc.set(p.acc);
        self.carry.set(p.carry);
        self.acc_r.set(p.acc_r);
        self.carry_r.set(p.carry_r);
        self.dust.set(p.dust);
        self.tokens_free = p.tokens_free;
        self.borrowed_q = p.borrowed_q;
        self.receivable = p.receivable;
        self.max_release = p.max_release;
        self.vest_bal = p.vest_bal;
        self.vest_last = p.vest_last;
        self.vest_end = p.vest_end;
        self.share_epoch = p.share_epoch;
        self.set_flag(MARKET_FLAG_WIPE_LIVE, p.wipe_live != 0);
        self.set_flag(
            MARKET_FLAG_WIPE_TRADER_SWEPT,
            p.wipe_swept & WIPE_TRADER_SWEPT != 0,
        );
        self.set_flag(
            MARKET_FLAG_WIPE_REWARDS_CLAIMED,
            p.wipe_swept & WIPE_REWARDS_CLAIMED != 0,
        );
    }

    /// Rent basis indices.
    pub fn basis(&self) -> Basis {
        Basis {
            j: get_all(&self.rent_basis),
        }
    }

    /// Stores the rent basis indices.
    pub fn set_basis(&mut self, b: &Basis) {
        set_all(&mut self.rent_basis, &b.j);
    }

    /// Rent aggregate `A_j`.
    pub fn aggr(&self) -> Aggr {
        Aggr {
            a: get_all(&self.rent_aggr),
        }
    }

    /// Stores the rent aggregate.
    pub fn set_aggr(&mut self, a: &Aggr) {
        set_all(&mut self.rent_aggr, &a.a);
    }

    /// The market-level EMA parameters (ARCH §2.3.4 "market-level params"; τ is the constant
    /// `EMA_TAU_S`).
    pub fn ema_params(&self) -> EmaParams {
        EmaParams::new(
            self.ema_dtmax_s,
            self.ema_clamp_bps,
            self.liq_slow_tau_s,
            self.liq_slow_dtmax_s,
        )
    }

    /// The observation state (bucket, last completed bucket, EMA, ring) as
    /// `shortwin_math::observe::Observation`, on the heap (1,376 B). Field by field; the ring
    /// keeps its slot order (`ring_head`, `ring_count`).
    pub fn observation(&self) -> Box<Observation> {
        let mut o = Box::<Observation>::default();
        o.bucket = Bucket {
            lo: self.b_lo.get(),
            hi: self.b_hi.get(),
            llo: self.b_llo.get(),
            lhi: self.b_lhi.get(),
            sec: self.b_sec,
            _pad: [0; 8],
        };
        o.last = LastBucket {
            lo: self.lb_lo.get(),
            hi: self.lb_hi.get(),
            sec: self.lb_sec,
            _pad: [0; 8],
        };
        o.ema = Ema {
            price: self.ema_price.get(),
            liq: self.ema_liq.get(),
            liq_slow: self.liq_slow.get(),
            ts: self.ema_ts,
            slow_ts: self.slow_ts,
            count: self.ema_count,
            warm: self.ema_warm,
            guard: self.ema_guard,
            _pad: [0; 6],
        };
        o.ring = self.ring_math();
        o
    }

    /// The ring alone (`shortwin_math::observe::Ring`).
    pub fn ring_math(&self) -> Ring {
        let mut r = Ring::default();
        for (m, e) in r.entries.iter_mut().zip(self.ring.iter()) {
            *m = MathRingEntry {
                p: e.p.get(),
                l: e.l.get(),
                x: e.x.get(),
                y: e.y.get(),
                lo: e.lo.get(),
                hi: e.hi.get(),
                llo: e.llo.get(),
                lhi: e.lhi.get(),
                ts: e.ts,
                _pad: [0; 8],
            };
        }
        r.head = self.ring_head;
        r.len = self.ring_count;
        r
    }

    /// Stores the observation state back (the inverse of [`Market::observation`]).
    pub fn set_observation(&mut self, o: &Observation) {
        self.b_lo.set(o.bucket.lo);
        self.b_hi.set(o.bucket.hi);
        self.b_llo.set(o.bucket.llo);
        self.b_lhi.set(o.bucket.lhi);
        self.b_sec = o.bucket.sec;
        self.lb_lo.set(o.last.lo);
        self.lb_hi.set(o.last.hi);
        self.lb_sec = o.last.sec;
        self.ema_price.set(o.ema.price);
        self.ema_liq.set(o.ema.liq);
        self.liq_slow.set(o.ema.liq_slow);
        self.ema_ts = o.ema.ts;
        self.slow_ts = o.ema.slow_ts;
        self.ema_count = o.ema.count;
        self.ema_warm = o.ema.warm;
        self.ema_guard = o.ema.guard;
        for (e, m) in self.ring.iter_mut().zip(o.ring.entries.iter()) {
            e.p.set(m.p);
            e.l.set(m.l);
            e.x.set(m.x);
            e.y.set(m.y);
            e.lo.set(m.lo);
            e.hi.set(m.hi);
            e.llo.set(m.llo);
            e.lhi.set(m.lhi);
            e.ts = m.ts;
        }
        self.ring_head = o.ring.head;
        self.ring_count = o.ring.len;
    }

    /// The newest ring entry's cached reprojection `(x, y)` (ARCH §3.4 step 9, the D4 EMA
    /// leg), `(0, 0)` without an entry (a quote on `x = 0` is undefined).
    pub fn newest_ring_xy(&self) -> (u128, u128) {
        let n = usize::from(self.ring_count).min(RING_LEN);
        if n == 0 {
            return (0, 0);
        }
        self.ring
            .get(usize::from(self.ring_head))
            .map_or((0, 0), |e| (e.x.get(), e.y.get()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    /// OWNER-M6D-1 / M6E-1 (ARCH changelog 57, ECON #106): PumpSwap credits its burned LP's `D`
    /// at `λ_min` = 100 % whatever the governed `pool_lock_min_bps` is; Raydium divides `D` by
    /// `pool_lock_min_bps`; venues without a locked depth have no `λ_min`.
    #[test]
    fn lock_min_bps_is_100_percent_on_pumpswap_pool_lock_min_bps_on_raydium() {
        for lam in [2_000u32, 5_000, 10_000] {
            assert_eq!(VenueKind::PumpSwap.lock_min_bps(lam), Some(10_000));
            for k in [VenueKind::RaydiumV4, VenueKind::RaydiumCpmm] {
                assert_eq!(k.lock_min_bps(lam), Some(lam));
            }
            for k in [
                VenueKind::PumpCurve,
                VenueKind::PumpMigrating,
                VenueKind::None,
            ] {
                assert_eq!(k.lock_min_bps(lam), None);
            }
            #[cfg(feature = "test-venue")]
            assert_eq!(VenueKind::Mock.lock_min_bps(lam), None);
        }
        for k in [
            VenueKind::None,
            VenueKind::PumpCurve,
            VenueKind::PumpMigrating,
            VenueKind::PumpSwap,
            VenueKind::RaydiumV4,
            VenueKind::RaydiumCpmm,
        ] {
            assert_eq!(
                k.lock_min_bps(5_000).is_some(),
                k.has_locked_depth(),
                "{k:?}"
            );
        }
    }

    /// `is_pda` re-derives `["market", mint]` with the stored bump: the real PDA passes, any
    /// other key or bump does not (M1-SEC-6).
    #[test]
    fn is_pda_checks_key_and_bump() {
        let mint = Pubkey::new_unique();
        let (pda, bump) = Pubkey::find_program_address(&[Market::SEED, mint.as_ref()], &crate::ID);
        let mut m = Market::zeroed();
        m.mint = mint;
        m.bump = bump;
        assert!(m.is_pda(&pda));
        assert!(!m.is_pda(&Pubkey::new_unique()));
        m.bump = bump.wrapping_sub(1);
        assert!(!m.is_pda(&pda));
        m.bump = bump;
        m.mint = Pubkey::new_unique();
        assert!(!m.is_pda(&pda));
    }
}
