//! `propose_pool(candidate_kind)` (ARCH §2.5 `None`, §2.6, §3.9; D1, D2, decision 7; A42, A60,
//! A71, A72; the M4 security review): anyone proposes a Raydium SOL pool for a market. The
//! rules are `crate::engine::pool_select` (module table there): a list-only (`None`) market
//! with no open position adopts any eligible candidate, one with open positions collects them
//! in a window (M4-SEC-4); a Raydium market adopts one immediately only while it has no open
//! position, with `pool_list_margin_bps` (5 %) inside its listing window and
//! `pool_switch_margin_bps` (25 %) after it; otherwise, once its incumbent has failed
//! `pool_fail` for `pool_fast_route_s`, the proposal opens or improves the fast path's
//! collection window (improvements only while the chain that armed it holds, M4-SEC-2), which
//! `execute_pool_switch` resolves.
//!
//! **Accounts:** `config`, `venue_guard` (read; below), `market` (mut), `mint`, `oracle_cache`
//! (read: the tier of a window candidate, step 6), event accounts. No signer: anyone may call it
//! (the transaction's fee payer pays).
//!
//! **Remaining** (every slot mandatory and key-checked, ARCH §3.0):
//!
//! - a list-only market: the detection set `[bonding_curve, canonical_pool]` (the market's
//!   stored keys), then the candidate's ReadSet;
//! - a Raydium market: the incumbent's ReadSet (the market's pool cache, as `crank_price`
//!   takes it), then the candidate's ReadSet.
//!
//! The candidate's ReadSet is the adapter's (`candidate_kind` = 4: AMM v4 `[pool, coin vault, pc
//! vault, LP mint]`; 5: CPMM `[pool, amm_config, token 0 vault, token 1 vault, LP mint]`),
//! verified exactly as a Raydium listing verifies it (`venues::raydium_v4::verify`,
//! `venues::raydium_cpmm::verify`: owner, length, status, nonce, the WSOL and token sides and
//! their programs, the vaults, config and LP mint the pool names, then `eligible()` —
//! executable, fee ≤ `venue_fee_cap_bps`, and the locked-depth floor
//! `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` at the market's `P_ema` (the
//! candidate's spot on a market that never had a price): nothing a single transaction can move
//! for the length of the proposal, and nothing an unlocked deposit can hold down (M4-SEC-1,
//! M4-SEC-3; v0.4.1's live `R` and `λ` clauses are gone).
//!
//! **Order:**
//!
//! 1. `mint.owner == market.token_program` (`AccountMismatch`); `candidate_kind` is a Raydium
//!    kind (`NoSupportedVenue`).
//! 2. The market's venue: a pump venue refuses (`PumpVenuePrecedence`, D1: ARCH §2.5 "create_market
//!    and propose_pool reject Raydium candidates for such markets"); a list-only market runs
//!    `classify_pump` on its detection set and refuses a live curve, a migrating curve, a
//!    valid canonical PumpSwap pool or an account at the canonical-pool address that does not
//!    read as a valid pool (M5-SEC-5: fail closed) the same way (a `None` market of a pump coin moves to its
//!    pump venue through `crank_price`'s re-detection, M5); a curve that graduated elsewhere
//!    proceeds and sets `graduated_elsewhere`. The test-venue mock refuses (`NoSupportedVenue`).
//! 3. The venue-upgrade latch (`VenueUpgraded`): no proposal of a candidate while an upgrade of
//!    **its own** program awaits the Emergency review (M4-SEC-8; `guard::candidate_latch`): the
//!    candidate is verified with that program's compiled reader and an adoption fixes the pool,
//!    its SOL side and (on a list-only market) `graduated_at`. An upgrade of the other Raydium
//!    program — typically the incumbent's, whose pools then fail `pool_fail` — does not hold up
//!    the fast path. **Deviation** (flagged): ARCH §3.9 names no latch and no `venue_guard` for
//!    `propose_pool`; a listing keeps the whole Raydium mask (M3-SEC-2).
//! 4. The prologue (ARCH §3.0 steps 2, 4–8) on the incumbent: accrue, roll, record the
//!    incumbent's observation if it is executable, the S3 and `pool_fail` chains, `pool_d`
//!    refreshed from an executable incumbent (ARCH §2.3.4). Like `crank_price` it carries no
//!    `MarketVault`: the accrual's vault side runs on a scratch copy, which must stay untouched.
//! 5. The candidate verified (above) at `P_ref = pool_select::reference_price` (the post-roll
//!    `P_ema`); the incumbent's own pool is not a candidate (`PoolNotDominant`).
//! 6. `pool_select::decide`, then the adoption (`pool_select::adopt`: ring rewrite to the new
//!    pool's anchor `⌊D·10⁴/λ_min⌋` and the post-change restart marks, M4R; chains and window
//!    cleared, age, listing window; event `PoolSwitched`) or the window
//!    (`pool_select::record_window`; event `PoolProposed`, kind fast). A window's own candidate
//!    cannot improve on itself (`PoolNotDominant`): an improvement must be another, strictly
//!    deeper pool. A window's candidate also needs the **window floor** — its locked SOL alone
//!    `≥ pool_min_real_sol`, no `/λ_min` (`pool_select::window_floor_ok`, else `PoolTooThin`;
//!    M4V-1): its adoption moves open positions, which the SOL floor exists to keep off thin
//!    pools. A proposal judges it at `min(P_ema, P_cand)` (`pool_select::window_price`, M4R-2,
//!    **PROPOSED**: `P_ema` is frozen while the incumbent cannot execute, and after a dump a
//!    pool far under the floor passed at it); the execution again at `P_ema`. A proposer's dump
//!    around its own proposal only hurts that proposal. And **switch only if all fit** (owner
//!    decision of 2026-09-29; `pool_select::carried_fit`): every open position — bounded in O(1)
//!    by `min(max_q_hwm, ΣB_Q)` — fits the candidate's per-position impact and fee-scaled size caps
//!    on its conservative locked-depth `X₀`, and `ΣB_Q` its per-market impact and fee-scaled OI
//!    caps (the fee-scaled caps at `min(min_open_fee_ppm, φ_cand, CARRIED_FEE_CAP_PPM)`, the fee
//!    the positions were margined at if lower, and never above 25 bps; FIT-2, FITS-1), on the
//!    tier the market would have on it, at `min(P_ema,
//!    P_cand)` and at `P_ema` (a
//!    dump around the proposal lowers `P_cand` and would loosen `X₀`), with the candidate's live
//!    token reserve as an open reads it (`FitAt::Proposal`: the proposer's own transaction), else
//!    `CarriedPositionsTooLarge` (a window with no open position fits trivially).
//!
//! **Errors:** `AccountMismatch`, `BadRemainingAccounts`, `VenueAccountMismatch`,
//! `NoSupportedVenue`, `PumpVenuePrecedence` (and `classify_pump`'s `CurveMayhemUnsupported`,
//! `CurveQuoteNotSol`, `CurveNeedsExtend`, `CurveCashbackCoin` — a live cashback curve on a
//! list-only market), `VenueUpgraded`, `PoolIneligible`, `PoolTooThin`,
//! `PoolNotDominant`, `PositionsOpen`, `PoolWindowClosed`, `CarriedPositionsTooLarge`.

