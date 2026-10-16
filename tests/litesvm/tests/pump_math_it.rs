//! T-PUMP-MATH, execution half (M5; ARCH §4.2, §4.3 curve rows, §8.2; R-PUMP §3.5, §4, §7.2–
//! §7.3; Update 2026-09-29 U4, U6, U7): a **normal user's** `buy_v2`, `sell_v2` and
//! `buy_exact_quote_in_v2` on the **real** pump program dumped from mainnet (deploy slot
//! 449,734,335, with the real pump-fees program it CPIs for the fee tier;
//! `tests/fixtures/`, loaded by `shortwin_litesvm_tests::fork`) charge exactly what
//! `shortwin-math` computes on the `shortwin-venue-state` readers' view of the curve, the live
//! `FeeConfig` and `Global`:
//!
//! - **buys** (`buy_v2`, exact tokens out): `quote::sol_leg::curve_charge_exact_out` —
//!   `n = ⌊q·vQ/(vT − q)⌋ + 1`, each fee `⌈n·bps/10⁴⌉` — with `fees::pump_curve`'s tier; and
//!   short.win's own conservative quote (`sol_leg::buy_exact_out`, ε₁ = 1) is never below the
//!   charge and at most `1 + #fees` above it (ECON §3.1, §3.2);
//! - **sells** (`sell_v2`, exact tokens in): `quote::sol_leg::sell_exact_in` with the real-reserve
//!   cap `rQ`;
//! - **exact quote in** (`buy_exact_quote_in_v2`): R-PUMP §4's rule with the fees on the
//!   pre-adjustment net (Update 2026-09-29 U4; `fork::pump_exact_quote_in`, a test-side
//!   reference because short.win never trades exact-in), and its tokens bracketed by
//!   `curve_charge_exact_out`;
//! - over amount grids from 1 raw unit to the whole curve, and seeded random sequences, on every
//!   live SOL curve of the dump: 125 bytes (`create_v2` today), 151 (extended), 115 (Token-2022
//!   and legacy SPL-Token) and the 2024 49-byte layout (the first v2 trade grows a curve under
//!   141 bytes to 141 since pump's 2026-10-02 redeploy — 125 before —, the user topping up its
//!   rent reserve); at the live fee tier and under
//!   rewritten `FeeConfig`s (PumpSwap's 25 market-cap tiers, thresholds at a curve's own market
//!   cap, the per-coin creator override, a default creator). One deviation is pinned as a
//!   tripwire: the deployed pump resolves the tier at the mint's **live** supply, `fees::
//!   pump_curve` at a fixed 10¹⁵ (no quote differs under today's single tier);
//! - the exact bound passes and one unit tighter fails (6002 / 6003 / 6042); every lamport and
//!   token delta (user, curve, fee and buyback recipients, creator vault) and the curve's
//!   post-trade reserves (reader) equal the quote, and so does the venue's own `TradeEvent`;
//! - a buy beyond `rT` reverts with 6021 on every buy instruction (the undocumented trailing
//!   cap byte of U7 fills exactly `rT` instead), the completing buy of exactly `rT` completes the
//!   curve, and every trade on a complete curve — the one just completed and the three migrated
//!   curves of the dump — reverts with 6005;
//! - venue-side rent (T-RENT's curve part): a creator vault that does not exist is topped up to
//!   `Rent.minimum_balance(0)` from the user on buys and sells alike, with mainnet's Rent.
//!
//! Since the 2026-10-13 re-pin every test runs pump's 2026-10-13 build (`PumpDeploy::CURRENT`,
//! `Oct2026Chain`; the first re-pin, to the 2026-10-08 build, measured the same figures, and
//! `the_2026_10_13_build_runs_short_wins_paths_as_the_2026_10_08_build` replays every path below
//! on both builds and compares every account, event and error):
//! its v2 trades charge and pay exactly as before; a curve shorter than 166 bytes grows to 166 on
//! its next trade, the user topping its rent reserve — `lamports − rQ −` the fee buckets — up to
//! `minimum_balance(166)`. **T-PUMP-MATH-V3** (`v3_*`, gate G0) runs pump's own v3 trades as a
//! third party, no short.win instruction involved: (a) `buy_v3` / `sell_v3` on 200 random states
//! per live curve against the v3 model (`n = ⌈q·vQ/(vT − q)⌉`, the fees `⌈⌉` per component into
//! the buckets less the buyback, `vQ − rQ` unchanged; exact divisions recorded); (b) the synthetic
//! migration — `buy_v3(rT + δ)` charged `F(n₁) + F(n₂)` on the live `X_s = curve_base_ata − rT`,
//! the events `TradeEvent`, `CompleteEvent`, `PostCompleteBuyEvent`, short.win's `B̂_v3` within
//! the spec's bound of the charge; (b′) the same after a token donation to `curve_base_ata`;
//! (b″) the same with the curve's own `creator_fee_bps` (the flag on and off) and with a default
//! creator, so that both parts follow `φ_s`'s creator rule; (c) `buy_exact_quote_in_v3` past `rT`; (d) `migrate_v2` seeding exactly the live
//! `curve_base_ata` and `rQ + pcQ − fee`; (e) the pool part's fee-tier tripwire; (f) the cashback
//! refusal 6094; (g) no synthetic path on the 2026-10-02 build. Since P1, `B̂_v3` is
//! `shortwin_math::quote::curve_synthetic` at the synthetic fee `fees::pump_curve_at_supply` (the
//! curve's tier on the mint's live supply, `PumpCurve::synthetic_fees`), and the boost is
//! `quote::migration_boost`.
//!
//! Every transaction goes through `Env::send` (the invariant hooks run after every
//! instruction). `PUMP_MATH_SEQ` sets the random sequence length per curve (default 30) and
//! `PUMP_MATH_SEED` its seed.

// `TxResult` is LiteSVM's own result type (see the harness's lib.rs).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use shortwin::constants::{
    PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_PROGRAM_ID, PUMP_FEE_CONFIG,
    PUMP_FEES_PROGRAM_ID, PUMP_GLOBAL, PUMP_PROGRAM_ID, VENUE_GUARD_SLOTS_2026_09_27,
    VENUE_PROGRAM_DATA,
};
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::fork::{
    self, PumpCoin, PumpCurve, PumpDeploy, PumpPool, PumpProgram, PumpTradeEvent, PumpV2,
};
use shortwin_litesvm_tests::pump::{self as spump, CurveUser};
use shortwin_litesvm_tests::{Env, TxResult, pda, read};
use shortwin_math::fees::{self as mfees, FeeBps, FeeTier as MathFeeTier};
use shortwin_math::quote::{
    CurveState, MigrationParams, curve_synthetic, migrated_pool, migration_boost, sol_leg,
};
use shortwin_venue_state::{program_data, pump, pump_amm};
use solana_keypair::Keypair;
use solana_signer::Signer;

// ---- users and trades ----------------------------------------------------------------------------

/// A normal user: a funded keypair with its base-token ATA and its pump UVA, both created before
/// any trade (the ATA program and pump's own `init_user_volume_accumulator`, through `Env::send`),
/// so a trade's deltas carry no account creation.
struct User {
    kp: Keypair,
    ata: Pubkey,
}

fn user(env: &mut Env, c: &PumpCurve) -> User {
    let kp = env.funded_keypair();
    let payer = env.payer.pubkey();
    let program = c.token_program.id();
    let ixs = [
        fork::create_ata_idempotent_ix(&payer, &kp.pubkey(), &c.mint, &program),
        fork::pump_init_uva_ix(&payer, &kp.pubkey()),
    ];
    ok(env.send(&ixs, &[]), "user setup");
    let uva = fork::pump_user_volume_accumulator(&kp.pubkey());
    let acc = env.svm.get_account(&uva).expect("UVA");
    let u = pump::read_user_volume_accumulator(&acc.owner.to_bytes(), &acc.data).expect("UVA");
    assert_eq!(u.user, kp.pubkey().to_bytes());
    assert_eq!(acc.data.len(), pump::USER_VOLUME_ACCUMULATOR_LEN);
    User {
        ata: pda::ata(&kp.pubkey(), &c.mint, &program),
        kp,
    }
}

#[track_caller]
fn ok(res: TxResult, what: &str) -> litesvm::types::TransactionMetadata {
    match res {
        Ok(m) => m,
        Err(f) => panic!("{what}: failed with {:?}\n{}", f.err, f.meta.pretty_logs()),
    }
}

fn describe(res: &TxResult) -> String {
    match res {
        Ok(m) => format!("succeeded\n{}", m.pretty_logs()),
        Err(f) => format!("failed with {:?}\n{}", f.err, f.meta.pretty_logs()),
    }
}

/// Fee recipient `k % 8` and buyback recipient `(k / 8) % 8` of the live `Global` (every listed
/// recipient is accepted; spread like the keeper spreads them).
fn recipients(svm: &LiteSVM, k: u64) -> (Pubkey, Pubkey) {
    let i = usize::try_from(k % 8).unwrap();
    let j = usize::try_from((k / 8) % 8).unwrap();
    (
        fork::pump_fee_recipient(svm, i),
        fork::pump_buyback_fee_recipient(svm, j),
    )
}

/// One v2 trade by `u`, with recipients chosen by `k`.
fn trade(env: &mut Env, c: &PumpCurve, u: &User, kind: PumpV2, a: u64, b: u64, k: u64) -> TxResult {
    let (fr, bb) = recipients(&env.svm, k);
    let ix = c.v2_ix(&env.svm, kind, &u.kp.pubkey(), &u.ata, &fr, &bb, a, b);
    let signer = u.kp.insecure_clone();
    env.send(&[ix], &[&signer])
}

/// Everything a trade moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Balances {
    user: u64,
    user_tokens: u64,
    curve: u64,
    curve_tokens: u64,
    fee_recipient: u64,
    buyback: u64,
    creator_vault: u64,
}

fn balances(svm: &LiteSVM, c: &PumpCurve, u: &User, k: u64) -> Balances {
    let (fr, bb) = recipients(svm, k);
    Balances {
        user: read::lamports(svm, &u.kp.pubkey()),
        user_tokens: read::token_amount(svm, &u.ata).unwrap_or(0),
        curve: read::lamports(svm, &c.curve),
        curve_tokens: read::token_amount(svm, &c.curve_base_ata).unwrap_or(0),
        fee_recipient: read::lamports(svm, &fr),
        buyback: read::lamports(svm, &bb),
        creator_vault: read::lamports(svm, &c.creator_vault(svm)),
    }
}

fn fee_up(x: u64, bps: u64) -> u64 {
    (u128::from(x) * u128::from(bps))
        .div_ceil(10_000)
        .try_into()
        .unwrap()
}

/// The buyback share of a protocol fee: `⌊protocol·buyback_bps/10⁴⌋` (R-PUMP §4 "Where the fees
/// go"; the fee recipient gets the rest).
fn buyback_share(svm: &LiteSVM, protocol: u64) -> u64 {
    (u128::from(protocol) * u128::from(fork::pump_buyback_bps(svm)) / 10_000)
        .try_into()
        .unwrap()
}

/// The venue-side rent pump takes from the user for the creator vault (Update 2026-09-29 U6): a
/// vault holding less than `Rent.minimum_balance(0)` is topped up to it, on top of the fee.
fn creator_top_up(svm: &LiteSVM, vault_before: u64, creator_fee: u64) -> u64 {
    let min0 = svm.minimum_balance_for_rent_exemption(0);
    if creator_fee > 0 && vault_before < min0 {
        min0 - vault_before
    } else {
        0
    }
}

/// The single `TradeEvent` of a transaction.
fn one_trade(meta: &litesvm::types::TransactionMetadata) -> PumpTradeEvent {
    let mut ev = fork::pump_trade_events(meta);
    assert_eq!(ev.len(), 1, "one TradeEvent\n{}", meta.pretty_logs());
    ev.remove(0)
}

/// The curve's growth on a trade, as the deployed ELF does it: a curve shorter than the current
/// layout grows to [`PumpDeploy::CURRENT`]'s length (166 bytes since the 2026-10-08 redeploy, the
/// fee buckets and the post-completion fields included; 141 on the 2026-10-02 build, 125 before
/// — not the docs' 115), and the user tops its rent reserve (`lamports − rQ −` the fee buckets,
/// `fork::curve_growth_top_up`) up to `Rent.minimum_balance(len)`; a longer curve is left alone.
/// Returns the expected top-up.
#[track_caller]
fn curve_growth(
    svm: &LiteSVM,
    ctx: &str,
    len_before: usize,
    len_after: usize,
    lamports_before: u64,
    rq_before: u64,
    buckets_before: (u64, u64),
) -> u64 {
    let grows_to = PumpDeploy::CURRENT.curve_len();
    assert_eq!(
        len_after,
        len_before.max(grows_to),
        "{ctx}: a trade leaves the curve at max(len, {grows_to})"
    );
    fork::curve_growth_top_up(
        svm,
        len_before,
        lamports_before,
        rq_before,
        buckets_before.0 + buckets_before.1,
        grows_to,
    )
}

/// The reader's curve state after a trade equals the quote's and the event's post-state.
#[track_caller]
fn check_post_state(
    ctx: &str,
    after: &pump::BondingCurve,
    before: &pump::BondingCurve,
    ev: &PumpTradeEvent,
    (vt, vq, rt, rq): (u64, u64, u64, u64),
) {
    assert_eq!(
        (
            after.virtual_token_reserves,
            after.virtual_quote_reserves,
            after.real_token_reserves,
            after.real_quote_reserves
        ),
        (vt, vq, rt, rq),
        "{ctx}: the curve after the trade"
    );
    assert_eq!(
        (
            ev.virtual_token_reserves,
            ev.virtual_sol_reserves,
            ev.real_token_reserves,
            ev.real_sol_reserves
        ),
        (vt, vq, rt, rq),
        "{ctx}: the TradeEvent's post-state"
    );
    assert_eq!(after.complete, rt == 0, "{ctx}: complete iff rT = 0");
    assert_eq!(
        (after.creator, after.quote_mint, after.token_total_supply),
        (before.creator, before.quote_mint, before.token_total_supply),
        "{ctx}: nothing else changes"
    );
}

/// The expected failure of a trade the quote says is undefined.
fn expected_failure(before: &pump::BondingCurve, kind: PumpV2, amount: u64) -> Option<u32> {
    if before.complete {
        return Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE);
    }
    match kind {
        PumpV2::Buy if amount == 0 => Some(fork::PUMP_ERR_BUY_ZERO_AMOUNT),
        PumpV2::Buy if amount > before.real_token_reserves => {
            Some(fork::PUMP_ERR_NOT_ENOUGH_TOKENS_TO_BUY)
        }
        _ => None,
    }
}

/// An exact-out buy of `q` checked end to end (module docs). Returns whether it executed.
#[track_caller]
fn exact_buy(env: &mut Env, c: &PumpCurve, u: &User, q: u64, k: u64) -> bool {
    let ctx = format!("{:?} buy_v2 q {q}", c.coin);
    let before = c.state(&env.svm);
    let Some(ch) = c.charge_buy(&env.svm, q) else {
        let saved = env.svm.clone();
        let res = trade(env, c, u, PumpV2::Buy, q, u64::MAX, k);
        assert!(
            res.is_err(),
            "{ctx}: no charge, but the venue {}",
            describe(&res)
        );
        if let Some(want) = expected_failure(&before, PumpV2::Buy, q) {
            assert_eq!(custom_code(&res), Some(want), "{ctx}: {}", describe(&res));
        }
        env.svm = saved;
        return false;
    };
    let fees = c.fees(&env.svm).expect("fees");
    // short.win's own quote (ECON §3.1) covers the charge, by at most 1 + #fees lamports.
    let quote = c.quote_buy(&env.svm, q).expect("quote");
    let n_fees = [fees.protocol, fees.creator]
        .iter()
        .filter(|&&b| b > 0)
        .count() as u64;
    assert!(
        quote.cost >= ch.cost && quote.cost <= ch.cost + 1 + n_fees,
        "{ctx}: quote {} vs charge {}",
        quote.cost,
        ch.cost
    );
    let protocol = fee_up(ch.net, fees.protocol);
    let creator = fee_up(ch.net, fees.creator);
    assert_eq!(ch.cost, ch.net + protocol + creator);

    // One lamport under the charge: TooMuchSolRequired, nothing moves.
    let res = trade(env, c, u, PumpV2::Buy, q, ch.cost - 1, k);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_TOO_MUCH_SOL_REQUIRED),
        "{ctx}: one lamport under the charge; {}",
        describe(&res)
    );

    let (len_before, buckets_before) = (c.data_len(&env.svm), buckets(&env.svm, c));
    let b0 = balances(&env.svm, c, u, k);
    let top_up = creator_top_up(&env.svm, b0.creator_vault, creator);
    let meta = ok(trade(env, c, u, PumpV2::Buy, q, ch.cost, k), &ctx);
    let b1 = balances(&env.svm, c, u, k);
    let after = c.state(&env.svm);
    let ev = one_trade(&meta);
    let len_after = c.data_len(&env.svm);
    let curve_top_up = curve_growth(
        &env.svm,
        &ctx,
        len_before,
        len_after,
        b0.curve,
        before.real_quote_reserves,
        buckets_before,
    );
    assert_eq!(
        b1.curve - b0.curve,
        ch.net + curve_top_up,
        "{ctx}: curve lamports"
    );
    let buyback = buyback_share(&env.svm, protocol);
    assert_eq!(
        b0.user - b1.user,
        ch.cost + top_up + curve_top_up,
        "{ctx}: user paid"
    );
    assert_eq!(b1.user_tokens - b0.user_tokens, q, "{ctx}: user received");
    assert_eq!(b0.curve_tokens - b1.curve_tokens, q, "{ctx}: curve tokens");
    assert_eq!(
        b1.fee_recipient - b0.fee_recipient,
        protocol - buyback,
        "{ctx}: fee recipient"
    );
    assert_eq!(b1.buyback - b0.buyback, buyback, "{ctx}: buyback recipient");
    assert_eq!(
        b1.creator_vault - b0.creator_vault,
        creator + top_up,
        "{ctx}: creator vault"
    );
    assert_eq!(
        (
            ev.sol_amount,
            ev.token_amount,
            ev.fee,
            ev.creator_fee,
            ev.buyback_fee
        ),
        (ch.net, q, protocol, creator, buyback),
        "{ctx}: TradeEvent"
    );
    assert!(ev.is_buy && ev.ix_name == "buy" && ev.mint == c.mint && ev.user == u.kp.pubkey());
    assert_eq!(
        ev.fee_basis_points, fees.protocol,
        "{ctx}: event protocol bps"
    );
    check_post_state(
        &ctx,
        &after,
        &before,
        &ev,
        (
            u64::try_from(ch.token_after).unwrap(),
            u64::try_from(ch.sol_after).unwrap(),
            before.real_token_reserves - q,
            before.real_quote_reserves + ch.net,
        ),
    );
    true
}

/// An exact-in sale of `q` checked end to end. The user's balance is planted if short (these
/// mints have no mint authority; pump reads only the account). Returns whether it executed.
#[track_caller]
fn exact_sell(env: &mut Env, c: &PumpCurve, u: &User, q: u64, k: u64) -> bool {
    let ctx = format!("{:?} sell_v2 q {q}", c.coin);
    let have = read::token_amount(&env.svm, &u.ata).unwrap_or(0);
    if have < q {
        fork::set_token_amount(&mut env.svm, &u.ata, q);
    }
    let before = c.state(&env.svm);
    let Some(sq) = c.quote_sell(&env.svm, q) else {
        let saved = env.svm.clone();
        let res = trade(env, c, u, PumpV2::Sell, q, 0, k);
        assert!(
            res.is_err(),
            "{ctx}: no quote, but the venue {}",
            describe(&res)
        );
        if let Some(want) = expected_failure(&before, PumpV2::Sell, q) {
            assert_eq!(custom_code(&res), Some(want), "{ctx}: {}", describe(&res));
        }
        env.svm = saved;
        return false;
    };
    let fees = c.fees(&env.svm).expect("fees");
    let g = before.virtual_quote_reserves - u64::try_from(sq.sol_after).unwrap();
    let protocol = fee_up(g, fees.protocol);
    let creator = fee_up(g, fees.creator);
    assert_eq!(sq.proceeds, g - protocol - creator);

    let res = trade(env, c, u, PumpV2::Sell, q, sq.proceeds + 1, k);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_TOO_LITTLE_SOL_RECEIVED),
        "{ctx}: one lamport over the proceeds; {}",
        describe(&res)
    );

    let (len_before, buckets_before) = (c.data_len(&env.svm), buckets(&env.svm, c));
    let b0 = balances(&env.svm, c, u, k);
    let top_up = creator_top_up(&env.svm, b0.creator_vault, creator);
    let meta = ok(trade(env, c, u, PumpV2::Sell, q, sq.proceeds, k), &ctx);
    let b1 = balances(&env.svm, c, u, k);
    let after = c.state(&env.svm);
    let ev = one_trade(&meta);
    let curve_top_up = curve_growth(
        &env.svm,
        &ctx,
        len_before,
        c.data_len(&env.svm),
        b0.curve,
        before.real_quote_reserves,
        buckets_before,
    );
    assert_eq!(
        b1.curve + g,
        b0.curve + curve_top_up,
        "{ctx}: curve lamports"
    );
    let buyback = buyback_share(&env.svm, protocol);
    assert_eq!(
        i128::from(b1.user) - i128::from(b0.user),
        i128::from(sq.proceeds) - i128::from(top_up) - i128::from(curve_top_up),
        "{ctx}: user received"
    );
    assert_eq!(b0.user_tokens - b1.user_tokens, q, "{ctx}: user sold");
    assert_eq!(b1.curve_tokens - b0.curve_tokens, q, "{ctx}: curve tokens");
    assert_eq!(
        b1.fee_recipient - b0.fee_recipient,
        protocol - buyback,
        "{ctx}: fee recipient"
    );
    assert_eq!(b1.buyback - b0.buyback, buyback, "{ctx}: buyback recipient");
    assert_eq!(
        b1.creator_vault - b0.creator_vault,
        creator + top_up,
        "{ctx}: creator vault"
    );
    assert_eq!(
        (
            ev.sol_amount,
            ev.token_amount,
            ev.fee,
            ev.creator_fee,
            ev.buyback_fee
        ),
        (g, q, protocol, creator, buyback),
        "{ctx}: TradeEvent"
    );
    assert!(!ev.is_buy && ev.ix_name == "sell");
    check_post_state(
        &ctx,
        &after,
        &before,
        &ev,
        (
            u64::try_from(sq.token_after).unwrap(),
            u64::try_from(sq.sol_after).unwrap(),
            before.real_token_reserves + q,
            before.real_quote_reserves - g,
        ),
    );
    true
}

