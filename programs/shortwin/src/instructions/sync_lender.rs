//! `sync_lender(max_epochs)` (ARCH §3.3; C0-6): **anyone** advances a lender through up to
//! `max_epochs` share epochs and touches it — but only the position's **owner** converts it
//! through a rebase record (owner decision of 2026-10-07, KI65-REV-SEC1); anyone else's sync of
//! a lender behind one is the **pot-only touch** (owner decision of 2026-10-07, seventh round,
//! KI-66), with its **forward chain** (eighth round, 2026-10-08, KI-66 full).
//!
//! **Signed by `owner`:** `pool::convert` with the first `max_epochs` `ShareEpoch` records of
//! `remaining_accounts` (key-checked): value moves only into that lender's own `pending` /
//! `real`; its shares, lock and lock term scale with each record, and a lender left with
//! nothing jumps to the current epoch (V2-12). If that brought the lender to the current epoch
//! (or parked it on a live full-wipe record), `pool::touch` follows: its surplus of realised
//! SOL over entitlement moves to the pot (R3A-2) and its own expired lock is cleared (the
//! calendar already dropped the lock from the aggregate, v0.4.1). Otherwise the partial
//! conversion is kept and the next call continues it.
//!
//! **Sent by anyone else:** `pool::touch_pot`, the same except at a rebase record: the order
//! of the conversions through a rebase record decides who receives its rounding units (KI-65,
//! below), and with a permissionless conversion a party could order other owners' conversions
//! around its own positions and collect every unit and, where that left every other holder at 0
//! shares, the whole unreleased vest (ECON §9.5). So the lender is never converted through one:
//! a lender that would be (holding shares, a lock or a lock term at the record's epoch, with
//! the record passed) is accrued up to the values the record froze for its epoch (`acc_end`,
//! `acc_r_end`, which become its snapshots, so its owner's later conversion accrues nothing
//! there twice) and its surplus moves to the pot (R3A-2), so the deficit lenders are paid
//! without waiting for an owner that may never return (ECON R3A-2, ARCH I-P6). Its shares,
//! lock, lock term and epoch stay and no record is written: it orders no conversion.
//! Idempotent.
//!
//! **The forward chain (KI-66 full).** From the held record on, the pot-only touch also accrues
//! what the lender's shares earn in the later epochs, on the floor of its shares through the
//! later records (`fwd_shares`, which no conversion order can lower), to each record's ends and
//! the current `acc` / `acc_r` by cumulative floor, and moves that surplus to the pot too: the
//! lender's `pending`, `real`, snapshots (the chain's while it runs) and `fwd*` fields change,
//! nothing else. A lender with a chain continues it from `fwd_epoch`, at most `max_epochs`
//! records at a time (a lender further behind is walked over several calls; anyone may send
//! them); the owner's later conversion takes the chain's accruals back exactly
//! (`pool::touch_pot`). What stays with the owner is the latent rounding unit (its shares
//! beyond the chain's floor, 0 or 1 per record). Plain catch-up through full-wipe records, the V2-12 jump of an empty lender,
//! the parking on a live wipe record and the touch of a current lender (the web's claim-time
//! `sync_lender(0)` of other lenders) are as with the owner. `deposit`, `withdraw`,
//! `claim_sol` and `cover_deficit` are always signed by the owner (their lender PDA is derived
//! from the signer), and no other instruction converts a lender. `RebaseNeedsOwner` stays as a
//! post-condition of a third party's sync, unreachable through `touch_pot`: it read a rebase
//! record (`engine::lender::reads_rebase`), or it changed a record (defence in depth: the
//! records it was given must come back unchanged, so it can never write a `conv_rem`).
//!
//! `sync_lender(0)` is a plain touch (a behind lender stays behind). Does not write `Market`
//! (C0-17). Event `LenderSynced` (`epoch` is the lender's: unchanged after a pot-only touch).
//!
//! Remaining accounts: up to `max_epochs` `ShareEpoch` records, in epoch order, starting at or
//! before the lender's epoch — for a third party's sync of a lender with a forward chain, at or
//! before its `fwd_epoch` (the walk looks records up by epoch; the web passes them from
//! `fwd_epoch`); extra accounts are ignored. The pot-only touch only reads them, so they may be
//! passed read-only. A rebase record the owner's conversion passes through must be writable: the conversion rounds cumulatively and writes the
//! record's `conv_rem` (KI-65, no rebase orphans: the order in which lenders convert decides
//! only who receives each record's rounding units, never below a lender's floor;
//! `engine::lender::store_records`, else `EpochRecordReadOnly`); the pot-only touch writes no
//! record. Apart from those records it writes only the lender and `MarketVault`.

