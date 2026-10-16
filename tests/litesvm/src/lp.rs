//! A liquidity provider on a CPMM pool of the fork, through the dumped program's own `deposit` and
//! `withdraw` instructions (M4F-1: the attacker's `[deposit k·LP counter, crank_price, withdraw]`
//! bundle; shared with the keeper scenarios). The program tests `m4r_it`, `m4v_it`, `m4_sec_it`
//! and `pool_attacks_it` carry their own copies of these builders.

use anchor_lang::prelude::{Pubkey, pubkey};
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::constants::{RAYDIUM_CPMM_AUTHORITY, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
use shortwin_math::fees::Direction;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::Env;
use crate::fork::{self, AnyPool, CpmmPool};
use crate::tokens::TokenProgram;

/// The SPL Memo program the CPMM `withdraw` names.
pub const MEMO: Pubkey = pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

/// A funded liquidity provider: its WSOL, token and LP accounts, in the pool's token order.
pub struct Provider {
    pub owner: Keypair,
    pub t0: Pubkey,
    pub t1: Pubkey,
    pub lp: Pubkey,
}

impl Provider {
    /// The provider's key.
    pub fn key(&self) -> Pubkey {
        self.owner.pubkey()
    }
}

/// A new provider on `p` holding `sol` lamports of WSOL, `tokens` of the pool's token and `lp` LP.
pub fn provider(env: &mut Env, p: &CpmmPool, sol: u64, tokens: u64, lp: u64) -> Provider {
    let owner = env.funded_keypair();
    let w = fork::fund_wsol(env, &owner.pubkey(), sol);
    let t = fork::fund_token(
        env,
        &owner.pubkey(),
        &p.token_mint(),
        p.token_program(),
        tokens,
    );
    let l = fork::fund_token(env, &owner.pubkey(), &p.lp_mint, TokenProgram::Spl, lp);
    let (t0, t1) = if p.sol_is_token0 { (w, t) } else { (t, w) };
    Provider {
        owner,
        t0,
        t1,
        lp: l,
    }
}

/// The SOL and tokens a deposit of `lp` LP into `p` needs (rounded up, plus one).
pub fn lp_needed(env: &Env, p: &CpmmPool, lp: u64) -> (u64, u64) {
    let counter = p.state(&env.svm).lp_supply;
    let (x, y) = AnyPool::Cpmm(*p)
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let part = |r: u64| {
        u64::try_from((u128::from(r) * u128::from(lp)).div_ceil(u128::from(counter))).expect("u64")
            + 1
    };
    (part(y), part(x))
}

fn disc(name: &str) -> [u8; 8] {
    let h = fork::sha256(format!("global:{name}").as_bytes());
    let mut d = [0u8; 8];
    d.copy_from_slice(&h[..8]);
    d
}

fn metas(p: &CpmmPool, who: &Provider) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(who.key(), true),
        AccountMeta::new_readonly(RAYDIUM_CPMM_AUTHORITY, false),
        AccountMeta::new(p.pool, false),
        AccountMeta::new(who.lp, false),
        AccountMeta::new(who.t0, false),
        AccountMeta::new(who.t1, false),
        AccountMeta::new(p.token_0_vault, false),
        AccountMeta::new(p.token_1_vault, false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
        AccountMeta::new_readonly(p.token_0_mint, false),
        AccountMeta::new_readonly(p.token_1_mint, false),
        AccountMeta::new(p.lp_mint, false),
    ]
}

fn data(name: &str, a: u64, b: u64, c: u64) -> Vec<u8> {
    let mut d = disc(name).to_vec();
    for v in [a, b, c] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d
}

/// CPMM `deposit` of `lp` LP (no maximum on either side).
pub fn deposit_ix(p: &CpmmPool, who: &Provider, lp: u64) -> Instruction {
    Instruction {
        program_id: fork::RaydiumProgram::Cpmm.id(),
        accounts: metas(p, who),
        data: data("deposit", lp, u64::MAX, u64::MAX),
    }
}

/// CPMM `withdraw` of `lp` LP (no minimum on either side).
pub fn withdraw_ix(p: &CpmmPool, who: &Provider, lp: u64) -> Instruction {
    let mut accounts = metas(p, who);
    accounts.push(AccountMeta::new_readonly(MEMO, false));
    Instruction {
        program_id: fork::RaydiumProgram::Cpmm.id(),
        accounts,
        data: data("withdraw", lp, 0, 0),
    }
}
