//! I18 (ARCH §8.3, §4.4; ECON §5.2; M4R post-change confirmation, **PROPOSED**, owner item
//! O18), checked by `check_ix` against the state just before each instruction
//! (`super::positions::PositionsSnapshot`) and the instruction's own events.
//!
//! | Part | What is checked |
//! |---|---|
//! | (a) restart | only an instruction that emits `PoolSwitched` for the market, a Raydium `create_market`, a `→ PumpSwap` graduation of a market with a price (`VenueStateChanged`, M6) or a PumpSwap market's price start (its first observation, M6) raises `ema_warm` (to `EMA_WARMUP_BUCKETS`) or sets `ema_guard`; after such an instruction both are `(8, 1)`; a market created on another venue starts `(0, 0)`; a PumpSwap price start writes `L_ema = min(√(X·(Y + VQR)), D)` (the anchor at PumpSwap's `λ_min` = 100 %, OWNER-M6D-1 / M6E-1) and `L_slow = min(L_ema, D)` on the pre-trade pool, `D` its locked depth (M6R-1, recomputed here) |
//! | (b) countdown and seed | across any other instruction `ema_warm` falls by the number of integrations (0 or 1) while it is positive and stays 0 otherwise; a warm-up integration leaves the newest ring entry and the EMA at the §4.1 seed over the newest `8 − ema_warm_after` entries, `p = max(min(min lo, the largest v0.4.1 step from the pre-instruction P_ema), the largest v0.4.1 fall from it)` (M4X-1: `observe::price_up_bound`; M6-SEC-1: `observe::price_down_bound`, recomputed here too; `dt = min(ts − ema_ts, ema_dtmax_s)`), `l = min(max lhi, ⌊lomed(lhi)·(10⁴ + C)/10⁴⌋)` (recomputed here), with `slow_ts` unchanged and `L_slow` unchanged — on PumpSwap, or `min(L_slow, pool_d)` (M6R-1) |
//! | (f) PumpSwap warm-up depth | M6R-1: on a PumpSwap market whose post-instruction `ema_warm > 0` and `pool_d > 0`, `L_slow ≤ pool_d` (every executable observation during the warm-up caps `L_slow` at the locked depth it writes into `pool_d`; the transition writes `pool_d = 0`); across a warm-up instruction `L_slow` only falls, and only to `pool_d` |
//! | (c) gate | no `Liquidate` or `Expire` event, no `SolSettle` of S1/S2 and no `SettleArmed` / `SettleDisarmed` on a market whose post-instruction `ema_warm ≠ 0` while its venue is executable; no Raydium or PumpSwap `Open` with `ema_warm ≠ 0` |
//! | (d) guard | `ema_guard` goes 1 → 0 only at an integration with `ema_warm = 0` after it and `L_ema·10⁴ ≤ L_mlo·10,100` (and does so at every such integration); an integration after the warm-up with the guard set never lifts `L_slow` above `max(L_slow before, L_mlo after)` (M4X-2: `L_slow` integrates `min(lobs, L_mlo)`); on a Raydium or PumpSwap market, and on any market while the guard is set, `Liquidate.b_ref` / `Expire.b_ref` are the risk quote at `reprice(max(L_live, min(max(L_ema', L_slow), cap)), P_ref)`, `L_ema' = min(L_ema, L_mlo)` while the guard is set, else `L_ema` (V2-14), `cap = pool_d` on Raydium (M4F-1; `⌊pool_d·10⁴/pool_lock_min_bps⌋` before AIR-14) and — since the owner's decision of 2026-09-30, M6R-1 — PumpSwap at `λ_min` = 100 % (OWNER-M6D-1 / M6E-1), so `L_b = L_live` on both, none elsewhere. On the mock, which never restarts, the guard-clear case is the exit predictors' (`liquidate_support`), and the hook tests' synthetic exits there carry arbitrary references |
//!
//! | (e) spot leg | FITS-2 (a): every `Liquidate` on an executable Raydium or (M6R-1) PumpSwap venue is CR-liquidatable, and its spot leg is judged at `max(L_live, L_floor)` — `Liquidate.cr_spot_bps` is `⌊V_eff·10⁴/B̂⌋` of the risk quote `B̂ = B̂(Q; reprice(max(L_live, L_floor), P_rs))` (recomputed here: `L_live` the pre-trade venue's `√(X·Y)`, `L_floor = min(L_ema, L_mhi)` while the guard is set (AIR-17), else `L_ema`, `P_rs = min(P_live, lb_lo)` fresh within `ref_max_age_s`, `V_eff = V − R_due`) and it is below the position's `L` — so no liquidation lands on a spot leg that only a drained `L_live` makes true. On the mock the exit predictors (`liquidate_support`, the same rule) check it, and the hook tests' synthetic exits there carry arbitrary figures |
//!
//! `L_mlo` is the lower median of the ring's bucket liquidity lows, `L_mhi` the upper median of
//! their highs. A market that never saw a
//! restart has `ema_warm = ema_guard = 0` throughout and integrates exactly as before M4R.

