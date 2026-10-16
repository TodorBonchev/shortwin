//! `liquidate(extras)` (ARCH §3.6; ECON §5.5, §8.2, §8.3, §8.5; SPEC §5.1–§5.2, §6.3): a keeper
//! (anyone) buys back a CR-liquidatable position's `q` tokens with the position's SOL, within
//! the fixed exit bound, and splits what is left by the liquidation waterfall.
//!
//! **Signer:** `keeper` (anyone; receives the penalty's keeper share and the keeper-floor
//! top-up, A74). **Named accounts:** as `close_short`, plus `borrower` (mut, `==
//! position.borrower`; receives the position close, A18, and the exposure record's rent when
//! its `q_open` reaches 0). **Remaining:** `ReadSet ++ TradeExtras ++ Extras` (buy set).
//!
//! **Exits never check the venue guard, pause flags, the route-broken flag or the oracle**
//! (I10; V2-1): none of those accounts is passed, and the flag never enters a leg, a quote or
//! the bound. Every risk quote (the legs, `B̂_ref`, the penalty base) is taken at the risk fee
//! `min(φ_live, venue_fee_hard_cap_bps_pos)`; the execution quote `B̂_live` (what the venue
//! charges) at the live fee decides "buyback allowed" and the bound check.
//!
//! **Preconditions** (ARCH §3.6, evaluated in this order; ECON §5.5, §8.2, §8.3):
//!
//! 1. The prologue (ARCH §3.0; its observation is the pre-trade state, the decisions read the
//!    post-roll EMA, ring and last bucket: the one-second lag); the exit-path mint guard (a
//!    mint that fails the filter now is reported with `MintSafetyTripped`, never a revert).
//!    The venue trades (`NoRoute` for `None`, `VenueMigrating` while `PumpMigrating` — also when
//!    the prologue's venue sync just moved a curve market there) and is `venue_executable`
//!    (`NoRoute`). On a live curve a `q > rT` buyback is an ordinary exit (pump-final §6.1.7):
//!    `buy_v3` completes the curve and buys the rest from the would-be pool, and every quote
//!    prices that path (`B̂_v3`, owner D3); a buyback the curve and its would-be pool cannot
//!    deliver has no execution quote, so it fails precondition 4 (`UseSolSettle`, §6.7).
//!    **1b** (M4R, **PROPOSED**, owner item O18): the post-change warm-up is over, `ema_warm ==
//!    0` after the roll (`EmaWarmingUp`): a restart — a Raydium adoption, a listing, a `→
//!    PumpSwap` graduation or a PumpSwap price start — wrote an anchor, and the next
//!    `EMA_WARMUP_BUCKETS` integrations re-seed the EMA from the new pool; no exit rests on the
//!    anchor or a partial seed. With a keeper cranking every second, exits resume at adoption +
//!    9 s (after a graduation 8–12 s after the migration, accepted by the owner on 2026-09-30,
//!    OWNER-M6R-4).
//! 2. A fresh completed bucket, `lb_sec ≥ now − ref_max_age_s` (locked), else
//!    `ReferenceStale` (the keeper cranks one second and acts the next, ECON §5.4).
//! 3. `liquidatable := (spot leg ∧ EMA leg) ∨ now ≥ deadline`, else `NotLiquidatable`: the
//!    spot leg `CR(V_eff, B̂(Q; reprice(max(L_live, L_ema'), P_rs))) < L` at the robust spot
//!    `P_rs = min(P_live, lb_lo)` — the live depth floored at the guarded EMA liquidity `L_ema'`
//!    (`min(L_ema, L_mlo)` while the post-change guard is set, else `L_ema`; FITS-2 (a),
//!    `shortwin_math::bounds::spot_liquidity`), so an LP's withdrawal in the same transaction
//!    cannot make it true (a low `L_ema` reached by a genuine held withdrawal, then frozen by
//!    withdraw-wrapped cranks, is read like any other: FITX-1, ECON §12.12 residual 10); the EMA
//!    leg on the newest `liq_persist_n` entries' cached `(x, y)` (decision 2), both through
//!    `shortwin_math::{risk, persist}`. [`assess`] also decides `expire`'s `UseLiquidate`: an
//!    expired position passes exactly one exit, and no withdrawal inside the transaction steers it.
//! 4. Normal buyback allowed, `B̂_live + ert ≤ V − K_s ∧ σ_live ≤ σ_max`, else `UseSolSettle`.
//! 5. Expired but not CR-liquidatable: `UseExpire` (expired **and** CR-liquidatable pays the
//!    penalty, ECON §8.4; the event says `expired`).
//! 6. The reference bound (v0.4.1, no reserve; `engine::bounds::{liq_reference,
//!    max_in_liq}`; `L_b` reads `min(L_ema, L_mlo)` while the post-change reference guard is set,
//!    M4R, and on Raydium and PumpSwap caps its EMA terms at `⌊pool_d·10⁴/λ_min⌋`, M4F-1,
//!    M6R-1): `max_in =
//!    min(⌊B̂_ref·(10⁴ + δ_liq)/10⁴⌋, V − K_s − ert)`; `B̂_live ≤
//!    max_in`, else `ExitBoundNotMet` (the position waits; an undefined reference quote waits
//!    too). Liveness (I11, I-E11): nothing else can refuse a liquidatable, buyback-allowed
//!    position with a fresh bucket once `B̂_live ≤ ⌊B̂_ref·(10⁴ + δ)/10⁴⌋`.
//!
//! **Execution:** trader normalisation (a donation is swept to lenders as a vesting lump,
//! `TraderSwept`), the buy leg with `max_in` (`B ≤ max_in + ert`, at least `q` tokens into
//! `pool_vault`, a surplus is a donation, A62), the waterfall `waterfall::liquidate` (`B` →
//! `r = min(R_due, V − B)` → penalty `π = min(⌈p·min(B̂_live,risk, B̂_ref)/10⁴⌉, V − B − r)`
//! split 50/20/30 (the lenders' half vests) → the keeper floor `min(max(0, K_s − ⌊0.3·π⌋),
//! remainder)` from the borrower → the borrower), the lender and protocol legs to
//! `market_vault`, the keeper legs straight to `keeper`; `pool::repay(r, lender penalty)`;
//! the position aggregates; `exposure.q_open −= q`. Event `Liquidate` (after `TraderSwept`
//! if the normalisation moved anything, and the `MintSafetyTripped` log event).
//!
//! This module also holds the keeper-exit machinery `expire` shares ([`keeper_exit`],
//! [`plan`], [`assess`]): both are the same buyback with a different reference, bound and
//! waterfall (ARCH §5.2).

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use shortwin_math::observe::{self as math_observe, Observation};
use shortwin_math::persist;
use shortwin_math::pool::{self, SweepSource};
use shortwin_math::quote::BuyQuote;
use shortwin_math::rent;
use shortwin_math::risk::{self, Budget};
use shortwin_math::waterfall::{self, ExitInputs, Payout};

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::bounds::{self, ExitBound};
use crate::engine::guard::{mint_guard, MintPath};
use crate::engine::{lender, prologue, waterfall as engine_waterfall};
use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::events::{Liquidate as LiquidateEvent, MintSafetyTripped, TraderSwept, SWEEP_TO_VEST};
use crate::lamports;
use crate::state::{
    GlobalConfig, Market, MarketVault, ShortPosition, VenueKind, WalletExposure, EXTRAS_IX_BUY,
    POOL_VAULT_SEED,
};
use crate::venues::{self, BuyOut, Carry, ExtrasSel, Leg, VenueState};

