//! Raydium on the fork (M3 / M4): Fartcoin's AMM v4 pool or a CPMM pool (USELESS, the
//! Token-2022 pool, the creator-fee pool), a trader, a CPMM liquidity provider (the dumped
//! program's own `deposit` / `withdraw`, M4F-1), the admins' halts (AMM v4 `status`, CPMM status
//! bits) and fee changes (AMM v4 swap fee, CPMM `AmmConfig` rates), and **pool selection**:
//! planted candidate pools of the market's token (AMM v4 or CPMM, deeper or thinner, any locked
//! share), `propose_pool` and `execute_pool_switch` (M4, "switch only if all fit").

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin::constants::{GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4};
use shortwin_litesvm_tests::engine::{VenueMarket, sweep_trader_ix_for};
use shortwin_litesvm_tests::fork::{
    self, AnyPool, CpmmFixture, RaydiumProgram, Swap, Trader, V4Fixture,
};
use shortwin_litesvm_tests::lp::{self, Provider};
use shortwin_litesvm_tests::pool_select::{self, PlantSpec};
use shortwin_litesvm_tests::raydium::{RayFixture, RayMarket, RaySpec, candidate_read_set};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{Env, errors, read};
use shortwin_math::fees::Direction;
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::pswap::program_data_slot;
use super::{Move, Venue, factor};
use crate::action::{Kind, Profile};
use crate::rng::Rng;
use crate::stats::Ctx;
use crate::world::Slot;

/// The largest swap input the fuzzer funds (10⁷ SOL or 10¹⁷ raw tokens).
const MAX_SWAP: u64 = 10_000_000_000_000_000;
/// Planted candidates kept at once.
const MAX_CANDIDATES: usize = 4;

/// A Raydium market as the fuzzer drives it.
pub struct RayVenue {
    pub rm: RayMarket,
    pub trader: Trader,
    /// The CPMM provider of the market's original pool (with a real deposit).
    pub lp: Option<(Provider, fork::CpmmPool)>,
    /// Planted candidate pools (and pools the market left).
    pub candidates: Vec<AnyPool>,
    /// The pools' original status and fees (restored by `restore`).
    v4_status: Option<u64>,
    v4_fee: Option<(u64, u64)>,
    cpmm_status: Option<u8>,
    cpmm_rates: Option<(Pubkey, [u64; 4])>,
}

impl RayVenue {
    fn mint(&self) -> (Pubkey, TokenProgram) {
        self.rm.pool.token()
    }

    fn fund(&self, env: &mut Env, wsol: u64, tokens: u64) {
        if wsol > 0 {
            let have = read::token_amount(&env.svm, &self.trader.wsol).unwrap_or(0);
            if have < wsol {
                fork::wrap(
                    env,
                    &self.trader.wsol,
                    (wsol - have).saturating_add(wsol / 10),
                );
            }
        }
        if tokens > 0 {
            let have = read::token_amount(&env.svm, &self.trader.token).unwrap_or(0);
            if have < tokens {
                fork::set_token_amount(
                    &mut env.svm,
                    &self.trader.token,
                    tokens.saturating_add(tokens / 10),
                );
            }
        }
    }

    fn swap(&self, env: &Env, dir: Direction, s: Swap) -> Instruction {
        self.rm
            .pool
            .swap_ix(&env.svm, &self.trader.owner.pubkey(), &self.trader, dir, s)
    }

    /// Follows an adoption: the market's cached pool is a candidate now.
    fn sync(&mut self, env: &Env) {
        let market = self.rm.market();
        let cached = pool_select::cached_pool(&env.svm, &market);
        let cur = pool_select::pool_key(&self.rm.pool);
        if cached == cur {
            return;
        }
        if let Some(i) = self
            .candidates
            .iter()
            .position(|c| pool_select::pool_key(c) == cached)
        {
            let next = self.candidates.remove(i);
            let old = self.rm.pool;
            self.rm = self.rm.on_pool(&env.svm, next);
            self.candidates.push(old);
        }
    }
}

impl Venue for RayVenue {
    fn label(&self) -> String {
        format!("raydium({:?})", self.rm.fixture)
    }

    fn vm(&self) -> &dyn VenueMarket {
        &self.rm
    }

    fn as_ray(&self) -> Option<&RayMarket> {
        Some(&self.rm)
    }