use litesvm::LiteSVM;
use shortwin::events::{
    Expire, Liquidate, MarketCreated, Open, PoolSwitched, SettleArmed, SettleDisarmed, SolSettle,
};
use shortwin::state::{Market, ShortPosition, VenueKind};
use shortwin_math::bounds;
use shortwin_math::consts::{EMA_WARMUP_BUCKETS, REF_GUARD_TOL_BPS};
use shortwin_math::num::{Rounding, mul_div};
use shortwin_math::observe::{self, Observation, price_down_bound, price_up_bound, ring_medians};
use shortwin_math::quote::liquidity;
use shortwin_math::rent;
use solana_clock::Clock;

use super::positions::{MarketPositions, PositionsSnapshot};
use super::venue_view::VenueView;
use super::{Violation, ensure, fail};
use crate::read;

/// The events of one instruction that I18 reads.
pub struct ConfirmFacts<'a> {
    pub created: &'a [MarketCreated],
    pub switched: &'a [PoolSwitched],
    /// Venue transitions: a `→ PumpSwap` graduation restarts the observation state of a market
    /// with a price exactly as an adoption does (M6).
    pub venue_changed: &'a [shortwin::events::VenueStateChanged],
    pub opens: &'a [Open],
    pub liquidations: &'a [Liquidate],
    pub expiries: &'a [Expire],
    pub settlements: &'a [SolSettle],
    pub armed: &'a [SettleArmed],
    pub disarmed: &'a [SettleDisarmed],
}

/// The instruction-level I18 checks for one instruction (module docs).
pub fn check_ix(
    svm: &LiteSVM,
    pre: &PositionsSnapshot,
    ix: &ConfirmFacts<'_>,
) -> Result<(), Violation> {
    for c in ix.created {
        created(svm, c)?;
    }
    for mp in &pre.markets {
        let Some(post) = read::zc::<Market>(svm, &mp.market) else {
            continue;
        };
        let switched = ix.switched.iter().any(|e| e.market == mp.market);
        // M6: a `→ PumpSwap` graduation of a market with a price, and a PumpSwap market's price
        // start (its first observation after a listing or a re-detection), restart the state.
        let priced = |m: &Market| m.ring_count > 0 && m.ema_price.get() > 0;
        let graduated = ix
            .venue_changed
            .iter()
            .any(|e| e.market == mp.market && e.to == VenueKind::PumpSwap as u8)
            && priced(&mp.state);
        let started =
            post.venue_kind() == Some(VenueKind::PumpSwap) && !priced(&mp.state) && priced(&post);
        if switched || graduated || started {
            ensure(
                (post.ema_warm, post.ema_guard) == (EMA_WARMUP_BUCKETS, 1),
                "I18",
                || {
                    format!(
                        "market {}: ema_warm {} / ema_guard {} after a restart (PoolSwitched, \
                         → PumpSwap, a PumpSwap price start), want {} / 1",
                        mp.market, post.ema_warm, post.ema_guard, EMA_WARMUP_BUCKETS
                    )
                },
            )?;
            if started {
                price_start(mp, &post)?;
            }
        } else {
            countdown_and_seed(mp, &post)?;
            guard_transition(mp, &post)?;
        }
        warm_up_depth(mp, &post)?;
        gate(mp, &post, ix)?;
        references(svm, mp, &post, ix)?;
        spot_legs(svm, mp, &post, ix)?;
    }
    Ok(())
}

