//! `execute_pool_switch()` (ARCH §2.6 rule 3, §3.9; A71, A72; V1-7, V2-1, V2-7, R3A-4,
//! R3B-8; the M4 security review): **collection windows only**. After a window of
//! `pool_fast_window_s` has run, anyone resolves it: the best candidate seen (`fp_*`) is
//! adopted if it is still eligible and — on a Raydium market — the `pool_fail` chain that armed
//! the window still holds (M4-SEC-2: `pool_fail` now and `pool_fail_since + pool_fast_route_s ≤
//! fp_t0`, so neither a recovery and a new failure nor a one-second failure after hours adopts
//! a stale window), whether or not positions are open; otherwise the window is cleared. A
//! list-only market with open positions (M5, ARCH §2.5; M4-SEC-4) resolves its window the same
//! way, without a chain (it has no venue at all), unless its detection set now shows a pump
//! venue (D1: cancelled). There is no pending switch for any other path (decision 7): listings,
//! listing-window replacements and zero-position adoptions happen inside `propose_pool`.
//!
//! **Accounts:** as `propose_pool` — `config`, `venue_guard` (read), `market` (mut), `mint`,
//! `oracle_cache` (read: the tier of the window's candidate, step 4), event accounts.
//! **Remaining:** the incumbent's ReadSet (the market's pool cache; on a list-only market its
//! detection set `[bonding_curve, canonical_pool]`), then the window candidate's ReadSet,
//! key-checked slot by slot against `fp_keys` (its adapter's order, `fp_venue` its kind).
//!
//! **Order:**
//!
//! 1. `mint.owner == market.token_program` (`AccountMismatch`); a Raydium or list-only market
//!    with a window (`NoPendingSwitch`: no window, or a market that never has one); the window
//!    has run (`now ≥ fp_t0 + pool_fast_window_s`, else `PoolSwitchNotDue`).
//! 2. The remaining accounts (`BadRemainingAccounts`, `VenueAccountMismatch`; a list-only
//!    market's `classify_pump` errors).
//! 3. The prologue on the incumbent (ARCH §3.0 steps 2, 4–8; no `MarketVault`, as
//!    `crank_price`): its observation if executable, the chains — so "the incumbent still fails
//!    `pool_fail`" and "the chain still holds" are this instruction's own observation of it,
//!    and never the Emergency route-broken flag (`pool_fail` does not read it, V1-7).
//! 4. The candidate verified again (`eligible()` now at the market's `P_ema`, the same pool the
//!    window recorded: its `window_keys` equal `fp_keys`) and the window floor (its locked SOL at
//!    `P_ema` alone `≥ pool_min_real_sol`, `pool_select::window_floor_ok`; M4V-1), and **switch
//!    only if all fit** (owner decision of 2026-09-29; `pool_select::carried_fit`): every open
//!    position, bounded by `min(max_q_hwm, ΣB_Q)`, fits the candidate's per-position impact and
//!    fee-scaled size caps on its conservative locked-depth `X₀`, and `ΣB_Q` its per-market caps
//!    (the fee-scaled caps at `min(min_open_fee_ppm, φ_cand, CARRIED_FEE_CAP_PPM)`; FIT-2,
//!    FITS-1), on the tier the market
//!    would have on it, at `min(P_ema, P_cand)` and at `P_ema` — judged
//!    on the terms no transaction can move (`FitAt::Execution`: `X₀` without the candidate's live
//!    token reserve, which an unlocked LP's withdrawal or a buy wrapped around this instruction
//!    would lower; M4-SEC-1 / M4-SEC-3's principle), and only for a window that may still be
//!    adopted. Any failure is "no longer eligible", never a revert: the window is then cancelled
//!    (with no candidate that fits, the positions exit on the failed pool through the S3 chain at
//!    fair value).
//! 5. Adopt if the candidate is adoptable now — eligible (step 4), the arming chain unbroken on a
//!    Raydium market (M4-SEC-2), no pump venue on a list-only market (D1), and its program not
//!    latched (M4V-4) — through `pool_select::adopt`: the ring rewritten to the anchor
//!    `⌊D·10⁴/λ_min⌋` — no sample of the pool, so neither a deposit nor an unlocked LP's
//!    withdrawal around this instruction changes anything (M4R-1, M4-SEC-5, M4V-2) — `L_slow =
//!    min(L_slow, D)`, the state marked for the post-change warm-up (`ema_warm = 8`, `ema_guard =
//!    1`: opens and keeper exits wait for 8 integrations of the new pool, which re-seed `P_ema`
//!    and `L_ema`; M4R), chains and window cleared, age kept, `pool_seen_at` per the
//!    listing-window rule; event `PoolSwitched`, kind fast.
//!    Otherwise cancel (`clear_window`; event `PoolSwitchCancelled`). A candidate whose program
//!    is flagged in `VenueGuard` (M4-SEC-8: its compiled reader is the one at risk) is **not
//!    adoptable now** and cancels (M4V-4): a reverting execution kept the window, and with it
//!    `PoolWindowClosed` for every other candidate — the other Raydium program's included —
//!    until the Emergency review. The chain stays armed, so a proposal on the unflagged program
//!    opens a new window at once, and after the review the flagged program's pool can be
//!    proposed again.
//!
//! Open positions keep every locked term (A26) and exit on the new pool after the warm-up; `P_ema`
//! and the ring's prices carry over until the warm-up re-seeds `P_ema` from the new pool's
//! bucket lows.

