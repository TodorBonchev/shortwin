//! **I19 — the open's terms, differentially** (M7-SEC-6; ARCH §8.3): every `Open` is recomputed
//! from the state just before its instruction, the harness's own venue views
//! ([`super::venue_view`]) and `shortwin-math`, never from the program's glue — so a mutant in
//! `open_short`, `engine::caps` or `engine::tier` that the program's own arithmetic would not
//! notice fails here, in every program test and in the fuzzer.
//!
//! Checked, for an open of `q` at clock `now` on tier `t = config.tiers[Open.tier]`:
//!
//! - **the locked terms** (A26): the position's tier terms are `t`'s and its protocol terms the
//!   config's `Params` (`ShortPosition::lock_terms`, field by field), `t_open = now`, `deadline =
//!   now + t.max_duration_s`, `V = S + M`;
//! - **the tier**: `tier::assign` on the market after the open's roll (`P_ema`, `L_slow`, `pool_d`,
//!   `graduated_at`: an open integrates nothing of its own), the venue's real SOL before the
//!   trade (the view's `Y`; PumpSwap `min(WSOL vault − buckets, WSOL vault + VQR)` with the boost
//!   `max(VQR + buckets, 0)` the tier's own input (PUMP-2026-10-1), both from the view taken before
//!   the instruction — after it the open's own sale has drained the vault: FUZZ-M7-24H-1), the
//!   cached oracle (with its confidence, M7-SEC-7) and `now`; the curve is Tier D;
//! - **the caps** (ECON §6.1, ARCH §4.6): `X₀` — the curve's at `P_ema` (M5-SEC-3), else
//!   `conservative_x0(X_now, L_slow, P_ema, (D, λ_min))` with Raydium's `pool_lock_min_bps` and
//!   PumpSwap's 100 % —, `q` within the per-position impact cap and the fee-scaled size cap at
//!   `min(φ_live, φ at P_ema)` (M7-SEC-5), `ΣQ + q` within `OI_cap` on `X₀ + q`, the wallet's
//!   `q_open + q` within `wallet_cap_bps` of `OI_cap`, Tier D's `ΣQ + q ≤ γ·(rT_guard + q)`, `q ≤ T`;
//! - **the notional** (ADOPTED Q11/Q21): `N_open` is the buyback of `q` on the venue after the
//!   sale (the view's quote), `min_notional ≤ N_open`, the market's `total_notional_open` moves by
//!   exactly `N_open` and stays within `market_max_notional`;
//! - **the margin** (D4, owner decision): `B̂_ema` is the buyback of `q` on the ring's newest
//!   cached reserves at the post-sale fees (constant-product venues; the curve's event value
//!   otherwise), and `M = max(0, ⌈cr_open·max(N_open, B̂_ema)/10⁴⌉ − S)`;
//! - **the open fee**: `⌈open_fee_bps·N_open/10⁴⌉`.

use litesvm::LiteSVM;
use shortwin::events::Open;
use shortwin::state::{
    GlobalConfig, Market, OracleCache, ShortPosition, VenueKind, WalletExposure,
};
use shortwin_math::quote::reprice;
use shortwin_math::tier::{self, OraclePrice, Tier, TierInputs, TierVenue};
use shortwin_math::{caps, risk};

use super::positions::MarketPositions;
use super::{Violation, ensure, venue_view};
use crate::read;

/// PumpSwap's sizing `λ_min` (OWNER-M6D-1: 100 %, written out, not the program's constant).
const PUMPSWAP_LAMBDA_MIN_BPS: u32 = 10_000;

