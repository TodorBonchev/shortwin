//! I15 (ARCH §8.3; M4): Raydium pool selection, checked by `check_ix` against the state just
//! before each instruction (`super::positions::PositionsSnapshot`) and the instruction's own
//! `PoolSwitched` / `PoolProposed` / `PoolSwitchCancelled` / `Open` events.
//!
//! | Part | What is checked |
//! |---|---|
//! | adoption only by event | a market's pool (venue byte and pool cache) changes to a Raydium pool only in an instruction that emits one `PoolSwitched` for it, naming that pool |
//! | Raydium listings | `create_market(venue_hint)` listed the event's pool, eligible, with `pool_d` its `D`, the age of O13 / O13b and `pool_seen_at` now |
//! | eligible when adopted | the adopted pool, read by the harness from its own accounts (`venue_view`, the LP counter and mint), is executable, its one-way fee ≤ `venue_fee_cap_bps`, and its locked depth passes the floor `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` at the market's `P_ema` after the instruction (which is the price the program judged at: `P_ema` carries over an adoption, and a market without a price starts it at the pool's spot) — M4-SEC-1 / M4-SEC-3, no live `R` or `λ` clause; a window's adoption (kind fast: it carries open positions) also the **window floor**, its locked SOL `⌊D·isqrt(P_ref)/10⁹⌋ ≥ pool_min_real_sol` without the `/λ_min` (M4V-1); the event's `d_cand` and the market's `pool_d` are its `D` |
//! | who may adopt | `listing`: the market was list-only **with no open position** (M4-SEC-4); `listing window`: no open position, inside the window, `D ≥ (1 + pool_list_margin_bps)·D_inc`; `zero-position`: no open position, after the window, `D ≥ (1 + pool_switch_margin_bps)·D_inc`; `fast`: a collection window ran its `pool_fast_window_s` and recorded this pool, and — on a Raydium market — the incumbent failed `pool_fail` on the state just before (never the route-broken flag) **and the chain that armed the window held** (the chain after this observation started `≥ pool_fast_route_s` before `fp_t0`, M4-SEC-2); a list-only market's window (positions open) needs no chain. `D_inc` is the incumbent's `D` on that state if it was executable, else its stored `pool_d` |
//! | the adoption's effects (A72, T-VENUE-SWITCH; M4R, **PROPOSED**) | on a market with a price `L_ema` and every ring entry's `l = llo = lhi` are the anchor `⌊D·10⁴/λ_min⌋` with `(x, y) = reprice(l, p)` recomputed (no sample of the pool), `L_slow ≤ D` (exactly `min(L_slow, D)` when the instruction integrated nothing), `ema_warm = 8`, `ema_guard = 1`, the ring's prices and price extremes kept (bar the entry this instruction integrated); a list-only market that never had a price: `L_ema = L_slow = min(√(X·Y), ⌊D·10⁴/λ_min⌋)`, marked; the bucket, both route chains and the window cleared; `graduated_at` kept (a list-only market: the pool's age start, O13); `pool_seen_at` kept inside a listing window, else now; the event's `window_ends` is the listing window's end. A Raydium listing starts the same marked price state |
//! | the collection window | `PoolProposed` records its candidate in `fp_*`; on a Raydium market it opens only on an armed fast path (`pool_fail` just before, the chain `≥ pool_fast_route_s` old after) with `D ≥ pool_fast_min_bps·pool_d`, and is improved only inside the window, while its arming chain holds, by a strictly deeper candidate other than its own; on a list-only market only with open positions, improved inside the window by a strictly deeper candidate; the window's candidate is eligible at `P_ema` and passes the window floor (M4V-1) at `min(P_ema, P_cand)` (M4R-2); `fp_*` changes only with one of the three events; a `PoolSwitchCancelled` resolves a window that ran out |
//! | switch only if all fit (owner decision of 2026-09-29) | an adoption that carries open positions (kind fast) happened, and a window proposal on a market with open positions was accepted, only if **every open position** (its own `q`, not the program's bound) fits the candidate's per-position impact cap and fee-scaled size cap on its conservative locked-depth `X₀ = min(X, L_slow'/√P, D/(λ_min·√P))` (`L_slow'` what the adoption leaves: `min(L_slow, D)`, or the pool's `min(√(X·Y), D/λ_min)` without a price), and `ΣQ` its per-market impact and fee-scaled OI caps at `X₀`, with the impact caps of the tier the market would have on the candidate and the fee-scaled caps at `φ = min(min_open_fee_ppm, φ_cand, CARRIED_FEE_CAP_PPM)` (FIT-2, FITS-1; `min(φ_cand, CARRIED_FEE_CAP_PPM)` for a zero mark) (the harness's own `tier::assign` on the candidate's `D`, `R`, `L_slow'`, the market's age — a list-only market's: the candidate's — and the cached oracle), at `P = min(P_ema, P_cand)` and at `P_ema` — a proposal's `X₀` with the candidate's live `X` (as an open), an adoption's without it (`execute_pool_switch` judges on the terms no transaction can move); `max_q_hwm` is at or above every open position's `q`, and both marks are 0 with none open (state part, `check_hwm`); `max_q_hwm` falls only at the exit that empties the market, and `min_open_fee_ppm` changes only at an open — to `open_fee_mark(mark, φ)`, `φ` the venue's fee on the pre-trade state — and at that exit (FIT-2) |
//! | opens sized on locked depth (V2-8) | an `Open` on a Raydium market happened after the listing window and the post-change warm-up (`ema_warm == 0` after the roll, M4R / M4V-5), with the fee ≤ `venue_fee_cap_bps`, `R_eff = min(R_now, R_slow, D·√P_ema/λ_min) ≥ pool_min_real_sol`, `pool_d` the pool's `D` on the pre-trade state, and `q` within the per-position impact cap on `X₀ = min(X_now, L_slow/√P_ema, D/(λ_min·√P_ema))` |
//! | PumpSwap opens sized on the burned LP (OWNER-M6D-1) | an `Open` on a PumpSwap market (ARCH row 57, A82; ECON #106): `pool_d` is the burned LP's `D` on the pre-trade state (the harness's own view of the pool and its LP mint), `q` within the per-position impact and fee-scaled caps and `ΣQ + q` within the per-market cap on `X₀ = min(X_now, L_slow/√P_ema, D/√P_ema)` — `λ_min` = 100 %, written out here, not the program's `PUMPSWAP_LOCK_MIN_BPS` — and the open's tier no better than the burned LP's real SOL `max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)` alone earns at the cached oracle |
//!
//! I17's M4 part (no adoption and no window candidate while **the candidate's own** Raydium
//! program is flagged — its program's bit, not the whole Raydium mask, M4-SEC-8; a latched
//! window candidate cancels, M4V-4) is `super::i17_no_adoption_on_flagged_venue`.

use anchor_lang::prelude::Pubkey;
use litesvm::LiteSVM;
use shortwin::constants::TOKEN_PROGRAM_ID;
use shortwin::events::{
    MarketCreated, Open, POOL_KIND_FAST, POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW,
    POOL_KIND_ZERO_POSITION, PoolProposed, PoolSwitchCancelled, PoolSwitched,
};
use shortwin::state::{
    GlobalConfig, MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN, Market, OracleCache, ShortPosition, VenueKind,
    raydium_cache,
};
use shortwin_math::consts::EMA_WARMUP_BUCKETS;
use shortwin_math::fees::exceeds_cap;
use shortwin_math::pool_select::{
    depth_at_lambda, dominates, fast_path_qualifies, locked_depth, locked_floor_ok,
    position_floor_ok,
};
use shortwin_math::quote::{liquidity, reprice};
use shortwin_math::settle;
use shortwin_math::tier::{self, OraclePrice, TierInputs, TierVenue};
use shortwin_venue_state::{mint, raydium_cpmm, raydium_v4};
use solana_clock::Clock;

