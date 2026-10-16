//! The mock venue (`tests/mock_cpamm`, test-venue build): a constant-product pool with knobs for
//! every venue-side disturbance ARCH §8.4 names — an admin halt (`disabled`), fee changes, a
//! venue-state layout change (`layout_version`), over-delivery, and an interface change
//! (`required_extras`) — and a pool admin whose `remove_liquidity` / `add_liquidity` are the
//! third-party liquidity moves.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::state::{PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER};
use shortwin::venues::{
    EXTRAS_SET_CURRENT, EXTRAS_SET_NONE, EXTRAS_SET_PREVIOUS, EXTRAS_SET_STABLE, ExtrasSel,
};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::Env;
use shortwin_litesvm_tests::drive::{Actor, swap_in_ix};
use shortwin_litesvm_tests::engine::{
    MarketSpec, MockMarket, VENUE_HINT_MOCK, VenueMarket, mock_extras_key, set_venue_extras_ix,
    sweep_trader_ix_for,
};
use shortwin_litesvm_tests::mock::{self, Dir, Knobs};
use shortwin_litesvm_tests::{read, tokens};
use shortwin_math::num::WAD;
use solana_keypair::Keypair;
use solana_signer::Signer;

use super::{Move, Venue, factor};
use crate::action::Kind;
use crate::stats::Ctx;

/// Mock fee levels (ppm on the input) of a fee shock: level `n mod 7`; level 0 is the
/// market's own fee.
const FEES: [u32; 7] = [0, 0, 1_000, 10_000, 30_000, 60_000, 150_000];

/// The largest swap input the fuzzer funds (10⁷ SOL or 10¹⁷ raw tokens): beyond it a push is
/// skipped (the airdrop faucet and the mint supply stay far from `u64::MAX`).
const MAX_SWAP: u64 = 10_000_000_000_000_000;

/// A mock market as the fuzzer drives it.
pub struct MockVenue {
    pub mm: MockMarket,
    /// The swapper of every push, trade and bundle (topped up as needed).
    pub trader: Actor,
    /// The pool admin's token accounts for liquidity moves.
    lp_base: Pubkey,
    lp_quote: Pubkey,
    pub fee_ppm: u32,
    /// The program whose per-mint PDA `["extra", mint]` an interface change may require (the
    /// pump 2026-04-28 class, T-REPLAY-PDA), and that PDA with its bump.
    extra_prog: Pubkey,
    extra: (Pubkey, u8),
}

/// The mock's `set_venue_extras` legs (`ix`): the sell leg (opens) and the buy leg (closes and
/// the keeper's exits).
pub const EXTRAS_SELL: u8 = 0;
pub const EXTRAS_BUY: u8 = 1;

fn seed_const(b: &[u8]) -> SeedPartArg {
    let mut bytes = [0u8; 32];
    bytes[..b.len()].copy_from_slice(b);
    SeedPartArg {
        kind: SEED_CONST,
        len: b.len() as u8,
        bytes,
    }
}

fn seed_mint() -> SeedPartArg {
    SeedPartArg {
        kind: SEED_PLACEHOLDER,
        len: PLACEHOLDER_MINT,
        bytes: [0; 32],
    }
}

impl MockVenue {
    /// A mock market per `spec` in `env` with its trader and LP accounts.
    pub fn new(env: &mut Env, spec: MarketSpec) -> Self {
        let mm = env.mock_market(spec);
        let trader = env.actor(&mm, spec.base, spec.quote);
        let admin = mm.pool.admin.pubkey();
        let lp_base =
            mock::base_account(env, mm.mock(), &admin, &mm.pool.mint_authority, spec.base);
        let lp_quote = mock::wsol_account(env, &admin, spec.quote);
        let extra_prog = Pubkey::new_unique();
        let extra = Pubkey::find_program_address(&[b"extra", mm.mint().as_ref()], &extra_prog);
        Self {
            fee_ppm: spec.fee_ppm,
            mm,
            trader,
            lp_base,
            lp_quote,
            extra_prog,
            extra,
        }
    }

    /// The template that derives [`MockVenue::extra`] (a PDA of `extra_prog` from `"extra"`
    /// and the market's mint).
    fn template(&self) -> ExtraEntryArg {
        ExtraEntryArg {
            kind: 2,
            writable: false,
            key: self.extra_prog,
            seeds: vec![seed_const(b"extra"), seed_mint()],
        }
    }

