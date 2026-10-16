//! `open_short(nonce, q, min_proceeds, max_margin, max_open_fee, extras)` (ARCH §3.4; ECON §4,
//! §6): a borrower opens a short of `q` tokens atomically — borrow from the pool, sell on the
//! market's venue, lock the proceeds `S` and the D4 margin `M` on the position.
//!
//! **Signer:** `borrower` (payer of the position, the exposure record, the ephemeral WSOL
//! account's rent for the length of the leg, the margin and the open fee).
//!
//! **Remaining:** `ReadSet ++ TradeExtras ++ Extras` of the market's venue (`crate::venues`);
//! `extras` selects the extras set (`none` / `current` / `previous`, ARCH §3.0, A41).
//!
//! **Steps** (ARCH §3.4; every step required, any failure reverts):
//!
//! 1. Static guards (`engine::guard::open_static`): opens paused globally or on the market
//!    (`OpensPaused`), keeper-coverage pause (`CoveragePaused`), the route-broken flag of the
//!    market or its venue (`RouteBroken`), a venue that trades (`NoSupportedVenue`,
//!    `VenueMigrating`) and is enabled (`VenueDisabled`); the mint guard on the entry path
//!    (`MintUnsafeFlagged`, the filter's codes). **`total_shares == 0` refuses**
//!    (`InsufficientInventory`): tokens without shares (the virtual shares' part, rebase
//!    orphans) are not lendable, so no borrow can create `S = 0 ∧ ΣB_Q > 0` (ARCH §9 M0b note
//!    4; M1-SEC-3).
//! 2. The prologue (`engine::prologue::run`): the observation recorded is the **pre-trade**
//!    state; the EMA the body reads is the post-roll one (the one-second lag).
//! 3. Venue guards: the latch (`VenueUpgraded`, I17; on Raydium the mask of **both** Raydium
//!    programs, R4B-7); the venue is executable (`NoRoute`); `!s3_no_route` — with the flags
//!    already refused, that is the fee against the global hard cap — and the one-way fee `φ ≤
//!    venue_fee_cap_bps·100` (`VenueFeeTooHigh`); Raydium: `now ≥ pool_seen_at +
//!    pool_list_window_s` (`PoolListWindow`), the post-change warm-up over — `ema_warm == 0`
//!    after the roll: every listing and adoption restarts the observation state and the next
//!    `EMA_WARMUP_BUCKETS` integrations re-seed it from the pool (`EmaWarmingUp`; M4R / M4V-5,
//!    **PROPOSED**: no open on a `P_ema` one transaction chose) —
//!    and `R_eff ≥ pool_min_real_sol` (`PoolTooThin`; `R_eff` bounded by the locked depth over
//!    `λ_min`, ARCH §2.6 rule 5). PumpSwap (M6): `disable_flags == 0` (`VenueFlagged`; A24 —
//!    the T-DISABLE buy bit already made the venue not executable, `NoRoute`) and the warm-up
//!    after a graduation or a price start over (`EmaWarmingUp`; the Raydium rule, kept by the
//!    owner on 2026-09-30, OWNER-M6R-2). The curve (M6-SEC-3, owner decision of 2026-09-30):
//!    PumpSwap's `disable_flags` bit 0 (`create_pool`, hence `migrate_v2`) clear
//!    (`GraduationHalted`; C6 of its ReadSet, no extra account). A curve that completed (or
//!    migrated) since the last observation moves the market in the prologue's venue sync (ARCH
//!    §2.5): the open refuses `VenueMigrating` (`NoRoute` if it went `None`), and nothing of it is
//!    kept.
//! 4. Open guards: `now ≥ created_at + market_warmup_s` (`MarketWarmingUp`); `|P_spot/P_ema − 1|
//!    ≤ open_ema_dev_bps` (`PriceDeviatesFromEma`); `P_ema ≤ P_OPEN_MAX` (`PriceAboveOpenMax`);
//!    PumpSwap's boost gate (`BoostInProgress`: the boost vault above `boost_dust_lamports`
//!    before `graduated_at + boost_window_s`; A13).
//! 5. Tier (`engine::tier::assign`), cached on the market.
//! 6. Caps (`engine::caps::check_open`; on Raydium and — since M6R-1, owner decision of
//!    2026-09-30 — PumpSwap the conservative reserve `X₀` is also bounded by the locked depth
//!    `pool_d` over `λ_min` (PumpSwap: the burned LP's depth); on the curve `X₀` and the Tier D
//!    guard's `rT` are the live ones bounded by the curve at `P_ema`, M5-SEC-3 — ARCH §4.6 said
//!    the live `vT` and `rT`, which a sale in the same transaction lifts):
//!    position impact and
//!    fee-scaled size (`CapPositionImpact`), market OI and fee-scaled OI (`CapMarketOi`),
//!    utilisation (`CapUtilization`), Tier D gamma (`CapCurveGamma`), wallet (`CapWallet`),
//!    `q ≤ T` (`InsufficientInventory`).
//! 7. `S_exp = quote_sell(q)` on the live state.
//! 8. The sell leg (`crate::venues::sell`) after the trader normalisation, with `min_out =
//!    max(min_proceeds, ⌊S_exp·(10⁴ − exec_tol_bps)/10⁴⌋)`; `S` measured (`ProceedsBelowMin`
//!    below it). On the pump venues the leg funds the trader with `ert = exec_rent_tol` from the
//!    borrower and refunds it: `S` is net of any venue-side rent (ARCH §2.4 rule 3, T-RENT; on
//!    PumpSwap `Open.venue_rent` reports it).
//! 9. The venue state reloaded: `N_open = B̂_spot(q)` on the post-open reserves; `B̂_ema = B̂(q;
//!    x, y)` of the newest ring entry (post-roll). `N_open ≥ min_notional`
//!    (`BelowMinNotional`); `total_notional_open + N_open ≤ market_max_notional`
//!    (`MarketNotionalCap`).
//! 10. Margin (D4; `risk::margin_d4`): `M = max(0, ⌈cr_open·max(B̂_spot, B̂_ema)/10⁴⌉ − S)`, the
//!     EMA leg dropped when undefined; `M ≤ max_margin` (`MarginAboveMax`); System transfer
//!     borrower → position.
//! 11. Open fee `⌈open_fee_bps·N_open/10⁴⌉ ≤ max_open_fee` (`OpenFeeAboveMax`); System transfer
//!     borrower → `market_vault`; `protocol_fees += fee`.
//! 12. The position: `q, s, m, v = S + M, n_open, fee_open, entry_price = ⌊S·WAD/q⌋, j_open,
//!     r_min` (`rent::min_rent` at the post-roll `P_ema` and the pre-open utilisation),
//!     `t_open, deadline`, every locked term (`ShortPosition::lock_terms`), arming 0.
//! 13. The market: `pool::borrow(q, release_at)`, the position aggregates
//!     (`engine::waterfall::add_position`: `ΣB_Q`, `A_j`, the counts, the high-water mark of `q`
//!     and the low-water mark of the step-6 fee φ, `min_open_fee_ppm`), `exposure.q_open += q`.
//!     Post-conditions: the leg's trader state (I7), `pool_vault.amount ≥ T`, the vault floor
//!     (I2), utilisation ≤ cap (I5).
//! 14. Event `Open` (and `TraderSwept` if the normalisation moved anything).
//!
//! The position's bump is Anchor's canonical one (`bump` in the `init` constraint): ARCH §3.4
//! has the client pass it; the canonical search costs ≈ 1.5 k CU per try and keeps one
//! position per `(market, borrower, nonce)`.

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use shortwin_math::consts::P_OPEN_MAX;
use shortwin_math::fees;
use shortwin_math::num::{mul_bps_u64, mul_div, Rounding, WAD};
use shortwin_math::pool::{self, SweepSource};
use shortwin_math::rent;
use shortwin_math::risk;
use shortwin_math::tier::Tier;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::caps::{self as engine_caps, CapInputs};
use crate::engine::guard::{self, mint_guard, MintPath};
use crate::engine::{lender, prologue, tier as engine_tier, waterfall};
use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::events::{Open, TraderSwept, SWEEP_TO_VEST};
use crate::state::{
    GlobalConfig, Market, MarketVault, OracleCache, ShortPosition, TierParams, VenueGuard,
    VenueKind, WalletExposure, EXTRAS_IX_SELL, POOL_VAULT_SEED, POSITION_OPEN,
};
use crate::venues::{self, Carry, ExtrasSel, Leg};