/// A `buy_exact_quote_in_v2(spendable, min_tokens_out)` checked end to end. Returns whether it
/// executed.
#[track_caller]
fn exact_quote_in(env: &mut Env, c: &PumpCurve, u: &User, spendable: u64, k: u64) -> bool {
    let ctx = format!("{:?} buy_exact_quote_in_v2 spendable {spendable}", c.coin);
    let before = c.state(&env.svm);
    let Some(r) = c.quote_exact_quote_in(&env.svm, spendable) else {
        let saved = env.svm.clone();
        let res = trade(env, c, u, PumpV2::BuyExactQuoteIn, spendable, 0, k);
        assert!(
            res.is_err(),
            "{ctx}: no quote, but the venue {}",
            describe(&res)
        );
        if before.complete {
            assert_eq!(
                custom_code(&res),
                Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE),
                "{ctx}"
            );
        }
        env.svm = saved;
        return false;
    };
    assert!(r.cost() <= spendable, "{ctx}: never more than spendable");
    // shortwin-math's exact-out charge brackets the tokens: n(t) ≤ net ≤ n(t + 1).
    let fees = c.fees(&env.svm).expect("fees");
    let (vt, vq) = (
        u128::from(before.virtual_token_reserves),
        u128::from(before.virtual_quote_reserves),
    );
    if r.tokens > 0 {
        let lo = sol_leg::curve_charge_exact_out(r.tokens, vt, vq, &fees).expect("n(t)");
        assert!(lo.net <= r.net, "{ctx}: n(t) {} > net {}", lo.net, r.net);
    }
    if let Some(hi) = sol_leg::curve_charge_exact_out(r.tokens + 1, vt, vq, &fees) {
        assert!(
            hi.net >= r.net,
            "{ctx}: n(t + 1) {} < net {}",
            hi.net,
            r.net
        );
    }

    let res = trade(
        env,
        c,
        u,
        PumpV2::BuyExactQuoteIn,
        spendable,
        r.tokens + 1,
        k,
    );
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_BUY_SLIPPAGE_BELOW_MIN_TOKENS_OUT),
        "{ctx}: one token over the output; {}",
        describe(&res)
    );

    let (len_before, buckets_before) = (c.data_len(&env.svm), buckets(&env.svm, c));
    let b0 = balances(&env.svm, c, u, k);
    let top_up = creator_top_up(&env.svm, b0.creator_vault, r.creator_fee);
    let meta = ok(
        trade(env, c, u, PumpV2::BuyExactQuoteIn, spendable, r.tokens, k),
        &ctx,
    );
    let b1 = balances(&env.svm, c, u, k);
    let after = c.state(&env.svm);
    let ev = one_trade(&meta);
    let curve_top_up = curve_growth(
        &env.svm,
        &ctx,
        len_before,
        c.data_len(&env.svm),
        b0.curve,
        before.real_quote_reserves,
        buckets_before,
    );
    assert_eq!(
        b1.curve - b0.curve,
        r.net + curve_top_up,
        "{ctx}: curve lamports"
    );
    let buyback = buyback_share(&env.svm, r.protocol_fee);
    assert_eq!(
        b0.user - b1.user,
        r.cost() + top_up + curve_top_up,
        "{ctx}: user paid"
    );
    assert_eq!(
        b1.user_tokens - b0.user_tokens,
        r.tokens,
        "{ctx}: user received"
    );
    assert_eq!(
        b1.fee_recipient - b0.fee_recipient,
        r.protocol_fee - buyback,
        "{ctx}: fee recipient"
    );
    assert_eq!(b1.buyback - b0.buyback, buyback, "{ctx}: buyback recipient");
    assert_eq!(
        b1.creator_vault - b0.creator_vault,
        r.creator_fee + top_up,
        "{ctx}: creator vault"
    );
    assert_eq!(
        (
            ev.sol_amount,
            ev.token_amount,
            ev.fee,
            ev.creator_fee,
            ev.buyback_fee
        ),
        (r.net, r.tokens, r.protocol_fee, r.creator_fee, buyback),
        "{ctx}: TradeEvent"
    );
    assert!(ev.is_buy && ev.ix_name == "buy_exact_quote_in");
    check_post_state(
        &ctx,
        &after,
        &before,
        &ev,
        (
            before.virtual_token_reserves - r.tokens,
            before.virtual_quote_reserves + r.net,
            before.real_token_reserves - r.tokens,
            before.real_quote_reserves + r.net,
        ),
    );
    true
}

// ---- grids ----------------------------------------------------------------------------------------

const BUYS: [u64; 14] = [
    1,
    2,
    10,
    1_000,
    1_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    5_000_000_000_000,
    10_000_000_000_000,
    30_000_000_000_000,
    100_000_000_000_000,
    300_000_000_000_000,
];

const SELLS: [u64; 12] = [
    1,
    10,
    1_000,
    1_000_000,
    1_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    5_000_000_000_000,
    10_000_000_000_000,
    30_000_000_000_000,
    100_000_000_000_000,
    300_000_000_000_000,
];

const SPENDS: [u64; 12] = [
    1,
    2,
    3,
    10,
    100,
    10_000,
    1_000_000,
    100_000_000,
    1_000_000_000,
    5_000_000_000,
    20_000_000_000,
    60_000_000_000,
];

/// Buys, then sells, then exact-quote-in buys over the grids on `coin`'s live curve; returns
/// how many executed.
fn grid(coin: PumpCoin) -> usize {
    let (mut env, c) = Env::fork_pump_coin(coin);
    let u = user(&mut env, &c);
    let mut executed = 0;
    for (k, q) in BUYS.iter().enumerate() {
        executed += usize::from(exact_buy(&mut env, &c, &u, *q, k as u64));
    }
    for (k, q) in SELLS.iter().enumerate() {
        executed += usize::from(exact_sell(&mut env, &c, &u, *q, 3 + k as u64));
    }
    for (k, s) in SPENDS.iter().enumerate() {
        executed += usize::from(exact_quote_in(&mut env, &c, &u, *s, 7 + k as u64));
    }
    executed
}

#[test]
fn fresh_125_byte_curve() {
    assert!(grid(PumpCoin::Fresh) >= 30);
}

#[test]
fn near_completion_151_byte_curve() {
    assert!(grid(PumpCoin::Near) >= 25);
}

#[test]
fn token_2022_115_byte_curve() {
    assert!(grid(PumpCoin::T22_115) >= 30);
}

#[test]
fn legacy_spl_115_byte_curve_without_a_creator() {
    let (env, c) = Env::fork_pump_coin(PumpCoin::Legacy115);
    assert_eq!(c.token_program.id(), shortwin::constants::TOKEN_PROGRAM_ID);
    assert!(c.state(&env.svm).creator_is_default());
    assert_eq!(c.fees(&env.svm).unwrap().creator, 0);
    drop(env);
    assert!(grid(PumpCoin::Legacy115) >= 30);
}

/// The 2024 layout: the first v2 trade grows the curve to the current layout — 166 bytes since
/// the 2026-10-08 redeploy (141 on the 2026-10-02 build, 125 before; the fields past the old end
/// zero) —, the user paying its rent reserve up to `minimum_balance(166)` = 1,493,520: 261,600
/// lamports here (134,600 to 141 bytes, 53,320 to 125), since the reserve was the old rent of 49
/// bytes, not the (166 − 49)·5,080 = 594,360 of a reserve at today's rent —, and every trade
/// after it is exact.
#[test]
fn legacy_spl_49_byte_curve_grows_to_the_current_layout() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Legacy49);
    assert_eq!(c.data_len(&env.svm), pump::CURVE_LEN_ORIGINAL);
    let s = c.state(&env.svm);
    let reserve = read::lamports(&env.svm, &c.curve) - s.real_quote_reserves;
    assert_eq!(
        reserve, 1_231_920,
        "the rent of 49 bytes at the old 6,960 lamports per byte"
    );
    let grows_to = PumpDeploy::CURRENT.curve_len();
    assert_eq!(grows_to, fork::CURVE_LEN_2026_10_08);
    let u = user(&mut env, &c);
    let u0 = read::lamports(&env.svm, &u.kp.pubkey());
    let ch = c.charge_buy(&env.svm, 1_000_000_000_000).unwrap();
    assert!(exact_buy(&mut env, &c, &u, 1_000_000_000_000, 0));
    assert_eq!(c.data_len(&env.svm), grows_to);
    assert_eq!(
        u0 - read::lamports(&env.svm, &u.kp.pubkey()),
        ch.cost + 261_600
    );
    let acc = env.svm.get_account(&c.curve).unwrap();
    assert!(acc.data[pump::CURVE_LEN_ORIGINAL..].iter().all(|&b| b == 0));
    assert_eq!(
        acc.lamports - c.state(&env.svm).real_quote_reserves,
        env.svm.minimum_balance_for_rent_exemption(grows_to)
    );
    drop(env);
    assert!(grid(PumpCoin::Legacy49) >= 30);
}

/// A curve shorter than 166 bytes that carries fee buckets (the 2026-10-02 build's v3 trades
/// left them on 141- and 151-byte curves: the 2026-10-03 census, README, counted 627 and 904,
/// every one quoted in another token than SOL; a 2026-10-13 sample of 15 live SOL curves of those
/// lengths held none, which proves nothing for the rest) grows on its next v2 or v3 trade, and
/// pump counts its rent reserve without the buckets: the user tops `lamports − rQ − creator_fee −
/// protocol_fees` up to `minimum_balance(166)`, as the harness's one model of the growth
/// (`fork::curve_growth_top_up`: `PumpCurve::growth_top_up`, the I13 hook's `venue_rent`, and
/// this file's `curve_growth` and `growth`) has it. With the reserve planted at
/// `minimum_balance(len)` and unequal buckets on top, below the shortfall, this is told apart
/// from a reserve counted with both buckets, or without only one of them; such a curve pays the
/// §3.5 table's amount for its length, as a curve without buckets does.
#[test]
fn a_bucketed_short_curve_grows_with_its_reserve_net_of_the_buckets() {
    const CREATOR_FEE: u64 = 20_000;
    const PROTOCOL_FEES: u64 = 30_000;
    for coin in [PumpCoin::Grow141, PumpCoin::Grow151] {
        for v3 in [false, true] {
            let ctx = format!("{coin:?} {}", if v3 { "buy_v3" } else { "buy_v2" });
            let (mut env, c) = Env::fork_pump_coin(coin);
            let len0 = c.data_len(&env.svm);
            assert!(len0 < fork::CURVE_LEN_2026_10_08, "{ctx}");
            assert_eq!(buckets(&env.svm, &c), (0, 0), "{ctx}: none at the dump");
            // The reserve at its length's rent (the dumped 151-byte curve holds more), the
            // buckets on top of it.
            let st = c.state(&env.svm);
            let mut acc = env.svm.get_account(&c.curve).unwrap();
            let dumped = acc.lamports - st.real_quote_reserves;
            let reserve = env.svm.minimum_balance_for_rent_exemption(len0);
            acc.data[125..133].copy_from_slice(&CREATOR_FEE.to_le_bytes());
            acc.data[133..141].copy_from_slice(&PROTOCOL_FEES.to_le_bytes());
            acc.lamports = st.real_quote_reserves + reserve + CREATOR_FEE + PROTOCOL_FEES;
            env.svm.set_account(c.curve, acc).unwrap();
            let lam0 = read::lamports(&env.svm, &c.curve);
            let need = env
                .svm
                .minimum_balance_for_rent_exemption(fork::CURVE_LEN_2026_10_08);
            assert!(
                need > reserve + CREATOR_FEE + PROTOCOL_FEES,
                "{ctx}: the buckets sit below the shortfall"
            );
            let model = c.growth_top_up(&env.svm);
            assert_eq!(model, need - reserve, "{ctx}: the harness's model");
            let u = CurveUser::new(&mut env, &c);
            let vault = c.creator_vault(&env.svm);
            let (user0, vault0) = (
                read::lamports(&env.svm, &u.kp.pubkey()),
                read::lamports(&env.svm, &vault),
            );
            let q = st.real_token_reserves / 100;
            let meta = if v3 {
                let ix = u.buy_v3_ix(&env.svm, &c, q, u64::MAX);
                ok(u.send(&mut env, ix), &ctx)
            } else {
                ok(u.buy(&mut env, &c, q, u64::MAX), &ctx)
            };
            let ev = one_trade(&meta);
            let st1 = c.state(&env.svm);
            let b1 = buckets(&env.svm, &c);
            assert_eq!(c.data_len(&env.svm), fork::CURVE_LEN_2026_10_08, "{ctx}");
            // The curve's side: what it gained beyond `rQ` and the buckets.
            let top_up = read::lamports(&env.svm, &c.curve)
                - lam0
                - (st1.real_quote_reserves - st.real_quote_reserves)
                - (b1.0 + b1.1 - CREATOR_FEE - PROTOCOL_FEES);
            // The user's side: v2 pays the fees out (the creator's into its vault, topped up to
            // rent if short), v3 into the buckets.
            let paid = user0 - read::lamports(&env.svm, &u.kp.pubkey());
            let fees_out = if v3 {
                ev.fee + ev.creator_fee
            } else {
                ev.fee + (read::lamports(&env.svm, &vault) - vault0)
            };
            println!(
                "{ctx}: {len0} → 166 bytes, buckets ({CREATOR_FEE}, {PROTOCOL_FEES}), reserve \
                 {reserve} (dumped {dumped}): the user topped up {top_up} (paid {paid} = n {} + \
                 fees {fees_out} + growth); without the buckets {}, with both {}, with the \
                 protocol fees only {}, with the creator fee only {}",
                ev.sol_amount,
                need - reserve,
                need - reserve - CREATOR_FEE - PROTOCOL_FEES,
                need - reserve - CREATOR_FEE,
                need - reserve - PROTOCOL_FEES
            );
            assert_eq!(paid - ev.sol_amount - fees_out, top_up, "{ctx}: user side");
            assert_eq!(top_up, need - reserve, "{ctx}: reserve net of the buckets");
            assert_eq!(
                read::lamports(&env.svm, &c.curve) - st1.real_quote_reserves - b1.0 - b1.1,
                need,
                "{ctx}: the reserve is minimum_balance(166) after"
            );
        }
    }
}

// ---- beyond rT, completion, complete curves ----------------------------------------------------

/// A buy beyond `rT` reverts (6021, no cap, no partial fill) with `buy_v2` and with
/// `buy_exact_quote_in_v2`; the buy of exactly `rT` completes the curve at the exact-out price;
/// then every trade reverts with 6005 (R-PUMP §7.2–§7.3).
#[test]
fn beyond_rt_reverts_and_the_completing_buy_completes() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    let u = user(&mut env, &c);
    let rt = c.state(&env.svm).real_token_reserves;
    for q in [rt + 1, rt + 1_000_000_000_000, 2 * rt] {
        assert!(!exact_buy(&mut env, &c, &u, q, 1), "q {q} > rT {rt}");
    }
    // Enough SOL for twice the curve: the exact-in buy asks for more than rT.
    let need = c.charge_buy(&env.svm, rt).unwrap().cost;
    let res = trade(&mut env, &c, &u, PumpV2::BuyExactQuoteIn, 3 * need, 1, 2);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_NOT_ENOUGH_TOKENS_TO_BUY),
        "{}",
        describe(&res)
    );
    assert!(c.quote_exact_quote_in(&env.svm, 3 * need).is_none());
    // The completing buy.
    assert!(exact_buy(&mut env, &c, &u, rt, 3));
    let s = c.state(&env.svm);
    assert!(s.complete && s.is_migrating() && s.real_token_reserves == 0);
    assert_eq!(
        read::token_amount(&env.svm, &c.curve_base_ata),
        Some(206_900_000_000_000)
    );
    // Complete: every trade reverts with 6005, whatever its size.
    for q in [1, 1_000_000, rt] {
        assert!(!exact_buy(&mut env, &c, &u, q, 4));
        assert!(!exact_sell(&mut env, &c, &u, q, 5));
    }
    assert!(!exact_quote_in(&mut env, &c, &u, 1_000_000_000, 6));
    for kind in [PumpV2::Buy, PumpV2::Sell, PumpV2::BuyExactQuoteIn] {
        let res = trade(
            &mut env,
            &c,
            &u,
            kind,
            1_000,
            if kind == PumpV2::Buy { u64::MAX } else { 0 },
            7,
        );
        assert_eq!(
            custom_code(&res),
            Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE),
            "{kind:?}: {}",
            describe(&res)
        );
    }
}

/// The dump's migrated curves (PumpSwap graduates old and new, and Fartcoin, which graduated to
/// Raydium) refuse every trade with 6005.
#[test]
fn migrated_curves_refuse_every_trade() {
    for coin in [PumpCoin::Graduated, PumpCoin::Legacy150, PumpCoin::Fartcoin] {
        let (mut env, c) = Env::fork_pump_coin(coin);
        assert!(c.state(&env.svm).is_migrated());
        assert!(c.charge_buy(&env.svm, 1).is_none() && c.quote_sell(&env.svm, 1).is_none());
        let u = user(&mut env, &c);
        fork::set_token_amount(&mut env.svm, &u.ata, 1_000_000);
        for kind in [PumpV2::Buy, PumpV2::Sell, PumpV2::BuyExactQuoteIn] {
            let bound = if kind == PumpV2::Buy { u64::MAX } else { 0 };
            let res = trade(&mut env, &c, &u, kind, 1_000, bound, 0);
            assert_eq!(
                custom_code(&res),
                Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE),
                "{coin:?} {kind:?}: {}",
                describe(&res)
            );
        }
    }
}

/// The undocumented trailing byte of R-PUMP Update 2026-09-29 U7 on `buy_v2`: absent or 0 keeps
/// the 6021 revert; 1 fills exactly `rT` at the exact-out charge of `rT` and completes the
/// curve; 2 does not deserialise. short.win never sets it (the protocol owes exactly `Q`).
#[test]
fn the_trailing_cap_byte_fills_exactly_rt() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    let u = user(&mut env, &c);
    let rt = c.state(&env.svm).real_token_reserves;
    let ch = c.charge_buy(&env.svm, rt).unwrap();
    let q = rt + 1_000_000_000_000;
    let (fr, bb) = recipients(&env.svm, 0);
    let base = c.v2_ix(
        &env.svm,
        PumpV2::Buy,
        &u.kp.pubkey(),
        &u.ata,
        &fr,
        &bb,
        q,
        u64::MAX,
    );
    let ix = |flag: Option<u8>| {
        let mut ix = base.clone();
        ix.data.extend(flag);
        ix
    };
    let signer = u.kp.insecure_clone();
    for flag in [None, Some(0u8)] {
        let res = env.send(&[ix(flag)], &[&signer]);
        assert_eq!(
            custom_code(&res),
            Some(fork::PUMP_ERR_NOT_ENOUGH_TOKENS_TO_BUY),
            "{flag:?}: {}",
            describe(&res)
        );
    }
    let res = env.send(&[ix(Some(2))], &[&signer]);
    assert_eq!(
        custom_code(&res),
        Some(102),
        "InstructionDidNotDeserialize: {}",
        describe(&res)
    );
    let before = c.state(&env.svm);
    let u0 = read::lamports(&env.svm, &u.kp.pubkey());
    let (len0, curve0, buckets0) = (
        c.data_len(&env.svm),
        read::lamports(&env.svm, &c.curve),
        buckets(&env.svm, &c),
    );
    let meta = ok(env.send(&[ix(Some(1))], &[&signer]), "capped buy");
    let ev = one_trade(&meta);
    let after = c.state(&env.svm);
    assert_eq!((ev.token_amount, ev.sol_amount), (rt, ch.net));
    // The 151-byte curve grows to 166 on the 2026-10-08 build (76,200 lamports from the user).
    let growth = curve_growth(
        &env.svm,
        "capped buy",
        len0,
        c.data_len(&env.svm),
        curve0,
        before.real_quote_reserves,
        buckets0,
    );
    assert_eq!(
        u0 - read::lamports(&env.svm, &u.kp.pubkey()),
        ch.cost + growth
    );
    assert!(after.complete && after.real_token_reserves == 0);
    assert_eq!(
        after.virtual_quote_reserves,
        before.virtual_quote_reserves + ch.net
    );
}

