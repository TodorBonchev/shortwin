//! Positions on PumpSwap to the lamport (ARCH §2.4.1 PumpSwap row, §3.4–§3.8; M6), on the dumped
//! pAMM, pump-fees and token ELFs, SPL Token and Token-2022 pools:
//!
//! - open → close on every fixture pool: `S` and `B` equal `quote::sol_leg` on the readers'
//!   state (the live fee tier by market cap, `Y = vault + VQR`, the sale capped by the real SOL);
//! - an expiry and a liquidation after a real pump buy back on the pool: PumpSwap's charge, `q`
//!   back in the lending vault through `trader_base_ata`, the trader clean;
//! - `sol_settle` on the pool: a no-op for a healthy position (its observation kept), and S1
//!   after a held ×10 pump the position's value cannot buy back.
//!
//! Every instruction goes through `Env::send`: the invariant hooks run after each one (I13's
//! PumpSwap view `invariants::venue_view::pump_swap` recomputes each quote independently).

use shortwin::events::{Expire, Liquidate};

use crate::common::*;

/// Open → close on `coin`'s pool: `S` and `B` equal `quote::sol_leg`, the trader side ends clean.
fn open_close(coin: PammCoin) {
    let (mut env, pm, q) = setup(coin);
    let s_exp = quote_sell(&env, &pm.pool, q).expect("quote");
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, s_exp, "{coin:?}: S = the pool's quote");
    assert_eq!(open.venue, VenueKind::PumpSwap as u8);
    assert_eq!(open.venue_rent, 0, "{coin:?}: no venue-side rent");
    assert_trader_clean(&env, &pm);
    env.warp(10);
    crank(&mut env, &pm);
    let b_exp = quote_buy(&env, &pm.pool, q).expect("quote");
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.b_live, b_exp, "{coin:?}: B = the pool's quote");
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
    assert_trader_clean(&env, &pm);
}

/// Open → close on the SPL-Token 261-byte pool.
#[test]
fn open_close_on_the_legacy261_pool() {
    open_close(PammCoin::Legacy261);
}

/// Open → close on the SPL-Token 211-byte pool (no coin creator; its rent covers its growth).
#[test]
fn open_close_on_the_legacy211_pool() {
    open_close(PammCoin::Legacy211);
}

/// Open → close on the Token-2022 pools: the mature pool (third-party LP), a cashback coin, and
/// the fresh pool once its boost window has passed.
#[test]
fn open_close_on_the_token_2022_pools() {
    for coin in [PammCoin::Mature, PammCoin::Cashback, PammCoin::Fresh] {
        open_close(coin);
    }
}

/// An expiry by a keeper right after the deadline on an SPL and a Token-2022 pool: PumpSwap's
/// charge, the waterfall's expiry fee and keeper reward, `q` back to the lending vault.
#[test]
fn an_expiry_buys_back_on_the_pool() {
    for coin in [PammCoin::Legacy261, PammCoin::Mature] {
        let (mut env, pm, q) = setup(coin);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
        crank(&mut env, &pm);
        env.warp(1);
        crank(&mut env, &pm);
        let kp = keeper(&mut env);
        let charge = quote_buy(&env, &pm.pool, q).expect("quote");
        let vault0 = tokens(&env, &pm.keys.pool_vault);
        let meta = ok(expire(&mut env, &pm, &kp, &b.key(), nonce));
        eprintln!(
            "{coin:?}: expire on PumpSwap: {} CU",
            meta.compute_units_consumed
        );
        let ev: Expire = events::one(&meta);
        assert_eq!(ev.buyback_cost, charge, "{coin:?}: B = PumpSwap's charge");
        assert_eq!(ev.b_live, charge);
        assert!(ev.expiry_fee > 0 && ev.keeper_reward > 0);
        assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
        assert_trader_clean(&env, &pm);
        assert_eq!(market(&env, &pm).open_positions, 0);
    }
}

