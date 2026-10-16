//! `sweep_trader()` (ARCH §3.3, §3.9; A33, A44, R3A-10, R4A-7): **anyone** recovers value
//! left on a market's trader side between instructions — lamports donated to the `trader` PDA
//! or pre-funded to the `trader_wsol` address, and `ATA(trader, WSOL)` (anyone can create and
//! fund it; no leg uses it) — and credits it to lenders.
//!
//! **Why it exists in M2 (M2-SEC-4).** ARCH schedules it for M6, but M2's `sol_settle` makes a
//! full wipe reachable (lenders withdraw every free token, the last borrowed tokens settle S1),
//! and after a full wipe `deposit` waits for `wipe_trader_swept` (R4A-7), which only this
//! instruction sets unconditionally: the trading legs' normalisation sets it only when it moves
//! lamports, and no leg can run once `S = 0` (opens refuse a shareless pool, no position is
//! left to exit). Without it the market — the only one its mint can have — would take no
//! deposit, and hence no open, until a program upgrade.
//!
//! **Accounts** (ARCH §3.9, less the pump venues' token leg): `config` (read), `market` (mut,
//! bound to its PDA with the stored bump), `market_vault` (mut), `trader` (mut) and
//! `trader_wsol` (mut) at the market's stored keys, `trader_wsol_ata` (mut) =
//! `ATA(trader, WSOL)` under SPL Token (the key cached at listing, M5-SEC-6; passed whether or
//! not it exists, ARCH §3.0), SPL Token, System, event accounts. **Remaining:** on a pump-venue
//! market (`PumpCurve`, `PumpMigrating`, `PumpSwap`) first its token leg `[trader_base_ata (W),
//! pool_vault (W), mint, the mint's token program, the trader's pump UVA]` (M5; ARCH §3.9 "the
//! pump venues add `trader_base_ata` and `pool_vault`"; the UVA since M5-SEC-1; each key-checked
//! against the market) — on PumpSwap followed by `ATA(pAMM UVA(trader), WSOL)` (M6, below) —;
//! on any other market that has the trader's pump UVA (`Market.pump_uva` set: a curve market
//! that left pump, AIR-05, below) the UVA alone; then the latest `ShareEpoch` record
//! (`["share_epoch", market, share_epoch − 1]`, key-checked with its stored bump) exactly while a
//! full-wipe record is live (`S = 0 ∧ wipe_live`), nothing otherwise (`BadRemainingAccounts`).
//! The mock markets and the markets listed on Raydium have no trader base ATA and no pump UVA and
//! pass no token leg.
//!
//! **A market that left pump (AIR-05).** A curve market that graduated elsewhere (`→ None`, then
//! possibly a Raydium pool) keeps its trader's pump UVA, and the curve-era cashback in it stays
//! claimable; `engine::lender::rewards_required` keeps the venue-reward bit required there, and
//! this instruction reads the UVA (its one-account token leg, key-checked against
//! `Market.pump_uva`) and sets the bit when it holds nothing claimable, as on the curve kinds
//! below. Its base ATA is not touched off the pump venues (no leg there uses it).
//!
//! **The venue-reward bit on the curve (M5-SEC-1; a deviation from ARCH §3.3 / §3.9, flagged).**
//! A deposit into a live full-wipe record of a `PumpCurve` market waits for
//! `wipe_rewards_claimed` as well (R4A-7: pre-wipe pump cashback must reach the wiped epoch's
//! lenders), which ARCH has `claim_venue_rewards` set — an M6 instruction. Without it an organic
//! full wipe on a live curve locked the market's deposits, and hence its opens, for as long as
//! the curve stayed live. On the curve kinds (`PumpCurve`, `PumpMigrating`) this instruction
//! therefore also sets `wipe_rewards_claimed` (through `SweepSource::VenueRewards`, 0 lamports)
//! **when the trader's pump UVA holds no claimable cashback** — its lamports above its
//! rent-exempt minimum, which is exactly what pump's `claim_cashback` pays out (pump's
//! `PUMP_CASHBACK_README`; an account pump does not own holds none). Cashback accrues only on the
//! trader's own trades (legacy cashback coins), none of which can run once `S = 0`, so "nothing
//! claimable now" means every pre-wipe cashback lamport has already reached `market_vault`
//! through the trader (earlier normalisations, or this sweep). With cashback outstanding the bit
//! stays clear; pump's `claim_cashback` is permissionless (`user` does not sign; T-CASHBACK, pump
//! part, verified on the dumped ELF by `m5_sec_curve_it`), so anyone unlocks the market with
//! `[pump claim_cashback(user = trader), sweep_trader]` in one transaction: the claim pays the
//! trader, this sweep credits the wipe record and sets both bits. No pump CPI here, so neither a
//! change of pump's claim interface nor a venue that cannot execute can lock deposits (no M1-FID-12
//! deadlock; `engine::lender::rewards_required` documents why the bit can always be set).
//!
//! **PumpSwap (M6; the M5-SEC-1 remainder, ARCH §3.9 "M6's choice").** PumpSwap accrues the
//! trader's cashback as WSOL in `ATA(pAMM UVA(trader), WSOL)` (R-VEN Update H7), so a PumpSwap
//! market's token leg adds that ATA (6 accounts; key-checked against the market's PumpSwap cache),
//! and the bit is set when it is missing or holds 0 WSOL **and** the trader's pump UVA holds
//! nothing claimable either (curve-era cashback of a graduated market; a market listed on PumpSwap
//! never had a pump UVA). With cashback outstanding anyone bundles `[create_idempotent
//! ATA(trader, WSOL), pAMM claim_cashback(user = trader, user_wsol = ATA(trader, WSOL)),
//! sweep_trader]`: pAMM's `claim_cashback` needs no signature of the trader (T-CASHBACK, pAMM
//! part, verified on the dumped ELF), the claim moves the ATA's whole balance into
//! `ATA(trader, WSOL)`, and this sweep closes that account into the trader, credits the wipe
//! record and sets both bits (the bundler's 1,488,440 lamports of ATA rent are its donation to the
//! lenders). This is the pattern ARCH §3.9 offers in place of `claim_venue_rewards`, which is not
//! built: an unconditional pAMM `claim_cashback` CPI reverts (3012) whenever the UVA's ATA does not
//! exist — the normal state of a non-cashback market — and would need the venue program to run.
//!
//! **Effects, in order:**
//! 1. `CloseAccount(ATA(trader, WSOL) → trader)` if it is an initialised token account
//!    (`engine::trader::close_trader_ata`), then the §2.4 normalisation: every lamport of
//!    `trader` and of the `trader_wsol` address moves to `market_vault`
//!    (`engine::trader::normalise_pdas`; `TraderStateInvalid` if either is not an empty System
//!    account); on a pump venue every token of `trader_base_ata` moves to `pool_vault` (a
//!    donation: `T` is unchanged; `engine::trader::sweep_base_ata`, which requires the account
//!    to be the trader's, with no delegate and no close authority, `TraderStateInvalid`).
//! 2. The market prologue of a lender instruction (ARCH §3.0 steps 1–5, A16: params-version
//!    sync, accrue with the vest release, roll a completed bucket; no ReadSet, so no record).
//! 3. `pool::sweep(…, SweepSource::Trader)`: `lender_sol += swept`; while `S = 0` (a live
//!    full-wipe record or, since AIR-08, a market every lender left) `wipe_trader_swept` is set
//!    **even when nothing moved**; the SOL tops up a live record, vests over `lump_vest_s` with
//!    `S > 0`, and goes to `protocol_fees` with `S = 0` and no live wipe (R3A-10). On the pump
//!    venues, and off them on a market with a pump UVA (AIR-05), while `S = 0` and the trader
//!    holds no claimable cashback, `pool::sweep(…, 0, SweepSource::VenueRewards)` also sets
//!    `wipe_rewards_claimed` (M5-SEC-1, above; AIR-08: a deposit into a market every lender left
//!    waits for both bits too, so the next depositor cannot take the cashback of the earlier
//!    lenders' trading that KI-10 gives the protocol).
//! 4. Post-conditions (step 10): `trader` and `trader_wsol` hold 0 lamports; `market_vault`
//!    holds rent + `lender_sol + protocol_fees` (I2).
//! 5. Event `TraderSwept{lamports, tokens, to, wipe_bits}` (`wipe_bits`: the math crate's
//!    `wipe_swept` bits, `WIPE_TRADER_SWEPT` = 1, `WIPE_REWARDS_CLAIMED` = 2, plus 0x80 while
//!    `wipe_live`).
//!
//! Sweeping nothing succeeds, so a keeper can always call it (ECON §5.8 duty 6: right after a
//! full wipe, before any deposit). It moves no principal and no position's SOL.