// ---- fee tiers through the real pump-fees program ------------------------------------------------

/// The market cap pump-fees selects a curve's tier by: `⌊vQ·10¹⁵/vT⌋` (R-PUMP §4 step 1).
fn mcap(c: &pump::BondingCurve) -> u128 {
    u128::from(c.virtual_quote_reserves) * mfees::PUMP_CURVE_MCAP_SUPPLY
        / u128::from(c.virtual_token_reserves)
}

/// The market cap the **deployed** pump-fees resolves a curve's tier at: `⌊vQ·supply/vT⌋` with the
/// mint's **live** supply (burns lower it), not the fixed 10¹⁵ of R-PUMP §4 step 1 / ARCH §4.2 /
/// `fees::pump_curve` (see `tripwire_the_curve_tier_market_cap_uses_the_live_mint_supply`).
fn mcap_live(svm: &LiteSVM, c: &PumpCurve) -> u128 {
    let s = c.state(svm);
    let mint = svm.get_account(&c.mint).unwrap();
    let supply = shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &mint.data)
        .unwrap()
        .supply;
    u128::from(s.virtual_quote_reserves) * u128::from(supply) / u128::from(s.virtual_token_reserves)
}

/// A buy, a sale and an exact-in buy on each live curve, under `FeeConfig` tiers rewritten as the
/// admin's `update_fee_config` would leave them, are charged exactly the tier
/// `fees::pump_curve` selects: PumpSwap's 25 live market-cap tiers (tier 0 carries an LP fee the
/// curve never charges), thresholds at the curve's own market cap and one above it, an unsorted
/// list ("the last tier whose threshold is ≤ mcap"), a tier above the market cap only (tier 0
/// applies), and a 3.82 % schedule. On a coin whose supply was burned the thresholds bracket both
/// market caps (fixed-supply and live-supply), the band between them being the tripwire's.
#[test]
fn fee_tiers_as_the_fee_program_resolves_them() {
    let pamm_tiers = {
        let (env, _) = Env::fork_pump_coin(PumpCoin::Fresh);
        fork::fee_config_specs(&env.svm, &PUMP_AMM_FEE_CONFIG).1
    };
    for coin in [
        PumpCoin::Fresh,
        PumpCoin::Near,
        PumpCoin::T22_115,
        PumpCoin::Legacy115,
    ] {
        let schedules: Vec<Vec<fork::TierSpec>> = {
            let (env, c) = Env::fork_pump_coin(coin);
            let (a, b) = (mcap(&c.state(&env.svm)), mcap_live(&env.svm, &c));
            let (lo, hi) = (a.min(b), a.max(b));
            vec![
                pamm_tiers.clone(),
                vec![(0, [0, 95, 30]), (lo, [0, 40, 10]), (hi + 1, [0, 10, 3])],
                vec![(0, [0, 95, 30]), (hi + 1, [0, 40, 10])],
                vec![
                    (lo / 2, [0, 70, 20]),
                    (lo - 1, [0, 50, 0]),
                    (u128::MAX, [0, 1, 1]),
                ],
                vec![(hi * 2, [0, 120, 45])],
                vec![(0, [7, 250, 125])],
            ]
        };
        for tiers in schedules {
            let (mut env, c) = Env::fork_pump_coin(coin);
            fork::set_fee_config(
                &mut env.svm,
                &PUMP_FEE_CONFIG,
                [0, 95, 30],
                &tiers,
                &tiers,
                [0, 95, 30],
            );
            let fees = c.fees(&env.svm).unwrap();
            let s = c.state(&env.svm);
            let math_tiers: Vec<MathFeeTier> = tiers
                .iter()
                .map(|(t, f)| MathFeeTier {
                    threshold: *t,
                    fees: FeeBps::new(f[0], f[1], f[2]),
                })
                .collect();
            let tier = mfees::fee_tier(&math_tiers, mcap(&s)).unwrap();
            let creator = if s.creator_is_default() {
                0
            } else {
                tier.creator
            };
            assert_eq!(
                fees,
                FeeBps::new(0, tier.protocol, creator),
                "{coin:?} {tiers:?}"
            );
            // The reader's streamed selection is the same tier.
            let fc = env.svm.get_account(&PUMP_FEE_CONFIG).unwrap();
            let rc = pump::read_fee_config(&fc.owner.to_bytes(), &fc.data).unwrap();
            let sel = rc.fee_tiers.select(mcap(&s)).unwrap();
            assert_eq!(
                (sel.lp, sel.protocol, sel.creator),
                (tier.lp, tier.protocol, tier.creator)
            );
            let u = user(&mut env, &c);
            assert!(
                exact_buy(&mut env, &c, &u, 2_000_000_000_000, 1),
                "{coin:?} {tiers:?}"
            );
            assert!(
                exact_sell(&mut env, &c, &u, 1_000_000_000_000, 2),
                "{coin:?} {tiers:?}"
            );
            assert!(
                exact_quote_in(&mut env, &c, &u, 300_000_000, 3),
                "{coin:?} {tiers:?}"
            );
        }
    }
}

/// **Tripwire (a shortwin-math / ARCH §4.2 deviation found here; M5 design note).** The deployed
/// pump resolves the curve's fee tier at `⌊vQ·mint.supply/vT⌋` with the mint's live supply, while
/// `fees::pump_curve` (ARCH §4.2, R-PUMP §4 step 1) uses a fixed 10¹⁵. The two differ only on a
/// coin whose supply was burned (the near-completion coin: 999,978,034,493,483), inside the band
/// `[mcap_live, mcap_fixed)` of a tier threshold. With the single live tier (threshold 0) no
/// quote differs today. A threshold in the band: the ELF charges the lower tier, the math the
/// upper. When the math takes the live supply, the second assertion flips.
#[test]
fn tripwire_the_curve_tier_market_cap_uses_the_live_mint_supply() {
    for coin in [PumpCoin::Near, PumpCoin::Legacy115] {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let (fixed, live) = (mcap(&c.state(&env.svm)), mcap_live(&env.svm, &c));
        assert!(live < fixed, "{coin:?}: a burned supply");
        let lower = [0, 95, 30];
        let upper = [0, 40, 10];
        let tiers = [(0, lower), (fixed, upper)];
        fork::set_fee_config(&mut env.svm, &PUMP_FEE_CONFIG, lower, &tiers, &tiers, lower);
        let creator = |bps: u64| {
            if c.state(&env.svm).creator_is_default() {
                0
            } else {
                bps
            }
        };
        // shortwin-math: the upper tier (fixed supply).
        assert_eq!(
            c.fees(&env.svm).unwrap(),
            FeeBps::new(0, 40, creator(10)),
            "{coin:?}"
        );
        // The ELF: the lower tier (live supply), charged exactly.
        let elf = FeeBps::new(0, 95, creator(30));
        let s = c.curve_state(&env.svm);
        let q = 1_000_000_000_000;
        let ch = sol_leg::curve_charge_exact_out(q, s.vt, s.vq, &elf).unwrap();
        let u = user(&mut env, &c);
        let res = trade(&mut env, &c, &u, PumpV2::Buy, q, ch.cost - 1, 0);
        assert_eq!(
            custom_code(&res),
            Some(fork::PUMP_ERR_TOO_MUCH_SOL_REQUIRED)
        );
        let u0 = read::lamports(&env.svm, &u.kp.pubkey());
        let (len0, curve0, rq0, buckets0) = (
            c.data_len(&env.svm),
            read::lamports(&env.svm, &c.curve),
            c.state(&env.svm).real_quote_reserves,
            buckets(&env.svm, &c),
        );
        let meta = ok(trade(&mut env, &c, &u, PumpV2::Buy, q, ch.cost, 0), "buy");
        // A curve under 166 bytes grows on the 2026-10-08 build (the user tops its rent up).
        let len1 = c.data_len(&env.svm);
        let growth = curve_growth(&env.svm, "buy", len0, len1, curve0, rq0, buckets0);
        assert_eq!(
            u0 - read::lamports(&env.svm, &u.kp.pubkey()),
            ch.cost + growth
        );
        let ev = one_trade(&meta);
        assert_eq!((ev.fee_basis_points, ev.sol_amount), (95, ch.net));
        // At a threshold ≤ the live cap both agree again.
        let (mut env, c) = Env::fork_pump_coin(coin);
        let tiers = [(0, lower), (live, upper)];
        fork::set_fee_config(&mut env.svm, &PUMP_FEE_CONFIG, lower, &tiers, &tiers, lower);
        let u = user(&mut env, &c);
        assert!(exact_buy(&mut env, &c, &u, q, 0), "{coin:?}");
    }
}

/// `creator = default` charges no creator fee whatever the tier says; `Global.creator_fee_
/// configurable ∧ curve.creator_fee_bps ≠ 0` replaces the tier's creator bps; with the flag off
/// the tier's applies (R-PUMP §4 steps 4–5; `fees::pump_curve`).
#[test]
fn creator_fee_rules() {
    // Default creator (the legacy coins): no creator fee even at a 125-bps tier.
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Legacy115);
    fork::set_fee_config(
        &mut env.svm,
        &PUMP_FEE_CONFIG,
        [0, 95, 30],
        &[(0, [0, 95, 125])],
        &[],
        [0, 95, 30],
    );
    assert_eq!(c.fees(&env.svm).unwrap(), FeeBps::new(0, 95, 0));
    let u = user(&mut env, &c);
    assert!(exact_buy(&mut env, &c, &u, 3_000_000_000_000, 0));
    assert!(exact_sell(&mut env, &c, &u, 1_000_000_000_000, 1));
    assert!(exact_quote_in(&mut env, &c, &u, 700_000_000, 2));

    // The per-coin override on a SOL curve (a byte-level state; create_v2 sets it only for
    // custom pairs), with the flag on and off.
    for configurable in [true, false] {
        let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
        fork::set_curve_creator_fee_bps(&mut env.svm, &c.curve, 200);
        fork::set_global_creator_fee_configurable(&mut env.svm, configurable);
        let want = if configurable { 200 } else { 30 };
        assert_eq!(c.fees(&env.svm).unwrap(), FeeBps::new(0, 95, want));
        let u = user(&mut env, &c);
        assert!(
            exact_buy(&mut env, &c, &u, 3_000_000_000_000, 0),
            "configurable {configurable}"
        );
        assert!(
            exact_sell(&mut env, &c, &u, 1_000_000_000_000, 1),
            "configurable {configurable}"
        );
        assert!(
            exact_quote_in(&mut env, &c, &u, 700_000_000, 2),
            "configurable {configurable}"
        );
    }
}

// ---- venue-side rent -----------------------------------------------------------------------------

/// A creator vault that does not exist is topped up to `Rent.minimum_balance(0)` (650,240 under
/// mainnet's Rent) from the user, on top of the creator fee — on a buy, and on a sale before
/// the proceeds are credited (R-PUMP Update 2026-09-29 U6; T-RENT, curve part). A funded vault
/// takes only the fee.
#[test]
fn an_empty_creator_vault_is_topped_up_by_the_user() {
    for kind in [PumpV2::Buy, PumpV2::Sell, PumpV2::BuyExactQuoteIn] {
        let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
        assert_eq!(
            env.svm.minimum_balance_for_rent_exemption(0),
            650_240,
            "mainnet Rent"
        );
        let u = user(&mut env, &c);
        // Some SOL in the curve for the sale.
        assert!(exact_buy(&mut env, &c, &u, 50_000_000_000_000, 0));
        let vault = c.creator_vault(&env.svm);
        env.svm
            .set_account(vault, solana_account::Account::default())
            .unwrap();
        assert_eq!(read::lamports(&env.svm, &vault), 0);
        let executed = match kind {
            PumpV2::Buy => exact_buy(&mut env, &c, &u, 10_000_000_000_000, 1),
            PumpV2::Sell => exact_sell(&mut env, &c, &u, 10_000_000_000_000, 1),
            PumpV2::BuyExactQuoteIn => exact_quote_in(&mut env, &c, &u, 400_000_000, 1),
        };
        assert!(executed, "{kind:?}");
        assert!(
            read::lamports(&env.svm, &vault) >= 650_240,
            "{kind:?}: the vault is rent-exempt"
        );
    }
}

// ---- recipients --------------------------------------------------------------------------------

/// pump enforces the live `Global` lists that short.win checks as well (C0-14): a fee recipient
/// outside the 8 normal ones (a reserved one, a random key) is `NotAuthorized` (6000), a buyback
/// recipient outside its list 6057; every listed recipient is accepted (the grids rotate them).
#[test]
fn recipients_outside_the_live_lists_are_refused() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
    let u = user(&mut env, &c);
    let ch = c.charge_buy(&env.svm, 1_000_000_000).unwrap();
    let g = env.svm.get_account(&PUMP_GLOBAL).unwrap().data;
    let global = pump::read_global(&pump::PROGRAM, &g).unwrap();
    let reserved = Pubkey::new_from_array(global.reserved_fee_recipients()[1]);
    let normal = fork::pump_fee_recipient(&env.svm, 0);
    let buyback = fork::pump_buyback_fee_recipient(&env.svm, 0);
    let signer = u.kp.insecure_clone();
    let cases = [
        (reserved, buyback, fork::PUMP_ERR_NOT_AUTHORIZED),
        (Pubkey::new_unique(), buyback, fork::PUMP_ERR_NOT_AUTHORIZED),
        (
            normal,
            normal,
            fork::PUMP_ERR_BUYBACK_FEE_RECIPIENT_NOT_AUTHORIZED,
        ),
        (
            normal,
            Pubkey::new_unique(),
            fork::PUMP_ERR_BUYBACK_FEE_RECIPIENT_NOT_AUTHORIZED,
        ),
    ];
    for (fr, bb, want) in cases {
        for (kind, a, b) in [
            (PumpV2::Buy, 1_000_000_000, ch.cost),
            (PumpV2::Sell, 1_000_000, 0),
        ] {
            if kind == PumpV2::Sell {
                fork::set_token_amount(&mut env.svm, &u.ata, 1_000_000);
            }
            let ix = c.v2_ix(&env.svm, kind, &u.kp.pubkey(), &u.ata, &fr, &bb, a, b);
            let res = env.send(&[ix], &[&signer]);
            assert_eq!(
                custom_code(&res),
                Some(want),
                "{kind:?} {fr} {bb}: {}",
                describe(&res)
            );
        }
    }
    for k in 0..64 {
        assert!(exact_buy(&mut env, &c, &u, 1_000_000_000 + k, k));
    }
}

// ---- random sequences ----------------------------------------------------------------------------