#[event_cpi]
#[derive(Accounts)]
pub struct Liquidate<'info> {
    /// The keeper (anyone).
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: `== position.borrower` (constraint on `position`); receives the position close.
    #[account(mut)]
    pub borrower: UncheckedAccount<'info>,
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
        has_one = mint @ ShortwinError::AccountMismatch,
        has_one = market_vault @ ShortwinError::AccountMismatch,
        has_one = pool_vault @ ShortwinError::AccountMismatch,
        has_one = token_program @ ShortwinError::AccountMismatch,
        has_one = trader @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        close = borrower,
        has_one = borrower @ ShortwinError::AccountMismatch,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = position.is_pda(&position.key()) @ anchor_lang::error::ErrorCode::ConstraintSeeds
    )]
    pub position: Box<Account<'info, ShortPosition>>,
    #[account(
        mut,
        seeds = [WalletExposure::SEED, market.key().as_ref(), borrower.key().as_ref()],
        bump = exposure.bump,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = exposure.wallet == borrower.key() @ ShortwinError::AccountMismatch
    )]
    pub exposure: Box<Account<'info, WalletExposure>>,
    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [POOL_VAULT_SEED, market.key().as_ref()],
        bump = market.load()?.pool_vault_bump
    )]
    pub pool_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    /// CHECK: `== market.trader` (`has_one`).
    #[account(mut)]
    pub trader: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(ctx: Context<'info, Liquidate<'info>>, extras: ExtrasSel) -> Result<()> {
    let clock = Clock::get()?;
    let pos = (**ctx.accounts.position).clone();
    let mut out = {
        let a = &ctx.accounts;
        let accs = ExitAccounts {
            keeper: a.keeper.to_account_info(),
            config: &a.config,
            market: &a.market,
            position: a.position.to_account_info(),
            exposure: a.exposure.key(),
            mint: a.mint.to_account_info(),
            decimals: a.mint.decimals,
            pool_vault: a.pool_vault.to_account_info(),
            market_vault: &a.market_vault,
            trader: a.trader.to_account_info(),
            token_program: a.token_program.to_account_info(),
            system_program: a.system_program.to_account_info(),
        };
        keeper_exit(
            &accs,
            &pos,
            ctx.remaining_accounts,
            extras,
            ExitKind::Liquidation,
            &clock,
        )?
    };
    let borrower = ctx.accounts.borrower.to_account_info();
    close_exposure(&mut ctx.accounts.exposure, borrower, pos.q)?;

    let market_key = ctx.accounts.market.key();
    if !out.mint_ok {
        emit!(MintSafetyTripped {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            mint: ctx.accounts.mint.key(),
        });
    }
    if out.swept.any() {
        emit_cpi!(TraderSwept {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            lamports: out.swept.lamports,
            tokens: out.swept.tokens,
            to: SWEEP_TO_VEST,
            wipe_bits: 0,
        });
    }
    if let Some(ev) = out.completed.take() {
        emit_cpi!(ev);
    }
    let p = &out.payout;
    emit_cpi!(LiquidateEvent {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        position: ctx.accounts.position.key(),
        keeper: ctx.accounts.keeper.key(),
        cr_spot_bps: out.assessment.cr_spot_bps,
        cr_ema_bps: out.assessment.cr_ema_bps,
        b_live: out.b_live,
        b_ref: out.b_ref,
        p_ref: out.bound.p_ref,
        delta_bps: pos.delta_liq_bps,
        buyback_cost: out.leg.spent,
        rent: p.rent,
        penalty: p.penalty,
        penalty_lender: p.penalty_lenders,
        penalty_protocol: p.penalty_protocol,
        penalty_keeper: p.penalty_keeper,
        keeper_topup: p.keeper_topup,
        borrower_payout: p.borrower,
        expired: out.assessment.expired,
        token_surplus: out.token_surplus,
        b_live_risk: out.b_live_risk,
        max_in: out.max_in,
    });
    Ok(())
}

