//! The lender lab: one list-only market on LiteSVM, lenders acting through the program, and
//! the borrower side injected into the accounts with `shortwin_math::pool`.
//!
//! **Lender instructions** (`deposit`, `withdraw`, `claim_sol`, `cover_deficit`, `sync_lender`)
//! are real transactions through [`Env::send`], so the invariant hooks (I1, I2, I4, I8, I17)
//! run after each one. Each is also checked against the math crate: before sending, the lab
//! computes what the handler's documented composition of `shortwin_math::pool` calls does to
//! the pre-state (prologue accrual = the vest release, since lab markets have no price and no
//! rent aggregate; then `deposit` / `withdraw` + `claim` / `touch` + `claim` / `cover` /
//! `convert` + `touch`), and requires the stored post-state (`Market` pool group,
//! `MarketVault`, the `LenderPosition` or its closing), the token and lamport movements, the
//! event and any refusal (the `MathError` → `ShortwinError` mapping) to match exactly.
//!
//! **Borrower side** (M2 instructions that do not exist yet): borrow, rent credit, repay,
//! settle and the trader sweep are injected with `pool::{borrow, accrue, repay, settle,
//! sweep, release}` on the stored accounts, with the matching token and lamport movements
//! (tokens leave and return to `pool_vault`; SOL reaches `MarketVault`) and `ShareEpoch`
//! records written at their PDAs; `open_positions` and `borrowed_q_by_tier` follow. Rent uses
//! `pool_model`'s single index: [`Lab::advance`] credits `A·ΔI/(WAD·10⁴)` and [`Lab::repay`]
//! trues up with `⌈q·(I − I_open)·(10⁴ − p)/(WAD·10⁴)⌉`, exactly as `pool_model` does, so
//! scenario results equal the model's to the lamport. After every injection the invariants
//! run too ([`Env::assert_invariants`]).
//!
//! Time: the lab's clock starts at a day boundary `base`, so `pool_model`'s time `t`
//! (which starts at 0) is `base + t` and day rounding is the same.