/// A liquidation after a real pump on the pool (a buyer lifts the price × 2.8, held): the keeper
/// cranks every second until both legs are liquidatable, then liquidates on the pool; only
/// `NotLiquidatable` before that.
#[test]
fn a_liquidation_after_a_pump_buys_back_on_the_pool() {
    for coin in [PammCoin::Legacy261, PammCoin::Mature] {
        let (mut env, pm, q) = setup(coin);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        env.warp(1);
        let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
        ok(u.push_price(&mut env, &pm.pool, 28, 10));
        let kp = keeper(&mut env);
        let vault0 = tokens(&env, &pm.keys.pool_vault);
        let mut done = None;
        for _ in 0..900 {
            env.warp(1);
            crank(&mut env, &pm);
            match liquidate(&mut env, &pm, &kp, &b.key(), nonce) {
                Ok(meta) => {
                    done = Some(meta);
                    break;
                }
                Err(f) => {
                    let got = custom_code(&Err(f));
                    assert_eq!(got, Some(code(E::NotLiquidatable)), "only not-yet");
                }
            }
        }
        let meta = done.expect("liquidated within 15 min");
        eprintln!(
            "{coin:?}: liquidate on PumpSwap: {} CU",
            meta.compute_units_consumed
        );
        let ev: Liquidate = events::one(&meta);
        assert!(!ev.expired);
        assert!(ev.buyback_cost > 0 && ev.buyback_cost <= ev.b_live);
        assert!(ev.penalty > 0);
        assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
        assert_trader_clean(&env, &pm);
    }
}

/// `sol_settle` on the pool. A healthy position: an `Ok` no-op whose prologue observation stands
/// (no event, the position open). After a held ×10 pump the position's value cannot buy back:
/// liquidations fail, and the keeper's per-second `sol_settle` arms and executes **S1** — the
/// lenders take `V − K_s`, no venue CPI, no token moves.
#[test]
fn sol_settle_on_the_pool() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let kp = keeper(&mut env);
    env.warp(1);
    let ring0 = market(&env, &pm).ring_count;
    let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
    assert!(
        events::of::<SolSettle>(&meta).is_empty(),
        "healthy: nothing"
    );
    assert!(
        market(&env, &pm).ring_count >= ring0,
        "the observation stands"
    );
    assert_eq!(market(&env, &pm).open_positions, 1);

    let u = PammUser::new(&mut env, &pm.pool, 20_000 * SOL);
    ok(u.push_price(&mut env, &pm.pool, 10, 1));
    let pos = env.position(&pm, &b.key(), nonce);
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let mut settled = None;
    for s in 1..=300 {
        env.warp(1);
        crank(&mut env, &pm);
        if liquidate(&mut env, &pm, &kp, &b.key(), nonce).is_ok() {
            panic!("+{s} s: the value cannot buy back ×10");
        }
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            settled = Some((s, ev));
            break;
        }
    }
    let (s, ev) = settled.expect("settled within 5 min");
    eprintln!(
        "S{} on PumpSwap +{s} s after a ×10 pump: lenders {} lamports, keeper {}",
        ev.condition, ev.lender_amount, ev.k_s
    );
    assert_eq!(ev.condition, 1, "S1");
    assert_eq!(ev.lender_amount + ev.k_s, pos.v, "S1 pays V − K_s");
    assert_eq!(ev.borrower_amount, 0);
    assert_eq!(tokens(&env, &pm.keys.pool_vault), vault0, "no token moves");
    assert_trader_clean(&env, &pm);
    assert_eq!(market(&env, &pm).open_positions, 0);
}

/// The zero-lamport borrower on PumpSwap (M10 G-H10; `engine::zero_lamport_exits`): with the wallet
/// emptied after the open (A) or inside the exit's transaction (B), the borrower's close and a
/// keeper's expiry land and credit at least the position's and the exposure record's rent.
#[test]
fn a_zero_lamport_borrower_exits_on_pumpswap() {
    shortwin_litesvm_tests::engine::zero_lamport_exits(|| setup(PammCoin::Legacy261), "PumpSwap");
}