use super::positions::{MarketPositions, PositionsSnapshot};
use super::venue_view::{self, VenueView};
use super::{Violation, ensure, fail};
use crate::read;

/// The events of one instruction that I15 reads.
pub struct PoolFacts<'a> {
    /// Listings: a Raydium listing (`create_market` with a hint) is an adoption too.
    pub created: &'a [MarketCreated],
    pub opens: &'a [Open],
    pub switched: &'a [PoolSwitched],
    pub proposed: &'a [PoolProposed],
    pub cancelled: &'a [PoolSwitchCancelled],
    /// Venue transitions (M5): `crank_price`'s `None →` pump re-detection clears a collection
    /// window (D1; ARCH §2.5, M4's carry-over).
    pub venue_changed: &'a [shortwin::events::VenueStateChanged],
}

/// A Raydium pool as the harness reads it from its own accounts: the view (executable,
/// reserves, fee), the LP counter and the LP mint's supply.
#[derive(Clone, Copy, Debug)]
pub struct PoolRead {
    pub view: VenueView,
    pub counter: u64,
    pub supply: u64,
}

impl PoolRead {
    /// `D` on the view's reserves (0 if not executable).
    pub fn depth(&self) -> u128 {
        self.view.reserves.map_or(0, |(x, y)| {
            locked_depth(self.counter, self.supply, x, y).unwrap_or(0)
        })
    }
}

fn lp_supply(svm: &LiteSVM, key: &Pubkey) -> Option<u64> {
    let acc = svm.get_account(key)?;
    if acc.owner != TOKEN_PROGRAM_ID {
        return None;
    }
    mint::read_base(&acc.owner.to_bytes(), &acc.data)
        .ok()
        .map(|m| m.supply)
}

/// The LP counter of the pool `m`'s cache names (AMM v4 `lp_amount`, CPMM `lp_supply`) and its
/// LP mint's supply.
pub fn lp_of(svm: &LiteSVM, m: &Market) -> Option<(u64, u64)> {
    let acc = svm.get_account(&m.pool_cache[raydium_cache::RAY_POOL])?;
    let counter = match m.venue_kind()? {
        VenueKind::RaydiumV4 => {
            raydium_v4::read(&acc.owner.to_bytes(), &acc.data)
                .ok()?
                .lp_amount
        }
        VenueKind::RaydiumCpmm => {
            raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data)
                .ok()?
                .lp_supply
        }
        _ => return None,
    };
    let supply = lp_supply(svm, &m.pool_cache[raydium_cache::RAY_LP_MINT])?;
    Some((counter, supply))
}

/// `m` re-pointed at the Raydium pool `pool` of kind `kind`, with the cache and SOL side the
/// pool's own accounts give (what an adoption of it would write), `None` if it is not a WSOL
/// pool of `m`'s mint.
pub fn market_on(svm: &LiteSVM, m: &Market, kind: VenueKind, pool: &Pubkey) -> Option<Market> {
    let acc = svm.get_account(pool)?;
    let mut out = *m;
    out.venue = kind as u8;
    out.pool_cache = [Pubkey::default(); 10];
    let k = Pubkey::new_from_array;
    let (sol_first, vault_sol, vault_tok, config, observation, lp) = match kind {
        VenueKind::RaydiumV4 => {
            let i = raydium_v4::read(&acc.owner.to_bytes(), &acc.data).ok()?;
            let s = i.sol_is_coin()?;
            let (vs, vt) = if s {
                (i.coin_vault, i.pc_vault)
            } else {
                (i.pc_vault, i.coin_vault)
            };
            (s, vs, vt, [0; 32], [0; 32], i.lp_mint)
        }
        VenueKind::RaydiumCpmm => {
            let p = raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data).ok()?;
            let s = p.sol_is_token0()?;
            let (vs, vt) = if s {
                (p.token_0_vault, p.token_1_vault)
            } else {
                (p.token_1_vault, p.token_0_vault)
            };
            (s, vs, vt, p.amm_config, p.observation_key, p.lp_mint)
        }
        _ => return None,
    };
    out.pool_cache[raydium_cache::RAY_POOL] = *pool;
    out.pool_cache[raydium_cache::RAY_VAULT_SOL] = k(vault_sol);
    out.pool_cache[raydium_cache::RAY_VAULT_TOK] = k(vault_tok);
    out.pool_cache[raydium_cache::RAY_AMM_CONFIG] = k(config);
    out.pool_cache[raydium_cache::RAY_OBSERVATION] = k(observation);
    out.pool_cache[raydium_cache::RAY_LP_MINT] = k(lp);
    out.pool_cache[raydium_cache::RAY_TOK_PROGRAM] = m.token_program;
    out.set_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN, sol_first);
    Some(out)
}

/// The pool `m`'s cache names, read from its accounts.
pub fn read_pool(svm: &LiteSVM, m: &Market) -> Option<PoolRead> {
    let view = venue_view::view(svm, m)?;
    let (counter, supply) = lp_of(svm, m)?;
    Some(PoolRead {
        view,
        counter,
        supply,
    })
}

/// `eligible()` (ARCH §2.6 as amended by M4-SEC-1 / M4-SEC-3) of a pool read by the harness at
/// the reference price `p_ref`, with the failing clause.
fn eligible(p: &PoolRead, config: &GlobalConfig, p_ref: u128) -> Result<(), String> {
    let params = &config.params;
    if !p.view.executable {
        return Err("not executable".into());
    }
    if exceeds_cap(p.view.fee_ppm, u64::from(params.venue_fee_cap_bps)) {
        return Err(format!("fee {} ppm above the cap", p.view.fee_ppm));
    }
    let d = p.depth();
    if !locked_floor_ok(
        d,
        p_ref,
        u64::from(params.pool_lock_min_bps),
        params.pool_min_real_sol,
    ) {
        return Err(format!(
            "locked depth {d} under the floor at P_ref {p_ref} (λ_min {}, floor {})",
            params.pool_lock_min_bps, params.pool_min_real_sol
        ));
    }
    Ok(())
}

/// The window floor of a collection window's candidate (M4V-1): its locked SOL at `p_ref` alone
/// reaches `pool_min_real_sol`.
fn window_floor(p: &PoolRead, config: &GlobalConfig, p_ref: u128) -> Result<(), String> {
    let d = p.depth();
    if position_floor_ok(d, p_ref, config.params.pool_min_real_sol) {
        Ok(())
    } else {
        Err(format!(
            "locked depth {d} at P_ref {p_ref} holds less than pool_min_real_sol {} of locked SOL \
             (the window floor, M4V-1)",
            config.params.pool_min_real_sol
        ))
    }
}

/// The candidate's opening caps at one price (switch only if all fit): `X₀`, the per-position cap
/// `min(max_q_position(X₀, ι_pos), fee_size_cap(X₀, φ))`, the per-market cap `oi_cap(X₀, ι_mkt,
/// φ)`, and the tier they come from.
#[derive(Clone, Copy, Debug)]
pub struct FitCaps {
    pub x0: u64,
    pub pos: u64,
    pub oi: u64,
    pub tier: tier::Tier,
}

