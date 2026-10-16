//! PumpSwap on the fork (M6): a canonical pool of the `pamm_state` dump (`fresh` with a running
//! boost, `mature` with third-party LP, `cashback`, `legacy261`), a trader, and a third-party
//! LP whose `deposit` / `withdraw` are the LP-wrapped bundles of M6R-1; `disable_flags` as the
//! venue's halt (bit 3 trading, bit 0 `create_pool`); since M10's program batch 4 the trader's
//! `buy_v2` / `sell_v2` and the sweeps of the 2026-10-02 redeploy ([`Kind::FeeBuckets`]), so the
//! hooks meet pools whose quote vault holds fee buckets (PUMP-2026-10-1: `R = min(vault −
//! buckets, vault + VQR)`, the tier's virtual part `max(VQR + buckets, 0)`).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin::constants::{
    GUARD_PUMP_AMM, GUARD_PUMP_FEES, PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG,
    VENUE_PROGRAM_DATA,
};
use shortwin_litesvm_tests::engine::VenueMarket;
use shortwin_litesvm_tests::fork::{self, PumpProgram, TierSpec};
use shortwin_litesvm_tests::pamm::{
    self, GC_DISABLE_FLAGS, PammCoin, PammLp, PammMarket, PammTrade, PammUser, pamm_trade_ix,
};
use shortwin_litesvm_tests::{Env, errors, read};
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::{Move, Venue, factor};
use crate::action::Kind;
use crate::rng::Rng;
use crate::stats::Ctx;
use crate::world::Slot;

/// The largest swap input the fuzzer funds (10⁷ SOL or 10¹⁷ raw tokens).
const MAX_SWAP: u64 = 10_000_000_000_000_000;

/// A PumpSwap market as the fuzzer drives it.
pub struct PammVenue {
    pub pm: PammMarket,
    pub user: PammUser,
    pub lp: PammLp,
    pub coin: String,
    /// PumpSwap's `FeeConfig` as a fee shock found it (restored at level 0).
    pub fee_saved: Option<FeeSpecs>,
}

/// A pump-fees `FeeConfig`'s fee lists as [`fork::fee_config_specs`] reads them: `(flat, tiers,
/// stable, exotic)`.
pub type FeeSpecs = ([u64; 3], Vec<TierSpec>, Vec<TierSpec>, [u64; 3]);

/// The protocol fee (bps) a fee shock of `level` writes into every list of a pump-fees
/// `FeeConfig`, the LP and creator parts kept: from none to ten times the hard cap's reach —
/// with the curve's 0 / 30 and PumpSwap's 20 / 5 bps around it, one-way fees from about 0.3 % to
/// 10.3 %, below `venue_fee_cap_bps` (150), between the caps and above `venue_fee_hard_cap_bps`
/// (500; [`crate::world::FEE_ABOVE_CAP`] ≡ 5 (mod 6) is the highest).
const PUMP_FEE_SHOCK_BPS: [u64; 6] = [0, 10, 50, 150, 300, 1_000];

