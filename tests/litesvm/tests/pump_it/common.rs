//! Shared helpers of the pump-curve adapter tests.

#![allow(dead_code)]

pub use anchor_lang::prelude::Pubkey;
pub use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
pub use litesvm::types::TransactionMetadata;
pub use shortwin::errors::ShortwinError as E;
pub use shortwin::events::{Close, Open};
pub use shortwin::state::{ShortPosition, VenueKind};
pub use shortwin::venues::ExtrasSel;
pub use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, open_short_ix, sol_settle_ix,
};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
pub use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve};
pub use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
pub use shortwin_litesvm_tests::tokens::TokenProgram;
pub use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
pub use solana_signer::Signer;

/// The error code of `e`.
pub fn code(e: E) -> u32 {
    u32::from(e)
}

/// A position size within every default cap on the curve: `vT/200` (0.5 % of the virtual token
/// reserve, under Tier D's 1.489 % impact and 1.16 % wallet caps).
pub fn q_for(env: &Env, pm: &PumpMarket) -> u64 {
    let c = pm.curve.state(&env.svm);
    c.virtual_token_reserves / 200
}

/// A pump market on `coin` with `40·q` lent (`q` the default size), warmed up; returns it and
/// `q`. The fresh curve (no real SOL yet: its `rQ` is one lamport) first gets 5 SOL of buys by a
/// normal user, so it can pay for a sale.
pub fn setup(coin: PumpCoin) -> (Env, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(coin);
    if matches!(coin, PumpCoin::Fresh | PumpCoin::Fresh166) {
        let u = CurveUser::new(&mut env, &c);
        let s = c.curve_state(&env.svm);
        let q = u64::try_from(s.vt / 7).unwrap();
        ok(u.buy(&mut env, &c, q, u64::MAX));
    }
    ok(env.list_pump(&c));
    let vt = c.state(&env.svm).virtual_token_reserves;
    let q = vt / 200;
    let pm = env.pump_market_listed(PumpSpec::new(coin, q.saturating_mul(40)), c);
    (env, pm, q)
}

/// An open that must succeed: its event, nonce and metadata.
pub fn open_ok(
    env: &mut Env,
    pm: &PumpMarket,
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
    pm: &PumpMarket,
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

/// The trader side holds nothing: `trader` at 0 lamports (System, no data), `trader_base_ata`
/// at 0 tokens, the lending vault without a delegate.
pub fn assert_trader_clean(env: &Env, pm: &PumpMarket) {
    assert_eq!(lamports(env, &pm.keys.trader), 0, "trader holds nothing");
    assert_eq!(tokens(env, &pm.trader_base_ata), 0, "trader_base_ata empty");
    assert_eq!(
        read::token_delegate(&env.svm, &pm.keys.pool_vault),
        None,
        "pool_vault keeps no delegate"
    );
}

/// The market as stored.
pub fn market(env: &Env, pm: &PumpMarket) -> shortwin::state::Market {
    env.market(&pm.market())
}

/// A pump market seen as list-only (`None`): its ReadSet is the detection set `[bonding_curve,
/// canonical_pool]` (ARCH §3.12 `None` row), no TradeExtras.
pub struct AsNone<'a>(pub &'a PumpMarket);

impl VenueMarket for AsNone<'_> {
    fn keys(&self) -> &shortwin_litesvm_tests::fixtures::MarketKeys {
        &self.0.keys
    }
    fn read_set(&self, _writable: bool) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.0.curve.curve, false),
            AccountMeta::new_readonly(pda::canonical_pool(&self.0.keys.mint), false),
        ]
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        Vec::new()
    }
}

/// The program's own venue state of `pm` now: the host build of `venues::read_state` on the
/// ReadSet's accounts as the svm holds them (what the next instruction would read).
pub fn host_state(env: &Env, pm: &PumpMarket) -> shortwin::venues::VenueState {
    host_state_with(env, pm, None)
}