/// [`FitCaps`] of the candidate `p` at `price` (module docs): `X₀ = conservative_x0(X, l_slow,
/// P, (D, λ_min))` — at an execution (`live = false`) without the live `X`, the terms no
/// transaction can move —, the tier the market would have on the candidate (`tier::assign`: its
/// `D`, `R = Y`, `Y(reprice(l_slow, P))`, `graduated_at`, the cached oracle), the caps of that
/// tier; the fee-scaled caps at `φ = min(fee_mark, φ_cand, CARRIED_FEE_CAP_PPM)` (FIT-2:
/// `fee_mark` the market's `min_open_fee_ppm`, whose moves I15 checks at every open and exit, 0
/// bounds nothing; FITS-1: the 25-bps ceiling).
#[allow(clippy::too_many_arguments)]
fn fit_caps(
    svm: &LiteSVM,
    config: &GlobalConfig,
    p: &PoolRead,
    l_slow: u128,
    graduated_at: i64,
    price: u128,
    t: i64,
    live: bool,
    fee_mark: u32,
) -> Result<FitCaps, String> {
    let params = &config.params;
    let Some((x, y)) = p.view.reserves else {
        return Err("the candidate has no reserves".into());
    };
    let d = p.depth();
    let lambda_min = params.pool_lock_min_bps;
    let oracle = read::borsh::<OracleCache>(svm, &crate::pda::oracle_cache()).map_or(
        OraclePrice::default(),
        |o| OraclePrice {
            price: o.price,
            conf: o.conf,
            expo: o.expo,
            publish_time: o.publish_time,
        },
    );
    let x_now = if live { x } else { u64::MAX };
    let x0 = shortwin_math::caps::conservative_x0(x_now, l_slow, price, Some((d, lambda_min)))
        .map_err(|e| format!("X₀ at P {price}: {e:?}"))?;
    let t_ = tier::assign(&TierInputs {
        venue: TierVenue::Raydium {
            locked_depth: d,
            lambda_min_bps: lambda_min,
        },
        r_now: y,
        y_slow: reprice(l_slow, price).map_or(0, |(_, y)| y),
        vqr: 0,
        p_ema: price,
        now: t,
        graduated_at,
        oracle,
        oracle_stale_s: params.oracle_stale_s,
        theta_a_usd_e6: params.theta_a_usd_e6,
        theta_b_usd_e6: params.theta_b_usd_e6,
    });
    let tp = config.tiers[t_.index()];
    let phi = if fee_mark == 0 {
        p.view.fee_ppm
    } else {
        fee_mark.min(p.view.fee_ppm)
    }
    .min(shortwin_math::consts::CARRIED_FEE_CAP_PPM);
    let impact = shortwin_math::caps::max_q_position(x0, tp.impact_pos_bps).unwrap_or(0);
    let fee = shortwin_math::caps::fee_size_cap(x0, phi, params.fee_size_cap_x100)
        .ok()
        .flatten()
        .unwrap_or(u64::MAX);
    let oi = shortwin_math::caps::oi_cap(x0, tp.impact_mkt_bps, phi, params.fee_oi_cap_x100)
        .unwrap_or(0);
    Ok(FitCaps {
        x0,
        pos: impact.min(fee),
        oi,
        tier: t_,
    })
}

/// "Switch only if all fit" (owner decision of 2026-09-29; module docs), recomputed from the
/// candidate `p`'s own accounts and **every open position's own `q`**: at each price of `prices`
/// ([`fit_caps`]; `live`: a proposal's `X₀` with the live `X`; `fee_mark`: the market's
/// `min_open_fee_ppm`), every `q` within the per-position cap and `ΣQ` within the per-market cap.
#[allow(clippy::too_many_arguments)]
fn carried_fit(
    svm: &LiteSVM,
    config: &GlobalConfig,
    positions: &[(Pubkey, ShortPosition, u64)],
    p: &PoolRead,
    l_slow: u128,
    graduated_at: i64,
    prices: &[u128],
    t: i64,
    live: bool,
    fee_mark: u32,
) -> Result<(), String> {
    if positions.is_empty() {
        return Ok(());
    }
    let sum: u128 = positions.iter().map(|(_, pos, _)| u128::from(pos.q)).sum();
    for &price in prices {
        let c = fit_caps(
            svm,
            config,
            p,
            l_slow,
            graduated_at,
            price,
            t,
            live,
            fee_mark,
        )?;
        for (pk, pos, _) in positions {
            if pos.q > c.pos {
                return Err(format!(
                    "position {pk}: q {} above the candidate's per-position cap {} on X₀ {} at P \
                     {price}, tier {:?}",
                    pos.q, c.pos, c.x0, c.tier
                ));
            }
        }
        if sum > u128::from(c.oi) {
            return Err(format!(
                "ΣQ {sum} above the candidate's OI cap {} on X₀ {} at P {price}, tier {:?}",
                c.oi, c.x0, c.tier
            ));
        }
    }
    Ok(())
}

/// The candidate's caps for the market `m` (as stored) to carry its positions onto the pool `pool`
/// of kind `kind` — `L_slow' = min(L_slow, D)` (the pool's `min(√(X·Y), D/λ_min)` on a market
/// without a price), the market's age (a list-only market's: the candidate's), at
/// `min(P_ema, P_cand)` and at `P_ema` — as `(per-position cap, per-market cap)`, the smaller at
/// either price. `None` if the pool does not read. For tests that size a position to fit (or not
/// fit) a planned candidate; the program's own check runs on the state after the proposing
/// instruction's prologue.
pub fn carried_caps(
    svm: &LiteSVM,
    m: &Market,
    kind: VenueKind,
    pool: &Pubkey,
) -> Option<(u64, u64)> {
    let config = read::zc::<GlobalConfig>(svm, &crate::pda::config())?;
    let read = read_pool(svm, &market_on(svm, m, kind, pool)?)?;
    let d = read.depth();
    let p_ema = m.ema_price.get();
    let priced = m.ring_count > 0 && p_ema > 0;
    let l_slow = if priced {
        m.liq_slow.get().min(d)
    } else {
        // AIR-13: a price start's `L_slow` is at most `D`, as an adoption's.
        let (x, y) = read.view.reserves?;
        liquidity(u128::from(x), u128::from(y))
            .ok()?
            .min(depth_at_lambda(
                d,
                u64::from(config.params.pool_lock_min_bps),
            ))
            .min(d)
    };
    let age = if m.venue_kind() == Some(VenueKind::None) {
        match kind {
            VenueKind::RaydiumCpmm => {
                let acc = svm.get_account(pool)?;
                raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data)
                    .ok()?
                    .age_start(now(svm))
            }
            _ => now(svm),
        }
    } else {
        m.graduated_at
    };
    let t = market_now(svm, m);
    let mut out: Option<(u64, u64)> = None;
    for price in fit_prices(priced.then_some(p_ema), spot_of(&read)) {
        let c = fit_caps(
            svm,
            &config,
            &read,
            l_slow,
            age,
            price,
            t,
            true,
            m.min_open_fee_ppm,
        )
        .ok()?;
        out = Some(out.map_or((c.pos, c.oi), |(p, o)| (p.min(c.pos), o.min(c.oi))));
    }
    out
}

/// The prices "switch only if all fit" is judged at: `min(P_ema, P_cand)` and `P_ema` on a market
/// with a price, `P_cand` alone without one.
fn fit_prices(p_ema: Option<u128>, p_cand: u128) -> Vec<u128> {
    match p_ema {
        Some(e) => vec![e.min(p_cand), e],
        None => vec![p_cand],
    }
}