/// (a) at creation: a Raydium listing is marked, any other venue is not.
fn created(svm: &LiteSVM, c: &MarketCreated) -> Result<(), Violation> {
    let Some(m) = read::zc::<Market>(svm, &c.market) else {
        return Ok(());
    };
    let raydium = VenueKind::from_u8(c.venue).is_some_and(VenueKind::is_raydium);
    let want = if raydium {
        (EMA_WARMUP_BUCKETS, 1)
    } else {
        (0, 0)
    };
    ensure((m.ema_warm, m.ema_guard) == want, "I18", || {
        format!(
            "market {} created on venue {}: ema_warm {} / ema_guard {}, want {want:?}",
            c.market, c.venue, m.ema_warm, m.ema_guard
        )
    })
}

/// The reference cap of a Raydium or PumpSwap market, and a PumpSwap price start's anchor: the
/// locked depth `d` itself — on PumpSwap the burned LP's (OWNER-M6D-1, ECON #106; its exit
/// reference and price start since M6E-1), on Raydium's reference since AIR-14
/// (`⌊d·10⁴/pool_lock_min_bps⌋` before) — written out here, not read from the program's
/// `engine::bounds::ref_depth_cap`.
fn depth_cap(d: u128) -> u128 {
    d
}

/// (a) M6R-1: a PumpSwap price start writes `L_ema = min(√(X·(Y + VQR)), D)` (the anchor at
/// PumpSwap's `λ_min` = 100 %, OWNER-M6D-1 / M6E-1; `⌊D·10⁴/pool_lock_min_bps⌋` before) and — the
/// warm-up's depth cap in the same prologue — `L_slow = min(L_ema, D)`, on the pool as the
/// instruction found it (`D` recomputed by the venue view).
fn price_start(mp: &MarketPositions, post: &Market) -> Result<(), Violation> {
    let market = mp.market;
    let Some(v) = mp.venue.filter(|v| v.executable) else {
        return fail(
            "I18",
            format!("market {market}: a PumpSwap price start on a pool that cannot execute"),
        );
    };
    let (Some((x, y)), Some(d)) = (v.reserves, v.locked_depth) else {
        return fail(
            "I18",
            format!("market {market}: a PumpSwap price start without reserves or a locked depth"),
        );
    };
    let root = liquidity(u128::from(x), u128::from(y)).unwrap_or(0);
    let cap = depth_cap(d);
    let l = root.min(cap);
    ensure(
        post.ema_liq.get() == l && post.liq_slow.get() == l.min(d) && post.pool_d.get() == d,
        "I18",
        || {
            format!(
                "market {market}: PumpSwap price start L_ema {} / L_slow {} / pool_d {}, want \
                 min(√k {root}, D {cap}) = {l} / min(that, D {d}) / D (M6R-1, M6E-1)",
                post.ema_liq.get(),
                post.liq_slow.get(),
                post.pool_d.get()
            )
        },
    )
}

/// (f) M6R-1: during a PumpSwap warm-up `L_slow` never exceeds a refreshed locked depth, and a
/// warm-up instruction moves `L_slow` only down to `pool_d`.
fn warm_up_depth(mp: &MarketPositions, post: &Market) -> Result<(), Violation> {
    let market = mp.market;
    let pre = &mp.state;
    let pumpswap = |m: &Market| m.venue_kind() == Some(VenueKind::PumpSwap);
    if !pumpswap(post) {
        return Ok(());
    }
    let (slow, d) = (post.liq_slow.get(), post.pool_d.get());
    ensure(post.ema_warm == 0 || d == 0 || slow <= d, "I18", || {
        format!(
            "market {market}: PumpSwap warm-up (ema_warm {}) with L_slow {slow} above the locked \
             depth pool_d {d} (M6R-1)",
            post.ema_warm
        )
    })?;
    if pumpswap(pre) && pre.ema_warm > 0 && post.ema_count == pre.ema_count {
        let before = pre.liq_slow.get();
        ensure(slow == before || slow == before.min(d), "I18", || {
            format!(
                "market {market}: L_slow {before} -> {slow} in a PumpSwap warm-up without an \
                 integration, want unchanged or min(L_slow, pool_d {d}) (M6R-1)"
            )
        })?;
    }
    Ok(())
}

