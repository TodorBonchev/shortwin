//! Venue admin on PumpSwap with a position open (M10 WP-8a; ARCH §3.12 PumpSwap, §4.3 "two fee
//! views", §5.3, §8.2; A43, A63, V1-8, V2-1, O15, Q6): what PumpSwap's admin, the pool's own
//! state and the Emergency multisig can do to an open PumpSwap position, on the dumped pAMM and
//! pump-fees ELFs.
//!
//! - **G-G4, PumpSwap `FeeConfig` hikes**: pAMM's `FeeConfig` (`5PHirr…`) has pump-fees' layout,
//!   so `fork::set_fee_config` is its byte builder (the state `update_fee_config` /
//!   `upsert_fee_tiers` leave). Between `venue_fee_cap_bps` and the hard cap: opens refused, no
//!   S3 evidence, closes and liquidations at the live fee. Above the hard cap: the S3 chain runs while the pool is
//!   still observed, the risk view stays at the position's cap (a position the live fee would
//!   make CR-liquidatable is not liquidated), the borrower's close executes at the live fee, and
//!   S3 pays FV at the cap.
//! - **G-H9, mayhem** (the canonical-pool rule `pump_amm.rs:633`): a mayhem canonical pool is
//!   never listed as PumpSwap (with the pAMM init set the listing is refused; without it the
//!   market is list-only), and clearing the bit lets the detection crank move the market onto
//!   it (control). A pool turned mayhem mid-position is not executable. A non-WSOL quote pool
//!   cannot exist at the canonical address (pAMM derives it from the WSOL mint,
//!   `pda::canonical_pool`); a byte-planted one is refused like mayhem.
//! - **G-H3, a pool layout change** (the pool's discriminator) with a position open: not
//!   executable — the crank and `sol_settle` succeed and record nothing, the S3 chain starts,
//!   opens and closes `NoRoute` — and S3 at the timer prices FV at the position's cap on the
//!   frozen EMA point (the adapter's `fee_view` with no readable pool).
//! - **G-D6, the Emergency flag** (`set_route_broken`, the market or the PumpSwap venue bit):
//!   the S3 chain only; S3 of an expired healthy position after `s3_wait_s` with the expiry fee
//!   and FV at the live fee; liquidations along a real pump identical second by second with and
//!   without it; and O15's race of that S3 against an honest `expire`.
//! - **G-H2, extras on the sell leg**: a harmless fixed extra rides along (appended to the `sell`
//!   CPI, which pAMM runs ignoring the trailing account: the proceeds equal an open without
//!   extras); a hostile `current` set naming the lending vault is refused `DeniedAccount`, and
//!   `previous` lands.
//! - **M10 batch 3, the extras denylist's pool and borrower keys**: the canonical pool and its
//!   base / quote vaults on every extras site, and the position's borrower on the keeper exits,
//!   are `DeniedAccount`; a fresh key in the same set shape rides along (the control).
//!
//! PumpSwap has no `pool_fail` chain (Raydium's fast path only): it stays 0 throughout. Every
//! instruction goes through `Env::send`: the invariant hooks run after each one.

use shortwin::constants::{PUMP_AMM_FEE_CONFIG, VENUE_BIT_PUMP_SWAP};
use shortwin::events::{Expire, Liquidate};
use shortwin::state::EXTRAS_IX_SELL;
use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS};
use shortwin::{ExtraEntryArg, PauseScope, SeedPartArg};
use shortwin_litesvm_tests::engine::{set_route_broken_ix, set_venue_extras_ix};
use shortwin_math::quote::Pricing;
use shortwin_math::rent;
use solana_keypair::Keypair;

use crate::common::*;

/// Anchor's `AccountNotInitialized`: a closed position account.
const ANCHOR_ACCOUNT_NOT_INITIALIZED: u32 = 3_012;

/// Rewrites PumpSwap's `FeeConfig` so every tier charges `protocol` bps (lp and creator kept).
fn set_protocol_fee(env: &mut Env, protocol: u64) {
    let (flat, mut tiers, stable, exotic) = fork::fee_config_specs(&env.svm, &PUMP_AMM_FEE_CONFIG);
    for t in &mut tiers {
        t.1[1] = protocol;
    }
    fork::set_fee_config(
        &mut env.svm,
        &PUMP_AMM_FEE_CONFIG,
        flat,
        &tiers,
        &stable,
        exotic,
    );
}

/// `K_s` of a position (its locked settlement-keeper terms).
fn k_s(p: &ShortPosition) -> u64 {
    let pct = u128::from(p.v) * u128::from(p.settle_keeper_bps) / 10_000;
    (u128::from(p.settle_keeper_fixed) + pct)
        .min(u128::from(p.settle_keeper_cap))
        .min(u128::from(p.v)) as u64
}