use anchor_lang::prelude::*;
use anchor_lang::AccountDeserialize;
use shortwin_math::pool::{self, SweepSource};

use crate::constants::{CONFIG_VERSION, MARKET_VERSION, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID};
use crate::engine::trader::{
    close_trader_ata, normalise_pdas, sweep_base_ata, BaseSide, TraderPdas,
};
use crate::engine::{lender, prologue};
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::{TraderSwept, SWEEP_TO_PROTOCOL, SWEEP_TO_VEST, SWEEP_TO_WIPE_RECORD};
use crate::state::{pump_swap_cache, GlobalConfig, Market, MarketVault, ShareEpoch, VenueKind};
use crate::venues::{pump_amm, pump_curve};

/// Length of a pump-curve market's token leg in the remaining accounts: `trader_base_ata`,
/// `pool_vault`, the mint, its token program and the trader's pump UVA.
pub const TOKEN_LEG_LEN: usize = 5;
/// Length of a PumpSwap market's token leg (M6): the curve's five, then `ATA(pAMM UVA(trader),
/// WSOL)` — where PumpSwap accrues the trader's cashback.
pub const TOKEN_LEG_LEN_PAMM: usize = 6;
/// Length of the token leg of a market off the pump venues that has the trader's pump UVA
/// (AIR-05: a curve market that left pump): the UVA alone.
pub const TOKEN_LEG_LEN_UVA: usize = 1;
/// Index of the trader's pump UVA in a pump market's token leg (and in the market's leg keys).
const LEG_PUMP_UVA: usize = 4;

