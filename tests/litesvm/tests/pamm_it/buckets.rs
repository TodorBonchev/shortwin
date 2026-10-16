//! PumpSwap's fee buckets on the 2026-10-02 redeploy (M10 pump revalidation; `fork::PumpDeploy::
//! Oct2026`, the build every fork loads; ARCH U30; R-VEN Update 2026-09-30 H2, which ran the same
//! release's unreleased artifacts from `pump-rust-client` 0.2.0):
//!
//! - **the release is deployed:** a third party's `buy_v2` / `sell_v2` keep the protocol fee
//!   (less the buyback slice) and the creator fee in the quote vault, counted in
//!   `Pool.protocol_fees` @271 / `creator_fees` @279 and taken off `virtual_quote_reserves`, so
//!   the price `vault + VQR` excludes them; the 2026-09 build has no such instruction;
//! - **v1 sales are paid from `vault − buckets`:** a `sell` whose payout lies between the two
//!   fails 6063 on the redeploy and executes on the 2026-09 build;
//! - the permissionless sweeps pay the buckets out without moving the price;
//! - **short.win's trades are unchanged:** its v1 `sell` / `buy` on a pool carrying buckets open
//!   and close to the lamport of `quote::sol_leg` on `vault + VQR` (the hooks run), and leave the
//!   buckets as they were;
//! - **what short.win reads since PUMP-2026-10-1** (M10 program batch 4): its real SOL `R` is the
//!   payable `min(vault − buckets, vault + VQR)` — the sale cap and the tier's `R_now` — and the
//!   tier's virtual part the boost `max(VQR + buckets, 0)` off `R_slow` and the burned-LP bound;
//!   the build before it read `min(vault, vault + VQR)` and `max(VQR, 0)` = `boost − buckets`,
//!   overstating each by `min(buckets, boost)`: `the_program_reads_the_payable_sol_under_buckets`;
//!   the open's cap at the payable SOL to the lamport (`ProceedsBelowMin` one lamport above it,
//!   before PumpSwap's 6063); the keeper exits on a pool carrying buckets; a negative-VQR pool
//!   with buckets (`R = vault + VQR`, nothing virtual); buckets above the vault not executable.
//!   The θ_B crossings on the payable `Λ` are `tier_transition_it::pump::
//!   pumpswap_thresholds_on_the_payable_sol_under_fee_buckets`.

use shortwin::events::{Expire, Liquidate};
use shortwin_litesvm_tests::fork::{PumpDeploy, PumpProgram};
use shortwin_litesvm_tests::invariants;
use shortwin_litesvm_tests::pamm::{
    PAMM_ERR_INSUFFICIENT_REAL_QUOTE, POOL_OFF_CREATOR_FEES, POOL_OFF_PROTOCOL_FEES, PammTrade,
    coin_creator_vault_authority, pamm_sweep_ix, plant_buckets, pool_buckets,
    protocol_fee_recipient,
};

use crate::common::*;
use crate::negvqr::host_state;

fn vqr(env: &Env, p: &PammPool) -> i128 {
    p.state(&env.svm).virtual_quote_reserves
}

/// The largest sale whose `quote::sol_leg` payout (capped by the vault only, as the 2026-09
/// build pays) stays at or below `cap`, by bisection on the base amount.
fn sale_paying_at_most(env: &Env, p: &PammPool, cap: u64) -> (u64, u64) {
    let r = p.reserves(&env.svm).expect("reserves");
    let f = pool_fees(env, p);
    let pay = |q: u64| {
        sol_leg::sell_exact_in(q, u128::from(r.base), r.effective_quote, &f, r.real_quote)
            .map(|s| s.proceeds)
    };
    let (mut lo, mut hi) = (1u64, r.base.saturating_mul(1_000));
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        match pay(mid) {
            Some(s) if s <= cap => lo = mid,
            _ => hi = mid,
        }
    }
    (lo, pay(lo).expect("a payout"))
}