/// The candidate's spot `⌊Y·WAD/X⌋` (0 without reserves).
fn spot_of(p: &PoolRead) -> u128 {
    p.view.reserves.map_or(0, |(x, y)| {
        shortwin_math::risk::price_wad(u128::from(x), u128::from(y)).unwrap_or(0)
    })
}

/// I15, state part (switch only if all fit): `max_q_hwm` is at or above every open position's
/// `q`; with no position open it and `min_open_fee_ppm` (FIT-2) are 0.
pub fn check_hwm(
    key: &Pubkey,
    m: &Market,
    positions: &[(Pubkey, ShortPosition, u64)],
) -> Result<(), Violation> {
    if m.open_positions == 0 {
        return ensure(m.max_q_hwm == 0 && m.min_open_fee_ppm == 0, "I15", || {
            format!(
                "market {key}: max_q_hwm {} / min_open_fee_ppm {} with no open position (reset \
                 at 0)",
                m.max_q_hwm, m.min_open_fee_ppm
            )
        });
    }
    for (pk, pos, _) in positions {
        ensure(m.max_q_hwm >= pos.q, "I15", || {
            format!(
                "market {key}: max_q_hwm {} below open position {pk}'s q {}",
                m.max_q_hwm, pos.q
            )
        })?;
    }
    Ok(())
}

/// The `pool_fail` chain as the program's prologue leaves it after an observation of the
/// incumbent at `t` failing (`failed`) or not, from the state just before (`settle::chain_step`).
fn chain_after(pre: &Market, t: i64, failed: bool) -> i64 {
    settle::chain_step(pre.pool_fail_since, pre.last_pool_fail_obs, t, failed).0
}

/// The chain that armed `pre`'s collection window holds after this observation (M4-SEC-2).
fn window_chain_holds(
    pre: &Market,
    params: &shortwin::state::Params,
    t: i64,
    failed: bool,
) -> bool {
    let since = chain_after(pre, t, failed);
    pre.fp_t0 != 0
        && failed
        && since != 0
        && since + i64::from(params.pool_fast_route_s) <= pre.fp_t0
}

fn now(svm: &LiteSVM) -> i64 {
    svm.get_sysvar::<Clock>().unix_timestamp
}

/// The prologue's time: the clock, never before the market's last accrual.
fn market_now(svm: &LiteSVM, pre: &Market) -> i64 {
    now(svm).max(pre.last_accrual_ts)
}

fn in_window(m: &Market, config: &GlobalConfig, t: i64) -> bool {
    t < m.pool_seen_at + i64::from(config.params.pool_list_window_s)
}

/// `D_inc` as the proposing instruction saw it: the incumbent's `D` on the state just before
/// if it was executable (the prologue refreshes `pool_d`), else its stored `pool_d`.
fn d_inc(svm: &LiteSVM, mp: &MarketPositions) -> u128 {
    let pre = &mp.state;
    match (mp.venue, lp_of(svm, pre)) {
        (Some(v), Some((counter, supply))) if v.executable => PoolRead {
            view: v,
            counter,
            supply,
        }
        .depth(),
        _ => pre.pool_d.get(),
    }
}

/// The instruction-level I15 checks for one instruction (module docs).
pub fn check_ix(
    svm: &LiteSVM,
    pre: &PositionsSnapshot,
    ix: &PoolFacts<'_>,
) -> Result<(), Violation> {
    let Some(config) = read::zc::<GlobalConfig>(svm, &crate::pda::config()) else {
        return Ok(());
    };
    for c in ix.created {
        listing(svm, &config, c)?;
    }
    for mp in &pre.markets {
        let Some(post) = read::zc::<Market>(svm, &mp.market) else {
            continue;
        };
        let switched: Vec<&PoolSwitched> = ix
            .switched
            .iter()
            .filter(|e| e.market == mp.market)
            .collect();
        adoption_only_by_event(mp, &post, &switched)?;
        for e in &switched {
            adoption(svm, &config, mp, &post, e)?;
        }
        window(svm, &config, mp, &post, ix, !switched.is_empty())?;
        for o in ix.opens.iter().filter(|o| o.market == mp.market) {
            open_sized_on_locked_depth(svm, &config, mp, &post, o)?;
            pumpswap_open_sized_on_burned_depth(svm, &config, mp, &post, o)?;
        }
        marks_move_only_at_opens_and_the_last_exit(mp, &post, ix.opens)?;
    }
    Ok(())
}

/// I15, the marks (switch only if all fit; FIT-2): `max_q_hwm` falls only at the exit that
/// empties the market (to 0); `min_open_fee_ppm` changes only at an open — to
/// `open_fee_mark(mark, φ)`, `φ` the one-way fee the open's caps read on the pre-trade state,
/// `min(φ_live, φ at P_ema)` on the pump venues (M7-SEC-5; `VenueView::cap_fee_ppm`)
/// (`caps::open_fee_mark`: the first open after the market emptied sets it) — and at that exit
/// (to 0).
fn marks_move_only_at_opens_and_the_last_exit(
    mp: &MarketPositions,
    post: &Market,
    opens: &[Open],
) -> Result<(), Violation> {
    let key = mp.market;
    let pre = &mp.state;
    ensure(
        post.max_q_hwm >= pre.max_q_hwm || post.open_positions == 0,
        "I15",
        || {
            format!(
                "market {key}: max_q_hwm fell {} → {} with {} position(s) open",
                pre.max_q_hwm, post.max_q_hwm, post.open_positions
            )
        },
    )?;
    let n_opens = opens.iter().filter(|o| o.market == key).count();
    let want = if n_opens > 0 {
        // The pump venues have no view here (none is live in M4): their opens are not checked.
        let Some(v) = mp.venue else {
            return Ok(());
        };
        // M7-SEC-5: the fee the open's caps took, `min(φ_live, φ at P_ema)`.
        (0..n_opens).fold(pre.min_open_fee_ppm, |m, _| {
            shortwin_math::caps::open_fee_mark(m, v.cap_fee_ppm)
        })
    } else if post.open_positions == 0 {
        0
    } else {
        pre.min_open_fee_ppm
    };
    ensure(post.min_open_fee_ppm == want, "I15", || {
        format!(
            "market {key}: min_open_fee_ppm {} → {} ({n_opens} open(s), venue fee {:?}, {} → {} \
             position(s) open), want {want}",
            pre.min_open_fee_ppm,
            post.min_open_fee_ppm,
            mp.venue.map(|v| (v.fee_ppm, v.cap_fee_ppm)),
            pre.open_positions,
            post.open_positions
        )
    })
}