// ---- the keeper-exit machinery (shared with `expire`) -------------------------------------

/// Which keeper-initiated buyback (ARCH §3.6, §3.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitKind {
    /// `liquidate`: the liquidation reference, fixed `δ_liq`, no reserve, the penalty.
    Liquidation,
    /// `expire`: the expiry reference, `δ_exp`, rent and the expiry fee senior.
    Expiry,
}

/// The accounts a keeper exit touches (the named accounts of `liquidate` and `expire`, which
/// are identical).
pub(crate) struct ExitAccounts<'a, 'info> {
    pub keeper: AccountInfo<'info>,
    pub config: &'a AccountLoader<'info, GlobalConfig>,
    pub market: &'a AccountLoader<'info, Market>,
    pub position: AccountInfo<'info>,
    pub exposure: Pubkey,
    pub mint: AccountInfo<'info>,
    pub decimals: u8,
    pub pool_vault: AccountInfo<'info>,
    pub market_vault: &'a AccountLoader<'info, MarketVault>,
    pub trader: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
}

/// A position's risk picture after the prologue (ARCH §4.5; ECON §3.3, §5.5, §5.6): what the
/// keeper exits decide on. Every quote is the position's, on the post-roll observation state
/// and the venue's pre-trade state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// `R_due = max(rent_accrued, r_min)` on the post-accrual basis.
    pub r_due: u64,
    /// `V_eff = V − R_due` (floored at 0).
    pub v_eff: u64,
    /// `K_s` with the locked settlement keeper terms.
    pub k_s: u64,
    /// A fresh completed bucket (`lb_sec ≥ now − ref_max_age_s`).
    pub fresh: bool,
    /// The execution quote `B̂_live(q)` at the live fee (`None`: undefined).
    pub b_live: Option<BuyQuote>,
    /// `B̂_live` at the risk fee `min(φ_live, venue_fee_hard_cap_bps_pos)`, on PumpSwap also at
    /// most φ at `P_ema` (`VenueState::exit_risk_cap_bps`, AIR-10).
    pub b_live_risk: Option<u64>,
    /// σ of the live buyback (`u64::MAX` if undefined).
    pub sigma_live: u64,
    /// The spot leg's risk quote `B̂(Q; reprice(max(L_live, L_ema'), P_rs))` at the robust spot
    /// (`shortwin_math::bounds::spot_liquidity`, FITS-2; `None`: undefined or no fresh bucket).
    pub b_rs: Option<u64>,
    /// The liquidation spot leg.
    pub spot_leg: bool,
    /// The liquidation EMA leg (newest `liq_persist_n` ring entries).
    pub ema_leg: bool,
    /// `now ≥ deadline`.
    pub expired: bool,
    /// `B̂_live + ert ≤ V − K_s ∧ σ_live ≤ σ_max`.
    pub buyback_allowed: bool,
    /// `CR(V_eff, B̂_rs)` in bps (event; `u64::MAX` past `u64`).
    pub cr_spot_bps: u64,
    /// `CR(V_eff, B̂(Q; x, y))` on the newest ring entry (event).
    pub cr_ema_bps: u64,
}

