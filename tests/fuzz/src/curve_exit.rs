//! The pump curve's differential exit oracle (M10, G-I11-real; ARCH §3.6, §3.7, §8.3 I11 /
//! I-E11): every keeper `liquidate` / `expire` the fuzzer sends on a live curve market is decided
//! first from `shortwin-math` and the venue-state readers alone — the curve E2E's keeper view
//! ([`crate::curve_support::status`]: the robust-spot and EMA legs, `B̂_live` at the live fee,
//! "normal buyback allowed") and the references and bounds of `shortwin_math::bounds` on the
//! curve's `CurvePricing` at the position's hard cap (`B̂_ref = B̂(Q; reprice(L_b, P_ref))`, `L_b =
//! max(L_live, max(L_ema', L_slow))` with no depth cap off Raydium and PumpSwap) — on the state
//! after a same-second `crank_price` on a clone of the VM (the exit's own prologue is exactly a
//! crank's), in the program's order (ARCH §3.6 precondition 1, 1b, steps 2–6; §3.7):
//!
//! `NoRoute` (not executable) → `EmaWarmingUp` → `ReferenceStale` → `NotLiquidatable` /
//! `NotExpired` … `UseSolSettle` → `UseExpire` / `UseLiquidate` → `ExitBoundNotMet` (an undefined
//! reference quote, or `B̂_live > max_in`) → executes. Since pump-final §6 a keeper exit of `q > rT`
//! is no refusal of its own: it completes the curve by pump's synthetic migration, priced `B̂_v3`
//! (the keeper view's `CurveCtx`), and one the would-be pool cannot deliver has no `B̂_live`, so it
//! is `UseSolSettle` (§6.7).
//!
//! The exit must then do exactly that: land when the prediction executes (with the event's
//! `b_live`, `b_ref` and `max_in` equal to the prediction's), or be refused with the predicted code.
//! In particular no `ExitBoundNotMet` while `B̂_live ≤ max_in = min(⌊B̂_ref·(10⁴ + δ)/10⁴⌋, …)`
//! with a fresh bucket (I-E11's liveness clause, which the hooks cannot see on a refusal). Its
//! domain is a market still on the live curve after the crank (a completion or a graduation in
//! the crank moves it: not modelled, the exit goes out unchecked).
//!
//! An exit of `q ≥ rT` that lands completes the curve in its own instruction (pump-final §12,
//! [`check_completion`]): the market is `PumpMigrating` with `migration_seen_at = now`, the curve
//! complete, and pump's post-complete fields are the synthetic buy's — `pcB = q − rT` and `pcQ =
//! ⌈Y_sᵖ·(q − rT)/(X_s − q + rT)⌉` on the would-be pool `X_s = curve_base_ata − rT`, `Y_sᵖ = rQ +
//! n₁ᵖ − fee_m` (`n₁ᵖ = rQ' − rQ`, the curve part pump took: `⌈rT·vQ/(vT − rT)⌉`, or `⌊⌋ + 1` on an
//! exact division; the pool part may take `⌊⌋ + 1` there too) — both 0 for `q = rT`.

use anchor_lang::prelude::Pubkey;
use shortwin::errors::ShortwinError as E;
use shortwin::events::{Expire, Liquidate};
use shortwin::state::{ShortPosition, VenueKind};
use shortwin_litesvm_tests::engine::crank_price_ix;
use shortwin_litesvm_tests::pump::PumpMarket;
use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
use shortwin_math::bounds;
use shortwin_math::quote;

use crate::curve_support::{self as cs, CurveCtx, Status};

/// Which keeper exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Liquidate,
    Expire,
}

/// A keeper exit's predicted decision and the figures it rests on.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub which: Which,
    /// The refusal, or the bound the leg runs under.
    pub decision: Result<Bound, E>,
    pub status: Status,
    /// `B̂_ref` and `max_in` of the exit's reference (`None`: undefined).
    pub b_ref: Option<u64>,
    pub max_in: Option<u64>,
    pub ema_warm: u8,
    pub q: u64,
    pub rt: u128,
    /// What the curve's quotes read (the live state, `X_s`, `fee_m`): [`check_completion`]'s
    /// pre-state.
    pub ctx: Option<CurveCtx>,
}

/// An executing exit's bound (the event's figures).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    pub b_live: u64,
    pub b_ref: u64,
    pub max_in: u64,
}

