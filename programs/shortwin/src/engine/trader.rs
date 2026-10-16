//! The trader PDA and the ephemeral WSOL account (ARCH §2.4 rules 1–5, §2.4.1; A4, A5, A33;
//! C0-3, C1-3).
//!
//! `trader = PDA(["trader", market])` is a system-owned, zero-data account with 0 lamports at
//! the end of every short.win instruction; it is the only key short.win ever signs with towards
//! a venue. `trader_wsol = PDA(["trader_wsol", market])` is an SPL Token WSOL account owned
//! (as token authority) by `trader`, created and closed inside each WSOL leg.
//!
//! Anyone can credit the trader or pre-fund the `trader_wsol` address (a System transfer
//! checks only `from`), and anyone can create `ATA(trader, WSOL)` (the ATA program checks only
//! the address). None of that can block a leg:
//!
//! - [`TraderAccounts::normalise`] runs at the start of every venue leg, **before any funding**:
//!   it requires both PDAs to be System-owned with no data (only short.win can sign `allocate`
//!   / `assign` for them, so a third party cannot change that), and sweeps any lamports they
//!   hold to `market_vault`; the caller credits them to lenders as a lump (`pool::sweep`,
//!   vesting over `lump_vest_s`) and emits `TraderSwept`. Legs measure against this
//!   post-normalisation baseline.
//! - [`TraderAccounts::open_wsol`] creates `trader_wsol` grief-proof: after the sweep the
//!   address holds 0 lamports, so it funds exactly `rent(165) + amount`, then `allocate` and
//!   `assign` signed with the PDA's seeds (never `create_account`, which a pre-funded address
//!   would fail), then `InitializeAccount3(WSOL, owner = trader)`, which sets `amount =
//!   lamports − rent` (no `SyncNative`).
//! - `ATA(trader, WSOL)` is never used by a leg; `sweep_trader` closes it ([`close_trader_ata`];
//!   built in M2 instead of M6, M2-SEC-4).
//! - **Pump venues** (M5: the curve) trade through `trader_base_ata = ATA(trader, mint)` instead of
//!   `trader_wsol` (ARCH §2.4.1 PumpCurve row: the curve pays and credits native lamports). Anyone
//!   can send tokens to it, so the normalisation also moves every token it holds to `pool_vault`
//!   (a donation: `T` is unchanged, I1 tolerates it) after checking that only the trader could have
//!   set its owner, delegate and close authority ([`BaseSide`]; `sweep_trader`'s token leg is
//!   [`sweep_base_ata`]).
//! - Post-conditions assert **deltas and authorities**, never that an account a third party can
//!   create does not exist ([`TraderAccounts::post_conditions`], I7).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::system_program::{self, Allocate, Assign, Transfer};
use anchor_spl::token_interface::{self, CloseAccount, InitializeAccount3, TransferChecked};
use shortwin_venue_state::token_account::{self, TokenAccount};

use crate::constants::TOKEN_PROGRAM_ID;
use crate::errors::{CheckedExt, ShortwinError};
use crate::lamports;
use crate::state::{TRADER_SEED, TRADER_WSOL_SEED};

/// Size of the ephemeral WSOL account (a plain SPL Token account).
pub const WSOL_ACCOUNT_LEN: usize = 165;

/// Where the lamports of a new `trader_wsol` come from.
pub enum Funding<'a, 'info> {
    /// A signer's System account (the borrower on a sell leg): System transfer.
    Signer(&'a AccountInfo<'info>),
    /// The `ShortPosition` itself (a buy leg): direct debit, no CPI (ARCH §2.4 rule 3).
    Position(&'a AccountInfo<'info>),
}

/// The WSOL side of a leg (PumpSwap, Raydium, the mock): the ephemeral `trader_wsol` address,
/// the WSOL mint and SPL Token.
#[derive(Clone)]
pub struct WsolSide<'info> {
    pub trader_wsol: AccountInfo<'info>,
    pub trader_wsol_bump: u8,
    /// The SPL Token native mint (WSOL).
    pub wsol_mint: AccountInfo<'info>,
    /// SPL Token (`Tokenkeg`), the WSOL side's program.
    pub wsol_token_program: AccountInfo<'info>,
}