/// A Raydium listing (`create_market(venue_hint)`, ARCH §2.6 rule 1, §3.2): the listed pool is
/// the event's, eligible, with `pool_d` its `D`; the age per O13 / O13b; `pool_seen_at` now.
fn listing(svm: &LiteSVM, config: &GlobalConfig, c: &MarketCreated) -> Result<(), Violation> {
    let kind = VenueKind::from_u8(c.venue);
    if !kind.is_some_and(VenueKind::is_raydium) {
        return Ok(());
    }
    // A listing's market exists after it; an event without one (the synthetic `MarketCreated`
    // of a hook test) has no state to check.
    let Some(m) = read::zc::<Market>(svm, &c.market) else {
        return Ok(());
    };
    ensure(
        m.pool_cache[raydium_cache::RAY_POOL] == c.ray_pool,
        "I15",
        || {
            format!(
                "listing of {}: MarketCreated names {} but the market holds {}",
                c.market,
                c.ray_pool,
                m.pool_cache[raydium_cache::RAY_POOL]
            )
        },
    )?;
    let Some(p) = read_pool(svm, &m) else {
        return fail(
            "I15",
            format!("listing of {}: the pool does not read", c.market),
        );
    };
    if let Err(why) = eligible(&p, config, m.ema_price.get()) {
        return fail(
            "I15",
            format!(
                "listing of {}: {} was not eligible: {why}",
                c.market, c.ray_pool
            ),
        );
    }
    ensure(m.pool_d.get() == p.depth(), "I15", || {
        format!(
            "listing of {}: pool_d {} is not the pool's D {}",
            c.market,
            m.pool_d.get(),
            p.depth()
        )
    })?;
    // M4R: a Raydium price start is marked, with `L = min(√(X·Y), ⌊D·10⁴/λ_min⌋)` and — AIR-13 —
    // `L_slow = min(L, D)`.
    let (x, y) = p.view.reserves.unwrap_or((0, 0));
    let l_live = liquidity(u128::from(x), u128::from(y)).unwrap_or(0);
    let want = l_live.min(depth_at_lambda(
        p.depth(),
        u64::from(config.params.pool_lock_min_bps),
    ));
    let want_slow = want.min(p.depth());
    ensure(
        (m.ema_liq.get(), m.liq_slow.get(), m.ema_warm, m.ema_guard)
            == (want, want_slow, EMA_WARMUP_BUCKETS, 1),
        "I15",
        || {
            format!(
                "listing of {}: L_ema {} / L_slow {} / ema_warm {} / ema_guard {}, want {want}, \
                 {want_slow}, {EMA_WARMUP_BUCKETS}, 1 (M4R, AIR-13)",
                c.market,
                m.ema_liq.get(),
                m.liq_slow.get(),
                m.ema_warm,
                m.ema_guard
            )
        },
    )?;
    let t = now(svm);
    let age = match kind {
        Some(VenueKind::RaydiumCpmm) => {
            let acc = svm.get_account(&c.ray_pool).expect("pool");
            raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data)
                .map_or(i64::MIN, |s| s.age_start(t))
        }
        _ => t,
    };
    ensure(m.graduated_at == age && m.pool_seen_at == t, "I15", || {
        format!(
            "listing of {}: graduated_at {} (want {age}), pool_seen_at {} (want {t})",
            c.market, m.graduated_at, m.pool_seen_at
        )
    })
}

/// A market's pool changes only with its `PoolSwitched`.
fn adoption_only_by_event(
    mp: &MarketPositions,
    post: &Market,
    switched: &[&PoolSwitched],
) -> Result<(), Violation> {
    let pre = &mp.state;
    let to_raydium = post.venue_kind().is_some_and(VenueKind::is_raydium);
    let changed = pre.venue != post.venue || pre.pool_cache != post.pool_cache;
    if to_raydium && changed {
        ensure(switched.len() == 1, "I15", || {
            format!(
                "market {}: its pool changed ({} -> {}) with {} PoolSwitched events",
                mp.market,
                pre.pool_cache[raydium_cache::RAY_POOL],
                post.pool_cache[raydium_cache::RAY_POOL],
                switched.len()
            )
        })?;
    }
    if !switched.is_empty() {
        ensure(switched.len() == 1 && to_raydium, "I15", || {
            format!(
                "market {}: {} PoolSwitched events, venue after {}",
                mp.market,
                switched.len(),
                post.venue
            )
        })?;
    }
    Ok(())
}

