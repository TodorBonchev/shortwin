//! Events (ARCH §3.10): one `emit_cpi!` per instruction (merging what v0.1 emitted separately,
//! to save instruction-trace entries). Two `emit!` log events (advisory, no CPI, no trace
//! entry): `CoverageGap`, and `MintSafetyTripped` on an exit path (an exit never fails on the
//! mint guard; `flag_unsafe_mint` emits it with `emit_cpi!`).
//!
//! Every event carries the common fields `market`, `slot`, `ts` (`market = Pubkey::default()`
//! for the global admin events). `initialize_config` emits nothing (no §3.10 event; its account
//! list is already at the legacy transaction size limit).

use anchor_lang::prelude::*;

// ---- markets and lenders ------------------------------------------------------------------

/// `create_market`.
#[event]
pub struct MarketCreated {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub venue: u8,
    pub creator: Pubkey,
    pub bonding_curve: Pubkey,
    pub canonical_pool: Pubkey,
    /// Raydium pool adopted at listing (`Pubkey::default()` otherwise).
    pub ray_pool: Pubkey,
    pub tier: u8,
    pub creation_fee: u64,
}

/// Declares the five lender events, which share one field list (ARCH §3.10).
macro_rules! lender_event {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[event]
        pub struct $name {
            pub market: Pubkey,
            pub slot: u64,
            pub ts: i64,
            pub owner: Pubkey,
            /// Tokens (deposit, withdraw) or lamports (cover).
            pub amount: u64,
            /// Shares minted or burned.
            pub shares: u128,
            /// `T` after the instruction.
            pub t_after: u64,
            /// `S` after the instruction.
            pub s_after: u128,
            /// SOL paid to the lender.
            pub sol_paid: u64,
            /// Of which drawn from the pot.
            pub pot_draw: u64,
            /// The lender's lock after the instruction.
            pub lock: u128,
            pub lock_until: i64,
            /// The lender's share epoch after the instruction.
            pub epoch: u32,
        }
    };
}

lender_event!(
    /// `deposit`.
    Deposit
);
lender_event!(
    /// `withdraw`.
    Withdraw
);
lender_event!(
    /// `claim_sol`.
    ClaimSol
);
lender_event!(
    /// `cover_deficit`.
    DeficitCovered
);
lender_event!(
    /// `sync_lender`.
    LenderSynced
);

// ---- positions (M2+) ----------------------------------------------------------------------

/// `open_short`.
#[event]
pub struct Open {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub borrower: Pubkey,
    pub nonce: u64,
    pub q: u64,
    pub s: u64,
    pub m: u64,
    pub v: u64,
    pub n_open: u64,
    pub fee_open: u64,
    pub tier: u8,
    pub entry_price: u128,
    pub spot_wad: u128,
    pub ema_wad: u128,
    pub b_spot: u64,
    pub b_ema: u64,
    pub deadline: i64,
    pub venue: u8,
    /// PumpSwap venue-side rent `r` (V1-13); 0 elsewhere.
    pub venue_rent: u64,
}

/// `close_short`. `b_live` (the execution quote the leg was bounded by, I13) and
/// `token_surplus` (tokens a venue delivered beyond `q`, kept in the pool vault as a
/// donation, A62, V2-10) are M2 additions to ARCH §3.10's field list.
#[event]
pub struct Close {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub borrower: Pubkey,
    pub q: u64,
    pub buyback_cost: u64,
    pub rent: u64,
    pub rent_protocol: u64,
    pub borrower_payout: u64,
    pub b_live: u64,
    pub token_surplus: u64,
}