/// `FV = B̂ + ⌈p'·B̂/10⁴⌉ + R_due` for a risk quote `b`.
fn fv_of(b: u64, pprime_bps: u16, r_due: u64) -> u64 {
    b + (u128::from(b) * u128::from(pprime_bps)).div_ceil(10_000) as u64 + r_due
}

/// `B̂(q; reprice(L, P))` with PumpSwap fees `f`.
fn b_repriced(f: FeeBps, q: u64, l: u128, p: u128) -> u64 {
    Pricing::PumpSwap(f)
        .buy_repriced(q, l, p)
        .expect("B̂_FV")
        .cost
}

/// The Emergency multisig sets or clears the market's route-broken flag (`venue_scope =
/// false`) or PumpSwap's bit of `venue_route_broken`.
fn set_flag(env: &mut Env, pm: &PammMarket, venue_scope: bool, broken: bool) {
    let ea = env.pause_authority.insecure_clone();
    let ix = if venue_scope {
        let bit = VENUE_BIT_PUMP_SWAP.trailing_zeros() as u8;
        set_route_broken_ix(&ea.pubkey(), PauseScope::Global, bit, broken, None)
    } else {
        set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            broken,
            Some(pm.market()),
        )
    };
    ok(env.send(&[ix], &[&ea]));
}

/// Jumps the clock to `deadline + after` of `pos` (a new slot range) and cranks 3 s there.
fn jump_to_deadline(env: &mut Env, pm: &PammMarket, pos: &ShortPosition, after: i64) {
    let slot = env.clock().slot;
    env.set_clock(pos.deadline + after, slot + 1_500_000);
    crank_secs(env, pm, 3);
}

/// Cranks and calls `sol_settle` every `step` seconds, at most `n` times: the settlement.
fn run_to_s3(
    env: &mut Env,
    pm: &PammMarket,
    kp: &Keypair,
    b: &Borrower,
    nonce: u64,
    step: i64,
    n: usize,
) -> Option<SolSettle> {
    for _ in 0..n {
        env.warp(step);
        crank(env, pm);
        let meta = ok(settle(env, pm, kp, &b.key(), nonce));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            return Some(ev);
        }
    }
    None
}

/// A position of `q` on a fresh `coin` market, cranked 3 s: the environment, the market, the
/// borrower, the nonce and the position.
fn opened(coin: PammCoin) -> (Env, PammMarket, u64, Borrower, u64, ShortPosition) {
    let (mut env, pm, q) = setup(coin);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let pos = env.position(&pm, &b.key(), nonce);
    crank_secs(&mut env, &pm, 3);
    (env, pm, q, b, nonce, pos)
}

// ---- G-G4: FeeConfig hikes ---------------------------------------------------------------------

/// G-G4 (a): PumpSwap's fee raised between `venue_fee_cap_bps` (150) and the hard cap (500)
/// with a position open: opens refused `VenueFeeTooHigh`, no S3 evidence (the chain stays
/// clear, the pool observed), the borrower's close buys back at the live fee, and so does a
/// keeper's liquidation after a pump, its bound and risk quote at the live fee.
#[test]
fn a_fee_between_the_caps() {
    let (mut env, pm, q, b, nonce, _) = opened(PammCoin::Legacy261);
    let base = pool_fees(&env, &pm.pool).oneway_bps();
    set_protocol_fee(&mut env, 300);
    let fee = pool_fees(&env, &pm.pool).oneway_bps();
    assert!(fee > 150 && fee <= 500 && fee > base, "{base} → {fee} bps");
    env.warp(1);
    crank(&mut env, &pm);
    let m = market(&env, &pm);
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    assert!(m.b_lo.get() > 0, "observed");
    let mut b2 = env.borrower();
    assert_err(&env.open(&pm, &mut b2, q).0, E::VenueFeeTooHigh);
    // The borrower's close, on a clone: PumpSwap's live charge.
    let saved = env.svm.clone();
    env.warp(1);
    let b_exp = quote_buy(&env, &pm.pool, q).unwrap();
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, b_exp, "PumpSwap's live charge");
    assert_trader_clean(&env, &pm);
    // A keeper's liquidation after a real pump (a buyer lifts the price × 2.8, held), the fee
    // still between the caps: only `NotLiquidatable` until it executes, and then the risk view
    // and the exit's bound are at the live fee, not at `venue_fee_cap_bps` — the buyback and
    // `B̂_live` are PumpSwap's live charge, and the risk quote equals it (`min(φ_live, cap) =
    // φ_live`).
    env.svm = saved;
    let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut env, &pm.pool, 28, 10));
    let kp = keeper(&mut env);
    let mut done = None;
    for _ in 0..900 {
        env.warp(1);
        crank(&mut env, &pm);
        let charge = quote_buy(&env, &pm.pool, q).unwrap();
        let r = pm.pool.reserves(&env.svm).unwrap();
        let at_venue_cap = Pricing::PumpSwap(pool_fees(&env, &pm.pool).capped(150))
            .buy(q, u128::from(r.base), r.effective_quote)
            .unwrap()
            .cost;
        let res = liquidate(&mut env, &pm, &kp, &b.key(), nonce);
        match &res {
            Ok(meta) => {
                done = Some((events::one::<Liquidate>(meta), charge, at_venue_cap));
                break;
            }
            Err(_) => assert_eq!(
                custom_code(&res),
                Some(code(E::NotLiquidatable)),
                "only not-yet"
            ),
        }
    }
    let (ev, charge, at_venue_cap) = done.expect("liquidated within 15 min");
    let fee = pool_fees(&env, &pm.pool).oneway_bps();
    assert!(fee > 150 && fee <= 500, "still between the caps: {fee} bps");
    assert!(!ev.expired && ev.penalty > 0);
    assert_eq!(ev.buyback_cost, charge, "PumpSwap's live charge");
    assert_eq!(ev.b_live, charge, "bounded by B̂_live(q) at the live fee");
    assert_eq!(ev.b_live_risk, ev.b_live, "the risk view at the live fee");
    assert!(
        at_venue_cap < ev.b_live_risk,
        "a risk view at 150 bps would quote less: {at_venue_cap} vs {}",
        ev.b_live_risk
    );
    assert_trader_clean(&env, &pm);
}