use anchor_lang::prelude::*;
use shortwin_math::pool::Vault;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::guard;
use crate::engine::pool_select::{self, FitAt, Proposal, SelectParams, Situation};
use crate::engine::prologue;
use crate::errors::ShortwinError;
use crate::events::{PoolProposed, PoolSwitched, POOL_KIND_FAST};
use crate::state::{
    raydium_cache, GlobalConfig, Market, OracleCache, VenueGuard, VenueKind,
    MARKET_FLAG_GRADUATED_ELSEWHERE,
};
use crate::venues::detect::{self, PumpClass};
use crate::venues::{self, VenueState};

#[event_cpi]
#[derive(Accounts)]
pub struct ProposePool<'info> {
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// The venue-upgrade latch (A7; module docs step 3): a proposal is refused while the bit of
    /// the candidate's program is set (`VenueUpgraded`, M4-SEC-8). Read only.
    #[account(seeds = [VenueGuard::SEED], bump = venue_guard.load()?.bump)]
    pub venue_guard: AccountLoader<'info, VenueGuard>,
    #[account(
        mut,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump = market.load()?.bump,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = mint @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    /// CHECK: `== market.mint` (`has_one`) and owned by the market's token program (checked in
    /// the handler); the candidate's token side must be this mint under that program.
    pub mint: UncheckedAccount<'info>,
    /// The cached SOL/USD price: the tier the market would have on a window candidate, whose
    /// opening caps its open positions must fit ("switch only if all fit"; module docs step 6).
    /// Read only.
    #[account(seeds = [OracleCache::SEED], bump = oracle_cache.bump)]
    pub oracle_cache: Box<Account<'info, OracleCache>>,
}