/// A seeded xorshift.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Log-uniform in `[1, max]`.
    fn amount(&mut self, max: u64) -> u64 {
        let bits = 1 + self.next() % u64::from(64 - max.leading_zeros());
        let v = self.next() & (u64::MAX >> (64 - bits));
        v.clamp(1, max)
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Seeded random buys, sells and exact-in buys on the evolving state of every live curve, each
/// checked end to end.
#[test]
fn random_sequences_on_every_live_curve() {
    let n = env_u64("PUMP_MATH_SEQ", 30);
    let seed = env_u64("PUMP_MATH_SEED", 0x5eed_c0de_0005);
    for (i, coin) in PumpCoin::LIVE_SOL.into_iter().enumerate() {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let u = user(&mut env, &c);
        let mut rng = Rng(seed ^ (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut executed = 0;
        for k in 0..n {
            let s = c.state(&env.svm);
            if s.complete {
                break;
            }
            executed += usize::from(match rng.next() % 3 {
                0 => {
                    let q = rng.amount(s.real_token_reserves.clamp(1, 400_000_000_000_000));
                    exact_buy(&mut env, &c, &u, q, k)
                }
                1 => exact_sell(&mut env, &c, &u, rng.amount(400_000_000_000_000), k),
                _ => exact_quote_in(&mut env, &c, &u, rng.amount(40_000_000_000), k),
            });
        }
        println!("{coin:?}: {executed} of {n} random trades executed");
        assert!(
            executed * 2 >= usize::try_from(n).unwrap(),
            "{coin:?}: {executed} of {n}"
        );
    }
}

// ---- fixtures, programs, readers ---------------------------------------------------------------------

/// The dumped programs are mainnet's, in the four deploys the fixtures hold: the 2026-09 deploys
/// ARCH §8.2 names (deploy slots = `VENUE_GUARD_SLOTS_2026_09_27`), the 2026-10-02 redeploys
/// (`VENUE_GUARD_SLOTS_2026_10_03`; both kept for T-LATCH), the 2026-10-08/12 redeploys (pump's
/// 2026-10-08 build kept for the differential) and pump's 2026-10-13 redeploy with PumpSwap's
/// 2026-10-08 and pump-fees' 2026-10-12 builds, which every fork loads ([`PumpDeploy::CURRENT`],
/// slots = [`fork::FIXTURE_GUARD_SLOTS`] = `VENUE_GUARD_SLOTS_2026_10_13`'s): trimmed-ELF sha256
/// as pinned by `scripts/dump-fixtures.py`, SBPF v0, one upgrade authority, ELF headers that tell
/// the builds apart ([`PumpDeploy::held`]), the ProgramData rebuilt to the mainnet sha256 when
/// loaded. Every
/// pump-state account of a group is one snapshot, the reader constants equal the program's, the
/// statics sit at their PDAs (pump's `Global` the 1,088-byte one of the 2026-10-08 redeploy:
/// `pump_state`'s 1,087 bytes, then `max_curve_depth` = 1), and every role of both state groups
/// holds what the dump script expects of it.
#[test]
fn fixtures_programs_and_readers() {
    let authority = Pubkey::from_str_const("7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8");
    let trimmed = |deploy, p| match (deploy, p) {
        (PumpDeploy::Sep2026, PumpProgram::Pump) => {
            "dbf5541c092438c2c9d19a278a3516114b88dfd7c8756730ac0d11046921c42f"
        }
        (PumpDeploy::Sep2026, PumpProgram::Fees) => {
            "1092b124324088948a419910e718ca31fb6479a8dab514adbf05a3a6ee2db788"
        }
        (PumpDeploy::Sep2026, PumpProgram::Amm) => {
            "c04bea8bee62502c62a9fa796fc99ce0d616469908b85595a517ade2bfe36522"
        }
        (PumpDeploy::Oct2026, PumpProgram::Pump) => {
            "b0b777c64ccf811ab21493d47d618b8db272f9bfd06b4fba4ba66ef565cb0c75"
        }
        (PumpDeploy::Oct2026, PumpProgram::Fees) => {
            "f166c2967fbfa6a9d80146360e5b79a2b263e821c999f1e74f266162788462fd"
        }
        (PumpDeploy::Oct2026, PumpProgram::Amm) => {
            "015570358c672ae42e5706daa87eb67d4c45e8fd8afec80c66bd85b9f9333cc3"
        }
        (PumpDeploy::Oct2026Synth, PumpProgram::Pump) => {
            "6956c2f83297b6f7f06bbe8033241589c39da9650597dc5c59fb5a431906bf6f"
        }
        (PumpDeploy::Oct2026Synth | PumpDeploy::Oct2026Chain, PumpProgram::Fees) => {
            "8ad2ca93c7ae97a7fb0de8a99de4d8825ccfdf89f73138cd49bda948e124b8b8"
        }
        (PumpDeploy::Oct2026Synth | PumpDeploy::Oct2026Chain, PumpProgram::Amm) => {
            "3ffd5e3b5a31ccc86b434405e8a1876926da1e8685f891dc964b7f919c172ee1"
        }
        (PumpDeploy::Oct2026Chain, PumpProgram::Pump) => {
            "e202096978d8f20935153a4271938fd9f0a421d64c1749986b4c5c20ca2a52be"
        }
    };
    // The 2026-10-08/12 deploy: today's slots but pump's 2026-10-08 one.
    let mut synth_slots = fork::FIXTURE_GUARD_SLOTS;
    synth_slots[usize::from(PumpProgram::Pump.guard())] = fork::PUMP_DEPLOY_SLOT_2026_10_08;
    for (deploy, slots) in [
        (PumpDeploy::Sep2026, VENUE_GUARD_SLOTS_2026_09_27),
        (
            PumpDeploy::Oct2026,
            shortwin::constants::VENUE_GUARD_SLOTS_2026_10_03,
        ),
        (PumpDeploy::Oct2026Synth, synth_slots),
        (PumpDeploy::Oct2026Chain, fork::FIXTURE_GUARD_SLOTS),
    ] {
        for p in PumpProgram::ALL {
            let pd = deploy.program_data(p);
            let elf = deploy.elf(p);
            let h = program_data::read(&pd.account.owner.to_bytes(), &pd.account.data)
                .expect("ProgramData header");
            let guard = usize::from(p.guard());
            assert_eq!(pd.address, VENUE_PROGRAM_DATA[guard], "{deploy:?} {p:?}");
            assert_eq!(h.slot, slots[guard], "{deploy:?} {p:?}: deploy slot");
            assert_eq!(h.slot, deploy.deploy_slot(p), "{deploy:?} {p:?}");
            assert_eq!(
                h.upgrade_authority,
                Some(authority.to_bytes()),
                "{deploy:?} {p:?}"
            );
            assert!(
                pd.account.data[program_data::HEADER_LEN + elf.len()..]
                    .iter()
                    .all(|&b| b == 0)
            );
            assert_eq!(&elf[..4], b"\x7fELF");
            assert_eq!(elf[48..52], [0, 0, 0, 0], "{deploy:?} {p:?}: SBPF v0");
            let end = elf.iter().rposition(|&b| b != 0).unwrap() + 1;
            assert_eq!(
                fork::sha256_hex(&elf[..end]),
                trimmed(deploy, p),
                "{deploy:?} {p:?}"
            );
            let prog = deploy.program(p);
            assert_eq!(prog.address, p.id());
            assert!(prog.account.executable);
            assert_eq!(prog.account.data[4..36], pd.address.to_bytes());
        }
    }
    assert_eq!(PumpDeploy::CURRENT, PumpDeploy::Oct2026Chain);
    assert_eq!(
        fork::FIXTURE_GUARD_SLOTS[usize::from(PumpProgram::Pump.guard())],
        456_494_698,
        "pump's 2026-10-13 redeploy"
    );
    for p in PumpProgram::ALL {
        let g = usize::from(p.guard());
        assert_eq!(
            fork::FIXTURE_GUARD_SLOTS[g],
            shortwin::constants::VENUE_GUARD_SLOTS_2026_10_13[g],
            "{p:?}: the fork's slot is the newest baseline's"
        );
        assert_eq!(p.elf(), PumpDeploy::CURRENT.elf(p));
        assert_eq!(
            fork::pump_fixture_at(&p.program_data().address)
                .unwrap()
                .name,
            p.program_data().name,
            "an address lookup resolves to the latest redeploy"
        );
        // The builds' ELF headers differ pairwise (what `PumpDeploy::held` reads); PumpSwap and
        // pump-fees are one build in the last two deploys, and `held` names the newer.
        for (i, a) in PumpDeploy::ALL.into_iter().enumerate() {
            for b in &PumpDeploy::ALL[i + 1..] {
                if a.same_build(*b, p) {
                    assert_eq!(a.elf(p), b.elf(p), "{p:?}: {a:?} vs {b:?}");
                    assert_ne!(p, PumpProgram::Pump);
                } else {
                    assert_ne!(a.elf(p)[..64], b.elf(p)[..64], "{p:?}: {a:?} vs {b:?}");
                }
            }
        }
    }
    let s = fork::pump_snapshot();
    for f in fork::all_pump_fixtures().filter(|f| f.group == "pump_state") {
        assert_eq!(f.slot, s.slot, "{}", f.name);
    }
    assert!(s.block_time > 1_790_600_000);
    let s13 = PumpCoin::Near166.snapshot();
    for f in fork::all_pump_fixtures().filter(|f| f.group == "pump_state_2026_10_13") {
        assert_eq!(f.slot, s13.slot, "{}", f.name);
    }
    assert!(s13.slot > 456_132_171 && s13.block_time > s.block_time);
    // The statics both state groups hold resolve to the newer one: the 1,088-byte Global.
    let g13 = fork::pump_fixture_at(&PUMP_GLOBAL).unwrap();
    let g_old = fork::fixture("pump_global");
    assert_eq!(
        (g13.name.as_str(), g13.account.data.len()),
        ("pump_global_1088", 1_088)
    );
    assert_eq!(
        g13.account.data[..1_087],
        g_old.account.data[..],
        "bytes 0..1087"
    );
    assert_eq!(g13.account.data[1_087], 1, "max_curve_depth");
    for (key, old) in [
        (PUMP_FEE_CONFIG, "pump_fee_config"),
        (PUMP_AMM_GLOBAL_CONFIG, "pamm_global_config"),
        (PUMP_AMM_FEE_CONFIG, "pamm_fee_config"),
    ] {
        let new = fork::pump_fixture_at(&key).unwrap();
        assert_eq!(new.group, "pump_state_2026_10_13", "{old}");
        assert_eq!(new.account.data, fork::fixture(old).account.data, "{old}");
    }
    assert_eq!(pump::PROGRAM, PUMP_PROGRAM_ID.to_bytes());
    assert_eq!(pump::FEE_PROGRAM, PUMP_FEES_PROGRAM_ID.to_bytes());
    assert_eq!(pump_amm::PROGRAM, PUMP_AMM_PROGRAM_ID.to_bytes());
    let pda = |seeds: &[&[u8]], p: &Pubkey| Pubkey::find_program_address(seeds, p).0;
    assert_eq!(pda(&[pump::GLOBAL_SEED], &PUMP_PROGRAM_ID), PUMP_GLOBAL);
    assert_eq!(
        pda(
            &[pump::FEE_CONFIG_SEED, PUMP_PROGRAM_ID.as_ref()],
            &PUMP_FEES_PROGRAM_ID
        ),
        PUMP_FEE_CONFIG
    );
    assert_eq!(
        pda(&[pump_amm::GLOBAL_CONFIG_SEED], &PUMP_AMM_PROGRAM_ID),
        PUMP_AMM_GLOBAL_CONFIG
    );
    assert_eq!(
        pda(
            &[pump::FEE_CONFIG_SEED, PUMP_AMM_PROGRAM_ID.as_ref()],
            &PUMP_FEES_PROGRAM_ID
        ),
        PUMP_AMM_FEE_CONFIG
    );
    assert_eq!(
        pda(&[pump::EVENT_AUTHORITY_SEED], &PUMP_PROGRAM_ID),
        shortwin::constants::PUMP_EVENT_AUTHORITY
    );
    assert_eq!(
        pda(&[pump::GLOBAL_VOLUME_ACCUMULATOR_SEED], &PUMP_PROGRAM_ID),
        shortwin::constants::PUMP_GLOBAL_VOLUME_ACCUMULATOR
    );
    // Every role of both state groups at its keys, with the expected shape.
    let raw = |env: &Env, c: &PumpCurve, off: usize| -> u64 {
        let d = env.svm.get_account(&c.curve).unwrap().data;
        d.get(off..off + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    for coin in PumpCoin::ALL.into_iter().chain(PumpCoin::ALL_2026_10_13) {
        let (env, c) = Env::fork_pump_coin(coin);
        assert_eq!(c.curve, pda::bonding_curve(&c.mint));
        let st = c.state(&env.svm);
        let len = c.data_len(&env.svm);
        let d = env.svm.get_account(&c.curve).unwrap().data;
        // The 2026-10-08 fields (raw: the reader learns them in P1): buckets @125 / @133,
        // `depth` @141, `ivqr` @142, `pcB` @150, `pcQ` @158.
        let (buckets, depth) = (
            (raw(&env, &c, 125), raw(&env, &c, 133)),
            d.get(141).copied().unwrap_or(0),
        );
        let (ivqr, pcb, pcq) = (raw(&env, &c, 142), raw(&env, &c, 150), raw(&env, &c, 158));
        let offset = st
            .virtual_quote_reserves
            .checked_sub(st.real_quote_reserves);
        match coin {
            PumpCoin::Fresh => assert!(st.is_live() && len == pump::CURVE_LEN_V2),
            PumpCoin::Near => {
                assert!(st.is_live() && len == pump::CURVE_LEN_EXTENDED);
                assert!(st.real_token_reserves <= 100_000_000_000_000);
            }
            PumpCoin::T22_115 | PumpCoin::Legacy115 => {
                assert!(st.is_live() && len == pump::CURVE_MIN_LEN)
            }
            PumpCoin::Legacy49 => assert!(st.is_live() && len == pump::CURVE_LEN_ORIGINAL),
            PumpCoin::Graduated => {
                assert!(st.is_migrated() && !st.is_mayhem_mode);
                let p = PumpPool::of(coin);
                let pool = p.state(&env.svm);
                assert_eq!(pool.base_mint, c.mint.to_bytes());
                assert!(pool.is_sol_quote() && pool.virtual_quote_reserves > 0);
                let r = p.reserves(&env.svm).unwrap();
                assert_eq!(
                    r.effective_quote,
                    u128::from(r.real_quote) + u128::try_from(pool.virtual_quote_reserves).unwrap()
                );
                assert!(
                    p.boost_amount(&env.svm) > 1_000_000_000,
                    "the boost is running"
                );
            }
            PumpCoin::Mayhem => assert!(st.is_mayhem_mode && st.is_live()),
            PumpCoin::Usdc => assert!(!st.is_sol_quote() && st.is_live()),
            PumpCoin::Legacy150 => {
                assert!(st.is_migrated() && len == pump::CURVE_LEN_LEGACY_EXTENDED);
                assert!(
                    PumpPool::of(coin).boost_amount(&env.svm) == 0,
                    "no boost ATA"
                );
            }
            PumpCoin::Fartcoin => {
                assert!(st.is_migrated());
                assert!(env.svm.get_account(&pda::canonical_pool(&c.mint)).is_none());
            }
            PumpCoin::Near166 => {
                assert!(st.is_live() && st.is_sol_quote() && len == fork::CURVE_LEN_2026_10_08);
                assert!(!st.is_mayhem_mode && !st.is_cashback_coin && depth == 0);
                assert!(st.real_token_reserves <= 100_000_000_000_000);
                assert!(buckets.0 > 0 && buckets.1 > 0, "{buckets:?}");
                assert_eq!((ivqr, offset), (30_000_000_000, Some(30_000_000_000)));
            }
            PumpCoin::Fresh166 => {
                assert!(st.is_live() && st.is_sol_quote() && len == fork::CURVE_LEN_2026_10_08);
                assert_eq!(
                    c.token_program.id(),
                    shortwin::constants::TOKEN_2022_PROGRAM_ID
                );
                assert!(!st.is_mayhem_mode && !st.is_cashback_coin && depth == 0);
                assert_eq!((ivqr, offset), (30_000_000_000, Some(30_000_000_000)));
            }
            PumpCoin::Buckets166 => {
                assert!(st.is_live() && st.is_sol_quote() && len == fork::CURVE_LEN_2026_10_08);
                assert!(
                    buckets.0 > 100_000_000 && buckets.1 > 100_000_000,
                    "{buckets:?}"
                );
            }
            PumpCoin::Grow141 | PumpCoin::Grow151 => {
                let want = if coin == PumpCoin::Grow141 { 141 } else { 151 };
                assert!(st.is_live() && st.is_sol_quote() && len == want);
                assert!(!st.is_mayhem_mode && !st.is_cashback_coin);
                assert_eq!(offset, Some(30_000_000_000));
            }
            PumpCoin::SyntheticStuck => {
                assert!(st.complete && st.virtual_token_reserves > 0 && !st.is_sol_quote());
                assert_eq!(
                    c.curve.to_string(),
                    "E1bYRCNq72Tyos6tsiMRokitMDJtzcPoKmkmdX8PdCHg"
                );
                assert_eq!(depth, 1);
                assert_eq!(
                    (ivqr, pcb, pcq),
                    (4_975_123_817_261, 125_468_585_973_711, 21_720_607_727_767)
                );
                assert_eq!(offset, Some(ivqr), "consistent");
            }
            PumpCoin::GraduatedSynthetic => {
                assert!(st.is_migrated() && st.is_sol_quote());
                assert_eq!((pcb, pcq), (6_238_850_918_425, 2_642_475_552));
                let p = PumpPool::of(coin);
                assert_eq!(
                    p.pool.to_string(),
                    "D4PACvyrY7uYmxUmQZYRUDsSjywJUg4Eq5RT8E53BN4o"
                );
                assert_eq!(p.state(&env.svm).base_mint, c.mint.to_bytes());
            }
            PumpCoin::Cashback166 => {
                assert!(st.is_live() && st.is_cashback_coin && st.is_sol_quote());
                assert!(!st.is_mayhem_mode && len == fork::CURVE_LEN_2026_10_08);
            }
            PumpCoin::Mayhem166 => {
                assert!(st.is_live() && st.is_mayhem_mode && len == fork::CURVE_LEN_2026_10_08);
                assert!(ivqr != 0 && offset != Some(ivqr), "vQ − rQ ≠ ivqr");
            }
            PumpCoin::WsolPoolBuckets => {
                assert!(st.is_migrated() && st.is_sol_quote());
                let p = PumpPool::of(coin);
                assert_eq!(
                    p.pool.to_string(),
                    "ERtQ5UhnSHJWb8exNA1BYumVnr27huYRhx3Ni2NFh88S"
                );
                let pd = env.svm.get_account(&p.pool).unwrap().data;
                let at = |o: usize| u64::from_le_bytes(pd[o..o + 8].try_into().unwrap());
                assert!(
                    pd.len() >= 287 && at(271) > 0 && at(279) > 0,
                    "pool buckets"
                );
            }
        }
    }
}

/// The reader's streamed tier selection equals `shortwin_math::fees::fee_tier` on PumpSwap's 25
/// live tiers (and the stable ones) at every threshold, one below it, and at random caps.
#[test]
fn reader_tier_selection_matches_shortwin_math() {
    let (env, _) = Env::fork_pump_coin(PumpCoin::Fresh);
    let acc = env.svm.get_account(&PUMP_AMM_FEE_CONFIG).unwrap();
    let c = pump::read_fee_config(&acc.owner.to_bytes(), &acc.data).unwrap();
    let mut rng = Rng(7);
    for tiers in [c.fee_tiers, c.stable_fee_tiers] {
        let math: Vec<MathFeeTier> = tiers
            .iter()
            .map(|t| MathFeeTier {
                threshold: t.threshold,
                fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
            })
            .collect();
        let mut caps: Vec<u128> = vec![0, 1, u128::MAX];
        for t in tiers.iter() {
            caps.extend([t.threshold.saturating_sub(1), t.threshold, t.threshold + 1]);
        }
        caps.extend((0..500).map(|_| u128::from(rng.next() % 200_000_000_000_000)));
        for m in caps {
            let a = tiers.select(m).unwrap();
            let b = mfees::fee_tier(&math, m).unwrap();
            assert_eq!(
                (a.lp, a.protocol, a.creator),
                (b.lp, b.protocol, b.creator),
                "mcap {m}"
            );
        }
    }
}

// ---- T-PUMP-MATH-V3: v3 trades and the synthetic migration (pump 2026-10-08) -------------------

/// The curve's fee buckets `(creator_fee @125, protocol_fees @133)` (0 past the account's end).
fn buckets(svm: &LiteSVM, c: &PumpCurve) -> (u64, u64) {
    let d = svm.get_account(&c.curve).expect("curve").data;
    let at = |o: usize| {
        d.get(o..o + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    (at(125), at(133))
}

/// The 2026-10-08 curve fields `(depth @141, ivqr @142, pcB @150, pcQ @158)`, read raw (the
/// reader learns them later; 0 past the account's end).
fn pc_fields(svm: &LiteSVM, c: &PumpCurve) -> (u8, u64, u64, u64) {
    let d = svm.get_account(&c.curve).expect("curve").data;
    let at = |o: usize| {
        d.get(o..o + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    (d.get(141).copied().unwrap_or(0), at(142), at(150), at(158))
}

/// pump's v3 exact-out input (C3): `n = ⌈q·y/(x − q)⌉`, and whether the division is exact (v2's
/// `⌊⌋ + 1` is one more there, equal elsewhere).
fn ceil_n(q: u64, x: u64, y: u64) -> (u64, bool) {
    let num = u128::from(q) * u128::from(y);
    let den = u128::from(x) - u128::from(q);
    (u64::try_from(num.div_ceil(den)).unwrap(), num % den == 0)
}

/// `(n + ⌈n·p/10⁴⌉ + ⌈n·c/10⁴⌉, ⌈n·p/10⁴⌉, ⌈n·c/10⁴⌉)`: a curve-fee charge on `n`.
fn charged(n: u64, fees: &FeeBps) -> (u64, u64, u64) {
    let (p, c) = (fee_up(n, fees.protocol), fee_up(n, fees.creator));
    (n + p + c, p, c)
}

/// What a v3 trade moves.
#[derive(Clone, Copy, Debug)]
struct Snap {
    user: u64,
    user_tokens: u64,
    curve: u64,
    curve_tokens: u64,
    buyback: u64,
    buckets: (u64, u64),
    uva: u64,
    uva_len: usize,
    len: usize,
    st: pump::BondingCurve,
}

fn snap(svm: &LiteSVM, c: &PumpCurve, u: &CurveUser, bb: &Pubkey) -> Snap {
    let uva = svm
        .get_account(&fork::pump_user_volume_accumulator(&u.kp.pubkey()))
        .expect("UVA");
    Snap {
        user: read::lamports(svm, &u.kp.pubkey()),
        user_tokens: read::token_amount(svm, &u.ata).unwrap_or(0),
        curve: read::lamports(svm, &c.curve),
        curve_tokens: read::token_amount(svm, &c.curve_base_ata).unwrap_or(0),
        buyback: read::lamports(svm, bb),
        buckets: buckets(svm, c),
        uva: uva.lamports,
        uva_len: uva.data.len(),
        len: c.data_len(svm),
        st: c.state(svm),
    }
}

/// The rent a trade on `s`'s curve takes from the user to grow it to the current layout (166
/// bytes; 0 at it): the reserve `lamports − rQ − buckets` raised to `minimum_balance(166)`
/// (`fork::curve_growth_top_up`).
fn growth(svm: &LiteSVM, s: &Snap) -> u64 {
    fork::curve_growth_top_up(
        svm,
        s.len,
        s.curve,
        s.st.real_quote_reserves,
        s.buckets.0 + s.buckets.1,
        PumpDeploy::CURRENT.curve_len(),
    )
}

/// How pump charged the exact-division states the v3 probes met (recorded, not a G0 stop: §5.2's
/// `+1` covers either).
#[derive(Clone, Copy, Debug, Default)]
struct ExactDivision {
    states: u32,
    ceil: u32,
    floor_plus_one: u32,
}

impl ExactDivision {
    fn record(&mut self, took_ceil: bool) {
        self.states += 1;
        if took_ceil {
            self.ceil += 1;
        } else {
            self.floor_plus_one += 1;
        }
    }
}

/// `Global.pool_migration_fee` and `buyback_basis_points` now.
fn migration_fee(svm: &LiteSVM) -> u64 {
    let g = svm.get_account(&PUMP_GLOBAL).unwrap();
    pump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee()
}

/// The curve's state after a trade, from the reader and the `TradeEvent`.
#[track_caller]
fn check_v3_post(
    ctx: &str,
    after: &pump::BondingCurve,
    ev: &PumpTradeEvent,
    want: (u64, u64, u64, u64),
) {
    let got = (
        after.virtual_token_reserves,
        after.virtual_quote_reserves,
        after.real_token_reserves,
        after.real_quote_reserves,
    );
    assert_eq!(got, want, "{ctx}: the curve after the trade");
    assert_eq!(
        (
            ev.virtual_token_reserves,
            ev.virtual_sol_reserves,
            ev.real_token_reserves,
            ev.real_sol_reserves
        ),
        want,
        "{ctx}: the TradeEvent's post-state"
    );
}

/// A third party's `buy_v3(q)` (`q < rT`) checked against the v3 model (T-PUMP-MATH-V3 (a)):
/// `n = ⌈q·vQ/(vT − q)⌉` (on an exact division pump may take `⌊⌋ + 1`: recorded), each fee
/// `⌈n·b/10⁴⌉`, the buyback `⌊protocol·buyback_bps/10⁴⌋` paid out, the rest of the protocol fee
/// and the creator fee kept in the curve's buckets outside `rQ`, `vQ − rQ` unchanged, the
/// curve grown to 166 bytes at the user's expense, the UVA untouched; one lamport under the
/// charge fails 6002.
#[track_caller]
fn v3_buy(env: &mut Env, c: &PumpCurve, u: &CurveUser, q: u64, ed: &mut ExactDivision) {
    let ctx = format!("{:?} buy_v3 q {q}", c.coin);
    let bb = CurveUser::v3_buyback_recipient(&env.svm);
    let fees = c.fees(&env.svm).expect("fees");
    let s0 = snap(&env.svm, c, u, &bb);
    let st = s0.st;
    assert!(q > 0 && q < st.real_token_reserves, "{ctx}");
    let (n_ceil, exact) = ceil_n(q, st.virtual_token_reserves, st.virtual_quote_reserves);
    let cost_ceil = charged(n_ceil, &fees).0;
    let ix = u.buy_v3_ix(&env.svm, c, q, cost_ceil - 1);
    let res = u.send(env, ix);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_TOO_MUCH_SOL_REQUIRED),
        "{ctx}: one lamport under the ⌈⌉ charge; {}",
        describe(&res)
    );
    let mut n = n_ceil;
    let ix = u.buy_v3_ix(&env.svm, c, q, cost_ceil);
    let mut res = u.send(env, ix);
    if exact {
        let took_ceil = custom_code(&res) != Some(fork::PUMP_ERR_TOO_MUCH_SOL_REQUIRED);
        ed.record(took_ceil);
        if !took_ceil {
            n += 1;
            let ix = u.buy_v3_ix(&env.svm, c, q, charged(n, &fees).0);
            res = u.send(env, ix);
        }
    }
    let meta = ok(res, &ctx);
    let (cost, protocol, creator) = charged(n, &fees);
    let buyback = buyback_share(&env.svm, protocol);
    let s1 = snap(&env.svm, c, u, &bb);
    let grow = growth(&env.svm, &s0);
    let ev = one_trade(&meta);
    assert_eq!(
        (
            ev.sol_amount,
            ev.token_amount,
            ev.fee,
            ev.creator_fee,
            ev.buyback_fee
        ),
        (n, q, protocol, creator, buyback),
        "{ctx}: TradeEvent"
    );
    assert!(
        ev.is_buy && ev.ix_name == "buy" && ev.mint == c.mint,
        "{ctx}: {ev:?}"
    );
    assert_eq!(
        ev.fee_recipient,
        Pubkey::default(),
        "{ctx}: no fee recipient"
    );
    assert_eq!(ev.fee_basis_points, fees.protocol, "{ctx}");
    // A coin without a creator pays no creator fee; its event still names the tier's bps.
    if !st.creator_is_default() {
        assert_eq!(ev.creator_fee_basis_points, fees.creator, "{ctx}");
    }
    assert_eq!(s1.len, s0.len.max(PumpDeploy::CURRENT.curve_len()), "{ctx}");
    assert_eq!((s1.uva, s1.uva_len), (s0.uva, s0.uva_len), "{ctx}: the UVA");
    assert_eq!(s0.user - s1.user, cost + grow, "{ctx}: user paid");
    assert_eq!(s1.user_tokens - s0.user_tokens, q, "{ctx}: user received");
    assert_eq!(s0.curve_tokens - s1.curve_tokens, q, "{ctx}: curve tokens");
    assert_eq!(s1.buyback - s0.buyback, buyback, "{ctx}: buyback recipient");
    assert_eq!(
        s1.buckets,
        (s0.buckets.0 + creator, s0.buckets.1 + protocol - buyback),
        "{ctx}: the buckets grow by creator_fee / fee − buyback"
    );
    assert_eq!(
        s1.curve - s0.curve,
        n + protocol - buyback + creator + grow,
        "{ctx}: curve lamports"
    );
    check_v3_post(
        &ctx,
        &s1.st,
        &ev,
        (
            st.virtual_token_reserves - q,
            st.virtual_quote_reserves + n,
            st.real_token_reserves - q,
            st.real_quote_reserves + n,
        ),
    );
    assert!(!s1.st.complete, "{ctx}");
}

/// A third party's `sell_v3(q)` checked against the model (T-PUMP-MATH-V3 (a)): proceeds `g =
/// ⌊q·vQ/(vT + q)⌋` less each fee `⌈g·b/10⁴⌉` (the v2 quote), the fees into the buckets and the
/// buyback out as on a buy; one lamport over the proceeds fails 6003. Returns whether it
/// executed (`false` where the quote is undefined: then pump refuses too).
#[track_caller]
fn v3_sell(env: &mut Env, c: &PumpCurve, u: &CurveUser, q: u64) -> bool {
    let ctx = format!("{:?} sell_v3 q {q}", c.coin);
    u.plant(env, q);
    let bb = CurveUser::v3_buyback_recipient(&env.svm);
    let fees = c.fees(&env.svm).expect("fees");
    let s0 = snap(&env.svm, c, u, &bb);
    let st = s0.st;
    let Some(sq) = sol_leg::sell_exact_in(
        q,
        u128::from(st.virtual_token_reserves),
        u128::from(st.virtual_quote_reserves),
        &fees,
        st.real_quote_reserves,
    ) else {
        let ix = u.sell_v3_ix(&env.svm, c, q, 0);
        let res = u.send(env, ix);
        assert!(
            res.is_err(),
            "{ctx}: no quote, but the venue {}",
            describe(&res)
        );
        return false;
    };
    let g = st.virtual_quote_reserves - u64::try_from(sq.sol_after).unwrap();
    let (protocol, creator) = (fee_up(g, fees.protocol), fee_up(g, fees.creator));
    assert_eq!(sq.proceeds, g - protocol - creator);
    let ix = u.sell_v3_ix(&env.svm, c, q, sq.proceeds + 1);
    let res = u.send(env, ix);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_TOO_LITTLE_SOL_RECEIVED),
        "{ctx}: one lamport over the proceeds; {}",
        describe(&res)
    );
    let ix = u.sell_v3_ix(&env.svm, c, q, sq.proceeds);
    let meta = ok(u.send(env, ix), &ctx);
    let buyback = buyback_share(&env.svm, protocol);
    let s1 = snap(&env.svm, c, u, &bb);
    let grow = growth(&env.svm, &s0);
    let ev = one_trade(&meta);
    assert_eq!(
        (
            ev.sol_amount,
            ev.token_amount,
            ev.fee,
            ev.creator_fee,
            ev.buyback_fee
        ),
        (g, q, protocol, creator, buyback),
        "{ctx}: TradeEvent"
    );
    assert!(!ev.is_buy && ev.ix_name == "sell", "{ctx}: {ev:?}");
    assert_eq!(s1.len, s0.len.max(PumpDeploy::CURRENT.curve_len()), "{ctx}");
    assert_eq!((s1.uva, s1.uva_len), (s0.uva, s0.uva_len), "{ctx}: the UVA");
    assert_eq!(
        i128::from(s1.user) - i128::from(s0.user),
        i128::from(sq.proceeds) - i128::from(grow),
        "{ctx}: user received"
    );
    assert_eq!(s0.user_tokens - s1.user_tokens, q, "{ctx}: user sold");
    assert_eq!(s1.curve_tokens - s0.curve_tokens, q, "{ctx}: curve tokens");
    assert_eq!(s1.buyback - s0.buyback, buyback, "{ctx}: buyback recipient");
    assert_eq!(
        s1.buckets,
        (s0.buckets.0 + creator, s0.buckets.1 + protocol - buyback),
        "{ctx}: buckets"
    );
    assert_eq!(
        i128::from(s1.curve) - i128::from(s0.curve),
        i128::from(protocol - buyback + creator + grow) - i128::from(g),
        "{ctx}: curve lamports"
    );
    check_v3_post(
        &ctx,
        &s1.st,
        &ev,
        (
            st.virtual_token_reserves + q,
            st.virtual_quote_reserves - g,
            st.real_token_reserves + q,
            st.real_quote_reserves - g,
        ),
    );
    true
}

/// Moves `c` to the point of its own curve where `rT = rt`: `vT·vQ` and the offsets `vT − rT`,
/// `vQ − rQ` (= `ivqr`) kept, the curve's lamports and base ATA following (its rent reserve, its
/// buckets and the would-be pool's tokens kept). `vq` overrides the constant-product `vQ`.
fn set_state(env: &mut Env, c: &PumpCurve, rt: u64, vq: Option<u64>) {
    let st = c.state(&env.svm);
    let (ot, oq) = (
        st.virtual_token_reserves - st.real_token_reserves,
        st.virtual_quote_reserves - st.real_quote_reserves,
    );
    let k = u128::from(st.virtual_token_reserves) * u128::from(st.virtual_quote_reserves);
    let vt = rt + ot;
    let vq = vq.unwrap_or_else(|| u64::try_from(k.div_ceil(u128::from(vt))).unwrap().max(oq));
    let rq = vq - oq;
    let (b0, b1) = buckets(&env.svm, c);
    let mut acc = env.svm.get_account(&c.curve).unwrap();
    let reserve = acc.lamports - st.real_quote_reserves - b0 - b1;
    acc.lamports = reserve + rq + b0 + b1;
    env.svm.set_account(c.curve, acc).unwrap();
    fork::set_curve_reserves(&mut env.svm, &c.curve, vt, vq, rt, rq);
    let ata = read::token_amount(&env.svm, &c.curve_base_ata).unwrap();
    fork::set_token_amount(
        &mut env.svm,
        &c.curve_base_ata,
        rt + (ata - st.real_token_reserves),
    );
}

/// Moves `c` to a random point of its curve where a buy of the returned `q` divides exactly:
/// `vT − q = e·2²⁰` with `q = e·j`, and `vQ` (the constant product's) rounded up to a multiple
/// of 2²⁰, so that `q·vQ/(vT − q) = j·vQ/2²⁰`.
fn exact_state(env: &mut Env, c: &PumpCurve, rng: &mut Rng) -> u64 {
    const F: u64 = 1 << 20;
    const RT_MAX: u64 = 793_100_000_000_000;
    let st = c.state(&env.svm);
    let (ot, oq) = (
        st.virtual_token_reserves - st.real_token_reserves,
        st.virtual_quote_reserves - st.real_quote_reserves,
    );
    let k = u128::from(st.virtual_token_reserves) * u128::from(st.virtual_quote_reserves);
    let e = ot / F + 1 + rng.next() % (RT_MAX / F);
    let d = e * F;
    let room = RT_MAX.saturating_sub(d - ot) / e;
    let q = e * (1 + rng.next() % room.max(1));
    let vt = d + q;
    let vq = u64::try_from(k.div_ceil(u128::from(vt)))
        .unwrap()
        .max(oq)
        .div_ceil(F)
        * F;
    set_state(env, c, vt - ot, Some(vq));
    q
}

/// T-PUMP-MATH-V3 (a): a third party's `buy_v3` and `sell_v3` on `PUMP_MATH_V3_STATES` (200)
/// seeded random states of every live SOL curve of both state groups (the 2026-10-08 build grows
/// the shorter ones to 166 bytes on their first trade), each checked end to end against the v3
/// model; every fifth state is an exact division by construction, and how pump charged those is
/// recorded (`⌈⌉` or v2's `⌊⌋ + 1`; either is within §5.2's bound, neither stops G0).
#[test]
fn v3_buys_and_sells_follow_the_v3_model_on_every_live_curve() {
    let n = env_u64("PUMP_MATH_V3_STATES", 200);
    let seed = env_u64("PUMP_MATH_SEED", 0x5eed_c0de_0005);
    let mut ed = ExactDivision::default();
    let mut sells = 0;
    let coins: Vec<PumpCoin> = PumpCoin::LIVE_SOL
        .into_iter()
        .chain(PumpCoin::LIVE_SOL_2026_10_13)
        .collect();
    for (i, coin) in coins.into_iter().enumerate() {
        let (mut env, c) = Env::fork_pump_coin(coin);
        let u = CurveUser::new(&mut env, &c);
        let mut acc = env.svm.get_account(&u.kp.pubkey()).unwrap();
        acc.lamports = 1_000_000_000_000_000;
        env.svm.set_account(u.kp.pubkey(), acc).unwrap();
        let mut rng = Rng(seed ^ (i as u64 + 17).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        for k in 0..n {
            let q = if k % 5 == 4 {
                exact_state(&mut env, &c, &mut rng)
            } else {
                let rt = rng.amount(793_100_000_000_000).max(1_000);
                set_state(&mut env, &c, rt, None);
                rng.amount(rt - 1)
            };
            v3_buy(&mut env, &c, &u, q, &mut ed);
            let s = c.state(&env.svm);
            let sold = rng.amount(s.virtual_token_reserves);
            sells += usize::from(v3_sell(&mut env, &c, &u, sold));
        }
        assert_eq!(c.data_len(&env.svm), fork::CURVE_LEN_2026_10_08, "{coin:?}");
    }
    println!(
        "T-PUMP-MATH-V3 (a): {} buys, {sells} sells; exact divisions {}: pump charged ⌈⌉ on {}, \
         ⌊⌋ + 1 on {}",
        10 * n,
        ed.states,
        ed.ceil,
        ed.floor_plus_one
    );
    assert!(ed.states >= 10 * u32::try_from(n / 5).unwrap());
    assert!(sells * 2 >= 10 * usize::try_from(n).unwrap());
}

/// The model's charge of a synthetic `buy_v3(rT + δ)` (§5.2, V3) on the state before it.
#[derive(Clone, Copy, Debug)]
struct Synthetic {
    /// `n₁ᵖ`: the curve part's input (`⌈rT·vQ/(vT − rT)⌉`, or `⌊⌋ + 1` where pump took it).
    n1: u64,
    /// `n₂ᵖ = ⌈Y_s·δ/(X_s − δ)⌉` (or `⌊⌋ + 1` where pump took it).
    n2: u64,
    /// `X_s = curve_base_ata − rT` (donations included), `Y_s = rQ + n₁ − fee_m`.
    x_s: u64,
    y_s: u64,
    /// Each part's `(protocol, creator)` fees and the total charge.
    fees1: (u64, u64),
    fees2: (u64, u64),
    charge: u64,
}

/// The model with `n₁ = n1` and `n₂ = ⌈Y_s·δ/(X_s − δ)⌉ + up2` (`up2` = 1: v2's `⌊⌋ + 1` on an
/// exact division); also whether `n₂`'s division is exact.
fn synthetic_model(
    st: &pump::BondingCurve,
    ata: u64,
    fee_m: u64,
    delta: u64,
    fees: &FeeBps,
    n1: u64,
    up2: u64,
) -> (Synthetic, bool) {
    let x_s = ata - st.real_token_reserves;
    let y_s = st.real_quote_reserves + n1 - fee_m;
    let (n2, exact2) = ceil_n(delta, x_s, y_s);
    let n2 = n2 + up2;
    let (c1, p1, cr1) = charged(n1, fees);
    let (c2, p2, cr2) = charged(n2, fees);
    (
        Synthetic {
            n1,
            n2,
            x_s,
            y_s,
            fees1: (p1, cr1),
            fees2: (p2, cr2),
            charge: c1 + c2,
        },
        exact2,
    )
}

/// `B̂_v3(rT + δ)` (§5.2): `shortwin_math::quote::curve_synthetic` (P1) on the pre-trade curve,
/// the curve leg `buy_exact_out(rT, vT, vQ, ε₁ = 1, φ)` plus the synthetic-pool leg
/// `buy_exact_out(δ, X_s, Y_s, ε₁ = 1, φ)` on `migrated_pool`'s `(X_s, Y_s)` with
/// `pool_tokens = curve_base_ata − rT` — the live leg the adapter quotes (pump-final §5.2) —
/// both legs at `fees`.
fn b_hat_v3(st: &pump::BondingCurve, ata: u64, fee_m: u64, delta: u64, fees: &FeeBps) -> u64 {
    let s = CurveState {
        vt: u128::from(st.virtual_token_reserves),
        vq: u128::from(st.virtual_quote_reserves),
        rt: u128::from(st.real_token_reserves),
        rq: u128::from(st.real_quote_reserves),
        complete: false,
    };
    let m = MigrationParams {
        migration_fee: fee_m,
        pool_tokens: u128::from(ata - st.real_token_reserves),
        post_complete_quote_in: 0,
    };
    let b = curve_synthetic(st.real_token_reserves + delta, &s, fees, fees, &m).unwrap();
    // The composition, from the primitives (and the would-be pool pump priced on).
    let leg1 = sol_leg::buy_exact_out(st.real_token_reserves, s.vt, s.vq, true, fees).unwrap();
    let (xs, ys) = migrated_pool(&s, &m).unwrap();
    let leg2 = sol_leg::buy_exact_out(delta, xs, ys, true, fees).unwrap();
    assert_eq!(b.cost, leg1.cost + leg2.cost);
    b.cost
}

/// §5.2's general bound on `B̂_v3 − charge`: `(1 + Σ⌈bᵢ/10⁴⌉) + (Δ₂ + Σ⌈Δ₂·bᵢ/10⁴⌉)` with
/// `Δ₂ = 1 + ⌈q₂/(X_s − q₂)⌉` over the non-zero fee components.
fn v3_bound(delta: u64, x_s: u64, fees: &FeeBps) -> u64 {
    let d2 = 1 + delta.div_ceil(x_s - delta);
    let b: Vec<u64> = [fees.protocol, fees.creator]
        .into_iter()
        .filter(|&b| b > 0)
        .collect();
    (1 + b.iter().map(|&b| b.div_ceil(10_000)).sum::<u64>())
        + (d2 + b.iter().map(|&b| fee_up(d2, b)).sum::<u64>())
}

/// A third party's synthetic `buy_v3(rT + δ)` (T-PUMP-MATH-V3 (b), (b′)): the charge is
/// `F(n₁ᵖ) + F(n₂ᵖ)` with `X_s` the live `curve_base_ata − rT` (a donation included); one lamport
/// under fails 6002; the events are `TradeEvent` (the curve part), `CompleteEvent`,
/// `PostCompleteBuyEvent` (the pool part, its `pool_base_reserves_after` = the live
/// `curve_base_ata`); the curve completes with `pcB = δ`, `pcQ = n₂ᵖ`, `rQ = rQ₀ + n₁ᵖ` and
/// `vQ − rQ = ivqr`; and `0 ≤ B̂_v3 − charge ≤` §5.2's bound (≤ 7 for `δ ≤ X_s/2`). `partial`
/// appends the optional `partial_fill` byte (25 bytes; else the 24-byte form). Returns the model.
#[track_caller]
fn synthetic_buy(
    env: &mut Env,
    c: &PumpCurve,
    u: &CurveUser,
    delta: u64,
    partial: Option<bool>,
    ed: &mut ExactDivision,
) -> Synthetic {
    let ctx = format!("{:?} synthetic buy_v3 rT + {delta}", c.coin);
    let bb = CurveUser::v3_buyback_recipient(&env.svm);
    let fees = c.fees(&env.svm).expect("fees");
    // short.win's synthetic fee (P1 correction (a)): the tier at the live supply; the live curve
    // `FeeConfig`'s single flat tier makes it the fixed-supply one too.
    let phi_s = c.synthetic_fees(&env.svm).expect("φ_s");
    assert_eq!(phi_s, fees, "{ctx}: one flat tier");
    let fee_m = migration_fee(&env.svm);
    let s0 = snap(&env.svm, c, u, &bb);
    let st = s0.st;
    let (_, ivqr, pcb0, pcq0) = pc_fields(&env.svm, c);
    assert_eq!((pcb0, pcq0), (0, 0), "{ctx}: a live curve");
    let rt = st.real_token_reserves;
    let q = rt + delta;
    let ix = |svm: &LiteSVM, bound: u64| {
        let ix = u.buy_v3_ix(svm, c, q, bound);
        match partial {
            Some(flag) => spump::with_partial_fill(ix, flag),
            None => ix,
        }
    };
    let (n1_ceil, exact1) = ceil_n(rt, st.virtual_token_reserves, st.virtual_quote_reserves);
    let model = |up1: u64, up2: u64| {
        synthetic_model(
            &st,
            s0.curve_tokens,
            fee_m,
            delta,
            &fees,
            n1_ceil + up1,
            up2,
        )
    };
    let (mut m, _) = model(0, 0);
    let under = ix(&env.svm, m.charge - 1);
    let res = u.send(env, under);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_TOO_MUCH_SOL_REQUIRED),
        "{ctx}: one lamport under the ⌈⌉ model; {}",
        describe(&res)
    );
    // On an exact division of a part pump may take v2's `⌊⌋ + 1`: the candidates in charge order.
    let mut res = None;
    for (up1, up2) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let (cand, exact2) = model(up1, 0);
        if (up1 == 1 && !exact1) || (up2 == 1 && !exact2) {
            continue;
        }
        let cand = if up2 == 1 { model(up1, 1).0 } else { cand };
        let attempt = ix(&env.svm, cand.charge);
        let r = u.send(env, attempt);
        if custom_code(&r) == Some(fork::PUMP_ERR_TOO_MUCH_SOL_REQUIRED) {
            continue;
        }
        let took = |up: u64| if up == 0 { "⌈⌉" } else { "⌊⌋ + 1" };
        if exact1 {
            ed.record(up1 == 0);
            println!("{ctx}: an exact curve part; pump charged {}", took(up1));
        }
        if exact2 {
            ed.record(up2 == 0);
            println!("{ctx}: an exact pool part; pump charged {}", took(up2));
        }
        m = cand;
        res = Some(r);
        break;
    }
    let meta = ok(res.expect("a candidate charge executes"), &ctx);
    assert_eq!(
        fork::pump_event_names(&meta)
            .into_iter()
            .filter(|n| *n != "other")
            .collect::<Vec<_>>(),
        ["TradeEvent", "CompleteEvent", "PostCompleteBuyEvent"],
        "{ctx}: the events, in order"
    );
    let (bb1, bb2) = (
        buyback_share(&env.svm, m.fees1.0),
        buyback_share(&env.svm, m.fees2.0),
    );
    let ev = one_trade(&meta);
    assert_eq!(
        (
            ev.sol_amount,
            ev.token_amount,
            ev.fee,
            ev.creator_fee,
            ev.buyback_fee
        ),
        (m.n1, rt, m.fees1.0, m.fees1.1, bb1),
        "{ctx}: the curve part's TradeEvent"
    );
    assert!(ev.is_buy && ev.ix_name == "buy", "{ctx}: {ev:?}");
    // A coin without a creator pays no creator fee (`fees.creator` = 0 in the model); its events
    // may still name the tier's bps (as `v3_buy`'s), so the bps are checked on the others only.
    let creator_set = !st.creator_is_default();
    assert_eq!(ev.fee_basis_points, fees.protocol, "{ctx}");
    if creator_set {
        assert_eq!(ev.creator_fee_basis_points, fees.creator, "{ctx}");
    }
    let post = fork::pump_post_complete_buy_events(&meta);
    assert_eq!(post.len(), 1, "{ctx}");
    let post = &post[0];
    assert_eq!(
        (post.mint, post.bonding_curve, post.user, post.quote_mint),
        (c.mint, c.curve, u.kp.pubkey(), Pubkey::default()),
        "{ctx}"
    );
    assert_eq!(
        (
            post.base_out,
            post.quote_in,
            post.fee,
            post.creator_fee,
            post.buyback_fee
        ),
        (delta, m.n2, m.fees2.0, m.fees2.1, bb2),
        "{ctx}: the pool part"
    );
    assert_eq!(post.fee_basis_points, fees.protocol, "{ctx}");
    if creator_set {
        assert_eq!(post.creator_fee_basis_points, fees.creator, "{ctx}");
    } else {
        println!(
            "{ctx}: a default creator; the events name creator bps {} and {}, charge 0",
            ev.creator_fee_basis_points, post.creator_fee_basis_points
        );
    }
    assert_eq!(
        (
            post.pool_base_reserves_before,
            post.pool_quote_reserves_before,
            post.pool_base_reserves_after,
            post.pool_quote_reserves_after
        ),
        (m.x_s, m.y_s, m.x_s - delta, m.y_s + m.n2),
        "{ctx}: the would-be pool"
    );
    let s1 = snap(&env.svm, c, u, &bb);
    assert_eq!(
        post.pool_base_reserves_after, s1.curve_tokens,
        "{ctx}: the pool part leaves the live curve_base_ata"
    );
    assert_eq!(
        s0.user - s1.user,
        m.charge + growth(&env.svm, &s0),
        "{ctx}: user paid"
    );
    assert_eq!(s1.user_tokens - s0.user_tokens, q, "{ctx}");
    assert_eq!(s0.curve_tokens - s1.curve_tokens, q, "{ctx}");
    assert_eq!(
        s1.buyback - s0.buyback,
        bb1 + bb2,
        "{ctx}: buyback recipient"
    );
    assert_eq!(
        s1.buckets,
        (
            s0.buckets.0 + m.fees1.1 + m.fees2.1,
            s0.buckets.1 + m.fees1.0 - bb1 + m.fees2.0 - bb2
        ),
        "{ctx}: buckets"
    );
    assert_eq!(
        s1.curve - s0.curve,
        m.n1 + m.n2 + m.fees1.0 - bb1 + m.fees1.1 + m.fees2.0 - bb2
            + m.fees2.1
            + growth(&env.svm, &s0),
        "{ctx}: curve lamports"
    );
    let after = s1.st;
    assert!(
        after.complete && after.is_migrating(),
        "{ctx}: complete, not migrated"
    );
    assert_eq!(
        (
            after.virtual_token_reserves,
            after.virtual_quote_reserves,
            after.real_token_reserves,
            after.real_quote_reserves
        ),
        (
            st.virtual_token_reserves - rt,
            st.virtual_quote_reserves + m.n1,
            0,
            st.real_quote_reserves + m.n1
        ),
        "{ctx}: rQ excludes the pool part"
    );
    let (_, ivqr1, pcb, pcq) = pc_fields(&env.svm, c);
    assert_eq!(
        (pcb, pcq),
        (delta, m.n2),
        "{ctx}: post_complete_base_out / quote_in"
    );
    assert_eq!(ivqr1, ivqr);
    if ivqr != 0 {
        assert_eq!(
            after.virtual_quote_reserves - after.real_quote_reserves,
            ivqr,
            "{ctx}: vQ − rQ = ivqr"
        );
    } else {
        // A legacy curve pump grew to 166 bytes leaves `ivqr` 0 (never backfilled): the
        // invariant is vacuous (`BondingCurve::quote_offset_consistent`); the offset is kept.
        assert_eq!(
            after.virtual_quote_reserves - after.real_quote_reserves,
            st.virtual_quote_reserves - st.real_quote_reserves,
            "{ctx}: vQ − rQ kept"
        );
    }
    assert!(after.quote_offset_consistent(), "{ctx}");
    // §5.2: short.win's B̂_v3 covers the charge, by at most the bound.
    let b = b_hat_v3(&st, s0.curve_tokens, fee_m, delta, &phi_s);
    let bound = v3_bound(delta, m.x_s, &fees);
    assert!(
        b >= m.charge && b - m.charge <= bound,
        "{ctx}: B̂_v3 {b} vs charge {} (bound {bound})",
        m.charge
    );
    if 2 * delta <= m.x_s {
        assert!(b - m.charge <= 7, "{ctx}: ≤ 7 for δ ≤ X_s/2");
    }
    println!(
        "{ctx}: charge {}, B̂_v3 {b} (+{}, bound {bound}); X_s {}, Y_s {}, n1 {}, n2 {}",
        m.charge,
        b - m.charge,
        m.x_s,
        m.y_s,
        m.n1,
        m.n2
    );
    m
}

/// A token donation of `d` to `c`'s `curve_base_ata`: a planted holder's real `TransferChecked`.
fn donate(env: &mut Env, c: &PumpCurve, d: u64) {
    let holder = env.funded_keypair();
    let program = c.token_program.id();
    let payer = env.payer.pubkey();
    ok(
        env.send(
            &[fork::create_ata_idempotent_ix(
                &payer,
                &holder.pubkey(),
                &c.mint,
                &program,
            )],
            &[],
        ),
        "holder ATA",
    );
    let src = pda::ata(&holder.pubkey(), &c.mint, &program);
    fork::set_token_amount(&mut env.svm, &src, d);
    let decimals = env.svm.get_account(&c.mint).unwrap().data[44];
    let mut data = vec![12u8];
    data.extend_from_slice(&d.to_le_bytes());
    data.push(decimals);
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: program,
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(src, false),
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(c.mint, false),
            anchor_lang::solana_program::instruction::AccountMeta::new(c.curve_base_ata, false),
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                holder.pubkey(),
                true,
            ),
        ],
        data,
    };
    let before = read::token_amount(&env.svm, &c.curve_base_ata).unwrap();
    ok(env.send(&[ix], &[&holder]), "donation");
    assert_eq!(
        read::token_amount(&env.svm, &c.curve_base_ata).unwrap(),
        before + d
    );
}