/// One adoption (module docs: eligibility, who may adopt, the effects).
fn adoption(
    svm: &LiteSVM,
    config: &GlobalConfig,
    mp: &MarketPositions,
    post: &Market,
    e: &PoolSwitched,
) -> Result<(), Violation> {
    let pre = &mp.state;
    let market = mp.market;
    let t = market_now(svm, pre);
    let params = &config.params;
    ensure(
        post.pool_cache[raydium_cache::RAY_POOL] == e.candidate,
        "I15",
        || {
            format!(
                "market {market}: PoolSwitched names {} but the market adopted {}",
                e.candidate,
                post.pool_cache[raydium_cache::RAY_POOL]
            )
        },
    )?;
    // Eligible when adopted: the pool's own accounts (the instruction moved nothing in it).
    let Some(p) = read_pool(svm, post) else {
        return fail(
            "I15",
            format!(
                "market {market}: the adopted pool {} does not read",
                e.candidate
            ),
        );
    };
    if let Err(why) = eligible(&p, config, post.ema_price.get()) {
        return fail(
            "I15",
            format!(
                "market {market}: adopted {} while it was not eligible: {why}",
                e.candidate
            ),
        );
    }
    if e.kind == POOL_KIND_FAST
        && let Err(why) = window_floor(&p, config, post.ema_price.get())
    {
        return fail(
            "I15",
            format!(
                "market {market}: a window adopted {} carrying open positions: {why}",
                e.candidate
            ),
        );
    }
    // Switch only if all fit: the adoption's `L_slow` (`min(L_slow, D)`, or the pool's start) and
    // age are the market's after it; `P_ema` carries over (a market without a price: the
    // candidate's spot alone). An execution judges `X₀` without the live `X` (the terms no
    // transaction can move).
    let was_priced = pre.ring_count > 0 && pre.ema_price.get() > 0;
    let prices = fit_prices(was_priced.then(|| post.ema_price.get()), spot_of(&p));
    if let Err(why) = carried_fit(
        svm,
        config,
        &mp.positions,
        &p,
        post.liq_slow.get(),
        post.graduated_at,
        &prices,
        market_now(svm, pre),
        e.kind != POOL_KIND_FAST,
        pre.min_open_fee_ppm,
    ) {
        return fail(
            "I15",
            format!(
                "market {market}: adopted {} (kind {}) with open positions that do not fit its \
                 opening caps: {why}",
                e.candidate, e.kind
            ),
        );
    }
    let d = p.depth();
    ensure(e.d_cand == d && post.pool_d.get() == d, "I15", || {
        format!(
            "market {market}: d_cand {} / pool_d {} but the pool's D is {d}",
            e.d_cand,
            post.pool_d.get()
        )
    })?;
    // Who may adopt.
    let pre_kind = pre.venue_kind();
    let from_raydium = pre_kind.is_some_and(VenueKind::is_raydium);
    let want_inc = if from_raydium { d_inc(svm, mp) } else { 0 };
    ensure(e.d_inc == want_inc, "I15", || {
        format!(
            "market {market}: d_inc {} but the incumbent's D was {want_inc}",
            e.d_inc
        )
    })?;
    let inside = from_raydium && in_window(pre, config, t);
    match e.kind {
        POOL_KIND_LISTING => ensure(
            pre_kind == Some(VenueKind::None) && pre.open_positions == 0,
            "I15",
            || {
                format!(
                    "market {market}: a listing adoption on venue {} with {} open positions \
                     (M4-SEC-4: a list-only market with positions adopts through a window)",
                    pre.venue, pre.open_positions
                )
            },
        )?,
        POOL_KIND_LISTING_WINDOW | POOL_KIND_ZERO_POSITION => {
            ensure(pre.open_positions == 0, "I15", || {
                format!(
                    "market {market}: a non-fast adoption with {} open positions (decision 7)",
                    pre.open_positions
                )
            })?;
            let (want_inside, margin) = if e.kind == POOL_KIND_LISTING_WINDOW {
                (true, params.pool_list_margin_bps)
            } else {
                (false, params.pool_switch_margin_bps)
            };
            ensure(
                from_raydium && inside == want_inside && dominates(d, want_inc, u64::from(margin)),
                "I15",
                || {
                    format!(
                        "market {market}: kind {} adoption (inside window {inside}) of D {d} over \
                         D_inc {want_inc} with margin {margin} bps",
                        e.kind
                    )
                },
            )?;
        }
        POOL_KIND_FAST => {
            let ends = pre.fp_t0 + i64::from(params.pool_fast_window_s);
            let failed = mp.venue.is_some_and(|v| {
                settle::pool_fail(
                    v.executable,
                    v.fee_ppm,
                    u64::from(params.venue_fee_hard_cap_bps),
                )
            });
            // M4-SEC-2: on a Raydium market the chain that armed the window must hold; a
            // list-only market's window (M4-SEC-4) has none.
            let chain = !from_raydium || window_chain_holds(pre, params, t, failed);
            ensure(
                (from_raydium || pre_kind == Some(VenueKind::None))
                    && pre.fp_t0 != 0
                    && t >= ends
                    && pre.fp_keys[0] == e.candidate
                    && (failed || !from_raydium)
                    && chain,
                "I15",
                || {
                    format!(
                        "market {market}: a fast adoption of {} (window {} ends {ends}, now {t}, \
                         recorded {}, incumbent failing {failed}, chain since {} → {}, \
                         holds {chain})",
                        e.candidate,
                        pre.fp_t0,
                        pre.fp_keys[0],
                        pre.pool_fail_since,
                        chain_after(pre, t, failed)
                    )
                },
            )?;
        }
        k => return fail("I15", format!("market {market}: unknown adoption kind {k}")),
    }
    // The effects (M4R): the ring's liquidity rewritten to the new pool's anchor ⌊D·10⁴/λ_min⌋
    // — no sample of the pool — `L_slow` to at most its locked depth, prices kept, the state
    // marked for the post-change warm-up.
    let (x, y) = p.view.reserves.unwrap_or((0, 0));
    let l_live = liquidity(u128::from(x), u128::from(y)).unwrap_or(0);
    let l = post.ema_liq.get();
    let priced = pre.ring_count > 0 && pre.ema_price.get() > 0;
    let cap = depth_at_lambda(d, u64::from(params.pool_lock_min_bps));
    let slow = post.liq_slow.get();
    let integrated = post.ema_ts != pre.ema_ts;
    let want_l = if priced { cap } else { l_live.min(cap) };
    let slow_ok = if !priced {
        // AIR-13: a price start's `L_slow` is at most `D` too.
        slow == want_l.min(d)
    } else if integrated {
        slow <= d
    } else {
        slow == pre.liq_slow.get().min(d)
    };
    ensure(l == want_l && slow_ok, "I15", || {
        format!(
            "market {market}: L_ema {l} / L_slow {slow} after the adoption, want L_ema {want_l} \
             (anchor {cap}, √(X·Y) {l_live}, D {d}, priced before: {priced}, integrated: \
             {integrated}) and L_slow ≤ D (before {})",
            pre.liq_slow.get()
        )
    })?;
    ensure(
        (post.ema_warm, post.ema_guard) == (EMA_WARMUP_BUCKETS, 1),
        "I15",
        || {
            format!(
                "market {market}: ema_warm {} / ema_guard {} after an adoption, want \
                 {EMA_WARMUP_BUCKETS} / 1 (M4R)",
                post.ema_warm, post.ema_guard
            )
        },
    )?;
    let n = usize::from(post.ring_count).min(post.ring.len());
    for (i, r) in post.ring.iter().enumerate().take(n) {
        ensure(
            (r.l.get(), r.llo.get(), r.lhi.get()) == (l, l, l)
                && (r.x.get(), r.y.get()) == reprice(l, r.p.get()).unwrap_or((0, 0)),
            "I15",
            || {
                format!(
                    "market {market}: ring entry {i} keeps (l, llo, lhi) = ({}, {}, {}) after an \
                     adoption, not the reset L_ema {l} with (x, y) re-cached (A72, \
                     T-VENUE-SWITCH)",
                    r.l.get(),
                    r.llo.get(),
                    r.lhi.get()
                )
            },
        )?;
        if priced && r.ts != post.ema_ts {
            let kept = pre.ring.iter().any(|q| {
                (q.ts, q.p.get(), q.lo.get(), q.hi.get())
                    == (r.ts, r.p.get(), r.lo.get(), r.hi.get())
            });
            ensure(kept, "I15", || {
                format!(
                    "market {market}: ring entry {i} (ts {}) changed its price or price extremes \
                     in an adoption",
                    r.ts
                )
            })?;
        }
    }
    ensure(post.b_sec == 0 && post.b_lo.get() == 0, "I15", || {
        format!("market {market}: the open bucket survived an adoption")
    })?;
    ensure(
        (
            post.no_route_since,
            post.last_no_route_obs,
            post.pool_fail_since,
            post.last_pool_fail_obs,
        ) == (0, 0, 0, 0),
        "I15",
        || format!("market {market}: a route chain survived an adoption"),
    )?;
    ensure(
        post.fp_t0 == 0 && post.fp_d.get() == 0 && post.fp_keys == [Pubkey::default(); 6],
        "I15",
        || format!("market {market}: the fast-path window survived an adoption"),
    )?;
    // Age and listing window.
    let want_age = if from_raydium {
        pre.graduated_at
    } else {
        match post.venue_kind() {
            Some(VenueKind::RaydiumCpmm) => {
                let acc = svm.get_account(&e.candidate).expect("pool");
                raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data)
                    .map_or(i64::MIN, |s| s.age_start(now(svm)))
            }
            _ => now(svm),
        }
    };
    ensure(post.graduated_at == want_age, "I15", || {
        format!(
            "market {market}: graduated_at {} after the adoption, want {want_age} (O13)",
            post.graduated_at
        )
    })?;
    let want_seen = if inside { pre.pool_seen_at } else { t };
    ensure(post.pool_seen_at == want_seen, "I15", || {
        format!(
            "market {market}: pool_seen_at {} after the adoption, want {want_seen} (a chain of \
             proposals cannot extend a window)",
            post.pool_seen_at
        )
    })?;
    let ends = post.pool_seen_at + i64::from(params.pool_list_window_s);
    ensure(e.window_ends == ends, "I15", || {
        format!("market {market}: window_ends {} != {ends}", e.window_ends)
    })
}

