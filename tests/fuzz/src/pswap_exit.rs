//! PumpSwap's differential exit oracle (M10, decision level; ARCH §3.6, §3.7, §8.3 I11 / I-E11):
//! every keeper `liquidate` / `expire` the fuzzer sends on a PumpSwap market is decided first by
//! `raydium_v4_support`'s venue-generic predictor ([`crate::v4::predict`]: the robust-spot and
//! EMA legs, "normal buyback allowed", the references and `max_in` of `shortwin_math::bounds`, in
//! the program's order) on a view built here from the hooks' PumpSwap venue view — its pricing
//! reserves `(base vault, quote vault + VQR)` and `Pricing::PumpSwap` at the live tier's fees, the
//! risk view capped at the hard cap and (AIR-10) the fee at `P_ema` read before the crank —
//! with the reference liquidity capped at the burned-LP depth `D` itself (`λ_min` =
//! `PUMPSWAP_LOCK_MIN_BPS` = 100 %, OWNER-M6D-1 / M6R-1), on the state after a same-second
//! `crank_price` on a clone of the VM (sent without the hooks: a measurement). The exit must then land with the predicted `b_live`,
//! `b_ref` and `max_in` or be refused with the predicted code ([`crate::curve_exit::check_outcome`]).
//! Lamports and tokens are the hooks' (I9, I11, I12): PumpSwap's buy legs carry the coin creator's
//! vault, the cashback and the fee recipients, which no support module of `tests/litesvm`
//! recomputes yet.

use anchor_lang::prelude::Pubkey;
use shortwin::state::{ShortPosition, VenueKind};
use shortwin_litesvm_tests::engine::{VenueMarket, crank_price_ix};
use shortwin_litesvm_tests::invariants::venue_view;
use shortwin_litesvm_tests::{Env, pda, read};
use shortwin_math::fees::FeeBps;
use shortwin_math::quote::Pricing;

use crate::curve_exit::{Bound, Which};
use crate::v4;

/// A keeper exit's predicted decision on PumpSwap.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub which: Which,
    pub decision: Result<Bound, shortwin::errors::ShortwinError>,
    /// The figures the decision rests on (a mismatch's message).
    pub detail: String,
}

/// The predictions of both keeper exits — `liquidate`, then `expire` — for `borrower`'s position
/// `nonce` on the PumpSwap market `vm`, on one clone after a same-second crank (the VM is
/// restored); `None` off PumpSwap (a market still on the curve, or one the crank moves). The
/// keeper tries the expiry right after a refused liquidation in the same second, on the same
/// state: the world reuses the second prediction then.
pub fn predict(
    env: &mut Env,
    vm: &dyn VenueMarket,
    borrower: &Pubkey,
    nonce: u64,
) -> Option<(Prediction, Prediction)> {
    // AIR-10: the exit reads the fee at `P_ema` (which caps its risk view) before its prologue
    // rolls the EMA, so on the state before the crank.
    let pre = env.market(&vm.market());
    let ema_fee_ppm = venue_view::view(&env.svm, &pre)
        .filter(|v| v.executable)
        .map(|v| v.cap_fee_ppm);
    // The crank on the clone is the exit's own prologue, a measurement: sent without the hooks
    // (the real exit goes through `Env::send`), as `World::observe_exits` cranks its pre-state.
    let saved = env.svm.clone();
    let tx = env.transaction(&[crank_price_ix(&vm)], &[]);
    let crank =
        shortwin_litesvm_tests::env::send_direct(&mut env.svm, |svm| svm.send_transaction(tx));
    let p = crank.is_ok().then(|| {
        Some((
            decide(
                env,
                &vm.market(),
                borrower,
                nonce,
                Which::Liquidate,
                ema_fee_ppm,
            )?,
            decide(
                env,
                &vm.market(),
                borrower,
                nonce,
                Which::Expire,
                ema_fee_ppm,
            )?,
        ))
    });
    env.svm = saved;
    p.flatten()
}

/// `ema_fee_ppm`: the pre-crank view's `min(φ_live, φ at P_ema)`, which caps the risk quotes
/// exactly as the program's `min(hard cap, ⌈φ at P_ema/100⌉)` does on an executable pool.
fn decide(
    env: &Env,
    market: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    which: Which,
    ema_fee_ppm: Option<u32>,
) -> Option<Prediction> {
    let m = env.market(market);
    if m.venue_kind() != Some(VenueKind::PumpSwap) {
        return None;
    }
    let pos: ShortPosition = read::borsh(&env.svm, &pda::position(market, borrower, nonce).0)?;
    let venue = venue_view::view(&env.svm, &m)?;
    // A pool that cannot execute refuses `NoRoute` before any quote: the fallback is never read.
    let live = venue
        .pricing
        .unwrap_or(Pricing::PumpSwap(FeeBps::new(0, 0, 0)));
    let view = v4::View {
        m: v4::MarketView {
            now: m.last_accrual_ts,
            pool: m.pool(),
            vault: *env.market_vault(market).vault(),
            venue,
            live,
            donated: 0,
            s3_no_route: false,
            ema_fee_ppm,
            market: m,
        },
        pos,
        pos_lamports: 0,
        exposure_q_open: 0,
        exposure_lamports: 0,
    };
    let pred = v4::predict(&view);
    let w = match which {
        Which::Liquidate => v4::Which::Liquidate,
        Which::Expire => v4::Which::Expire,
    };
    let decision = pred.decision(w).map(|p| Bound {
        b_live: p.b_live,
        b_ref: p.b_ref,
        max_in: p.max_in,
    });
    let r = match which {
        Which::Liquidate => pred.liq,
        Which::Expire => pred.exp,
    };
    let detail = format!(
        "executable {} ema_warm {} fresh {} spot_leg {} ema_leg {} expired {} buyback_allowed {} \
         b_live {:?} reference {r:?} q {} reserves {:?} pool_d {}",
        pred.view.executable(),
        pred.view.market().ema_warm,
        pred.fresh,
        pred.spot_leg,
        pred.ema_leg,
        pred.expired,
        pred.buyback_allowed,
        pred.b_live,
        pred.view.pos.q,
        pred.view.m.venue.reserves,
        pred.view.market().pool_d.get(),
    );
    Some(Prediction {
        which,
        decision,
        detail,
    })
}
