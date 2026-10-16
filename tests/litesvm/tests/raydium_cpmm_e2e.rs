//! Fork E2E on Raydium **CPMM** (M3 exit criterion "fork E2E on … USELESS (CPMM) through open →
//! liquidation → S2/S3"; ARCH §2.4.1, §2.6, §3.2–§3.8, §4.5, §4.7, §5.3, §5.5, §8.2, §9 M3;
//! SPEC §2–§5; O13, D3, O15): the release short.win against the **dumped** CPMM program
//! (`CPMMoo8…`, its 2026-10 redeploy at slot 452,376,460) and three dumped mainnet pools —
//! USELESS `Q2sPHPdU…` (SPL, config 0), the Token-2022 pool `7ZRtFcgf…` and the creator-fee pool
//! `FQd8z3…` — driven only through real instructions: `create_market`, lender `deposit` /
//! `withdraw`, `open_short`, `close_short`, `crank_price`, `liquidate`, `expire`, `sol_settle`,
//! real CPMM swaps by other market participants (the price moves), and the pool admin's real
//! `update_pool_status` / `update_amm_config` (signature forged, `fork::raydium_admin_send`).
//!
//! | Test | Journey |
//! |---|---|
//! | `useless_journey` | list (O13 age = `open_time`), two lenders, the listing window, Tier A; a dump and a profitable close; a whale pump and a keeper liquidation; an admin halt of a liquidatable position → S3 after the continuous chain (FV at the EMA point); re-enabled, the price back, an expiry at the deadline; an out-of-model drain + pump → `liquidate` refused (`UseSolSettle`) → S2 capped at FV; both lenders exit with tokens and SOL |
//! | `token_2022_journey` | the same engine on a Token-2022 base: pump → liquidation delivered into the Token-2022 lending vault; a config fee hike far above the hard cap → normal buyback refused, S1 not reachable at the capped risk fee → S3 at FV priced at the locked cap; a status-bit halt freezes a healthy position (no close, no settlement) until re-enabled |
//! | `creator_fee_journey` | Tier B pool with the creator fee on WSOL: every leg's `SwapEvent` and fee counters against R-RAY §3.3 recomputed here; the creator fee moved to the token side with positions open (U11) → liquidation and expiry buy exactly `q` with `q' = ⌈q·10⁶/(10⁶ − c)⌉` taken from the pool |
//! | `u11_exact_out_creator_fee_on_either_side` | T-RAY-MATH's U11 through short.win's own buy legs on the ELF: creator fee on the SOL input, on the token output, on both, on SPL and Token-2022 bases; the venue's charge, `SwapEvent`, counters and vault deltas equal R-RAY §3.3 recomputed independently |
//! | `settlements_on_every_fixture` | S3 on an admin halt, S3 on a fee hike above the hard cap and S2 on a drained pool, each on USELESS, the Token-2022 pool, the creator-fee pool and that pool with its creator fee on the token side: `FV(P_ema, L_ema)` at `min(φ_live, cap)` with the creator fee on the side the pool charges it, the take, the borrower's excess and every lamport |
//! | `o13_age_of_a_cpmm_pool` | `graduated_at = min(now, open_time)` from `T_CLAMP` on, the listing time before it (SPEC §4 O13): a pool opened 2 h before listing is Tier C until exactly `open_time + 24 h`, B until exactly `open_time + 7 d`, then A, positions keep the terms of their tier; a pre-clamp pool ages from its listing (Tier C for 24 h, B until day 7); `open_time == now` lists with age 0; one second later is not executable |
//!
//! Every instruction goes through `Env::send` (or `fork::raydium_admin_send`, which runs the
//! same hooks): the invariant hooks of ARCH §8.3 run after every instruction, on Raydium
//! markets with the harness's own venue view (I13 re-quotes, I16 route rules). Every executed
//! keeper exit is also held to ARCH §6.2's 9 trace entries and §6.1's 200 k CU, every executed
//! settlement to 2 entries (+3 with a share-epoch record) and 150 k CU (T-RAY-TRACE / T-RAY-CU;
//! `-- --nocapture` prints the figures).
//!
//! **S2 on CPMM needs an out-of-model drain.** Opens are sized on the locked depth over `λ_min`
//! (ARCH §4.6: `X₀ ≤ D/(λ_min·√P) = 2·X_locked` at `λ_min` = 50 %, so `q ≤ 2.47 %·X₀ ≤
//! 4.94 %` of the locked token reserve), and burned CPMM LP can never be redeemed (R-RAY §2.4,
//! §6.3), so real LP withdrawals leave `σ = q/(X − q) ≤ ≈ 5.2 %`; `σ > σ_max = 20 %` then
//! needs the token reserve below `6·q`, a price rise of ≈ 11×, far into S1 territory. The S2
//! phases therefore drain both reserves byte-level at an unchanged price (what a venue exploit
//! or a malicious upgrade could do) and let a real swap pump the thin pool.

// `TxResult` is LiteSVM's own result type (see the harness's lib.rs).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use litesvm::types::TransactionMetadata;
use shortwin::constants::T_CLAMP;
use shortwin::errors::ShortwinError as E;
use shortwin::events::{Close, Deposit, Expire, Liquidate, Open, SettleArmed, SolSettle, Withdraw};
use shortwin::state::{LenderPosition, Market, ShortPosition, VenueKind};
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, expire_ix, liquidate_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
use shortwin_litesvm_tests::fork::{
    self, AnyPool, CpmmFixture, CpmmPool, CpmmSwapEvent, Swap, Trader,
};
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::raydium::{self, RayFixture, RayMarket, RaySpec};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{Env, events, pda, read};
use shortwin_math::fees::Direction;
use shortwin_math::num::WAD;
use shortwin_math::quote::Pricing;
use shortwin_math::rent;
use shortwin_math::tier::Tier;
use shortwin_venue_state::raydium_cpmm;
use solana_keypair::Keypair;
use solana_signer::Signer;

const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);

/// `D` of R-RAY §3.3 (rates in ppm).
const PPM: u128 = 1_000_000;
/// `pool_list_window_s` (ECON §15 default).
const LIST_WINDOW_S: i64 = 3_600;
/// ARCH §4.7's `AGE_C_S` (Tier C below it)…
const DAY: i64 = 86_400;
/// … and `AGE_A_S` (Tier A from it).
const WEEK: i64 = 604_800;

fn code(e: E) -> u32 {
    u32::from(e)
}

// ---- markets, lenders, borrowers ----------------------------------------------------------------

/// A CPMM market on the fork: the market, its first lender's deposit (`lend`), no warm-up yet
/// (`now` is the listing second).
fn list(env: &mut Env, fixture: RayFixture, pool: AnyPool, lend: u64) -> RayMarket {
    let res = env.list_ray(&pool, fixture.hint());
    ok(res);
    env.ray_market_listed(
        RaySpec {
            fixture,
            lend,
            warmup: 0,
        },
        pool,
    )
}

/// The CPMM pool of `rm`.
fn cpmm(rm: &RayMarket) -> CpmmPool {
    *rm.cpmm()
}

/// A second lender of `rm`: funded with `amount` tokens and deposited.
fn add_lender(env: &mut Env, rm: &RayMarket, amount: u64) -> (Keypair, LenderKeys) {
    let owner = env.funded_keypair();
    let (mint, program) = rm.pool.token();
    let acc = fork::fund_token(env, &owner.pubkey(), &mint, program, amount);
    let keys = LenderKeys {
        owner: owner.pubkey(),
        mint,
        token_program: program.id(),
        owner_token_account: acc,
    };
    let meta = ok(env.send(&[ix::deposit(&keys, amount, 0)], &[&owner]));
    let d: Deposit = events::one(&meta);
    assert_eq!(d.amount, amount);
    (owner, keys)
}

/// `withdraw` of every share `owner` holds (with the epoch records it is behind on): the event.
fn withdraw_all(env: &mut Env, rm: &RayMarket, owner: &Keypair, keys: &LenderKeys) -> Withdraw {
    let market = rm.market();
    let lp: LenderPosition = env.lender(&market, &owner.pubkey()).expect("lender");
    let epoch = env.market(&market).share_epoch;
    let ix = ix::with_remaining(
        ix::withdraw(keys, lp.shares, 0),
        &ix::epoch_records(&market, lp.epoch, epoch),
    );
    events::one(&ok(env.send(&[ix], &[owner])))
}

/// An open that must succeed: the event and the nonce.
fn open_ok(
    env: &mut Env,
    rm: &RayMarket,
    b: &mut Borrower,
    q: u64,
) -> (Open, u64, TransactionMetadata) {
    let (res, nonce) = env.open(rm, b, q);
    let meta = ok(res);
    (events::one(&meta), nonce, meta)
}

fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}

fn tokens(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or(0)
}

fn market(env: &Env, rm: &RayMarket) -> Market {
    env.market(&rm.market())
}

/// `K_s` of a position (its locked settlement-keeper terms, ARCH §4.5).
fn k_s(p: &ShortPosition) -> u64 {
    let pct = u128::from(p.v) * u128::from(p.settle_keeper_bps) / 10_000;
    (u128::from(p.settle_keeper_fixed) + pct)
        .min(u128::from(p.settle_keeper_cap))
        .min(u128::from(p.v)) as u64
}

/// The pool's pricing spot `P = ⌊Y·WAD/X⌋` (SOL per raw token).
fn spot(env: &Env, rm: &RayMarket) -> u128 {
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    u128::from(y) * WAD / u128::from(x)
}

/// The live fees of `rm`'s pool as short.win resolves them (execution quotes).
fn live_pricing(env: &Env, rm: &RayMarket) -> Pricing {
    let p = cpmm(rm);
    Pricing::Cpmm {
        buy: p.fees(&env.svm, Direction::Buy).expect("buy fees"),
        sell: p.fees(&env.svm, Direction::Sell).expect("sell fees"),
    }
}