/// A fee shock of the pump-fees `FeeConfig` at `key` (pump's `8Wf5Ti…` for the curve, PumpSwap's
/// `5PHirr…` for the pools; the admin's `update_fee_config` / `upsert_fee_tiers`, here written by
/// `fork::set_fee_config`): level 0 restores what `saved` holds, any other level saves the config
/// first (once) and either sets every tier's protocol fee to
/// [`PUMP_FEE_SHOCK_BPS`]`[level mod 6]` or — with the market caps `(at the spot, at P_ema)`
/// (PumpSwap) and `level / 6` odd — plants a **tier boundary**: midway between the two when the
/// spot is below `P_ema`, else at `(90 + level mod 9) %` of the spot's; the fees in effect at the
/// spot now below it, a third of the LP and creator parts and no protocol fee above it (M7-SEC-5's
/// shape: the live fee above the fee at `P_ema` the fee-scaled caps take, which then bind below
/// the impact and wallet caps — an open after the spot fell, or wrapped in a sale inside the open's
/// spot–EMA guard, meets it; `open_near_cap` probes the cap's edge).
pub fn pump_fee_shock(
    cx: &mut Ctx,
    key: &Pubkey,
    saved: &mut Option<FeeSpecs>,
    level: u32,
    mcaps: Option<(u128, u128)>,
) -> bool {
    if level == 0 {
        if let Some((flat, tiers, stable, exotic)) = saved.take() {
            fork::set_fee_config(&mut cx.env.svm, key, flat, &tiers, &stable, exotic);
        }
    } else {
        let (flat, tiers, stable, exotic) = saved
            .get_or_insert_with(|| fork::fee_config_specs(&cx.env.svm, key))
            .clone();
        match mcaps.filter(|_| (level / 6) % 2 == 1) {
            Some((cap, at_ema)) => {
                // The tier in effect at `cap`: the last whose threshold it reaches.
                let now = tiers
                    .iter()
                    .filter(|(t, _)| *t <= cap)
                    .max_by_key(|(t, _)| *t)
                    .map_or(flat, |(_, f)| *f);
                let above = [now[0] / 3, 0, now[2] / 3];
                let boundary = if cap < at_ema {
                    cap / 2 + at_ema / 2
                } else {
                    cap / 100 * u128::from(90 + level % 9)
                };
                fork::set_fee_config(
                    &mut cx.env.svm,
                    key,
                    flat,
                    &[(0, now), (boundary, above)],
                    &stable,
                    exotic,
                );
            }
            None => {
                let p = PUMP_FEE_SHOCK_BPS[(level % 6) as usize];
                let set = |f: [u64; 3]| [f[0], p, f[2]];
                let list =
                    |l: &[TierSpec]| l.iter().map(|(t, f)| (*t, set(*f))).collect::<Vec<_>>();
                fork::set_fee_config(
                    &mut cx.env.svm,
                    key,
                    set(flat),
                    &list(&tiers),
                    &list(&stable),
                    set(exotic),
                );
            }
        }
    }
    cx.env.assert_invariants();
    true
}

/// The pAMM `GlobalConfig.disable_flags` byte.
pub fn disable_flags(env: &Env) -> u8 {
    env.svm
        .get_account(&PUMP_AMM_GLOBAL_CONFIG)
        .map_or(0, |a| a.data[GC_DISABLE_FLAGS])
}

/// The live deploy slot of a venue program's ProgramData.
pub fn program_data_slot(env: &Env, index: u8) -> Option<u64> {
    let key = VENUE_PROGRAM_DATA.get(usize::from(index))?;
    let acc = env.svm.get_account(key)?;
    Some(shortwin_venue_state::bytes::u64_at(&acc.data, 4))
}

/// The pump program of a guard index.
pub fn pump_program(index: u8) -> PumpProgram {
    match index {
        0 => PumpProgram::Pump,
        1 => PumpProgram::Amm,
        _ => PumpProgram::Fees,
    }
}

impl PammVenue {
    /// The venue of `pm`: a trader and a third-party LP holding a real 5 % deposit.
    pub fn new(env: &mut Env, pm: PammMarket, coin: &str) -> Self {
        let user = PammUser::new(env, &pm.pool, 1_000_000_000_000_000);
        let lp = PammLp::funded_for(env, &pm.pool, 4_000);
        let minted = PammLp::lp_for(&env.svm, &pm.pool, 500);
        let kp = lp.kp.insecure_clone();
        // A pool whose deposits are disabled leaves the LP empty (its moves are then skipped).
        let _ = env.send(&[lp.deposit_ix(&pm.pool, minted)], &[&kp]);
        Self {
            pm,
            user,
            lp,
            coin: coin.to_string(),
            fee_saved: None,
        }
    }

    fn fund_user(&self, env: &mut Env, wsol: u64, base: u64) {
        if wsol > 0 {
            let have = read::token_amount(&env.svm, &self.user.wsol).unwrap_or(0);
            if have < wsol {
                fork::wrap(
                    env,
                    &self.user.wsol,
                    (wsol - have).saturating_add(wsol / 10),
                );
            }
        }
        if base > 0 {
            let have = read::token_amount(&env.svm, &self.user.base).unwrap_or(0);
            if have < base {
                fork::set_token_amount(
                    &mut env.svm,
                    &self.user.base,
                    base.saturating_add(base / 10),
                );
            }
        }
    }