use anchor_lang::prelude::*;
use shortwin_math::pool::{self, Standing};

use crate::constants::MARKET_VERSION;
use crate::engine::lender;
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::LenderSynced;
use crate::state::{LenderPosition, Market, MarketVault};

#[event_cpi]
#[derive(Accounts)]
pub struct SyncLender<'info> {
    /// Read-only (C0-17). Bound to its PDA `["market", mint]` with the stored bump
    /// (`Market::is_pda`; ARCH §2.3; M1-SEC-6 / M1-FID-6).
    #[account(
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = market_vault @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    #[account(
        mut,
        seeds = [LenderPosition::SEED, market.key().as_ref(), owner.key().as_ref()],
        bump = lender.bump,
        has_one = owner @ ShortwinError::Unauthorized,
        has_one = market @ ShortwinError::AccountMismatch
    )]
    pub lender: Box<Account<'info, LenderPosition>>,
    /// CHECK: key-checked by `has_one` on `lender` and by the lender's seeds; never written.
    /// The position's owner. Signing, it converts the lender through rebase records
    /// (KI65-REV-SEC1); unsigned, anyone may send the instruction, which then stops at a rebase
    /// record with the pot-only touch (KI-66).
    pub owner: UncheckedAccount<'info>,
}

pub fn handler<'info>(ctx: Context<'info, SyncLender<'info>>, max_epochs: u8) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let market_key = ctx.accounts.market.key();

    let (owner, t_after, s_after, after) = {
        let market = ctx.accounts.market.load()?;
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let pool_state = market.pool();
        let mut vault = vault_acc.vault();
        let mut records =
            lender::load_records(&market_key, ctx.remaining_accounts, usize::from(max_epochs))?;
        let loaded = records.clone();
        let position = &mut ctx.accounts.lender;
        let before = position.lender();
        let mut l = before;
        let owner_signs = ctx.accounts.owner.is_signer;
        if owner_signs {
            let standing = pool::convert(&pool_state, &mut l, &mut records).math()?;
            if standing != Standing::Behind {
                pool::touch(&pool_state, &mut vault, &mut l, &mut records, now).math()?;
            }
        } else {
            // KI-66: never through a rebase record; the surplus up to it still reaches the pot.
            pool::touch_pot(&pool_state, &mut vault, &mut l, &records, now).math()?;
        }
        // KI65-REV-SEC1: only the owner converts its position through a rebase record, and a
        // third party's sync writes no record (KI-66 full: `conv_rem` is the owner's to order).
        require!(
            owner_signs || (!lender::reads_rebase(&before, &l, &loaded) && records == loaded),
            ShortwinError::RebaseNeedsOwner
        );
        lender::store_records(ctx.remaining_accounts, &loaded, &records)?;

        vault_acc.set_vault(&vault);
        position.set_lender(&l);
        (
            position.owner,
            pool_state.tokens_free,
            pool_state.total_shares,
            l,
        )
    };

    emit_cpi!(LenderSynced {
        market: market_key,
        slot: clock.slot,
        ts: now,
        owner,
        amount: 0,
        shares: 0,
        t_after,
        s_after,
        sol_paid: 0,
        pot_draw: 0,
        lock: after.lock,
        lock_until: after.lock_until,
        epoch: after.epoch,
    });
    Ok(())
}