/// T-PUMP-MATH-V3 (d): `migrate_v2` creates exactly `(curve_base_ata, rQ + pcQ − fee_m)` — the
/// `curve_base_ata` read just before it, donations included — with the boost `VQR = ⌊X₁·Y₁/
/// 10¹⁵⌋` (pump-final §3.4 (d)'s `⌊(Y − pcQ)·2069/10⁴⌋` only without a donation); the curve keeps
/// `pcB` / `pcQ`.
#[track_caller]
fn migrate_and_check(env: &mut Env, c: &PumpCurve, ctx: &str) {
    let a = read::token_amount(&env.svm, &c.curve_base_ata).unwrap();
    let st = c.state(&env.svm);
    let (_, _, pcb, pcq) = pc_fields(&env.svm, c);
    let fee_m = migration_fee(&env.svm);
    let migrator = env.funded_keypair();
    let mig = spump::migrate_v2_ix(&env.svm, &migrator.pubkey(), c);
    ok(
        env.send(&[spump::cu_limit_ix(600_000), mig], &[&migrator]),
        &format!("{ctx}: migrate_v2"),
    );
    let np = spump::NewPool::of(c);
    let y = st.real_quote_reserves + pcq - fee_m;
    let (base, vault) = (
        fork::vault_amount(&env.svm, &np.base_vault),
        fork::vault_amount(&env.svm, &np.quote_vault),
    );
    let pool = np.state(&env.svm).expect("the canonical pool");
    let vqr = pool.virtual_quote_reserves;
    let spec = i128::try_from(u128::from(y - pcq) * 2_069 / 10_000).unwrap();
    // The boost is `⌊X₁·Y₁/10¹⁵⌋` (the created pool's effective reserves, the 10¹⁵ supply): the
    // spec's `⌊(Y − pcQ)·2069/10⁴⌋` (+1 at most, the pool part's rounding up keeping
    // `X₁·Y₁ ≥ 206.9e12·(Y − pcQ)`) only while no token was donated to curve_base_ata.
    let boost = i128::try_from(migration_boost(u128::from(a), u128::from(y)).unwrap()).unwrap();
    assert_eq!(
        boost,
        i128::try_from(u128::from(y) * u128::from(a) / 1_000_000_000_000_000).unwrap()
    );
    println!(
        "{ctx}: migrate_v2 created base {base} (curve_base_ata {a}), quote vault {vault} + VQR \
         {vqr} = {} (rQ + pcQ − fee = {y}); ⌊X₁·Y₁/10¹⁵⌋ = {boost}, ⌊(Y − pcQ)·2069/10⁴⌋ = \
         {spec}",
        i128::from(vault) + vqr
    );
    assert_eq!(base, a, "{ctx}: X₁ = the live curve_base_ata");
    // P1's `migrated_pool` with the live curve_base_ata and pcQ (pump-final §5.1, §5.4) is the
    // pool migrate_v2 created, exactly (the curve is complete: rT = 0, no curve leg).
    let done = CurveState {
        vt: u128::from(st.virtual_token_reserves),
        vq: u128::from(st.virtual_quote_reserves),
        rt: u128::from(st.real_token_reserves),
        rq: u128::from(st.real_quote_reserves),
        complete: st.complete,
    };
    assert_eq!(
        migrated_pool(
            &done,
            &MigrationParams {
                migration_fee: fee_m,
                pool_tokens: u128::from(a),
                post_complete_quote_in: pcq,
            },
        ),
        Some((u128::from(base), u128::from(y))),
        "{ctx}: (X₁, Y₁)"
    );
    assert_eq!(
        i128::from(vault) + vqr,
        i128::from(y),
        "{ctx}: Y₁ = rQ + pcQ − fee (vault + VQR)"
    );
    assert_eq!(vqr, boost, "{ctx}: the boost VQR = ⌊X₁·Y₁/10¹⁵⌋");
    if a + pcb == 206_900_000_000_000 {
        assert!(
            (spec..=spec + 1).contains(&vqr),
            "{ctx}: without a donation, ⌊(Y − pcQ)·2069/10⁴⌋ (+1)"
        );
    }
    assert!(c.state(&env.svm).is_migrated(), "{ctx}");
    let (_, _, pcb1, pcq1) = pc_fields(&env.svm, c);
    assert_eq!((pcb1, pcq1), (pcb, pcq), "{ctx}: the curve keeps pc*");
}