/// The candidate kind of `propose_pool`'s argument: AMM v4 or CPMM (`NoSupportedVenue`
/// otherwise).
pub fn candidate_kind_of(v: u8) -> Result<VenueKind> {
    match VenueKind::from_u8(v) {
        Some(k @ (VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm)) => Ok(k),
        _ => err!(ShortwinError::NoSupportedVenue),
    }
}

/// What the market's venue contributes to a proposal (or an execution): the incumbent's state
/// for the prologue, the candidate's accounts, and on a list-only market the `classify_pump`
/// outcome of its detection set (whether the curve graduated elsewhere; whether a pump venue
/// now exists, which refuses every Raydium candidate, D1).
pub(crate) struct Incumbent<'a, 'info> {
    pub(crate) state: VenueState,
    pub(crate) candidate: &'a [AccountInfo<'info>],
    pub(crate) graduated_elsewhere: bool,
    /// A list-only market's detection set shows a pump venue (D1): `propose_pool` refuses
    /// (`PumpVenuePrecedence`), `execute_pool_switch` cancels.
    pub(crate) pump_venue: bool,
    /// The incumbent pool (`Pubkey::default()` on a list-only market).
    pub(crate) pool: Pubkey,
}

/// Splits `remaining` by the market's venue (module docs, "Remaining") and classifies a
/// list-only market's detection set. A pump venue market is refused (`PumpVenuePrecedence`).
pub(crate) fn incumbent<'a, 'info>(
    market: &Market,
    kind: VenueKind,
    remaining: &'a [AccountInfo<'info>],
    now: i64,
) -> Result<Incumbent<'a, 'info>> {
    match kind {
        VenueKind::None => {
            let (bc, cp, candidate) = match remaining {
                [bc, cp, rest @ ..] => (bc, cp, rest),
                _ => return err!(ShortwinError::BadRemainingAccounts),
            };
            require_keys_eq!(
                bc.key(),
                market.bonding_curve,
                ShortwinError::VenueAccountMismatch
            );
            require_keys_eq!(
                cp.key(),
                market.canonical_pool,
                ShortwinError::VenueAccountMismatch
            );
            let class = detect::classify_pump_with(
                bc,
                cp,
                &market.mint,
                &detect::pool_authority_of(market),
            )?;
            Ok(Incumbent {
                state: VenueState::not_executable(VenueKind::None),
                candidate,
                graduated_elsewhere: class == PumpClass::GraduatedElsewhere,
                pump_venue: class.pump_venue(),
                pool: Pubkey::default(),
            })
        }
        VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => {
            let rl = venues::read_len(kind)?;
            require!(remaining.len() >= rl, ShortwinError::BadRemainingAccounts);
            let (read, candidate) = remaining.split_at(rl);
            venues::check_read(market, kind, read)?;
            Ok(Incumbent {
                state: venues::read_state(market, kind, read, now),
                candidate,
                graduated_elsewhere: false,
                pump_venue: false,
                pool: market
                    .pool_cache
                    .get(raydium_cache::RAY_POOL)
                    .copied()
                    .unwrap_or_default(),
            })
        }
        VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap => {
            err!(ShortwinError::PumpVenuePrecedence)
        }
        #[allow(unreachable_patterns)]
        _ => err!(ShortwinError::NoSupportedVenue),
    }
}

