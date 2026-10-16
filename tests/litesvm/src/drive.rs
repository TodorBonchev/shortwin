//! Time and price drivers for the engine tests (ARCH §8.2 "scenario builders"): market
//! participants that trade the mock pool directly, per-second keeper cranks, price pushes.
//!
//! - [`Actor`]: a keypair with a base-token account and a WSOL account ([`Env::actor`]), the
//!   arbitrageur, attacker or honest trader of a scenario.
//! - [`swap_in_ix`] / [`Env::trade`]: an exact-in swap on the mock pool.
//! - [`Env::push_price_ix`] / [`Env::push_price`]: the one swap that moves the pool's spot to a
//!   target price (constant product, fee on the input; approximate to a few ppm).
//! - [`Env::tick`]: `n` seconds of the in-house keeper, one `crank_price` per second (ECON §5.8
//!   duty 1), each an observation of the market.
//! - [`Env::spot`]: the pool's spot `P = ⌊Y·WAD/X⌋` on its pricing reserves.
//!
//! Every instruction goes through [`Env::send`] (the invariant hooks).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin_math::num::WAD;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::engine::{MockMarket, VenueMarket};
use crate::mock::{self, Dir, Leg};
use crate::{Env, TxResult, tokens};

/// A participant trading the mock pool directly.
pub struct Actor {
    pub kp: Keypair,
    /// Its base-token account.
    pub base: Pubkey,
    /// Its WSOL account.
    pub wsol: Pubkey,
}

impl Actor {
    /// The actor's key.
    pub fn key(&self) -> Pubkey {
        self.kp.pubkey()
    }
}

/// An exact-in swap by `actor` on `mm`'s pool: `BaseIn` sells base tokens for WSOL, `QuoteIn`
/// buys base tokens with WSOL.
pub fn swap_in_ix(
    mm: &MockMarket,
    actor: &Actor,
    dir: Dir,
    amount_in: u64,
    min_out: u64,
) -> Instruction {
    let (source, destination) = match dir {
        Dir::BaseIn => (actor.base, actor.wsol),
        Dir::QuoteIn => (actor.wsol, actor.base),
    };
    let leg = Leg {
        dir,
        authority: actor.key(),
        source,
        destination,
    };
    mock::swap_exact_in_ix(mm.mock(), &leg, amount_in, min_out, &[])
}

impl Env {
    /// A funded actor holding `base` raw tokens (minted) and `sol` wrapped lamports.
    pub fn actor(&mut self, mm: &MockMarket, base: u64, sol: u64) -> Actor {
        let kp = self.funded_keypair();
        let base_acc =
            mock::base_account(self, mm.mock(), &kp.pubkey(), &mm.pool.mint_authority, base);
        let wsol = mock::wsol_account(self, &kp.pubkey(), sol);
        Actor {
            kp,
            base: base_acc,
            wsol,
        }
    }

    /// Mints `amount` more base tokens to `actor`.
    pub fn fund_base(&mut self, mm: &MockMarket, actor: &Actor, amount: u64) {
        let payer = self.payer.insecure_clone();
        tokens::mint_to(
            &mut self.svm,
            &payer,
            &mm.mint(),
            &actor.base,
            &mm.pool.mint_authority,
            amount,
            mm.spec.program,
        );
    }

    /// An exact-in swap by `actor`.
    pub fn trade(&mut self, mm: &MockMarket, actor: &Actor, dir: Dir, amount_in: u64) -> TxResult {
        let ix = swap_in_ix(mm, actor, dir, amount_in, 0);
        let kp = actor.kp.insecure_clone();
        self.send(&[ix], &[&kp])
    }

    /// The pool's spot `P = ⌊Y·WAD/X⌋` on its pricing reserves.
    pub fn spot(&self, mm: &MockMarket) -> u128 {
        let r = mock::reserves(&self.svm, mm.mock());
        u128::from(r.quote) * WAD / u128::from(r.base)
    }

    /// The exact-in swap that moves the pool's spot from its current value to about
    /// `target` (WAD), or `None` if it is already there: a buy (`QuoteIn`) to raise it, a sale
    /// (`BaseIn`) to lower it. Constant product with the fee on the input: the net input is
    /// `√(k·P₁) − Y` SOL (or `√(k/P₁) − X` tokens), grossed up by the fee.
    pub fn push_price_ix(
        &self,
        mm: &MockMarket,
        actor: &Actor,
        target: u128,
    ) -> Option<(Dir, u64, Instruction)> {
        let r = mock::reserves(&self.svm, mm.mock());
        let fee = mock::state(&self.svm, &mm.mock().pool).fee_ppm as f64 / 1e6;
        let (x, y) = (r.base as f64, r.quote as f64);
        let k = x * y;
        let p1 = target as f64 / WAD as f64;
        let p0 = y / x;
        let (dir, net) = if p1 > p0 {
            (Dir::QuoteIn, (k * p1).sqrt() - y)
        } else if p1 < p0 {
            (Dir::BaseIn, (k / p1).sqrt() - x)
        } else {
            return None;
        };
        let gross = (net / (1.0 - fee)).ceil();
        if gross < 1.0 {
            return None;
        }
        let amount = gross as u64;
        Some((dir, amount, swap_in_ix(mm, actor, dir, amount, 0)))
    }

    /// Pushes the pool's spot to about `target` with one swap by `actor` (which must hold
    /// enough of the input side).
    pub fn push_price(&mut self, mm: &MockMarket, actor: &Actor, target: u128) {
        if let Some((_, _, ix)) = self.push_price_ix(mm, actor, target) {
            let kp = actor.kp.insecure_clone();
            crate::errors::ok(self.send(&[ix], &[&kp]));
        }
    }

    /// `secs` seconds of the in-house keeper: each second, advance the clock by one and crank.
    pub fn tick(&mut self, mm: &impl VenueMarket, secs: u32) {
        for _ in 0..secs {
            self.warp(1);
            self.crank(mm);
        }
    }
}