/// A third party on `Near166` funded for the largest synthetic parts.
fn near166_user() -> (Env, PumpCurve, CurveUser) {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near166);
    let u = CurveUser::new(&mut env, &c);
    env.fund(&u.kp.pubkey(), 2_000_000_000_000);
    (env, c, u)
}

/// T-PUMP-MATH-V3 (b) and (d), no donation: a third party's `buy_v3(rT + δ)` on `near166` for δ
/// from one raw unit to 100e12 (24-byte data, and the 25-byte `partial_fill = [false]` form on
/// every other δ) follows §5.2's v3 charge model, then `migrate_v2` creates the pool `(X₁, Y₁)`.
#[test]
fn v3_synthetic_buy_past_rt_follows_the_charge_model() {
    let mut ed = ExactDivision::default();
    for (i, delta) in [
        1,
        1_000_000_000,
        1_000_000_000_000,
        10_000_000_000_000,
        50_000_000_000_000,
        100_000_000_000_000,
    ]
    .into_iter()
    .enumerate()
    {
        let (mut env, c, u) = near166_user();
        let partial = (i % 2 == 1).then_some(false);
        synthetic_buy(&mut env, &c, &u, delta, partial, &mut ed);
        migrate_and_check(&mut env, &c, &format!("(d) δ {delta}, no donation"));
    }
    // Exact divisions by construction: the pool part at δ = X_s/2 (`Y_s·δ/(X_s − δ) = Y_s`), and
    // the curve part at `rT` a multiple of 279.9e12/2¹¹ with `vQ` a multiple of 2¹¹.
    let (mut env, c, u) = near166_user();
    let st = c.state(&env.svm);
    let x_s = read::token_amount(&env.svm, &c.curve_base_ata).unwrap() - st.real_token_reserves;
    synthetic_buy(&mut env, &c, &u, x_s / 2, None, &mut ed);
    let (mut env, c, u) = near166_user();
    let st = c.state(&env.svm);
    let ot = st.virtual_token_reserves - st.real_token_reserves;
    assert_eq!(ot % 2_048, 0);
    let rt = (st.real_token_reserves / (ot / 2_048)) * (ot / 2_048);
    let k = u128::from(st.virtual_token_reserves) * u128::from(st.virtual_quote_reserves);
    let vq = u64::try_from(k.div_ceil(u128::from(rt + ot)))
        .unwrap()
        .div_ceil(2_048)
        * 2_048;
    set_state(&mut env, &c, rt, Some(vq));
    assert!(ceil_n(rt, rt + ot, vq).1, "an exact curve part");
    synthetic_buy(&mut env, &c, &u, 10_000_000_000_000, None, &mut ed);
    println!(
        "T-PUMP-MATH-V3 (b): exact divisions {} (pump charged ⌈⌉ on {}, ⌊⌋ + 1 on {})",
        ed.states, ed.ceil, ed.floor_plus_one
    );
    assert_eq!(ed.states, 2);
}

/// T-PUMP-MATH-V3 (b′) and (d): a token donation `D` to `curve_base_ata` before the synthetic
/// buy (a planted holder's `TransferChecked`) — `D` = 1e12, and `D` = 206.9e12 with `δ` >
/// 206.9e12 so that `pcB ≥ 206.9e12` — is priced on `X_s = curve_base_ata − rT`, donation
/// included; `migrate_v2` then seeds the live `curve_base_ata`. A donation after the completion
/// (no pool part priced on it) is seeded too.
#[test]
fn v3_synthetic_buy_prices_a_donated_curve_base_ata() {
    let mut ed = ExactDivision::default();
    for (d, delta) in [
        (1_000_000_000_000, 10_000_000_000_000),
        (206_900_000_000_000, 250_000_000_000_000),
    ] {
        let (mut env, c, u) = near166_user();
        let rt = c.state(&env.svm).real_token_reserves;
        let a0 = read::token_amount(&env.svm, &c.curve_base_ata).unwrap();
        donate(&mut env, &c, d);
        let m = synthetic_buy(&mut env, &c, &u, delta, None, &mut ed);
        assert_eq!(
            u128::from(m.x_s),
            u128::from(a0 + d - rt),
            "X_s counts the donation"
        );
        let (_, _, pcb, _) = pc_fields(&env.svm, &c);
        if d == 206_900_000_000_000 {
            assert!(pcb >= 206_900_000_000_000, "pcB {pcb} ≥ 206.9e12");
        }
        migrate_and_check(&mut env, &c, &format!("(d) D {d} before, δ {delta}"));
    }
    // A donation after the completion.
    let (mut env, c, u) = near166_user();
    synthetic_buy(&mut env, &c, &u, 10_000_000_000_000, None, &mut ed);
    donate(&mut env, &c, 3_000_000_000_000);
    migrate_and_check(&mut env, &c, "(d) D 3e12 after the completion");
    // Control: a donation and a completing buy of exactly rT (no pool part): `pc* = 0`, and
    // migrate_v2 seeds the live curve_base_ata (the donation included), as before the redeploy.
    let (mut env, c, u) = near166_user();
    donate(&mut env, &c, 2_000_000_000_000);
    let rt = c.state(&env.svm).real_token_reserves;
    let ix = u.buy_v3_ix(&env.svm, &c, rt, u64::MAX);
    let meta = ok(u.send(&mut env, ix), "buy_v3(rT)");
    assert_eq!(
        fork::pump_event_names(&meta)
            .into_iter()
            .filter(|n| *n != "other")
            .collect::<Vec<_>>(),
        ["TradeEvent", "CompleteEvent"],
        "no pool part"
    );
    let (_, _, pcb, pcq) = pc_fields(&env.svm, &c);
    assert_eq!((pcb, pcq), (0, 0), "no pool part");
    migrate_and_check(&mut env, &c, "(d) D 2e12, no pool part");
    println!("T-PUMP-MATH-V3 (b′): exact divisions {ed:?}");
}

/// T-PUMP-MATH-V3 (b″), the creator-fee rule on both parts. `B̂_v3 ≥` pump's charge assumes pump
/// prices the pool part, as the curve part, with `fees::pump_curve_at_supply`'s creator rule (`φ_s`,
/// `PumpCurve::synthetic_fees`): the curve's own `creator_fee_bps` when `Global.creator_fee_
/// configurable` is set, the tier's otherwise, and no creator fee for a default creator. Every
/// SOL fixture curve (b) runs on carries `creator_fee_bps` 0 and a creator, and `creator_fee_rules`
/// checks the override on v2 only. Here, as (b) checks it: `near166` patched to 200 bps with the
/// flag on (`φ_s`'s creator 200) and off (the tier's 30), and `legacy115` (no creator; a first
/// `buy_v3` grows it to 166 bytes, its `ivqr` left 0, then it is moved near completion) — each at
/// a small and a large pool part. One lamport under the model fails 6002, both parts' events name
/// the bps charged (a default creator's name the tier's 30 and charge 0), and `B̂_v3` at `φ_s`
/// covers the charge within §5.2's bound.
#[test]
fn v3_synthetic_buy_follows_the_creator_fee_rules() {
    let mut ed = ExactDivision::default();
    for configurable in [true, false] {
        for delta in [1_000_000_000_000, 100_000_000_000_000] {
            let (mut env, c, u) = near166_user();
            assert!(!c.state(&env.svm).creator_is_default());
            assert_eq!(c.state(&env.svm).creator_fee_bps, 0, "the fixture's");
            fork::set_curve_creator_fee_bps(&mut env.svm, &c.curve, 200);
            fork::set_global_creator_fee_configurable(&mut env.svm, configurable);
            let want = if configurable { 200 } else { 30 };
            let phi_s = c.synthetic_fees(&env.svm).expect("φ_s");
            assert_eq!(
                phi_s,
                FeeBps::new(0, 95, want),
                "configurable {configurable}"
            );
            let m = synthetic_buy(&mut env, &c, &u, delta, None, &mut ed);
            assert_eq!(
                (m.fees1.1, m.fees2.1),
                (fee_up(m.n1, want), fee_up(m.n2, want)),
                "configurable {configurable}: both parts at creator bps {want}"
            );
        }
    }
    for delta in [1_000_000_000_000, 100_000_000_000_000] {
        let (mut env, c) = Env::fork_pump_coin(PumpCoin::Legacy115);
        assert!(c.state(&env.svm).creator_is_default());
        let u = CurveUser::new(&mut env, &c);
        env.fund(&u.kp.pubkey(), 2_000_000_000_000);
        let ix = u.buy_v3_ix(&env.svm, &c, 1_000_000_000, u64::MAX);
        ok(u.send(&mut env, ix), "legacy115: a first buy_v3");
        assert_eq!(c.data_len(&env.svm), fork::CURVE_LEN_2026_10_08);
        assert_eq!(pc_fields(&env.svm, &c).1, 0, "the growth leaves ivqr 0");
        set_state(&mut env, &c, 30_000_000_000_000, None);
        let phi_s = c.synthetic_fees(&env.svm).expect("φ_s");
        assert_eq!(phi_s, FeeBps::new(0, 95, 0), "no creator");
        let m = synthetic_buy(&mut env, &c, &u, delta, None, &mut ed);
        assert_eq!(
            (m.fees1.1, m.fees2.1),
            (0, 0),
            "no creator fee on either part"
        );
    }
    println!("T-PUMP-MATH-V3 (b″): exact divisions {ed:?}");
}