#[event_cpi]
#[derive(Accounts)]
#[instruction(nonce: u64)]
pub struct OpenShort<'info> {
    /// The borrower: pays for the position, the exposure record, the margin and the open fee.
    #[account(mut)]
    pub borrower: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// The venue-upgrade latch (A7): read only by opens.
    #[account(seeds = [VenueGuard::SEED], bump = venue_guard.load()?.bump)]
    pub venue_guard: AccountLoader<'info, VenueGuard>,
    /// The cached SOL/USD price for the tier (ARCH §4.7).
    #[account(seeds = [OracleCache::SEED], bump = oracle_cache.bump)]
    pub oracle_cache: Box<Account<'info, OracleCache>>,
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
        init,
        payer = borrower,
        space = ShortPosition::SPACE,
        seeds = [
            ShortPosition::SEED,
            market.key().as_ref(),
            borrower.key().as_ref(),
            &nonce.to_le_bytes()
        ],
        bump
    )]
    pub position: Box<Account<'info, ShortPosition>>,
    #[account(
        init_if_needed,
        payer = borrower,
        space = WalletExposure::SPACE,
        seeds = [WalletExposure::SEED, market.key().as_ref(), borrower.key().as_ref()],
        bump
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
    /// CHECK: `== market.trader` (`has_one`); system-owned, zero data, checked by the leg's
    /// normalisation.
    #[account(mut)]
    pub trader: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