/// The cashback pump's `claim_cashback` would pay the trader now (module docs, M5-SEC-1): the
/// lamports of its pump UVA above the account's rent-exempt minimum; 0 for an account pump does
/// not own (never created: nothing accrued).
fn pump_cashback_claimable(uva: &AccountInfo) -> Result<u64> {
    if *uva.owner != PUMP_PROGRAM_ID {
        return Ok(0);
    }
    let floor = Rent::get()?.minimum_balance(uva.data_len());
    Ok(uva.lamports().saturating_sub(floor))
}

/// `TraderSwept.wipe_bits` flag: a full-wipe record is live after the sweep.
pub const WIPE_BITS_LIVE: u8 = 0x80;

#[event_cpi]
#[derive(Accounts)]
pub struct SweepTrader<'info> {
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// Bound to its PDA `["market", mint]` with the stored bump (`Market::is_pda`; no mint is
    /// passed).
    #[account(
        mut,
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = market_vault @ ShortwinError::AccountMismatch,
        has_one = trader @ ShortwinError::AccountMismatch,
        has_one = trader_wsol @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    /// CHECK: `== market.trader` (`has_one`); a System account with no data (checked by the
    /// normalisation).
    #[account(mut)]
    pub trader: UncheckedAccount<'info>,
    /// CHECK: `== market.trader_wsol` (`has_one`); a System account with no data between
    /// instructions (checked by the normalisation).
    #[account(mut)]
    pub trader_wsol: UncheckedAccount<'info>,
    /// CHECK: `ATA(trader, WSOL)` under SPL Token (key-checked by the handler); may not exist.
    #[account(mut)]
    pub trader_wsol_ata: UncheckedAccount<'info>,
    /// CHECK: SPL Token (the WSOL side's program).
    #[account(address = TOKEN_PROGRAM_ID @ ShortwinError::AccountMismatch)]
    pub token_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(ctx: Context<'info, SweepTrader<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let (trader_bump, trader_wsol_bump, pump, pamm, uva_only, token_keys, decimals, wsol_ata) = {
        let m = ctx.accounts.market.load()?;
        let kind = m.venue_kind();
        let pump = matches!(
            kind,
            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
        );
        let pamm = kind == Some(VenueKind::PumpSwap);
        // AIR-05: off the pump venues, a market that kept its trader's pump UVA reads it.
        let uva_only = !pump && m.pump_uva != Pubkey::default();
        (
            m.trader_bump,
            m.trader_wsol_bump,
            pump,
            pamm,
            uva_only,
            [
                m.trader_base_ata,
                m.pool_vault,
                m.mint,
                m.token_program,
                m.pump_uva,
                pump_amm::cached(&m, pump_swap_cache::PAMM_UVA_QUOTE_ATA),
            ],
            m.decimals,
            pump_curve::trader_wsol_ata(&m),
        )
    };
    // A token leg comes first in the remaining accounts (module docs): a pump market's, or off
    // the pump venues the trader's pump UVA alone (AIR-05).
    let (leg_len, first_key) = if pamm {
        (TOKEN_LEG_LEN_PAMM, 0)
    } else if pump {
        (TOKEN_LEG_LEN, 0)
    } else if uva_only {
        (TOKEN_LEG_LEN_UVA, LEG_PUMP_UVA)
    } else {
        (0, 0)
    };
    let (token_leg, rem) = if leg_len > 0 {
        require!(
            ctx.remaining_accounts.len() >= leg_len,
            ShortwinError::BadRemainingAccounts
        );
        let (leg, rest) = ctx.remaining_accounts.split_at(leg_len);
        for (info, key) in leg.iter().zip(token_keys.iter().skip(first_key)) {
            require_keys_eq!(info.key(), *key, ShortwinError::AccountMismatch);
        }
        (Some(leg), rest)
    } else {
        (None, ctx.remaining_accounts)
    };
    require_keys_eq!(
        ctx.accounts.trader_wsol_ata.key(),
        wsol_ata,
        ShortwinError::AccountMismatch
    );
    // M5-SEC-1 (the curve kinds), its M6 remainder (PumpSwap) and AIR-05 (a market that left
    // pump): whether the trader holds no claimable venue cashback — its pump UVA's lamports above
    // rent, and on PumpSwap also the WSOL of its pAMM UVA's ATA (read before anything moves;
    // nothing below touches either).
    let rewards_clear = match token_leg {
        Some(leg) if uva_only => {
            let pump_uva = leg.first().ok_or(ShortwinError::BadRemainingAccounts)?;
            pump_cashback_claimable(pump_uva)? == 0
        }
        Some(leg) => {
            let pump_uva = leg
                .get(LEG_PUMP_UVA)
                .ok_or(ShortwinError::BadRemainingAccounts)?;
            let pamm_clear = match leg.get(5) {
                Some(ata) => pump_amm::cashback_claimable(ata) == 0,
                None => true,
            };
            pump_cashback_claimable(pump_uva)? == 0 && pamm_clear
        }
        None => false,
    };

    // ---- 1. the trader side's lamports into `market_vault` (untracked until booked) --------
    let accs = &ctx.accounts;
    let from_ata = close_trader_ata(
        &market_key,
        &accs.trader.to_account_info(),
        trader_bump,
        &accs.trader_wsol_ata.to_account_info(),
        &accs.token_program.to_account_info(),
    )?;
    let swept = normalise_pdas(&TraderPdas {
        market: market_key,
        trader: accs.trader.to_account_info(),
        trader_bump,
        trader_wsol: accs.trader_wsol.to_account_info(),
        trader_wsol_bump,
        market_vault: accs.market_vault.to_account_info(),
        system_program: accs.system_program.to_account_info(),
    })?;
    require_gte!(swept, from_ata, ShortwinError::TraderStateInvalid);
    let tokens = match token_leg {
        Some([ata, pool_vault, mint, program, ..]) if pump => sweep_base_ata(
            &market_key,
            &accs.trader.to_account_info(),
            trader_bump,
            &BaseSide {
                trader_base_ata: ata.clone(),
                pool_vault: pool_vault.clone(),
                mint: mint.clone(),
                decimals,
                token_program: program.clone(),
            },
        )?,
        _ => 0,
    };

    // ---- 2.–3. the prologue and the booking ------------------------------------------------
    let (to, wipe_bits, tracked) = {
        let config = ctx.accounts.config.load()?;
        let mut market = ctx.accounts.market.load_mut()?;
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let now = prologue::market_now(&market, clock.unix_timestamp);
        prologue::sync_market_params(&config, &mut market, now);
        let mut pool = market.pool();
        let mut vault = vault_acc.vault();
        prologue::accrue(&mut market, &mut pool, &mut vault, now)?;
        prologue::roll_only(&market_key, &mut market, now, clock.slot)?;

        let wipe_live = pool.total_shares == 0 && pool.is_wipe_live();
        let to = if pool.total_shares > 0 {
            SWEEP_TO_VEST
        } else if wipe_live {
            SWEEP_TO_WIPE_RECORD
        } else {
            SWEEP_TO_PROTOCOL
        };
        if wipe_live {
            // The live full-wipe record: the latest one (the epoch the wipe closed).
            require!(rem.len() == 1, ShortwinError::BadRemainingAccounts);
            let recs = lender::load_records(&market_key, rem, 1)?;
            let mut rec = *recs.first().ok_or(ShortwinError::EpochRecordMismatch)?;
            require!(
                rec.epoch.checked_add(1) == Some(pool.share_epoch) && rec.is_full_wipe(),
                ShortwinError::EpochRecordMismatch
            );
            pool::sweep(
                &mut pool,
                &mut vault,
                Some(&mut rec),
                now,
                config.params.lump_vest_s,
                swept,
                SweepSource::Trader,
            )
            .math()?;
            if rewards_clear {
                // M5-SEC-1: no pre-wipe cashback left to claim — the venue-reward sweep of
                // nothing (ARCH §3.9: "even if nothing was claimable").
                pool::sweep(
                    &mut pool,
                    &mut vault,
                    Some(&mut rec),
                    now,
                    config.params.lump_vest_s,
                    0,
                    SweepSource::VenueRewards,
                )
                .math()?;
            }
            let info = rem.first().ok_or(ShortwinError::EpochRecordMismatch)?;
            require!(info.is_writable, ShortwinError::EpochRecordMismatch);
            let mut account = {
                let data = info.try_borrow_data()?;
                ShareEpoch::try_deserialize(&mut &data[..])
                    .map_err(|_| error!(ShortwinError::EpochRecordMismatch))?
            };
            account.set_record(&rec);
            let mut data = info.try_borrow_mut_data()?;
            let mut w: &mut [u8] = &mut data;
            account.try_serialize(&mut w)?;
        } else {
            require!(rem.is_empty(), ShortwinError::BadRemainingAccounts);
            pool::sweep(
                &mut pool,
                &mut vault,
                None,
                now,
                config.params.lump_vest_s,
                swept,
                SweepSource::Trader,
            )
            .math()?;
            if rewards_clear && pool.total_shares == 0 {
                // AIR-08: after the last lender's exit a deposit waits for the sweeps too, the
                // venue-reward bit on a pump venue included: set it as after a full wipe
                // (M5-SEC-1), once the trader holds no claimable cashback.
                pool::sweep(
                    &mut pool,
                    &mut vault,
                    None,
                    now,
                    config.params.lump_vest_s,
                    0,
                    SweepSource::VenueRewards,
                )
                .math()?;
            }
        }
        market.set_pool(&pool);
        vault_acc.set_vault(&vault);
        let live_bit = if pool.is_wipe_live() {
            WIPE_BITS_LIVE
        } else {
            0
        };
        (to, pool.wipe_swept | live_bit, lender::tracked(&vault)?)
    };

    // ---- 4. post-conditions ------------------------------------------------------------------
    require!(
        ctx.accounts.trader.lamports() == 0 && ctx.accounts.trader_wsol.lamports() == 0,
        ShortwinError::TraderStateInvalid
    );
    lender::check_vault_floor(&ctx.accounts.market_vault.to_account_info(), tracked)?;

    emit_cpi!(TraderSwept {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        lamports: swept,
        tokens,
        to,
        wipe_bits,
    });
    Ok(())
}
