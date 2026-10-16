//! Shared machinery of the ECON §12 ports on Raydium **CPMM** (`tests/raydium_cpmm_attacks.rs`)
//! and **PumpSwap** (`tests/pumpswap_attacks.rs`) (ARCH §8.2 "adversarial suite … on every real
//! venue as it lands"; M10 G-C-CPMM, G-C-PSwap, G-D-real, G-E4): the release short.win on the
//! dumped venue programs and pools, driven only by real instructions — the attacker's and other
//! traders' venue swaps (CPMM `swap_base_input` / `swap_base_output`, PumpSwap `buy` / `sell`),
//! third-party LP `deposit` / `withdraw`, and short.win's own instructions — every one through
//! `Env::send`, so the invariant hooks run after each instruction of every bundle.
//!
//! `tests/raydium_cpmm_attacks.rs` and `tests/pumpswap_attacks.rs` include this file with
//! `#[path]`; compiled on its own it is an empty test binary.
//!
//! **No differential oracle on these venues.** AMM v4 (`raydium_v4_support.rs`) and the curve
//! (`pump_curve_support.rs`) have one; here the program's **own decision** is read instead, on a
//! clone of the VM: [`Lab::try_send`] sends a transaction to a clone (no hook, nothing kept) and
//! returns the result or the error code — "would the honest keeper's `liquidate` land now?",
//! "the largest front-run after which the attacker's bundle still lands". The claims are then
//! **structural**, on the events of what lands for real: the I11 spend bound `B ≤
//! ⌊B̂_ref·(1 + δ)⌋` and `B ≤ max_in`, the lenders' and protocol's legs against the honest run's,
//! the attacker's extra against the borrower's loss, `NotLiquidatable` on the final bundles,
//! settlements within `fv_dev_bps` of the honest one and never S1 where the honest one is S2/S3.
//! No ECON sim market is either venue's pool, so no figure is compared to the lamport (the
//! scripts' to-the-lamport ports are the mock's, AMM v4's and the curve's).
//!
//! The ECON scripts' primitives, as on the other venues:
//!
//! | Function | Script |
//! |---|---|
//! | [`Lab::second`], [`Lab::tick`] | the honest keeper's crank each second (`step_to`) |
//! | [`Lab::second_at`] | `exo_move` + `step_to`: other traders move the pool, then the keeper cranks |
//! | [`Lab::atomic_second`] | `push_second(.., "atomic")`: the honest crank, then `[pump, crank_price, dump]` |
//! | [`Lab::attacker_only_second`] | `push_second(.., "held")` / `attacker_bundle` alone in its second: the attacker's `[move, crank_price, move back]` is the second's only observation |
//! | [`Lab::frozen_second`] | the honest crank and the attacker's observation just off `P_ema` (push-then-freeze) |
//! | [`Lab::best_dx`], [`Lab::sandwich`] | `sandwich_exit`: the largest front-run after which `[buy dX, exit, sell dX]` lands |

#![allow(
    dead_code,
    unused_imports,
    clippy::result_large_err,
    clippy::too_many_arguments
)]

pub use anchor_lang::prelude::Pubkey;
pub use anchor_lang::solana_program::instruction::Instruction;
pub use litesvm::types::TransactionMetadata;
pub use shortwin::errors::ShortwinError;
pub use shortwin::events::{
    Expire, Liquidate, Open, SettleArmed, SettleDisarmed, SolSettle, VenueStateChanged,
};
pub use shortwin::state::{InitConfigArgs, Market, ShortPosition, VenueKind};
pub use shortwin::venues::ExtrasSel;
pub use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    set_route_broken_ix, sol_settle_ix, with_cu_limit,
};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
pub use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, Swap, Trader};
pub use shortwin_litesvm_tests::ix::{self, LenderKeys};
pub use shortwin_litesvm_tests::pamm::{self, PammCoin, PammLp, PammMarket, PammTrade, PammUser};
pub use shortwin_litesvm_tests::pump::cu_limit_ix;
pub use shortwin_litesvm_tests::raydium::{self, RayFixture, RayMarket, RaySpec};
pub use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
pub use shortwin_math::fees::Direction;
pub use shortwin_math::num::WAD;
pub use solana_keypair::Keypair;
pub use solana_signer::Signer;

use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_math::num::{Rounding, mul_div, sqrt_u128};
use shortwin_math::quote::{self, Pricing};
use shortwin_math::{rent, risk};

/// short.win's error codes.
pub type E = ShortwinError;
/// `WAD`.
pub const W: u128 = WAD;
/// A lamport-denominated SOL.
pub const SOL: u64 = 1_000_000_000;
/// The CU limit of an attacker's bundle and of a keeper exit.
pub const BUNDLE_CU: u32 = 1_400_000;

/// The config's per-market sizing limits lifted within their compiled bounds
/// (`market_max_notional` 10¹³, `wallet_cap_bps` 10⁴): positions at the SPEC size caps, the
/// size at which ECON prices the attacks (the beta notional cap of 25 SOL binds first otherwise).
pub fn lift_notional(a: &mut InitConfigArgs) {
    a.params.market_max_notional = 10_000_000_000_000;
    a.params.wallet_cap_bps = 10_000;
}

/// `⌊p·m⌋` for a price and a float factor.
pub fn scale(p: u128, m: f64) -> u128 {
    ((p as f64) * m) as u128
}

/// `x` lamports as a signed SOL string with 4 decimals.
pub fn sol4(x: i128) -> String {
    format!("{:+.4}", x as f64 / 1e9)
}

// ---- the venue -------------------------------------------------------------------------------

/// The market under attack: a CPMM or a PumpSwap market.
pub enum Venue {
    Cpmm(RayMarket),
    Pswap(PammMarket),
}

impl VenueMarket for Venue {
    fn keys(&self) -> &MarketKeys {
        match self {
            Self::Cpmm(m) => m.keys(),
            Self::Pswap(m) => m.keys(),
        }
    }
    fn read_set(&self, writable: bool) -> Vec<anchor_lang::prelude::AccountMeta> {
        match self {
            Self::Cpmm(m) => m.read_set(writable),
            Self::Pswap(m) => m.read_set(writable),
        }
    }
    fn trade_extras(&self) -> Vec<anchor_lang::prelude::AccountMeta> {
        match self {
            Self::Cpmm(m) => m.trade_extras(),
            Self::Pswap(m) => m.trade_extras(),
        }
    }
    fn cu_limit(&self) -> Option<u32> {
        match self {
            Self::Cpmm(m) => m.cu_limit(),
            Self::Pswap(m) => m.cu_limit(),
        }
    }
}

/// A participant trading the pool directly (attacker, whale): a WSOL account and a token account
/// of the pool's token (on PumpSwap also its pAMM UVA).
pub struct Actor {
    pub kp: Keypair,
    pub wsol: Pubkey,
    pub token: Pubkey,
}

impl Actor {
    pub fn key(&self) -> Pubkey {
        self.kp.pubkey()
    }

    fn trader(&self) -> Trader {
        Trader {
            owner: self.kp.insecure_clone(),
            wsol: self.wsol,
            token: self.token,
        }
    }
}

/// The exit a keeper sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Liquidate,
    Expire,
}

/// What a keeper `sol_settle` did.
pub struct Settled {
    pub ev: Option<SolSettle>,
    pub armed: Option<SettleArmed>,
    pub disarmed: Option<SettleDisarmed>,
}

impl Settled {
    fn of(meta: &TransactionMetadata) -> Self {
        Self {
            ev: events::of::<SolSettle>(meta).pop(),
            armed: events::of::<SettleArmed>(meta).pop(),
            disarmed: events::of::<SettleDisarmed>(meta).pop(),
        }
    }
}

/// The market on the fork with its keeper and a whale (other traders).
pub struct Lab {
    pub env: Env,
    pub v: Venue,
    pub keeper: Keypair,
    pub whale: Actor,
}

impl Lab {
    /// A CPMM market on `fixture`: the release short.win initialized with `f` applied to the
    /// harness's default arguments, both Raydium programs (CPMM's 2026-10 redeploy) and the fork's
    /// clock; listed, `lend(X, Y)` tokens lent (`X`, `Y` the pool's pricing reserves), the listing
    /// window waited out with the keeper's warm-up observations at its end, one crank.
    pub fn cpmm(
        fixture: RayFixture,
        lend: impl FnOnce(u64, u64) -> u64,
        f: impl FnOnce(&mut InitConfigArgs),
    ) -> Self {
        let mut env = Env::new();
        let mut args = env.default_init_args();
        f(&mut args);
        ok(env.initialize_config(args));
        env.load_raydium_programs();
        env.set_fork_clock();
        let pool = env.load_ray_pool(fixture);
        let (x, y) = pool
            .io_reserves(&env.svm, Direction::Sell)
            .expect("reserves");
        let lend = lend(x, y);
        ok(env.list_ray(&pool, fixture.hint()));
        let rm = env.ray_market_listed(
            RaySpec {
                fixture,
                lend,
                warmup: 0,
            },
            pool,
        );
        let window = i64::from(env.config().params.pool_list_window_s);
        env.warp(window - raydium::PRICE_WARMUP_CRANKS);
        env.warm_up_price(&rm);
        env.warp(1);
        env.crank(&rm);
        Self::with(env, Venue::Cpmm(rm))
    }

    /// A PumpSwap market on `coin`'s canonical pool: the release short.win initialized with `f`,
    /// the pump and PumpSwap programs and statics, the `pamm_state` clock; listed (a 2024-layout
    /// curve extended first), `lend(X, Y)` tokens lent, 60 s of cranks, the boost window waited out
    /// with a crank every 30 s.
    pub fn pswap(
        coin: PammCoin,
        lend: impl FnOnce(u64, u64) -> u64,
        f: impl FnOnce(&mut InitConfigArgs),
    ) -> Self {
        let (mut env, p) = Env::fork_pamm_coin_with(coin, f);
        let r = p.reserves(&env.svm).expect("reserves");
        let lend = lend(r.base, u64::try_from(r.effective_quote).unwrap());
        let curve = pda::bonding_curve(&p.mint);
        if env
            .svm
            .get_account(&curve)
            .is_some_and(|a| a.data.len() < shortwin_venue_state::pump::CURVE_MIN_LEN)
        {
            let payer = env.payer.pubkey();
            ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
        }
        ok(env.list_pamm(&p.mint, &p.base_program));
        let pm = env.pamm_market_listed(&p.mint, lend, 60);
        let params = env.config().params;
        loop {
            let m = env.market(&pm.market());
            if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
                || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
            {
                break;
            }
            env.warp(30);
            env.crank(&pm);
        }
        env.warp(1);
        env.crank(&pm);
        Self::with(env, Venue::Pswap(pm))
    }

