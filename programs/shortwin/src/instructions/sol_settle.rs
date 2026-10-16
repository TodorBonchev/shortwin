//! `sol_settle()` (ARCH §3.8; ECON §5.6, §8.6, §10.3; SPEC §5.3, D3, O15): a keeper settles a
//! position in SOL instead of buying back its tokens — S1 (a real gap: lenders take `V − K_s`,
//! never capped), S2 (buyback slippage above `σ_max`) or S3 (no executable route for the S3
//! timer) — or, most of the time, arms, keeps or disarms the position's settlement clock.
//!
//! **Signer:** `keeper` (anyone; receives `K_s`; pays for a `ShareEpoch` record when the
//! settlement rolls the share epoch). **Named accounts:** `config`, `market` (mut), `position`
//! (mut; closed to `borrower` only when a settlement executes), `borrower` (mut, `==
//! position.borrower`), `exposure` (mut), `market_vault` (mut), `mint` (read), `share_epoch_rec`
//! (mut; used — and then key-checked against `PDA(["share_epoch", market, epoch])` of the epoch
//! the roll closes, `EpochRecordMismatch` — only when the settlement rolls the epoch), System,
//! event accounts. **Remaining:** the venue's `ReadSet` only (C0-2): no venue CPI, no token
//! movement. A `PumpMigrating` market passes its curve's ReadSet; a list-only (`None`) market —
//! a curve that graduated elsewhere with positions open — its detection set `[bonding_curve,
//! canonical_pool]` (M2-SEC-5, M5: S3 is the only settlement on both, ARCH §3.8 step 4, §5.3).
//!
//! **Exits never read the latch, the oracle, a pause flag or the route-broken flag as a gate**
//! (I10): none of those accounts is passed. The Emergency flag reaches this instruction only
//! as S3 evidence, through `s3_no_route` (the prologue's S3 chain, and the position's "no route
//! now", `engine::route::position_no_route`, which also counts a live fee above the position's
//! locked hard cap, AIR-19; V2-1): it never makes the spot leg true and never enters a quote,
//! the box, the arming clock or `pool_fail` (T-S3-BROKEN).
//!
//! **Steps** (ARCH §3.8; the order equals `shortwin_sim.Market.try_exit(settle_only = True)`):
//!
//! 1. The prologue (`engine::prologue::run`): accrual, the observation roll, this call's
//!    pre-trade observation (only while the venue executes), the route chains. A correctly
//!    keyed venue-state account that no longer parses is "not executable" (A63), never a
//!    revert. The mint guard on the exit path (a failing filter is a `MintSafetyTripped` log
//!    event, never a revert). Then `R_due`, `V_eff`, `K_s`, the legs and the settlement box
//!    (`engine::arming::inputs`).
//! 2. – 4. `shortwin_math::settle::evaluate` (see `engine::arming`): not liquidatable and not
//!    expired → disarm only on the box, else keep the clock; on an executable venue the box
//!    refutation, then the S1 / S2 legs on one clock with `settle_persist_n` EMA updates, the
//!    kind chosen at execution (S1 if its legs hold, else S2; an S2 whose amount is not
//!    box-consistent waits); S3 (no arming) when due on the market's continuous chain with the
//!    position's locked timers. **M4R gate** (**PROPOSED**, owner item O18): while the
//!    post-change warm-up runs on an executable venue (`Market.ema_warm > 0` after the roll)
//!    nothing is evaluated — no arm change, no settlement, no S3 — and the call is an `Ok` crank;
//!    a venue that cannot execute is never gated.
//! 5. **Every "no change" outcome is an `Ok` no-op** that keeps the prologue (its observation
//!    and route chains), so the keeper's per-second call doubles as a crank. `SettleArmed` /
//!    `SettleDisarmed` are emitted when the clock changed state; a clock that only keeps
//!    running (`s_last = now`) emits nothing.
//!
//! **An executing settlement** (waterfall `settle`, ARCH §5.2):
//!
//! - `K_s` (locked terms) to the keeper; the lenders' `x` (S1 `V − K_s`; S2/S3 `x(P_ema,
//!   L_ema)`, box-consistent) to `market_vault` through `pool::settle` — on every kind (S1
//!   since AIR-21) the part above `⌊Q·P_ema/WAD⌋ + R_due` vests as a lump (on a venue that
//!   cannot execute the part above `R_due`: `P_ema` is frozen there, AIR-09), the rest is
//!   immediate (R4B-6) but for its rent top-up: the immediate part pays `R_due` first, and the
//!   part of that above the rent credited while the position was open (`pool::rent_top_up`)
//!   vests with the lump
//!   (finding M10-8C-1, every kind); the token debt is extinguished; the protocol gets nothing
//!   (SPEC §6.3);
//! - the position's aggregates leave the market (`open_positions`, `ΣB_Q` by tier, `A_j`,
//!   notional), the exposure falls by `q` (closed at 0), and the position — with the borrower's
//!   `V − K_s − x` and its rent — is closed to the borrower (A18);
//! - a settlement that leaves at most 1/1,000 of the token claims it found (a full wipe, or
//!   the dust a run or a deposit before it leaves; AIR-07, `pool::VEST_FLUSH_RATIO`) releases
//!   the unreleased vest, its own lump included, to the shares that held them inside
//!   `pool::settle`, and `SolSettle.vest_flushed` reports it, with or without a roll (the
//!   indexer's `vest_bal`);
//! - `pool::maybe_roll` inside `pool::settle`: when the epoch rolls (a full wipe, or a rebase),
//!   the `ShareEpoch` record is created with the grief-proof helper (payer keeper; lamports a
//!   third party pre-funded are swept to `market_vault` and credited to lenders as a lump —
//!   the live wipe record's own top-up when the roll is a full wipe) and `EpochRolled` emitted
//!   (`vest_flushed`: the vest the settlement flushed, as in `SolSettle`; 0 when it did not).
//!
//! Lamport moves out of the position are direct (no CPI) and come after every CPI that names
//! the keeper or `market_vault` (a direct move is visible to the runtime's per-CPI balance
//! check only once both accounts are in a CPI).
//!
//! **Event:** `SolSettle{condition, b_live, b_rs, b_ema, p_lo, p_hi, l_lo, l_hi, x_lo, x_hi,
//! pprime_lo, pprime_hi, fv, k_s, lender_amount, lump, borrower_amount, ema_age_s,
//! vest_flushed}` — the box corners' amounts and `p'` for S2/S3 (0 for S1), `fv = 0` for S1 or
//! when undefined, `b_* = 0` when undefined; `ema_age_s = now − ema_ts` (V2-5); `vest_flushed`
//! the vest the settlement flushed (AIR-07), 0 when it did not. `SettleArmed.reason` is 0
//! (armed on the S1 or the S2 legs; the kind is chosen at execution).

