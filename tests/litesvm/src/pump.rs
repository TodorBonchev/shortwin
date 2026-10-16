//! Pump-curve **markets** on the mainnet fork (M5; ARCH §2.5, §3.2, §3.12 PumpCurve, §5.4): the
//! release short.win trading against the **dumped** pump, pump-fees and PumpSwap programs and the
//! pump coins of [`crate::fork`] (`tests/fixtures/`, group `pump_state`).
//!
//! - [`Env::pump_market`]: [`Env::fork_pump_coin`] (release short.win, the three pump programs,
//!   the pump statics, the coin's accounts, mainnet's Rent, the snapshot clock), then
//!   `create_market(Auto)` (real instruction: `classify_pump` lists the live curve as
//!   `PumpCurve`, creates the trader's base ATA and pump UVA), a lender's deposit (the mainnet
//!   mints have no mint authority: the lender's balance is planted) and the listing warm-up
//!   ([`PumpSpec`]).
//! - [`PumpMarket`]: the market's keys and its venue's account lists in ARCH §3.12's order (the
//!   ReadSet C0–C6, the TradeExtras T0–T9), built from the **fixture's** keys and the harness's
//!   own derivations, not from the program's key functions; it implements [`VenueMarket`], so
//!   every builder of [`crate::engine`] (`open_short_ix`, `close_short_ix`, `crank_price_ix`,
//!   `liquidate_ix`, `expire_ix`, `sol_settle_ix`) and [`Env::open`] / [`Env::close`] work on it.
//! - [`create_pump_market_ix`]: `create_market(Auto)` with the pump init set (pump program, the
//!   trader's UVA, pump's event authority) after the detection set.
//! - [`sweep_trader_pump_ix`]: `sweep_trader` with a pump market's token leg (and the trader's
//!   pump UVA, M5-SEC-1); [`claim_cashback_ix`]: pump's permissionless `claim_cashback` for a
//!   user who does not sign (T-CASHBACK, pump part).
//! - [`CurveUser`]: a normal user of the curve (its base ATA and pump UVA created first), for
//!   moving the curve with real `buy_v2` / `sell_v2` trades, and building pump's v3 trades
//!   (`buy_v3`, `sell_v3`, `buy_exact_quote_in_v3`; [`with_partial_fill`] for the optional
//!   trailing byte) — the third-party moves of the 2026-10-08 build's synthetic migration.
//! - Graduation on the real ELFs: [`migrate_v2_ix`] (27 IDL accounts + the two boost accounts,
//!   `idl/pump.json` of pump-public-docs), [`NewPool`] (the canonical pool it creates, derived),
//!   [`pamm_extend_account_ix`] and [`pamm_buy_ix`] (PumpSwap `buy` with its remaining
//!   accounts, R-VEN §2.2) — the execution path `B̂_str` prices (ARCH §4.3, §5.4). (The straddle
//!   bundle's `complete_curve` helper, A66, is retired with pump's v3 exits: pump-final D2.)
//! - [`cu_limit_ix`] and ARCH §6.1's PROPOSED limits ([`CURVE_TRADE_CU`],
//!   [`CURVE_KEEPER_EXIT_CU`]); [`PumpMarket`]'s `VenueMarket::cu_limit` makes `Env::open` /
//!   `Env::close` request the first.
//!
//! Every instruction goes through [`Env::send`] (the invariant hooks, which see pump markets
//! through `invariants::venue_view`).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_FEE_CONFIG,
    PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, PUMP_AMM_PROGRAM_ID,
    PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG, PUMP_FEES_PROGRAM_ID, PUMP_GLOBAL,
    PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
    WSOL_MINT,
};
use shortwin::instructions::create_market::VENUE_HINT_AUTO;
use shortwin_venue_state::{pump, pump_amm};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::engine::VenueMarket;
use crate::fixtures::MarketKeys;
use crate::fork::{self, PumpCoin, PumpCurve, PumpV2, PumpV3};
use crate::ix::{self, LenderKeys};
use crate::{Env, TxResult, errors, pda};