    /// The Emergency's `set_venue_extras(Mock, leg, entries)`.
    fn set_extras(&self, cx: &mut Ctx, leg: u8, entries: Vec<ExtraEntryArg>) -> bool {
        let ea = cx.env.pause_authority.insecure_clone();
        let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, leg, entries);
        cx.send(&[ix], &[&ea]).is_ok()
    }

    fn reserves_now(&self, env: &Env) -> mock::Reserves {
        mock::reserves(&env.svm, self.mm.mock())
    }

    /// Makes sure the trader holds `need` of the input side of `dir`.
    fn fund_trader(&self, env: &mut Env, dir: Dir, need: u64) {
        let need = need.min(MAX_SWAP);
        match dir {
            Dir::BaseIn => {
                let have = read::token_amount(&env.svm, &self.trader.base).unwrap_or(0);
                if have < need {
                    env.fund_base(&self.mm, &self.trader, need - have + need / 10 + 1);
                }
            }
            Dir::QuoteIn => {
                let have = read::token_amount(&env.svm, &self.trader.wsol).unwrap_or(0);
                if have < need {
                    mock::wrap(env, &self.trader.wsol, need - have + need / 10 + 1);
                }
            }
        }
    }

    fn knobs(&self, env: &Env) -> Knobs {
        mock::knobs(&env.svm, &self.mm.mock().pool)
    }

    fn set_knobs(&self, cx: &mut Ctx, f: impl FnOnce(&mut Knobs)) -> bool {
        let mut k = self.knobs(cx.env);
        f(&mut k);
        let ix = mock::set_knobs_ix(&self.mm.mock().pool, &self.mm.pool.admin.pubkey(), &k);
        let admin = self.mm.pool.admin.insecure_clone();
        cx.send(&[ix], &[&admin]).is_ok()
    }

    fn swap_move(&self, env: &mut Env, dir: Dir, amount: u64) -> Option<Move> {
        if amount == 0 || amount > MAX_SWAP {
            return None;
        }
        self.fund_trader(env, dir, amount);
        let out = mock::quote_exact_in(&env.svm, self.mm.mock(), dir, amount).map(|q| q.proceeds);
        let back_dir = match dir {
            Dir::BaseIn => Dir::QuoteIn,
            Dir::QuoteIn => Dir::BaseIn,
        };
        let back = out.filter(|o| *o > 0).map(|o| {
            self.fund_trader(env, back_dir, o);
            swap_in_ix(&self.mm, &self.trader, back_dir, o, 0)
        });
        Some(Move {
            front: swap_in_ix(&self.mm, &self.trader, dir, amount, 0),
            back,
            signer: self.trader.kp.insecure_clone(),
        })
    }
}

impl Venue for MockVenue {
    fn label(&self) -> String {
        format!(
            "mock({:?}, fee {} ppm)",
            self.mm.spec.program, self.mm.spec.fee_ppm
        )
    }

    fn vm(&self) -> &dyn VenueMarket {
        &self.mm
    }

    fn as_mock(&self) -> Option<&MockMarket> {
        Some(&self.mm)
    }

    fn seed_lender(&self) -> (Keypair, Pubkey) {
        (self.mm.lender.insecure_clone(), self.mm.lender_account)
    }

    fn fund_tokens(&mut self, env: &mut Env, owner: &Pubkey, amount: u64) -> Pubkey {
        mock::base_account(
            env,
            self.mm.mock(),
            owner,
            &self.mm.pool.mint_authority,
            amount,
        )
    }

    fn top_up(&mut self, env: &mut Env, account: &Pubkey, amount: u64) {
        if amount == 0 {
            return;
        }
        let payer = env.payer.insecure_clone();
        tokens::mint_to(
            &mut env.svm,
            &payer,
            &self.mm.mint(),
            account,
            &self.mm.pool.mint_authority,
            amount,
            self.mm.spec.program,
        );
    }

    fn reserves(&self, env: &Env) -> Option<(u64, u64)> {
        let r = self.reserves_now(env);
        (r.base > 0 && r.quote > 0).then_some((r.base, r.quote))
    }

    fn push(&mut self, env: &mut Env, up: bool, k_ppm: u64) -> Option<Move> {
        let r = self.reserves_now(env);
        if r.base == 0 || r.quote == 0 {
            return None;
        }
        let k = factor(k_ppm);
        let spot = u128::from(r.quote) * WAD / u128::from(r.base);
        let target = if up {
            (spot as f64 * k) as u128
        } else {
            (spot as f64 / k) as u128
        };
        let (dir, amount, _) = env.push_price_ix(&self.mm, &self.trader, target.max(1))?;
        self.swap_move(env, dir, amount)
    }

    fn trade(&mut self, env: &mut Env, buy: bool, size: u64) -> Option<Move> {
        let dir = if buy { Dir::QuoteIn } else { Dir::BaseIn };
        self.swap_move(env, dir, size)
    }