use anchor_lang::prelude::*;
use shortwin_math::pool::{self, Resolution};
use shortwin_math::rent;
use shortwin_math::settle::{self, ArmChange, BoxQuotes, DisarmReason, Evaluation, Settlement};
use shortwin_math::waterfall::{self, Payout};

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::arming::{self, Facts, RiskQuotes};
use crate::engine::guard::{mint_guard, MintPath};
use crate::engine::{bounds, lender, prologue, waterfall as engine_waterfall};
use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::events::{
    EpochRolled, MintSafetyTripped, SettleArmed, SettleDisarmed, SolSettle as SolSettleEvent,
    SETTLE_REASON_NOT_LIQUIDATABLE, SETTLE_REASON_REFUTED,
};
use crate::lamports;
use crate::state::{GlobalConfig, Market, MarketVault, ShareEpoch, ShortPosition, WalletExposure};
use crate::venues::{self, Carry};

#[event_cpi]
#[derive(Accounts)]
pub struct SolSettle<'info> {
    /// The keeper (anyone); pays for a new `ShareEpoch` record.
    #[account(mut)]
    pub keeper: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump = market.load()?.bump,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = mint @ ShortwinError::AccountMismatch,
        has_one = market_vault @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        has_one = borrower @ ShortwinError::AccountMismatch,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = position.is_pda(&position.key()) @ anchor_lang::error::ErrorCode::ConstraintSeeds
    )]
    pub position: Box<Account<'info, ShortPosition>>,
    /// CHECK: `== position.borrower` (constraint on `position`).
    #[account(mut)]
    pub borrower: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [WalletExposure::SEED, market.key().as_ref(), borrower.key().as_ref()],
        bump = exposure.bump,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = exposure.wallet == borrower.key() @ ShortwinError::AccountMismatch
    )]
    pub exposure: Box<Account<'info, WalletExposure>>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    /// CHECK: owner (the market's token program) checked by the track's handler.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: key checked by the handler against `PDA(["share_epoch", market,
    /// market.share_epoch])`; created with the grief-proof helper only when the epoch rolls.
    #[account(mut)]
    pub share_epoch_rec: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// An executing settlement as phase 1 booked it.