use anchor_lang::prelude::*;
use shortwin_math::pool::Vault;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::guard;
use crate::engine::pool_select::{self, FitAt, SelectParams, Situation};
use crate::engine::prologue;
use crate::errors::ShortwinError;
use crate::events::{PoolSwitchCancelled, PoolSwitched, POOL_KIND_FAST};
use crate::instructions::propose_pool::incumbent;
use crate::state::{
    GlobalConfig, Market, OracleCache, VenueGuard, VenueKind, MARKET_FLAG_GRADUATED_ELSEWHERE,
};
use crate::venues;

#[event_cpi]
#[derive(Accounts)]
pub struct ExecutePoolSwitch<'info> {
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// The venue-upgrade latch: a window whose candidate's program bit is set is cancelled, not
    /// adopted (module docs step 5; M4-SEC-8, M4V-4). Read only.
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
    /// the handler).
    pub mint: UncheckedAccount<'info>,
    /// The cached SOL/USD price: the tier the market would have on the window's candidate, whose
    /// opening caps its open positions must fit ("switch only if all fit"; module docs step 4).
    /// Read only.
    #[account(seeds = [OracleCache::SEED], bump = oracle_cache.bump)]
    pub oracle_cache: Box<Account<'info, OracleCache>>,
}

pub fn handler<'info>(ctx: Context<'info, ExecutePoolSwitch<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let config = ctx.accounts.config.load()?;
    let mut market = ctx.accounts.market.load_mut()?;
    require_keys_eq!(
        *ctx.accounts.mint.owner,
        market.token_program,
        ShortwinError::AccountMismatch
    );
    let kind = market.venue_kind().ok_or(ShortwinError::NoPendingSwitch)?;
    require!(
        (kind.is_raydium() || kind == VenueKind::None) && market.fp_t0 != 0,
        ShortwinError::NoPendingSwitch
    );
    let cand_kind = VenueKind::from_u8(market.fp_venue)
        .filter(|k| k.is_raydium())
        .ok_or(ShortwinError::NoPendingSwitch)?;
    let p = SelectParams::of(&config.params);
    let ends = pool_select::window_ends(market.fp_t0, p.fast_window_s)?;
    require_gte!(
        prologue::market_now(&market, clock.unix_timestamp),
        ends,
        ShortwinError::PoolSwitchNotDue
    );

    // Step 2: the incumbent's ReadSet (a list-only market's detection set), then exactly the
    // window candidate's.
    let cl = venues::read_len(cand_kind)?;
    let inc = incumbent(&market, kind, ctx.remaining_accounts, clock.unix_timestamp)?;
    let candidate = inc.candidate;
    require!(candidate.len() == cl, ShortwinError::BadRemainingAccounts);
    for (info, key) in candidate.iter().zip(market.fp_keys.iter()) {
        require_keys_eq!(info.key(), *key, ShortwinError::VenueAccountMismatch);
    }

    // Step 3: the prologue on the incumbent.
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

    // Step 4: still eligible, still the recorded pool, still above the window floor (M4V-1), and
    // every open position still fits its opening caps (switch only if all fit).
    let p_ref = pool_select::reference_price(&market);
    let fit = pool_select::FitContext::of(&config, &ctx.accounts.oracle_cache, pro.now);
    let situation = Situation::of(&market, kind, pro.now, pro.pool_fail);
    // M4V-4: a latched candidate program is "not adoptable now" — a cancellation, not a revert.
    let latched = guard::candidate_latched(&*ctx.accounts.venue_guard.load()?, cand_kind);
    let adoptable = !inc.pump_venue && !latched && pool_select::window_adoptable(&situation, &p);
    let adoption = pool_select::verify_candidate(
        cand_kind,
        candidate,
        &market.mint,
        &market.token_program,
        &config.params,
        clock.unix_timestamp,
        p_ref,
    )
    .ok()
    .filter(|a| a.window_keys() == market.fp_keys)
    .filter(|a| pool_select::window_floor_ok(a, &config.params, p_ref))
    // The fit last, and only for a window that may still be adopted (its CU).
    .filter(|a| {
        adoptable
            && pool_select::carried_fit(&market, a, &fit, p_ref, FitAt::Execution)
                .is_ok_and(|f| f.fits())
    });
    let d_inc = market.pool_d.get();
    let best = market.fp_keys.first().copied().unwrap_or_default();
    let d_best = market.fp_d.get();

    // Step 5: adopt or cancel.
    match adoption {
        Some(a) if adoptable => {
            pool_select::adopt(
                &mut market,
                &a,
                pro.now,
                p.list_window_s,
                config.params.pool_lock_min_bps,
            )?;
            if inc.graduated_elsewhere {
                market.set_flag(MARKET_FLAG_GRADUATED_ELSEWHERE, true);
            }
            let event = PoolSwitched {
                market: market_key,
                slot: clock.slot,
                ts: clock.unix_timestamp,
                candidate: a.pool,
                kind: POOL_KIND_FAST,
                d_cand: a.state.locked_depth,
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
        _ => {
            pool_select::clear_window(&mut market);
            let event = PoolSwitchCancelled {
                market: market_key,
                slot: clock.slot,
                ts: clock.unix_timestamp,
                candidate: best,
                kind: POOL_KIND_FAST,
                d_cand: d_best,
                d_inc,
                window_ends: ends,
            };
            drop(market);
            drop(config);
            emit_cpi!(event);
        }
    }
    Ok(())
}