impl Assessment {
    /// CR-liquidatable: spot leg ∧ EMA leg (expiry not included).
    pub const fn cr_liquidatable(&self) -> bool {
        self.spot_leg && self.ema_leg
    }

    /// `liquidatable = (spot leg ∧ EMA leg) ∨ now ≥ deadline` (SPEC §5.1; ECON §5.5).
    pub const fn liquidatable(&self) -> bool {
        self.cr_liquidatable() || self.expired
    }
}

/// The position's risk picture (ARCH §4.5): `R_due`, `V_eff`, `K_s`, the live quotes (execution
/// and risk) and σ, the robust spot leg, the EMA leg on the newest `liq_persist_n` ring
/// entries' cached reprojections, expiry and "normal buyback allowed". Every formula is
/// `shortwin_math`'s (`rent::due`, `risk::{v_eff, k_s, spot_leg, ring_below, buyback_allowed,
/// sigma_xy, cr_bps}`, `persist::liq_ema_leg`, `observe::robust_spot`).
#[inline(never)]
pub(crate) fn assess(
    pos: &ShortPosition,
    market: &Market,
    obs: &Observation,
    state: &VenueState,
    now: i64,
) -> Result<Assessment> {
    let q = pos.q;
    let r_due = rent::due(&pos.pos_rent(), &market.basis()).math()?;
    let v_eff = risk::v_eff(pos.v, r_due);
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).math()?;
    // AIR-10: on PumpSwap the risk view at most φ at `P_ema`, so a sale earlier in this
    // transaction cannot raise the fee of the spot and EMA legs (the execution quote stays live).
    let cap = state.exit_risk_cap_bps(u64::from(pos.venue_fee_hard_cap_bps));
    let l_bps = pos.cr_liq_bps;
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);

    let b_live = state.buy(q);
    let b_live_risk = state.buy_risk(q, cap).map(|b| b.cost);
    let sigma_live = match b_live {
        Some(b) if state.x > 0 => {
            risk::sigma_xy(Some(b.net), q, u128::from(state.x), u128::from(state.y)).math()?
        }
        _ => u64::MAX,
    };
    let risky = state.quoter().map(|p| p.capped(cap));
    let quote_at =
        move |x: u128, y: u128| -> Option<u64> { risky?.buy_xy(q, x, y).map(|b| b.cost) };

    // Spot leg (ECON §5.5): the robust spot, valid only while fresh, on `max(L_live, L_ema')`
    // (FITS-2 (a): a withdrawal inside the transaction cannot drain it below the EMA's depth).
    let l_live = state.liquidity().unwrap_or(obs.ema.liq);
    let l_spot = shortwin_math::bounds::spot_liquidity(l_live, &obs.ema, &obs.ring).math()?;
    let p_rs = state
        .price()
        .and_then(|p| math_observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let b_rs = p_rs.and_then(|p| risky?.buy_repriced(q, l_spot, p).map(|b| b.cost));
    let spot_leg = risk::spot_leg(state.executable, fresh, v_eff, b_rs, l_bps).math()?;
    // EMA leg (decision 2): the newest `liq_persist_n` entries' cached `(x, y)`.
    let ema_leg = persist::liq_ema_leg(
        &obs.ring,
        risk::ring_below(v_eff, l_bps, quote_at),
        pos.liq_persist_n,
    )
    .math()?;

    let budget = Budget {
        v: pos.v,
        k_s,
        ert: pos.ert,
    };
    let buyback_allowed = risk::buyback_allowed(
        &budget,
        b_live.map(|b| b.cost),
        sigma_live,
        u32::from(pos.max_liq_slip_bps),
    )
    .math()?;
    let (x, y) = market.newest_ring_xy();
    Ok(Assessment {
        r_due,
        v_eff,
        k_s,
        fresh,
        b_live,
        b_live_risk,
        sigma_live,
        b_rs,
        spot_leg,
        ema_leg,
        expired: now >= pos.deadline,
        buyback_allowed,
        cr_spot_bps: risk::cr_bps(v_eff, b_rs).unwrap_or(u64::MAX),
        cr_ema_bps: risk::cr_bps(v_eff, quote_at(x, y)).unwrap_or(u64::MAX),
    })
}