/// The fields of a pAMM `BuyEvent` the indexer reads (`services/indexer` `pump_amm_trade`): the
/// pre-trade quote vault, `quote_amount_in_with_lp_fee`, `ix_name` and the trailing
/// `virtual_quote_reserves`.
struct BuyEventView {
    pool_quote: u64,
    quote_net_lp: u64,
    ix_name: String,
    vqr: i128,
}

fn buy_event(meta: &TransactionMetadata) -> BuyEventView {
    let disc = &fork::sha256(b"event:BuyEvent")[..8];
    let raw = events::raw(meta);
    let b = raw
        .iter()
        .find_map(|p| p.strip_prefix(disc))
        .expect("a BuyEvent");
    let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    // timestamp, base_amount_out, max_quote_amount_in, user base / quote reserves, pool base /
    // quote reserves, quote_amount_in, lp_fee_bps, lp_fee, protocol_fee_bps, protocol_fee,
    // quote_amount_in_with_lp_fee (8 B each), user_quote_amount_in, 7 keys, coin_creator_fee_bps,
    // coin_creator_fee, track_volume, 5 × u64, ix_name, 4 × u64, virtual_quote_reserves.
    let pool_quote = u64_at(8 * 6);
    let quote_net_lp = u64_at(8 * 12);
    let mut o = 8 * 14 + 7 * 32 + 8 * 2 + 1 + 8 * 5;
    let n = u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let ix_name = String::from_utf8_lossy(&b[o + 4..o + 4 + n]).into_owned();
    o += 4 + n + 8 * 4;
    let vqr = i128::from_le_bytes(b[o..o + 16].try_into().unwrap());
    BuyEventView {
        pool_quote,
        quote_net_lp,
        ix_name,
        vqr,
    }
}

/// A third party's `buy_v2` and `sell_v2` on the mature pool leave the protocol fee (less the
/// buyback slice, paid in the trade) and the creator fee in the quote vault: the buckets grow by
/// exactly what `virtual_quote_reserves` falls, the trader pays and receives the v1 quote, and
/// the 2026-09 build has no `buy_v2` (Anchor's 101 `InstructionFallbackNotFound`).
#[test]
fn v2_trades_keep_their_fees_in_the_pool_buckets() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let u = PammUser::new(&mut env, &p, 1_000 * SOL);
    assert_eq!(pool_buckets(&env.svm, &p.pool), (0, 0), "none at the dump");
    let buyback = wsol_ata(&pamm::buyback_fee_recipient(&env.svm, 0));
    let q = p.reserves(&env.svm).unwrap().base / 1_000;

    // buy_v2: the cost is the v1 quote; the vault keeps all of it but the buyback slice.
    let (vqr0, vault0, bb0, w0) = (
        vqr(&env, &p),
        tokens(&env, &p.quote_vault),
        tokens(&env, &buyback),
        tokens(&env, &u.wsol),
    );
    let cost = quote_buy(&env, &p, q).expect("quote");
    let meta = ok(u.trade_v2(&mut env, &p, PammTrade::Buy, q, cost));
    let (pf, cf) = pool_buckets(&env.svm, &p.pool);
    assert!(pf > 0 && cf > 0, "both buckets filled: {pf} / {cf}");
    assert_eq!(w0 - tokens(&env, &u.wsol), cost, "v2 costs the v1 quote");
    let bb = tokens(&env, &buyback) - bb0;
    assert!(bb > 0, "the buyback slice is paid in the trade");
    assert_eq!(tokens(&env, &p.quote_vault) - vault0, cost - bb);
    assert_eq!(
        vqr(&env, &p),
        vqr0 - i128::from(pf + cf),
        "VQR = boost − buckets"
    );

    // The v2 `BuyEvent` keeps the v1 layout and carries the **pre-trade** VQR; the indexer's
    // reconstruction (vault before + `quote_amount_in_with_lp_fee` + the event's VQR) is then the
    // post-trade effective reserve exactly: the buckets the trade left in the vault are the VQR
    // it took off.
    let ev = buy_event(&meta);
    let r1 = p.reserves(&env.svm).unwrap();
    assert_eq!(ev.pool_quote, vault0);
    assert_eq!(ev.vqr, vqr0, "the pre-trade VQR");
    let indexer = i128::from(ev.pool_quote + ev.quote_net_lp) + ev.vqr;
    eprintln!(
        "BuyEvent ix_name {:?}: indexer's post-trade quote {indexer}, effective {}",
        ev.ix_name, r1.effective_quote
    );
    assert_eq!(
        indexer,
        i128::try_from(r1.effective_quote).unwrap(),
        "the indexer's reconstruction holds for v2"
    );

    // sell_v2: the buckets grow again by what VQR falls; the payout is the v1 quote.
    let (vqr1, w1) = (vqr(&env, &p), tokens(&env, &u.wsol));
    let s = quote_sell(&env, &p, q / 2).expect("quote");
    ok(u.trade_v2(&mut env, &p, PammTrade::Sell, q / 2, s));
    let (pf2, cf2) = pool_buckets(&env.svm, &p.pool);
    assert!(pf2 > pf && cf2 > cf);
    assert_eq!(tokens(&env, &u.wsol) - w1, s, "v2 pays the v1 quote");
    assert_eq!(vqr(&env, &p), vqr1 - i128::from(pf2 - pf + cf2 - cf));
    eprintln!(
        "buy_v2 {q}: cost {cost}, buyback {bb}, buckets {pf} + {cf}; sell_v2 {}: {s}, buckets \
         {pf2} + {cf2}; VQR {vqr0} -> {}",
        q / 2,
        vqr(&env, &p)
    );

    fork::load_pump_deploy(&mut env.svm, PumpProgram::Amm, PumpDeploy::Sep2026);
    env.warp(1);
    let res = u.trade_v2(&mut env, &p, PammTrade::Buy, q, cost * 2);
    assert_eq!(
        custom_code(&res),
        Some(101),
        "no buy_v2 in the 2026-09 build"
    );
}

