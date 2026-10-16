//! `crank_price()` (ARCH §3.9): anyone runs a market's prologue — accrue (vest release, rent),
//! the observation roll, the venue sync, the record (only while the venue is executable), the
//! route chains — and refreshes the cached tier. The in-house keeper sends one per second near
//! thresholds and batches the rest (ARCH §7.1; ECON §5.8).
//!
//! **Accounts:** `config`, `market` (mut), `mint`, `oracle_cache` (read), event accounts.
//! **Remaining:** the venue's `ReadSet`; for a list-only (`None`) market the detection set
//! `[bonding_curve, canonical_pool]` (key-checked); a `PumpMigrating` market its curve's ReadSet.
//!
//! **Venue transitions** (ARCH §2.5): the prologue's venue sync moves a curve market to
//! `PumpMigrating` when its curve completes, to `PumpSwap` when its canonical pool is valid (M6:
//! the ring rewrite and the post-change warm-up, `venues::pump_amm::graduate`) and to `None` when
//! it completed with zeroed reserves and no canonical pool (`venues::pump_curve::sync`); on a
//! list-only market this instruction
//! re-runs `classify_pump` on the detection set and moves it onto a pump venue it finds (M5: a
//! live curve → `PumpCurve`, a migrating one → `PumpMigrating`; M6: a valid canonical pool →
//! `PumpSwap`, `redetect_pool`): the curve cache is derived, the price state
//! starts at the curve's spot (or, on a market that already has one, its liquidity is rewritten
//! to the curve's, A72), the route chains restart, the tier is D, and a Raydium collection window
//! is cleared (`fp_*`; M4's carry-over: pump venues take precedence, D1). A detection that cannot
//! classify (a mayhem or non-SOL curve, a curve account that does not parse) leaves the market
//! as it is: a crank never reverts on venue state. The trader's pump UVA and `trader_base_ata`
//! are not created here (no payer): opens refuse `VenueAccountMismatch` / `TraderStateInvalid`
//! until someone creates them (both are permissionless). A Raydium pool is adopted through
//! `propose_pool` (M4), never here.
//!
//! **Never reverts on a venue-state account that no longer parses** (A63, V1-8): a correctly
//! keyed account whose owner, discriminator or layout changed makes the venue "not
//! executable" — no observation is recorded and the S3 chain runs. Only a wrong or missing key
//! reverts (`VenueAccountMismatch`, `BadRemainingAccounts`), so a batch of cranks fails only on
//! wrong keys.
//!
//! **The PumpSwap coin-creator cache** (M6-SEC-4): on a `PumpSwap` market whose pool names another
//! coin creator than the market's cache (anyone can change it through pAMM), the cache is rewritten
//! to it (`venues::pump_amm::refresh_coin_creator`), so trades stop re-deriving the creator vault.
//!
//! **No `MarketVault`** (ARCH §3.9's list): the accrual's vault side is run on a scratch copy
//! and must leave it untouched. It always does: the accrual touches the vault only to send a
//! credit to the protocol while `S = 0`, and `S = 0 ⇒ ΣB_Q = 0 ⇒ A_j = 0` (M1-SEC-3,
//! `open_short`'s `S > 0` rule), so the rent credit is 0; the vest never runs with `S = 0`
//! (`pool::release` refuses it as an invariant break).
//!
//! **Events:** `PriceCranked`, plus `RouteChainChanged` when a chain started or cleared and
//! `VenueStateChanged` on a venue transition.

use anchor_lang::prelude::*;
use shortwin_math::pool::Vault;
use shortwin_math::rent;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::{observe, pool_select, prologue, tier as engine_tier};
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::{PriceCranked, RouteChainChanged};
use crate::state::{GlobalConfig, Market, OracleCache, VenueKind};
use crate::venues::detect::{self, PumpClass};
use crate::venues::pump_curve::{self, Transition};
use crate::venues::{self, Carry};

