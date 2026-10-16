//! Raydium markets on the mainnet fork (M3; ARCH §2.6, §3.2, §3.12, §8.2): the release short.win
//! trading against the **dumped** AMM v4 and CPMM programs and pools of [`crate::fork`].
//!
//! - [`Env::ray_fork`]: [`Env::initialized`] (release build, every MVP venue enabled, the
//!   dumped deploy slots in `VenueGuard`), both Raydium programs deployed from their dumped
//!   ProgramData, the clock at the fixtures' snapshot. [`Env::ray_market`] loads a pool, lists
//!   it with `create_market(venue_hint = RaydiumV4 | RaydiumCpmm)` (real instruction), seeds a
//!   lender and waits out the listing window ([`RaySpec`]).
//! - [`RayMarket`]: the market's keys and its venue's account lists, built from the **fixture's**
//!   pool keys (not from the program's own key functions), in ARCH §3.12's order; it implements
//!   [`VenueMarket`], so every builder of [`crate::engine`] (`open_short_ix`, `close_short_ix`,
//!   `crank_price_ix`, `liquidate_ix`, `expire_ix`, `sol_settle_ix`, …) and
//!   [`Env::open`] / [`Env::close`] / [`Env::tick`] work on it.
//! - [`candidate_read_set`] / [`create_ray_market_ix`]: the listing instruction for any dumped
//!   pool, for the refusal tests.
//! - Pump detection fixtures (D1 precedence at a Raydium listing): [`FARTCOIN_CURVE_HEX`] (the
//!   mainnet curve of a coin that graduated to Raydium) and [`plant_curve`] /
//!   [`plant_canonical_pool`] builders.
//!
//! Every instruction goes through [`Env::send`] (the invariant hooks, which see Raydium markets
//! through `invariants::venue_view`).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use shortwin::constants::{
    PUMP_AMM_PROGRAM_ID, PUMP_PROGRAM_ID, RAYDIUM_CPMM_AUTHORITY, RAYDIUM_CPMM_PROGRAM_ID,
    RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};
pub use shortwin::instructions::create_market::{VENUE_HINT_RAYDIUM_CPMM, VENUE_HINT_RAYDIUM_V4};
use shortwin::state::VenueKind;
use shortwin_math::fees::Direction;
use shortwin_math::quote::{BuyQuote, SellQuote};
use shortwin_venue_state::{pump, pump_amm};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::engine::VenueMarket;
use crate::fixtures::MarketKeys;
use crate::fork::{self, AnyPool, CpmmFixture, CpmmPool, V4Fixture, V4Pool};
use crate::ix::{self, LenderKeys};
use crate::tokens::TokenProgram;
use crate::{Env, TxResult, errors, pda};

/// A dumped Raydium pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RayFixture {
    V4(V4Fixture),
    Cpmm(CpmmFixture),
}

impl RayFixture {
    /// The venue kind a market on it has.
    pub fn kind(self) -> VenueKind {
        match self {
            Self::V4(_) => VenueKind::RaydiumV4,
            Self::Cpmm(_) => VenueKind::RaydiumCpmm,
        }
    }

    /// `create_market`'s hint for it.
    pub fn hint(self) -> u8 {
        match self {
            Self::V4(_) => VENUE_HINT_RAYDIUM_V4,
            Self::Cpmm(_) => VENUE_HINT_RAYDIUM_CPMM,
        }
    }
}

/// The pool's token (non-WSOL) side program.
pub fn token_program_of(pool: &AnyPool) -> TokenProgram {
    pool.token().1
}

fn m(key: Pubkey, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(key, false)
    } else {
        AccountMeta::new_readonly(key, false)
    }
}

/// The config a CPMM pool names now (its live `amm_config`), or the one it named when loaded if
/// the pool account no longer parses (a test that broke it on purpose).
pub fn cpmm_config_key(svm: &LiteSVM, p: &CpmmPool) -> Pubkey {
    svm.get_account(&p.pool)
        .and_then(|a| {
            shortwin_venue_state::raydium_cpmm::read_pool(&a.owner.to_bytes(), &a.data).ok()
        })
        .map_or(p.amm_config, |s| Pubkey::new_from_array(s.amm_config))
}