/// G-G4 (b): PumpSwap's fee raised far above the hard cap (every tier's protocol fee at 200 %)
/// with a position open. The pool is still observed, but the fee is S3 evidence: the chain
/// starts. Opens are refused. The risk view stays at the position's 500 bps: at the live fee
/// the position would be CR-liquidatable, yet `liquidate` refuses `NotLiquidatable`. The
/// borrower's own close executes at the live fee. At the deadline, after `s3_wait_s` of chain,
/// S3 pays FV at the cap — the live fee's FV is far above it.
#[test]
fn a_fee_above_the_hard_cap() {
    let (mut env, pm, q, b, nonce, pos) = opened(PammCoin::Legacy261);
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    set_protocol_fee(&mut env, 20_000);
    let live_fee = pool_fees(&env, &pm.pool);
    assert!(live_fee.oneway_bps() > cap);
    env.warp(1);
    crank(&mut env, &pm);
    let m = market(&env, &pm);
    assert_eq!(m.no_route_since, env.now(), "S3 evidence");
    assert_eq!(m.pool_fail_since, 0, "PumpSwap has no fast-path chain");
    assert!(
        m.b_lo.get() > 0,
        "a fee above the cap does not stop observation"
    );
    let mut b2 = env.borrower();
    assert_err(&env.open(&pm, &mut b2, q).0, E::VenueFeeTooHigh);
    // At the live fee the position would be CR-liquidatable; at its cap it is not.
    let r = pm.pool.reserves(&env.svm).unwrap();
    let (x, y) = (u128::from(r.base), r.effective_quote);
    let live = Pricing::PumpSwap(live_fee).buy(q, x, y).unwrap().cost;
    let capped = Pricing::PumpSwap(live_fee.capped(cap))
        .buy(q, x, y)
        .unwrap()
        .cost;
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let v_eff = u128::from(pos.v - r_due);
    assert!(v_eff * 10_000 < u128::from(live) * u128::from(pos.cr_liq_bps));
    assert!(v_eff * 10_000 > u128::from(capped) * u128::from(pos.cr_liq_bps));
    let kp = keeper(&mut env);
    for _ in 0..5 {
        env.warp(1);
        crank(&mut env, &pm);
        assert_err(
            &liquidate(&mut env, &pm, &kp, &b.key(), nonce),
            E::NotLiquidatable,
        );
    }
    // The borrower's close, on a clone: the live fee.
    let saved = env.svm.clone();
    env.warp(1);
    let b_exp = quote_buy(&env, &pm.pool, q).unwrap();
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, b_exp, "the live 200 %+ fee");
    env.svm = saved;
    // S3 at the deadline, FV at the locked cap.
    jump_to_deadline(&mut env, &pm, &pos, -5);
    let since = market(&env, &pm).no_route_since;
    let ev = run_to_s3(&mut env, &pm, &kp, &b, nonce, 30, 30).expect("S3");
    assert_eq!(ev.condition, 3);
    assert!(env.now() >= since + i64::from(pos.s3_wait_s) && env.now() < since + 630);
    assert_eq!(ev.pprime_lo, pos.expiry_fee_bps, "O15");
    let m = market(&env, &pm);
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let live_fee = pool_fees(&env, &pm.pool);
    let fv = fv_of(
        b_repriced(live_fee.capped(cap), pos.q, l, p),
        ev.pprime_lo,
        r_due,
    );
    assert_eq!(ev.fv, fv, "FV at (P_ema, L_ema) and the locked cap");
    let at_live = fv_of(b_repriced(live_fee, pos.q, l, p), ev.pprime_lo, r_due);
    assert!(at_live > 2 * fv, "never the live fee");
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
}

