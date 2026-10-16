//! SOL settlement on the settlement box: the inputs of one `sol_settle` evaluation, the risk
//! quotes the box predicates read, and the grief-proof creation of the `ShareEpoch` record a
//! settlement may roll (ARCH §2.1 `engine/settle.rs` — built as this module; §2.4.1, §3.8,
//! §4.5, §5.3; ECON §5.6, §8.6, §10.3; A65).
//!
//! **Every decision is `shortwin_math::settle`'s**: [`settle::box_of_capped`] (the box on the
//! ring's bucket extremes, its `L_mhi` capped at the market's depth cap, AIR-15),
//! [`settle::evaluate`] (ARCH §3.8 steps 2–4 in the sim's order: "not
//! liquidatable" disarms only on the box, refutation on the box, the S1 / S2 legs on the live
//! state, the robust spot and the box corner, one arming clock, the kind chosen at execution,
//! S3 on the market's chain), [`settle::settlement`] (S1 `V − K_s`, never capped; S2 / S3
//! `x(P_ema, L_ema)` only if the corners agree within `fv_dev_bps`, each corner with its own
//! `p'`). This module only assembles what they read, from the state the prologue left
//! (ARCH §3.0: the post-roll EMA, ring and last bucket, which never contain this instruction's
//! own observation):
//!
//! - [`RiskQuotes`]: `B̂(Q; reprice(L, P))` and σ at `φ_risk = min(φ_live,
//!   venue_fee_hard_cap_bps_pos)` (ARCH §4.3 "two fee views"; the venue's
//!   `crate::venues::risk_pricing`, which also prices a venue that cannot execute).
//! - [`inputs`]: whether the post-change warm-up is over (M4R: `settle::evaluate` decides nothing
//!   while it is not, on an executable venue), `R_due`, `K_s`, `V_eff`, the spot leg
//!   (`risk::spot_leg` on the robust spot `P_rs = min(P_live, lb_lo)` at the liquidity
//!   `max(L_live, L_ema')` — `liquidate`'s, `shortwin_math::bounds::spot_liquidity`, FITS-2 (a) —
//!   true without evaluation only when the venue cannot execute: the route-broken flag and the
//!   fee hard cap never make it true, V2-1), the EMA leg (`persist::liq_ema_leg` on the newest
//!   `liq_persist_n` ring entries' cached `(x, y)`), the live risk quote and σ, the box, the S3
//!   chain.
//!
//! **Which live-liquidity inputs take the EMA floor (FITS-2 (a)).** Only the spot leg of
//! "CR-liquidatable" — the gate of `settle::evaluate`'s step 2 (not CR-liquidatable and not
//! expired: no S1/S2 evaluation) and the "liquidatable" of the S3 timer — is judged at
//! `max(L_live, L_ema')`: it is what `liquidate` and `expire` decide on, and a withdrawal inside a
//! `[withdraw, sol_settle, deposit]` bundle must not open a gate the honest state keeps shut (an
//! S2 at fair value pays the penalty in `p'` like a liquidation). The rest keeps `L_live`: the
//! S1 / S2 legs are conjunctions with the box corner `(P_lo, L_hi)` — the corner least favourable
//! to them, on the post-roll EMA and ring medians no transaction moves — so a drained `L_live`
//! cannot make them hold alone; the refutation reads `min(L_live, L_lo)` against deposits (a
//! withdrawal only makes it harder, and an unrefuted clock still needs the corner's S1 / S2 legs
//! to execute); "not liquidatable" (the disarm) reads only the box corner `(P_hi, L_lo)` and the
//! EMA leg. What the drain does do (AIR-20, kept): inside such a bundle the live and robust-spot
//! S1 / S2 legs read the drained pool, so the box corner is their only veto. It vetoes whatever
//! the honest robust spot `(P_rs, L_live)` vetoes while that lies in the box, not once the honest
//! state has left it — a reversal below `P_lo`, or depth above `L_hi` (a deposit the medians and
//! `L_ema` have not followed yet, or third-party depth above the depth cap) — and there an
//! unlocked LP can arm an S1 / S2 the honest live legs veto, and execute it if the state is
//! still outside the box `settle_persist_n` updates later. In AIR-20's model runs at the default
//! parameters the borrower loses nothing by it (where it bites, the honest liquidation leaves the
//! borrower nothing either; the protocol's rent and penalty share go to the lenders, `K_s` to the
//! keeper); a loss of up to about 1.2 % of `B̂` needs `pool_lock_min_bps` at its 20 % floor and a
//! 1 % fee. Flooring these legs at `max(L_live, L_ema')` would change ECON §5.6's leg definitions
//! and the curve's live quote.
//! - [`terms`]: the position's locked settlement terms (A26).
//! - [`create_pda_account`]: the grief-proof PDA creation of ARCH §2.4.1.
//!
//! The Emergency route-broken flag enters **only** through `s3_no_route` (the S3 chain the
//! prologue ran and "no route now"): never a quote, a leg, the box or the arming clock (I10,
//! I16; T-S3-BROKEN).

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Allocate, Assign, Transfer};
use shortwin_math::observe::{robust_spot, Observation};
use shortwin_math::persist;
use shortwin_math::quote::reprice;
use shortwin_math::rent;
use shortwin_math::risk::{self, Budget};
use shortwin_math::settle::{self, BoxQuotes, S3Chain, SettleInputs, SettleTerms};
use shortwin_math::MathError;