    fn seed_lender(&self) -> (Keypair, Pubkey) {
        (self.rm.lender.insecure_clone(), self.rm.lender_account)
    }

    fn fund_tokens(&mut self, env: &mut Env, owner: &Pubkey, amount: u64) -> Pubkey {
        let (mint, program) = self.mint();
        fork::fund_token(env, owner, &mint, program, amount)
    }

    fn top_up(&mut self, env: &mut Env, account: &Pubkey, amount: u64) {
        let have = read::token_amount(&env.svm, account).unwrap_or(0);
        fork::set_token_amount(&mut env.svm, account, have.saturating_add(amount));
    }

    fn reserves(&self, env: &Env) -> Option<(u64, u64)> {
        let (x, y) = self.rm.reserves(&env.svm)?;
        (x > 0 && y > 0).then_some((x, y))
    }

    fn push(&mut self, env: &mut Env, up: bool, k_ppm: u64) -> Option<Move> {
        let (x, y) = self.reserves(env)?;
        let k = factor(k_ppm).sqrt();
        let signer = self.trader.owner.insecure_clone();
        if up {
            let out = (x as f64 * (1.0 - 1.0 / k)) as u64;
            let cost = (y as f64 * (k - 1.0) * 1.1) as u64;
            if out == 0 || out >= x || cost > MAX_SWAP {
                return None;
            }
            self.fund(env, cost.max(1), 0);
            Some(Move {
                front: self.swap(env, Direction::Buy, Swap::ExactOut(u64::MAX, out)),
                back: Some(self.swap(env, Direction::Sell, Swap::ExactIn(out, 0))),
                signer,
            })
        } else {
            let inp = (x as f64 * (k - 1.0) * 1.02) as u64;
            if inp == 0 || inp > MAX_SWAP {
                return None;
            }
            self.fund(env, y.saturating_mul(2).min(MAX_SWAP), inp);
            Some(Move {
                front: self.swap(env, Direction::Sell, Swap::ExactIn(inp, 0)),
                back: Some(self.swap(env, Direction::Buy, Swap::ExactOut(u64::MAX, inp))),
                signer,
            })
        }
    }

    fn trade(&mut self, env: &mut Env, buy: bool, size: u64) -> Option<Move> {
        let signer = self.trader.owner.insecure_clone();
        let size = size.min(MAX_SWAP);
        if buy {
            self.fund(env, size, 0);
            Some(Move {
                front: self.swap(env, Direction::Buy, Swap::ExactIn(size, 0)),
                back: None,
                signer,
            })
        } else {
            self.fund(env, 0, size);
            Some(Move {
                front: self.swap(env, Direction::Sell, Swap::ExactIn(size, 0)),
                back: None,
                signer,
            })
        }
    }

    fn lp_pair(&mut self, env: &mut Env, bps: u32) -> Option<(Instruction, Instruction, Keypair)> {
        let (prov, pool) = self.lp.as_ref()?;
        let counter = pool.state(&env.svm).lp_supply;
        let have = read::token_amount(&env.svm, &prov.lp).unwrap_or(0);
        let amount = (u128::from(counter) * u128::from(bps) / 10_000) as u64;
        let amount = amount.min(have);
        if amount == 0 {
            return None;
        }
        let (sol, tokens) = lp::lp_needed(env, pool, amount);
        let (w, t) = if pool.sol_is_token0 {
            (prov.t0, prov.t1)
        } else {
            (prov.t1, prov.t0)
        };
        let hw = read::token_amount(&env.svm, &w).unwrap_or(0);
        if hw < sol {
            fork::wrap(env, &w, sol - hw);
        }
        let ht = read::token_amount(&env.svm, &t).unwrap_or(0);
        if ht < tokens {
            fork::set_token_amount(&mut env.svm, &t, tokens);
        }
        Some((
            lp::withdraw_ix(pool, prov, amount),
            lp::deposit_ix(pool, prov, amount),
            prov.owner.insecure_clone(),
        ))
    }