// ---- G-H9 and G-H3: a pool that is no longer the canonical pool, or no longer parses ----------

/// A byte-level change of the pool account.
type PoolPatch = fn(&mut Vec<u8>);

/// A list-only market's detection set `[bonding_curve, canonical_pool]` (ARCH §3.12 `None`).
struct Detection(shortwin_litesvm_tests::fixtures::MarketKeys, Pubkey, Pubkey);

impl VenueMarket for Detection {
    fn keys(&self) -> &shortwin_litesvm_tests::fixtures::MarketKeys {
        &self.0
    }
    fn read_set(&self, _w: bool) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.1, false),
            AccountMeta::new_readonly(self.2, false),
        ]
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        Vec::new()
    }
}

/// G-H9 (a), Q6: a canonical pool in mayhem mode at listing. With the pAMM init set
/// `create_market(Auto)` is refused `BadRemainingAccounts` (the detection is not PumpSwap, so
/// the init set is one account list too many); without it the market lists list-only (`None`),
/// never PumpSwap. Control: with the mayhem bit cleared, the detection crank moves the market
/// onto the pool (`None → PumpSwap`).
#[test]
fn a_mayhem_pool_is_never_listed_as_pumpswap() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let saved = env.svm.get_account(&p.pool).expect("pool");
    fork::patch(&mut env.svm, &p.pool, |d| d[vamm::OFF_IS_MAYHEM_MODE] = 1);
    assert!(
        p.state(&env.svm).is_mayhem_mode,
        "the pool parses, in mayhem"
    );
    assert_err(
        &env.list_pamm(&p.mint, &p.base_program),
        E::BadRemainingAccounts,
    );
    let creator = env.funded_keypair();
    let mut ix = pamm::create_pamm_market_ix(&creator.pubkey(), &p.mint, &p.base_program);
    ix.accounts.truncate(ix.accounts.len() - 3);
    ok(env.send(&[ix], &[&creator]));
    let market_key = pda::market(&p.mint).0;
    assert_eq!(
        env.market(&market_key).venue_kind(),
        Some(VenueKind::None),
        "list-only"
    );
    let lender = env.funded_keypair();
    let pm = PammMarket::new(&env.svm, &p.mint, lender, Pubkey::default());
    let det = Detection(pm.keys, pda::bonding_curve(&p.mint), p.pool);
    env.warp(1);
    let meta = crank(&mut env, &det);
    assert!(
        events::of::<VenueStateChanged>(&meta).is_empty(),
        "still mayhem: stays list-only"
    );
    env.svm.set_account(p.pool, saved).unwrap();
    env.warp(1);
    venue_changed(&crank(&mut env, &det), VenueKind::None, VenueKind::PumpSwap);
}