/// The collection window (module docs).
fn window(
    svm: &LiteSVM,
    config: &GlobalConfig,
    mp: &MarketPositions,
    post: &Market,
    ix: &PoolFacts<'_>,
    switched: bool,
) -> Result<(), Violation> {
    let pre = &mp.state;
    let market = mp.market;
    let params = &config.params;
    let proposed: Vec<&PoolProposed> = ix.proposed.iter().filter(|e| e.market == market).collect();
    let cancelled = ix.cancelled.iter().filter(|e| e.market == market).count();
    let fp = |m: &Market| (m.fp_keys, m.fp_d.get(), m.fp_t0, m.fp_venue, m._pad1a);
    // A list-only market moved onto a pump venue (M5): the window is cleared, D1.
    let to_pump = ix.venue_changed.iter().any(|e| {
        e.market == market
            && e.from == VenueKind::None as u8
            && matches!(
                VenueKind::from_u8(e.to),
                Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
            )
    });
    if to_pump {
        ensure(
            post.fp_t0 == 0 && post.fp_d.get() == 0 && post.fp_keys == [Pubkey::default(); 6],
            "I15",
            || {
                format!(
                    "market {market}: a None → pump transition left the window {:?}",
                    fp(post)
                )
            },
        )?;
    }
    if fp(pre) != fp(post) {
        ensure(
            switched || proposed.len() == 1 || cancelled == 1 || to_pump,
            "I15",
            || format!("market {market}: the fast-path window changed without a pool event"),
        )?;
    }
    if cancelled > 0 {
        let ends = pre.fp_t0 + i64::from(params.pool_fast_window_s);
        ensure(
            pre.fp_t0 != 0 && market_now(svm, pre) >= ends && post.fp_t0 == 0,
            "I15",
            || format!("market {market}: a window cancelled before it ran out or not cleared"),
        )?;
    }
    for e in proposed {
        let t = market_now(svm, pre);
        ensure(
            post.fp_keys[0] == e.candidate && post.fp_d.get() == e.d_cand,
            "I15",
            || {
                format!(
                    "market {market}: PoolProposed {} not recorded in fp_*",
                    e.candidate
                )
            },
        )?;
        let kind = VenueKind::from_u8(post.fp_venue);
        let cand = kind
            .filter(|k| k.is_raydium())
            .and_then(|k| market_on(svm, post, k, &e.candidate));
        let Some(read) = cand.as_ref().and_then(|m| read_pool(svm, m)) else {
            return fail(
                "I15",
                format!(
                    "market {market}: the window's candidate {} does not read",
                    e.candidate
                ),
            );
        };
        // M4R-2: the window floor at min(P_ema, P_cand), P_cand the candidate's spot (a
        // proposal does not trade), P_cand alone on a market without a price.
        let p_ema = post.ema_price.get();
        let p_cand = read.view.reserves.map_or(0, |(x, y)| {
            shortwin_math::risk::price_wad(u128::from(x), u128::from(y)).unwrap_or(0)
        });
        let (p_ref, p_win) = if p_ema > 0 {
            (p_ema, p_ema.min(p_cand))
        } else {
            (p_cand, p_cand)
        };
        if let Err(why) =
            eligible(&read, config, p_ref).and_then(|()| window_floor(&read, config, p_win))
        {
            return fail(
                "I15",
                format!(
                    "market {market}: window candidate {} not eligible: {why}",
                    e.candidate
                ),
            );
        }
        ensure(read.depth() == e.d_cand, "I15", || {
            format!(
                "market {market}: the window's d_cand {} is not the pool's D {}",
                e.d_cand,
                read.depth()
            )
        })?;
        let list_only = pre.venue_kind() == Some(VenueKind::None);
        // Switch only if all fit, as the adoption would leave the market: `L_slow' = min(L_slow,
        // D)` (a market without a price: the pool's `min(√(X·Y), D/λ_min, D)`, AIR-13); the age
        // kept (a list-only market: the candidate's age start, O13).
        let d = read.depth();
        let priced = post.ring_count > 0 && p_ema > 0;
        let l_slow = if priced {
            post.liq_slow.get().min(d)
        } else {
            let (x, y) = read.view.reserves.unwrap_or((0, 0));
            liquidity(u128::from(x), u128::from(y))
                .unwrap_or(0)
                .min(depth_at_lambda(d, u64::from(params.pool_lock_min_bps)))
                .min(d)
        };
        let age = if list_only {
            match kind {
                Some(VenueKind::RaydiumCpmm) => {
                    let acc = svm.get_account(&e.candidate).expect("pool");
                    raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data)
                        .map_or(i64::MIN, |s| s.age_start(now(svm)))
                }
                _ => now(svm),
            }
        } else {
            post.graduated_at
        };
        if let Err(why) = carried_fit(
            svm,
            config,
            &mp.positions,
            &read,
            l_slow,
            age,
            &fit_prices(priced.then_some(p_ema), p_cand),
            t,
            true,
            mp.state.min_open_fee_ppm,
        ) {
            return fail(
                "I15",
                format!(
                    "market {market}: window candidate {} accepted with open positions that do \
                     not fit its opening caps: {why}",
                    e.candidate
                ),
            );
        }
        let qualifies = fast_path_qualifies(
            e.d_cand,
            post.pool_d.get(),
            u64::from(params.pool_fast_min_bps),
        );
        let failed = mp.venue.is_some_and(|v| {
            settle::pool_fail(
                v.executable,
                v.fee_ppm,
                u64::from(params.venue_fee_hard_cap_bps),
            )
        });
        if pre.fp_t0 == 0 {
            if list_only {
                // M4-SEC-4: a list-only market opens a window only with positions open.
                ensure(pre.open_positions > 0 && post.fp_t0 == t, "I15", || {
                    format!(
                        "market {market}: a list-only window opened at {} (now {t}) with {} \
                             open positions",
                        post.fp_t0, pre.open_positions
                    )
                })?;
                continue;
            }
            let armed = failed
                && post.pool_fail_since != 0
                && t - post.pool_fail_since >= i64::from(params.pool_fast_route_s);
            ensure(armed && qualifies && post.fp_t0 == t, "I15", || {
                format!(
                    "market {market}: a window opened at {} (now {t}) without an armed fast path \
                     (pool_fail {failed}, since {}) or a qualifying candidate ({qualifies})",
                    post.fp_t0, post.pool_fail_since
                )
            })?;
        } else {
            let ends = pre.fp_t0 + i64::from(params.pool_fast_window_s);
            // M4-SEC-2: a Raydium window takes candidates only while its arming chain holds.
            let chain = list_only || window_chain_holds(pre, params, t, failed);
            ensure(
                t < ends
                    && e.d_cand > pre.fp_d.get()
                    && (qualifies || list_only)
                    && chain
                    && e.candidate != pre.fp_keys[0]
                    && post.fp_t0 == pre.fp_t0,
                "I15",
                || {
                    format!(
                        "market {market}: the window's candidate replaced by D {} over {} at {t} \
                         (window ends {ends}, arming chain holds {chain})",
                        e.d_cand,
                        pre.fp_d.get()
                    )
                },
            )?;
        }
    }
    Ok(())
}