/// `liquidate`. `cr_spot_bps` is the robust spot leg's CR (`CR(V_eff, B̂(Q; reprice(max(L_live,
/// L_ema'), P_rs)))` at the risk fee; FITS-2), `cr_ema_bps` the newest ring entry's (its cached
/// `(x, y)`);
/// `b_live` is the execution quote the leg was bounded by (I13). `b_live_risk` (the live
/// quote at the risk fee, the penalty's base with `b_ref`) and `max_in` (I11: `B ≤ max_in +
/// ert`) are M2 additions to ARCH §3.10's field list.
#[event]
pub struct Liquidate {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub keeper: Pubkey,
    pub cr_spot_bps: u64,
    pub cr_ema_bps: u64,
    pub b_live: u64,
    pub b_ref: u64,
    pub p_ref: u128,
    /// The fixed δ.
    pub delta_bps: u16,
    pub buyback_cost: u64,
    pub rent: u64,
    pub penalty: u64,
    pub penalty_lender: u64,
    pub penalty_protocol: u64,
    pub penalty_keeper: u64,
    pub keeper_topup: u64,
    pub borrower_payout: u64,
    pub expired: bool,
    pub token_surplus: u64,
    pub b_live_risk: u64,
    pub max_in: u64,
}

/// `expire`. `b_live` is the execution quote the leg was bounded by (I13). `b_live_risk` (the
/// fee base with `b_ref`), `max_in` (I11) and `token_surplus` (tokens a venue delivered beyond
/// `q`, A62, V2-10) are M2 additions to ARCH §3.10's field list.
#[event]
pub struct Expire {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub keeper: Pubkey,
    pub b_live: u64,
    pub b_ref: u64,
    pub pi_ref: u64,
    pub buyback_cost: u64,
    pub rent: u64,
    pub expiry_fee: u64,
    pub keeper_reward: u64,
    pub borrower_payout: u64,
    pub b_live_risk: u64,
    pub max_in: u64,
    pub token_surplus: u64,
}

/// `sol_settle` (an executing settlement). `vest_flushed` (AIR-07) is an addition to ARCH
/// §3.10's field list.
#[event]
pub struct SolSettle {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub keeper: Pubkey,
    /// 1 = S1, 2 = S2, 3 = S3.
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
    /// `now − ema_ts` (V2-5).
    pub ema_age_s: i64,
    /// The unreleased vest the settlement paid to the shares at once, its own lump and rent
    /// top-up included: it left at most 1/1,000 of the token claims it found, a full wipe
    /// included (AIR-07). Reported with or without an epoch roll (`EpochRolled.vest_flushed`
    /// repeats it); 0 when nothing was flushed.
    pub vest_flushed: u64,
}

/// `SettleDisarmed.reason` / `SettleArmed.reason`: refuted on the settlement box.
pub const SETTLE_REASON_REFUTED: u8 = 1;
/// Not liquidatable on the box.
pub const SETTLE_REASON_NOT_LIQUIDATABLE: u8 = 2;

/// `sol_settle` armed the clock.
#[event]
pub struct SettleArmed {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub s_count: u64,
    pub s_since: i64,
    pub reason: u8,
}

/// `sol_settle` disarmed the clock.
#[event]
pub struct SettleDisarmed {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub s_count: u64,
    pub s_since: i64,
    /// [`SETTLE_REASON_REFUTED`] or [`SETTLE_REASON_NOT_LIQUIDATABLE`].
    pub reason: u8,
}

/// A share-epoch roll.
#[event]
pub struct EpochRolled {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub epoch: u32,
    pub acc_end: i128,
    pub num: u8,
    /// `1,000^k`.
    pub den: u128,
    pub vest_flushed: u64,
}

/// `sweep_protocol_fees`.
#[event]
pub struct ProtocolFeesSwept {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub amount: u64,
    pub recipient: Pubkey,
}

/// `add_margin`.
#[event]
pub struct MarginAdded {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub position: Pubkey,
    pub amount: u64,
}

// ---- cranks and venues --------------------------------------------------------------------

/// `crank_price`.
#[event]
pub struct PriceCranked {
    pub market: Pubkey,
    pub slot: u64,
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
}

/// A venue state change (graduation, `None →` pump re-detection, …).
#[event]
pub struct VenueStateChanged {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub from: u8,
    pub to: u8,
    pub pool: Pubkey,
    pub graduated_at: i64,
}