/// The lower median of `vals` (index `⌊(n − 1)/2⌋` of the sorted values).
fn lomed(mut vals: Vec<u128>) -> u128 {
    vals.sort_unstable();
    vals[(vals.len() - 1) / 2]
}

/// The upper median of `vals` (index `⌊n/2⌋` of the sorted values).
fn upmed(mut vals: Vec<u128>) -> u128 {
    vals.sort_unstable();
    vals[vals.len() / 2]
}

/// (a) and (b) on an instruction without `PoolSwitched`.
fn countdown_and_seed(mp: &MarketPositions, post: &Market) -> Result<(), Violation> {
    let pre = &mp.state;
    let market = mp.market;
    let steps = post.ema_count.wrapping_sub(pre.ema_count);
    ensure(steps <= 1, "I18", || {
        format!(
            "market {market}: {steps} integrations in one instruction (ema_count {} -> {})",
            pre.ema_count, post.ema_count
        )
    })?;
    let want_warm = if pre.ema_warm > 0 {
        pre.ema_warm - u8::from(steps == 1)
    } else {
        0
    };
    ensure(post.ema_warm == want_warm, "I18", || {
        format!(
            "market {market}: ema_warm {} -> {} across {steps} integration(s) without a restart",
            pre.ema_warm, post.ema_warm
        )
    })?;
    ensure(pre.ema_guard == 1 || post.ema_guard == 0, "I18", || {
        format!("market {market}: ema_guard set without a restart")
    })?;
    if pre.ema_warm == 0 || steps == 0 {
        return Ok(());
    }
    // A warm-up integration: the seed over the post-change entries.
    let obs = post.observation();
    let k = usize::from(EMA_WARMUP_BUCKETS - post.ema_warm);
    let ents: Vec<_> = obs.ring.iter().take(k).copied().collect();
    let Some(newest) = ents.first().copied() else {
        return fail(
            "I18",
            format!("market {market}: a warm-up step left no ring entry"),
        );
    };
    // M4X-1 / M6-SEC-1: the price seed rises and falls no faster than the largest v0.4.1 step
    // from the pre-instruction P_ema (both bounds recomputed here as ECON §5.2 states them, and
    // compared with the math crate's functions).
    let dt = u128::try_from(
        newest
            .ts
            .saturating_sub(pre.ema_ts)
            .clamp(0, i64::from(post.ema_dtmax_s)),
    )
    .unwrap_or(0);
    let (p0, c) = (pre.ema_price.get(), u128::from(post.ema_clamp_bps));
    let step = |obs: Option<u128>| {
        obs.and_then(|o| {
            p0.checked_mul(5)?
                .checked_add(o.checked_mul(dt)?)?
                .checked_div(5 + dt)
        })
    };
    let bound = price_up_bound(p0, dt, post.ema_clamp_bps).unwrap_or(u128::MAX);
    let floor = price_down_bound(p0, dt, post.ema_clamp_bps).unwrap_or(0);
    let (up_re, down_re) = (
        step(mul_div(p0, 10_000 + c, 10_000, Rounding::Floor).ok()),
        step(mul_div(p0, 10_000, 10_000 + c, Rounding::Floor).ok()),
    );
    ensure(
        up_re.is_none_or(|u| u == bound) && down_re.is_none_or(|d| d == floor),
        "I18",
        || {
            format!(
                "market {market}: the warm-up price bounds from P_ema {p0} over dt {dt}: \
                 price_up_bound {bound} / price_down_bound {floor}, recomputed {up_re:?} / \
                 {down_re:?}"
            )
        },
    )?;
    let p = ents
        .iter()
        .map(|e| e.lo)
        .min()
        .unwrap_or(0)
        .min(bound)
        .max(floor);
    let highs: Vec<u128> = ents.iter().map(|e| e.lhi).collect();
    let max = highs.iter().copied().max().unwrap_or(0);
    let up = mul_div(
        lomed(highs),
        10_000 + u128::from(post.ema_clamp_bps),
        10_000,
        Rounding::Floor,
    )
    .unwrap_or(u128::MAX);
    let l = max.min(up);
    // M6R-1: on PumpSwap the prologue also caps L_slow at the locked depth it refreshed.
    let slow_ok = post.liq_slow.get() == pre.liq_slow.get()
        || (post.venue_kind() == Some(VenueKind::PumpSwap)
            && post.liq_slow.get() == pre.liq_slow.get().min(post.pool_d.get()));
    ensure(
        ents.len() == k
            && (newest.p, newest.l) == (p, l)
            && (obs.ema.price, obs.ema.liq) == (p, l)
            && slow_ok
            && post.slow_ts == pre.slow_ts,
        "I18",
        || {
            format!(
                "market {market}: warm-up step {k}: newest entry (p, l) = ({}, {}), EMA ({}, {}), \
                 want the seed ({p}, {l}) over {} entries; L_slow {} -> {}, slow_ts {} -> {}",
                newest.p,
                newest.l,
                obs.ema.price,
                obs.ema.liq,
                ents.len(),
                pre.liq_slow.get(),
                post.liq_slow.get(),
                pre.slow_ts,
                post.slow_ts
            )
        },
    )
}