/// On a pool carrying buckets (planted as v2 trades leave them: half the vault), a v1 `sell` whose
/// payout lies between `vault − buckets` and the vault fails 6063 `InsufficientRealQuoteReserves`
/// on the redeploys (2026-10-02 and 2026-10-08) and executes on the 2026-09 build, which ignores
/// bytes 271–287.
#[test]
fn a_v1_sale_is_paid_from_the_vault_less_the_buckets() {
    for deploy in PumpDeploy::ALL {
        let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
        fork::load_pump_deploy(&mut env.svm, PumpProgram::Amm, deploy);
        let vault = tokens(&env, &p.quote_vault);
        let bucket = vault / 2;
        plant_buckets(&mut env.svm, &p, bucket, 0);
        let (q, payout) = sale_paying_at_most(&env, &p, vault - bucket + 10 * SOL);
        assert!(payout > vault - bucket && payout <= vault, "{payout}");
        let u = PammUser::new(&mut env, &p, SOL);
        let res = u.sell(&mut env, &p, q, payout);
        match deploy {
            PumpDeploy::Sep2026 => {
                ok(res);
            }
            PumpDeploy::Oct2026 | PumpDeploy::Oct2026Synth | PumpDeploy::Oct2026Chain => {
                assert_eq!(
                    custom_code(&res),
                    Some(PAMM_ERR_INSUFFICIENT_REAL_QUOTE),
                    "{deploy:?}"
                );
            }
        }
    }
}