/// OWNER-M6D-1 (module docs; ARCH row 57, A82; ECON #106): an open on a PumpSwap market is sized
/// and tiered on its burned LP only. Recomputed here from the harness's own view of the pool and its
/// LP mint on the pre-trade state (`venue_view`: `D = ⌊λ_bps·isqrt(X·(Y + VQR))/10⁴⌋`) and at
/// `λ_min` = 100 % written out — the reprojection of `D` itself, not the program's constant —:
/// `pool_d = D`; `q` within the per-position impact and fee-scaled caps on `X₀ = min(X_now,
/// ⌊L_slow·10⁹/isqrt⁺(P_ema)⌋, ⌊D·10⁹/isqrt⁺(P_ema)⌋)`; `ΣQ + q` within the per-market cap on
/// `X₀ + q`; and the open's tier no better than the one the burned LP's real SOL `max(⌊D·
/// isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)` (PUMP-2026-10-1: the boost) alone earns at the
/// cached oracle (the tier is monotone in liquidity, and `R_eff ≤` that bound). At
/// `pool_lock_min_bps` (as built at row 56) a held third-party deposit sized opens at up to
/// `λ/λ_min` of this `X₀` and could lift the tier (M6D-1).
fn pumpswap_open_sized_on_burned_depth(
    svm: &LiteSVM,
    config: &GlobalConfig,
    mp: &MarketPositions,
    post: &Market,
    o: &Open,
) -> Result<(), Violation> {
    let pre = &mp.state;
    if pre.venue_kind() != Some(VenueKind::PumpSwap) {
        return Ok(());
    }
    let market = mp.market;
    let params = &config.params;
    let Some((view, (x, _), d)) = mp
        .venue
        .filter(|v| v.executable)
        .and_then(|v| Some((v, v.reserves?, v.locked_depth?)))
    else {
        return fail(
            "I15",
            format!(
                "open {} on a PumpSwap pool that could not execute",
                o.position
            ),
        );
    };
    ensure(post.pool_d.get() == d, "I15", || {
        format!(
            "open {}: pool_d {} is not the burned LP's D {d} on the pre-trade state",
            o.position,
            post.pool_d.get()
        )
    })?;
    let p_ema = post.ema_price.get();
    let at_ema = |l: u128| shortwin_math::quote::reprice_token(l, p_ema).unwrap_or(u128::MAX);
    let x0 = u128::from(x)
        .min(at_ema(post.liq_slow.get()))
        .min(at_ema(d));
    let x0 = u64::try_from(x0).unwrap_or(u64::MAX);
    let Some(tp) = config.tiers.get(usize::from(o.tier)) else {
        return fail("I15", format!("open {}: tier {}", o.position, o.tier));
    };
    let impact = shortwin_math::caps::max_q_position(x0, tp.impact_pos_bps).unwrap_or(0);
    let fee = shortwin_math::caps::fee_size_cap(x0, view.cap_fee_ppm, params.fee_size_cap_x100)
        .ok()
        .flatten()
        .unwrap_or(u64::MAX);
    ensure(o.q <= impact.min(fee), "I15", || {
        format!(
            "open {}: q {} above the per-position cap {} on the burned depth's X₀ {x0} (D {d}, \
             OWNER-M6D-1)",
            o.position,
            o.q,
            impact.min(fee)
        )
    })?;
    let oi = shortwin_math::caps::oi_cap(
        x0.saturating_add(o.q),
        tp.impact_mkt_bps,
        view.cap_fee_ppm,
        params.fee_oi_cap_x100,
    )
    .unwrap_or(0);
    ensure(pre.borrowed_q.saturating_add(o.q) <= oi, "I15", || {
        format!(
            "open {}: ΣQ {} + q {} above the per-market cap {oi} on the burned depth's X₀ {x0} + q",
            o.position, pre.borrowed_q, o.q
        )
    })?;
    // The tier: `R_eff ≤ max(⌊D·isqrt(P_ema)/10⁹⌋ − VQR⁺, 0)`, the burned LP's real SOL — `VQR⁺`
    // = `max(VQR + buckets, 0)` (PUMP-2026-10-1) of the pre-trade view, as `D` (never the pool
    // after the open's own sale; FUZZ-M7-24H-1).
    let vqr = u128::from(view.pump_tier.map_or(0, |(_, v)| v));
    let locked = d.saturating_mul(p_ema.isqrt()) / 1_000_000_000;
    let bound = locked.saturating_sub(vqr);
    let oracle = read::borsh::<OracleCache>(svm, &crate::pda::oracle_cache()).map_or(
        OraclePrice::default(),
        |c| OraclePrice {
            price: c.price,
            conf: c.conf,
            expo: c.expo,
            publish_time: c.publish_time,
        },
    );
    let best = tier::assign(&TierInputs {
        venue: TierVenue::Unlocked,
        r_now: u64::try_from(bound).unwrap_or(u64::MAX),
        y_slow: bound,
        vqr: 0,
        p_ema,
        now: market_now(svm, pre),
        graduated_at: post.graduated_at,
        oracle,
        oracle_stale_s: params.oracle_stale_s,
        theta_a_usd_e6: params.theta_a_usd_e6,
        theta_b_usd_e6: params.theta_b_usd_e6,
    });
    ensure(usize::from(o.tier) >= best.index(), "I15", || {
        format!(
            "market {market}: open {} at tier {} better than the burned LP's real SOL {bound} \
             earns ({best:?}; D {d}, VQR⁺ {vqr}, OWNER-M6D-1)",
            o.position, o.tier
        )
    })
}

/// An open on a Raydium market is sized on locked depth (module docs; V2-8, ARCH §2.6 rule 5).
fn open_sized_on_locked_depth(
    svm: &LiteSVM,
    config: &GlobalConfig,
    mp: &MarketPositions,
    post: &Market,
    o: &Open,
) -> Result<(), Violation> {
    let pre = &mp.state;
    if !pre.venue_kind().is_some_and(VenueKind::is_raydium) {
        return Ok(());
    }
    let market = mp.market;
    let params = &config.params;
    let t = market_now(svm, pre);
    let Some(view) = mp.venue.filter(|v| v.executable) else {
        return fail(
            "I15",
            format!("open {} on a pool that could not execute", o.position),
        );
    };
    let (x, y) = view.reserves.unwrap_or((0, 0));
    let Some((counter, supply)) = lp_of(svm, post) else {
        return fail(
            "I15",
            format!("market {market}: the pool's LP state does not read"),
        );
    };
    let d = locked_depth(counter, supply, x, y).unwrap_or(0);
    ensure(post.pool_d.get() == d, "I15", || {
        format!(
            "open {}: pool_d {} is not the pool's D {d} on the pre-trade state",
            o.position,
            post.pool_d.get()
        )
    })?;
    ensure(
        t >= pre.pool_seen_at + i64::from(params.pool_list_window_s),
        "I15",
        || format!("open {} inside the listing window", o.position),
    )?;
    // M4R / M4V-5: the post-roll observation state finished its post-change warm-up.
    ensure(post.ema_warm == 0, "I15", || {
        format!(
            "open {} during the post-change warm-up: ema_warm {} (M4R)",
            o.position, post.ema_warm
        )
    })?;
    ensure(
        !exceeds_cap(view.fee_ppm, u64::from(params.venue_fee_cap_bps)),
        "I15",
        || {
            format!(
                "open {} at a fee of {} ppm above the cap",
                o.position, view.fee_ppm
            )
        },
    )?;
    let lambda_min = params.pool_lock_min_bps;
    let p_ema = post.ema_price.get();
    let y_slow = reprice(post.liq_slow.get(), p_ema).map_or(0, |(_, y)| y);
    let inp = TierInputs {
        venue: TierVenue::Raydium {
            locked_depth: d,
            lambda_min_bps: lambda_min,
        },
        r_now: y,
        y_slow,
        vqr: 0,
        p_ema,
        now: t,
        graduated_at: post.graduated_at,
        oracle: OraclePrice::default(),
        oracle_stale_s: params.oracle_stale_s,
        theta_a_usd_e6: params.theta_a_usd_e6,
        theta_b_usd_e6: params.theta_b_usd_e6,
    };
    let r_eff = tier::liquidity_lamports(&inp)
        .ok()
        .flatten()
        .map_or(0, |l| l / 2);
    ensure(r_eff >= u128::from(params.pool_min_real_sol), "I15", || {
        format!(
            "open {}: R_eff {r_eff} below pool_min_real_sol {}",
            o.position, params.pool_min_real_sol
        )
    })?;
    let x0 =
        shortwin_math::caps::conservative_x0(x, post.liq_slow.get(), p_ema, Some((d, lambda_min)))
            .unwrap_or(0);
    let impact = config
        .tiers
        .get(usize::from(o.tier))
        .map_or(0, |tp| tp.impact_pos_bps);
    let cap = shortwin_math::caps::max_q_position(x0, impact).unwrap_or(0);
    ensure(o.q <= cap, "I15", || {
        format!(
            "open {}: q {} above the impact cap {cap} on the locked-depth X₀ {x0}",
            o.position, o.q
        )
    })
}