    fn trade_ix(&self, env: &Env, kind: PammTrade, a: u64, b: u64) -> Instruction {
        pamm_trade_ix(
            &env.svm,
            &self.pm.pool,
            kind,
            &self.user.kp.pubkey(),
            &self.user.base,
            &self.user.wsol,
            a,
            b,
        )
    }

    fn pool_xy(&self, env: &Env) -> Option<(u64, u64)> {
        let r = self.pm.pool.reserves(&env.svm)?;
        let y = u64::try_from(r.effective_quote).ok()?;
        (r.base > 0 && y > 0).then_some((r.base, y))
    }

    /// [`Kind::FeeBuckets`]: the trader's `buy_v2` / `sell_v2` (the buckets grow by what the VQR
    /// falls), or both sweeps; a refusal (a cashback coin's v2 trade, a halt, nothing to sweep)
    /// is the venue's and changes nothing.
    fn fee_buckets(&mut self, cx: &mut Ctx, a: u32, b: u32) {
        let Some((x, y)) = self.pool_xy(cx.env) else {
            return;
        };
        if a % 8 == 7 {
            let payer = cx.env.funded_keypair();
            let coin_creator = Pubkey::new_from_array(self.pm.pool.state(&cx.env.svm).coin_creator);
            for (protocol, recipient) in [
                (true, pamm::protocol_fee_recipient(&cx.env.svm, 0)),
                (false, pamm::coin_creator_vault_authority(&coin_creator)),
            ] {
                let ix = pamm::pamm_sweep_ix(&self.pm.pool, protocol, &payer.pubkey(), &recipient);
                let tag = match cx.send(&[ix], &[&payer]) {
                    Ok(_) => "fuzz:fee_buckets-swept",
                    Err(_) => "fuzz:fee_buckets-sweep-refused",
                };
                *cx.stats.events.entry(tag.into()).or_default() += 1;
            }
            return;
        }
        let frac = 1 + u64::from(b % 500);
        let q = (u128::from(x) * u128::from(frac) / 10_000) as u64;
        if q == 0 {
            return;
        }
        let (kind, bound) = if a.is_multiple_of(2) {
            let cost = (u128::from(y) * u128::from(frac) * 2 / 10_000) as u64;
            self.fund_user(cx.env, cost.clamp(1, MAX_SWAP), 0);
            (PammTrade::Buy, u64::MAX)
        } else {
            self.fund_user(cx.env, 0, q);
            (PammTrade::Sell, 0)
        };
        let ix = pamm::pamm_trade_v2_ix(
            &cx.env.svm,
            &self.pm.pool,
            kind,
            &self.user.kp.pubkey(),
            &self.user.base,
            &self.user.wsol,
            q,
            bound,
        );
        let signer = self.user.kp.insecure_clone();
        let tag = match cx.send_cu(Some(pamm::PAMM_TRADE_CU), &[ix], &[&signer]) {
            Ok(_) if pamm::pool_buckets(&cx.env.svm, &self.pm.pool.pool) != (0, 0) => {
                "fuzz:fee_buckets-v2-landed"
            }
            Ok(_) => "fuzz:fee_buckets-v2-landed-empty",
            Err(_) => "fuzz:fee_buckets-v2-refused",
        };
        *cx.stats.events.entry(tag.into()).or_default() += 1;
    }
}

impl Venue for PammVenue {
    fn label(&self) -> String {
        format!("pumpswap({})", self.coin)
    }

    fn vm(&self) -> &dyn VenueMarket {
        &self.pm
    }

    fn seed_lender(&self) -> (Keypair, Pubkey) {
        (self.pm.lender.insecure_clone(), self.pm.lender_account)
    }