/// The token side of a pump-venue leg (ARCH §2.4 rule 2, pump venues): `trader_base_ata =
/// ATA(trader, mint, market.token_program)` (key-checked against the market by the adapter), the
/// pool vault that receives what it holds, the base mint and its program.
#[derive(Clone)]
pub struct BaseSide<'info> {
    pub trader_base_ata: AccountInfo<'info>,
    pub pool_vault: AccountInfo<'info>,
    pub mint: AccountInfo<'info>,
    pub decimals: u8,
    pub token_program: AccountInfo<'info>,
}

/// The trader-side accounts of a venue leg.
#[derive(Clone)]
pub struct TraderAccounts<'info> {
    /// The market PDA the trader PDAs derive from.
    pub market: Pubkey,
    pub trader: AccountInfo<'info>,
    pub trader_bump: u8,
    /// Receives swept lamports.
    pub market_vault: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
    /// The WSOL side (PumpSwap, Raydium, the mock); `None` on the curve.
    pub wsol: Option<WsolSide<'info>>,
    /// The token side of the pump venues (`trader_base_ata`); `None` on Raydium and the mock.
    pub base: Option<BaseSide<'info>>,
}

/// What a leg's normalisation moved (ARCH §2.4 rule 2): lamports into `market_vault` (a lender
/// lump) and tokens of `trader_base_ata` into `pool_vault` (a donation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub lamports: u64,
    pub tokens: u64,
}

impl Swept {
    /// Whether anything moved (the caller emits `TraderSwept` then).
    pub const fn any(&self) -> bool {
        self.lamports > 0 || self.tokens > 0
    }
}

/// A token account's fields (`AccountMismatch` if it does not parse).
pub fn token_state(info: &AccountInfo) -> Result<TokenAccount> {
    let data = info.try_borrow_data()?;
    token_account::read(&info.owner.to_bytes(), &data)
        .map_err(|_| error!(ShortwinError::AccountMismatch))
}

/// `true` if `info` is a System account with no data (what an unused PDA always is).
fn is_system_empty(info: &AccountInfo) -> bool {
    *info.owner == System::id() && info.data_is_empty()
}

/// The two trader PDAs of a market and where their lamports go: what the normalisation needs
/// (a venue leg's [`TraderAccounts`], or `sweep_trader`, which has no leg).
pub struct TraderPdas<'info> {
    /// The market PDA the two trader PDAs derive from.
    pub market: Pubkey,
    pub trader: AccountInfo<'info>,
    pub trader_bump: u8,
    pub trader_wsol: AccountInfo<'info>,
    pub trader_wsol_bump: u8,
    /// Receives swept lamports.
    pub market_vault: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
}

/// ARCH §2.4 rule 2, the normalisation (C0-3, C1-3): requires `trader` and the `trader_wsol`
/// address to be System-owned with no data (`TraderStateInvalid`) and moves every lamport they
/// hold to `market_vault` (each PDA signs its own System transfer). Returns the lamports moved.
pub fn normalise_pdas(t: &TraderPdas<'_>) -> Result<u64> {
    require!(
        is_system_empty(&t.trader),
        ShortwinError::TraderStateInvalid
    );
    require!(
        is_system_empty(&t.trader_wsol),
        ShortwinError::TraderStateInvalid
    );
    let from_trader = t.trader.lamports();
    if from_trader > 0 {
        let bump = [t.trader_bump];
        let seeds: &[&[u8]] = &[TRADER_SEED, t.market.as_ref(), &bump];
        system_program::transfer(
            CpiContext::new_with_signer(
                t.system_program.key(),
                Transfer {
                    from: t.trader.clone(),
                    to: t.market_vault.clone(),
                },
                &[seeds],
            ),
            from_trader,
        )?;
    }
    let from_wsol = t.trader_wsol.lamports();
    if from_wsol > 0 {
        let bump = [t.trader_wsol_bump];
        let seeds: &[&[u8]] = &[TRADER_WSOL_SEED, t.market.as_ref(), &bump];
        system_program::transfer(
            CpiContext::new_with_signer(
                t.system_program.key(),
                Transfer {
                    from: t.trader_wsol.clone(),
                    to: t.market_vault.clone(),
                },
                &[seeds],
            ),
            from_wsol,
        )?;
    }
    from_trader.checked_add(from_wsol).or_math()
}