fn m(key: Pubkey, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(key, false)
    } else {
        AccountMeta::new_readonly(key, false)
    }
}

/// `ATA(owner, WSOL)` under SPL Token.
pub fn wsol_ata(owner: &Pubkey) -> Pubkey {
    pda::ata(owner, &WSOL_MINT, &TOKEN_PROGRAM_ID)
}

/// `PDA(pfee, ["sharing-config", mint])`.
pub fn sharing_config(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[pump::SHARING_CONFIG_SEED, mint.as_ref()],
        &PUMP_FEES_PROGRAM_ID,
    )
    .0
}

/// The pump init set of a pump listing (after the detection set): `[pump program, the trader's
/// pump UVA (W), pump event authority]`.
pub fn pump_init_set(market: &Pubkey) -> Vec<AccountMeta> {
    let trader = pda::trader(market).0;
    vec![
        m(PUMP_PROGRAM_ID, false),
        m(fork::pump_user_volume_accumulator(&trader), true),
        m(PUMP_EVENT_AUTHORITY, false),
    ]
}

/// `create_market(Auto, u64::MAX)` of `c`'s mint with the pump init set.
pub fn create_pump_market_ix(creator: &Pubkey, c: &PumpCurve) -> Instruction {
    let ix = ix::create_market(
        creator,
        &c.mint,
        &c.token_program.id(),
        VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    let market = pda::market(&c.mint).0;
    ix::with_remaining(ix, &pump_init_set(&market))
}

/// A pump-curve market on the fork: the curve's keys, the market's keys, its lender and the
/// recipients its trades name.
pub struct PumpMarket {
    pub curve: PumpCurve,
    pub keys: MarketKeys,
    /// `ATA(trader, mint)` (created at listing).
    pub trader_base_ata: Pubkey,
    /// The lender that seeded the market (and its token account).
    pub lender: Keypair,
    pub lender_account: Pubkey,
    /// The fee and buyback recipients the trades name (from the live `Global`).
    pub fee_recipient: Pubkey,
    pub buyback_recipient: Pubkey,
    /// The creator vault the trades name (re-derive with [`PumpMarket::refresh_creator`] after
    /// a creator change).
    pub creator_vault: Pubkey,
}

impl PumpMarket {
    /// The market.
    pub fn market(&self) -> Pubkey {
        self.keys.market
    }

    /// The coin role.
    pub fn coin(&self) -> PumpCoin {
        self.curve.coin
    }

    /// The trader PDA.
    pub fn trader(&self) -> Pubkey {
        self.keys.trader
    }

    /// Re-derives the creator vault from the live curve (after `set_curve_creator`).
    pub fn refresh_creator(&mut self, svm: &LiteSVM) {
        self.creator_vault = self.curve.creator_vault(svm);
    }

    /// Chooses other recipients (fee recipient `i`, buyback recipient `j` of the live `Global`).
    pub fn use_recipients(&mut self, svm: &LiteSVM, i: usize, j: usize) {
        self.fee_recipient = fork::pump_fee_recipient(svm, i);
        self.buyback_recipient = fork::pump_buyback_fee_recipient(svm, j);
    }

    /// The lender's keys for the lender builders.
    pub fn lender_keys(&self) -> LenderKeys {
        LenderKeys {
            owner: self.lender.pubkey(),
            mint: self.keys.mint,
            token_program: self.keys.token_program,
            owner_token_account: self.lender_account,
        }
    }

    /// The TradeExtras T0–T9 as a list of keys (ARCH §3.12 order).
    pub fn trade_keys(&self) -> Vec<Pubkey> {
        self.trade_extras().iter().map(|a| a.pubkey).collect()
    }
}

impl VenueMarket for PumpMarket {
    fn keys(&self) -> &MarketKeys {
        &self.keys
    }

    /// C0 curve (W), C1 `Global`, C2 pump `FeeConfig`, C3 the canonical pool, C4 the curve's
    /// base ATA (W), C5 PumpSwap `FeeConfig`, C6 PumpSwap `GlobalConfig`.
    fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        vec![
            m(self.curve.curve, writable),
            m(PUMP_GLOBAL, false),
            m(PUMP_FEE_CONFIG, false),
            m(pda::canonical_pool(&self.keys.mint), false),
            m(self.curve.curve_base_ata, writable),
            m(PUMP_AMM_FEE_CONFIG, false),
            m(PUMP_AMM_GLOBAL_CONFIG, false),
        ]
    }

    /// T0–T9 (pump-final §6.1.1; ARCH §3.12 for pump's v3 trades): pump, its event authority,
    /// WSOL, Tokenkeg, the curve's WSOL ATA (W), the trader's WSOL ATA (W), the trader's pump UVA
    /// (W), the buyback recipient (W), the trader's base ATA (W), System.
    fn trade_extras(&self) -> Vec<AccountMeta> {
        v3_trade_extras(
            &self.keys,
            &self.curve,
            &self.trader_base_ata,
            &self.buyback_recipient,
        )
    }

    /// ARCH §6.1's PROPOSED limit for curve opens and closes. On pump's v3 trades opens measure
    /// 139.1–154.6 k CU and closes 108.2–135.8 k (`tests/bench/m5_cu`), below the 200 k default.
    fn cu_limit(&self) -> Option<u32> {
        Some(CURVE_TRADE_CU)
    }
}