/// T-PUMP-MATH-V3 (c) and (d): `buy_exact_quote_in_v3` with more than the curve's remainder buys
/// `rT` on the curve and, with the rest, `out = ⌊(in − 1)·X_s/(Y_s + in − 1)⌋` from the
/// would-be pool (`in` = the pool part's `quote_in`); then `migrate_v2` creates `(X₁, Y₁)`.
#[test]
fn v3_buy_exact_quote_in_past_rt_buys_the_rest_from_the_would_be_pool() {
    for extra in [50_000_000, 5_000_000_000, 40_000_000_000] {
        let (mut env, c, u) = near166_user();
        let ctx = format!("buy_exact_quote_in_v3 rT's charge + {extra}");
        let bb = CurveUser::v3_buyback_recipient(&env.svm);
        let fees = c.fees(&env.svm).unwrap();
        let fee_m = migration_fee(&env.svm);
        let s0 = snap(&env.svm, &c, &u, &bb);
        let st = s0.st;
        let rt = st.real_token_reserves;
        let (n1, _) = ceil_n(rt, st.virtual_token_reserves, st.virtual_quote_reserves);
        let spend = charged(n1, &fees).0 + extra;
        let ix = u.buy_exact_quote_in_v3_ix(&env.svm, &c, spend, 1);
        let meta = ok(u.send(&mut env, ix), &ctx);
        let ev = one_trade(&meta);
        assert_eq!(ev.token_amount, rt, "{ctx}: the curve part buys rT");
        assert_eq!(ev.ix_name, "buy_exact_quote_in", "{ctx}");
        let n1 = ev.sol_amount;
        let (c1, p1, cr1) = charged(n1, &fees);
        assert_eq!((ev.fee, ev.creator_fee), (p1, cr1), "{ctx}");
        let post = fork::pump_post_complete_buy_events(&meta).remove(0);
        let (x_s, y_s) = (s0.curve_tokens - rt, st.real_quote_reserves + n1 - fee_m);
        assert_eq!(
            (
                post.pool_base_reserves_before,
                post.pool_quote_reserves_before
            ),
            (x_s, y_s),
            "{ctx}"
        );
        let qin = post.quote_in;
        let out = u128::from(qin - 1) * u128::from(x_s) / u128::from(y_s + qin - 1);
        assert_eq!(
            u128::from(post.base_out),
            out,
            "{ctx}: out = ⌊(in−1)·X_s/(Y_s+in−1)⌋"
        );
        let (c2, p2, cr2) = charged(qin, &fees);
        assert_eq!((post.fee, post.creator_fee), (p2, cr2), "{ctx}");
        let s1 = snap(&env.svm, &c, &u, &bb);
        assert_eq!(s0.user - s1.user, c1 + c2, "{ctx}: user paid both parts");
        assert!(c1 + c2 <= spend, "{ctx}");
        assert_eq!(s1.user_tokens - s0.user_tokens, rt + post.base_out, "{ctx}");
        let (_, _, pcb, pcq) = pc_fields(&env.svm, &c);
        assert_eq!((pcb, pcq), (post.base_out, qin), "{ctx}");
        // How `in` follows from the remainder: v2's exact-quote-in rule on it (recorded).
        let rest = spend - c1;
        let model = fork::pump_exact_quote_in(rest, x_s, y_s, &fees);
        println!(
            "{ctx}: remainder {rest}, quote_in {qin} (v2 exact-in rule on the remainder: {:?}), \
             out {}, unspent {}",
            model.map(|m| (m.net, m.tokens)),
            post.base_out,
            spend - c1 - c2
        );
        migrate_and_check(&mut env, &c, &format!("(d) after (c) {extra}"));
    }
}

/// T-PUMP-MATH-V3 (e), the tier tripwire. The market caps a synthetic buy could rate its parts at
/// — the curve before the trade, the curve at completion, the would-be pool before and after the
/// pool part, each at the mint's live supply, and the curve before the trade at the fixed 10¹⁵
/// supply where the supply was burned — each get a tier of their own in a synthetic pump
/// `FeeConfig`: thresholds at the midpoints between neighbouring candidates and as far below the
/// lowest and above the highest again, so that a bottom tier (from 0) and a top tier hold no
/// candidate. Every tier, the stable tiers (the same thresholds, other bps), `flat_fees` and
/// `exotic_flat_fees` carry a protocol bps no other entry carries, so the bps each part's event
/// names tells which schedule and which market cap pump rated it at. The curve part is charged at
/// the curve's pre-trade tier (as v2), the pool part at the candidate pinned here; the user pays
/// exactly the charge, and `B̂_v3` with that one schedule on both legs covers it within §5.2's
/// bound. That schedule is short.win's synthetic `φ_s` since P1 (correction (a):
/// `fees::pump_curve_at_supply` on the mint's live supply, `PumpCurve::synthetic_fees`). The
/// existing curve path's `φ` (`fees::pump_curve`, the fixed 10¹⁵, unchanged for `q ≤ rT`)
/// resolves the same pre-trade curve to another tier on this burned coin, and `B̂_v3` at it would
/// fall below the charge — G0's 59,057,853-lamport finding, recorded here (the `q ≤ rT` side is
/// `tripwire_the_curve_tier_market_cap_uses_the_live_mint_supply`'s).
#[test]
fn v3_tier_tripwire_pins_the_pool_parts_market_cap() {
    let (mut env, c, u) = near166_user();
    let bb = CurveUser::v3_buyback_recipient(&env.svm);
    let st = c.state(&env.svm);
    let fee_m = migration_fee(&env.svm);
    let supply = {
        let m = env.svm.get_account(&c.mint).unwrap();
        u128::from(
            shortwin_venue_state::mint::read_base(&m.owner.to_bytes(), &m.data)
                .unwrap()
                .supply,
        )
    };
    let a = read::token_amount(&env.svm, &c.curve_base_ata).unwrap();
    let rt = st.real_token_reserves;
    let delta = 50_000_000_000_000;
    let (n1, exact1) = ceil_n(rt, st.virtual_token_reserves, st.virtual_quote_reserves);
    let (x_s, y_s) = (a - rt, st.real_quote_reserves + n1 - fee_m);
    let (n2, _) = ceil_n(delta, x_s, y_s);
    let cap = |y: u64, x: u64| u128::from(y) * supply / u128::from(x);
    let mut cands = vec![
        (
            "curve before",
            cap(st.virtual_quote_reserves, st.virtual_token_reserves),
        ),
        (
            "curve at completion",
            cap(
                st.virtual_quote_reserves + n1,
                st.virtual_token_reserves - rt,
            ),
        ),
        ("pool before", cap(y_s, x_s)),
        ("pool after", cap(y_s + n2, x_s - delta)),
    ];
    let fixed = mcap(&st);
    if fixed != cands[0].1 {
        cands.push(("curve before at 10¹⁵", fixed));
    }
    let mut sorted: Vec<u128> = cands.iter().map(|(_, m)| *m).collect();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), cands.len(), "distinct candidates: {cands:?}");
    let (lo, hi) = (sorted[0], sorted[sorted.len() - 1]);
    // A tier holds the caps from its threshold up to the next one's: each candidate alone.
    let mut thresholds = vec![0, lo - (sorted[1] - lo).min(lo) / 2];
    thresholds.extend(sorted.windows(2).map(|w| (w[0] + w[1]).div_ceil(2)));
    thresholds.push(hi + (hi - sorted[sorted.len() - 2]) / 2 + 1);
    let tiers: Vec<fork::TierSpec> = thresholds
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let i = u64::try_from(i).unwrap();
            (*t, [0, 90 - 10 * i, 28 - 3 * i])
        })
        .collect();
    let stable: Vec<fork::TierSpec> = tiers
        .iter()
        .map(|(t, f)| (*t, [f[0], f[1] - 5, f[2] - 1]))
        .collect();
    let (flat, exotic) = ([0, 99, 33], [0, 97, 31]);
    fork::set_fee_config(
        &mut env.svm,
        &PUMP_FEE_CONFIG,
        flat,
        &tiers,
        &stable,
        exotic,
    );
    let tier_of = |m: u128| {
        tiers
            .iter()
            .rev()
            .find(|(t, _)| *t <= m)
            .map(|(_, f)| FeeBps::new(f[0], f[1], f[2]))
            .unwrap()
    };
    let mut protocol_bps: Vec<u64> = tiers
        .iter()
        .chain(&stable)
        .map(|(_, f)| f[1])
        .chain([flat[1], exotic[1]])
        .collect();
    protocol_bps.sort_unstable();
    protocol_bps.dedup();
    assert_eq!(protocol_bps.len(), 2 * tiers.len() + 2, "one bps per entry");
    // What a part's protocol bps says it was rated at.
    let named = |bps: u64| -> String {
        if bps == flat[1] {
            return "flat_fees".into();
        }
        if bps == exotic[1] {
            return "exotic_flat_fees".into();
        }
        if let Some(i) = stable.iter().position(|(_, f)| f[1] == bps) {
            return format!("stable tier {i}");
        }
        let names: Vec<&str> = cands
            .iter()
            .filter(|(_, m)| tier_of(*m).protocol == bps)
            .map(|(n, _)| *n)
            .collect();
        match tiers.iter().position(|(_, f)| f[1] == bps) {
            Some(0) => "the bottom tier (below every candidate)".into(),
            Some(i) if i == tiers.len() - 1 => "the top tier (above every candidate)".into(),
            Some(i) => format!("tier {i} ({names:?})"),
            None => format!("no entry of the FeeConfig ({bps} bps)"),
        }
    };
    // short.win's fees on the pre-trade curve under these tiers: the curve path's `φ`
    // (`PumpCurve::fees` = `fees::pump_curve`, the market cap at the fixed 10¹⁵ supply) and the
    // synthetic legs' `φ_s` (`PumpCurve::synthetic_fees` = `fees::pump_curve_at_supply`, the
    // mint's live supply).
    let phi = c.fees(&env.svm).expect("φ");
    let phi_s = c.synthetic_fees(&env.svm).expect("φ_s");
    let s0 = snap(&env.svm, &c, &u, &bb);
    let ix = u.buy_v3_ix(&env.svm, &c, rt + delta, u64::MAX);
    let meta = ok(
        u.send(&mut env, ix),
        "synthetic buy under the tripwire tiers",
    );
    let s1 = snap(&env.svm, &c, &u, &bb);
    let ev = one_trade(&meta);
    let post = fork::pump_post_complete_buy_events(&meta).remove(0);
    println!(
        "T-PUMP-MATH-V3 (e): supply {supply}; candidates {cands:?}; tiers {tiers:?}; stable \
         {stable:?}; flat {flat:?}; exotic {exotic:?}; the curve part at {} / {} bps ({}), the \
         pool part at {} / {} bps ({})",
        ev.fee_basis_points,
        ev.creator_fee_basis_points,
        named(ev.fee_basis_points),
        post.fee_basis_points,
        post.creator_fee_basis_points,
        named(post.fee_basis_points)
    );
    // The curve part: the pre-trade curve's tier at the live supply, as v2.
    let curve_fees = tier_of(cands[0].1);
    assert_eq!(
        (ev.fee_basis_points, ev.creator_fee_basis_points),
        (curve_fees.protocol, curve_fees.creator),
        "the curve part's tier is {}'s",
        cands[0].0
    );
    // The pool part: pinned (the tripwire flips if pump changes the market cap it rates it at).
    let pool_fees = tier_of(cands[POOL_PART_TIER_CANDIDATE].1);
    assert_eq!(
        (post.fee_basis_points, post.creator_fee_basis_points),
        (pool_fees.protocol, pool_fees.creator),
        "the pool part's tier is {}'s",
        cands[POOL_PART_TIER_CANDIDATE].0
    );
    // The quote follows.
    assert!(
        ev.sol_amount == n1 || (exact1 && ev.sol_amount == n1 + 1),
        "n₁ {} vs ⌈⌉ {n1}",
        ev.sol_amount
    );
    let n1 = ev.sol_amount;
    let y_s = st.real_quote_reserves + n1 - fee_m;
    let (n2, exact2) = ceil_n(delta, x_s, y_s);
    assert!(
        post.quote_in == n2 || (exact2 && post.quote_in == n2 + 1),
        "n₂ {} vs ⌈⌉ {n2}",
        post.quote_in
    );
    let n2 = post.quote_in;
    let charge = charged(n1, &curve_fees).0 + charged(n2, &pool_fees).0;
    assert_eq!(
        (ev.fee, ev.creator_fee, post.fee, post.creator_fee),
        (
            fee_up(n1, curve_fees.protocol),
            fee_up(n1, curve_fees.creator),
            fee_up(n2, pool_fees.protocol),
            fee_up(n2, pool_fees.creator)
        )
    );
    assert_eq!(
        s0.user - s1.user,
        charge + growth(&env.svm, &s0),
        "the user paid the charge"
    );
    // §5.2: one schedule prices both legs of B̂_v3. At pump's (the tier of the curve's pre-trade
    // market cap at the live supply) it covers the charge within the bound — and that is
    // short.win's synthetic φ_s since P1 (correction (a)).
    assert_eq!(
        phi_s, curve_fees,
        "φ_s is the tier pump charged on both parts"
    );
    let b = b_hat_v3(&st, a, fee_m, delta, &phi_s);
    let bound = v3_bound(delta, x_s, &curve_fees);
    assert!(
        b >= charge && b - charge <= bound,
        "B̂_v3 at φ_s {b} vs charge {charge} (bound {bound})"
    );
    println!(
        "T-PUMP-MATH-V3 (e): charge {charge}, the user paid {}; B̂_v3 at short.win's φ_s {} / {} \
         bps (pump's tier) {b} (+{}, bound {bound})",
        s0.user - s1.user,
        phi_s.protocol,
        phi_s.creator,
        b - charge
    );
    // The record of G0's finding: the curve path's φ (unchanged for q ≤ rT, M5's open item) rates
    // the same pre-trade curve at the fixed 10¹⁵ supply, which on this burned coin falls in
    // another tier; B̂_v3 at it would undercut pump's charge (an exit's `max_in` below it:
    // 6002). Without a burn the two coincide.
    let at_fixed = cands
        .iter()
        .find(|(n, _)| *n == "curve before at 10¹⁵")
        .map(|(_, m)| tier_of(*m));
    match at_fixed {
        Some(tier) => {
            assert_eq!(phi, tier, "φ is the 10¹⁵ market cap's tier");
            assert_ne!(phi, phi_s, "φ is not the tier pump charged");
            let b_phi = b_hat_v3(&st, a, fee_m, delta, &phi);
            println!(
                "T-PUMP-MATH-V3 (e): the curve path's φ {} / {} bps (the 10¹⁵ market cap's tier): \
                 B̂_v3 at it would be {b_phi}, {} under the charge",
                phi.protocol,
                phi.creator,
                i128::from(charge) - i128::from(b_phi)
            );
            assert!(b_phi < charge, "B̂_v3 at φ {b_phi} vs charge {charge}");
        }
        None => assert_eq!(phi, phi_s, "an unburned supply: one market cap"),
    }
}

/// Which of (e)'s candidate market caps rates the pool part (index into its `cands`): the curve's
/// own pre-trade market cap, the curve part's.
const POOL_PART_TIER_CANDIDATE: usize = 0;

/// T-PUMP-MATH-V3 (f): every v3 trade — `buy_v3`, `buy_exact_quote_in_v3`, `sell_v3` — refuses a
/// cashback coin (6094 `CashbackCoinNotSupported`).
#[test]
fn v3_trades_refuse_a_cashback_coin() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Cashback166);
    assert!(c.state(&env.svm).is_cashback_coin);
    let u = CurveUser::new(&mut env, &c);
    let ix = u.buy_v3_ix(&env.svm, &c, 1_000_000_000_000, u64::MAX);
    let res = u.send(&mut env, ix);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_CASHBACK_COIN_NOT_SUPPORTED),
        "buy_v3: {}",
        describe(&res)
    );
    let ix = u.buy_exact_quote_in_v3_ix(&env.svm, &c, 100_000_000, 1);
    let res = u.send(&mut env, ix);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_CASHBACK_COIN_NOT_SUPPORTED),
        "buy_exact_quote_in_v3: {}",
        describe(&res)
    );
    u.plant(&mut env, 1_000_000_000);
    let ix = u.sell_v3_ix(&env.svm, &c, 1_000_000_000, 0);
    let res = u.send(&mut env, ix);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_CASHBACK_COIN_NOT_SUPPORTED),
        "sell_v3: {}",
        describe(&res)
    );
}

/// T-PUMP-MATH-V3 (g): the synthetic path is a 2026-10-08 dependency — on pump's 2026-10-02
/// build a `buy_v3` past `rT` reverts with `NotEnoughTokensToBuy` (6021).
#[test]
fn v3_buy_past_rt_reverts_on_the_2026_10_02_build() {
    let (mut env, c, u) = near166_user();
    fork::load_pump_deploy(&mut env.svm, PumpProgram::Pump, PumpDeploy::Oct2026);
    env.warp(1);
    assert_eq!(
        env.pump_build_running(PumpProgram::Pump),
        PumpDeploy::Oct2026
    );
    let rt = c.state(&env.svm).real_token_reserves;
    let ix = u.buy_v3_ix(&env.svm, &c, rt + 1_000_000_000_000, u64::MAX);
    let res = u.send(&mut env, ix);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_NOT_ENOUGH_TOKENS_TO_BUY),
        "{}",
        describe(&res)
    );
}

// ---- the 2026-10-13 build against the 2026-10-08 build (the re-pin to 456,494,698) ------------

/// Two forks of one coin driven in lockstep: `old` runs pump's 2026-10-08 build
/// ([`PumpDeploy::Oct2026Synth`]), `new` its 2026-10-13 build ([`PumpDeploy::CURRENT`]); PumpSwap,
/// pump-fees and every account start equal. Each fork is asked which build executes
/// ([`Env::pump_build_running`], whose `create_v2` depth probe tells the two apart), not only
/// which one its ProgramData holds, so the comparison cannot pass on one ELF run twice.
struct Twin {
    old: Env,
    new: Env,
    c: PumpCurve,
    /// Transactions compared, those that failed (on both), and the compute units of the
    /// successful ones (equal on both).
    sent: usize,
    failed: usize,
    units: u64,
}

impl Twin {
    fn fork(coin: PumpCoin) -> Self {
        let (mut old, c) = Env::fork_pump_coin(coin);
        fork::load_pump_deploy(&mut old.svm, PumpProgram::Pump, PumpDeploy::Oct2026Synth);
        let (mut new, _) = Env::fork_pump_coin(coin);
        assert_eq!(
            PumpDeploy::held(&old.svm, PumpProgram::Pump),
            Some(PumpDeploy::Oct2026Synth)
        );
        assert_eq!(
            PumpDeploy::held(&new.svm, PumpProgram::Pump),
            Some(PumpDeploy::Oct2026Chain)
        );
        // The probes run alike on both forks: refused transactions, which leave every account
        // as they found it but for the fee the harness payer pays.
        for (env, deploy) in [
            (&mut old, PumpDeploy::Oct2026Synth),
            (&mut new, PumpDeploy::Oct2026Chain),
        ] {
            env.warp(1);
            assert_eq!(
                env.pump_build_running(PumpProgram::Pump),
                deploy,
                "{coin:?}: the build that executes"
            );
        }
        Self {
            old,
            new,
            c,
            sent: 0,
            failed: 0,
            units: 0,
        }
    }

    /// The same byte-level change on both forks.
    fn both(&mut self, f: impl Fn(&mut Env)) {
        f(&mut self.old);
        f(&mut self.new);
    }

    /// A keypair funded with `lamports` on both forks, with its base-token ATA and pump UVA
    /// (paid by itself, so no fork-specific key enters an account).
    fn user(&mut self, lamports: u64) -> (Keypair, Pubkey) {
        let kp = Keypair::new();
        let k = kp.pubkey();
        self.both(|env| env.fund(&k, lamports));
        let program = self.c.token_program.id();
        let ixs = [
            fork::create_ata_idempotent_ix(&k, &k, &self.c.mint, &program),
            fork::pump_init_uva_ix(&k, &k),
        ];
        self.send("user setup", &ixs, &[&kp], None);
        (kp, pda::ata(&k, &self.c.mint, &program))
    }

    /// [`Twin::compare`], and the custom code is `expect` (`None`: success).
    #[track_caller]
    fn send(&mut self, what: &str, ixs: &[Instruction], signers: &[&Keypair], expect: Option<u32>) {
        let code = self.compare(what, ixs, signers);
        assert_eq!(code, expect, "{what}");
    }