/// `sweep_protocol_fee` (to a listed protocol fee recipient) and `sweep_creator_fee` (to the
/// coin creator's vault authority), paid by anyone, take the buckets out of the vault and add them
/// back to `virtual_quote_reserves`: the price `vault + VQR` does not move.
#[test]
fn the_sweeps_pay_the_buckets_out_without_moving_the_price() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let u = PammUser::new(&mut env, &p, 1_000 * SOL);
    let q = p.reserves(&env.svm).unwrap().base / 1_000;
    ok(u.trade_v2(&mut env, &p, PammTrade::Buy, q, 1_000 * SOL));
    let (pf, cf) = pool_buckets(&env.svm, &p.pool);
    let payer = env.funded_keypair();
    let creator =
        coin_creator_vault_authority(&Pubkey::new_from_array(p.state(&env.svm).coin_creator));
    for (protocol, recipient, amount) in [
        (true, protocol_fee_recipient(&env.svm, 0), pf),
        (false, creator, cf),
    ] {
        let r0 = p.reserves(&env.svm).unwrap();
        let to0 = read::token_amount(&env.svm, &wsol_ata(&recipient)).unwrap_or(0);
        let ix = pamm_sweep_ix(&p, protocol, &payer.pubkey(), &recipient);
        ok(env.send(&[ix], &[&payer]));
        let r1 = p.reserves(&env.svm).unwrap();
        assert_eq!(r1.real_quote, r0.real_quote - amount, "out of the vault");
        assert_eq!(r1.effective_quote, r0.effective_quote, "the price holds");
        assert_eq!(
            read::token_amount(&env.svm, &wsol_ata(&recipient)).unwrap() - to0,
            amount
        );
    }
    assert_eq!(pool_buckets(&env.svm, &p.pool), (0, 0));
}

/// short.win's own v1 trades on a pool that carries real buckets (a third party's `buy_v2` and
/// `sell_v2` after the listing): the open's sale and the close's buyback equal `quote::sol_leg` on
/// `vault + VQR` to the lamport (the hooks re-quote both), and the v1 trades pay their fees out,
/// leaving the buckets as they were.
#[test]
fn short_win_trades_a_pool_carrying_buckets() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let p = pm.pool;
    let u = PammUser::new(&mut env, &p, 1_000 * SOL);
    let qb = p.reserves(&env.svm).unwrap().base / 1_000;
    ok(u.trade_v2(&mut env, &p, PammTrade::Buy, qb, 1_000 * SOL));
    ok(u.trade_v2(&mut env, &p, PammTrade::Sell, qb / 2, 0));
    let buckets = pool_buckets(&env.svm, &p.pool);
    assert!(buckets.0 > 0 && buckets.1 > 0);
    env.warp(1);
    crank(&mut env, &pm);

    let s_exp = quote_sell(&env, &p, q).expect("quote");
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, s_exp);
    assert_eq!(
        pool_buckets(&env.svm, &p.pool),
        buckets,
        "v1 pays its fees out"
    );
    env.warp(1);
    crank(&mut env, &pm);
    let b_exp = quote_buy(&env, &p, q).expect("quote");
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.b_live, b_exp);
    assert_eq!(pool_buckets(&env.svm, &p.pool), buckets);
    assert_trader_clean(&env, &pm);
}

