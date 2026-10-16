//! `close_short(max_buyback_cost, extras)` (ARCH §3.5; ECON §8.1): the borrower buys back its
//! `q` tokens with position SOL, pays rent and takes the rest.
//!
//! **Signer:** `borrower` (`== position.borrower`). **Named accounts:** as `open_short` without
//! `venue_guard` and `oracle_cache`; `position` is closed to the borrower (A18), `exposure` is
//! closed to it when its `q_open` reaches 0. **Remaining:** `ReadSet ++ TradeExtras ++ Extras`.
//!
//! **Exits never check the venue guard, pause flags, the route-broken flag or the oracle**
//! (I10, V2-1): none of those accounts is even passed. A borrower may close while liquidatable
//! (no penalty, A25, ECON G28) and while the venue fee is above the hard cap, if its own
//! `max_buyback_cost` allows.
//!
//! **Steps** (ARCH §3.5):
//!
//! 1. The prologue; the mint guard on the exit path (never reverts on the filter: a mint that
//!    fails it now is reported with a `MintSafetyTripped` log event, M1-SEC-11).
//! 2. The venue trades (`NoRoute` for `None`, `VenueMigrating` while `PumpMigrating` — also when
//!    this instruction's venue sync just moved a curve market there) and is executable
//!    (`NoRoute`). On a live curve a `q > rT` buyback is an ordinary exit (pump-final §6.1.7):
//!    `buy_v3` buys `rT` on the curve and the rest from the would-be PumpSwap pool by pump's
//!    synthetic migration, priced `B̂_v3`; `CurveStraddle` only when that pool cannot deliver
//!    `q − rT` (`q − rT ≥ X_s`, unreachable under the per-market cap, pump-final C8) or the
//!    curve has no synthetic route (`venues::pump_curve::close_venue_checks`).
//! 3. `R = rent::due(position, basis)`.
//! 4. `B̂ = B̂_live(q)` at the **live** fee; `B̂ + R + ert ≤ V`, else `InsufficientVault` (an
//!    undefined quote too).
//! 5. The buy leg with `max_in = min(max_buyback_cost, V − R − ert)` (C0-14); `B̂ ≤ max_in`,
//!    else `BuybackAboveMax`. The leg asserts `B ≤ max_in + ert` and at least `q` tokens into
//!    `pool_vault`; **a surplus is accepted** as a donation to the pool vault (`T` unchanged)
//!    and reported (`Close.token_surplus`; A62, V2-10), so a venue that over-delivers cannot
//!    freeze exits.
//! 6. The waterfall `close` (`B` → `R`, split by the position's locked `rent_protocol_bps` →
//!    the remainder stays on the position).
//! 7. The market: `pool::repay(R, 0)` (token return, rent true-up, `resolve(q)`), the
//!    position aggregates, `exposure.q_open −= q`. Post-conditions (I2, I7, `pool_vault ≥ T`).
//!    A buyback that completed the curve (`q ≥ rT`: exactly `rT`, or past it by the synthetic
//!    migration) moves the market to `PumpMigrating` after the leg (ARCH §3.5 step 7, §5.4
//!    case 1; `migration_seen_at = now`).
//! 8. Event `Close` (and `TraderSwept` if the normalisation moved anything, `VenueStateChanged`
//!    if the buyback completed the curve).

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use shortwin_math::pool::{self, SweepSource};
use shortwin_math::rent;
use shortwin_math::waterfall;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::guard::{mint_guard, MintPath};
use crate::engine::{bounds, lender, prologue, waterfall as engine_waterfall};
use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::events::{Close, MintSafetyTripped, TraderSwept, SWEEP_TO_VEST};
use crate::state::{
    GlobalConfig, Market, MarketVault, ShortPosition, VenueKind, WalletExposure, EXTRAS_IX_BUY,
    POOL_VAULT_SEED,
};
use crate::venues::{self, Carry, ExtrasSel, Leg};