use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::lamports;
use crate::state::{Market, ShortPosition, VenueKind};
use crate::venues::{Quoter, VenueState};

/// The position's risk quotes (ARCH §4.3, §4.5): every quote at the capped fee view the caller
/// resolved (`crate::venues::risk_quoter`), for the position's `q`. The box predicates call
/// [`BoxQuotes`] on `(P, L)` states; the EMA leg calls [`RiskQuotes::cost_xy`] on `(x, y)`
/// states (a ring entry's cached reprojection: on the curve the virtual reserves with the live
/// offsets), the live leg [`RiskQuotes::live`].
#[derive(Clone, Copy, Debug)]
pub struct RiskQuotes<'a> {
    quoter: Quoter<'a>,
    q: u64,
}

impl<'a> RiskQuotes<'a> {
    /// Risk quotes of `q` tokens at `quoter` (already capped at the position's hard cap and, on
    /// PumpSwap, at φ at `P_ema`: `VenueState::exit_risk_cap_bps`, AIR-10).
    pub const fn new(quoter: Quoter<'a>, q: u64) -> Self {
        Self { quoter, q }
    }

    /// `B̂(q; x, y)`: `None` (`+∞`) if undefined, including `x = 0` (a ring entry without a
    /// reprojection).
    pub fn cost_xy(&self, x: u128, y: u128) -> Option<u64> {
        self.quoter.buy_xy(self.q, x, y).map(|b| b.cost)
    }

    /// σ of a quote on the state `(x, y)` (`risk::sigma_xy` of its fee-free input; for `B̂_str`
    /// the pool's `n` over the curve's price, ARCH §4.3): `u64::MAX` (`+∞`) when the quote or the
    /// state's price is undefined.
    fn sigma_of(&self, n: Option<u64>, x: u128, y: u128) -> core::result::Result<u64, MathError> {
        if x == 0 || y == 0 {
            return Ok(u64::MAX);
        }
        match risk::sigma_xy(n, self.q, x, y) {
            Err(MathError::DivByZero) => Ok(u64::MAX),
            r => r,
        }
    }

    /// σ on `(x, y)` (`risk::sigma_xy` of the quote's fee-free input): `u64::MAX` (`+∞`) when
    /// the quote or the state's price is undefined.
    pub fn sigma_on(&self, x: u128, y: u128) -> core::result::Result<u64, MathError> {
        let n = self.quoter.buy_xy(self.q, x, y).map(|b| b.net);
        self.sigma_of(n, x, y)
    }

    /// The live leg's risk quote and σ on the venue's live reserves `(x, y)` (the curve: its
    /// live state and the live `X_p` of `B̂_str`, ARCH §4.3).
    pub fn live(&self, x: u128, y: u128) -> core::result::Result<(Option<u64>, u64), MathError> {
        let b = self.quoter.buy_live(self.q, x, y);
        Ok((b.map(|b| b.cost), self.sigma_of(b.map(|b| b.net), x, y)?))
    }
}

impl BoxQuotes for RiskQuotes<'_> {
    fn cost(&mut self, p: u128, l: u128) -> core::result::Result<Option<u64>, MathError> {
        Ok(self.quoter.buy_repriced(self.q, l, p).map(|b| b.cost))
    }

    fn sigma(&mut self, p: u128, l: u128) -> core::result::Result<u64, MathError> {
        // `reprice(L, P)` is the curve at `P` too (`curve_reprice` keeps the offsets, which
        // `Quoter::buy_xy` applies): the sims' `slip_repriced`.
        match reprice(l, p) {
            Some((x, y)) => self.sigma_on(x, y),
            None => Ok(u64::MAX),
        }
    }

    fn fv_cost(&mut self, p: u128, l: u128) -> core::result::Result<Option<u64>, MathError> {
        Ok(self.quoter.fv_repriced(self.q, l, p).map(|b| b.cost))
    }
}

