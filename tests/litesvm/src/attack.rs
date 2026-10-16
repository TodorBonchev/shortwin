//! Atomic attacker bundles for the engine tests (ARCH §8.2 "adversarial suite"; ECON §12): an
//! attacker's instructions and a victim's in **one transaction**, so nothing can land between
//! them — the atomic model of ECON §12.1 (and a Jito bundle's all-or-nothing execution).
//!
//! - [`Sandwich`]: a front-run swap, the victim instruction, and the back-run that unwinds the
//!   front-run (exact in: the front-run's quoted output, sold back), built from the pool's live
//!   state; [`Env::sandwich`] sends it and reports the attacker's SOL result.
//! - [`Env::bundle`]: any instruction list in one transaction.
//!
//! The invariant hooks check the state after **every** instruction of a bundle (prefix by
//! prefix, `Env::send`).

use anchor_lang::solana_program::instruction::Instruction;
use solana_keypair::Keypair;

use crate::drive::{Actor, swap_in_ix};
use crate::engine::MockMarket;
use crate::mock::{self, Dir};
use crate::{Env, TxResult, read};

/// A front-run, a victim and a back-run, in that order.
pub struct Sandwich {
    pub front: Instruction,
    pub back: Instruction,
    /// The front-run's direction and input.
    pub dir: Dir,
    pub amount_in: u64,
}

impl Env {
    /// Sends `ixs` in one transaction.
    pub fn bundle(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> TxResult {
        self.send(ixs, signers)
    }

    /// The sandwich of `amount_in` in direction `dir` by `attacker` around a victim: the
    /// back-run sells exactly what the front-run buys (its quote on the live state).
    pub fn sandwich_ixs(
        &self,
        mm: &MockMarket,
        attacker: &Actor,
        dir: Dir,
        amount_in: u64,
    ) -> Sandwich {
        let out =
            mock::quote_exact_in(&self.svm, mm.mock(), dir, amount_in).map_or(0, |q| q.proceeds);
        let back_dir = match dir {
            Dir::BaseIn => Dir::QuoteIn,
            Dir::QuoteIn => Dir::BaseIn,
        };
        Sandwich {
            front: swap_in_ix(mm, attacker, dir, amount_in, 0),
            back: swap_in_ix(mm, attacker, back_dir, out, 0),
            dir,
            amount_in,
        }
    }

    /// Sends `[front, victim, back]` in one transaction. Returns the result and the attacker's
    /// change in wrapped SOL and in base tokens (both signed; the base change is 0 when the
    /// bundle succeeds, since the back-run sells what the front-run bought).
    pub fn sandwich(
        &mut self,
        mm: &MockMarket,
        attacker: &Actor,
        victim: Instruction,
        victim_signer: &Keypair,
        dir: Dir,
        amount_in: u64,
    ) -> (TxResult, i128, i128) {
        let s = self.sandwich_ixs(mm, attacker, dir, amount_in);
        let sol0 = i128::from(read::token_amount(&self.svm, &attacker.wsol).unwrap_or(0));
        let base0 = i128::from(read::token_amount(&self.svm, &attacker.base).unwrap_or(0));
        let kp = attacker.kp.insecure_clone();
        let res = self.send(&[s.front, victim, s.back], &[&kp, victim_signer]);
        let sol1 = i128::from(read::token_amount(&self.svm, &attacker.wsol).unwrap_or(0));
        let base1 = i128::from(read::token_amount(&self.svm, &attacker.base).unwrap_or(0));
        (res, sol1 - sol0, base1 - base0)
    }
}