#[event_cpi]
#[derive(Accounts)]
pub struct CrankPrice<'info> {
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump = market.load()?.bump,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = mint @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    /// CHECK: `== market.mint` (`has_one`) and owned by the market's token program (checked
    /// in the handler); PumpSwap's fee tier reads its supply (M6).
    pub mint: UncheckedAccount<'info>,
    #[account(seeds = [OracleCache::SEED], bump = oracle_cache.bump)]
    pub oracle_cache: Box<Account<'info, OracleCache>>,
}

pub fn handler<'info>(ctx: Context<'info, CrankPrice<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let config = ctx.accounts.config.load()?;
    let mut market = ctx.accounts.market.load_mut()?;
    require_keys_eq!(
        *ctx.accounts.mint.owner,
        market.token_program,
        ShortwinError::AccountMismatch
    );
    let kind = market.venue_kind().ok_or(ShortwinError::NoSupportedVenue)?;
    let rem = venues::parse(&market, kind, ctx.remaining_accounts, Carry::ReadSet, &[])?;
    let state = venues::read_state_boxed(
        &market,
        kind,
        rem.read,
        clock.unix_timestamp,
        venues::mint_supply(&ctx.accounts.mint.to_account_info()),
    );

    let mut scratch = Box::new(Vault::new());
    let pro = prologue::run(
        &config,
        &market_key,
        &mut market,
        &mut scratch,
        Some(&*state),
        clock.unix_timestamp,
        clock.slot,
    )?;
    require!(
        scratch.lender_sol == 0 && scratch.protocol_fees == 0 && scratch.unmatched == 0,
        ShortwinError::MathOverflow
    );
    let mut transition = pro.transition;
    if kind == VenueKind::None {
        if let [bc, cp] = rem.read {
            transition = redetect(&mut market, bc, cp, pro.now)?.or(transition);
        }
    }
    // M6-SEC-4: anyone can change a PumpSwap pool's coin creator (pAMM `set_coin_creator`,
    // `migrate_pool_coin_creator`); the cache follows it here, so trades stop re-deriving T11 / T12.
    if kind == VenueKind::PumpSwap && market.venue_kind() == Some(VenueKind::PumpSwap) {
        venues::pump_amm::refresh_coin_creator(&mut market, rem.read)?;
    }

    if let Some(tier) = engine_tier::assign(
        &config,
        &market,
        &state,
        &ctx.accounts.oracle_cache,
        pro.now,
    ) {
        if state.executable {
            market.tier = tier as u8;
            market.tier_computed_at = pro.now;
        }
    }
    let util_bps = rent::utilization_bps(market.tokens_free, market.borrowed_q).math()?;
    let event = PriceCranked {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        spot_wad: state.price().unwrap_or(0),
        ema_price: market.ema_price.get(),
        ema_liq: market.ema_liq.get(),
        liq_slow: market.liq_slow.get(),
        ema_count: market.ema_count,
        lb_lo: market.lb_lo.get(),
        lb_hi: market.lb_hi.get(),
        tier: market.tier,
        util_bps,
        rent_basis: market.basis().j,
        vest_bal: market.vest_bal,
    };
    let chains = RouteChainChanged {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        no_route_since: market.no_route_since,
        last_no_route_obs: market.last_no_route_obs,
        pool_fail_since: market.pool_fail_since,
        last_pool_fail_obs: market.last_pool_fail_obs,
    };
    let changed = transition.map(|t| pump_curve::transition_event(&market, t, market_key, &clock));
    drop(market);
    drop(config);
    if let Some(ev) = changed {
        emit_cpi!(ev);
    }
    emit_cpi!(event);
    if pro.chains_changed {
        emit_cpi!(chains);
    }
    Ok(())
}