struct Booked {
    settlement: Settlement,
    payout: Payout,
    pool: pool::Pool,
    vault: Box<pool::Vault>,
    rec: Option<pool::ShareEpoch>,
    /// The vest `pool::settle` flushed (`Settled::flushed`: a full wipe, or AIR-07's dust):
    /// `vest_bal` before it plus what it vested (`Settled::vested`, the lump and the rent
    /// top-up); 0 when it did not flush. `SolSettle` and `EpochRolled` report it.
    vest_flushed: u64,
    lump_vest_s: u32,
}

/// What phase 1 (the prologue and the evaluation) hands to the effects.
struct Evaluated {
    now: i64,
    ev: Evaluation,
    arm: settle::ArmState,
    /// The prologue's venue sync moved a curve market (`VenueStateChanged`).
    transition: Option<crate::events::VenueStateChanged>,
    facts: Facts,
    bx: settle::SettleBox,
    ema_age_s: i64,
    b_ema: Option<u64>,
    mint_ok: bool,
    booked: Option<Booked>,
}

/// ARCH §3.8 steps 1–4 (phase 1: the prologue and the evaluation; module docs). Its own frame:
/// the handler's would exceed SBF's 4 KiB.
#[inline(never)]
fn evaluate<'info>(
    ctx: &Context<'info, SolSettle<'info>>,
    clock: &Clock,
    pos: &ShortPosition,
    market_key: Pubkey,
) -> Result<Box<Evaluated>> {
    let config = ctx.accounts.config.load()?;
    let mut market = ctx.accounts.market.load_mut()?;
    let kind = market.venue_kind().ok_or(ShortwinError::NoSupportedVenue)?;
    let token_program = market.token_program;
    let mint_ok = mint_guard(
        &market,
        &ctx.accounts.mint.to_account_info(),
        &token_program,
        MintPath::Exit,
    )?;
    let rem = venues::parse(&market, kind, ctx.remaining_accounts, Carry::ReadSet, &[])?;
    // AIR-10: on PumpSwap the risk view also at most φ at `P_ema` (`exit_risk_cap_bps`).
    let state = venues::read_state_boxed_for_exit(
        &market,
        kind,
        rem.read,
        clock.unix_timestamp,
        venues::mint_supply(&ctx.accounts.mint.to_account_info()),
    );
    let risk = venues::risk_quoter(
        &market,
        &state,
        rem.read,
        state.exit_risk_cap_bps(u64::from(pos.venue_fee_hard_cap_bps)),
    )
    .ok_or(ShortwinError::NoSupportedVenue)?;
    let mut vault = ctx.accounts.market_vault.load()?.vault();
    let tracked_before = (vault.lender_sol, vault.unmatched, vault.protocol_fees);
    let pro = prologue::run(
        &config,
        &market_key,
        &mut market,
        &mut vault,
        Some(&*state),
        clock.unix_timestamp,
        clock.slot,
    )?;
    let now = pro.now;
    let transition = pro
        .transition
        .map(|t| venues::pump_curve::transition_event(&market, t, market_key, clock));
    let obs = market.observation();
    let mut quotes = RiskQuotes::new(risk, pos.q);
    // AIR-15: the box's `L_mhi` is capped where the exit reference's EMA terms are (M4F-1,
    // M6R-1: `pool_d` just refreshed by the prologue over `λ_min`; `None` without a locked depth).
    let depth_cap = bounds::ref_depth_cap(&config, &market, kind);
    // AIR-19: "no route now" is the position's own — a live fee above the cap it locked is no
    // route for it under any global cap (its keeper exits are bounded at that cap); the
    // prologue's chain ran at the lowest cap an open position locked.
    let pos_s3_no_route = crate::engine::route::position_no_route(
        &config,
        &market,
        &state,
        pos.venue_fee_hard_cap_bps,
    );
    let (inp, facts) = arming::inputs(
        pos,
        &market,
        &obs,
        &state,
        &mut quotes,
        pos_s3_no_route,
        depth_cap,
        now,
    )?;
    let mut arm = pos.arm_state();
    // `→ PumpSwap` in this prologue (M6): the ReadSet is the curve's and the observation state
    // was just restarted onto the pool — nothing is evaluated on it (the post-change warm-up's
    // no-op, ARCH §3.8 step 1b); the next call reads the pool.
    let graduated = pro
        .transition
        .is_some_and(|t| t.to == crate::state::VenueKind::PumpSwap);
    let ev = if graduated {
        Evaluation::default()
    } else {
        settle::evaluate(&mut arm, &inp, &arming::terms(pos), &mut quotes).math()?
    };

    let booked = match ev.settlement {
        None => {
            // A no-op keeps the prologue. Its accrual touches the vault only while `S = 0`,
            // which an open position excludes (M1-SEC-3); stored if it did anyway.
            if (vault.lender_sol, vault.unmatched, vault.protocol_fees) != tracked_before {
                ctx.accounts.market_vault.load_mut()?.set_vault(&vault);
            }
            None
        }
        Some(s) => {
            // AIR-09: a venue that cannot execute records nothing, so `P_ema` is its pre-halt
            // value and the box its point: the extinguished tokens have no market value that
            // could be paid at once. Priced at 0, the whole token part of the take vests and
            // only `R_due` stays immediate (S1 and S2 run on an executable venue only).
            let p_imm = if inp.executable { inp.p_ema } else { 0 };
            let payout =
                waterfall::settle(pos.v, s.kind, s.k_s, s.fv, pos.q, p_imm, inp.r_due).math()?;
            require!(
                payout.keeper == s.k_s
                    && payout.lenders == s.lenders
                    && payout.borrower == s.borrower,
                ShortwinError::MathOverflow
            );
            let credited = rent::credited_for(&pos.pos_rent(), &market.basis()).math()?;
            let mut pool = pro.pool;
            let vest_before = pool.vest_bal;
            let settled = pool::settle(
                &mut pool,
                &mut vault,
                now,
                config.params.lump_vest_s,
                &Resolution {
                    q: pos.q,
                    credited,
                    lump: payout.lump,
                },
                payout.lenders,
                inp.r_due,
            )
            .math()?;
            // A flush releases the vest the settlement found and its own vest at once (a full
            // wipe always flushes; since AIR-07 so does a settlement that left dust, with or
            // without a roll).
            let vest_flushed = if settled.flushed {
                vest_before.checked_add(settled.vested).or_math()?
            } else {
                0
            };
            let mut aggr = market.aggr();
            engine_waterfall::remove_position(&mut market, &mut aggr, pos)?;
            market.set_aggr(&aggr);
            market.set_pool(&pool);
            Some(Booked {
                settlement: s,
                payout,
                pool,
                vault,
                rec: settled.rec,
                vest_flushed,
                lump_vest_s: config.params.lump_vest_s,
            })
        }
    };
    let b_ema = if booked.is_some() {
        quotes.cost(inp.p_ema, inp.l_ema).math()?
    } else {
        None
    };
    Ok(Box::new(Evaluated {
        now,
        ev,
        arm,
        transition,
        facts,
        bx: inp.bx,
        ema_age_s: now.saturating_sub(obs.ema.ts),
        b_ema,
        mint_ok,
        booked,
    }))
}

