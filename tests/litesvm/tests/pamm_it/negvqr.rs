//! A negative `virtual_quote_reserves` (ARCH §4.1 PumpSwap row; ECON §1.2, §6.2 #103; R-VEN
//! Update 2026-09-30 H3, H10) on the **synthetic** `pamm_mature_pool_negative_vqr` fixture — the
//! e/acc pool with VQR = −⌊vault/2⌋, so its effective quote reserve is half its real SOL (no
//! mainnet pool is negative yet; the deployed ELF prices it exactly as `vault + VQR`):
//!
//! - the program prices on `Y = vault + VQR`, its fee tier on the effective market cap (creator
//!   60 bps instead of the source pool's 35), and a trade executes to the lamport;
//! - the real SOL it reads is the smaller of the vault and `Y` (`VenueState.r`: the sale's payout
//!   cap, which cannot bind below `Y`, and the tier's `R_now`); `max(VQR, 0)` = 0 comes off
//!   `R_slow` (ECON §6.2's delegated PROPOSED reading, built in M6).

use crate::common::*;

/// The program's own venue state of `pm` now (the host build of `venues::read_state_with` on the
/// ReadSet as the VM holds it, with the mint's supply).
pub fn host_state(env: &Env, pm: &PammMarket) -> shortwin::venues::VenueState {
    use anchor_lang::prelude::AccountInfo;
    let mut accs: Vec<(Pubkey, u64, Vec<u8>, Pubkey)> = pm
        .read_set(false)
        .iter()
        .map(|a| {
            let acc = env.svm.get_account(&a.pubkey).unwrap_or_default();
            (a.pubkey, acc.lamports, acc.data, acc.owner)
        })
        .collect();
    let infos: Vec<AccountInfo> = accs
        .iter_mut()
        .map(|(k, l, d, o)| AccountInfo::new(k, false, false, l, d, o, false))
        .collect();
    let m = market(env, pm);
    let mint = env.svm.get_account(&pm.keys.mint).unwrap();
    let supply = shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &mint.data)
        .unwrap()
        .supply;
    shortwin::venues::read_state_with(&m, VenueKind::PumpSwap, &infos, env.now(), Some(supply))
}

/// The synthetic pool lists, prices on `vault + VQR` with the tier of the effective market cap,
/// and an open and a close execute on the dumped ELF to the lamport of `quote::sol_leg`; the
/// program's real SOL is `min(vault, vault + VQR)`.
#[test]
fn a_negative_vqr_pool_prices_on_the_effective_reserve() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let src_fees = pool_fees(&env, &p);
    env.plant_negative_vqr();
    let st = p.state(&env.svm);
    assert!(st.virtual_quote_reserves < 0, "the synthetic pool");
    let r = p.reserves(&env.svm).unwrap();
    assert_eq!(
        r.effective_quote * 2,
        u128::from(r.real_quote),
        "effective = vault / 2"
    );
    let fees = pool_fees(&env, &p);
    assert_ne!(fees, src_fees, "the tier follows the effective market cap");
    assert_eq!(fees.creator, 60);
    assert_eq!(src_fees.creator, 35);

    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q * 40);
    let st = host_state(&env, &pm);
    assert!(st.executable);
    let r = p.reserves(&env.svm).unwrap();
    assert_eq!(u128::from(st.y), r.effective_quote);
    assert_eq!(st.x, r.base);
    assert_eq!(
        st.r,
        u64::try_from(r.effective_quote).unwrap(),
        "R = min(vault, vault + VQR)"
    );
    assert!(st.r < r.real_quote);
    assert_eq!(st.fee_ppm, fees.oneway_ppm());
    let pool = st.pool.expect("pool facts");
    assert_eq!(pool.virtual_part, 0, "a negative VQR is no virtual part");

    let s_exp = quote_sell(&env, &p, q).expect("quote");
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, s_exp);
    env.warp(1);
    crank(&mut env, &pm);
    let b_exp = quote_buy(&env, &p, q).expect("quote");
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.b_live, b_exp);
}

/// ECON §6.2's tier inputs under a negative VQR (#103): the program's `R_eff` is the smaller of
/// its real and effective inputs — `R_now = min(vault, vault + VQR)`, `R_slow = Y(reprice(L_slow,
/// P_ema)) − max(VQR, 0)` — and of the burned LP's real SOL `max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR,
/// 0), 0)` (M6R-1 at `λ_min` = 100 % since OWNER-M6D-1, ECON #106; the pool's own `D` from its LP
/// mint), recomputed here from the readers and `shortwin-math`; the vault alone would credit the
/// pool with twice its pricing depth. On this pool (e/acc's, λ 92.98 %) the burned LP's bound is
/// the one that binds (at `λ_min` 50 % it did not).
#[test]
fn the_tier_inputs_are_the_smaller_of_real_and_effective_sol() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    env.plant_negative_vqr();
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q * 40);
    let config = env.config();
    let m = market(&env, &pm);
    let st = host_state(&env, &pm);
    let r_eff = shortwin::engine::tier::r_eff(&config, &m, &st, env.now()).expect("R_eff");
    let r = p.reserves(&env.svm).unwrap();
    let y_slow = shortwin_math::quote::reprice(m.liq_slow.get(), m.ema_price.get())
        .map(|(_, y)| y)
        .unwrap();
    let (_, d) = pm.pool.locked_depth(&env.svm);
    assert_eq!(m.pool_d.get(), d);
    let vqr = u128::try_from(p.state(&env.svm).virtual_quote_reserves.max(0)).unwrap();
    assert_eq!(vqr, 0, "a negative VQR counts as 0");
    let locked = d * m.ema_price.get().isqrt() / 1_000_000_000 - vqr;
    let want = u128::from(r.real_quote)
        .min(r.effective_quote)
        .min(y_slow - vqr)
        .min(locked);
    assert_eq!(r_eff, want);
    assert!(
        locked < y_slow.min(r.effective_quote),
        "the burned LP's bound binds"
    );
    assert!(
        r_eff < u128::from(r.real_quote) / 2 + 1_000_000_000,
        "not the vault"
    );
}