/// The change the redeploy needed (ARCH U30's "read `R = vault − buckets` before
/// `revalidate_venue`"; PUMP-2026-10-1): with buckets `B ≤ boost` on the pool, the program's real
/// SOL is the payable `min(vault − B, vault + VQR)`, and the tier's virtual part the boost
/// `max(VQR + B, 0)` (the build before it subtracted `max(VQR, 0)` = `boost − B` and read
/// `R = vault`, overstated by `B` in the sale cap and in every term of `R_eff`).
#[test]
fn the_program_reads_the_payable_sol_under_buckets() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let boost = u64::try_from(vqr(&env, &p)).expect("a boosted pool");
    plant_buckets(&mut env.svm, &p, boost / 4, boost / 4);
    let (pf, cf) = pool_buckets(&env.svm, &p.pool);
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q * 40);
    let config = env.config();
    let m = market(&env, &pm);
    let st = host_state(&env, &pm);
    let r = p.reserves(&env.svm).unwrap();
    let effective = u64::try_from(r.effective_quote).unwrap();
    let payable = r.real_quote - pf - cf;
    assert_eq!(
        st.r,
        payable.min(effective),
        "R_now = min(vault − buckets, vault + VQR)"
    );

    let r_eff = shortwin::engine::tier::r_eff(&config, &m, &st, env.now()).expect("R_eff");
    let y_slow = shortwin_math::quote::reprice(m.liq_slow.get(), m.ema_price.get())
        .map(|(_, y)| y)
        .unwrap();
    let virt = u128::try_from((vqr(&env, &p) + i128::from(pf + cf)).max(0)).unwrap();
    assert_eq!(virt, u128::from(boost), "the virtual part is the boost");
    let (_, d) = pm.pool.locked_depth(&env.svm);
    let locked = (d * m.ema_price.get().isqrt() / 1_000_000_000).saturating_sub(virt);
    let want = u128::from(payable.min(effective))
        .min(y_slow.saturating_sub(virt))
        .min(locked);
    assert_eq!(r_eff, want);
}

/// The buckets as a boost of `VQR + protocol + creator` leaves them after v2 trades (written
/// over whatever the pool held; `virtual_quote_reserves`, the vault and so the price unchanged):
/// the payable SOL `vault − buckets` moves on its own, the spot–EMA guard and the fee tier stay.
fn hold_buckets(env: &mut Env, p: &PammPool, protocol: u64, creator: u64) {
    fork::patch(&mut env.svm, &p.pool, |d| {
        d[POOL_OFF_PROTOCOL_FEES..POOL_OFF_PROTOCOL_FEES + 8]
            .copy_from_slice(&protocol.to_le_bytes());
        d[POOL_OFF_CREATOR_FEES..POOL_OFF_CREATOR_FEES + 8].copy_from_slice(&creator.to_le_bytes());
    });
}

/// What a sale of `q` takes out of the quote vault on `p` now, `g − ⌈g·lp/10⁴⌉` (the amount
/// PumpSwap checks against `vault − buckets`, 6063; `sol_leg::sell_exact_in`'s cap).
fn vault_out(env: &Env, p: &PammPool, q: u64) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let g = u64::try_from(u128::from(q) * r.effective_quote / (u128::from(r.base) + u128::from(q)))
        .expect("u64");
    g - (g * pool_fees(env, p).lp).div_ceil(10_000)
}

/// short.win's own `ProceedsBelowMin`, raised before any PumpSwap CPI: the code alone does not
/// tell them apart — PumpSwap's `InsufficientRealQuoteReserves` is 6063 too —, so no pAMM
/// instruction may have run.
fn refused_before_the_venue(res: &TxResult) {
    assert_err(res, E::ProceedsBelowMin);
    let invoke = format!(
        "Program {} invoke",
        shortwin::constants::PUMP_AMM_PROGRAM_ID
    );
    let logs = &res.as_ref().expect_err("refused").meta.logs;
    assert!(
        !logs.iter().any(|l| l.starts_with(&invoke)),
        "refused by short.win, not by PumpSwap's 6063:\n{}",
        logs.join("\n")
    );
}