    fn fund_tokens(&mut self, env: &mut Env, owner: &Pubkey, amount: u64) -> Pubkey {
        let program = self.pm.pool.token_program();
        fork::fund_token(env, owner, &self.pm.pool.mint, program, amount)
    }

    fn top_up(&mut self, env: &mut Env, account: &Pubkey, amount: u64) {
        let have = read::token_amount(&env.svm, account).unwrap_or(0);
        fork::set_token_amount(&mut env.svm, account, have.saturating_add(amount));
    }

    fn reserves(&self, env: &Env) -> Option<(u64, u64)> {
        self.pool_xy(env)
    }

    fn push(&mut self, env: &mut Env, up: bool, k_ppm: u64) -> Option<Move> {
        let (x, y) = self.pool_xy(env)?;
        let k = factor(k_ppm).sqrt();
        let signer = self.user.kp.insecure_clone();
        if up {
            let out = (x as f64 * (1.0 - 1.0 / k)) as u64;
            if out == 0 || out >= x {
                return None;
            }
            let cost = ((y as f64) * (k - 1.0) * 1.1) as u64;
            if cost > MAX_SWAP {
                return None;
            }
            self.fund_user(env, cost.max(1), out);
            Some(Move {
                front: self.trade_ix(env, PammTrade::Buy, out, u64::MAX),
                back: Some(self.trade_ix(env, PammTrade::Sell, out, 0)),
                signer,
            })
        } else {
            let inp = (x as f64 * (k - 1.0) * 1.02) as u64;
            if inp == 0 || inp > MAX_SWAP {
                return None;
            }
            self.fund_user(env, y.saturating_mul(2).min(MAX_SWAP), inp);
            Some(Move {
                front: self.trade_ix(env, PammTrade::Sell, inp, 0),
                back: Some(self.trade_ix(env, PammTrade::Buy, inp, u64::MAX)),
                signer,
            })
        }
    }

    fn trade(&mut self, env: &mut Env, buy: bool, size: u64) -> Option<Move> {
        let (x, y) = self.pool_xy(env)?;
        let signer = self.user.kp.insecure_clone();
        if buy {
            let out =
                (u128::from(x) * u128::from(size) / u128::from(y.saturating_add(size))) as u64;
            if out == 0 {
                return None;
            }
            self.fund_user(env, size.saturating_mul(2).min(MAX_SWAP), 0);
            Some(Move {
                front: self.trade_ix(env, PammTrade::Buy, out, u64::MAX),
                back: None,
                signer,
            })
        } else {
            self.fund_user(env, 0, size.min(MAX_SWAP));
            Some(Move {
                front: self.trade_ix(env, PammTrade::Sell, size.min(MAX_SWAP), 0),
                back: None,
                signer,
            })
        }
    }

    fn lp_pair(&mut self, env: &mut Env, bps: u32) -> Option<(Instruction, Instruction, Keypair)> {
        let have = self.lp.lp_balance(&env.svm);
        let lp = PammLp::lp_for(&env.svm, &self.pm.pool, u64::from(bps)).min(have);
        if lp == 0 {
            return None;
        }
        // Funds for the re-deposit (the pool may have moved since the LP was funded).
        let r = self.pm.pool.reserves(&env.svm)?;
        let base_need = r.base.saturating_mul(u64::from(bps)) / 5_000 + 1;
        let sol_need = r.real_quote.saturating_mul(u64::from(bps)) / 5_000 + 1;
        let hb = read::token_amount(&env.svm, &self.lp.base).unwrap_or(0);
        if hb < base_need {
            fork::set_token_amount(&mut env.svm, &self.lp.base, base_need);
        }
        let hw = read::token_amount(&env.svm, &self.lp.wsol).unwrap_or(0);
        if hw < sol_need {
            fork::wrap(env, &self.lp.wsol, sol_need - hw);
        }
        Some((
            self.lp.withdraw_ix(&self.pm.pool, lp),
            self.lp.deposit_ix(&self.pm.pool, lp),
            self.lp.kp.insecure_clone(),
        ))
    }