/// `FV(P, L) = B̂(Q; reprice(L, P); φ_FV) + ⌈p'·B̂/10⁴⌉ + R_due` (ARCH §4.5).
fn fair_value(pricing: Pricing, q: u64, p: u128, l: u128, pprime_bps: u16, r_due: u64) -> u64 {
    let b = pricing.buy_repriced(q, l, p).expect("FV quote").cost;
    b + (u128::from(b) * u128::from(pprime_bps)).div_ceil(10_000) as u64 + r_due
}

// ---- other market participants (real CPMM swaps) ---------------------------------------------

/// A whale holding nothing yet (topped up per swap).
fn whale(env: &mut Env, rm: &RayMarket) -> Trader {
    let (mint, program) = rm.pool.token();
    fork::trader(env, &mint, program, 0, 0)
}

/// A real swap by `w` that moves the pool's spot by about `mult` (constant product): a buy of
/// `Y·(√m − 1)` lamports raises it, a sale of `X·(1/√m − 1)` tokens lowers it (fees make the
/// move slightly smaller). The whale's source is topped up first.
fn move_price(env: &mut Env, rm: &RayMarket, w: &Trader, mult: f64) -> TransactionMetadata {
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    let r = mult.sqrt();
    if mult > 1.0 {
        let amount = (y as f64 * (r - 1.0)) as u64;
        let have = tokens(env, &w.wsol);
        if have < amount {
            fork::wrap(env, &w.wsol, amount - have);
        }
        ok(fork::swap(
            env,
            &rm.pool,
            w,
            Direction::Buy,
            Swap::ExactIn(amount, 0),
        ))
    } else {
        let amount = (x as f64 * (1.0 / r - 1.0)) as u64;
        let have = tokens(env, &w.token);
        fork::set_token_amount(&mut env.svm, &w.token, have + amount);
        ok(fork::swap(
            env,
            &rm.pool,
            w,
            Direction::Sell,
            Swap::ExactIn(amount, 0),
        ))
    }
}

/// Out-of-model drain (module docs): both pricing reserves scaled so the token reserve is
/// `x_target`, at an unchanged price; the fee counters stay excluded as before.
fn drain_to(env: &mut Env, rm: &RayMarket, x_target: u64) {
    let p = cpmm(rm);
    let st = p.state(&env.svm);
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    let y_target = (u128::from(y) * u128::from(x_target) / u128::from(x)) as u64;
    let (tok_vault, sol_vault) = rm.vaults();
    let (c_sol, c_tok) = if p.sol_is_token0 {
        (
            st.protocol_fees_token_0 + st.fund_fees_token_0 + st.creator_fees_token_0,
            st.protocol_fees_token_1 + st.fund_fees_token_1 + st.creator_fees_token_1,
        )
    } else {
        (
            st.protocol_fees_token_1 + st.fund_fees_token_1 + st.creator_fees_token_1,
            st.protocol_fees_token_0 + st.fund_fees_token_0 + st.creator_fees_token_0,
        )
    };
    fork::set_token_amount(&mut env.svm, &tok_vault, x_target + c_tok);
    fork::set_token_amount(&mut env.svm, &sol_vault, y_target + c_sol);
    assert_eq!(rm.reserves(&env.svm), Some((x_target, y_target)));
}

// ---- the keeper -----------------------------------------------------------------------------

/// One keeper second: the clock advances one second and the keeper cranks the market.
fn second(env: &mut Env, rm: &RayMarket) {
    env.warp(1);
    env.crank(rm);
}

/// `secs` of the listing window, the last `PRICE_WARMUP_CRANKS` of them observed every second
/// (the keeper's listing-window observations: the price warm-up of M4V-5 before the first open).
fn warm_window(env: &mut Env, rm: &RayMarket, secs: i64) {
    env.warp(secs - raydium::PRICE_WARMUP_CRANKS);
    env.warm_up_price(rm);
}

/// Keeper seconds until the spot and the EMA agree within `bps` (at most `max_s`).
fn ema_converges(env: &mut Env, rm: &RayMarket, bps: u128, max_s: u32) {
    for _ in 0..max_s {
        second(env, rm);
        let (s, e) = (spot(env, rm), market(env, rm).ema_price.get());
        if s.abs_diff(e) * 10_000 <= e * bps {
            return;
        }
    }
    panic!("the EMA did not converge within {max_s} s");
}

/// An executed keeper exit: its event, its metadata, the balances and the pool just before it,
/// and the quote `B̂_live(q)` on that pre-trade state.
struct Exit<T> {
    ev: T,
    meta: TransactionMetadata,
    before: Balances,
    pool: PoolSnap,
    b_exp: u64,
}

/// Keeper seconds until `liquidate` of `(b, nonce)` executes (a waiting position answers
/// `NotLiquidatable`, `ExitBoundNotMet` or `ReferenceStale`).
fn liquidate_when_due(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
    max_s: u32,
) -> Exit<Liquidate> {
    let q = env.position(rm, &b.key(), nonce).q;
    for _ in 0..max_s {
        second(env, rm);
        let before = balances(env, rm, &keeper.pubkey(), b, nonce);
        let pool = pool_snap(env, &cpmm(rm));
        let b_exp = rm.quote_buy(&env.svm, q);
        let ix = liquidate_ix(rm, &keeper.pubkey(), &b.key(), nonce);
        let res = env.send(&[ix], &[keeper]);
        match custom_code(&res) {
            None => {
                let meta = ok(res);
                assert_exit_budget("liquidate", &meta);
                return Exit {
                    ev: events::one(&meta),
                    meta,
                    before,
                    pool,
                    b_exp: b_exp.expect("a liquidation has a quote").cost,
                };
            }
            Some(c)
                if c == code(E::NotLiquidatable)
                    || c == code(E::ExitBoundNotMet)
                    || c == code(E::ReferenceStale) => {}
            Some(c) => panic!("liquidate failed with {c}: {res:?}"),
        }
    }
    panic!("never liquidated");
}

/// `expire` of `(b, nonce)` by `keeper` now (must succeed).
fn expire_now(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
) -> Exit<Expire> {
    let q = env.position(rm, &b.key(), nonce).q;
    let before = balances(env, rm, &keeper.pubkey(), b, nonce);
    let pool = pool_snap(env, &cpmm(rm));
    let b_exp = rm
        .quote_buy(&env.svm, q)
        .expect("an expiry has a quote")
        .cost;
    let meta = ok(env.send(
        &[expire_ix(rm, &keeper.pubkey(), &b.key(), nonce)],
        &[keeper],
    ));
    assert_exit_budget("expire", &meta);
    Exit {
        ev: events::one(&meta),
        meta,
        before,
        pool,
        b_exp,
    }
}

/// Keeper seconds until `liquidate` of `(b, nonce)` answers anything but `NotLiquidatable`
/// (at most `max_s`; it must not execute): that answer.
fn keeper_refusal(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
    max_s: u32,
) -> u32 {
    for _ in 0..max_s {
        second(env, rm);
        let ix = liquidate_ix(rm, &keeper.pubkey(), &b.key(), nonce);
        let res = env.send(&[ix], &[keeper]);
        match custom_code(&res) {
            Some(c) if c == code(E::NotLiquidatable) => {}
            Some(c) => return c,
            None => panic!("the liquidation executed: {res:?}"),
        }
    }
    panic!("never liquidatable within {max_s} s");
}

/// One `sol_settle` by `keeper` (must succeed): `(settlement, armed)` events and the metadata.
fn settle(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
) -> (Option<SolSettle>, Option<SettleArmed>, TransactionMetadata) {
    let ix = sol_settle_ix(env, rm, &keeper.pubkey(), &b.key(), nonce);
    let meta = ok(env.send(&[ix], &[keeper]));
    (
        events::of::<SolSettle>(&meta).into_iter().next(),
        events::of::<SettleArmed>(&meta).into_iter().next(),
        meta,
    )
}

/// Instruction-trace entries of a one-instruction transaction (ARCH §6.2): the instruction and
/// its inner instructions.
fn trace_entries(meta: &TransactionMetadata) -> usize {
    1 + meta.inner_instructions.first().map_or(0, Vec::len)
}

/// T-RAY-TRACE / T-RAY-CU for an executed Raydium keeper exit (`liquidate`, `expire`): ARCH
/// §6.2's 9 entries (the buy leg 7, the instruction and its event) and §6.1's 200 k limit.
fn assert_exit_budget(what: &str, meta: &TransactionMetadata) {
    let (t, cu) = (trace_entries(meta), meta.compute_units_consumed);
    eprintln!("{what}: {t} trace entries, {cu} CU");
    assert_eq!(t, 9, "{what}");
    assert!(cu <= 200_000, "{what}: {cu} CU");
}

/// T-TRACE / T-CU for an executed `sol_settle` (no venue CPI): ARCH §6.2's 2 entries (+3 when
/// the settlement rolls a share epoch and creates its record) and §6.1's 150 k limit.
fn assert_settle_budget(what: &str, meta: &TransactionMetadata) {
    let (t, cu) = (trace_entries(meta), meta.compute_units_consumed);
    eprintln!("{what}: {t} trace entries, {cu} CU");
    assert!(t == 2 || t == 5, "{what}: {t} trace entries");
    assert!(cu <= 150_000, "{what}: {cu} CU");
}

/// Keeper steps of `step` seconds until the settlement executes (at most `n` steps): a crank,
/// then `sol_settle` one second later — so the robust spot reads a completed bucket at most
/// `ref_max_age_s` old (ARCH §4.5; ECON §5.4: crank one second, act the next) and the chain's
/// observations stay within `S3_OBS_GAP_S`. Returns the event, the balances just before it,
/// and the step it armed at (if it armed).
fn settle_when_due(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
    step: i64,
    n: usize,
) -> (SolSettle, Balances, Option<usize>) {
    let mut armed = None;
    assert!(step >= 1);
    for i in 0..n {
        env.warp(step - 1);
        env.crank(rm);
        env.warp(1);
        let before = balances(env, rm, &keeper.pubkey(), b, nonce);
        let (s, a, meta) = settle(env, rm, keeper, b, nonce);
        if a.is_some() {
            armed = Some(i);
        }
        if let Some(s) = s {
            assert_settle_budget(&format!("sol_settle S{}", s.condition), &meta);
            return (s, before, armed);
        }
    }
    panic!("no settlement within {n} steps");
}