/// The prediction for `borrower`'s position `nonce` on the curve market `pm`, on a clone after a
/// same-second crank (the VM is restored); `None` outside the oracle's domain (module docs).
pub fn predict(
    env: &mut Env,
    pm: &PumpMarket,
    borrower: &Pubkey,
    nonce: u64,
    which: Which,
) -> Option<Prediction> {
    let saved = env.svm.clone();
    let crank = env.send(&[crank_price_ix(pm)], &[]);
    let p = crank
        .is_ok()
        .then(|| decide(env, pm, borrower, nonce, which));
    env.svm = saved;
    p.flatten()
}

fn decide(
    env: &Env,
    pm: &PumpMarket,
    borrower: &Pubkey,
    nonce: u64,
    which: Which,
) -> Option<Prediction> {
    let m = env.market(&pm.market());
    if m.venue_kind() != Some(VenueKind::PumpCurve) {
        return None;
    }
    let key = pda::position(&pm.market(), borrower, nonce).0;
    let pos: ShortPosition = read::borsh(&env.svm, &key)?;
    let st = cs::status(env, pm, borrower, nonce);
    let live = cs::curve(env, pm);
    let ctx = CurveCtx::read(env, pm);
    let obs = m.observation();
    let now = env.now();
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    let q = pos.q;
    // The reference: `L_b` with no depth cap (the curve has no locked depth), the risk fee.
    let l_live = if st.executable {
        quote::liquidity(live.vt, live.vq).ok()?
    } else {
        obs.ema.liq
    };
    let l_b = bounds::ref_liquidity(l_live, &obs.ema, &obs.ring, None).ok()?;
    let prm = pos.exit_params();
    let p_ref = match which {
        Which::Liquidate => bounds::liq_reference(&obs.ema, &obs.ring, &obs.last, now, &prm),
        Which::Expire => bounds::expiry_reference(&obs.ema, &obs.ring, &obs.last, now, &prm),
    }
    .ok()?;
    let b_ref = match (&ctx, p_ref) {
        (Some(c), Some(p)) => c.buy_repriced(q, l_b, p, cap).map(|b| b.cost),
        _ => None,
    };
    let max_in = b_ref.and_then(|b| match which {
        Which::Liquidate => bounds::max_in_liq(b, pos.delta_liq_bps, pos.v, st.k_s, pos.ert),
        Which::Expire => {
            let pi = bounds::pi_ref(b, pos.expiry_fee_bps).ok()?;
            bounds::max_in_exp(
                b,
                pos.delta_exp_bps,
                pos.exec_tol_bps,
                pos.v,
                st.k_s,
                st.r_due,
                pi,
                pos.ert,
            )
        }
    });
    let decision = (|| {
        if !st.executable {
            return Err(E::NoRoute);
        }
        if m.ema_warm != 0 {
            return Err(E::EmaWarmingUp);
        }
        match which {
            Which::Liquidate => {
                if !st.fresh {
                    return Err(E::ReferenceStale);
                }
                if !(st.cr_liq() || st.expired) {
                    return Err(E::NotLiquidatable);
                }
                if !st.buyback_allowed {
                    return Err(E::UseSolSettle);
                }
                if st.expired && !st.cr_liq() {
                    return Err(E::UseExpire);
                }
            }
            Which::Expire => {
                if !st.expired {
                    return Err(E::NotExpired);
                }
                if !st.fresh {
                    return Err(E::ReferenceStale);
                }
                if st.cr_liq() {
                    return Err(E::UseLiquidate);
                }
                if !st.buyback_allowed {
                    return Err(E::UseSolSettle);
                }
            }
        }
        p_ref.ok_or(E::ReferenceStale)?;
        let b_ref = b_ref.ok_or(E::ExitBoundNotMet)?;
        let max_in = max_in.ok_or(E::ExitBoundNotMet)?;
        let b_live = st.b_live.ok_or(E::UseSolSettle)?;
        if b_live > max_in {
            return Err(E::ExitBoundNotMet);
        }
        Ok(Bound {
            b_live,
            b_ref,
            max_in,
        })
    })();
    Some(Prediction {
        which,
        decision,
        status: st,
        b_ref,
        max_in,
        ema_warm: m.ema_warm,
        q,
        rt: live.rt,
        ctx,
    })
}

