//! Shared helpers of the PumpSwap adapter tests.

#![allow(dead_code)]

pub use anchor_lang::prelude::Pubkey;
pub use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
pub use litesvm::types::TransactionMetadata;
pub use shortwin::errors::ShortwinError as E;
pub use shortwin::events::{Close, Open, SolSettle, VenueStateChanged};
pub use shortwin::state::{Market, ShortPosition, VenueKind};
pub use shortwin::venues::ExtrasSel;
pub use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
pub use shortwin_litesvm_tests::fork;
pub use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammPool, PammUser};
pub use shortwin_litesvm_tests::pump::{self, cu_limit_ix, wsol_ata};
pub use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
pub use shortwin_math::fees::{self, FeeBps, FeeTier, PammFeeSchedule};
pub use shortwin_math::quote::sol_leg;
pub use shortwin_venue_state::{pump as vpump, pump_amm as vamm};
pub use solana_signer::Signer;

/// One SOL in lamports.
pub const SOL: u64 = 1_000_000_000;
/// ARCH §6.1's PROPOSED limit for keeper exits.
pub const KEEPER_EXIT_CU: u32 = 400_000;

/// The error code of `e`.
pub fn code(e: E) -> u32 {
    u32::from(e)
}

/// A position size inside every default cap on `p` now: 1 % of the base reserve, and a notional
/// of at most ≈ 10 SOL (`market_max_notional` is 25 SOL), at least ≈ 0.2 SOL.
pub fn q_for(env: &Env, p: &PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let y = r.effective_quote.max(1);
    let by_notional = base * u128::from(10 * SOL) / y;
    let q = (base / 100).min(by_notional);
    u64::try_from(q).expect("u64")
}

/// A PumpSwap market on `coin` with `40·q` lent, its price started and warmed up (60 s of
/// cranks), and — on a pool whose boost is still running — the boost window waited out with a
/// crank every 30 s. Returns it and `q`.
pub fn setup(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q.saturating_mul(40));
    wait_out_boost(&mut env, &pm);
    let q = q_for(&env, &pm.pool);
    (env, pm, q)
}

/// Lists `p`'s mint on PumpSwap (extending a 2024-layout curve first) and seeds it with `lend`
/// tokens (`Env::pamm_market_listed`: lender, venue accounts, 60 s of cranks).
pub fn list_on(env: &mut Env, p: &PammPool, lend: u64) -> PammMarket {
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    env.pamm_market_listed(&p.mint, lend, 60)
}

/// Cranks every 30 s until the boost gate no longer blocks (`boost_window_s` after
/// `graduated_at`, or the boost vault at dust).
pub fn wait_out_boost(env: &mut Env, pm: &PammMarket) {
    let params = env.config().params;
    loop {
        let m = market(env, pm);
        let boost = pm.pool.boost_amount(&env.svm);
        if boost <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            return;
        }
        env.warp(30);
        crank(env, pm);
    }
}

/// The market as stored.
pub fn market(env: &Env, pm: &PammMarket) -> Market {
    env.market(&pm.market())
}

/// `crank_price` on `mm` that must succeed.
pub fn crank(env: &mut Env, mm: &impl VenueMarket) -> TransactionMetadata {
    ok(env.send(&[crank_price_ix(mm)], &[]))
}

/// `n` seconds of one crank each.
pub fn crank_secs(env: &mut Env, mm: &impl VenueMarket, n: u32) {
    for _ in 0..n {
        env.warp(1);
        crank(env, mm);
    }
}

/// An open that must succeed: its event, nonce and metadata.
pub fn open_ok(
    env: &mut Env,
    pm: &PammMarket,
    b: &mut Borrower,
    q: u64,
) -> (Open, u64, TransactionMetadata) {
    let (res, nonce) = env.open(pm, b, q);
    let meta = ok(res);
    (events::one(&meta), nonce, meta)
}

/// A close that must succeed: its event and metadata.
pub fn close_ok(
    env: &mut Env,
    pm: &PammMarket,
    b: &Borrower,
    nonce: u64,
) -> (Close, TransactionMetadata) {
    let meta = ok(env.close(pm, b, nonce));
    (events::one(&meta), meta)
}

/// Lamports of `key`.
pub fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}

/// Token amount of `key`.
pub fn tokens(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or(0)
}

/// A keeper (funded keypair).
pub fn keeper(env: &mut Env) -> solana_keypair::Keypair {
    env.funded_keypair()
}

/// `liquidate` of `b`'s position `nonce` by `keeper` (with ARCH §6.1's keeper-exit CU limit).
pub fn liquidate(
    env: &mut Env,
    mm: &impl VenueMarket,
    keeper: &solana_keypair::Keypair,
    b: &Pubkey,
    nonce: u64,
) -> TxResult {
    let ix = liquidate_ix(mm, &keeper.pubkey(), b, nonce);
    env.send(&[cu_limit_ix(KEEPER_EXIT_CU), ix], &[keeper])
}

/// `expire` of `b`'s position `nonce` by `keeper`.
pub fn expire(
    env: &mut Env,
    mm: &impl VenueMarket,
    keeper: &solana_keypair::Keypair,
    b: &Pubkey,
    nonce: u64,
) -> TxResult {
    let ix = expire_ix(mm, &keeper.pubkey(), b, nonce);
    env.send(&[cu_limit_ix(KEEPER_EXIT_CU), ix], &[keeper])
}