/// The position's locked settlement terms (ARCH §2.3.5; A26).
pub fn terms(pos: &ShortPosition) -> SettleTerms {
    SettleTerms {
        cr_liq_bps: pos.cr_liq_bps,
        sigma_max_bps: u32::from(pos.max_liq_slip_bps),
        penalty_bps: u32::from(pos.penalty_bps),
        expiry_fee_bps: u32::from(pos.expiry_fee_bps),
        fv_dev_bps: u32::from(pos.fv_dev_bps),
        persist_n: pos.settle_persist_n,
        arm_gap_s: pos.settle_arm_gap_s,
        s3: pos.s3_terms(),
    }
}

/// What the `SolSettle` event reports besides the settlement itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The risk quote on the live reserves (`None` when the venue cannot execute).
    pub b_live: Option<u64>,
    /// The spot leg's risk quote at the robust spot, `reprice(max(L_live, L_ema'), P_rs)`
    /// (FITS-2 (a)).
    pub b_rs: Option<u64>,
    /// The spot leg (CR at the robust spot below `L`, or no route).
    pub spot_leg: bool,
}

fn nonzero(t: i64) -> Option<i64> {
    (t != 0).then_some(t)
}

/// The market's S3 chain as the prologue left it (ARCH §3.0 step 8, §5.3), with the position's
/// `s3_no_route` of this instruction's observation (`engine::route::position_no_route`: the
/// lower of the global hard cap and the position's locked cap, AIR-19; R3A-4 c).
pub fn s3_chain(market: &Market, s3_no_route: bool) -> S3Chain {
    S3Chain {
        no_route_now: s3_no_route,
        no_route_since: nonzero(market.no_route_since),
        graduated_at: nonzero(market.graduated_at),
        migrating: market.venue_kind() == Some(VenueKind::PumpMigrating),
        migration_seen_at: nonzero(market.migration_seen_at),
    }
}

/// ARCH §3.8 step 1 after the prologue: `R_due`, `V_eff`, `K_s`, the legs of §4.5 and the
/// box, as `settle::evaluate` reads them (module docs). `obs` is the market's post-roll
/// observation state, `state` the venue's pre-trade state this instruction observed,
/// `depth_cap` the market's `engine::bounds::ref_depth_cap` (the cap of the box's `L_mhi`,
/// AIR-15), `now` the prologue's time.
#[allow(clippy::too_many_arguments)]
pub fn inputs(
    pos: &ShortPosition,
    market: &Market,
    obs: &Observation,
    state: &VenueState,
    quotes: &mut RiskQuotes<'_>,
    s3_no_route: bool,
    depth_cap: Option<u128>,
    now: i64,
) -> Result<(SettleInputs, Facts)> {
    let r_due = rent::due(&pos.pos_rent(), &market.basis()).math()?;
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).math()?;
    let v_eff = risk::v_eff(pos.v, r_due);
    let executable = state.executable;
    let l_live = state.liquidity().unwrap_or(obs.ema.liq);
    let p_rs = state
        .price()
        .and_then(|p| robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let (b_live, sigma_live) = if executable {
        quotes
            .live(u128::from(state.x), u128::from(state.y))
            .math()?
    } else {
        (None, u64::MAX)
    };
    // The spot leg at `max(L_live, L_ema')` (FITS-2 (a); `liquidate`'s): the gate of step 2 (and
    // of the S3 timer's "liquidatable") cannot be opened by a withdrawal inside the transaction.
    // The S1 / S2 legs and the refutation keep `L_live` (module docs).
    let l_spot = shortwin_math::bounds::spot_liquidity(l_live, &obs.ema, &obs.ring).math()?;
    let b_rs = match p_rs {
        Some(p) => quotes.cost(p, l_spot).math()?,
        None => None,
    };
    let spot_leg = risk::spot_leg(executable, fresh, v_eff, b_rs, pos.cr_liq_bps).math()?;
    let ema_leg = {
        let q = *quotes;
        let below = risk::ring_below(v_eff, pos.cr_liq_bps, |x, y| q.cost_xy(x, y));
        persist::liq_ema_leg(&obs.ring, below, pos.liq_persist_n).math()?
    };
    // The box collapses to the EMA point only on a venue that cannot execute now (M2-SEC-1):
    // on an executable venue `deposit` / `withdraw` roll a pushed bucket without recording
    // (A16), so an aged EMA is no evidence that nothing was pushed. AIR-15: its `L_mhi` credits
    // no more depth than sizing does (the exit reference's M4F-1 cap), so deposit-wrapped
    // cranks cannot widen `L_hi` at will.
    let bx = settle::box_of_capped(&obs.ema, &obs.ring, now, executable, depth_cap).math()?;
    let inp = SettleInputs {
        now,
        deadline: pos.deadline,
        ema_count: obs.ema.count,
        p_ema: obs.ema.price,
        l_ema: obs.ema.liq,
        bx,
        budget: Budget {
            v: pos.v,
            k_s,
            ert: pos.ert,
        },
        r_due,
        cr_liquidatable: spot_leg && ema_leg,
        ema_leg,
        executable,
        warmed: shortwin_math::observe::warmed(&obs.ema),
        p_rs,
        l_live,
        b_live,
        sigma_live,
        s3: s3_chain(market, s3_no_route),
    };
    Ok((
        inp,
        Facts {
            b_live,
            b_rs,
            spot_leg,
        },
    ))
}