/// The ReadSet of `pool` in the adapter's order (ARCH §3.12), from the fixture's keys: AMM v4
/// `[pool, coin_vault, pc_vault, lp_mint]`, CPMM `[pool, amm_config, token_0_vault,
/// token_1_vault, lp_mint]` (the config the pool names now). Pool and vaults are writable when
/// `writable` (trades).
pub fn candidate_read_set(svm: &LiteSVM, pool: &AnyPool, writable: bool) -> Vec<AccountMeta> {
    match pool {
        AnyPool::V4(p) => vec![
            m(p.pool, writable),
            m(p.coin_vault, writable),
            m(p.pc_vault, writable),
            m(p.lp_mint, false),
        ],
        AnyPool::Cpmm(p) => vec![
            m(p.pool, writable),
            m(cpmm_config_key(svm, p), false),
            m(p.token_0_vault, writable),
            m(p.token_1_vault, writable),
            m(p.lp_mint, false),
        ],
    }
}

/// The TradeExtras of a market on `pool` whose `trader_wsol` is `trader_wsol` (ARCH §3.12):
/// AMM v4 `[program, authority, Tokenkeg, WSOL, System, trader_wsol W]`, CPMM `[program,
/// authority, observation W, Tokenkeg, the token side's program, WSOL, System, trader_wsol W]`.
pub fn trade_extras_of(pool: &AnyPool, trader_wsol: &Pubkey) -> Vec<AccountMeta> {
    let system = anchor_lang::system_program::ID;
    match pool {
        AnyPool::V4(_) => vec![
            m(RAYDIUM_V4_PROGRAM_ID, false),
            m(RAYDIUM_V4_AUTHORITY, false),
            m(TOKEN_PROGRAM_ID, false),
            m(WSOL_MINT, false),
            m(system, false),
            m(*trader_wsol, true),
        ],
        AnyPool::Cpmm(p) => vec![
            m(RAYDIUM_CPMM_PROGRAM_ID, false),
            m(RAYDIUM_CPMM_AUTHORITY, false),
            m(p.observation, true),
            m(TOKEN_PROGRAM_ID, false),
            m(p.token_program().id(), false),
            m(WSOL_MINT, false),
            m(system, false),
            m(*trader_wsol, true),
        ],
    }
}

/// `create_market(hint, u64::MAX)` for `pool`'s token mint: the detection set, then the
/// candidate ReadSet (read-only).
pub fn create_ray_market_ix(
    svm: &LiteSVM,
    creator: &Pubkey,
    pool: &AnyPool,
    hint: u8,
) -> Instruction {
    let (mint, program) = pool.token();
    let ix = ix::create_market(
        creator,
        &mint,
        &program.id(),
        hint,
        &pda::canonical_pool(&mint),
    );
    ix::with_remaining(ix, &candidate_read_set(svm, pool, false))
}

/// A Raydium market on the fork: the pool, the market's keys, its lender.
pub struct RayMarket {
    pub fixture: RayFixture,
    pub pool: AnyPool,
    pub keys: MarketKeys,
    pub trader_wsol: Pubkey,
    /// The lender that seeded the market (and its token account).
    pub lender: Keypair,
    pub lender_account: Pubkey,
    /// The CPMM config the pool named at listing (the market caches it).
    pub amm_config: Option<Pubkey>,
}

impl RayMarket {
    /// The venue kind.
    pub fn kind(&self) -> VenueKind {
        self.fixture.kind()
    }

    /// The token side's program.
    pub fn program(&self) -> TokenProgram {
        token_program_of(&self.pool)
    }

    /// The AMM v4 pool (panics on CPMM).
    pub fn v4(&self) -> &V4Pool {
        match &self.pool {
            AnyPool::V4(p) => p,
            AnyPool::Cpmm(_) => panic!("not an AMM v4 market"),
        }
    }

    /// The CPMM pool (panics on AMM v4).
    pub fn cpmm(&self) -> &CpmmPool {
        match &self.pool {
            AnyPool::Cpmm(p) => p,
            AnyPool::V4(_) => panic!("not a CPMM market"),
        }
    }

    /// The pool's `(token vault, WSOL vault)`.
    pub fn vaults(&self) -> (Pubkey, Pubkey) {
        match &self.pool {
            AnyPool::V4(p) => {
                if p.sol_is_coin {
                    (p.pc_vault, p.coin_vault)
                } else {
                    (p.coin_vault, p.pc_vault)
                }
            }
            AnyPool::Cpmm(p) => {
                if p.sol_is_token0 {
                    (p.token_1_vault, p.token_0_vault)
                } else {
                    (p.token_0_vault, p.token_1_vault)
                }
            }
        }
    }

