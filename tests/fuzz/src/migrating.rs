//! pump-final §12's **`migrating_fv` oracle** (G-7; §5.4, §6.1.5): a `PumpMigrating` market —
//! a curve completed, by an exact buy of `rT` or by pump's synthetic migration, and not yet
//! migrated — is priced on the pool `migrate_v2` will create, and the fuzzer recomputes that
//! pool and every S3 fair value on it host-side, from the dumped accounts and `shortwin-math`'s
//! primitives (never the program's adapter, nor `quote::migrating_fv` itself):
//!
//! ```text
//! A        = curve_base_ata (live; rT = 0)                     a token donation included
//! pc       = pcB ≠ 0 ∨ pcQ ≠ 0                                 a completing buy with a pool part
//! (X₁, Y₁) = (pc ? A : 206.9e12, rQ − fee_m + pcQ)              quote::migrated_pool
//! (X₀, Y₀) = (206.9e12, rQ − fee_m)                            the same curve without either
//! p'       = pc ? ⌈⌈p·Y₁/Y₀⌉·X₀/X₁⌉ : p                         quote::migrating_fv_price
//! B̂_FV(p)  = sol_leg::buy_exact_out(q, reprice(isqrt(X₁·Y₁), p'), ε₁ = 0,
//!                                   fees::pump_amm(Y₁, X₁, PumpSwap tiers, the position's hard cap))
//! FV(p)    = B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due   (p' the expiry fee iff expired and CR(V_eff, B̂_FV) ≥ L)
//! ```
//!
//! - [`check_s3`]: every `SolSettle(S3)` of such a market must carry, at the EMA point
//!   (`P_ema` as the settlement leaves it stored), `fv` = `FV(P_ema)`, and at its box corners
//!   `x_lo` / `x_hi` = `min(FV(p_lo), take)` / `min(FV(p_hi), take)` with their `p'`; the
//!   lenders get `min(FV, take)`, `take = V − K_s`. When the post-complete fields are not known
//!   (§6.1.5: one of `pcB`, `pcQ` set alone, or `vQ − rQ ≠ ivqr` after a pool part) FV is
//!   undefined: `fv` 0 and the lenders get the whole take (the documented exception to A63).
//! - [`check_created_pool`]: once `migrate_v2` lands, the pool it created is `(A, Y₁)`: its base
//!   vault exactly the live `curve_base_ata` it found — `X₁` itself after a pool part, donations
//!   included —, its effective quote at most one lamport below `Y₁` (exact on a complete curve).
//!   Without a pool part FV prices the standard 206.9e12 (a donation ignored, ARCH §4.3), which
//!   the created pool's `A` meets or exceeds.

use anchor_lang::prelude::Pubkey;
use shortwin::constants::{PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG, PUMP_GLOBAL};
use shortwin::events::SolSettle;
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::pump::{self, PumpMarket};
use shortwin_litesvm_tests::{Env, read};
use shortwin_math::fees::{self, FeeBps, FeeTier, PammFeeSchedule};
use shortwin_math::quote::{self, MigrationParams, STANDARD_POOL_TOKENS, sol_leg};
use shortwin_math::{rent, risk};
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};