/// The balances a settlement or an exit moves.
#[derive(Clone, Copy, Debug)]
struct Balances {
    keeper: u64,
    borrower: u64,
    market_vault: u64,
    position: u64,
    exposure: u64,
    pool_vault: u64,
}

fn balances(env: &Env, rm: &RayMarket, keeper: &Pubkey, b: &Borrower, nonce: u64) -> Balances {
    let market = rm.market();
    Balances {
        keeper: lamports(env, keeper),
        borrower: lamports(env, &b.key()),
        market_vault: lamports(env, &rm.keys.market_vault),
        position: lamports(env, &pda::position(&market, &b.key(), nonce).0),
        exposure: lamports(env, &pda::exposure(&market, &b.key()).0),
        pool_vault: tokens(env, &rm.keys.pool_vault),
    }
}

/// The lamports a settlement moves (ARCH §5.2 "settle"): `K_s` to the keeper, the lenders' `x`
/// to `market_vault`, the rest of the position (and the exposure record once empty) to the
/// borrower; the token debt is extinguished without any token movement.
fn assert_settlement_moves(
    before: &Balances,
    after: &Balances,
    ev: &SolSettle,
    exposure_closed: bool,
) {
    assert_eq!(after.keeper - before.keeper, ev.k_s, "K_s to the keeper");
    assert_eq!(
        after.market_vault - before.market_vault,
        ev.lender_amount,
        "the lenders' take to market_vault"
    );
    assert_eq!(after.position, 0, "the position is closed");
    let exposure = if exposure_closed { before.exposure } else { 0 };
    assert_eq!(
        after.borrower - before.borrower,
        before.position - ev.k_s - ev.lender_amount + exposure,
        "the borrower gets V − K_s − x and the position's rent (A18)"
    );
    assert_eq!(after.pool_vault, before.pool_vault, "no token moves");
}

/// `σ(q)` on the live state (ARCH §4.5; `risk::sigma`): the fee-free input `n = ⌈Y·q/(X − q)⌉`
/// over `q·P`, in bps above par; `u64::MAX` when `q ≥ X`.
fn sigma_live(env: &Env, rm: &RayMarket, q: u64) -> u64 {
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    if q >= x {
        return u64::MAX;
    }
    let n = (u128::from(y) * u128::from(q)).div_ceil(u128::from(x - q));
    let p = u128::from(y) * WAD / u128::from(x);
    ((n * WAD * 10_000 / (u128::from(q) * p)) as u64).saturating_sub(10_000)
}

// ---- USELESS -----------------------------------------------------------------------------------