/// The supply of `pm`'s mint now (`supply` @36), as the program's exits, cranks, settles and opens
/// read it from the named mint (`venues::mint_supply`).
pub fn mint_supply(env: &Env, pm: &PumpMarket) -> u64 {
    let m = env.svm.get_account(&pm.keys.mint).expect("mint");
    shortwin_venue_state::mint::read_base(&m.owner.to_bytes(), &m.data)
        .expect("mint")
        .supply
}

/// [`host_state`] with the mint's `supply` (`venues::read_state_with`; `None` is
/// `venues::read_state`).
pub fn host_state_with(
    env: &Env,
    pm: &PumpMarket,
    supply: Option<u64>,
) -> shortwin::venues::VenueState {
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
    let kind = m.venue_kind().expect("venue");
    shortwin::venues::read_state_with(&m, kind, &infos, env.now(), supply)
}

/// `crank_price` on `mm` that must succeed.
pub fn crank(env: &mut Env, mm: &impl VenueMarket) -> TransactionMetadata {
    ok(env.send(&[crank_price_ix(mm)], &[]))
}

/// The one `VenueStateChanged` of `meta`, checked `from → to`.
pub fn venue_changed(
    meta: &TransactionMetadata,
    from: VenueKind,
    to: VenueKind,
) -> shortwin::events::VenueStateChanged {
    let ev: shortwin::events::VenueStateChanged = events::one(meta);
    assert_eq!((ev.from, ev.to), (from as u8, to as u8), "the transition");
    ev
}

/// No `VenueStateChanged` in `meta`.
pub fn no_venue_change(meta: &TransactionMetadata) {
    assert!(
        events::of::<shortwin::events::VenueStateChanged>(meta).is_empty(),
        "no venue transition"
    );
}

/// An external user buys `rT − rt_left` on the curve (real `buy_v2`), leaving `rt_left`; 0
/// completes it.
pub fn buy_down_to(env: &mut Env, pm: &PumpMarket, rt_left: u64) -> CurveUser {
    let u = CurveUser::new(env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(env, &pm.curve, rt - rt_left, u64::MAX));
    assert_eq!(pm.curve.state(&env.svm).real_token_reserves, rt_left);
    u
}

/// A keeper (funded keypair).
pub fn keeper(env: &mut Env) -> solana_keypair::Keypair {
    env.funded_keypair()
}

/// `sol_settle` of `b`'s position `nonce` on `mm` by `keeper`: the result.
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

/// The `SolSettle` event of a successful `sol_settle`, if it settled.
pub fn settled(meta: &TransactionMetadata) -> Option<shortwin::events::SolSettle> {
    events::of::<shortwin::events::SolSettle>(meta).pop()
}

/// `K_s` of a position (its locked settlement-keeper terms).
pub fn k_s(p: &ShortPosition) -> u64 {
    let pct = u128::from(p.v) * u128::from(p.settle_keeper_bps) / 10_000;
    (u128::from(p.settle_keeper_fixed) + pct)
        .min(u128::from(p.settle_keeper_cap))
        .min(u128::from(p.v)) as u64
}

/// Warps to `deadline + after` of `b`'s position `nonce`.
pub fn warp_to_deadline(env: &mut Env, pm: &PumpMarket, b: &Pubkey, nonce: u64, after: i64) {
    let to = env.position(pm, b, nonce).deadline + after;
    let now = env.now();
    if to > now {
        env.warp(to - now);
    }
}

/// Moves `amount` tokens from a curve user's (planted) balance into `to` (`TransferChecked`,
/// signed by the user): a donation anyone can make.
pub fn donate_tokens(env: &mut Env, pm: &PumpMarket, to: &Pubkey, amount: u64) {
    let u = CurveUser::new(env, &pm.curve);
    fork::set_token_amount(&mut env.svm, &u.ata, amount);
    let mut data = vec![12u8];
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(6);
    let ix = Instruction {
        program_id: pm.keys.token_program,
        accounts: vec![
            AccountMeta::new(u.ata, false),
            AccountMeta::new_readonly(pm.keys.mint, false),
            AccountMeta::new(*to, false),
            AccountMeta::new_readonly(u.kp.pubkey(), true),
        ],
        data,
    };
    ok(env.send(&[ix], &[&u.kp]));
}