/// A curve market's TradeExtras for pump's v3 trades (pump-final §6.1.1), from the harness's own
/// derivations: T0 pump, T1 its event authority, T2 WSOL, T3 Tokenkeg, T4 `ATA(curve, WSOL)` (W),
/// T5 `ATA(trader, WSOL)` (W), T6 the trader's pump UVA (W), T7 `buyback` (W), T8 the trader's
/// base ATA (W), T9 System.
pub fn v3_trade_extras(
    keys: &MarketKeys,
    curve: &PumpCurve,
    trader_base_ata: &Pubkey,
    buyback: &Pubkey,
) -> Vec<AccountMeta> {
    let trader = keys.trader;
    vec![
        m(PUMP_PROGRAM_ID, false),
        m(PUMP_EVENT_AUTHORITY, false),
        m(WSOL_MINT, false),
        m(TOKEN_PROGRAM_ID, false),
        m(wsol_ata(&curve.curve), true),
        m(wsol_ata(&trader), true),
        m(fork::pump_user_volume_accumulator(&trader), true),
        m(*buyback, true),
        m(*trader_base_ata, true),
        m(anchor_lang::system_program::ID, false),
    ]
}

/// The v2-era TradeExtras T0–T19 of a curve market (shortwin 1.0.0's adapter, before pump-final
/// §6): the 20 keys a client or keeper built for the v2 trades, for the tests that the v3 adapter
/// refuses them (`BadRemainingAccounts`) and that a shortwin 1.0.0 build still takes them.
pub fn v2_trade_extras(env: &Env, pm: &PumpMarket) -> Vec<AccountMeta> {
    let trader = pm.keys.trader;
    let uva = fork::pump_user_volume_accumulator(&trader);
    let fee_recipient = fork::pump_fee_recipient(&env.svm, 0);
    let creator_vault = pm.curve.creator_vault(&env.svm);
    vec![
        m(PUMP_PROGRAM_ID, false),
        m(PUMP_EVENT_AUTHORITY, false),
        m(PUMP_FEES_PROGRAM_ID, false),
        m(WSOL_MINT, false),
        m(TOKEN_PROGRAM_ID, false),
        m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
        m(fee_recipient, true),
        m(wsol_ata(&fee_recipient), true),
        m(pm.buyback_recipient, true),
        m(wsol_ata(&pm.buyback_recipient), true),
        m(wsol_ata(&pm.curve.curve), true),
        m(wsol_ata(&trader), true),
        m(creator_vault, true),
        m(wsol_ata(&creator_vault), true),
        m(sharing_config(&pm.keys.mint), false),
        m(PUMP_GLOBAL_VOLUME_ACCUMULATOR, false),
        m(uva, true),
        m(wsol_ata(&uva), true),
        m(pm.trader_base_ata, true),
        m(anchor_lang::system_program::ID, false),
    ]
}