/// The M3 exit criterion's CPMM journey on USELESS (module docs), one market from listing to
/// the lenders' exit, every instruction checked by the hooks.
#[test]
fn useless_journey() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(USELESS);
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    let open_time = i64::try_from(p.state(&env.svm).open_time).unwrap();
    let listed_at = env.now();
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    // 2 SOL of notional per position.
    let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;

    // ---- listing: O13 age from the pool's own open_time, lenders, the listing window -------
    let rm = list(&mut env, USELESS, pool, q * 40);
    let m = market(&env, &rm);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert!(open_time > T_CLAMP && open_time < listed_at);
    assert_eq!(
        m.graduated_at, open_time,
        "O13: min(now, open_time) for an open_time from T_CLAMP on"
    );
    assert_eq!(m.pool_seen_at, listed_at);
    let (lender_a, keys_a) = (rm.lender.insecure_clone(), rm.lender_keys());
    let (lender_b, keys_b) = add_lender(&mut env, &rm, q * 20);
    let deposits = q * 60;

    let mut b1 = env.borrower();
    warm_window(&mut env, &rm, LIST_WINDOW_S - 1);
    env.fresh_oracle();
    env.crank(&rm);
    assert_err(&env.open(&rm, &mut b1, q).0, E::PoolListWindow);
    second(&mut env, &rm);
    assert_eq!(env.now(), listed_at + LIST_WINDOW_S);
    assert_eq!(
        market(&env, &rm).tier,
        Tier::A as u8,
        "Λ ≈ 2·23,500 SOL ≫ θ_A and age ≫ 7 d: Tier A at once"
    );
    let (o1, n1, _) = open_ok(&mut env, &rm, &mut b1, q);
    let pos1 = env.position(&rm, &b1.key(), n1);
    assert_eq!((o1.tier, pos1.cr_liq_bps), (Tier::A as u8, 12_500));
    assert_eq!(pos1.deadline, pos1.t_open + 2_592_000, "Tier A: 30 days");
    let p0 = spot(&env, &rm);
    let (mut b2, mut b3) = (env.borrower(), env.borrower());
    let (o2, n2, _) = open_ok(&mut env, &rm, &mut b2, q);
    let (o3, n3, _) = open_ok(&mut env, &rm, &mut b3, q);
    assert_eq!(market(&env, &rm).open_positions, 3);

    // ---- a dump by another trader; b1 closes at a profit --------------------------------------
    let w = whale(&mut env, &rm);
    move_price(&mut env, &rm, &w, 0.85);
    ema_converges(&mut env, &rm, 10, 300);
    env.warp(1);
    let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
    let c: Close = events::one(&ok(env.close(&rm, &b1, n1)));
    assert_eq!(c.buyback_cost, b_exp, "B = B̂_live(q)");
    assert!(c.buyback_cost < o1.s, "bought back below the sale");
    assert!(
        c.borrower_payout > o1.m + o1.fee_open,
        "a profitable short: {} back for {} paid in",
        c.borrower_payout,
        o1.m + o1.fee_open
    );

    // ---- a whale pump to ≈ 1.8× the entry: the keeper liquidates b2 ---------------------------
    // Tier A: liquidatable above ≈ 1.6× (V ≈ 2·B̂ at open, CR_liq 125 %), a normal buyback below
    // ≈ 1.97× (B̂ + ert ≤ V − K_s).
    let mult = 1.8 * p0 as f64 / spot(&env, &rm) as f64;
    move_price(&mut env, &rm, &w, mult);
    let keeper = env.funded_keypair();
    let pos2 = env.position(&rm, &b2.key(), n2);
    let Exit {
        ev: l2,
        before,
        b_exp,
        ..
    } = liquidate_when_due(&mut env, &rm, &keeper, &b2, n2, 120);
    let after = balances(&env, &rm, &keeper.pubkey(), &b2, n2);
    assert_eq!(l2.buyback_cost, b_exp, "B = B̂_live at the live fee");
    assert!(!l2.expired);
    assert!(
        l2.cr_spot_bps < u64::from(pos2.cr_liq_bps) && l2.cr_ema_bps < u64::from(pos2.cr_liq_bps)
    );
    assert_eq!(
        after.pool_vault - before.pool_vault,
        q,
        "q straight into the lending vault"
    );
    assert_eq!(
        l2.buyback_cost + l2.rent + l2.penalty + l2.keeper_topup + l2.borrower_payout,
        pos2.v,
        "the liquidation waterfall conserves V"
    );
    assert_eq!(
        l2.penalty_lender + l2.penalty_protocol + l2.penalty_keeper,
        l2.penalty
    );
    assert_eq!(
        after.keeper - before.keeper,
        l2.penalty_keeper + l2.keeper_topup
    );
    assert_eq!(
        after.market_vault - before.market_vault,
        l2.rent + l2.penalty_lender + l2.penalty_protocol,
        "rent and the lender and protocol penalty shares to market_vault"
    );
    if l2.borrower_payout > 0 {
        assert!(
            l2.penalty_keeper + l2.keeper_topup >= k_s(&pos2),
            "N22 keeper floor"
        );
    }
    assert_eq!(read::token_delegate(&env.svm, &rm.keys.pool_vault), None);

    // ---- the pool admin halts the pool before the keeper reaches b3 (liquidatable): S3 --------
    let pos3 = env.position(&rm, &b3.key(), n3);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_pool_status_ix(&p, raydium_cpmm::STATUS_BIT_SWAP),
    ));
    env.warp(1);
    assert_err(&env.close(&rm, &b3, n3), E::NoRoute);
    let ix = liquidate_ix(&rm, &keeper.pubkey(), &b3.key(), n3);
    assert_err(&env.send(&[ix], &[&keeper]), E::NoRoute);
    let (none, _, _) = settle(&mut env, &rm, &keeper, &b3, n3);
    assert!(none.is_none(), "the S3 chain has only started");
    let since = market(&env, &rm).no_route_since;
    assert_eq!(since, env.now());
    let m_halt = market(&env, &rm);
    // The keeper observes every 30 s (the chain stays continuous, S3_OBS_GAP_S = 60).
    let (s3, before, armed) = settle_when_due(&mut env, &rm, &keeper, &b3, n3, 30, 40);
    let after = balances(&env, &rm, &keeper.pubkey(), &b3, n3);
    assert_eq!(s3.condition, 3, "S3");
    assert_eq!(armed, None, "S3 needs no arming clock");
    assert!(env.now() >= since + i64::from(pos3.s3_wait_s));
    assert!(env.now() < since + i64::from(pos3.s3_wait_s) + 30);
    let m = market(&env, &rm);
    let (pe, le) = (m.ema_price.get(), m.ema_liq.get());
    assert_eq!(
        (pe, le),
        (m_halt.ema_price.get(), m_halt.ema_liq.get()),
        "the EMA is frozen while the pool cannot execute"
    );
    assert_eq!(
        (s3.p_lo, s3.p_hi, s3.l_lo, s3.l_hi),
        (pe, pe, le, le),
        "the box collapses to the EMA point (ARCH §4.5, M2-SEC-1)"
    );
    assert_eq!(
        (s3.pprime_lo, s3.pprime_hi),
        (pos3.penalty_bps, pos3.penalty_bps),
        "CR-liquidatable, not expired: the penalty"
    );
    let r_due = rent::due(&pos3.pos_rent(), &m.basis()).unwrap();
    let pricing = live_pricing(&env, &rm).capped(u64::from(pos3.venue_fee_hard_cap_bps));
    let fv = fair_value(pricing, q, pe, le, s3.pprime_lo, r_due);
    assert_eq!(
        s3.fv, fv,
        "FV on the frozen EMA point at the halted pool's own fee"
    );
    assert_eq!(s3.lender_amount, fv.min(pos3.v - k_s(&pos3)));
    assert_eq!(s3.borrower_amount, pos3.v - k_s(&pos3) - s3.lender_amount);
    assert_settlement_moves(&before, &after, &s3, true);
    assert_eq!(market(&env, &rm).open_positions, 0);

    // ---- re-enabled: the chains clear, an arbitrageur brings the price back -------------------
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_pool_status_ix(&p, 0),
    ));
    second(&mut env, &rm);
    let m = market(&env, &rm);
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    let back = p0 as f64 / spot(&env, &rm) as f64;
    move_price(&mut env, &rm, &w, back);
    ema_converges(&mut env, &rm, 10, 300);

    // ---- an expiry at the deadline: a healthy position bought back by the keeper ------------
    env.fresh_oracle();
    second(&mut env, &rm);
    let mut b4 = env.borrower();
    let (o4, n4, _) = open_ok(&mut env, &rm, &mut b4, q);
    let pos4 = env.position(&rm, &b4.key(), n4);
    assert_eq!(pos4.tier, Tier::A as u8);
    let slot = env.clock().slot;
    env.set_clock(pos4.deadline + 1, slot + 6_480_000);
    env.tick(&rm, 3);
    let Exit {
        ev: e,
        before,
        b_exp,
        ..
    } = expire_now(&mut env, &rm, &keeper, &b4, n4);
    let after = balances(&env, &rm, &keeper.pubkey(), &b4, n4);
    assert_eq!(e.buyback_cost, b_exp);
    assert!(e.expiry_fee > 0 && e.keeper_reward > 0);
    assert_eq!(
        e.buyback_cost + e.rent + e.expiry_fee + e.keeper_reward + e.borrower_payout,
        pos4.v
    );
    assert_eq!(after.pool_vault - before.pool_vault, q);
    assert_eq!(after.keeper - before.keeper, e.keeper_reward);
    assert_eq!(
        after.market_vault - before.market_vault,
        e.rent + e.expiry_fee,
        "rent and the expiry fee (100 % lenders)"
    );

    // ---- S2: an out-of-model drain, then a real pump of the thin pool --------------------------
    env.fresh_oracle();
    second(&mut env, &rm);
    let mut b5 = env.borrower();
    let (o5, n5, _) = open_ok(&mut env, &rm, &mut b5, q);
    let pos5 = env.position(&rm, &b5.key(), n5);
    assert_eq!(pos5.max_liq_slip_bps, 2_000, "σ_max 20 %");
    env.tick(&rm, 5);
    drain_to(&mut env, &rm, q * 5);
    env.tick(&rm, 120);
    // ≈ 10 SOL of real reserve left: new opens are refused (ARCH §2.6 rule 5).
    let mut b6 = env.borrower();
    assert_err(&env.open(&rm, &mut b6, q).0, E::PoolTooThin);
    move_price(&mut env, &rm, &w, 1.35);
    let sigma = sigma_live(&env, &rm, q);
    assert!(
        sigma > u64::from(pos5.max_liq_slip_bps),
        "σ_live {sigma} bps"
    );
    // Once the EMA leg holds too, the keeper's normal buyback is refused: σ_live > σ_max.
    assert_eq!(
        keeper_refusal(&mut env, &rm, &keeper, &b5, n5, 60),
        code(E::UseSolSettle)
    );
    let (s2, before, armed) = settle_when_due(&mut env, &rm, &keeper, &b5, n5, 1, 300);
    let after = balances(&env, &rm, &keeper.pubkey(), &b5, n5);
    assert_eq!(s2.condition, 2, "S2");
    let armed = armed.expect("S2 runs on the arming clock");
    eprintln!(
        "S2: armed at step {armed}; fv {} x [{}, {}] lenders {} lump {} borrower {} k_s {}",
        s2.fv, s2.x_lo, s2.x_hi, s2.lender_amount, s2.lump, s2.borrower_amount, s2.k_s
    );
    let take = pos5.v - k_s(&pos5);
    assert!(s2.fv > 0);
    assert_eq!(
        s2.lender_amount,
        s2.fv.min(take),
        "D3: the lenders' take capped at FV"
    );
    assert_eq!(s2.borrower_amount, take - s2.lender_amount);
    let (lo, hi) = (s2.x_lo.min(s2.x_hi), s2.x_lo.max(s2.x_hi));
    assert!(u128::from(hi - lo) * 10_000 <= u128::from(pos5.fv_dev_bps) * u128::from(lo));
    assert!(s2.lender_amount + 2 >= lo && s2.lender_amount <= hi + 2);
    assert_eq!(
        (s2.pprime_lo, s2.pprime_hi),
        (pos5.penalty_bps, pos5.penalty_bps)
    );
    assert_settlement_moves(&before, &after, &s2, true);

    // ---- the lenders exit: tokens pro rata, and SOL for the tokens they lost -------------------
    let lost = 2 * q;
    env.warp(21_601);
    env.crank(&rm);
    let m = market(&env, &rm);
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0));
    assert_eq!(
        m.tokens_free,
        deposits - lost,
        "S2 and S3 extinguished two debts of q"
    );
    let wa = withdraw_all(&mut env, &rm, &lender_a, &keys_a);
    let wb = withdraw_all(&mut env, &rm, &lender_b, &keys_b);
    let v = env.market_vault(&rm.market());
    eprintln!(
        "lender A {} tokens {} SOL; lender B {} tokens {} SOL; vault lender_sol {} protocol {} unmatched {}",
        wa.amount, wa.sol_paid, wb.amount, wb.sol_paid, v.lender_sol, v.protocol_fees, v.unmatched
    );
    assert!(wa.amount + wb.amount <= deposits - lost);
    assert!(
        wa.sol_paid > 0 && wb.sol_paid > 0,
        "SOL income and settlements"
    );
    // Where the SOL came from (ARCH §5.2): rent net of the protocol's 15 %, the lenders' half
    // of the penalty, the expiry fee, the S3 and S2 takes; the protocol kept the open fees,
    // its rent share and its penalty share.
    let rent_protocol = |r: u64| r * u64::from(pos1.rent_protocol_bps) / 10_000;
    let rents = [c.rent, l2.rent, e.rent];
    let lenders_in: u64 = rents.iter().map(|&r| r - rent_protocol(r)).sum::<u64>()
        + l2.penalty_lender
        + e.expiry_fee
        + s3.lender_amount
        + s2.lender_amount;
    let protocol_in: u64 = [
        o1.fee_open,
        o2.fee_open,
        o3.fee_open,
        o4.fee_open,
        o5.fee_open,
    ]
    .iter()
    .sum::<u64>()
        + rents.iter().map(|&r| rent_protocol(r)).sum::<u64>()
        + l2.penalty_protocol;
    assert_eq!(
        wa.sol_paid + wb.sol_paid + v.lender_sol,
        lenders_in,
        "every lamport credited to lenders is paid out or left as dust"
    );
    assert!(v.lender_sol < 1_000, "dust {}", v.lender_sol);
    assert_eq!(
        v.protocol_fees,
        protocol_in + env.config().params.market_creation_fee,
        "the protocol holds the creation fee, the open fees, its rent and penalty shares"
    );
    // Pro rata 2 : 1 (both deposited before any income or loss).
    assert!(wa.amount.abs_diff(2 * wb.amount) <= 2 && wa.sol_paid.abs_diff(2 * wb.sol_paid) <= 2);
    let left = market(&env, &rm).tokens_free;
    eprintln!("tokens left in the pool: {left}");
    assert_eq!(wa.amount + wb.amount + left, deposits - lost);
}

// ---- Token-2022 -------------------------------------------------------------------------------