thread_local! {
    static SYNTHETIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Skips I19 on this test thread: only the hook tests (`tests/invariants_m2_hooks.rs`), whose
/// `Open` events are built by hand against planted states to aim at another invariant (I3, I5,
/// I7, I13). Every other invariant still runs; I19's own hook test sets it back.
pub fn allow_synthetic_opens(on: bool) {
    SYNTHETIC.with(|c| c.set(on));
}

/// I19 for one `Open` of the market `mp` describes (module docs). `post` is the market after the
/// instruction.
pub fn check_open(
    svm: &LiteSVM,
    config: &GlobalConfig,
    mp: &MarketPositions,
    post: &Market,
    o: &Open,
) -> Result<(), Violation> {
    if SYNTHETIC.with(std::cell::Cell::get) {
        return Ok(());
    }
    let Some(pos) = read::borsh::<ShortPosition>(svm, &o.position) else {
        // A synthetic event (a hook test): I3 judges a missing position.
        return Ok(());
    };
    let pre = &mp.state;
    let params = &config.params;
    let now = svm
        .get_sysvar::<anchor_lang::solana_program::clock::Clock>()
        .unix_timestamp;
    let Some(tp) = config.tiers.get(usize::from(o.tier)) else {
        return super::fail(
            "I19",
            format!("open {}: tier {} out of range", o.position, o.tier),
        );
    };
    let q = o.q;
    let who = o.position;

    // ---- the locked terms --------------------------------------------------------------------
    let narrow = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let terms = [
        (
            "cr_liq_bps",
            u64::from(pos.cr_liq_bps),
            u64::from(tp.cr_liq_bps),
        ),
        (
            "penalty_bps",
            u64::from(pos.penalty_bps),
            u64::from(narrow(tp.penalty_bps)),
        ),
        (
            "max_liq_slip_bps",
            u64::from(pos.max_liq_slip_bps),
            u64::from(narrow(tp.max_liq_slip_bps)),
        ),
        (
            "rate_base_bps",
            u64::from(pos.rate_base_bps),
            u64::from(tp.rate_base_bps),
        ),
        (
            "rate_slope1_bps",
            u64::from(pos.rate_slope1_bps),
            u64::from(tp.rate_slope1_bps),
        ),
        (
            "rate_slope2_bps",
            u64::from(pos.rate_slope2_bps),
            u64::from(tp.rate_slope2_bps),
        ),
        (
            "expiry_fee_bps",
            u64::from(pos.expiry_fee_bps),
            u64::from(narrow(params.expiry_fee_bps)),
        ),
        (
            "rent_protocol_bps",
            u64::from(pos.rent_protocol_bps),
            u64::from(narrow(params.rent_protocol_bps)),
        ),
        (
            "delta_liq_bps",
            u64::from(pos.delta_liq_bps),
            u64::from(params.delta_liq_bps),
        ),
        (
            "kappa_liq_bps",
            u64::from(pos.kappa_liq_bps),
            u64::from(params.kappa_liq_bps),
        ),
        (
            "exec_tol_bps",
            u64::from(pos.exec_tol_bps),
            u64::from(params.exec_tol_bps),
        ),
        (
            "s3_wait_s",
            u64::from(pos.s3_wait_s),
            u64::from(params.s3_wait_s),
        ),
        (
            "settle_keeper_fixed",
            pos.settle_keeper_fixed,
            params.settle_keeper_fixed,
        ),
        (
            "expiry_keeper_fixed",
            pos.expiry_keeper_fixed,
            params.expiry_keeper_fixed,
        ),
    ];
    for (name, got, want) in terms {
        ensure(got == want, "I19", || {
            format!(
                "open {who} (tier {}): locked {name} {got}, the tier / config say {want}",
                o.tier
            )
        })?;
    }
    ensure(pos.tier == o.tier && pos.t_open == now, "I19", || {
        format!(
            "open {who}: tier {} / t_open {} (event tier {}, clock {now})",
            pos.tier, pos.t_open, o.tier
        )
    })?;
    let deadline = now.saturating_add(i64::from(tp.max_duration_s));
    ensure(
        pos.deadline == deadline && o.deadline == deadline,
        "I19",
        || {
            format!(
                "open {who}: deadline {} (event {}), want now {now} + max_duration_s {} = {deadline}",
                pos.deadline, o.deadline, tp.max_duration_s
            )
        },
    )?;
    ensure(o.v == o.s.saturating_add(o.m), "I19", || {
        format!("open {who}: V {} ≠ S {} + M {}", o.v, o.s, o.m)
    })?;

    // ---- the tier ----------------------------------------------------------------------------
    let kind = pre.venue_kind();
    let view = mp.venue.filter(|v| v.executable);
    let p_ema = post.ema_price.get();
    let l_slow = post.liq_slow.get();
    let oracle = read::borsh::<OracleCache>(svm, &crate::pda::oracle_cache()).map_or(
        OraclePrice::default(),
        |c| OraclePrice {
            price: c.price,
            conf: c.conf,
            expo: c.expo,
            publish_time: c.publish_time,
        },
    );
    let want_tier = match (kind, view) {
        (Some(VenueKind::PumpCurve | VenueKind::PumpMigrating), _) => Some(Tier::D),
        (Some(k), Some(v)) => {
            let (_, y) = v.reserves.unwrap_or((0, 0));
            let (venue, r_now, vqr) = match k {
                VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => (
                    TierVenue::Raydium {
                        locked_depth: post.pool_d.get(),
                        lambda_min_bps: params.pool_lock_min_bps,
                    },
                    y,
                    0,
                ),
                VenueKind::PumpSwap => {
                    // `R_now` and `max(VQR + buckets, 0)` of the view taken before the instruction,
                    // never `svm` after it: the open's own sale has taken its SOL out of the WSOL
                    // vault by then (FUZZ-M7-24H-1).
                    let (r_now, vqr) = v.pump_tier.unwrap_or((0, 0));
                    (
                        TierVenue::PumpSwap {
                            locked_depth: post.pool_d.get(),
                            lambda_min_bps: PUMPSWAP_LAMBDA_MIN_BPS,
                        },
                        r_now,
                        vqr,
                    )
                }
                _ => (TierVenue::Unlocked, y, 0),
            };
            Some(tier::assign(&TierInputs {
                venue,
                r_now,
                y_slow: reprice(l_slow, p_ema).map_or(0, |(_, ys)| ys),
                vqr,
                p_ema,
                now,
                graduated_at: post.graduated_at,
                oracle,
                oracle_stale_s: params.oracle_stale_s,
                theta_a_usd_e6: params.theta_a_usd_e6,
                theta_b_usd_e6: params.theta_b_usd_e6,
            }))
        }
        _ => None,
    };
    if let Some(t) = want_tier {
        ensure(usize::from(o.tier) == t.index(), "I19", || {
            format!(
                "open {who}: tier {} but the market's inputs give {t:?} (P_ema {p_ema}, L_slow \
                 {l_slow}, D {}, graduated_at {}, oracle {:?}, now {now})",
                o.tier,
                post.pool_d.get(),
                post.graduated_at,
                oracle
            )
        })?;
    }

    // ---- the caps ----------------------------------------------------------------------------
    let borrowed = post.borrowed_q.saturating_sub(q);
    let free = post.tokens_free.saturating_add(q);
    let wallet =
        read::borsh::<WalletExposure>(svm, &crate::pda::exposure(&o.market, &o.borrower).0)
            .map_or(0, |e| e.q_open.saturating_sub(q));
    let caps_view = match (kind, view) {
        (Some(VenueKind::PumpCurve), Some(v)) => v.curve.map(|(c, _)| {
            // M5-SEC-3: the curve at P_ema whenever that is the smaller one.
            let vt = u64::try_from(c.vt).unwrap_or(u64::MAX);
            let rt = u64::try_from(c.rt).unwrap_or(u64::MAX);
            let p_spot = c.vq.saturating_mul(1_000_000_000_000_000_000) / c.vt.max(1);
            if p_ema <= p_spot {
                (vt, Some(rt), v.cap_fee_ppm)
            } else {
                let l = shortwin_math::quote::liquidity(c.vt, c.vq).unwrap_or(0);
                let x_ema = shortwin_math::quote::reprice_token(l, p_ema).unwrap_or(u128::MAX);
                let off = c.vt.saturating_sub(c.rt);
                let x0 = c.vt.min(x_ema);
                let rtg = c.rt.min(x_ema.saturating_sub(off));
                (
                    u64::try_from(x0).unwrap_or(u64::MAX),
                    Some(u64::try_from(rtg).unwrap_or(u64::MAX)),
                    v.cap_fee_ppm,
                )
            }
        }),
        (Some(k), Some(v)) => v.reserves.map(|(x, _)| {
            let lock = match k {
                VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => {
                    Some((post.pool_d.get(), params.pool_lock_min_bps))
                }
                VenueKind::PumpSwap => Some((post.pool_d.get(), PUMPSWAP_LAMBDA_MIN_BPS)),
                _ => None,
            };
            let x0 = caps::conservative_x0(x, l_slow, p_ema, lock).unwrap_or(0);
            (x0, None, v.cap_fee_ppm)
        }),
        _ => None,
    };
    if let Some((x0, rt_guard, phi)) = caps_view {
        let impact = caps::max_q_position(x0, tp.impact_pos_bps).unwrap_or(0);
        ensure(q <= impact, "I19", || {
            format!("open {who}: q {q} above the per-position impact cap {impact} on X₀ {x0}")
        })?;
        if let Ok(Some(fee_cap)) = caps::fee_size_cap(x0, phi, params.fee_size_cap_x100) {
            ensure(q <= fee_cap, "I19", || {
                format!(
                    "open {who}: q {q} above the fee-scaled size cap {fee_cap} (φ {phi} ppm) on \
                     X₀ {x0}"
                )
            })?;
        }
        let oi = caps::oi_cap(
            x0.saturating_add(q),
            tp.impact_mkt_bps,
            phi,
            params.fee_oi_cap_x100,
        )
        .unwrap_or(0);
        ensure(borrowed.saturating_add(q) <= oi, "I19", || {
            format!("open {who}: ΣQ {borrowed} + q {q} above OI_cap {oi} on X₀ {x0} + q")
        })?;
        ensure(
            caps::wallet_ok(wallet, q, oi, params.wallet_cap_bps).unwrap_or(false),
            "I19",
            || {
                format!(
                    "open {who}: the wallet's {wallet} + q {q} above {} bps of OI_cap {oi}",
                    params.wallet_cap_bps
                )
            },
        )?;
        if let Some(rt) = rt_guard {
            ensure(
                caps::curve_gamma_ok(borrowed, q, rt, params.curve_oi_gamma_bps).unwrap_or(false),
                "I19",
                || format!("open {who}: Tier D's γ: ΣQ {borrowed} + q {q} against rT {rt}"),
            )?;
        }
    }
    ensure(q <= free, "I19", || {
        format!("open {who}: q {q} above the free inventory {free}")
    })?;

    // ---- the notional ------------------------------------------------------------------------
    let post_view = venue_view::view(svm, post).filter(|v| v.executable || v.curve.is_some());
    if let Some(n) = post_view.and_then(|v| v.buy(q)) {
        ensure(o.n_open == n, "I19", || {
            format!(
                "open {who}: N_open {} but the buyback of q on the venue after the sale is {n}",
                o.n_open
            )
        })?;
    }
    ensure(o.b_spot == o.n_open, "I19", || {
        format!("open {who}: Open.b_spot {} ≠ N_open {}", o.b_spot, o.n_open)
    })?;
    ensure(o.n_open >= params.min_notional, "I19", || {
        format!(
            "open {who}: N_open {} below min_notional {}",
            o.n_open, params.min_notional
        )
    })?;
    let total = pre.total_notional_open.saturating_add(o.n_open);
    ensure(post.total_notional_open == total, "I19", || {
        format!(
            "open {who}: total_notional_open {} → {}, want + N_open {}",
            pre.total_notional_open, post.total_notional_open, o.n_open
        )
    })?;
    ensure(total <= params.market_max_notional, "I19", || {
        format!(
            "open {who}: total notional {total} above market_max_notional {}",
            params.market_max_notional
        )
    })?;

    // ---- the margin and the fee -------------------------------------------------------------
    let (ex, ey) = post.newest_ring_xy();
    let b_ema = match post_view.and_then(|v| v.pricing) {
        Some(pr) if ex > 0 && ey > 0 => pr.buy(q, ex, ey).map(|b| b.cost),
        _ => None,
    };
    if let Some(b) = b_ema {
        ensure(o.b_ema == b, "I19", || {
            format!(
                "open {who}: Open.b_ema {} but the buyback of q on the ring's newest reserves \
                 ({ex}, {ey}) is {b}",
                o.b_ema
            )
        })?;
    }
    let b_ema = b_ema.or((o.b_ema > 0).then_some(o.b_ema));
    let m = risk::margin_d4(o.s, o.n_open, b_ema, tp.cr_open_bps).unwrap_or(u64::MAX);
    ensure(o.m == m, "I19", || {
        format!(
            "open {who}: M {} but D4 gives {m} (cr_open {} bps on max(N_open {}, B̂_ema {b_ema:?}) − S {})",
            o.m, tp.cr_open_bps, o.n_open, o.s
        )
    })?;
    let fee = risk::open_fee(o.n_open, params.open_fee_bps).unwrap_or(u64::MAX);
    ensure(o.fee_open == fee, "I19", || {
        format!(
            "open {who}: open fee {} but ⌈{} bps · N_open {}⌉ = {fee}",
            o.fee_open, params.open_fee_bps, o.n_open
        )
    })?;
    Ok(())
}