/// (d) the guard clears exactly at the integrations that find `L_ema` within
/// `REF_GUARD_TOL_BPS` of `L_mlo` after the warm-up.
fn guard_transition(mp: &MarketPositions, post: &Market) -> Result<(), Violation> {
    let pre = &mp.state;
    if pre.ema_guard == 0 {
        return Ok(());
    }
    let market = mp.market;
    let integrated = post.ema_count != pre.ema_count;
    let l_mlo = ring_medians(&post.ring_math()).map_or(0, |m| m.2);
    let within = post.ema_liq.get().saturating_mul(10_000)
        <= l_mlo.saturating_mul(10_000 + u128::from(REF_GUARD_TOL_BPS));
    let want = if integrated && post.ema_warm == 0 && within {
        0
    } else {
        1
    };
    ensure(post.ema_guard == want, "I18", || {
        format!(
            "market {market}: ema_guard 1 -> {} (integrated {integrated}, ema_warm {}, L_ema {}, \
             L_mlo {l_mlo}), want {want}",
            post.ema_guard,
            post.ema_warm,
            post.ema_liq.get()
        )
    })?;
    // M4X-2: a guarded steady integration feeds L_slow at most the ring's lows.
    let steady = integrated && pre.ema_warm == 0;
    let cap = pre.liq_slow.get().max(l_mlo);
    ensure(!steady || post.liq_slow.get() <= cap, "I18", || {
        format!(
            "market {market}: L_slow {} -> {} while the guard was set, above max(L_slow, L_mlo \
             {l_mlo}) (L_ema {})",
            pre.liq_slow.get(),
            post.liq_slow.get(),
            post.ema_liq.get()
        )
    })
}

/// (c) the gate.
fn gate(mp: &MarketPositions, post: &Market, ix: &ConfirmFacts<'_>) -> Result<(), Violation> {
    if post.ema_warm == 0 {
        return Ok(());
    }
    let market = mp.market;
    let executable = mp.venue.is_some_and(|v| v.executable);
    // Raydium (M4R) and PumpSwap (M6: the graduation's and the price start's warm-up) opens wait.
    let gated = post
        .venue_kind()
        .is_some_and(|k| k.is_raydium() || k == VenueKind::PumpSwap);
    let opens = ix.opens.iter().filter(|e| e.market == market).count();
    ensure(!gated || opens == 0, "I18", || {
        format!(
            "market {market}: {opens} Raydium / PumpSwap open(s) with ema_warm {}",
            post.ema_warm
        )
    })?;
    if !executable {
        return Ok(());
    }
    let exits = ix
        .liquidations
        .iter()
        .filter(|e| e.market == market)
        .count()
        + ix.expiries.iter().filter(|e| e.market == market).count();
    let s12 = ix
        .settlements
        .iter()
        .filter(|e| e.market == market && e.condition != 3)
        .count();
    let clock = ix.armed.iter().filter(|e| e.market == market).count()
        + ix.disarmed.iter().filter(|e| e.market == market).count();
    ensure(exits + s12 + clock == 0, "I18", || {
        format!(
            "market {market}: {exits} keeper exit(s), {s12} S1/S2 settlement(s) and {clock} arm \
             change(s) while warming up on an executable venue (ema_warm {})",
            post.ema_warm
        )
    })
}