/// The journey on the Token-2022 pool `7ZRtFcgf…` (module docs): a liquidation that the venue
/// delivers straight into the Token-2022 lending vault; the pool admin's config fee hike far above
/// the hard cap turns a liquidatable position into an S3 at FV priced at the position's locked
/// cap (the normal buyback at the live 70 % fee does not fit the vault, and S1 is judged at the
/// capped risk fee); then a status-bit halt freezes a healthy position until the pool resumes.
#[test]
fn token_2022_journey() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(T22);
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    assert_eq!(p.token_program(), TokenProgram::Token2022);
    let open_time = i64::try_from(p.state(&env.svm).open_time).unwrap();
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    let rm = list(&mut env, T22, pool, q * 40);
    assert_eq!(market(&env, &rm).graduated_at, open_time);
    let (lender, keys) = (rm.lender.insecure_clone(), rm.lender_keys());
    warm_window(&mut env, &rm, LIST_WINDOW_S);
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(
        market(&env, &rm).tier,
        Tier::A as u8,
        "3,300 SOL real, a year old"
    );
    let (mut b1, mut b2) = (env.borrower(), env.borrower());
    let p0 = spot(&env, &rm);
    let pool_vault0 = tokens(&env, &rm.keys.pool_vault);
    let (_, n1, _) = open_ok(&mut env, &rm, &mut b1, q);
    let (_, n2, _) = open_ok(&mut env, &rm, &mut b2, q);
    assert_eq!(pool_vault0 - tokens(&env, &rm.keys.pool_vault), 2 * q);
    assert_eq!(read::token_delegate(&env.svm, &rm.keys.pool_vault), None);

    // ---- a pump to ≈ 1.75× the entry: b1 liquidated into the Token-2022 lending vault ---------
    let w = whale(&mut env, &rm);
    let mult = 1.75 * p0 as f64 / spot(&env, &rm) as f64;
    move_price(&mut env, &rm, &w, mult);
    let keeper = env.funded_keypair();
    let pos1 = env.position(&rm, &b1.key(), n1);
    let Exit {
        ev: l1,
        before,
        b_exp,
        ..
    } = liquidate_when_due(&mut env, &rm, &keeper, &b1, n1, 120);
    let after = balances(&env, &rm, &keeper.pubkey(), &b1, n1);
    assert_eq!(l1.buyback_cost, b_exp);
    assert_eq!(
        after.pool_vault - before.pool_vault,
        q,
        "U10: delivered into the T22 vault"
    );
    assert_eq!(
        l1.buyback_cost + l1.rent + l1.penalty + l1.keeper_topup + l1.borrower_payout,
        pos1.v
    );

    // ---- the admin raises config 0's trade fee to 70 % (every pool on it, at once) ------------
    let pos2 = env.position(&rm, &b2.key(), n2);
    let cfg = p.config_key(&env.svm);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 0, 700_000),
    ));
    second(&mut env, &rm);
    let m = market(&env, &rm);
    assert_eq!(
        (m.no_route_since, m.pool_fail_since),
        (env.now(), env.now())
    );
    assert!(m.b_lo.get() > 0, "still executable: still observed");
    let since = m.no_route_since;
    let mut b3 = env.borrower();
    assert_err(&env.open(&rm, &mut b3, q).0, E::VenueFeeTooHigh);
    // b2 is liquidatable at the capped risk fee, but its normal buyback at the live fee does
    // not fit its vault, and its S1 legs (at the capped fee) do not hold.
    let live = rm.quote_buy(&env.svm, q).unwrap().cost;
    assert!(live > pos2.v, "70 %: {live} > V {}", pos2.v);
    assert_eq!(
        keeper_refusal(&mut env, &rm, &keeper, &b2, n2, 30),
        code(E::UseSolSettle)
    );
    let (s3, before, armed) = settle_when_due(&mut env, &rm, &keeper, &b2, n2, 30, 40);
    let after = balances(&env, &rm, &keeper.pubkey(), &b2, n2);
    assert_eq!(s3.condition, 3, "S3 on the fee above the hard cap");
    assert_eq!(armed, None);
    assert!(env.now() >= since + i64::from(pos2.s3_wait_s));
    let m = market(&env, &rm);
    let (pe, le) = (m.ema_price.get(), m.ema_liq.get());
    let r_due = rent::due(&pos2.pos_rent(), &m.basis()).unwrap();
    let cap = u64::from(pos2.venue_fee_hard_cap_bps);
    let at_cap = live_pricing(&env, &rm).capped(cap);
    let Pricing::Cpmm { buy, .. } = at_cap else {
        unreachable!()
    };
    assert_eq!(
        buy.trade,
        cap * 100,
        "risk and FV at the locked 500 bps, never 70 %"
    );
    assert_eq!(
        (s3.pprime_lo, s3.pprime_hi),
        (pos2.penalty_bps, pos2.penalty_bps)
    );
    let fv = fair_value(at_cap, q, pe, le, s3.pprime_lo, r_due);
    assert_eq!(s3.fv, fv, "FV at the capped fee on (P_ema, L_ema)");
    assert_eq!(s3.lender_amount, fv.min(pos2.v - k_s(&pos2)));
    let (lo, hi) = (s3.x_lo.min(s3.x_hi), s3.x_lo.max(s3.x_hi));
    assert!(u128::from(hi - lo) * 10_000 <= u128::from(pos2.fv_dev_bps) * u128::from(lo));
    assert_settlement_moves(&before, &after, &s3, true);

    // ---- the fee restored; a new position frozen by a halt while healthy ---------------------
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 0, 2_500),
    ));
    second(&mut env, &rm);
    assert_eq!(market(&env, &rm).no_route_since, 0);
    env.fresh_oracle();
    second(&mut env, &rm);
    let (_, n3, _) = open_ok(&mut env, &rm, &mut b3, q);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_pool_status_ix(&p, raydium_cpmm::STATUS_BIT_SWAP),
    ));
    for _ in 0..30 {
        env.warp(30);
        env.crank(&rm);
        let (s, a, _) = settle(&mut env, &rm, &keeper, &b3, n3);
        assert!(s.is_none() && a.is_none(), "healthy: frozen, never settled");
    }
    let pos3 = env.position(&rm, &b3.key(), n3);
    assert!(
        env.now() - market(&env, &rm).no_route_since > i64::from(pos3.s3_wait_s),
        "the chain ran past s3_wait_s: S3 needs a liquidatable or expired position"
    );
    assert_err(&env.close(&rm, &b3, n3), E::NoRoute);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_pool_status_ix(&p, 0),
    ));
    second(&mut env, &rm);
    env.warp(1);
    let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
    let c: Close = events::one(&ok(env.close(&rm, &b3, n3)));
    assert_eq!(
        c.buyback_cost, b_exp,
        "the route is back: the borrower closes"
    );

    // ---- the lender exits with Token-2022 tokens and SOL ---------------------------------------
    env.warp(21_601);
    env.crank(&rm);
    let free = market(&env, &rm).tokens_free;
    assert_eq!(free, q * 40 - q, "the S3 extinguished one debt of q");
    let wd = withdraw_all(&mut env, &rm, &lender, &keys);
    assert_eq!(wd.amount, free);
    assert!(
        wd.sol_paid > s3.lender_amount,
        "the S3 take, rent and penalties"
    );
    assert_eq!(tokens(&env, &keys.owner_token_account), free);
}

// ---- R-RAY §3.3 recomputed (T-RAY-MATH, U11) ----------------------------------------------------

/// `pre(a, r) = ⌈a·10⁶/(10⁶ − r)⌉`, `pre(a, 0) = a` (R-RAY §3.3).
fn gross_up(a: u128, r: u128) -> u128 {
    if r == 0 {
        a
    } else {
        (a * PPM).div_ceil(PPM - r)
    }
}

/// One CPMM swap as R-RAY §3.3 states it, recomputed here from the pool's reserves and the
/// config's rates (independently of `shortwin-math`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CpmmTrade {
    amount_in: u64,
    amount_out: u64,
    trade_fee: u64,
    creator_fee: u64,
    protocol_fee: u64,
    fund_fee: u64,
    creator_fee_on_input: bool,
}

/// The rates a swap in `dir` pays on pool state `st` under `p`'s live config: `(t, c, p, f,
/// fee_on_input)` (R-RAY §3.3: `c` only if `enable_creator_fee`; the creator fee is on the
/// input iff `creator_fee_on` is 0 or names the input token).
fn rates(
    env: &Env,
    p: &CpmmPool,
    st: &raydium_cpmm::PoolState,
    dir: Direction,
) -> (u128, u128, u128, u128, bool) {
    let cfg = p.config(&env.svm);
    let input_is_token0 = p.input_is_token0(dir);
    let on_input = match st.creator_fee_on {
        0 => true,
        1 => input_is_token0,
        2 => !input_is_token0,
        other => panic!("creator_fee_on {other}"),
    };
    let c = if st.enable_creator_fee {
        cfg.creator_fee_rate
    } else {
        0
    };
    (
        u128::from(cfg.trade_fee_rate),
        u128::from(c),
        u128::from(cfg.protocol_fee_rate),
        u128::from(cfg.fund_fee_rate),
        on_input,
    )
}

/// `swap_base_input(s)` on `(x, y)` (input, output reserves).
fn exact_in(
    s: u64,
    x: u64,
    y: u64,
    (t, c, p, f, on_input): (u128, u128, u128, u128, bool),
) -> CpmmTrade {
    let (s, x, y) = (u128::from(s), u128::from(x), u128::from(y));
    let (out, trade, creator) = if on_input {
        let total = (s * (t + c)).div_ceil(PPM);
        let creator = (total * c).checked_div(t + c).unwrap_or(0);
        let net = s - total;
        (net * y / (x + net), total - creator, creator)
    } else {
        let trade = (s * t).div_ceil(PPM);
        let net = s - trade;
        let sw = net * y / (x + net);
        let creator = (sw * c).div_ceil(PPM);
        (sw - creator, trade, creator)
    };
    CpmmTrade {
        amount_in: s as u64,
        amount_out: out as u64,
        trade_fee: trade as u64,
        creator_fee: creator as u64,
        protocol_fee: (trade * p / PPM) as u64,
        fund_fee: (trade * f / PPM) as u64,
        creator_fee_on_input: on_input,
    }
}

/// `swap_base_output(q)` on `(x, y)` (input, output reserves): with the creator fee on the
/// output, `q` is grossed up to `q' = pre(q, c)` (U11).
fn exact_out(
    q: u64,
    x: u64,
    y: u64,
    (t, c, p, f, on_input): (u128, u128, u128, u128, bool),
) -> CpmmTrade {
    let (qq, x, y) = (u128::from(q), u128::from(x), u128::from(y));
    let actual = if on_input { qq } else { gross_up(qq, c) };
    let sw = (x * actual).div_ceil(y - actual);
    let (cost, trade, creator) = if on_input {
        let cost = gross_up(sw, t + c);
        let total = cost - sw;
        let creator = (total * c).checked_div(t + c).unwrap_or(0);
        (cost, total - creator, creator)
    } else {
        let cost = gross_up(sw, t);
        (cost, cost - sw, actual - qq)
    };
    CpmmTrade {
        amount_in: cost as u64,
        amount_out: q,
        trade_fee: trade as u64,
        creator_fee: creator as u64,
        protocol_fee: (trade * p / PPM) as u64,
        fund_fee: (trade * f / PPM) as u64,
        creator_fee_on_input: on_input,
    }
}