    fn with(mut env: Env, v: Venue) -> Self {
        let keeper = env.funded_keypair();
        let mut lab = Self {
            env,
            v,
            keeper,
            whale: Actor {
                kp: Keypair::new(),
                wsol: Pubkey::default(),
                token: Pubkey::default(),
            },
        };
        lab.whale = lab.actor(100_000 * SOL, 0);
        lab
    }

    /// The CPMM market (panics on PumpSwap).
    pub fn rm(&self) -> &RayMarket {
        match &self.v {
            Venue::Cpmm(m) => m,
            Venue::Pswap(_) => panic!("not a CPMM market"),
        }
    }

    /// The PumpSwap market (panics on CPMM).
    pub fn pm(&self) -> &PammMarket {
        match &self.v {
            Venue::Pswap(m) => m,
            Venue::Cpmm(_) => panic!("not a PumpSwap market"),
        }
    }

    // ---- reading ----------------------------------------------------------------------------

    pub fn market(&self) -> Market {
        self.env.market(&self.v.market())
    }

    pub fn pos(&self, b: &Pubkey, nonce: u64) -> ShortPosition {
        self.env.position(&self.v, b, nonce)
    }

    /// Whether `b`'s position `nonce` is still open (its account holds lamports).
    pub fn is_open(&self, b: &Pubkey, nonce: u64) -> bool {
        let key = pda::position(&self.v.market(), b, nonce).0;
        self.env
            .svm
            .get_account(&key)
            .is_some_and(|a| a.lamports > 0)
    }

    /// The position account of `b`'s position `nonce`.
    pub fn pos_key(&self, b: &Pubkey, nonce: u64) -> Pubkey {
        pda::position(&self.v.market(), b, nonce).0
    }

    /// The pool's pricing reserves `(X, Y)`: tokens and SOL (PumpSwap: the quote vault + VQR).
    pub fn reserves(&self) -> (u128, u128) {
        match &self.v {
            Venue::Cpmm(m) => {
                let (x, y) = m.reserves(&self.env.svm).expect("reserves");
                (u128::from(x), u128::from(y))
            }
            Venue::Pswap(m) => {
                let r = m.pool.reserves(&self.env.svm).expect("reserves");
                (u128::from(r.base), r.effective_quote)
            }
        }
    }

    /// The pool's spot `⌊Y·WAD/X⌋`.
    pub fn spot(&self) -> u128 {
        let (x, y) = self.reserves();
        risk::price_wad(x, y).expect("price")
    }

    /// The venue's live pricing (execution quotes), for the scripts' targets.
    pub fn pricing(&self) -> Pricing {
        match &self.v {
            Venue::Cpmm(m) => {
                let p = m.cpmm();
                Pricing::Cpmm {
                    buy: p.fees(&self.env.svm, Direction::Buy).expect("buy fees"),
                    sell: p.fees(&self.env.svm, Direction::Sell).expect("sell fees"),
                }
            }
            Venue::Pswap(m) => Pricing::PumpSwap(pswap_fees(&self.env, &m.pool)),
        }
    }

    /// Tokens to buy (> 0) or sell (< 0) to bring the pool's spot to `target` along its constant
    /// product (`move_size`): `X − isqrt(X·Y·WAD/target)`.
    pub fn move_size(&self, target: u128) -> i128 {
        let (x, y) = self.reserves();
        let tx = sqrt_u128(
            mul_div(x * y, W, target.max(1), Rounding::Floor).expect("k/P"),
            Rounding::Floor,
        );
        i128::try_from(x).unwrap() - i128::try_from(tx).unwrap()
    }

    // ---- participants and trades --------------------------------------------------------------

    /// A new participant holding `sol` wrapped lamports and `tokens` of the pool's token
    /// (planted: the mints have no authority).
    pub fn actor(&mut self, sol: u64, tokens: u64) -> Actor {
        match &self.v {
            Venue::Cpmm(m) => {
                let (mint, program) = m.pool.token();
                let t = fork::trader(&mut self.env, &mint, program, sol, tokens);
                Actor {
                    kp: t.owner,
                    wsol: t.wsol,
                    token: t.token,
                }
            }
            Venue::Pswap(m) => {
                let pool = m.pool;
                let u = PammUser::new(&mut self.env, &pool, sol);
                if tokens > 0 {
                    fork::set_token_amount(&mut self.env.svm, &u.base, tokens);
                }
                Actor {
                    kp: u.kp,
                    wsol: u.wsol,
                    token: u.base,
                }
            }
        }
    }

    /// The venue's buy of exactly `tokens` by `a` with its WSOL (no price bound).
    pub fn buy_ix(&self, a: &Actor, tokens: u64) -> Instruction {
        match &self.v {
            Venue::Cpmm(m) => m.pool.swap_ix(
                &self.env.svm,
                &a.key(),
                &a.trader(),
                Direction::Buy,
                Swap::ExactOut(u64::MAX, tokens),
            ),
            Venue::Pswap(m) => pamm::pamm_trade_ix(
                &self.env.svm,
                &m.pool,
                PammTrade::Buy,
                &a.key(),
                &a.token,
                &a.wsol,
                tokens,
                u64::MAX,
            ),
        }
    }

    /// The venue's sale of exactly `tokens` by `a` for WSOL (no bound).
    pub fn sell_ix(&self, a: &Actor, tokens: u64) -> Instruction {
        match &self.v {
            Venue::Cpmm(m) => m.pool.swap_ix(
                &self.env.svm,
                &a.key(),
                &a.trader(),
                Direction::Sell,
                Swap::ExactIn(tokens, 0),
            ),
            Venue::Pswap(m) => pamm::pamm_trade_ix(
                &self.env.svm,
                &m.pool,
                PammTrade::Sell,
                &a.key(),
                &a.token,
                &a.wsol,
                tokens,
                0,
            ),
        }
    }

    /// `[buy d … , middle, sell d]` (`d > 0`) or `[sell −d, middle, buy −d]` (`d < 0`): the
    /// attacker's round trip around `middle`.
    pub fn round_trip(&self, a: &Actor, d: i128, middle: Vec<Instruction>) -> Vec<Instruction> {
        let mut ixs = vec![cu_limit_ix(BUNDLE_CU)];
        match d {
            0 => ixs.extend(middle),
            d if d > 0 => {
                let d = u64::try_from(d).unwrap();
                ixs.push(self.buy_ix(a, d));
                ixs.extend(middle);
                ixs.push(self.sell_ix(a, d));
            }
            d => {
                let d = u64::try_from(-d).unwrap();
                ixs.push(self.sell_ix(a, d));
                ixs.extend(middle);
                ixs.push(self.buy_ix(a, d));
            }
        }
        ixs
    }

    /// `a`'s WSOL.
    pub fn wsol(&self, a: &Actor) -> i128 {
        i128::from(read::token_amount(&self.env.svm, &a.wsol).unwrap_or(0))
    }

    /// `a`'s tokens.
    pub fn tokens(&self, a: &Actor) -> u64 {
        read::token_amount(&self.env.svm, &a.token).unwrap_or(0)
    }

    /// `a`'s SOL: WSOL and lamports (keeper rewards are paid in lamports to the signer).
    pub fn sol_of(&self, a: &Actor) -> i128 {
        self.wsol(a) + i128::from(read::lamports(&self.env.svm, &a.key()))
    }

    /// Tops `a`'s WSOL up to `sol` and its tokens to `tokens` (scenario funding).
    pub fn fund(&mut self, a: &Actor, sol: u64, tokens: u64) {
        let have = read::token_amount(&self.env.svm, &a.wsol).unwrap_or(0);
        if have < sol {
            fork::wrap(&mut self.env, &a.wsol, sol - have);
        }
        if self.tokens(a) < tokens {
            fork::set_token_amount(&mut self.env.svm, &a.token, tokens);
        }
    }

    /// Sends `ixs` signed by `a` through `Env::send` (must land). Returns the metadata and `a`'s
    /// SOL change.
    pub fn send_as(&mut self, a: &Actor, ixs: &[Instruction]) -> (TransactionMetadata, i128) {
        let s0 = self.sol_of(a);
        let kp = a.kp.insecure_clone();
        let meta = ok(self.env.send(ixs, &[&kp]));
        let d = self.sol_of(a) - s0;
        (meta, d)
    }