    fn lp_pair(&mut self, env: &mut Env, bps: u32) -> Option<(Instruction, Instruction, Keypair)> {
        let r = self.reserves_now(env);
        let bps = u128::from(bps.clamp(1, 9_000));
        let base = u64::try_from(u128::from(r.base) * bps / 10_000).ok()?;
        let quote = u64::try_from(u128::from(r.quote) * bps / 10_000).ok()?;
        if base == 0 || quote == 0 {
            return None;
        }
        // The admin's accounts must hold what the add puts in (a remove first refills them).
        let have_b = read::token_amount(&env.svm, &self.lp_base).unwrap_or(0);
        if have_b < base {
            self.top_up(env, &self.lp_base.clone(), base - have_b);
        }
        let have_q = read::token_amount(&env.svm, &self.lp_quote).unwrap_or(0);
        if have_q < quote {
            mock::wrap(env, &self.lp_quote, quote - have_q);
        }
        let admin = self.mm.pool.admin.pubkey();
        let pool = self.mm.mock();
        Some((
            mock::remove_liquidity_ix(pool, &admin, &self.lp_base, &self.lp_quote, base, quote),
            mock::add_liquidity_ix(pool, &admin, &self.lp_base, &self.lp_quote, base, quote),
            self.mm.pool.admin.insecure_clone(),
        ))
    }

    fn halt(&mut self, cx: &mut Ctx, level: u32) -> bool {
        self.set_knobs(cx, |k| k.disabled = level != 0)
    }

    fn fee_shock(&mut self, cx: &mut Ctx, level: u32) -> bool {
        let fee = match level % 7 {
            0 => self.fee_ppm,
            n => FEES[n as usize],
        };
        self.set_knobs(cx, |k| k.fee_ppm = fee)
    }

    fn special(&mut self, cx: &mut Ctx, kind: Kind, a: u32, b: u32) -> Option<()> {
        match kind {
            Kind::LayoutShock => {
                self.set_knobs(cx, |k| k.layout_version = u8::from(a.is_multiple_of(4)));
            }
            Kind::OverDelivery => {
                self.set_knobs(cx, |k| {
                    k.over_delivery = u64::from(a % 3) * u64::from(a % 1_000_000)
                });
            }
            Kind::ExtrasShock => {
                let keys: Vec<Pubkey> = match a % 3 {
                    0 => vec![self.extra.0],
                    1 => (0..1 + (a / 3) as usize % 2)
                        .map(|_| Pubkey::new_unique())
                        .collect(),
                    _ => Vec::new(),
                };
                self.set_knobs(cx, |k| k.required_extras = keys);
            }
            Kind::ExtrasSet => {
                let legs: &[u8] = match b % 3 {
                    0 => &[EXTRAS_SELL],
                    1 => &[EXTRAS_BUY],
                    _ => &[EXTRAS_SELL, EXTRAS_BUY],
                };
                for leg in legs {
                    let entries = match a % 4 {
                        0 | 3 => vec![self.template()],
                        1 => vec![ExtraEntryArg {
                            kind: 1,
                            writable: (a / 4).is_multiple_of(2),
                            key: Pubkey::new_unique(),
                            seeds: vec![],
                        }],
                        _ => Vec::new(),
                    };
                    self.set_extras(cx, *leg, entries);
                }
            }
            _ => return None,
        }
        Some(())
    }

    fn extras(&self, env: &Env, pick: u32) -> (ExtrasSel, Vec<AccountMeta>) {
        // A selection mostly while the venue requires extras (a client follows the venue);
        // one trade in sixteen carries one regardless.
        let required = !self.knobs(env).required_extras.is_empty();
        let set = if !required && pick % 16 != 15 {
            EXTRAS_SET_NONE
        } else {
            match (pick / 16) % 8 {
                0 => EXTRAS_SET_NONE,
                1..=4 => EXTRAS_SET_CURRENT,
                5 | 6 => EXTRAS_SET_PREVIOUS,
                _ => EXTRAS_SET_STABLE,
            }
        };
        if set == EXTRAS_SET_NONE {
            return (ExtrasSel::NONE, Vec::new());
        }
        (
            ExtrasSel {
                set,
                bumps: [self.extra.1, 0, 0, 0],
            },
            vec![
                AccountMeta::new_readonly(mock_extras_key(), false),
                AccountMeta::new_readonly(self.extra.0, false),
            ],
        )
    }

    fn sweep_ix(&self, env: &Env) -> Instruction {
        sweep_trader_ix_for(env, &self.mm.market())
    }

    fn restore(&mut self, cx: &mut Ctx) {
        let fee = self.fee_ppm;
        self.set_knobs(cx, |k| {
            k.disabled = false;
            k.fee_ppm = fee;
            k.layout_version = 0;
            k.over_delivery = 0;
            k.required_extras.clear();
        });
    }

    fn disturbed(&self, env: &Env) -> bool {
        let k = self.knobs(env);
        k.disabled || k.layout_version != 0 || !k.required_extras.is_empty()
    }
}