/// A pump market's parameters.
#[derive(Clone, Copy, Debug)]
pub struct PumpSpec {
    pub coin: PumpCoin,
    /// Tokens the lender deposits (planted in its account first).
    pub lend: u64,
    /// Seconds to warp after listing (≥ `market_warmup_s` = 50 by default).
    pub warmup: i64,
}

impl PumpSpec {
    /// `coin` with `lend` tokens lent and 60 s of warm-up.
    pub fn new(coin: PumpCoin, lend: u64) -> Self {
        Self {
            coin,
            lend,
            warmup: 60,
        }
    }
}

impl Env {
    /// Lists `c`'s mint with `create_market(Auto)` and the pump init set, as a new funded
    /// creator.
    pub fn list_pump(&mut self, c: &PumpCurve) -> TxResult {
        let creator = self.funded_keypair();
        let ix = create_pump_market_ix(&creator.pubkey(), c);
        self.send(&[ix], &[&creator])
    }

    /// A pump market per `spec` on a fresh fork (module docs), through real instructions.
    pub fn pump_market(spec: PumpSpec) -> (Self, PumpMarket) {
        let (mut env, c) = Self::fork_pump_coin(spec.coin);
        errors::ok(env.list_pump(&c));
        let pm = env.pump_market_listed(spec, c);
        (env, pm)
    }

    /// The [`PumpMarket`] of an already listed curve: seeds the lender and warps (`spec`).
    pub fn pump_market_listed(&mut self, spec: PumpSpec, c: PumpCurve) -> PumpMarket {
        let market = pda::market(&c.mint).0;
        let program = c.token_program;
        let keys = MarketKeys {
            mint: c.mint,
            token_program: program.id(),
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            trader: pda::trader(&market).0,
        };
        let lender = self.funded_keypair();
        let lender_account = fork::fund_token(self, &lender.pubkey(), &c.mint, program, spec.lend);
        let pm = PumpMarket {
            trader_base_ata: pda::ata(&keys.trader, &c.mint, &program.id()),
            fee_recipient: fork::pump_fee_recipient(&self.svm, 0),
            buyback_recipient: fork::pump_buyback_fee_recipient(&self.svm, 0),
            creator_vault: c.creator_vault(&self.svm),
            curve: c,
            keys,
            lender,
            lender_account,
        };
        if spec.lend > 0 {
            let lender = pm.lender.insecure_clone();
            errors::ok(self.send(&[ix::deposit(&pm.lender_keys(), spec.lend, 0)], &[&lender]));
        }
        self.warp(spec.warmup);
        pm
    }
}

/// `sweep_trader()` of a pump market: the token leg `[trader_base_ata, pool_vault, mint, token
/// program, the trader's pump UVA]` first (the UVA since M5-SEC-1: on the curve kinds the sweep
/// also sets `wipe_rewards_claimed` when it holds no claimable cashback), then the latest
/// `ShareEpoch` record while a full-wipe record is live.
pub fn sweep_trader_pump_ix(env: &Env, pm: &PumpMarket) -> Instruction {
    let base = crate::engine::sweep_trader_ix_for(env, &pm.market());
    let (head, rest) = base
        .accounts
        .split_at(base.accounts.len() - usize::from(wipe_live(env, pm)));
    let mut accounts = head.to_vec();
    accounts.extend([
        m(pm.trader_base_ata, true),
        m(pm.keys.pool_vault, true),
        m(pm.keys.mint, false),
        m(pm.keys.token_program, false),
        m(fork::pump_user_volume_accumulator(&pm.trader()), false),
    ]);
    accounts.extend_from_slice(rest);
    Instruction {
        program_id: base.program_id,
        accounts,
        data: base.data,
    }
}

fn wipe_live(env: &Env, pm: &PumpMarket) -> bool {
    let mk = env.market(&pm.market());
    mk.total_shares.get() == 0 && mk.has_flag(shortwin::state::MARKET_FLAG_WIPE_LIVE)
}