    /// `S_exp(q)`: the exact-in sale of `q` tokens on the live state (`quote::ray_v4` /
    /// `quote::cpmm` on the readers' reserves).
    pub fn quote_sell(&self, svm: &LiteSVM, q: u64) -> Option<SellQuote> {
        self.pool.quote_exact_in(svm, Direction::Sell, q)
    }

    /// `B̂_live(q)`: the exact-out buy of `q` tokens on the live state.
    pub fn quote_buy(&self, svm: &LiteSVM, q: u64) -> Option<BuyQuote> {
        self.pool.quote_exact_out(svm, Direction::Buy, q)
    }

    /// The pricing reserves `(X, Y)` (token, SOL) on the live state.
    pub fn reserves(&self, svm: &LiteSVM) -> Option<(u64, u64)> {
        self.pool.io_reserves(svm, Direction::Sell)
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

impl VenueMarket for RayMarket {
    fn keys(&self) -> &MarketKeys {
        &self.keys
    }

    fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        match &self.pool {
            AnyPool::V4(p) => vec![
                m(p.pool, writable),
                m(p.coin_vault, writable),
                m(p.pc_vault, writable),
                m(p.lp_mint, false),
            ],
            AnyPool::Cpmm(p) => vec![
                m(p.pool, writable),
                m(self.amm_config.unwrap_or(p.amm_config), false),
                m(p.token_0_vault, writable),
                m(p.token_1_vault, writable),
                m(p.lp_mint, false),
            ],
        }
    }

    fn trade_extras(&self) -> Vec<AccountMeta> {
        trade_extras_of(&self.pool, &self.trader_wsol)
    }
}

/// A Raydium market's parameters.
#[derive(Clone, Copy, Debug)]
pub struct RaySpec {
    pub fixture: RayFixture,
    /// Tokens the lender deposits (planted in its account first: the mainnet mints have no mint
    /// authority).
    pub lend: u64,
    /// Seconds to warp after listing (default: `pool_list_window_s` + 1 = 3,601, which also
    /// covers the 50 s warm-up). A warp of at least the listing window ends with the post-change
    /// warm-up of M4R / M4V-5 ([`PRICE_WARMUP_CRANKS`] cranks one second apart, as the keeper
    /// observes a market at the end of its listing window); a shorter one warps only.
    pub warmup: i64,
}

/// Cranks one second apart that finish the post-change warm-up of a freshly listed or adopted
/// pool (M4R / M4V-5: `EMA_WARMUP_BUCKETS` integrations, `ema_warm == 0`): after them the EMA has
/// integrated `EMA_WARMUP_BUCKETS + 1` buckets.
pub const PRICE_WARMUP_CRANKS: i64 = shortwin_math::consts::EMA_WARMUP_BUCKETS as i64 + 2;

impl RaySpec {
    /// `fixture` with `lend` tokens lent and the default warm-up.
    pub fn new(fixture: RayFixture, lend: u64) -> Self {
        Self {
            fixture,
            lend,
            warmup: 3_601,
        }
    }
}

impl Env {
    /// The release short.win, initialized, with both Raydium programs and the fork's clock
    /// (module docs). No pool is loaded yet.
    pub fn ray_fork() -> Self {
        let mut env = Self::initialized();
        env.load_raydium_programs();
        env.set_fork_clock();
        env
    }

    /// Loads the dumped pool `fixture` (and every account its swaps need).
    pub fn load_ray_pool(&mut self, fixture: RayFixture) -> AnyPool {
        match fixture {
            RayFixture::V4(f) => AnyPool::V4(self.load_ray_v4(f)),
            RayFixture::Cpmm(f) => AnyPool::Cpmm(self.load_cpmm(f)),
        }
    }

    /// Lists `pool` with `create_market(hint)` as a new funded creator.
    pub fn list_ray(&mut self, pool: &AnyPool, hint: u8) -> TxResult {
        let creator = self.funded_keypair();
        let ix = create_ray_market_ix(&self.svm, &creator.pubkey(), pool, hint);
        self.send(&[ix], &[&creator])
    }