/// `sweep_trader`'s first step (ARCH §3.9): if `ATA(trader, WSOL)` — which anyone can create
/// and fund, and which no leg uses — is an initialised SPL Token account of `trader`,
/// `CloseAccount(ata → trader)` with `trader` signing moves all its lamports (rent and wrapped
/// SOL; a native account closes with any balance) to `trader`, where the normalisation picks
/// them up. Returns the lamports moved (0 if the account does not exist or is not a token
/// account yet: lamports sent to a not-yet-created ATA address stay there until someone
/// creates it). `ata` must already be key-checked as `ATA(trader, WSOL)` under SPL Token.
pub fn close_trader_ata<'info>(
    market: &Pubkey,
    trader: &AccountInfo<'info>,
    trader_bump: u8,
    ata: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
) -> Result<u64> {
    if *ata.owner != TOKEN_PROGRAM_ID || ata.data_is_empty() {
        return Ok(0);
    }
    let t = token_state(ata)?;
    require!(
        t.owner == trader.key.to_bytes() && t.mint == crate::constants::WSOL_MINT.to_bytes(),
        ShortwinError::AccountMismatch
    );
    let lamports = ata.lamports();
    let bump = [trader_bump];
    let seeds: &[&[u8]] = &[TRADER_SEED, market.as_ref(), &bump];
    token_interface::close_account(CpiContext::new_with_signer(
        token_program.key(),
        CloseAccount {
            account: ata.clone(),
            destination: trader.clone(),
            authority: trader.clone(),
        },
        &[seeds],
    ))?;
    Ok(lamports)
}

/// ARCH §2.4 rule 2 for a pump venue's token side (and `sweep_trader`'s token leg, ARCH §3.9):
/// `trader_base_ata` must be a token account of the market's mint under its program, owned by
/// `trader`, with no delegate and no close authority (only the trader could set them), else
/// `TraderStateInvalid`; every token it holds moves to `pool_vault` (`transfer_checked`, the
/// trader signing). Returns the tokens moved. A base ATA that does not exist yet is
/// `TraderStateInvalid` too: the pump venues create it at listing (or the keeper at a transition)
/// and a leg cannot run without it.
pub fn sweep_base_ata<'info>(
    market: &Pubkey,
    trader: &AccountInfo<'info>,
    trader_bump: u8,
    base: &BaseSide<'info>,
) -> Result<u64> {
    let t = base_ata_state(trader, base)?;
    if t.amount == 0 {
        return Ok(0);
    }
    let bump = [trader_bump];
    let seeds: &[&[u8]] = &[TRADER_SEED, market.as_ref(), &bump];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            base.token_program.key(),
            TransferChecked {
                from: base.trader_base_ata.clone(),
                mint: base.mint.clone(),
                to: base.pool_vault.clone(),
                authority: trader.clone(),
            },
            &[seeds],
        ),
        t.amount,
        base.decimals,
    )?;
    Ok(t.amount)
}

/// The fields of `trader_base_ata`, checked as [`sweep_base_ata`] says (`TraderStateInvalid`).
pub fn base_ata_state(trader: &AccountInfo, base: &BaseSide) -> Result<TokenAccount> {
    require_keys_eq!(
        *base.trader_base_ata.owner,
        base.token_program.key(),
        ShortwinError::TraderStateInvalid
    );
    let t = token_state(&base.trader_base_ata)
        .map_err(|_| error!(ShortwinError::TraderStateInvalid))?;
    require!(
        t.mint == base.mint.key.to_bytes()
            && t.owner == trader.key.to_bytes()
            && t.delegate.is_none()
            && t.close_authority.is_none()
            && !t.frozen,
        ShortwinError::TraderStateInvalid
    );
    Ok(t)
}

/// Moves every lamport of the System-owned, data-less `trader` to `market_vault` (the trader
/// signs; `TraderStateInvalid` if it carries data or is not System-owned). Returns the lamports.
fn sweep_trader_lamports<'info>(
    market: &Pubkey,
    trader: &AccountInfo<'info>,
    trader_bump: u8,
    market_vault: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
) -> Result<u64> {
    require!(is_system_empty(trader), ShortwinError::TraderStateInvalid);
    let from_trader = trader.lamports();
    if from_trader > 0 {
        let bump = [trader_bump];
        let seeds: &[&[u8]] = &[TRADER_SEED, market.as_ref(), &bump];
        system_program::transfer(
            CpiContext::new_with_signer(
                system_program.key(),
                Transfer {
                    from: trader.clone(),
                    to: market_vault.clone(),
                },
                &[seeds],
            ),
            from_trader,
        )?;
    }
    Ok(from_trader)
}