/// (e) FITS-2 (a): a liquidation's spot leg holds at `max(L_live, L_floor)` (module docs).
fn spot_legs(
    svm: &LiteSVM,
    mp: &MarketPositions,
    post: &Market,
    ix: &ConfirmFacts<'_>,
) -> Result<(), Violation> {
    let market = mp.market;
    // On the mock the exit predictors (`liquidate_support`) check the spot leg, and the hook
    // tests' synthetic exits there carry arbitrary figures. Raydium and (M6R-1) PumpSwap.
    if !post.venue_kind().is_some_and(VenueKind::has_locked_depth) {
        return Ok(());
    }
    let Some(v) = mp.venue.filter(|v| v.executable) else {
        return Ok(());
    };
    let (Some((x, y)), Some(pricing)) = (v.reserves, v.pricing) else {
        return Ok(());
    };
    let obs = post.observation();
    let now = svm
        .get_sysvar::<Clock>()
        .unix_timestamp
        .max(mp.state.last_accrual_ts);
    let (x, y) = (u128::from(x), u128::from(y));
    let l_live = liquidity(x, y).ok();
    let p_live = shortwin_math::risk::price_wad(x, y).ok();
    // AIR-17: under the guard the floor is min(L_ema, L_mhi), the upper median of the bucket
    // liquidity highs (not the lows' L_mlo, which withdraw-wrapped cranks set).
    let l_floor = if obs.ema.guard != 0 {
        obs.ema
            .liq
            .min(upmed(obs.ring.iter().map(|e| e.lhi).collect()))
    } else {
        obs.ema.liq
    };
    for e in ix.liquidations.iter().filter(|e| e.market == market) {
        let Some((_, pos, _)) = mp.positions.iter().find(|(pk, _, _)| *pk == e.position) else {
            continue;
        };
        let l_spot = l_live.map(|l| l.max(l_floor));
        let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
        let b = match (l_spot, p_rs) {
            (Some(l), Some(p)) => pricing
                .capped(exit_risk_cap(pos, mp.state.venue_kind(), &v))
                .buy_repriced(pos.q, l, p)
                .map(|b| b.cost),
            _ => None,
        };
        let r_due = rent::due(&pos.pos_rent(), &post.basis()).unwrap_or(u64::MAX);
        let v_eff = shortwin_math::risk::v_eff(pos.v, r_due);
        let want = shortwin_math::risk::cr_bps(v_eff, b).unwrap_or(u64::MAX);
        ensure(
            e.cr_spot_bps == want && u128::from(want) < u128::from(pos.cr_liq_bps),
            "I18",
            || {
                format!(
                    "market {market}: Liquidate {} cr_spot_bps {}, want {want} < L {} at                      max(L_live {l_live:?}, L_floor {l_floor}) and P_rs {p_rs:?} (FITS-2)",
                    e.position, e.cr_spot_bps, pos.cr_liq_bps
                )
            },
        )?;
    }
    Ok(())
}

/// The exit risk view's fee cap the program takes (AIR-10, `VenueState::exit_risk_cap_bps`): the
/// position's locked hard cap and, on PumpSwap, φ at the stored `P_ema` — an executable view's
/// `cap_fee_ppm` is `min(φ_live, φ at P_ema)`, which caps the quotes exactly as the program's
/// `min(hard cap, ⌈φ at P_ema/100⌉)` does.
fn exit_risk_cap(pos: &ShortPosition, kind: Option<VenueKind>, v: &VenueView) -> u64 {
    let hard = u64::from(pos.venue_fee_hard_cap_bps);
    if kind == Some(VenueKind::PumpSwap) {
        hard.min(u64::from(v.cap_fee_ppm).div_ceil(100))
    } else {
        hard
    }
}