    /// A Raydium market per `spec` (module docs), through real instructions.
    pub fn ray_market(&mut self, spec: RaySpec) -> RayMarket {
        let pool = self.load_ray_pool(spec.fixture);
        errors::ok(self.list_ray(&pool, spec.fixture.hint()));
        self.ray_market_listed(spec, pool)
    }

    /// The [`RayMarket`] of an already listed `pool`: seeds the lender and warps (`spec`).
    pub fn ray_market_listed(&mut self, spec: RaySpec, pool: AnyPool) -> RayMarket {
        let (mint, program) = pool.token();
        let market = pda::market(&mint).0;
        let keys = MarketKeys {
            mint,
            token_program: program.id(),
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            trader: pda::trader(&market).0,
        };
        let lender = self.funded_keypair();
        let lender_account = fork::fund_token(self, &lender.pubkey(), &mint, program, spec.lend);
        let amm_config = match &pool {
            AnyPool::Cpmm(p) => Some(cpmm_config_key(&self.svm, p)),
            AnyPool::V4(_) => None,
        };
        let rm = RayMarket {
            fixture: spec.fixture,
            pool,
            keys,
            trader_wsol: pda::trader_wsol(&market).0,
            lender,
            lender_account,
            amm_config,
        };
        if spec.lend > 0 {
            let lender = rm.lender.insecure_clone();
            errors::ok(self.send(&[ix::deposit(&rm.lender_keys(), spec.lend, 0)], &[&lender]));
        }
        let window = i64::from(self.config().params.pool_list_window_s);
        if spec.warmup >= window && spec.warmup > PRICE_WARMUP_CRANKS {
            self.warp(spec.warmup - PRICE_WARMUP_CRANKS);
            self.warm_up_price(&rm);
        } else {
            self.warp(spec.warmup);
        }
        rm
    }

    /// [`PRICE_WARMUP_CRANKS`] cranks of `rm`, one second apart (the first after one second):
    /// the observations the keeper sends at the end of a listing window (M4V-5).
    pub fn warm_up_price(&mut self, rm: &RayMarket) {
        for _ in 0..PRICE_WARMUP_CRANKS {
            self.warp(1);
            self.crank(rm);
        }
    }
}

// ---- pump detection fixtures (D1 precedence) --------------------------------------------------

/// Fartcoin's bonding curve `TBHe5tJnuT4CQbHorJ1uVdfUoaYGPKgfCpiv2jgesVN` exactly as mainnet
/// holds it (`getAccountInfo` at slot 451,197,110: 150 bytes, owner pump, 3,934,883 lamports):
/// complete, every reserve 0, supply 10¹⁵ — a coin that graduated to Raydium before PumpSwap
/// (ARCH §2.5 "GraduatedElsewhere", ARCH `[V3]`).
pub const FARTCOIN_CURVE_HEX: &str = "17b7f83760d8ac6000000000000000000000000000000000000000000000000000000000000000000080c6a47e8d0300010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

/// Lamports of Fartcoin's curve account at that slot.
pub const FARTCOIN_CURVE_LAMPORTS: u64 = 3_934_883;

/// A pump `BondingCurve` (ARCH §4.1 offsets) for the detection tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct CurveSpec {
    pub virtual_token_reserves: u64,
    pub virtual_quote_reserves: u64,
    pub real_token_reserves: u64,
    pub real_quote_reserves: u64,
    pub complete: bool,
    pub is_mayhem_mode: bool,
    /// `Pubkey::default()` for SOL.
    pub quote_mint: Pubkey,
    /// The account length (150 today; 115 is the minimum a listing accepts).
    pub len: usize,
}