/// A CPMM pool's state around one short.win leg.
struct PoolSnap {
    st: raydium_cpmm::PoolState,
    vaults: (u64, u64),
}

fn pool_snap(env: &Env, p: &CpmmPool) -> PoolSnap {
    PoolSnap {
        st: p.state(&env.svm),
        vaults: p.vault_amounts(&env.svm),
    }
}

/// What R-RAY §3.3 says short.win's leg of `amount` in `dir` (sell: exact in; buy: exact out)
/// does on the state `pre` the caller snapshotted just before the transaction.
fn expected_leg(env: &Env, p: &CpmmPool, pre: &PoolSnap, dir: Direction, amount: u64) -> CpmmTrade {
    let (r0, r1) = pre
        .st
        .reserves(pre.vaults.0, pre.vaults.1)
        .expect("reserves");
    let (x, y) = if p.input_is_token0(dir) {
        (r0, r1)
    } else {
        (r1, r0)
    };
    let r = rates(env, p, &pre.st, dir);
    match dir {
        Direction::Sell => exact_in(amount, x, y, r),
        Direction::Buy => exact_out(amount, x, y, r),
    }
}

/// short.win's leg in `meta` against R-RAY §3.3 on the pre-trade state: the venue's own
/// `SwapEvent` (one, on this pool), the vaults' deltas and the fee counters' deltas (protocol
/// and fund on the input token, the creator fee on the side it is charged). Returns the trade.
fn assert_leg(
    env: &Env,
    p: &CpmmPool,
    pre: &PoolSnap,
    meta: &TransactionMetadata,
    dir: Direction,
    amount: u64,
) -> CpmmTrade {
    let exp = expected_leg(env, p, pre, dir, amount);
    let ev = fork::cpmm_swap_events(&meta.logs);
    assert_eq!(ev.len(), 1, "one SwapEvent");
    let ev: CpmmSwapEvent = ev[0];
    let (r0, r1) = pre.st.reserves(pre.vaults.0, pre.vaults.1).unwrap();
    let in0 = p.input_is_token0(dir);
    let (x, y) = if in0 { (r0, r1) } else { (r1, r0) };
    assert_eq!(ev.pool_id, p.pool);
    assert_eq!(
        (ev.input_vault_before, ev.output_vault_before),
        (x, y),
        "the venue priced on the reader's reserves"
    );
    assert_eq!(ev.base_input, dir == Direction::Sell);
    assert_eq!(
        (
            ev.input_amount,
            ev.output_amount,
            ev.trade_fee,
            ev.creator_fee,
            ev.creator_fee_on_input
        ),
        (
            exp.amount_in,
            exp.amount_out,
            exp.trade_fee,
            exp.creator_fee,
            exp.creator_fee_on_input
        ),
        "{dir:?} of {amount}: the ELF's SwapEvent equals R-RAY §3.3"
    );
    let post = pool_snap(env, p);
    let (d_in, d_out) = if in0 {
        (post.vaults.0 - pre.vaults.0, pre.vaults.1 - post.vaults.1)
    } else {
        (post.vaults.1 - pre.vaults.1, pre.vaults.0 - post.vaults.0)
    };
    assert_eq!(
        (d_in, d_out),
        (exp.amount_in, exp.amount_out),
        "vault deltas"
    );
    let (a, b) = (&pre.st, &post.st);
    let delta = |after: u64, before: u64| after - before;
    let (prot, fund, cr_in, cr_out) = if in0 {
        (
            delta(b.protocol_fees_token_0, a.protocol_fees_token_0),
            delta(b.fund_fees_token_0, a.fund_fees_token_0),
            delta(b.creator_fees_token_0, a.creator_fees_token_0),
            delta(b.creator_fees_token_1, a.creator_fees_token_1),
        )
    } else {
        (
            delta(b.protocol_fees_token_1, a.protocol_fees_token_1),
            delta(b.fund_fees_token_1, a.fund_fees_token_1),
            delta(b.creator_fees_token_1, a.creator_fees_token_1),
            delta(b.creator_fees_token_0, a.creator_fees_token_0),
        )
    };
    let (exp_in, exp_out) = if exp.creator_fee_on_input {
        (exp.creator_fee, 0)
    } else {
        (0, exp.creator_fee)
    };
    assert_eq!(
        (prot, fund, cr_in, cr_out),
        (exp.protocol_fee, exp.fund_fee, exp_in, exp_out),
        "fee counters"
    );
    exp
}

/// Whether `p`'s creator fee (as the pool holds it now) names the token side, i.e. a buy pays
/// it on its output (U11).
fn creator_fee_on_token_side(env: &Env, p: &CpmmPool) -> bool {
    let st = p.state(&env.svm);
    st.enable_creator_fee && !rates(env, p, &st, Direction::Buy).4
}

/// U11 (ARCH §12; R-RAY §3.4 "exact out with the creator fee on the output side … from source
/// only"): short.win's own legs on the dumped CPMM ELF, with the creator fee off, on the SOL
/// side (as mainnet's `FQd8z3…` charges it), on the token side (a scenario builder: no mainnet
/// pool charges it there), on both, on SPL and on Token-2022 bases. For each: the open's
/// exact-in sale, the close's exact-out buy with `max_buyback_cost` at the formula (one lamport
/// below is `BuybackAboveMax`: short.win's quote is the formula), a liquidation's exact-out buy
/// under the exit bound after a real pump, and a direct user swap with the venue's own bound at
/// the formula (one below is CPMM `ExceededSlippage`). Every leg's `SwapEvent`, vault deltas and
/// fee counters equal R-RAY §3.3 recomputed here; the lending vault receives exactly `q` while
/// the pool's reserve gives up `q' = ⌈q·10⁶/(10⁶ − c)⌉`. **U11 is proven** on the deployed ELF.
#[test]
fn u11_exact_out_creator_fee_on_either_side() {
    let cases: [(RayFixture, bool, u8, bool); 6] = [
        (USELESS, false, 0, false),
        (CREATOR_FEE, true, 1, false),
        (CREATOR_FEE, true, 2, true),
        (CREATOR_FEE, true, 0, false),
        (T22, true, 1, false),
        (T22, true, 2, true),
    ];
    for (fixture, enable, on, u11) in cases {
        let ctx = format!("{fixture:?} creator fee {enable} on {on}");
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(fixture);
        let AnyPool::Cpmm(p) = pool else {
            unreachable!()
        };
        assert!(p.sol_is_token0);
        fork::set_cpmm_creator_fee(&mut env.svm, &p, enable, on);
        assert_eq!(creator_fee_on_token_side(&env, &p), u11, "{ctx}");
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
        let rm = list(&mut env, fixture, pool, q * 40);
        warm_window(&mut env, &rm, LIST_WINDOW_S);
        env.crank(&rm);

        // The open's sale (exact in).
        let mut b = env.borrower();
        let pre = pool_snap(&env, &p);
        let (o, n, meta) = open_ok(&mut env, &rm, &mut b, q);
        let sell = assert_leg(&env, &p, &pre, &meta, Direction::Sell, q);
        assert_eq!(o.s, sell.amount_out, "{ctx}: S is the ELF's output");

        // The close's buy (exact out) with the borrower's bound at the formula.
        second(&mut env, &rm);
        env.warp(1);
        let pre = pool_snap(&env, &p);
        let exp = expected_leg(&env, &p, &pre, Direction::Buy, q);
        assert_eq!(
            rm.quote_buy(&env.svm, q).unwrap().cost,
            exp.amount_in,
            "{ctx}: shortwin-math's B̂ is the formula"
        );
        let kp = b.kp.insecure_clone();
        let ix = close_short_ix(&rm, &b.key(), n, exp.amount_in - 1, ExtrasSel::NONE, &[]);
        assert_err(&env.send(&[ix], &[&kp]), E::BuybackAboveMax);
        let pool_vault0 = tokens(&env, &rm.keys.pool_vault);
        let ix = close_short_ix(&rm, &b.key(), n, exp.amount_in, ExtrasSel::NONE, &[]);
        let meta = ok(env.send(&[ix], &[&kp]));
        let buy = assert_leg(&env, &p, &pre, &meta, Direction::Buy, q);
        let c: Close = events::one(&meta);
        assert_eq!(c.buyback_cost, buy.amount_in, "{ctx}");
        assert_eq!(
            tokens(&env, &rm.keys.pool_vault) - pool_vault0,
            q,
            "{ctx}: exactly q into the lending vault"
        );
        if u11 {
            assert_eq!(
                u128::from(q + buy.creator_fee),
                gross_up(
                    u128::from(q),
                    u128::from(p.config(&env.svm).creator_fee_rate)
                ),
                "{ctx}: the pool's reserve gave up q' = pre(q, c)"
            );
        }

        // The venue's own bound, on a direct swap: the formula passes, one lamport less fails.
        let (mint, program) = rm.pool.token();
        let t = fork::trader(&mut env, &mint, program, 100_000_000_000, 0);
        let pre = pool_snap(&env, &p);
        let exp = expected_leg(&env, &p, &pre, Direction::Buy, q);
        let res = fork::swap(
            &mut env,
            &rm.pool,
            &t,
            Direction::Buy,
            Swap::ExactOut(exp.amount_in - 1, q),
        );
        assert_eq!(
            custom_code(&res),
            Some(fork::CPMM_ERR_EXCEEDED_SLIPPAGE),
            "{ctx}"
        );
        let meta = ok(fork::swap(
            &mut env,
            &rm.pool,
            &t,
            Direction::Buy,
            Swap::ExactOut(exp.amount_in, q),
        ));
        assert_leg(&env, &p, &pre, &meta, Direction::Buy, q);
        assert_eq!(
            tokens(&env, &t.token),
            q,
            "{ctx}: the user received exactly q"
        );

        // A liquidation's buy after a real pump: the exit bound is above the quote, the venue
        // still charges exactly the formula. Tier C (the fork's oracle is stale): liquidatable
        // above ≈ 2× the entry, a normal buyback below ≈ 2.9×.
        ema_converges(&mut env, &rm, 10, 120);
        let mut b2 = env.borrower();
        let p_entry = spot(&env, &rm);
        let (_, n2, _) = open_ok(&mut env, &rm, &mut b2, q);
        let w = whale(&mut env, &rm);
        let mult = 2.4 * p_entry as f64 / spot(&env, &rm) as f64;
        move_price(&mut env, &rm, &w, mult);
        let keeper = env.funded_keypair();
        let x = liquidate_when_due(&mut env, &rm, &keeper, &b2, n2, 120);
        let buy = assert_leg(&env, &p, &x.pool, &x.meta, Direction::Buy, q);
        assert_eq!(
            (x.ev.buyback_cost, x.b_exp),
            (buy.amount_in, buy.amount_in),
            "{ctx}"
        );
        assert!(
            x.ev.max_in > buy.amount_in,
            "{ctx}: the bound was not the constraint"
        );
        assert_eq!(buy.creator_fee_on_input, !u11);
    }
}