/// PUMP-2026-10-1, the open's sale cap (ARCH §3.4 step 7): with the pool's buckets leaving
/// `vault − buckets` one lamport below what the open's sale of `q` takes out of the vault, the
/// program refuses it `ProceedsBelowMin` before any CPI — the build before the fix capped the
/// sale at the vault and sent it, and PumpSwap failed it 6063 (`InsufficientRealQuoteReserves`,
/// the same number as short.win's `ProceedsBelowMin`, hence the log check) —, and with exactly that
/// much payable the same open executes to the lamport of `quote::sol_leg` (the venue's own check
/// passes there too: the program's bound is PumpSwap's). The price never moves (`hold_buckets`).
#[test]
fn an_open_is_capped_at_the_payable_sol() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let p = pm.pool;
    let vault = tokens(&env, &p.quote_vault);
    let out = vault_out(&env, &p, q);
    let s_exp = quote_sell(&env, &p, q).expect("quote");
    hold_buckets(&mut env, &p, vault - out + 1, 0);
    assert_eq!(p.reserves(&env.svm).unwrap().payable_quote, out - 1);
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    refused_before_the_venue(&res);
    assert_eq!(tokens(&env, &p.quote_vault), vault, "no venue CPI");
    // Split across both buckets, one lamport less: payable = out.
    hold_buckets(
        &mut env,
        &p,
        (vault - out) / 2,
        (vault - out) - (vault - out) / 2,
    );
    assert_eq!(p.reserves(&env.svm).unwrap().payable_quote, out);
    let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, s_exp);
    assert_eq!(
        vault - tokens(&env, &p.quote_vault),
        out,
        "the sale took `out`"
    );
    assert_eq!(
        p.reserves(&env.svm).unwrap().payable_quote,
        0,
        "the buckets stay"
    );
    eprintln!(
        "open of {q}: {out} out of the vault {vault}; refused at {} payable, executed at {out}",
        out - 1
    );
}

/// PUMP-2026-10-1, the keeper exits on a pool carrying real buckets (a third party's `buy_v2` /
/// `sell_v2` after the open): an expiry, and a liquidation after a real ×2.8 pump, buy back on
/// the pool to the lamport of `quote_buy` (a buyback is not capped by the payable SOL), the
/// hooks re-quote both and the buckets stay as the v2 trades left them.
#[test]
fn keeper_exits_on_a_pool_carrying_buckets() {
    for liquidation in [false, true] {
        let (mut env, pm, q) = setup(PammCoin::Mature);
        let p = pm.pool;
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        env.warp(1);
        let u = PammUser::new(&mut env, &p, 3_000 * SOL);
        let qb = p.reserves(&env.svm).unwrap().base / 1_000;
        ok(u.trade_v2(&mut env, &p, PammTrade::Buy, qb, 1_000 * SOL));
        ok(u.trade_v2(&mut env, &p, PammTrade::Sell, qb / 2, 0));
        let buckets = pool_buckets(&env.svm, &p.pool);
        assert!(buckets.0 > 0 && buckets.1 > 0);
        let kp = keeper(&mut env);
        let vault0 = tokens(&env, &pm.keys.pool_vault);
        let (cost, b_live) = if liquidation {
            ok(u.push_price(&mut env, &p, 28, 10));
            let mut done = None;
            for _ in 0..900 {
                env.warp(1);
                crank(&mut env, &pm);
                let charge = quote_buy(&env, &p, q).expect("quote");
                match liquidate(&mut env, &pm, &kp, &b.key(), nonce) {
                    Ok(meta) => {
                        done = Some((meta, charge));
                        break;
                    }
                    Err(f) => assert_eq!(custom_code(&Err(f)), Some(code(E::NotLiquidatable))),
                }
            }
            let (meta, charge) = done.expect("liquidated within 15 min");
            let ev: Liquidate = events::one(&meta);
            assert!(!ev.expired && ev.penalty > 0);
            assert_eq!(ev.b_live, charge);
            (ev.buyback_cost, ev.b_live)
        } else {
            warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
            crank(&mut env, &pm);
            env.warp(1);
            crank(&mut env, &pm);
            let charge = quote_buy(&env, &p, q).expect("quote");
            let ev: Expire = events::one(&ok(expire(&mut env, &pm, &kp, &b.key(), nonce)));
            assert_eq!(ev.buyback_cost, charge, "B = PumpSwap's charge");
            (ev.buyback_cost, ev.b_live)
        };
        assert!(cost > 0 && cost <= b_live);
        assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
        assert_eq!(
            pool_buckets(&env.svm, &p.pool),
            buckets,
            "v1 pays its fees out"
        );
        assert_trader_clean(&env, &pm);
        assert_eq!(market(&env, &pm).open_positions, 0);
    }
}