/// The bytes of a `BondingCurve` per `spec`.
pub fn curve_bytes(spec: &CurveSpec) -> Vec<u8> {
    let mut d = vec![0u8; spec.len.max(8)];
    let put = |d: &mut Vec<u8>, off: usize, b: &[u8]| {
        if off + b.len() <= d.len() {
            d[off..off + b.len()].copy_from_slice(b);
        }
    };
    put(&mut d, 0, &pump::BONDING_CURVE_DISCRIMINATOR);
    put(
        &mut d,
        pump::OFF_VIRTUAL_TOKEN_RESERVES,
        &spec.virtual_token_reserves.to_le_bytes(),
    );
    put(
        &mut d,
        pump::OFF_VIRTUAL_QUOTE_RESERVES,
        &spec.virtual_quote_reserves.to_le_bytes(),
    );
    put(
        &mut d,
        pump::OFF_REAL_TOKEN_RESERVES,
        &spec.real_token_reserves.to_le_bytes(),
    );
    put(
        &mut d,
        pump::OFF_REAL_QUOTE_RESERVES,
        &spec.real_quote_reserves.to_le_bytes(),
    );
    put(
        &mut d,
        pump::OFF_TOKEN_TOTAL_SUPPLY,
        &1_000_000_000_000_000u64.to_le_bytes(),
    );
    put(&mut d, pump::OFF_COMPLETE, &[u8::from(spec.complete)]);
    put(
        &mut d,
        pump::OFF_IS_MAYHEM_MODE,
        &[u8::from(spec.is_mayhem_mode)],
    );
    put(&mut d, pump::OFF_QUOTE_MINT, spec.quote_mint.as_ref());
    d
}