pub fn handler<'info>(ctx: Context<'info, SolSettle<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let position_key = ctx.accounts.position.key();
    let keeper_key = ctx.accounts.keeper.key();
    let pos = (**ctx.accounts.position).clone();

    // ---- phase 1: steps 1–4 ----------------------------------------------------------------
    let mut e = evaluate(&ctx, &clock, &pos, market_key)?;
    if let Some(ev) = e.transition.take() {
        emit_cpi!(ev);
    }
    if !e.mint_ok {
        emit!(MintSafetyTripped {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            mint: ctx.accounts.mint.key(),
        });
    }
    // The clock's state changes (steps 2 and 3; ARCH §3.10). A disarm and an S3 can come in
    // one call (refuted on the box, then S3 due).
    let (s_count, s_since) = (e.arm.count, e.arm.since);
    match e.ev.arm {
        ArmChange::Armed => emit_cpi!(SettleArmed {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            position: position_key,
            s_count,
            s_since,
            reason: 0,
        }),
        ArmChange::Disarmed(why) => emit_cpi!(SettleDisarmed {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            position: position_key,
            s_count,
            s_since,
            reason: match why {
                DisarmReason::Refuted => SETTLE_REASON_REFUTED,
                DisarmReason::NotLiquidatable => SETTLE_REASON_NOT_LIQUIDATABLE,
            },
        }),
        ArmChange::None | ArmChange::Kept => {}
    }

    let Some(mut b) = e.booked else {
        // Step 5: no settlement. Only the clock is written (Anchor persists the position).
        ctx.accounts.position.set_arm_state(&e.arm);
        return Ok(());
    };

    // ---- the share-epoch record: its CPIs come before any direct lamport move --------------
    let accs = &ctx.accounts;
    let mut rolled = None;
    if let Some(mut rec) = b.rec {
        let epoch_le = rec.epoch.to_le_bytes();
        let (expected, bump) = Pubkey::find_program_address(
            &[ShareEpoch::SEED, market_key.as_ref(), &epoch_le],
            &crate::ID,
        );
        require_keys_eq!(
            accs.share_epoch_rec.key(),
            expected,
            ShortwinError::EpochRecordMismatch
        );
        let bump_seed = [bump];
        let seeds: &[&[u8]] = &[ShareEpoch::SEED, market_key.as_ref(), &epoch_le, &bump_seed];
        let rec_info = accs.share_epoch_rec.to_account_info();
        // A record of an epoch not yet closed cannot exist (only this roll writes it); the
        // helper checks it again.
        require!(
            *rec_info.owner == System::id() && rec_info.data_is_empty(),
            ShortwinError::EpochRecordMismatch
        );
        let swept = arming::create_pda_account(
            &accs.keeper.to_account_info(),
            &rec_info,
            &accs.market_vault.to_account_info(),
            &accs.system_program.to_account_info(),
            seeds,
            ShareEpoch::SPACE,
            &crate::ID,
        )?;
        if swept > 0 {
            // A donation to the record's address: lender SOL, vested as a lump (with `S = 0`
            // after a full wipe it tops up the wipe record it was sent to).
            b.vault.lender_sol = b.vault.lender_sol.checked_add(swept).or_math()?;
            let wipe = (b.pool.total_shares == 0 && b.pool.wipe_live != 0).then_some(&mut rec);
            pool::vest(&mut b.pool, &mut b.vault, wipe, e.now, b.lump_vest_s, swept).math()?;
            accs.market.load_mut()?.set_pool(&b.pool);
        }
        let record = ShareEpoch {
            market: market_key,
            epoch: rec.epoch,
            acc_end: rec.acc_end,
            acc_r_end: rec.acc_r_end,
            s_old: rec.s_old,
            carry: rec.carry,
            carry_r: rec.carry_r,
            // 0: no lender has converted through the new record yet (KI-65).
            conv_rem: rec.conv_rem,
            num: rec.num,
            den_log1000: rec.den_log1000,
            bump,
        };
        {
            let mut data = rec_info.try_borrow_mut_data()?;
            let mut w: &mut [u8] = &mut data;
            record.try_serialize(&mut w)?;
        }
        rolled = Some(EpochRolled {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            epoch: rec.epoch,
            acc_end: rec.acc_end,
            num: rec.num,
            den: rec.den().math()?,
            vest_flushed: b.vest_flushed,
        });
    }

    // ---- the waterfall's lamports: direct moves, after every CPI above ---------------------
    let position_info = accs.position.to_account_info();
    let vault_info = accs.market_vault.to_account_info();
    lamports::move_lamports(
        &position_info,
        &accs.keeper.to_account_info(),
        b.payout.keeper,
    )?;
    lamports::move_lamports(&position_info, &vault_info, b.payout.lenders)?;
    let tracked = {
        let mut va = accs.market_vault.load_mut()?;
        va.set_vault(&b.vault);
        lender::tracked(&b.vault)?
    };
    lender::check_vault_floor(&vault_info, tracked)?;

    let exposure = &mut ctx.accounts.exposure;
    exposure.q_open = exposure.q_open.checked_sub(pos.q).or_math()?;
    if exposure.q_open == 0 {
        let to = ctx.accounts.borrower.to_account_info();
        ctx.accounts.exposure.close(to)?;
    }
    // A18: the borrower's `V − K_s − x` and the account's rent go to the borrower.
    let to = ctx.accounts.borrower.to_account_info();
    ctx.accounts.position.close(to)?;

    let s = b.settlement;
    let none = settle::CornerAmount {
        x: 0,
        fv: None,
        pprime: None,
    };
    let [lo, hi] = s.corners.unwrap_or([none; 2]);
    let pp = |c: &settle::CornerAmount| c.pprime.and_then(|p| u16::try_from(p).ok()).unwrap_or(0);
    emit_cpi!(SolSettleEvent {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        position: position_key,
        keeper: keeper_key,
        condition: s.kind as u8,
        b_live: e.facts.b_live.unwrap_or(0),
        b_rs: e.facts.b_rs.unwrap_or(0),
        b_ema: e.b_ema.unwrap_or(0),
        p_lo: e.bx.p_lo,
        p_hi: e.bx.p_hi,
        l_lo: e.bx.l_lo,
        l_hi: e.bx.l_hi,
        x_lo: lo.x,
        x_hi: hi.x,
        pprime_lo: pp(&lo),
        pprime_hi: pp(&hi),
        fv: s.fv.unwrap_or(0),
        k_s: b.payout.keeper,
        lender_amount: b.payout.lenders,
        lump: b.payout.lump,
        borrower_amount: b.payout.borrower,
        ema_age_s: e.ema_age_s,
        vest_flushed: b.vest_flushed,
    });
    if let Some(ev) = rolled {
        emit_cpi!(ev);
    }
    Ok(())
}