/// PUMP-2026-10-1 under a negative VQR (the synthetic pool of `negvqr`, VQR = −⌊vault/2⌋) with
/// buckets planted as v2 trades leave them (off the VQR): the effective reserve `vault + VQR` is
/// below the payable `vault − buckets`, so it stays the real SOL `R` — the fix changes nothing
/// there —, the virtual part is 0 (`max(VQR + buckets, 0)`), `R_eff` is `min(vault + VQR,
/// R_slow, the burned LP's SOL)` recomputed from the raw fields, and an open and a close execute
/// to the lamport, the buckets untouched.
#[test]
fn a_negative_vqr_pool_carrying_buckets() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    env.plant_negative_vqr();
    let vault = tokens(&env, &p.quote_vault);
    plant_buckets(&mut env.svm, &p, vault / 20, vault / 40);
    let buckets = pool_buckets(&env.svm, &p.pool);
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q * 40);
    let st = host_state(&env, &pm);
    assert!(st.executable);
    let r = p.reserves(&env.svm).unwrap();
    let effective = u64::try_from(r.effective_quote).unwrap();
    assert!(vqr(&env, &p) + i128::from(buckets.0 + buckets.1) < 0);
    assert!(
        effective < r.payable_quote,
        "{effective} < {}",
        r.payable_quote
    );
    assert_eq!(st.r, effective, "R = vault + VQR");
    assert_eq!(st.pool.expect("pool facts").virtual_part, 0);
    let config = env.config();
    let m = market(&env, &pm);
    let r_eff = shortwin::engine::tier::r_eff(&config, &m, &st, env.now()).expect("R_eff");
    let y_slow = shortwin_math::quote::reprice(m.liq_slow.get(), m.ema_price.get())
        .map(|(_, y)| y)
        .unwrap();
    let (_, d) = pm.pool.locked_depth(&env.svm);
    let locked = d * m.ema_price.get().isqrt() / 1_000_000_000;
    assert_eq!(
        r_eff,
        u128::from(effective).min(y_slow).min(locked),
        "nothing virtual comes off"
    );

    let s_exp = quote_sell(&env, &p, q).expect("quote");
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, s_exp);
    env.warp(1);
    crank(&mut env, &pm);
    let b_exp = quote_buy(&env, &p, q).expect("quote");
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.b_live, b_exp);
    assert_eq!(pool_buckets(&env.svm, &p.pool), buckets);
}

/// Buckets above the quote vault break pump's invariant (it never pays them out of anything
/// else): `vault − buckets` is undefined, so the program's reader makes the venue not
/// executable (`PoolReserves::payable_quote`, like a negative `vault + VQR`) and opens are
/// refused `NoRoute`, as the hooks' own view has it (I16 after every instruction); `crank_price`
/// still runs. With the buckets equal to the vault it executes, with no payable SOL: `R = 0`, and
/// an open is refused at its sale cap (`ProceedsBelowMin`).
#[test]
fn buckets_above_the_vault_are_not_executable() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let p = pm.pool;
    let vault = tokens(&env, &p.quote_vault);
    hold_buckets(&mut env, &p, vault, 1);
    assert_eq!(p.reserves(&env.svm), None);
    let st = host_state(&env, &pm);
    assert!(!st.executable && st.r == 0);
    let view = invariants::venue_view::view(&env.svm, &market(&env, &pm)).expect("a view");
    assert!(!view.executable, "the hooks' view agrees");
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    assert_err(&res, E::NoRoute);
    env.warp(1);
    crank(&mut env, &pm);

    hold_buckets(&mut env, &p, vault, 0);
    let st = host_state(&env, &pm);
    assert!(st.executable);
    assert_eq!(st.r, 0, "nothing payable");
    let (res, _) = env.open(&pm, &mut b, q);
    refused_before_the_venue(&res);
}