/// ARCH §2.5 `None` row, M5 part (module docs): `classify_pump` on the key-checked detection set
/// and, for a live or migrating curve, the move onto it. Returns the transition, if any; a
/// detection that cannot classify changes nothing (never a revert).
#[inline(never)]
fn redetect(
    market: &mut Market,
    bc: &AccountInfo,
    cp: &AccountInfo,
    now: i64,
) -> Result<Option<Transition>> {
    let authority = detect::pool_authority_of(market);
    let Ok(class) = detect::classify_pump_with(bc, cp, &market.mint, &authority) else {
        return Ok(None);
    };
    let to = match class {
        PumpClass::PumpCurve => VenueKind::PumpCurve,
        PumpClass::PumpMigrating => VenueKind::PumpMigrating,
        PumpClass::PumpSwap => return redetect_pool(market, cp, now),
        // A canonical-pool account that does not read as a valid pool (M5-SEC-5) is a pump
        // venue short.win cannot read: the market stays list-only, and `propose_pool` /
        // `execute_pool_switch` keep Raydium out (D1).
        PumpClass::PumpPoolUnreadable | PumpClass::GraduatedElsewhere | PumpClass::NotPump => {
            return Ok(None)
        }
    };
    let Some(curve) = pump_curve::parse_curve(bc) else {
        return Ok(None);
    };
    let Some((p, l)) = pump_curve::spot(&curve) else {
        return Ok(None);
    };
    // The curve cache only (M5): the PumpSwap cache a curve listing precomputes is left to this
    // market's `→ PumpSwap` transition (`pump_amm::graduate` derives what is still empty), so the
    // re-detection stays inside ARCH §6.1's `crank_price` limit (with the precompute: 82,942 of
    // 60,000 CU on the `m5_cu` bench's re-detection).
    pump_curve::write_cache(market, &Pubkey::new_from_array(curve.creator));
    let priced = market.ring_count > 0 && market.ema_price.get() > 0;
    let mut obs = market.observation();
    if priced {
        observe::rewrite_liquidity(&mut obs, l, shortwin_math::observe::VenueChange::Graduation)?;
    } else {
        *obs = shortwin_math::observe::Observation::new(now, p, l).math()?;
    }
    market.set_observation(&obs);
    market.venue = to as u8;
    if to == VenueKind::PumpMigrating {
        market.migration_seen_at = now;
    }
    market.tier = shortwin_math::tier::Tier::D as u8;
    market.tier_computed_at = now;
    market.no_route_since = 0;
    market.last_no_route_obs = 0;
    market.pool_fail_since = 0;
    market.last_pool_fail_obs = 0;
    pool_select::clear_window(market);
    Ok(Some(Transition {
        from: VenueKind::None,
        to,
    }))
}

/// ARCH §2.5 `None` row, M6 part: a list-only market whose canonical PumpSwap pool is valid moves
/// onto it — the pool's cache (`pump_amm::graduate`: `graduated_at = now`, both route chains and
/// a collection window cleared, the tier C). A market without a price starts it at its first
/// PumpSwap observation (marked for the warm-up, `pump_amm::start_price`: the detection set
/// carries no vault); one with a price has its observation state rewritten to the pool's
/// `lp_supply` anchor and restarted, as a graduation does. The trader's base ATA and pAMM UVA are
/// not created here (no payer): trades refuse until someone creates them (both permissionless).
#[inline(never)]
fn redetect_pool(market: &mut Market, cp: &AccountInfo, now: i64) -> Result<Option<Transition>> {
    let authority = detect::pool_authority_of(market);
    let (_, Some(pool)) = detect::canonical_pool_read(cp, &market.mint, &authority) else {
        return Ok(None);
    };
    venues::pump_amm::graduate(market, &pool, now)?;
    let mut obs = market.observation();
    venues::pump_amm::restart_observation(&mut obs, venues::pump_amm::graduation_anchor(&pool))?;
    market.set_observation(&obs);
    Ok(Some(Transition {
        from: VenueKind::None,
        to: VenueKind::PumpSwap,
    }))
}