use std::collections::BTreeMap;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::types::TransactionMetadata;
use shortwin::errors::ShortwinError;
use shortwin::state::{GlobalConfig, LenderPosition, Market, MarketVault, ShareEpoch, VenueKind};
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::fixtures::{self, MarketKeys};
use shortwin_litesvm_tests::invariants::lender::{PoolState, reads_rebase};
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
use shortwin_math::MathError;
use shortwin_math::num::WAD;
use shortwin_math::pool::{
    self, Lender, Pool, Resolution, ShareEpoch as MathShareEpoch, Standing, SweepSource, Vault,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

/// One day.
pub const D: i64 = 86_400;
/// Basis points.
pub const BPS: u128 = 10_000;
/// Raw token units every lab user starts with.
pub const USER_TOKENS: u64 = 100_000_000_000_000_000;
/// Lamports every lab user starts with (for `cover_deficit`).
pub const USER_LAMPORTS: u64 = 1_000_000_000_000_000;
/// `ShareEpoch` records one lender instruction carries; a lender further behind is synced in
/// steps first (ARCH §3.3: "If a lender is behind by more epochs than fit in one transaction,
/// `sync_lender` advances it first").
pub const MAX_RECORDS_PER_TX: usize = 16;

/// `⌈a/b⌉`.
pub fn cdiv(a: u128, b: u128) -> u128 {
    a.div_ceil(b)
}

/// The program error code of `e`.
pub fn code(e: ShortwinError) -> u32 {
    u32::from(e)
}

/// A lab lender: its keypair and token account.
pub struct User {
    pub kp: Keypair,
    pub ata: Pubkey,
}

impl User {
    /// The owner key.
    pub fn pubkey(&self) -> Pubkey {
        self.kp.pubkey()
    }
}

/// An injected position (`pool_model.Pool.positions[pid] = [q, index_open, p_rent, …]`).
#[derive(Clone, Copy, Debug)]
pub struct Pos {
    pub q: u64,
    pub i_open: u128,
    pub p: u32,
}

/// The largest CU a lender instruction used in this lab, by name.
pub type CuTable = BTreeMap<&'static str, u64>;

/// What the math crate says a lender instruction does to its pre-state.
struct Expect {
    pool: Pool,
    vault: Box<Vault>,
    /// The lender afterwards (`None`: the account was closed).
    lender: Option<Lender>,
    /// Shares minted (deposit) or tokens paid (withdraw).
    result: u128,
    /// SOL paid.
    pay: u64,
    /// Of which drawn from the pot.
    pot_draw: u64,
    /// The records the instruction was given, as the math crate left them (KI-65: a
    /// conversion writes `conv_rem`; the program must store exactly these).
    records: Vec<MathShareEpoch>,
}

/// A list-only market with lenders and an injected borrower side.
pub struct Lab {
    pub env: Env,
    pub k: MarketKeys,
    pub program: TokenProgram,
    pub mint_authority: Keypair,
    pub users: BTreeMap<String, User>,
    /// Unix time of model time 0 (a day boundary).
    pub base: i64,
    /// `pool_model`'s positions (scenario API).
    pub positions: BTreeMap<String, Pos>,
    /// `pool_model`'s rent index (WAD lamports per raw token).
    pub index: u128,
    /// `Σ q·(10⁴ − p)` over the positions.
    pub a: u128,
    /// `pool_model.Pool.credit_rem` (AIR-03): the carried remainder of the last credit.
    pub credit_rem: u128,
    /// The `rent_protocol_bps` new positions snapshot.
    pub rent_protocol_bps: u32,
    /// `pool_model`'s utilisation cap for [`Lab::borrow`].
    pub util_cap_bps: u32,
    /// `lump_vest_s` of the injected repays, settlements and sweeps (the config value).
    pub lump_vest_s: u32,
    /// Largest CU per lender instruction.
    pub cu: CuTable,
}

impl Lab {
    /// A lab on `program` with `pool_model`'s default 90 % utilisation cap.
    pub fn new(program: TokenProgram) -> Self {
        Self::with_cap(program, 9_000)
    }

    /// A lab whose [`Lab::borrow`] enforces `util_cap_bps` (`pool_model.Pool(util_cap_bps)`).
    pub fn with_cap(program: TokenProgram, util_cap_bps: u32) -> Self {
        let env = Env::initialized();
        Self::on(env, program, util_cap_bps)
    }

    /// A lab on an already initialized environment (e.g. after a config change).
    pub fn on(mut env: Env, program: TokenProgram, util_cap_bps: u32) -> Self {
        // The lab injects the borrower side without `ShortPosition` accounts (module docs), so
        // the M2 position-accounting part of I6 cannot match it; every other check runs.
        shortwin_litesvm_tests::invariants::positions::allow_injected_borrower_side(true);
        let base = (env.now() / D + 1) * D;
        let slot = env.clock().slot + 100;
        env.set_clock(base, slot);
        let (k, mint_authority) = env.install_market(program, 6);
        let config: GlobalConfig = env.config();
        Self {
            env,
            k,
            program,
            mint_authority,
            users: BTreeMap::new(),
            base,
            positions: BTreeMap::new(),
            index: 0,
            a: 0,
            credit_rem: 0,
            rent_protocol_bps: config.params.rent_protocol_bps,
            util_cap_bps,
            lump_vest_s: config.params.lump_vest_s,
            cu: CuTable::new(),
        }
    }

    // ---- time -------------------------------------------------------------------------------

    /// Unix time.
    pub fn now(&self) -> i64 {
        self.env.now()
    }

    // ---- users --------------------------------------------------------------------------------

    /// The user `name`, created on first use with [`USER_TOKENS`] and [`USER_LAMPORTS`].
    pub fn user(&mut self, name: &str) -> &User {
        if !self.users.contains_key(name) {
            let payer = self.env.payer.insecure_clone();
            let h = tokens::holder(
                &mut self.env.svm,
                &payer,
                &self.k.mint,
                &self.mint_authority,
                USER_TOKENS,
                self.program,
            );
            // Written directly: LiteSVM's airdrop faucet is finite.
            let key = h.owner.pubkey();
            let mut acc = self.env.svm.get_account(&key).expect("holder");
            acc.lamports = USER_LAMPORTS;
            self.env.svm.set_account(key, acc).expect("fund user");
            self.users.insert(
                name.to_owned(),
                User {
                    kp: h.owner,
                    ata: h.token_account,
                },
            );
        }
        &self.users[name]
    }

    /// The owner key of `name`.
    pub fn key(&mut self, name: &str) -> Pubkey {
        self.user(name).kp.pubkey()
    }

    fn keys(&mut self, name: &str) -> (Keypair, LenderKeys) {
        let k = self.k;
        let u = self.user(name);
        (
            u.kp.insecure_clone(),
            LenderKeys {
                owner: u.kp.pubkey(),
                mint: k.mint,
                token_program: k.token_program,
                owner_token_account: u.ata,
            },
        )
    }

    /// The stored `LenderPosition` of `name`.
    pub fn position(&mut self, name: &str) -> Option<LenderPosition> {
        let owner = self.key(name);
        self.env.lender(&self.k.market, &owner)
    }

    /// The market's lender-pool state as the program sees it.
    pub fn state(&self) -> PoolState {
        PoolState::load(&self.env.svm, &self.k.market).expect("market state")
    }

    /// The stored `Market`.
    pub fn market(&self) -> Market {
        self.env.market(&self.k.market)
    }

    /// The stored `MarketVault`.
    pub fn vault(&self) -> MarketVault {
        self.env.market_vault(&self.k.market)
    }

    /// `pool_vault` token balance.
    pub fn pool_vault_amount(&self) -> u64 {
        read::token_amount(&self.env.svm, &self.k.pool_vault).unwrap_or(0)
    }

    /// Token balance of `name`.
    pub fn tokens_of(&mut self, name: &str) -> u64 {
        let ata = self.user(name).ata;
        read::token_amount(&self.env.svm, &ata).unwrap_or(0)
    }

    /// Lamports of `name`.
    pub fn lamports_of(&mut self, name: &str) -> u64 {
        let k = self.key(name);
        read::lamports(&self.env.svm, &k)
    }

    // ---- ShareEpoch records -------------------------------------------------------------------

    fn record_metas(&self, from: u32, count: usize) -> Vec<AccountMeta> {
        let share_epoch = self.market().share_epoch;
        (from..share_epoch)
            .take(count)
            .map(|e| AccountMeta::new(pda::share_epoch(&self.k.market, e).0, false))
            .collect()
    }

    fn records_slice(st: &PoolState, from: u32, count: usize) -> Vec<MathShareEpoch> {
        st.records_from(from).iter().take(count).copied().collect()
    }

    /// Brings `name` within [`MAX_RECORDS_PER_TX`] epochs of the market with `sync_lender`
    /// steps signed by `name` (each checked like any lender instruction).
    fn catch_up(&mut self, name: &str) {
        loop {
            let Some(l) = self.position(name) else { return };
            let behind = self.market().share_epoch.saturating_sub(l.epoch) as usize;
            if behind <= MAX_RECORDS_PER_TX || (l.shares == 0 && l.lock == 0 && l.lock_term == 0) {
                return;
            }
            let steps = u8::try_from(MAX_RECORDS_PER_TX).expect("u8");
            self.sync_signed(name, steps).expect("sync_lender step");
        }
    }

    // ---- lender instructions (on-chain, checked against the math crate) ------------------------

    fn run(
        &mut self,
        name: &'static str,
        ix: Instruction,
        signer: &Keypair,
    ) -> Result<TransactionMetadata, u32> {
        let res: TxResult = self.env.send(&[ix], &[signer]);
        let code = custom_code(&res);
        match res {
            Ok(meta) => {
                let cu = self.cu.entry(name).or_insert(0);
                *cu = (*cu).max(meta.compute_units_consumed);
                Ok(meta)
            }
            Err(f) => match code {
                Some(c) => Err(c),
                None => panic!(
                    "{name} failed without a program error: {:?}\n{}",
                    f.err,
                    f.meta.pretty_logs()
                ),
            },
        }
    }

    fn prologue(st: &PoolState, m: &Market) -> (Pool, Box<Vault>, i64) {
        assert_eq!(
            m.ema_price.get(),
            0,
            "lab markets have no price (rent credit 0)"
        );
        assert!(
            m.rent_aggr.iter().all(|a| a.get() == 0),
            "lab markets keep A_j = 0"
        );
        let now = st.now.max(m.last_accrual_ts);
        let (mut p, mut v) = (st.pool, st.vault.clone());
        pool::accrue(&mut p, &mut v, now, 0).expect("prologue accrual");
        (p, v, now)
    }

    fn compare(&mut self, what: &str, owner: &Pubkey, e: &Expect) {
        let st = self.state();
        assert_eq!(
            st.pool, e.pool,
            "{what}: Market pool state differs from the math crate"
        );
        assert_eq!(
            *st.vault, *e.vault,
            "{what}: MarketVault differs from the math crate"
        );
        let stored = self.env.lender(&self.k.market, owner).map(|l| l.lender());
        assert_eq!(
            stored, e.lender,
            "{what}: LenderPosition differs from the math crate"
        );
        // KI-65: every conversion remainder the math crate wrote is stored.
        if let Some(first) = e.records.first() {
            let on_chain = st.records_from(first.epoch);
            assert_eq!(
                &on_chain[..e.records.len().min(on_chain.len())],
                e.records.as_slice(),
                "{what}: ShareEpoch records differ from the math crate (conv_rem not stored?)"
            );
        }
    }

    fn check_error(what: &str, got: u32, want: Result<(), MathError>) {
        // The mint guard (ARCH §3.0 step 3) runs before any pool arithmetic.
        let mint_group =
            code(ShortwinError::MintUnsupportedProgram)..=code(ShortwinError::MintUnsafeFlagged);
        if mint_group.contains(&got) {
            return;
        }
        match want {
            Ok(()) => panic!("{what}: the program refused with {got}, the math crate accepts"),
            Err(e) => assert_eq!(
                got,
                code(ShortwinError::from(e)),
                "{what}: program error {got}, math crate {e:?}"
            ),
        }
    }

    /// `deposit(amount, 0)` by `name`: shares minted, or the program error code.
    pub fn deposit(&mut self, name: &str, amount: u64) -> Result<u128, u32> {
        self.deposit_min(name, amount, 0)
    }

    /// `deposit(amount, min_shares)`.
    pub fn deposit_min(&mut self, name: &str, amount: u64, min_shares: u128) -> Result<u128, u32> {
        self.catch_up(name);
        let (kp, lk) = self.keys(name);
        let st = self.state();
        let m = self.market();
        let from = st.lender_of(&lk.owner).map_or(m.share_epoch, |l| l.epoch);
        let metas = self.record_metas(from, MAX_RECORDS_PER_TX);
        let expect = (|| -> Result<Expect, MathError> {
            let (mut p, mut v, now) = Self::prologue(&st, &m);
            let mut l = st
                .lender_of(&lk.owner)
                .map_or_else(|| Lender::new(&p), LenderPosition::lender);
            let mut recs = Self::records_slice(&st, from, MAX_RECORDS_PER_TX);
            let rewards = matches!(
                VenueKind::from_u8(m.venue),
                Some(VenueKind::PumpCurve | VenueKind::PumpSwap)
            );
            if amount == 0 {
                return Err(MathError::DepositTooSmall);
            }
            let minted = pool::deposit(&mut p, &mut v, &mut l, &mut recs, now, amount, rewards)?;
            Ok(Expect {
                pool: p,
                vault: v,
                lender: Some(l),
                result: minted,
                pay: 0,
                pot_draw: 0,
                records: recs,
            })
        })();
        let pv = self.pool_vault_amount();
        let tok = self.tokens_of(name);
        let ix = ix::with_remaining(ix::deposit(&lk, amount, min_shares), &metas);
        match self.run("deposit", ix, &kp) {
            Ok(meta) => {
                let e = expect.unwrap_or_else(|err| {
                    panic!("deposit: program accepted, math crate refuses: {err:?}")
                });
                assert!(e.result >= min_shares);
                self.compare("deposit", &lk.owner, &e);
                assert_eq!(self.pool_vault_amount(), pv + amount);
                assert_eq!(self.tokens_of(name), tok - amount);
                let ev = events::one::<shortwin::events::Deposit>(&meta);
                assert_eq!(
                    (ev.amount, ev.shares, ev.t_after, ev.s_after),
                    (amount, e.result, e.pool.tokens_free, e.pool.total_shares)
                );
                Ok(e.result)
            }
            Err(c) => {
                if c != code(ShortwinError::SharesBelowMin) {
                    Self::check_error("deposit", c, expect.map(|_| ()));
                }
                Err(c)
            }
        }
    }

    /// `withdraw(shares, 0)` by `name`: `(tokens, sol_paid)`, or the program error code. The
    /// program pays the SOL part in the same instruction (ARCH §3.3), so `pool_model`'s
    /// `withdraw` followed by `claim_sol` is one call here.
    pub fn withdraw(&mut self, name: &str, shares: u128) -> Result<(u64, u64), u32> {
        self.withdraw_min(name, shares, 0)
    }

    /// `withdraw(shares, min_tokens, 0)`.
    pub fn withdraw_min(
        &mut self,
        name: &str,
        shares: u128,
        min_tokens: u64,
    ) -> Result<(u64, u64), u32> {
        self.withdraw_bounded(name, shares, min_tokens, 0)
    }

    /// `withdraw(shares, min_tokens, min_sol)`. Besides the math crate's refusals the program
    /// refuses a withdrawal that would leave `S = 0` while tokens are borrowed
    /// (`LastSharesBorrowed`, ARCH §9 M0b note 4) and one whose SOL part is below `min_sol`
    /// (`SolBelowMin`); both are expected here from the math crate's result.
    pub fn withdraw_bounded(
        &mut self,
        name: &str,
        shares: u128,
        min_tokens: u64,
        min_sol: u64,
    ) -> Result<(u64, u64), u32> {
        self.catch_up(name);
        let (kp, lk) = self.keys(name);
        let st = self.state();
        let m = self.market();
        let lock_grace_s = self.env.config().params.lock_grace_s;
        let from = st.lender_of(&lk.owner).map_or(m.share_epoch, |l| l.epoch);
        let metas = self.record_metas(from, MAX_RECORDS_PER_TX);
        let expect = (|| -> Result<Expect, MathError> {
            let (mut p, mut v, now) = Self::prologue(&st, &m);
            let mut l = st
                .lender_of(&lk.owner)
                .expect("withdraw needs a lender")
                .lender();
            let mut recs = Self::records_slice(&st, from, MAX_RECORDS_PER_TX);
            let tokens =
                pool::withdraw(&mut p, &mut v, &mut l, &mut recs, now, lock_grace_s, shares)?;
            let touched = l;
            let pay = pool::claim(&p, &mut v, &mut l, &mut recs, now)?;
            let closed = pool::closable(&p, &l);
            Ok(Expect {
                pool: p,
                vault: v,
                lender: (!closed).then_some(l),
                result: u128::from(tokens),
                pay,
                pot_draw: l.real - touched.real,
                records: recs,
            })
        })();
        let pv = self.pool_vault_amount();
        let tok = self.tokens_of(name);
        let lam = self.lamports_of(name);
        let vault_lam = read::lamports(&self.env.svm, &self.k.market_vault);
        let lender_key = pda::lender(&self.k.market, &lk.owner).0;
        let lender_rent = read::lamports(&self.env.svm, &lender_key);
        // The program's own checks after `pool::withdraw` (in the handler's order).
        let program_refusal = expect.as_ref().ok().and_then(|e| {
            if e.result < u128::from(min_tokens) {
                Some(code(ShortwinError::TokensBelowMin))
            } else if e.pool.total_shares == 0 && e.pool.borrowed_q > 0 {
                Some(code(ShortwinError::LastSharesBorrowed))
            } else if e.pay < min_sol {
                Some(code(ShortwinError::SolBelowMin))
            } else {
                None
            }
        });
        let ix = ix::with_remaining(
            ix::withdraw_with(&lk, shares, min_tokens, min_sol, false),
            &metas,
        );
        match self.run("withdraw", ix, &kp) {
            Ok(meta) => {
                let e = expect.unwrap_or_else(|err| {
                    panic!("withdraw: program accepted, math crate refuses: {err:?}")
                });
                assert_eq!(
                    program_refusal, None,
                    "withdraw: the program accepted what its own checks refuse"
                );
                let tokens = u64::try_from(e.result).expect("u64");
                assert!(tokens >= min_tokens);
                assert!(e.pay >= min_sol);
                self.compare("withdraw", &lk.owner, &e);
                assert_eq!(self.pool_vault_amount(), pv - tokens);
                assert_eq!(self.tokens_of(name), tok + tokens);
                assert_eq!(
                    read::lamports(&self.env.svm, &self.k.market_vault),
                    vault_lam - e.pay
                );
                let refund = if e.lender.is_none() { lender_rent } else { 0 };
                assert_eq!(self.lamports_of(name), lam + e.pay + refund);
                let ev = events::one::<shortwin::events::Withdraw>(&meta);
                assert_eq!(
                    (ev.amount, ev.shares, ev.sol_paid, ev.pot_draw),
                    (tokens, shares, e.pay, e.pot_draw)
                );
                Ok((tokens, e.pay))
            }
            Err(c) => {
                match program_refusal {
                    Some(p) => assert_eq!(c, p, "withdraw: program error {c}, expected {p}"),
                    None => Self::check_error("withdraw", c, expect.map(|_| ())),
                }
                Err(c)
            }
        }
    }

    /// `claim_sol()` by `name`: lamports paid (0 for a user without a position), or the
    /// program error code. A claim that leaves the position `pool::closable` closes it and
    /// returns its rent to the owner (KI-67), as `withdraw` does.
    pub fn claim(&mut self, name: &str) -> Result<u64, u32> {
        let owner = self.key(name);
        if self.env.lender(&self.k.market, &owner).is_none() {
            return Ok(0);
        }
        self.catch_up(name);
        let (kp, _) = self.keys(name);
        let st = self.state();
        let l0 = st.lender_of(&owner).expect("lender");
        let from = l0.epoch;
        let metas = self.record_metas(from, MAX_RECORDS_PER_TX);
        let expect = (|| -> Result<Expect, MathError> {
            let (p, mut v) = (st.pool, st.vault.clone());
            let mut l = l0.lender();
            let mut recs = Self::records_slice(&st, from, MAX_RECORDS_PER_TX);
            pool::touch(&p, &mut v, &mut l, &mut recs, st.now)?;
            let touched = l;
            let pay = pool::claim(&p, &mut v, &mut l, &mut recs, st.now)?;
            Ok(Expect {
                pool: p,
                vault: v,
                lender: (!pool::closable(&p, &l)).then_some(l),
                result: 0,
                pay,
                pot_draw: l.real - touched.real,
                records: recs,
            })
        })();
        let lam = self.lamports_of(name);
        let lender_rent = read::lamports(&self.env.svm, &pda::lender(&self.k.market, &owner).0);
        let ix = ix::with_remaining(ix::claim_sol(&owner, &self.k.market), &metas);
        match self.run("claim_sol", ix, &kp) {
            Ok(meta) => {
                let e = expect.unwrap_or_else(|err| {
                    panic!("claim_sol: program accepted, math crate refuses: {err:?}")
                });
                self.compare("claim_sol", &owner, &e);
                let refund = if e.lender.is_none() { lender_rent } else { 0 };
                assert_eq!(self.lamports_of(name), lam + e.pay + refund);
                let ev = events::one::<shortwin::events::ClaimSol>(&meta);
                assert_eq!((ev.sol_paid, ev.pot_draw), (e.pay, e.pot_draw));
                Ok(e.pay)
            }
            Err(c) => {
                Self::check_error("claim_sol", c, expect.map(|_| ()));
                Err(c)
            }
        }
    }

    /// `cover_deficit(lamports)` by `name`.
    pub fn cover(&mut self, name: &str, lamports: u64) -> Result<(), u32> {
        self.catch_up(name);
        let (kp, _) = self.keys(name);
        let owner = kp.pubkey();
        let st = self.state();
        let l0 = st.lender_of(&owner).expect("cover needs a lender");
        let from = l0.epoch;
        let metas = self.record_metas(from, MAX_RECORDS_PER_TX);
        let expect = (|| -> Result<Expect, MathError> {
            if lamports == 0 {
                return Err(MathError::DepositTooSmall); // ZeroAmount
            }
            let (p, mut v) = (st.pool, st.vault.clone());
            let mut l = l0.lender();
            let mut recs = Self::records_slice(&st, from, MAX_RECORDS_PER_TX);
            pool::cover(&p, &mut v, &mut l, &mut recs, st.now, lamports)?;
            Ok(Expect {
                pool: p,
                vault: v,
                lender: Some(l),
                result: 0,
                pay: 0,
                pot_draw: 0,
                records: recs,
            })
        })();
        let lam = self.lamports_of(name);
        let vault_lam = read::lamports(&self.env.svm, &self.k.market_vault);
        let ix = ix::with_remaining(ix::cover_deficit(&owner, &self.k.market, lamports), &metas);
        match self.run("cover_deficit", ix, &kp) {
            Ok(_) => {
                let e = expect.unwrap_or_else(|err| {
                    panic!("cover_deficit: program accepted, math crate refuses: {err:?}")
                });
                self.compare("cover_deficit", &owner, &e);
                assert_eq!(self.lamports_of(name), lam - lamports);
                assert_eq!(
                    read::lamports(&self.env.svm, &self.k.market_vault),
                    vault_lam + lamports
                );
                Ok(())
            }
            Err(c) => {
                Self::check_error("cover_deficit", c, expect.map(|_| ()));
                Err(c)
            }
        }
    }

    /// `sync_lender(max_epochs)` for `name` sent by a third party (the harness payer, without
    /// the owner's signature), with the next `max_epochs` records from the lender's epoch, or
    /// from its forward chain's `fwd_epoch` (KI-66 full; as the web passes them): the pot-only
    /// touch (`pool::touch_pot`, KI-66), which never converts through a rebase record
    /// (KI65-REV-SEC1, [`reads_rebase`]) but moves the surplus accrued up to it, and along the
    /// forward chain after it, to the pot.
    pub fn sync(&mut self, name: &str, max_epochs: u8) -> Result<Standing, u32> {
        self.sync_by(name, max_epochs, false, None)
    }

    /// A third party's `sync_lender(max_epochs)` of `name` with the records from epoch `from`
    /// (a slice starting before, at or after the chain's `fwd_epoch`; KI-66 full).
    #[allow(dead_code)] // ki66_sec_it's sliced walks; lender_it does not use it
    pub fn sync_from(&mut self, name: &str, from: u32, max_epochs: u8) -> Result<Standing, u32> {
        self.sync_by(name, max_epochs, false, Some(from))
    }

    /// `sync_lender(max_epochs)` signed by `name`, the position's owner: it may convert
    /// through rebase records.
    pub fn sync_signed(&mut self, name: &str, max_epochs: u8) -> Result<Standing, u32> {
        self.sync_by(name, max_epochs, true, None)
    }

    fn sync_by(
        &mut self,
        name: &str,
        max_epochs: u8,
        owner_signs: bool,
        from: Option<u32>,
    ) -> Result<Standing, u32> {
        let owner = self.key(name);
        let st = self.state();
        let l0 = st.lender_of(&owner).expect("sync needs a lender");
        let from = from.unwrap_or(if owner_signs || l0.fwd == pool::FWD_NONE {
            l0.epoch
        } else {
            l0.fwd_epoch
        });
        let n = usize::from(max_epochs);
        let metas = self.record_metas(from, n);
        let mut standing = Standing::Behind;
        let expect = (|| -> Result<Expect, MathError> {
            let (p, mut v) = (st.pool, st.vault.clone());
            let mut l = l0.lender();
            let mut recs = Self::records_slice(&st, from, n);
            let loaded = recs.clone();
            if owner_signs {
                standing = pool::convert(&p, &mut l, &mut recs)?;
                if standing != Standing::Behind {
                    pool::touch(&p, &mut v, &mut l, &mut recs, st.now)?;
                }
            } else {
                standing = pool::touch_pot(&p, &mut v, &mut l, &recs, st.now)?;
                assert!(
                    !reads_rebase(&p, &l0.lender(), &l, &loaded) && recs == loaded,
                    "the pot-only touch converted {name} through a rebase record"
                );
            }
            Ok(Expect {
                pool: p,
                vault: v,
                lender: Some(l),
                result: 0,
                pay: 0,
                pot_draw: 0,
                records: recs,
            })
        })();
        let (kp, _) = self.keys(name);
        let (ixn, signer) = if owner_signs {
            (
                ix::sync_lender_signed(&owner, &self.k.market, max_epochs),
                kp,
            )
        } else {
            let payer = self.env.payer.insecure_clone();
            (ix::sync_lender(&owner, &self.k.market, max_epochs), payer)
        };
        let ix = ix::with_remaining(ixn, &metas);
        match self.run("sync_lender", ix, &signer) {
            Ok(_) => {
                let e = expect.unwrap_or_else(|err| {
                    panic!("sync_lender: program accepted, math crate refuses: {err:?}")
                });
                self.compare("sync_lender", &owner, &e);
                Ok(standing)
            }
            Err(c) => {
                Self::check_error("sync_lender", c, expect.map(|_| ()));
                Err(c)
            }
        }
    }

    /// `pool_model._touch(who)`: a committed touch by the lender itself (`sync_lender` with
    /// every record needed, signed by the owner).
    pub fn touch(&mut self, name: &str) -> Option<LenderPosition> {
        let owner = self.key(name);
        self.env.lender(&self.k.market, &owner)?;
        self.catch_up(name);
        let steps = u8::try_from(MAX_RECORDS_PER_TX).expect("u8");
        self.sync_signed(name, steps).expect("touch");
        self.position(name)
    }

    /// `pool_model.free_allowance(who)`: the committed touch, then the allowance on the
    /// stored state (0 without a position).
    pub fn free_allowance(&mut self, name: &str) -> u64 {
        let Some(l) = self.touch(name) else { return 0 };
        let st = self.state();
        pool::allowance(&st.pool, &st.vault, &l.lender(), st.now).expect("allowance")
    }

    /// Shares that burn `allow` tokens: `⌊allow·(S + VS)/(T + ΣB_Q + VA)⌋`, capped at the
    /// lender's shares (the pool_model demos' `min(L.shares, a·(S + VS)//(T + B + VA))`).
    pub fn shares_for(&mut self, name: &str, allow: u64) -> u128 {
        let p = self.market().pool();
        let s = u128::from(allow) * (p.total_shares + pool::VIRTUAL_SHARES)
            / (u128::from(p.tokens_free) + u128::from(p.borrowed_q) + pool::VIRTUAL_ASSETS);
        s.min(self.position(name).map_or(0, |l| l.shares))
    }

    /// `pool_model.token_value`: the token claim of `name`'s stored shares on the current epoch.
    pub fn token_value(&mut self, name: &str) -> u128 {
        let p = self.market().pool();
        match self.position(name) {
            Some(l) if l.epoch == p.share_epoch => {
                pool::tokens_for_shares(&p, l.shares).expect("value")
            }
            _ => 0,
        }
    }

    // ---- injected borrower side ---------------------------------------------------------------

    /// Runs `f` on the stored pool state and writes the result back: the
    /// `Market` pool group (with `borrowed_q_by_tier` = everything in tier C and `open_positions`
    /// moved by `open_delta`), the `MarketVault` (its lamports follow `lender_sol +
    /// protocol_fees`), the live wipe record if `f` topped it up, `pool_vault`'s balance moved
    /// by `token_delta`. Returns `f`'s result; on an error nothing is written. The invariants
    /// run afterwards.
    pub fn inject<R>(
        &mut self,
        open_delta: i32,
        token_delta: i128,
        f: impl FnOnce(&mut Pool, &mut Vault, Option<&mut MathShareEpoch>) -> Result<R, MathError>,
    ) -> Result<R, MathError> {
        let r = self.inject_raw(open_delta, token_delta, f)?;
        self.env.assert_invariants();
        Ok(r)
    }

    /// [`Lab::inject`] without the invariant check (the caller writes more first).
    pub fn inject_raw<R>(
        &mut self,
        open_delta: i32,
        token_delta: i128,
        f: impl FnOnce(&mut Pool, &mut Vault, Option<&mut MathShareEpoch>) -> Result<R, MathError>,
    ) -> Result<R, MathError> {
        let mut m = self.market();
        let mut mv = self.vault();
        let mut p = m.pool();
        let mut v = mv.vault();
        let before = u128::from(v.lender_sol) + u128::from(v.protocol_fees);
        let mut rec = if p.is_wipe_live() && p.share_epoch > 0 {
            let key = pda::share_epoch(&self.k.market, p.share_epoch - 1).0;
            read::borsh::<ShareEpoch>(&self.env.svm, &key).map(|r| r.record())
        } else {
            None
        };
        let r = f(&mut p, &mut v, rec.as_mut())?;
        let after = u128::from(v.lender_sol) + u128::from(v.protocol_fees);
        m.set_pool(&p);
        m.borrowed_q_by_tier = [0, 0, p.borrowed_q, 0];
        m.open_positions = m
            .open_positions
            .checked_add_signed(open_delta)
            .expect("open_positions");
        mv.set_vault(&v);
        write_zc(&mut self.env, &self.k.market, &m, 0);
        write_zc(
            &mut self.env,
            &self.k.market_vault,
            &mv,
            after as i128 - before as i128,
        );
        if let Some(r) = rec {
            self.write_record(&r);
        }
        if token_delta != 0 {
            let mut acc = self
                .env
                .svm
                .get_account(&self.k.pool_vault)
                .expect("pool vault");
            let amount = u64::from_le_bytes(acc.data[64..72].try_into().expect("amount"));
            let amount =
                u64::try_from(i128::from(amount) + token_delta).expect("pool vault amount");
            acc.data[64..72].copy_from_slice(&amount.to_le_bytes());
            self.env
                .svm
                .set_account(self.k.pool_vault, acc)
                .expect("set pool vault");
        }
        Ok(r)
    }

    /// Writes a `ShareEpoch` record at its PDA.
    pub fn write_record(&mut self, r: &MathShareEpoch) {
        let (key, bump) = pda::share_epoch(&self.k.market, r.epoch);
        let mut acc = ShareEpoch {
            market: self.k.market,
            epoch: r.epoch,
            acc_end: 0,
            acc_r_end: 0,
            s_old: 0,
            carry: 0,
            carry_r: 0,
            conv_rem: 0,
            num: 0,
            den_log1000: 0,
            bump,
        };
        acc.set_record(r);
        fixtures::set_borsh(&mut self.env.svm, &key, &acc, ShareEpoch::SPACE, 0);
    }

    /// `pool_model.advance(dt, d_index)`: time passes, the vest releases, and the rent index
    /// grows by `d_index`, crediting `⌊(A·d_index + rem)/(WAD·10⁴)⌋` to lenders with the
    /// remainder carried (AIR-03; as the prologue of any `Market`-writing instruction at that
    /// time would).
    pub fn advance(&mut self, dt: i64, d_index: u128) {
        self.env.warp(dt);
        let credited = if d_index > 0 {
            self.index += d_index;
            let n = self.a * d_index + self.credit_rem;
            self.credit_rem = n % (WAD * BPS);
            u64::try_from(n / (WAD * BPS)).expect("credit")
        } else {
            0
        };
        let now = self.now();
        self.inject(0, 0, |p, v, _| pool::accrue(p, v, now, credited))
            .expect("advance");
    }

    /// `pool_model.accrue(d_index)` (no time passes).
    pub fn accrue(&mut self, d_index: u128) {
        self.advance(0, d_index);
    }

    /// `pool_model.borrow(pid, q, duration, tail)`: refused (after the committed vest release)
    /// above the utilisation cap, as the model does.
    pub fn borrow(&mut self, pid: &str, q: u64, duration: i64, tail: i64) -> Result<(), ()> {
        let now = self.now();
        let st = self.market().pool();
        let (t, b) = (u128::from(st.tokens_free), u128::from(st.borrowed_q));
        if q == 0
            || u128::from(q) > t
            || (b + u128::from(q)) * BPS > u128::from(self.util_cap_bps) * (t + b)
        {
            self.inject(0, 0, |p, _, _| pool::release(p, now))
                .expect("release");
            return Err(());
        }
        let release_at = now + duration + tail;
        self.inject(1, -i128::from(q), |p, _, _| {
            pool::borrow(p, now, q, release_at)
        })
        .expect("borrow");
        let p = self.rent_protocol_bps;
        self.positions.insert(
            pid.to_owned(),
            Pos {
                q,
                i_open: self.index,
                p,
            },
        );
        self.a += u128::from(q) * (BPS - u128::from(p));
        Ok(())
    }

    /// [`Lab::borrow`] with `pool_model`'s defaults (30 days, S3 tail 4,800 s).
    pub fn borrow30(&mut self, pid: &str, q: u64) -> Result<(), ()> {
        self.borrow(pid, q, 30 * D, 3_600 + 600 + 600)
    }

    fn take(&mut self, pid: &str) -> (Pos, u64) {
        let pos = self.positions.remove(pid).expect("position");
        self.a -= u128::from(pos.q) * (BPS - u128::from(pos.p));
        let credited = cdiv(
            u128::from(pos.q) * (self.index - pos.i_open) * (BPS - u128::from(pos.p)),
            WAD * BPS,
        );
        (pos, u64::try_from(credited).expect("credited"))
    }

    /// The rent a position owes at the index: `⌈q·(I − I_open)/WAD⌉`.
    pub fn owed(&self, pid: &str) -> u64 {
        let pos = self.positions[pid];
        u64::try_from(cdiv(u128::from(pos.q) * (self.index - pos.i_open), WAD)).expect("owed")
    }

    /// `pool_model.repay(pid, rent_paid, lender_lump)`: the tokens return; the lenders' part of
    /// the rent and the lump reach `MarketVault` (the protocol's part of the rent goes to
    /// `protocol_fees`, as the M2 exits will book it).
    pub fn repay(&mut self, pid: &str, rent_paid: u64, lump: u64) {
        let now = self.now();
        let lump_vest_s = self.lump_vest_s;
        let (pos, credited) = self.take(pid);
        let res = Resolution {
            q: pos.q,
            credited,
            lump,
        };
        self.inject(-1, i128::from(pos.q), |p, v, _| {
            let lender_rent = pool::repay(p, v, now, lump_vest_s, &res, rent_paid, pos.p)?;
            v.protocol_fees += rent_paid - lender_rent;
            Ok(())
        })
        .expect("repay");
    }

    /// `pool_model.settle(pid, sol_in, lump)`: the token debt is extinguished, `sol_in` reaches
    /// `MarketVault` (with the model's default rent, the position's [`Self::owed`], as `R_due`:
    /// its part above the credited rent vests, M10-8C-1); a rolled epoch's record is written.
    pub fn settle(&mut self, pid: &str, sol_in: u64, lump: u64) {
        let now = self.now();
        let lump_vest_s = self.lump_vest_s;
        let rent = self.owed(pid);
        let (pos, credited) = self.take(pid);
        let res = Resolution {
            q: pos.q,
            credited,
            lump,
        };
        let rec = self
            .inject_raw(-1, 0, |p, v, _| {
                pool::settle(p, v, now, lump_vest_s, &res, sol_in, rent).map(|s| s.rec)
            })
            .expect("settle");
        if let Some(r) = rec {
            self.write_record(&r);
        }
        self.env.assert_invariants();
    }

    /// `pool_model.sweep_sol(lamports)` (`sweep_trader`): a lump that vests, or tops up the live
    /// wipe record while `S = 0`.
    pub fn sweep_sol(&mut self, lamports: u64) {
        self.sweep_from(lamports, SweepSource::Trader);
    }

    /// A sweep from `source` (`claim_venue_rewards` for [`SweepSource::VenueRewards`]).
    pub fn sweep_from(&mut self, lamports: u64, source: SweepSource) {
        let now = self.now();
        let lump_vest_s = self.lump_vest_s;
        self.inject(0, 0, |p, v, rec| {
            pool::sweep(p, v, rec, now, lump_vest_s, lamports, source)
        })
        .expect("sweep");
    }

    /// The vest release alone (`pool_model._release`).
    pub fn release(&mut self) {
        let now = self.now();
        self.inject(0, 0, |p, _, _| pool::release(p, now))
            .expect("release");
    }

    /// Adds `lamports` of protocol fees (as an exit would) and the lamports behind them.
    pub fn add_protocol_fees(&mut self, lamports: u64) {
        self.inject(0, 0, |_, v, _| {
            v.protocol_fees += lamports;
            Ok(())
        })
        .expect("protocol fees");
    }

    /// Sets the market's venue (the M1 program reads it only for the post-wipe gate).
    pub fn set_venue(&mut self, venue: VenueKind) {
        let mut m = self.market();
        m.venue = venue as u8;
        write_zc(&mut self.env, &self.k.market, &m, 0);
    }

    // ---- trace API (`crates/shortwin-math/tests/vectors/pool`, gen_pool.py vocabulary) ----------

    /// `b Q RELEASE_AT`: `pool::borrow` with a unix `release_at`.
    pub fn borrow_raw(&mut self, q: u64, release_at: i64) -> Result<(), MathError> {
        let now = self.now();
        self.inject(1, -i128::from(q), |p, _, _| {
            pool::borrow(p, now, q, release_at)
        })
    }

    /// `x DT LAMPORTS`: time passes by `dt`, then the lender rent credit; refused as a whole
    /// (time included).
    pub fn accrue_raw(&mut self, dt: i64, lamports: u64) -> Result<(), MathError> {
        let clock = self.env.clock();
        self.env.warp(dt);
        let now = self.now();
        let r = self.inject(0, 0, |p, v, _| pool::accrue(p, v, now, lamports));
        if r.is_err() {
            self.env.set_clock(clock.unix_timestamp, clock.slot);
        }
        r
    }

    /// `r Q RENT P CREDITED LUMP`: `pool::repay` (the tokens return).
    pub fn repay_raw(
        &mut self,
        q: u64,
        rent_paid: u64,
        rent_protocol_bps: u32,
        credited: u64,
        lump: u64,
    ) -> Result<(), MathError> {
        let now = self.now();
        let lump_vest_s = self.lump_vest_s;
        let res = Resolution { q, credited, lump };
        self.inject(-1, i128::from(q), |p, v, _| {
            pool::repay(p, v, now, lump_vest_s, &res, rent_paid, rent_protocol_bps).map(|_| ())
        })
    }

    /// `s Q SOL CREDITED LUMP RENT`: `pool::settle` (a rolled epoch's record is written).
    pub fn settle_raw(
        &mut self,
        q: u64,
        sol_in: u64,
        credited: u64,
        lump: u64,
        rent: u64,
    ) -> Result<(), MathError> {
        let now = self.now();
        let lump_vest_s = self.lump_vest_s;
        let res = Resolution { q, credited, lump };
        let rec = self.inject_raw(-1, 0, |p, v, _| {
            pool::settle(p, v, now, lump_vest_s, &res, sol_in, rent).map(|s| s.rec)
        })?;
        if let Some(r) = rec {
            self.write_record(&r);
        }
        self.env.assert_invariants();
        Ok(())
    }

    /// `p LAMPORTS`: the trader sweep.
    pub fn sweep_raw(&mut self, lamports: u64) -> Result<(), MathError> {
        let now = self.now();
        let lump_vest_s = self.lump_vest_s;
        self.inject(0, 0, |p, v, rec| {
            pool::sweep(p, v, rec, now, lump_vest_s, lamports, SweepSource::Trader)
        })
    }

    /// `k`: `pool::cal_advance`.
    pub fn cal_advance_raw(&mut self) -> Result<(), MathError> {
        let now = self.now();
        self.inject(0, 0, |_, v, _| pool::cal_advance(v, now))
    }

    /// The model's committed first step of a refused `d` / `w` (release, then the lender's
    /// touch), which a reverted transaction does not keep: injected so that the stored state
    /// stays the model's.
    pub fn commit_pre(&mut self, name: &str) -> Result<(), MathError> {
        let now = self.now();
        let owner = self.key(name);
        let key = pda::lender(&self.k.market, &owner).0;
        let Some(mut pos) = read::borsh::<LenderPosition>(&self.env.svm, &key) else {
            return self.inject(0, 0, |p, _, _| pool::release(p, now).map(|_| ()));
        };
        let mut recs = self.state().records_from(pos.epoch).to_vec();
        let mut l = pos.lender();
        self.inject_raw(0, 0, |p, v, _| {
            pool::release(p, now)?;
            pool::touch(p, v, &mut l, &mut recs, now).map(|_| ())
        })?;
        pos.set_lender(&l);
        let lamports = read::lamports(&self.env.svm, &key);
        let mut acc = self.env.svm.get_account(&key).expect("lender");
        let mut data = Vec::with_capacity(LenderPosition::SPACE);
        anchor_lang::AccountSerialize::try_serialize(&pos, &mut data).expect("serialize");
        data.resize(LenderPosition::SPACE, 0);
        acc.data = data;
        acc.lamports = lamports;
        self.env.svm.set_account(key, acc).expect("set lender");
        // KI-65: the touch's conversion remainders, as the program would have written them.
        for r in &recs {
            self.write_record(r);
        }
        self.env.assert_invariants();
        Ok(())
    }

    /// A report line of the largest CU per instruction.
    pub fn cu_report(&self) -> String {
        self.cu
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Rewrites a zero-copy short.win account's body in place; its lamports move by `delta`.
pub fn write_zc<T: bytemuck::Pod>(env: &mut Env, key: &Pubkey, value: &T, delta: i128) {
    let mut acc = env.svm.get_account(key).expect("account");
    let n = core::mem::size_of::<T>();
    acc.data[8..8 + n].copy_from_slice(bytemuck::bytes_of(value));
    acc.lamports = u64::try_from(i128::from(acc.lamports) + delta).expect("lamports");
    env.svm.set_account(*key, acc).expect("set account");
}

/// Governance proposes `change`, the timelock passes, anyone applies it.
pub fn propose_and_apply(env: &mut Env, change: shortwin::state::ConfigChange) {
    let gov = env.governance.insecure_clone();
    shortwin_litesvm_tests::errors::ok(
        env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov]),
    );
    env.warp(i64::from(env.config().timelock_s));
    let apply = env.apply_config_ix();
    shortwin_litesvm_tests::errors::ok(env.send(&[apply], &[]));
}

/// A config whose `Params` differ from the current ones by `edit` (a `Params` change).
pub fn params_change(
    env: &Env,
    edit: impl FnOnce(&mut shortwin::state::Params),
) -> shortwin::state::ConfigChange {
    let c = env.config();
    let mut params = c.params;
    edit(&mut params);
    shortwin::state::ConfigChange::Params {
        params,
        tiers: c.tiers,
    }
}