/// Decodes [`FARTCOIN_CURVE_HEX`].
pub fn fartcoin_curve_bytes() -> Vec<u8> {
    (0..FARTCOIN_CURVE_HEX.len() / 2)
        .map(|i| u8::from_str_radix(&FARTCOIN_CURVE_HEX[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// Writes `data` as `mint`'s pump bonding curve (owner pump).
pub fn plant_curve(svm: &mut LiteSVM, mint: &Pubkey, data: Vec<u8>) {
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        pda::bonding_curve(mint),
        Account {
            lamports,
            data,
            owner: PUMP_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("plant curve");
}

/// Writes a PumpSwap `Pool` at `mint`'s canonical-pool address: index 0, created by pump's
/// `pool-authority` PDA of `mint`, base `mint`, quote `quote_mint`, mayhem off (a valid canonical
/// pool when `quote_mint` is WSOL).
pub fn plant_canonical_pool(svm: &mut LiteSVM, mint: &Pubkey, quote_mint: &Pubkey) {
    let authority =
        Pubkey::find_program_address(&[b"pool-authority", mint.as_ref()], &PUMP_PROGRAM_ID).0;
    let mut d = vec![0u8; 301];
    d[..8].copy_from_slice(&pump_amm::POOL_DISCRIMINATOR);
    d[pump_amm::OFF_POOL_BUMP] = 255;
    d[pump_amm::OFF_INDEX..pump_amm::OFF_INDEX + 2].copy_from_slice(&0u16.to_le_bytes());
    d[pump_amm::OFF_CREATOR..pump_amm::OFF_CREATOR + 32].copy_from_slice(authority.as_ref());
    d[pump_amm::OFF_BASE_MINT..pump_amm::OFF_BASE_MINT + 32].copy_from_slice(mint.as_ref());
    d[pump_amm::OFF_QUOTE_MINT..pump_amm::OFF_QUOTE_MINT + 32].copy_from_slice(quote_mint.as_ref());
    let lamports = svm.minimum_balance_for_rent_exemption(d.len());
    svm.set_account(
        pda::canonical_pool(mint),
        Account {
            lamports,
            data: d,
            owner: PUMP_AMM_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("plant canonical pool");
}

// ---- scenario builders -----------------------------------------------------------------------

/// Sets an SPL mint's `supply` (@36): an LP burn (lower) or unlocked LP minted (higher), which
/// moves the pool's locked share `λ = (counter − supply)/counter` (ARCH §2.6).
pub fn set_mint_supply(svm: &mut LiteSVM, mint: &Pubkey, supply: u64) {
    fork::patch(svm, mint, |d| {
        d[shortwin_venue_state::mint::SUPPLY_OFFSET..shortwin_venue_state::mint::SUPPLY_OFFSET + 8]
            .copy_from_slice(&supply.to_le_bytes());
    });
}

/// The LP mint's supply that makes the pool's locked share exactly `lambda_bps` (floor of the
/// counter's complement): `supply = counter − ⌈counter·λ/10⁴⌉`.
pub fn supply_for_lambda(counter: u64, lambda_bps: u64) -> u64 {
    let locked = (u128::from(counter) * u128::from(lambda_bps)).div_ceil(10_000);
    counter - locked as u64
}

/// The pool's internal LP counter (AMM v4 `lp_amount` @720, CPMM `lp_supply` @333).
pub fn lp_counter(svm: &LiteSVM, pool: &AnyPool) -> u64 {
    match pool {
        AnyPool::V4(p) => p.info(svm).lp_amount,
        AnyPool::Cpmm(p) => p.state(svm).lp_supply,
    }
}

/// The pool's LP mint.
pub fn lp_mint(pool: &AnyPool) -> Pubkey {
    match pool {
        AnyPool::V4(p) => p.lp_mint,
        AnyPool::Cpmm(p) => p.lp_mint,
    }
}

/// An SPL / Token-2022 mint's supply.
pub fn mint_supply(svm: &LiteSVM, mint: &Pubkey) -> u64 {
    let acc = svm.get_account(mint).expect("mint");
    shortwin_venue_state::mint::read_base(&acc.owner.to_bytes(), &acc.data)
        .expect("mint")
        .supply
}

/// The pool's locked depth `D` on its live state (`pool_select::locked_depth`: λ floored to
/// bps, then `⌊λ·isqrt(X·Y)/10⁴⌋`), computed from the readers.
pub fn locked_depth(svm: &LiteSVM, pool: &AnyPool) -> u128 {
    let (x, y) = pool.io_reserves(svm, Direction::Sell).expect("reserves");
    shortwin_math::pool_select::locked_depth(
        lp_counter(svm, pool),
        mint_supply(svm, &lp_mint(pool)),
        x,
        y,
    )
    .expect("D")
}

impl Env {
    /// Writes a fresh SOL/USD price into the oracle cache ($121.20, `expo = −8`, published now),
    /// as `crank_sol_usd` would (ARCH §4.7): the fork's dumped Pyth account is older than
    /// `oracle_max_age_s` at the fork's clock.
    pub fn fresh_oracle(&mut self) {
        let key = pda::oracle_cache();
        let mut c = self.oracle_cache();
        c.price = 12_120_000_000;
        c.conf = 1_000_000;
        c.expo = -8;
        c.publish_time = self.now();
        c.updated_slot = self.clock().slot;
        crate::fixtures::set_borsh(
            &mut self.svm,
            &key,
            &c,
            shortwin::state::OracleCache::SPACE,
            0,
        );
    }
}

// ---- transaction size (T-SIZE) ---------------------------------------------------------------

/// The Address Lookup Table program.
pub const ALT_PROGRAM: Pubkey =
    anchor_lang::prelude::pubkey!("AddressLookupTab1e1111111111111111111111111");

/// The Raydium part of ARCH §6.4's **protocol ALT** (A28): the venue programs and their
/// authorities, the token programs and the ATA program, System, the WSOL mint, all 21 CPMM
/// `AmmConfig`s, short.win's `config`, `venue_guard`, `oracle_cache` and event authority. (The
/// pump part — its programs, statics and fee recipients — changes nothing for a Raydium
/// transaction.) Per-market accounts stay inline, and so does every top-level program id.
pub fn protocol_alt_addresses() -> Vec<Pubkey> {
    use shortwin::constants::{
        ASSOCIATED_TOKEN_PROGRAM_ID, RAYDIUM_CPMM_AUTHORITY, RAYDIUM_CPMM_PROGRAM_ID,
        RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    };
    let mut v = vec![
        RAYDIUM_V4_PROGRAM_ID,
        RAYDIUM_V4_AUTHORITY,
        RAYDIUM_CPMM_PROGRAM_ID,
        RAYDIUM_CPMM_AUTHORITY,
        TOKEN_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
        ASSOCIATED_TOKEN_PROGRAM_ID,
        anchor_lang::system_program::ID,
        WSOL_MINT,
        pda::config(),
        pda::venue_guard(),
        pda::oracle_cache(),
        pda::event_authority(),
    ];
    v.extend((0..fork::CPMM_CONFIGS).map(fork::cpmm_config));
    v
}

/// Writes an active Address Lookup Table at `key` holding `addresses` (the program's
/// `LookupTableMeta`: type 1, never deactivated, extended at slot 0, no authority; then the
/// addresses from byte 56).
pub fn install_alt(svm: &mut LiteSVM, key: &Pubkey, addresses: &[Pubkey]) {
    let mut data = vec![0u8; 56];
    data[0..4].copy_from_slice(&1u32.to_le_bytes());
    data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    for a in addresses {
        data.extend_from_slice(a.as_ref());
    }
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        *key,
        Account {
            lamports,
            data,
            owner: ALT_PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("install ALT");
}

/// `shortvec` length of `n`.
fn shortvec_len(mut n: usize) -> usize {
    let mut len = 1;
    while n >= 0x80 {
        n >>= 7;
        len += 1;
    }
    len
}

/// The wire size of a legacy transaction of `ixs` paid by `payer`: signatures and message.
pub fn legacy_size(payer: &Pubkey, ixs: &[Instruction]) -> (usize, usize) {
    let msg = solana_message::Message::new(ixs, Some(payer));
    let sigs = usize::from(msg.header.num_required_signatures);
    (
        shortvec_len(sigs) + 64 * sigs + msg.serialize().len(),
        msg.account_keys.len(),
    )
}

/// A v0 message of `ixs` paid by `payer` with the lookup table `(alt, addresses)`.
pub fn v0_message(
    payer: &Pubkey,
    ixs: &[Instruction],
    alt: &Pubkey,
    addresses: &[Pubkey],
    blockhash: solana_message::Hash,
) -> solana_message::VersionedMessage {
    let table = solana_message::AddressLookupTableAccount {
        key: *alt,
        addresses: addresses.to_vec(),
    };
    solana_message::VersionedMessage::V0(
        solana_message::v0::Message::try_compile(payer, ixs, &[table], blockhash)
            .expect("compile v0"),
    )
}

/// The wire size of a v0 transaction of `ixs` with the lookup table, and its account count.
pub fn v0_size(
    payer: &Pubkey,
    ixs: &[Instruction],
    alt: &Pubkey,
    addresses: &[Pubkey],
) -> (usize, usize) {
    let msg = v0_message(payer, ixs, alt, addresses, solana_message::Hash::default());
    let sigs = usize::from(msg.header().num_required_signatures);
    let keys = match &msg {
        solana_message::VersionedMessage::V0(m) => {
            m.account_keys.len()
                + m.address_table_lookups
                    .iter()
                    .map(|l| l.writable_indexes.len() + l.readonly_indexes.len())
                    .sum::<usize>()
        }
        solana_message::VersionedMessage::Legacy(m) => m.account_keys.len(),
        other => panic!("not a v0 message: {other:?}"),
    };
    (shortvec_len(sigs) + 64 * sigs + msg.serialize().len(), keys)
}

impl Env {
    /// Sends one instruction as a **v0** transaction through the lookup table `(alt,
    /// addresses)` (installed with [`install_alt`]), with the invariant hooks of a
    /// one-instruction [`Env::send`]: `check_ix` against the state before, then `check_all`
    /// on every tracked market.
    pub fn send_v0(
        &mut self,
        ix: Instruction,
        signers: &[&Keypair],
        alt: &Pubkey,
        addresses: &[Pubkey],
    ) -> TxResult {
        for m in &ix.accounts {
            if crate::read::zc::<shortwin::state::Market>(&self.svm, &m.pubkey).is_some() {
                self.track_market(m.pubkey);
            }
        }
        let admins = self.admin_keys();
        let pre = crate::invariants::snapshot(&self.svm, self.tracked_markets());
        let msg = v0_message(
            &self.payer.pubkey(),
            std::slice::from_ref(&ix),
            alt,
            addresses,
            self.svm.latest_blockhash(),
        );
        let payer = self.payer.insecure_clone();
        let mut all: Vec<&Keypair> = vec![&payer];
        all.extend(
            signers
                .iter()
                .copied()
                .filter(|s| s.pubkey() != payer.pubkey()),
        );
        let tx = solana_transaction::versioned::VersionedTransaction::try_new(msg, &all)
            .expect("sign v0");
        // `execute_recorded`: the session recording hook (record.rs; INDEXER.md §11.3).
        let res = self.execute_recorded(tx);
        self.svm.expire_blockhash();
        if self.check_invariants {
            if let Ok(meta) = &res {
                let ev = crate::invariants::IxEvents::of_instruction(meta, 0, &ix);
                let admin = crate::invariants::is_admin_signed(&ix, &admins);
                if let Err(v) = crate::invariants::check_ix(&self.svm, &pre, admin, &ev) {
                    panic!("{v}\nlogs:\n{}", meta.pretty_logs());
                }
            }
            self.assert_invariants();
        }
        res
    }
}