    fn halt(&mut self, cx: &mut Ctx, level: u32) -> bool {
        // Bits 0..3 of `disable_flags` (bit 3 halts trading; bit 0 halts `create_pool`).
        let flags = (level % 16) as u8;
        pamm::set_disable_flags(&mut cx.env.svm, flags);
        cx.env.assert_invariants();
        true
    }

    fn fee_shock(&mut self, cx: &mut Ctx, level: u32) -> bool {
        // The pool's market cap as pump-fees tiers it, `effective_quote·supply/base`, and at the
        // market's `P_ema`, `P_ema·supply/WAD` (the fee the caps take, M7-SEC-5).
        let p_ema = cx.env.market(&self.pm.market()).ema_price.get();
        let mcaps = self.pm.pool.reserves(&cx.env.svm).and_then(|r| {
            let a = cx.env.svm.get_account(&self.pm.pool.mint)?;
            let supply = shortwin_venue_state::mint::read_base(&a.owner.to_bytes(), &a.data)
                .ok()?
                .supply;
            let spot = r.effective_quote * u128::from(supply) / u128::from(r.base.max(1));
            let at_ema = p_ema.checked_mul(u128::from(supply))? / shortwin_math::num::WAD;
            (r.base > 0).then_some((spot, at_ema))
        });
        pump_fee_shock(cx, &PUMP_AMM_FEE_CONFIG, &mut self.fee_saved, level, mcaps)
    }

    fn special(&mut self, cx: &mut Ctx, kind: Kind, a: u32, b: u32) -> Option<()> {
        match kind {
            Kind::GraduationHalt => {
                let f = disable_flags(cx.env);
                let f = if a % 2 == 1 { f | 1 } else { f & !1 };
                pamm::set_disable_flags(&mut cx.env.svm, f);
                cx.env.assert_invariants();
                Some(())
            }
            Kind::FeeBuckets => {
                self.fee_buckets(cx, a, b);
                Some(())
            }
            _ => None,
        }
    }

    fn sweep_ix(&self, env: &Env) -> Instruction {
        pamm::sweep_trader_pamm_ix(env, &self.pm)
    }

    fn donation_targets(&self) -> Vec<Pubkey> {
        vec![self.pm.pool.boost_vault_ata]
    }

    fn exit_cu(&self) -> Option<u32> {
        Some(400_000)
    }

    fn guard_programs(&self) -> Vec<u8> {
        vec![GUARD_PUMP_AMM, GUARD_PUMP_FEES]
    }

    fn set_deploy_slot(&mut self, env: &mut Env, index: u8, slot: u64) {
        fork::set_pump_deploy_slot(&mut env.svm, pump_program(index), slot);
    }

    fn deploy_slot(&self, env: &Env, index: u8) -> Option<u64> {
        program_data_slot(env, index)
    }

    fn restore(&mut self, cx: &mut Ctx) {
        pamm::set_disable_flags(&mut cx.env.svm, 0);
        self.fee_shock(cx, 0);
    }

    fn disturbed(&self, env: &Env) -> bool {
        disable_flags(env) != 0 || self.fee_saved.is_some()
    }
}

/// The PumpSwap profile: a random pAMM coin listed on the fork, `40·q` lent (as `pamm_it`), a
/// fresh oracle three times in four.
pub fn setup(rng: &mut Rng) -> (Env, Vec<Slot>) {
    let coin = [
        PammCoin::Fresh,
        PammCoin::Mature,
        PammCoin::Cashback,
        PammCoin::Legacy261,
    ][rng.below(4) as usize];
    let (mut env, p) = Env::fork_pamm_coin_with(coin, crate::setup::tweak_args(rng));
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let q = (base / 100).min(base * 10_000_000_000 / r.effective_quote.max(1)) as u64;
    let curve = shortwin_litesvm_tests::pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < shortwin_venue_state::pump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        errors::ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    errors::ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 60);
    if rng.chance(3, 4) {
        env.fresh_oracle();
    }
    let v = PammVenue::new(&mut env, pm, coin.role());
    let slot = crate::setup::slot(&mut env, Box::new(v), q.saturating_mul(10));
    (env, vec![slot])
}