/// What phase 1 (guards, prologue, caps) hands to the leg and the booking.
struct Staged {
    kind: VenueKind,
    now: i64,
    pool: shortwin_math::pool::Pool,
    vault: Box<shortwin_math::pool::Vault>,
    tier: Tier,
    tp: TierParams,
    min_out: u64,
    p_spot: u128,
    p_ema: u128,
    util_pre: u32,
    kink: u32,
    ema_xy: (u128, u128),
    market_bump: u8,
    /// The venue's one-way fee φ the caps were computed at (step 6): the market's
    /// `min_open_fee_ppm` low-water mark takes it (step 13; FIT-2).
    phi_ppm: u32,
}

#[allow(clippy::too_many_arguments)]
pub fn handler<'info>(
    ctx: Context<'info, OpenShort<'info>>,
    nonce: u64,
    q: u64,
    min_proceeds: u64,
    max_margin: u64,
    max_open_fee: u64,
    extras: ExtrasSel,
) -> Result<()> {
    require!(q > 0, ShortwinError::ZeroAmount);
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let position_key = ctx.accounts.position.key();
    let borrower_key = ctx.accounts.borrower.key();
    let config = ctx.accounts.config.load()?;

    // ---- phase 1: steps 1–7 --------------------------------------------------------------
    let (st, rem) = {
        let mut market = ctx.accounts.market.load_mut()?;
        let kind = market.venue_kind().ok_or(ShortwinError::NoSupportedVenue)?;
        guard::open_static(&config, &market, kind, clock.unix_timestamp)?;
        mint_guard(
            &market,
            &ctx.accounts.mint.to_account_info(),
            ctx.accounts.token_program.key,
            MintPath::Entry,
        )?;
        require!(
            market.total_shares.get() > 0,
            ShortwinError::InsufficientInventory
        );
        let deny = [
            ctx.accounts.config.key(),
            ctx.accounts.venue_guard.key(),
            ctx.accounts.oracle_cache.key(),
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
                ix: EXTRAS_IX_SELL,
                sel: extras,
            },
            &deny,
        )?;
        // M7-SEC-5: the pre-trade read also resolves the pump fee at `P_ema` (the caps' `φ`).
        let state = venues::read_state_boxed_for_open(
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
        let now = pro.now;
        // The venue sync moved the market off its venue (a curve that completed, or migrated
        // onto its canonical pool): no open.
        if let Some(t) = pro.transition {
            return venues::pump_curve::transition_refusal(t);
        }

        // Step 3: venue guards.
        guard::latch(&*ctx.accounts.venue_guard.load()?, kind)?;
        require!(state.executable, ShortwinError::NoRoute);
        require!(!pro.s3_no_route, ShortwinError::VenueFeeTooHigh);
        require!(
            !fees::exceeds_cap(state.fee_ppm, u64::from(config.params.venue_fee_cap_bps)),
            ShortwinError::VenueFeeTooHigh
        );
        if kind.is_raydium() {
            // Raydium (ARCH §2.6 rules 1 and 5): opens wait out the listing window after an
            // adoption and the price warm-up after a start, and the pool's effective real SOL —
            // bounded by its locked depth over `λ_min` — must reach `pool_min_real_sol`.
            let window_end = market
                .pool_seen_at
                .checked_add(i64::from(config.params.pool_list_window_s))
                .or_math()?;
            require_gte!(now, window_end, ShortwinError::PoolListWindow);
            // M4R (M4V-5 unified): a restarted observation state — a listing, an adoption —
            // must first finish its post-change warm-up (post-roll `ema_warm == 0`).
            require!(market.ema_warm == 0, ShortwinError::EmaWarmingUp);
            let r_eff = engine_tier::r_eff(&config, &market, &state, now)
                .ok_or(ShortwinError::PoolTooThin)?;
            require_gte!(
                r_eff,
                u128::from(config.params.pool_min_real_sol),
                ShortwinError::PoolTooThin
            );
        }
        if kind == VenueKind::PumpSwap {
            // PumpSwap (ARCH §3.4 step 3; A24, C0-10): any non-zero `disable_flags` blocks opens
            // (`VenueFlagged`; the buy bit already made the venue not executable, `NoRoute`,
            // T-DISABLE), and — the post-change confirmation a graduation or a price start marks,
            // as on Raydium (M4R; M6, flagged: ARCH §3.4 names it for Raydium) — the warm-up must
            // be over (`EmaWarmingUp`).
            let pool = state.pool.as_ref().ok_or(ShortwinError::NoRoute)?;
            require!(pool.disable_flags == 0, ShortwinError::VenueFlagged);
            require!(market.ema_warm == 0, ShortwinError::EmaWarmingUp);
        }
        if kind == VenueKind::PumpCurve {
            // M6-SEC-3 (owner decision of 2026-09-30; ARCH §3.4 step 3): no curve open while
            // PumpSwap's `create_pool` is halted (`disable_flags` bit 0, T-DISABLE; C6 of the
            // curve's ReadSet): pump's `migrate_v2` creates the canonical pool through it, so a
            // curve that completes under it cannot graduate. Positions already open are
            // unaffected (no exit reads the bit). An executable curve always has its pricing
            // inputs (C6 parsed), so a missing view here refuses as not executable.
            let pricing = state
                .curve
                .as_ref()
                .and_then(|c| c.pricing.as_ref())
                .ok_or(ShortwinError::NoRoute)?;
            require!(
                pricing.pool_disable_flags & venues::pump_amm::PAMM_CREATE_POOL_DISABLED_BIT == 0,
                ShortwinError::GraduationHalted
            );
        }

        // Step 4: open guards (the post-roll EMA; the pre-trade spot).
        let warm = market
            .created_at
            .checked_add(i64::from(config.params.market_warmup_s))
            .or_math()?;
        require_gte!(now, warm, ShortwinError::MarketWarmingUp);
        let p_ema = market.ema_price.get();
        let p_spot = state.price().ok_or(ShortwinError::NoRoute)?;
        require!(
            shortwin_math::caps::spot_near_ema(p_spot, p_ema, config.params.open_ema_dev_bps),
            ShortwinError::PriceDeviatesFromEma
        );
        require!(p_ema <= P_OPEN_MAX, ShortwinError::PriceAboveOpenMax);
        // PumpSwap boost gate (ARCH §3.4 step 4; A13, ADOPTED Q15, C1-2): no open while the
        // boost vault holds more than dust inside the window after graduation.
        if let Some(pool) = state.pool.as_ref() {
            require!(
                !venues::pump_amm::boost_blocks(pool, &config.params, market.graduated_at, now),
                ShortwinError::BoostInProgress
            );
        }

        // Step 5: tier.
        let tier = engine_tier::assign(&config, &market, &state, &ctx.accounts.oracle_cache, now)
            .ok_or(ShortwinError::NoSupportedVenue)?;
        market.tier = tier as u8;
        market.tier_computed_at = now;
        let tp = *config
            .tiers
            .get(tier.index())
            .ok_or(ShortwinError::InvalidParams)?;

        // Step 6: caps.
        engine_caps::check_open(
            &CapInputs {
                q,
                x_now: state.x,
                l_slow: market.liq_slow.get(),
                p_ema,
                // `(D, λ_min)`: Raydium `pool_lock_min_bps`; PumpSwap `PUMPSWAP_LOCK_MIN_BPS`
                // (100 %: sized on its burned LP only, OWNER-M6D-1; ARCH §4.6, row 57).
                raydium_lock: kind
                    .lock_min_bps(config.params.pool_lock_min_bps)
                    .map(|lambda_min| (market.pool_d.get(), lambda_min)),
                // M7-SEC-5: `min(φ_live, φ at P_ema)` on the pump venues (a sale in this
                // transaction cannot move the pool into a higher-fee tier for the caps).
                phi_ppm: state.cap_fee_ppm(),
                tokens_free: pro.pool.tokens_free,
                borrowed_q: pro.pool.borrowed_q,
                wallet_q: ctx.accounts.exposure.q_open,
                curve: curve_cap(&state, p_spot)?,
            },
            &tp,
            &config.params,
        )?;

        // Step 7: expected proceeds and the leg's bound.
        let s_exp = state
            .sell(q)
            .ok_or(ShortwinError::ProceedsBelowMin)?
            .proceeds;
        let tol = mul_bps_u64(
            s_exp,
            10_000u64.saturating_sub(u64::from(config.params.exec_tol_bps)),
            Rounding::Floor,
        )
        .math()?;
        let util_pre = rent::utilization_bps(pro.pool.tokens_free, pro.pool.borrowed_q).math()?;
        (
            Staged {
                kind,
                now,
                pool: pro.pool,
                vault,
                tier,
                tp,
                min_out: min_proceeds.max(tol),
                p_spot,
                p_ema,
                util_pre,
                kink: market.util_kink_bps,
                ema_xy: market.newest_ring_xy(),
                market_bump: market.bump,
                phi_ppm: state.cap_fee_ppm(),
            },
            rem,
        )
    };
    let Staged {
        kind,
        now,
        mut pool,
        mut vault,
        tier,
        tp,
        min_out,
        p_spot,
        p_ema,
        util_pre,
        kink,
        ema_xy,
        market_bump,
        phi_ppm,
    } = st;

    // ---- phase 2: step 8, the sell leg ---------------------------------------------------
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
    let ert = venues::effective_ert(kind, config.params.exec_rent_tol);
    let leg = Leg {
        kind,
        market: accs.market.to_account_info(),
        market_bump,
        pool_vault: accs.pool_vault.to_account_info(),
        mint: accs.mint.to_account_info(),
        decimals: accs.mint.decimals,
        token_program: accs.token_program.to_account_info(),
        sol_first,
        ert,
        trader,
        read: rem.read,
        trade: rem.trade,
        extra_metas: rem.extra_metas,
        extra_infos: rem.extra_infos,
    };
    let borrower_info = accs.borrower.to_account_info();
    let position_info = accs.position.to_account_info();
    let sold = venues::sell(&leg, &borrower_info, &position_info, q, min_out)?;
    let s = sold.s;
    require_gte!(s, min_out, ShortwinError::ProceedsBelowMin);

    // ---- step 9: the post-open state, notional ---------------------------------------------
    let post = {
        let m = accs.market.load()?;
        venues::read_state_boxed(
            &m,
            kind,
            rem.read,
            clock.unix_timestamp,
            Some(accs.mint.supply),
        )
    };
    let n_open = post.buy(q).ok_or(ShortwinError::VenueResultMismatch)?.cost;
    let b_ema = post.buy_at(q, ema_xy.0, ema_xy.1).map(|b| b.cost);
    require_gte!(
        n_open,
        config.params.min_notional,
        ShortwinError::BelowMinNotional
    );
    let open_total = {
        let m = accs.market.load()?;
        m.total_notional_open
    };
    require!(
        shortwin_math::caps::notional_ok(
            n_open,
            open_total,
            config.params.min_notional,
            config.params.market_max_notional
        ),
        ShortwinError::MarketNotionalCap
    );

    // ---- step 10: margin (D4) --------------------------------------------------------------
    let m = risk::margin_d4(s, n_open, b_ema, tp.cr_open_bps).math()?;
    require_gte!(max_margin, m, ShortwinError::MarginAboveMax);
    if m > 0 {
        system_program::transfer(
            CpiContext::new(
                accs.system_program.key(),
                Transfer {
                    from: borrower_info.clone(),
                    to: position_info.clone(),
                },
            ),
            m,
        )?;
    }

    // ---- step 11: open fee -----------------------------------------------------------------
    let fee_open = risk::open_fee(n_open, config.params.open_fee_bps).math()?;
    require_gte!(max_open_fee, fee_open, ShortwinError::OpenFeeAboveMax);
    if fee_open > 0 {
        system_program::transfer(
            CpiContext::new(
                accs.system_program.key(),
                Transfer {
                    from: borrower_info.clone(),
                    to: accs.market_vault.to_account_info(),
                },
            ),
            fee_open,
        )?;
        vault.protocol_fees = vault.protocol_fees.checked_add(fee_open).or_math()?;
    }

    // ---- steps 12–13: the position and the market ----------------------------------------
    let v = s.checked_add(m).or_math()?;
    let entry_price = mul_div(u128::from(s), WAD, u128::from(q), Rounding::Floor).math()?;
    let (basis_j, deadline) = {
        let mk = accs.market.load()?;
        let deadline = now.checked_add(i64::from(tp.max_duration_s)).or_math()?;
        (mk.basis().j, deadline)
    };
    let r_min =
        rent::min_rent(q, p_ema, &tp.to_math().rate, util_pre, kink, tp.min_rent_s).math()?;
    let position_bump = ctx.bumps.position;
    let exposure_bump = ctx.bumps.exposure;
    let pos = &mut ctx.accounts.position;
    pos.borrower = borrower_key;
    pos.market = market_key;
    pos.nonce = nonce;
    pos.bump = position_bump;
    pos.status = POSITION_OPEN;
    pos.tier = tier as u8;
    pos.venue_at_open = kind as u8;
    pos.q = q;
    pos.s = s;
    pos.m = m;
    pos.v = v;
    pos.n_open = n_open;
    pos.fee_open = fee_open;
    pos.entry_price = entry_price;
    pos.j_open = basis_j;
    pos.r_min = r_min;
    pos.t_open = now;
    pos.deadline = deadline;
    pos.lock_terms(&config.params, &tp, ert)?;
    let release_at = pos.release_at()?;
    let pos_snapshot = (**pos).clone();

    let exposure = &mut ctx.accounts.exposure;
    if exposure.wallet == Pubkey::default() {
        exposure.market = market_key;
        exposure.wallet = borrower_key;
        exposure.bump = exposure_bump;
    }
    exposure.q_open = exposure.q_open.checked_add(q).or_math()?;

    pool::borrow(&mut pool, now, q, release_at).math()?;
    let (t_after, tracked) = {
        let mut market = ctx.accounts.market.load_mut()?;
        let mut aggr = market.aggr();
        waterfall::add_position(&mut market, &mut aggr, &pos_snapshot, phi_ppm)?;
        market.set_aggr(&aggr);
        market.set_pool(&pool);
        let mut va = ctx.accounts.market_vault.load_mut()?;
        va.set_vault(&vault);
        (pool.tokens_free, lender::tracked(&vault)?)
    };

    // Post-conditions (step 10 of ARCH §3.0; I2, I5, I7).
    leg.trader
        .post_conditions(&ctx.accounts.pool_vault.to_account_info())?;
    let pv = crate::engine::trader::token_state(&ctx.accounts.pool_vault.to_account_info())?;
    require_gte!(pv.amount, t_after, ShortwinError::AccountMismatch);
    lender::check_vault_floor(&ctx.accounts.market_vault.to_account_info(), tracked)?;
    require!(
        shortwin_math::caps::util_ok(
            pool.tokens_free,
            pool.borrowed_q,
            0,
            config.params.util_cap_bps
        )
        .math()?,
        ShortwinError::CapUtilization
    );

    // The sell leg's WSOL rent (and on PumpSwap the venue-side rent netted out of `S`) goes back
    // to the borrower. A direct move, so after the last CPI that names the position or the
    // borrower (the event CPI names neither).
    crate::lamports::move_lamports(
        &ctx.accounts.position.to_account_info(),
        &ctx.accounts.borrower.to_account_info(),
        sold.refund,
    )?;

    emit_cpi!(Open {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        position: position_key,
        borrower: borrower_key,
        nonce,
        q,
        s,
        m,
        v,
        n_open,
        fee_open,
        tier: tier as u8,
        entry_price,
        spot_wad: p_spot,
        ema_wad: p_ema,
        b_spot: n_open,
        b_ema: b_ema.unwrap_or(0),
        deadline,
        venue: kind as u8,
        venue_rent: sold.venue_rent,
    });
    Ok(())
}

/// The curve caps' input (ARCH §4.6; M5-SEC-3, `engine::caps::curve_reserves`): the live curve
/// before the open and its spot; `None` off the curve.
fn curve_cap(state: &venues::VenueState, p_spot: u128) -> Result<Option<engine_caps::CurveCap>> {
    let to_u64 = |v: u128| u64::try_from(v).map_err(|_| error!(ShortwinError::MathOverflow));
    match (state.kind, state.curve.as_ref()) {
        (VenueKind::PumpCurve, Some(c)) => Ok(Some(engine_caps::CurveCap {
            vt: to_u64(c.live.vt)?,
            vq: to_u64(c.live.vq)?,
            rt: to_u64(c.live.rt)?,
            p_spot,
        })),
        (VenueKind::PumpCurve, None) => err!(ShortwinError::NoRoute),
        _ => Ok(None),
    }
}