// ---- the creator-fee pool ----------------------------------------------------------------------

/// The journey on mainnet's creator-fee pool `FQd8z3…` (config 0, `creator_fee_on = 1`: the
/// creator fee on WSOL = token 0), Tier B (≈ 575 SOL real: θ_B ≤ Λ < θ_A): every leg's
/// `SwapEvent` and counters equal R-RAY §3.3 (sales pay the creator fee on their SOL output,
/// buys on their SOL input); then the fee moves to the token side with two positions open (a
/// scenario builder; U11), and the liquidation and the expiry that follow buy exactly `q` into
/// the lending vault while the pool's reserve gives up `q' = pre(q, c)`; the lender gets every
/// token back and SOL income.
#[test]
fn creator_fee_journey() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(CREATOR_FEE);
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    let st = p.state(&env.svm);
    assert!(st.enable_creator_fee && st.creator_fee_on == 1 && p.sol_is_token0);
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    let rm = list(&mut env, CREATOR_FEE, pool, q * 40);
    assert_eq!(
        market(&env, &rm).graduated_at,
        i64::try_from(st.open_time).unwrap()
    );
    let (lender, keys) = (rm.lender.insecure_clone(), rm.lender_keys());
    warm_window(&mut env, &rm, LIST_WINDOW_S);
    env.fresh_oracle();
    env.crank(&rm);
    assert_eq!(
        market(&env, &rm).tier,
        Tier::B as u8,
        "θ_B ≤ 2·575 SOL < θ_A"
    );

    let mut bs = [env.borrower(), env.borrower(), env.borrower()];
    let p_entry = spot(&env, &rm);
    let mut nonces = [0u64; 3];
    for (i, b) in bs.iter_mut().enumerate() {
        let pre = pool_snap(&env, &p);
        let (o, n, meta) = open_ok(&mut env, &rm, b, q);
        let sell = assert_leg(&env, &p, &pre, &meta, Direction::Sell, q);
        assert!(
            !sell.creator_fee_on_input && sell.creator_fee > 0,
            "on the SOL output"
        );
        assert_eq!(o.s, sell.amount_out);
        let pos = env.position(&rm, &b.key(), n);
        assert_eq!(
            (pos.tier, pos.cr_liq_bps, pos.penalty_bps),
            (Tier::B as u8, 13_500, 700)
        );
        assert_eq!(pos.deadline, pos.t_open + 1_209_600, "Tier B: 14 days");
        nonces[i] = n;
    }
    let [b1, b2, b3] = &bs;

    // b1 closes: the creator fee on the buy's SOL input.
    second(&mut env, &rm);
    env.warp(1);
    let pre = pool_snap(&env, &p);
    let meta = ok(env.close(&rm, b1, nonces[0]));
    let buy = assert_leg(&env, &p, &pre, &meta, Direction::Buy, q);
    assert!(buy.creator_fee_on_input && buy.creator_fee > 0);
    let c: Close = events::one(&meta);
    assert_eq!(c.buyback_cost, buy.amount_in);

    // The creator fee moves to the token side (token 1) with b2 and b3 open.
    fork::set_cpmm_creator_fee(&mut env.svm, &p, true, raydium_cpmm::CREATOR_FEE_ON_TOKEN_1);
    assert!(creator_fee_on_token_side(&env, &p));
    second(&mut env, &rm);

    // A pump to ≈ 2.15× the entry (Tier B: liquidatable above ≈ 1.85×, a normal buyback below
    // ≈ 2.45×): the keeper liquidates b2; the buy's creator fee is on its token output.
    let w = whale(&mut env, &rm);
    let mult = 2.15 * p_entry as f64 / spot(&env, &rm) as f64;
    move_price(&mut env, &rm, &w, mult);
    let keeper = env.funded_keypair();
    let pos2 = env.position(&rm, &b2.key(), nonces[1]);
    let x = liquidate_when_due(&mut env, &rm, &keeper, b2, nonces[1], 120);
    let buy = assert_leg(&env, &p, &x.pool, &x.meta, Direction::Buy, q);
    assert!(!buy.creator_fee_on_input && buy.creator_fee > 0, "U11");
    let after = balances(&env, &rm, &keeper.pubkey(), b2, nonces[1]);
    assert_eq!(x.ev.buyback_cost, buy.amount_in);
    assert_eq!(after.pool_vault - x.before.pool_vault, q);
    assert_eq!(
        x.ev.buyback_cost + x.ev.rent + x.ev.penalty + x.ev.keeper_topup + x.ev.borrower_payout,
        pos2.v
    );

    // The price comes back; b3 stays healthy until its deadline and expires: again U11.
    let back = p_entry as f64 / spot(&env, &rm) as f64;
    move_price(&mut env, &rm, &w, back);
    ema_converges(&mut env, &rm, 10, 300);
    let pos3 = env.position(&rm, &b3.key(), nonces[2]);
    let slot = env.clock().slot;
    env.set_clock(pos3.deadline + 1, slot + 3_024_000);
    env.tick(&rm, 3);
    let x = expire_now(&mut env, &rm, &keeper, b3, nonces[2]);
    let buy = assert_leg(&env, &p, &x.pool, &x.meta, Direction::Buy, q);
    assert!(!buy.creator_fee_on_input && buy.creator_fee > 0, "U11");
    assert_eq!((x.ev.buyback_cost, x.b_exp), (buy.amount_in, buy.amount_in));
    assert_eq!(
        x.ev.buyback_cost + x.ev.rent + x.ev.expiry_fee + x.ev.keeper_reward + x.ev.borrower_payout,
        pos3.v
    );

    // Every position was bought back: the lender gets all its tokens and SOL income.
    env.warp(21_601);
    env.crank(&rm);
    let wd = withdraw_all(&mut env, &rm, &lender, &keys);
    assert_eq!(wd.amount, q * 40);
    assert!(wd.sol_paid > 0);
    assert_eq!(market(&env, &rm).open_positions, 0);
}

// ---- O13 on CPMM -----------------------------------------------------------------------------

/// Sets the clock to `t` (slots at 2.5 per second), writes a fresh oracle price and cranks:
/// the market's tier then.
fn tier_at(env: &mut Env, rm: &RayMarket, t: i64) -> u8 {
    let c = env.clock();
    assert!(t >= c.unix_timestamp);
    env.set_clock(
        t,
        c.slot + u64::try_from(t - c.unix_timestamp).unwrap() * 5 / 2,
    );
    env.fresh_oracle();
    env.crank(rm);
    market(env, rm).tier
}