/// `B̂_ref` of `pos` at `p_ref` with the (guarded, on Raydium and PumpSwap capped) reference
/// liquidity of `post` and the pre-trade venue `v` (V2-14, M4R, M4F-1, M6R-1) at the fee cap
/// `fee_cap` ([`exit_risk_cap`]). `cap` is the market's `pool_d` on a Raydium or PumpSwap market
/// (AIR-14 on Raydium; [`depth_cap`]), `None` elsewhere.
fn b_ref_of(
    pos: &ShortPosition,
    obs: &Observation,
    v: &VenueView,
    p_ref: u128,
    cap: Option<u128>,
    fee_cap: u64,
) -> Option<u64> {
    let (x, y) = v.reserves?;
    let l_live = liquidity(u128::from(x), u128::from(y)).ok()?;
    let l_ema = if obs.ema.guard != 0 {
        obs.ema.liq.min(ring_medians(&obs.ring).ok()?.2)
    } else {
        obs.ema.liq
    };
    let credited = l_ema.max(obs.ema.liq_slow);
    let l_b = l_live.max(cap.map_or(credited, |c| credited.min(c)));
    // The program's own function agrees with the recomputation above.
    if bounds::ref_liquidity(l_live, &obs.ema, &obs.ring, cap).ok()? != l_b {
        return None;
    }
    v.pricing?
        .capped(fee_cap)
        .buy_repriced(pos.q, l_b, p_ref)
        .map(|b| b.cost)
}

/// (d) the keeper exits' `B̂_ref` reads the guarded reference liquidity.
fn references(
    svm: &LiteSVM,
    mp: &MarketPositions,
    post: &Market,
    ix: &ConfirmFacts<'_>,
) -> Result<(), Violation> {
    let market = mp.market;
    let Some(v) = mp.venue.filter(|v| v.executable) else {
        return Ok(());
    };
    let locked = post.venue_kind().is_some_and(VenueKind::has_locked_depth);
    if post.ema_guard == 0 && !locked {
        return Ok(());
    }
    let obs = post.observation();
    let now = svm
        .get_sysvar::<Clock>()
        .unix_timestamp
        .max(mp.state.last_accrual_ts);
    // M4F-1 (Raydium), M6R-1 (PumpSwap): the EMA terms are capped at D, the locked depth the
    // exit's prologue refreshed (`pool_d`) — at λ = 100 % on PumpSwap since M6E-1 and on Raydium
    // since AIR-14 — so L_b = L_live there.
    let cap = locked.then(|| depth_cap(post.pool_d.get()));
    let pos_of = |k: &anchor_lang::prelude::Pubkey| {
        mp.positions
            .iter()
            .find(|(pk, _, _)| pk == k)
            .map(|(_, p, _)| p.clone())
    };
    for e in ix.liquidations.iter().filter(|e| e.market == market) {
        let Some(pos) = pos_of(&e.position) else {
            continue;
        };
        let fee_cap = exit_risk_cap(&pos, mp.state.venue_kind(), &v);
        let want = b_ref_of(&pos, &obs, &v, e.p_ref, cap, fee_cap);
        ensure(want == Some(e.b_ref), "I18", || {
            format!(
                "market {market}: Liquidate {} b_ref {} at p_ref {}, want {want:?} (guard {}, \
                 L_ema {}, L_slow {})",
                e.position, e.b_ref, e.p_ref, obs.ema.guard, obs.ema.liq, obs.ema.liq_slow
            )
        })?;
    }
    for e in ix.expiries.iter().filter(|e| e.market == market) {
        let Some(pos) = pos_of(&e.position) else {
            continue;
        };
        let p_ref =
            bounds::expiry_reference(&obs.ema, &obs.ring, &obs.last, now, &pos.exit_params())
                .ok()
                .flatten();
        let fee_cap = exit_risk_cap(&pos, mp.state.venue_kind(), &v);
        let want = p_ref.and_then(|p| b_ref_of(&pos, &obs, &v, p, cap, fee_cap));
        ensure(want == Some(e.b_ref), "I18", || {
            format!(
                "market {market}: Expire {} b_ref {}, want {want:?} at P_ref {p_ref:?} (guard {})",
                e.position, e.b_ref, obs.ema.guard
            )
        })?;
    }
    Ok(())
}