/// Grief-proof creation of a short.win-owned PDA account (ARCH §2.4.1 "The ephemeral WSOL
/// account", C0-3; used for `ShareEpoch` records, A39). A third party can pre-fund the address
/// (a System transfer checks only its source), which makes `create_account` fail with
/// `AccountAlreadyInUse`, but it cannot allocate or assign it (only short.win can sign for the
/// PDA). So: (1) any pre-funded lamports are swept to `sweep_to` (the PDA signs a System
/// transfer), (2) `payer` funds exactly the rent-exempt minimum of `space`, (3) `allocate(space)`
/// and `assign(owner)` signed with `seeds` (the PDA's seeds with its bump). Returns the swept
/// lamports, which the caller books.
///
/// Errors: `AccountMismatch` if the account is not System-owned with no data (it already
/// exists).
pub fn create_pda_account<'info>(
    payer: &AccountInfo<'info>,
    account: &AccountInfo<'info>,
    sweep_to: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    seeds: &[&[u8]],
    space: usize,
    owner: &Pubkey,
) -> Result<u64> {
    require!(
        *account.owner == System::id() && account.data_is_empty(),
        ShortwinError::AccountMismatch
    );
    let swept = account.lamports();
    if swept > 0 {
        system_program::transfer(
            CpiContext::new_with_signer(
                system_program.key(),
                Transfer {
                    from: account.clone(),
                    to: sweep_to.clone(),
                },
                &[seeds],
            ),
            swept,
        )?;
    }
    system_program::transfer(
        CpiContext::new(
            system_program.key(),
            Transfer {
                from: payer.clone(),
                to: account.clone(),
            },
        ),
        lamports::rent_min(space)?,
    )?;
    let space_u64 = u64::try_from(space).ok().or_math()?;
    system_program::allocate(
        CpiContext::new_with_signer(
            system_program.key(),
            Allocate {
                account_to_allocate: account.clone(),
            },
            &[seeds],
        ),
        space_u64,
    )?;
    system_program::assign(
        CpiContext::new_with_signer(
            system_program.key(),
            Assign {
                account_to_assign: account.clone(),
            },
            &[seeds],
        ),
        owner,
    )?;
    Ok(swept)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use shortwin_math::fees::CpmmFees;
    use shortwin_math::num::WAD;
    use shortwin_math::quote::Pricing;

    fn cpmm(fee_ppm: u64) -> Pricing {
        let f = CpmmFees {
            trade: fee_ppm,
            creator: 0,
            creator_on_input: false,
            protocol: 120_000,
            fund: 0,
        };
        Pricing::Cpmm { buy: f, sell: f }
    }

    /// The box predicates' quotes are the repriced risk quotes, and an undefined state reads
    /// as `+∞` (cost `None`, σ `u64::MAX`), never an error.
    #[test]
    fn risk_quotes_on_repriced_states() {
        let mut q = RiskQuotes::new(Quoter::Pool(cpmm(2_500)), 1_000_000);
        let (l, p) = (10u128.pow(12), WAD / 1_000);
        let (x, y) = reprice(l, p).unwrap();
        assert_eq!(q.cost(p, l).unwrap(), q.cost_xy(x, y));
        assert_eq!(
            q.cost_xy(x, y),
            cpmm(2_500).buy_repriced(1_000_000, l, p).map(|b| b.cost)
        );
        let s = q.sigma(p, l).unwrap();
        assert!(s < 10, "a tiny trade has almost no premium: {s}");
        // More liquidity at the same price: cheaper and less slippage (B̂ and σ fall with L),
        // for a trade of 3 % of the token reserve.
        let mut big = RiskQuotes::new(Quoter::Pool(cpmm(2_500)), 1_000_000_000_000);
        assert!(big.cost(p, l * 4).unwrap() < big.cost(p, l).unwrap());
        assert!(big.sigma(p, l * 4).unwrap() < big.sigma(p, l).unwrap());
        // Undefined: no tokens to buy, or no reprojection.
        let mut huge = RiskQuotes::new(Quoter::Pool(cpmm(2_500)), u64::MAX);
        assert_eq!(huge.cost(p, 1_000).unwrap(), None);
        assert_eq!(huge.sigma(p, 1_000).unwrap(), u64::MAX);
        assert_eq!(q.cost_xy(0, 1), None);
        assert_eq!(q.sigma_on(0, 1).unwrap(), u64::MAX);
        assert_eq!(q.sigma(0, l).unwrap(), u64::MAX);
    }
}
