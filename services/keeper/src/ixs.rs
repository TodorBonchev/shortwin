//! The instructions the keeper sends (ARCHITECTURE §3.6–§3.9, pool selection included), built
//! from the program's own Anchor account and instruction types (`shortwin::accounts::*`,
//! `shortwin::instruction::*`), with the venue's remaining accounts from [`crate::venue`].

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};

use shortwin::state::{Market, VenueKind};

use crate::pda;
use crate::venue::{self, CurveExtrasFormat, ExtrasChoice};
use crate::view::{MarketView, PositionView};

fn build(
    accounts: impl ToAccountMetas,
    data: impl InstructionData,
    rest: Vec<AccountMeta>,
) -> Instruction {
    let mut metas = accounts.to_account_metas(None);
    metas.extend(rest);
    Instruction {
        program_id: shortwin::ID,
        accounts: metas,
        data: data.data(),
    }
}

/// The ReadSet of `crank_price` and `sol_settle`: every slot read-only.
fn read_only(mv: &MarketView) -> Vec<AccountMeta> {
    read_only_keys(&mv.read_keys)
}

/// `crank_price()` with the market's ReadSet (read-only).
pub fn crank_price(mv: &MarketView) -> Instruction {
    crank_price_with(&mv.key, &mv.market.mint, &mv.read_keys)
}