    fn halt(&mut self, cx: &mut Ctx, level: u32) -> bool {
        match self.rm.pool {
            AnyPool::V4(p) => {
                let orig = self.v4_status.unwrap_or(6);
                let s = if level == 0 {
                    orig
                } else {
                    let v = 1 + u64::from(level % 7);
                    if v == orig { 1 } else { v }
                };
                fork::set_v4_status(&mut cx.env.svm, &p, s);
            }
            AnyPool::Cpmm(p) => {
                let orig = self.cpmm_status.unwrap_or(0);
                let s = if level == 0 {
                    orig
                } else {
                    1 + (level % 7) as u8
                };
                fork::set_cpmm_status(&mut cx.env.svm, &p, s);
            }
        }
        cx.env.assert_invariants();
        true
    }

    fn fee_shock(&mut self, cx: &mut Ctx, level: u32) -> bool {
        match self.rm.pool {
            AnyPool::V4(p) => {
                let (n, d) = if level == 0 {
                    self.v4_fee.unwrap_or((25, 10_000))
                } else {
                    ([0, 10, 25, 100, 300, 1_000][(level % 6) as usize], 10_000)
                };
                fork::set_v4_swap_fee(&mut cx.env.svm, &p, n, d);
            }
            AnyPool::Cpmm(p) => {
                let cfg = p.config_key(&cx.env.svm);
                let orig = match self.cpmm_rates {
                    Some((k, r)) if k == cfg => r,
                    _ => {
                        let c = p.config(&cx.env.svm);
                        [
                            c.trade_fee_rate,
                            c.protocol_fee_rate,
                            c.fund_fee_rate,
                            c.creator_fee_rate,
                        ]
                    }
                };
                let trade = if level == 0 {
                    orig[0]
                } else {
                    [0, 1_000, 2_500, 10_000, 30_000, 100_000][(level % 6) as usize]
                };
                fork::set_cpmm_config_rates(
                    &mut cx.env.svm,
                    &cfg,
                    trade,
                    orig[1],
                    orig[2],
                    orig[3],
                );
            }
        }
        cx.env.assert_invariants();
        true
    }

    fn special(&mut self, cx: &mut Ctx, kind: Kind, a: u32, b: u32) -> Option<()> {
        let (mint, program) = self.mint();
        match kind {
            Kind::PlantCandidate => {
                let (num, den) = [(3, 1), (1, 1), (1, 3), (2, 1), (1, 10)][a as usize % 5];
                let lambda = [10_000, 6_000, 3_000, 1_000][(a / 5) as usize % 4];
                let v4 = b.is_multiple_of(2) && program == TokenProgram::Spl;
                let sol_first = (b / 2).is_multiple_of(2);
                let spec: PlantSpec =
                    pool_select::spec_like(&cx.env.svm, &self.rm.pool, sol_first, lambda)
                        .scaled(num, den);
                if spec.sol == 0 || spec.tokens == 0 {
                    return None;
                }
                let pool = if v4 {
                    AnyPool::V4(pool_select::plant_v4(cx.env, V4Fixture::Fartcoin, spec))
                } else {
                    AnyPool::Cpmm(pool_select::plant_cpmm(cx.env, CpmmFixture::Useless, spec))
                };
                cx.env.assert_invariants();
                if self.candidates.len() >= MAX_CANDIDATES {
                    self.candidates.remove(0);
                }
                self.candidates.push(pool);
                Some(())
            }
            Kind::ProposePool => {
                if self.candidates.is_empty() {
                    return None;
                }
                let n = self.candidates.len();
                let cand = self.candidates[if a == u32::MAX { n - 1 } else { a as usize % n }];
                let prefix = candidate_read_set(&cx.env.svm, &self.rm.pool, false);
                let ix = pool_select::propose_pool_ix(&cx.env.svm, &mint, &prefix, &cand);
                cx.send(&[ix], &[]);
                self.sync(cx.env);
                Some(())
            }
            Kind::ExecuteSwitch => {
                if self.candidates.is_empty() {
                    return None;
                }
                let m = cx.env.market(&self.rm.market());
                let window = m.fp_keys[0];
                let cand = self
                    .candidates
                    .iter()
                    .copied()
                    .find(|c| pool_select::pool_key(c) == window)
                    .unwrap_or_else(|| {
                        let n = self.candidates.len();
                        self.candidates[if a == u32::MAX { n - 1 } else { a as usize % n }]
                    });
                let ix =
                    pool_select::execute_pool_switch_ix(&cx.env.svm, &mint, &self.rm.pool, &cand);
                cx.send(&[ix], &[]);
                self.sync(cx.env);
                Some(())
            }
            _ => None,
        }
    }