    /// Sends `ixs` on both forks and asserts one outcome: success on both or the same
    /// `TransactionError`, the same logs once the "consumed … compute units" lines are dropped
    /// (every `msg!`, event and error message), the same compute units on success, and every
    /// account the instructions name byte-identical afterwards. Returns the custom code.
    #[track_caller]
    fn compare(&mut self, what: &str, ixs: &[Instruction], signers: &[&Keypair]) -> Option<u32> {
        let a = self.old.send(ixs, signers);
        let b = self.new.send(ixs, signers);
        let view = |r: &TxResult| match r {
            Ok(m) => (None, m.logs.clone(), m.compute_units_consumed),
            Err(f) => (
                Some(format!("{:?}", f.err)),
                f.meta.logs.clone(),
                f.meta.compute_units_consumed,
            ),
        };
        let ((ea, la, ua), (eb, lb, ub)) = (view(&a), view(&b));
        let strip = |l: Vec<String>| -> Vec<String> {
            l.into_iter()
                .filter(|s| !(s.contains(" consumed ") && s.ends_with(" compute units")))
                .collect()
        };
        assert_eq!(
            ea,
            eb,
            "{}: error\n{}\n----\n{}",
            what,
            describe(&a),
            describe(&b)
        );
        assert_eq!(strip(la), strip(lb), "{what}: logs");
        if ea.is_none() {
            assert_eq!(ua, ub, "{what}: compute units");
            self.units += ua;
        } else {
            self.failed += 1;
        }
        self.sent += 1;
        let mut keys: Vec<Pubkey> = ixs
            .iter()
            .flat_map(|ix| ix.accounts.iter().map(|m| m.pubkey))
            .collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            assert_eq!(
                self.old.svm.get_account(&k),
                self.new.svm.get_account(&k),
                "{what}: account {k}"
            );
        }
        custom_code(&a)
    }
}

/// The re-pin's differential (gate G0b, the 2026-10-13 redeploy at 456,494,698): pump's
/// 2026-10-13 build runs every path short.win uses and every third-party move the harness makes
/// exactly as its 2026-10-08 build — the same errors, logs (every event, `msg!` and error
/// message), compute units, and account bytes after each transaction:
///
/// - on each live SOL curve of both state groups (49 to 166 bytes, SPL Token and Token-2022;
///   the shorter ones grow to 166 on their first trade): `buy_v2`, `sell_v2`,
///   `buy_exact_quote_in_v2` (short.win's legs today) and `buy_v3`, `sell_v3`,
///   `buy_exact_quote_in_v3` (its legs after P2), then each refusal those legs can meet — 6002 /
///   6003 one unit past a bound, 6042 below a minimum out, 6021 past `rT` on v2;
/// - on `near166`: the synthetic migration (`buy_v3(rT + δ)`, then 6005 on every trade of the
///   complete curve) and `migrate_v2`, with and without a token donation to `curve_base_ata`;
///   `buy_exact_quote_in_v3` past `rT`;
/// - on `cashback166`: 6094 on every v3 trade, v2 trades and `claim_cashback`; the mayhem, the
///   complete unmigrated and the migrated synthetic curves: whatever each answers, the same.
///
/// The ELF diff (`tests/fixtures/README.md`) found only `create_v2` and its quote derivation
/// changed; short.win never calls them.
#[test]
fn the_2026_10_13_build_runs_short_wins_paths_as_the_2026_10_08_build() {
    const SOL: u64 = 1_000_000_000;
    let (mut sent, mut failed, mut units) = (0, 0, 0);
    let mut tally = |t: &Twin| {
        sent += t.sent;
        failed += t.failed;
        units += t.units;
    };
    for coin in PumpCoin::LIVE_SOL
        .into_iter()
        .chain(PumpCoin::LIVE_SOL_2026_10_13)
    {
        let mut t = Twin::fork(coin);
        let c = t.c;
        let (u, ata) = t.user(10_000 * SOL);
        let up = u.pubkey();
        let (fr, bb) = (
            fork::pump_fee_recipient(&t.new.svm, 1),
            fork::pump_buyback_fee_recipient(&t.new.svm, 1),
        );
        let v2 = |t: &Twin, kind: PumpV2, a: u64, b: u64| {
            c.v2_ix(&t.new.svm, kind, &up, &ata, &fr, &bb, a, b)
        };
        let v3 = |kind: fork::PumpV3, a: u64, b: u64| c.v3_ix(kind, &up, &ata, &bb, a, b, None);
        let q = (c.state(&t.new.svm).real_token_reserves / 20).max(1);
        let ctx = |s: &str| format!("{coin:?}: {s}");
        let ix = v2(&t, PumpV2::Buy, q, u64::MAX);
        t.send(&ctx("buy_v2"), &[ix], &[&u], None);
        let ix = v2(&t, PumpV2::Sell, q / 2, 0);
        t.send(&ctx("sell_v2"), &[ix], &[&u], None);
        // (an exact-in buy with `min_tokens_out` = 0 is `BuyZeroAmount`, 6020, on both builds)
        let ix = v2(&t, PumpV2::BuyExactQuoteIn, SOL / 3, 1);
        t.send(&ctx("buy_exact_quote_in_v2"), &[ix], &[&u], None);
        let ix = v3(fork::PumpV3::Buy, q / 2, u64::MAX);
        t.send(&ctx("buy_v3"), &[ix], &[&u], None);
        let ix = v3(fork::PumpV3::Sell, q / 4, 0);
        t.send(&ctx("sell_v3"), &[ix], &[&u], None);
        let ix = v3(fork::PumpV3::BuyExactQuoteIn, SOL / 5, 1);
        t.send(&ctx("buy_exact_quote_in_v3"), &[ix], &[&u], None);
        let ix = v3(fork::PumpV3::Buy, q / 2, u64::MAX);
        t.send(
            &ctx("buy_v3 with partial_fill"),
            &[spump::with_partial_fill(ix, false)],
            &[&u],
            None,
        );
        // The refusals: one unit past each bound.
        let ix = v2(&t, PumpV2::Buy, q, 1);
        t.send(&ctx("buy_v2 over max"), &[ix], &[&u], Some(6002));
        let ix = v2(&t, PumpV2::Sell, q / 8, u64::MAX);
        t.send(&ctx("sell_v2 under min"), &[ix], &[&u], Some(6003));
        let ix = v3(fork::PumpV3::Buy, q, 1);
        t.send(&ctx("buy_v3 over max"), &[ix], &[&u], Some(6002));
        let ix = v3(fork::PumpV3::Sell, q / 8, u64::MAX);
        t.send(&ctx("sell_v3 under min"), &[ix], &[&u], Some(6003));
        let ix = v3(fork::PumpV3::BuyExactQuoteIn, SOL / 5, u64::MAX);
        t.send(
            &ctx("buy_exact_quote_in_v3 under min"),
            &[ix],
            &[&u],
            Some(6042),
        );
        let rt = c.state(&t.new.svm).real_token_reserves;
        let ix = v2(&t, PumpV2::Buy, rt + 1, u64::MAX);
        t.send(
            &ctx("buy_v2 past rT"),
            &[ix],
            &[&u],
            Some(fork::PUMP_ERR_NOT_ENOUGH_TOKENS_TO_BUY),
        );
        assert_eq!(
            c.data_len(&t.new.svm),
            fork::CURVE_LEN_2026_10_08,
            "{coin:?}"
        );
        tally(&t);
    }

    // The synthetic migration on near166, without and with a donation to curve_base_ata (before
    // the completion, and after it), and `migrate_v2`.
    for donation in [0, 1_000_000_000_000] {
        let mut t = Twin::fork(PumpCoin::Near166);
        let c = t.c;
        let (u, ata) = t.user(2_000 * SOL);
        let bb = fork::pump_buyback_fee_recipient(&t.new.svm, 1);
        let program = c.token_program.id();
        let donate = |t: &mut Twin, d: u64| {
            let holder = Keypair::new();
            let h = holder.pubkey();
            t.both(|env| env.fund(&h, SOL));
            let ix = fork::create_ata_idempotent_ix(&h, &h, &c.mint, &program);
            t.send("holder ATA", &[ix], &[&holder], None);
            let src = pda::ata(&h, &c.mint, &program);
            t.both(|env| fork::set_token_amount(&mut env.svm, &src, d));
            let decimals = t.new.svm.get_account(&c.mint).unwrap().data[44];
            let mut data = vec![12u8];
            data.extend_from_slice(&d.to_le_bytes());
            data.push(decimals);
            let ix = Instruction {
                program_id: program,
                accounts: vec![
                    AccountMeta::new(src, false),
                    AccountMeta::new_readonly(c.mint, false),
                    AccountMeta::new(c.curve_base_ata, false),
                    AccountMeta::new_readonly(h, true),
                ],
                data,
            };
            t.send("donation", &[ix], &[&holder], None);
        };
        if donation > 0 {
            donate(&mut t, donation);
        }
        let rt = c.state(&t.new.svm).real_token_reserves;
        let ix = c.v3_ix(
            fork::PumpV3::Buy,
            &u.pubkey(),
            &ata,
            &bb,
            rt + 10_000_000_000_000,
            u64::MAX,
            None,
        );
        let ctx = format!("near166, donation {donation}");
        t.send(&format!("{ctx}: buy_v3(rT + δ)"), &[ix], &[&u], None);
        assert!(c.state(&t.new.svm).complete, "{ctx}: complete");
        for (kind, bound) in [(fork::PumpV3::Buy, u64::MAX), (fork::PumpV3::Sell, 0)] {
            let ix = c.v3_ix(kind, &u.pubkey(), &ata, &bb, 1_000_000, bound, None);
            t.send(
                &format!("{ctx}: {kind:?} on the complete curve"),
                &[ix],
                &[&u],
                Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE),
            );
        }
        if donation > 0 {
            donate(&mut t, donation);
        }
        let migrator = Keypair::new();
        let m = migrator.pubkey();
        t.both(|env| env.fund(&m, 10 * SOL));
        let mig = spump::migrate_v2_ix(&t.new.svm, &m, &c);
        t.send(
            &format!("{ctx}: migrate_v2"),
            &[spump::cu_limit_ix(600_000), mig],
            &[&migrator],
            None,
        );
        assert!(c.state(&t.new.svm).is_migrated(), "{ctx}");
        let ix = c.v3_ix(
            fork::PumpV3::Buy,
            &u.pubkey(),
            &ata,
            &bb,
            1_000_000,
            u64::MAX,
            None,
        );
        t.send(
            &format!("{ctx}: buy_v3 on the migrated curve"),
            &[ix],
            &[&u],
            Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE),
        );
        tally(&t);
    }

    // buy_exact_quote_in_v3 past rT: the rest from the would-be pool.
    let mut t = Twin::fork(PumpCoin::Near166);
    let c = t.c;
    let (u, ata) = t.user(2_000 * SOL);
    let bb = fork::pump_buyback_fee_recipient(&t.new.svm, 1);
    let ix = c.v3_ix(
        fork::PumpV3::BuyExactQuoteIn,
        &u.pubkey(),
        &ata,
        &bb,
        200 * SOL,
        1,
        None,
    );
    t.send("near166: buy_exact_quote_in_v3 past rT", &[ix], &[&u], None);
    assert!(c.state(&t.new.svm).complete);
    tally(&t);

    // The cashback coin: every v3 trade refused (6094); v2 trades and claim_cashback run.
    let mut t = Twin::fork(PumpCoin::Cashback166);
    let c = t.c;
    let (u, ata) = t.user(100 * SOL);
    let up = u.pubkey();
    let (fr, bb) = (
        fork::pump_fee_recipient(&t.new.svm, 1),
        fork::pump_buyback_fee_recipient(&t.new.svm, 1),
    );
    t.both(|env| fork::set_token_amount(&mut env.svm, &ata, 1_000_000_000));
    for kind in [
        fork::PumpV3::Buy,
        fork::PumpV3::Sell,
        fork::PumpV3::BuyExactQuoteIn,
    ] {
        let (a, b) = match kind {
            fork::PumpV3::Buy => (1_000_000, u64::MAX),
            fork::PumpV3::Sell => (1_000_000, 0),
            fork::PumpV3::BuyExactQuoteIn => (SOL, 1),
        };
        let ix = c.v3_ix(kind, &up, &ata, &bb, a, b, None);
        t.send(
            &format!("cashback166: {kind:?}"),
            &[ix],
            &[&u],
            Some(fork::PUMP_ERR_CASHBACK_COIN_NOT_SUPPORTED),
        );
    }
    let q = c.state(&t.new.svm).real_token_reserves / 50;
    let ix = c.v2_ix(&t.new.svm, PumpV2::Buy, &up, &ata, &fr, &bb, q, u64::MAX);
    t.send("cashback166: buy_v2", &[ix], &[&u], None);
    let ix = c.v2_ix(&t.new.svm, PumpV2::Sell, &up, &ata, &fr, &bb, q / 2, 0);
    t.send("cashback166: sell_v2", &[ix], &[&u], None);
    let code = t.compare(
        "cashback166: claim_cashback",
        &[spump::claim_cashback_ix(&up)],
        &[],
    );
    println!("cashback166: claim_cashback → {code:?} on both builds");
    tally(&t);

    // The curves short.win refuses or that are no longer live: the same answer on both builds.
    for coin in [
        PumpCoin::Mayhem166,
        PumpCoin::SyntheticStuck,
        PumpCoin::GraduatedSynthetic,
    ] {
        let mut t = Twin::fork(coin);
        let c = t.c;
        let (u, ata) = t.user(100 * SOL);
        let up = u.pubkey();
        let (fr, bb) = (
            fork::pump_fee_recipient(&t.new.svm, 1),
            fork::pump_buyback_fee_recipient(&t.new.svm, 1),
        );
        for kind in [PumpV2::Buy, PumpV2::Sell] {
            let b = if kind == PumpV2::Sell { 0 } else { u64::MAX };
            let ix = c.v2_ix(&t.new.svm, kind, &up, &ata, &fr, &bb, 1_000_000, b);
            let code = t.compare(&format!("{coin:?}: {kind:?} v2"), &[ix], &[&u]);
            println!("{coin:?}: {kind:?} v2 → {code:?} on both builds");
        }
        for kind in [fork::PumpV3::Buy, fork::PumpV3::Sell] {
            let b = if kind == fork::PumpV3::Sell {
                0
            } else {
                u64::MAX
            };
            let ix = c.v3_ix(kind, &up, &ata, &bb, 1_000_000, b, None);
            let code = t.compare(&format!("{coin:?}: {kind:?}"), &[ix], &[&u]);
            println!("{coin:?}: {kind:?} → {code:?} on both builds");
        }
        tally(&t);
    }
    // The total varies from run to run: the users are random keys, whose PDA and ATA bump
    // searches cost different units. Only the per-transaction equality is asserted.
    println!(
        "G0b differential: {sent} transactions on pump's 2026-10-08 and 2026-10-13 builds, \
         {failed} refused on both with the same error; equal logs, accounts and compute units \
         per transaction ({units} CU over the successful ones in this run, random keys)"
    );
    assert!(sent >= 150 && failed >= 60, "{sent} / {failed}");
}

/// What pump's 2026-10-13 redeploy changed, on the dumped ELFs: `create_v2` of a coin quoted in
/// a nested pump coin (short.win never calls `create_v2`). `Q` is the `synthetic_stuck` coin
/// (depth 1, quoted in the pump coin `W5vQ…`, whose curve is the chain's depth-0 root and not in
/// the dump), and a `QuoteControl` admitting no mint stands at its PDA (the dump holds none):
///
/// 1. `max_curve_depth` = 1 (the dumped `Global`'s, slot 456,395,752; the depth probe sets it):
///    the new curve's depth 2 is refused `CurveDepthExceeded` (6105) by both builds, the
///    2026-10-13 one naming its cap of 5;
/// 2. `max_curve_depth` = 2 (mainnet's since slot 456,495,021, so depth-2 coins can be created
///    there): `Q`'s complete, unmigrated curve is refused `QuoteCurveAwaitingMigration` (6107)
///    by both, the 2026-10-13 build's message speaking of the quote chain;
/// 3. `Q`'s curve patched live: the 2026-10-08 build creates the depth-2 coin from `Q`'s curve
///    alone (its `initial_virtual_quote_reserves` derived from `Q`'s stored one); the 2026-10-13
///    build refuses `QuoteChainAccountsRequired` (6109) without the root's curve;
/// 4. with a live depth-0 SOL curve written as the root's (`near166`'s bytes): both create the
///    coin, the 2026-10-08 build ignoring the extra account (the same reserves as in 3), the
///    2026-10-13 build deriving them through the chain from the root's live reserves.
#[test]
fn the_2026_10_13_build_requires_the_quote_chain_in_create_v2() {
    // The new curve's `initial_virtual_quote_reserves` (@142) per build: (Q alone, with root).
    let mut ivqr = Vec::new();
    for deploy in [PumpDeploy::Oct2026Synth, PumpDeploy::Oct2026Chain] {
        let chain = deploy == PumpDeploy::Oct2026Chain;
        let (mut env, q) = Env::fork_pump_coin(PumpCoin::SyntheticStuck);
        fork::load_pump_deploy(&mut env.svm, PumpProgram::Pump, deploy);
        env.warp(1);
        assert_eq!(env.pump_build_running(PumpProgram::Pump), deploy);
        let has = |logs: &[String], s: &str| logs.iter().any(|l| l.contains(s));
        // 1. Too deep at max_curve_depth 1 (the dump's).
        env.warp(1);
        let (code, logs) = env.pump_create_v2_depth_probe();
        assert_eq!(
            code,
            Some(fork::PUMP_ERR_CURVE_DEPTH_EXCEEDED),
            "{deploy:?}"
        );
        assert_eq!(
            has(&logs, fork::PUMP_DEPTH_CAP_MESSAGE),
            chain,
            "{deploy:?}"
        );
        // 2. Deep enough at mainnet's max_curve_depth (2), but Q's curve is complete and not
        //    migrated.
        let qc = fork::pump_quote_control();
        let acc = fork::empty_quote_control(&env.svm);
        env.svm.set_account(qc, acc).unwrap();
        let mut g = env.svm.get_account(&PUMP_GLOBAL).unwrap();
        assert_eq!(
            g.data[fork::PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH],
            1,
            "the dump's"
        );
        g.data[fork::PUMP_OFF_GLOBAL_MAX_CURVE_DEPTH] = 2;
        env.svm.set_account(PUMP_GLOBAL, g).unwrap();
        let create = |env: &mut Env, chain_accounts: &[Pubkey]| {
            env.warp(1);
            let user = env.funded_keypair();
            let mint = Keypair::new();
            let ix = fork::pump_create_v2_ix(
                &user.pubkey(),
                &mint.pubkey(),
                fork::pump_coin_quote_accounts(&mint.pubkey(), &q, chain_accounts),
            );
            let res = env.send(&[spump::cu_limit_ix(400_000), ix], &[&user, &mint]);
            (res, mint.pubkey())
        };
        let (res, _) = create(&mut env, &[]);
        assert_eq!(
            custom_code(&res),
            Some(fork::PUMP_ERR_QUOTE_CURVE_AWAITING_MIGRATION),
            "{deploy:?}: {}",
            describe(&res)
        );
        let logs = res.as_ref().err().unwrap().meta.logs.clone();
        assert_eq!(
            has(&logs, "A quote-chain coin's curve"),
            chain,
            "{deploy:?}"
        );
        // 3. Q live: the 2026-10-08 build reads Q alone, the 2026-10-13 one wants the chain.
        let mut c = env.svm.get_account(&q.curve).unwrap();
        c.data[48] = 0;
        env.svm.set_account(q.curve, c).unwrap();
        let created = |env: &Env, mint: &Pubkey| {
            let d = env.svm.get_account(&pda::bonding_curve(mint)).unwrap().data;
            assert_eq!(
                (d.len(), d[141]),
                (fork::CURVE_LEN_2026_10_08, 2),
                "depth 2"
            );
            u64::from_le_bytes(d[142..150].try_into().unwrap())
        };
        let (res, mint) = create(&mut env, &[]);
        let alone = if chain {
            assert_eq!(
                custom_code(&res),
                Some(fork::PUMP_ERR_QUOTE_CHAIN_ACCOUNTS_REQUIRED),
                "{}",
                describe(&res)
            );
            None
        } else {
            ok(res, "2026-10-08: a depth-2 coin from Q's curve alone");
            Some(created(&env, &mint))
        };
        // 4. With a live depth-0 SOL curve as the root's (near166's bytes at W5vQ…'s PDA).
        let root = pda::bonding_curve(&q.quote_mint);
        let near = fork::fixture("pump_near166_curve").account.clone();
        env.svm.set_account(root, near).unwrap();
        let (res, mint) = create(&mut env, &[root]);
        ok(res, &format!("{deploy:?}: with the root's curve"));
        let with_root = created(&env, &mint);
        println!(
            "{deploy:?}: ivqr of the depth-2 coin: Q alone {alone:?}, with the root {with_root}"
        );
        ivqr.push((alone, with_root));
    }
    assert_eq!(
        ivqr,
        [
            (Some(41_982_750_120_570), 41_982_750_120_570),
            (None, 66_510_807_491_306),
        ],
        "2026-10-08: Q's stored reserves, the root ignored; 2026-10-13: through the chain"
    );
}