/// G-H9 (b) and G-H3: the canonical pool, with a position open, turns mayhem (Q6), gets a
/// non-WSOL quote mint (a byte plant: pAMM cannot create one at the canonical address), or
/// changes layout (its discriminator). Each is not executable: the crank and `sol_settle`
/// succeed and record nothing (`b_lo` 0, the EMA frozen), the S3 chain starts, the market stays
/// PumpSwap, opens and closes fail `NoRoute`, the keeper's liquidation too. The expired position
/// settles S3 once the chain has run `s3_wait_s`, at the frozen EMA point, priced at the
/// position's hard cap (`fee_view` without a readable canonical pool) — not at the pool's last
/// live fee (that FV differs). The old bytes back, the chain clears at the next observation.
#[test]
fn a_pool_that_stops_being_canonical_or_parsing_with_a_position_open() {
    /// A quote mint other than WSOL (any key; pAMM never creates such a pool at this address).
    const OTHER_QUOTE: [u8; 32] = [7; 32];
    let cases: [(&str, PoolPatch); 3] = [
        ("mayhem", |d| d[vamm::OFF_IS_MAYHEM_MODE] = 1),
        ("non-WSOL quote", |d| {
            d[vamm::OFF_QUOTE_MINT..vamm::OFF_QUOTE_MINT + 32].copy_from_slice(&OTHER_QUOTE)
        }),
        ("layout", |d| d[0] ^= 0xff),
    ];
    for (what, patch) in cases {
        let (mut env, pm, q, b, nonce, pos) = opened(PammCoin::Legacy261);
        let live_fee = pool_fees(&env, &pm.pool);
        let original = env.svm.get_account(&pm.pool.pool).unwrap();
        fork::patch(&mut env.svm, &pm.pool.pool, patch);
        env.warp(1);
        let meta = crank(&mut env, &pm);
        assert!(
            events::of::<VenueStateChanged>(&meta).is_empty(),
            "{what}: no venue transition"
        );
        let m = market(&env, &pm);
        assert_eq!(m.venue_kind(), Some(VenueKind::PumpSwap));
        assert_eq!(m.no_route_since, env.now(), "{what}: the chain starts");
        assert_eq!(m.b_lo.get(), 0, "{what}: nothing recorded");
        // The hooks' own view (I13 / I16) sees the venue as not executable too, so a recorded
        // observation or a missing chain here would fail every instruction's hooks.
        let view = shortwin_litesvm_tests::invariants::venue_view::view(&env.svm, &m);
        assert!(!view.expect("a venue view").executable);
        let kp = keeper(&mut env);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        assert!(events::of::<SolSettle>(&meta).is_empty());
        assert_err(&env.close(&pm, &b, nonce), E::NoRoute);
        assert_err(&liquidate(&mut env, &pm, &kp, &b.key(), nonce), E::NoRoute);
        let mut b2 = env.borrower();
        assert_err(&env.open(&pm, &mut b2, q).0, E::NoRoute);
        let frozen = market(&env, &pm);
        let saved = env.svm.clone();
        jump_to_deadline(&mut env, &pm, &pos, -5);
        let since = market(&env, &pm).no_route_since;
        let ev = run_to_s3(&mut env, &pm, &kp, &b, nonce, 30, 30).expect("S3");
        assert_eq!(ev.condition, 3, "{what}");
        assert!(env.now() >= since + i64::from(pos.s3_wait_s) && env.now() < since + 630);
        let m = market(&env, &pm);
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        assert_eq!(
            (p, l),
            (frozen.ema_price.get(), frozen.ema_liq.get()),
            "{what}: the EMA is frozen"
        );
        assert_eq!((ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi), (p, p, l, l));
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps);
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let fv = fv_of(
            b_repriced(FeeBps::new(0, cap, 0), pos.q, l, p),
            ev.pprime_lo,
            r_due,
        );
        assert_eq!(ev.fv, fv, "{what}: FV at the position's hard cap");
        assert_ne!(
            ev.fv,
            fv_of(
                b_repriced(live_fee.capped(cap), pos.q, l, p),
                ev.pprime_lo,
                r_due
            ),
            "{what}: not at the pool's last live fee"
        );
        assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
        // The old bytes back.
        env.svm = saved;
        env.svm.set_account(pm.pool.pool, original).unwrap();
        env.warp(1);
        crank(&mut env, &pm);
        assert_eq!(market(&env, &pm).no_route_since, 0, "{what}");
    }
}

// ---- G-D6: the Emergency flag -----------------------------------------------------------------

/// G-D6 on PumpSwap (T-S3-BROKEN; ARCH §5.3, V2-1, O15): the Emergency flag — the market's or
/// PumpSwap's venue bit — on an executable pool starts the S3 chain at the next observation and
/// nothing else (opens `RouteBroken`; the pool still observed). Flagged 300 s before the
/// deadline, the healthy position never settles before `max(deadline, since + s3_wait_s)` and
/// settles S3 at the first keeper call after it, with the expiry fee at both corners and FV at
/// the EMA point at the live fee (under the cap). Clearing the flag clears the chain.
#[test]
fn an_emergency_flag_settles_expired_positions_after_the_timer() {
    for venue_scope in [false, true] {
        let (mut env, pm, q, b, nonce, pos) = opened(PammCoin::Legacy261);
        jump_to_deadline(&mut env, &pm, &pos, -300);
        set_flag(&mut env, &pm, venue_scope, true);
        let mut b2 = env.borrower();
        assert_err(&env.open(&pm, &mut b2, q).0, E::RouteBroken);
        env.warp(1);
        crank(&mut env, &pm);
        let m = market(&env, &pm);
        let since = m.no_route_since;
        assert_eq!(
            since,
            env.now(),
            "venue scope {venue_scope}: the chain starts"
        );
        assert!(m.b_lo.get() > 0, "the pool is still observed");
        let kp = keeper(&mut env);
        let due = (since + i64::from(pos.s3_wait_s)).max(pos.deadline);
        let ev = loop {
            env.warp(30);
            crank(&mut env, &pm);
            let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
            if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
                break ev;
            }
            assert!(env.now() < due, "S3 is due at {due}");
        };
        assert!(env.now() >= due && env.now() < due + 30);
        assert_eq!(ev.condition, 3);
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.expiry_fee_bps, pos.expiry_fee_bps)
        );
        let m = market(&env, &pm);
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let fv = fv_of(
            b_repriced(pool_fees(&env, &pm.pool).capped(cap), pos.q, l, p),
            ev.pprime_lo,
            r_due,
        );
        assert_eq!(ev.fv, fv, "FV at the EMA point, the live fee");
        let take = pos.v - k_s(&pos);
        assert_eq!(ev.lender_amount, fv.min(take));
        assert_eq!(ev.borrower_amount, take - ev.lender_amount);
        set_flag(&mut env, &pm, venue_scope, false);
        env.warp(1);
        crank(&mut env, &pm);
        assert_eq!(market(&env, &pm).no_route_since, 0);
    }
}