thread_local! {
    /// Lamports added to every `FV` [`check_s3`] expects (`tests/smoke.rs` shows a 1-lamport
    /// difference fails the run; 0 otherwise).
    pub static FV_SKEW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The pool `migrate_v2` will create from a complete curve, as pump-final §5.4 prices it (module
/// docs), and the PumpSwap schedule its fee comes from.
#[derive(Clone, Debug)]
pub struct PricedPool {
    /// The live `curve_base_ata`.
    pub a: u128,
    /// A pool part: `pcB` or `pcQ` set.
    pub pc: bool,
    /// The post-complete fields are known (pump-final §6.1.5): no pool part, or `pcB` and `pcQ`
    /// set together and `vQ − rQ = ivqr`.
    pub known: bool,
    pub x0: u128,
    pub y0: u128,
    pub x1: u128,
    pub y1: u128,
    tiers: Vec<FeeTier>,
    creator_fee_configurable: bool,
    creator_is_default: bool,
}

impl PricedPool {
    /// `B̂_FV(q; p)` at the risk view `cap_bps` (module docs); `None` when undefined.
    pub fn b_fv(&self, q: u64, p: u128, cap_bps: u64) -> Option<u64> {
        let p1 = if self.pc {
            quote::migrating_fv_price(p, self.x0, self.y0, self.x1, self.y1)?
        } else {
            p
        };
        let sched = PammFeeSchedule {
            tiers: &self.tiers,
            base_supply: 1_000_000_000_000_000,
            creator_fee_configurable: self.creator_fee_configurable,
            pool_creator_fee_bps: 0,
            coin_creator_is_default: self.creator_is_default,
            cap_bps: Some(cap_bps),
        };
        let f = fees::pump_amm(self.y1, self.x1, &sched).ok()?;
        let l = quote::liquidity(self.x1, self.y1).ok()?;
        let (x, y) = quote::reprice(l, p1)?;
        sol_leg::buy_exact_out(q, x, y, false, &f).map(|b| b.cost)
    }
}

/// [`PricedPool`] of `pm`'s curve now; `None` unless it is complete and not migrated (`vT > 0`).
pub fn priced_pool(env: &Env, pm: &PumpMarket) -> Option<PricedPool> {
    let svm = &env.svm;
    let live = pm.curve.curve_state(svm);
    if !live.complete || live.vt == 0 {
        return None;
    }
    let c = pm.curve.state(svm);
    let a = u128::from(read::token_amount(svm, &pm.curve.curve_base_ata)?);
    let (pcb, pcq) = (c.post_complete_base_out, c.post_complete_quote_in);
    let pc = pcb != 0 || pcq != 0;
    let known = !pc || ((pcb != 0) == (pcq != 0) && c.quote_offset_consistent());
    let g = svm.get_account(&PUMP_GLOBAL)?;
    let fee_m = vpump::read_global(&g.owner.to_bytes(), &g.data)
        .ok()?
        .pool_migration_fee();
    let (x1, y1) = quote::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee: fee_m,
            pool_tokens: if pc { a } else { STANDARD_POOL_TOKENS },
            post_complete_quote_in: pcq,
        },
    )?;
    let (x0, y0) = quote::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee: fee_m,
            pool_tokens: STANDARD_POOL_TOKENS,
            post_complete_quote_in: 0,
        },
    )?;
    let pfc = svm.get_account(&PUMP_AMM_FEE_CONFIG)?;
    let tiers = vpump::read_fee_config(&pfc.owner.to_bytes(), &pfc.data)
        .ok()?
        .fee_tiers
        .iter()
        .map(|t| FeeTier {
            threshold: t.threshold,
            fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
        })
        .collect();
    let gc = svm.get_account(&PUMP_AMM_GLOBAL_CONFIG)?;
    let creator_fee_configurable = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data)
        .ok()?
        .creator_fee_configurable();
    Some(PricedPool {
        a,
        pc,
        known,
        x0,
        y0,
        x1,
        y1,
        tiers,
        creator_fee_configurable,
        creator_is_default: c.creator_is_default(),
    })
}

/// `(FV, p')` of `pos` at `p` on `pp` (module docs) at `now`, given its rent due; `None` when
/// `B̂_FV` is undefined.
fn fv_at(
    pp: &PricedPool,
    pos: &ShortPosition,
    p: u128,
    r_due: u64,
    now: i64,
) -> Option<(u64, u32)> {
    let b = pp.b_fv(pos.q, p, u64::from(pos.venue_fee_hard_cap_bps))?;
    let v_eff = risk::v_eff(pos.v, r_due);
    let below = risk::cr_below(v_eff, Some(b), pos.cr_liq_bps).ok()?;
    let rate = risk::fv_rate_bps(
        !(now >= pos.deadline && !below),
        u32::from(pos.penalty_bps),
        u32::from(pos.expiry_fee_bps),
    );
    let fv = risk::fair_value(Some(b), rate, r_due)?;
    Some((fv.saturating_add(FV_SKEW.with(std::cell::Cell::get)), rate))
}