/// A normal user of a curve: a funded keypair with its base ATA and its pump UVA (created
/// through `Env::send` before any trade, so a trade's deltas carry no account creation).
pub struct CurveUser {
    pub kp: Keypair,
    pub ata: Pubkey,
}

impl CurveUser {
    /// A new user of `c`.
    pub fn new(env: &mut Env, c: &PumpCurve) -> Self {
        let kp = env.funded_keypair();
        let payer = env.payer.pubkey();
        let program = c.token_program.id();
        let ixs = [
            fork::create_ata_idempotent_ix(&payer, &kp.pubkey(), &c.mint, &program),
            fork::pump_init_uva_ix(&payer, &kp.pubkey()),
        ];
        errors::ok(env.send(&ixs, &[]));
        Self {
            ata: pda::ata(&kp.pubkey(), &c.mint, &program),
            kp,
        }
    }

    /// `buy_v2(q, max_sol_cost)` with the first recipients of the live `Global`.
    pub fn buy(&self, env: &mut Env, c: &PumpCurve, q: u64, max_sol_cost: u64) -> TxResult {
        self.trade(env, c, PumpV2::Buy, q, max_sol_cost)
    }

    /// `sell_v2(q, min_sol_output)` (plants the balance first if short).
    pub fn sell(&self, env: &mut Env, c: &PumpCurve, q: u64, min_sol_output: u64) -> TxResult {
        let have = crate::read::token_amount(&env.svm, &self.ata).unwrap_or(0);
        if have < q {
            fork::set_token_amount(&mut env.svm, &self.ata, q);
        }
        self.trade(env, c, PumpV2::Sell, q, min_sol_output)
    }

    fn trade(&self, env: &mut Env, c: &PumpCurve, kind: PumpV2, a: u64, b: u64) -> TxResult {
        let fr = fork::pump_fee_recipient(&env.svm, 1);
        let bb = fork::pump_buyback_fee_recipient(&env.svm, 1);
        let ix = c.v2_ix(&env.svm, kind, &self.kp.pubkey(), &self.ata, &fr, &bb, a, b);
        let signer = self.kp.insecure_clone();
        env.send(&[ix], &[&signer])
    }

    /// The buyback recipient the v3 builders name: the second of the live `Global` (as the v2
    /// trades').
    pub fn v3_buyback_recipient(svm: &LiteSVM) -> Pubkey {
        fork::pump_buyback_fee_recipient(svm, 1)
    }

    fn v3_ix(&self, svm: &LiteSVM, c: &PumpCurve, kind: PumpV3, a: u64, b: u64) -> Instruction {
        let bb = Self::v3_buyback_recipient(svm);
        c.v3_ix(kind, &self.kp.pubkey(), &self.ata, &bb, a, b, None)
    }

    /// pump `buy_v3(amount, max_sol_cost)` by this user (17 accounts, 24 bytes: no
    /// `partial_fill`); past `rT` on the 2026-10-08 build it completes the curve and buys the
    /// rest from the would-be pool (the synthetic migration).
    pub fn buy_v3_ix(
        &self,
        svm: &LiteSVM,
        c: &PumpCurve,
        amount: u64,
        max_sol_cost: u64,
    ) -> Instruction {
        self.v3_ix(svm, c, PumpV3::Buy, amount, max_sol_cost)
    }

    /// pump `sell_v3(amount, min_sol_output)` by this user (24 bytes).
    pub fn sell_v3_ix(
        &self,
        svm: &LiteSVM,
        c: &PumpCurve,
        amount: u64,
        min_out: u64,
    ) -> Instruction {
        self.v3_ix(svm, c, PumpV3::Sell, amount, min_out)
    }

    /// pump `buy_exact_quote_in_v3(spendable_quote_in, min_tokens_out)` by this user (24 bytes).
    pub fn buy_exact_quote_in_v3_ix(
        &self,
        svm: &LiteSVM,
        c: &PumpCurve,
        spend: u64,
        min_out: u64,
    ) -> Instruction {
        self.v3_ix(svm, c, PumpV3::BuyExactQuoteIn, spend, min_out)
    }