/// G-D6, the liquidation side on PumpSwap (V2-1, I10): from the same state, along the same path
/// of real pAMM `buy`s by another trader (the price × 1.02 a second), with and without the
/// Emergency flag the keeper's `liquidate` refuses with the same codes in the same seconds and
/// executes in the same second with identical amounts; only the flagged run has an S3 chain.
#[test]
fn liquidations_are_identical_with_and_without_the_flag() {
    let (mut env, pm, _q, b, nonce, _) = opened(PammCoin::Legacy261);
    let kp = keeper(&mut env);
    let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
    let saved = env.svm.clone();
    let mut runs = Vec::new();
    for flag in [false, true] {
        env.svm = saved.clone();
        if flag {
            set_flag(&mut env, &pm, false, true);
        }
        let mut trace: Vec<Result<[u64; 10], Option<u32>>> = Vec::new();
        for _ in 0..150 {
            ok(u.push_price(&mut env, &pm.pool, 102, 100));
            env.warp(1);
            crank(&mut env, &pm);
            let res = liquidate(&mut env, &pm, &kp, &b.key(), nonce);
            let done = res.is_ok();
            trace.push(match &res {
                Ok(meta) => {
                    let e: Liquidate = events::one(meta);
                    Ok([
                        e.buyback_cost,
                        e.b_live,
                        e.b_ref,
                        e.max_in,
                        e.rent,
                        e.penalty,
                        e.penalty_keeper,
                        e.keeper_topup,
                        e.borrower_payout,
                        e.token_surplus,
                    ])
                }
                Err(_) => Err(custom_code(&res)),
            });
            if done {
                break;
            }
        }
        runs.push((trace, market(&env, &pm).no_route_since));
    }
    let (plain, flagged) = (&runs[0], &runs[1]);
    assert!(
        plain.0.last().is_some_and(Result::is_ok),
        "the path liquidates"
    );
    assert!(plain.0.len() > 1, "healthy first");
    assert_eq!(plain.0, flagged.0, "the same attempts, second by second");
    assert_eq!(plain.1, 0);
    assert!(flagged.1 > 0, "only the flagged run has an S3 chain");
}

/// O15's race on PumpSwap under the Emergency flag (ARCH §8.2 round 3): at the deadline of a
/// flagged, expired, healthy position whose chain has run `s3_wait_s`, the honest `expire()` and
/// a lender-keeper's `sol_settle(S3)` are both valid; whichever lands first closes the position
/// and the other finds none (`AccountNotInitialized`). The S3 take equals the expiry's buyback,
/// fee and rent within 0.1 %, and the borrower's difference is `K_s − K_e` within that margin.
#[test]
fn an_emergency_flag_s3_races_an_honest_expiry() {
    let (mut env, pm, _q, b, nonce, pos) = opened(PammCoin::Legacy261);
    jump_to_deadline(&mut env, &pm, &pos, -700);
    set_flag(&mut env, &pm, false, true);
    let kp = keeper(&mut env);
    for _ in 0..23 {
        env.warp(30);
        crank(&mut env, &pm);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        assert!(events::of::<SolSettle>(&meta).is_empty());
    }
    let now = env.now();
    env.warp(pos.deadline - 5 - now);
    crank_secs(&mut env, &pm, 5);
    assert_eq!(env.now(), pos.deadline);
    let honest = keeper(&mut env);
    let saved = env.svm.clone();

    let exp: Expire = events::one(&ok(expire(&mut env, &pm, &honest, &b.key(), nonce)));
    assert_eq!(
        custom_code(&settle(&mut env, &pm, &kp, &b.key(), nonce)),
        Some(ANCHOR_ACCOUNT_NOT_INITIALIZED),
        "the lender-keeper's S3 finds no position"
    );

    env.svm = saved;
    let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
    let ev = events::of::<SolSettle>(&meta).pop().expect("S3");
    assert_eq!(ev.condition, 3);
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.expiry_fee_bps, pos.expiry_fee_bps)
    );
    assert_eq!(
        custom_code(&expire(&mut env, &pm, &honest, &b.key(), nonce)),
        Some(ANCHOR_ACCOUNT_NOT_INITIALIZED),
        "the honest expiry finds no position"
    );
    let expiry_value = exp.buyback_cost + exp.expiry_fee + exp.rent;
    let diff = ev.lender_amount.abs_diff(expiry_value);
    assert!(
        diff * 1_000 <= expiry_value,
        "S3 take {} vs the expiry's B + fee + rent {expiry_value}",
        ev.lender_amount
    );
    let extra = i128::from(exp.borrower_payout) - i128::from(ev.borrower_amount);
    let keeper_gap = i128::from(ev.k_s) - i128::from(exp.keeper_reward);
    assert!(
        (extra - keeper_gap).unsigned_abs() <= u128::from(diff),
        "borrower: expiry {} vs S3 {} (K_s {} / K_e {})",
        exp.borrower_payout,
        ev.borrower_amount,
        ev.k_s,
        exp.keeper_reward
    );
}