/// O13 / ARCH §4.7 on CPMM (`graduated_at = min(now, open_time)` when `open_time ≥ T_CLAMP`,
/// else the listing time; SPEC §4 O13, R3B-12, R-RAY §7):
///
/// - a pool whose `open_time` is 2 h before its listing (created after `T_CLAMP`, so its
///   `open_time` bounds its age) ages from `open_time`, not from the listing: with a fresh
///   oracle and liquidity far above θ_A it is Tier C until exactly `open_time + 24 h`, Tier B
///   until exactly `open_time + 7 d`, then Tier A; the listing window still counts from the
///   listing; each position locks its tier's terms and keeps them (A26);
/// - a pool whose `open_time` precedes `T_CLAMP` stores no provable creation time and ages from
///   its listing like an AMM v4 pool: Tier C for 24 h, B until day 7, then A;
/// - `open_time == now` lists with age 0 (the venue already swaps: `now ≥ open_time`), one
///   second later it is not executable and not eligible.
///
/// (`open_time` is immutable on mainnet, R-RAY §5.2; the scenario builder sets the value a
/// pool would have been created with.)
#[test]
fn o13_age_of_a_cpmm_pool() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(USELESS);
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    let listed = env.now();
    let opened = listed - 7_200;
    fork::set_cpmm_open_time(&mut env.svm, &p, u64::try_from(opened).unwrap());
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    let rm = list(&mut env, USELESS, pool, q * 40);
    let m = market(&env, &rm);
    assert_eq!((m.graduated_at, m.pool_seen_at), (opened, listed));

    let mut b = env.borrower();
    warm_window(&mut env, &rm, LIST_WINDOW_S - 2);
    assert_eq!(
        tier_at(&mut env, &rm, listed + LIST_WINDOW_S - 1),
        Tier::C as u8
    );
    assert_err(&env.open(&rm, &mut b, q).0, E::PoolListWindow);
    assert_eq!(
        tier_at(&mut env, &rm, listed + LIST_WINDOW_S),
        Tier::C as u8
    );
    let (o1, n1, _) = open_ok(&mut env, &rm, &mut b, q);
    let pos1 = env.position(&rm, &b.key(), n1);
    assert_eq!(
        (o1.tier, pos1.cr_liq_bps, pos1.penalty_bps),
        (Tier::C as u8, 15_000, 800),
        "age 3 h: Tier C although Λ ≫ θ_A"
    );
    assert_eq!(pos1.deadline, pos1.t_open + 604_800, "Tier C: 7 days");

    assert_eq!(tier_at(&mut env, &rm, opened + DAY - 1), Tier::C as u8);
    assert_eq!(
        tier_at(&mut env, &rm, opened + DAY),
        Tier::B as u8,
        "24 h after open_time"
    );
    let (o2, n2, _) = open_ok(&mut env, &rm, &mut b, q);
    let pos2 = env.position(&rm, &b.key(), n2);
    assert_eq!((o2.tier, pos2.cr_liq_bps), (Tier::B as u8, 13_500));
    assert_eq!(tier_at(&mut env, &rm, opened + WEEK - 1), Tier::B as u8);
    assert_eq!(
        tier_at(&mut env, &rm, opened + WEEK),
        Tier::A as u8,
        "7 d after open_time"
    );
    let (o3, n3, _) = open_ok(&mut env, &rm, &mut b, q);
    let pos3 = env.position(&rm, &b.key(), n3);
    assert_eq!((o3.tier, pos3.cr_liq_bps), (Tier::A as u8, 12_500));
    assert_eq!(pos3.deadline, pos3.t_open + 2_592_000);
    // Open positions keep the terms of the tier they opened in.
    assert_eq!(env.position(&rm, &b.key(), n1), pos1);
    assert!(
        env.now() < pos1.deadline,
        "b's Tier C position is not expired yet"
    );
    for n in [n1, n2, n3] {
        env.warp(1);
        let c: Close = events::one(&ok(env.close(&rm, &b, n)));
        assert!(c.buyback_cost > 0);
    }

    // A pool whose open_time precedes T_CLAMP proves no creation time: its age counts from the
    // listing (O13), Tier C for 24 h and at most B until day 7, whatever its liquidity.
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(USELESS);
    fork::set_cpmm_open_time(&mut env.svm, &p, 1_600_000_000);
    let listed = env.now();
    let rm = list(&mut env, USELESS, pool, q * 40);
    assert_eq!(market(&env, &rm).graduated_at, listed);
    warm_window(&mut env, &rm, LIST_WINDOW_S - 1);
    assert_eq!(
        tier_at(&mut env, &rm, listed + LIST_WINDOW_S),
        Tier::C as u8
    );
    let mut b = env.borrower();
    let (o, _, _) = open_ok(&mut env, &rm, &mut b, q);
    assert_eq!(o.tier, Tier::C as u8, "age 1 h: Tier C although Λ ≫ θ_A");
    assert_eq!(tier_at(&mut env, &rm, listed + DAY - 1), Tier::C as u8);
    assert_eq!(tier_at(&mut env, &rm, listed + DAY), Tier::B as u8);
    assert_eq!(tier_at(&mut env, &rm, listed + WEEK - 1), Tier::B as u8);
    assert_eq!(tier_at(&mut env, &rm, listed + WEEK), Tier::A as u8);

    // open_time == now: age 0 at the listing; one second in the future: not executable.
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(USELESS);
    let now = env.now();
    fork::set_cpmm_open_time(&mut env.svm, &p, u64::try_from(now + 1).unwrap());
    let res = env.list_ray(&pool, USELESS.hint());
    assert_err(&res, E::PoolIneligible);
    fork::set_cpmm_open_time(&mut env.svm, &p, u64::try_from(now).unwrap());
    let rm = list(&mut env, USELESS, pool, q * 40);
    assert_eq!(market(&env, &rm).graduated_at, now, "min(now, open_time)");
    assert_eq!(tier_at(&mut env, &rm, now + LIST_WINDOW_S), Tier::C as u8);
    assert_eq!(tier_at(&mut env, &rm, now + DAY), Tier::B as u8);
}

// ---- every settlement path on every CPMM fixture ------------------------------------------------

/// What makes a liquidatable position settle in SOL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    /// The pool admin sets the swap bit (`update_pool_status(4)`): S3 on a venue that cannot
    /// execute, FV on the frozen EMA point.
    Halt,
    /// The pool admin raises the config's trade fee to 70 % (`update_amm_config(0, …)`): S3 on
    /// an executable venue (`s3_no_route`), FV at the position's locked hard cap.
    FeeHike,
    /// The out-of-model drain (module docs) and a real pump of the thin pool: S2.
    Drain,
}

/// One position on `fixture` (the creator fee moved to the token side if `creator_on_token`)
/// settled in SOL by `trigger`, with the lenders' take equal to `min(V − K_s, FV(P_ema, L_ema))`
/// at `min(φ_live, cap)` and every lamport accounted for. Tier C (the fork's oracle is stale):
/// `V ≈ 3·B̂` at open, liquidatable above ≈ 2× the entry, S1 only above ≈ 2.9×.
fn settle_case(fixture: RayFixture, creator_on_token: bool, trigger: Trigger) {
    let ctx = format!("{fixture:?} creator fee on the token side {creator_on_token} {trigger:?}");
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(fixture);
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    if creator_on_token {
        fork::set_cpmm_creator_fee(&mut env.svm, &p, true, raydium_cpmm::CREATOR_FEE_ON_TOKEN_1);
        assert!(creator_fee_on_token_side(&env, &p));
    }
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    let rm = list(&mut env, fixture, pool, q * 40);
    warm_window(&mut env, &rm, LIST_WINDOW_S);
    env.crank(&rm);
    let mut b = env.borrower();
    let p_entry = spot(&env, &rm);
    let (o, n, _) = open_ok(&mut env, &rm, &mut b, q);
    assert_eq!(o.tier, Tier::C as u8, "{ctx}");
    let pos = env.position(&rm, &b.key(), n);
    let keeper = env.funded_keypair();
    let w = whale(&mut env, &rm);
    env.tick(&rm, 5);

    let (ev, before) = match trigger {
        Trigger::Halt | Trigger::FeeHike => {
            let mult = 2.3 * p_entry as f64 / spot(&env, &rm) as f64;
            move_price(&mut env, &rm, &w, mult);
            ema_converges(&mut env, &rm, 10, 300);
            if trigger == Trigger::Halt {
                ok(fork::raydium_admin_send(
                    &mut env,
                    fork::cpmm_update_pool_status_ix(&p, raydium_cpmm::STATUS_BIT_SWAP),
                ));
            } else {
                let cfg = p.config_key(&env.svm);
                ok(fork::raydium_admin_send(
                    &mut env,
                    fork::cpmm_update_amm_config_ix(&cfg, 0, 700_000),
                ));
                // The keeper's normal buyback would cost more than the vault holds.
                assert_eq!(
                    keeper_refusal(&mut env, &rm, &keeper, &b, n, 5),
                    code(E::UseSolSettle),
                    "{ctx}"
                );
            }
            let (ev, before, armed) = settle_when_due(&mut env, &rm, &keeper, &b, n, 30, 40);
            assert_eq!((ev.condition, armed), (3, None), "{ctx}: S3, no arming");
            (ev, before)
        }
        Trigger::Drain => {
            drain_to(&mut env, &rm, q * 5);
            env.tick(&rm, 120);
            let mult = 1.8 * p_entry as f64 / spot(&env, &rm) as f64;
            move_price(&mut env, &rm, &w, mult);
            assert_eq!(
                keeper_refusal(&mut env, &rm, &keeper, &b, n, 60),
                code(E::UseSolSettle),
                "{ctx}"
            );
            assert!(sigma_live(&env, &rm, q) > u64::from(pos.max_liq_slip_bps));
            let (ev, before, armed) = settle_when_due(&mut env, &rm, &keeper, &b, n, 1, 300);
            assert_eq!(ev.condition, 2, "{ctx}: S2");
            assert!(armed.is_some(), "{ctx}: S2 on the arming clock");
            let (lo, hi) = (ev.x_lo.min(ev.x_hi), ev.x_lo.max(ev.x_hi));
            assert!(u128::from(hi - lo) * 10_000 <= u128::from(pos.fv_dev_bps) * u128::from(lo));
            (ev, before)
        }
    };
    let after = balances(&env, &rm, &keeper.pubkey(), &b, n);
    let m = market(&env, &rm);
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let pricing = live_pricing(&env, &rm).capped(u64::from(pos.venue_fee_hard_cap_bps));
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.penalty_bps, pos.penalty_bps),
        "{ctx}"
    );
    let fv = fair_value(
        pricing,
        q,
        m.ema_price.get(),
        m.ema_liq.get(),
        pos.penalty_bps,
        r_due,
    );
    let take = pos.v - k_s(&pos);
    assert_eq!(ev.fv, fv, "{ctx}: FV(P_ema, L_ema) at min(φ_live, cap)");
    assert_eq!(ev.lender_amount, fv.min(take), "{ctx}");
    assert_eq!(ev.borrower_amount, take - ev.lender_amount, "{ctx}");
    assert_settlement_moves(&before, &after, &ev, true);
    let m = market(&env, &rm);
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0), "{ctx}");
    assert_eq!(
        m.tokens_free,
        q * 39,
        "{ctx}: the debt of q is extinguished"
    );
}

/// S3 on an admin halt, S3 on a fee hike above the hard cap and S2 on a drained pool, on
/// USELESS, the Token-2022 pool, the creator-fee pool as mainnet has it (creator fee on WSOL)
/// and with its creator fee on the token side: the settlement's FV prices the creator fee on the
/// side the pool charges it, at the capped risk view (above the cap: the trade fee alone, at the
/// cap), and the take, the borrower's excess and every lamport follow ARCH §5.2.
#[test]
fn settlements_on_every_fixture() {
    for (fixture, creator_on_token) in [
        (USELESS, false),
        (T22, false),
        (CREATOR_FEE, false),
        (CREATOR_FEE, true),
    ] {
        for trigger in [Trigger::Halt, Trigger::FeeHike, Trigger::Drain] {
            settle_case(fixture, creator_on_token, trigger);
        }
    }
}