    /// Sends `ix` signed by this user (for a sale, plant the balance first: [`CurveUser::plant`]).
    pub fn send(&self, env: &mut Env, ix: Instruction) -> TxResult {
        let signer = self.kp.insecure_clone();
        env.send(&[ix], &[&signer])
    }

    /// Plants a token balance of at least `q` in the user's base ATA (these mints have no mint
    /// authority; pump reads only the account).
    pub fn plant(&self, env: &mut Env, q: u64) {
        let have = crate::read::token_amount(&env.svm, &self.ata).unwrap_or(0);
        if have < q {
            fork::set_token_amount(&mut env.svm, &self.ata, q);
        }
    }
}

/// `ix` (a v3 buy, 24 bytes) with the optional trailing `partial_fill` byte set to `flag` (25
/// bytes; pump ignores it on non-mayhem coins).
pub fn with_partial_fill(mut ix: Instruction, flag: bool) -> Instruction {
    assert_eq!(ix.data.len(), 24, "a v3 trade without partial_fill");
    assert_ne!(
        ix.data[..8],
        fork::PUMP_SELL_V3,
        "sell_v3 has no partial_fill"
    );
    ix.data.push(u8::from(flag));
    ix
}

// ---- graduation on the real ELFs (the execution path `B̂_str` prices, ARCH §4.3, §5.4) ------

/// `migrate_v2`'s discriminator (`idl/pump.json`, R-PUMP §3.6).
pub const PUMP_MIGRATE_V2: [u8; 8] = [0xbb, 0xcb, 0x12, 0x1f, 0xce, 0xed, 0xfe, 0x29];
/// PumpSwap `buy`'s discriminator (R-VEN §2.2).
pub const PAMM_BUY: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
/// PumpSwap `extend_account`'s discriminator (R-VEN §1.2).
pub const PAMM_EXTEND_ACCOUNT: [u8; 8] = [0xea, 0x66, 0xc2, 0xcb, 0x96, 0x48, 0x3e, 0xe5];

/// The canonical PumpSwap pool `migrate_v2` creates for a SOL curve's mint, derived (R-VEN §1.3,
/// R-PUMP §3.6): the pool, its vaults (ATAs of the pool), the LP mint, the boost vault and its
/// WSOL ATA, the pool authority.
#[derive(Clone, Copy, Debug)]
pub struct NewPool {
    pub mint: Pubkey,
    pub base_program: Pubkey,
    pub pool_authority: Pubkey,
    pub pool: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub lp_mint: Pubkey,
    pub boost_vault: Pubkey,
    pub boost_vault_ata: Pubkey,
}

impl NewPool {
    /// The pool of `c`'s mint.
    pub fn of(c: &PumpCurve) -> Self {
        let mint = c.mint;
        let base_program = c.token_program.id();
        let pool_authority =
            Pubkey::find_program_address(&[b"pool-authority", mint.as_ref()], &PUMP_PROGRAM_ID).0;
        let pool = pda::canonical_pool(&mint);
        let lp_mint =
            Pubkey::find_program_address(&[b"pool_lp_mint", pool.as_ref()], &PUMP_AMM_PROGRAM_ID).0;
        let boost_vault = Pubkey::find_program_address(
            &[pump_amm::BOOST_VAULT_SEED, pool.as_ref()],
            &PUMP_AMM_PROGRAM_ID,
        )
        .0;
        Self {
            mint,
            base_program,
            pool_authority,
            pool,
            base_vault: pda::ata(&pool, &mint, &base_program),
            quote_vault: wsol_ata(&pool),
            lp_mint,
            boost_vault,
            boost_vault_ata: wsol_ata(&boost_vault),
        }
    }

    /// The pool as the reader sees it (`None` before the migration).
    pub fn state(&self, svm: &LiteSVM) -> Option<pump_amm::Pool> {
        let acc = svm.get_account(&self.pool)?;
        pump_amm::read_pool(&acc.owner.to_bytes(), &acc.data).ok()
    }

