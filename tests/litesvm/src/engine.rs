//! The position engine on the mock venue (M2): a ready-to-trade environment, mock markets,
//! borrowers and the instruction builders of every M2 instruction.
//!
//! - [`Env::test_venue`]: short.win's **test-venue** build (`VenueKind::Mock`; A49) with the mock
//!   venue deployed, the config initialized with the mock's venue bit enabled, and the mock's
//!   `VenueExtras` account (`["venue_extras", 6]`, which `initialize_config` creates only for the
//!   four real venues) installed empty.
//! - [`Env::mock_market`]: a mint, its mock pool with liquidity, `create_market(venue_hint =
//!   Mock)`, a lender deposit and the warm-up — through real instructions ([`MarketSpec`]).
//! - [`MockMarket`]: the market's keys and its remaining-account lists (ReadSet, TradeExtras).
//! - builders: [`open_short_ix`], [`close_short_ix`], [`crank_price_ix`],
//!   [`set_route_broken_ix`], [`set_venue_extras_ix`], [`sweep_trader_ix`] (M2-SEC-4), and the
//!   account lists of the pending tracks ([`add_margin_ix`], [`liquidate_ix`], [`expire_ix`],
//!   [`sol_settle_ix`]).
//! - [`Borrower`]: a funded borrower with its nonces; [`Env::open`] / [`Env::close`] run the real
//!   instructions with generous bounds.
//!
//! Every instruction goes through [`Env::send`], so the invariant hooks run after each one.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, MOCK_CPAMM_AUTHORITY, MOCK_CPAMM_PROGRAM_ID, TOKEN_PROGRAM_ID,
    VENUE_BIT_MOCK, VENUE_EXTRAS_VERSION, WSOL_MINT,
};
use shortwin::state::{ShortPosition, VenueExtras, VenueKind};
use shortwin::venues::ExtrasSel;
use shortwin::{ExtraEntryArg, PauseScope};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::fixtures::{self, MarketKeys};
use crate::ix::{self, LenderKeys};
use crate::mock::{self, MockPool, PoolFixture};
use crate::tokens::{self, TokenProgram};
use crate::{Env, TxResult, errors, pda, read};

/// `create_market`'s mock hint (`VenueKind::Mock as u8`).
pub const VENUE_HINT_MOCK: u8 = VenueKind::Mock as u8;

fn system_program() -> Pubkey {
    anchor_lang::system_program::ID
}