impl<'info> TraderAccounts<'info> {
    /// The rent-exempt minimum of a WSOL account.
    pub fn wsol_rent() -> Result<u64> {
        lamports::rent_min(WSOL_ACCOUNT_LEN)
    }

    /// The WSOL side (`BadRemainingAccounts` on a venue without one: a programming error, the
    /// adapters of the WSOL venues always build it).
    pub fn wsol(&self) -> Result<&WsolSide<'info>> {
        self.wsol
            .as_ref()
            .ok_or_else(|| error!(ShortwinError::BadRemainingAccounts))
    }

    /// The ephemeral `trader_wsol` (WSOL venues).
    pub fn trader_wsol(&self) -> Result<&AccountInfo<'info>> {
        Ok(&self.wsol()?.trader_wsol)
    }

    /// ARCH §2.4 rule 2, the normalisation (C0-3, C1-3), before any funding: the WSOL venues'
    /// [`normalise_pdas`] (the `trader` and `trader_wsol` lamports into `market_vault`); the pump
    /// venues' `trader` lamports into `market_vault` and `trader_base_ata`'s tokens into
    /// `pool_vault` ([`sweep_base_ata`]). The caller books the lamports as a lender lump (the
    /// tokens are a donation: `T` unchanged) and emits `TraderSwept`.
    pub fn normalise(&self) -> Result<Swept> {
        let lamports = match &self.wsol {
            Some(w) => normalise_pdas(&TraderPdas {
                market: self.market,
                trader: self.trader.clone(),
                trader_bump: self.trader_bump,
                trader_wsol: w.trader_wsol.clone(),
                trader_wsol_bump: w.trader_wsol_bump,
                market_vault: self.market_vault.clone(),
                system_program: self.system_program.clone(),
            })?,
            None => sweep_trader_lamports(
                &self.market,
                &self.trader,
                self.trader_bump,
                &self.market_vault,
                &self.system_program,
            )?,
        };
        let tokens = match &self.base {
            Some(b) => sweep_base_ata(&self.market, &self.trader, self.trader_bump, b)?,
            None => 0,
        };
        Ok(Swept { lamports, tokens })
    }

    /// Creates `trader_wsol` holding `amount` wrapped lamports (A5; grief-proof, module docs):
    /// funds `rent(165) + amount` from `funding`, then `allocate` and `assign` (the PDA signs),
    /// then `InitializeAccount3(WSOL, owner = trader)`. Must follow [`Self::normalise`] in the
    /// same instruction (the address then holds 0 lamports).
    pub fn open_wsol(&self, funding: Funding<'_, 'info>, amount: u64) -> Result<()> {
        let w = self.wsol()?;
        require!(
            is_system_empty(&w.trader_wsol) && w.trader_wsol.lamports() == 0,
            ShortwinError::TraderStateInvalid
        );
        let lamports_needed = Self::wsol_rent()?.checked_add(amount).or_math()?;
        let bump = [w.trader_wsol_bump];
        let seeds: &[&[u8]] = &[TRADER_WSOL_SEED, self.market.as_ref(), &bump];
        let space =
            u64::try_from(WSOL_ACCOUNT_LEN).map_err(|_| error!(ShortwinError::MathOverflow))?;
        match funding {
            Funding::Signer(payer) => {
                system_program::transfer(
                    CpiContext::new(
                        self.system_program.key(),
                        Transfer {
                            from: payer.clone(),
                            to: w.trader_wsol.clone(),
                        },
                    ),
                    lamports_needed,
                )?;
                system_program::allocate(
                    CpiContext::new_with_signer(
                        self.system_program.key(),
                        Allocate {
                            account_to_allocate: w.trader_wsol.clone(),
                        },
                        &[seeds],
                    ),
                    space,
                )?;
            }
            Funding::Position(position) => {
                lamports::move_lamports_in_leg(position, &w.trader_wsol, lamports_needed)?;
                // A direct lamport move is visible to the runtime only once both accounts are
                // part of a CPI: the next CPI's push checks that the caller's accounts still sum
                // to their original lamports, and it sees the credited `trader_wsol` but not
                // the debited position unless the position is in that CPI too. The System
                // program's `allocate` reads only its first account, so the position rides
                // along as a trailing writable account (never a signer).
                let mut ix = anchor_lang::solana_program::system_instruction::allocate(
                    w.trader_wsol.key,
                    space,
                );
                ix.accounts.push(AccountMeta::new(position.key(), false));
                invoke_signed(&ix, &[w.trader_wsol.clone(), position.clone()], &[seeds])?;
            }
        }
        system_program::assign(
            CpiContext::new_with_signer(
                self.system_program.key(),
                Assign {
                    account_to_assign: w.trader_wsol.clone(),
                },
                &[seeds],
            ),
            &TOKEN_PROGRAM_ID,
        )?;
        token_interface::initialize_account3(CpiContext::new(
            TOKEN_PROGRAM_ID,
            InitializeAccount3 {
                account: w.trader_wsol.clone(),
                mint: w.wsol_mint.clone(),
                authority: self.trader.clone(),
            },
        ))
    }

    /// The wrapped amount in `trader_wsol` (`VenueResultMismatch` if it is not the WSOL
    /// account of `trader`).
    pub fn wsol_amount(&self) -> Result<u64> {
        let w = self.wsol()?;
        let t =
            token_state(&w.trader_wsol).map_err(|_| error!(ShortwinError::VenueResultMismatch))?;
        require!(
            t.mint == w.wsol_mint.key.to_bytes() && t.owner == self.trader.key.to_bytes(),
            ShortwinError::VenueResultMismatch
        );
        Ok(t.amount)
    }

    /// `CloseAccount(trader_wsol → destination)`, `trader` signing: every lamport (rent and
    /// the wrapped amount) goes to `destination` (A5: it can be the program-owned position).
    pub fn close_wsol(&self, destination: &AccountInfo<'info>) -> Result<()> {
        let w = self.wsol()?;
        let bump = [self.trader_bump];
        let seeds: &[&[u8]] = &[TRADER_SEED, self.market.as_ref(), &bump];
        token_interface::close_account(CpiContext::new_with_signer(
            TOKEN_PROGRAM_ID,
            CloseAccount {
                account: w.trader_wsol.clone(),
                destination: destination.clone(),
                authority: self.trader.clone(),
            },
            &[seeds],
        ))
    }

    /// `trader` signs a System transfer of `lamports` to `to` (0 is allowed: it puts both accounts
    /// into one CPI, which makes a direct lamport move between them visible to the runtime's
    /// per-CPI balance check, see `open_wsol`).
    pub fn pay_from_trader(&self, to: &AccountInfo<'info>, lamports: u64) -> Result<()> {
        let bump = [self.trader_bump];
        let seeds: &[&[u8]] = &[TRADER_SEED, self.market.as_ref(), &bump];
        system_program::transfer(
            CpiContext::new_with_signer(
                self.system_program.key(),
                Transfer {
                    from: self.trader.clone(),
                    to: to.clone(),
                },
                &[seeds],
            ),
            lamports,
        )
    }

    /// ARCH §2.4 rule 5 (I7), after a leg: `trader` holds 0 lamports and is System-owned with
    /// no data; `trader_wsol` is closed (0 lamports, no data) on the WSOL venues;
    /// `trader_base_ata` holds 0 tokens with its owner, delegate and close authority unchanged on
    /// the pump venues; the pool vault has no delegate (`DelegateNotCleared`; a sell leg's
    /// allowance clears itself at 0 [V12]).
    pub fn post_conditions(&self, pool_vault: &AccountInfo) -> Result<()> {
        require!(
            is_system_empty(&self.trader) && self.trader.lamports() == 0,
            ShortwinError::TraderStateInvalid
        );
        if let Some(w) = &self.wsol {
            require!(
                w.trader_wsol.lamports() == 0 && w.trader_wsol.data_is_empty(),
                ShortwinError::TraderStateInvalid
            );
        }
        if let Some(b) = &self.base {
            let t = base_ata_state(&self.trader, b)?;
            require!(t.amount == 0, ShortwinError::TraderStateInvalid);
        }
        let pv = token_state(pool_vault)?;
        require!(pv.delegate.is_none(), ShortwinError::DelegateNotCleared);
        Ok(())
    }
}
