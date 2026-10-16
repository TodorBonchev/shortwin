//! `add_margin(amount)` (ARCH §3.5; A20, ADOPTED Q12): the borrower adds SOL margin to one of
//! its positions. It **only raises CR**.
//!
//! **Signer:** `borrower` (`== position.borrower`; pays). **Named accounts:** `market`
//! (**read**: `add_margin` does not write `Market`, C0-17, ARCH §6.6 — a borrower topping up
//! never competes with the market's cranks and exits for its write lock), `position` (mut),
//! System, event accounts. No venue, oracle, latch or config account: nothing it does depends
//! on a price, a route, a pause or a parameter.
//!
//! **Effect:** a System transfer of `amount` lamports from the borrower to the position, then
//! `v += amount`, `m += amount` (checked before the transfer). Nothing else changes — the
//! snapshot rules (A26):
//!
//! - every **locked** term of ARCH §2.3.5, `deadline`, `r_min`, `j_open`, `q`, `s`, `n_open`,
//!   `fee_open`, `entry_price`, the tier and the venue at open stay as `open_short` wrote them:
//!   rent is still owed from `j_open` at the locked rate, and the exit bounds, penalties,
//!   keeper terms and S3 timers are the open's;
//! - the **settlement arming state** is untouched: a higher `V` can only make the S1/S2 legs
//!   false, and `sol_settle` itself decides on the box whether that disarms (ARCH §3.8 step 2),
//!   so `add_margin` never resets a clock that only box-robust evidence may reset (I12);
//! - `Market` is not written: no aggregate (`ΣB_Q`, `A_j`, `open_positions`,
//!   `total_notional_open`, `max_release`) depends on `v` or `m`.
//!
//! **Allowed on every open position** — while opens are paused, the route is flagged, the venue
//! is upgraded, disabled or migrating, the mint is flagged unsafe, the position is
//! liquidatable, armed for settlement or past its deadline: it moves SOL *into* the position
//! only, so no gate protects anyone from it (the exits' I10 reasoning). `amount == 0` is
//! refused (`ZeroAmount`; ARCH names no check, and a zero top-up would only emit an event). A
//! position whose `status` is not Open (1 = ExitInProgress, reserved for v1.1) is refused
//! (`AccountMismatch`): a defensive constraint beyond ARCH, since no MVP instruction writes
//! that status.
//!
//! **I2 / I3:** the position's lamports rise by exactly `amount` and `v` by the same, so
//! `lamports ≥ rent_min + v` is preserved; position SOL never leaves here.
//!
//! **Event:** `MarginAdded{position, amount}` (`emit_cpi!`). Instruction trace: the instruction,
//! the System transfer and the event (3 entries).

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer};

use crate::constants::MARKET_VERSION;
use crate::errors::{CheckedExt, ShortwinError};
use crate::events::MarginAdded;
use crate::state::{Market, ShortPosition, POSITION_OPEN};

// Field doc comments are part of the IDL: they are kept as the foundation wrote them.
#[event_cpi]
#[derive(Accounts)]
pub struct AddMargin<'info> {
    #[account(mut)]
    pub borrower: Signer<'info>,
    /// Read only (C0-17); bound to its PDA with the stored bump.
    #[account(
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        has_one = borrower @ ShortwinError::AccountMismatch,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = position.is_pda(&position.key()) @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = position.status == POSITION_OPEN @ ShortwinError::AccountMismatch
    )]
    pub position: Box<Account<'info, ShortPosition>>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<AddMargin>, amount: u64) -> Result<()> {
    require!(amount > 0, ShortwinError::ZeroAmount);
    let clock = Clock::get()?;
    let position_key = ctx.accounts.position.key();
    let market_key = ctx.accounts.market.key();

    // `v` and `m` must stay representable; checked before any lamport moves.
    let v = ctx.accounts.position.v.checked_add(amount).or_math()?;
    let m = ctx.accounts.position.m.checked_add(amount).or_math()?;

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.key(),
            Transfer {
                from: ctx.accounts.borrower.to_account_info(),
                to: ctx.accounts.position.to_account_info(),
            },
        ),
        amount,
    )?;

    let position = &mut ctx.accounts.position;
    position.v = v;
    position.m = m;

    emit_cpi!(MarginAdded {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        position: position_key,
        amount,
    });
    Ok(())
}