    /// PumpSwap's pricing reserves now (vaults + VQR).
    pub fn reserves(&self, svm: &LiteSVM) -> Option<pump_amm::PoolReserves> {
        self.state(svm)?.reserves(
            fork::vault_amount(svm, &self.base_vault),
            fork::vault_amount(svm, &self.quote_vault),
        )
    }
}

/// `migrate_v2` of `c` by `user` (anyone, R-PUMP §7.4): the 27 IDL accounts (`idl/pump.json`,
/// pump-public-docs) plus the two boost accounts mainnet passes (R-PUMP §3.6).
pub fn migrate_v2_ix(svm: &LiteSVM, user: &Pubkey, c: &PumpCurve) -> Instruction {
    let p = NewPool::of(c);
    let global = svm.get_account(&PUMP_GLOBAL).expect("pump Global");
    let withdraw_authority = Pubkey::new_from_array(
        pump::read_global(&global.owner.to_bytes(), &global.data)
            .expect("Global")
            .withdraw_authority(),
    );
    let t22 = TOKEN_2022_PROGRAM_ID;
    let accounts = vec![
        m(PUMP_GLOBAL, false),
        m(withdraw_authority, true),
        m(c.mint, false),
        m(WSOL_MINT, false),
        m(c.curve, true),
        m(c.curve_base_ata, true),
        m(wsol_ata(&c.curve), true),
        AccountMeta::new(*user, true),
        m(anchor_lang::system_program::ID, false),
        m(PUMP_AMM_PROGRAM_ID, false),
        m(p.pool, true),
        m(p.pool_authority, true),
        m(pda::ata(&p.pool_authority, &c.mint, &p.base_program), true),
        m(wsol_ata(&p.pool_authority), true),
        m(PUMP_AMM_GLOBAL_CONFIG, false),
        m(p.lp_mint, true),
        m(pda::ata(&p.pool_authority, &p.lp_mint, &t22), true),
        m(p.base_vault, true),
        m(p.quote_vault, true),
        m(p.base_program, false),
        m(TOKEN_PROGRAM_ID, false),
        m(t22, false),
        m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
        m(PUMP_AMM_EVENT_AUTHORITY, false),
        m(
            anchor_lang::prelude::pubkey!("SysvarRent111111111111111111111111111111111"),
            false,
        ),
        m(PUMP_EVENT_AUTHORITY, false),
        m(PUMP_PROGRAM_ID, false),
        m(p.boost_vault, false),
        m(p.boost_vault_ata, true),
    ];
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts,
        data: PUMP_MIGRATE_V2.to_vec(),
    }
}

/// pump `claim_cashback`'s discriminator (`idl/pump.json`; ARCH [V11] `253a237ebe35e4c5`).
pub const PUMP_CLAIM_CASHBACK: [u8; 8] = [0x25, 0x3a, 0x23, 0x7e, 0xbe, 0x35, 0xe4, 0xc5];

/// pump `claim_cashback` for `user` (the IDL's `[user W, user_volume_accumulator W, system,
/// event_authority, program]`; `user` is **not** a signer — T-CASHBACK: anyone may send it, the
/// cashback goes to `user`).
pub fn claim_cashback_ix(user: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts: vec![
            m(*user, true),
            m(fork::pump_user_volume_accumulator(user), true),
            m(anchor_lang::system_program::ID, false),
            m(PUMP_EVENT_AUTHORITY, false),
            m(PUMP_PROGRAM_ID, false),
        ],
        data: PUMP_CLAIM_CASHBACK.to_vec(),
    }
}

/// PumpSwap `extend_account` of `account` (a pool under 300 bytes needs it before its first
/// trade, R-VEN §1.2): `[account W, user S W, system, event_authority, program]`.
pub fn pamm_extend_account_ix(user: &Pubkey, account: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            m(*account, true),
            AccountMeta::new(*user, true),
            m(anchor_lang::system_program::ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data: PAMM_EXTEND_ACCOUNT.to_vec(),
    }
}