/// The completion an exit predicted to execute with `q ≥ rT` must leave (module docs), checked
/// after it landed. `Ok(None)` for an exit that does not complete the curve; `Ok(Some(tag))`
/// names the case; `Err` describes a mismatch.
pub fn check_completion(
    env: &Env,
    pm: &PumpMarket,
    pred: &Prediction,
) -> Result<Option<&'static str>, String> {
    use shortwin_math::num::{Rounding, mul_div};
    let q = u128::from(pred.q);
    if pred.decision.is_err() || q < pred.rt {
        return Ok(None);
    }
    let ctx = pred
        .ctx
        .as_ref()
        .ok_or("a completing exit without the curve's context")?;
    let m = env.market(&pm.market());
    let c = pm.curve.state(&env.svm);
    let now = env.now();
    let (pcb, pcq) = (
        u128::from(c.post_complete_base_out),
        u128::from(c.post_complete_quote_in),
    );
    let msg = |what: String| {
        format!(
            "{what}\nq {q}, rT {}, live {:?}, X_s {}, fee_m {}; after: venue {:?}, \
             migration_seen_at {} (now {now}), complete {}, rQ {}, pcB {pcb}, pcQ {pcq}",
            pred.rt,
            ctx.live,
            ctx.live_pool_tokens,
            ctx.migration_fee,
            m.venue_kind(),
            m.migration_seen_at,
            c.complete,
            c.real_quote_reserves
        )
    };
    if m.venue_kind() != Some(VenueKind::PumpMigrating) || m.migration_seen_at != now || !c.complete
    {
        return Err(msg(
            "a completing exit did not leave the market PumpMigrating since now on a complete \
             curve"
                .into(),
        ));
    }
    let q2 = q - pred.rt;
    if q2 == 0 {
        return if (pcb, pcq) == (0, 0) {
            Ok(Some("exact"))
        } else {
            Err(msg("an exact completion wrote post-complete fields".into()))
        };
    }
    let (live, x_s) = (ctx.live, ctx.live_pool_tokens);
    // The curve part pump took, n₁ᵖ = rQ' − rQ: ⌈rT·vQ/(vT − rT)⌉, ⌊⌋ + 1 on an exact division.
    let rest = live.vt - live.rt;
    let ceil1 = mul_div(live.rt, live.vq, rest, Rounding::Ceil).map_err(|e| format!("{e:?}"))?;
    let floor1 = mul_div(live.rt, live.vq, rest, Rounding::Floor).map_err(|e| format!("{e:?}"))?;
    let n1 = u128::from(c.real_quote_reserves)
        .checked_sub(live.rq)
        .ok_or_else(|| msg("rQ fell".into()))?;
    if n1 != ceil1 && n1 != floor1 + 1 {
        return Err(msg(format!(
            "the curve part's n₁ {n1}, not ⌈⌉ {ceil1} or ⌊⌋ + 1 {}",
            floor1 + 1
        )));
    }
    let y_s = (live.rq + n1)
        .checked_sub(u128::from(ctx.migration_fee))
        .ok_or_else(|| msg("rQ + n₁ below the migration fee".into()))?;
    if q2 >= x_s {
        return Err(msg("a synthetic exit past the would-be pool landed".into()));
    }
    let n2 = mul_div(y_s, q2, x_s - q2, Rounding::Ceil).map_err(|e| format!("{e:?}"))?;
    let exact2 = mul_div(y_s, q2, x_s - q2, Rounding::Floor).map_err(|e| format!("{e:?}"))? == n2;
    if pcb != q2 || !(pcq == n2 || (exact2 && pcq == n2 + 1)) {
        return Err(msg(format!(
            "pump's post-complete fields are not the synthetic buy's (pcB {q2}, pcQ ⌈⌉ {n2}{})",
            if exact2 { " or ⌊⌋ + 1" } else { "" }
        )));
    }
    Ok(Some("synthetic"))
}

/// Compares the exit's outcome `res` (`code`: its short.win error code, if it failed in
/// short.win) with the prediction. `Err` describes a mismatch; `Ok` names the outcome for the
/// summary.
pub fn check(pred: &Prediction, res: &TxResult, code: Option<u32>) -> Result<String, String> {
    let ctx = format!(
        "prediction: {:?} (b_ref {:?}, max_in {:?}, q {}, rT {}, ema_warm {})\nstatus: {:?}",
        pred.decision, pred.b_ref, pred.max_in, pred.q, pred.rt, pred.ema_warm, pred.status
    );
    check_outcome("curve", pred.which, &pred.decision, &ctx, res, code)
}