/// A route chain (S3 or fast path) changed.
#[event]
pub struct RouteChainChanged {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub no_route_since: i64,
    pub last_no_route_obs: i64,
    pub pool_fail_since: i64,
    pub last_pool_fail_obs: i64,
}

/// Declares the three pool-selection events (ARCH §3.10).
macro_rules! pool_event {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[event]
        pub struct $name {
            pub market: Pubkey,
            pub slot: u64,
            pub ts: i64,
            /// The candidate pool (`PoolSwitched`: the pool now adopted).
            pub candidate: Pubkey,
            /// `POOL_KIND_*`: listing / listing window / zero-position / fast.
            pub kind: u8,
            /// The candidate's locked depth `D` (`PoolSwitchCancelled`: the best one seen).
            pub d_cand: u128,
            /// The incumbent's locked depth `pool_d` (0 for a list-only market).
            pub d_inc: u128,
            /// `PoolProposed` / `PoolSwitchCancelled`: the end of the fast-path collection
            /// window; `PoolSwitched`: the end of the listing window after the adoption (opens
            /// resume then).
            pub window_ends: i64,
        }
    };
}

pool_event!(
    /// `propose_pool` opened or improved a fast-path collection window (ARCH §2.6 rule 3).
    PoolProposed
);
pool_event!(
    /// A pool adoption (`propose_pool`: listing, listing window, zero-position;
    /// `execute_pool_switch`: fast path). ARCH §2.6 rule 4.
    PoolSwitched
);
pool_event!(
    /// A collection window closed without adoption (`execute_pool_switch`, ARCH §2.6 rule 3):
    /// the window's candidate is no longer eligible or no longer passes the window floor (its
    /// locked SOL at `P_ema`, M4V-1); on a Raydium market the `pool_fail` chain that armed the
    /// window broke — the incumbent no longer fails `pool_fail`, or it recovered and failed
    /// again (M4-SEC-2); on a list-only market its detection set now shows a pump venue (D1,
    /// M4-SEC-4); or the candidate's own Raydium program is flagged in the venue-upgrade latch
    /// (M4V-4: not adoptable now; the chain stays armed).
    PoolSwitchCancelled
);

/// `Pool*.kind` (ARCH §3.10 "listing / zero-position / fast", with the listing split in two):
/// a list-only (`None`) market adopts its first Raydium pool (ARCH §2.5 `None`, §2.6 rule 1).
pub const POOL_KIND_LISTING: u8 = 0;
/// A replacement inside the listing window, `pool_list_margin_bps` (ARCH §2.6 rule 1).
pub const POOL_KIND_LISTING_WINDOW: u8 = 1;
/// An adoption without a failure on a market with no open position,
/// `pool_switch_margin_bps` (ARCH §2.6 rule 2).
pub const POOL_KIND_ZERO_POSITION: u8 = 2;
/// The fast path on an incumbent failing `pool_fail` (ARCH §2.6 rule 3).
pub const POOL_KIND_FAST: u8 = 3;

/// `TraderSwept.to` / `VenueRewardsClaimed.to`: credited through `vest`.
pub const SWEEP_TO_VEST: u8 = 0;
/// Credited to the live full-wipe record.
pub const SWEEP_TO_WIPE_RECORD: u8 = 1;
/// Credited to `protocol_fees` (no lender and no live wipe).
pub const SWEEP_TO_PROTOCOL: u8 = 2;

/// Trader normalisation or `sweep_trader`.
#[event]
pub struct TraderSwept {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub lamports: u64,
    pub tokens: u64,
    /// `SWEEP_TO_*`.
    pub to: u8,
    /// `Market.flags` wipe bits after the sweep.
    pub wipe_bits: u8,
}

/// `claim_venue_rewards`.
#[event]
pub struct VenueRewardsClaimed {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub lamports: u64,
    /// `SWEEP_TO_*`.
    pub to: u8,
    pub wipe_bits: u8,
}