pub fn handler<'info>(ctx: Context<'info, ProposePool<'info>>, candidate_kind: u8) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let config = ctx.accounts.config.load()?;
    let mut market = ctx.accounts.market.load_mut()?;
    require_keys_eq!(
        *ctx.accounts.mint.owner,
        market.token_program,
        ShortwinError::AccountMismatch
    );
    let cand_kind = candidate_kind_of(candidate_kind)?;
    let kind = market.venue_kind().ok_or(ShortwinError::NoSupportedVenue)?;
    let inc = incumbent(&market, kind, ctx.remaining_accounts, clock.unix_timestamp)?;
    require!(!inc.pump_venue, ShortwinError::PumpVenuePrecedence);
    guard::candidate_latch(&*ctx.accounts.venue_guard.load()?, cand_kind)?;

    let mut scratch = Box::new(Vault::new());
    let pro = prologue::run(
        &config,
        &market_key,
        &mut market,
        &mut scratch,
        Some(&inc.state),
        clock.unix_timestamp,
        clock.slot,
    )?;
    require!(
        scratch.lender_sol == 0 && scratch.protocol_fees == 0 && scratch.unmatched == 0,
        ShortwinError::MathOverflow
    );

    let p_ref = pool_select::reference_price(&market);
    let adoption = pool_select::verify_candidate(
        cand_kind,
        inc.candidate,
        &market.mint,
        &market.token_program,
        &config.params,
        clock.unix_timestamp,
        p_ref,
    )?;
    require_keys_neq!(adoption.pool, inc.pool, ShortwinError::PoolNotDominant);
    let p = SelectParams::of(&config.params);
    let d_cand = adoption.state.locked_depth;
    let d_inc = market.pool_d.get();
    let situation = Situation::of(&market, kind, pro.now, pro.pool_fail);
    let base = (market_key, clock.slot, clock.unix_timestamp, adoption.pool);
    match pool_select::decide(&situation, d_cand, &p)? {
        Proposal::Adopt(switch_kind) => {
            pool_select::adopt(
                &mut market,
                &adoption,
                pro.now,
                p.list_window_s,
                config.params.pool_lock_min_bps,
            )?;
            if inc.graduated_elsewhere {
                market.set_flag(MARKET_FLAG_GRADUATED_ELSEWHERE, true);
            }
            let event = PoolSwitched {
                market: base.0,
                slot: base.1,
                ts: base.2,
                candidate: base.3,
                kind: switch_kind,
                d_cand,
                d_inc,
                window_ends: pool_select::listing_window_ends(
                    market.pool_seen_at,
                    p.list_window_s,
                )?,
            };
            drop(market);
            drop(config);
            emit_cpi!(event);
        }
        decision @ (Proposal::OpenWindow | Proposal::ImproveWindow) => {
            let open = decision == Proposal::OpenWindow;
            // An improvement must be another, strictly deeper pool (the window's own candidate's
            // `D` grows with every trade's fee).
            require!(
                open || market.fp_keys.first() != Some(&adoption.pool),
                ShortwinError::PoolNotDominant
            );
            // M4V-1: the window's adoption moves open positions, so its candidate's locked SOL
            // alone must reach the SOL floor (the listing floor's `/λ_min` admitted 12.5 SOL);
            // M4R-2: judged at `min(P_ema, P_cand)`, so a dump during the incumbent's halt does
            // not admit a pool the stale-high `P_ema` over-values.
            require!(
                pool_select::window_floor_ok(
                    &adoption,
                    &config.params,
                    pool_select::window_price(p_ref, &adoption)
                ),
                ShortwinError::PoolTooThin
            );
            // Switch only if all fit (owner decision of 2026-09-29): every open position and the
            // market's OI fit the candidate's normal opening caps, at `min(P_ema, P_cand)` and at
            // `P_ema`, on the tier the market would have on it.
            let fit = pool_select::FitContext::of(&config, &ctx.accounts.oracle_cache, pro.now);
            require!(
                pool_select::carried_fit(&market, &adoption, &fit, p_ref, FitAt::Proposal)?.fits(),
                ShortwinError::CarriedPositionsTooLarge
            );
            pool_select::record_window(&mut market, &adoption, pro.now, open);
            let event = PoolProposed {
                market: base.0,
                slot: base.1,
                ts: base.2,
                candidate: base.3,
                kind: POOL_KIND_FAST,
                d_cand,
                d_inc,
                window_ends: pool_select::window_ends(market.fp_t0, p.fast_window_s)?,
            };
            drop(market);
            drop(config);
            emit_cpi!(event);
        }
    }
    Ok(())
}