/// The decision of a keeper exit before its leg: the risk picture, the reference and the bound.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExitPlan {
    pub assessment: Assessment,
    pub bound: ExitBound,
    /// `B̂_ref` (defined).
    pub b_ref: u64,
    /// `max_in` of ARCH §3.6 step 6 / §3.7.
    pub max_in: u64,
    /// `π_ref = ⌈expiry_fee_bps·B̂_ref/10⁴⌉` (expiry; 0 for a liquidation).
    pub pi_ref: u64,
    /// The execution quote `B̂_live` (defined, `≤ max_in`).
    pub b_live: u64,
}

/// The preconditions of ARCH §3.6 (steps 2–6) or §3.7 after the prologue and the venue checks,
/// on the post-roll state. Every refusal is a revert (nothing is written).
///
/// `expire` (ARCH §3.7): `NotExpired`, `ReferenceStale`, `UseLiquidate` (expired **and**
/// CR-liquidatable: the penalty applies, ECON §8.4), `UseSolSettle`, then the expiry bound
/// `max_in = min(⌊B̂_ref·(10⁴ + δ_exp)/10⁴⌋, V − K_s − ert, max(V − R − π_ref − ert,
/// ⌊B̂_ref·(10⁴ + ε)/10⁴⌋))` on `P_ref = min(P_ema, min over the ring of p_i, lb_lo)`
/// (`ExitBoundNotMet`).
#[inline(never)]
pub(crate) fn plan(
    which: ExitKind,
    pos: &ShortPosition,
    market: &Market,
    state: &VenueState,
    now: i64,
    depth_cap: Option<u128>,
) -> Result<ExitPlan> {
    let obs = market.observation();
    let a = assess(pos, market, &obs, state, now)?;
    let (bound, max_in, pi_ref) = match which {
        ExitKind::Liquidation => {
            require!(a.fresh, ShortwinError::ReferenceStale);
            require!(a.liquidatable(), ShortwinError::NotLiquidatable);
            require!(a.buyback_allowed, ShortwinError::UseSolSettle);
            require!(a.cr_liquidatable() || !a.expired, ShortwinError::UseExpire);
            let bound = bounds::liq_reference(pos, &obs, state, now, depth_cap)?
                .ok_or(ShortwinError::ReferenceStale)?;
            let b_ref = bound.b_ref.ok_or(ShortwinError::ExitBoundNotMet)?;
            let max_in =
                bounds::max_in_liq(pos, b_ref, a.k_s).ok_or(ShortwinError::ExitBoundNotMet)?;
            (bound, max_in, 0)
        }
        ExitKind::Expiry => {
            require!(a.expired, ShortwinError::NotExpired);
            require!(a.fresh, ShortwinError::ReferenceStale);
            require!(!a.cr_liquidatable(), ShortwinError::UseLiquidate);
            require!(a.buyback_allowed, ShortwinError::UseSolSettle);
            let bound = bounds::expiry_reference(pos, &obs, state, now, depth_cap)?
                .ok_or(ShortwinError::ReferenceStale)?;
            let b_ref = bound.b_ref.ok_or(ShortwinError::ExitBoundNotMet)?;
            let max_in = bounds::max_in_exp(pos, b_ref, a.k_s, a.r_due)?
                .ok_or(ShortwinError::ExitBoundNotMet)?;
            let pi_ref = shortwin_math::bounds::pi_ref(b_ref, pos.expiry_fee_bps).math()?;
            (bound, max_in, pi_ref)
        }
    };
    let b_ref = bound.b_ref.ok_or(ShortwinError::ExitBoundNotMet)?;
    // "Buyback allowed" makes the execution quote defined.
    let b_live = a.b_live.ok_or(ShortwinError::UseSolSettle)?.cost;
    require_gte!(max_in, b_live, ShortwinError::ExitBoundNotMet);
    Ok(ExitPlan {
        assessment: a,
        bound,
        b_ref,
        max_in,
        pi_ref,
        b_live,
    })
}