/// `crank_price()` of `market` with the ReadSet `keys` (read-only): a curve market's transition
/// transaction names the curve's ReadSet, then the pool's (`crate::graduation::transition_ixs`).
pub fn crank_price_with(market: &Pubkey, mint: &Pubkey, keys: &[Pubkey]) -> Instruction {
    build(
        shortwin::accounts::CrankPrice {
            config: pda::config(),
            market: *market,
            mint: *mint,
            oracle_cache: pda::oracle_cache(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CrankPrice {},
        read_only_keys(keys),
    )
}

/// `ReadSet ++ TradeExtras ++ Extras` of a trading exit: the ReadSet writable per the venue's
/// trade marks ([`venue::read_writable`]), a `PumpCurve` market's TradeExtras in `format`.
fn trade_remaining(
    mv: &MarketView,
    extras: &ExtrasChoice,
    format: CurveExtrasFormat,
) -> anyhow::Result<Vec<AccountMeta>> {
    let mut v = venue::read_metas(mv.kind, &mv.read_keys, true)?;
    v.extend(venue::trade_extras_as(
        mv.kind,
        &mv.market,
        &mv.read_accounts,
        format,
    )?);
    v.extend(extras.metas.iter().cloned());
    Ok(v)
}

/// `flag_venue_upgraded(venue)` (ARCH §3.1, §3.9; A7): anyone records a venue program upgrade
/// in the `VenueGuard` latch; `venue` is a `VenueGuard` index, and the ProgramData account the
/// compile-time constant for it.
pub fn flag_venue_upgraded(venue: u8) -> anyhow::Result<Instruction> {
    let program_data = shortwin::constants::venue_program_data(venue)
        .ok_or_else(|| anyhow::anyhow!("venue index {venue} out of range"))?;
    Ok(build(
        shortwin::accounts::FlagVenueUpgraded {
            venue_guard: pda::venue_guard(),
            program_data,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::FlagVenueUpgraded { venue },
        Vec::new(),
    ))
}

/// `crank_sol_usd()` with the configured Pyth `PriceUpdateV2` `price_update` (ARCH §3.9; duty 11):
/// anyone refreshes the `OracleCache`.
pub fn crank_sol_usd(price_update: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::CrankSolUsd {
            config: pda::config(),
            oracle_cache: pda::oracle_cache(),
            price_update: *price_update,
        },
        shortwin::instruction::CrankSolUsd {},
        Vec::new(),
    )
}

/// `CoveragePauseSet.reason` of the keeper's coverage SLO (O17): the program's
/// `events::COVERAGE_REASON_KEEPER`, the only reason it accepts.
pub const COVERAGE_REASON_KEEPER: u8 = shortwin::events::COVERAGE_REASON_KEEPER;

/// `set_coverage_pause(paused, reason = COVERAGE_REASON_KEEPER)` on `market`, signed by the
/// `coverage_authority` `authority` (O17; ARCH §3.1) — or, for a clear only, by the Emergency
/// vault `pause_authority` (OWNER-M7-3 (b), 2026-10-01): sets or clears `Market.flags` bit 6
/// (`paused_coverage`), which refuses opens only (`CoveragePaused`); it moves no funds and
/// never touches an exit. Accounts `[coverage_authority (signer), config, market (writable),
/// event_authority, program]` (`#[event_cpi]`).
pub fn set_coverage_pause(authority: &Pubkey, market: &Pubkey, paused: bool) -> Instruction {
    build(
        shortwin::accounts::SetCoveragePause {
            coverage_authority: *authority,
            config: pda::config(),
            market: *market,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetCoveragePause {
            paused,
            reason: COVERAGE_REASON_KEEPER,
        },
        Vec::new(),
    )
}

fn read_only_keys(keys: &[Pubkey]) -> Vec<AccountMeta> {
    keys.iter()
        .map(|k| AccountMeta::new_readonly(*k, false))
        .collect()
}

/// `propose_pool(candidate_kind)` on `market` (ARCH §2.6, §3.9; M4): no signer. Remaining, all
/// read-only: `prefix` — a list-only market's detection set `[bonding_curve, canonical_pool]`
/// ([`detection_set`]) or a Raydium market's own ReadSet — then the candidate's ReadSet in its
/// adapter's order (AMM v4 `[pool, coin vault, pc vault, LP mint]`, CPMM `[pool, amm_config,
/// token 0 vault, token 1 vault, LP mint]`). `candidate_kind` is 4 (AMM v4) or 5 (CPMM).
pub fn propose_pool(
    market_key: &Pubkey,
    mint: &Pubkey,
    prefix: &[Pubkey],
    candidate_kind: VenueKind,
    candidate: &[Pubkey],
) -> Instruction {
    let mut rest = read_only_keys(prefix);
    rest.extend(read_only_keys(candidate));
    build(
        shortwin::accounts::ProposePool {
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            market: *market_key,
            mint: *mint,
            oracle_cache: pda::oracle_cache(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ProposePool {
            candidate_kind: candidate_kind as u8,
        },
        rest,
    )
}

/// `execute_pool_switch()` on `mv`'s market (collection windows only; ARCH §2.6 rule 3, §3.9):
/// the market's ReadSet, then the collection window's candidate —
/// `fp_keys[..read_len(fp_venue)]` ([`crate::pool_monitor::window_candidate`]) — all read-only.
pub fn execute_pool_switch(mv: &MarketView, candidate: &[Pubkey]) -> Instruction {
    execute_pool_switch_with(&mv.key, &mv.market.mint, &mv.read_keys, candidate)
}

/// `execute_pool_switch()` on `market_key` with the incumbent's accounts `prefix` — a Raydium
/// market's ReadSet, or a list-only market's detection set (a window of a list-only market with
/// positions, M4-SEC-4) — then the window candidate's ReadSet, all read-only.
pub fn execute_pool_switch_with(
    market_key: &Pubkey,
    mint: &Pubkey,
    prefix: &[Pubkey],
    candidate: &[Pubkey],
) -> Instruction {
    let mut rest = read_only_keys(prefix);
    rest.extend(read_only_keys(candidate));
    build(
        shortwin::accounts::ExecutePoolSwitch {
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            market: *market_key,
            mint: *mint,
            oracle_cache: pda::oracle_cache(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ExecutePoolSwitch {},
        rest,
    )
}

/// A list-only market's detection set `[bonding_curve, canonical_pool]` (the market's stored
/// PDAs; `propose_pool` key-checks them and runs `classify_pump` on them, D1).
pub fn detection_set(market: &Market) -> [Pubkey; 2] {
    [market.bonding_curve, market.canonical_pool]
}

/// `liquidate(extras)` of `pos` by `keeper` (ARCH §3.6); a curve market's TradeExtras are the v3
/// list ([`liquidate_as`] for the other).
pub fn liquidate(
    keeper: &Pubkey,
    mv: &MarketView,
    pos: &PositionView,
    extras: &ExtrasChoice,
) -> anyhow::Result<Instruction> {
    liquidate_as(keeper, mv, pos, extras, CurveExtrasFormat::V3)
}

/// [`liquidate`] with a curve market's TradeExtras in `format` (pump-final §7's upgrade window).
pub fn liquidate_as(
    keeper: &Pubkey,
    mv: &MarketView,
    pos: &PositionView,
    extras: &ExtrasChoice,
    format: CurveExtrasFormat,
) -> anyhow::Result<Instruction> {
    let m = &mv.market;
    Ok(build(
        shortwin::accounts::Liquidate {
            keeper: *keeper,
            borrower: pos.pos.borrower,
            config: pda::config(),
            market: mv.key,
            position: pos.key,
            exposure: pda::exposure(&mv.key, &pos.pos.borrower),
            mint: m.mint,
            pool_vault: m.pool_vault,
            market_vault: m.market_vault,
            trader: m.trader,
            token_program: m.token_program,
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Liquidate { extras: extras.sel },
        trade_remaining(mv, extras, format)?,
    ))
}

/// `expire(extras)` of `pos` by `keeper` (ARCH §3.7); a curve market's TradeExtras are the v3
/// list ([`expire_as`] for the other).
pub fn expire(
    keeper: &Pubkey,
    mv: &MarketView,
    pos: &PositionView,
    extras: &ExtrasChoice,
) -> anyhow::Result<Instruction> {
    expire_as(keeper, mv, pos, extras, CurveExtrasFormat::V3)
}

/// [`expire`] with a curve market's TradeExtras in `format` (pump-final §7's upgrade window).
pub fn expire_as(
    keeper: &Pubkey,
    mv: &MarketView,
    pos: &PositionView,
    extras: &ExtrasChoice,
    format: CurveExtrasFormat,
) -> anyhow::Result<Instruction> {
    let m = &mv.market;
    Ok(build(
        shortwin::accounts::Expire {
            keeper: *keeper,
            borrower: pos.pos.borrower,
            config: pda::config(),
            market: mv.key,
            position: pos.key,
            exposure: pda::exposure(&mv.key, &pos.pos.borrower),
            mint: m.mint,
            pool_vault: m.pool_vault,
            market_vault: m.market_vault,
            trader: m.trader,
            token_program: m.token_program,
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Expire { extras: extras.sel },
        trade_remaining(mv, extras, format)?,
    ))
}

/// `sol_settle()` of `pos` by `keeper` (ARCH §3.8): ReadSet only; the `ShareEpoch` record
/// of the market's current epoch (created only if this settlement rolls it).
pub fn sol_settle(keeper: &Pubkey, mv: &MarketView, pos: &PositionView) -> Instruction {
    let m = &mv.market;
    build(
        shortwin::accounts::SolSettle {
            keeper: *keeper,
            config: pda::config(),
            market: mv.key,
            position: pos.key,
            borrower: pos.pos.borrower,
            exposure: pda::exposure(&mv.key, &pos.pos.borrower),
            market_vault: m.market_vault,
            mint: m.mint,
            share_epoch_rec: pda::share_epoch(&mv.key, m.share_epoch),
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SolSettle {},
        read_only(mv),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The M7 interface contract: `discriminator ‖ paused: bool ‖ reason: u8`, the authority the
    /// only signer, the market the only writable account.
    #[test]
    fn set_coverage_pause_is_the_contract() {
        let h = solana_sha256_hasher::hash(b"global:set_coverage_pause");
        let (a, m) = (Pubkey::new_unique(), Pubkey::new_unique());
        let ix = set_coverage_pause(&a, &m, true);
        assert_eq!(ix.program_id, shortwin::ID);
        assert_eq!(&ix.data[..8], &h.to_bytes()[..8]);
        assert_eq!(ix.data[8..], [1, COVERAGE_REASON_KEEPER]);
        assert_eq!(COVERAGE_REASON_KEEPER, 1);
        assert_eq!(set_coverage_pause(&a, &m, false).data[8], 0);
        let metas: Vec<(Pubkey, bool, bool)> = ix
            .accounts
            .iter()
            .map(|x| (x.pubkey, x.is_signer, x.is_writable))
            .collect();
        assert_eq!(
            metas,
            vec![
                (a, true, false),
                (pda::config(), false, false),
                (m, false, true),
                (pda::event_authority(), false, false),
                (shortwin::ID, false, false),
            ]
        );
    }
}