    fn after_crank(&mut self, cx: &mut Ctx) {
        self.sync(cx.env);
    }

    fn sweep_ix(&self, env: &Env) -> Instruction {
        sweep_trader_ix_for(env, &self.rm.market())
    }

    fn guard_programs(&self) -> Vec<u8> {
        vec![GUARD_RAYDIUM_V4, GUARD_RAYDIUM_CPMM]
    }

    fn set_deploy_slot(&mut self, env: &mut Env, index: u8, slot: u64) {
        let p = if index == GUARD_RAYDIUM_V4 {
            RaydiumProgram::AmmV4
        } else {
            RaydiumProgram::Cpmm
        };
        fork::set_deploy_slot(&mut env.svm, p, slot);
    }

    fn deploy_slot(&self, env: &Env, index: u8) -> Option<u64> {
        program_data_slot(env, index)
    }

    fn restore(&mut self, cx: &mut Ctx) {
        self.halt(cx, 0);
        self.fee_shock(cx, 0);
    }

    fn disturbed(&self, env: &Env) -> bool {
        match self.rm.pool {
            AnyPool::V4(p) => {
                let i = p.info(&env.svm);
                Some(i.status) != self.v4_status
                    || Some((i.swap_fee_numerator, i.swap_fee_denominator)) != self.v4_fee
            }
            AnyPool::Cpmm(p) => Some(p.state(&env.svm).status) != self.cpmm_status,
        }
    }
}

/// The Raydium profiles: Fartcoin on AMM v4, or a random CPMM fixture; `40·Q` lent (as
/// `ray_it`), the listing window waited out; a fresh oracle three times in four.
pub fn setup(profile: Profile, rng: &mut Rng) -> (Env, Vec<Slot>) {
    let mut env = crate::setup::release_env(rng);
    env.load_raydium_programs();
    env.set_fork_clock();
    let fixture = match profile {
        Profile::AmmV4 => RayFixture::V4(V4Fixture::Fartcoin),
        _ => RayFixture::Cpmm(
            [
                CpmmFixture::Useless,
                CpmmFixture::Token2022,
                CpmmFixture::CreatorFee,
            ][rng.below(3) as usize],
        ),
    };
    let pool = env.load_ray_pool(fixture);
    errors::ok(env.list_ray(&pool, fixture.hint()));
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let q = (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    if rng.chance(3, 4) {
        env.fresh_oracle();
    }
    let (mint, program) = rm.pool.token();
    let trader = fork::trader(
        &mut env,
        &mint,
        program,
        1_000_000_000_000_000,
        x.saturating_mul(2),
    );
    let lp = match rm.pool {
        AnyPool::Cpmm(p) => {
            let (px, py) = (x, y);
            let prov = lp::provider(&mut env, &p, py, px, 0);
            let counter = p.state(&env.svm).lp_supply;
            let minted = counter / 20;
            let kp = prov.owner.insecure_clone();
            let _ = env.send(&[lp::deposit_ix(&p, &prov, minted)], &[&kp]);
            Some((prov, p))
        }
        AnyPool::V4(_) => None,
    };
    let (v4_status, v4_fee, cpmm_status, cpmm_rates) = match rm.pool {
        AnyPool::V4(p) => {
            let i = p.info(&env.svm);
            (
                Some(i.status),
                Some((i.swap_fee_numerator, i.swap_fee_denominator)),
                None,
                None,
            )
        }
        AnyPool::Cpmm(p) => {
            let c = p.config(&env.svm);
            (
                None,
                None,
                Some(p.state(&env.svm).status),
                Some((
                    p.config_key(&env.svm),
                    [
                        c.trade_fee_rate,
                        c.protocol_fee_rate,
                        c.fund_fee_rate,
                        c.creator_fee_rate,
                    ],
                )),
            )
        }
    };
    let v = RayVenue {
        rm,
        trader,
        lp,
        candidates: Vec::new(),
        v4_status,
        v4_fee,
        cpmm_status,
        cpmm_rates,
    };
    let slot = crate::setup::slot(&mut env, Box::new(v), q.saturating_mul(10));
    (env, vec![slot])
}