fn build(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction {
        program_id: shortwin::ID,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

impl Env {
    /// The ready-to-trade engine environment (module docs).
    pub fn test_venue() -> Self {
        let mut env = Self::new_test_venue_program();
        env.load_mock_cpamm();
        let mut args = env.default_init_args();
        args.venue_enabled |= VENUE_BIT_MOCK;
        errors::ok(env.initialize_config(args));
        env.install_mock_extras();
        env
    }

    /// Writes the mock's empty `VenueExtras` account (`["venue_extras", 6]`), as
    /// `initialize_config` writes the four real venues' (ARCH §2.3.2).
    pub fn install_mock_extras(&mut self) {
        let (key, bump) =
            Pubkey::find_program_address(&[VenueExtras::SEED, &[VENUE_HINT_MOCK]], &shortwin::ID);
        let mut ve: VenueExtras = bytemuck::Zeroable::zeroed();
        ve.version = VENUE_EXTRAS_VERSION;
        ve.bump = bump;
        ve.venue = VENUE_HINT_MOCK;
        fixtures::set_zc(&mut self.svm, &key, &ve, 0);
    }
}

/// The mock's `VenueExtras` account.
pub fn mock_extras_key() -> Pubkey {
    pda::venue_extras_seed(VENUE_HINT_MOCK)
}

// ---- markets ------------------------------------------------------------------------------

/// A mock market's parameters.
#[derive(Clone, Copy, Debug)]
pub struct MarketSpec {
    pub program: TokenProgram,
    pub decimals: u8,
    /// The mock's fee on the input, ppm (φ).
    pub fee_ppm: u32,
    /// Pool reserves: base tokens (raw) and SOL (lamports).
    pub base: u64,
    pub quote: u64,
    /// Tokens the lender deposits.
    pub lend: u64,
    /// Seconds to warp after listing (≥ `market_warmup_s` = 50 by default).
    pub warmup: i64,
}

impl Default for MarketSpec {
    /// 10⁹ tokens (6 decimals) against 1,000 SOL (`P = 10⁻³` lamport per raw unit), a 25 bps fee,
    /// 10 % of the pool lent, 60 s of warm-up.
    fn default() -> Self {
        Self {
            program: TokenProgram::Spl,
            decimals: 6,
            fee_ppm: 2_500,
            base: 1_000_000_000_000_000,
            quote: 1_000_000_000_000,
            lend: 100_000_000_000_000,
            warmup: 60,
        }
    }
}

/// A mock market: the pool, the market's keys and its lender.
pub struct MockMarket {
    pub pool: PoolFixture,
    pub keys: MarketKeys,
    pub spec: MarketSpec,
    pub trader_wsol: Pubkey,
    /// The lender that seeded the pool (and its token account).
    pub lender: Keypair,
    pub lender_account: Pubkey,
}

impl MockMarket {
    /// The mint.
    pub fn mint(&self) -> Pubkey {
        self.keys.mint
    }

    /// The market.
    pub fn market(&self) -> Pubkey {
        self.keys.market
    }

    /// The mock pool's keys.
    pub fn mock(&self) -> &MockPool {
        &self.pool.keys
    }

    /// The ReadSet `[pool, base_vault, quote_vault]` (writable in trades).
    pub fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        let m = |k: Pubkey| {
            if writable {
                AccountMeta::new(k, false)
            } else {
                AccountMeta::new_readonly(k, false)
            }
        };
        vec![
            m(self.pool.keys.pool),
            m(self.pool.keys.base_vault),
            m(self.pool.keys.quote_vault),
        ]
    }

    /// The TradeExtras `[mock program, pool authority, WSOL mint, SPL Token, trader_wsol (W)]`.
    pub fn trade_extras(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(MOCK_CPAMM_PROGRAM_ID, false),
            AccountMeta::new_readonly(MOCK_CPAMM_AUTHORITY, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new(self.trader_wsol, false),
        ]
    }

    /// The trading remaining accounts: `ReadSet ++ TradeExtras ++ extras`.
    pub fn trade_remaining(&self, extras: &[AccountMeta]) -> Vec<AccountMeta> {
        let mut v = self.read_set(true);
        v.extend(self.trade_extras());
        v.extend_from_slice(extras);
        v
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
}

/// A market the instruction builders and [`Env::open`] / [`Env::close`] can trade: its keys and
/// its venue's remaining-account lists (ARCH §3.0: `ReadSet ++ TradeExtras ++ Extras`). The mock
/// markets of this module and the Raydium markets of [`crate::raydium`] implement it.
pub trait VenueMarket {
    /// The market's keys.
    fn keys(&self) -> &MarketKeys;
    /// The venue's ReadSet (writable in trades).
    fn read_set(&self, writable: bool) -> Vec<AccountMeta>;
    /// The venue's TradeExtras.
    fn trade_extras(&self) -> Vec<AccountMeta>;
    /// The market.
    fn market(&self) -> Pubkey {
        self.keys().market
    }
    /// The mint.
    fn mint(&self) -> Pubkey {
        self.keys().mint
    }
    /// The trading remaining accounts: `ReadSet ++ TradeExtras ++ extras`.
    fn trade_remaining(&self, extras: &[AccountMeta]) -> Vec<AccountMeta> {
        let mut v = self.read_set(true);
        v.extend(self.trade_extras());
        v.extend_from_slice(extras);
        v
    }
    /// The compute-unit limit a client requests for this venue's opens and closes (ARCH §6.1,
    /// PROPOSED: 250 k on the pump venues); `None` = the runtime's 200 k default, which
    /// [`Env::open`] / [`Env::close`] then leave alone.
    fn cu_limit(&self) -> Option<u32> {
        None
    }
}

impl<T: VenueMarket + ?Sized> VenueMarket for &T {
    fn keys(&self) -> &MarketKeys {
        (**self).keys()
    }
    fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        (**self).read_set(writable)
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        (**self).trade_extras()
    }
    fn cu_limit(&self) -> Option<u32> {
        (**self).cu_limit()
    }
}

impl VenueMarket for MockMarket {
    fn keys(&self) -> &MarketKeys {
        &self.keys
    }
    fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        MockMarket::read_set(self, writable)
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        MockMarket::trade_extras(self)
    }
}

/// `create_market(Mock, u64::MAX)` for `pool`'s base mint: the detection set, then the mock
/// ReadSet.
pub fn create_mock_market_ix(creator: &Pubkey, pool: &MockPool) -> Instruction {
    let ix = ix::create_market(
        creator,
        &pool.base_mint,
        &pool.base_program.id(),
        VENUE_HINT_MOCK,
        &pda::canonical_pool(&pool.base_mint),
    );
    ix::with_remaining(
        ix,
        &[
            AccountMeta::new_readonly(pool.pool, false),
            AccountMeta::new_readonly(pool.base_vault, false),
            AccountMeta::new_readonly(pool.quote_vault, false),
        ],
    )
}

impl Env {
    /// A mock market per `spec` (module docs), through real instructions: the pool, its
    /// liquidity, `create_market(Mock)`, the lender's deposit, the warm-up.
    pub fn mock_market(&mut self, spec: MarketSpec) -> MockMarket {
        let pool = mock::new_pool(self, spec.program, spec.decimals, spec.fee_ppm);
        mock::seed(self, &pool, spec.base, spec.quote);
        let creator = self.funded_keypair();
        errors::ok(self.send(
            &[create_mock_market_ix(&creator.pubkey(), &pool.keys)],
            &[&creator],
        ));
        let market = pda::market(&pool.keys.base_mint).0;
        let keys = MarketKeys {
            mint: pool.keys.base_mint,
            token_program: spec.program.id(),
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            trader: pda::trader(&market).0,
        };
        let lender = self.funded_keypair();
        let payer = self.payer.insecure_clone();
        let lender_account = tokens::create_token_account(
            &mut self.svm,
            &payer,
            &keys.mint,
            &lender.pubkey(),
            spec.program,
        );
        let mm = MockMarket {
            trader_wsol: pda::trader_wsol(&market).0,
            pool,
            keys,
            spec,
            lender,
            lender_account,
        };
        if spec.lend > 0 {
            tokens::mint_to(
                &mut self.svm,
                &payer,
                &mm.keys.mint,
                &mm.lender_account,
                &mm.pool.mint_authority,
                spec.lend,
                spec.program,
            );
            let lender = mm.lender.insecure_clone();
            errors::ok(self.send(&[ix::deposit(&mm.lender_keys(), spec.lend, 0)], &[&lender]));
        }
        self.warp(spec.warmup);
        mm
    }