// ---- G-H2: extras on the sell leg --------------------------------------------------------------

/// The Emergency multisig publishes `entries` as PumpSwap's sell-leg `current` set.
fn publish_sell_extras(env: &mut Env, entries: Vec<ExtraEntryArg>) {
    let ea = env.pause_authority.insecure_clone();
    let ix = set_venue_extras_ix(
        &ea.pubkey(),
        VenueKind::PumpSwap as u8,
        EXTRAS_IX_SELL as u8,
        entries,
    );
    ok(env.send(&[ix], &[&ea]));
}

/// A fixed read-only extras entry.
fn fixed(key: Pubkey) -> ExtraEntryArg {
    ExtraEntryArg {
        kind: shortwin::state::ENTRY_FIXED,
        writable: false,
        key,
        seeds: Vec::<SeedPartArg>::new(),
    }
}

/// The transaction of an open of `q` by `b` (nonce 0) with extras selection `set` and `extras`
/// after the venue accounts; the open is its last instruction.
fn open_with_extras_ixs(
    pm: &PammMarket,
    b: &Borrower,
    q: u64,
    set: u8,
    extras: &[AccountMeta],
) -> Vec<Instruction> {
    let sel = ExtrasSel { set, bumps: [0; 4] };
    let ix = open_short_ix(pm, &b.key(), 0, q, 0, u64::MAX, u64::MAX, sel, extras);
    shortwin_litesvm_tests::engine::with_cu_limit(pm, ix)
}

/// An open of `q` by `b` (nonce 0) with extras selection `set` and `extras` after the venue
/// accounts.
fn open_with_extras(
    env: &mut Env,
    pm: &PammMarket,
    b: &Borrower,
    q: u64,
    set: u8,
    extras: &[AccountMeta],
) -> TxResult {
    env.send(&open_with_extras_ixs(pm, b, q, set, extras), &[&b.kp])
}

/// The account list of the one PumpSwap CPI at height 2 (short.win's `sell` / `buy`) inside the
/// last instruction of `ixs`, a transaction the harness payer sent.
fn pamm_leg_cpi_accounts(
    env: &Env,
    ixs: &[Instruction],
    meta: &TransactionMetadata,
) -> Vec<Pubkey> {
    use shortwin::constants::PUMP_AMM_PROGRAM_ID;
    let keys = solana_message::Message::new(ixs, Some(&env.payer.pubkey())).account_keys;
    let calls: Vec<Vec<Pubkey>> = meta.inner_instructions[ixs.len() - 1]
        .iter()
        .filter(|i| {
            i.stack_height == 2
                && keys[usize::from(i.instruction.program_id_index)] == PUMP_AMM_PROGRAM_ID
        })
        .map(|i| {
            i.instruction
                .accounts
                .iter()
                .map(|a| keys[usize::from(*a)])
                .collect()
        })
        .collect();
    assert_eq!(calls.len(), 1, "one PumpSwap call at height 2");
    calls.into_iter().next().unwrap()
}