/// What a keeper exit did (for its events and the exposure update).
pub(crate) struct ExitOutcome {
    /// The mint passes the safety filter now (else the exit path's `MintSafetyTripped`).
    pub mint_ok: bool,
    /// What the trader normalisation swept (`TraderSwept`): lamports to lenders, tokens of a
    /// pump venue's `trader_base_ata` to the pool vault.
    pub swept: crate::engine::trader::Swept,
    /// The buyback completed the curve: the market moved to `PumpMigrating` (`VenueStateChanged`).
    pub completed: Option<crate::events::VenueStateChanged>,
    pub assessment: Assessment,
    pub bound: ExitBound,
    pub b_ref: u64,
    pub max_in: u64,
    pub pi_ref: u64,
    /// The execution quote the leg was bounded by.
    pub b_live: u64,
    /// The live leg of the penalty / expiry-fee base `min(B̂_live,risk, B̂_ref)`: the risk quote
    /// (the execution quote if the risk quote were undefined, which a defined execution quote
    /// excludes: a lower fee only lowers the cost).
    pub b_live_risk: u64,
    pub leg: BuyOut,
    pub token_surplus: u64,
    pub payout: Payout,
}

/// A keeper-initiated buyback (ARCH §3.6 / §3.7), everything but the exposure update and the
/// events: the prologue and the venue checks, [`plan`], the trader normalisation, the buy
/// leg, the waterfall, the lamport legs (lenders and protocol to `market_vault`, the keeper
/// to `keeper`; the borrower's stays on the position, closed to the borrower, A18), the pool
/// booking (`pool::repay` with the vesting lump), the position aggregates and the
/// post-conditions (I2, I7, `pool_vault ≥ T`).
///
/// Lamport moves out of the position happen only after the leg's last CPI (a direct move is
/// visible to the runtime's per-CPI balance check only when both accounts are in that CPI).
#[inline(never)]
pub(crate) fn keeper_exit<'info>(
    accs: &ExitAccounts<'_, 'info>,
    pos: &ShortPosition,
    remaining: &'info [AccountInfo<'info>],
    extras: ExtrasSel,
    which: ExitKind,
    clock: &Clock,
) -> Result<Box<ExitOutcome>> {
    let config = accs.config.load()?;
    let market_key = accs.market.key();

    // ---- phase 1: prologue, venue checks, preconditions and the bound ---------------------
    let (kind, now, mut pool, mut vault, rem, plan, mint_ok, market_bump) = {
        let mut market = accs.market.load_mut()?;
        let kind = market.venue_kind().ok_or(ShortwinError::NoRoute)?;
        match kind {
            VenueKind::None => return err!(ShortwinError::NoRoute),
            VenueKind::PumpMigrating => return err!(ShortwinError::VenueMigrating),
            _ => {}
        }
        let mint_ok = mint_guard(&market, &accs.mint, accs.token_program.key, MintPath::Exit)?;
        let deny = [
            accs.config.key(),
            market_key,
            market.market_vault,
            market.pool_vault,
            market.trader,
            market.trader_wsol,
            accs.position.key(),
            accs.exposure,
            pos.borrower,
        ];
        let rem = venues::parse(
            &market,
            kind,
            remaining,
            Carry::Trade {
                ix: EXTRAS_IX_BUY,
                sel: extras,
            },
            &deny,
        )?;
        // AIR-10: on PumpSwap also φ at `P_ema`, which caps the risk view (`assess`, `B̂_ref`).
        let state = venues::read_state_boxed_for_exit(
            &market,
            kind,
            rem.read,
            clock.unix_timestamp,
            venues::mint_supply(&accs.mint),
        );
        let mut vault = accs.market_vault.load()?.vault();
        let pro = prologue::run(
            &config,
            &market_key,
            &mut market,
            &mut vault,
            Some(&*state),
            clock.unix_timestamp,
            clock.slot,
        )?;
        venues::pump_curve::exit_venue_checks(&pro, &state)?;
        // Precondition 1b (M4R): no keeper exit while the observation state is in its
        // post-change warm-up on this (executable) venue.
        require!(market.ema_warm == 0, ShortwinError::EmaWarmingUp);
        // M4F-1 (Raydium), M6R-1 (PumpSwap): the reference credits no more depth than sizing
        // (`pool_d` just refreshed by the prologue over `λ_min`).
        let depth_cap = bounds::ref_depth_cap(&config, &market, kind);
        let plan = plan(which, pos, &market, &state, pro.now, depth_cap)?;
        (
            kind,
            pro.now,
            pro.pool,
            vault,
            rem,
            plan,
            mint_ok,
            market.bump,
        )
    };

    // ---- phase 2: normalisation and the buy leg -------------------------------------------
    let (trader, sol_first) = {
        let m = accs.market.load()?;
        let t = venues::trader_accounts(
            kind,
            market_key,
            &m,
            accs.trader.clone(),
            accs.market_vault.to_account_info(),
            rem.trade,
            venues::Named {
                system_program: accs.system_program.clone(),
                pool_vault: accs.pool_vault.clone(),
                mint: accs.mint.clone(),
                decimals: accs.decimals,
                token_program: accs.token_program.clone(),
            },
        )?;
        (t, venues::raydium::sol_first(&m))
    };
    let swept = trader.normalise()?;
    if swept.lamports > 0 {
        pool::sweep(
            &mut pool,
            &mut vault,
            None,
            now,
            config.params.lump_vest_s,
            swept.lamports,
            SweepSource::Trader,
        )
        .math()?;
    }
    let leg = Leg {
        kind,
        market: accs.market.to_account_info(),
        market_bump,
        pool_vault: accs.pool_vault.clone(),
        mint: accs.mint.clone(),
        decimals: accs.decimals,
        token_program: accs.token_program.clone(),
        sol_first,
        ert: pos.ert,
        trader,
        read: rem.read,
        trade: rem.trade,
        extra_metas: rem.extra_metas,
        extra_infos: rem.extra_infos,
    };
    let out = venues::buy(&leg, &accs.position, pos.q, plan.max_in)?;
    let spend_bound = plan.max_in.checked_add(pos.ert).or_math()?;
    require_gte!(spend_bound, out.spent, ShortwinError::VenueResultMismatch);
    let token_surplus = out.received.checked_sub(pos.q).or_math()?;

    // ---- phase 3: the waterfall, the lamport legs, the pool and the market -----------------
    let a = &plan.assessment;
    let b_live_risk = a.b_live_risk.unwrap_or(plan.b_live);
    let inputs = ExitInputs {
        v: pos.v,
        b: out.spent,
        r_due: a.r_due,
        b_live: b_live_risk,
        b_ref: plan.b_ref,
        rent_protocol_bps: u32::from(pos.rent_protocol_bps),
    };
    let payout = match which {
        ExitKind::Liquidation => waterfall::liquidate(
            &inputs,
            u32::from(pos.penalty_bps),
            &pos.penalty_split(),
            a.k_s,
        ),
        ExitKind::Expiry => waterfall::expire(
            &inputs,
            u32::from(pos.expiry_fee_bps),
            &pos.expiry_keeper_terms(),
        ),
    }
    .math()?;
    let market_vault_info = accs.market_vault.to_account_info();
    engine_waterfall::pay_vault(&accs.position, &market_vault_info, &payout)?;
    lamports::move_lamports(&accs.position, &accs.keeper, payout.keeper)?;
    let (t_after, tracked) = {
        let mut market = accs.market.load_mut()?;
        let basis = market.basis();
        engine_waterfall::book_repay(
            &mut pool,
            &mut vault,
            now,
            config.params.lump_vest_s,
            pos,
            &basis,
            &payout,
        )?;
        let mut aggr = market.aggr();
        engine_waterfall::remove_position(&mut market, &mut aggr, pos)?;
        market.set_aggr(&aggr);
        market.set_pool(&pool);
        let mut va = accs.market_vault.load_mut()?;
        va.set_vault(&vault);
        (pool.tokens_free, lender::tracked(&vault)?)
    };
    // A buyback that completed the curve: the market is migrating from now on (ARCH §5.4 case 1).
    let completed = {
        let mut m = accs.market.load_mut()?;
        venues::pump_curve::sync_after_leg(&mut m, kind, rem.read, now)
            .map(|t| venues::pump_curve::transition_event(&m, t, market_key, clock))
    };
    leg.trader.post_conditions(&accs.pool_vault)?;
    let pv = crate::engine::trader::token_state(&accs.pool_vault)?;
    require_gte!(pv.amount, t_after, ShortwinError::AccountMismatch);
    lender::check_vault_floor(&market_vault_info, tracked)?;

    Ok(Box::new(ExitOutcome {
        mint_ok,
        swept,
        completed,
        assessment: plan.assessment,
        bound: plan.bound,
        b_ref: plan.b_ref,
        max_in: plan.max_in,
        pi_ref: plan.pi_ref,
        b_live: plan.b_live,
        b_live_risk,
        leg: out,
        token_surplus,
        payout,
    }))
}

/// `exposure.q_open −= q`; the record is closed to the borrower at 0 (as `close_short`).
pub(crate) fn close_exposure<'info>(
    exposure: &mut Account<'info, WalletExposure>,
    borrower: AccountInfo<'info>,
    q: u64,
) -> Result<()> {
    exposure.q_open = exposure.q_open.checked_sub(q).or_math()?;
    if exposure.q_open == 0 {
        exposure.close(borrower)?;
    }
    Ok(())
}