    /// `crank_price` on `mm` (must succeed).
    pub fn crank(&mut self, mm: &impl VenueMarket) {
        errors::ok(self.send(&[crank_price_ix(mm)], &[]));
    }
}

// ---- builders -----------------------------------------------------------------------------

/// `open_short(nonce, q, min_proceeds, max_margin, max_open_fee, sel)` with
/// `ReadSet ++ TradeExtras ++ extras`.
#[allow(clippy::too_many_arguments)]
pub fn open_short_ix(
    mm: &impl VenueMarket,
    borrower: &Pubkey,
    nonce: u64,
    q: u64,
    min_proceeds: u64,
    max_margin: u64,
    max_open_fee: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = mm.market();
    let ix = build(
        shortwin::accounts::OpenShort {
            borrower: *borrower,
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            oracle_cache: pda::oracle_cache(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: mm.mint(),
            pool_vault: mm.keys().pool_vault,
            market_vault: mm.keys().market_vault,
            trader: mm.keys().trader,
            token_program: mm.keys().token_program,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::OpenShort {
            nonce,
            q,
            min_proceeds,
            max_margin,
            max_open_fee,
            extras: sel,
        },
    );
    ix::with_remaining(ix, &mm.trade_remaining(extras))
}

/// `close_short(max_buyback_cost, sel)` of `borrower`'s position `nonce`.
pub fn close_short_ix(
    mm: &impl VenueMarket,
    borrower: &Pubkey,
    nonce: u64,
    max_buyback_cost: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = mm.market();
    let ix = build(
        shortwin::accounts::CloseShort {
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: mm.mint(),
            pool_vault: mm.keys().pool_vault,
            market_vault: mm.keys().market_vault,
            trader: mm.keys().trader,
            token_program: mm.keys().token_program,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CloseShort {
            max_buyback_cost,
            extras: sel,
        },
    );
    ix::with_remaining(ix, &mm.trade_remaining(extras))
}

/// `crank_price()` with the market's venue ReadSet (read-only).
pub fn crank_price_ix(mm: &impl VenueMarket) -> Instruction {
    let ix = build(
        shortwin::accounts::CrankPrice {
            config: pda::config(),
            market: mm.market(),
            mint: mm.mint(),
            oracle_cache: pda::oracle_cache(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CrankPrice {},
    );
    ix::with_remaining(ix, &mm.read_set(false))
}

/// `set_route_broken(scope, venue_bit, broken)`; `market` only for `PauseScope::Market`.
pub fn set_route_broken_ix(
    authority: &Pubkey,
    scope: PauseScope,
    venue_bit: u8,
    broken: bool,
    market: Option<Pubkey>,
) -> Instruction {
    build(
        shortwin::accounts::SetRouteBroken {
            authority: *authority,
            config: pda::config(),
            market,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetRouteBroken {
            scope,
            venue_bit,
            broken,
        },
    )
}

/// `set_venue_extras(venue, ix, entries)`.
pub fn set_venue_extras_ix(
    authority: &Pubkey,
    venue: u8,
    ix_kind: u8,
    entries: Vec<ExtraEntryArg>,
) -> Instruction {
    build(
        shortwin::accounts::SetVenueExtras {
            authority: *authority,
            config: pda::config(),
            venue_extras: pda::venue_extras_seed(venue),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetVenueExtras {
            venue,
            ix: ix_kind,
            entries,
        },
    )
}

/// `add_margin(amount)` (the account list; the body is its track's).
pub fn add_margin_ix(
    mm: &impl VenueMarket,
    borrower: &Pubkey,
    nonce: u64,
    amount: u64,
) -> Instruction {
    let market = mm.market();
    build(
        shortwin::accounts::AddMargin {
            borrower: *borrower,
            market,
            position: pda::position(&market, borrower, nonce).0,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::AddMargin { amount },
    )
}

/// `liquidate(sel)` by `keeper` (the account list; the body is its track's).
pub fn liquidate_ix(
    mm: &impl VenueMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
) -> Instruction {
    let market = mm.market();
    let ix = build(
        shortwin::accounts::Liquidate {
            keeper: *keeper,
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: mm.mint(),
            pool_vault: mm.keys().pool_vault,
            market_vault: mm.keys().market_vault,
            trader: mm.keys().trader,
            token_program: mm.keys().token_program,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Liquidate {
            extras: ExtrasSel::NONE,
        },
    );
    ix::with_remaining(ix, &mm.trade_remaining(&[]))
}

/// `expire(sel)` by `keeper` (the account list; the body is its track's).
pub fn expire_ix(
    mm: &impl VenueMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
) -> Instruction {
    let market = mm.market();
    let ix = build(
        shortwin::accounts::Expire {
            keeper: *keeper,
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: mm.mint(),
            pool_vault: mm.keys().pool_vault,
            market_vault: mm.keys().market_vault,
            trader: mm.keys().trader,
            token_program: mm.keys().token_program,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Expire {
            extras: ExtrasSel::NONE,
        },
    );
    ix::with_remaining(ix, &mm.trade_remaining(&[]))
}

/// `sol_settle()` by `keeper` (the account list; the body is its track's).
pub fn sol_settle_ix(
    env: &Env,
    mm: &impl VenueMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
) -> Instruction {
    let market = mm.market();
    let epoch = env.market(&market).share_epoch;
    let ix = build(
        shortwin::accounts::SolSettle {
            keeper: *keeper,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            borrower: *borrower,
            exposure: pda::exposure(&market, borrower).0,
            market_vault: mm.keys().market_vault,
            mint: mm.mint(),
            share_epoch_rec: pda::share_epoch(&market, epoch).0,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SolSettle {},
    );
    ix::with_remaining(ix, &mm.read_set(false))
}

/// `ATA(trader, WSOL)` of a market (SPL Token): the account `sweep_trader` closes.
pub fn trader_wsol_ata(market: &Pubkey) -> Pubkey {
    let trader = pda::trader(market).0;
    Pubkey::find_program_address(
        &[
            trader.as_ref(),
            TOKEN_PROGRAM_ID.as_ref(),
            WSOL_MINT.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// `sweep_trader()` (ARCH §3.9; M2-SEC-4) of `market`; `wipe_record`: the latest `ShareEpoch`
/// record (writable), which the instruction takes exactly while a full-wipe record is live.
pub fn sweep_trader_ix(market: &Pubkey, wipe_record: Option<Pubkey>) -> Instruction {
    let ix = build(
        shortwin::accounts::SweepTrader {
            config: pda::config(),
            market: *market,
            market_vault: pda::market_vault(market).0,
            trader: pda::trader(market).0,
            trader_wsol: pda::trader_wsol(market).0,
            trader_wsol_ata: trader_wsol_ata(market),
            token_program: TOKEN_PROGRAM_ID,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SweepTrader {},
    );
    let extra: Vec<AccountMeta> = wipe_record
        .map(|k| AccountMeta::new(k, false))
        .into_iter()
        .collect();
    ix::with_remaining(ix, &extra)
}

/// [`sweep_trader_ix`] with the remaining accounts the market's state asks for: off the pump
/// venues, the trader's pump UVA when the market has one (AIR-05: a curve market that left pump;
/// the pump venues' token legs are `pump::sweep_trader_pump_ix` / `pamm::sweep_trader_pamm_ix`);
/// then the latest `ShareEpoch` record while a full-wipe record is live.
pub fn sweep_trader_ix_for(env: &Env, market: &Pubkey) -> Instruction {
    use shortwin::state::VenueKind;
    let m = env.market(market);
    let live = m.total_shares.get() == 0 && m.has_flag(shortwin::state::MARKET_FLAG_WIPE_LIVE);
    let rec = live.then(|| pda::share_epoch(market, m.share_epoch.saturating_sub(1)).0);
    let pump = matches!(
        m.venue_kind(),
        Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
    );
    let mut ix = sweep_trader_ix(market, None);
    if !pump && m.pump_uva != Pubkey::default() {
        ix.accounts
            .push(AccountMeta::new_readonly(m.pump_uva, false));
    }
    ix.accounts.extend(rec.map(|k| AccountMeta::new(k, false)));
    ix
}

/// `ix` preceded by `SetComputeUnitLimit(mm.cu_limit())` when the venue asks for one
/// ([`VenueMarket::cu_limit`]; the hooks check the last instruction of a transaction).
pub fn with_cu_limit(mm: &impl VenueMarket, ix: Instruction) -> Vec<Instruction> {
    match mm.cu_limit() {
        Some(units) => vec![crate::pump::cu_limit_ix(units), ix],
        None => vec![ix],
    }
}

// ---- borrowers ----------------------------------------------------------------------------

/// A funded borrower and the nonce of its next position.
pub struct Borrower {
    pub kp: Keypair,
    pub next_nonce: u64,
}

impl Borrower {
    /// The borrower's key.
    pub fn key(&self) -> Pubkey {
        self.kp.pubkey()
    }
}

impl Env {
    /// A new borrower with [`crate::env::FUNDING`] lamports.
    pub fn borrower(&mut self) -> Borrower {
        Borrower {
            kp: self.funded_keypair(),
            next_nonce: 0,
        }
    }

    /// `open_short` of `q` with no bounds of its own (min proceeds 0, max margin and fee
    /// `u64::MAX`), extras `none`. Returns the result and the nonce used (the nonce advances
    /// only on success).
    pub fn open(&mut self, mm: &impl VenueMarket, b: &mut Borrower, q: u64) -> (TxResult, u64) {
        let nonce = b.next_nonce;
        let ix = open_short_ix(
            mm,
            &b.key(),
            nonce,
            q,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        );
        let kp = b.kp.insecure_clone();
        let res = self.send(&with_cu_limit(mm, ix), &[&kp]);
        if res.is_ok() {
            b.next_nonce += 1;
        }
        (res, nonce)
    }

    /// `close_short` of `b`'s position `nonce` with `max_buyback_cost = u64::MAX`, extras
    /// `none`.
    pub fn close(&mut self, mm: &impl VenueMarket, b: &Borrower, nonce: u64) -> TxResult {
        let ix = close_short_ix(mm, &b.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
        let kp = b.kp.insecure_clone();
        self.send(&with_cu_limit(mm, ix), &[&kp])
    }

    /// A position (panics if missing).
    pub fn position(&self, mm: &impl VenueMarket, borrower: &Pubkey, nonce: u64) -> ShortPosition {
        read::borsh(&self.svm, &pda::position(&mm.market(), borrower, nonce).0)
            .unwrap_or_else(|| panic!("no position {borrower}/{nonce}"))
    }
}

// ---- the zero-lamport borrower (M10 G-H10) ----------------------------------------------------

/// The System transfer of every lamport `from` holds now to a fresh address: the wallet is reaped
/// (the zero-lamport borrower, ARCH §8.2 venue group; A18).
pub fn drain_ix(env: &Env, from: &Pubkey) -> Instruction {
    let all = read::lamports(&env.svm, from);
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&all.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            AccountMeta::new(*from, true),
            AccountMeta::new(Pubkey::new_unique(), false),
        ],
        data,
    }
}

/// The zero-lamport borrower's exits on any venue (M10 G-H10), in both meanings: **(A)** the wallet
/// emptied right after the open (its own transaction) and kept at 0, **(B)** emptied inside the
/// exit's own transaction just before the exit. For each meaning, on a fresh `make()` market with
/// size `q`: one borrower opens and then closes (`close_short`, it signs; the harness payer pays
/// the fee), another opens and is expired by a keeper after its deadline (`expire`). Every exit
/// lands, and the borrower's account ends holding at least the position's rent plus the exposure
/// record's (A18: the borrower is paid through the position's close, so the credit is never below
/// the rent-exempt minimum). `second` advances the market one honest second (a warp and a crank).
/// Every instruction goes through [`Env::send`]: the hooks (I3, I9, I12) run on each one.
pub fn zero_lamport_exits<M: VenueMarket>(mut make: impl FnMut() -> (Env, M, u64), label: &str) {
    const EXIT_CU: u32 = 400_000;
    for meaning in ['A', 'B'] {
        let (mut env, mm, q) = make();
        let what = format!("{label} ({meaning})");
        let mut b1 = env.borrower();
        let (res, n1) = env.open(&mm, &mut b1, q);
        errors::ok(res);
        let mut b2 = env.borrower();
        let (res, n2) = env.open(&mm, &mut b2, q);
        errors::ok(res);
        if meaning == 'A' {
            for b in [&b1, &b2] {
                let ix = drain_ix(&env, &b.key());
                errors::ok(env.send(&[ix], &[&b.kp]));
                assert_eq!(read::lamports(&env.svm, &b.key()), 0, "{what}: reaped");
            }
        }
        let records = |env: &Env, b: &Pubkey, n: u64| {
            read::lamports(&env.svm, &pda::position(&mm.market(), b, n).0).min(
                env.svm
                    .minimum_balance_for_rent_exemption(ShortPosition::SPACE),
            ) + read::lamports(&env.svm, &pda::exposure(&mm.market(), b).0)
        };
        let min0 = env.svm.minimum_balance_for_rent_exemption(0);

        // The borrower's own close.
        env.warp(1);
        env.crank(&mm);
        let owed = records(&env, &b1.key(), n1);
        let close = close_short_ix(&mm, &b1.key(), n1, u64::MAX, ExtrasSel::NONE, &[]);
        let mut ixs = vec![crate::pump::cu_limit_ix(EXIT_CU)];
        if meaning == 'B' {
            ixs.push(drain_ix(&env, &b1.key()));
        }
        ixs.push(close);
        let kp = b1.kp.insecure_clone();
        errors::ok(env.send(&ixs, &[&kp]));
        let got = read::lamports(&env.svm, &b1.key());
        assert!(
            got >= owed && got >= min0,
            "{what}: close credits {got} ≥ {owed}"
        );

        // The keeper's expiry.
        let deadline = env.position(&mm, &b2.key(), n2).deadline;
        let now = env.now();
        env.warp(deadline - now + 1);
        env.crank(&mm);
        for _ in 0..3 {
            env.warp(1);
            env.crank(&mm);
        }
        let owed = records(&env, &b2.key(), n2);
        let k = env.funded_keypair();
        let exp = expire_ix(&mm, &k.pubkey(), &b2.key(), n2);
        let mut ixs = vec![crate::pump::cu_limit_ix(EXIT_CU)];
        if meaning == 'B' {
            ixs.push(drain_ix(&env, &b2.key()));
        }
        ixs.push(exp);
        let kp = b2.kp.insecure_clone();
        let signers: Vec<&Keypair> = if meaning == 'B' {
            vec![&kp, &k]
        } else {
            vec![&k]
        };
        errors::ok(env.send(&ixs, &signers));
        let got = read::lamports(&env.svm, &b2.key());
        assert!(
            got >= owed && got >= min0,
            "{what}: expiry credits {got} ≥ {owed}"
        );
    }
}

// ---- the extras denylist: the adopted pool and the borrower (M10 batch 3) ----------------------

/// `ix` with its trailing `ExtrasSel` argument replaced by `sel` and `extras` appended (the
/// keeper exits' builders select `none`).
pub fn with_extras(mut ix: Instruction, sel: ExtrasSel, extras: &[AccountMeta]) -> Instruction {
    let n = ix.data.len();
    ix.data.truncate(n - 5);
    ix.data.push(sel.set);
    ix.data.extend_from_slice(&sel.bumps);
    ix.accounts.extend_from_slice(extras);
    ix
}

/// A fixed read-only extras entry.
pub fn fixed_extra(key: Pubkey) -> ExtraEntryArg {
    ExtraEntryArg {
        kind: shortwin::state::ENTRY_FIXED,
        writable: false,
        key,
        seeds: Vec::new(),
    }
}

/// The adopted pool's own accounts the extras denylist keeps out (ARCH §2.4 rule 4; M10 batch 3,
/// the program's `venues::pool_denylist`), in the harness's own view: the slots of `mm`'s ReadSet
/// (and the curve's TradeExtras) that name the pool and its two reserve accounts — the mock pool
/// and its vaults, the AMM v4 pool and its coin / pc vaults, the CPMM pool and its token 0 / 1
/// vaults, the canonical PumpSwap pool and its base / quote vaults, the bonding curve (its SOL
/// reserve), its base ATA and its quote ATA.
pub fn pool_denied_keys(kind: VenueKind, mm: &impl VenueMarket) -> Vec<(String, Pubkey)> {
    use shortwin::venues::{mock as vm, pump_amm, pump_curve, raydium_cpmm, raydium_v4};
    let read = mm.read_set(false);
    let r = |what: &str, i: usize| (what.to_string(), read[i].pubkey);
    match kind {
        VenueKind::Mock => vec![
            r("the mock pool", vm::R_POOL),
            r("its base vault", vm::R_BASE_VAULT),
            r("its quote vault", vm::R_QUOTE_VAULT),
        ],
        VenueKind::RaydiumV4 => vec![
            r("the AMM v4 pool", raydium_v4::R_POOL),
            r("its coin vault", raydium_v4::R_COIN_VAULT),
            r("its pc vault", raydium_v4::R_PC_VAULT),
        ],
        VenueKind::RaydiumCpmm => vec![
            r("the CPMM pool", raydium_cpmm::R_POOL),
            r("its token 0 vault", raydium_cpmm::R_TOKEN_0_VAULT),
            r("its token 1 vault", raydium_cpmm::R_TOKEN_1_VAULT),
        ],
        VenueKind::PumpSwap => vec![
            r("the PumpSwap pool", pump_amm::P_POOL),
            r("its base vault", pump_amm::P_BASE_VAULT),
            r("its quote vault", pump_amm::P_QUOTE_VAULT),
        ],
        VenueKind::PumpCurve => vec![
            r("the bonding curve", pump_curve::C_CURVE),
            r("its base ATA", pump_curve::C_CURVE_BASE_ATA),
            (
                "its quote ATA".to_string(),
                mm.trade_extras()[pump_curve::T_CURVE_QUOTE_ATA].pubkey,
            ),
        ],
        other => panic!("no trading venue: {other:?}"),
    }
}

/// M10 batch 3 (ARCH §2.4 rule 4; the owner's decision of 2026-10-03) on a fork venue, on a fresh
/// `make()` market with size `q`: each of the adopted pool's own accounts ([`pool_denied_keys`])
/// published by the Emergency key as a one-entry fixed `current` set — writable exactly as the
/// instruction names it, so the parser's writable-flag check passes and only the denylist can
/// refuse it (on the batch-2 ELF these selections passed the parser) — and selected is
/// `DeniedAccount` on `open_short` (the sell set), `close_short`, `liquidate` and `expire` (the
/// buy set), and on the keeper exits so is the position's borrower (not a signer there; on the
/// borrower's own instructions it signs, which the parser refuses already) — the position
/// unchanged by every refusal. The control on each site is the same set shape with a fresh key,
/// which the venue ignores as a trailing account: the open and the close land, the healthy
/// position's liquidation reaches its body (`NotLiquidatable`), the expiry after the deadline
/// lands. Every instruction goes through [`Env::send`]: the hooks run on each one.
pub fn pool_and_borrower_denied_extras<M: VenueMarket>(
    make: impl FnOnce() -> (Env, M, u64),
    label: &str,
) {
    use shortwin::state::{EXTRAS_IX_BUY, EXTRAS_IX_SELL};
    use shortwin::venues::EXTRAS_SET_CURRENT;
    const EXIT_CU: u32 = 400_000;
    let (mut env, mm, q) = make();
    let kind = env.market(&mm.market()).venue_kind().expect("a venue kind");
    let venue = kind as u8;
    let pool = pool_denied_keys(kind, &mm);
    let ve = AccountMeta::new_readonly(pda::venue_extras_seed(venue), false);
    let current = ExtrasSel {
        set: EXTRAS_SET_CURRENT,
        bumps: [0; 4],
    };
    let ea = env.pause_authority.insecure_clone();
    // A fixed entry, writable exactly when the instruction already names the key writable: the
    // parser's flag check then passes, so only the denylist can refuse it whatever the order of
    // the two checks (as built `parse_extras` checks the denylist first, so since batch 3 an entry
    // with the other flag is `DeniedAccount` too; before batch 3 it was `BadExtras`).
    let publish = |env: &mut Env, ix_kind: usize, key: Pubkey, writable: bool| {
        let mut entry = fixed_extra(key);
        entry.writable = writable;
        let ix = set_venue_extras_ix(&ea.pubkey(), venue, ix_kind as u8, vec![entry]);
        errors::ok(env.send(&[ix], &[&ea]));
        [
            ve.clone(),
            AccountMeta {
                pubkey: key,
                is_signer: false,
                is_writable: writable,
            },
        ]
    };
    let send = |env: &mut Env, ix: Instruction, signers: &[&Keypair]| {
        env.send(&[crate::pump::cu_limit_ix(EXIT_CU), ix], signers)
    };
    let denied = |env: &mut Env,
                  site: &str,
                  ix_kind: usize,
                  keys: &[(String, Pubkey)],
                  build: &dyn Fn(&[AccountMeta]) -> Instruction,
                  signers: &[&Keypair],
                  watch: Option<Pubkey>| {
        let before = watch.and_then(|p| env.svm.get_account(&p).map(|a| a.data));
        let named = build(&[]).accounts;
        for (what, key) in keys {
            let writable = named.iter().any(|m| m.pubkey == *key && m.is_writable);
            let extras = publish(env, ix_kind, *key, writable);
            let res = send(env, build(&extras), signers);
            assert_eq!(
                errors::custom_code(&res),
                Some(u32::from(shortwin::errors::ShortwinError::DeniedAccount)),
                "{label}: {site}: {what} ({key}) as an extra"
            );
        }
        if let Some(p) = watch {
            assert_eq!(
                env.svm.get_account(&p).map(|a| a.data),
                before,
                "{label}: {site}: the position unchanged"
            );
        }
    };

    // open_short (the sell set): the pool's accounts; the control opens position 0.
    let b = env.borrower();
    let bk = b.kp.insecure_clone();
    let open = |x: &[AccountMeta]| {
        open_short_ix(&mm, &bk.pubkey(), 0, q, 0, u64::MAX, u64::MAX, current, x)
    };
    denied(
        &mut env,
        "open_short",
        EXTRAS_IX_SELL,
        &pool,
        &open,
        &[&bk],
        None,
    );
    let x = publish(&mut env, EXTRAS_IX_SELL, Pubkey::new_unique(), false);
    let _: shortwin::events::Open =
        crate::events::one(&errors::ok(send(&mut env, open(&x), &[&bk])));
    // The keeper exits' position (another wallet: the wallet cap).
    let mut b2 = env.borrower();
    let (res, n2) = env.open(&mm, &mut b2, q);
    errors::ok(res);
    env.warp(1);
    env.crank(&mm);

    // close_short (the buy set) of position 0: the pool's accounts and the borrower (it signs).
    let pos0 = pda::position(&mm.market(), &b.key(), 0).0;
    let mut keys = pool.clone();
    keys.push(("the borrower (signer)".into(), b.key()));
    let close = |x: &[AccountMeta]| close_short_ix(&mm, &bk.pubkey(), 0, u64::MAX, current, x);
    denied(
        &mut env,
        "close_short",
        EXTRAS_IX_BUY,
        &keys,
        &close,
        &[&bk],
        Some(pos0),
    );

    // liquidate (the buy set) of the healthy position: the pool's accounts and its borrower.
    let k = env.funded_keypair();
    let pos2 = pda::position(&mm.market(), &b2.key(), n2).0;
    let mut keys = pool.clone();
    keys.push(("the borrower (not a signer)".into(), b2.key()));
    let (kk, b2k) = (k.pubkey(), b2.key());
    let liq = |x: &[AccountMeta]| with_extras(liquidate_ix(&mm, &kk, &b2k, n2), current, x);
    denied(
        &mut env,
        "liquidate",
        EXTRAS_IX_BUY,
        &keys,
        &liq,
        &[&k],
        Some(pos2),
    );
    let x = publish(&mut env, EXTRAS_IX_BUY, Pubkey::new_unique(), false);
    errors::assert_err(
        &send(&mut env, liq(&x), &[&k]),
        shortwin::errors::ShortwinError::NotLiquidatable,
    );
    // The close's control on the same harmless set.
    let _: shortwin::events::Close =
        crate::events::one(&errors::ok(send(&mut env, close(&x), &[&bk])));

    // expire (the buy set) after the deadline (the reference needs a few honest seconds).
    let deadline = env.position(&mm, &b2.key(), n2).deadline;
    let now = env.now();
    env.warp(deadline - now + 1);
    env.crank(&mm);
    for _ in 0..3 {
        env.warp(1);
        env.crank(&mm);
    }
    let exp = |x: &[AccountMeta]| with_extras(expire_ix(&mm, &kk, &b2k, n2), current, x);
    denied(
        &mut env,
        "expire",
        EXTRAS_IX_BUY,
        &keys,
        &exp,
        &[&k],
        Some(pos2),
    );
    let x = publish(&mut env, EXTRAS_IX_BUY, Pubkey::new_unique(), false);
    let _: shortwin::events::Expire =
        crate::events::one(&errors::ok(send(&mut env, exp(&x), &[&k])));
}

/// A constant seed part of a template entry (`bytes` ≤ 32).
pub fn seed_const(bytes: &[u8]) -> shortwin::SeedPartArg {
    let mut b = [0u8; 32];
    b[..bytes.len()].copy_from_slice(bytes);
    shortwin::SeedPartArg {
        kind: shortwin::state::SEED_CONST,
        len: bytes.len() as u8,
        bytes: b,
    }
}

/// A placeholder seed part of a template entry (`PLACEHOLDER_*`).
pub fn seed_placeholder(id: u8) -> shortwin::SeedPartArg {
    shortwin::SeedPartArg {
        kind: shortwin::state::SEED_PLACEHOLDER,
        len: id,
        bytes: [0; 32],
    }
}

/// One template entry that derives one of the adopted pool's own accounts from the placeholders
/// (for [`template_aliases_denied`]): a label, the derivation program, the seed parts and the
/// seeds they resolve to on the market under test.
pub struct PoolAlias {
    pub what: String,
    pub program: Pubkey,
    pub seeds: Vec<shortwin::SeedPartArg>,
    pub resolved: Vec<Vec<u8>>,
}

/// M10 batch 3 (the security review): a template entry is re-derived on chain before the
/// denylist runs, so an Emergency template that derives one of the adopted pool's own accounts
/// from the `pool` / `mint` / `bonding_curve` placeholders (an ATA of the pool, a CPMM
/// `pool_vault` PDA) is refused like the fixed key. Each alias of `aliases` must resolve to one
/// of [`pool_denied_keys`]; published as the `current` sell set (writable as `open_short` names the
/// key, the canonical bump) it is `DeniedAccount` on `open_short`, and so it stays once a harmless
/// write moves it to `previous` and `previous` is selected.
pub fn template_aliases_denied<M: VenueMarket>(
    make: impl FnOnce() -> (Env, M, u64),
    label: &str,
    aliases: impl FnOnce(&Env, &M) -> Vec<PoolAlias>,
) {
    use shortwin::state::{ENTRY_TEMPLATE, EXTRAS_IX_SELL};
    use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS};
    let (mut env, mm, q) = make();
    let kind = env.market(&mm.market()).venue_kind().expect("a venue kind");
    let venue = kind as u8;
    let denied: Vec<Pubkey> = pool_denied_keys(kind, &mm)
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    let ea = env.pause_authority.insecure_clone();
    let ve = AccountMeta::new_readonly(pda::venue_extras_seed(venue), false);
    let b = env.borrower();
    let bk = b.kp.insecure_clone();
    let open = |sel: ExtrasSel, x: &[AccountMeta]| {
        open_short_ix(&mm, &bk.pubkey(), 0, q, 0, u64::MAX, u64::MAX, sel, x)
    };
    let named = open(ExtrasSel::NONE, &[]).accounts;
    let list = aliases(&env, &mm);
    assert!(!list.is_empty(), "{label}: no alias");
    for a in list {
        let seeds: Vec<&[u8]> = a.resolved.iter().map(Vec::as_slice).collect();
        let (key, bump) = Pubkey::find_program_address(&seeds, &a.program);
        assert!(
            denied.contains(&key),
            "{label}: {}: the template must derive a pool account ({key})",
            a.what
        );
        let writable = named.iter().any(|m| m.pubkey == key && m.is_writable);
        let entry = ExtraEntryArg {
            kind: ENTRY_TEMPLATE,
            writable,
            key: a.program,
            seeds: a.seeds,
        };
        let ix = set_venue_extras_ix(&ea.pubkey(), venue, EXTRAS_IX_SELL as u8, vec![entry]);
        errors::ok(env.send(&[ix], &[&ea]));
        let extras = [
            ve.clone(),
            AccountMeta {
                pubkey: key,
                is_signer: false,
                is_writable: writable,
            },
        ];
        for (set, which) in [
            (EXTRAS_SET_CURRENT, "current"),
            (EXTRAS_SET_PREVIOUS, "previous"),
        ] {
            if set == EXTRAS_SET_PREVIOUS {
                // A harmless write moves the alias to `previous`.
                let ix = set_venue_extras_ix(
                    &ea.pubkey(),
                    venue,
                    EXTRAS_IX_SELL as u8,
                    vec![fixed_extra(Pubkey::new_unique())],
                );
                errors::ok(env.send(&[ix], &[&ea]));
            }
            let sel = ExtrasSel {
                set,
                bumps: [bump, 0, 0, 0],
            };
            let res = env.send(
                &[crate::pump::cu_limit_ix(400_000), open(sel, &extras)],
                &[&bk],
            );
            assert_eq!(
                errors::custom_code(&res),
                Some(u32::from(shortwin::errors::ShortwinError::DeniedAccount)),
                "{label}: {} through a template ({which})",
                a.what
            );
        }
        env.warp(1);
    }
}