/// PumpSwap `buy(base_amount_out, max_quote_amount_in)` of `p` by `user` (R-VEN §2.2: the 23
/// fixed accounts, then pool-v2 when the coin creator is set, the buyback recipient and its WSOL
/// ATA); `user_base` receives the tokens, `user_wsol` (a WSOL account `user` owns) pays. The
/// first protocol and buyback recipients of the live `GlobalConfig`.
#[allow(clippy::too_many_arguments)]
pub fn pamm_buy_ix(
    svm: &LiteSVM,
    p: &NewPool,
    user: &Pubkey,
    user_base: &Pubkey,
    user_wsol: &Pubkey,
    base_out: u64,
    max_in: u64,
) -> Instruction {
    let pool = p.state(svm).expect("pool");
    let gc = svm
        .get_account(&PUMP_AMM_GLOBAL_CONFIG)
        .expect("GlobalConfig");
    let g = pump_amm::read_global_config(&gc.owner.to_bytes(), &gc.data).expect("GlobalConfig");
    let protocol = Pubkey::new_from_array(g.protocol_fee_recipients()[0]);
    let buyback = Pubkey::new_from_array(g.buyback_fee_recipients()[0]);
    let coin_creator = Pubkey::new_from_array(pool.coin_creator);
    let vault_authority = Pubkey::find_program_address(
        &[b"creator_vault", coin_creator.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0;
    let uva = Pubkey::find_program_address(
        &[b"user_volume_accumulator", user.as_ref()],
        &PUMP_AMM_PROGRAM_ID,
    )
    .0;
    let mut accounts = vec![
        m(p.pool, true),
        AccountMeta::new(*user, true),
        m(PUMP_AMM_GLOBAL_CONFIG, false),
        m(p.mint, false),
        m(WSOL_MINT, false),
        m(*user_base, true),
        m(*user_wsol, true),
        m(p.base_vault, true),
        m(p.quote_vault, true),
        m(protocol, false),
        m(wsol_ata(&protocol), true),
        m(p.base_program, false),
        m(TOKEN_PROGRAM_ID, false),
        m(anchor_lang::system_program::ID, false),
        m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
        m(PUMP_AMM_EVENT_AUTHORITY, false),
        m(PUMP_AMM_PROGRAM_ID, false),
        m(wsol_ata(&vault_authority), true),
        m(vault_authority, false),
        m(PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, false),
        m(uva, true),
        m(PUMP_AMM_FEE_CONFIG, false),
        m(PUMP_FEES_PROGRAM_ID, false),
    ];
    if pool.is_cashback_coin {
        accounts.push(m(wsol_ata(&uva), true));
    }
    if coin_creator != Pubkey::default() {
        let pool_v2 =
            Pubkey::find_program_address(&[b"pool-v2", p.mint.as_ref()], &PUMP_AMM_PROGRAM_ID).0;
        accounts.push(m(pool_v2, false));
    }
    accounts.extend([m(buyback, false), m(wsol_ata(&buyback), true)]);
    let mut data = PAMM_BUY.to_vec();
    data.extend_from_slice(&base_out.to_le_bytes());
    data.extend_from_slice(&max_in.to_le_bytes());
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts,
        data,
    }
}

/// ARCH §6.1's PROPOSED transaction CU limit for curve opens and closes.
pub const CURVE_TRADE_CU: u32 = 250_000;
/// ARCH §6.1's PROPOSED transaction CU limit for curve liquidations and expiries.
pub const CURVE_KEEPER_EXIT_CU: u32 = 300_000;

/// `ComputeBudgetInstruction::SetComputeUnitLimit(units)` (tag 2, `u32` LE): `migrate_v2` needs
/// ~320 k CU on mainnet (R-PUMP §6.8), above the 200 k default.
pub fn cu_limit_ix(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: anchor_lang::prelude::pubkey!("ComputeBudget111111111111111111111111111111"),
        accounts: Vec::new(),
        data,
    }
}