/// The mint failed the safety filter (`flag_unsafe_mint`, or an exit path's tripwire).
#[event]
pub struct MintSafetyTripped {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub mint: Pubkey,
}

/// `flag_venue_upgraded` (the latch set, or `flagged_slot` refreshed while set).
#[event]
pub struct VenueUpgradeFlagged {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    /// `VenueGuard` index.
    pub venue: u8,
    pub guard_slot: u64,
    pub programdata_slot: u64,
    pub flagged_at: i64,
}

/// `revalidate_venue`.
#[event]
pub struct VenueRevalidated {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    /// `VenueGuard` index.
    pub venue: u8,
    /// The previous `guard_slot`.
    pub guard_slot: u64,
    /// The ProgramData deploy slot now recorded.
    pub programdata_slot: u64,
    /// `flagged_at` of the flag being cleared (0 if the bit was not set).
    pub flagged_at: i64,
    pub expected_slot: u64,
}

/// A bucket integration found the EMA older than `COVERAGE_GAP_S` with open positions
/// (`emit!`, not `emit_cpi!`; A69).
#[event]
pub struct CoverageGap {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub gap_s: i64,
    pub open_positions: u32,
}

/// `CoveragePauseSet.reason`: the in-house keeper lost coverage of the market (an outage, a
/// keeper budget that can no longer cover it; ARCH §3.1 `KeeperCoverage`, O17). The only reason
/// accepted, from either signer (the coverage key, or the Emergency vault's clear).
pub const COVERAGE_REASON_KEEPER: u8 = 1;

/// `set_coverage_pause` (O17): `paused_coverage` set or cleared on `market` by `by` (the
/// `coverage_authority`, or — a clear only, OWNER-M7-3 — the Emergency vault `pause_authority`),
/// with the reason code ([`COVERAGE_REASON_KEEPER`]).
#[event]
pub struct CoveragePauseSet {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub paused: bool,
    pub reason: u8,
    pub by: Pubkey,
}

// ---- admin --------------------------------------------------------------------------------

/// `propose_config`.
#[event]
pub struct ConfigProposed {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    /// `ConfigChange` kind (variant index + 1).
    pub kind: u8,
    /// `sha256(kind ‖ payload)`.
    pub hash: [u8; 32],
    pub not_before: i64,
}

/// `apply_config`.
#[event]
pub struct ConfigApplied {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub kind: u8,
    pub hash: [u8; 32],
}

/// `cancel_config`.
#[event]
pub struct ConfigCancelled {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub kind: u8,
    pub hash: [u8; 32],
    /// Who cancelled (Governance or Emergency).
    pub by: Pubkey,
}

/// `PauseSet.scope` / `RouteBrokenSet.scope`: global (the config).
pub const SCOPE_GLOBAL: u8 = 0;
/// A market.
pub const SCOPE_MARKET: u8 = 1;

/// `set_pause`.
#[event]
pub struct PauseSet {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    /// [`SCOPE_GLOBAL`] or [`SCOPE_MARKET`].
    pub scope: u8,
    pub paused: bool,
    pub by: Pubkey,
}

/// `disable_venue`.
#[event]
pub struct VenueDisabled {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    /// The `venue_enabled` bit index cleared.
    pub bit: u8,
    /// `venue_enabled` after the change.
    pub venue_enabled: u32,
}

/// `set_route_broken` (O6; M2).
#[event]
pub struct RouteBrokenSet {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub scope: u8,
    /// `venue_enabled`-style bit index for the global scope.
    pub venue_bit: u8,
    pub broken: bool,
}

/// `set_venue_extras` (O6; M2).
#[event]
pub struct VenueExtrasSet {
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    pub venue: u8,
    pub ix: u8,
    pub version: u32,
    pub keys: [Pubkey; 4],
    /// The version of the kind's `stable` set after the write (M2-SEC-2): it changes when the
    /// displaced `current` had stood for `EXTRAS_STABLE_S` or Governance wrote.
    pub stable_version: u32,
}