/// The comparison of [`check`] for any venue's decision-level prediction (the curve's, and
/// since M10 PumpSwap's, [`crate::pswap_exit`]): an executing prediction must land with the
/// event's `(b_live, b_ref, max_in)`, a refusal must be the predicted code; a venue refusal of
/// a leg predicted to execute fails (I-E11 / I13), a compute-budget exhaustion does not (an
/// anomaly). `venue` tags the outcome (`liquidate:<venue>-differential-…`); `ctx` describes the
/// prediction for a mismatch.
pub fn check_outcome(
    venue: &str,
    which: Which,
    decision: &Result<Bound, E>,
    ctx: &str,
    res: &TxResult,
    code: Option<u32>,
) -> Result<String, String> {
    let name = match which {
        Which::Liquidate => "liquidate",
        Which::Expire => "expire",
    };
    let mismatch = |what: String| format!("{what}\n{ctx}");
    match (res, decision) {
        (Ok(meta), Ok(want)) => {
            let got = match which {
                Which::Liquidate => events::of::<Liquidate>(meta).first().map(|e| Bound {
                    b_live: e.b_live,
                    b_ref: e.b_ref,
                    max_in: e.max_in,
                }),
                Which::Expire => events::of::<Expire>(meta).first().map(|e| Bound {
                    b_live: e.b_live,
                    b_ref: e.b_ref,
                    max_in: e.max_in,
                }),
            };
            if got != Some(*want) {
                return Err(mismatch(format!(
                    "{name} landed with (b_live, b_ref, max_in) {got:?}, predicted {want:?}"
                )));
            }
            Ok(format!("{name}:{venue}-differential-landed"))
        }
        (Ok(_), Err(e)) => Err(mismatch(format!(
            "{name} landed; predicted the refusal {e:?}"
        ))),
        (Err(_), Ok(_)) => match code {
            // I-E11: an executable, fresh, liquidatable, buyback-allowed position with
            // `B̂_live ≤ max_in` must not wait (nor be refused otherwise by short.win).
            Some(c) if venue_failed(res) => Err(mismatch(format!(
                "{name}: the venue refused the buyback the program chose to run (code {c}); \
                 predicted to execute (I-E11 / I13)"
            ))),
            Some(c) => Err(mismatch(format!(
                "{name} refused {} ({c}); predicted to execute (I-E11 for ExitBoundNotMet)",
                crate::world::code_name(c)
            ))),
            // The compute budget ran out (`Ctx::send` records it as an anomaly): not a decision.
            None if compute_exhausted(res) => {
                Ok(format!("{name}:{venue}-differential-compute-exhausted"))
            }
            // Any other failure without a code (an account, funds or CPI error) refused the leg
            // the program chose to run: its `B̂_live` or `max_in` wrong for the venue (I-E11, I13).
            None => Err(mismatch(format!(
                "{name} failed with {} (no short.win code); predicted to execute: the leg the \
                 program chose to run was refused (I-E11 / I13)",
                match res {
                    Err(f) => format!("{:?}", f.err),
                    Ok(_) => String::new(),
                }
            ))),
        },
        (Err(_), Err(e)) => {
            if code != Some(u32::from(*e)) {
                return Err(mismatch(format!(
                    "{name} refused {}; predicted {e:?}",
                    code.map_or_else(|| "outside short.win".into(), crate::world::code_name)
                )));
            }
            Ok(format!("{name}:{venue}-differential-refused:{e:?}"))
        }
    }
}

/// Whether `res` failed on the compute budget (the error, or the runtime's log line: a CPI that
/// runs out surfaces as its caller's failure).
fn compute_exhausted(res: &TxResult) -> bool {
    use solana_instruction::error::InstructionError as I;
    use solana_transaction_error::TransactionError as T;
    let Err(f) = res else { return false };
    matches!(
        f.err,
        T::InstructionError(
            _,
            I::ComputationalBudgetExceeded | I::ProgramFailedToComplete
        )
    ) || f.meta.logs.iter().any(|l| l.contains("exceeded CUs meter"))
}

/// Whether a program other than short.win failed inside `res` (a CPI's own error, as `Ctx::send`
/// tells a venue refusal from a framework error).
fn venue_failed(res: &TxResult) -> bool {
    let Err(f) = res else { return false };
    let me = shortwin::ID.to_string();
    f.meta
        .logs
        .iter()
        .any(|l| l.contains("failed: ") && l.starts_with("Program ") && !l.contains(&me))
}