/// G-H2 on PumpSwap (A41, V2-6; C0-4 a): the Emergency publishes a harmless fixed extra on the
/// sell leg and an open selects `current`: short.win appends it to the `sell` CPI (read from the
/// transaction's inner instructions: the plain open's accounts plus the extra last), and pAMM
/// ignores the trailing account, so the proceeds equal those of the same open without extras
/// (from the same state). A second, hostile write
/// names the lending vault: the open selecting `current` is refused `DeniedAccount` (the open's
/// denylist), and the same open selecting `previous` — the harmless set — lands with the same
/// proceeds.
#[test]
fn extras_on_the_sell_leg_ride_along_and_a_hostile_set_is_bypassed() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let b = env.borrower();
    let extra = env.funded_keypair().pubkey();
    publish_sell_extras(&mut env, vec![fixed(extra)]);
    let seed = AccountMeta::new_readonly(pda::venue_extras_seed(VenueKind::PumpSwap as u8), false);
    let harmless = [seed.clone(), AccountMeta::new_readonly(extra, false)];
    let saved = env.svm.clone();
    let ixs = open_with_extras_ixs(&pm, &b, q, ExtrasSel::NONE.set, &[]);
    let meta = ok(env.send(&ixs, &[&b.kp]));
    let plain: Open = events::one(&meta);
    let plain_cpi = pamm_leg_cpi_accounts(&env, &ixs, &meta);
    env.svm = saved.clone();
    let ixs = open_with_extras_ixs(&pm, &b, q, EXTRAS_SET_CURRENT, &harmless);
    let meta = ok(env.send(&ixs, &[&b.kp]));
    let with: Open = events::one(&meta);
    // The extra reaches pAMM: the `sell` CPI is the plain open's account list plus the published
    // key at its end (forwarded, not dropped), and pAMM runs it.
    let with_cpi = pamm_leg_cpi_accounts(&env, &ixs, &meta);
    assert_eq!(
        with_cpi.split_last(),
        Some((&extra, plain_cpi.as_slice())),
        "sell carries the extra last"
    );
    assert_eq!(
        (with.s, with.n_open),
        (plain.s, plain.n_open),
        "proceeds unchanged"
    );
    env.svm = saved;
    publish_sell_extras(&mut env, vec![fixed(pm.keys.pool_vault)]);
    let hostile = [seed, AccountMeta::new_readonly(pm.keys.pool_vault, false)];
    assert_err(
        &open_with_extras(&mut env, &pm, &b, q, EXTRAS_SET_CURRENT, &hostile),
        E::DeniedAccount,
    );
    let prev: Open = events::one(&ok(open_with_extras(
        &mut env,
        &pm,
        &b,
        q,
        EXTRAS_SET_PREVIOUS,
        &harmless,
    )));
    assert_eq!(prev.s, plain.s, "previous: the harmless set");
}

/// M10 batch 3 on PumpSwap (ARCH §2.4 rule 4; `engine::pool_and_borrower_denied_extras`): the
/// canonical pool and its base / quote vaults — and, on the keeper exits, the position's borrower —
/// each published as a fixed extra, are `DeniedAccount` on `open_short`, `close_short`,
/// `liquidate` and `expire`, the position unchanged; the same set shape with a fresh key rides
/// along (pAMM accepts trailing accounts, R-VEN Update H5): the open, the close and the expiry
/// land, the healthy liquidation reaches `NotLiquidatable`.
#[test]
fn the_pool_and_the_borrower_are_denied_extras_on_pumpswap() {
    shortwin_litesvm_tests::engine::pool_and_borrower_denied_extras(
        || setup(PammCoin::Legacy261),
        "PumpSwap",
    );
}

/// M10 batch 3 (the security review) on PumpSwap (legacy261): the canonical pool's base and
/// quote vaults are ATA-program PDAs of the `pool` placeholder, so an Emergency template derives
/// them; the denylist runs on the derived key, so each is `DeniedAccount` on `open_short` through
/// `current` and through `previous` (`engine::template_aliases_denied`).
#[test]
fn template_aliases_of_the_pool_vaults_are_denied_extras_on_pumpswap() {
    use shortwin::constants::{ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT};
    use shortwin::state::{PLACEHOLDER_MINT, PLACEHOLDER_POOL};
    use shortwin_litesvm_tests::engine::{PoolAlias, VenueMarket, seed_const, seed_placeholder};
    shortwin_litesvm_tests::engine::template_aliases_denied(
        || setup(PammCoin::Legacy261),
        "PumpSwap",
        |env, pm| {
            let pool = pm.read_set(false)[shortwin::venues::pump_amm::P_POOL].pubkey;
            let mint = pm.mint();
            let tp = env.svm.get_account(&mint).unwrap().owner;
            let ata = |what: &str, prog: Pubkey, m: SeedPartArg, mk: Pubkey| PoolAlias {
                what: what.into(),
                program: ASSOCIATED_TOKEN_PROGRAM_ID,
                seeds: vec![
                    seed_placeholder(PLACEHOLDER_POOL),
                    seed_const(prog.as_ref()),
                    m,
                ],
                resolved: vec![
                    pool.to_bytes().to_vec(),
                    prog.to_bytes().to_vec(),
                    mk.to_bytes().to_vec(),
                ],
            };
            vec![
                ata(
                    "the base vault",
                    tp,
                    seed_placeholder(PLACEHOLDER_MINT),
                    mint,
                ),
                ata(
                    "the quote vault",
                    TOKEN_PROGRAM_ID,
                    seed_const(WSOL_MINT.as_ref()),
                    WSOL_MINT,
                ),
            ]
        },
    );
}