/// The `migrating_fv` oracle on one `SolSettle(S3)` event `ev` of `pm`'s market, which was
/// `PumpMigrating` before the transaction and still is (module docs): `pos` the settled
/// position as it stood before. `Ok` names the case for the counters; `Err` describes a
/// mismatch.
pub fn check_s3(
    env: &Env,
    pm: &PumpMarket,
    pos: &ShortPosition,
    ev: &SolSettle,
) -> Result<&'static str, String> {
    let pp = priced_pool(env, pm).ok_or("a migrating curve without a priced pool")?;
    let m = env.market(&pm.market());
    let now = env.now();
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).map_err(|e| format!("R_due: {e:?}"))?;
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).map_err(|e| format!("K_s: {e:?}"))?;
    let take = pos.v - k_s;
    let p_ema = m.ema_price.get();
    let ctx = || {
        format!(
            "position {} q {} V {} take {take} R_due {r_due} deadline {} now {now}; P_ema {p_ema}; \
             pool {pp:?}; event: box P ({}, {}), x ({}, {}), p' ({}, {}), fv {}, K_s {}, lenders {}",
            ev.position,
            pos.q,
            pos.v,
            pos.deadline,
            ev.p_lo,
            ev.p_hi,
            ev.x_lo,
            ev.x_hi,
            ev.pprime_lo,
            ev.pprime_hi,
            ev.fv,
            ev.k_s,
            ev.lender_amount
        )
    };
    if ev.k_s != k_s {
        return Err(format!("S3 paid K_s {}, not {k_s}\n{}", ev.k_s, ctx()));
    }
    // The box corners: `(P_lo, L_hi)` and `(P_hi, L_lo)` (a migrating FV reads no `L`), each with
    // its own `p'`; an undefined FV settles both at the take.
    let at = |p: u128| {
        if pp.known {
            fv_at(&pp, pos, p, r_due, now)
        } else {
            None
        }
    };
    for (corner, p, got_x, got_p) in [
        ("lo", ev.p_lo, ev.x_lo, ev.pprime_lo),
        ("hi", ev.p_hi, ev.x_hi, ev.pprime_hi),
    ] {
        let want = at(p);
        let x = want.map_or(take, |(f, _)| f.min(take));
        let rate = want.map_or(0, |(_, r)| r);
        if (got_x, u32::from(got_p)) != (x, rate) {
            return Err(format!(
                "the box corner {corner} settled (x, p') ({got_x}, {got_p}), §5.4 gives \
                 ({x}, {rate})\n{}",
                ctx()
            ));
        }
    }
    let want_fv = at(p_ema).map(|(f, _)| f);
    let fv = want_fv.unwrap_or(0);
    if ev.fv != fv {
        return Err(format!(
            "S3's FV {} at P_ema, §5.4 on (X₁, Y₁) gives {fv}{}\n{}",
            ev.fv,
            if pp.known {
                ""
            } else {
                " (undefined: the post-complete fields are not known)"
            },
            ctx()
        ));
    }
    let lenders = want_fv.map_or(take, |f| f.min(take));
    if ev.lender_amount != lenders {
        return Err(format!(
            "S3 paid the lenders {}, min(FV, take) is {lenders}\n{}",
            ev.lender_amount,
            ctx()
        ));
    }
    Ok(match (pp.known, pp.pc) {
        (false, _) => "fuzz:migrating-fv-undefined",
        (true, true) => "fuzz:migrating-fv-pool-part",
        (true, false) => "fuzz:migrating-fv-standard",
    })
}

/// The second half of the oracle (module docs): after `migrate_v2` landed, the pool it created
/// against `priced`, the curve's [`PricedPool`] read just before it.
pub fn check_created_pool(
    env: &Env,
    pm: &PumpMarket,
    priced: Option<&PricedPool>,
) -> Result<&'static str, String> {
    let pp = priced.ok_or("migrate_v2 landed on a curve that was not complete")?;
    let r = pump::NewPool::of(&pm.curve)
        .reserves(&env.svm)
        .ok_or("migrate_v2 landed but the canonical pool does not read")?;
    let base = u128::from(r.base);
    let ctx = || {
        format!(
            "created (base {base}, effective quote {}); priced {pp:?}",
            r.effective_quote
        )
    };
    if base != pp.a {
        return Err(format!(
            "migrate_v2 seeded {base} tokens, not the live curve_base_ata\n{}",
            ctx()
        ));
    }
    if pp.pc && base != pp.x1 {
        return Err(format!(
            "after a pool part X₁ {} is not the created pool's\n{}",
            pp.x1,
            ctx()
        ));
    }
    if !pp.pc && base < pp.x1 {
        return Err(format!(
            "the created pool holds less than the standard X₁\n{}",
            ctx()
        ));
    }
    if !(r.effective_quote <= pp.y1 && pp.y1 - r.effective_quote <= 1) {
        return Err(format!(
            "the created pool's effective quote is not Y₁ {} (≤ +1)\n{}",
            pp.y1,
            ctx()
        ));
    }
    Ok(match (pp.pc, base == STANDARD_POOL_TOKENS) {
        (true, _) => "fuzz:migrate-pool-checked:pool-part",
        (false, true) => "fuzz:migrate-pool-checked:standard",
        (false, false) => "fuzz:migrate-pool-checked:donated",
    })
}

/// The open positions of `pm`'s market as they stand (`key → ShortPosition`), for
/// [`check_s3`]'s pre-state.
pub fn positions(
    env: &Env,
    keys: impl Iterator<Item = Pubkey>,
) -> std::collections::BTreeMap<Pubkey, ShortPosition> {
    keys.filter_map(|k| read::borsh::<ShortPosition>(&env.svm, &k).map(|p| (k, p)))
        .collect()
}