/// `sol_settle` of `b`'s position `nonce` by `keeper`.
pub fn settle(
    env: &mut Env,
    mm: &impl VenueMarket,
    keeper: &solana_keypair::Keypair,
    b: &Pubkey,
    nonce: u64,
) -> TxResult {
    let ix = sol_settle_ix(env, mm, &keeper.pubkey(), b, nonce);
    env.send(&[ix], &[keeper])
}

/// PumpSwap's fee on `p` now, resolved by `shortwin-math` on the readers' state (the live
/// `FeeConfig`, `GlobalConfig`, the pool and the mint's live supply).
pub fn pool_fees(env: &Env, p: &PammPool) -> FeeBps {
    let r = p.reserves(&env.svm).expect("reserves");
    let pool = p.state(&env.svm);
    let fc = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_FEE_CONFIG)
        .expect("FeeConfig");
    let fc = vpump::read_fee_config(&fc.owner.to_bytes(), &fc.data).expect("FeeConfig");
    let tiers: Vec<FeeTier> = fc
        .fee_tiers
        .iter()
        .map(|t| FeeTier {
            threshold: t.threshold,
            fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
        })
        .collect();
    let gc = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_GLOBAL_CONFIG)
        .expect("GlobalConfig");
    let gc = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data).expect("GlobalConfig");
    let mint = env.svm.get_account(&p.mint).expect("mint");
    let supply = shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &mint.data)
        .expect("mint")
        .supply;
    fees::pump_amm(
        r.effective_quote,
        u128::from(r.base),
        &PammFeeSchedule {
            tiers: &tiers,
            base_supply: supply,
            creator_fee_configurable: gc.creator_fee_configurable(),
            pool_creator_fee_bps: pool.creator_fee_bps,
            coin_creator_is_default: pool.coin_creator_is_default(),
            cap_bps: None,
        },
    )
    .expect("fees")
}

/// The exact-in sale of `q` on `p` now (`quote::sol_leg`, capped by the real SOL, ε₁ = 0).
pub fn quote_sell(env: &Env, p: &PammPool, q: u64) -> Option<u64> {
    let r = p.reserves(&env.svm)?;
    let f = pool_fees(env, p);
    let cap = u64::try_from(r.effective_quote).ok()?.min(r.real_quote);
    sol_leg::sell_exact_in(q, u128::from(r.base), r.effective_quote, &f, cap).map(|s| s.proceeds)
}

/// The exact-out buy of `q` on `p` now (`quote::sol_leg`, ε₁ = 0).
pub fn quote_buy(env: &Env, p: &PammPool, q: u64) -> Option<u64> {
    let r = p.reserves(&env.svm)?;
    let f = pool_fees(env, p);
    sol_leg::buy_exact_out(q, u128::from(r.base), r.effective_quote, false, &f).map(|b| b.cost)
}

/// The trader side holds nothing: `trader` at 0 lamports, `trader_wsol` closed, the base ATA
/// empty, the lending vault without a delegate.
pub fn assert_trader_clean(env: &Env, pm: &PammMarket) {
    assert_eq!(lamports(env, &pm.keys.trader), 0, "trader holds nothing");
    assert_eq!(
        lamports(env, &pda::trader_wsol(&pm.market()).0),
        0,
        "trader_wsol closed"
    );
    assert_eq!(tokens(env, &pm.trader_base_ata), 0, "trader_base_ata empty");
    assert_eq!(
        read::token_delegate(&env.svm, &pm.keys.pool_vault),
        None,
        "pool_vault keeps no delegate"
    );
}

/// The one `VenueStateChanged` of `meta`, checked `from → to`.
pub fn venue_changed(
    meta: &TransactionMetadata,
    from: VenueKind,
    to: VenueKind,
) -> VenueStateChanged {
    let ev: VenueStateChanged = events::one(meta);
    assert_eq!((ev.from, ev.to), (from as u8, to as u8), "the transition");
    ev
}

/// Warps to `deadline + after` of `b`'s position `nonce`.
pub fn warp_to_deadline(env: &mut Env, pm: &PammMarket, b: &Pubkey, nonce: u64, after: i64) {
    let to = env.position(pm, b, nonce).deadline + after;
    let now = env.now();
    if to > now {
        env.warp(to - now);
    }
}

/// Moves `amount` of the market's tokens from a third party's (planted) account into `to`
/// (`TransferChecked`, signed by the third party): a donation anyone can make.
pub fn donate_tokens(env: &mut Env, pm: &PammMarket, to: &Pubkey, amount: u64) {
    let u = PammUser::new(env, &pm.pool, SOL);
    fork::set_token_amount(&mut env.svm, &u.base, amount);
    let mint = env.svm.get_account(&pm.keys.mint).expect("mint");
    let decimals = shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &mint.data)
        .expect("mint")
        .decimals;
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    let ix = Instruction {
        program_id: pm.keys.token_program,
        accounts: vec![
            AccountMeta::new(u.base, false),
            AccountMeta::new_readonly(pm.keys.mint, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(u.kp.pubkey(), true),
        ],
        data,
    };
    ok(env.send(&[ix], &[&u.kp]));
}