#[event_cpi]
#[derive(Accounts)]
pub struct CloseShort<'info> {
    /// The position's borrower; receives the position's remainder and rent (A18).
    #[account(mut)]
    pub borrower: Signer<'info>,
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
    /// Bound to its PDA `["position", market, borrower, nonce]` with the stored bump.
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
    /// CHECK: `== market.trader` (`has_one`); checked by the leg's normalisation.
    #[account(mut)]
    pub trader: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'info, CloseShort<'info>>,
    max_buyback_cost: u64,
    extras: ExtrasSel,
) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let position_key = ctx.accounts.position.key();
    let borrower_key = ctx.accounts.borrower.key();
    let config = ctx.accounts.config.load()?;
    let pos = (**ctx.accounts.position).clone();

    // ---- phase 1: steps 1–5 (up to the leg) -----------------------------------------------
    let (kind, now, mut pool, mut vault, rem, r_due, b_hat, max_in, mint_ok, market_bump) = {
        let mut market = ctx.accounts.market.load_mut()?;
        let kind = market.venue_kind().ok_or(ShortwinError::NoRoute)?;
        match kind {
            VenueKind::None => return err!(ShortwinError::NoRoute),
            VenueKind::PumpMigrating => return err!(ShortwinError::VenueMigrating),
            _ => {}
        }
        let mint_ok = mint_guard(
            &market,
            &ctx.accounts.mint.to_account_info(),
            ctx.accounts.token_program.key,
            MintPath::Exit,
        )?;
        let deny = [
            ctx.accounts.config.key(),
            market_key,
            market.market_vault,
            market.pool_vault,
            market.trader,
            market.trader_wsol,
            position_key,
            ctx.accounts.exposure.key(),
            borrower_key,
        ];
        let rem = venues::parse(
            &market,
            kind,
            ctx.remaining_accounts,
            Carry::Trade {
                ix: EXTRAS_IX_BUY,
                sel: extras,
            },
            &deny,
        )?;
        let state = venues::read_state_boxed(
            &market,
            kind,
            rem.read,
            clock.unix_timestamp,
            Some(ctx.accounts.mint.supply),
        );
        let mut vault = ctx.accounts.market_vault.load()?.vault();
        let pro = prologue::run(
            &config,
            &market_key,
            &mut market,
            &mut vault,
            Some(&*state),
            clock.unix_timestamp,
            clock.slot,
        )?;
        venues::pump_curve::close_venue_checks(&pro, &state, pos.q)?;
        let r_due = rent::due(&pos.pos_rent(), &market.basis()).math()?;
        let b_hat = state
            .buy(pos.q)
            .ok_or(ShortwinError::InsufficientVault)?
            .cost;
        let need = b_hat
            .checked_add(r_due)
            .and_then(|x| x.checked_add(pos.ert))
            .or_math()?;
        require_gte!(pos.v, need, ShortwinError::InsufficientVault);
        let max_in = bounds::close_max_in(pos.v, r_due, pos.ert, max_buyback_cost)?
            .ok_or(ShortwinError::InsufficientVault)?;
        require_gte!(max_in, b_hat, ShortwinError::BuybackAboveMax);
        (
            kind,
            pro.now,
            pro.pool,
            vault,
            rem,
            r_due,
            b_hat,
            max_in,
            mint_ok,
            market.bump,
        )
    };

    // ---- phase 2: the buy leg --------------------------------------------------------------
    let accs = &ctx.accounts;
    let (trader, sol_first) = {
        let m = accs.market.load()?;
        let t = venues::trader_accounts(
            kind,
            market_key,
            &m,
            accs.trader.to_account_info(),
            accs.market_vault.to_account_info(),
            rem.trade,
            venues::Named {
                system_program: accs.system_program.to_account_info(),
                pool_vault: accs.pool_vault.to_account_info(),
                mint: accs.mint.to_account_info(),
                decimals: accs.mint.decimals,
                token_program: accs.token_program.to_account_info(),
            },
        )?;
        (t, venues::raydium::sol_first(&m))
    };
    let swept = trader.normalise()?;
    if swept.any() {
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
        emit_cpi!(TraderSwept {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            lamports: swept.lamports,
            tokens: swept.tokens,
            to: SWEEP_TO_VEST,
            wipe_bits: 0,
        });
    }
    let leg = Leg {
        kind,
        market: accs.market.to_account_info(),
        market_bump,
        pool_vault: accs.pool_vault.to_account_info(),
        mint: accs.mint.to_account_info(),
        decimals: accs.mint.decimals,
        token_program: accs.token_program.to_account_info(),
        sol_first,
        ert: pos.ert,
        trader,
        read: rem.read,
        trade: rem.trade,
        extra_metas: rem.extra_metas,
        extra_infos: rem.extra_infos,
    };
    let position_info = accs.position.to_account_info();
    let out = venues::buy(&leg, &position_info, pos.q, max_in)?;
    let bound = max_in.checked_add(pos.ert).or_math()?;
    require_gte!(bound, out.spent, ShortwinError::VenueResultMismatch);
    let token_surplus = out.received.checked_sub(pos.q).or_math()?;

    // ---- phase 3: steps 6–7, the waterfall and the market -----------------------------------
    let payout =
        waterfall::close(pos.v, out.spent, r_due, u32::from(pos.rent_protocol_bps)).math()?;
    engine_waterfall::pay_vault(
        &position_info,
        &accs.market_vault.to_account_info(),
        &payout,
    )?;
    let (t_after, tracked) = {
        let mut market = accs.market.load_mut()?;
        let basis = market.basis();
        engine_waterfall::book_repay(
            &mut pool,
            &mut vault,
            now,
            config.params.lump_vest_s,
            &pos,
            &basis,
            &payout,
        )?;
        let mut aggr = market.aggr();
        engine_waterfall::remove_position(&mut market, &mut aggr, &pos)?;
        market.set_aggr(&aggr);
        market.set_pool(&pool);
        let mut va = accs.market_vault.load_mut()?;
        va.set_vault(&vault);
        (pool.tokens_free, lender::tracked(&vault)?)
    };
    // A buyback that completed the curve: the market is migrating from now on (ARCH §3.5 step 7).
    let completed = {
        let mut m = accs.market.load_mut()?;
        venues::pump_curve::sync_after_leg(&mut m, kind, rem.read, now)
            .map(|t| venues::pump_curve::transition_event(&m, t, market_key, &clock))
    };
    leg.trader
        .post_conditions(&accs.pool_vault.to_account_info())?;
    let pv = crate::engine::trader::token_state(&accs.pool_vault.to_account_info())?;
    require_gte!(pv.amount, t_after, ShortwinError::AccountMismatch);
    lender::check_vault_floor(&accs.market_vault.to_account_info(), tracked)?;

    let exposure = &mut ctx.accounts.exposure;
    exposure.q_open = exposure.q_open.checked_sub(pos.q).or_math()?;
    if exposure.q_open == 0 {
        let to = ctx.accounts.borrower.to_account_info();
        ctx.accounts.exposure.close(to)?;
    }

    if !mint_ok {
        emit!(MintSafetyTripped {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            mint: ctx.accounts.mint.key(),
        });
    }
    if let Some(ev) = completed {
        emit_cpi!(ev);
    }
    emit_cpi!(Close {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        position: position_key,
        borrower: borrower_key,
        q: pos.q,
        buyback_cost: out.spent,
        rent: payout.rent,
        rent_protocol: payout.rent_protocol,
        borrower_payout: payout.borrower,
        b_live: b_hat,
        token_surplus,
    });
    Ok(())
}