    /// `ixs` sent to a clone of the VM (no hook, nothing kept): the metadata, or the custom error
    /// code (`None` for any other failure).
    pub fn try_send(
        &self,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> Result<TransactionMetadata, Option<u32>> {
        let tx = self.env.transaction(ixs, signers);
        let mut svm = self.env.svm.clone();
        svm.send_transaction(tx).map_err(|f| match f.err {
            solana_transaction_error::TransactionError::InstructionError(
                _,
                solana_instruction::error::InstructionError::Custom(c),
            ) => Some(c),
            _ => None,
        })
    }

    // ---- the clock and other traders --------------------------------------------------------

    /// `crank_price` (must succeed).
    pub fn crank(&mut self) {
        ok(self.env.send(&[crank_price_ix(&self.v)], &[]));
    }

    /// One keeper second: the clock advances one second and the keeper cranks.
    pub fn second(&mut self) {
        self.env.warp(1);
        self.crank();
    }

    /// `n` keeper seconds.
    pub fn tick(&mut self, n: u32) {
        for _ in 0..n {
            self.second();
        }
    }

    /// Other traders move the pool to about `target` now with one real swap (`exo_move`).
    pub fn move_to(&mut self, target: u128) {
        let d = self.move_size(target);
        if d == 0 {
            return;
        }
        let w = Actor {
            kp: self.whale.kp.insecure_clone(),
            wsol: self.whale.wsol,
            token: self.whale.token,
        };
        let ix = if d > 0 {
            self.buy_ix(&w, u64::try_from(d).unwrap())
        } else {
            let t = u64::try_from(-d).unwrap();
            self.fund(&w, 0, t);
            self.sell_ix(&w, t)
        };
        let kp = w.kp.insecure_clone();
        ok(self.env.send(&[cu_limit_ix(BUNDLE_CU), ix], &[&kp]));
    }

    /// One keeper second in which other traders first move the pool to `target`.
    pub fn second_at(&mut self, target: u128) {
        self.env.warp(1);
        self.move_to(target);
        self.crank();
    }

    // ---- positions ----------------------------------------------------------------------------

    /// An open of `q` by a new borrower (must land).
    pub fn open(&mut self, q: u64) -> (Borrower, u64, Open) {
        let mut b = self.env.borrower();
        let (res, nonce) = self.env.open(&self.v, &mut b, q);
        let meta = ok(res);
        (b, nonce, events::one(&meta))
    }

    /// An open of `q` by `b` at its next nonce, tried on a clone: `Ok` or the error code.
    pub fn open_probe(&self, b: &Borrower, q: u64) -> Result<(), Option<u32>> {
        let ix = open_short_ix(
            &self.v,
            &b.key(),
            b.next_nonce,
            q,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        );
        self.try_send(&[cu_limit_ix(BUNDLE_CU), ix], &[&b.kp])
            .map(|_| ())
    }

    /// The largest `q` the size and OI caps admit now (bisection on clones, to `X/10⁶`); 0 if no
    /// open of at least the minimum notional fits.
    pub fn max_open_q(&mut self) -> u64 {
        let (x, _) = self.reserves();
        let x = u64::try_from(x).unwrap();
        let b = self.env.borrower();
        let capped = |c: Option<u32>| {
            [
                E::CapPositionImpact,
                E::CapMarketOi,
                E::CapUtilization,
                E::CapWallet,
                E::InsufficientInventory,
                E::MarketNotionalCap,
            ]
            .iter()
            .any(|e| c == Some(u32::from(*e)))
        };
        let (mut lo, mut hi) = (0u64, x / 10);
        assert!(capped(self.open_probe(&b, hi).err().flatten()));
        while hi - lo > (x / 1_000_000).max(1) {
            let mid = lo + (hi - lo) / 2;
            if capped(self.open_probe(&b, mid).err().flatten()) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        if self.open_probe(&b, lo).is_ok() {
            lo
        } else {
            0
        }
    }

    // ---- keeper exits and settlements ------------------------------------------------------------

    /// `liquidate` / `expire` of `b`'s position `nonce` by `keeper`.
    pub fn exit_ix(&self, which: Which, keeper: &Pubkey, b: &Pubkey, nonce: u64) -> Instruction {
        match which {
            Which::Liquidate => liquidate_ix(&self.v, keeper, b, nonce),
            Which::Expire => expire_ix(&self.v, keeper, b, nonce),
        }
    }

    /// Whether the honest keeper's exit would land now (on a clone): `Ok` or the error code.
    pub fn exit_probe(&self, which: Which, b: &Pubkey, nonce: u64) -> Result<(), Option<u32>> {
        let ix = self.exit_ix(which, &self.keeper.pubkey(), b, nonce);
        self.try_send(&[cu_limit_ix(BUNDLE_CU), ix], &[&self.keeper])
            .map(|_| ())
    }

    /// The honest keeper's exit now (must land): its metadata.
    pub fn exit(&mut self, which: Which, b: &Pubkey, nonce: u64) -> TransactionMetadata {
        let kp = self.keeper.insecure_clone();
        let ix = self.exit_ix(which, &kp.pubkey(), b, nonce);
        ok(self.env.send(&[cu_limit_ix(BUNDLE_CU), ix], &[&kp]))
    }

    /// The honest keeper's `sol_settle` now (must land).
    pub fn settle(&mut self, b: &Pubkey, nonce: u64) -> Settled {
        let kp = self.keeper.insecure_clone();
        let ix = sol_settle_ix(&self.env, &self.v, &kp.pubkey(), b, nonce);
        Settled::of(&ok(self.env.send(&[cu_limit_ix(BUNDLE_CU), ix], &[&kp])))
    }

    /// One keeper second: a crank and a `sol_settle`. The settlement, if any.
    pub fn keeper_second(&mut self, b: &Pubkey, nonce: u64) -> Settled {
        self.second();
        self.settle(b, nonce)
    }

    /// The attacker-keeper's `[buy dX, exit, sell dX]` as one transaction (dX = 0: the exit
    /// alone).
    pub fn sandwich_ixs(
        &self,
        a: &Actor,
        which: Which,
        b: &Pubkey,
        nonce: u64,
        dx: u64,
    ) -> Vec<Instruction> {
        let exit = self.exit_ix(which, &a.key(), b, nonce);
        self.round_trip(a, i128::from(dx), vec![exit])
    }

    /// The largest front-run `dX` after which the attacker's sandwich lands (the scripts'
    /// `sandwich_exit` optimum: the attacker's P&L grows with `dX` while the exit executes): the
    /// window's upper edge on a 0.5 % grid of the token reserve, then by bisection to `X/10⁶`. 0 if
    /// the exit lands only without a front-run.
    pub fn best_dx(&self, a: &Actor, which: Which, b: &Pubkey, nonce: u64) -> u64 {
        let (x, _) = self.reserves();
        let x = u64::try_from(x).unwrap();
        let lands = |dx: u64| {
            self.try_send(&self.sandwich_ixs(a, which, b, nonce, dx), &[&a.kp])
                .is_ok()
        };
        let step = x / 200;
        let first = (x / 1_000_000).max(1);
        let mut lo = if lands(first) {
            first
        } else {
            let mut d = step;
            while !lands(d) {
                d += step;
                if d > x / 10 * 7 {
                    return 0;
                }
            }
            d
        };
        let mut hi = lo + step;
        while lands(hi) {
            lo = hi;
            hi += step;
        }
        while hi - lo > (x / 1_000_000).max(1) {
            let mid = lo + (hi - lo) / 2;
            if lands(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The attacker-keeper's sandwich `[buy dX, exit, sell dX]` through `Env::send` (must land):
    /// its metadata and the attacker's P&L (WSOL and lamports; the back-run sells exactly what
    /// the front-run bought).
    pub fn sandwich(
        &mut self,
        a: &Actor,
        which: Which,
        b: &Pubkey,
        nonce: u64,
        dx: u64,
    ) -> (TransactionMetadata, i128) {
        let t0 = self.tokens(a);
        let ixs = self.sandwich_ixs(a, which, b, nonce, dx);
        let out = self.send_as(a, &ixs);
        assert_eq!(
            self.tokens(a),
            t0,
            "the back-run sells what the front-run bought"
        );
        out
    }

    // ---- the scripts' seconds -----------------------------------------------------------------

    /// `push_second(.., "atomic")`: the honest keeper cranks, then the attacker's one-transaction
    /// `[buy d, crank_price, sell d]` shows the pool at `target` to the same second's bucket.
    /// Returns the attacker's cost.
    pub fn atomic_second(&mut self, a: &Actor, target: u128) -> i128 {
        self.second();
        let d = self.move_size(target);
        if d <= 0 {
            return 0;
        }
        let ixs = self.round_trip(a, d, vec![crank_price_ix(&self.v)]);
        -self.send_as(a, &ixs).1
    }

    /// An attacker-only second: the clock advances and the attacker's `[move to target,
    /// crank_price, move back]` is the second's only observation (no honest crank). Returns its
    /// cost.
    pub fn attacker_only_second(&mut self, a: &Actor, target: u128) -> i128 {
        self.env.warp(1);
        let d = self.move_size(target);
        if d < 0 {
            self.fund(a, 0, u64::try_from(-d).unwrap());
        }
        let ixs = self.round_trip(a, d, vec![crank_price_ix(&self.v)]);
        -self.send_as(a, &ixs).1
    }

    /// A frozen second: the honest keeper cranks, then the attacker's observation at `target`
    /// (just off `P_ema`: the bucket straddles it). Returns its cost.
    pub fn frozen_second(&mut self, a: &Actor, target: u128) -> i128 {
        self.second();
        let d = self.move_size(target);
        if d < 0 {
            self.fund(a, 0, u64::try_from(-d).unwrap());
        }
        let ixs = self.round_trip(a, d, vec![crank_price_ix(&self.v)]);
        -self.send_as(a, &ixs).1
    }

    /// The attacker's `[pump to mult × spot, sol_settle, dump]` one second later (the flip
    /// bundle; checked to land). The settlement, if any.
    pub fn flip_bundle(&mut self, a: &Actor, b: &Pubkey, nonce: u64, mult: f64) -> Settled {
        self.env.warp(1);
        let d = self.move_size(scale(self.spot(), mult)).max(0);
        let settle = sol_settle_ix(&self.env, &self.v, &a.key(), b, nonce);
        let ixs = self.round_trip(a, d, vec![settle]);
        Settled::of(&self.send_as(a, &ixs).0)
    }

    // ---- the scripts' targets -------------------------------------------------------------------

    /// `R_due` of a position now.
    pub fn r_due(&self, p: &ShortPosition) -> u64 {
        rent::due(&p.pos_rent(), &self.market().basis()).expect("rent")
    }

    /// `K_s` with the position's locked terms.
    pub fn k_s(p: &ShortPosition) -> u64 {
        risk::k_s(p.v, &p.settle_keeper_terms()).expect("K_s")
    }

    /// The spot multiple at which `p`'s S1 live leg starts to hold (`B̂(q) + ert > V − K_s` on the
    /// pool moved along its constant product at the live fees; `arming.s1_boundary`).
    pub fn s1_boundary(&self, p: &ShortPosition) -> f64 {
        let room = u128::from(p.v - Self::k_s(p));
        let (x, y) = self.reserves();
        let (x, y) = (x as f64, y as f64);
        let pricing = self.pricing();
        let cost = |m: f64| {
            let x1 = (x / m.sqrt()).floor() as u128;
            let y1 = (y * m.sqrt()).ceil() as u128;
            pricing.buy(p.q, x1, y1).map(|b| u128::from(b.cost))
        };
        let (mut lo, mut hi) = (1.0f64, 50.0f64);
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            match cost(mid) {
                Some(c) if c + u128::from(p.ert) <= room => lo = mid,
                _ => hi = mid,
            }
        }
        hi
    }

    /// `block_spot.liq_price_target`: the largest spot at which the liquidation spot leg still
    /// fails — `CR(V_eff, B̂(q; reprice(L_live, P))) ≥ L` at the live fees — by bisection on `P`.
    pub fn liq_price_target(&self, p: &ShortPosition) -> u128 {
        let v_eff = u128::from(p.v.saturating_sub(self.r_due(p)));
        let (x, y) = self.reserves();
        let l = quote::liquidity(x, y).expect("L");
        let pricing = self.pricing();
        let healthy = |price: u128| {
            pricing
                .buy_repriced(p.q, l, price)
                .is_some_and(|b| v_eff * 10_000 / u128::from(b.cost) >= u128::from(p.cr_liq_bps))
        };
        let spot = self.spot();
        let (mut lo, mut hi) = (spot / 4, spot);
        for _ in 0..80 {
            let mid = (lo + hi) / 2;
            if healthy(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The Emergency multisig sets or clears the market's route-broken flag (O15).
    pub fn set_route_broken(&mut self, broken: bool) {
        let ea = self.env.pause_authority.insecure_clone();
        let ix = set_route_broken_ix(
            &ea.pubkey(),
            shortwin::PauseScope::Market,
            0,
            broken,
            Some(self.v.market()),
        );
        ok(self.env.send(&[ix], &[&ea]));
    }
}

/// PumpSwap's fee on `p` now (`shortwin-math` on the readers' state: the live `FeeConfig`,
/// `GlobalConfig`, the pool and the mint's supply).
pub fn pswap_fees(env: &Env, p: &pamm::PammPool) -> shortwin_math::fees::FeeBps {
    use shortwin_math::fees::{self, FeeTier, PammFeeSchedule};
    use shortwin_venue_state::{pump as vpump, pump_amm as vamm};
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
            fees: fees::FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
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

// ---- the claims ---------------------------------------------------------------------------------

/// The lenders' and protocol's take of a liquidation: rent and their penalty shares.
pub fn lenders_take(e: &Liquidate) -> u64 {
    e.rent + e.penalty_lender + e.penalty_protocol
}

/// The penalty base `min(B̂_live,risk, B̂_ref)` of a liquidation.
pub fn penalty_base(e: &Liquidate) -> u64 {
    e.b_live_risk.min(e.b_ref)
}

/// The lenders' and protocol's legs of an attacked liquidation (`ev`) against the honest one's
/// (`h`; position penalty `penalty_bps`). Rent is senior and equal; the penalty is
/// `⌈p·min(B̂_live,risk, B̂_ref)⌉`, split with floors. A front-run's own fees stay in the pool
/// and lower `B̂_ref` by a few hundred-thousandths of a percent (USELESS: 38,524 lamports of
/// 358.78 SOL), which moves the legs by the penalty on that drop (2,158 lamports). So: the base
/// falls by at most 1 ppm, and the legs fall by at most the lenders' and protocol's penalty on
/// the base's drop plus 2 lamports of flooring — a real loss to the lenders would be orders of
/// magnitude above it.
pub fn assert_lenders_keep(h: &Liquidate, ev: &Liquidate, penalty_bps: u16, what: &str) {
    let (hl, al) = (lenders_take(h), lenders_take(ev));
    let drop = penalty_base(h).saturating_sub(penalty_base(ev));
    assert!(
        u128::from(drop) * 1_000_000 <= u128::from(penalty_base(h)),
        "{what}: the penalty base {} → {} falls by at most 1 ppm",
        penalty_base(h),
        penalty_base(ev)
    );
    let allowance = (u128::from(drop) * u128::from(penalty_bps)).div_ceil(10_000) as u64 + 2;
    assert!(
        al + allowance >= hl,
        "{what}: lenders and protocol keep the honest legs: {hl} vs {al} lamports (allowance \
         {allowance}: the penalty on the base's drop of {drop})"
    );
}

/// I11 on a liquidation: `B ≤ ⌊B̂_ref·(10⁴ + δ)/10⁴⌋` and `B ≤ max_in`.
pub fn assert_spend_bound(ev: &Liquidate, delta_bps: u16, what: &str) {
    assert!(
        u128::from(ev.buyback_cost) * 10_000
            <= u128::from(ev.b_ref) * (10_000 + u128::from(delta_bps))
            && ev.buyback_cost <= ev.max_in,
        "{what}: I11, B {} within ⌊B̂_ref·(1 + δ)⌋ of B̂_ref {} (δ {delta_bps} bps) and max_in {}",
        ev.buyback_cost,
        ev.b_ref,
        ev.max_in
    );
}

// ---- the scenarios, shared by both venues ---------------------------------------------------------

/// A market built by `make` and one position at the per-position cap (the largest open the caps
/// admit; `make` lifts the sizing limits).
pub fn capped(make: &dyn Fn() -> Lab, label: &str) -> (Lab, Borrower, u64) {
    let mut lab = make();
    let q = lab.max_open_q();
    let (b, nonce, open) = lab.open(q);
    let (x, y) = lab.reserves();
    eprintln!(
        "{label} victim: q = {q} ({:.3} % of X = {x}), N_open = {:.2} SOL, V = {:.2} SOL, pool \
         {:.0} SOL",
        q as f64 * 100.0 / x as f64,
        open.n_open as f64 / 1e9,
        open.v as f64 / 1e9,
        y as f64 / 1e9
    );
    (lab, b, nonce)
}

/// An attacker (or attacker-keeper) with 100,000 SOL of WSOL.
pub fn attacker(lab: &mut Lab) -> Actor {
    lab.actor(100_000 * SOL, 0)
}

/// Whether the honest keeper's `liquidate` would be refused `NotLiquidatable` now.
pub fn not_liquidatable(lab: &Lab, b: &Pubkey, nonce: u64) -> bool {
    lab.exit_probe(Which::Liquidate, b, nonce) == Err(Some(u32::from(E::NotLiquidatable)))
}

/// `sandwich.setup_liq` on the real pool: 120 s of honest cranks, a quick honest rise to ×1.8
/// with the EMA settled (60 s), then +0.1 %/s (other traders' real buy and the keeper's crank
/// every second) until the first second the honest keeper's `liquidate` lands (tried on a
/// clone). Returns the multiple of the post-open spot reached.
pub fn setup_liq(make: &dyn Fn() -> Lab, label: &str) -> (Lab, Borrower, u64, f64) {
    let (mut lab, b, nonce) = capped(make, label);
    lab.tick(120);
    let p0 = lab.spot();
    for k in 1..=5 {
        lab.second_at(scale(p0, 1.8f64.powf(f64::from(k) / 5.0)));
    }
    lab.tick(60);
    assert!(
        not_liquidatable(&lab, &b.key(), nonce),
        "{label}: healthy at ×1.8"
    );
    let p1 = lab.spot();
    let mut m = 1.0f64;
    for _ in 0..2_000 {
        m *= 1.001;
        lab.second_at(scale(p1, m));
        match lab.exit_probe(Which::Liquidate, &b.key(), nonce) {
            Ok(()) => return (lab, b, nonce, 1.8 * m),
            Err(c) => assert!(
                c == Some(u32::from(E::NotLiquidatable))
                    || c == Some(u32::from(E::ExitBoundNotMet)),
                "{label}: {c:?}"
            ),
        }
    }
    panic!("{label}: never liquidatable");
}

/// `sandwich.py` / `fixed_delta.py` (d), liquidation, atomic model: the honest keeper at the first
/// liquidatable second, the attacker-keeper's best `[buy dX, liquidate, sell dX]` in the same
/// second and after 4 s of waiting (nobody exits). The spend stays within `⌊B̂_ref·(1 + δ)⌋`
/// (I11); lenders and protocol get the honest legs; the attacker's extra comes out of the
/// borrower's remainder only, at most `δ·B̂_ref`; waiting keeps δ.
pub fn liquidation_sandwich(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce, k) = setup_liq(make, label);
    let a = attacker(&mut lab);
    let delta = lab.pos(&b.key(), nonce).delta_liq_bps;
    let penalty = lab.pos(&b.key(), nonce).penalty_bps;
    let saved = lab.env.svm.clone();
    let h: Liquidate = events::one(&lab.exit(Which::Liquidate, &b.key(), nonce));
    let honest_keeper = i128::from(h.penalty_keeper + h.keeper_topup);

    lab.env.svm = saved.clone();
    let dx = lab.best_dx(&a, Which::Liquidate, &b.key(), nonce);
    let (meta, pnl) = lab.sandwich(&a, Which::Liquidate, &b.key(), nonce, dx);
    let ev: Liquidate = events::one(&meta);
    assert_spend_bound(&ev, delta, label);
    let extra = pnl - honest_keeper;
    let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
    eprintln!(
        "{label} liquidatable at ×{k:.3}: dX = {dx}, B {:.4} → {:.4} SOL (B̂_ref {} → {} \
         lamports), attacker extra {} SOL, borrower loss {} SOL, lenders + protocol {} → {} \
         lamports",
        h.buyback_cost as f64 / 1e9,
        ev.buyback_cost as f64 / 1e9,
        h.b_ref,
        ev.b_ref,
        sol4(extra),
        sol4(loss),
        lenders_take(&h),
        lenders_take(&ev)
    );
    assert!(dx > 0, "{label}: a front-run lands");
    assert!(
        loss > 0 && extra < loss,
        "{label}: the attacker's extra comes out of the borrower only"
    );
    assert!(
        u128::try_from(loss).unwrap() * 10_000 <= u128::from(ev.b_ref) * u128::from(delta),
        "{label}: the borrower loses at most δ·B̂_ref"
    );
    assert_lenders_keep(&h, &ev, penalty, label);

    lab.env.svm = saved;
    lab.tick(4);
    let dx4 = lab.best_dx(&a, Which::Liquidate, &b.key(), nonce);
    let (meta4, pnl4) = lab.sandwich(&a, Which::Liquidate, &b.key(), nonce, dx4);
    let ev4: Liquidate = events::one(&meta4);
    assert_eq!(ev4.delta_bps, ev.delta_bps, "{label}: the fixed δ");
    assert_spend_bound(&ev4, delta, label);
    let loss4 = i128::from(h.borrower_payout) - i128::from(ev4.borrower_payout);
    eprintln!(
        "{label} after 4 s: attacker extra {} SOL, borrower loss {} SOL",
        sol4(pnl4 - honest_keeper),
        sol4(loss4)
    );
    assert!(
        loss4 * 100 <= loss * 125 + 1_000_000,
        "{label}: waiting does not widen the bound: loss {loss} → {loss4}"
    );
}

/// `sandwich.py`, expiry, atomic model: the victim held to its deadline at a flat price (a crank
/// an hour, every second for the last 120 s); the honest keeper's expiry and the attacker's best
/// sandwich. Rent is senior, the expiry fee within 0.1 % of the honest one (its base moves only by
/// the front-run's liquidity), the spend within `δ_exp`, the extra out of the borrower only.
pub fn expiry_sandwich(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce) = capped(make, label);
    let pos = lab.pos(&b.key(), nonce);
    while lab.env.now() + 3_600 < pos.deadline - 120 {
        lab.env.warp(3_600);
        lab.crank();
    }
    let to = pos.deadline - 120 - lab.env.now();
    lab.env.warp(to);
    lab.tick(120);
    assert_eq!(lab.env.now(), pos.deadline);
    let a = attacker(&mut lab);
    let saved = lab.env.svm.clone();
    let h: Expire = events::one(&lab.exit(Which::Expire, &b.key(), nonce));
    lab.env.svm = saved;
    let dx = lab.best_dx(&a, Which::Expire, &b.key(), nonce);
    let (meta, pnl) = lab.sandwich(&a, Which::Expire, &b.key(), nonce, dx);
    let ev: Expire = events::one(&meta);
    let extra = pnl - i128::from(h.keeper_reward);
    let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
    eprintln!(
        "{label} expiry sandwich: dX = {dx}, B {:.4} → {:.4} SOL (B̂_ref {:.4}), rent {:.6} / \
         {:.6}, fee {:.6} / {:.6}, attacker extra {} SOL, borrower loss {} SOL",
        h.buyback_cost as f64 / 1e9,
        ev.buyback_cost as f64 / 1e9,
        ev.b_ref as f64 / 1e9,
        h.rent as f64 / 1e9,
        ev.rent as f64 / 1e9,
        h.expiry_fee as f64 / 1e9,
        ev.expiry_fee as f64 / 1e9,
        sol4(extra),
        sol4(loss)
    );
    assert_eq!(ev.rent, h.rent, "{label}: rent is senior");
    assert!(
        h.expiry_fee.abs_diff(ev.expiry_fee) * 1_000 <= h.expiry_fee,
        "{label}: the expiry fee is senior"
    );
    assert!(
        u128::from(ev.buyback_cost) * 10_000
            <= u128::from(ev.b_ref) * (10_000 + u128::from(pos.delta_exp_bps))
            && ev.buyback_cost <= ev.max_in,
        "{label}: the spend within δ_exp"
    );
    assert!(
        dx == 0 || (loss > 0 && extra < loss),
        "{label}: the attacker's extra comes out of the borrower only"
    );
}

/// `sandwich.attack(m, pos, t, ("liquidation",), "held")` (ECON §12.6, O19): the attacker-keeper
/// holds the pool at `P_ema × (1 + x)` for `h` attacker-only seconds (every observation sees it:
/// the degraded-mode exposure) before its best sandwich — 1 s at +2.5 % and 3 s at +15 %. The
/// spend stays within `⌊B̂_ref·(1 + δ)⌋` of the lifted reference; while the borrower keeps a
/// remainder the lenders' and protocol's legs are at least the honest ones; the attacker's extra
/// over an honest keeper, net of its pushes, comes out of the borrower's remainder only.
pub fn held_sandwich(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce, _) = setup_liq(make, label);
    let a = attacker(&mut lab);
    let delta = lab.pos(&b.key(), nonce).delta_liq_bps;
    let penalty = lab.pos(&b.key(), nonce).penalty_bps;
    let saved = lab.env.svm.clone();
    let h: Liquidate = events::one(&lab.exit(Which::Liquidate, &b.key(), nonce));
    let honest_keeper = i128::from(h.penalty_keeper + h.keeper_topup);
    lab.env.svm = saved.clone();
    let dx = lab.best_dx(&a, Which::Liquidate, &b.key(), nonce);
    let (_, atomic) = lab.sandwich(&a, Which::Liquidate, &b.key(), nonce, dx);
    for (secs, x_bps) in [(1u32, 250u128), (3, 1_500)] {
        lab.env.svm = saved.clone();
        let mut pre = 0i128;
        for _ in 0..secs {
            let target = lab.market().ema_price.get() * (10_000 + x_bps) / 10_000;
            pre += lab.attacker_only_second(&a, target);
        }
        lab.env.warp(1);
        let dx = lab.best_dx(&a, Which::Liquidate, &b.key(), nonce);
        let (meta, pnl) = lab.sandwich(&a, Which::Liquidate, &b.key(), nonce, dx);
        let ev: Liquidate = events::one(&meta);
        assert_spend_bound(&ev, delta, label);
        let (hl, al) = (lenders_take(&h), lenders_take(&ev));
        if ev.borrower_payout > 0 {
            let what = format!("{label} {secs} s at +{x_bps} bps");
            assert_lenders_keep(&h, &ev, penalty, &what);
        }
        let net = pnl - pre;
        let extra = net - honest_keeper;
        let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
        eprintln!(
            "{label} held {secs} s at +{:.1} %: push {} SOL, sandwich + keeper {} SOL, net {} SOL \
             (atomic {}), extra {} SOL; B̂_ref {:.4} → {:.4} SOL, lenders + protocol {} → {} \
             lamports, borrower {:.4} → {:.4} SOL",
            x_bps as f64 / 100.0,
            sol4(pre),
            sol4(pnl),
            sol4(net),
            sol4(atomic),
            sol4(extra),
            h.b_ref as f64 / 1e9,
            ev.b_ref as f64 / 1e9,
            hl,
            al,
            h.borrower_payout as f64 / 1e9,
            ev.borrower_payout as f64 / 1e9
        );
        assert!(
            extra <= 0 || extra < loss,
            "{label}: the attacker's extra comes out of the borrower's remainder only"
        );
    }
}

/// The attacker's final `[buy d, liquidate, sell d]` (it pumps the live spot to `target` inside
/// its own transaction): refused `NotLiquidatable`.
pub fn final_bundle_refused(lab: &mut Lab, a: &Actor, b: &Pubkey, nonce: u64, target: u128) {
    let d = u64::try_from(lab.move_size(target).max(1)).unwrap();
    let ixs = lab.sandwich_ixs(a, Which::Liquidate, b, nonce, d);
    let kp = a.kp.insecure_clone();
    assert_err(&lab.env.send(&ixs, &[&kp]), E::NotLiquidatable);
}

/// `ema_atomic.py`, atomic model: a fresh position (60 s after the open) under the attacker's
/// `[pump, crank_price, dump]` every second at ×1.25 … ×3 for 30 s next to the honest keeper's
/// crank: the EMA never rises above the honest evidence, the honest keeper's `liquidate` and the
/// attacker's own final bundle are refused `NotLiquidatable` every second. The control: one
/// three attacker-only seconds (no honest crank) at ×3 do lift the EMA.
pub fn atomic_fresh(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce) = capped(make, label);
    lab.tick(60);
    let a = attacker(&mut lab);
    let p_true = lab.spot();
    let ema0 = lab.market().ema_price.get();
    let saved = lab.env.svm.clone();
    for mult in [1.25, 1.5, 2.0, 3.0] {
        lab.env.svm = saved.clone();
        let target = scale(p_true, mult);
        let mut cost = 0i128;
        // The pool's own price after each round trip (its fees stay in the pool).
        let mut honest_max = p_true;
        for n in 1..=30 {
            cost += lab.atomic_second(&a, target);
            honest_max = honest_max.max(lab.spot());
            assert!(
                lab.market().ema_price.get() <= ema0.max(honest_max),
                "{label} ×{mult}, second {n}: the pushes do not lift the EMA"
            );
            assert!(
                not_liquidatable(&lab, &b.key(), nonce),
                "{label} ×{mult}, second {n}"
            );
            final_bundle_refused(&mut lab, &a, &b.key(), nonce, target);
        }
        eprintln!(
            "{label} ×{mult}: 30 atomic seconds cost the attacker {} SOL",
            sol4(cost)
        );
    }
    // The protection is the honest observation in each second: without it (attacker-only
    // seconds, the held model) the same pushes at ×3 lift the EMA within seconds.
    lab.env.svm = saved;
    for _ in 0..3 {
        lab.attacker_only_second(&a, scale(p_true, 3.0));
    }
    let lifted = lab.market().ema_price.get();
    eprintln!(
        "{label}: three attacker-only seconds at ×3 lift P_ema ×{:.4}",
        lifted as f64 / ema0 as f64
    );
    assert!(
        lifted > ema0.max(p_true) * 10_001 / 10_000,
        "{label}: an attacker-only push does move the EMA"
    );
}

/// `ema_atomic.py`, held model (AMM v4's `held_pushes_on_a_fresh_position_do_not_pay`, the mock's
/// `held_pushes_on_a_fresh_position_do_not_pay_ra`): the attacker holds the pool at ×2.5 of a fresh
/// position's spot (60 s after the open) with attacker-only seconds — its `[pump, crank_price,
/// dump]` the second's only observation, no honest crank — until its sandwich of the liquidation
/// would land the next second (tried on a clone), then lands it with its best front-run. The spend stays within `⌊B̂_ref·(1 + δ)⌋`; the borrower loses its remainder, but the
/// held seconds cost the attacker more than the sandwich and the keeper legs bring: net < 0.
pub fn held_fresh(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce) = capped(make, label);
    lab.tick(60);
    let a = attacker(&mut lab);
    let delta = lab.pos(&b.key(), nonce).delta_liq_bps;
    let target = scale(lab.spot(), 2.5);
    let mut paid = 0i128;
    let mut secs = 0u32;
    // After each held second: would the attacker's sandwich land in the next one (its front-run
    // lifts the live spot leg; the pool is back at the true price between its seconds)?
    let dx = loop {
        secs += 1;
        paid += lab.attacker_only_second(&a, target);
        let held = lab.env.svm.clone();
        lab.env.warp(1);
        let dx = lab.best_dx(&a, Which::Liquidate, &b.key(), nonce);
        let ixs = lab.sandwich_ixs(&a, Which::Liquidate, &b.key(), nonce, dx);
        if lab.try_send(&ixs, &[&a.kp]).is_ok() {
            break dx;
        }
        lab.env.svm = held;
        assert!(secs < 120, "{label}: never liquidatable");
    };
    let (meta, pnl) = lab.sandwich(&a, Which::Liquidate, &b.key(), nonce, dx);
    let ev: Liquidate = events::one(&meta);
    assert_spend_bound(&ev, delta, label);
    let net = pnl - paid;
    eprintln!(
        "{label} held ×2.5 for {secs} s: push cost {} SOL, sandwich + keeper legs {} SOL, net {} \
         SOL; dX = {dx}, borrower payout {:.4} SOL",
        sol4(paid),
        sol4(pnl),
        sol4(net),
        ev.borrower_payout as f64 / 1e9
    );
    assert!(net < 0, "{label}: the held attack loses money: {net}");
}

/// `ema_near.py`, atomic model: honest rises to 85 %, 90 % and 95 % of the multiple `k_liq` at
/// which [`setup_liq`]'s ramp liquidates, the EMA caught up (300 s of cranks); then 20 s of atomic
/// bundles at ×1.05 … ×2 never make the position liquidatable (the honest keeper's `liquidate`
/// refused every second), and the attacker's final bundle is refused `NotLiquidatable`.
pub fn atomic_near(make: &dyn Fn() -> Lab, label: &str, k_liq: f64) {
    for f in [0.85, 0.90, 0.95] {
        let (mut lab, b, nonce) = capped(make, label);
        lab.tick(120);
        let p0 = lab.spot();
        let k = k_liq * f;
        for i in 1..=5 {
            lab.second_at(scale(p0, k.powf(f64::from(i) / 5.0)));
        }
        lab.tick(300);
        assert!(
            not_liquidatable(&lab, &b.key(), nonce),
            "{label} ×{k:.3}: not liquidatable honestly"
        );
        let a = attacker(&mut lab);
        let p_true = lab.spot();
        let saved = lab.env.svm.clone();
        for mult in [1.05, 1.1, 1.2, 1.5, 2.0] {
            lab.env.svm = saved.clone();
            let target = scale(p_true, mult);
            for n in 1..=20 {
                lab.atomic_second(&a, target);
                assert!(
                    not_liquidatable(&lab, &b.key(), nonce),
                    "{label} ×{k:.3} push ×{mult}, second {n}"
                );
            }
            final_bundle_refused(&mut lab, &a, &b.key(), nonce, target);
        }
        eprintln!(
            "{label} ema_near at {:.0} % of ×{k_liq:.3}: no push liquidates",
            f * 100.0
        );
    }
}

/// `ema_atomic_multi.fill` on the real pool: positions at the largest size the caps admit, one a
/// second, until no open of the minimum notional fits (a further open is refused `CapMarketOi`);
/// then 60 s of cranks.
pub fn oi_basket(make: &dyn Fn() -> Lab, label: &str) -> (Lab, Vec<(Borrower, u64)>) {
    let mut lab = make();
    let mut basket = Vec::new();
    loop {
        let q = lab.max_open_q();
        if q == 0 {
            break;
        }
        let (b, nonce, _) = lab.open(q);
        basket.push((b, nonce));
        lab.second();
        assert!(basket.len() < 20, "{label}: the OI cap binds");
    }
    let first = lab.pos(&basket[0].0.key(), basket[0].1).q;
    let probe = lab.env.borrower();
    assert_eq!(
        lab.open_probe(&probe, first / 2),
        Err(Some(u32::from(E::CapMarketOi))),
        "{label}: the per-market cap binds"
    );
    let (x, _) = lab.reserves();
    let sum: u64 = basket.iter().map(|(b, n)| lab.pos(&b.key(), *n).q).sum();
    eprintln!(
        "{label} OI basket: {} positions, ΣQ {:.3} % of X",
        basket.len(),
        sum as f64 * 100.0 / x as f64
    );
    lab.tick(60);
    (lab, basket)
}

/// `ema_atomic_multi.py` (a) and (b), atomic model, on the market filled to its OI cap: (a) at ×1.5
/// / ×2 / ×3 the attacker's `[pump, crank_price, dump]` every second for 60 s next to the honest
/// crank, its `[pump, liquidate, dump]` on every basket position refused `NotLiquidatable` every
/// 10 s; (b) a lender-keeper pushes past the basket's lowest S1 boundary (×1.1 / ×1.6 of it) with
/// `sol_settle` on every position inside its pumped transaction each second for 60 s: nothing
/// arms or settles.
pub fn basket_attacks(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, basket) = oi_basket(make, label);
    assert!(basket.len() >= 2, "{label}: a basket");
    let a = attacker(&mut lab);
    let p_true = lab.spot();
    let saved = lab.env.svm.clone();
    for mult in [1.5, 2.0, 3.0] {
        lab.env.svm = saved.clone();
        let target = scale(p_true, mult);
        for n in 1..=60 {
            lab.atomic_second(&a, target);
            if n % 10 == 0 {
                for (b, nonce) in &basket {
                    final_bundle_refused(&mut lab, &a, &b.key(), *nonce, target);
                }
            }
        }
    }
    let kb = basket
        .iter()
        .map(|(b, n)| lab.s1_boundary(&lab.pos(&b.key(), *n)))
        .fold(f64::MAX, f64::min);
    for m in [1.1, 1.6] {
        lab.env.svm = saved.clone();
        let target = scale(p_true, kb * m);
        for n in 1..=60 {
            lab.second();
            let d = lab.move_size(target).max(0);
            let settles = basket
                .iter()
                .map(|(b, nonce)| sol_settle_ix(&lab.env, &lab.v, &a.key(), &b.key(), *nonce))
                .collect();
            let ixs = lab.round_trip(&a, d, settles);
            let (meta, _) = lab.send_as(&a, &ixs);
            assert!(
                events::of::<SolSettle>(&meta).is_empty()
                    && events::of::<SettleArmed>(&meta).is_empty(),
                "{label} ×{:.2}, second {n}: nothing arms or settles",
                kb * m
            );
        }
        for (b, nonce) in &basket {
            assert_eq!(lab.pos(&b.key(), *nonce).s_armed, 0);
        }
    }
    eprintln!(
        "{label} OI basket: (a) ×1.5 / ×2 / ×3 for 60 s and (b) ×{:.2} / ×{:.2} for 60 s force \
         nothing",
        kb * 1.1,
        kb * 1.6
    );
}

/// ECON §12.11 `fv_push.py` one second before an honest capped settlement (`saved`, `honest`:
/// S2 on a thin pool, S3 under the Emergency flag): (i) one attacker-only push up (×1.3) or down
/// (×0.8), then the keeper every second: the same kind, its box corners within `fv_dev_bps` of
/// each other and its amount within twice that of the honest one (as on the mock), never S1; (ii) the flip bundle `[pump past the S1 boundary → sol_settle → dump]` after 1–2
/// pushed seconds, after 1 push and 2 / 4 / 6 frozen seconds, after 1–4 consecutive attacker-only
/// seconds and after 2–6 pushes with an honest second between each: never S1, any settlement
/// within `fv_dev_bps`. 5–6 consecutive attacker-only seconds (degraded mode, the documented
/// limit R4A-9) are reported.
pub fn amount_pushes_and_flips(
    lab: &mut Lab,
    b: &Pubkey,
    nonce: u64,
    saved: &litesvm::LiteSVM,
    honest: &SolSettle,
    label: &str,
) {
    lab.env.svm = saved.clone();
    let pos = lab.pos(b, nonce);
    let x_h = honest.lender_amount;
    let fv_dev = u128::from(pos.fv_dev_bps);
    let a = attacker(lab);
    let kb = lab.s1_boundary(&pos);
    let saved = lab.env.svm.clone();
    // As on the mock (`attacks_settle_it::fv_push`): the program bounds a settlement's own box
    // corners to `fv_dev_bps` of each other; the honest amount is somewhere in its own box, so
    // the pushed amount stays within `2·fv_dev_bps` of it.
    let within = |x: u64| u128::from(x.abs_diff(x_h)) * 10_000 <= 2 * fv_dev * u128::from(x_h);
    let corners = |ev: &SolSettle| {
        let (lo, hi) = (ev.x_lo.min(ev.x_hi), ev.x_lo.max(ev.x_hi));
        u128::from(hi - lo) * 10_000 <= fv_dev * u128::from(lo)
    };
    for mult in [1.3, 0.8] {
        lab.env.svm = saved.clone();
        let t = scale(lab.spot(), mult);
        let cost = lab.attacker_only_second(&a, t);
        let mut got = None;
        for s in 1..=60 {
            if let Some(ev) = lab.keeper_second(b, nonce).ev {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("settles");
        eprintln!(
            "{label} push ×{mult} (cost {} SOL): S{} {s} s later, {:.6} SOL vs honest S{} {:.6} SOL",
            sol4(cost),
            ev.condition,
            ev.lender_amount as f64 / 1e9,
            honest.condition,
            x_h as f64 / 1e9
        );
        assert_eq!(ev.condition, honest.condition, "{label} ×{mult}: never S1");
        assert!(
            corners(&ev) && within(ev.lender_amount),
            "{label} ×{mult}: the corners within fv_dev_bps ({} bps), the amount within twice \
             that of the honest one",
            pos.fv_dev_bps
        );
    }
    let check = |what: &str, ev: Option<SolSettle>| {
        if let Some(ev) = &ev {
            assert_ne!(ev.condition, 1, "{label} {what}: no S1");
            assert!(
                corners(ev) && within(ev.lender_amount),
                "{label} {what}: within fv_dev_bps"
            );
        }
        eprintln!(
            "{label} {what}: {}",
            ev.map_or("no settlement".to_owned(), |e| format!("S{}", e.condition))
        );
    };
    for n in [1, 2] {
        lab.env.svm = saved.clone();
        for _ in 0..n {
            let t = scale(lab.spot(), 1.3);
            lab.attacker_only_second(&a, t);
        }
        let ev = lab.flip_bundle(&a, b, nonce, kb * 1.1).ev;
        check(&format!("{n} pushed seconds + flip"), ev);
    }
    for k in [2, 4, 6] {
        lab.env.svm = saved.clone();
        let t = scale(lab.spot(), 1.3);
        lab.attacker_only_second(&a, t);
        for _ in 0..k {
            let ema = lab.market().ema_price.get();
            lab.frozen_second(&a, ema * 10_005 / 10_000);
        }
        let ev = lab.flip_bundle(&a, b, nonce, kb * 1.1).ev;
        check(&format!("1 push + {k} frozen seconds + flip"), ev);
    }
    for n in 1..=6u32 {
        for gap in [0u32, 1] {
            if n == 1 && gap == 1 {
                continue;
            }
            lab.env.svm = saved.clone();
            for i in 0..n {
                let t = scale(lab.spot(), 1.3);
                lab.attacker_only_second(&a, t);
                if i + 1 < n {
                    lab.tick(gap);
                }
            }
            let ev = lab.flip_bundle(&a, b, nonce, kb * 1.1).ev;
            let what = format!("{n} pushes, {gap} honest s between");
            if n >= 5 && gap == 0 {
                eprintln!(
                    "{label} {what} (the documented limit): {}",
                    ev.as_ref()
                        .map_or("no settlement".to_owned(), |e| format!("S{}", e.condition))
                );
            } else {
                check(&what, ev);
            }
        }
    }
}

/// ECON §12.11 (V), the `p′` veto (R4A-3; the mock's
/// `attacks_settle_it::a_contrary_observation_does_not_change_pprime`): the capped position, the
/// Emergency flag set 30 min before its deadline and a genuine ×2.3 move (other traders' real buy)
/// that makes it CR-liquidatable — and at the deadline expired — with the keeper cranking every
/// 30 s, then every second for the last minute (nobody liquidates under the flag). In the
/// deadline's second, after the keeper's crank, the borrower lands one contrary low observation
/// `[sell to 0.99 × the liquidation price, crank_price, buy back]` (it fails the robust spot leg);
/// the keeper's `sol_settle` the next second. `p′` is decided on each box corner, not on the raw
/// legs: the S3 with and without the veto applies the penalty at both corners, and the lenders'
/// amount, the lump and the borrower's remainder are the same to the lamport.
pub fn pprime_veto(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce) = capped(make, label);
    lab.tick(5);
    let pos = lab.pos(&b.key(), nonce);
    let deadline = pos.deadline;
    let to = deadline - 1_800 - lab.env.now();
    lab.env.warp(to);
    lab.crank();
    lab.set_route_broken(true);
    let p0 = lab.spot();
    lab.second_at(scale(p0, 2.3));
    while lab.env.now() + 30 < deadline - 60 {
        lab.env.warp(30);
        lab.crank();
    }
    let to = deadline - 1 - lab.env.now();
    lab.tick(u32::try_from(to).unwrap());
    let a = attacker(&mut lab);
    let saved = lab.env.svm.clone();
    let mut runs = Vec::new();
    for veto in [false, true] {
        lab.env.svm = saved.clone();
        lab.second();
        assert_eq!(lab.env.now(), deadline);
        if veto {
            let p = lab.pos(&b.key(), nonce);
            let target = lab.liq_price_target(&p) * 99 / 100;
            let d = lab.move_size(target);
            assert!(d < 0, "{label}: a low observation needs a sale");
            lab.fund(&a, 0, u64::try_from(-d).unwrap());
            let ixs = lab.round_trip(&a, d, vec![crank_price_ix(&lab.v)]);
            lab.send_as(&a, &ixs);
        }
        let ev = lab.keeper_second(&b.key(), nonce).ev.expect("S3");
        runs.push(ev);
    }
    let (h, v) = (&runs[0], &runs[1]);
    eprintln!(
        "{label} p′ veto: S{} / S{}, p′ {} / {} and {} / {} bps (penalty {}), lenders {} / {}, lump \
         {} / {}, borrower {} / {} lamports",
        h.condition,
        v.condition,
        h.pprime_lo,
        h.pprime_hi,
        v.pprime_lo,
        v.pprime_hi,
        pos.penalty_bps,
        h.lender_amount,
        v.lender_amount,
        h.lump,
        v.lump,
        h.borrower_amount,
        v.borrower_amount
    );
    assert_eq!(
        (h.condition, v.condition),
        (3, 3),
        "{label}: S3 under the flag"
    );
    assert_eq!(
        (h.pprime_lo, h.pprime_hi),
        (pos.penalty_bps, pos.penalty_bps),
        "{label}: the penalty at both corners"
    );
    assert_eq!(
        (v.pprime_lo, v.pprime_hi),
        (pos.penalty_bps, pos.penalty_bps),
        "{label}: the veto leaves p′ at both corners"
    );
    assert_eq!(
        (h.lender_amount, h.lump, h.borrower_amount),
        (v.lender_amount, v.lump, v.borrower_amount),
        "{label}: the veto changes nothing"
    );
}

/// `arming.py`'s set-up: the capped position, 120 s of cranks, a genuine gap (other traders' real
/// buy) to `beyond` × its S1 boundary.
pub fn s1_gap(make: &dyn Fn() -> Lab, label: &str, beyond: f64) -> (Lab, Borrower, u64) {
    let (mut lab, b, nonce) = capped(make, label);
    lab.tick(120);
    let kb = lab.s1_boundary(&lab.pos(&b.key(), nonce));
    let p0 = lab.spot();
    lab.move_to(scale(p0, kb * beyond));
    (lab, b, nonce)
}

/// Keeper seconds (a crank and a `sol_settle` each) until a settlement: its second and event.
pub fn honest_s1(lab: &mut Lab, b: &Pubkey, nonce: u64) -> (u32, SolSettle) {
    for s in 1..=600u32 {
        if let Some(ev) = lab.keeper_second(b, nonce).ev {
            return (s, ev);
        }
    }
    panic!("no settlement within 600 s");
}

/// `mature_soon` of `refute_push.py`: armed, and `lead` more EMA updates complete the clock.
pub fn mature_soon(lab: &Lab, b: &Pubkey, nonce: u64, lead: u64) -> bool {
    let p = lab.pos(b, nonce);
    p.s_armed == 1 && lab.market().ema_count + lead >= p.s_count + u64::from(p.settle_persist_n)
}

/// ECON §12.10 (`arming.py`, `refute_push.py` (a), (b), (c)) after a genuine gap 5 % past the S1
/// boundary, the keeper's crank and `sol_settle` every second:
/// - honest: S1 after `s_h` seconds;
/// - (V2-4) the borrower's `[dump below the S1 price → sol_settle → buy back]` every 20 s neither
///   settles nor disarms, and S1 lands in the honest second;
/// - (a) a refutation push — one attacker-only second at ×0.7 one EMA update before maturity,
///   every cycle — delays S1 by at most 4 s (the mock: 2–4 s);
/// - (c) push-then-freeze — one attacker-only ×0.7 second three updates before maturity, then 4
///   frozen seconds just below `P_ema` — by at most 8 s;
/// - (b) a spot-leg veto — from 20 s after arming, one low observation per second below the
///   liquidation price for 9 / 12 / 20 s — keeps the clock running: at most 6 s late.
pub fn round_3_s1(make: &dyn Fn() -> Lab, label: &str) {
    let (mut lab, b, nonce) = s1_gap(make, label, 1.05);
    let (honest, h) = honest_s1(&mut lab, &b.key(), nonce);
    assert_eq!(h.condition, 1, "{label}: the gap settles S1");
    eprintln!("{label} honest S1 after {honest} s");

    // arming.py: a borrower bundle every 20 s.
    {
        let (mut lab, b, nonce) = s1_gap(make, label, 1.05);
        let a = attacker(&mut lab);
        let kb = lab.s1_boundary(&lab.pos(&b.key(), nonce));
        let p0 = scale(lab.spot(), 1.0 / (kb * 1.05));
        let mut bundles = 0;
        let mut got = None;
        for s in 1..=600u32 {
            if let Some(ev) = lab.keeper_second(&b.key(), nonce).ev {
                got = Some((s, ev));
                break;
            }
            if s % 20 == 0 {
                let d = lab.move_size(scale(p0, kb * 0.99));
                assert!(d < 0, "a dump below the S1 price");
                lab.fund(&a, 0, u64::try_from(-d).unwrap());
                let settle = sol_settle_ix(&lab.env, &lab.v, &a.key(), &b.key(), nonce);
                let ixs = lab.round_trip(&a, d, vec![settle]);
                let (meta, _) = lab.send_as(&a, &ixs);
                assert!(
                    events::of::<SolSettle>(&meta).is_empty()
                        && events::of::<SettleDisarmed>(&meta).is_empty(),
                    "{label}: the bundle cannot disarm"
                );
                bundles += 1;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!("{label} borrower bundle every 20 s ({bundles}): S1 after {s} s");
        assert_eq!((ev.condition, s), (1, honest), "{label}: no delay");
    }

    // (a) refutation push and (c) push-then-freeze.
    for (lead, freeze, max_delay) in [(1u64, 0u32, 4u32), (3, 4, 8)] {
        let (mut lab, b, nonce) = s1_gap(make, label, 1.05);
        let a = attacker(&mut lab);
        let (mut pushes, mut frozen, mut freeze_left) = (0, 0, 0);
        let mut last = None;
        let mut got = None;
        for s in 1..=600u32 {
            let since = lab.pos(&b.key(), nonce).s_since;
            if freeze_left == 0 && mature_soon(&lab, &b.key(), nonce, lead) && last != Some(since) {
                last = Some(since);
                let t = scale(lab.spot(), 0.7);
                lab.attacker_only_second(&a, t);
                pushes += 1;
                freeze_left = freeze;
                continue;
            }
            if freeze_left > 0 {
                let ema = lab.market().ema_price.get();
                lab.frozen_second(&a, ema * 9_995 / 10_000);
                frozen += 1;
                freeze_left -= 1;
                if let Some(ev) = lab.settle(&b.key(), nonce).ev {
                    got = Some((s, ev));
                    break;
                }
                continue;
            }
            if let Some(ev) = lab.keeper_second(&b.key(), nonce).ev {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!(
            "{label} {} ({pushes} attacker-only, {frozen} frozen seconds): S1 after {s} s (honest \
             {honest})",
            if freeze == 0 {
                "refutation push"
            } else {
                "push-then-freeze"
            }
        );
        assert_eq!(ev.condition, 1, "{label}");
        assert!(pushes >= 1, "{label}: the attack ran");
        assert!(
            s <= honest + max_delay,
            "{label}: delayed by {} s (at most {max_delay})",
            s - honest
        );
    }

    // (b) the spot-leg veto.
    for w in [9u32, 12, 20] {
        let (mut lab, b, nonce) = s1_gap(make, label, 1.05);
        let a = attacker(&mut lab);
        let (mut armed_at, mut blocked) = (None, 0);
        let mut got = None;
        for s in 1..=600u32 {
            if armed_at.is_none() && lab.pos(&b.key(), nonce).s_armed == 1 {
                armed_at = Some(s);
            }
            if armed_at.is_some_and(|t| s - t >= 20) && blocked < w {
                let pos = lab.pos(&b.key(), nonce);
                lab.second();
                let target = lab.liq_price_target(&pos) * 995 / 1_000;
                let d = lab.move_size(target);
                assert!(d < 0, "{label}: a low observation needs a sale");
                lab.fund(&a, 0, u64::try_from(-d).unwrap());
                let ixs = lab.round_trip(&a, d, vec![crank_price_ix(&lab.v)]);
                lab.send_as(&a, &ixs);
                let out = lab.settle(&b.key(), nonce);
                assert!(
                    out.ev.is_none() && out.disarmed.is_none(),
                    "{label}: a vetoed second neither settles nor disarms"
                );
                blocked += 1;
                continue;
            }
            if let Some(ev) = lab.keeper_second(&b.key(), nonce).ev {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!("{label} spot-leg veto of {w} s: S1 after {s} s (honest {honest})");
        assert_eq!(ev.condition, 1, "{label}");
        assert!(
            s <= honest + 6,
            "{label} veto of {w} s: delayed by {} s",
            s - honest
        );
    }
}

/// Builds `[deposit LP, settle, withdraw LP]` around a `sol_settle` and returns it with the
/// liquidity provider that signs it.
pub type LpWrap = dyn Fn(&mut Lab, Instruction) -> (Vec<Instruction>, Keypair);

/// V2-4's liquidity variant (G-E4; ECON §12.10, R4B-2): from the gap past the S1 boundary, while
/// the keeper cranks and calls `sol_settle` every second, a liquidity provider lands `[deposit LP,
/// sol_settle, withdraw LP]` (`wrap` builds the venue's own LP instructions around the
/// settlement, doubling the pool's depth inside the transaction) after the keeper's crank and
/// before its `sol_settle`, until the S1 (the matrix's "every 20 s for 10 min" ends there: the
/// position is settled). No wrapped settlement settles or disarms. Two cadences:
/// - every 20 s and in the 5th, 3rd and 1st second before the honest S1 (at a 57-s S1: 5
///   bundles): its observations hold the doubled depth in at most 3 of the ring's 8 buckets, which
///   cannot move the box's medians — S1 lands in the honest second with the honest box (`P_lo`,
///   `P_hi`, `L_lo`, `L_hi`, within 1 bp: the round trips leave dust in the pool) and amounts;
/// - every 20 s and in each of the 5 seconds before the honest S1 (7 bundles): 5 of the 8
///   buckets' highs hold the doubled depth, so their upper median `L_mhi` doubles, but the box
///   reads `L_hi = max(L_ema, min(L_mhi, cap))` with the venue's depth cap, the locked depth `D`
///   on PumpSwap and, since AIR-14, on Raydium (AIR-15; `bounds::ref_depth_cap`, which no
///   capital-only bundle moves). `D` is at or below the live depth: the wraps widen nothing, and
///   the S1 lands in the honest second with the honest box. Were the cap above the honest
///   `L_hi` (Raydium's `⌊D·10⁴/λ_min⌋` before AIR-14), `L_hi` would widen at most to the cap —
///   ECON §5.6's disclosed residual "deferral by widening", which can only defer: the S1 lands at
///   most 4 s late, its price side and amounts the honest ones.
pub fn lp_around_settle(make: &dyn Fn() -> Lab, label: &str, wrap: &LpWrap) {
    let (hs, h) = {
        let (mut lab, b, nonce) = s1_gap(make, label, 1.05);
        honest_s1(&mut lab, &b.key(), nonce)
    };
    assert_eq!(h.condition, 1, "{label}: the gap settles S1");
    let near = |a: u128, b: u128| a.abs_diff(b) * 10_000 <= b;
    for dense in [false, true] {
        let what = if dense {
            "every 20 s and each of the last 5 s"
        } else {
            "every 20 s and 3 of the last 5 s"
        };
        let plan = |s: u32| {
            s.is_multiple_of(20) || (s + 5 >= hs && (dense || !(hs - s).is_multiple_of(2)))
        };
        let (mut lab, b, nonce) = s1_gap(make, label, 1.05);
        let mut bundles = 0;
        let mut got = None;
        for s in 1..=600u32 {
            lab.second();
            if s < hs && plan(s) {
                let settle = sol_settle_ix(&lab.env, &lab.v, &lab.keeper.pubkey(), &b.key(), nonce);
                let (ixs, signer) = wrap(&mut lab, settle);
                let kp = lab.keeper.insecure_clone();
                let meta = ok(lab.env.send(&ixs, &[&signer, &kp]));
                assert!(
                    events::of::<SolSettle>(&meta).is_empty()
                        && events::of::<SettleDisarmed>(&meta).is_empty(),
                    "{label} {what}, second {s}: the wrapped settlement neither settles nor \
                     disarms"
                );
                bundles += 1;
            }
            if let Some(ev) = lab.settle(&b.key(), nonce).ev {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!(
            "{label} LP around sol_settle {what} ({bundles} bundles): S{} after {s} s (honest S1 \
             after {hs} s); L_lo {} vs {}, L_hi {} vs {}, P_lo {} vs {}, P_hi {} vs {}; lenders \
             {} vs {} lamports",
            ev.condition,
            ev.l_lo,
            h.l_lo,
            ev.l_hi,
            h.l_hi,
            ev.p_lo,
            h.p_lo,
            ev.p_hi,
            h.p_hi,
            ev.lender_amount,
            h.lender_amount
        );
        let planned = (1..hs).filter(|&s| plan(s)).count();
        assert_eq!(bundles, planned, "{label} {what}: the attack ran");
        assert_eq!(ev.condition, 1, "{label} {what}: S1");
        assert_eq!(
            (ev.lender_amount, ev.borrower_amount, ev.k_s),
            (h.lender_amount, h.borrower_amount, h.k_s),
            "{label} {what}: the honest amounts"
        );
        assert!(
            near(ev.p_lo, h.p_lo) && near(ev.p_hi, h.p_hi),
            "{label} {what}: the honest price side of the box"
        );
        if !dense {
            assert_eq!(s, hs, "{label} {what}: S1 in the honest second");
            assert!(
                near(ev.l_lo, h.l_lo) && near(ev.l_hi, h.l_hi),
                "{label} {what}: the honest box"
            );
        } else {
            // AIR-15: the program's `L_hi` from the state the settling call evaluated (its
            // post-roll EMA and ring, and the `pool_d` its prologue refreshed).
            // The cap is the locked depth `D` on either venue (AIR-14: `⌊D·10⁴/λ⌋` at λ = 100 %).
            let m = lab.market();
            assert!(
                m.venue_kind().is_some_and(|k| k.has_locked_depth()),
                "a venue with a locked depth"
            );
            let cap = shortwin_math::bounds::ref_depth_cap(m.pool_d.get(), 10_000);
            let l_mhi = shortwin_math::observe::ring_medians(&m.ring_math())
                .unwrap()
                .3;
            assert_eq!(
                ev.l_hi,
                m.ema_liq.get().max(l_mhi.min(cap)),
                "{label} {what}: L_hi = max(L_ema, min(L_mhi, cap)), L_mhi {l_mhi}, cap {cap}"
            );
            if cap <= h.l_hi {
                assert_eq!(s, hs, "{label} {what}: S1 in the honest second");
                assert!(
                    near(ev.l_lo, h.l_lo) && near(ev.l_hi, h.l_hi),
                    "{label} {what}: the honest box"
                );
            } else {
                assert!(
                    s >= hs && s <= hs + 4,
                    "{label} {what}: a deferral of at most 4 s"
                );
                assert!(
                    near(ev.l_lo, h.l_lo) && ev.l_hi >= h.l_hi,
                    "{label} {what}: only L_hi widens"
                );
            }
        }
    }
}
